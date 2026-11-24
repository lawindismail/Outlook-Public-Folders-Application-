//! Extended MAPI: the channel classic Outlook itself uses to talk to
//! Exchange, loaded into this app from Outlook's own MAPI library.
//!
//! Why: Outlook's automation interface answers other programs one request
//! at a time on Outlook's busy window thread (0.1 s to 20 s per request on
//! some PCs). MAPI runs inside this app instead, and one "hierarchy table"
//! request returns every folder at once. Used for mapping the folder tree;
//! everything else still goes through classic Outlook.

#![cfg(windows)]

use crate::model::FolderInfo;
use std::collections::HashMap;
use std::ffi::c_void;
use windows_sys::core::GUID;

type Hr = i32;
type FnInit = unsafe extern "system" fn(*mut c_void) -> Hr;
type FnLogon = unsafe extern "system" fn(usize, *const u16, *const u16, u32, *mut *mut c_void) -> Hr;
type FnFreeProws = unsafe extern "system" fn(*mut SRowSet);
type FnFreeBuffer = unsafe extern "system" fn(*mut c_void) -> u32;
type FnAllocBuffer = unsafe extern "system" fn(u32, *mut *mut c_void) -> Hr;
type FnAllocMore = unsafe extern "system" fn(u32, *mut c_void, *mut *mut c_void) -> Hr;
type FnFreePadrlist = unsafe extern "system" fn(*mut c_void);

const MAPI_EXTENDED: u32 = 0x0000_0020;
const MAPI_USE_DEFAULT: u32 = 0x0000_0040;
const MAPI_NO_MAIL: u32 = 0x0000_8000;
const MAPI_UNICODE: u32 = 0x8000_0000;
const MDB_NO_DIALOG: u32 = 0x0000_0001;
const CONVENIENT_DEPTH: u32 = 0x0000_0001;

const PR_ENTRYID: u32 = 0x0FFF_0102;
const PR_PARENT_ENTRYID: u32 = 0x0E09_0102;
const PR_DISPLAY_NAME_W: u32 = 0x3001_001F;
const PR_CONTENT_COUNT: u32 = 0x3602_0003;
const PR_SUBFOLDERS: u32 = 0x360A_000B;
const PR_CONTAINER_CLASS_W: u32 = 0x3613_001F;
const PR_DEPTH: u32 = 0x3005_0003;
/// The long-term id Outlook understands (table rows carry short-term ids).
const PR_LONGTERM_ENTRYID_FROM_TABLE: u32 = 0x6670_0102;

#[repr(C)]
struct SPropValue {
    tag: u32,
    pad: u32,
    value: [u64; 2],
}

#[repr(C)]
struct SRow {
    pad: u32,
    count: u32,
    props: *mut SPropValue,
}

#[repr(C)]
pub struct SRowSet {
    rows: u32,
    first: [SRow; 1], // really `rows` of them
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn hex_to_bytes(h: &str) -> Vec<u8> {
    (0..h.len() / 2).filter_map(|i| u8::from_str_radix(&h[i * 2..i * 2 + 2], 16).ok()).collect()
}

fn bytes_to_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02X}")).collect()
}

/// Calls method `index` of a COM-style object.
unsafe fn vfn<T: Copy>(obj: *mut c_void, index: usize) -> T {
    let vtbl = *(obj as *const *const usize);
    std::mem::transmute_copy(&*vtbl.add(index))
}

unsafe fn release(obj: *mut c_void) {
    if !obj.is_null() {
        let f: unsafe extern "system" fn(*mut c_void) -> u32 = vfn(obj, 2);
        f(obj);
    }
}

fn check(hr: Hr, what: &str) -> Result<(), String> {
    if hr < 0 {
        Err(format!("MAPI {what} failed ({:#010x})", hr as u32))
    } else {
        Ok(())
    }
}

/// Outlook's MAPI library (msmapi32.dll / olmapi32.dll).
struct Library {
    logon: FnLogon,
    free_prows: FnFreeProws,
    free_buffer: FnFreeBuffer,
    alloc_buffer: FnAllocBuffer,
    alloc_more: FnAllocMore,
    free_padrlist: FnFreePadrlist,
}

/// Where Outlook's MAPI libraries are (registered path first, then the
/// usual Click-to-Run locations).
fn mapi_dll_candidates() -> Vec<String> {
    // Outlook's own MAPI (OLMAPI32) first: Outlook's e-mail converter uses
    // it, so messages must come from the same library to be filled in.
    let mut candidates = Vec::new();
    if let Some(root) = reg_string("SOFTWARE\\Microsoft\\Office\\ClickToRun", "InstallPath") {
        candidates.push(format!("{root}\\root\\Office16\\OLMAPI32.DLL"));
    }
    candidates.push("C:\\Program Files\\Microsoft Office\\root\\Office16\\OLMAPI32.DLL".into());
    candidates.push("C:\\Program Files (x86)\\Microsoft Office\\root\\Office16\\OLMAPI32.DLL".into());
    if let Some(p) = reg_string("SOFTWARE\\Clients\\Mail\\Microsoft Outlook", "DLLPathEx") {
        candidates.push(p);
    }
    candidates
}

fn load() -> Result<Library, String> {
    use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    for path in mapi_dll_candidates() {
        let w = wide(&path);
        let h = unsafe { LoadLibraryW(w.as_ptr()) };
        if h.is_null() {
            continue;
        }
        unsafe {
            let init = GetProcAddress(h, c"MAPIInitialize".as_ptr().cast());
            let logon = GetProcAddress(h, c"MAPILogonEx".as_ptr().cast());
            let free = GetProcAddress(h, c"FreeProws".as_ptr().cast());
            let free_buffer = GetProcAddress(h, c"MAPIFreeBuffer".as_ptr().cast());
            let alloc_buffer = GetProcAddress(h, c"MAPIAllocateBuffer".as_ptr().cast());
            let alloc_more = GetProcAddress(h, c"MAPIAllocateMore".as_ptr().cast());
            let free_padrlist = GetProcAddress(h, c"FreePadrlist".as_ptr().cast());
            if let (Some(init), Some(logon), Some(free), Some(free_buffer), Some(alloc_buffer), Some(alloc_more), Some(free_padrlist)) =
                (init, logon, free, free_buffer, alloc_buffer, alloc_more, free_padrlist)
            {
                let init: FnInit = std::mem::transmute(init);
                check(init(std::ptr::null_mut()), "start")?;
                return Ok(Library { logon: std::mem::transmute(logon), free_prows: std::mem::transmute(free), free_buffer: std::mem::transmute(free_buffer),
                    alloc_buffer: std::mem::transmute(alloc_buffer),
                    alloc_more: std::mem::transmute(alloc_more),
                    free_padrlist: std::mem::transmute(free_padrlist),
                });
            }
        }
    }
    Err("Outlook's MAPI library wasn't found.".into())
}

