//! The window: sign-in card, folder tree, item list and reading pane.

use crate::cache::Cached;
use crate::demo::Demo;
use crate::ews::{ConnectConfig, EwsClient, SignIn};
use crate::model::*;
use crate::oauth;
use crate::settings::{Favorite, Mode, Settings};
use eframe::egui::{
    self, text::LayoutJob, Align, Align2, Color32, FontId, Galley, Layout, Margin, Pos2, Rect, RichText, Rounding, Sense,
    Shape, Stroke, TextFormat, Ui, Vec2,
};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

const ACCENT: Color32 = Color32::from_rgb(15, 108, 189);
const ACCENT_DARK: Color32 = Color32::from_rgb(17, 94, 163);
const ACCENT_LIGHT: Color32 = Color32::from_rgb(214, 232, 250);
const BG: Color32 = Color32::from_rgb(240, 242, 245);
const FOLDER_BG: Color32 = Color32::from_rgb(245, 246, 248);
const WHITE: Color32 = Color32::WHITE;
const TEXT: Color32 = Color32::from_rgb(36, 36, 36);
const WEAK: Color32 = Color32::from_rgb(105, 105, 110);
const BORDER: Color32 = Color32::from_rgb(222, 224, 228);
const HOVER: Color32 = Color32::from_rgb(234, 240, 247);
const DANGER: Color32 = Color32::from_rgb(196, 43, 28);

enum Msg {
    DeviceCode { code: String, url: String },
    /// The connection, the top-level folders and (not for the demo) the saved copy.
    Connected(Result<(Arc<dyn Backend>, Vec<FolderInfo>, Option<Arc<Cached>>), String>),
    Children { folder_id: String, result: Result<Vec<FolderInfo>, String> },
    Items { folder_id: String, offset: u32, result: Result<ItemPage, String> },
    Detail { item_id: String, result: Result<ItemDetail, String> },
    Saved(Result<String, String>),
    Uploaded { folder_id: String, folder_name: String, added: usize, problems: Vec<String> },
}

struct Node {
    info: FolderInfo,
    depth: usize,
    children: Option<Vec<usize>>,
    expanded: bool,
    loading: bool,
    error: Option<String>,
}

#[derive(PartialEq)]
enum Screen {
    Connect,
    Browse,
}

struct Shot {
    path: PathBuf,
    frames: u32,
    requested: bool,
}

enum Act {
    Toggle(usize),
    Select(FolderInfo),
    OpenItem(String),
    LoadMore,
    Refresh,
    ToggleFavorite,
    SaveAttachment(Attachment),
    SaveEml,
    Disconnect,
    /// Choose files to add to this folder.
    UploadTo(FolderInfo),
    /// Export one item (true = with its attachments as separate files).
    ExportItem(String, bool),
    /// Open the export window for just this folder.
    ExportFolder(FolderInfo),
}

pub struct App {
    ctx: egui::Context,
    settings: Settings,
    screen: Screen,
    password: String,
    connecting: bool,
    connect_error: Option<String>,
    device: Option<(String, String)>,
    cancel: Arc<AtomicBool>,
    backend: Option<Arc<dyn Backend>>,
    account_key: String,
    demo_mode: bool,
    nodes: Vec<Node>,
    selected: Option<FolderInfo>,
    items: Vec<ItemSummary>,
    items_total: u32,
    items_more: bool,
    items_next: u32,
    items_loading: bool,
    items_error: Option<String>,
    filter: String,
    selected_item: Option<String>,
    detail: Option<ItemDetail>,
    detail_loading: bool,
    detail_error: Option<String>,
    status: String,
    status_error: bool,
    pending: usize,
    generation: u64,
    tx: Sender<(u64, Msg)>,
    rx: Receiver<(u64, Msg)>,
    shot: Option<Shot>,
    demo_autoselect: Option<String>,
    export_open: bool,
    export_dest: PathBuf,
    export_attachments: bool,
    export_status: Arc<std::sync::Mutex<crate::export::ExportStatus>>,
    export_cancel: Arc<AtomicBool>,
    /// The saved copy of the public folders (not in the demo).
    cache: Option<Arc<Cached>>,
    folder_search: String,
    tree_version: u64,
    /// Every saved folder (lower-case name, folder) for the search box.
    folder_index: Vec<(String, crate::cache::FolderPath)>,
    folder_index_version: Option<u64>,
    /// Export only this folder (right-click → Export this folder).
    export_scope: Option<FolderInfo>,
    /// Files waiting for "Add to folder?" confirmation.
    pending_upload: Option<(FolderInfo, Vec<PathBuf>)>,
    /// Where each folder row was drawn this frame (for dropping on it).
    folder_rows: Vec<(Rect, FolderInfo)>,
    #[cfg(windows)]
    drop_state: Option<Arc<std::sync::Mutex<crate::dnd::DropState>>>,
    dnd_ready: bool,
    /// Scroll the folder list to the selected folder (after a search pick).
    scroll_to_selected: bool,
}

fn setup_style(ctx: &egui::Context) {
    ctx.set_theme(egui::ThemePreference::Light);
    let mut v = egui::Visuals::light();
    v.panel_fill = WHITE;
    v.window_fill = WHITE;
    v.extreme_bg_color = WHITE;
    v.selection.bg_fill = ACCENT_LIGHT;
    v.selection.stroke = Stroke::new(1.0_f32, ACCENT);
    v.hyperlink_color = ACCENT;
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, TEXT);
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, BORDER);
    for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
        w.rounding = Rounding::same(5.0);
    }
    v.widgets.inactive.bg_fill = Color32::from_rgb(245, 246, 248);
    v.widgets.inactive.weak_bg_fill = Color32::from_rgb(245, 246, 248);
    v.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, BORDER);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, TEXT);
    v.widgets.hovered.weak_bg_fill = HOVER;
    v.widgets.hovered.bg_fill = HOVER;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(190, 200, 214));
    ctx.set_visuals(v);
    ctx.style_mut(|s| {
        use egui::TextStyle::*;
        s.text_styles.insert(Heading, FontId::proportional(20.0));
        s.text_styles.insert(Body, FontId::proportional(14.0));
        s.text_styles.insert(Button, FontId::proportional(14.0));
        s.text_styles.insert(Small, FontId::proportional(11.5));
        s.text_styles.insert(Monospace, FontId::monospace(13.0));
        s.spacing.item_spacing = egui::vec2(8.0, 6.0);
        s.spacing.button_padding = egui::vec2(10.0, 5.0);
        s.spacing.interact_size.y = 26.0;
    });
}

fn one_line(ui: &Ui, text: &str, size: f32, color: Color32, max_w: f32) -> Arc<Galley> {
    let mut job = LayoutJob::single_section(
        text.to_owned(),
        TextFormat { font_id: FontId::proportional(size), color, ..Default::default() },
    );
    job.wrap.max_width = max_w.max(8.0);
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    job.wrap.overflow_character = Some('…');
    ui.fonts(|f| f.layout_job(job))
}

fn initials(name: &str) -> String {
    let words: Vec<&str> = name.split(|c: char| c.is_whitespace() || c == '.' || c == '@').filter(|w| !w.is_empty()).collect();
    let mut s: String = words.iter().take(2).filter_map(|w| w.chars().next()).collect();
    if s.is_empty() {
        s.push('?');
    }
    s.to_uppercase()
}

fn avatar_color(name: &str) -> Color32 {
    const PALETTE: [Color32; 8] = [
        Color32::from_rgb(0, 120, 212),
        Color32::from_rgb(135, 100, 184),
        Color32::from_rgb(0, 138, 125),
        Color32::from_rgb(202, 80, 16),
        Color32::from_rgb(196, 49, 75),
        Color32::from_rgb(73, 130, 5),
        Color32::from_rgb(3, 131, 135),
        Color32::from_rgb(142, 86, 46),
    ];
    let h = name.bytes().fold(7u32, |a, b| a.wrapping_mul(31).wrapping_add(b as u32));
    PALETTE[(h % PALETTE.len() as u32) as usize]
}

fn paint_avatar(ui: &Ui, center: Pos2, radius: f32, name: &str, icon: Option<Icon>) {
    let p = ui.painter();
    p.circle_filled(center, radius, avatar_color(name));
    match icon {
        Some(i) => paint_icon(p, Rect::from_center_size(center, Vec2::splat(radius * 1.1)), i, WHITE),
        None => {
            p.text(center, Align2::CENTER_CENTER, initials(name), FontId::proportional(radius * 0.8), WHITE);
        }
    }
}

fn paint_logo(ui: &mut Ui, size: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, Rounding::same(size * 0.22), ACCENT);
    // A simple folder shape.
    let r = rect.shrink(size * 0.22);
    let tab = Rect::from_min_size(r.min + egui::vec2(0.0, r.height() * 0.08), egui::vec2(r.width() * 0.45, r.height() * 0.25));
    p.rect_filled(tab, Rounding::same(size * 0.05), Color32::from_rgb(190, 222, 250));
    let body = Rect::from_min_max(r.min + egui::vec2(0.0, r.height() * 0.22), r.max - egui::vec2(0.0, r.height() * 0.05));
    p.rect_filled(body, Rounding::same(size * 0.06), WHITE);
}

#[derive(Clone, Copy, PartialEq)]
enum Icon {
    Folder,
    Calendar,
    Contact,
    Mail,
    Star,
}

fn folder_icon(class: &str) -> Icon {
    if class.starts_with("IPF.Appointment") {
        Icon::Calendar
    } else if class.starts_with("IPF.Contact") {
        Icon::Contact
    } else {
        Icon::Folder
    }
}

/// Draw a small icon inside `r` (a square).
fn paint_icon(p: &egui::Painter, r: Rect, icon: Icon, color: Color32) {
    let s = r.width();
    let stroke = Stroke::new((s * 0.09).max(1.2), color);
    match icon {
        Icon::Folder => {
            let tab = Rect::from_min_size(r.min + egui::vec2(s * 0.08, s * 0.18), egui::vec2(s * 0.38, s * 0.2));
            p.rect_filled(tab, Rounding::same(s * 0.06), color);
            let body = Rect::from_min_max(r.min + egui::vec2(s * 0.08, s * 0.3), r.max - egui::vec2(s * 0.08, s * 0.16));
            p.rect_filled(body, Rounding::same(s * 0.08), color);
        }
        Icon::Calendar => {
            let body = Rect::from_min_max(r.min + egui::vec2(s * 0.12, s * 0.18), r.max - egui::vec2(s * 0.12, s * 0.1));
            p.rect_stroke(body, Rounding::same(s * 0.08), stroke);
            let bar = Rect::from_min_max(body.min, egui::pos2(body.max.x, body.min.y + s * 0.2));
            p.rect_filled(bar, Rounding { nw: s * 0.08, ne: s * 0.08, sw: 0.0, se: 0.0 }, color);
            for i in 0..3 {
                let x = body.min.x + body.width() * (0.25 + 0.25 * i as f32);
                p.circle_filled(egui::pos2(x, body.max.y - body.height() * 0.3), s * 0.05, color);
            }
        }
        Icon::Contact => {
            p.circle_filled(egui::pos2(r.center().x, r.min.y + s * 0.34), s * 0.17, color);
            let body = Rect::from_min_max(egui::pos2(r.min.x + s * 0.18, r.min.y + s * 0.58), egui::pos2(r.max.x - s * 0.18, r.max.y - s * 0.1));
            p.rect_filled(body, Rounding { nw: s * 0.3, ne: s * 0.3, sw: s * 0.05, se: s * 0.05 }, color);
        }
        Icon::Mail => {
            let body = Rect::from_min_max(r.min + egui::vec2(s * 0.08, s * 0.22), r.max - egui::vec2(s * 0.08, s * 0.2));
            p.rect_stroke(body, Rounding::same(s * 0.06), stroke);
            p.line_segment([body.left_top(), egui::pos2(body.center().x, body.center().y + s * 0.04)], stroke);
            p.line_segment([body.right_top(), egui::pos2(body.center().x, body.center().y + s * 0.04)], stroke);
        }
        Icon::Star => {
            p.text(r.center(), Align2::CENTER_CENTER, "★", FontId::proportional(s * 0.95), color);
        }
    }
}

