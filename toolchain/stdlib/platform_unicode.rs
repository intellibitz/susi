//! Original Susi native Unicode backend. No character data is distributed.
//! Product-specific std: runtime OS ICU repertoire, no Unicode const API.
#![allow(missing_docs)]
#![deny(unsafe_code)]

// Builder pins the OS repertoire and qualification proves this exact version.
#[cfg(not(target_os = "linux"))]
pub const UNICODE_VERSION: (u8, u8, u8) = (15, 1, 0);
// Linux hosts select one qualified OS ICU at first use (see `linux_icu`).
// The constant reports the oldest qualified repertoire: every admitted host
// provides at least it, and a newer host is still held to its own exact pin.
#[cfg(target_os = "linux")]
pub const UNICODE_VERSION: (u8, u8, u8) = linux_icu::QUALIFIED[linux_icu::QUALIFIED.len() - 1].1;

#[cfg(not(target_os = "linux"))]
#[cfg_attr(windows, link(name = "icu", kind = "raw-dylib"))]
// SAFETY: signatures match the Windows SDK ICU C ABI; only OS DLL imports.
#[allow(unsafe_code)]
unsafe extern "C" {
    fn u_getUnicodeVersion(version: *mut u8);
    fn u_hasBinaryProperty(c: i32, property: i32) -> i8;
    fn u_charType(c: i32) -> i8;
    fn u_strToLower(
        dst: *mut u16,
        capacity: i32,
        src: *const u16,
        len: i32,
        locale: *const u8,
        error: *mut i32,
    ) -> i32;
    fn u_strToUpper(
        dst: *mut u16,
        capacity: i32,
        src: *const u16,
        len: i32,
        locale: *const u8,
        error: *mut i32,
    ) -> i32;
    fn u_strToTitle(
        dst: *mut u16,
        capacity: i32,
        src: *const u16,
        len: i32,
        breaker: *mut (),
        locale: *const u8,
        error: *mut i32,
    ) -> i32;
    fn u_strFoldCase(
        dst: *mut u16,
        capacity: i32,
        src: *const u16,
        len: i32,
        options: u32,
        error: *mut i32,
    ) -> i32;
}

/// Linux distributions ship different ICU majors, each exporting its C API with
/// a `_<major>` symbol suffix. Rather than link one major (which stops the
/// executable from starting anywhere else), load the newest qualified major
/// present in a standard absolute OS library directory, the same directory
/// policy as the vendored IDNA and regex Unicode adapters. Nothing is read from
/// PATH, LD_LIBRARY_PATH, the working directory or configuration.
#[cfg(target_os = "linux")]
mod linux_icu {
    use crate::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

    /// Qualified OS ICU majors, newest first, with the exact Unicode version
    /// each must report: ICU 78 (Unicode 17), ICU 76 (Debian 13),
    /// ICU 74 (Ubuntu 24.04 LTS), and
    /// ICU 72 (Debian 12 Bookworm).
    /// Adding a major requires differential and private-stdlib consumer qualification.
    pub const QUALIFIED: [(u8, (u8, u8, u8)); 4] = [
        (78, (17, 0, 0)),
        (76, (16, 0, 0)),
        (74, (15, 1, 0)),
        (72, (15, 0, 0)),
    ];
    const DIRECTORIES: [&[u8]; 8] = [
        b"/usr/lib/x86_64-linux-gnu",
        b"/lib/x86_64-linux-gnu",
        b"/usr/lib/aarch64-linux-gnu",
        b"/lib/aarch64-linux-gnu",
        b"/usr/lib64",
        b"/lib64",
        b"/usr/lib",
        b"/lib",
    ];
    const SYMBOLS: [&[u8]; 7] = [
        b"u_getUnicodeVersion",
        b"u_hasBinaryProperty",
        b"u_charType",
        b"u_strToLower",
        b"u_strToUpper",
        b"u_strToTitle",
        b"u_strFoldCase",
    ];
    const RTLD_NOW: i32 = 2;
    const RTLD_LOCAL: i32 = 0;

    // glibc exports these from libc.so.6 (2.34+) and libdl.so.2; std links both.
    // SAFETY: signatures match the POSIX dlfcn C ABI; pointers are opaque bytes.
    #[allow(unsafe_code)]
    unsafe extern "C" {
        fn dlopen(path: *const u8, flags: i32) -> *mut u8;
        fn dlsym(handle: *mut u8, name: *const u8) -> *mut u8;
        fn dlclose(handle: *mut u8) -> i32;
    }