fn reg_string(key: &str, value: &str) -> Option<String> {
    use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ};
    let (k, v) = (wide(key), wide(value));
    let mut buf = vec![0u16; 1024];
    let mut size = (buf.len() * 2) as u32;
    let r = unsafe { RegGetValueW(HKEY_LOCAL_MACHINE, k.as_ptr(), v.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), buf.as_mut_ptr().cast(), &mut size) };
    if r != 0 {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(0);
    Some(String::from_utf16_lossy(&buf[..len]))
}

unsafe fn prop_text(p: &SPropValue) -> String {
    let ptr = p.value[0] as *const u16;
    if ptr.is_null() {
        return String::new();
    }
    let mut n = 0;
    while *ptr.add(n) != 0 {
        n += 1;
    }
    String::from_utf16_lossy(std::slice::from_raw_parts(ptr, n))
}

unsafe fn prop_bin(p: &SPropValue) -> Vec<u8> {
    let cb = p.value[0] as u32 as usize;
    let ptr = p.value[1] as *const u8;
    if ptr.is_null() || cb == 0 {
        return Vec::new();
    }
    std::slice::from_raw_parts(ptr, cb).to_vec()
}

/// One MAPI sign-in with the public folder store open, kept on its own
/// thread for the life of the connection.
struct Open {
    lib: Library,
    session: *mut c_void,
    store: *mut c_void,
    root_hex: String,
}

impl Open {
    unsafe fn new(store_hex: &str, root_hex: &str) -> Result<Open, String> {
        let lib = load()?;
        let mut session: *mut c_void = std::ptr::null_mut();
        check(
            (lib.logon)(0, std::ptr::null(), std::ptr::null(), MAPI_EXTENDED | MAPI_USE_DEFAULT | MAPI_NO_MAIL | MAPI_UNICODE, &mut session),
            "sign-in",
        )?;
        let store_id = hex_to_bytes(store_hex);
        let mut store: *mut c_void = std::ptr::null_mut();
        let open_store: unsafe extern "system" fn(*mut c_void, usize, u32, *const u8, *const c_void, u32, *mut *mut c_void) -> Hr = vfn(session, 5);
        let hr = open_store(session, 0, store_id.len() as u32, store_id.as_ptr(), std::ptr::null(), MDB_NO_DIALOG, &mut store);
        let me = Open { lib, session, store, root_hex: root_hex.to_string() };
        check(hr, "open store")?;
        Ok(me)
    }

    unsafe fn open_folder(&self, hex: &str) -> Result<*mut c_void, String> {
        self.open_folder_with(hex, 0)
    }

    unsafe fn open_folder_with(&self, hex: &str, flags: u32) -> Result<*mut c_void, String> {
        let id = hex_to_bytes(hex);
        let mut kind = 0u32;
        let mut folder: *mut c_void = std::ptr::null_mut();
        let open_entry: unsafe extern "system" fn(*mut c_void, u32, *const u8, *const c_void, u32, *mut u32, *mut *mut c_void) -> Hr = vfn(self.store, 17);
        let hr = open_entry(self.store, id.len() as u32, id.as_ptr(), std::ptr::null(), flags, &mut kind, &mut folder);
        if hr == E_ACCESSDENIED {
            return Err("You don't have permission to add items to this folder.".into());
        }
        check(hr, "open folder")?;
        Ok(folder)
    }

    #[cfg(test)]
    unsafe fn convert_in_inbox(&self, path: &std::path::Path) -> Result<String, String> {
        const PR_DEFAULT_STORE: u32 = 0x3400_000B;
        // Find the default (own mailbox) store.
        let mut table: *mut c_void = std::ptr::null_mut();
        let get_stores: unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> Hr = vfn(self.session, 4);
        check(get_stores(self.session, 0, &mut table), "store list")?;
        let cols: [u32; 3] = [2, PR_ENTRYID, PR_DEFAULT_STORE];
        let set_columns: unsafe extern "system" fn(*mut c_void, *const u32, u32) -> Hr = vfn(table, 7);
        check(set_columns(table, cols.as_ptr(), 0), "columns")?;
        let query_rows: unsafe extern "system" fn(*mut c_void, i32, u32, *mut *mut SRowSet) -> Hr = vfn(table, 19);
        let mut rows: *mut SRowSet = std::ptr::null_mut();
        check(query_rows(table, 50, 0, &mut rows), "stores")?;
        let mut store_id = Vec::new();
        let first = std::ptr::addr_of!((*rows).first) as *const SRow;
        for i in 0..(*rows).rows as usize {
            let row = &*first.add(i);
            let props = std::slice::from_raw_parts(row.props, row.count as usize);
            let default = props.iter().any(|p| p.tag == PR_DEFAULT_STORE && p.value[0] as u16 != 0);
            if default {
                if let Some(p) = props.iter().find(|p| p.tag == PR_ENTRYID) {
                    store_id = prop_bin(p);
                }
            }
        }
        (self.lib.free_prows)(rows);
        release(table);
        let mut store: *mut c_void = std::ptr::null_mut();
        let open_store: unsafe extern "system" fn(*mut c_void, usize, u32, *const u8, *const c_void, u32, *mut *mut c_void) -> Hr = vfn(self.session, 5);
        check(open_store(self.session, 0, store_id.len() as u32, store_id.as_ptr(), std::ptr::null(), MDB_NO_DIALOG | 0x4, &mut store), "own mailbox")?;
        // Inbox = the receive folder.
        let mut cb = 0u32;
        let mut eid: *mut u8 = std::ptr::null_mut();
        let get_receive: unsafe extern "system" fn(*mut c_void, *const u16, u32, *mut u32, *mut *mut u8, *mut *mut u16) -> Hr = vfn(store, 19);
        check(get_receive(store, std::ptr::null(), MAPI_UNICODE, &mut cb, &mut eid, std::ptr::null_mut()), "inbox")?;
        let mut kind = 0u32;
        let mut inbox: *mut c_void = std::ptr::null_mut();
        let open_entry: unsafe extern "system" fn(*mut c_void, u32, *const u8, *const c_void, u32, *mut u32, *mut *mut c_void) -> Hr = vfn(store, 17);
        let hr = open_entry(store, cb, eid, std::ptr::null(), MAPI_MODIFY, &mut kind, &mut inbox);
        (self.lib.free_buffer)(eid.cast());
        check(hr, "open inbox")?;
        let mut msg: *mut c_void = std::ptr::null_mut();
        let create_message: unsafe extern "system" fn(*mut c_void, *const GUID, u32, *mut *mut c_void) -> Hr = vfn(inbox, 19);
        check(create_message(inbox, std::ptr::null(), 0, &mut msg), "create message")?;
        let r = fill_from_eml(&self.lib, msg, path).map(|_| {
            // Subject | To line | number of attachments
            let mut table: *mut c_void = std::ptr::null_mut();
            let get_attachments: unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> Hr = vfn(msg, 14);
            let mut count = 0u32;
            if get_attachments(msg, 0, &mut table) >= 0 && !table.is_null() {
                let get_count: unsafe extern "system" fn(*mut c_void, u32, *mut u32) -> Hr = vfn(table, 9);
                get_count(table, 0, &mut count);
                release(table);
            }
            let mut rtable: *mut c_void = std::ptr::null_mut();
            let get_recips: unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> Hr = vfn(msg, 18);
            let mut recips = 0u32;
            if get_recips(msg, 0, &mut rtable) >= 0 && !rtable.is_null() {
                let get_count: unsafe extern "system" fn(*mut c_void, u32, *mut u32) -> Hr = vfn(rtable, 9);
                get_count(rtable, 0, &mut recips);
                release(rtable);
            }
            format!("{}|{} recipients|{} attachments", get_text(&self.lib, msg, PR_SUBJECT_W), recips, count)
        });
        release(msg); // never saved
        release(inbox);
        release(store);
        r
    }

