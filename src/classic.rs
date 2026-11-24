//! Public folders through classic Outlook on this PC.
//!
//! Classic Outlook talks to Exchange with MAPI (MAPI over HTTP, or RPC over
//! HTTP on older servers), a protocol Microsoft publishes. Rather than
//! re-implementing MAPI, this backend asks classic Outlook itself, through
//! its official automation interface (the Outlook object model), and so
//! reuses Outlook's existing profile and sign-in: no login details needed.
//!
//! Speed: every request to Outlook crosses into the Outlook program, and on
//! some PCs each one takes a tenth to half a second. So this asks as little
//! as possible: a whole page of items comes from one Table request, a
//! folder's details from one GetProperties request, member names are looked
//! up once, and answers are remembered until Refresh.

use crate::model::*;

/// Is classic Outlook installed here?
pub fn is_installed() -> bool {
    #[cfg(windows)]
    {
        crate::com::is_registered("Outlook.Application")
    }
    #[cfg(not(windows))]
    {
        false
    }
}

#[cfg(all(test, windows))]
mod tests {
    /// Live, read-only check against classic Outlook on this PC.
    /// Run with: cargo test classic -- --ignored --nocapture
    #[test]
    #[ignore]
    fn lists_public_folders_through_classic_outlook() {
        let calls = crate::com::calls;
        let t = std::time::Instant::now();
        let b = super::connect().expect("connect");
        println!("connected in {:?}", t.elapsed());
        let (t, c) = (std::time::Instant::now(), calls());
        let root = b.root_folders().expect("root folders");
        println!("root: {} folders in {:?}, {} calls", root.len(), t.elapsed(), calls() - c);
        let mut all = root.clone();
        for f in root.iter().filter(|f| f.child_count > 0) {
            let (t, c) = (std::time::Instant::now(), calls());
            let kids = b.child_folders(&f.id).expect("subfolders");
            println!("subfolders: {} in {:?}, {} calls", kids.len(), t.elapsed(), calls() - c);
            all.extend(kids);
        }
        println!("folders with items: {}", all.iter().filter(|f| f.total_count > 0).count());
        if let Some(f) = all.iter().max_by_key(|f| f.total_count) {
            let (t, c) = (std::time::Instant::now(), calls());
            let page = b.items(f, 0).expect("items");
            println!("largest folder: {} items, listed {} in {:?}, {} calls", page.total, page.items.len(), t.elapsed(), calls() - c);
            let it = &page.items[0];
            println!(
                "first row: id {} chars, subject {} chars, from {} chars, date {:?}",
                it.id.len(),
                it.subject.len(),
                it.from.len(),
                !it.date.is_empty()
            );
            let (t, c) = (std::time::Instant::now(), calls());
            let page2 = b.items(f, page.next_offset).expect("page 2");
            println!("page 2: listed {} in {:?}, {} calls", page2.items.len(), t.elapsed(), calls() - c);
            let (t, c) = (std::time::Instant::now(), calls());
            let d = b.item(&f.id, &it.id).expect("item");
            println!(
                "opened an item: subject {} chars, body {} chars, date {:?}, in {:?}, {} calls",
                d.subject.len(),
                d.body.len(),
                !d.date.is_empty(),
                t.elapsed(),
                calls() - c
            );
            let (t, c) = (std::time::Instant::now(), calls());
            let _ = b.items(f, 0).expect("cached");
            println!("same folder again: {:?}, {} calls", t.elapsed(), calls() - c);
        }
    }
}