    static ADDRESSES: [AtomicPtr<u8>; 7] = [const { AtomicPtr::new(crate::ptr::null_mut()) }; 7];
    /// Index into QUALIFIED plus one; zero until a complete library is published.
    static SELECTED: AtomicUsize = AtomicUsize::new(0);

    /// Writes `prefix ++ middle ++ decimal(major) ++ NUL` into `buffer`.
    fn c_name<'a>(buffer: &'a mut [u8; 64], prefix: &[u8], middle: &[u8], major: u8) -> &'a [u8] {
        assert!(
            (10..100).contains(&major),
            "ICU major outside the two-digit range"
        );
        let digits: &[u8] = &[b'0' + major / 10, b'0' + major % 10];
        let length = prefix.len() + middle.len() + digits.len();
        assert!(
            length < buffer.len(),
            "ICU library or symbol name exceeds its bound"
        );
        let mut at = 0;
        for part in [prefix, middle, digits] {
            buffer[at..at + part.len()].copy_from_slice(part);
            at += part.len();
        }
        buffer[at] = 0;
        &buffer[..=at]
    }

    // SAFETY: dlopen/dlsym receive NUL-terminated names from bounded local
    // buffers; a handle is closed only when it is not published.
    #[allow(unsafe_code)]
    fn load() -> usize {
        let selected = SELECTED.load(Ordering::Acquire);
        if selected != 0 {
            return selected - 1;
        }
        for (index, (major, _)) in QUALIFIED.iter().enumerate() {
            for directory in DIRECTORIES {
                let mut path = [0u8; 64];
                let path = c_name(&mut path, directory, b"/libicuuc.so.", *major);
                // SAFETY: absolute, NUL-terminated OS path and published flags.
                let handle = unsafe { dlopen(path.as_ptr(), RTLD_NOW | RTLD_LOCAL) };
                if handle.is_null() {
                    continue;
                }
                let mut found = [crate::ptr::null_mut(); 7];
                for (slot, name) in found.iter_mut().zip(SYMBOLS) {
                    let mut symbol = [0u8; 64];
                    let symbol = c_name(&mut symbol, name, b"_", *major);
                    // SAFETY: live handle and NUL-terminated symbol name.
                    *slot = unsafe { dlsym(handle, symbol.as_ptr()) };
                }
                if found.iter().any(|address| address.is_null()) {
                    // SAFETY: this unpublished handle owns one loader reference.
                    unsafe { dlclose(handle) };
                    continue;
                }
                // The handle stays open for the life of the process. Racing
                // initialisers resolve the same library, so their stores agree.
                for (slot, address) in ADDRESSES.iter().zip(found) {
                    slot.store(address, Ordering::Relaxed);
                }
                SELECTED.store(index + 1, Ordering::Release);
                return index;
            }
        }
        panic!(
            "no qualified OS ICU (libicuuc.so.78, libicuuc.so.76, libicuuc.so.74, or libicuuc.so.72) in a system library directory"
        );
    }

    /// The exact Unicode version the selected ICU major must report.
    pub fn expected_version() -> (u8, u8, u8) {
        QUALIFIED[load()].1
    }

    fn address(index: usize) -> *mut u8 {
        load();
        ADDRESSES[index].load(Ordering::Relaxed)
    }

    // SAFETY (all wrappers): each address was resolved from the `_<major>`
    // symbol of the same name in a qualified ICU, whose C signature matches
    // the declared function pointer type; callers uphold the ICU contract.
    #[allow(unsafe_code)]
    pub unsafe fn u_getUnicodeVersion(version: *mut u8) {
        // SAFETY: see the wrapper contract above.
        let f: unsafe extern "C" fn(*mut u8) = unsafe { crate::mem::transmute(address(0)) };
        // SAFETY: the caller supplies a valid four-byte buffer.
        unsafe { f(version) }
    }
    #[allow(unsafe_code)]
    pub unsafe fn u_hasBinaryProperty(c: i32, property: i32) -> i8 {
        // SAFETY: see the wrapper contract above.
        let f: unsafe extern "C" fn(i32, i32) -> i8 = unsafe { crate::mem::transmute(address(1)) };
        // SAFETY: ICU accepts every code point and documented property enum.
        unsafe { f(c, property) }
    }
    #[allow(unsafe_code)]
    pub unsafe fn u_charType(c: i32) -> i8 {
        // SAFETY: see the wrapper contract above.
        let f: unsafe extern "C" fn(i32) -> i8 = unsafe { crate::mem::transmute(address(2)) };
        // SAFETY: ICU accepts every code point.
        unsafe { f(c) }
    }
    type Case = unsafe extern "C" fn(*mut u16, i32, *const u16, i32, *const u8, *mut i32) -> i32;
    #[allow(unsafe_code)]
    pub unsafe fn u_strToLower(
        dst: *mut u16,
        capacity: i32,
        src: *const u16,
        len: i32,
        locale: *const u8,
        error: *mut i32,
    ) -> i32 {
        // SAFETY: see the wrapper contract above.
        let f: Case = unsafe { crate::mem::transmute(address(3)) };
        // SAFETY: the caller supplies valid, disjoint, bounded buffers.
        unsafe { f(dst, capacity, src, len, locale, error) }
    }
    #[allow(unsafe_code)]
    pub unsafe fn u_strToUpper(
        dst: *mut u16,
        capacity: i32,
        src: *const u16,
        len: i32,
        locale: *const u8,
        error: *mut i32,
    ) -> i32 {
        // SAFETY: see the wrapper contract above.
        let f: Case = unsafe { crate::mem::transmute(address(4)) };
        // SAFETY: the caller supplies valid, disjoint, bounded buffers.
        unsafe { f(dst, capacity, src, len, locale, error) }
    }
    #[allow(unsafe_code)]
    pub unsafe fn u_strToTitle(
        dst: *mut u16,
        capacity: i32,
        src: *const u16,
        len: i32,
        breaker: *mut (),
        locale: *const u8,
        error: *mut i32,
    ) -> i32 {
        // SAFETY: see the wrapper contract above.
        let f: unsafe extern "C" fn(
            *mut u16,
            i32,
            *const u16,
            i32,
            *mut (),
            *const u8,
            *mut i32,
        ) -> i32 = unsafe { crate::mem::transmute(address(5)) };
        // SAFETY: the caller supplies valid, disjoint, bounded buffers.
        unsafe { f(dst, capacity, src, len, breaker, locale, error) }
    }
    #[allow(unsafe_code)]
    pub unsafe fn u_strFoldCase(
        dst: *mut u16,
        capacity: i32,
        src: *const u16,
        len: i32,
        options: u32,
        error: *mut i32,
    ) -> i32 {
        // SAFETY: see the wrapper contract above.
        let f: unsafe extern "C" fn(*mut u16, i32, *const u16, i32, u32, *mut i32) -> i32 =
            unsafe { crate::mem::transmute(address(6)) };
        // SAFETY: the caller supplies valid, disjoint, bounded buffers.
        unsafe { f(dst, capacity, src, len, options, error) }
    }
}
#[cfg(target_os = "linux")]
use linux_icu::{
    u_charType, u_getUnicodeVersion, u_hasBinaryProperty, u_strFoldCase, u_strToLower,
    u_strToTitle, u_strToUpper,
};

