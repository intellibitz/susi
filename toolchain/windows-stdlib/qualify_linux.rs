//! Audit-only differential review of the Linux OS ICU backend. Compile with the
//! unmodified pinned compiler (stock Unicode 17 core) on a host that selects
//! ICU 78. Every Unicode scalar is compared; differences are reported and make
//! the audit fail. No count of accepted differences exists or may be added.
#![feature(unicode_internals, case_ignorable, char_unassigned_private_use)]
#![expect(
    internal_features,
    reason = "audit-only comparison with the pinned compiler's unchanged implementation"
)]
#![deny(unsafe_code)]
pub use std::{mem, ptr, sync};
#[path = "platform_unicode.rs"]
#[allow(dead_code, non_snake_case)] // standalone audit of the original ICU ABI names
mod platform;

use core::unicode::unicode_data as stock;
use std::fmt::Write;
use std::panic::catch_unwind;

const AUDITED_MAJOR: &str = "78";
const PROPERTIES: [&str; 11] = [
    "alphabetic",
    "lowercase",
    "uppercase",
    "n",
    "white_space",
    "case_ignorable",
    "cf",
    "cn_planes_0_3",
    "default_ignorable_code_point",
    "grapheme_extend",
    "lt",
];
const CONVERSIONS: [&str; 4] = ["lower", "upper", "title", "fold"];
type Row = ([bool; 11], [[char; 3]; 4]);

// Use the stock caller contracts, not truncated internal lookup domains:
// char::is_case_ignorable handles ASCII itself, and char::is_assigned handles
// U+3FFFE/U+3FFFF outside Cn_planes_0_3's table domain. Public predicates also
// preserve the stock ASCII fast paths for the first five properties.
fn stock_row(c: char) -> Row {
    (
        [
            c.is_alphabetic(),
            c.is_lowercase(),
            c.is_uppercase(),
            c.is_numeric(),
            c.is_whitespace(),
            c.is_case_ignorable(),
            stock::cf::lookup(c),
            (c as u32) < 0x40000 && !c.is_assigned(),
            stock::default_ignorable_code_point::lookup(c),
            stock::grapheme_extend::lookup(c),
            stock::lt::lookup(c),
        ],
        [
            stock::conversions::to_lower(c),
            stock::conversions::to_upper(c),
            stock::conversions::to_title(c),
            stock::conversions::to_casefold(c),
        ],
    )
}
fn os_row(c: char) -> Row {
    (
        [
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
        ],
        [
            platform::conversions::to_lower(c),
            platform::conversions::to_upper(c),
            platform::conversions::to_title(c),
            platform::conversions::to_casefold(c),
        ],
    )
}
/// Padding is trailing NULs only.
fn well_formed(m: [char; 3]) -> bool {
    !(m[0] == '\0' && m[1] != '\0') && !(m[1] == '\0' && m[2] != '\0')
}

fn main() {
    assert_eq!(
        char::UNICODE_VERSION,
        (17, 0, 0),
        "audit requires the pinned stock Unicode 17 compiler"
    );
    // First use, with the default panic hook: platform loads the newest
    // qualified ICU and enforces that major's exact Unicode version pin.
    let _ = platform::alphabetic::lookup('A');
    let maps = std::fs::read_to_string("/proc/self/maps").expect("readable /proc/self/maps");
    let mut loaded: Vec<&str> = maps
        .lines()
        .filter_map(|line| line.split("/libicuuc.so.").nth(1))
        .filter_map(|tail| tail.split(|ch: char| !ch.is_ascii_digit()).next())
        .collect();
    loaded.sort_unstable();
    loaded.dedup();
    assert_eq!(
        loaded,
        [AUDITED_MAJOR],
        "host must select exactly ICU {AUDITED_MAJOR}; audit not applicable"
    );

    // Scalar titlecase must not inherit word-start skipping or indiscriminately
    // uppercase lowercase letters. Georgian Mkhedruli is the counterexample.
    assert_eq!(
        platform::conversions::to_title('\u{345}'),
        ['\u{399}', '\0', '\0']
    );
    for value in (0x10d0..=0x10fa).chain(0x10fd..=0x10ff) {
        let c = char::from_u32(value).unwrap();
        assert_eq!(platform::conversions::to_title(c), [c, '\0', '\0']);
    }
    assert_eq!(
        platform::conversions::to_title('\u{1c5}'),
        ['\u{1c5}', '\0', '\0']
    );
    assert_eq!(platform::conversions::to_title('\u{df}'), ['S', 's', '\0']);
    assert_eq!(platform::conversions::to_title('\u{fb03}'), ['F', 'f', 'i']);

    std::panic::set_hook(Box::new(|_| {}));
    let mut scalars = 0u32;
    let mut mismatching = 0u32;
    let mut panicking = 0u32;
    let mut per_property = [0u32; 11];
    let mut per_conversion = [0u32; 4];
    for value in 0..=0x10FFFFu32 {
        let Some(c) = char::from_u32(value) else {
            continue;
        };
        scalars += 1;
        let (stock, os) = match (catch_unwind(|| stock_row(c)), catch_unwind(|| os_row(c))) {
            (Ok(s), Ok(o)) => (s, o),
            (s, o) => {
                mismatching += 1;
                panicking += 1;
                println!(
                    "U+{value:04X}: panic stock={} OS={}",
                    s.is_err(),
                    o.is_err()
                );
                continue;
            }
        };
        let mut line = String::new();
        for i in 0..11 {
            if stock.0[i] != os.0[i] {
                per_property[i] += 1;
                write!(
                    line,
                    " {}: stock={} OS={};",
                    PROPERTIES[i], stock.0[i], os.0[i]
                )
                .unwrap();
            }
        }
        for i in 0..4 {
            let (s, o) = (stock.1[i], os.1[i]);
            if s != o || !well_formed(s) || !well_formed(o) {
                per_conversion[i] += 1;
                write!(line, " {}: stock={s:?} OS={o:?};", CONVERSIONS[i]).unwrap();
            }
        }
        if !line.is_empty() {
            mismatching += 1;
            println!("U+{value:04X}:{line}");
        }
    }
    let _ = std::panic::take_hook();

    assert_eq!(scalars, 0x110000 - 0x800, "scalar sweep incomplete");
    println!(
        "ICU {AUDITED_MAJOR} vs stock Unicode {:?}: {scalars} scalars compared; \
         {mismatching} mismatching ({panicking} panicking)",
        char::UNICODE_VERSION
    );
    for (name, count) in PROPERTIES.iter().zip(per_property) {
        println!("  property {name}: {count}");
    }
    for (name, count) in CONVERSIONS.iter().zip(per_conversion) {
        println!("  conversion {name}: {count}");
    }
    if mismatching != 0 {
        std::process::exit(1);
    }
}