fn section_label(ui: &mut Ui, text: &str) {
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        ui.label(RichText::new(text).size(11.0).color(WEAK).strong());
    });
    ui.add_space(2.0);
}

fn primary_button(text: &str, width: f32) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.to_string()).color(WHITE).size(15.0))
        .fill(ACCENT)
        .stroke(Stroke::NONE)
        .min_size(egui::vec2(width, 36.0))
}

fn open_in_browser(ctx: &egui::Context, url: &str) {
    ctx.open_url(egui::OpenUrl::new_tab(url));
}

fn safe_file_name(s: &str) -> String {
    let cleaned: String = s.chars().map(|c| if "\\/:*?\"<>|".contains(c) || c.is_control() { '_' } else { c }).collect();
    let t = cleaned.trim().trim_end_matches('.');
    if t.is_empty() { "message".into() } else { t.chars().take(80).collect() }
}

impl App {
    /// `demo`: start in the demo, opening this folder.
    pub fn new(cc: &eframe::CreationContext<'_>, shot: Option<PathBuf>, demo: Option<String>) -> Self {
        setup_style(&cc.egui_ctx);
        let (tx, rx) = channel();
        let mut app = Self {
            ctx: cc.egui_ctx.clone(),
            settings: Settings::load(),
            screen: Screen::Connect,
            password: String::new(),
            connecting: false,
            connect_error: None,
            device: None,
            cancel: Arc::new(AtomicBool::new(false)),
            backend: None,
            account_key: String::new(),
            demo_mode: false,
            nodes: Vec::new(),
            selected: None,
            items: Vec::new(),
            items_total: 0,
            items_more: false,
            items_next: 0,
            items_loading: false,
            items_error: None,
            filter: String::new(),
            selected_item: None,
            detail: None,
            detail_loading: false,
            detail_error: None,
            status: String::new(),
            status_error: false,
            pending: 0,
            generation: 0,
            tx,
            rx,
            shot: shot.map(|path| Shot { path, frames: 0, requested: false }),
            demo_autoselect: demo,
            // `--show-export` opens the export window at start (for checking its look).
            export_open: std::env::args().any(|a| a == "--show-export"),
            export_dest: crate::export::default_destination(),
            export_attachments: true,
            export_status: Default::default(),
            export_cancel: Arc::new(AtomicBool::new(false)),
            cache: None,
            folder_search: String::new(),
            tree_version: 0,
            folder_index: Vec::new(),
            folder_index_version: None,
            export_scope: None,
            pending_upload: None,
            folder_rows: Vec::new(),
            #[cfg(windows)]
            drop_state: None,
            dnd_ready: false,
            scroll_to_selected: false,
        };
        if app.settings.email.trim().is_empty() && app.shot.is_none() {
            // On a company PC, Windows knows the person's work address.
            if let Some(upn) = crate::winauth::current_upn() {
                app.settings.email = upn;
            }
        }
        if app.demo_autoselect.is_some() {
            app.start_demo();
        }
        app
    }

    fn spawn(&mut self, f: impl FnOnce() -> Msg + Send + 'static) {
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        let gen = self.generation;
        self.pending += 1;
        std::thread::spawn(move || {
            let m = f();
            let _ = tx.send((gen, m));
            ctx.request_repaint();
        });
    }

    fn reset_browse(&mut self) {
        self.generation += 1;
        self.pending = 0;
        self.nodes.clear();
        self.selected = None;
        self.items.clear();
        self.items_error = None;
        self.items_loading = false;
        self.selected_item = None;
        self.detail = None;
        self.detail_error = None;
        self.detail_loading = false;
        self.filter.clear();
        self.status.clear();
        self.status_error = false;
    }

    fn start_demo(&mut self) {
        self.reset_browse();
        self.demo_mode = true;
        self.account_key = "demo".into();
        let demo: Arc<dyn Backend> = Arc::new(Demo::new());
        self.connecting = true;
        self.spawn(move || {
            let r = demo.root_folders().map(|f| (demo.clone(), f, None));
            Msg::Connected(r)
        });
    }

    /// Sign in with this PC's Windows login (company server only).
    fn use_windows(&self) -> bool {
        cfg!(windows) && self.settings.use_windows.unwrap_or_else(crate::winauth::on_company_domain)
    }

    fn start_connect(&mut self) {
        if self.settings.mode == Mode::Classic {
            self.reset_browse();
            self.demo_mode = false;
            self.account_key = "classic-outlook".into();
            self.connecting = true;
            self.connect_error = None;
            self.device = None;
            let key = self.account_key.clone();
            self.spawn(move || {
                Msg::Connected(crate::classic::connect().and_then(|b| {
                    // The saved copy answers at once if this PC has seen the folders before.
                    let cached = Cached::open(b, &key);
                    let root = cached.root_folders()?;
                    let backend: Arc<dyn Backend> = cached.clone();
                    Ok((backend, root, Some(cached)))
                }))
            });
            return;
        }
        let s = &self.settings;
        let m365 = s.mode == Mode::Microsoft365;
        let use_windows = self.use_windows();
        let missing = if m365 {
            if s.email.trim().is_empty() {
                Some("Enter your email address.")
            } else if s.client_id.trim().is_empty() {
                Some("Enter the app (client) ID from your IT admin. See “Where do I find these details?” below.")
            } else {
                None
            }
        } else if s.email.trim().is_empty() && s.server.trim().is_empty() {
            Some("Enter your work email address — the one you use in Outlook.")
        } else if s.email.trim().is_empty() && s.username.trim().is_empty() && !use_windows {
            Some("Enter your work email address — the one you use in Outlook.")
        } else if !use_windows && self.password.is_empty() {
            Some("Enter your password — the one you use for Outlook or to sign in to your work computer.")
        } else {
            None
        };
        if let Some(m) = missing {
            self.connect_error = Some(m.into());
            return;
        }
        let cfg = ConnectConfig {
            server: if m365 { s.m365_server.clone() } else { s.server.clone() },
            email: s.email.trim().to_string(),
            username: s.username.trim().to_string(),
            password: self.password.clone(),
            sign_in: if m365 {
                SignIn::Microsoft
            } else if use_windows {
                SignIn::Windows
            } else {
                match s.sign_in.as_str() {
                    "ntlm" => SignIn::Ntlm,
                    "basic" => SignIn::Basic,
                    _ => SignIn::Auto,
                }
            },
            ignore_cert_errors: s.ignore_cert_errors,
            version: s.version.clone(),
            tenant: s.tenant.clone(),
            client_id: s.client_id.trim().to_string(),
        };
        self.reset_browse();
        self.demo_mode = false;
        self.account_key = format!(
            "{}|{}|{}",
            if m365 { "m365" } else { "server" },
            cfg.server.to_lowercase(),
            if cfg.email.is_empty() { cfg.username.to_lowercase() } else { cfg.email.to_lowercase() }
        );
        self.connecting = true;
        self.connect_error = None;
        self.device = None;
        self.cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.cancel.clone();
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        let gen = self.generation;
        let key = self.account_key.clone();
        self.spawn(move || {
            let r = (|| {
                let client = EwsClient::new(cfg.clone())?;
                if cfg.sign_in == SignIn::Microsoft {
                    let dc = oauth::start(client.agent(), &cfg.tenant, &cfg.client_id)?;
                    let _ = tx.send((gen, Msg::DeviceCode { code: dc.user_code.clone(), url: dc.verification_uri.clone() }));
                    ctx.request_repaint();
                    let token = oauth::poll(client.agent(), &cfg.tenant, &cfg.client_id, &dc, &cancel)?;
                    client.set_token(token);
                }
                client.discover_server()?;
                client.discover_hierarchy();
                let cached = Cached::open(Arc::new(client), &key);
                let root = cached.root_folders()?;
                let backend: Arc<dyn Backend> = cached.clone();
                Ok((backend, root, Some(cached)))
            })();
            Msg::Connected(r)
        });
    }

    /// Open a demo folder (expanding its parent) and its first item.
    fn demo_open(&mut self, folder_id: &str) {
        let Some(b) = self.backend.clone() else { return };
        if self.node_for(folder_id).is_none() {
            let parents: Vec<usize> = self.nodes[0].children.clone().unwrap_or_default();
            for p in parents {
                let kids = b.child_folders(&self.nodes[p].info.id).unwrap_or_default();
                if kids.iter().any(|k| k.id == folder_id) {
                    self.set_children(p, kids);
                    self.nodes[p].expanded = true;
                    break;
                }
            }
        }
        if let Some(i) = self.node_for(folder_id) {
            let info = self.nodes[i].info.clone();
            self.select_folder(info);
            self.selected_item = Some(format!("{folder_id}:0"));
        }
    }

    fn node_for(&self, folder_id: &str) -> Option<usize> {
        self.nodes.iter().position(|n| n.info.id == folder_id)
    }

    fn set_children(&mut self, idx: usize, list: Vec<FolderInfo>) {
        let depth = self.nodes[idx].depth + 1;
        let mut ids = Vec::new();
        for info in list {
            let leaf = info.child_count == 0;
            ids.push(self.nodes.len());
            self.nodes.push(Node {
                info,
                depth,
                children: if leaf { Some(Vec::new()) } else { None },
                expanded: false,
                loading: false,
                error: None,
            });
        }
        let n = &mut self.nodes[idx];
        n.info.child_count = ids.len() as u32;
        n.children = Some(ids);
        n.loading = false;
        n.error = None;
    }

    fn toggle(&mut self, idx: usize) {
        let n = &mut self.nodes[idx];
        if n.expanded {
            n.expanded = false;
            return;
        }
        n.expanded = true;
        let id = n.info.id.clone();
        if n.children.is_none() {
            // Saved copy: instant.
            if let Some(kids) = self.cache.as_ref().and_then(|c| c.saved_children(&id)) {
                self.set_children(idx, kids);
                self.save_tree_state();
                return;
            }
        }
        let n = &mut self.nodes[idx];
        if n.children.is_none() && !n.loading {
            n.loading = true;
            if let Some(b) = self.backend.clone() {
                self.spawn(move || Msg::Children { result: b.child_folders(&id), folder_id: id });
            }
        }
    }

    /// Remember which folders are open and which is selected (per account).
    fn save_tree_state(&mut self) {
        if self.demo_mode || self.account_key.is_empty() || self.nodes.is_empty() {
            return;
        }
        let expanded = self.nodes.iter().skip(1).filter(|n| n.expanded).map(|n| n.info.id.clone()).collect();
        self.settings.tree.insert(self.account_key.clone(), crate::settings::TreeState { expanded, selected: self.selected.clone() });
    }

