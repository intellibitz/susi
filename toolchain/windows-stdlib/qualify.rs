//! Compile with the unmodified pinned compiler, comparing original backend
//! against stock Unicode17 over every scalar already assigned by Windows ICU.
#![feature(unicode_internals)]
#![expect(
    internal_features,
    reason = "audit-only comparison with the pinned compiler's unchanged implementation"
)]
pub use std::{char, ptr, sync};
#[path = "platform_unicode.rs"]
mod platform;
#[link(name = "icu", kind = "raw-dylib")]
unsafe extern "C" {
    fn u_charType(c: i32) -> i8;
}
fn main() {
    let mut examined = 0u32;
    let mut mismatches = 0u32;
    for value in 0..=0x10ffff {
        let Some(c) = char::from_u32(value) else {
            continue;
        };
        // SAFETY: ICU accepts all Unicode scalar values.
        if unsafe { u_charType(c as i32) } == 0 {
            continue;
        }
        examined += 1;
        use core::unicode::unicode_data as stock;
        let expected = [
            c.is_alphabetic(),
            c.is_lowercase(),
            c.is_uppercase(),
            c.is_numeric(),
            c.is_whitespace(),
            stock::case_ignorable::lookup(c),
            stock::cf::lookup(c),
            stock::cn_planes_0_3::lookup(c),
            stock::default_ignorable_code_point::lookup(c),
            stock::grapheme_extend::lookup(c),
            stock::lt::lookup(c),
        ];
        let actual = [
            platform::alphabetic::lookup(c),
            platform::lowercase::lookup(c),
            platform::uppercase::lookup(c),
            platform::n::lookup(c),
            platform::white_space::lookup(c),
            platform::case_ignorable::lookup(c),
            platform::cf::lookup(c),
            platform::cn_planes_0_3::lookup(c),
            platform::default_ignorable_code_point::lookup(c),
            platform::grapheme_extend::lookup(c),
            platform::lt::lookup(c),
        ];
        let mut lower = ['\0'; 3];
        let mut upper = ['\0'; 3];
        for (i, v) in c.to_lowercase().enumerate() {
            lower[i] = v;
        }
        for (i, v) in c.to_uppercase().enumerate() {
            upper[i] = v;
        }
        let actual_lower = platform::conversions::to_lower(c);
        let actual_upper = platform::conversions::to_upper(c);
        let title = stock::conversions::to_title(c);
        let fold = stock::conversions::to_casefold(c);
        let actual_title = platform::conversions::to_title(c);
        let actual_fold = platform::conversions::to_casefold(c);
        if expected != actual
            || lower != actual_lower
            || upper != actual_upper
            || title != actual_title
            || fold != actual_fold
        {
            mismatches += 1;
            println!("U+{value:04X}: stock={expected:?}/{lower:?}/{upper:?}/{title:?}/{fold:?}, OS={actual:?}/{actual_lower:?}/{actual_upper:?}/{actual_title:?}/{actual_fold:?}");
        }
    }
    assert_eq!(platform::conversions::to_upper('ß'), ['S', 'S', '\0']);
    assert_eq!(platform::conversions::to_lower('İ'), ['i', '\u{307}', '\0']);
    assert_eq!(platform::conversions::to_lower('I'), ['i', '\0', '\0']);
    assert_eq!(platform::conversions::to_upper('ﬃ'), ['F', 'F', 'I']);
    assert_eq!(platform::conversions::to_lower('𐐀'), ['𐐨', '\0', '\0']);
    assert!(!platform::n::lookup('一'));
    println!("Examined {examined} assigned scalars; {mismatches} version-dependent differences");
    // These are aggregate qualification measurements, not character tables.
    // A changed OS/compiler repertoire must receive a fresh differential review.
    assert_eq!(examined, 287346);
    assert_eq!(mismatches, 71);
}
