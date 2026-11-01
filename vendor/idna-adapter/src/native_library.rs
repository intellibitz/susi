// SPDX-License-Identifier: Apache-2.0 OR MIT
//! Independently implemented, private OS-loader boundary for system ICU.
//! No third-party loader implementation, library binary or Unicode data is included.

use std::ffi::{c_void, CStr};
use std::io;
use std::marker::PhantomData;
use std::ops::Deref;
use std::ptr::NonNull;

pub(crate) struct Library(NonNull<c_void>);

// SAFETY: This owns one OS loader reference, never a borrowed module handle.
// The supported OS loaders permit resolution and reference release from any
// thread. Drop requires exclusive ownership; Symbol borrows prevent releasing
// this reference while a symbol wrapper exists. These impls do not assert that
// arbitrary resolved functions are thread safe: callers must uphold their ABI.
unsafe impl Send for Library {}
unsafe impl Sync for Library {}

pub(crate) struct Symbol<'library, T> {
    value: T,
    library: PhantomData<&'library Library>,
}

impl<T> Deref for Symbol<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.value
    }
}

impl Library {
    pub(crate) fn new(path: &str) -> io::Result<Self> {
        native::open(path).map(Self)
    }

    /// # Safety
    /// `T` must be the exact exported function-pointer type, including its
    /// calling convention and argument/return ABI. Any copy of the pointer
    /// must not be called after this Library is dropped. A size check alone
    /// cannot prove those requirements; the private ICU binder supplies them.
    pub(crate) unsafe fn get<T: Copy>(&self, name: &[u8]) -> io::Result<Symbol<'_, T>> {
        let name = CStr::from_bytes_with_nul(name).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "symbol must have one terminating NUL",
            )
        })?;
        if name.to_bytes().is_empty()
            || std::mem::size_of::<T>() != std::mem::size_of::<*mut c_void>()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid function symbol type or name",
            ));
        }
        let address = native::symbol(self.0, name)?;
        let pointer = address.as_ptr();
        // SAFETY: The non-null OS address has pointer size; the caller supplies
        // the matching function-pointer ABI and preserves library lifetime.
        let value = unsafe { std::mem::transmute_copy::<*mut c_void, T>(&pointer) };
        Ok(Symbol {
            value,
            library: PhantomData,
        })
    }
}

impl Drop for Library {
    fn drop(&mut self) {
        // SAFETY: Exactly one successful open created this owned reference.
        // No clones or raw handle escape exist; borrowing excludes live symbols.
        unsafe { native::close(self.0) };
    }
}

#[cfg(windows)]
mod native {
    use super::*;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LoadLibraryExW(name: *const u16, reserved: *mut c_void, flags: u32) -> *mut c_void;
        fn GetProcAddress(module: *mut c_void, name: *const std::ffi::c_char) -> *mut c_void;
        fn FreeLibrary(module: *mut c_void) -> i32;
    }

    pub(super) fn open(path: &str) -> io::Result<NonNull<c_void>> {
        // Only bare system DLL names are allowed. Flags restrict the actual
        // directory to System32; never consult PATH, CWD or an application path.
        if !path.ends_with(".dll")
            || path.starts_with('.')
            || path.contains("..")
            || !path
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "only a system DLL basename is allowed",
            ));
        }
        let wide: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
        // SAFETY: The name is a valid terminated UTF-16 buffer. No borrowed
        // handles are supplied; 0x800 is LOAD_LIBRARY_SEARCH_SYSTEM32.
        let handle = unsafe { LoadLibraryExW(wide.as_ptr(), std::ptr::null_mut(), 0x800) };
        NonNull::new(handle).ok_or_else(io::Error::last_os_error)
    }

    pub(super) fn symbol(handle: NonNull<c_void>, name: &CStr) -> io::Result<NonNull<c_void>> {
        // SAFETY: Library owns the live handle; CStr guarantees a terminated
        // non-ordinal name, and borrowing prevents concurrent handle release.
        NonNull::new(unsafe { GetProcAddress(handle.as_ptr(), name.as_ptr()) })
            .ok_or_else(io::Error::last_os_error)
    }

    pub(super) unsafe fn close(handle: NonNull<c_void>) {
        // SAFETY: The caller owns exactly one matching loader reference.
        let _ = unsafe { FreeLibrary(handle.as_ptr()) };
    }
}