    /// Open these folders using the saved copy (no waiting).
    fn expand_from_cache(&mut self, ids: &std::collections::HashSet<String>) {
        let Some(cache) = self.cache.clone() else { return };
        let mut i = 1;
        while i < self.nodes.len() {
            let id = self.nodes[i].info.id.clone();
            if ids.contains(&id) {
                if self.nodes[i].children.is_none() {
                    if let Some(kids) = cache.saved_children(&id) {
                        self.set_children(i, kids);
                    }
                }
                if self.nodes[i].children.is_some() {
                    self.nodes[i].expanded = true;
                }
            }
            i += 1;
        }
    }

    /// The saved folder tree changed (background mapping): redraw it,
    /// keeping the same folders open.
    fn rebuild_tree_from_cache(&mut self) {
        let Some(root) = self.cache.as_ref().and_then(|c| c.saved_children("")) else { return };
        let open: std::collections::HashSet<String> = self.nodes.iter().skip(1).filter(|n| n.expanded).map(|n| n.info.id.clone()).collect();
        self.nodes.truncate(1);
        self.nodes[0].children = None;
        self.nodes[0].expanded = true;
        self.set_children(0, root);
        self.expand_from_cache(&open);
    }

    /// Open the folders above this one and select it.
    fn reveal(&mut self, fp: crate::cache::FolderPath) {
        let ancestors: std::collections::HashSet<String> = fp.ids[..fp.ids.len().saturating_sub(1)].iter().cloned().collect();
        self.expand_from_cache(&ancestors);
        self.folder_search.clear();
        self.select_folder(fp.folder);
        // Show it in the folder list: scroll there on the next draw.
        self.scroll_to_selected = true;
    }

    fn select_folder(&mut self, info: FolderInfo) {
        self.selected = Some(info);
        self.save_tree_state();
        self.items.clear();
        self.items_total = 0;
        self.items_more = false;
        self.items_next = 0;
        self.items_error = None;
        self.filter.clear();
        self.selected_item = None;
        self.detail = None;
        self.detail_error = None;
        self.detail_loading = false;
        self.load_items(0);
    }

    fn load_items(&mut self, offset: u32) {
        let (Some(folder), Some(b)) = (self.selected.clone(), self.backend.clone()) else { return };
        if folder.id.is_empty() {
            return;
        }
        self.items_loading = true;
        self.spawn(move || Msg::Items { result: b.items(&folder, offset), folder_id: folder.id, offset });
    }

    fn open_item(&mut self, id: String) {
        let (Some(folder), Some(b)) = (self.selected.clone(), self.backend.clone()) else { return };
        self.selected_item = Some(id.clone());
        self.detail = None;
        self.detail_error = None;
        self.detail_loading = true;
        self.spawn(move || Msg::Detail { result: b.item(&folder.id, &id), item_id: id });
    }

    fn favorites(&self) -> Vec<Favorite> {
        self.settings.favorites.get(&self.account_key).cloned().unwrap_or_default()
    }

    fn toggle_favorite(&mut self) {
        let Some(f) = self.selected.clone() else { return };
        let list = self.settings.favorites.entry(self.account_key.clone()).or_default();
        if let Some(i) = list.iter().position(|x| x.id == f.id) {
            list.remove(i);
        } else {
            list.push(Favorite { id: f.id, name: f.name, class: f.class });
        }
        if !self.demo_mode {
            self.settings.save();
        }
    }

    fn save_attachment(&mut self, att: Attachment) {
        let (Some(folder), Some(b)) = (self.selected.clone(), self.backend.clone()) else { return };
        let Some(path) = rfd::FileDialog::new().set_file_name(safe_file_name(&att.name)).save_file() else { return };
        self.status = format!("Saving {}…", att.name);
        self.status_error = false;
        self.spawn(move || {
            Msg::Saved(
                b.attachment(&folder.id, &att.id)
                    .and_then(|bytes| std::fs::write(&path, bytes).map_err(|e| format!("Couldn't save the file: {e}")))
                    .map(|_| path.display().to_string()),
            )
        });
    }

    fn save_eml(&mut self) {
        let (Some(folder), Some(b), Some(d)) = (self.selected.clone(), self.backend.clone(), self.detail.clone()) else { return };
        let ext = b.message_ext();
        let name = format!("{}.{ext}", safe_file_name(&d.subject));
        let Some(path) = rfd::FileDialog::new().set_file_name(name).add_filter("Email message", &[ext]).save_file() else {
            return;
        };
        self.status = "Saving message…".into();
        self.status_error = false;
        self.spawn(move || {
            Msg::Saved(
                b.mime(&folder.id, &d.id)
                    .and_then(|bytes| std::fs::write(&path, bytes).map_err(|e| format!("Couldn't save the file: {e}")))
                    .map(|_| path.display().to_string()),
            )
        });
    }

    fn refresh(&mut self) {
        // Reload the folder list and the open folder.
        let Some(b) = self.backend.clone() else { return };
        b.clear_cache();
        // Ask the server again for the top level and the open folder, and
        // re-check everything else in the background.
        b.forget("");
        if let Some(f) = &self.selected {
            b.forget(&f.id);
        }
        if let Some(c) = &self.cache {
            c.start_mapping(false);
        }
        if !self.nodes.is_empty() {
            self.nodes.truncate(1);
            self.nodes[0].children = None;
            self.nodes[0].loading = true;
            self.nodes[0].expanded = true;
            self.spawn(move || Msg::Children { result: b.root_folders(), folder_id: String::new() });
        }
        if let Some(f) = self.selected.clone() {
            let keep = self.selected_item.clone();
            self.select_folder(f);
            if let Some(id) = keep {
                self.open_item(id);
            }
        }
    }

    fn disconnect(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        self.export_cancel.store(true, Ordering::Relaxed);
        self.export_open = false;
        self.save_tree_state();
        if !self.demo_mode {
            self.settings.save();
        }
        if let Some(c) = self.cache.take() {
            c.stop();
        }
        self.reset_browse();
        self.backend = None;
        self.connecting = false;
        self.device = None;
        self.screen = Screen::Connect;
    }

    fn handle_messages(&mut self) {
        while let Ok((gen, msg)) = self.rx.try_recv() {
            if gen != self.generation {
                continue;
            }
            if !matches!(msg, Msg::DeviceCode { .. }) {
                self.pending = self.pending.saturating_sub(1);
            }
            match msg {
                Msg::DeviceCode { code, url } => self.device = Some((code, url)),
                Msg::Connected(Ok((backend, root, cache))) => {
                    self.connecting = false;
                    self.device = None;
                    self.status = format!("Connected — {}", backend.label());
                    self.status_error = false;
                    self.backend = Some(backend);
                    if let Some(old) = self.cache.take() {
                        old.stop();
                    }
                    self.cache = cache;
                    self.nodes = vec![Node {
                        info: FolderInfo { id: String::new(), name: "All Public Folders".into(), ..Default::default() },
                        depth: 0,
                        children: None,
                        expanded: true,
                        loading: false,
                        error: None,
                    }];
                    self.set_children(0, root);
                    self.screen = Screen::Browse;
                    if !self.demo_mode {
                        self.password.clear();
                        self.settings.save();
                    }
                    if let Some(target) = self.demo_autoselect.take() {
                        self.demo_open(&target);
                    }
                    if let Some(c) = self.cache.clone() {
                        // Bring back the folders that were open last time, then
                        // map everything in the background.
                        self.tree_version = c.status().tree_version;
                        if let Some(state) = self.settings.tree.get(&self.account_key).cloned() {
                            self.expand_from_cache(&state.expanded.into_iter().collect());
                            if let Some(sel) = state.selected {
                                self.select_folder(sel);
                            }
                        }
                        // A full map keeps Outlook busy for minutes, so only redo it
                        // when the saved copy is old (Refresh redoes it any time).
                        let stale = c.status().mapped_at.map(|t| chrono::Utc::now().timestamp() - t > 6 * 3600).unwrap_or(true);
                        // The tree is cheap with MAPI, so always refresh it; item lists only when stale.
                        if self.shot.is_none() {
                            c.start_mapping(stale);
                        }
                    }
                }
                Msg::Connected(Err(e)) => {
                    self.connecting = false;
                    self.device = None;
                    self.connect_error = Some(e);
                }
                Msg::Children { folder_id, result } => {
                    if let Some(idx) = self.node_for(&folder_id) {
                        match result {
                            Ok(list) => self.set_children(idx, list),
                            Err(e) => {
                                self.nodes[idx].loading = false;
                                self.nodes[idx].expanded = false;
                                self.nodes[idx].error = Some(e.clone());
                                self.status = format!("Couldn't open “{}”: {e}", self.nodes[idx].info.name);
                                self.status_error = true;
                            }
                        }
                    }
                }
                Msg::Items { folder_id, offset, result } => {
                    if self.selected.as_ref().map(|f| f.id != folder_id).unwrap_or(true) {
                        continue;
                    }
                    self.items_loading = false;
                    match result {
                        Ok(page) => {
                            if offset == 0 {
                                self.items.clear();
                            }
                            self.items.extend(page.items);
                            self.items_total = page.total.max(self.items.len() as u32);
                            self.items_more = page.more;
                            self.items_next = page.next_offset;
                            self.items_error = None;
                            if let Some(id) = self.selected_item.clone() {
                                if self.detail.is_none() && !self.detail_loading && self.items.iter().any(|i| i.id == id) {
                                    self.open_item(id);
                                }
                            }
                        }
                        Err(e) => self.items_error = Some(e),
                    }
                }
                Msg::Detail { item_id, result } => {
                    if self.selected_item.as_deref() != Some(item_id.as_str()) {
                        continue;
                    }
                    self.detail_loading = false;
                    match result {
                        Ok(d) => self.detail = Some(d),
                        Err(e) => self.detail_error = Some(e),
                    }
                }
                Msg::Saved(r) => match r {
                    Ok(p) => {
                        self.status = format!("Saved to {p}");
                        self.status_error = false;
                    }
                    Err(e) => {
                        self.status = e;
                        self.status_error = true;
                    }
                },
                Msg::Uploaded { folder_id, folder_name, added, problems } => {
                    self.status = match (added, problems.len()) {
                        (n, 0) => format!("Added {n} item{} to “{folder_name}”", if n == 1 { "" } else { "s" }),
                        (0, _) => format!("Nothing was added to “{folder_name}”: {}", problems.join("; ")),
                        (n, _) => format!("Added {n} to “{folder_name}”; not added: {}", problems.join("; ")),
                    };
                    self.status_error = !problems.is_empty();
                    // Show the new items if that folder is open.
                    if self.selected.as_ref().map(|f| f.id == folder_id).unwrap_or(false) {
                        if let Some(f) = self.selected.clone() {
                            self.select_folder(f);
                        }
                    }
                }
            }
        }
    }

    // ---------------------------------------------------------------- sign-in

