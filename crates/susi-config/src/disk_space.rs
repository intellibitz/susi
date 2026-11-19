//! Native Linux filesystem capacity: `(bytes available to this caller, total bytes)`.
//! Query failure is `None`; callers retain their own path and fallback policy.

use std::path::Path;

#[cfg(unix)]
pub fn available(path: &Path) -> Option<(u64, u64)> {
    let path = std::ffi::CString::new(path.to_str()?).ok()?;
    // SAFETY: statvfs overwrites a live, zeroed C output struct; path is
    // NUL-terminated and remains alive for the call.
    #[allow(unsafe_code)]
    let stat = unsafe {
        let mut stat: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(path.as_ptr(), &mut stat) != 0 {
            return None;
        }
        stat
    };
    Some((
        (stat.f_bavail as u64) * (stat.f_frsize as u64),
        (stat.f_blocks as u64) * (stat.f_frsize as u64),
    ))
}

#[cfg(not(unix))]
pub fn available(_path: &Path) -> Option<(u64, u64)> {
    None
}
