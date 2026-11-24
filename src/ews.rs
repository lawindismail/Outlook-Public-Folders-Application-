//! Exchange Web Services (EWS) client for public folders.

use crate::model::*;
use crate::{ntlm, oauth};
use base64::Engine as _;
use roxmltree::{Document, Node};
use std::collections::HashMap;
use std::io::Read;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;
const V2013: &str = "Exchange2013_SP1";
const V2010: &str = "Exchange2010_SP2";

#[derive(Clone, Debug, PartialEq)]
pub enum SignIn {
    /// Ask the server which method it wants.
    Auto,
    Ntlm,
    Basic,
    /// The Windows login of whoever uses this PC (no password typed).
    Windows,
    /// Microsoft 365: OAuth device code.
    Microsoft,
}

#[derive(Clone)]
pub struct ConnectConfig {
    pub server: String,
    pub email: String,
    pub username: String,
    pub password: String,
    pub sign_in: SignIn,
    pub ignore_cert_errors: bool,
    /// "auto", or an EWS schema version.
    pub version: String,
    pub tenant: String,
    pub client_id: String,
}

#[derive(Clone, Debug, PartialEq)]
enum Method {
    Unknown,
    Basic,
    /// NTLM, sent under this scheme name ("NTLM" or "Negotiate").
    Ntlm(String),
    /// Windows login through SSPI, sent under this scheme name.
    Sspi(String),
    Bearer,
}

pub struct EwsClient {
    agent: ureq::Agent,
    ews_url: Mutex<String>,
    autodiscover_url: Mutex<String>,
    cfg: ConnectConfig,
    method: Mutex<Method>,
    token: Mutex<Option<oauth::Token>>,
    version: Mutex<String>,
    version_fixed: bool,
    hierarchy_mailbox: Mutex<Option<String>>,
    routes: Mutex<HashMap<String, Option<String>>>,
    /// NTLM signs in a connection, so requests are sent one at a time.
    lock: Mutex<()>,
}

pub fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Turn what the person typed ("mail.company.com" or a full URL) into the
/// EWS and Autodiscover addresses.
pub fn urls_for(server: &str) -> (String, String) {
    let s = server.trim().trim_end_matches('/');
    let with_scheme = if s.starts_with("http://") || s.starts_with("https://") {
        s.to_string()
    } else {
        format!("https://{s}")
    };
    let after_scheme = with_scheme.find("://").map(|i| i + 3).unwrap_or(0);
    let (base, path) = match with_scheme[after_scheme..].find('/') {
        Some(i) => (with_scheme[..after_scheme + i].to_string(), with_scheme[after_scheme + i..].to_string()),
        None => (with_scheme.clone(), String::new()),
    };
    let ews = if path.to_ascii_lowercase().ends_with(".asmx") {
        with_scheme.clone()
    } else {
        format!("{base}/EWS/Exchange.asmx")
    };
    (ews, format!("{base}/autodiscover/autodiscover.svc"))
}

