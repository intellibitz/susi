// SPDX-License-Identifier: Apache-2.0 OR MIT
//! Original OS ICU adapter. No Unicode data or foreign implementation is bundled.
mod native_library;
use native_library::Library;
use std::{collections::BTreeMap, ffi::c_void, sync::OnceLock};
type Open = unsafe extern "C" fn(i32, i32) -> *mut c_void;
type Close = unsafe extern "C" fn(*mut c_void);
type Apply = unsafe extern "C" fn(*mut c_void, *const u16, i32, *const u16, i32, *mut i32);
type Count = unsafe extern "C" fn(*const c_void) -> i32;
type Item =
    unsafe extern "C" fn(*const c_void, i32, *mut i32, *mut i32, *mut u16, i32, *mut i32) -> i32;
type Fold = unsafe extern "C" fn(i32, u32) -> i32;
type Age = unsafe extern "C" fn(i32, *mut u8);
type Version = unsafe extern "C" fn(*mut u8);
struct Api {
    _library: Library,
    open: Open,
    close: Close,
    apply: Apply,
    count: Count,
    item: Item,
    fold: Fold,
    version: Version,
    age: Age,
}
#[derive(Debug, Clone, Copy)]
pub struct Unavailable;
fn load() -> Result<Api, Unavailable> {
    let mut paths = Vec::new();
    if cfg!(windows) {
        paths.push("icu.dll".to_owned());
    }
    if cfg!(target_os = "macos") {
        paths.push("/usr/lib/libicucore.A.dylib".to_owned());
    }
    if cfg!(all(unix, not(target_os = "macos"))) {
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
                    if entry
                        .file_name()
                        .to_string_lossy()
                        .strip_prefix("libicuuc.so.")
                        .and_then(|s| s.parse::<u16>().ok())
                        .is_some()
                    {
                        paths.push(entry.path().to_string_lossy().into_owned());
                    }
                }
            }
        }
        paths.sort();
        paths.reverse();
    }
    for path in paths {
        let Ok(library) = Library::new(&path) else {
            continue;
        };
        let mut suffixes = vec![String::new()];
        if !cfg!(windows) {
            suffixes.extend((40..=200).rev().map(|n| format!("_{n}")));
        }
        for suffix in suffixes {
            // SAFETY: These are ICU's published C signatures. The owned library
            // remains alive in Api as long as any stored function is reachable.
            let result = unsafe {
                (|| {
                    Ok::<_, std::io::Error>((
                        *library.get::<Open>(format!("uset_open{suffix}\0").as_bytes())?,
                        *library.get::<Close>(format!("uset_close{suffix}\0").as_bytes())?,
                        *library.get::<Apply>(
                            format!("uset_applyPropertyAlias{suffix}\0").as_bytes(),
                        )?,
                        *library
                            .get::<Count>(format!("uset_getRangeCount{suffix}\0").as_bytes())?,
                        *library.get::<Item>(format!("uset_getItem{suffix}\0").as_bytes())?,
                        *library.get::<Fold>(format!("u_foldCase{suffix}\0").as_bytes())?,
                        *library
                            .get::<Version>(format!("u_getUnicodeVersion{suffix}\0").as_bytes())?,
                        *library.get::<Age>(format!("u_charAge{suffix}\0").as_bytes())?,
                    ))
                })()
            };
            if let Ok((open, close, apply, count, item, fold, version, age)) = result {
                return Ok(Api {
                    _library: library,
                    open,
                    close,
                    apply,
                    count,
                    item,
                    fold,
                    version,
                    age,
                });
            }
        }
    }
    Err(Unavailable)
}
fn api() -> Result<&'static Api, Unavailable> {
    static API: OnceLock<Result<Api, Unavailable>> = OnceLock::new();
    API.get_or_init(load).as_ref().map_err(|e| *e)
}
pub fn unicode_version() -> Result<[u8; 4], Unavailable> {
    let mut version = [0; 4];
    // SAFETY: ICU writes precisely the four-byte version buffer.
    unsafe {
        (api()?.version)(version.as_mut_ptr());
    }
    Ok(version)
}
pub fn ranges(property: &str, value: &str) -> Result<Vec<(char, char)>, Unavailable> {
    let api = api()?;
    if property.len() > 128 || value.len() > 128 || !property.is_ascii() || !value.is_ascii() {
        return Err(Unavailable);
    }
    let property: Vec<u16> = property.encode_utf16().collect();
    let value: Vec<u16> = value.encode_utf16().collect();
    // SAFETY: The constructor takes an empty interval; null is checked. All
    // subsequent operations receive this owned live set and bounded buffers.
    let raw = unsafe { (api.open)(1, 0) };
    if raw.is_null() {
        return Err(Unavailable);
    }
    struct Set<'a> {
        api: &'a Api,
        raw: *mut c_void,
    }
    impl Drop for Set<'_> {
        fn drop(&mut self) {
            unsafe { (self.api.close)(self.raw) }
        }
    }
    let set = Set { api, raw };
    let mut status = 0;
    unsafe {
        (api.apply)(
            set.raw,
            property.as_ptr(),
            property.len() as i32,
            value.as_ptr(),
            value.len() as i32,
            &mut status,
        )
    };
    if status > 0 {
        return Err(Unavailable);
    }
    let count = unsafe { (api.count)(set.raw) };
    if !(0..=0x110000).contains(&count) {
        return Err(Unavailable);
    }
    let mut ranges = Vec::new();
    for index in 0..count {
        let (mut start, mut end) = (0, 0);
        let length = unsafe {
            (api.item)(
                set.raw,
                index,
                &mut start,
                &mut end,
                std::ptr::null_mut(),
                0,
                &mut status,
            )
        };
        if status > 0 || length != 0 || start < 0 || end > 0x10ffff || start > end {
            return Err(Unavailable);
        }
        let lo = if (0xd800..=0xdfff).contains(&start) {
            0xe000
        } else {
            start
        };
        let hi = if (0xd800..=0xdfff).contains(&end) {
            0xd7ff
        } else {
            end
        };
        {
            if lo <= hi {
                ranges.push((
                    char::from_u32(lo as u32).ok_or(Unavailable)?,
                    char::from_u32(hi as u32).ok_or(Unavailable)?,
                ));
            }
        }
    }
    Ok(ranges)
}
pub type FoldTable = [(char, &'static [char])];
pub fn folds() -> Result<&'static FoldTable, Unavailable> {
    static TABLE: OnceLock<Result<Box<FoldTable>, Unavailable>> = OnceLock::new();
    TABLE
        .get_or_init(|| {
            let api = api()?;
            let mut groups: BTreeMap<char, Vec<char>> = BTreeMap::new();
            for scalar in 0..=0x10ffff {
                let Some(c) = char::from_u32(scalar) else {
                    continue;
                };
                // SAFETY: ICU accepts every scalar; option zero selects default simple folding.
                let folded = unsafe { (api.fold)(scalar as i32, 0) };
                let folded = char::from_u32(folded as u32).ok_or(Unavailable)?;
                if folded != c {
                    let group = groups.entry(folded).or_insert_with(|| vec![folded]);
                    group.push(c);
                }
            }
            let mut result = Vec::new();
            for group in groups.values() {
                for &c in group {
                    let mut others: Vec<_> = group.iter().copied().filter(|x| *x != c).collect();
                    others.sort_unstable();
                    others.dedup();
                    result.push((c, &*Box::leak(others.into_boxed_slice())));
                }
            }
            result.sort_unstable_by_key(|x| x.0);
            Ok(result.into_boxed_slice())
        })
        .as_ref()
        .map(|v| v.as_ref())
        .map_err(|e| *e)
}

pub fn supports_age(version: [u8; 2]) -> Result<bool, Unavailable> {
    static AGES: OnceLock<Result<std::collections::BTreeSet<[u8; 2]>, Unavailable>> =
        OnceLock::new();
    let ages = AGES
        .get_or_init(|| {
            let api = api()?;
            let mut ages = std::collections::BTreeSet::new();
            for scalar in (0..=0x10ffff).filter(|n| char::from_u32(*n).is_some()) {
                let mut age = [0; 4];
                // SAFETY: Valid scalar and writable four-byte ICU version array.
                unsafe { (api.age)(scalar as i32, age.as_mut_ptr()) };
                if age[0] != 0 {
                    ages.insert([age[0], age[1]]);
                }
            }
            Ok(ages)
        })
        .as_ref()
        .map_err(|e| *e)?;
    Ok(ages.contains(&version))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capabilities_ranges_and_failures() {
        let version = unicode_version().unwrap();
        assert!(version[0] >= 1);
        assert!(supports_age([3, 0]).unwrap());
        assert!(!supports_age([1, 2]).unwrap());
        assert!(!supports_age([255, 0]).unwrap());
        assert_eq!(ranges("Any", "").unwrap(), vec![('\0', '\u{10ffff}')]);
        assert!(ranges("gc", "Cs").unwrap().is_empty());
        assert!(ranges("not_a_property", "").is_err());
        assert_eq!(
            ranges("gc", "Lu").unwrap(),
            ranges("General_Category", "Uppercase_Letter").unwrap()
        );
        assert!(ranges("scx", "Greek")
            .unwrap()
            .iter()
            .any(|&(a, b)| a <= 'λ' && 'λ' <= b));
    }
    #[test]
    fn every_scalar_ranges_are_ordered_and_fold_equivalence_is_symmetric() {
        for (property, value) in [
            ("gc", "L"),
            ("White_Space", ""),
            ("scx", "Greek"),
            ("gcb", "Extend"),
        ] {
            let ranges = ranges(property, value).unwrap();
            assert!(ranges.windows(2).all(|r| r[0].1 < r[1].0));
            assert!(ranges.iter().all(|&(a, b)| a <= b));
        }
        let table = folds().unwrap();
        for &(c, others) in table {
            assert!(!others.contains(&c));
            for other in others {
                let index = table.binary_search_by_key(other, |x| x.0).unwrap();
                assert!(table[index].1.contains(&c));
            }
        }
    }
}