    /// Turn an .eml file into a real message in a folder, the way Outlook
    /// imports one: a new message, filled by Outlook's own MIME converter,
    /// marked as a received (not draft) message. `save` = false only tries
    /// the conversion (used by the tests) and returns the subject.
    unsafe fn import_eml(&self, folder_hex: &str, path: &std::path::Path, save: bool) -> Result<String, String> {
        let folder = self.open_folder_with(folder_hex, if save { MAPI_MODIFY } else { MAPI_BEST_ACCESS })?;
        let r = (|| {
            let mut msg: *mut c_void = std::ptr::null_mut();
            let create_message: unsafe extern "system" fn(*mut c_void, *const GUID, u32, *mut *mut c_void) -> Hr = vfn(folder, 19);
            let hr = create_message(folder, std::ptr::null(), 0, &mut msg);
            if hr == E_ACCESSDENIED || hr == MAPI_E_NO_ACCESS {
                return Err("You don't have permission to add items to this folder.".into());
            }
            check(hr, "create a message")?;
            let r = (|| {
                fill_from_eml(&self.lib, msg, path)?;
                // Received and read, not a draft ("This message hasn't been sent").
                let flags = SPropValue { tag: PR_MESSAGE_FLAGS, pad: 0, value: [MSGFLAG_READ as u64, 0] };
                let set_props: unsafe extern "system" fn(*mut c_void, u32, *const SPropValue, *mut *mut c_void) -> Hr = vfn(msg, 8);
                let _ = set_props(msg, 1, &flags, std::ptr::null_mut());
                let subject = get_text(&self.lib, msg, PR_SUBJECT_W);
                if save {
                    let save_changes: unsafe extern "system" fn(*mut c_void, u32) -> Hr = vfn(msg, 4);
                    check(save_changes(msg, 0), "save the message")?;
                }
                Ok(subject)
            })();
            release(msg);
            r
        })();
        release(folder);
        r
    }

    unsafe fn tree(&self) -> Result<HashMap<String, Vec<FolderInfo>>, String> {
        let root = self.open_folder(&self.root_hex)?;
        let r = read_hierarchy(&self.lib, root, &self.root_hex);
        release(root);
        r
    }

    unsafe fn items(&self, folder_hex: &str) -> Result<Vec<crate::model::ItemSummary>, String> {
        let folder = self.open_folder(folder_hex)?;
        let r = read_contents(&self.lib, folder);
        release(folder);
        r
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        unsafe {
            release(self.store);
            if !self.session.is_null() {
                let logoff: unsafe extern "system" fn(*mut c_void, usize, u32, u32) -> Hr = vfn(self.session, 17);
                logoff(self.session, 0, 0, 0);
                release(self.session);
            }
        }
    }
}

type Job = Box<dyn FnOnce(&Open) + Send>;

/// MAPI on its own thread. Ids in and out are hex (no "|store" part).
pub struct Mapi {
    jobs: std::sync::Mutex<std::sync::mpsc::Sender<Job>>,
}

impl Mapi {
    /// Sign in to MAPI and open the public folder store. `store_hex` and
    /// `root_hex` are classic Outlook's StoreID and the root folder's EntryID.
    pub fn open(store_hex: &str, root_hex: &str) -> Result<Mapi, String> {
        let (tx, rx) = std::sync::mpsc::channel::<Job>();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
        let (store_hex, root_hex) = (store_hex.to_string(), root_hex.to_string());
        std::thread::spawn(move || {
            // COM first (the e-mail converter is a COM object), then MAPI.
            crate::com::init_thread();
            let open = match unsafe { Open::new(&store_hex, &root_hex) } {
                Ok(o) => o,
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            let _ = ready_tx.send(Ok(()));
            while let Ok(job) = rx.recv() {
                job(&open);
            }
        });
        ready_rx.recv().map_err(|_| "MAPI stopped unexpectedly.".to_string())??;
        Ok(Mapi { jobs: std::sync::Mutex::new(tx) })
    }

    fn run<T: Send + 'static>(&self, f: impl FnOnce(&Open) -> Result<T, String> + Send + 'static) -> Result<T, String> {
        let (tx, rx) = std::sync::mpsc::channel();
        self.jobs
            .lock()
            .unwrap()
            .send(Box::new(move |o| {
                let _ = tx.send(f(o));
            }))
            .map_err(|_| "MAPI stopped.".to_string())?;
        rx.recv().map_err(|_| "MAPI stopped.".to_string())?
    }