fn el<'a, 'i>(n: Node<'a, 'i>, name: &str) -> Option<Node<'a, 'i>> {
    n.children().find(|c| c.is_element() && c.tag_name().name() == name)
}
fn els<'a, 'i>(n: Node<'a, 'i>, name: &'a str) -> impl Iterator<Item = Node<'a, 'i>> + 'a {
    n.children().filter(move |c| c.is_element() && c.tag_name().name() == name)
}
fn text_of(n: Node, name: &str) -> String {
    el(n, name).and_then(|c| c.text()).unwrap_or_default().trim().to_string()
}
fn find<'a, 'i>(n: Node<'a, 'i>, name: &str) -> Option<Node<'a, 'i>> {
    n.descendants().find(|c| c.is_element() && c.tag_name().name() == name)
}

fn mailbox_name(n: Option<Node>) -> (String, String) {
    let Some(n) = n else { return Default::default() };
    let Some(mb) = find(n, "Mailbox") else { return Default::default() };
    let name = text_of(mb, "Name");
    let email = text_of(mb, "EmailAddress");
    (if name.is_empty() { email.clone() } else { name }, email)
}

fn recipients(n: Option<Node>) -> String {
    let Some(n) = n else { return String::new() };
    els(n, "Mailbox")
        .map(|mb| {
            let name = text_of(mb, "Name");
            let email = text_of(mb, "EmailAddress");
            match (name.is_empty(), email.is_empty()) {
                (false, false) if name != email => format!("{name} <{email}>"),
                (false, _) => name,
                _ => email,
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn date_long(s: &str) -> String {
    parse_ews_date(s).map(|d| fmt_long_date(&d)).unwrap_or_else(|| s.to_string())
}

/// Check an EWS answer for a SOAP fault or an error response.
fn check(text: &str) -> Result<(), String> {
    let doc = Document::parse(text).map_err(|_| "The server sent an answer that isn't EWS XML. Check the server address.".to_string())?;
    let root = doc.root();
    if let Some(f) = find(root, "Fault") {
        let msg = find(f, "faultstring").and_then(|n| n.text()).unwrap_or("unknown error");
        let code = find(f, "ResponseCode").and_then(|n| n.text()).unwrap_or("");
        return Err(format!("{code} {msg}").trim().to_string());
    }
    for rc in root.descendants().filter(|n| n.is_element() && n.tag_name().name() == "ResponseCode") {
        let code = rc.text().unwrap_or("").trim();
        if code != "NoError" {
            let msg = rc.parent().and_then(|p| el(p, "MessageText")).and_then(|n| n.text()).unwrap_or("");
            return Err(friendly_ews_error(code, msg));
        }
    }
    Ok(())
}

fn friendly_ews_error(code: &str, msg: &str) -> String {
    match code {
        "ErrorNoPublicFolderReplicaAvailable" | "ErrorNoPublicFolderServerAvailable" => {
            "No public folder server is available for your mailbox.".into()
        }
        "ErrorAccessDenied" => "You don't have permission to open this folder.".into(),
        "ErrorItemNotFound" | "ErrorFolderNotFound" => "It can't be found any more. Try Refresh.".into(),
        _ => format!("{code}: {msg}"),
    }
}

impl EwsClient {
    pub fn new(cfg: ConnectConfig) -> Result<Self, String> {
        let tls = native_tls::TlsConnector::builder()
            .danger_accept_invalid_certs(cfg.ignore_cert_errors)
            .danger_accept_invalid_hostnames(cfg.ignore_cert_errors)
            .build()
            .map_err(|e| format!("Secure connection setup failed: {e}"))?;
        let agent = ureq::AgentBuilder::new()
            .tls_connector(Arc::new(tls))
            .timeout_connect(Duration::from_secs(12))
            .timeout(Duration::from_secs(300))
            .try_proxy_from_env(true)
            .user_agent("PublicFolderViewer/0.1")
            .build();
        let mut cfg = cfg;
        if cfg.username.trim().is_empty() {
            // Most Exchange servers accept the email address as the user name.
            cfg.username = cfg.email.trim().to_string();
        }
        let (ews_url, autodiscover_url) = urls_for(&cfg.server);
        let method = initial_method(&cfg.sign_in);
        let (version, fixed) = if cfg.version == "auto" || cfg.version.is_empty() {
            (V2013.to_string(), false)
        } else {
            (cfg.version.clone(), true)
        };
        Ok(Self {
            agent,
            ews_url: Mutex::new(ews_url),
            autodiscover_url: Mutex::new(autodiscover_url),
            cfg,
            method: Mutex::new(method),
            token: Mutex::new(None),
            version: Mutex::new(version),
            version_fixed: fixed,
            hierarchy_mailbox: Mutex::new(None),
            routes: Mutex::new(HashMap::new()),
            lock: Mutex::new(()),
        })
    }

    pub fn agent(&self) -> &ureq::Agent {
        &self.agent
    }

    pub fn set_token(&self, t: oauth::Token) {
        *self.token.lock().unwrap() = Some(t);
    }

    fn is_2010(&self) -> bool {
        self.version.lock().unwrap().starts_with("Exchange2010")
    }

    fn access_token(&self) -> Result<String, String> {
        let mut guard = self.token.lock().unwrap();
        let t = guard.as_ref().ok_or("Not signed in to Microsoft 365.")?;
        if t.expires_at > Instant::now() + Duration::from_secs(60) {
            return Ok(t.access.clone());
        }
        let r = t.refresh.clone().ok_or("Your Microsoft sign-in has expired. Disconnect and sign in again.")?;
        let fresh = oauth::refresh(&self.agent, &self.cfg.tenant, &self.cfg.client_id, &r)?;
        let access = fresh.access.clone();
        *guard = Some(fresh);
        Ok(access)
    }

    fn send_raw(
        &self,
        url: &str,
        body: &str,
        auth: Option<&str>,
        route: Option<&str>,
        soap_action: Option<&str>,
    ) -> Result<(u16, Vec<String>, String), String> {
        let mut req = self.agent.post(url).set("Content-Type", "text/xml; charset=utf-8");
        if let Some(a) = auth {
            req = req.set("Authorization", a);
        }
        if let Some(a) = soap_action {
            req = req.set("SOAPAction", a);
        }
        match route {
            Some(mbx) => {
                req = req.set("X-AnchorMailbox", mbx).set("X-PublicFolderMailbox", mbx);
            }
            None if !self.cfg.email.trim().is_empty() => {
                req = req.set("X-AnchorMailbox", self.cfg.email.trim());
            }
            None => {}
        }
        let resp = match req.send_string(body) {
            Ok(r) => r,
            Err(ureq::Error::Status(_, r)) => r,
            Err(ureq::Error::Transport(t)) => return Err(format!("Could not reach the server: {t}")),
        };
        let status = resp.status();
        let auths = resp.all("www-authenticate").iter().map(|s| s.to_string()).collect();
        let mut text = String::new();
        resp.into_reader()
            .read_to_string(&mut text)
            .map_err(|e| format!("The connection broke while reading: {e}"))?;
        Ok((status, auths, text))
    }

    fn post(&self, url: &str, body: &str, route: Option<&str>, soap_action: Option<&str>) -> Result<String, String> {
        let _one_at_a_time = self.lock.lock().unwrap();
        let mut method = self.method.lock().unwrap().clone();
        if method == Method::Unknown {
            let (status, auths, text) = self.send_raw(url, body, None, route, soap_action)?;
            if status != 401 {
                return finish(status, text);
            }
            let has = |p: &str| auths.iter().any(|h| h.to_ascii_lowercase().starts_with(p));
            method = if self.cfg.sign_in == SignIn::Windows {
                if has("negotiate") {
                    Method::Sspi("Negotiate".into())
                } else if has("ntlm") {
                    Method::Sspi("NTLM".into())
                } else {
                    return Err("This server doesn't accept Windows logins. Untick “Sign in as me” and type your password.".into());
                }
            } else if has("ntlm") {
                Method::Ntlm("NTLM".into())
            } else if has("negotiate") {
                Method::Ntlm("Negotiate".into())
            } else if has("basic") {
                Method::Basic
            } else if has("bearer") {
                return Err("This server needs a Microsoft 365 sign-in. Choose Microsoft 365 on the sign-in screen.".into());
            } else {
                return Err(format!("The server asked for a sign-in this app doesn't support ({}).", auths.join(", ")));
            };
            *self.method.lock().unwrap() = method.clone();
        }
        let (status, _, text) = match &method {
            Method::Unknown => unreachable!(),
            Method::Basic => {
                let raw = format!("{}:{}", self.cfg.username.trim(), self.cfg.password);
                let h = format!("Basic {}", B64.encode(raw));
                self.send_raw(url, body, Some(&h), route, soap_action)?
            }
            Method::Bearer => {
                let h = format!("Bearer {}", self.access_token()?);
                self.send_raw(url, body, Some(&h), route, soap_action)?
            }
            Method::Ntlm(scheme) => {
                let t1 = format!("{scheme} {}", B64.encode(ntlm::negotiate()));
                let (status, auths, text) = self.send_raw(url, body, Some(&t1), route, soap_action)?;
                if status != 401 {
                    (status, auths, text)
                } else {
                    let prefix = format!("{} ", scheme.to_ascii_lowercase());
                    let token = auths
                        .iter()
                        .find(|h| h.to_ascii_lowercase().starts_with(&prefix) && h.len() > prefix.len())
                        .map(|h| h[prefix.len()..].trim().to_string())
                        .ok_or("The server refused the Windows (NTLM) sign-in. Try Basic under Advanced.")?;
                    let bytes = B64.decode(token).map_err(|_| "The server's sign-in challenge was unreadable.")?;
                    let ch = ntlm::parse_challenge(&bytes)?;
                    let (user, domain) = ntlm::split_user(self.cfg.username.trim());
                    let ws = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "WORKSTATION".into());
                    let t3 = ntlm::authenticate(&ch, &user, &domain, &self.cfg.password, &ws);
                    let h = format!("{scheme} {}", B64.encode(t3));
                    self.send_raw(url, body, Some(&h), route, soap_action)?
                }
            }
            Method::Sspi(scheme) => self.sspi_send(scheme, url, body, route, soap_action)?,
        };
        if status == 401 && matches!(method, Method::Sspi(_)) {
            return Err("The server didn't accept your Windows login. Untick “Sign in as me” and type your password.".into());
        }
        finish(status, text)
    }

    #[cfg(windows)]
    fn sspi_send(
        &self,
        scheme: &str,
        url: &str,
        body: &str,
        route: Option<&str>,
        soap_action: Option<&str>,
    ) -> Result<(u16, Vec<String>, String), String> {
        let host = url.split("://").nth(1).and_then(|h| h.split(['/', ':']).next()).unwrap_or("").to_string();
        let package = if scheme == "Negotiate" { "Negotiate" } else { "NTLM" };
        let mut ctx = crate::winauth::Context::new(package, &host)?;
        let mut token = ctx.step(None)?;
        let prefix = format!("{} ", scheme.to_ascii_lowercase());
        for _ in 0..4 {
            let h = format!("{scheme} {}", B64.encode(&token));
            let (status, auths, text) = self.send_raw(url, body, Some(&h), route, soap_action)?;
            if status != 401 {
                return Ok((status, auths, text));
            }
            let Some(next) = auths
                .iter()
                .find(|h| h.to_ascii_lowercase().starts_with(&prefix) && h.len() > prefix.len())
                .and_then(|h| B64.decode(h[prefix.len()..].trim()).ok())
            else {
                return Ok((status, auths, text));
            };
            token = ctx.step(Some(&next))?;
            if token.is_empty() {
                return Ok((status, auths, text));
            }
        }
        Err("Windows sign-in didn't finish.".into())
    }

    #[cfg(not(windows))]
    fn sspi_send(
        &self,
        _scheme: &str,
        _url: &str,
        _body: &str,
        _route: Option<&str>,
        _soap_action: Option<&str>,
    ) -> Result<(u16, Vec<String>, String), String> {
        Err("Signing in with your Windows login only works on Windows.".into())
    }

    /// When no server was typed, find it from the email address with
    /// Autodiscover, the way Outlook does.
    pub fn discover_server(&self) -> Result<(), String> {
        if !self.cfg.server.trim().is_empty() {
            return Ok(());
        }
        let mut addr = self.cfg.email.trim().to_string();
        let Some(domain) = addr.split('@').nth(1).filter(|d| d.contains('.')).map(str::to_string) else {
            return Err("Enter your work email address so the app can find your server.".into());
        };
        let mut candidates = autodiscover_candidates(&domain);
        if let Some(u) = self.redirected_autodiscover(&domain) {
            candidates.push(u);
        }
        let mut tried = std::collections::HashSet::new();
        let mut sign_in_error = None;
        let mut i = 0;
        while i < candidates.len() && i < 10 {
            let url = candidates[i].clone();
            i += 1;
            if !tried.insert(url.to_lowercase()) {
                continue;
            }
            match self.post(&url, &pox_request(&addr), None, None) {
                Ok(text) => match parse_pox(&text) {
                    Pox::Ews { ews, pf_mailbox } => {
                        let low = ews.to_lowercase();
                        if low.contains("outlook.office365.com") || low.contains("outlook.office.com") {
                            return Err("Your mailbox is in Microsoft 365. Choose “Microsoft 365” at the top.".into());
                        }
                        let (e, a) = urls_for(&ews);
                        *self.ews_url.lock().unwrap() = e;
                        *self.autodiscover_url.lock().unwrap() = a;
                        if pf_mailbox.is_some() {
                            *self.hierarchy_mailbox.lock().unwrap() = pf_mailbox;
                        }
                        // Let the Exchange server pick its own sign-in method.
                        *self.method.lock().unwrap() = initial_method(&self.cfg.sign_in);
                        return Ok(());
                    }
                    Pox::RedirectAddr(a) => {
                        if let Some(d) = a.split('@').nth(1) {
                            candidates.extend(autodiscover_candidates(d));
                        }
                        addr = a;
                    }
                    Pox::RedirectUrl(u) => candidates.push(u),
                    Pox::Nothing => {}
                },
                Err(e) if e.contains("Sign-in was refused") || e.contains("Windows login") => sign_in_error = Some(e),
                Err(_) => {}
            }
            // A website that isn't Exchange may have set a sign-in method; start fresh.
            *self.method.lock().unwrap() = initial_method(&self.cfg.sign_in);
        }
        Err(sign_in_error.unwrap_or_else(|| {
            format!(
                "Couldn't find the Exchange server for {domain} by itself. Open “Advanced” and type the server name — see “Where do I find these details?”."
            )
        }))
    }

    /// Outlook's third Autodiscover step: plain-HTTP redirect to the real address.
    fn redirected_autodiscover(&self, domain: &str) -> Option<String> {
        let agent = ureq::AgentBuilder::new().redirects(0).timeout_connect(Duration::from_secs(8)).timeout(Duration::from_secs(15)).build();
        let resp = match agent.get(&format!("http://autodiscover.{domain}/autodiscover/autodiscover.xml")).call() {
            Ok(r) => r,
            Err(ureq::Error::Status(_, r)) => r,
            Err(_) => return None,
        };
        let loc = resp.header("location")?.to_string();
        (loc.starts_with("https://") && loc.to_lowercase().ends_with("autodiscover.xml")).then_some(loc)
    }

    fn envelope(&self, inner: &str) -> String {
        let v = self.version.lock().unwrap().clone();
        format!(
            r#"<?xml version="1.0" encoding="utf-8"?>
<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types" xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages">
<soap:Header><t:RequestServerVersion Version="{v}"/></soap:Header>
<soap:Body>{inner}</soap:Body>
</soap:Envelope>"#
        )
    }

    /// Send one EWS operation; falls back to the Exchange 2010 schema when an
    /// older server rejects the newer one.
    fn call(&self, build: &dyn Fn(bool) -> String, route: Option<&str>) -> Result<String, String> {
        let ews_url = self.ews_url.lock().unwrap().clone();
        let text = self.post(&ews_url, &self.envelope(&build(self.is_2010())), route, None)?;
        match check(&text) {
            Ok(()) => Ok(text),
            Err(e) if !self.version_fixed && !self.is_2010() && (e.contains("ServerVersion") || e.contains("SchemaValidation")) => {
                *self.version.lock().unwrap() = V2010.to_string();
                let text = self.post(&ews_url, &self.envelope(&build(true)), route, None)?;
                check(&text)?;
                Ok(text)
            }
            Err(e) => Err(e),
        }
    }

    /// Ask Autodiscover which mailbox holds the public folder hierarchy
    /// (Exchange 2013 and later, including Microsoft 365). Best effort.
    pub fn discover_hierarchy(&self) {
        let email = self.cfg.email.trim();
        if email.is_empty() || self.hierarchy_mailbox.lock().unwrap().is_some() {
            return;
        }
        let autodiscover_url = self.autodiscover_url.lock().unwrap().clone();
        let body = format!(
            r#"<?xml version="1.0" encoding="utf-8"?>
<soap:Envelope xmlns:a="http://schemas.microsoft.com/exchange/2010/Autodiscover" xmlns:wsa="http://www.w3.org/2005/08/addressing" xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/">
<soap:Header>
<a:RequestedServerVersion>Exchange2013</a:RequestedServerVersion>
<wsa:Action>http://schemas.microsoft.com/exchange/2010/Autodiscover/Autodiscover/GetUserSettings</wsa:Action>
<wsa:To>{to}</wsa:To>
</soap:Header>
<soap:Body>
<a:GetUserSettingsRequestMessage>
<a:Request><a:Users><a:User><a:Mailbox>{mb}</a:Mailbox></a:User></a:Users>
<a:RequestedSettings><a:Setting>PublicFolderInformation</a:Setting></a:RequestedSettings></a:Request>
</a:GetUserSettingsRequestMessage>
</soap:Body>
</soap:Envelope>"#,
            to = xml_escape(&autodiscover_url),
            mb = xml_escape(email)
        );
        let action = "http://schemas.microsoft.com/exchange/2010/Autodiscover/Autodiscover/GetUserSettings";
        let Ok(text) = self.post(&autodiscover_url, &body, None, Some(action)) else { return };
        let Ok(doc) = Document::parse(&text) else { return };
        for s in doc.descendants().filter(|n| n.is_element() && n.tag_name().name() == "UserSetting") {
            if text_of(s, "Name") == "PublicFolderInformation" {
                let v = text_of(s, "Value");
                if v.contains('@') {
                    *self.hierarchy_mailbox.lock().unwrap() = Some(v);
                }
            }
        }
    }

    fn hierarchy_route(&self) -> Option<String> {
        self.hierarchy_mailbox.lock().unwrap().clone()
    }

    /// Work out which mailbox holds a folder's content (Exchange 2013+),
    /// from the folder's replica list. Falls back to the hierarchy mailbox.
    fn content_route(&self, folder_id: &str) -> Option<String> {
        if self.is_2010() {
            return None;
        }
        if let Some(r) = self.routes.lock().unwrap().get(folder_id) {
            return r.clone().or_else(|| self.hierarchy_route());
        }
        let domain = self.cfg.email.split('@').nth(1).map(str::to_string);
        let id = xml_escape(folder_id);
        let found = self
            .call(
                &|_| {
                    format!(
                        r#"<m:GetFolder><m:FolderShape><t:BaseShape>IdOnly</t:BaseShape><t:AdditionalProperties><t:ExtendedFieldURI PropertyTag="0x6698" PropertyType="Binary"/></t:AdditionalProperties></m:FolderShape><m:FolderIds><t:FolderId Id="{id}"/></m:FolderIds></m:GetFolder>"#
                    )
                },
                self.hierarchy_route().as_deref(),
            )
            .ok()
            .and_then(|text| {
                let doc = Document::parse(&text).ok()?;
                let ep = find(doc.root(), "ExtendedProperty")?;
                let raw = B64.decode(text_of(ep, "Value")).ok()?;
                let s = String::from_utf8_lossy(&raw);
                let guid = s.split('\0').next()?.trim().to_string();
                (guid.len() == 36 && domain.is_some()).then(|| format!("{guid}@{}", domain.unwrap()))
            });
        self.routes.lock().unwrap().insert(folder_id.to_string(), found.clone());
        found.or_else(|| self.hierarchy_route())
    }

    fn find_folders(&self, parent: &str) -> Result<Vec<FolderInfo>, String> {
        let parent_xml = if parent.is_empty() {
            r#"<t:DistinguishedFolderId Id="publicfoldersroot"/>"#.to_string()
        } else {
            format!(r#"<t:FolderId Id="{}"/>"#, xml_escape(parent))
        };
        let text = self.call(
            &|_| {
                format!(
                    r#"<m:FindFolder Traversal="Shallow"><m:FolderShape><t:BaseShape>Default</t:BaseShape><t:AdditionalProperties><t:FieldURI FieldURI="folder:FolderClass"/><t:FieldURI FieldURI="folder:ChildFolderCount"/><t:FieldURI FieldURI="folder:TotalCount"/></t:AdditionalProperties></m:FolderShape><m:ParentFolderIds>{parent_xml}</m:ParentFolderIds></m:FindFolder>"#
                )
            },
            self.hierarchy_route().as_deref(),
        )?;
        parse_folders(&text)
    }
}

pub fn parse_folders(text: &str) -> Result<Vec<FolderInfo>, String> {
    {
        let doc = Document::parse(text).map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        if let Some(folders) = find(doc.root(), "Folders") {
            for f in folders.children().filter(|c| c.is_element()) {
                let id = el(f, "FolderId").and_then(|n| n.attribute("Id")).unwrap_or_default().to_string();
                if id.is_empty() {
                    continue;
                }
                out.push(FolderInfo {
                    id,
                    name: text_of(f, "DisplayName"),
                    class: text_of(f, "FolderClass"),
                    child_count: text_of(f, "ChildFolderCount").parse().unwrap_or(0),
                    total_count: text_of(f, "TotalCount").parse().unwrap_or(0),
                });
            }
        }
        out.sort_by_key(|f| f.name.to_lowercase());
        Ok(out)
    }
}

fn finish(status: u16, text: String) -> Result<String, String> {
    match status {
        200..=299 => Ok(text),
        500 if text.contains("Envelope") => Ok(text),
        401 => Err("Sign-in was refused. Check your user name and password.".into()),
        403 => Err("The server refused access (403). Your account may not be allowed to use EWS.".into()),
        404 => Err("Exchange Web Services wasn't found at this address (404). Check the server name.".into()),
        s => Err(format!("The server answered with an error (HTTP {s}).")),
    }
}

fn initial_method(sign_in: &SignIn) -> Method {
    match sign_in {
        SignIn::Auto | SignIn::Windows => Method::Unknown,
        SignIn::Ntlm => Method::Ntlm("NTLM".into()),
        SignIn::Basic => Method::Basic,
        SignIn::Microsoft => Method::Bearer,
    }
}

fn autodiscover_candidates(domain: &str) -> Vec<String> {
    vec![
        format!("https://autodiscover.{domain}/autodiscover/autodiscover.xml"),
        format!("https://{domain}/autodiscover/autodiscover.xml"),
    ]
}

fn pox_request(email: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<Autodiscover xmlns="http://schemas.microsoft.com/exchange/autodiscover/outlook/requestschema/2006">
<Request><EMailAddress>{}</EMailAddress>
<AcceptableResponseSchema>http://schemas.microsoft.com/exchange/autodiscover/outlook/responseschema/2006a</AcceptableResponseSchema>
</Request></Autodiscover>"#,
        xml_escape(email)
    )
}

#[derive(Debug, PartialEq)]
pub enum Pox {
    Ews { ews: String, pf_mailbox: Option<String> },
    RedirectAddr(String),
    RedirectUrl(String),
    Nothing,
}

/// Read an Outlook (POX) Autodiscover answer.
pub fn parse_pox(text: &str) -> Pox {
    let Ok(doc) = Document::parse(text) else { return Pox::Nothing };
    let Some(acc) = find(doc.root(), "Account") else { return Pox::Nothing };
    match text_of(acc, "Action").as_str() {
        "redirectAddr" => {
            let a = text_of(acc, "RedirectAddr");
            if a.contains('@') {
                return Pox::RedirectAddr(a);
            }
        }
        "redirectUrl" => {
            let u = text_of(acc, "RedirectUrl");
            if u.starts_with("https://") {
                return Pox::RedirectUrl(u);
            }
        }
        _ => {}
    }
    let mut ews: Option<String> = None;
    for p in els(acc, "Protocol") {
        let u = text_of(p, "EwsUrl");
        if !u.is_empty() && (ews.is_none() || text_of(p, "Type") == "EXCH") {
            ews = Some(u);
        }
    }
    let pf_mailbox = find(acc, "PublicFolderInformation").map(|n| text_of(n, "SmtpAddress")).filter(|s| s.contains('@'));
    match ews {
        Some(ews) => Pox::Ews { ews, pf_mailbox },
        None => Pox::Nothing,
    }
}

const PAGE: u32 = 100;

impl Backend for EwsClient {
    fn label(&self) -> String {
        let who = if self.cfg.email.trim().is_empty() { self.cfg.username.trim() } else { self.cfg.email.trim() };
        let host = self.ews_url.lock().unwrap().clone();
        let host = host.split("://").nth(1).and_then(|h| h.split('/').next()).unwrap_or("").to_string();
        format!("{who} on {host}")
    }

    fn root_folders(&self) -> Result<Vec<FolderInfo>, String> {
        self.find_folders("")
    }

    fn child_folders(&self, folder_id: &str) -> Result<Vec<FolderInfo>, String> {
        self.find_folders(folder_id)
    }

    fn items(&self, folder: &FolderInfo, offset: u32) -> Result<ItemPage, String> {
        let route = self.content_route(&folder.id);
        let id = xml_escape(&folder.id);
        let calendar = folder.class.starts_with("IPF.Appointment");
        let text = self.call(
            &|old| {
                let mut props = String::from(
                    r#"<t:FieldURI FieldURI="item:Subject"/><t:FieldURI FieldURI="item:DateTimeReceived"/><t:FieldURI FieldURI="item:ItemClass"/><t:FieldURI FieldURI="item:HasAttachments"/><t:FieldURI FieldURI="message:From"/>"#,
                );
                if calendar {
                    props.push_str(r#"<t:FieldURI FieldURI="calendar:Start"/><t:FieldURI FieldURI="calendar:Location"/>"#);
                }
                if !old {
                    props.push_str(r#"<t:FieldURI FieldURI="item:Preview"/>"#);
                }
                let sort = if calendar { "calendar:Start" } else { "item:DateTimeReceived" };
                format!(
                    r#"<m:FindItem Traversal="Shallow"><m:ItemShape><t:BaseShape>IdOnly</t:BaseShape><t:AdditionalProperties>{props}</t:AdditionalProperties></m:ItemShape><m:IndexedPageItemView MaxEntriesReturned="{PAGE}" Offset="{offset}" BasePoint="Beginning"/><m:SortOrder><t:FieldOrder Order="Descending"><t:FieldURI FieldURI="{sort}"/></t:FieldOrder></m:SortOrder><m:ParentFolderIds><t:FolderId Id="{id}"/></m:ParentFolderIds></m:FindItem>"#
                )
            },
            route.as_deref(),
        )?;
        parse_item_page(&text, calendar, offset)
    }

    fn item(&self, folder_id: &str, item_id: &str) -> Result<ItemDetail, String> {
        let route = self.content_route(folder_id);
        let id = xml_escape(item_id);
        let text = self.call(
            &|_| {
                format!(
                    r#"<m:GetItem><m:ItemShape><t:BaseShape>Default</t:BaseShape><t:BodyType>Text</t:BodyType><t:AdditionalProperties><t:FieldURI FieldURI="item:Subject"/><t:FieldURI FieldURI="item:Body"/><t:FieldURI FieldURI="item:Attachments"/><t:FieldURI FieldURI="item:ItemClass"/><t:FieldURI FieldURI="item:DateTimeReceived"/><t:FieldURI FieldURI="item:DateTimeSent"/></t:AdditionalProperties></m:ItemShape><m:ItemIds><t:ItemId Id="{id}"/></m:ItemIds></m:GetItem>"#
                )
            },
            route.as_deref(),
        )?;
        parse_item_detail(&text, item_id)
    }

    fn attachment(&self, folder_id: &str, attachment_id: &str) -> Result<Vec<u8>, String> {
        let route = self.content_route(folder_id);
        let id = xml_escape(attachment_id);
        let text = self.call(
            &|_| {
                format!(
                    r#"<m:GetAttachment><m:AttachmentShape><t:IncludeMimeContent>true</t:IncludeMimeContent></m:AttachmentShape><m:AttachmentIds><t:AttachmentId Id="{id}"/></m:AttachmentIds></m:GetAttachment>"#
                )
            },
            route.as_deref(),
        )?;
        parse_base64(&text, &["Content", "MimeContent"], "The attachment had no content.")
    }

    fn upload(&self, folder: &FolderInfo, path: &std::path::Path) -> Result<(), String> {
        let bytes = std::fs::read(path).map_err(|e| format!("Couldn't read the file: {e}"))?;
        let route = self.content_route(&folder.id);
        let fid = xml_escape(&folder.id);
        let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        let content = B64.encode(&bytes);
        if ext == "eml" {
            // A standard e-mail file goes in as the message itself, marked as
            // received and read (PR_MESSAGE_FLAGS = MSGFLAG_READ), not as a draft.
            self.call(
                &|_| {
                    format!(
                        r#"<m:CreateItem MessageDisposition="SaveOnly"><m:SavedItemFolderId><t:FolderId Id="{fid}"/></m:SavedItemFolderId><m:Items><t:Message><t:MimeContent CharacterSet="UTF-8">{content}</t:MimeContent><t:ExtendedProperty><t:ExtendedFieldURI PropertyTag="0x0E07" PropertyType="Integer"/><t:Value>1</t:Value></t:ExtendedProperty></t:Message></m:Items></m:CreateItem>"#
                    )
                },
                route.as_deref(),
            )?;
            return Ok(());
        }
        // Anything else: a post with the file attached.
        let name = xml_escape(&path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "File".into()));
        let text = self.call(
            &|_| {
                format!(
                    r#"<m:CreateItem><m:SavedItemFolderId><t:FolderId Id="{fid}"/></m:SavedItemFolderId><m:Items><t:PostItem><t:Subject>{name}</t:Subject></t:PostItem></m:Items></m:CreateItem>"#
                )
            },
            route.as_deref(),
        )?;
        let doc = Document::parse(&text).map_err(|e| e.to_string())?;
        let id = find(doc.root(), "ItemId").ok_or("The server didn't create the post.")?;
        let item = xml_escape(id.attribute("Id").unwrap_or_default());
        let key = xml_escape(id.attribute("ChangeKey").unwrap_or_default());
        self.call(
            &|_| {
                format!(
                    r#"<m:CreateAttachment><m:ParentItemId Id="{item}" ChangeKey="{key}"/><m:Attachments><t:FileAttachment><t:Name>{name}</t:Name><t:Content>{content}</t:Content></t:FileAttachment></m:Attachments></m:CreateAttachment>"#
                )
            },
            route.as_deref(),
        )?;
        Ok(())
    }

    fn mime(&self, folder_id: &str, item_id: &str) -> Result<Vec<u8>, String> {
        let route = self.content_route(folder_id);
        let id = xml_escape(item_id);
        let text = self.call(
            &|_| {
                format!(
                    r#"<m:GetItem><m:ItemShape><t:BaseShape>IdOnly</t:BaseShape><t:IncludeMimeContent>true</t:IncludeMimeContent></m:ItemShape><m:ItemIds><t:ItemId Id="{id}"/></m:ItemIds></m:GetItem>"#
                )
            },
            route.as_deref(),
        )?;
        parse_base64(&text, &["MimeContent"], "The server didn't send the message file.")
    }
}

pub fn parse_item_page(text: &str, calendar: bool, offset: u32) -> Result<ItemPage, String> {
    {
        let doc = Document::parse(text).map_err(|e| e.to_string())?;
        let root = find(doc.root(), "RootFolder").ok_or("The server's answer had no item list.")?;
        let total: u32 = root.attribute("TotalItemsInView").and_then(|v| v.parse().ok()).unwrap_or(0);
        let last = root.attribute("IncludesLastItemInRange").map(|v| v == "true").unwrap_or(true);
        let next: u32 = root.attribute("IndexedPagingOffset").and_then(|v| v.parse().ok()).unwrap_or(offset + PAGE);
        let mut items = Vec::new();
        if let Some(list) = el(root, "Items") {
            for it in list.children().filter(|c| c.is_element()) {
                let id = el(it, "ItemId").and_then(|n| n.attribute("Id")).unwrap_or_default().to_string();
                let class = text_of(it, "ItemClass");
                let (from, _) = mailbox_name(el(it, "From"));
                let start = text_of(it, "Start");
                let when = if !start.is_empty() { start } else { text_of(it, "DateTimeReceived") };
                let when = parse_ews_date(&when);
                let date = when.as_ref().map(fmt_list_date).unwrap_or_default();
                let mut preview = text_of(it, "Preview").replace(['\r', '\n'], " ");
                if calendar {
                    let loc = text_of(it, "Location");
                    let s = parse_ews_date(&text_of(it, "Start")).map(|d| fmt_long_date(&d)).unwrap_or_default();
                    preview = [s, loc].into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join(" · ");
                }
                items.push(ItemSummary {
                    id,
                    subject: text_of(it, "Subject"),
                    from,
                    date,
                    class,
                    has_attachments: text_of(it, "HasAttachments") == "true",
                    preview,
                    when,
                });
            }
        }
        Ok(ItemPage { more: !last, total, next_offset: next, items })
    }
}

pub fn parse_item_detail(text: &str, item_id: &str) -> Result<ItemDetail, String> {
        let doc = Document::parse(text).map_err(|e| e.to_string())?;
        let items = find(doc.root(), "Items").ok_or("The item couldn't be read.")?;
        let it = items.children().find(|c| c.is_element()).ok_or("The item couldn't be read.")?;
        let class = text_of(it, "ItemClass");
        let (mut from, mut from_email) = mailbox_name(el(it, "From"));
        if from.is_empty() {
            (from, from_email) = mailbox_name(el(it, "Sender").or_else(|| el(it, "Organizer")));
        }
        let mut subject = text_of(it, "Subject");
        if subject.is_empty() {
            subject = text_of(it, "DisplayName");
        }
        let sent = text_of(it, "DateTimeSent");
        let when = if sent.is_empty() { text_of(it, "DateTimeReceived") } else { sent };
        let date = date_long(&when);
        let mut fields = Vec::new();
        for (tag, label) in [("Start", "Starts"), ("End", "Ends"), ("DueDate", "Due")] {
            let v = text_of(it, tag);
            if !v.is_empty() {
                fields.push((label.to_string(), date_long(&v)));
            }
        }
        for (tag, label) in [
            ("Location", "Location"),
            ("CompanyName", "Company"),
            ("JobTitle", "Job title"),
            ("Department", "Department"),
            ("Status", "Status"),
        ] {
            let v = text_of(it, tag);
            if !v.is_empty() {
                fields.push((label.to_string(), v));
            }
        }
        for (tag, label) in [("EmailAddresses", "Email"), ("PhoneNumbers", "Phone")] {
            if let Some(list) = el(it, tag) {
                for e in els(list, "Entry") {
                    let v = e.text().unwrap_or("").trim().to_string();
                    if !v.is_empty() {
                        let key = e.attribute("Key").unwrap_or("");
                        let key = if tag == "PhoneNumbers" { key.replace("Phone", "") } else { String::new() };
                        let l = if key.is_empty() { label.to_string() } else { format!("{label} ({key})") };
                        fields.push((l, v));
                    }
                }
            }
        }
        let mut attachments = Vec::new();
        if let Some(list) = el(it, "Attachments") {
            for a in list.children().filter(|c| c.is_element()) {
                if text_of(a, "IsInline") == "true" {
                    continue;
                }
                let id = el(a, "AttachmentId").and_then(|n| n.attribute("Id")).unwrap_or_default().to_string();
                let mut name = text_of(a, "Name");
                if a.tag_name().name() == "ItemAttachment" && !name.to_lowercase().ends_with(".eml") {
                    name.push_str(".eml");
                }
                attachments.push(Attachment { id, name, size: text_of(a, "Size").parse().unwrap_or(0) });
            }
        }
        Ok(ItemDetail {
            id: item_id.to_string(),
            subject,
            from,
            from_email,
            to: recipients(el(it, "ToRecipients")),
            cc: recipients(el(it, "CcRecipients")),
            date,
            body: el(it, "Body").and_then(|b| b.text()).unwrap_or("").replace("\r\n", "\n").trim().to_string(),
            class,
            fields,
            attachments,
        })
}

/// Decode the first base64 element with one of these names.
pub fn parse_base64(text: &str, names: &[&str], missing: &str) -> Result<Vec<u8>, String> {
    let doc = Document::parse(text).map_err(|e| e.to_string())?;
    let content = names
        .iter()
        .find_map(|n| find(doc.root(), n))
        .and_then(|n| n.text())
        .ok_or_else(|| missing.to_string())?;
    B64.decode(content.split_whitespace().collect::<String>()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIND_FOLDER: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body>
<m:FindFolderResponse xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
<m:ResponseMessages><m:FindFolderResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode>
<m:RootFolder TotalItemsInView="2" IncludesLastItemInRange="true"><t:Folders>
<t:Folder><t:FolderId Id="AAB=" ChangeKey="x"/><t:FolderClass>IPF.Note</t:FolderClass><t:DisplayName>Sales</t:DisplayName><t:TotalCount>12</t:TotalCount><t:ChildFolderCount>2</t:ChildFolderCount></t:Folder>
<t:CalendarFolder><t:FolderId Id="CCD="/><t:FolderClass>IPF.Appointment</t:FolderClass><t:DisplayName>Holidays</t:DisplayName><t:TotalCount>3</t:TotalCount><t:ChildFolderCount>0</t:ChildFolderCount></t:CalendarFolder>
</t:Folders></m:RootFolder></m:FindFolderResponseMessage></m:ResponseMessages></m:FindFolderResponse></s:Body></s:Envelope>"#;

    #[test]
    fn reads_folders() {
        check(FIND_FOLDER).unwrap();
        let f = parse_folders(FIND_FOLDER).unwrap();
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].name, "Holidays");
        assert_eq!(f[0].class, "IPF.Appointment");
        assert_eq!(f[1].id, "AAB=");
        assert_eq!(f[1].child_count, 2);
        assert_eq!(f[1].total_count, 12);
    }

    #[test]
    fn reads_items() {
        let xml = r#"<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body>
<m:FindItemResponse xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
<m:ResponseMessages><m:FindItemResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode>
<m:RootFolder IndexedPagingOffset="100" TotalItemsInView="250" IncludesLastItemInRange="false"><t:Items>
<t:PostItem><t:ItemId Id="I1"/><t:Subject>Hello</t:Subject><t:DateTimeReceived>2026-11-20T08:15:00Z</t:DateTimeReceived><t:ItemClass>IPM.Post</t:ItemClass><t:HasAttachments>true</t:HasAttachments><t:Preview>First line</t:Preview><t:From><t:Mailbox><t:Name>Sam</t:Name><t:EmailAddress>sam@example.com</t:EmailAddress></t:Mailbox></t:From></t:PostItem>
<t:Message><t:ItemId Id="I2"/><t:Subject>Second</t:Subject><t:ItemClass>IPM.Note</t:ItemClass><t:HasAttachments>false</t:HasAttachments></t:Message>
</t:Items></m:RootFolder></m:FindItemResponseMessage></m:ResponseMessages></m:FindItemResponse></s:Body></s:Envelope>"#;
        let p = parse_item_page(xml, false, 0).unwrap();
        assert_eq!(p.items.len(), 2);
        assert!(p.more);
        assert_eq!(p.total, 250);
        assert_eq!(p.next_offset, 100);
        assert_eq!(p.items[0].from, "Sam");
        assert!(p.items[0].has_attachments);
        assert_eq!(p.items[0].preview, "First line");
        assert!(!p.items[0].date.is_empty());
        assert_eq!(p.items[1].subject, "Second");
    }

    #[test]
    fn reads_item_detail() {
        let xml = r#"<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body>
<m:GetItemResponse xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
<m:ResponseMessages><m:GetItemResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:Items>
<t:Message><t:ItemId Id="I1"/><t:Subject>Plan</t:Subject><t:Body BodyType="Text">Line one&#13;
Line two</t:Body>
<t:Attachments><t:FileAttachment><t:AttachmentId Id="A1"/><t:Name>plan.pdf</t:Name><t:Size>2048</t:Size><t:IsInline>false</t:IsInline></t:FileAttachment></t:Attachments>
<t:ItemClass>IPM.Note</t:ItemClass><t:DateTimeSent>2026-11-20T08:15:00Z</t:DateTimeSent>
<t:ToRecipients><t:Mailbox><t:Name>Team</t:Name><t:EmailAddress>team@example.com</t:EmailAddress></t:Mailbox></t:ToRecipients>
<t:From><t:Mailbox><t:Name>Alex</t:Name><t:EmailAddress>alex@example.com</t:EmailAddress></t:Mailbox></t:From>
</t:Message></m:Items></m:GetItemResponseMessage></m:ResponseMessages></m:GetItemResponse></s:Body></s:Envelope>"#;
        let d = parse_item_detail(xml, "I1").unwrap();
        assert_eq!(d.subject, "Plan");
        assert_eq!(d.from, "Alex");
        assert_eq!(d.from_email, "alex@example.com");
        assert_eq!(d.to, "Team <team@example.com>");
        assert_eq!(d.body, "Line one\nLine two");
        assert_eq!(d.attachments.len(), 1);
        assert_eq!(d.attachments[0].size, 2048);
    }

    #[test]
    fn reports_errors() {
        let xml = r#"<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body>
<m:FindItemResponse xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages"><m:ResponseMessages>
<m:FindItemResponseMessage ResponseClass="Error"><m:MessageText>Denied.</m:MessageText><m:ResponseCode>ErrorAccessDenied</m:ResponseCode></m:FindItemResponseMessage>
</m:ResponseMessages></m:FindItemResponse></s:Body></s:Envelope>"#;
        assert!(check(xml).unwrap_err().contains("permission"));
        let fault = r#"<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body><s:Fault><faultcode>a:ErrorInvalidServerVersion</faultcode><faultstring>The specified server version is invalid.</faultstring><detail><e:ResponseCode xmlns:e="http://schemas.microsoft.com/exchange/services/2006/errors">ErrorInvalidServerVersion</e:ResponseCode></detail></s:Fault></s:Body></s:Envelope>"#;
        assert!(check(fault).unwrap_err().contains("ServerVersion"));
    }

    #[test]
    fn decodes_base64_content() {
        let xml = r#"<r xmlns:t="x"><t:MimeContent CharacterSet="UTF-8">SGVs
bG8=</t:MimeContent></r>"#;
        assert_eq!(parse_base64(xml, &["MimeContent"], "none").unwrap(), b"Hello");
    }

    #[test]
    fn reads_autodiscover() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<Autodiscover xmlns="http://schemas.microsoft.com/exchange/autodiscover/responseschema/2006">
<Response xmlns="http://schemas.microsoft.com/exchange/autodiscover/outlook/responseschema/2006a">
<User><DisplayName>Sam</DisplayName></User>
<Account><AccountType>email</AccountType><Action>settings</Action>
<Protocol><Type>WEB</Type><EwsUrl>https://web.example.com/EWS/Exchange.asmx</EwsUrl></Protocol>
<Protocol><Type>EXCH</Type><Server>x</Server><EwsUrl>https://mail.example.com/EWS/Exchange.asmx</EwsUrl></Protocol>
<PublicFolderInformation><SmtpAddress>pf@example.com</SmtpAddress></PublicFolderInformation>
</Account></Response></Autodiscover>"#;
        assert_eq!(
            parse_pox(xml),
            Pox::Ews { ews: "https://mail.example.com/EWS/Exchange.asmx".into(), pf_mailbox: Some("pf@example.com".into()) }
        );
        let redirect = r#"<Autodiscover><Response><Account><Action>redirectAddr</Action><RedirectAddr>sam@corp.example.com</RedirectAddr></Account></Response></Autodiscover>"#;
        assert_eq!(parse_pox(redirect), Pox::RedirectAddr("sam@corp.example.com".into()));
        assert_eq!(parse_pox("<html>nope</html>"), Pox::Nothing);
    }

    #[test]
    fn builds_urls() {
        assert_eq!(
            urls_for("mail.example.com"),
            ("https://mail.example.com/EWS/Exchange.asmx".into(), "https://mail.example.com/autodiscover/autodiscover.svc".into())
        );
        assert_eq!(urls_for("https://mx.example.com/EWS/Exchange.asmx").0, "https://mx.example.com/EWS/Exchange.asmx");
        assert_eq!(urls_for("http://10.0.0.5/").0, "http://10.0.0.5/EWS/Exchange.asmx");
    }
}
