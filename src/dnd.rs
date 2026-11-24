//! Drag and drop onto the window (Windows): files from File Explorer, and
//! e-mails or attachments dragged straight out of Outlook.
//!
//! Outlook doesn't drag real files: it offers "virtual files" (a
//! FileGroupDescriptorW list plus FileContents for each, as an IStorage for
//! messages or a stream for attachments). The window library only accepts
//! real files, so this registers its own OLE drop target and saves each
//! virtual file to a temp folder first.

#![cfg(windows)]

use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use windows_sys::core::GUID;

#[derive(Default)]
pub struct DropState {
    /// Something droppable is over the window.
    pub hovering: bool,
    /// Pointer position in egui points (window coordinates).
    pub pos: Option<(f32, f32)>,
    /// Files just dropped, and where.
    pub dropped: Option<(Vec<PathBuf>, Option<(f32, f32)>)>,
    /// Problems while reading dropped items.
    pub problems: Vec<String>,
}

const CF_HDROP: u16 = 15;
const DVASPECT_CONTENT: u32 = 1;
const TYMED_HGLOBAL: u32 = 1;
const TYMED_ISTREAM: u32 = 4;
const TYMED_ISTORAGE: u32 = 8;
const DROPEFFECT_NONE: u32 = 0;
const DROPEFFECT_COPY: u32 = 1;
const FD_FILESIZE: u32 = 0x40;
const IID_IUNKNOWN: GUID = GUID { data1: 0, data2: 0, data3: 0, data4: [0xC0, 0, 0, 0, 0, 0, 0, 0x46] };
const IID_IDROPTARGET: GUID = GUID { data1: 0x0000_0122, data2: 0, data3: 0, data4: [0xC0, 0, 0, 0, 0, 0, 0, 0x46] };

#[repr(C)]
struct FormatEtc {
    cf: u16,
    ptd: *mut c_void,
    aspect: u32,
    lindex: i32,
    tymed: u32,
}

#[repr(C)]
struct StgMedium {
    tymed: u32,
    handle: *mut c_void,
    release: *mut c_void,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct PointL {
    x: i32,
    y: i32,
}

#[repr(C)]
struct Vtbl {
    query_interface: unsafe extern "system" fn(*mut Target, *const GUID, *mut *mut c_void) -> i32,
    add_ref: unsafe extern "system" fn(*mut Target) -> u32,
    release: unsafe extern "system" fn(*mut Target) -> u32,
    drag_enter: unsafe extern "system" fn(*mut Target, *mut c_void, u32, PointL, *mut u32) -> i32,
    drag_over: unsafe extern "system" fn(*mut Target, u32, PointL, *mut u32) -> i32,
    drag_leave: unsafe extern "system" fn(*mut Target) -> i32,
    drop: unsafe extern "system" fn(*mut Target, *mut c_void, u32, PointL, *mut u32) -> i32,
}

#[repr(C)]
struct Target {
    vtbl: *const Vtbl,
    refs: AtomicU32,
    accept: AtomicU32,
    hwnd: isize,
    cf_group: u16,
    cf_contents: u16,
    state: Arc<Mutex<DropState>>,
    ctx: eframe::egui::Context,
}

static VTBL: Vtbl = Vtbl { query_interface, add_ref, release, drag_enter, drag_over, drag_leave, drop: on_drop };

fn same(a: &GUID, b: &GUID) -> bool {
    a.data1 == b.data1 && a.data2 == b.data2 && a.data3 == b.data3 && a.data4 == b.data4
}

unsafe extern "system" fn query_interface(this: *mut Target, iid: *const GUID, out: *mut *mut c_void) -> i32 {
    if same(&*iid, &IID_IUNKNOWN) || same(&*iid, &IID_IDROPTARGET) {
        *out = this.cast();
        add_ref(this);
        0
    } else {
        *out = std::ptr::null_mut();
        0x8000_4002_u32 as i32 // E_NOINTERFACE
    }
}

unsafe extern "system" fn add_ref(this: *mut Target) -> u32 {
    (*this).refs.fetch_add(1, Ordering::Relaxed) + 1
}

unsafe extern "system" fn release(this: *mut Target) -> u32 {
    // The target lives as long as the window; never freed.
    (*this).refs.fetch_sub(1, Ordering::Relaxed).saturating_sub(1)
}

/// Calls method `index` of a COM object.
unsafe fn vfn<T: Copy>(obj: *mut c_void, index: usize) -> T {
    let vtbl = *(obj as *const *const usize);
    std::mem::transmute_copy(&*vtbl.add(index))
}

unsafe fn has_format(data: *mut c_void, cf: u16, tymed: u32) -> bool {
    let fe = FormatEtc { cf, ptd: std::ptr::null_mut(), aspect: DVASPECT_CONTENT, lindex: -1, tymed };
    let query: unsafe extern "system" fn(*mut c_void, *const FormatEtc) -> i32 = vfn(data, 5);
    query(data, &fe) == 0
}

impl Target {
    unsafe fn point(&self, pt: PointL) -> Option<(f32, f32)> {
        let mut p = windows_sys::Win32::Foundation::POINT { x: pt.x, y: pt.y };
        windows_sys::Win32::Graphics::Gdi::ScreenToClient(self.hwnd as _, &mut p);
        let ppp = self.ctx.pixels_per_point();
        Some((p.x as f32 / ppp, p.y as f32 / ppp))
    }

