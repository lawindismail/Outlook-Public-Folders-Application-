//! Remembered connection details (never the password) and favourites.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum Mode {
    /// Use classic Outlook on this PC (its own sign-in).
    Classic,
    Server,
    Microsoft365,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Favorite {
    pub id: String,
    pub name: String,
    pub class: String,
}

/// Which folders were open, per account, so the tree comes back as it was.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TreeState {
    pub expanded: Vec<String>,
    pub selected: Option<crate::model::FolderInfo>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub tree: HashMap<String, TreeState>,
    pub mode: Mode,
    pub server: String,
    pub m365_server: String,
    pub email: String,
    pub username: String,
    /// "auto", "ntlm" or "basic" (company server only).
    pub sign_in: String,
    /// Sign in with this PC's Windows login; `None` = decide from the PC.
    pub use_windows: Option<bool>,
    pub ignore_cert_errors: bool,
    pub version: String,
    pub tenant: String,
    pub client_id: String,
    pub favorites: HashMap<String, Vec<Favorite>>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            tree: HashMap::new(),
            mode: if crate::classic::is_installed() { Mode::Classic } else { Mode::Server },
            server: String::new(),
            m365_server: "outlook.office365.com".into(),
            email: String::new(),
            username: String::new(),
            sign_in: "auto".into(),
            use_windows: None,
            ignore_cert_errors: false,
            version: "auto".into(),
            tenant: "organizations".into(),
            client_id: String::new(),
            favorites: HashMap::new(),
        }
    }
}

fn path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("PublicFolderViewer").join("settings.json"))
}

impl Settings {
    pub fn load() -> Self {
        path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        if let Some(p) = path() {
            if let Some(dir) = p.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Ok(t) = serde_json::to_string_pretty(self) {
                let _ = std::fs::write(p, t);
            }
        }
    }
}
