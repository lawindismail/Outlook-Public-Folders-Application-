//! Minimal NTLMv2 client (MS-NLMP) for signing in to on-premises Exchange.

use hmac::{Hmac, Mac};
use md4::{Digest, Md4};
use md5::Md5;

type HmacMd5 = Hmac<Md5>;

const NEGOTIATE_UNICODE: u32 = 0x0000_0001;
const REQUEST_TARGET: u32 = 0x0000_0004;
const NEGOTIATE_NTLM: u32 = 0x0000_0200;
const ALWAYS_SIGN: u32 = 0x0000_8000;
const EXTENDED_SESSION_SECURITY: u32 = 0x0008_0000;
const TARGET_INFO: u32 = 0x0080_0000;
const NEGOTIATE_128: u32 = 0x2000_0000;
const NEGOTIATE_56: u32 = 0x8000_0000;

const FLAGS: u32 = NEGOTIATE_UNICODE
    | REQUEST_TARGET
    | NEGOTIATE_NTLM
    | ALWAYS_SIGN
    | EXTENDED_SESSION_SECURITY
    | TARGET_INFO
    | NEGOTIATE_128
    | NEGOTIATE_56;

pub struct Challenge {
    server_challenge: [u8; 8],
    flags: u32,
    target_info: Vec<u8>,
}

fn u16le(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}
fn u32le(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}
fn utf16(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(|c| c.to_le_bytes()).collect()
}

/// Type 1 (negotiate) message.
pub fn negotiate() -> Vec<u8> {
    let mut m = Vec::with_capacity(32);
    m.extend_from_slice(b"NTLMSSP\0");
    m.extend_from_slice(&1u32.to_le_bytes());
    m.extend_from_slice(&FLAGS.to_le_bytes());
    m.extend_from_slice(&[0u8; 16]); // empty domain and workstation buffers
    m
}

/// Parse the server's type 2 (challenge) message.
pub fn parse_challenge(b: &[u8]) -> Result<Challenge, String> {
    if b.len() < 32 || &b[0..8] != b"NTLMSSP\0" || u32le(b, 8) != 2 {
        return Err("The server sent an unexpected sign-in challenge.".into());
    }
    let flags = u32le(b, 20);
    let mut server_challenge = [0u8; 8];
    server_challenge.copy_from_slice(&b[24..32]);
    let mut target_info = Vec::new();
    if b.len() >= 48 {
        let len = u16le(b, 40) as usize;
        let off = u32le(b, 44) as usize;
        if off + len <= b.len() {
            target_info = b[off..off + len].to_vec();
        }
    }
    Ok(Challenge { server_challenge, flags, target_info })
}

fn av_timestamp(info: &[u8]) -> Option<[u8; 8]> {
    let mut i = 0;
    while i + 4 <= info.len() {
        let id = u16le(info, i);
        let len = u16le(info, i + 2) as usize;
        if id == 0 {
            break;
        }
        if id == 7 && len == 8 && i + 12 <= info.len() {
            let mut t = [0u8; 8];
            t.copy_from_slice(&info[i + 4..i + 12]);
            return Some(t);
        }
        i += 4 + len;
    }
    None
}

fn filetime_now() -> [u8; 8] {
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let t = (d.as_secs() + 11_644_473_600) * 10_000_000 + u64::from(d.subsec_nanos() / 100);
    t.to_le_bytes()
}

fn hmac(key: &[u8], parts: &[&[u8]]) -> Vec<u8> {
    let mut mac = HmacMd5::new_from_slice(key).expect("HMAC accepts any key length");
    for p in parts {
        mac.update(p);
    }
    mac.finalize().into_bytes().to_vec()
}

/// NTOWFv2: the NTLMv2 key derived from the password, user and domain.
fn ntowfv2(password: &str, user: &str, domain: &str) -> Vec<u8> {
    let nt_hash = Md4::digest(utf16(password));
    hmac(&nt_hash, &[&utf16(&(user.to_uppercase() + domain))])
}

/// The NTLMv2 response: NTProofStr followed by the client blob.
fn nt_response(ntv2: &[u8], server_challenge: &[u8; 8], client_challenge: &[u8; 8], timestamp: &[u8; 8], target_info: &[u8]) -> Vec<u8> {
    let mut blob = vec![1, 1, 0, 0, 0, 0, 0, 0];
    blob.extend_from_slice(timestamp);
    blob.extend_from_slice(client_challenge);
    blob.extend_from_slice(&[0; 4]);
    blob.extend_from_slice(target_info);
    blob.extend_from_slice(&[0; 4]);
    let mut nt = hmac(ntv2, &[server_challenge, &blob]);
    nt.extend_from_slice(&blob);
    nt
}