    fn ui_connect(&mut self, ctx: &egui::Context) {
        let mut go = false;
        let mut demo = false;
        let mut cancel = false;
        egui::CentralPanel::default().frame(egui::Frame::none().fill(BG)).show(ctx, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
                let card_w = 480.0;
                let pad = ((ui.available_width() - card_w) / 2.0).max(12.0);
                ui.add_space(((ui.available_height() - 640.0) / 2.0).clamp(16.0, 80.0));
                ui.horizontal(|ui| {
                    ui.add_space(pad);
                    egui::Frame::none()
                        .fill(WHITE)
                        .stroke(Stroke::new(1.0_f32, BORDER))
                        .rounding(Rounding::same(12.0))
                        .shadow(egui::epaint::Shadow {
                            offset: egui::vec2(0.0, 4.0),
                            blur: 18.0,
                            spread: 0.0,
                            color: Color32::from_black_alpha(22),
                        })
                        .inner_margin(Margin::same(30.0))
                        .show(ui, |ui| {
                            ui.set_width(card_w - 60.0);
                            ui.vertical(|ui| self.connect_card(ui, &mut go, &mut demo, &mut cancel));
                        });
                });
                ui.add_space(24.0);
            });
        });
        if self.shot.is_some() {
            // A picture-taking run must never start a connection, even if a
            // stray click or key press reaches its window.
            go = false;
            demo = false;
        }
        if go {
            self.start_connect();
        }
        if demo {
            self.start_demo();
        }
        if cancel {
            self.cancel.store(true, Ordering::Relaxed);
            self.generation += 1;
            self.pending = 0;
            self.connecting = false;
            self.device = None;
        }
    }

    fn connect_card(&mut self, ui: &mut Ui, go: &mut bool, demo: &mut bool, cancel: &mut bool) {
        let width = ui.available_width();
        ui.horizontal(|ui| {
            paint_logo(ui, 40.0);
            ui.add_space(4.0);
            ui.vertical(|ui| {
                ui.label(RichText::new("Public Folders").size(24.0).color(TEXT).strong());
                ui.label(RichText::new("A lightweight viewer for Exchange public folders").color(WEAK));
            });
        });
        ui.add_space(14.0);
        ui.label(
            RichText::new("Open the shared public folders that the new Outlook doesn't show. Sign in with the same account you use for Outlook.")
                .color(TEXT),
        );
        ui.add_space(14.0);

        let busy = self.connecting;
        ui.add_enabled_ui(!busy, |ui| {
            ui.label(RichText::new("Where are your public folders?").color(WEAK).size(12.5));
            ui.horizontal(|ui| {
                let s = &mut self.settings;
                let third = (width - 16.0) / 3.0;
                for (mode, label) in [(Mode::Classic, "Classic Outlook"), (Mode::Server, "Exchange server"), (Mode::Microsoft365, "Microsoft 365")] {
                    let on = s.mode == mode;
                    let b = egui::Button::new(RichText::new(label).color(if on { ACCENT_DARK } else { TEXT }))
                        .fill(if on { ACCENT_LIGHT } else { WHITE })
                        .stroke(Stroke::new(1.0_f32, if on { ACCENT } else { BORDER }))
                        .min_size(egui::vec2(third, 32.0));
                    if ui.add(b).clicked() {
                        s.mode = mode;
                    }
                }
            });
            ui.add_space(10.0);

            let field_w = width - 130.0;
            let mut enter = false;
            let mut use_windows = self.use_windows();
            if self.settings.mode == Mode::Classic {
                let installed = crate::classic::is_installed();
                egui::Frame::none()
                    .fill(if installed { Color32::from_rgb(239, 246, 252) } else { Color32::from_rgb(253, 236, 234) })
                    .rounding(Rounding::same(8.0))
                    .inner_margin(Margin::same(12.0))
                    .show(ui, |ui| {
                        ui.set_width(width - 24.0);
                        if installed {
                            ui.label(RichText::new("No login details needed").strong().color(TEXT));
                            ui.add(
                                egui::Label::new(
                                    RichText::new(
                                        "The app opens your public folders through classic Outlook on this computer, using the account it's already signed in to. Keep classic Outlook set up with your work account. It can sit alongside the new Outlook.",
                                    )
                                    .color(WEAK),
                                )
                                .wrap(),
                            );
                        } else {
                            ui.add(
                                egui::Label::new(
                                    RichText::new("Classic Outlook isn't installed on this computer. Choose “Exchange server” or “Microsoft 365” instead.")
                                        .color(DANGER),
                                )
                                .wrap(),
                            );
                        }
                    });
            } else {
            egui::Grid::new("fields").num_columns(2).spacing([12.0, 10.0]).min_col_width(110.0).show(ui, |ui| {
                let s = &mut self.settings;
                if s.mode == Mode::Server {
                    ui.label("Work email");
                    let r = ui.add(egui::TextEdit::singleline(&mut s.email).hint_text("name@yourcompany.com").desired_width(field_w));
                    enter |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    ui.end_row();
                    if cfg!(windows) {
                        ui.label("");
                        if ui
                            .checkbox(&mut use_windows, "Sign in as me (my Windows login)")
                            .on_hover_text("Works on a company computer that you sign in to with your work account. No password needed.")
                            .changed()
                        {
                            s.use_windows = Some(use_windows);
                        }
                        ui.end_row();
                    }
                    if !use_windows {
                        ui.label("Password");
                        let r = ui.add(
                            egui::TextEdit::singleline(&mut self.password)
                                .password(true)
                                .hint_text("Your Outlook password")
                                .desired_width(field_w),
                        );
                        enter |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        ui.end_row();
                    }
                } else {
                    ui.label("Email address");
                    let r = ui.add(egui::TextEdit::singleline(&mut s.email).hint_text("name@company.com").desired_width(field_w));
                    enter |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    ui.end_row();
                    ui.label("App (client) ID");
                    ui.add(egui::TextEdit::singleline(&mut s.client_id).hint_text("From your IT admin").desired_width(field_w));
                    ui.end_row();
                }
            });
            *go |= enter;

            ui.add_space(4.0);
            let note = if self.settings.mode == Mode::Server {
                "That's usually all you need: the app finds your server from your email address. Not sure what to enter? See the help below."
            } else {
                "Microsoft 365 also needs an app ID from your IT admin (see below)."
            };
            ui.label(RichText::new(note).size(12.0).color(WEAK));

            ui.add_space(6.0);
            let open_adv = self.settings.mode == Mode::Server && !self.settings.server.trim().is_empty();
            egui::CollapsingHeader::new(RichText::new("Advanced").color(WEAK)).id_salt("advanced").default_open(open_adv).show(ui, |ui| {
                let s = &mut self.settings;
                egui::Grid::new("adv").num_columns(2).spacing([12.0, 8.0]).min_col_width(110.0).show(ui, |ui| {
                    if s.mode == Mode::Server {
                        ui.label("Server");
                        ui.add(egui::TextEdit::singleline(&mut s.server).hint_text("Leave empty to find it automatically").desired_width(field_w));
                        ui.end_row();
                        if !use_windows {
                            ui.label("User name");
                            ui.add(
                                egui::TextEdit::singleline(&mut s.username)
                                    .hint_text("Leave empty to use your email")
                                    .desired_width(field_w),
                            );
                            ui.end_row();
                        }
                        ui.label("Sign-in method");
                        egui::ComboBox::from_id_salt("signin")
                            .selected_text(match s.sign_in.as_str() {
                                "ntlm" => "Windows (NTLM)",
                                "basic" => "Basic",
                                _ => "Automatic",
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut s.sign_in, "auto".to_string(), "Automatic");
                                ui.selectable_value(&mut s.sign_in, "ntlm".to_string(), "Windows (NTLM)");
                                ui.selectable_value(&mut s.sign_in, "basic".to_string(), "Basic");
                            });
                        ui.end_row();
                    } else {
                        ui.label("Server");
                        ui.add(egui::TextEdit::singleline(&mut s.m365_server).desired_width(field_w));
                        ui.end_row();
                        ui.label("Tenant");
                        ui.add(egui::TextEdit::singleline(&mut s.tenant).hint_text("organizations").desired_width(field_w));
                        ui.end_row();
                    }
                    ui.label("Exchange version");
                    egui::ComboBox::from_id_salt("version")
                        .selected_text(match s.version.as_str() {
                            "Exchange2013_SP1" => "2013 or newer",
                            "Exchange2010_SP2" => "2010",
                            _ => "Automatic",
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut s.version, "auto".to_string(), "Automatic");
                            ui.selectable_value(&mut s.version, "Exchange2013_SP1".to_string(), "2013 or newer");
                            ui.selectable_value(&mut s.version, "Exchange2010_SP2".to_string(), "2010");
                        });
                    ui.end_row();
                    ui.label("");
                    ui.checkbox(&mut s.ignore_cert_errors, "Trust the server's certificate even if it isn't valid");
                    ui.end_row();
                });
            });
            } // not classic
        });

        if let Some(e) = &self.connect_error {
            ui.add_space(8.0);
            egui::Frame::none()
                .fill(Color32::from_rgb(253, 236, 234))
                .rounding(Rounding::same(6.0))
                .inner_margin(Margin::symmetric(10.0, 8.0))
                .show(ui, |ui| {
                    ui.set_width(width - 20.0);
                    ui.label(RichText::new(e).color(DANGER));
                });
        }

        if let Some((code, url)) = self.device.clone() {
            ui.add_space(10.0);
            egui::Frame::none()
                .fill(Color32::from_rgb(239, 246, 252))
                .stroke(Stroke::new(1.0_f32, ACCENT_LIGHT))
                .rounding(Rounding::same(8.0))
                .inner_margin(Margin::same(12.0))
                .show(ui, |ui| {
                    ui.set_width(width - 24.0);
                    ui.label(format!("Open {url} in your browser and enter this code:"));
                    ui.label(RichText::new(&code).monospace().size(26.0).color(ACCENT_DARK).strong());
                    ui.horizontal(|ui| {
                        if ui.button("Copy code").clicked() {
                            ui.ctx().copy_text(code.clone());
                        }
                        if ui.button("Open sign-in page").clicked() {
                            open_in_browser(ui.ctx(), &url);
                        }
                    });
                    ui.label(RichText::new("Waiting for you to finish signing in…").color(WEAK).size(12.0));
                });
        }

        ui.add_space(16.0);
        if busy {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().size(20.0).color(ACCENT));
                ui.label(RichText::new("Connecting…").color(TEXT));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("Cancel").clicked() {
                        *cancel = true;
                    }
                });
            });
        } else if ui.add_sized([width, 38.0], primary_button("Connect", width)).clicked() {
            *go = true;
        }
        ui.add_space(10.0);
        ui.vertical_centered(|ui| {
            ui.add_enabled_ui(!busy, |ui| {
                if ui.add(egui::Button::new(RichText::new("Try it with demo folders").color(ACCENT)).frame(false)).clicked() {
                    *demo = true;
                }
            });
            ui.add_space(4.0);
            if self.settings.mode != Mode::Classic {
                ui.label(RichText::new("Your password is only kept while the app is open.").size(11.5).color(WEAK));
            }
        });
        ui.add_space(10.0);
        egui::CollapsingHeader::new(RichText::new("Where do I find these details?").color(ACCENT))
            .id_salt("help")
            .default_open(self.settings.email.trim().is_empty())
            .show(ui, |ui| help_text(ui, &self.settings.mode));
    }

    // ----------------------------------------------------------------- browse

    /// Add files (e-mails or anything else) to a folder, in the background.
    fn start_upload(&mut self, folder: FolderInfo, files: Vec<PathBuf>) {
        let Some(b) = self.backend.clone() else { return };
        if self.shot.is_some() {
            return;
        }
        self.pending_upload = None;
        self.status = format!("Adding {} item{} to “{}”…", files.len(), if files.len() == 1 { "" } else { "s" }, folder.name);
        self.status_error = false;
        self.spawn(move || {
            let (mut added, mut problems) = (0, Vec::new());
            for f in &files {
                let name = f.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                if f.is_dir() {
                    problems.push(format!("{name} is a folder (add the files inside it)"));
                    continue;
                }
                match b.upload(&folder, f) {
                    Ok(()) => added += 1,
                    Err(e) => problems.push(format!("{name}: {e}")),
                }
            }
            Msg::Uploaded { folder_id: folder.id.clone(), folder_name: folder.name.clone(), added, problems }
        });
    }

    /// Right-click → Export: one item, alone or with its attachments.
    fn export_one_item(&mut self, item_id: &str, with_attachments: bool) {
        let (Some(folder), Some(b)) = (self.selected.clone(), self.backend.clone()) else { return };
        let Some(it) = self.items.iter().find(|i| i.id == item_id).cloned() else { return };
        let base = crate::export::item_base_name(&it);
        let ext = b.message_ext();
        if with_attachments {
            // A folder of your choice gets the message plus "<name> - attachments".
            let Some(dir) = rfd::FileDialog::new().set_title("Export with attachments to…").set_directory(crate::export::desktop()).pick_folder() else {
                return;
            };
            self.status = format!("Exporting “{}” with attachments…", it.subject);
            self.status_error = false;
            self.spawn(move || {
                Msg::Saved(b.export_item(&folder.id, &it.id, &dir, &base, true).map(|r| {
                    let extra = if r.attachments > 0 { format!(" and {} attachment{}", r.attachments, if r.attachments == 1 { "" } else { "s" }) } else { String::new() };
                    format!("{}{extra} in {}", r.file, dir.display())
                }))
            });
        } else {
            let Some(path) = rfd::FileDialog::new().set_file_name(format!("{base}.{ext}")).add_filter("E-mail message", &[ext]).save_file() else {
                return;
            };
            self.status = format!("Exporting “{}”…", it.subject);
            self.status_error = false;
            self.spawn(move || {
                Msg::Saved(
                    b.mime(&folder.id, &it.id)
                        .and_then(|bytes| std::fs::write(&path, bytes).map_err(|e| format!("Couldn't save the file: {e}")))
                        .map(|_| path.display().to_string()),
                )
            });
        }
    }

    /// Files dropped on the window: add them to the folder under the
    /// pointer, or else to the open folder.
    fn handle_drop(&mut self, ctx: &egui::Context) {
        let mut dropped: Option<(Vec<PathBuf>, Option<(f32, f32)>)> = None;
        #[cfg(windows)]
        if let Some(state) = &self.drop_state {
            let mut s = state.lock().unwrap();
            dropped = s.dropped.take();
            if !s.problems.is_empty() {
                self.status = format!("Some dropped items couldn't be read: {}", s.problems.join("; "));
                self.status_error = true;
                s.problems.clear();
            }
        }
        // Other systems: the window library's own file drops.
        let raw: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect());
        if dropped.is_none() && !raw.is_empty() {
            dropped = Some((raw, ctx.input(|i| i.pointer.hover_pos()).map(|p| (p.x, p.y))));
        }
        let Some((files, pos)) = dropped else { return };
        if self.screen != Screen::Browse || self.backend.is_none() {
            self.status = "Connect first, then drop items on a folder.".into();
            self.status_error = true;
            return;
        }
        let target = pos
            .and_then(|(x, y)| self.folder_rows.iter().find(|(r, _)| r.contains(Pos2::new(x, y))).map(|(_, f)| f.clone()))
            .or_else(|| self.selected.clone())
            .filter(|f| !f.id.is_empty());
        match target {
            Some(f) => self.pending_upload = Some((f, files)),
            None => {
                self.status = "Drop the items on a folder (or open a folder first).".into();
                self.status_error = true;
            }
        }
    }

    /// The folder row under the pointer while something is dragged over.
    fn drag_hover(&self) -> Option<(f32, f32)> {
        #[cfg(windows)]
        if let Some(state) = &self.drop_state {
            let s = state.lock().unwrap();
            if s.hovering {
                return s.pos.or(Some((-1.0, -1.0)));
            }
        }
        None
    }

    fn ui_upload_confirm(&mut self, ctx: &egui::Context) {
        let Some((folder, files)) = self.pending_upload.clone() else { return };
        let mut go = false;
        let mut cancel = false;
        egui::Window::new(RichText::new("Add to public folder").size(16.0).strong())
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .frame(egui::Frame::window(&ctx.style()).inner_margin(Margin::same(18.0)).rounding(Rounding::same(10.0)))
            .show(ctx, |ui| {
                ui.set_width(440.0);
                ui.label(format!("Add {} item{} to “{}”?", files.len(), if files.len() == 1 { "" } else { "s" }, folder.name));
                ui.add_space(6.0);
                egui::ScrollArea::vertical().max_height(160.0).show(ui, |ui| {
                    for f in &files {
                        let name = f.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                        ui.label(RichText::new(format!("• {name}")).color(WEAK));
                    }
                });
                ui.add_space(6.0);
                ui.add(
                    egui::Label::new(
                        RichText::new("E-mail files (.msg, .eml) go in as messages. Other files go in as a post with the file attached, like dropping a file on a public folder in Outlook.")
                            .size(12.0)
                            .color(WEAK),
                    )
                    .wrap(),
                );
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.add(primary_button("Add", 120.0)).clicked() {
                        go = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
        if go {
            self.start_upload(folder, files);
        } else if cancel {
            self.pending_upload = None;
        }
    }

    fn start_export(&mut self) {
        let Some(mut b) = self.backend.clone() else { return };
        // Export straight from the server, not the saved copy, so nothing is missed.
        if let Some(c) = &self.cache {
            b = c.inner();
        }
        if self.shot.is_some() || self.demo_mode {
            return; // never export from a picture-taking run or the demo
        }
        self.export_cancel = Arc::new(AtomicBool::new(false));
        let (status, cancel, dest, att, ctx) =
            (self.export_status.clone(), self.export_cancel.clone(), self.export_dest.clone(), self.export_attachments, self.ctx.clone());
        status.lock().unwrap().running = true;
        let scope = self.export_scope.clone();
        std::thread::spawn(move || crate::export::run(b, dest, att, scope, status, cancel, move || ctx.request_repaint()));
    }

    fn ui_export(&mut self, ctx: &egui::Context) {
        if !self.export_open {
            return;
        }
        let s = self.export_status.lock().unwrap().clone();
        if s.running {
            ctx.request_repaint_after(std::time::Duration::from_millis(300));
        }
        let mut open = true;
        let mut start = false;
        let mut reset = false;
        let title = match &self.export_scope {
            Some(f) => format!("Export “{}”", f.name),
            None => "Export everything".to_string(),
        };
        let scope_note = self.export_scope.as_ref().map(|f| {
            format!("Copies “{}” and every folder inside it, with all their items and attachments, to a folder on this computer, keeping the same folder tree.", f.name)
        });
        egui::Window::new(RichText::new(title).size(17.0).strong())
            .id(egui::Id::new("export_window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .default_width(540.0)
            .frame(egui::Frame::window(&ctx.style()).inner_margin(Margin::same(18.0)).rounding(Rounding::same(10.0)))
            .show(ctx, |ui| {
                ui.set_width(540.0);
                let line = |ui: &mut Ui, text: String| {
                    ui.add(egui::Label::new(RichText::new(text).color(WEAK)).wrap());
                };
                if !s.running && !s.done {
                    ui.add(
                        egui::Label::new(scope_note.clone().unwrap_or_else(|| {
                            "Copies every public folder, with all its messages, posts, events, contacts and attachments, to a folder on this computer. The folder tree stays the same, ready to upload to SharePoint.".to_string()
                        }))
                        .wrap(),
                    );
                    ui.add_space(10.0);
                    ui.label(RichText::new("Save to").strong());
                    ui.horizontal(|ui| {
                        ui.add(egui::Label::new(RichText::new(self.export_dest.display().to_string()).monospace()).truncate());
                    });
                    ui.horizontal(|ui| {
                        if ui.button("Choose another folder…").clicked() {
                            if let Some(p) = rfd::FileDialog::new().set_directory(crate::export::desktop()).pick_folder() {
                                self.export_dest = p;
                            }
                        }
                        if self.export_dest != crate::export::default_destination() && ui.button("Use Desktop\\Public Folders Project").clicked() {
                            self.export_dest = crate::export::default_destination();
                        }
                    });
                    ui.add_space(8.0);
                    ui.checkbox(&mut self.export_attachments, "Also save attachments as separate files (recommended for SharePoint)");
                    ui.add_space(8.0);
                    let fmt = if self.backend.as_ref().map(|b| b.message_ext()) == Some("msg") { "an Outlook .msg file" } else { "an .eml email file" };
                    line(ui, format!("Each item is saved as {fmt}, named by date and subject. A list of everything goes in _manifest.csv (opens in Excel), and any problems go in _export-log.txt."));
                    line(ui, "Large public folders can take hours. Your computer stays awake while it runs, and you can keep browsing. If it stops, run it again: items already saved are skipped.".into());
                    ui.add_space(12.0);
                    if ui.add_sized([ui.available_width(), 38.0], primary_button("Start export", 540.0)).clicked() {
                        start = true;
                    }
                } else {
                    let handled = s.saved + s.skipped + s.failed;
                    let frac = if s.items_found > 0 { handled as f32 / s.items_found as f32 } else { 0.0 };
                    let title = if s.running {
                        "Exporting…"
                    } else if s.fatal.is_some() {
                        "The export couldn't run"
                    } else if s.cancelled {
                        "Export stopped"
                    } else {
                        "Export finished"
                    };
                    ui.label(RichText::new(title).size(16.0).strong().color(if s.fatal.is_some() { DANGER } else { TEXT }));
                    ui.add_space(6.0);
                    if s.running {
                        ui.add(egui::ProgressBar::new(frac).show_percentage().fill(ACCENT));
                        line(ui, "The total grows as more folders are found.".into());
                    }
                    egui::Grid::new("export_stats").num_columns(2).spacing([16.0, 4.0]).show(ui, |ui| {
                        let row = |ui: &mut Ui, k: &str, v: String| {
                            ui.label(RichText::new(k).color(WEAK));
                            ui.label(RichText::new(v).color(TEXT));
                            ui.end_row();
                        };
                        row(ui, "Folders", format!("{} of {} done", s.folders_done, s.folders_found));
                        row(ui, "Items saved", format!("{}", s.saved));
                        if s.skipped > 0 {
                            row(ui, "Already saved before", format!("{}", s.skipped));
                        }
                        row(ui, "Items found so far", format!("{}", s.items_found));
                        row(ui, "Attachments saved", format!("{}", s.attachments));
                        if s.failed > 0 {
                            row(ui, "Couldn't save", format!("{} (see _export-log.txt)", s.failed));
                        }
                        let secs = s.took.unwrap_or_else(|| s.started.map(|t| t.elapsed()).unwrap_or_default()).as_secs();
                        row(ui, "Time", format!("{}h {:02}m {:02}s", secs / 3600, secs / 60 % 60, secs % 60));
                    });
                    if s.running && !s.current.is_empty() {
                        ui.add_space(4.0);
                        ui.add(egui::Label::new(RichText::new(format!("Now: {}", s.current)).size(12.0).color(WEAK)).truncate());
                    }
                    if let Some(e) = &s.fatal {
                        ui.add(egui::Label::new(RichText::new(e).color(DANGER)).wrap());
                    } else if !s.last_problem.is_empty() {
                        ui.add(egui::Label::new(RichText::new(format!("Last problem: {}", s.last_problem)).size(12.0).color(DANGER)).truncate());
                    }
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if s.running {
                            if ui.button("Stop").on_hover_text("Stop after the current item. Run it again later to carry on.").clicked() {
                                self.export_cancel.store(true, Ordering::Relaxed);
                            }
                            line(ui, "You can close this window; the export keeps going.".into());
                        } else {
                            if ui.add(primary_button("Open the folder", 160.0)).clicked() {
                                let _ = std::process::Command::new("explorer").arg(&s.dest).spawn();
                            }
                            if ui.button(if s.cancelled || s.failed > 0 { "Carry on / try again" } else { "Export again" }).clicked() {
                                reset = true;
                            }
                        }
                    });
                }
            });
        if start {
            self.start_export();
        }
        if reset {
            *self.export_status.lock().unwrap() = Default::default();
        }
        if !open {
            self.export_open = false;
        }
    }

    fn ui_browse(&mut self, ctx: &egui::Context) {
        let mut acts: Vec<Act> = Vec::new();

        egui::TopBottomPanel::top("top")
            .frame(egui::Frame::none().fill(WHITE).inner_margin(Margin::symmetric(12.0, 8.0)).stroke(Stroke::new(1.0_f32, BORDER)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    paint_logo(ui, 26.0);
                    ui.label(RichText::new("Public Folders").size(17.0).color(TEXT).strong());
                    ui.add_space(10.0);
                    if let Some(b) = &self.backend {
                        ui.label(RichText::new(b.label()).color(WEAK));
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let label = if self.demo_mode { "Leave demo" } else { "Disconnect" };
                        if ui.button(label).clicked() {
                            acts.push(Act::Disconnect);
                        }
                        if ui.button("⟳ Refresh").on_hover_text("Reload the folders and the open folder").clicked() {
                            acts.push(Act::Refresh);
                        }
                        let export = egui::Button::new(RichText::new("Export everything…").color(WHITE)).fill(ACCENT).stroke(Stroke::NONE);
                        // Not in the demo: it would put sample items in the migration folder.
                        if (!self.demo_mode || self.export_open)
                            && ui
                            .add(export)
                            .on_hover_text("Save every public folder, item and attachment to this computer, ready to move to SharePoint")
                            .clicked()
                        {
                            if !self.export_status.lock().unwrap().running {
                                self.export_scope = None;
                                *self.export_status.lock().unwrap() = Default::default();
                            }
                            self.export_open = true;
                        }
                    });
                });
            });

        egui::TopBottomPanel::bottom("status")
            .exact_height(26.0)
            .frame(egui::Frame::none().fill(FOLDER_BG).inner_margin(Margin::symmetric(12.0, 4.0)).stroke(Stroke::new(1.0_f32, BORDER)))
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    if self.pending > 0 {
                        ui.add(egui::Spinner::new().size(13.0).color(ACCENT));
                        ui.label(RichText::new("Working…").size(12.0).color(WEAK));
                    }
                    let color = if self.status_error { DANGER } else { WEAK };
                    ui.label(RichText::new(&self.status).size(12.0).color(color));
                    if let Some(c) = &self.cache {
                        let m = c.status();
                        let text = if m.running && m.phase == "Mapping folders" {
                            format!("· Saving the folder map on this PC: {} of {} folders", m.folders_done, m.folders_known.max(m.folders_done))
                        } else if m.running {
                            format!("· Saving item lists on this PC: {} of {} folders", m.lists_done, m.lists_total)
                        } else if let Some(t) = m.mapped_at {
                            let when = chrono::DateTime::from_timestamp(t, 0).map(|d| d.with_timezone(&chrono::Local).format("%d/%m %H:%M").to_string()).unwrap_or_default();
                            format!("· Saved copy on this PC, updated {when}")
                        } else {
                            String::new()
                        };
                        if m.running {
                            ui.add(egui::Spinner::new().size(12.0).color(ACCENT));
                        }
                        ui.label(RichText::new(text).size(12.0).color(WEAK))
                            .on_hover_text("Public folders aren't kept offline by Outlook, and Outlook answers other programs slowly. So this app keeps its own copy of the folder tree and item lists, shows it instantly, and refreshes it in the background.");
                    }
                    let ex = self.export_status.lock().unwrap().clone();
                    if ex.running {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.small_button("Show export").clicked() {
                                self.export_open = true;
                            }
                            ui.label(RichText::new(format!("Exporting… {} saved, {} to go", ex.saved + ex.skipped, ex.items_found.saturating_sub(ex.saved + ex.skipped + ex.failed))).size(12.0).color(ACCENT_DARK));
                            ui.add(egui::Spinner::new().size(12.0).color(ACCENT));
                        });
                    }
                });
            });
        self.ui_export(ctx);
        self.ui_upload_confirm(ctx);
        // While something is dragged over the window: say where it will go.
        if let Some((x, y)) = self.drag_hover() {
            let target = self
                .folder_rows
                .iter()
                .find(|(r, _)| r.contains(Pos2::new(x, y)))
                .map(|(_, f)| f.name.clone())
                .or_else(|| self.selected.as_ref().filter(|f| !f.id.is_empty()).map(|f| f.name.clone()));
            let text = match target {
                Some(n) => format!("Drop to add to “{n}”"),
                None => "Drop on a folder to add these items".to_string(),
            };
            let at = if x >= 0.0 { Pos2::new(x + 18.0, y + 18.0) } else { ctx.screen_rect().center() };
            egui::Area::new(egui::Id::new("drop_hint")).fixed_pos(at).order(egui::Order::Tooltip).interactable(false).show(ctx, |ui| {
                egui::Frame::none().fill(ACCENT).rounding(Rounding::same(6.0)).inner_margin(Margin::symmetric(10.0, 6.0)).show(ui, |ui| {
                    ui.label(RichText::new(text).color(WHITE).strong());
                });
            });
        }

        egui::SidePanel::left("folders")
            .resizable(true)
            .default_width(270.0)
            .width_range(190.0..=480.0)
            .frame(egui::Frame::none().fill(FOLDER_BG).inner_margin(Margin::symmetric(6.0, 8.0)).stroke(Stroke::new(1.0_f32, BORDER)))
            .show(ctx, |ui| self.ui_folders(ui, &mut acts));

        egui::SidePanel::right("reader")
            .resizable(true)
            .default_width(480.0)
            .width_range(300.0..=900.0)
            .frame(egui::Frame::none().fill(WHITE).inner_margin(Margin::symmetric(22.0, 18.0)).stroke(Stroke::new(1.0_f32, BORDER)))
            .show(ctx, |ui| self.ui_reader(ui, &mut acts));

        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(WHITE).inner_margin(Margin::same(0.0)))
            .show(ctx, |ui| self.ui_items(ui, &mut acts));

        for a in acts {
            match a {
                Act::Toggle(i) => self.toggle(i),
                Act::Select(f) => {
                    if self.selected.as_ref().map(|s| s.id != f.id).unwrap_or(true) {
                        self.select_folder(f)
                    }
                }
                Act::OpenItem(id) => {
                    if self.selected_item.as_deref() != Some(id.as_str()) {
                        self.open_item(id)
                    }
                }
                Act::LoadMore => {
                    let next = self.items_next;
                    self.load_items(next)
                }
                Act::Refresh => self.refresh(),
                Act::ToggleFavorite => self.toggle_favorite(),
                Act::SaveAttachment(a) => self.save_attachment(a),
                Act::SaveEml => self.save_eml(),
                Act::Disconnect => self.disconnect(),
                Act::UploadTo(f) => {
                    if let Some(files) = rfd::FileDialog::new().set_title(format!("Add to “{}”", f.name)).pick_files() {
                        self.start_upload(f, files);
                    }
                }
                Act::ExportItem(id, with_attachments) => self.export_one_item(&id, with_attachments),
                Act::ExportFolder(f) => {
                    if !self.export_status.lock().unwrap().running {
                        *self.export_status.lock().unwrap() = Default::default();
                    }
                    self.export_scope = Some(f);
                    self.export_open = true;
                }
            }
        }
    }

    fn visible_nodes(&self) -> Vec<usize> {
        let mut out = Vec::new();
        let mut stack = vec![0usize];
        while let Some(i) = stack.pop() {
            out.push(i);
            let n = &self.nodes[i];
            if n.expanded {
                if let Some(ch) = &n.children {
                    stack.extend(ch.iter().rev());
                }
            }
        }
        out
    }

    fn ui_folders(&mut self, ui: &mut Ui, acts: &mut Vec<Act>) {
        let selected_id = self.selected.as_ref().map(|f| f.id.clone());
        let hover = self.drag_hover();
        let mut rows: Vec<(Rect, FolderInfo)> = Vec::new();
        // Find any folder by name, using the saved folder map.
        if let Some(cache) = self.cache.clone() {
            ui.add(egui::TextEdit::singleline(&mut self.folder_search).hint_text("🔍  Find a folder…").desired_width(f32::INFINITY));
            ui.add_space(4.0);
            let q = self.folder_search.trim().to_lowercase();
            if !q.is_empty() {
                // The list of every folder is rebuilt only when the saved tree changes.
                let version = cache.status().tree_version;
                if self.folder_index_version != Some(version) {
                    self.folder_index = cache.all_folders().into_iter().map(|f| (f.folder.name.to_lowercase(), f)).collect();
                    self.folder_index_version = Some(version);
                }
                let matches: Vec<_> = self.folder_index.iter().filter(|(name, _)| name.contains(&q)).take(200).map(|(_, f)| f.clone()).collect();
                let mut chosen = None;
                egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
                    if matches.is_empty() {
                        let m = cache.status();
                        let note = if m.running && m.phase == "Mapping folders" {
                            "No match yet — still mapping folders."
                        } else {
                            "No folder has that name."
                        };
                        ui.label(RichText::new(note).size(12.0).color(WEAK));
                    }
                    for fp in matches {
                        let sel = selected_id.as_deref() == Some(fp.folder.id.as_str());
                        let count = if fp.folder.total_count > 0 { fp.folder.total_count.to_string() } else { String::new() };
                        let r = folder_row(ui, 0, false, false, folder_icon(&fp.folder.class), &fp.folder.name, &count, sel, false).on_hover_text(&fp.path);
                        folder_extras(ui, &r, &fp.folder, hover, &mut rows, acts);
                        let parent = fp.path.rsplit_once(" › ").map(|(p, _)| p.to_string()).unwrap_or_default();
                        if !parent.is_empty() {
                            ui.horizontal(|ui| {
                                ui.add_space(44.0);
                                ui.add(egui::Label::new(RichText::new(format!("in {parent}")).size(11.0).color(WEAK)).truncate());
                            });
                        }
                        if r.clicked() {
                            chosen = Some(fp);
                        }
                    }
                });
                self.folder_rows = rows;
                if let Some(fp) = chosen {
                    self.reveal(fp);
                }
                return;
            }
        }
        let mut scrolled = false;
        egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
            section_label(ui, "FAVOURITES");
            let favs = self.favorites();
            if favs.is_empty() {
                ui.horizontal(|ui| {
                    ui.add_space(10.0);
                    ui.label(RichText::new("Star a folder to keep it here.").size(12.0).color(WEAK));
                });
            }
            for f in favs {
                let sel = selected_id.as_deref() == Some(f.id.as_str());
                let r = folder_row(ui, 0, false, false, Icon::Star, &f.name, "", sel, false);
                let info = FolderInfo { id: f.id, name: f.name, class: f.class, ..Default::default() };
                folder_extras(ui, &r, &info, hover, &mut rows, acts);
                if r.clicked() {
                    acts.push(Act::Select(info));
                }
            }
            ui.add_space(10.0);
            section_label(ui, "FOLDERS");
            if self.nodes.is_empty() {
                return;
            }
            for idx in self.visible_nodes() {
                let n = &self.nodes[idx];
                let has_children = n.children.as_ref().map(|c| !c.is_empty()).unwrap_or(n.info.child_count > 0);
                let is_root = idx == 0;
                let icon = if is_root { Icon::Folder } else { folder_icon(&n.info.class) };
                let count = if n.loading {
                    "…".to_string()
                } else if n.error.is_some() {
                    "!".to_string()
                } else if n.info.total_count > 0 {
                    n.info.total_count.to_string()
                } else {
                    String::new()
                };
                let sel = !is_root && selected_id.as_deref() == Some(n.info.id.as_str());
                let resp = folder_row(ui, n.depth, has_children, n.expanded, icon, &n.info.name, &count, sel, is_root);
                let resp = match &n.error {
                    Some(e) => resp.on_hover_text(e),
                    None => resp,
                };
                if !is_root {
                    folder_extras(ui, &resp, &n.info, hover, &mut rows, acts);
                }
                if sel && self.scroll_to_selected {
                    resp.scroll_to_me(Some(Align::Center));
                    scrolled = true;
                }
                let toggle_hit = resp.clicked()
                    && resp
                        .interact_pointer_pos()
                        .map(|p| p.x < resp.rect.left() + 6.0 + n.depth as f32 * 16.0 + 18.0)
                        .unwrap_or(false);
                if has_children && (toggle_hit || resp.double_clicked() || (is_root && resp.clicked())) {
                    acts.push(Act::Toggle(idx));
                } else if resp.clicked() && !is_root {
                    acts.push(Act::Select(n.info.clone()));
                }
            }
        });
        // Done after one full draw of the tree (the row was either shown or isn't there).
        let _ = scrolled;
        self.scroll_to_selected = false;
        self.folder_rows = rows;
    }

    fn ui_items(&mut self, ui: &mut Ui, acts: &mut Vec<Act>) {
        let Some(folder) = self.selected.clone() else {
            empty_state(ui, Icon::Folder, "Choose a folder", "Pick a public folder on the left to see what's in it.");
            return;
        };
        let is_fav = self.favorites().iter().any(|f| f.id == folder.id);
        egui::Frame::none().inner_margin(Margin { left: 16.0, right: 12.0, top: 12.0, bottom: 8.0 }).show(ui, |ui| {
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(Vec2::splat(24.0), Sense::hover());
                paint_icon(ui.painter(), r, folder_icon(&folder.class), ACCENT);
                let w = ui.available_width() - 40.0;
                ui.add_sized(
                    [0.0, 24.0],
                    egui::Label::new(one_line(ui, &folder.name, 19.0, TEXT, w)).selectable(false),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let (star, tip) = if is_fav { ("★", "Remove from favourites") } else { ("☆", "Add to favourites") };
                    let b = egui::Button::new(RichText::new(star).size(18.0).color(if is_fav { Color32::from_rgb(214, 160, 0) } else { WEAK }))
                        .frame(false);
                    if ui.add(b).on_hover_text(tip).clicked() {
                        acts.push(Act::ToggleFavorite);
                    }
                    if !self.demo_mode
                        && ui
                            .button("⬆ Upload…")
                            .on_hover_text("Add e-mails or files to this folder (you can also drag them from Outlook or File Explorer onto a folder)")
                            .clicked()
                    {
                        acts.push(Act::UploadTo(folder.clone()));
                    }
                });
            });
            let count = if self.items_loading && self.items.is_empty() {
                "Loading…".to_string()
            } else if self.items_total == 1 {
                "1 item".to_string()
            } else {
                format!("{} items", self.items_total)
            };
            ui.label(RichText::new(count).size(12.0).color(WEAK));
            ui.add_space(4.0);
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text("🔍  Filter by sender, subject or text")
                    .desired_width(f32::INFINITY),
            );
        });
        ui.painter().hline(ui.max_rect().x_range(), ui.cursor().top(), Stroke::new(1.0_f32, BORDER));

        if let Some(e) = self.items_error.clone() {
            ui.add_space(16.0);
            ui.horizontal(|ui| {
                ui.add_space(16.0);
                ui.vertical(|ui| {
                    ui.label(RichText::new("This folder couldn't be opened.").color(DANGER).strong());
                    ui.add(egui::Label::new(RichText::new(e).color(WEAK)).wrap());
                    if ui.button("Try again").clicked() {
                        acts.push(Act::Refresh);
                    }
                });
            });
            return;
        }
        if self.items.is_empty() {
            if self.items_loading {
                ui.add_space(40.0);
                ui.vertical_centered(|ui| ui.add(egui::Spinner::new().size(24.0).color(ACCENT)));
            } else if folder.child_count > 0 {
                empty_state(ui, Icon::Folder, "Only subfolders here", "This folder holds other folders. Open it on the left to see them.");
            } else {
                empty_state(ui, Icon::Mail, "Nothing here", "This folder is empty. Drag e-mails or files here, or use Upload.");
            }
            return;
        }

        let q = self.filter.trim().to_lowercase();
        let shown: Vec<usize> = (0..self.items.len())
            .filter(|&i| {
                q.is_empty() || {
                    let it = &self.items[i];
                    it.subject.to_lowercase().contains(&q) || it.from.to_lowercase().contains(&q) || it.preview.to_lowercase().contains(&q)
                }
            })
            .collect();
        let extra = usize::from(self.items_more);
        let row_h = 66.0;
        egui::ScrollArea::vertical().auto_shrink([false; 2]).show_rows(ui, row_h, shown.len() + extra, |ui, range| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for r in range {
                if r >= shown.len() {
                    ui.add_space(14.0);
                    ui.vertical_centered(|ui| {
                        if self.items_loading {
                            ui.add(egui::Spinner::new().color(ACCENT));
                        } else if ui.button(format!("Load more ({} of {})", self.items.len(), self.items_total)).clicked() {
                            acts.push(Act::LoadMore);
                        }
                    });
                    continue;
                }
                let it = &self.items[shown[r]];
                let sel = self.selected_item.as_deref() == Some(it.id.as_str());
                let resp = item_row(ui, it, sel, row_h);
                if resp.clicked() {
                    acts.push(Act::OpenItem(it.id.clone()));
                }
                // Right-click: export this item.
                let id = it.id.clone();
                resp.context_menu(|ui| {
                    if ui.button("Open").clicked() {
                        acts.push(Act::OpenItem(id.clone()));
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("Export…").on_hover_text("Save the message as one file").clicked() {
                        acts.push(Act::ExportItem(id.clone(), false));
                        ui.close_menu();
                    }
                    if ui.button("Export with attachments…").on_hover_text("Save the message, plus its attachments as separate files").clicked() {
                        acts.push(Act::ExportItem(id.clone(), true));
                        ui.close_menu();
                    }
                });
            }
        });
        if shown.is_empty() {
            ui.label(RichText::new("No items match the filter.").color(WEAK));
        }
    }

    fn ui_reader(&mut self, ui: &mut Ui, acts: &mut Vec<Act>) {
        if self.detail_loading {
            ui.add_space(60.0);
            ui.vertical_centered(|ui| ui.add(egui::Spinner::new().size(26.0).color(ACCENT)));
            return;
        }
        if let Some(e) = &self.detail_error {
            ui.label(RichText::new("This item couldn't be opened.").color(DANGER).strong());
            ui.add(egui::Label::new(RichText::new(e).color(WEAK)).wrap());
            return;
        }
        let Some(d) = self.detail.clone() else {
            empty_state(ui, Icon::Mail, "Select an item to read it", "Messages, posts, events and contacts open here.");
            return;
        };
        egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
            ui.add(egui::Label::new(RichText::new(if d.subject.is_empty() { "(no subject)" } else { &d.subject }).size(20.0).color(TEXT)).wrap());
            ui.add_space(12.0);
            let calendar = d.class.starts_with("IPM.Appointment");
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(Vec2::splat(40.0), Sense::hover());
                paint_avatar(ui, rect.center(), 20.0, &d.from, if calendar { Some(Icon::Calendar) } else { None });
                ui.vertical(|ui| {
                    ui.label(RichText::new(if d.from.is_empty() { "Unknown sender" } else { &d.from }).color(TEXT).strong().size(14.5));
                    if !d.from_email.is_empty() && d.from_email != d.from {
                        ui.label(RichText::new(&d.from_email).color(WEAK).size(12.0));
                    }
                    if !d.date.is_empty() {
                        ui.label(RichText::new(&d.date).color(WEAK).size(12.0));
                    }
                });
            });
            ui.add_space(6.0);
            for (label, v) in [("To", &d.to), ("Cc", &d.cc)] {
                if !v.is_empty() {
                    ui.add(egui::Label::new(RichText::new(format!("{label}: {v}")).color(WEAK).size(12.5)).wrap());
                }
            }
            if !d.fields.is_empty() {
                ui.add_space(6.0);
                egui::Frame::none().fill(FOLDER_BG).rounding(Rounding::same(6.0)).inner_margin(Margin::same(10.0)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    egui::Grid::new("fields_view").num_columns(2).spacing([14.0, 4.0]).show(ui, |ui| {
                        for (k, v) in &d.fields {
                            ui.label(RichText::new(k).color(WEAK));
                            ui.label(RichText::new(v).color(TEXT));
                            ui.end_row();
                        }
                    });
                });
            }
            if !d.attachments.is_empty() {
                ui.add_space(8.0);
                ui.horizontal_wrapped(|ui| {
                    for a in &d.attachments {
                        let label = format!("📎 {}  {}", a.name, if a.size > 0 { fmt_size(a.size) } else { String::new() });
                        let b = egui::Button::new(RichText::new(label).color(TEXT).size(13.0))
                            .fill(FOLDER_BG)
                            .stroke(Stroke::new(1.0_f32, BORDER));
                        if ui.add(b).on_hover_text("Save this attachment").clicked() {
                            acts.push(Act::SaveAttachment(a.clone()));
                        }
                    }
                });
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if !d.class.starts_with("IPM.Contact") && ui.button("Save as email file").on_hover_text("Save as .eml — opens in any mail app, including the new Outlook").clicked() {
                    acts.push(Act::SaveEml);
                }
                if ui.button("Copy text").clicked() {
                    ui.ctx().copy_text(format!("{}\n\n{}", d.subject, d.body));
                }
            });
            ui.add_space(8.0);
            let y = ui.cursor().top();
            ui.painter().hline(ui.max_rect().x_range(), y, Stroke::new(1.0_f32, BORDER));
            ui.add_space(14.0);
            if d.body.is_empty() {
                ui.label(RichText::new("(no text)").color(WEAK));
            } else {
                ui.add(egui::Label::new(RichText::new(&d.body).size(14.5).color(TEXT)).wrap());
            }
            ui.add_space(20.0);
        });
    }

    fn handle_screenshot(&mut self, ctx: &egui::Context) {
        let Some(shot) = &mut self.shot else { return };
        shot.frames += 1;
        ctx.request_repaint();
        if shot.frames >= 120 && !shot.requested {
            shot.requested = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot);
        }
        let image = ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(img) = image {
            if let Err(e) = save_png(&shot.path, &img) {
                eprintln!("Could not save screenshot: {e}");
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else if shot.frames > 1500 {
            eprintln!("Screenshot never arrived");
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else if shot.requested && shot.frames % 120 == 0 {
            // Ask again in case the first request was dropped.
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot);
        }
    }
}

