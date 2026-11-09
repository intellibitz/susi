//! Owner-only file ACLs through Win32, owned by the facade so `susi-core`,
//! `susi-error` and `susi-daemon` stay free of `unsafe` and never launch
//! `icacls.exe` to set or read a file's access list.
//!
//! Launching `icacls` from a process without a console (the detached daemon,
//! a test binary under the gate) made Windows allocate a new, visible console
//! window for every call (2026-11-09: a test run popped "error 0x800700e8
//! when launching `icacls` ... node.key"). These calls start no process.
//!
//! - [`restrict_to_current_user`] gives a file the access list that
//!   `icacls <path> /inheritance:r /grant:r <user>:F` gave a newly created
//!   file: a protected DACL (no inherited entries) with one explicit entry
//!   granting the current user full control (`FILE_ALL_ACCESS`). The user is
//!   the process token's user, not the `USERNAME` environment variable.
//! - [`read_dacl`] lists a file's allow and deny entries in order.
//!
//! References: [SetNamedSecurityInfoW](https://learn.microsoft.com/windows/win32/api/aclapi/nf-aclapi-setnamedsecurityinfow),
//! [GetNamedSecurityInfoW](https://learn.microsoft.com/windows/win32/api/aclapi/nf-aclapi-getnamedsecurityinfow),
//! [SID string format](https://learn.microsoft.com/windows/win32/secauthz/sid-components).
//! No raw pointer crosses out of this module.

use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use winapi::shared::minwindef::{DWORD, LPVOID};
use winapi::shared::ntdef::HANDLE;
use winapi::um::accctrl::SE_FILE_OBJECT;
use winapi::um::aclapi::{GetNamedSecurityInfoW, SetNamedSecurityInfoW};
use winapi::um::handleapi::CloseHandle;
use winapi::um::processthreadsapi::{GetCurrentProcess, OpenProcessToken};
use winapi::um::securitybaseapi::{
    AddAccessAllowedAce, GetAce, GetLengthSid, GetTokenInformation, InitializeAcl, IsValidSid,
};
use winapi::um::winbase::LocalFree;
use winapi::um::winnt::{
    TokenUser, ACCESS_ALLOWED_ACE, ACCESS_ALLOWED_ACE_TYPE, ACCESS_DENIED_ACE_TYPE, ACE_HEADER,
    ACL, ACL_REVISION, DACL_SECURITY_INFORMATION, INHERITED_ACE, PACL,
    PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, TOKEN_QUERY, TOKEN_USER,
};

pub use winapi::um::winnt::FILE_ALL_ACCESS;

/// One allow or deny entry of a file's DACL, in list order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ace {
    /// The trustee in SID string form (`S-1-1-0` is Everyone).
    pub sid: String,
    /// A deny entry; otherwise an allow entry.
    pub deny: bool,
    /// Inherited from the parent (`INHERITED_ACE`), not set on the file.
    pub inherited: bool,
    /// The access mask exactly as stored (generic bits are not mapped).
    pub mask: u32,
}

/// Restrict `path` to the current user: a protected DACL with a single
/// entry granting the process token's user `FILE_ALL_ACCESS`.
pub fn restrict_to_current_user(path: &Path) -> io::Result<()> {
    let user = current_user_sid()?;
    set_protected_dacl(path, &[(user.as_str(), FILE_ALL_ACCESS)])
}

