//! Tiny late-bound COM automation (IDispatch) helper, enough to drive
//! classic Outlook's object model.

#![cfg(windows)]

use std::ffi::c_void;
use windows_sys::core::GUID;

const IID_IDISPATCH: GUID = GUID { data1: 0x0002_0400, data2: 0, data3: 0, data4: [0xC0, 0, 0, 0, 0, 0, 0, 0x46] };
const IID_IUNKNOWN: GUID = GUID { data1: 0, data2: 0, data3: 0, data4: [0xC0, 0, 0, 0, 0, 0, 0, 0x46] };
const IID_NULL: GUID = GUID { data1: 0, data2: 0, data3: 0, data4: [0; 8] };
const LOCALE_USER_DEFAULT: u32 = 0x0400;
const DISPATCH_METHOD: u16 = 1;
const DISPATCH_PROPERTYGET: u16 = 2;
const DISPATCH_PROPERTYPUT: u16 = 4;
const DISP_E_EXCEPTION: i32 = 0x8002_0009_u32 as i32;

const VT_EMPTY: u16 = 0;
const VT_I2: u16 = 2;
const VT_I4: u16 = 3;
const VT_R4: u16 = 4;
const VT_R8: u16 = 5;
const VT_CY: u16 = 6;
const VT_DATE: u16 = 7;
const VT_BSTR: u16 = 8;
const VT_DISPATCH: u16 = 9;
const VT_BOOL: u16 = 11;
const VT_UNKNOWN: u16 = 13;
const VT_I1: u16 = 16;
const VT_UI1: u16 = 17;
const VT_UI2: u16 = 18;
const VT_UI4: u16 = 19;
const VT_I8: u16 = 20;
const VT_UI8: u16 = 21;
const VT_INT: u16 = 22;
const VT_UINT: u16 = 23;

/// Same layout as the Windows VARIANT (24 bytes on 64-bit).
#[repr(C)]
#[derive(Clone, Copy)]
struct RawVariant {
    vt: u16,
    r1: u16,
    r2: u16,
    r3: u16,
    val: [u64; 2],
}

impl RawVariant {
    fn empty() -> Self {
        Self { vt: VT_EMPTY, r1: 0, r2: 0, r3: 0, val: [0; 2] }
    }
}

#[repr(C)]
struct DispParams {
    rgvarg: *mut RawVariant,
    rgdispid_named_args: *mut i32,
    c_args: u32,
    c_named_args: u32,
}

#[repr(C)]
struct ExcepInfo {
    w_code: u16,
    w_reserved: u16,
    bstr_source: *mut u16,
    bstr_description: *mut u16,
    bstr_help_file: *mut u16,
    dw_help_context: u32,
    pv_reserved: *mut c_void,
    pfn_deferred_fill_in: *mut c_void,
    scode: i32,
}

#[repr(C)]
struct DispatchVtbl {
    query_interface: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> i32,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    get_type_info_count: *const c_void,
    get_type_info: *const c_void,
    get_ids_of_names: unsafe extern "system" fn(*mut c_void, *const GUID, *const *const u16, u32, u32, *mut i32) -> i32,
    invoke: unsafe extern "system" fn(
        *mut c_void,
        i32,
        *const GUID,
        u32,
        u16,
        *mut DispParams,
        *mut RawVariant,
        *mut ExcepInfo,
        *mut u32,
    ) -> i32,
}