    /// Every folder, as "subfolders of each folder" ("" = top level).
    pub fn tree(&self) -> Result<HashMap<String, Vec<FolderInfo>>, String> {
        self.run(|o| unsafe { o.tree() })
    }

    /// Every item in a folder, newest first.
    pub fn items(&self, folder_hex: &str) -> Result<Vec<crate::model::ItemSummary>, String> {
        let id = folder_hex.to_string();
        self.run(move |o| unsafe { o.items(&id) })
    }

    /// Add an .eml file to a folder as a proper message.
    pub fn import_eml(&self, folder_hex: &str, path: &std::path::Path) -> Result<(), String> {
        let (id, path) = (folder_hex.to_string(), path.to_path_buf());
        self.run(move |o| unsafe { o.import_eml(&id, &path, true) }).map(|_| ())
    }

    /// Convert an .eml without saving it anywhere; returns its subject.
    #[cfg(test)]
    pub fn try_convert_eml(&self, folder_hex: &str, path: &std::path::Path) -> Result<String, String> {
        let (id, path) = (folder_hex.to_string(), path.to_path_buf());
        self.run(move |o| unsafe { o.import_eml(&id, &path, false) })
    }

    /// Test only: convert an .eml into an unsaved message in the person's
    /// own Inbox (writable, and never saved, so nothing appears there).
    #[cfg(test)]
    pub fn try_convert_in_inbox(&self, path: &std::path::Path) -> Result<String, String> {
        let path = path.to_path_buf();
        self.run(move |o| unsafe { o.convert_in_inbox(&path) })
    }
}