/// Replace `path`'s DACL with a protected list (inherited entries removed)
/// of allow entries, one per `(SID string, access mask)` pair, in order.
#[allow(unsafe_code)]
pub fn set_protected_dacl(path: &Path, grants: &[(&str, u32)]) -> io::Result<()> {
    let sids = grants
        .iter()
        .map(|(sid, _)| SidBuf::parse(sid))
        .collect::<io::Result<Vec<_>>>()?;
    // ACL header, then per entry an ACCESS_ALLOWED_ACE whose trailing
    // `SidStart` DWORD is the first word of the SID itself.
    let entry_fixed = std::mem::size_of::<ACCESS_ALLOWED_ACE>() - std::mem::size_of::<DWORD>();
    let size = sids.iter().fold(std::mem::size_of::<ACL>(), |total, sid| {
        total + entry_fixed + sid.len
    });
    let size = (size + 3) & !3;
    let size_dword = DWORD::try_from(size)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "access list too large"))?;
    let mut acl_buf = vec![0u32; size / 4];
    let acl = acl_buf.as_mut_ptr().cast::<ACL>();
    // SAFETY: `acl_buf` is a writable, DWORD-aligned buffer of exactly
    // `size_dword` bytes that outlives every call below, as InitializeAcl
    // requires.
    if unsafe { InitializeAcl(acl, size_dword, ACL_REVISION as DWORD) } == 0 {
        return Err(io::Error::last_os_error());
    }
    for (sid, (_, mask)) in sids.iter().zip(grants) {
        // SAFETY: `acl` was initialised above with room for every entry;
        // `sid.as_psid()` points at a valid, DWORD-aligned SID of `sid.len`
        // bytes owned by `sids`, alive for the call. The ACL copies the SID.
        if unsafe { AddAccessAllowedAce(acl, ACL_REVISION as DWORD, *mask, sid.as_psid()) } == 0 {
            return Err(io::Error::last_os_error());
        }
    }
    let mut name = wide_path(path)?;
    // SAFETY: `name` is a NUL-terminated UTF-16 path owned for the call;
    // only the DACL is set (owner, group and SACL pointers are null, as the
    // security-information flags say), and `acl` is the valid ACL above.
    let status = unsafe {
        SetNamedSecurityInfoW(
            name.as_mut_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            acl,
            std::ptr::null_mut(),
        )
    };
    win32_status(status)
}