thread_local! {
    /// Member ids already looked up, per object kind.
    static IDS: std::cell::RefCell<std::collections::HashMap<(&'static str, String), i32>> = Default::default();
}

const VT_ERROR: u16 = 10;
const VT_VARIANT: u16 = 12;
const VT_ARRAY: u16 = 0x2000;

/// How many calls went to the COM server (each one may cross to Outlook).
static CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[allow(dead_code)] // used by the live timing test
pub fn calls() -> usize {
    CALLS.load(std::sync::atomic::Ordering::Relaxed)
}

fn count_call() {
    CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

const IID_IENUMVARIANT: GUID = GUID { data1: 0x0002_0404, data2: 0, data3: 0, data4: [0xC0, 0, 0, 0, 0, 0, 0, 0x46] };

#[repr(C)]
struct EnumVariantVtbl {
    query_interface: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> i32,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    next: unsafe extern "system" fn(*mut c_void, u32, *mut RawVariant, *mut u32) -> i32,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

unsafe fn bstr_to_string(b: *const u16) -> String {
    if b.is_null() {
        return String::new();
    }
    let len = windows_sys::Win32::Foundation::SysStringLen(b) as usize;
    String::from_utf16_lossy(std::slice::from_raw_parts(b, len))
}

/// Start COM on this thread (single-threaded apartment, as Office expects).
pub fn init_thread() {
    unsafe {
        windows_sys::Win32::System::Com::CoInitializeEx(std::ptr::null(), windows_sys::Win32::System::Com::COINIT_APARTMENTTHREADED as _);
    }
}

/// Is this COM program (e.g. "Outlook.Application") registered on this PC?
pub fn is_registered(prog_id: &str) -> bool {
    let mut clsid = IID_NULL;
    let p = wide(prog_id);
    unsafe { windows_sys::Win32::System::Com::CLSIDFromProgID(p.as_ptr(), &mut clsid) >= 0 }
}

/// An owned IDispatch pointer.
pub struct Disp(*mut c_void, Option<&'static str>);

impl Drop for Disp {
    fn drop(&mut self) {
        unsafe { (self.vtbl().release)(self.0) };
    }
}

impl Clone for Disp {
    fn clone(&self) -> Self {
        unsafe { (self.vtbl().add_ref)(self.0) };
        Disp(self.0, self.1)
    }
}

pub enum Arg<'a> {
    I4(i32),
    Str(&'a str),
    Bool(bool),
    /// An array of strings (sent as an array of VARIANTs).
    StrArray(&'a [String]),
    /// Another automation object (e.g. a folder to move an item into).
    Obj(&'a Disp),
}

impl Disp {
    /// Create (or attach to) a COM automation server by its ProgID.
    pub fn create(prog_id: &str) -> Result<Disp, String> {
        use windows_sys::Win32::System::Com::{CoCreateInstance, CLSIDFromProgID, CLSCTX_INPROC_SERVER, CLSCTX_LOCAL_SERVER};
        // Like .NET/PowerShell: in-process or separate program, but no
        // "in-process handler" (Outlook's handler won't hand out IDispatch).
        const CLSCTX_ALL: u32 = (CLSCTX_INPROC_SERVER | CLSCTX_LOCAL_SERVER) as u32;
        let mut clsid = IID_NULL;
        let p = wide(prog_id);
        let hr = unsafe { CLSIDFromProgID(p.as_ptr(), &mut clsid) };
        if hr < 0 {
            return Err(format!("{prog_id} isn't installed on this computer."));
        }
        // Ask for IUnknown first, then IDispatch: some Office servers refuse
        // IDispatch straight from CoCreateInstance.
        let mut unk: *mut c_void = std::ptr::null_mut();
        let hr = unsafe { CoCreateInstance(&clsid, std::ptr::null_mut(), CLSCTX_ALL, &IID_IUNKNOWN, &mut unk) };
        if hr < 0 || unk.is_null() {
            return Err(format!("Couldn't start {prog_id} (error {:#010x}).", hr as u32));
        }
        let mut ptr: *mut c_void = std::ptr::null_mut();
        let hr = unsafe {
            let vt = &**(unk as *mut *const DispatchVtbl);
            let hr = (vt.query_interface)(unk, &IID_IDISPATCH, &mut ptr);
            (vt.release)(unk);
            hr
        };
        if hr < 0 || ptr.is_null() {
            return Err(format!("{prog_id} doesn't allow automation (error {:#010x}).", hr as u32));
        }
        Ok(Disp(ptr, None))
    }

    fn vtbl(&self) -> &DispatchVtbl {
        unsafe { &**(self.0 as *mut *const DispatchVtbl) }
    }

    /// Tag this object with its kind ("Folder", "Table"...): objects of one
    /// kind share member ids, so each name is looked up only once. Every
    /// lookup is a round trip to Outlook, which can be slow.
    pub fn kind(mut self, kind: &'static str) -> Self {
        self.1 = Some(kind);
        self
    }

    fn dispid(&self, name: &str) -> Result<i32, String> {
        if name == "_NewEnum" {
            return Ok(-4); // DISPID_NEWENUM, the same for every collection
        }
        if let Some(kind) = self.1 {
            if let Some(id) = IDS.with(|m| m.borrow().get(&(kind, name.to_string())).copied()) {
                return Ok(id);
            }
        }
        let w = wide(name);
        let names = [w.as_ptr()];
        let mut id = 0i32;
        count_call();
        let hr = unsafe { (self.vtbl().get_ids_of_names)(self.0, &IID_NULL, names.as_ptr(), 1, LOCALE_USER_DEFAULT, &mut id) };
        if hr < 0 {
            return Err(format!("“{name}” isn't available here."));
        }
        if let Some(kind) = self.1 {
            IDS.with(|m| m.borrow_mut().insert((kind, name.to_string()), id));
        }
        Ok(id)
    }

    fn invoke(&self, name: &str, args: &[Arg]) -> Result<Var, String> {
        self.invoke_with(name, args, false)
    }

    /// Set a property (`obj.name = value`).
    pub fn put(&self, name: &str, value: Arg) -> Result<(), String> {
        self.invoke_with(name, &[value], true).map(|_| ())
    }

    fn invoke_with(&self, name: &str, args: &[Arg], put: bool) -> Result<Var, String> {
        let id = self.dispid(name)?;
        // Arguments go in reverse order.
        let mut raw: Vec<RawVariant> = args
            .iter()
            .rev()
            .map(|a| {
                let mut v = RawVariant::empty();
                match a {
                    Arg::I4(i) => {
                        v.vt = VT_I4;
                        v.val[0] = *i as u32 as u64;
                    }
                    Arg::Bool(b) => {
                        v.vt = VT_BOOL;
                        v.val[0] = if *b { 0xFFFF } else { 0 };
                    }
                    Arg::Str(s) => {
                        let w = wide(s);
                        v.vt = VT_BSTR;
                        v.val[0] = unsafe { windows_sys::Win32::Foundation::SysAllocString(w.as_ptr()) } as u64;
                    }
                    Arg::Obj(d) => unsafe {
                        // The VARIANT holds its own reference (VariantClear releases it).
                        (d.vtbl().add_ref)(d.0);
                        v.vt = VT_DISPATCH;
                        v.val[0] = d.0 as u64;
                    },
                    Arg::StrArray(items) => unsafe {
                        use windows_sys::Win32::System::Ole::{SafeArrayCreateVector, SafeArrayPutElement};
                        let psa = SafeArrayCreateVector(VT_VARIANT as _, 0, items.len() as u32);
                        for (i, s) in items.iter().enumerate() {
                            let w = wide(s);
                            let mut e = RawVariant::empty();
                            e.vt = VT_BSTR;
                            e.val[0] = windows_sys::Win32::Foundation::SysAllocString(w.as_ptr()) as u64;
                            let idx = i as i32;
                            SafeArrayPutElement(psa, &idx, (&e as *const RawVariant).cast());
                            windows_sys::Win32::System::Variant::VariantClear((&mut e as *mut RawVariant).cast());
                        }
                        v.vt = VT_ARRAY | VT_VARIANT;
                        v.val[0] = psa as u64;
                    },
                }
                v
            })
            .collect();
        let mut named_put: i32 = -3; // DISPID_PROPERTYPUT
        let mut params = DispParams {
            rgvarg: if raw.is_empty() { std::ptr::null_mut() } else { raw.as_mut_ptr() },
            rgdispid_named_args: if put { &mut named_put } else { std::ptr::null_mut() },
            c_args: raw.len() as u32,
            c_named_args: put as u32,
        };
        let mut result = RawVariant::empty();
        let mut excep: ExcepInfo = unsafe { std::mem::zeroed() };
        let mut arg_err = 0u32;
        count_call();
        let hr = unsafe {
            (self.vtbl().invoke)(
                self.0,
                id,
                &IID_NULL,
                LOCALE_USER_DEFAULT,
                if put { DISPATCH_PROPERTYPUT } else { DISPATCH_METHOD | DISPATCH_PROPERTYGET },
                &mut params,
                &mut result,
                &mut excep,
                &mut arg_err,
            )
        };
        for v in raw.iter_mut() {
            unsafe { windows_sys::Win32::System::Variant::VariantClear((v as *mut RawVariant).cast()) };
        }
        if hr < 0 {
            let mut msg = String::new();
            if hr == DISP_E_EXCEPTION {
                unsafe {
                    msg = bstr_to_string(excep.bstr_description);
                    for b in [excep.bstr_source, excep.bstr_description, excep.bstr_help_file] {
                        if !b.is_null() {
                            windows_sys::Win32::Foundation::SysFreeString(b);
                        }
                    }
                }
            }
            if msg.is_empty() {
                msg = format!("“{name}” failed (error {:#010x}).", hr as u32);
            }
            return Err(msg);
        }
        Ok(Var(result))
    }

    /// Read a property or call a method without arguments.
    pub fn get(&self, name: &str) -> Result<Var, String> {
        self.invoke(name, &[])
    }

    pub fn call(&self, name: &str, args: &[Arg]) -> Result<Var, String> {
        self.invoke(name, args)
    }

    /// A property that is itself an object.
    pub fn obj(&self, name: &str) -> Result<Disp, String> {
        self.get(name)?.into_disp().ok_or_else(|| format!("“{name}” is empty."))
    }

    /// Every member of a collection, fetched with one IEnumVARIANT::Next
    /// request instead of one Item(i) request each.
    pub fn enumerate(&self) -> Result<Vec<Var>, String> {
        let unk = self.get("_NewEnum")?;
        if unk.0.vt != VT_UNKNOWN && unk.0.vt != VT_DISPATCH {
            return Err("This collection can't be listed in one go.".into());
        }
        let p = unk.0.val[0] as *mut c_void;
        if p.is_null() {
            return Err("This collection can't be listed in one go.".into());
        }
        let mut en: *mut c_void = std::ptr::null_mut();
        count_call();
        let hr = unsafe { ((**(p as *mut *const DispatchVtbl)).query_interface)(p, &IID_IENUMVARIANT, &mut en) };
        if hr < 0 || en.is_null() {
            return Err("This collection can't be listed in one go.".into());
        }
        let vt = unsafe { &**(en as *mut *const EnumVariantVtbl) };
        let mut out = Vec::new();
        loop {
            let mut batch = vec![RawVariant::empty(); 256];
            let mut got = 0u32;
            count_call();
            let hr = unsafe { (vt.next)(en, batch.len() as u32, batch.as_mut_ptr(), &mut got) };
            out.extend(batch.into_iter().take(got as usize).map(Var));
            if hr != 0 || got == 0 {
                break; // S_FALSE: no more
            }
        }
        unsafe { (vt.release)(en) };
        Ok(out)
    }

    /// A text property; empty if it's missing.
    pub fn text(&self, name: &str) -> String {
        self.get(name).map(|v| v.to_text()).unwrap_or_default()
    }

    pub fn int(&self, name: &str) -> i64 {
        self.get(name).ok().and_then(|v| v.to_i64()).unwrap_or(0)
    }
}

/// An owned VARIANT result.
pub struct Var(RawVariant);

impl Drop for Var {
    fn drop(&mut self) {
        unsafe { windows_sys::Win32::System::Variant::VariantClear((&mut self.0 as *mut RawVariant).cast()) };
    }
}

impl Var {
    pub fn into_disp(mut self) -> Option<Disp> {
        match self.0.vt {
            VT_DISPATCH if self.0.val[0] != 0 => {
                let p = self.0.val[0] as *mut c_void;
                self.0.vt = VT_EMPTY; // ownership moves to Disp
                Some(Disp(p, None))
            }
            VT_UNKNOWN if self.0.val[0] != 0 => {
                let p = self.0.val[0] as *mut c_void;
                let mut out: *mut c_void = std::ptr::null_mut();
                unsafe {
                    let vt = &**(p as *mut *const DispatchVtbl);
                    if (vt.query_interface)(p, &IID_IDISPATCH, &mut out) >= 0 && !out.is_null() {
                        return Some(Disp(out, None));
                    }
                }
                None
            }
            _ => None,
        }
    }

    pub fn to_i64(&self) -> Option<i64> {
        let v = self.0.val[0];
        Some(match self.0.vt {
            VT_I1 => v as u8 as i8 as i64,
            VT_UI1 => v as u8 as i64,
            VT_I2 => v as u16 as i16 as i64,
            VT_UI2 => v as u16 as i64,
            VT_I4 | VT_INT => v as u32 as i32 as i64,
            VT_UI4 | VT_UINT => v as u32 as i64,
            VT_I8 | VT_UI8 | VT_CY => v as i64,
            VT_BOOL => (v as u16 != 0) as i64,
            VT_R8 => f64::from_bits(v) as i64,
            VT_R4 => f32::from_bits(v as u32) as i64,
            VT_BSTR => self.to_text().trim().parse().ok()?,
            _ => return None,
        })
    }

    fn array(&self) -> Option<*mut windows_sys::Win32::System::Com::SAFEARRAY> {
        (self.0.vt & VT_ARRAY != 0 && self.0.val[0] != 0).then(|| self.0.val[0] as *mut _)
    }

    /// Bounds of one dimension (1-based dimension number).
    fn bounds(psa: *mut windows_sys::Win32::System::Com::SAFEARRAY, dim: u32) -> (i32, i32) {
        use windows_sys::Win32::System::Ole::{SafeArrayGetLBound, SafeArrayGetUBound};
        let (mut lo, mut hi) = (0i32, -1i32);
        unsafe {
            SafeArrayGetLBound(psa, dim, &mut lo);
            SafeArrayGetUBound(psa, dim, &mut hi);
        }
        (lo, hi)
    }

    fn element(psa: *mut windows_sys::Win32::System::Com::SAFEARRAY, idx: &[i32]) -> Var {
        let mut v = RawVariant::empty();
        unsafe { windows_sys::Win32::System::Ole::SafeArrayGetElement(psa, idx.as_ptr(), (&mut v as *mut RawVariant).cast()) };
        Var(v)
    }

    /// A one-dimensional array of VARIANTs.
    pub fn to_vec(&self) -> Vec<Var> {
        let Some(psa) = self.array() else { return Vec::new() };
        if self.0.vt & 0xFFF != VT_VARIANT {
            return Vec::new();
        }
        let (lo, hi) = Self::bounds(psa, 1);
        (lo..=hi).map(|i| Self::element(psa, &[i])).collect()
    }

    /// A two-dimensional array of VARIANTs, as rows of columns.
    pub fn to_rows(&self) -> Vec<Vec<Var>> {
        let Some(psa) = self.array() else { return Vec::new() };
        if self.0.vt & 0xFFF != VT_VARIANT || unsafe { windows_sys::Win32::System::Ole::SafeArrayGetDim(psa) } != 2 {
            return Vec::new();
        }
        let (r0, r1) = Self::bounds(psa, 1);
        let (c0, c1) = Self::bounds(psa, 2);
        (r0..=r1).map(|r| (c0..=c1).map(|c| Self::element(psa, &[r, c])).collect()).collect()
    }

    /// A byte array (binary property), as upper-case hex.
    pub fn to_hex(&self) -> String {
        let Some(psa) = self.array() else { return String::new() };
        if self.0.vt & 0xFFF != VT_UI1 {
            return String::new();
        }
        let (lo, hi) = Self::bounds(psa, 1);
        let mut out = String::with_capacity(((hi - lo + 1).max(0) as usize) * 2);
        unsafe {
            let mut data: *mut c_void = std::ptr::null_mut();
            if windows_sys::Win32::System::Ole::SafeArrayAccessData(psa, &mut data) >= 0 {
                let bytes = std::slice::from_raw_parts(data as *const u8, (hi - lo + 1).max(0) as usize);
                for b in bytes {
                    out.push_str(&format!("{b:02X}"));
                }
                windows_sys::Win32::System::Ole::SafeArrayUnaccessData(psa);
            }
        }
        out
    }

    pub fn is_error(&self) -> bool {
        self.0.vt == VT_ERROR
    }

    /// An OLE date (days since 30 Dec 1899, local time).
    pub fn to_date(&self) -> Option<f64> {
        (self.0.vt == VT_DATE).then(|| f64::from_bits(self.0.val[0]))
    }

    pub fn to_text(&self) -> String {
        match self.0.vt {
            VT_BSTR => unsafe { bstr_to_string(self.0.val[0] as *const u16) },
            VT_R8 => f64::from_bits(self.0.val[0]).to_string(),
            VT_EMPTY => String::new(),
            _ => self.to_i64().map(|i| i.to_string()).unwrap_or_default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exercise the helper on a COM object every Windows PC has.
    #[test]
    fn drives_scripting_dictionary() {
        init_thread();
        let d = Disp::create("Scripting.Dictionary").unwrap();
        d.call("Add", &[Arg::Str("greeting"), Arg::Str("hello public folders")]).unwrap();
        d.call("Add", &[Arg::Str("n"), Arg::I4(42)]).unwrap();
        assert_eq!(d.int("Count"), 2);
        assert_eq!(d.call("Item", &[Arg::Str("greeting")]).unwrap().to_text(), "hello public folders");
        assert_eq!(d.call("Item", &[Arg::Str("n")]).unwrap().to_i64(), Some(42));
        assert!(d.call("Exists", &[Arg::Str("missing")]).unwrap().to_i64() == Some(0));
        assert!(d.get("NoSuchThing").is_err());
        let _ = Arg::Bool(true);
        assert!(is_registered("Scripting.Dictionary"));
    }
}
