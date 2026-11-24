//! "Sign in as me": use the Windows login of whoever is using this PC
//! (SSPI Negotiate/NTLM), so no user name or password has to be typed.

/// The signed-in Windows user's email-style name (UPN), when the PC belongs
/// to a company domain.
#[cfg(windows)]
pub fn current_upn() -> Option<String> {
    use windows_sys::Win32::Security::Authentication::Identity::{GetUserNameExW, NameUserPrincipal};
    let mut buf = vec![0u16; 512];
    let mut len = buf.len() as u32;
    let ok = unsafe { GetUserNameExW(NameUserPrincipal, buf.as_mut_ptr(), &mut len) };
    if !ok || len == 0 {
        return None;
    }
    let s = String::from_utf16_lossy(&buf[..len as usize]);
    s.contains('@').then_some(s)
}

#[cfg(not(windows))]
pub fn current_upn() -> Option<String> {
    None
}

/// Whether this PC is joined to a company (Active Directory) domain.
pub fn on_company_domain() -> bool {
    let domain = std::env::var("USERDOMAIN").unwrap_or_default();
    let computer = std::env::var("COMPUTERNAME").unwrap_or_default();
    cfg!(windows) && !domain.is_empty() && !domain.eq_ignore_ascii_case(&computer)
}

#[cfg(windows)]
pub use imp::Context;

#[cfg(all(test, windows))]
mod tests {
    #[test]
    fn windows_login_makes_a_token() {
        let mut ctx = super::Context::new("NTLM", "localhost").unwrap();
        let token = ctx.step(None).unwrap();
        assert_eq!(&token[..8], b"NTLMSSP\0");
        let _ = super::on_company_domain();
        let _ = super::current_upn();
    }
}

#[cfg(windows)]
mod imp {
    use std::ptr::null;
    use windows_sys::Win32::Security::Authentication::Identity::*;
    use windows_sys::Win32::Security::Credentials::SecHandle;

    const SEC_E_OK: i32 = 0;
    const SEC_I_CONTINUE_NEEDED: i32 = 0x0009_0312;
    const SEC_I_COMPLETE_NEEDED: i32 = 0x0009_0313;
    const SEC_I_COMPLETE_AND_CONTINUE: i32 = 0x0009_0314;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// One sign-in conversation with a server.
    pub struct Context {
        cred: SecHandle,
        ctx: SecHandle,
        has_ctx: bool,
        target: Vec<u16>,
    }

    impl Context {
        /// `package` is "Negotiate" or "NTLM"; `host` is the server's name.
        pub fn new(package: &str, host: &str) -> Result<Self, String> {
            let pkg = wide(package);
            let mut cred = SecHandle { dwLower: 0, dwUpper: 0 };
            let mut expiry = 0i64;
            let st = unsafe {
                AcquireCredentialsHandleW(
                    null(),
                    pkg.as_ptr(),
                    SECPKG_CRED_OUTBOUND,
                    null(),
                    null(),
                    None,
                    null(),
                    &mut cred,
                    &mut expiry,
                )
            };
            if st != SEC_E_OK {
                return Err(format!("Windows couldn't use your login for this server (code {st:#x})."));
            }
            Ok(Self { cred, ctx: SecHandle { dwLower: 0, dwUpper: 0 }, has_ctx: false, target: wide(&format!("HTTP/{host}")) })
        }

        /// Make the next token, given the server's last token (if any).
        pub fn step(&mut self, input: Option<&[u8]>) -> Result<Vec<u8>, String> {
            let mut out = vec![0u8; 64 * 1024];
            let mut out_buf = SecBuffer { cbBuffer: out.len() as u32, BufferType: SECBUFFER_TOKEN, pvBuffer: out.as_mut_ptr().cast() };
            let mut out_desc = SecBufferDesc { ulVersion: SECBUFFER_VERSION, cBuffers: 1, pBuffers: &mut out_buf };
            let mut in_data = input.map(|d| d.to_vec()).unwrap_or_default();
            let mut in_buf =
                SecBuffer { cbBuffer: in_data.len() as u32, BufferType: SECBUFFER_TOKEN, pvBuffer: in_data.as_mut_ptr().cast() };
            let in_desc = SecBufferDesc { ulVersion: SECBUFFER_VERSION, cBuffers: 1, pBuffers: &mut in_buf };
            let mut attrs = 0u32;
            let mut expiry = 0i64;
            let ctx_ptr: *mut SecHandle = &mut self.ctx;
            let ctx_in: *const SecHandle = if self.has_ctx { ctx_ptr } else { null() };
            let st = unsafe {
                InitializeSecurityContextW(
                    &self.cred,
                    ctx_in,
                    self.target.as_ptr(),
                    ISC_REQ_CONNECTION,
                    0,
                    SECURITY_NATIVE_DREP,
                    if input.is_some() { &in_desc } else { null() },
                    0,
                    ctx_ptr,
                    &mut out_desc,
                    &mut attrs,
                    &mut expiry,
                )
            };
            if st != SEC_E_OK && st != SEC_I_CONTINUE_NEEDED && st != SEC_I_COMPLETE_NEEDED && st != SEC_I_COMPLETE_AND_CONTINUE {
                return Err(format!("Windows sign-in failed (code {:#x}).", st as u32));
            }
            self.has_ctx = true;
            if st == SEC_I_COMPLETE_NEEDED || st == SEC_I_COMPLETE_AND_CONTINUE {
                unsafe { CompleteAuthToken(&self.ctx, &out_desc) };
            }
            out.truncate(out_buf.cbBuffer as usize);
            Ok(out)
        }
    }

    impl Drop for Context {
        fn drop(&mut self) {
            unsafe {
                if self.has_ctx {
                    DeleteSecurityContext(&self.ctx);
                }
                FreeCredentialsHandle(&self.cred);
            }
        }
    }
}
