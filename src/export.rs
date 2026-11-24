//! "Export everything": copy every public folder, item and attachment to a
//! local folder (by default Desktop\Public Folders Project), ready to move
//! to SharePoint.
//!
//! Layout: one Windows folder per public folder, with the same tree. Each
//! item is a message file (.msg from classic Outlook, .eml directly from
//! Exchange) named "YYYY-MM-DD HHMM - Subject". Attachments are also saved
//! as ordinary files in "<item> - attachments", so SharePoint can show them.
//! `_manifest.csv` lists every item, and `_export-log.txt` lists problems.
//! Running it again skips files that are already there, so an interrupted
//! export carries on where it stopped.

use crate::model::*;
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const FOLDER_NAME: &str = "Public Folders Project";

#[derive(Clone, Debug, Default)]
pub struct ExportStatus {
    pub running: bool,
    pub done: bool,
    pub cancelled: bool,
    pub dest: PathBuf,
    pub folders_found: u32,
    pub folders_done: u32,
    pub items_found: u64,
    pub saved: u64,
    pub skipped: u64,
    pub failed: u64,
    pub attachments: u64,
    pub current: String,
    pub last_problem: String,
    pub started: Option<Instant>,
    pub took: Option<Duration>,
    pub fatal: Option<String>,
}

/// The person's Desktop (it may live in OneDrive).
pub fn desktop() -> PathBuf {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::UI::Shell::{FOLDERID_Desktop, SHGetKnownFolderPath};
        let mut p: *mut u16 = std::ptr::null_mut();
        if SHGetKnownFolderPath(&FOLDERID_Desktop, 0, std::ptr::null_mut(), &mut p) >= 0 && !p.is_null() {
            let mut len = 0;
            while *p.add(len) != 0 {
                len += 1;
            }
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, len));
            windows_sys::Win32::System::Com::CoTaskMemFree(p as _);
            return PathBuf::from(s);
        }
    }
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).unwrap_or_default();
    PathBuf::from(home).join("Desktop")
}

pub fn default_destination() -> PathBuf {
    desktop().join(FOLDER_NAME)
}

/// A name that is safe for Windows and SharePoint, at most `max` characters.
pub fn safe_name(s: &str, max: usize, fallback: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| if "\\/:*?\"<>|".contains(c) || c.is_control() { '_' } else { c })
        .collect();
    let mut t: String = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.chars().count() > max {
        t = t.chars().take(max).collect();
    }
    let t = t.trim().trim_end_matches(['.', ' ']).trim_start_matches("~$").trim().to_string();
    if t.is_empty() {
        return fallback.to_string();
    }
    let stem = t.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = ["CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9"];
    if reserved.contains(&stem.as_str()) || t.eq_ignore_ascii_case("desktop.ini") || t.to_ascii_lowercase().starts_with("_vti_") {
        return format!("{t}_");
    }
    t
}

/// A unique, safe file name (keeps the extension) within `used`.
pub fn unique_file_name(used: &mut HashSet<String>, name: &str, fallback: &str) -> String {
    let base = safe_name(name, 100, fallback);
    let (stem, ext) = match base.rfind('.') {
        Some(i) if i > 0 && base.len() - i <= 10 => (base[..i].to_string(), base[i..].to_string()),
        _ => (base.clone(), String::new()),
    };
    unique(used, &stem, &ext)
}

fn unique(used: &mut HashSet<String>, stem: &str, ext: &str) -> String {
    let mut n = 1;
    loop {
        let candidate = if n == 1 { format!("{stem}{ext}") } else { format!("{stem} ({n}){ext}") };
        if used.insert(candidate.to_lowercase()) {
            return candidate;
        }
        n += 1;
    }
}

/// "2026-11-20 0815 - Subject" for an item.
pub fn item_base_name(it: &ItemSummary) -> String {
    let date = it.when.map(|d| d.format("%Y-%m-%d %H%M").to_string()).unwrap_or_else(|| "undated".into());
    let contact = it.class.to_ascii_uppercase().starts_with("IPM.CONTACT");
    let title = if contact && !it.from.trim().is_empty() {
        it.from.clone()
    } else if it.subject.trim().is_empty() {
        if it.from.trim().is_empty() { "(no subject)".to_string() } else { it.from.clone() }
    } else {
        it.subject.clone()
    };
    format!("{date} - {}", safe_name(&title, 70, "(no subject)"))
}

fn kind_of(class: &str) -> &'static str {
    let c = class.to_ascii_uppercase();
    if c.starts_with("IPM.APPOINTMENT") || c.starts_with("IPM.SCHEDULE") {
        "Event"
    } else if c.starts_with("IPM.CONTACT") || c.starts_with("IPM.DISTLIST") {
        "Contact"
    } else if c.starts_with("IPM.TASK") {
        "Task"
    } else if c.starts_with("IPM.POST") {
        "Post"
    } else if c.starts_with("IPM.STICKYNOTE") {
        "Note"
    } else {
        "Message"
    }
}

