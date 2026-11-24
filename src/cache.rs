//! A saved copy of the public folders on this PC.
//!
//! Why: classic Outlook doesn't keep public folders offline, and it answers
//! other programs one request at a time on its own busy thread, so every
//! folder or list can take seconds. This layer keeps the whole folder tree,
//! every folder's item list and the items already opened in a file under
//! %APPDATA%\PublicFolderViewer\cache, shows them instantly, and refreshes
//! them in the background ("mapping"). Requests from the person always go
//! before background ones (see `is_background`).

use crate::model::*;
use serde::{Deserialize, Serialize};
use std::cell::Cell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

thread_local! {
    static BACKGROUND: Cell<bool> = const { Cell::new(false) };
}

/// True on the background mapping thread: its Outlook requests wait behind
/// anything the person asked for.
pub fn is_background() -> bool {
    BACKGROUND.with(|b| b.get())
}

const PAGE: usize = 100;
const MAX_DETAILS: usize = 5000;
const FORMAT: u32 = 1;

#[derive(Clone, Serialize, Deserialize)]
struct Stamped<T> {
    at: i64,
    v: T,
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

#[derive(Default, Serialize, Deserialize)]
struct Data {
    format: u32,
    /// Subfolders of each folder ("" = top level).
    children: HashMap<String, Stamped<Vec<FolderInfo>>>,
    /// Every item in a folder, newest first.
    items: HashMap<String, Stamped<Vec<ItemSummary>>>,
    /// Items already opened.
    details: HashMap<String, ItemDetail>,
    detail_order: VecDeque<String>,
    /// When the last full mapping finished.
    mapped_at: Option<i64>,
}

#[derive(Clone, Debug, Default)]
pub struct MapStatus {
    pub running: bool,
    pub phase: String,
    pub folders_done: u32,
    pub folders_known: u32,
    pub lists_done: u32,
    pub lists_total: u32,
    pub mapped_at: Option<i64>,
    /// Goes up whenever the saved folder tree changes.
    pub tree_version: u64,
}

/// A folder with its path, for finding folders by name.
#[derive(Clone, Debug)]
pub struct FolderPath {
    pub folder: FolderInfo,
    /// Ids from the top level down to this folder (inclusive).
    pub ids: Vec<String>,
    pub path: String,
}

pub struct Cached {
    inner: Arc<dyn Backend>,
    data: Mutex<Data>,
    path: PathBuf,
    dirty: AtomicBool,
    last_save: Mutex<Instant>,
    status: Mutex<MapStatus>,
    /// Folders the person opened whose full list isn't saved yet.
    wanted: Mutex<VecDeque<FolderInfo>>,
    /// Bumped to stop a running mapping pass (new pass or disconnect).
    pass: AtomicU64,
}

fn cache_dir() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("PublicFolderViewer").join("cache"))
}

fn file_for(account: &str) -> PathBuf {
    let safe: String = account.chars().map(|c| if c.is_ascii_alphanumeric() || "-_.@".contains(c) { c } else { '_' }).take(120).collect();
    cache_dir().unwrap_or_else(std::env::temp_dir).join(format!("{safe}.json"))
}

impl Cached {
    /// Wrap a live connection, loading what was saved for this account.
    pub fn open(inner: Arc<dyn Backend>, account: &str) -> Arc<Cached> {
        let path = file_for(account);
        let mut data: Data = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        if data.format != FORMAT {
            data = Data { format: FORMAT, ..Default::default() };
        }
        let status = MapStatus { mapped_at: data.mapped_at, ..Default::default() };
        Arc::new(Cached {
            inner,
            data: Mutex::new(data),
            path,
            dirty: AtomicBool::new(false),
            last_save: Mutex::new(Instant::now()),
            status: Mutex::new(status),
            wanted: Mutex::new(VecDeque::new()),
            pass: AtomicU64::new(0),
        })
    }

    /// The live connection, without the saved copy (export uses it, so it
    /// always sees exactly what's on the server).
    pub fn inner(&self) -> Arc<dyn Backend> {
        self.inner.clone()
    }