/// `path`'s DACL entries in order; `None` for a NULL DACL, which grants
/// everyone full access. Entry types other than allow and deny (object,
/// callback, audit) are not listed.
#[allow(unsafe_code)]
pub fn read_dacl(path: &Path) -> io::Result<Option<Vec<Ace>>> {
    let name = wide_path(path)?;
    let mut dacl: PACL = std::ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: `name` is a NUL-terminated UTF-16 path owned for the call;
    // `dacl` and `descriptor` are writable out-pointers; the owner, group
    // and SACL out-pointers are null because only the DACL is requested.
    let status = unsafe {
        GetNamedSecurityInfoW(
            name.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut dacl,
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    win32_status(status)?;
    let entries = if dacl.is_null() {
        Ok(None)
    } else {
        dacl_entries(dacl).map(Some)
    };
    // SAFETY: on success GetNamedSecurityInfoW returned `descriptor` from
    // LocalAlloc and the caller frees it with LocalFree; `dacl` points into
    // it and is not used after this line.
    unsafe { LocalFree(descriptor) };
    entries
}

/// The process token's user as a SID string (`S-1-5-21-...`).
#[allow(unsafe_code)]
pub fn current_user_sid() -> io::Result<String> {
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: GetCurrentProcess returns a pseudo-handle that needs no
    // closing; `token` is a writable out-pointer.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let result = token_user_sid(token);
    // SAFETY: `token` was opened above and is closed exactly once.
    unsafe { CloseHandle(token) };
    result
}

#[allow(unsafe_code)]
fn token_user_sid(token: HANDLE) -> io::Result<String> {
    let mut needed: DWORD = 0;
    // SAFETY: a size query: no buffer (null, length 0), `needed` writable.
    // It fails with ERROR_INSUFFICIENT_BUFFER and reports the size.
    unsafe { GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut needed) };
    if (needed as usize) < std::mem::size_of::<TOKEN_USER>() {
        return Err(io::Error::last_os_error());
    }
    // u64 storage keeps TOKEN_USER's pointer field aligned.
    let mut buf = vec![0u64; (needed as usize).div_ceil(8)];
    // SAFETY: `buf` is writable, 8-byte aligned and at least `needed` bytes.
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            buf.as_mut_ptr().cast(),
            needed,
            &mut needed,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: on success `buf` holds a TOKEN_USER whose `User.Sid` points
    // into `buf`, which stays alive while the SID is copied out.
    let sid = unsafe { (*buf.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    sid_string_at(sid, usize::MAX).ok_or_else(|| io::Error::other("token user SID is invalid"))
}

#[allow(unsafe_code)]
fn dacl_entries(dacl: PACL) -> io::Result<Vec<Ace>> {
    // SAFETY: `dacl` is the non-null DACL GetNamedSecurityInfoW returned,
    // valid while its descriptor lives (the caller frees it afterwards).
    let count = unsafe { (*dacl).AceCount };
    let mut entries = Vec::with_capacity(usize::from(count));
    for index in 0..count {
        let mut ace: LPVOID = std::ptr::null_mut();
        // SAFETY: `index` is below the list's AceCount; `ace` is writable.
        if unsafe { GetAce(dacl, DWORD::from(index), &mut ace) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: GetAce succeeded, so `ace` points at an entry inside the
        // DACL that starts with an ACE_HEADER.
        let header = unsafe { *ace.cast::<ACE_HEADER>() };
        let deny = match header.AceType {
            ACCESS_ALLOWED_ACE_TYPE => false,
            ACCESS_DENIED_ACE_TYPE => true,
            _ => continue,
        };
        let fixed = std::mem::size_of::<ACCESS_ALLOWED_ACE>() - std::mem::size_of::<DWORD>();
        let Some(room) = usize::from(header.AceSize).checked_sub(fixed) else {
            return Err(io::Error::other("DACL entry shorter than its header"));
        };
        // Allow and deny entries share ACCESS_ALLOWED_ACE's layout: header,
        // mask, then the SID starting at `SidStart`.
        let entry = ace.cast::<ACCESS_ALLOWED_ACE>();
        // SAFETY: the entry is an allow or deny ACE of at least `fixed`
        // bytes (checked above), so `Mask` and the `SidStart` address are
        // inside it.
        let (mask, sid) = unsafe {
            (
                (*entry).Mask,
                std::ptr::addr_of!((*entry).SidStart).cast_mut().cast(),
            )
        };
        let sid = sid_string_at(sid, room)
            .ok_or_else(|| io::Error::other("DACL entry holds an invalid SID"))?;
        entries.push(Ace {
            sid,
            deny,
            inherited: header.AceFlags & INHERITED_ACE != 0,
            mask,
        });
    }
    Ok(entries)
}

/// Copy the SID at `sid` (at most `room` bytes long) into its string form.
#[allow(unsafe_code)]
fn sid_string_at(sid: PSID, room: usize) -> Option<String> {
    // SAFETY: callers pass a SID pointer from the token or a DACL entry;
    // IsValidSid checks its structure before its length is read.
    if sid.is_null() || unsafe { IsValidSid(sid) } == 0 {
        return None;
    }
    // SAFETY: the SID is valid (checked above).
    let len = unsafe { GetLengthSid(sid) } as usize;
    if len > room {
        return None;
    }
    // SAFETY: a valid SID occupies exactly `len` readable bytes, which fit
    // inside its containing buffer (`room`).
    let bytes = unsafe { std::slice::from_raw_parts(sid.cast::<u8>(), len) };
    sid_to_string(bytes)
}

/// `S-<revision>-<authority>-<sub>...` from a SID's binary form.
fn sid_to_string(bytes: &[u8]) -> Option<String> {
    let (&revision, rest) = bytes.split_first()?;
    let (&count, rest) = rest.split_first()?;
    let (authority, subs) = rest.split_at_checked(6)?;
    if subs.len() != usize::from(count) * 4 {
        return None;
    }
    let authority = authority
        .iter()
        .fold(0u64, |value, byte| (value << 8) | u64::from(*byte));
    let mut text = if authority >> 32 == 0 {
        format!("S-{revision}-{authority}")
    } else {
        format!("S-{revision}-0x{authority:012X}")
    };
    for sub in subs.chunks_exact(4) {
        let value = u32::from_le_bytes([sub[0], sub[1], sub[2], sub[3]]);
        text.push_str(&format!("-{value}"));
    }
    Some(text)
}

/// A SID in its binary form, DWORD-aligned for Win32.
struct SidBuf {
    words: Vec<u32>,
    len: usize,
}

impl SidBuf {
    /// Parse `S-1-<authority>-<sub>...` (decimal or `0x` hex authority,
    /// at most 15 sub-authorities).
    fn parse(text: &str) -> io::Result<Self> {
        let invalid = || {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("not a SID string: {text}"),
            )
        };
        let mut parts = text.split('-');
        if !parts.next().is_some_and(|s| s.eq_ignore_ascii_case("S")) {
            return Err(invalid());
        }
        let revision: u8 = parts
            .next()
            .and_then(|s| s.parse().ok())
            .ok_or_else(invalid)?;
        let authority_text = parts.next().ok_or_else(invalid)?;
        let authority = match authority_text
            .strip_prefix("0x")
            .or_else(|| authority_text.strip_prefix("0X"))
        {
            Some(hex) => u64::from_str_radix(hex, 16).ok(),
            None => authority_text.parse().ok(),
        }
        .filter(|value| value >> 48 == 0)
        .ok_or_else(invalid)?;
        let subs = parts
            .map(|s| s.parse::<u32>().map_err(|_| invalid()))
            .collect::<io::Result<Vec<_>>>()?;
        let count = u8::try_from(subs.len())
            .ok()
            .filter(|count| *count <= 15)
            .ok_or_else(invalid)?;
        let mut bytes = vec![revision, count];
        bytes.extend_from_slice(&authority.to_be_bytes()[2..]);
        for sub in subs {
            bytes.extend_from_slice(&sub.to_le_bytes());
        }
        let len = bytes.len();
        bytes.resize(len.div_ceil(4) * 4, 0);
        let words = bytes
            .chunks_exact(4)
            .map(|w| u32::from_ne_bytes([w[0], w[1], w[2], w[3]]))
            .collect();
        let sid = Self { words, len };
        if sid_string_at(sid.as_psid(), sid.len).is_none() {
            return Err(invalid());
        }
        Ok(sid)
    }

    fn as_psid(&self) -> PSID {
        self.words.as_ptr().cast_mut().cast()
    }
}

fn wide_path(path: &Path) -> io::Result<Vec<u16>> {
    let mut wide: Vec<u16> = OsStr::new(path).encode_wide().collect();
    if wide.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path contains NUL",
        ));
    }
    wide.push(0);
    Ok(wide)
}