fn save_png(path: &std::path::Path, img: &egui::ColorImage) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
    }
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), img.size[0] as u32, img.size[1] as u32);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let data: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
    let mut w = enc.write_header().map_err(|e| e.to_string())?;
    w.write_image_data(&data).map_err(|e| e.to_string())
}

/// Plain-language help for the sign-in card.
fn help_text(ui: &mut Ui, mode: &Mode) {
    let item = |ui: &mut Ui, title: &str, text: &str| {
        ui.label(RichText::new(title).strong().color(TEXT));
        ui.add(egui::Label::new(RichText::new(text).size(12.5).color(WEAK)).wrap());
        ui.add_space(6.0);
    };
    egui::Frame::none().fill(FOLDER_BG).rounding(Rounding::same(6.0)).inner_margin(Margin::same(10.0)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        item(
            ui,
            "Which one do I pick?",
            "If classic Outlook is set up on this computer and shows your public folders, pick Classic Outlook: you don't need any login details. Otherwise, if you read your work mail on the web at outlook.office.com, pick Microsoft 365. If your company has its own Outlook web address (like mail.yourcompany.com/owa), pick Exchange server. Not sure? Try Exchange server first: it tells you if your mailbox is in Microsoft 365.",
        );
        if *mode == Mode::Classic {
            item(
                ui,
                "Classic Outlook",
                "Classic Outlook must be installed and signed in to your work account. To get it back, open the new Outlook and turn off the “New Outlook” switch at the top right, or start “Outlook (classic)” from the Start menu. This app uses it in the background while you browse.",
            );
            item(
                ui,
                "If Outlook asks for permission",
                "Outlook may ask whether to allow a program to access your mail. Choose Allow so this app can read the public folders.",
            );
        } else if *mode == Mode::Server {
            item(ui, "Work email", "The email address you use in Outlook, like name@yourcompany.com.");
            item(
                ui,
                "Sign in as me",
                "Tick this if you're on your work computer and signed in to Windows with your work account. You won't need a password.",
            );
            item(
                ui,
                "Password",
                "The password you use for Outlook. At most companies it's the same password you use to sign in to your work computer.",
            );
            item(
                ui,
                "If the server isn't found",
                "Open Advanced and type the server name. To find it in the old Outlook: hold Ctrl, right-click the Outlook icon by the clock, choose “Connection Status” and copy a name from the “Server name” column. Or choose File > Account Settings > Account Settings, double-click your account and copy the Server box. Your IT team can also tell you; ask for the “Exchange server” or “EWS address”.",
            );
            item(
                ui,
                "If your password is refused",
                "Open Advanced and enter your user name the way you type it to sign in to your work computer, for example COMPANY\\yourname.",
            );
        } else {
            item(ui, "Email address", "The email address you use in Outlook.");
            item(
                ui,
                "App (client) ID",
                "A code your IT admin creates once for the whole company. Send them this: “Please register a public client app in Microsoft Entra with the delegated permission EWS.AccessAsUser.All, and send me its Application (client) ID.” Microsoft is switching this service off for Microsoft 365, so your admin may also need to keep it on.",
            );
        }
    });
}