#[cfg(all(test, windows))]
mod export_tests {
    /// Live check: save a few items from one folder to a temp folder.
    /// Run with: cargo test classic_export -- --ignored --nocapture
    #[test]
    #[ignore]
    fn classic_export_saves_msg_files() {
        let b = super::connect().expect("connect");
        let mut all = b.root_folders().unwrap();
        for f in all.clone().iter().filter(|f| f.child_count > 0) {
            all.extend(b.child_folders(&f.id).unwrap());
        }
        let f = all.iter().filter(|f| f.total_count > 0).min_by_key(|f| f.total_count).expect("a folder with items").clone();
        let t = std::time::Instant::now();
        let items = b.all_items(&f).unwrap();
        println!("all_items: {} of {} in {:?}", items.len(), f.total_count, t.elapsed());
        let dir = std::env::temp_dir().join(format!("pf-classic-export-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Also one item with attachments from the largest folder.
        let big = all.iter().max_by_key(|f| f.total_count).unwrap().clone();
        let t = std::time::Instant::now();
        let big_items = b.all_items(&big).unwrap();
        println!("all_items: {} of {} in {:?}", big_items.len(), big.total_count, t.elapsed());
        let mut picks: Vec<(String, super::ItemSummary)> = items.iter().take(2).map(|i| (f.id.clone(), i.clone())).collect();
        let mut with_att = 0;
        for g in all.iter().filter(|g| g.total_count > 0) {
            let list = b.all_items(g).unwrap_or_default();
            with_att += list.iter().filter(|i| i.has_attachments).count();
            if let Some(it) = list.iter().find(|i| i.has_attachments) {
                if picks.len() < 3 {
                    picks.push((g.id.clone(), it.clone()));
                }
            }
        }
        println!("items with attachments across folders: {with_att}");
        for (i, (fid, it)) in picks.iter().enumerate() {
            let f = super::FolderInfo { id: fid.clone(), ..Default::default() };
            let t = std::time::Instant::now();
            let base = format!("item {i}");
            let r = b.export_item(&f.id, &it.id, &dir, &base, it.has_attachments).expect("export");
            let size = std::fs::metadata(dir.join(&r.file)).map(|m| m.len()).unwrap_or(0);
            println!("saved {} ({size} bytes), {} attachments, {} warnings, in {:?}", r.file, r.attachments, r.warnings.len(), t.elapsed());
            assert!(size > 0);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(not(windows))]
pub fn connect() -> Result<std::sync::Arc<dyn Backend>, String> {
    Err("Classic Outlook only exists on Windows.".into())
}

#[cfg(windows)]
pub use imp::connect;

#[cfg(windows)]
mod imp {
    use super::*;
    use crate::com::{init_thread, Arg, Disp, Var};
    use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, TimeZone, Utc};
    use std::collections::{HashMap, VecDeque};
    use std::sync::mpsc::channel;
    use std::sync::{Arc, Condvar, Mutex};

    const OL_PUBLIC_FOLDERS_ALL: i32 = 18;
    const OL_MSG_UNICODE: i32 = 9;
    const PAGE: i32 = 100;

    fn tag(t: &str) -> String {
        format!("http://schemas.microsoft.com/mapi/proptag/{t}")
    }

    // Folder properties fetched in one request.
    const FOLDER_TAGS: [&str; 5] = [
        "0x0FFF0102", // PR_ENTRYID
        "0x3001001F", // PR_DISPLAY_NAME
        "0x36020003", // PR_CONTENT_COUNT
        "0x360A000B", // PR_SUBFOLDERS
        "0x3613001F", // PR_CONTAINER_CLASS
    ];

    // Message properties fetched in one request (dates are UTC).
    const ITEM_TAGS: [&str; 10] = [
        "0x0037001F", // PR_SUBJECT
        "0x0C1A001F", // PR_SENDER_NAME
        "0x0C1F001F", // PR_SENDER_EMAIL_ADDRESS
        "0x0E04001F", // PR_DISPLAY_TO
        "0x0E03001F", // PR_DISPLAY_CC
        "0x00390040", // PR_CLIENT_SUBMIT_TIME
        "0x0E060040", // PR_MESSAGE_DELIVERY_TIME
        "0x001A001F", // PR_MESSAGE_CLASS
        "0x1000001F", // PR_BODY
        "0x0E1B000B", // PR_HASATTACH
    ];

    struct Session {
        ns: Disp,
        root: Disp,
        store_id: String,
        _app: Disp,
        queue: Arc<Queue>,
        /// True while a background job runs (so it can step aside).
        in_background: std::cell::Cell<bool>,
    }

    /// Called often inside long background jobs: run anything the person
    /// asked for right now, then carry on.
    fn step_aside(s: &Session) {
        if !s.in_background.get() {
            return;
        }
        s.in_background.set(false);
        while let Some(job) = s.queue.pop_foreground() {
            job(s);
        }
        s.in_background.set(true);
    }

    type Job = Box<dyn FnOnce(&Session) + Send>;

    /// Two queues: what the person asked for goes first, background
    /// mapping (see cache.rs) only when nothing else is waiting.
    #[derive(Default)]
    struct Queue {
        jobs: Mutex<(VecDeque<Job>, VecDeque<Job>, bool)>, // (foreground, background, closed)
        ready: Condvar,
    }

    impl Queue {
        fn push(&self, job: Job, background: bool) -> Result<(), String> {
            let mut q = self.jobs.lock().unwrap();
            if q.2 {
                return Err("Lost the connection to classic Outlook.".into());
            }
            if background { q.1.push_back(job) } else { q.0.push_back(job) }
            self.ready.notify_one();
            Ok(())
        }

        fn pop_foreground(&self) -> Option<Job> {
            self.jobs.lock().unwrap().0.pop_front()
        }

        /// The next job, and whether it's a background one.
        fn next(&self) -> Option<(Job, bool)> {
            let mut q = self.jobs.lock().unwrap();
            loop {
                if let Some(j) = q.0.pop_front() {
                    return Some((j, false));
                }
                if let Some(j) = q.1.pop_front() {
                    return Some((j, true));
                }
                if q.2 {
                    return None;
                }
                q = self.ready.wait(q).unwrap();
            }
        }

        fn close(&self) {
            self.jobs.lock().unwrap().2 = true;
            self.ready.notify_all();
        }
    }

    /// All Outlook objects live on one worker thread; requests are sent to it.
    pub struct ClassicOutlook {
        queue: Arc<Queue>,
        label: String,
        store_id: String,
        root_id: String,
        /// Direct MAPI (much faster); opened on first use. `Err` = not available.
        mapi: Mutex<Option<Result<Arc<crate::mapi::Mapi>, String>>>,
        /// The whole folder tree from MAPI, once read.
        tree: Mutex<Option<HashMap<String, Vec<FolderInfo>>>>,
    }

    impl ClassicOutlook {
        fn mapi(&self) -> Option<Arc<crate::mapi::Mapi>> {
            let mut m = self.mapi.lock().unwrap();
            if m.is_none() {
                *m = Some(crate::mapi::Mapi::open(&self.store_id, &self.root_id).map(Arc::new));
            }
            m.as_ref().unwrap().as_ref().ok().cloned()
        }

        /// A folder's items through MAPI, when MAPI can show that kind of
        /// folder (calendars and contacts need Outlook for dates and names).
        fn mapi_items(&self, folder: &FolderInfo) -> Option<Vec<ItemSummary>> {
            if folder.class.starts_with("IPF.Appointment") || folder.class.starts_with("IPF.Contact") {
                return None;
            }
            let mapi = self.mapi()?;
            mapi.items(&split_id(&folder.id).0).ok()
        }
    }

    impl Drop for ClassicOutlook {
        fn drop(&mut self) {
            self.queue.close();
        }
    }

    pub fn connect() -> Result<Arc<dyn Backend>, String> {
        if !super::is_installed() {
            return Err("Classic Outlook isn't installed on this computer. Choose “Exchange server” instead.".into());
        }
        let queue = Arc::new(Queue::default());
        let worker_queue = queue.clone();
        let (ready_tx, ready_rx) = channel::<Result<(String, String, String), String>>();
        std::thread::spawn(move || {
            init_thread();
            let session = (|| -> Result<Session, String> {
                let app = Disp::create("Outlook.Application")?.kind("Application");
                let ns = app.call("GetNamespace", &[Arg::Str("MAPI")])?.into_disp().ok_or("Outlook has no mail profile.")?.kind("Namespace");
                let root = ns
                    .call("GetDefaultFolder", &[Arg::I4(OL_PUBLIC_FOLDERS_ALL)])
                    .map_err(|e| format!("Classic Outlook has no public folders for this account ({e})"))?
                    .into_disp()
                    .ok_or("Classic Outlook has no public folders for this account.")?
                    .kind("Folder");
                let store_id = root.text("StoreID");
                Ok(Session { ns, root, store_id, _app: app, queue: worker_queue.clone(), in_background: std::cell::Cell::new(false) })
            })();
            let session = match session {
                Ok(s) => s,
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            let who = session.ns.obj("CurrentUser").map(|u| u.text("Name")).unwrap_or_default();
            let root_id = session.root.text("EntryID");
            let _ = ready_tx.send(Ok((who, session.store_id.clone(), root_id)));
            while let Some((job, background)) = worker_queue.next() {
                session.in_background.set(background);
                job(&session);
                session.in_background.set(false);
            }
        });
        let (who, store_id, root_id) = ready_rx.recv().map_err(|_| "Classic Outlook stopped unexpectedly.".to_string())??;
        let label = if who.is_empty() { "Classic Outlook".to_string() } else { format!("{who} via classic Outlook") };
        Ok(Arc::new(ClassicOutlook { queue, label, store_id, root_id, mapi: Mutex::new(None), tree: Mutex::new(None) }))
    }

    impl ClassicOutlook {
        fn run<T: Send + 'static>(&self, f: impl FnOnce(&Session) -> Result<T, String> + Send + 'static) -> Result<T, String> {
            let (tx, rx) = channel();
            self.queue.push(
                Box::new(move |s| {
                    let _ = tx.send(f(s));
                }),
                crate::cache::is_background(),
            )?;
            rx.recv().map_err(|_| "Lost the connection to classic Outlook.".to_string())?
        }
    }

    /// Folder ids are the EntryID (all public folders share the session's store);
    /// older saved ids may still carry "|StoreID" after it.
    fn split_id(id: &str) -> (String, String) {
        let (e, s) = id.split_once('|').unwrap_or((id, ""));
        (e.to_string(), s.to_string())
    }

    fn folder_by_id(s: &Session, id: &str) -> Result<Disp, String> {
        let (entry, store) = split_id(id);
        let store = if store.is_empty() { s.store_id.clone() } else { store };
        Ok(s.ns
            .call("GetFolderFromID", &[Arg::Str(&entry), Arg::Str(&store)])?
            .into_disp()
            .ok_or_else(|| "Folder not found.".to_string())?
            .kind("Folder"))
    }

    fn item_by_id(s: &Session, folder_id: &str, item_id: &str) -> Result<Disp, String> {
        let (_, store) = split_id(folder_id);
        let store = if store.is_empty() { s.store_id.clone() } else { store };
        Ok(s.ns
            .call("GetItemFromID", &[Arg::Str(item_id), Arg::Str(&store)])?
            .into_disp()
            .ok_or_else(|| "Item not found.".to_string())?
            .kind("Item"))
    }

    fn ole_naive(days: f64) -> Option<NaiveDateTime> {
        // Outlook uses 1 Jan 4501 (about 949,998 days) for "no date".
        if !(1.0..900_000.0).contains(&days) {
            return None;
        }
        let base = NaiveDate::from_ymd_opt(1899, 12, 30)?.and_hms_opt(0, 0, 0)?;
        Some(base + chrono::Duration::milliseconds((days * 86_400_000.0).round() as i64))
    }

    /// An OLE date that is already in local time.
    fn local_date(v: &Var) -> Option<DateTime<Local>> {
        Local.from_local_datetime(&ole_naive(v.to_date()?)?).earliest()
    }

    /// An OLE date in UTC (raw MAPI properties).
    fn utc_date(v: &Var) -> Option<DateTime<Local>> {
        Some(Utc.from_utc_datetime(&ole_naive(v.to_date()?)?).with_timezone(&Local))
    }

    fn prop_date(d: &Disp, name: &str) -> Option<DateTime<Local>> {
        local_date(&d.get(name).ok()?)
    }

    /// Several properties in one round trip; `None` if Outlook refuses.
    fn properties(d: &Disp, tags: &[&str]) -> Option<Vec<Var>> {
        let pa = d.obj("PropertyAccessor").ok()?.kind("PropertyAccessor");
        let names: Vec<String> = tags.iter().map(|t| tag(t)).collect();
        let vals = pa.call("GetProperties", &[Arg::StrArray(&names)]).ok()?.to_vec();
        (vals.len() == tags.len()).then_some(vals)
    }

    fn list_folders(s: &Session, parent: &Disp) -> Result<Vec<FolderInfo>, String> {
        let folders = parent.obj("Folders")?.kind("Folders");
        // All subfolder objects in one request, rather than one per folder.
        let list: Vec<Disp> = match folders.enumerate() {
            Ok(v) if !v.is_empty() => v.into_iter().filter_map(|v| v.into_disp()).collect(),
            _ => (1..=folders.int("Count"))
                .filter_map(|i| folders.call("Item", &[Arg::I4(i as i32)]).ok().and_then(|v| v.into_disp()))
                .collect(),
        };
        let mut out = Vec::new();
        for f in list {
            step_aside(s);
            let f = f.kind("Folder");
            let info = match properties(&f, &FOLDER_TAGS) {
                Some(v) if !v[0].is_error() => FolderInfo {
                    id: v[0].to_hex(),
                    name: v[1].to_text(),
                    class: Some(v[4].to_text()).filter(|c| !c.is_empty()).unwrap_or_else(|| "IPF.Note".into()),
                    child_count: v[3].to_i64().unwrap_or(1).clamp(0, 1) as u32,
                    total_count: v[2].to_i64().unwrap_or(0).max(0) as u32,
                },
                // Slower fallback, one property at a time.
                _ => FolderInfo {
                    id: f.text("EntryID"),
                    name: f.text("Name"),
                    class: "IPF.Note".into(),
                    child_count: 1,
                    total_count: f.obj("Items").map(|c| c.int("Count") as u32).unwrap_or(0),
                },
            };
            out.push(info);
        }
        out.sort_by_key(|f| f.name.to_lowercase());
        Ok(out)
    }

    struct ItemTable {
        table: Disp,
        added: Vec<String>,
        calendar: bool,
        contacts: bool,
        total: u32,
    }

    /// A sorted Outlook table over a folder's items, with just the columns we need.
    fn open_table(s: &Session, folder: &FolderInfo) -> Result<ItemTable, String> {
        let f = folder_by_id(s, &folder.id)?;
        let calendar = folder.class.starts_with("IPF.Appointment");
        let contacts = folder.class.starts_with("IPF.Contact");
        let table = f.call("GetTable", &[])?.into_disp().ok_or("Outlook couldn't list this folder.")?.kind("Table");
        let cols = table.obj("Columns")?.kind("Columns");
        let _ = cols.call("RemoveAll", &[]);
        let hasattach = tag("0x0E1B000B");
        let wanted: Vec<&str> = if calendar {
            vec!["EntryID", "Subject", "MessageClass", "Start", "Location", "SenderName", &hasattach]
        } else if contacts {
            vec!["EntryID", "Subject", "MessageClass", "FullName", "CompanyName", "JobTitle", "Email1Address", "CreationTime", &hasattach]
        } else {
            vec!["EntryID", "Subject", "MessageClass", "SenderName", "ReceivedTime", "CreationTime", &hasattach]
        };
        let mut added = Vec::new();
        for c in wanted {
            if cols.call("Add", &[Arg::Str(c)]).is_ok() {
                added.push(c.to_string());
            }
        }
        let (sort, desc) = if calendar {
            ("[Start]", true)
        } else if contacts {
            ("[FullName]", false)
        } else {
            ("[ReceivedTime]", true)
        };
        let _ = table.call("Sort", &[Arg::Str(sort), Arg::Bool(desc)]);
        let total = table.int("GetRowCount").max(0) as u32;
        Ok(ItemTable { table, added, calendar, contacts, total })
    }

    fn rows_to_items(t: &ItemTable, rows: &[Vec<Var>]) -> Vec<ItemSummary> {
        let pos = |name: &str| t.added.iter().position(|c| c == name || (name == "HASATTACH" && c.ends_with("0x0E1B000B")));
        let col = |row: &[Var], name: &str| pos(name).and_then(|i| row.get(i)).map(|v| v.to_text()).unwrap_or_default();
        let date = |row: &[Var], name: &str| pos(name).and_then(|i| row.get(i)).and_then(local_date);
        let attach = |row: &[Var]| pos("HASATTACH").and_then(|i| row.get(i)).and_then(|v| v.to_i64()).unwrap_or(0) != 0;
        let mut items = Vec::new();
        for row in rows {
            let class = col(row, "MessageClass");
            let summary = if t.contacts {
                ItemSummary {
                    id: col(row, "EntryID"),
                    from: Some(col(row, "FullName")).filter(|n| !n.is_empty()).unwrap_or_else(|| col(row, "Subject")),
                    subject: [col(row, "CompanyName"), col(row, "JobTitle")].into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join(" · "),
                    date: String::new(),
                    class,
                    has_attachments: attach(row),
                    preview: col(row, "Email1Address"),
                    when: date(row, "CreationTime"),
                }
            } else {
                let when = if t.calendar { date(row, "Start") } else { date(row, "ReceivedTime").or_else(|| date(row, "CreationTime")) };
                let preview = if t.calendar {
                    [when.as_ref().map(fmt_long_date).unwrap_or_default(), col(row, "Location")]
                        .into_iter()
                        .filter(|x| !x.is_empty())
                        .collect::<Vec<_>>()
                        .join(" · ")
                } else {
                    String::new()
                };
                ItemSummary {
                    id: col(row, "EntryID"),
                    subject: col(row, "Subject"),
                    from: col(row, "SenderName"),
                    date: when.as_ref().map(fmt_list_date).unwrap_or_default(),
                    class,
                    has_attachments: attach(row),
                    preview,
                    when,
                }
            };
            items.push(summary);
        }
        items
    }

    fn list_items(s: &Session, folder: &FolderInfo, offset: u32) -> Result<ItemPage, String> {
        let t = open_table(s, folder)?;
        if offset > 0 {
            let _ = t.table.call("GetArray", &[Arg::I4(offset as i32)]); // skip earlier pages
        }
        let rows = t.table.call("GetArray", &[Arg::I4(PAGE)])?.to_rows();
        let items = rows_to_items(&t, &rows);
        let next = offset + items.len() as u32;
        Ok(ItemPage { total: t.total, more: next < t.total && !items.is_empty(), next_offset: next, items })
    }

    /// Every item, read from one table in large batches (for export).
    fn list_all(s: &Session, folder: &FolderInfo) -> Result<Vec<ItemSummary>, String> {
        let t = open_table(s, folder)?;
        let mut all = Vec::with_capacity(t.total as usize);
        loop {
            step_aside(s);
            let rows = t.table.call("GetArray", &[Arg::I4(200)])?.to_rows();
            if rows.is_empty() {
                break;
            }
            all.extend(rows_to_items(&t, &rows));
            if all.len() as u32 >= t.total {
                break;
            }
        }
        Ok(all)
    }

    /// Save one item as .msg (and its attachments as files) in one Outlook job.
    fn export_one(s: &Session, folder_id: &str, item_id: &str, dir: &std::path::Path, base: &str, attachments: bool) -> Result<Exported, String> {
        let it = item_by_id(s, folder_id, item_id)?;
        let file = format!("{base}.msg");
        let path = dir.join(&file);
        it.call("SaveAs", &[Arg::Str(&path.to_string_lossy()), Arg::I4(OL_MSG_UNICODE)])?;
        if !path.exists() {
            return Err("Outlook didn't write the file.".into());
        }
        let mut out = Exported { file, ..Default::default() };
        if attachments {
            let atts = it.obj("Attachments")?.kind("Attachments");
            let list: Vec<Disp> = match atts.enumerate() {
                Ok(v) if !v.is_empty() => v.into_iter().filter_map(|v| v.into_disp()).collect(),
                _ => (1..=atts.int("Count")).filter_map(|i| atts.call("Item", &[Arg::I4(i as i32)]).ok().and_then(|v| v.into_disp())).collect(),
            };
            if !list.is_empty() {
                let adir = dir.join(format!("{base} - attachments"));
                std::fs::create_dir_all(&adir).map_err(|e| e.to_string())?;
                let mut used = std::collections::HashSet::new();
                for (i, a) in list.into_iter().enumerate() {
                    let a = a.kind("Attachment");
                    let original = a.text("FileName");
                    let mut name = crate::export::unique_file_name(&mut used, &original, &format!("attachment {}", i + 1));
                    // Attached e-mails often have no extension.
                    if a.int("Type") == 5 && !name.to_lowercase().ends_with(".msg") {
                        name.push_str(".msg");
                    }
                    let p = adir.join(&name);
                    match a.call("SaveAsFile", &[Arg::Str(&p.to_string_lossy())]) {
                        Ok(_) => out.attachments += 1,
                        Err(e) => out.warnings.push(format!("attachment “{original}”: {e}")),
                    }
                }
            }
        }
        Ok(out)
    }

    fn read_item(s: &Session, folder_id: &str, item_id: &str) -> Result<ItemDetail, String> {
        let it = item_by_id(s, folder_id, item_id)?;
        let p = properties(&it, &ITEM_TAGS);
        let pv = |i: usize| p.as_ref().map(|v| v[i].to_text()).unwrap_or_default();
        let class = if p.is_some() { pv(7) } else { it.text("MessageClass") };
        let contact = class.starts_with("IPM.Contact");
        let calendar = class.starts_with("IPM.Appointment");
        let mut fields = Vec::new();
        let mut add = |label: &str, v: String| {
            if !v.is_empty() {
                fields.push((label.to_string(), v));
            }
        };
        if calendar {
            add("Starts", prop_date(&it, "Start").map(|d| fmt_long_date(&d)).unwrap_or_default());
            add("Ends", prop_date(&it, "End").map(|d| fmt_long_date(&d)).unwrap_or_default());
            add("Location", it.text("Location"));
        }
        if contact {
            add("Company", it.text("CompanyName"));
            add("Job title", it.text("JobTitle"));
            add("Email", it.text("Email1Address"));
            add("Phone (Business)", it.text("BusinessTelephoneNumber"));
            add("Phone (Mobile)", it.text("MobileTelephoneNumber"));
        }
        if class.starts_with("IPM.Task") {
            add("Due", prop_date(&it, "DueDate").map(|d| fmt_long_date(&d)).unwrap_or_default());
        }
        let has_attach = p.as_ref().map(|v| v[9].to_i64().unwrap_or(1) != 0).unwrap_or(true);
        let mut attachments = Vec::new();
        if has_attach {
            if let Ok(atts) = it.obj("Attachments") {
                let atts = atts.kind("Attachments");
                for i in 1..=atts.int("Count") {
                    if let Some(a) = atts.call("Item", &[Arg::I4(i as i32)]).ok().and_then(|v| v.into_disp()) {
                        let a = a.kind("Attachment");
                        attachments.push(Attachment { id: format!("{item_id}|{i}"), name: a.text("FileName"), size: a.int("Size").max(0) as u64 });
                    }
                }
            }
        }
        let (from, mut from_email) = if contact {
            (it.text("FullName"), it.text("Email1Address"))
        } else if calendar {
            (it.text("Organizer"), String::new())
        } else if p.is_some() {
            (pv(1), pv(2))
        } else {
            (it.text("SenderName"), it.text("SenderEmailAddress"))
        };
        if !from_email.contains('@') {
            from_email.clear(); // Exchange-internal (X500) address
        }
        let date = match &p {
            Some(v) => utc_date(&v[5]).or_else(|| utc_date(&v[6])),
            None => prop_date(&it, "SentOn").or_else(|| prop_date(&it, "ReceivedTime")),
        }
        .or_else(|| prop_date(&it, "CreationTime"));
        let mut subject = if p.is_some() { pv(0) } else { it.text("Subject") };
        if subject.is_empty() && contact {
            subject = from.clone();
        }
        // Long bodies may not come through GetProperties; ask for them directly.
        let mut body = if p.as_ref().map(|v| v[8].is_error()).unwrap_or(true) { it.text("Body") } else { pv(8) };
        body = body.replace("\r\n", "\n").trim().to_string();
        Ok(ItemDetail {
            id: item_id.to_string(),
            subject,
            from,
            from_email,
            to: if p.is_some() { pv(3) } else { it.text("To") },
            cc: if p.is_some() { pv(4) } else { it.text("CC") },
            date: date.map(|d| fmt_long_date(&d)).unwrap_or_default(),
            body,
            class,
            fields,
            attachments,
        })
    }

    const OL_POST_ITEM: i32 = 6;

    /// Add one file to a public folder through classic Outlook.
    fn upload_one(s: &Session, folder_id: &str, path: &std::path::Path) -> Result<(), String> {
        let folder = folder_by_id(s, folder_id)?;
        let p = path.to_string_lossy().to_string();
        let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        // An Outlook message, event or contact file: open it and move it in.
        if ["msg", "eml", "ics", "vcf", "oft"].contains(&ext.as_str()) {
            if let Some(item) = s.ns.call("OpenSharedItem", &[Arg::Str(&p)]).ok().and_then(|v| v.into_disp()) {
                let item = item.kind("Item");
                if item.call("Move", &[Arg::Obj(&folder)]).is_ok() {
                    return Ok(());
                }
                // Some items can't be moved; a copy can.
                if let Some(copy) = item.call("Copy", &[]).ok().and_then(|v| v.into_disp()) {
                    copy.kind("Item").call("Move", &[Arg::Obj(&folder)])?;
                    return Ok(());
                }
            }
            if ext == "msg" || ext == "eml" {
                // Don't quietly turn an e-mail into an attachment.
                return Err("Outlook couldn't open this e-mail file.".into());
            }
        }
        // Anything else: a post with the file attached.
        let post = folder.obj("Items")?.call("Add", &[Arg::I4(OL_POST_ITEM)])?.into_disp().ok_or("Outlook couldn't create a post here.")?;
        let post = post.kind("Item");
        let title = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "File".into());
        post.put("Subject", Arg::Str(&title))?;
        post.obj("Attachments")?.call("Add", &[Arg::Str(&p)])?;
        post.call("Post", &[])?;
        Ok(())
    }

    fn temp_path(ext: &str) -> std::path::PathBuf {
        let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        std::env::temp_dir().join(format!("public-folders-{}-{n}.{ext}", std::process::id()))
    }

    fn read_and_remove(p: &std::path::Path) -> Result<Vec<u8>, String> {
        let bytes = std::fs::read(p).map_err(|e| format!("Outlook didn't save the file: {e}"))?;
        let _ = std::fs::remove_file(p);
        Ok(bytes)
    }

    impl Backend for ClassicOutlook {
        fn label(&self) -> String {
            self.label.clone()
        }

        // Remembering answers is done by cache.rs, on disk.
        fn root_folders(&self) -> Result<Vec<FolderInfo>, String> {
            if let Some(t) = self.tree.lock().unwrap().as_ref() {
                return Ok(t.get("").cloned().unwrap_or_default());
            }
            self.run(|s| list_folders(s, &s.root))
        }

        fn child_folders(&self, folder_id: &str) -> Result<Vec<FolderInfo>, String> {
            if let Some(t) = self.tree.lock().unwrap().as_ref() {
                return Ok(t.get(folder_id).cloned().unwrap_or_default());
            }
            let id = folder_id.to_string();
            self.run(move |s| list_folders(s, &folder_by_id(s, &id)?))
        }

        /// The whole tree in one MAPI request (about 10 s for 11,000 folders,
        /// against hours one folder at a time through Outlook).
        fn whole_tree(&self) -> Option<Result<HashMap<String, Vec<FolderInfo>>, String>> {
            let mapi = self.mapi()?;
            Some(mapi.tree().map(|raw| {
                let mut tree: HashMap<String, Vec<FolderInfo>> = HashMap::new();
                let parents: std::collections::HashSet<String> = raw.keys().cloned().collect();
                for (key, list) in raw {
                    // Ids are bare EntryIDs: every public folder is in the same store,
                    // which the session remembers (keeps the saved copy small).
                    let list = list
                        .into_iter()
                        .map(|mut f| {
                            f.child_count = parents.contains(&f.id) as u32;
                            f
                        })
                        .collect();
                    tree.insert(key, list);
                }
                *self.tree.lock().unwrap() = Some(tree.clone());
                tree
            }))
        }

        fn items(&self, folder: &FolderInfo, offset: u32) -> Result<ItemPage, String> {
            if offset == 0 {
                if let Some(all) = self.mapi_items(folder) {
                    let total = all.len() as u32;
                    return Ok(ItemPage { items: all, total, more: false, next_offset: total });
                }
            }
            let folder = folder.clone();
            self.run(move |s| list_items(s, &folder, offset))
        }

        fn item(&self, folder_id: &str, item_id: &str) -> Result<ItemDetail, String> {
            let (fid, iid) = (folder_id.to_string(), item_id.to_string());
            self.run(move |s| read_item(s, &fid, &iid))
        }

        fn attachment(&self, folder_id: &str, attachment_id: &str) -> Result<Vec<u8>, String> {
            let fid = folder_id.to_string();
            let (iid, idx) = attachment_id.rsplit_once('|').map(|(a, b)| (a.to_string(), b.parse::<i32>().unwrap_or(1))).unwrap_or_default();
            self.run(move |s| {
                let it = item_by_id(s, &fid, &iid)?;
                let a = it.obj("Attachments")?.call("Item", &[Arg::I4(idx)])?.into_disp().ok_or("Attachment not found.")?;
                let p = temp_path("bin");
                a.call("SaveAsFile", &[Arg::Str(&p.to_string_lossy())])?;
                read_and_remove(&p)
            })
        }

        fn mime(&self, folder_id: &str, item_id: &str) -> Result<Vec<u8>, String> {
            let (fid, iid) = (folder_id.to_string(), item_id.to_string());
            self.run(move |s| {
                let it = item_by_id(s, &fid, &iid)?;
                let p = temp_path("msg");
                it.call("SaveAs", &[Arg::Str(&p.to_string_lossy()), Arg::I4(OL_MSG_UNICODE)])?;
                read_and_remove(&p)
            })
        }

        fn message_ext(&self) -> &'static str {
            "msg"
        }

        fn upload(&self, folder: &FolderInfo, path: &std::path::Path) -> Result<(), String> {
            // Outlook can't open .eml files as messages (it would only attach
            // them to a post), so convert them with Outlook's MIME converter.
            let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
            if ext == "eml" {
                if let Some(mapi) = self.mapi() {
                    return mapi.import_eml(&split_id(&folder.id).0, path);
                }
            }
            let (fid, path) = (folder.id.clone(), path.to_path_buf());
            self.run(move |s| upload_one(s, &fid, &path))
        }

        fn all_items(&self, folder: &FolderInfo) -> Result<Vec<ItemSummary>, String> {
            if let Some(all) = self.mapi_items(folder) {
                return Ok(all);
            }
            let folder = folder.clone();
            self.run(move |s| list_all(s, &folder))
        }

        fn export_item(&self, folder_id: &str, item_id: &str, dir: &std::path::Path, base: &str, attachments: bool) -> Result<Exported, String> {
            let (fid, iid, dir, base) = (folder_id.to_string(), item_id.to_string(), dir.to_path_buf(), base.to_string());
            self.run(move |s| export_one(s, &fid, &iid, &dir, &base, attachments))
        }
    }
}