    fn update(&self, f: impl FnOnce(&mut DropState)) {
        f(&mut self.state.lock().unwrap());
        self.ctx.request_repaint();
    }
}

unsafe extern "system" fn drag_enter(this: *mut Target, data: *mut c_void, _keys: u32, pt: PointL, effect: *mut u32) -> i32 {
    let t = &*this;
    let ok = has_format(data, CF_HDROP, TYMED_HGLOBAL) || has_format(data, t.cf_group, TYMED_HGLOBAL);
    t.accept.store(ok as u32, Ordering::Relaxed);
    *effect = if ok { DROPEFFECT_COPY } else { DROPEFFECT_NONE };
    let pos = t.point(pt);
    t.update(|s| {
        s.hovering = ok;
        s.pos = pos;
    });
    0
}

unsafe extern "system" fn drag_over(this: *mut Target, _keys: u32, pt: PointL, effect: *mut u32) -> i32 {
    let t = &*this;
    let ok = t.accept.load(Ordering::Relaxed) != 0;
    *effect = if ok { DROPEFFECT_COPY } else { DROPEFFECT_NONE };
    let pos = t.point(pt);
    t.update(|s| s.pos = pos);
    0
}

unsafe extern "system" fn drag_leave(this: *mut Target) -> i32 {
    (*this).update(|s| {
        s.hovering = false;
        s.pos = None;
    });
    0
}

unsafe extern "system" fn on_drop(this: *mut Target, data: *mut c_void, _keys: u32, pt: PointL, effect: *mut u32) -> i32 {
    let t = &*this;
    let pos = t.point(pt);
    let mut problems = Vec::new();
    let files = read_files(t, data, &mut problems);
    *effect = if files.is_empty() { DROPEFFECT_NONE } else { DROPEFFECT_COPY };
    t.update(|s| {
        s.hovering = false;
        s.pos = None;
        s.problems = problems;
        if !files.is_empty() {
            s.dropped = Some((files, pos));
        }
    });
    0
}

unsafe fn get(data: *mut c_void, cf: u16, lindex: i32, tymed: u32) -> Option<StgMedium> {
    let fe = FormatEtc { cf, ptd: std::ptr::null_mut(), aspect: DVASPECT_CONTENT, lindex, tymed };
    let mut m = StgMedium { tymed: 0, handle: std::ptr::null_mut(), release: std::ptr::null_mut() };
    let get_data: unsafe extern "system" fn(*mut c_void, *const FormatEtc, *mut StgMedium) -> i32 = vfn(data, 3);
    (get_data(data, &fe, &mut m) == 0).then_some(m)
}

unsafe fn free(m: &mut StgMedium) {
    windows_sys::Win32::System::Ole::ReleaseStgMedium((m as *mut StgMedium).cast());
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

unsafe fn read_files(t: &Target, data: *mut c_void, problems: &mut Vec<String>) -> Vec<PathBuf> {
    use windows_sys::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
    let mut out = Vec::new();
    // 1. Real files (File Explorer).
    if let Some(mut m) = get(data, CF_HDROP, -1, TYMED_HGLOBAL) {
        use windows_sys::Win32::UI::Shell::DragQueryFileW;
        let n = DragQueryFileW(m.handle as _, u32::MAX, std::ptr::null_mut(), 0);
        for i in 0..n {
            let len = DragQueryFileW(m.handle as _, i, std::ptr::null_mut(), 0);
            let mut buf = vec![0u16; len as usize + 1];
            DragQueryFileW(m.handle as _, i, buf.as_mut_ptr(), len + 1);
            out.push(PathBuf::from(String::from_utf16_lossy(&buf[..len as usize])));
        }
        free(&mut m);
        if !out.is_empty() {
            return out;
        }
    }
    // 2. Virtual files (Outlook messages and attachments).
    let Some(mut m) = get(data, t.cf_group, -1, TYMED_HGLOBAL) else { return out };
    let base = GlobalLock(m.handle as _) as *const u8;
    if base.is_null() {
        free(&mut m);
        return out;
    }
    let count = *(base as *const u32) as usize;
    let mut names = Vec::new();
    for i in 0..count {
        // FILEDESCRIPTORW is 592 bytes; the name is at offset 72.
        let d = base.add(4 + i * 592);
        let flags = *(d as *const u32);
        let size_low = *(d.add(68) as *const u32) as u64;
        let size_high = *(d.add(64) as *const u32) as u64;
        let name_ptr = d.add(72) as *const u16;
        let mut len = 0;
        while len < 260 && *name_ptr.add(len) != 0 {
            len += 1;
        }
        let name = String::from_utf16_lossy(std::slice::from_raw_parts(name_ptr, len));
        let size = (flags & FD_FILESIZE != 0).then_some((size_high << 32) | size_low);
        names.push((name, size));
    }
    GlobalUnlock(m.handle as _);
    free(&mut m);

    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("public-folders-drop-{stamp}"));
    if std::fs::create_dir_all(&dir).is_err() {
        return out;
    }
    let mut used = std::collections::HashSet::new();
    for (i, (name, size)) in names.into_iter().enumerate() {
        let file = crate::export::unique_file_name(&mut used, &name, &format!("dropped item {}", i + 1));
        let path = dir.join(&file);
        let Some(mut m) = get(data, t.cf_contents, i as i32, TYMED_HGLOBAL | TYMED_ISTREAM | TYMED_ISTORAGE) else {
            problems.push(format!("{name}: Outlook didn't hand it over"));
            continue;
        };
        let ok = match m.tymed {
            TYMED_HGLOBAL => {
                let p = GlobalLock(m.handle as _) as *const u8;
                let n = size.unwrap_or(GlobalSize(m.handle as _) as u64).min(GlobalSize(m.handle as _) as u64) as usize;
                let r = !p.is_null() && std::fs::write(&path, std::slice::from_raw_parts(p, n)).is_ok();
                GlobalUnlock(m.handle as _);
                r
            }
            TYMED_ISTREAM => {
                // ISequentialStream::Read is method 3.
                let read: unsafe extern "system" fn(*mut c_void, *mut c_void, u32, *mut u32) -> i32 = vfn(m.handle, 3);
                let mut bytes = Vec::new();
                let mut chunk = vec![0u8; 64 * 1024];
                loop {
                    let mut got = 0u32;
                    let hr = read(m.handle, chunk.as_mut_ptr().cast(), chunk.len() as u32, &mut got);
                    bytes.extend_from_slice(&chunk[..got as usize]);
                    if hr != 0 || got == 0 {
                        break;
                    }
                }
                std::fs::write(&path, bytes).is_ok()
            }
            TYMED_ISTORAGE => {
                // An Outlook message: copy its storage into a new .msg file.
                use windows_sys::Win32::System::Com::StructuredStorage::StgCreateDocfile;
                let w = wide(&path.to_string_lossy());
                let mut dest: *mut c_void = std::ptr::null_mut();
                // STGM_CREATE | STGM_READWRITE | STGM_SHARE_EXCLUSIVE
                let ok = StgCreateDocfile(w.as_ptr(), 0x1000 | 0x2 | 0x10, 0, &mut dest as *mut *mut c_void as _) >= 0 && !dest.is_null();
                let mut r = false;
                if ok {
                    let copy_to: unsafe extern "system" fn(*mut c_void, u32, *const GUID, *const c_void, *mut c_void) -> i32 = vfn(m.handle, 7);
                    let commit: unsafe extern "system" fn(*mut c_void, u32) -> i32 = vfn(dest, 9);
                    r = copy_to(m.handle, 0, std::ptr::null(), std::ptr::null(), dest) >= 0 && commit(dest, 0) >= 0;
                    let rel: unsafe extern "system" fn(*mut c_void) -> u32 = vfn(dest, 2);
                    rel(dest);
                }
                r
            }
            _ => false,
        };
        free(&mut m);
        if ok {
            out.push(path);
        } else {
            problems.push(format!("{name}: couldn't be read"));
        }
    }
    out
}

/// Take over drag and drop for this window.
pub fn register(hwnd: isize, ctx: eframe::egui::Context) -> Option<Arc<Mutex<DropState>>> {
    use windows_sys::Win32::System::DataExchange::RegisterClipboardFormatW;
    use windows_sys::Win32::System::Ole::{OleInitialize, RegisterDragDrop, RevokeDragDrop};
    unsafe {
        OleInitialize(std::ptr::null_mut());
        let state = Arc::new(Mutex::new(DropState::default()));
        let target = Box::new(Target {
            vtbl: &VTBL,
            refs: AtomicU32::new(1),
            accept: AtomicU32::new(0),
            hwnd,
            cf_group: RegisterClipboardFormatW(wide("FileGroupDescriptorW").as_ptr()) as u16,
            cf_contents: RegisterClipboardFormatW(wide("FileContents").as_ptr()) as u16,
            state: state.clone(),
            ctx,
        });
        let ptr = Box::into_raw(target);
        // Replace any drop target the window library registered.
        RevokeDragDrop(hwnd as _);
        if RegisterDragDrop(hwnd as _, ptr.cast()) < 0 {
            return None;
        }
        Some(state)
    }
}