/// For every folder row: remember where it is (for drops), highlight it
/// while something is dragged over it, and give it a right-click menu.
fn folder_extras(ui: &Ui, resp: &egui::Response, info: &FolderInfo, hover: Option<(f32, f32)>, rows: &mut Vec<(Rect, FolderInfo)>, acts: &mut Vec<Act>) {
    rows.push((resp.rect, info.clone()));
    if let Some((x, y)) = hover {
        if resp.rect.contains(Pos2::new(x, y)) {
            ui.painter().rect_stroke(resp.rect.shrink(1.0), Rounding::same(5.0), Stroke::new(2.0_f32, ACCENT));
        }
    }
    resp.context_menu(|ui| {
        if ui.button("Upload e-mails or files here…").clicked() {
            acts.push(Act::UploadTo(info.clone()));
            ui.close_menu();
        }
        if ui.button("Export this folder…").clicked() {
            acts.push(Act::ExportFolder(info.clone()));
            ui.close_menu();
        }
    });
}

fn empty_state(ui: &mut Ui, icon: Icon, title: &str, text: &str) {
    ui.add_space((ui.available_height() * 0.3).min(160.0));
    ui.vertical_centered(|ui| {
        let (r, _) = ui.allocate_exact_size(Vec2::splat(44.0), Sense::hover());
        paint_icon(ui.painter(), r, icon, Color32::from_rgb(170, 180, 195));
        ui.add_space(6.0);
        ui.label(RichText::new(title).size(16.0).color(TEXT).strong());
        ui.label(RichText::new(text).color(WEAK));
    });
}

