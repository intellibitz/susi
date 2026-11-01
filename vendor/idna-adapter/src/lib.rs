// SPDX-License-Identifier: Apache-2.0 OR MIT
// Interface compatibility: The rust-url developers; implementation: Susi contributors.
//! IDNA adapter backed by the Windows operating system's Unicode service.
//! No Unicode data files or foreign implementation are distributed by this crate.
//! The Unicode version follows the installed OS, available through `unicode_version`.

mod platform;
use platform::*;
pub use platform::{
    platform_is_assigned, platform_status, platform_xid_continue, platform_xid_start,
    PlatformError, PlatformStatus,
};

/// Actual Unicode data version supplied by the host operating system.
pub fn unicode_version() -> [u8; 4] {
    let mut version = [0; 4];
    // SAFETY: API writes exactly four bytes into the supplied valid buffer.
    unsafe { u_getUnicodeVersion(version.as_mut_ptr()) };
    version
}

#[derive(Clone, Copy)]
pub struct BidiClass(i32);
#[derive(Clone, Copy)]
pub struct BidiClassMask(u32);
impl BidiClassMask {
    pub fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }
}
impl BidiClass {
    pub fn to_mask(self) -> BidiClassMask {
        BidiClassMask(1 << self.0)
    }
    pub fn is_ltr(self) -> bool {
        self.0 == 0
    }
    pub fn is_nonspacing_mark(self) -> bool {
        self.0 == 17
    }
    pub fn is_european_number(self) -> bool {
        self.0 == 2
    }
    pub fn is_arabic_number(self) -> bool {
        self.0 == 5
    }
}
pub const RTL_MASK: BidiClassMask = BidiClassMask((1 << 1) | (1 << 13) | (1 << 5));
pub const FIRST_BC_MASK: BidiClassMask = BidiClassMask((1 << 0) | (1 << 1) | (1 << 13));
pub const LAST_LTR_MASK: BidiClassMask = BidiClassMask((1 << 0) | (1 << 2));
pub const LAST_RTL_MASK: BidiClassMask = BidiClassMask((1 << 1) | (1 << 13) | (1 << 2) | (1 << 5));
pub const MIDDLE_LTR_MASK: BidiClassMask = BidiClassMask(
    (1 << 0) | (1 << 2) | (1 << 3) | (1 << 6) | (1 << 4) | (1 << 10) | (1 << 18) | (1 << 17),
);
pub const MIDDLE_RTL_MASK: BidiClassMask = BidiClassMask(
    (1 << 1)
        | (1 << 13)
        | (1 << 5)
        | (1 << 2)
        | (1 << 3)
        | (1 << 6)
        | (1 << 4)
        | (1 << 10)
        | (1 << 18)
        | (1 << 17),
);

#[derive(Clone, Copy)]
pub struct JoiningType(i32);
#[derive(Clone, Copy)]
pub struct JoiningTypeMask(u32);
impl JoiningTypeMask {
    pub fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }
}
impl JoiningType {
    pub fn to_mask(self) -> JoiningTypeMask {
        JoiningTypeMask(1 << self.0)
    }
    pub fn is_transparent(self) -> bool {
        self.0 == 5
    }
}
pub const LEFT_OR_DUAL_JOINING_MASK: JoiningTypeMask = JoiningTypeMask((1 << 3) | (1 << 2));
pub const RIGHT_OR_DUAL_JOINING_MASK: JoiningTypeMask = JoiningTypeMask((1 << 4) | (1 << 2));

