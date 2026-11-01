//! Original Susi Windows Unicode backend. No character data is distributed.
//! Product-specific std: runtime Windows ICU repertoire, no Unicode const API.
#![allow(missing_docs)]
#![deny(unsafe_code)]

// Builder pins the OS repertoire and qualification proves this exact version.
pub const UNICODE_VERSION: (u8, u8, u8) = (15, 1, 0);

#[link(name = "icu", kind = "raw-dylib")]
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

// SAFETY: the only foreign write is bounded to a four-byte local array.
#[allow(unsafe_code)]
fn check_version() {
    use crate::sync::atomic::{AtomicBool, Ordering};
    static CHECKED: AtomicBool = AtomicBool::new(false);
    if !CHECKED.load(Ordering::Acquire) {
        let mut version = [0u8; 4];
        // SAFETY: ICU writes exactly four version bytes to this valid array.
        unsafe { u_getUnicodeVersion(version.as_mut_ptr()) };
        assert!(
            (version[0], version[1], version[2]) == UNICODE_VERSION,
            "Windows Unicode repertoire changed: rebuild and requalify Susi stdlib"
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
            "Windows Unicode conversion failed"
        );
        let mut result = ['\0'; 3];
        for (index, scalar) in
            char::decode_utf16(output[..written as usize].iter().copied()).enumerate()
        {
            assert!(
                index < 3,
                "Windows Unicode mapping exceeded Rust scalar capacity"
            );
            result[index] = scalar.expect("Windows Unicode returned invalid UTF-16");
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
        convert(c, 2)
    }
    pub fn to_casefold(c: char) -> [char; 3] {
        convert(c, 3)
    }
}