#[allow(clippy::too_many_arguments)]
fn folder_row(
    ui: &mut Ui,
    depth: usize,
    has_children: bool,
    expanded: bool,
    icon: Icon,
    name: &str,
    count: &str,
    selected: bool,
    strong: bool,
) -> egui::Response {
    let h = 28.0;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), h), Sense::click());
    let p = ui.painter();
    if selected {
        p.rect_filled(rect, Rounding::same(5.0), ACCENT_LIGHT);
    } else if resp.hovered() {
        p.rect_filled(rect, Rounding::same(5.0), HOVER);
    }
    let x0 = rect.left() + 6.0 + depth as f32 * 16.0;
    let cy = rect.center().y;
    if has_children {
        let c = Pos2::new(x0 + 7.0, cy);
        let pts = if expanded {
            vec![Pos2::new(c.x - 4.0, c.y - 2.0), Pos2::new(c.x + 4.0, c.y - 2.0), Pos2::new(c.x, c.y + 3.0)]
        } else {
            vec![Pos2::new(c.x - 2.0, c.y - 4.0), Pos2::new(c.x + 3.0, c.y), Pos2::new(c.x - 2.0, c.y + 4.0)]
        };
        p.add(Shape::convex_polygon(pts, WEAK, Stroke::NONE));
    }
    let icon_color = match icon {
        Icon::Star => Color32::from_rgb(214, 160, 0),
        _ if selected => ACCENT,
        _ => Color32::from_rgb(110, 135, 170),
    };
    paint_icon(p, Rect::from_center_size(Pos2::new(x0 + 26.0, cy), Vec2::splat(16.0)), icon, icon_color);
    let count_w = if count.is_empty() { 0.0 } else { 34.0 };
    if !count.is_empty() {
        p.text(Pos2::new(rect.right() - 8.0, cy), Align2::RIGHT_CENTER, count, FontId::proportional(12.0), if selected { ACCENT_DARK } else { WEAK });
    }
    let name_x = x0 + 38.0;
    let color = if selected { ACCENT_DARK } else { TEXT };
    let g = one_line(ui, name, if strong { 14.5 } else { 14.0 }, color, rect.right() - name_x - count_w - 8.0);
    let gy = cy - g.size().y / 2.0;
    ui.painter().galley(Pos2::new(name_x, gy), g, color);
    resp
}

