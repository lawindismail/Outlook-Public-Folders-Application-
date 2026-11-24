//! Microsoft 365 sign-in with the OAuth 2.0 device code flow.

use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub const SCOPE: &str = "https://outlook.office365.com/EWS.AccessAsUser.All offline_access";

pub struct DeviceCode {
    pub user_code: String,
    pub verification_uri: String,
    device_code: String,
    interval: u64,
    expires_in: u64,
}

#[derive(Clone)]
pub struct Token {
    pub access: String,
    pub refresh: Option<String>,
    pub expires_at: Instant,
}

fn endpoint(tenant: &str, what: &str) -> String {
    let tenant = if tenant.trim().is_empty() { "organizations" } else { tenant.trim() };
    format!("https://login.microsoftonline.com/{tenant}/oauth2/v2.0/{what}")
}

fn post_form(agent: &ureq::Agent, url: &str, form: &[(&str, &str)]) -> Result<serde_json::Value, String> {
    let resp = match agent.post(url).send_form(form) {
        Ok(r) => r,
        Err(ureq::Error::Status(_, r)) => r,
        Err(ureq::Error::Transport(t)) => return Err(format!("Could not reach Microsoft sign-in: {t}")),
    };
    let mut text = String::new();
    resp.into_reader().read_to_string(&mut text).map_err(|e| e.to_string())?;
    serde_json::from_str(&text).map_err(|_| "Microsoft sign-in sent an unreadable answer.".to_string())
}

fn str_of(v: &serde_json::Value, k: &str) -> String {
    v.get(k).and_then(|x| x.as_str()).unwrap_or_default().to_string()
}

pub fn start(agent: &ureq::Agent, tenant: &str, client_id: &str) -> Result<DeviceCode, String> {
    let v = post_form(agent, &endpoint(tenant, "devicecode"), &[("client_id", client_id), ("scope", SCOPE)])?;
    if let Some(desc) = v.get("error_description").and_then(|x| x.as_str()) {
        return Err(format!("Microsoft sign-in refused: {}", desc.lines().next().unwrap_or(desc)));
    }
    Ok(DeviceCode {
        user_code: str_of(&v, "user_code"),
        verification_uri: {
            let u = str_of(&v, "verification_uri");
            if u.is_empty() { "https://microsoft.com/devicelogin".into() } else { u }
        },
        device_code: str_of(&v, "device_code"),
        interval: v.get("interval").and_then(|x| x.as_u64()).unwrap_or(5),
        expires_in: v.get("expires_in").and_then(|x| x.as_u64()).unwrap_or(900),
    })
}

fn token_from(v: &serde_json::Value) -> Token {
    Token {
        access: str_of(v, "access_token"),
        refresh: v.get("refresh_token").and_then(|x| x.as_str()).map(str::to_string),
        expires_at: Instant::now() + Duration::from_secs(v.get("expires_in").and_then(|x| x.as_u64()).unwrap_or(3600)),
    }
}

pub fn poll(agent: &ureq::Agent, tenant: &str, client_id: &str, dc: &DeviceCode, cancel: &AtomicBool) -> Result<Token, String> {
    let deadline = Instant::now() + Duration::from_secs(dc.expires_in);
    let mut interval = dc.interval.max(1);
    loop {
        for _ in 0..interval * 4 {
            if cancel.load(Ordering::Relaxed) {
                return Err("Sign-in cancelled.".into());
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        if Instant::now() > deadline {
            return Err("The sign-in code expired. Try again.".into());
        }
        let v = post_form(
            agent,
            &endpoint(tenant, "token"),
            &[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("client_id", client_id),
                ("device_code", &dc.device_code),
            ],
        )?;
        match v.get("error").and_then(|x| x.as_str()) {
            None => return Ok(token_from(&v)),
            Some("authorization_pending") => {}
            Some("slow_down") => interval += 5,
            Some(_) => {
                let d = str_of(&v, "error_description");
                return Err(format!("Microsoft sign-in failed: {}", d.lines().next().unwrap_or(&d)));
            }
        }
    }
}

pub fn refresh(agent: &ureq::Agent, tenant: &str, client_id: &str, refresh_token: &str) -> Result<Token, String> {
    let v = post_form(
        agent,
        &endpoint(tenant, "token"),
        &[
            ("grant_type", "refresh_token"),
            ("client_id", client_id),
            ("refresh_token", refresh_token),
            ("scope", SCOPE),
        ],
    )?;
    if v.get("access_token").is_none() {
        return Err("Your Microsoft sign-in has expired. Disconnect and sign in again.".into());
    }
    let mut t = token_from(&v);
    if t.refresh.is_none() {
        t.refresh = Some(refresh_token.to_string());
    }
    Ok(t)
}