#[non_exhaustive]
pub struct Adapter {}
#[cfg(feature = "compiled_data")]
impl Default for Adapter {
    fn default() -> Self {
        Self::new()
    }
}
impl Adapter {
    #[cfg(feature = "compiled_data")]
    pub const fn new() -> Self {
        Self {}
    }
    pub fn is_virama(&self, c: char) -> bool {
        // SAFETY: All Unicode scalars are valid UChar32 values.
        unsafe { u_getCombiningClass(c as i32) == 9 }
    }
    pub fn is_mark(&self, c: char) -> bool {
        // ICU UCharCategory: Mn=6, Me=7, Mc=8.
        unsafe { matches!(u_charType(c as i32), 6..=8) }
    }
    pub fn bidi_class(&self, c: char) -> BidiClass {
        unsafe { BidiClass(u_charDirection(c as i32)) }
    }
    pub fn joining_type(&self, c: char) -> JoiningType {
        // UCHAR_JOINING_TYPE is stable public ABI value 0x1007.
        unsafe { JoiningType(u_getIntPropertyValue(c as i32, 0x1007)) }
    }
    pub fn map_normalize<'a, I: Iterator<Item = char> + 'a>(
        &'a self,
        iter: I,
    ) -> impl Iterator<Item = char> + 'a {
        map(iter, false).chars().collect::<Vec<_>>().into_iter()
    }
    pub fn normalize_validate<'a, I: Iterator<Item = char> + 'a>(
        &'a self,
        iter: I,
    ) -> impl Iterator<Item = char> + 'a {
        map(iter, true).chars().collect::<Vec<_>>().into_iter()
    }
}

fn property(c: char, number: i32) -> bool {
    // SAFETY: Scalar and property numbers are public ICU ABI values.
    unsafe { u_hasBinaryProperty(c as i32, number) != 0 }
}

fn deviation(c: char) -> bool {
    matches!(c, '\u{df}' | '\u{3c2}' | '\u{200c}' | '\u{200d}')
}

fn excluded(c: char) -> bool {
    matches!(c, '\u{fffc}' | '\u{fffd}' | '\u{e0001}'..='\u{e007f}')
        || unsafe { u_charType(c as i32) == 0 }
}

fn base_valid(c: char) -> bool {
    if c.is_ascii() {
        return c != '.';
    }
    // ICU categories: Cn=0, separators=12..14, controls/private/surrogates=15..18.
    if matches!(unsafe { u_charType(c as i32) }, 0 | 12..=18) {
        return false;
    }
    // UTS46 excludes ideographic-description operators. The unary operator
    // property is not exposed by this Windows SDK; its two defined operators
    // are protocol exclusions, not a generated Unicode property table.
    if property(c, 18) || property(c, 19) || matches!(c, '\u{2ffe}' | '\u{2fff}') {
        return false;
    }
    c == 'ß' || !property(c, 56)
}

fn base_mapping(c: char) -> String {
    match c {
        '.' | '\u{ff0e}' | '\u{3002}' | '\u{ff61}' => ".".into(),
        'ẞ' => "ß".into(),
        _ if property(c, 2) => c.to_string(),
        _ => normalize(&c.to_string(), c"nfkc_cf"),
    }
}

fn map(iter: impl Iterator<Item = char>, reject_ignored: bool) -> String {
    let mut mapped = String::new();
    for c in iter {
        if c.is_ascii() {
            mapped.push(c.to_ascii_lowercase());
            continue;
        }
        if deviation(c) {
            mapped.push(c);
            continue;
        }
        if excluded(c) {
            mapped.push('\u{fffd}');
            continue;
        }
        let value = base_mapping(c);
        let separator = matches!(c, '\u{ff0e}' | '\u{3002}' | '\u{ff61}');
        let valid = value.chars().all(|v| {
            (separator || base_valid(v))
                && !excluded(v)
                && (deviation(v) || base_mapping(v) == v.to_string())
        });
        if !valid || (value.is_empty() && reject_ignored) {
            mapped.push('\u{fffd}');
        } else {
            mapped.push_str(&value);
        }
    }
    normalize(&mapped, c"nfc")
}