const MAPI_MODIFY: u32 = 0x0000_0001;
const MAPI_BEST_ACCESS: u32 = 0x0000_0010;
const E_ACCESSDENIED: Hr = 0x8007_0005_u32 as i32;
const MAPI_E_NO_ACCESS: Hr = 0x8004_0105_u32 as i32;
const PR_MESSAGE_FLAGS: u32 = 0x0E07_0003;
const MSGFLAG_READ: u32 = 0x0000_0001;
/// Fill a new MAPI message from an .eml file: subject, sender, recipients,
/// dates, message ID, the original headers, text and HTML bodies, and every
/// attachment (inline pictures included). Outlook's own MIME converter
/// doesn't work from another program on Click-to-Run Office, so the e-mail
/// is read here and written property by property, as Outlook would store it.
unsafe fn fill_from_eml(lib: &Library, msg: *mut c_void, path: &std::path::Path) -> Result<(), String> {
    use mail_parser::{HeaderName, MessageParser, MimeHeaders};
    let raw = std::fs::read(path).map_err(|e| format!("Couldn't read the .eml file: {e}"))?;
    let mail = MessageParser::default().parse(&raw[..]).ok_or("This .eml file couldn't be read as an e-mail.")?;

    // Plain properties. `keep` holds the UTF-16 text so pointers stay valid.
    let mut keep: Vec<Vec<u16>> = Vec::new();
    let mut props: Vec<SPropValue> = Vec::new();
    let mut text = |props: &mut Vec<SPropValue>, tag: u32, s: &str| {
        if !s.is_empty() {
            let w = wide(s);
            props.push(SPropValue { tag, pad: 0, value: [w.as_ptr() as u64, 0] });
            keep.push(w);
        }
    };
    text(&mut props, PR_MESSAGE_CLASS_W, "IPM.Note");
    let subject = mail.subject().unwrap_or("");
    text(&mut props, PR_SUBJECT_W, subject);
    text(&mut props, PR_CONVERSATION_TOPIC_W, subject);
    let addr_of = |name: HeaderName| -> Vec<(String, String)> {
        mail.header(name)
            .and_then(|v| v.as_address())
            .and_then(|a| a.as_list())
            .map(|l| {
                l.iter()
                    .map(|a| {
                        let email = a.address().unwrap_or("").to_string();
                        let name = a.name().map(str::to_string).filter(|n| !n.is_empty()).unwrap_or_else(|| email.clone());
                        (name, email)
                    })
                    .filter(|(_, e)| !e.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    };
    if let Some((name, email)) = addr_of(HeaderName::From).into_iter().next() {
        for (n, e, t) in [
            (PR_SENDER_NAME_W, PR_SENDER_EMAIL_ADDRESS_W, PR_SENDER_ADDRTYPE_W),
            (PR_SENT_REPRESENTING_NAME_W, PR_SENT_REPRESENTING_EMAIL_ADDRESS_W, PR_SENT_REPRESENTING_ADDRTYPE_W),
        ] {
            text(&mut props, n, &name);
            text(&mut props, e, &email);
            text(&mut props, t, "SMTP");
        }
    }
    if let Some(id) = mail.message_id() {
        text(&mut props, PR_INTERNET_MESSAGE_ID_W, &format!("<{id}>"));
    }
    // The original headers (shown by Outlook under File > Properties).
    let head_end = raw.windows(4).position(|w| w == b"\r\n\r\n").or_else(|| raw.windows(2).position(|w| w == b"\n\n")).unwrap_or(0);
    text(&mut props, PR_TRANSPORT_MESSAGE_HEADERS_W, &String::from_utf8_lossy(&raw[..head_end]));
    if let Some(d) = mail.date() {
        let ft = ((d.to_timestamp() + 11_644_473_600).max(0) as u64) * 10_000_000;
        props.push(SPropValue { tag: PR_CLIENT_SUBMIT_TIME, pad: 0, value: [ft, 0] });
        props.push(SPropValue { tag: PR_MESSAGE_DELIVERY_TIME, pad: 0, value: [ft, 0] });
    }
    // Received and read, not a draft. (Only settable before the first save.)
    props.push(SPropValue { tag: PR_MESSAGE_FLAGS, pad: 0, value: [MSGFLAG_READ as u64, 0] });
    props.push(SPropValue { tag: PR_INTERNET_CPID, pad: 0, value: [65001, 0] }); // HTML is UTF-8
    set_props(msg, &props, "save the e-mail's details")?;

    // Bodies, through streams (they can be large).
    if let Some(t) = mail.body_text(0) {
        let bytes: Vec<u8> = t.encode_utf16().flat_map(|c| c.to_le_bytes()).collect();
        write_stream(msg, PR_BODY_W, &bytes)?;
    }
    if let Some(h) = mail.body_html(0) {
        if mail.html_body_count() > 0 {
            write_stream(msg, PR_HTML, h.as_bytes())?;
        }
    }

    // Recipients (To, Cc).
    let mut recips: Vec<(String, String, u32)> = Vec::new();
    for (header, kind) in [(HeaderName::To, MAPI_TO), (HeaderName::Cc, MAPI_CC)] {
        recips.extend(addr_of(header).into_iter().map(|(n, e)| (n, e, kind)));
    }
    if !recips.is_empty() {
        add_recipients(lib, msg, &recips)?;
    }

    // Attachments.
    for (i, part) in mail.attachments().enumerate() {
        let name = part.attachment_name().map(str::to_string).unwrap_or_else(|| {
            if part.is_content_type("message", "rfc822") { format!("attached message {}.eml", i + 1) } else { format!("attachment {}", i + 1) }
        });
        let mime = part.content_type().map(|c| format!("{}/{}", c.ctype(), c.subtype().unwrap_or("octet-stream"))).unwrap_or_default();
        let inline_id = part.content_id().map(|c| c.trim_matches(['<', '>']).to_string());
        add_attachment(msg, &name, &mime, inline_id.as_deref(), part.contents())?;
    }
    Ok(())
}

unsafe fn set_props(obj: *mut c_void, props: &[SPropValue], what: &str) -> Result<(), String> {
    let set: unsafe extern "system" fn(*mut c_void, u32, *const SPropValue, *mut *mut c_void) -> Hr = vfn(obj, 8);
    check(set(obj, props.len() as u32, props.as_ptr(), std::ptr::null_mut()), what)
}

/// Write a (possibly large) property through a stream.
unsafe fn write_stream(obj: *mut c_void, tag: u32, bytes: &[u8]) -> Result<(), String> {
    const IID_ISTREAM: GUID = GUID { data1: 0x0000_000C, data2: 0, data3: 0, data4: [0xC0, 0, 0, 0, 0, 0, 0, 0x46] };
    let open_property: unsafe extern "system" fn(*mut c_void, u32, *const GUID, u32, u32, *mut *mut c_void) -> Hr = vfn(obj, 7);
    let mut stm: *mut c_void = std::ptr::null_mut();
    // STGM_CREATE | STGM_WRITE, MAPI_CREATE | MAPI_MODIFY
    check(open_property(obj, tag, &IID_ISTREAM, 0x1000 | 0x1, 0x2 | 0x1, &mut stm), "write the e-mail's contents")?;
    let write: unsafe extern "system" fn(*mut c_void, *const c_void, u32, *mut u32) -> Hr = vfn(stm, 4);
    let mut r = Ok(());
    for chunk in bytes.chunks(64 * 1024) {
        let mut done = 0u32;
        if write(stm, chunk.as_ptr().cast(), chunk.len() as u32, &mut done) < 0 {
            r = Err("Couldn't write the e-mail's contents.".to_string());
            break;
        }
    }
    let commit: unsafe extern "system" fn(*mut c_void, u32) -> Hr = vfn(stm, 8);
    commit(stm, 0);
    release(stm);
    r
}

/// Add To/Cc recipients (MAPI wants this list in its own memory).
unsafe fn add_recipients(lib: &Library, msg: *mut c_void, recips: &[(String, String, u32)]) -> Result<(), String> {
    let n = recips.len();
    let mut list: *mut c_void = std::ptr::null_mut();
    check((lib.alloc_buffer)((8 + 16 * n) as u32, &mut list), "list recipients")?;
    *(list as *mut u32) = n as u32;
    for (i, (name, email, kind)) in recips.iter().enumerate() {
        let entry = (list as *mut u8).add(8 + 16 * i);
        let mut pv: *mut c_void = std::ptr::null_mut();
        check((lib.alloc_buffer)((4 * std::mem::size_of::<SPropValue>()) as u32, &mut pv), "list recipients")?;
        let copy = |s: &str| -> *mut u16 {
            let w = wide(s);
            let mut p: *mut c_void = std::ptr::null_mut();
            (lib.alloc_more)((w.len() * 2) as u32, pv, &mut p);
            std::ptr::copy_nonoverlapping(w.as_ptr(), p as *mut u16, w.len());
            p as *mut u16
        };
        let props = pv as *mut SPropValue;
        *props.add(0) = SPropValue { tag: PR_DISPLAY_NAME_W, pad: 0, value: [copy(name) as u64, 0] };
        *props.add(1) = SPropValue { tag: PR_EMAIL_ADDRESS_W, pad: 0, value: [copy(email) as u64, 0] };
        *props.add(2) = SPropValue { tag: PR_ADDRTYPE_W, pad: 0, value: [copy("SMTP") as u64, 0] };
        *props.add(3) = SPropValue { tag: PR_RECIPIENT_TYPE, pad: 0, value: [*kind as u64, 0] };
        *(entry as *mut u32) = 0;
        *(entry.add(4) as *mut u32) = 4;
        *(entry.add(8) as *mut *mut c_void) = pv;
    }
    // IMessage::ModifyRecipients is method 19; MODRECIP_ADD = 2.
    let modify: unsafe extern "system" fn(*mut c_void, u32, *mut c_void) -> Hr = vfn(msg, 19);
    let hr = modify(msg, 2, list);
    (lib.free_padrlist)(list);
    check(hr, "add the recipients")
}

unsafe fn add_attachment(msg: *mut c_void, name: &str, mime: &str, inline_id: Option<&str>, data: &[u8]) -> Result<(), String> {
    // IMessage::CreateAttach is method 16.
    let create: unsafe extern "system" fn(*mut c_void, *const GUID, u32, *mut u32, *mut *mut c_void) -> Hr = vfn(msg, 16);
    let mut num = 0u32;
    let mut att: *mut c_void = std::ptr::null_mut();
    check(create(msg, std::ptr::null(), 0, &mut num, &mut att), "add an attachment")?;
    let r = (|| {
        let mut keep: Vec<Vec<u16>> = Vec::new();
        let mut props = vec![SPropValue { tag: PR_ATTACH_METHOD, pad: 0, value: [ATTACH_BY_VALUE as u64, 0] }];
        let mut text = |props: &mut Vec<SPropValue>, tag: u32, s: &str| {
            if !s.is_empty() {
                let w = wide(s);
                props.push(SPropValue { tag, pad: 0, value: [w.as_ptr() as u64, 0] });
                keep.push(w);
            }
        };
        text(&mut props, PR_ATTACH_LONG_FILENAME_W, name);
        text(&mut props, PR_ATTACH_FILENAME_W, name);
        text(&mut props, PR_DISPLAY_NAME_W, name);
        text(&mut props, PR_ATTACH_MIME_TAG_W, mime);
        if let Some(id) = inline_id {
            text(&mut props, PR_ATTACH_CONTENT_ID_W, id);
            if mime.starts_with("image/") {
                props.push(SPropValue { tag: PR_ATTACHMENT_HIDDEN, pad: 0, value: [1, 0] });
            }
        }
        set_props(att, &props, "describe an attachment")?;
        write_stream(att, PR_ATTACH_DATA_BIN, data)?;
        let save: unsafe extern "system" fn(*mut c_void, u32) -> Hr = vfn(att, 4);
        check(save(att, 0), "save an attachment")
    })();
    release(att);
    r
}

const PR_CONVERSATION_TOPIC_W: u32 = 0x0070_001F;
const PR_SENDER_EMAIL_ADDRESS_W: u32 = 0x0C1F_001F;
const PR_SENDER_ADDRTYPE_W: u32 = 0x0C1E_001F;
const PR_SENT_REPRESENTING_NAME_W: u32 = 0x0042_001F;
const PR_SENT_REPRESENTING_EMAIL_ADDRESS_W: u32 = 0x0065_001F;
const PR_SENT_REPRESENTING_ADDRTYPE_W: u32 = 0x0064_001F;
const PR_INTERNET_MESSAGE_ID_W: u32 = 0x1035_001F;
const PR_TRANSPORT_MESSAGE_HEADERS_W: u32 = 0x007D_001F;
const PR_CLIENT_SUBMIT_TIME: u32 = 0x0039_0040;
const PR_INTERNET_CPID: u32 = 0x3FDE_0003;
const PR_BODY_W: u32 = 0x1000_001F;
const PR_HTML: u32 = 0x1013_0102;
const PR_EMAIL_ADDRESS_W: u32 = 0x3003_001F;
const PR_ADDRTYPE_W: u32 = 0x3002_001F;
const PR_RECIPIENT_TYPE: u32 = 0x0C15_0003;
const MAPI_TO: u32 = 1;
const MAPI_CC: u32 = 2;
const PR_ATTACH_METHOD: u32 = 0x3705_0003;
const ATTACH_BY_VALUE: u32 = 1;
const PR_ATTACH_LONG_FILENAME_W: u32 = 0x3707_001F;
const PR_ATTACH_FILENAME_W: u32 = 0x3704_001F;
const PR_ATTACH_MIME_TAG_W: u32 = 0x370E_001F;
const PR_ATTACH_CONTENT_ID_W: u32 = 0x3712_001F;
const PR_ATTACHMENT_HIDDEN: u32 = 0x7FFE_000B;
const PR_ATTACH_DATA_BIN: u32 = 0x3701_0102;
/// One text property of a MAPI object ("" if missing).
unsafe fn get_text(lib: &Library, obj: *mut c_void, tag: u32) -> String {
    let tags: [u32; 2] = [1, tag];
    let mut count = 0u32;
    let mut props: *mut SPropValue = std::ptr::null_mut();
    let get_props: unsafe extern "system" fn(*mut c_void, *const u32, u32, *mut u32, *mut *mut SPropValue) -> Hr = vfn(obj, 5);
    let mut out = String::new();
    let hr = get_props(obj, tags.as_ptr(), MAPI_UNICODE, &mut count, &mut props);
    if hr >= 0 && !props.is_null() {
        if count > 0 && (*props).tag == tag {
            out = prop_text(&*props);
        } else if cfg!(test) {
            eprintln!("get_text: hr {:#x}, count {count}, tag {:#010x}, value {:#x}", hr as u32, (*props).tag, (*props).value[0]);
        }
        (lib.free_buffer)(props.cast());
    } else if cfg!(test) {
        eprintln!("get_text: GetProps failed {:#x}", hr as u32);
    }
    out
}

const PR_SUBJECT_W: u32 = 0x0037_001F;
const PR_SENDER_NAME_W: u32 = 0x0C1A_001F;
const PR_MESSAGE_DELIVERY_TIME: u32 = 0x0E06_0040;
const PR_CREATION_TIME: u32 = 0x3007_0040;
const PR_MESSAGE_CLASS_W: u32 = 0x001A_001F;
const PR_HASATTACH: u32 = 0x0E1B_000B;
const TABLE_SORT_DESCEND: u32 = 1;

/// FILETIME (UTC) → local time.
fn filetime(v: u64) -> Option<chrono::DateTime<chrono::Local>> {
    if v == 0 {
        return None;
    }
    let secs = (v / 10_000_000) as i64 - 11_644_473_600;
    let nanos = ((v % 10_000_000) * 100) as u32;
    chrono::DateTime::from_timestamp(secs, nanos).map(|d| d.with_timezone(&chrono::Local))
}

unsafe fn read_contents(lib: &Library, folder: *mut c_void) -> Result<Vec<crate::model::ItemSummary>, String> {
    use crate::model::{fmt_list_date, ItemSummary};
    let mut table: *mut c_void = std::ptr::null_mut();
    let get_contents: unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> Hr = vfn(folder, 14);
    check(get_contents(folder, 0, &mut table), "item list")?;
    let result = (|| -> Result<Vec<ItemSummary>, String> {
        let cols: [u32; 9] =
            [8, PR_ENTRYID, PR_LONGTERM_ENTRYID_FROM_TABLE, PR_SUBJECT_W, PR_SENDER_NAME_W, PR_MESSAGE_DELIVERY_TIME, PR_CREATION_TIME, PR_MESSAGE_CLASS_W, PR_HASATTACH];
        let set_columns: unsafe extern "system" fn(*mut c_void, *const u32, u32) -> Hr = vfn(table, 7);
        check(set_columns(table, cols.as_ptr(), 0), "choose columns")?;
        // Newest first: SSortOrderSet { cSorts, cCategories, cExpanded, {tag, order} }.
        let sort: [u32; 5] = [1, 0, 0, PR_MESSAGE_DELIVERY_TIME, TABLE_SORT_DESCEND];
        let sort_table: unsafe extern "system" fn(*mut c_void, *const u32, u32) -> Hr = vfn(table, 17);
        let _ = sort_table(table, sort.as_ptr(), 0);
        let query_rows: unsafe extern "system" fn(*mut c_void, i32, u32, *mut *mut SRowSet) -> Hr = vfn(table, 19);
        let mut out = Vec::new();
        loop {
            let mut rows: *mut SRowSet = std::ptr::null_mut();
            check(query_rows(table, 1000, 0, &mut rows), "read items")?;
            if rows.is_null() {
                break;
            }
            let n = (*rows).rows as usize;
            let first = std::ptr::addr_of!((*rows).first) as *const SRow;
            for i in 0..n {
                let row = &*first.add(i);
                let props = std::slice::from_raw_parts(row.props, row.count as usize);
                let mut it = ItemSummary::default();
                let (mut entry, mut longterm) = (String::new(), String::new());
                let (mut delivered, mut created) = (None, None);
                for p in props {
                    match p.tag {
                        PR_ENTRYID => entry = bytes_to_hex(&prop_bin(p)),
                        PR_LONGTERM_ENTRYID_FROM_TABLE => longterm = bytes_to_hex(&prop_bin(p)),
                        PR_SUBJECT_W => it.subject = prop_text(p),
                        PR_SENDER_NAME_W => it.from = prop_text(p),
                        PR_MESSAGE_DELIVERY_TIME => delivered = filetime(p.value[0]),
                        PR_CREATION_TIME => created = filetime(p.value[0]),
                        PR_MESSAGE_CLASS_W => it.class = prop_text(p),
                        PR_HASATTACH => it.has_attachments = p.value[0] as u16 != 0,
                        _ => {}
                    }
                }
                it.id = if longterm.is_empty() { entry } else { longterm };
                it.when = delivered.or(created);
                it.date = it.when.as_ref().map(fmt_list_date).unwrap_or_default();
                if !it.id.is_empty() {
                    out.push(it);
                }
            }
            (lib.free_prows)(rows);
            if n == 0 {
                break;
            }
        }
        Ok(out)
    })();
    release(table);
    result
}

unsafe fn read_hierarchy(lib: &Library, root: *mut c_void, root_hex: &str) -> Result<HashMap<String, Vec<FolderInfo>>, String> {
    let mut table: *mut c_void = std::ptr::null_mut();
    let get_hierarchy: unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> Hr = vfn(root, 15);
    check(get_hierarchy(root, CONVENIENT_DEPTH, &mut table), "folder list")?;
    let result = (|| -> Result<Vec<(FolderInfo, String, String)>, String> {
        let cols: [u32; 9] = [8, PR_ENTRYID, PR_PARENT_ENTRYID, PR_DISPLAY_NAME_W, PR_CONTENT_COUNT, PR_SUBFOLDERS, PR_CONTAINER_CLASS_W, PR_DEPTH, PR_LONGTERM_ENTRYID_FROM_TABLE];
        let set_columns: unsafe extern "system" fn(*mut c_void, *const u32, u32) -> Hr = vfn(table, 7);
        check(set_columns(table, cols.as_ptr(), 0), "choose columns")?;
        let query_rows: unsafe extern "system" fn(*mut c_void, i32, u32, *mut *mut SRowSet) -> Hr = vfn(table, 19);
        // (folder, parent entry id, long-term id) in table order.
        let mut found: Vec<(FolderInfo, String, String)> = Vec::new();
        loop {
            let mut rows: *mut SRowSet = std::ptr::null_mut();
            check(query_rows(table, 500, 0, &mut rows), "read folders")?;
            if rows.is_null() {
                break;
            }
            let n = (*rows).rows as usize;
            let first = std::ptr::addr_of!((*rows).first) as *const SRow;
            for i in 0..n {
                let row = &*first.add(i);
                let props = std::slice::from_raw_parts(row.props, row.count as usize);
                let mut f = FolderInfo { class: "IPF.Note".into(), ..Default::default() };
                let mut parent = String::new();
                let mut depth = 0u32;
                let mut longterm = String::new();
                for p in props {
                    match p.tag {
                        PR_DEPTH => depth = p.value[0] as u32,
                        PR_LONGTERM_ENTRYID_FROM_TABLE => longterm = bytes_to_hex(&prop_bin(p)),
                        PR_ENTRYID => f.id = bytes_to_hex(&prop_bin(p)),
                        PR_PARENT_ENTRYID => parent = bytes_to_hex(&prop_bin(p)),
                        PR_DISPLAY_NAME_W => f.name = prop_text(p),
                        PR_CONTENT_COUNT => f.total_count = p.value[0] as u32,
                        PR_SUBFOLDERS => f.child_count = (p.value[0] as u16 != 0) as u32,
                        PR_CONTAINER_CLASS_W => {
                            let c = prop_text(p);
                            if !c.is_empty() {
                                f.class = c;
                            }
                        }
                        _ => {}
                    }
                }
                if !f.id.is_empty() {
                    // Depth 1 = directly under "All Public Folders".
                    found.push((f, if depth <= 1 { String::new() } else { parent }, longterm));
                }
            }
            (lib.free_prows)(rows);
            if n == 0 {
                break;
            }
        }
        Ok(found)
    })();
    release(table);
    let found = result?;
    // The table links folders by short-term ids; Outlook needs long-term ones.
    let long: HashMap<String, String> =
        found.iter().filter(|(_, _, l)| !l.is_empty()).map(|(f, _, l)| (f.id.clone(), l.clone())).collect();
    let to_long = |id: &str| long.get(id).cloned().unwrap_or_else(|| id.to_string());
    let mut tree: HashMap<String, Vec<FolderInfo>> = HashMap::new();
    for (mut f, parent, _) in found {
        let key = if parent.is_empty() || parent.eq_ignore_ascii_case(root_hex) { String::new() } else { to_long(&parent) };
        f.id = to_long(&f.id);
        tree.entry(key).or_default().push(f);
    }
    for list in tree.values_mut() {
        list.sort_by_key(|f| f.name.to_lowercase());
    }
    Ok(tree)
}

#[cfg(test)]
mod eml_tests {
    /// Live and safe: convert a sample .eml inside a public folder WITHOUT
    /// saving it (nothing is added). Run with:
    /// cargo test eml_conversion -- --ignored --nocapture
    #[test]
    #[ignore]
    fn eml_conversion() {
        crate::com::init_thread();
        let app = crate::com::Disp::create("Outlook.Application").unwrap();
        let ns = app.call("GetNamespace", &[crate::com::Arg::Str("MAPI")]).unwrap().into_disp().unwrap();
        let root = ns.call("GetDefaultFolder", &[crate::com::Arg::I4(18)]).unwrap().into_disp().unwrap();
        let (store, entry) = (root.text("StoreID"), root.text("EntryID"));
        let mapi = super::Mapi::open(&store, &entry).expect("MAPI");
        let tree = mapi.tree().expect("tree");
        let dir = std::env::temp_dir().join(format!("pf-eml-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let eml = dir.join("sample.eml");
        std::fs::write(
            &eml,
            concat!(
                "From: Sam Example <sam@example.com>\r\nTo: Team <team@example.com>\r\nSubject: Converted test message\r\n",
                "Date: Mon, 23 Nov 2026 09:30:00 +0000\r\nMessage-ID: <test-123@example.com>\r\nMIME-Version: 1.0\r\n",
                "Content-Type: multipart/mixed; boundary=\"b1\"\r\n\r\n",
                "--b1\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<p>Hello from an <b>.eml</b> file.</p>\r\n",
                "--b1\r\nContent-Type: text/plain; name=\"notes.txt\"\r\nContent-Disposition: attachment; filename=\"notes.txt\"\r\n\r\nSome notes.\r\n",
                "--b1--\r\n"
            ),
        )
        .unwrap();
        // Converted into an unsaved message in the own Inbox (never saved).
        let r = mapi.try_convert_in_inbox(&eml);
        println!("conversion check (own Inbox, not saved): {r:?}");
        // A public folder without "create items" rights says so clearly.
        if let Some(f) = tree.values().flatten().next() {
            println!("public folder check (not saved): {:?}", mapi.try_convert_eml(&f.id, &eml));
        }
        let _ = std::fs::remove_dir_all(&dir);
        let _ = tree;
        assert_eq!(r.unwrap(), "Converted test message|1 recipients|1 attachments");
    }
}

#[cfg(test)]
mod tests {
    /// Live: read the whole public folder tree through MAPI.
    /// Run with: cargo test mapi_tree -- --ignored --nocapture
    #[test]
    #[ignore]
    fn mapi_tree() {
        crate::com::init_thread();
        let app = crate::com::Disp::create("Outlook.Application").unwrap();
        let ns = app.call("GetNamespace", &[crate::com::Arg::Str("MAPI")]).unwrap().into_disp().unwrap();
        let root = ns.call("GetDefaultFolder", &[crate::com::Arg::I4(18)]).unwrap().into_disp().unwrap();
        let (store, entry) = (root.text("StoreID"), root.text("EntryID"));
        let t = std::time::Instant::now();
        let mapi = super::Mapi::open(&store, &entry).expect("MAPI sign-in");
        println!("MAPI signed in in {:?}", t.elapsed());
        let t = std::time::Instant::now();
        let tree = mapi.tree();
        if let Ok(tree) = &tree {
            // Item lists: walk folders until 50 with items are found.
            let t3 = std::time::Instant::now();
            let (mut tried, mut with_items, mut n, mut errors) = (0, 0, 0, 0);
            let mut sample = None;
            let mut biggest = (0usize, std::time::Duration::ZERO);
            for f in tree.values().flatten() {
                if with_items >= 50 || tried >= 600 {
                    break;
                }
                tried += 1;
                let t = std::time::Instant::now();
                match mapi.items(&f.id) {
                    Ok(v) if !v.is_empty() => {
                        with_items += 1;
                        n += v.len();
                        if v.len() > biggest.0 {
                            biggest = (v.len(), t.elapsed());
                        }
                        if sample.is_none() {
                            sample = v.into_iter().find(|i| i.class.starts_with("IPM.Note") || i.class.starts_with("IPM.Post"));
                        }
                    }
                    Ok(_) => {}
                    Err(_) => errors += 1,
                }
            }
            println!(
                "MAPI listed {n} items in {with_items} folders (tried {tried}, {errors} errors) in {:?}; biggest list {} items in {:?}",
                t3.elapsed(),
                biggest.0,
                biggest.1
            );
            if let Some(it) = sample {
                let opened = ns.call("GetItemFromID", &[crate::com::Arg::Str(&it.id), crate::com::Arg::Str(&store)]);
                println!("Outlook opens a MAPI item id: {} (dated: {}, subject {} chars)", opened.is_ok(), it.when.is_some(), it.subject.len());
            }
        }
        match tree {
            Ok(tree) => {
                let total: usize = tree.values().map(|v| v.len()).sum();
                println!("MAPI: {} folders under {} parents in {:?}; top level {}", total, tree.len(), t.elapsed(), tree.get("").map(|v| v.len()).unwrap_or(0));
                let ids: std::collections::HashSet<&String> = tree.values().flatten().map(|f| &f.id).collect();
                let orphans = tree.keys().filter(|k| !k.is_empty() && !ids.contains(k)).count();
                println!("parent lists whose parent wasn't found: {orphans}");
                // Do MAPI's ids match classic Outlook's?
                let folders = root.obj("Folders").unwrap();
                let first = folders.call("Item", &[crate::com::Arg::I4(1)]).unwrap().into_disp().unwrap();
                let oom_id = first.text("EntryID");
                let top = tree.get("").cloned().unwrap_or_default();
                println!("Outlook's first top folder id is in MAPI's top level: {}", top.iter().any(|f| f.id.eq_ignore_ascii_case(&oom_id)));
                if let Some(f) = top.iter().find(|f| f.child_count > 0) {
                    let via_oom = ns.call("GetFolderFromID", &[crate::com::Arg::Str(&f.id), crate::com::Arg::Str(&store)]);
                    println!("Outlook opens a MAPI folder id: {}", via_oom.is_ok());
                }
            }
            Err(e) => println!("MAPI failed after {:?}: {e}", t.elapsed()),
        }
    }
}
