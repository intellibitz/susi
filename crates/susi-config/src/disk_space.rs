//! Native filesystem capacity: `(bytes available to this caller, total bytes)`.
//! Query failure is `None`; callers retain their own path and fallback policy.

use std::path::Path;

/// Unix queries the exact UTF-8 path. Windows queries the nearest existing
/// ancestor, then the current directory, then `C:\`, matching the native probe.
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

#[cfg(windows)]
pub fn available(path: &Path) -> Option<(u64, u64)> {
    use std::os::windows::ffi::OsStrExt;
    #[allow(non_snake_case, unsafe_code)] // Exact OS ABI declaration, no owned handle.
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetDiskFreeSpaceExW(
            directory_name: *const u16,
            free_bytes_available_to_caller: *mut u64,
            total_number_of_bytes: *mut u64,
            total_number_of_free_bytes: *mut u64,
        ) -> i32;
    }
    let existing = path
        .ancestors()
        .find(|p| !p.as_os_str().is_empty() && p.exists())
        .map(Path::to_path_buf)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| std::path::PathBuf::from(r"C:\"));
    let wide: Vec<u16> = existing
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let (mut free, mut total, mut total_free) = (0, 0, 0);
    // SAFETY: wide is a live NUL-terminated UTF-16 path; all output pointers
    // reference live stack u64 values. No handle or pointer escapes the call.
    #[allow(unsafe_code)]
    let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut free, &mut total, &mut total_free) };
    (ok != 0).then_some((free, total))
}

#[cfg(not(any(unix, windows)))]
pub fn available(_path: &Path) -> Option<(u64, u64)> {
    None
}