    pub fn status(&self) -> MapStatus {
        self.status.lock().unwrap().clone()
    }

    /// Subfolders from the saved copy only (instant; None if not saved).
    pub fn saved_children(&self, folder_id: &str) -> Option<Vec<FolderInfo>> {
        self.data.lock().unwrap().children.get(folder_id).map(|s| s.v.clone())
    }

    pub fn has_saved_items(&self, folder_id: &str) -> bool {
        self.data.lock().unwrap().items.contains_key(folder_id)
    }

    /// Every saved folder with its path, top level first.
    pub fn all_folders(&self) -> Vec<FolderPath> {
        let d = self.data.lock().unwrap();
        let mut out = Vec::new();
        let mut stack: Vec<(String, Vec<String>, String)> = vec![(String::new(), vec![], String::new())];
        let mut seen = HashSet::new();
        while let Some((id, ids, path)) = stack.pop() {
            if !seen.insert(id.clone()) {
                continue;
            }
            if let Some(kids) = d.children.get(&id) {
                for k in kids.v.iter().rev() {
                    let mut kid_ids = ids.clone();
                    kid_ids.push(k.id.clone());
                    let kid_path = if path.is_empty() { k.name.clone() } else { format!("{path} › {}", k.name) };
                    out.push(FolderPath { folder: k.clone(), ids: kid_ids.clone(), path: kid_path.clone() });
                    stack.push((k.id.clone(), kid_ids, kid_path));
                }
            }
        }
        out.sort_by(|a, b| a.path.to_lowercase().cmp(&b.path.to_lowercase()));
        out
    }

    fn put_children(&self, id: &str, v: Vec<FolderInfo>) -> bool {
        let mut d = self.data.lock().unwrap();
        let changed = d.children.get(id).map(|s| s.v != v).unwrap_or(true);
        d.children.insert(id.to_string(), Stamped { at: now(), v });
        drop(d);
        self.dirty.store(true, Ordering::Relaxed);
        if changed {
            self.status.lock().unwrap().tree_version += 1;
        }
        self.save_soon();
        changed
    }

    fn put_items(&self, id: &str, v: Vec<ItemSummary>) {
        self.data.lock().unwrap().items.insert(id.to_string(), Stamped { at: now(), v });
        self.dirty.store(true, Ordering::Relaxed);
        self.save_soon();
    }

    fn put_detail(&self, d: ItemDetail) {
        let mut data = self.data.lock().unwrap();
        let id = d.id.clone();
        if data.details.insert(id.clone(), d).is_none() {
            data.detail_order.push_back(id);
            while data.detail_order.len() > MAX_DETAILS {
                if let Some(old) = data.detail_order.pop_front() {
                    data.details.remove(&old);
                }
            }
        }
        drop(data);
        self.dirty.store(true, Ordering::Relaxed);
        self.save_soon();
    }

    /// Save at most every few seconds while things change.
    fn save_soon(&self) {
        let due = self.last_save.lock().unwrap().elapsed() > Duration::from_secs(5);
        if due {
            self.save();
        }
    }

    /// Write the saved copy to disk (safely: temp file, then rename).
    pub fn save(&self) {
        if !self.dirty.swap(false, Ordering::Relaxed) {
            return;
        }
        *self.last_save.lock().unwrap() = Instant::now();
        let bytes = {
            let d = self.data.lock().unwrap();
            serde_json::to_vec(&*d)
        };
        let Ok(bytes) = bytes else { return };
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = self.path.with_extension("tmp");
        if std::fs::write(&tmp, bytes).is_ok() {
            let _ = std::fs::rename(&tmp, &self.path);
        }
    }

    /// The folder's contents changed: list them again next time.
    pub fn forget_items(&self, folder_id: &str) {
        self.data.lock().unwrap().items.remove(folder_id);
        self.dirty.store(true, Ordering::Relaxed);
    }