fn csv(fields: &[&str]) -> String {
    fields
        .iter()
        .map(|f| {
            let f = f.replace(['\r', '\n'], " ");
            if f.contains([',', '"']) || f.starts_with(['=', '+', '-', '@']) {
                format!("\"{}\"", f.replace('"', "\"\""))
            } else {
                f
            }
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// Keep Windows awake (not the screen) while exporting.
fn keep_awake(on: bool) {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::Power::{SetThreadExecutionState, ES_CONTINUOUS, ES_SYSTEM_REQUIRED};
        SetThreadExecutionState(if on { ES_CONTINUOUS | ES_SYSTEM_REQUIRED } else { ES_CONTINUOUS });
    }
    let _ = on;
}

/// Run the whole export on the calling thread, updating `status`.
/// start: export just this folder (and its subfolders); None = everything.
pub fn run(backend: Arc<dyn Backend>, dest: PathBuf, attachments: bool, start: Option<FolderInfo>, status: Arc<Mutex<ExportStatus>>, cancel: Arc<AtomicBool>, repaint: impl Fn()) {
    {
        let mut s = status.lock().unwrap();
        *s = ExportStatus { running: true, dest: dest.clone(), started: Some(Instant::now()), ..Default::default() };
    }
    keep_awake(true);
    let result = export_all(&*backend, &dest, attachments, start, &status, &cancel, &repaint);
    keep_awake(false);
    let mut s = status.lock().unwrap();
    s.running = false;
    s.done = true;
    s.cancelled = cancel.load(Ordering::Relaxed);
    s.took = s.started.map(|t| t.elapsed());
    s.current.clear();
    if let Err(e) = result {
        s.fatal = Some(e);
    }
    drop(s);
    repaint();
}

fn export_all(
    b: &dyn Backend,
    dest: &Path,
    attachments: bool,
    start: Option<FolderInfo>,
    status: &Mutex<ExportStatus>,
    cancel: &AtomicBool,
    repaint: &dyn Fn(),
) -> Result<(), String> {
    std::fs::create_dir_all(dest).map_err(|e| format!("Couldn't create {}: {e}", dest.display()))?;
    let mut manifest = std::fs::File::create(dest.join("_manifest.csv")).map_err(|e| format!("Couldn't write the manifest: {e}"))?;
    // UTF-8 byte order mark so Excel shows accents correctly.
    let _ = manifest.write_all(b"\xEF\xBB\xBF");
    let _ = writeln!(manifest, "{}", csv(&["Folder", "File", "Type", "Subject", "From", "Date", "Attachments saved", "Status"]));
    let mut log = std::fs::OpenOptions::new().create(true).append(true).open(dest.join("_export-log.txt")).map_err(|e| e.to_string())?;
    let _ = writeln!(log, "\n=== Export started {} from {} ===", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"), b.label());
    let problem = |log: &mut std::fs::File, msg: String| {
        let _ = writeln!(log, "{msg}");
        let mut s = status.lock().unwrap();
        s.last_problem = msg;
    };

    // Folders still to do: (folder, its relative path).
    let mut todo: Vec<(FolderInfo, PathBuf)> = Vec::new();
    let roots = match start {
        Some(f) => vec![f],
        None => b.root_folders()?,
    };
    let mut used = HashSet::new();
    for f in roots.into_iter().rev() {
        let name = unique(&mut used, &safe_name(&f.name, 60, "Folder"), "");
        todo.push((f, PathBuf::from(name)));
    }
    status.lock().unwrap().folders_found = todo.len() as u32;

    while let Some((folder, rel)) = todo.pop() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let dir = dest.join(&rel);
        let rel_text = rel.to_string_lossy().replace('/', "\\");
        status.lock().unwrap().current = rel_text.clone();
        repaint();
        if let Err(e) = std::fs::create_dir_all(&dir) {
            problem(&mut log, format!("[folder] {rel_text}: couldn't create it: {e}"));
            continue;
        }
        // Subfolders first go on the list (kept in name order).
        let mut used_names: HashSet<String> = HashSet::new();
        if folder.child_count > 0 {
            match b.child_folders(&folder.id) {
                Ok(kids) => {
                    let mut add = Vec::new();
                    for k in kids {
                        let name = unique(&mut used_names, &safe_name(&k.name, 60, "Folder"), "");
                        add.push((k, rel.join(name)));
                    }
                    status.lock().unwrap().folders_found += add.len() as u32;
                    todo.extend(add.into_iter().rev());
                }
                Err(e) => problem(&mut log, format!("[folder] {rel_text}: couldn't list subfolders: {e}")),
            }
        }
        // Then this folder's items.
        let items = match b.all_items(&folder) {
            Ok(v) => v,
            Err(e) => {
                problem(&mut log, format!("[folder] {rel_text}: couldn't list items: {e}"));
                status.lock().unwrap().folders_done += 1;
                continue;
            }
        };
        status.lock().unwrap().items_found += items.len() as u64;
        repaint();
        let ext = b.message_ext();
        for it in &items {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            let mut base = unique(&mut used_names, &item_base_name(it), "");
            // Stay well inside Windows' 260-character path limit.
            let room = 245usize.saturating_sub(dir.to_string_lossy().chars().count() + 1 + ext.len() + 1 + " - attachments".len() + 30);
            if base.chars().count() > room.max(20) {
                let short: String = base.chars().take(room.max(20)).collect();
                base = unique(&mut used_names, short.trim_end(), "");
            }
            let file = format!("{base}.{ext}");
            let kind = kind_of(&it.class);
            let date = it.when.map(|d| d.format("%Y-%m-%d %H:%M").to_string()).unwrap_or_default();
            let already = std::fs::metadata(dir.join(&file)).map(|m| m.len() > 0).unwrap_or(false);
            let (state, n_att) = if already {
                status.lock().unwrap().skipped += 1;
                ("Already saved".to_string(), String::new())
            } else {
                match b.export_item(&folder.id, &it.id, &dir, &base, attachments && it.has_attachments) {
                    Ok(r) => {
                        for w in &r.warnings {
                            problem(&mut log, format!("[item] {rel_text}\\{file}: {w}"));
                        }
                        let mut s = status.lock().unwrap();
                        s.saved += 1;
                        s.attachments += r.attachments as u64;
                        (if r.warnings.is_empty() { "Saved".into() } else { "Saved with problems".into() }, r.attachments.to_string())
                    }
                    Err(e) => {
                        problem(&mut log, format!("[item] {rel_text}\\{file}: {e}"));
                        status.lock().unwrap().failed += 1;
                        (format!("Failed: {e}"), String::new())
                    }
                }
            };
            let _ = writeln!(manifest, "{}", csv(&[&rel_text, &file, kind, &it.subject, &it.from, &date, &n_att, &state]));
            repaint();
        }
        let _ = manifest.flush();
        status.lock().unwrap().folders_done += 1;
    }
    let s = status.lock().unwrap().clone();
    let _ = writeln!(
        log,
        "=== {} : {} saved, {} already there, {} failed, {} attachments ===",
        if cancel.load(Ordering::Relaxed) { "Stopped" } else { "Finished" },
        s.saved,
        s.skipped,
        s.failed,
        s.attachments
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_safe() {
        assert_eq!(safe_name("Re: Q3 / Q4 plan?", 70, "x"), "Re_ Q3 _ Q4 plan_");
        assert_eq!(safe_name("CON", 70, "x"), "CON_");
        assert_eq!(safe_name("  ...  ", 70, "x"), "x");
        assert_eq!(safe_name("ends with dot.", 70, "x"), "ends with dot");
        let mut used = HashSet::new();
        assert_eq!(unique_file_name(&mut used, "report.pdf", "a"), "report.pdf");
        assert_eq!(unique_file_name(&mut used, "Report.pdf", "a"), "Report (2).pdf");
        assert_eq!(csv(&["a,b", "say \"hi\"", "=1+1"]), "\"a,b\",\"say \"\"hi\"\"\",\"=1+1\"");
    }

    #[test]
    fn exports_the_demo_folders() {
        let dir = std::env::temp_dir().join(format!("pf-export-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let b: Arc<dyn Backend> = Arc::new(crate::demo::Demo::new());
        let status = Arc::new(Mutex::new(ExportStatus::default()));
        run(b.clone(), dir.clone(), true, None, status.clone(), Arc::new(AtomicBool::new(false)), || {});
        let s = status.lock().unwrap().clone();
        assert!(s.done && s.fatal.is_none(), "{:?}", s.fatal);
        assert_eq!(s.failed, 0);
        assert!(s.saved >= 20, "saved {}", s.saved);
        assert!(s.attachments >= 5);
        assert!(dir.join("Company Announcements").is_dir());
        assert!(dir.join("IT Help Desk").join("Archive 2025").is_dir());
        let manifest = std::fs::read_to_string(dir.join("_manifest.csv")).unwrap();
        assert_eq!(manifest.lines().count() as u64, 1 + s.saved);
        // A second run skips everything already saved.
        run(b, dir.clone(), true, None, status.clone(), Arc::new(AtomicBool::new(false)), || {});
        let s2 = status.lock().unwrap().clone();
        assert_eq!(s2.saved, 0);
        assert_eq!(s2.skipped, s.saved);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