// SAFETY: the only foreign write is bounded to a four-byte local array.
#[allow(unsafe_code)]
fn check_version() {
    use crate::sync::atomic::{AtomicBool, Ordering};
    static CHECKED: AtomicBool = AtomicBool::new(false);
    if !CHECKED.load(Ordering::Acquire) {
        #[cfg(target_os = "linux")]
        let expected = linux_icu::expected_version();
        #[cfg(not(target_os = "linux"))]
        let expected = UNICODE_VERSION;
        let mut version = [0u8; 4];
        // SAFETY: ICU writes exactly four version bytes to this valid array.
        unsafe { u_getUnicodeVersion(version.as_mut_ptr()) };
        assert!(
            (version[0], version[1], version[2]) == expected,
            "OS Unicode repertoire changed: rebuild and requalify Susi stdlib"
        );
        CHECKED.store(true, Ordering::Release);
    }
}

// SAFETY: internal callers supply documented ICU binary-property constants.
#[allow(unsafe_code)]
fn property(c: char, p: i32) -> bool {
    check_version();
    // SAFETY: ICU accepts every Unicode scalar and documented property enum.
    unsafe { u_hasBinaryProperty(c as i32, p) != 0 }
}
// SAFETY: every char is a scalar representable by ICU's signed32 input.
#[allow(unsafe_code)]
fn category(c: char) -> i8 {
    check_version();
    // SAFETY: all Rust chars are valid Unicode scalar values.
    unsafe { u_charType(c as i32) }
}
macro_rules! property_module {
    ($name:ident, $value:literal) => {
        pub mod $name {
            pub fn lookup(c: char) -> bool {
                super::property(c, $value)
            }
        }
    };
}
property_module!(alphabetic, 0);
property_module!(case_ignorable, 50);
property_module!(default_ignorable_code_point, 5);
property_module!(grapheme_extend, 11);
property_module!(lowercase, 22);
property_module!(uppercase, 30);
property_module!(white_space, 31);
pub mod cf {
    pub fn lookup(c: char) -> bool {
        super::category(c) == 16
    }
}
pub mod cn_planes_0_3 {
    pub fn lookup(c: char) -> bool {
        (c as u32) < 0x40000 && super::category(c) == 0
    }
}
pub mod lt {
    pub fn lookup(c: char) -> bool {
        super::category(c) == 3
    }
}
pub mod n {
    pub fn lookup(c: char) -> bool {
        matches!(super::category(c), 9..=11)
    }
}