    /// Stop any running mapping pass.
    pub fn stop(&self) {
        self.pass.fetch_add(1, Ordering::Relaxed);
        self.save();
    }

    /// Map everything in the background: the whole folder tree, then (if
    /// `lists`) every folder's item list. Folders the person opens go first.
    pub fn start_mapping(self: &Arc<Self>, lists: bool) {
        let me = self.clone();
        let pass = self.pass.fetch_add(1, Ordering::Relaxed) + 1;
        std::thread::spawn(move || {
            BACKGROUND.with(|b| b.set(true));
            me.map_all(pass, lists);
            // Afterwards keep serving folders the person opens.
            while me.pass.load(Ordering::Relaxed) == pass {
                me.serve_wanted(&|| me.pass.load(Ordering::Relaxed) == pass);
                std::thread::sleep(Duration::from_millis(300));
            }
        });
    }

    /// Replace the saved tree with a complete one (one change, one save).
    fn put_tree(&self, tree: HashMap<String, Vec<FolderInfo>>) -> Vec<FolderInfo> {
        let all: Vec<FolderInfo> = tree.values().flatten().cloned().collect();
        let mut d = self.data.lock().unwrap();
        let t = now();
        let mut changed = false;
        let mut fresh: HashMap<String, Stamped<Vec<FolderInfo>>> = HashMap::new();
        for (k, v) in tree {
            changed |= d.children.get(&k).map(|s| s.v != v).unwrap_or(true);
            fresh.insert(k, Stamped { at: t, v });
        }
        for f in &all {
            fresh.entry(f.id.clone()).or_insert(Stamped { at: t, v: Vec::new() });
        }
        changed |= fresh.len() != d.children.len();
        d.children = fresh;
        drop(d);
        self.dirty.store(true, Ordering::Relaxed);
        if changed {
            self.status.lock().unwrap().tree_version += 1;
        }
        self.save();
        all
    }

    fn map_all(&self, pass: u64, lists: bool) {
        let live = || self.pass.load(Ordering::Relaxed) == pass;
        {
            let mut s = self.status.lock().unwrap();
            s.running = true;
            s.phase = "Mapping folders".into();
            s.folders_done = 0;
            s.folders_known = 1;
            s.lists_done = 0;
            s.lists_total = 0;
        }
        // 1. The folder tree: in one request if the connection can (MAPI),
        //    otherwise folder by folder.
        let mut all: Vec<FolderInfo> = Vec::new();
        let mut queue: VecDeque<String> = VecDeque::from([String::new()]);
        if let Some(Ok(tree)) = self.inner.whole_tree() {
            all = self.put_tree(tree);
            queue.clear();
            let mut s = self.status.lock().unwrap();
            s.folders_done = all.len() as u32;
            s.folders_known = all.len() as u32;
        }
        let mut seen = HashSet::new();
        while let Some(id) = queue.pop_front() {
            if !live() {
                return;
            }
            self.serve_wanted(&live);
            if !seen.insert(id.clone()) {
                continue;
            }
            let kids = if id.is_empty() { self.inner.root_folders() } else { self.inner.child_folders(&id) };
            if let Ok(kids) = kids {
                for k in &kids {
                    if k.child_count > 0 {
                        queue.push_back(k.id.clone());
                    }
                    all.push(k.clone());
                }
                self.put_children(&id, kids);
            }
            let mut s = self.status.lock().unwrap();
            s.folders_done += 1;
            s.folders_known = s.folders_done + queue.len() as u32;
        }
        // Folders with no subfolders have an empty list (so they show as leaves).
        for f in all.iter().filter(|f| f.child_count == 0) {
            let mut d = self.data.lock().unwrap();
            d.children.entry(f.id.clone()).or_insert(Stamped { at: now(), v: Vec::new() });
        }
        // 2. Every folder's item list, smallest first so most folders are ready soon.
        let mut with_items: Vec<FolderInfo> = if lists { all.into_iter().filter(|f| f.total_count > 0).collect() } else { Vec::new() };
        with_items.sort_by_key(|f| f.total_count);
        {
            let mut s = self.status.lock().unwrap();
            s.phase = "Saving item lists".into();
            s.lists_total = with_items.len() as u32;
        }
        for f in with_items {
            if !live() {
                return;
            }
            self.serve_wanted(&live);
            if let Ok(v) = self.inner.all_items(&f) {
                self.put_items(&f.id, v);
            }
            self.status.lock().unwrap().lists_done += 1;
        }
        let t = now();
        self.data.lock().unwrap().mapped_at = Some(t);
        self.dirty.store(true, Ordering::Relaxed);
        self.save();
        let mut s = self.status.lock().unwrap();
        s.running = false;
        s.phase.clear();
        s.mapped_at = Some(t);
        s.folders_known = s.folders_done.max(s.folders_known);
    }

