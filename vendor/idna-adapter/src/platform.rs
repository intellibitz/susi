// SPDX-License-Identifier: Apache-2.0 OR MIT
//! Dynamic OS ABI access. No library or Unicode data is installed or bundled.
#![allow(non_snake_case)]
use crate::native_library::Library;
use std::ffi::{c_char, c_void};
use std::sync::OnceLock;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlatformError {
    SystemLibraryUnavailable,
    RequiredApiUnavailable,
    NormalizationDataUnavailable,
}
impl std::fmt::Display for PlatformError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::SystemLibraryUnavailable => {
                "operating-system ICU library is unavailable; Unicode domain names are rejected"
            }
            Self::RequiredApiUnavailable => {
                "operating-system ICU lacks required APIs; Unicode domain names are rejected"
            }
            Self::NormalizationDataUnavailable => {
                "operating-system ICU lacks normalization data; Unicode domain names are rejected"
            }
        })
    }
}
impl std::error::Error for PlatformError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlatformStatus {
    pub unicode_version: [u8; 4],
    pub library: String,
}

type Version = unsafe extern "C" fn(*mut u8);
type Property = unsafe extern "C" fn(i32) -> i32;
type Category = unsafe extern "C" fn(i32) -> i8;
type Combining = unsafe extern "C" fn(i32) -> u8;
type Binary = unsafe extern "C" fn(i32, i32) -> i8;
type Integer = unsafe extern "C" fn(i32, i32) -> i32;
type Instance = unsafe extern "C" fn(*const c_char, *const c_char, i32, *mut i32) -> *const c_void;
type Normalize =
    unsafe extern "C" fn(*const c_void, *const u16, i32, *mut u16, i32, *mut i32) -> i32;

struct Api {
    // Owned for the full lifetime of every stored function pointer.
    _library: Library,
    status: PlatformStatus,
    version: Version,
    direction: Property,
    category: Category,
    combining: Combining,
    binary: Binary,
    integer: Integer,
    instance: Instance,
    normalize: Normalize,
}

fn api() -> Result<&'static Api, &'static PlatformError> {
    static API: OnceLock<Result<Api, PlatformError>> = OnceLock::new();
    API.get_or_init(load).as_ref()
}

pub fn platform_status() -> Result<PlatformStatus, PlatformError> {
    api().map(|a| a.status.clone()).map_err(Clone::clone)
}

/// OS XID_Start property for source-code tooling (not an IDNA classification).
pub fn platform_xid_start(c: char) -> Result<bool, PlatformError> {
    api()
        .map(|a| unsafe { (a.binary)(c as i32, 33) != 0 })
        .map_err(Clone::clone)
}

/// OS XID_Continue property for source-code tooling (not an IDNA classification).
pub fn platform_xid_continue(c: char) -> Result<bool, PlatformError> {
    api()
        .map(|a| unsafe { (a.binary)(c as i32, 32) != 0 })
        .map_err(Clone::clone)
}

/// Whether the OS Unicode database assigns this scalar a general category.
/// Callers must not confuse this with acceptance by any particular protocol.
pub fn platform_is_assigned(c: char) -> Result<bool, PlatformError> {
    api()
        .map(|a| unsafe { (a.category)(c as i32) != 0 })
        .map_err(Clone::clone)
}

fn candidates() -> Vec<String> {
    #[cfg(windows)]
    {
        vec!["icu.dll".into()]
    }
    #[cfg(target_os = "macos")]
    {
        vec!["/usr/lib/libicucore.A.dylib".into()]
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // Search only standard OS-managed absolute directories. Neither the
        // working directory nor application configuration chooses executable code.
        let mut paths = Vec::new();
        for dir in [
            "/usr/lib/x86_64-linux-gnu",
            "/lib/x86_64-linux-gnu",
            "/usr/lib/aarch64-linux-gnu",
            "/lib/aarch64-linux-gnu",
            "/usr/lib64",
            "/lib64",
            "/usr/lib",
            "/lib",
        ] {
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    let name = name.to_string_lossy();
                    if let Some(version) = name.strip_prefix("libicuuc.so.") {
                        if let Ok(major) = version.parse::<u16>() {
                            paths.push((major, entry.path().to_string_lossy().into_owned()));
                        }
                    }
                }
            }
        }
        paths.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        let mut result: Vec<_> = paths.into_iter().map(|(_, path)| path).collect();
        result.extend(["/usr/lib/libicuuc.so".into(), "/lib/libicuuc.so".into()]);
        result
    }
    #[cfg(not(any(windows, unix)))]
    {
        Vec::new()
    }
}

fn load() -> Result<Api, PlatformError> {
    load_candidates(candidates())
}

fn load_candidates(paths: Vec<String>) -> Result<Api, PlatformError> {
    let mut failure = PlatformError::SystemLibraryUnavailable;
    for path in paths {
        let library = Library::new(&path);
        if let Ok(library) = library {
            match bind(library, path) {
                Ok(api) => return Ok(api),
                Err(error) => failure = error,
            }
        }
    }
    Err(failure)
}