/// Type 3 (authenticate) message with an NTLMv2 response.
pub fn authenticate(ch: &Challenge, user: &str, domain: &str, password: &str, workstation: &str) -> Vec<u8> {
    let ntv2 = ntowfv2(password, user, domain);

    let mut client_challenge = [0u8; 8];
    let _ = getrandom::getrandom(&mut client_challenge);
    let (timestamp, server_time) = match av_timestamp(&ch.target_info) {
        Some(t) => (t, true),
        None => (filetime_now(), false),
    };

    let nt = nt_response(&ntv2, &ch.server_challenge, &client_challenge, &timestamp, &ch.target_info);

    let lm = if server_time {
        vec![0u8; 24]
    } else {
        let mut v = hmac(&ntv2, &[&ch.server_challenge, &client_challenge]);
        v.extend_from_slice(&client_challenge);
        v
    };

    let d = utf16(domain);
    let u = utf16(user);
    let w = utf16(workstation);

    let mut header = Vec::with_capacity(64);
    let mut payload = Vec::new();
    let base = 64u32;
    header.extend_from_slice(b"NTLMSSP\0");
    header.extend_from_slice(&3u32.to_le_bytes());
    // Order of buffers in header: LM, NT, domain, user, workstation, session key.
    // Order of data in payload: domain, user, workstation, LM, NT.
    let mut offsets = [0u32; 5];
    for (i, data) in [&d, &u, &w, &lm, &nt].iter().enumerate() {
        offsets[i] = base + payload.len() as u32;
        payload.extend_from_slice(data);
    }
    let secbuf = |h: &mut Vec<u8>, len: usize, off: u32| {
        h.extend_from_slice(&(len as u16).to_le_bytes());
        h.extend_from_slice(&(len as u16).to_le_bytes());
        h.extend_from_slice(&off.to_le_bytes());
    };
    secbuf(&mut header, lm.len(), offsets[3]);
    secbuf(&mut header, nt.len(), offsets[4]);
    secbuf(&mut header, d.len(), offsets[0]);
    secbuf(&mut header, u.len(), offsets[1]);
    secbuf(&mut header, w.len(), offsets[2]);
    secbuf(&mut header, 0, base + payload.len() as u32);
    let flags = (ch.flags & FLAGS) | NEGOTIATE_UNICODE | NEGOTIATE_NTLM;
    header.extend_from_slice(&flags.to_le_bytes());
    header.extend_from_slice(&payload);
    header
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    // Test values from Microsoft's NTLM specification (MS-NLMP 4.2.4).
    const SERVER_NAME: [u8; 36] = [
        0x02, 0x00, 0x0c, 0x00, 0x44, 0x00, 0x6f, 0x00, 0x6d, 0x00, 0x61, 0x00, 0x69, 0x00, 0x6e, 0x00, 0x01, 0x00, 0x0c, 0x00, 0x53,
        0x00, 0x65, 0x00, 0x72, 0x00, 0x76, 0x00, 0x65, 0x00, 0x72, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];

    #[test]
    fn ntlmv2_matches_spec() {
        let key = ntowfv2("Password", "User", "Domain");
        assert_eq!(hex(&key), "0c868a403bfd7a93a3001ef22ef02e3f");
        let server = [0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef];
        let client = [0xaa; 8];
        let nt = nt_response(&key, &server, &client, &[0; 8], &SERVER_NAME);
        assert_eq!(hex(&nt[..16]), "68cd0ab851e51c96aabc927bebef6a1c");
    }

    #[test]
    fn messages_are_well_formed() {
        let n = negotiate();
        assert_eq!(&n[..8], b"NTLMSSP\0");
        // A challenge message carrying the spec's target info.
        let mut c = b"NTLMSSP\0".to_vec();
        c.extend_from_slice(&2u32.to_le_bytes());
        c.extend_from_slice(&[0; 8]); // target name
        c.extend_from_slice(&FLAGS.to_le_bytes());
        c.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        c.extend_from_slice(&[0; 8]);
        c.extend_from_slice(&(SERVER_NAME.len() as u16).to_le_bytes());
        c.extend_from_slice(&(SERVER_NAME.len() as u16).to_le_bytes());
        c.extend_from_slice(&48u32.to_le_bytes());
        c.extend_from_slice(&SERVER_NAME);
        let ch = parse_challenge(&c).unwrap();
        assert_eq!(ch.target_info, SERVER_NAME);
        let a = authenticate(&ch, "User", "Domain", "Password", "PC");
        assert_eq!(&a[..8], b"NTLMSSP\0");
        assert_eq!(u32le(&a, 8), 3);
        // The user name buffer points at "User" in UTF-16.
        let (len, off) = (u16le(&a, 36) as usize, u32le(&a, 40) as usize);
        assert_eq!(&a[off..off + len], utf16("User").as_slice());
        // The NT response buffer holds 16 bytes of proof plus the blob.
        let nt_len = u16le(&a, 20) as usize;
        assert_eq!(nt_len, 16 + 28 + SERVER_NAME.len() + 4);
        assert_eq!(split_user("CORP\\sam"), ("sam".to_string(), "CORP".to_string()));
    }
}

/// Split "DOMAIN\user" into (user, domain). A UPN ("user@company.com") is
/// used whole as the user name with an empty domain.
pub fn split_user(name: &str) -> (String, String) {
    if let Some((domain, user)) = name.split_once('\\') {
        (user.to_string(), domain.to_string())
    } else {
        (name.to_string(), String::new())
    }
}