    /// Fetch full lists for folders the person just opened.
    fn serve_wanted(&self, live: &dyn Fn() -> bool) {
        loop {
            if !live() {
                return;
            }
            let Some(f) = self.wanted.lock().unwrap().pop_front() else { return };
            if self.has_saved_items(&f.id) {
                continue;
            }
            if let Ok(v) = self.inner.all_items(&f) {
                self.put_items(&f.id, v);
            }
        }
    }
}

impl Backend for Cached {
    fn label(&self) -> String {
        self.inner.label()
    }

    fn root_folders(&self) -> Result<Vec<FolderInfo>, String> {
        self.child_folders("")
    }

    fn child_folders(&self, folder_id: &str) -> Result<Vec<FolderInfo>, String> {
        if let Some(v) = self.saved_children(folder_id) {
            return Ok(v);
        }
        let v = if folder_id.is_empty() { self.inner.root_folders()? } else { self.inner.child_folders(folder_id)? };
        self.put_children(folder_id, v.clone());
        Ok(v)
    }

    fn items(&self, folder: &FolderInfo, offset: u32) -> Result<ItemPage, String> {
        let saved = self.data.lock().unwrap().items.get(&folder.id).map(|s| s.v.clone());
        if let Some(all) = saved {
            let start = (offset as usize).min(all.len());
            let end = (start + PAGE).min(all.len());
            let items = all[start..end]
                .iter()
                .cloned()
                .map(|mut it| {
                    // Dates like "14:05" depend on today, so redo them.
                    if let Some(w) = it.when {
                        if !it.date.is_empty() {
                            it.date = fmt_list_date(&w);
                        }
                    }
                    it
                })
                .collect();
            return Ok(ItemPage { items, total: all.len() as u32, more: end < all.len(), next_offset: end as u32 });
        }
        let page = self.inner.items(folder, offset)?;
        if offset == 0 && !page.more {
            self.put_items(&folder.id, page.items.clone());
        } else {
            // Ask the background mapping to save the whole list soon.
            let mut w = self.wanted.lock().unwrap();
            if !w.iter().any(|f| f.id == folder.id) {
                w.push_back(folder.clone());
            }
        }
        Ok(page)
    }

    fn item(&self, folder_id: &str, item_id: &str) -> Result<ItemDetail, String> {
        if let Some(d) = self.data.lock().unwrap().details.get(item_id) {
            return Ok(d.clone());
        }
        let d = self.inner.item(folder_id, item_id)?;
        self.put_detail(d.clone());
        Ok(d)
    }

    fn attachment(&self, folder_id: &str, attachment_id: &str) -> Result<Vec<u8>, String> {
        self.inner.attachment(folder_id, attachment_id)
    }

    fn mime(&self, folder_id: &str, item_id: &str) -> Result<Vec<u8>, String> {
        self.inner.mime(folder_id, item_id)
    }