fn normalize(source: &str, name: &std::ffi::CStr) -> String {
    if source.is_empty() {
        return String::new();
    }
    let input: Vec<u16> = source.encode_utf16().collect();
    let Ok(length) = i32::try_from(input.len()) else {
        return "\u{fffd}".into();
    };
    let mut error = 0;
    // SAFETY: null package selects immutable OS data, NUL-terminated data name;
    // returned singleton is OS-owned and must not be freed.
    let normalizer = unsafe { unorm2_getInstance(std::ptr::null(), name.as_ptr(), 0, &mut error) };
    if error > 0 || normalizer.is_null() {
        return "\u{fffd}".into();
    }
    // Preflight computes exact required UTF-16 capacity. BUFFER_OVERFLOW(15) is
    // expected for a nonempty result; all other failures reject the input.
    let required = unsafe {
        unorm2_normalize(
            normalizer,
            input.as_ptr(),
            length,
            std::ptr::null_mut(),
            0,
            &mut error,
        )
    };
    if required < 0 || (error > 0 && error != 15) {
        return "\u{fffd}".into();
    }
    if required == 0 {
        return String::new();
    }
    let mut output = vec![0u16; required as usize];
    error = 0;
    let actual = unsafe {
        unorm2_normalize(
            normalizer,
            input.as_ptr(),
            length,
            output.as_mut_ptr(),
            required,
            &mut error,
        )
    };
    if error > 0 || actual != required {
        return "\u{fffd}".into();
    }
    String::from_utf16(&output).unwrap_or_else(|_| "\u{fffd}".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn platform_mapping_and_validation() {
        let a = Adapter {};
        eprintln!("Windows ICU Unicode {:?}", unicode_version());
        for (source, expected) in [
            ("EXAMPLE", "example"),
            ("ｅxample", "example"),
            ("e\u{301}", "é"),
            ("faß", "faß"),
            ("x\u{ad}y", "xy"),
            ("x\u{200d}y", "x\u{200d}y"),
            ("\u{e000}", "\u{fffd}"),
        ] {
            assert_eq!(
                a.map_normalize(source.chars()).collect::<String>(),
                expected,
                "{source:?}"
            );
        }
        assert_eq!(
            a.normalize_validate("x\u{ad}y".chars()).collect::<String>(),
            "x\u{fffd}y"
        );
        assert!(a.is_mark('\u{301}'));
        assert!(a.is_virama('\u{94d}'));
        assert!(a.bidi_class('א').to_mask().intersects(RTL_MASK));
        assert!(a
            .joining_type('ب')
            .to_mask()
            .intersects(LEFT_OR_DUAL_JOINING_MASK));
    }

    #[test]
    fn modern_mapping_does_not_inherit_os_legacy_idna_exclusions() {
        let a = Adapter {};
        for (input, expected) in [
            ("Ӏ", "ӏ"),
            ("Ⴀ", "ⴀ"),
            ("ẞ", "ß"),
            ("a。b．c｡d", "a.b.c.d"),
            ("≠", "≠"),
        ] {
            assert_eq!(a.map_normalize(input.chars()).collect::<String>(), expected);
        }
    }

    #[test]
    fn protocol_invalid_inputs_remain_invalid() {
        let a = Adapter {};
        for input in [
            "\u{e000}",
            "\u{ffff}",
            "\u{fffc}",
            "\u{e0001}",
            "\u{202e}",
            "\u{2067}",
            "\u{2488}",
            "\u{fe12}",
            "\u{2ffe}",
        ] {
            assert_eq!(
                a.map_normalize(input.chars()).collect::<String>(),
                "\u{fffd}",
                "{input:?}"
            );
        }
    }

    #[test]
    fn unsupported_repertoire_fails_closed() {
        let a = Adapter {};
        // This scalar was introduced after the host used for qualification.
        // Future OS updates may supply it, so assert based on the public version.
        if unicode_version()[0] < 16 {
            assert_eq!(
                a.map_normalize("\u{1c89}".chars()).collect::<String>(),
                "\u{fffd}"
            );
        }
    }

    #[test]
    fn context_properties_use_correct_abi_masks() {
        let a = Adapter {};
        assert!(a.bidi_class('a').is_ltr());
        assert!(a.bidi_class('7').is_european_number());
        assert!(a.bidi_class('٧').is_arabic_number());
        assert!(a.bidi_class('\u{301}').is_nonspacing_mark());
        assert!(a.joining_type('\u{64e}').is_transparent());
        assert!(a
            .joining_type('ب')
            .to_mask()
            .intersects(RIGHT_OR_DUAL_JOINING_MASK));
        assert!(!a
            .joining_type('a')
            .to_mask()
            .intersects(RIGHT_OR_DUAL_JOINING_MASK));
    }
}