#[cfg(unix)]
mod native {
    use super::*;
    use std::ffi::CString;
    use std::path::{Component, Path};
    #[cfg_attr(target_os = "linux", link(name = "dl"))]
    unsafe extern "C" {
        fn dlopen(path: *const std::ffi::c_char, flags: i32) -> *mut c_void;
        fn dlsym(handle: *mut c_void, name: *const std::ffi::c_char) -> *mut c_void;
        fn dlclose(handle: *mut c_void) -> i32;
    }

    pub(super) fn open(path: &str) -> io::Result<NonNull<c_void>> {
        let candidate = Path::new(path);
        let approved = [
            "/usr/lib",
            "/lib",
            "/usr/lib64",
            "/lib64",
            "/usr/lib/x86_64-linux-gnu",
            "/lib/x86_64-linux-gnu",
            "/usr/lib/aarch64-linux-gnu",
            "/lib/aarch64-linux-gnu",
        ];
        if !candidate.is_absolute()
            || candidate
                .components()
                .any(|c| !matches!(c, Component::RootDir | Component::Normal(_)))
            || !candidate
                .parent()
                .is_some_and(|p| approved.iter().any(|dir| p == Path::new(dir)))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "library must be in an absolute OS library directory",
            ));
        }
        let name = CString::new(path).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "library path contains NUL")
        })?;
        // RTLD_NOW performs relocation before returning. Darwin's RTLD_LOCAL
        // is 4; on Linux it is 0. Neither platform exports symbols globally.
        let flags = 2 | if cfg!(target_os = "macos") { 4 } else { 0 };
        // SAFETY: Valid terminated absolute OS path and published loader flags.
        NonNull::new(unsafe { dlopen(name.as_ptr(), flags) })
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "OS library unavailable"))
    }

    pub(super) fn symbol(handle: NonNull<c_void>, name: &CStr) -> io::Result<NonNull<c_void>> {
        // SAFETY: Live owned handle and terminated name. A null address cannot
        // represent a callable function, regardless of dlerror's thread state.
        NonNull::new(unsafe { dlsym(handle.as_ptr(), name.as_ptr()) }).ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "required OS symbol unavailable")
        })
    }

    pub(super) unsafe fn close(handle: NonNull<c_void>) {
        // SAFETY: The caller owns exactly one matching loader reference.
        let _ = unsafe { dlclose(handle.as_ptr()) };
    }
}

#[cfg(not(any(windows, unix)))]
mod native {
    use super::*;
    pub(super) fn open(_: &str) -> io::Result<NonNull<c_void>> {
        Err(io::ErrorKind::Unsupported.into())
    }
    pub(super) fn symbol(_: NonNull<c_void>, _: &CStr) -> io::Result<NonNull<c_void>> {
        Err(io::ErrorKind::Unsupported.into())
    }
    pub(super) unsafe fn close(_: NonNull<c_void>) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_relative_paths_and_application_paths() {
        for path in [
            "./icu.dll",
            "../icu.dll",
            "C:\\app\\icu.dll",
            "/tmp/libicuuc.so",
            "libicuuc.so",
            "/usr/lib/../tmp/libicuuc.so",
        ] {
            assert!(Library::new(path).is_err(), "{path}");
        }
    }

    #[test]
    #[cfg(windows)]
    fn windows_system_icu_symbol_lifetime_missing_symbols_and_concurrent_lookup() {
        let library = std::sync::Arc::new(Library::new("icu.dll").unwrap());
        type Version = unsafe extern "C" fn(*mut u8);
        assert!(unsafe { library.get::<Version>(b"susi_missing_symbol_0123456789\0") }.is_err());
        assert!(unsafe { library.get::<Version>(b"u_getUnicodeVersion") }.is_err());
        assert!(unsafe { library.get::<u8>(b"u_getUnicodeVersion\0") }.is_err());
        let workers: Vec<_> = (0..4)
            .map(|_| {
                let library = library.clone();
                std::thread::spawn(move || {
                    let function =
                        unsafe { library.get::<Version>(b"u_getUnicodeVersion\0") }.unwrap();
                    let mut version = [0u8; 4];
                    // SAFETY: The exact documented ICU signature writes four bytes;
                    // this thread retains the owning Arc while calling the symbol.
                    unsafe { function(version.as_mut_ptr()) };
                    assert!(version[0] >= 10);
                })
            })
            .collect();
        drop(library);
        for worker in workers {
            worker.join().unwrap();
        }
        // The final Arc has released its reference; a fresh load is valid.
        assert!(Library::new("icu.dll").is_ok());
    }
}