    fn message_ext(&self) -> &'static str {
        self.inner.message_ext()
    }

    fn all_items(&self, folder: &FolderInfo) -> Result<Vec<ItemSummary>, String> {
        self.inner.all_items(folder)
    }

    fn export_item(&self, folder_id: &str, item_id: &str, dir: &std::path::Path, base: &str, attachments: bool) -> Result<Exported, String> {
        self.inner.export_item(folder_id, item_id, dir, base, attachments)
    }

    fn upload(&self, folder: &FolderInfo, path: &std::path::Path) -> Result<(), String> {
        let r = self.inner.upload(folder, path);
        self.forget_items(&folder.id);
        r
    }

    fn forget(&self, folder_id: &str) {
        let mut d = self.data.lock().unwrap();
        d.children.remove(folder_id);
        d.items.remove(folder_id);
        drop(d);
        self.dirty.store(true, Ordering::Relaxed);
    }

    fn clear_cache(&self) {
        self.inner.clear_cache();
    }
}

#[cfg(all(test, windows))]
mod live_tests {
    use super::*;

    /// Live: map the real public folders through classic Outlook, check a
    /// click gets in ahead of the mapping, then reopen from the saved copy.
    /// Run with: cargo test live_mapping -- --ignored --nocapture
    #[test]
    #[ignore]
    fn live_mapping() {
        let account = format!("live-test-{}", std::process::id());
        let t = Instant::now();
        let c = Cached::open(crate::classic::connect().expect("connect"), &account);
        println!("connected in {:?}", t.elapsed());
        let t = Instant::now();
        c.start_mapping(true);
        std::thread::sleep(Duration::from_secs(3));
        let probe = Instant::now();
        let top = c.inner().root_folders().expect("foreground request");
        println!("a click during mapping was answered in {:?} ({} folders)", probe.elapsed(), top.len());
        loop {
            std::thread::sleep(Duration::from_secs(2));
            let s = c.status();
            if !s.running && s.mapped_at.is_some() {
                println!("mapped {} folders and {} item lists in {:?}", s.folders_done, s.lists_done, t.elapsed());
                break;
            }
            if t.elapsed() > Duration::from_secs(1500) {
                println!("still mapping after 25 min: {s:?}");
                break;
            }
        }
        c.stop();
        drop(c);
        let t = Instant::now();
        let live = crate::classic::connect().expect("connect");
        println!("reconnected to Outlook in {:?}", t.elapsed());
        let t = Instant::now();
        let again = Cached::open(live, &account);
        let _ = again.root_folders().unwrap();
        let all = again.all_folders();
        println!("from the saved copy: tree of {} folders in {:?}", all.len(), t.elapsed());
        // Open a few folders the person might click (live through MAPI, then saved).
        let t = Instant::now();
        let mut listed = 0;
        for fp in all.iter().take(40) {
            listed += again.items(&fp.folder, 0).map(|p| p.total).unwrap_or(0);
        }
        println!("opened 40 folders ({listed} items) in {:?}", t.elapsed());
        let t = Instant::now();
        for fp in all.iter().take(40) {
            let _ = again.items(&fp.folder, 0);
        }
        println!("the same 40 folders again: {:?}", t.elapsed());
        let size = std::fs::metadata(file_for(&account)).map(|m| m.len()).unwrap_or(0);
        println!("saved copy size: {} KB", size / 1024);
        let _ = std::fs::remove_file(file_for(&account));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_and_remembers_the_demo() {
        let account = format!("test-{}", std::process::id());
        let c = Cached::open(Arc::new(crate::demo::Demo::new()), &account);
        c.map_all(c.pass.load(Ordering::Relaxed), true);
        let s = c.status();
        assert!(!s.running && s.mapped_at.is_some());
        let all = c.all_folders();
        assert!(all.iter().any(|f| f.path == "IT Help Desk › Archive 2025"));
        assert!(c.has_saved_items("ann"));
        c.save();
        // A fresh start reads the saved copy without asking the server.
        let again = Cached::open(Arc::new(crate::demo::Demo::new()), &account);
        assert_eq!(again.saved_children("").map(|v| v.len()), Some(6));
        let page = again.items(&FolderInfo { id: "ann".into(), ..Default::default() }, 0).unwrap();
        assert_eq!(page.items.len(), 6);
        let _ = std::fs::remove_file(file_for(&account));
    }
}