fn item_row(ui: &mut Ui, it: &ItemSummary, selected: bool, h: f32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), h), Sense::click());
    let p = ui.painter();
    if selected {
        p.rect_filled(rect, Rounding::ZERO, ACCENT_LIGHT);
        p.rect_filled(Rect::from_min_size(rect.min, egui::vec2(3.0, rect.height())), Rounding::ZERO, ACCENT);
    } else if resp.hovered() {
        p.rect_filled(rect, Rounding::ZERO, HOVER);
    }
    p.hline(rect.x_range(), rect.bottom() - 0.5, Stroke::new(1.0_f32, Color32::from_rgb(236, 237, 240)));

    let calendar = it.class.starts_with("IPM.Appointment");
    let (line1, line2) = if it.from.is_empty() { (it.subject.as_str(), "") } else { (it.from.as_str(), it.subject.as_str()) };
    paint_avatar(ui, Pos2::new(rect.left() + 32.0, rect.top() + 24.0), 16.0, line1, if calendar { Some(Icon::Calendar) } else { None });

    let x = rect.left() + 58.0;
    let right = rect.right() - 14.0;
    let date_g = one_line(ui, &it.date, 12.0, if selected { ACCENT_DARK } else { WEAK }, 140.0);
    let date_w = date_g.size().x;
    ui.painter().galley(Pos2::new(right - date_w, rect.top() + 10.0), date_g, WEAK);

    let g1 = one_line(ui, line1, 14.5, TEXT, right - x - date_w - 10.0);
    ui.painter().galley(Pos2::new(x, rect.top() + 8.0), g1, TEXT);
    let clip_w = if it.has_attachments { 20.0 } else { 0.0 };
    if !line2.is_empty() {
        let g2 = one_line(ui, line2, 13.0, if selected { ACCENT_DARK } else { Color32::from_rgb(30, 70, 120) }, right - x - clip_w);
        ui.painter().galley(Pos2::new(x, rect.top() + 27.0), g2, TEXT);
    }
    if it.has_attachments {
        ui.painter().text(Pos2::new(right, rect.top() + 35.0), Align2::RIGHT_CENTER, "📎", FontId::proportional(13.0), WEAK);
    }
    if !it.preview.is_empty() {
        let y = if line2.is_empty() { rect.top() + 27.0 } else { rect.top() + 45.0 };
        let g3 = one_line(ui, &it.preview, 12.0, WEAK, right - x);
        ui.painter().galley(Pos2::new(x, y), g3, WEAK);
    }
    resp
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        // Take over drag and drop once the window exists (accepts Outlook e-mails).
        if !self.dnd_ready {
            self.dnd_ready = true;
            #[cfg(windows)]
            {
                use raw_window_handle::{HasWindowHandle, RawWindowHandle};
                if let Ok(h) = frame.window_handle() {
                    if let RawWindowHandle::Win32(w) = h.as_raw() {
                        self.drop_state = crate::dnd::register(w.hwnd.get(), ctx.clone());
                    }
                }
            }
        }
        let _ = &frame;
        self.handle_messages();
        self.handle_drop(ctx);
        if let Some(c) = self.cache.clone() {
            let m = c.status();
            // The background map found new or changed folders: redraw the tree.
            if m.tree_version != self.tree_version {
                self.tree_version = m.tree_version;
                self.rebuild_tree_from_cache();
            }
            if m.running {
                ctx.request_repaint_after(std::time::Duration::from_millis(500));
            }
        }
        match self.screen {
            Screen::Connect => self.ui_connect(ctx),
            Screen::Browse => self.ui_browse(ctx),
        }
        self.handle_screenshot(ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.save_tree_state();
        if !self.demo_mode && self.shot.is_none() {
            self.settings.save();
        }
        if let Some(c) = &self.cache {
            c.stop();
        }
    }
}