fn win32_status(status: DWORD) -> io::Result<()> {
    if status == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(
            i32::try_from(status).unwrap_or(i32::MAX),
        ))
    }
}

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod inline_tests {
    use super::*;

    fn scratch_file(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "susi-acl-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("secret.key");
        std::fs::write(&file, b"secret").unwrap();
        (dir, file)
    }

    #[test]
    fn sid_strings_round_trip_through_the_binary_form() {
        for text in [
            "S-1-1-0",
            "S-1-5-11",
            "S-1-5-18",
            "S-1-5-32-545",
            "S-1-5-21-1004336348-1177238915-682003330-1001",
        ] {
            let sid = SidBuf::parse(text).unwrap();
            assert_eq!(sid_string_at(sid.as_psid(), sid.len).unwrap(), text);
        }
        for bad in ["", "S-1", "X-1-1-0", "S-1-1-x", "S-1-281474976710656-0"] {
            assert!(SidBuf::parse(bad).is_err(), "{bad:?} must be refused");
        }
    }

    #[test]
    fn a_restricted_file_grants_only_the_current_user_full_control() {
        let (dir, file) = scratch_file("restrict");
        // A file in the temp directory starts with inherited entries.
        let before = read_dacl(&file).unwrap().unwrap();
        assert!(before.iter().any(|ace| ace.inherited), "{before:?}");

        restrict_to_current_user(&file).unwrap();
        let after = read_dacl(&file).unwrap().unwrap();
        assert_eq!(
            after,
            vec![Ace {
                sid: current_user_sid().unwrap(),
                deny: false,
                inherited: false,
                mask: FILE_ALL_ACCESS,
            }]
        );
        // The owner can still read, rewrite and delete it.
        assert_eq!(std::fs::read(&file).unwrap(), b"secret");
        std::fs::write(&file, b"rotated").unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_protected_list_holds_exactly_the_requested_grants_in_order() {
        let (dir, file) = scratch_file("grants");
        let user = current_user_sid().unwrap();
        set_protected_dacl(
            &file,
            &[(user.as_str(), FILE_ALL_ACCESS), ("S-1-1-0", 0x0012_0089)],
        )
        .unwrap();
        let aces = read_dacl(&file).unwrap().unwrap();
        assert_eq!(
            aces.iter()
                .map(|a| (a.sid.as_str(), a.deny, a.inherited, a.mask))
                .collect::<Vec<_>>(),
            vec![
                (user.as_str(), false, false, FILE_ALL_ACCESS),
                ("S-1-1-0", false, false, 0x0012_0089),
            ]
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_missing_file_is_an_error_not_a_panic() {
        let missing = std::env::temp_dir().join(format!("susi-acl-missing-{}", std::process::id()));
        assert!(read_dacl(&missing).is_err());
        assert!(restrict_to_current_user(&missing).is_err());
        assert!(set_protected_dacl(Path::new("a\0b"), &[]).is_err());
    }
}