fn bind(library: Library, path: String) -> Result<Api, PlatformError> {
    // ICU normally versions C symbols (e.g. u_charType_76). Windows/macOS
    // system libraries expose the stable unversioned ABI. Derive Linux's
    // suffix from its SONAME; unversioned dev links use bounded ABI discovery.
    let major = path
        .rsplit(".so.")
        .next()
        .and_then(|s| s.parse::<u16>().ok());
    let mut suffixes = vec![String::new()];
    if let Some(major) = major {
        suffixes.push(format!("_{major}"));
    } else if !cfg!(windows) {
        suffixes.extend((40..=200).rev().map(|v| format!("_{v}")));
    }
    for suffix in suffixes {
        // SAFETY: Symbol names and signatures are stable published ICU C ABI.
        // All symbols must resolve from the same library and version suffix.
        let loaded = unsafe {
            (|| {
                Ok::<_, std::io::Error>((
                    *library.get::<Version>(format!("u_getUnicodeVersion{suffix}\0").as_bytes())?,
                    *library.get::<Property>(format!("u_charDirection{suffix}\0").as_bytes())?,
                    *library.get::<Category>(format!("u_charType{suffix}\0").as_bytes())?,
                    *library
                        .get::<Combining>(format!("u_getCombiningClass{suffix}\0").as_bytes())?,
                    *library.get::<Binary>(format!("u_hasBinaryProperty{suffix}\0").as_bytes())?,
                    *library
                        .get::<Integer>(format!("u_getIntPropertyValue{suffix}\0").as_bytes())?,
                    *library.get::<Instance>(format!("unorm2_getInstance{suffix}\0").as_bytes())?,
                    *library.get::<Normalize>(format!("unorm2_normalize{suffix}\0").as_bytes())?,
                ))
            })()
        };
        if let Ok((version, direction, category, combining, binary, integer, instance, normalize)) =
            loaded
        {
            let mut unicode_version = [0; 4];
            unsafe { version(unicode_version.as_mut_ptr()) };
            for name in [c"nfc", c"nfkc_cf"] {
                let mut error = 0;
                let ptr = unsafe { instance(std::ptr::null(), name.as_ptr(), 0, &mut error) };
                if ptr.is_null() || error > 0 {
                    return Err(PlatformError::NormalizationDataUnavailable);
                }
            }
            return Ok(Api {
                _library: library,
                status: PlatformStatus {
                    unicode_version,
                    library: path,
                },
                version,
                direction,
                category,
                combining,
                binary,
                integer,
                instance,
                normalize,
            });
        }
    }
    Err(PlatformError::RequiredApiUnavailable)
}

// Fallback values reject non-ASCII input. The public status API reports why;
// the upstream infallible adapter API cannot carry a richer error itself.
pub(super) unsafe fn u_getUnicodeVersion(out: *mut u8) {
    if let Ok(a) = api() {
        (a.version)(out);
    }
}
pub(super) unsafe fn u_charDirection(c: i32) -> i32 {
    api().map(|a| (a.direction)(c)).unwrap_or(18)
}
pub(super) unsafe fn u_charType(c: i32) -> i8 {
    api().map(|a| (a.category)(c)).unwrap_or(0)
}
pub(super) unsafe fn u_getCombiningClass(c: i32) -> u8 {
    api().map(|a| (a.combining)(c)).unwrap_or(0)
}
pub(super) unsafe fn u_hasBinaryProperty(c: i32, p: i32) -> i8 {
    api().map(|a| (a.binary)(c, p)).unwrap_or(0)
}
pub(super) unsafe fn u_getIntPropertyValue(c: i32, p: i32) -> i32 {
    api().map(|a| (a.integer)(c, p)).unwrap_or(0)
}
pub(super) unsafe fn unorm2_getInstance(
    package: *const c_char,
    name: *const c_char,
    mode: i32,
    error: *mut i32,
) -> *const c_void {
    api()
        .map(|a| (a.instance)(package, name, mode, error))
        .unwrap_or(std::ptr::null())
}
#[allow(clippy::too_many_arguments)] // Exact published operating-system ABI.
pub(super) unsafe fn unorm2_normalize(
    n: *const c_void,
    s: *const u16,
    l: i32,
    d: *mut u16,
    cap: i32,
    e: *mut i32,
) -> i32 {
    api()
        .map(|a| (a.normalize)(n, s, l, d, cap, e))
        .unwrap_or(-1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_platform_returns_a_typed_error() {
        assert!(matches!(
            load_candidates(Vec::new()),
            Err(PlatformError::SystemLibraryUnavailable)
        ));
    }

    #[test]
    #[cfg(windows)]
    fn non_icu_system_library_is_rejected() {
        let library = Library::new("kernel32.dll").unwrap();
        assert!(matches!(
            bind(library, "kernel32.dll".into()),
            Err(PlatformError::RequiredApiUnavailable)
        ));
    }

    #[test]
    fn source_identifier_properties_are_separate_from_idna() {
        assert!(platform_xid_start('λ').unwrap());
        assert!(!platform_xid_start('9').unwrap());
        assert!(platform_xid_continue('9').unwrap());
        assert!(platform_is_assigned('λ').unwrap());
        assert!(!platform_is_assigned('\u{378}').unwrap());
    }
}