pub mod conversions {
    // SAFETY: each API receives separately owned, bounded UTF16 buffers below.
    #[allow(unsafe_code)]
    fn convert(c: char, operation: u8) -> [char; 3] {
        super::check_version();
        let mut source = [0u16; 2];
        let input = c.encode_utf16(&mut source);
        let mut output = [0u16; 6];
        let mut error = 0;
        // SAFETY: input/output pointers are valid for their respective lengths,
        // disjoint, and root locale is a NUL-terminated empty string. Null break
        // iterator requests ICU's own titlecase iterator; no pointer is retained.
        let written = unsafe {
            match operation {
                0 => super::u_strToLower(
                    output.as_mut_ptr(),
                    6,
                    input.as_ptr(),
                    input.len() as i32,
                    c"".as_ptr().cast(),
                    &mut error,
                ),
                1 => super::u_strToUpper(
                    output.as_mut_ptr(),
                    6,
                    input.as_ptr(),
                    input.len() as i32,
                    c"".as_ptr().cast(),
                    &mut error,
                ),
                2 => super::u_strToTitle(
                    output.as_mut_ptr(),
                    6,
                    input.as_ptr(),
                    input.len() as i32,
                    crate::ptr::null_mut(),
                    c"".as_ptr().cast(),
                    &mut error,
                ),
                _ => super::u_strFoldCase(
                    output.as_mut_ptr(),
                    6,
                    input.as_ptr(),
                    input.len() as i32,
                    0,
                    &mut error,
                ),
            }
        };
        assert!(
            error <= 0 && (0..=6).contains(&written),
            "OS Unicode conversion failed"
        );
        let mut result = ['\0'; 3];
        for (index, scalar) in
            char::decode_utf16(output[..written as usize].iter().copied()).enumerate()
        {
            assert!(
                index < 3,
                "OS Unicode mapping exceeded Rust scalar capacity"
            );
            result[index] = scalar.expect("OS Unicode returned invalid UTF-16");
        }
        result
    }
    pub fn to_lower(c: char) -> [char; 3] {
        convert(c, 0)
    }
    pub fn to_upper(c: char) -> [char; 3] {
        convert(c, 1)
    }
    pub fn to_title(c: char) -> [char; 3] {
        // ICU word-title starts at letters, numbers, symbols or private-use
        // scalars. A lone combining mark is skipped (notably U+0345), whereas
        // scalar titlecase defaults to uppercase outside these categories.
        // Keep letter title mappings: Georgian Mkhedruli deliberately stays
        // lowercase, and titlecase letters and expansions differ from uppercase.
        // The pinned-repertoire scalar audit checks this distinction exhaustively.
        if matches!(super::category(c), 1..=5 | 9..=11 | 17 | 24..=27) {
            convert(c, 2)
        } else {
            convert(c, 1)
        }
    }
    pub fn to_casefold(c: char) -> [char; 3] {
        convert(c, 3)
    }
}
