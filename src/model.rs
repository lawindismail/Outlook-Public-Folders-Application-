//! Shared data types and the `Backend` trait implemented by the live
//! Exchange client and the offline demo.

use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FolderInfo {
    pub id: String,
    pub name: String,
    /// Exchange folder class, e.g. IPF.Note, IPF.Appointment, IPF.Contact.
    pub class: String,
    pub child_count: u32,
    pub total_count: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ItemSummary {
    pub id: String,
    pub subject: String,
    pub from: String,
    pub date: String,
    pub class: String,
    pub has_attachments: bool,
    pub preview: String,
    /// Full date (for export file names and the manifest).
    pub when: Option<DateTime<Local>>,
}

/// What exporting one item produced.
#[derive(Clone, Debug, Default)]
pub struct Exported {
    /// The message file's name.
    #[allow(dead_code)]
    pub file: String,
    /// Attachments saved as separate files.
    pub attachments: u32,
    /// Problems that didn't stop the item (e.g. one attachment failed).
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct ItemPage {
    pub items: Vec<ItemSummary>,
    pub total: u32,
    pub more: bool,
    pub next_offset: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Attachment {
    pub id: String,
    pub name: String,
    pub size: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ItemDetail {
    pub id: String,
    pub subject: String,
    pub from: String,
    pub from_email: String,
    pub to: String,
    pub cc: String,
    pub date: String,
    pub body: String,
    pub class: String,
    pub fields: Vec<(String, String)>,
    pub attachments: Vec<Attachment>,
}

/// Where folders and items come from: a real Exchange server or the demo.
pub trait Backend: Send + Sync {
    fn label(&self) -> String;
    fn root_folders(&self) -> Result<Vec<FolderInfo>, String>;
    fn child_folders(&self, folder_id: &str) -> Result<Vec<FolderInfo>, String>;
    fn items(&self, folder: &FolderInfo, offset: u32) -> Result<ItemPage, String>;
    fn item(&self, folder_id: &str, item_id: &str) -> Result<ItemDetail, String>;
    fn attachment(&self, folder_id: &str, attachment_id: &str) -> Result<Vec<u8>, String>;
    fn mime(&self, folder_id: &str, item_id: &str) -> Result<Vec<u8>, String>;
    /// File type `mime` produces: "eml", or "msg" from classic Outlook.
    fn message_ext(&self) -> &'static str {
        "eml"
    }
    /// Forget remembered answers (Refresh).
    fn clear_cache(&self) {}
    /// The whole folder tree in one go ("subfolders of each folder", "" =
    /// top level), if this connection can do that quickly. `None` = no.
    fn whole_tree(&self) -> Option<Result<std::collections::HashMap<String, Vec<FolderInfo>>, String>> {
        None
    }
    /// Add a file to a folder: an e-mail file (.msg, .eml) goes in as a
    /// message, anything else as a post with the file attached (as classic
    /// Outlook does when you drop a file on a public folder).
    fn upload(&self, _folder: &FolderInfo, _path: &std::path::Path) -> Result<(), String> {
        Err("Adding items isn't available here.".into())
    }
    /// Forget what's remembered about one folder ("" = the top level), so
    /// the next request asks Outlook or Exchange again.
    fn forget(&self, _folder_id: &str) {}
    /// Every item in a folder (for export; not remembered).
    fn all_items(&self, folder: &FolderInfo) -> Result<Vec<ItemSummary>, String> {
        let mut all = Vec::new();
        let mut offset = 0;
        loop {
            let page = self.items(folder, offset)?;
            let n = page.items.len();
            all.extend(page.items);
            if !page.more || n == 0 {
                return Ok(all);
            }
            offset = page.next_offset;
        }
    }

    /// Save one item as `<dir>/<base>.<message_ext>`, and, if asked, its
    /// attachments into `<dir>/<base> - attachments/`.
    fn export_item(&self, folder_id: &str, item_id: &str, dir: &std::path::Path, base: &str, attachments: bool) -> Result<Exported, String> {
        let file = format!("{base}.{}", self.message_ext());
        let bytes = self.mime(folder_id, item_id)?;
        std::fs::write(dir.join(&file), bytes).map_err(|e| format!("Couldn't write {file}: {e}"))?;
        let mut out = Exported { file, ..Default::default() };
        if attachments {
            let d = self.item(folder_id, item_id)?;
            if !d.attachments.is_empty() {
                let adir = dir.join(format!("{base} - attachments"));
                std::fs::create_dir_all(&adir).map_err(|e| e.to_string())?;
                let mut used = std::collections::HashSet::new();
                for (i, a) in d.attachments.iter().enumerate() {
                    let name = crate::export::unique_file_name(&mut used, &a.name, &format!("attachment {}", i + 1));
                    match self.attachment(folder_id, &a.id).and_then(|b| std::fs::write(adir.join(&name), b).map_err(|e| e.to_string())) {
                        Ok(()) => out.attachments += 1,
                        Err(e) => out.warnings.push(format!("attachment “{}”: {e}", a.name)),
                    }
                }
            }
        }
        Ok(out)
    }
}

pub fn fmt_list_date(dt: &DateTime<Local>) -> String {
    let now = Local::now();
    if dt.date_naive() == now.date_naive() {
        dt.format("%H:%M").to_string()
    } else if (now - *dt).num_days() < 6 && *dt < now {
        dt.format("%a %H:%M").to_string()
    } else {
        dt.format("%d/%m/%Y").to_string()
    }
}

pub fn fmt_long_date(dt: &DateTime<Local>) -> String {
    dt.format("%a %d/%m/%Y %H:%M").to_string()
}

/// Parse an EWS (ISO 8601) date into local time.
pub fn parse_ews_date(s: &str) -> Option<DateTime<Local>> {
    DateTime::parse_from_rfc3339(s.trim()).ok().map(|d| d.with_timezone(&Local))
}

pub fn fmt_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{} KB", bytes / 1024)
    } else {
        format!("{bytes} B")
    }
}
