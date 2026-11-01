use alloc::string::ToString;

use crate::hir;

/// An inclusive range of codepoints from a generated file (hence the static
/// lifetime).

/// An error that occurs when dealing with Unicode.
///
/// We don't impl the Error trait here because these always get converted
/// into other public errors. (This error type isn't exported.)
#[derive(Debug)]
pub enum Error {
    PropertyNotFound,
    PropertyValueNotFound,
    // Not used when unicode-perl is enabled.
    #[allow(dead_code)]
    PerlClassNotFound,
}

/// An error that occurs when Unicode-aware simple case folding fails.
///
/// This error can occur when the case mapping tables necessary for Unicode
/// aware case folding are unavailable. This only occurs when the
/// `unicode-case` feature is disabled. (The feature is enabled by default.)
#[derive(Debug)]
pub struct CaseFoldError(());

#[cfg(feature = "std")]
impl std::error::Error for CaseFoldError {}

impl core::fmt::Display for CaseFoldError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "Unicode-aware case folding is not available \
             (probably because the unicode-case feature is not enabled)"
        )
    }
}

/// An error that occurs when the Unicode-aware `\w` class is unavailable.
///
/// This error can occur when the data tables necessary for the Unicode aware
/// Perl character class `\w` are unavailable. This only occurs when the
/// `unicode-perl` feature is disabled. (The feature is enabled by default.)
#[derive(Debug)]
pub struct UnicodeWordError(());

#[cfg(feature = "std")]
impl std::error::Error for UnicodeWordError {}

impl core::fmt::Display for UnicodeWordError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "Unicode-aware \\w class is not available \
             (probably because the unicode-perl feature is not enabled)"
        )
    }
}

/// A state oriented traverser of the simple case folding table.
///
/// A case folder can be constructed via `SimpleCaseFolder::new()`, which will
/// return an error if the underlying case folding table is unavailable.
///
/// After construction, it is expected that callers will use
/// `SimpleCaseFolder::mapping` by calling it with codepoints in strictly
/// increasing order. For example, calling it on `b` and then on `a` is illegal
/// and will result in a panic.
///
/// The main idea of this type is that it tries hard to make mapping lookups
/// fast by exploiting the structure of the underlying table, and the ordering
/// assumption enables this.
#[derive(Debug)]
pub struct SimpleCaseFolder {
    /// The simple case fold table. It's a sorted association list, where the
    /// keys are Unicode scalar values and the values are the corresponding
    /// equivalence class (not including the key) of the "simple" case folded
    /// Unicode scalar values.
    table: &'static [(char, &'static [char])],
    /// The last codepoint that was used for a lookup.
    last: Option<char>,
    /// The index to the entry in `table` corresponding to the smallest key `k`
    /// such that `k > k0`, where `k0` is the most recent key lookup. Note that
    /// in particular, `k0` may not be in the table!
    next: usize,
}

impl SimpleCaseFolder {
    /// Create a new simple case folder, returning an error if the underlying
    /// case folding table is unavailable.
    pub fn new() -> Result<SimpleCaseFolder, CaseFoldError> {
        #[cfg(not(feature = "unicode-case"))]
        {
            Err(CaseFoldError(()))
        }
        #[cfg(feature = "unicode-case")]
        {
            Ok(SimpleCaseFolder {
                table: susi_os_unicode::folds().map_err(|_| CaseFoldError(()))?,
                last: None,
                next: 0,
            })
        }
    }

    /// Return the equivalence class of case folded codepoints for the given
    /// codepoint. The equivalence class returned never includes the codepoint
    /// given. If the given codepoint has no case folded codepoints (i.e.,
    /// no entry in the underlying case folding table), then this returns an
    /// empty slice.
    ///
    /// # Panics
    ///
    /// This panics when called with a `c` that is less than or equal to the
    /// previous call. In other words, callers need to use this method with
    /// strictly increasing values of `c`.
    pub fn mapping(&mut self, c: char) -> &'static [char] {
        if let Some(last) = self.last {
            assert!(
                last < c,
                "got codepoint U+{:X} which occurs before \
                 last codepoint U+{:X}",
                u32::from(c),
                u32::from(last),
            );
        }
        self.last = Some(c);
        if self.next >= self.table.len() {
            return &[];
        }
        let (k, v) = self.table[self.next];
        if k == c {
            self.next += 1;
            return v;
        }
        match self.get(c) {
            Err(i) => {
                self.next = i;
                &[]
            }
            Ok(i) => {
                // Since we require lookups to proceed
                // in order, anything we find should be
                // after whatever we thought might be
                // next. Otherwise, the caller is either
                // going out of order or we would have
                // found our next key at 'self.next'.
                assert!(i > self.next);
                self.next = i + 1;
                self.table[i].1
            }
        }
    }

    /// Returns true if and only if the given range overlaps with any region
    /// of the underlying case folding table. That is, when true, there exists
    /// at least one codepoint in the inclusive range `[start, end]` that has
    /// a non-trivial equivalence class of case folded codepoints. Conversely,
    /// when this returns false, all codepoints in the range `[start, end]`
    /// correspond to the trivial equivalence class of case folded codepoints,
    /// i.e., itself.
    ///
    /// This is useful to call before iterating over the codepoints in the
    /// range and looking up the mapping for each. If you know none of the
    /// mappings will return anything, then you might be able to skip doing it
    /// altogether.
    ///
    /// # Panics
    ///
    /// This panics when `end < start`.
    pub fn overlaps(&self, start: char, end: char) -> bool {
        use core::cmp::Ordering;

        assert!(start <= end);
        self.table
            .binary_search_by(|&(c, _)| {
                if start <= c && c <= end {
                    Ordering::Equal
                } else if c > end {
                    Ordering::Greater
                } else {
                    Ordering::Less
                }
            })
            .is_ok()
    }

    /// Returns the index at which `c` occurs in the simple case fold table. If
    /// `c` does not occur, then this returns an `i` such that `table[i-1].0 <
    /// c` and `table[i].0 > c`.
    fn get(&self, c: char) -> Result<usize, usize> {
        self.table.binary_search_by_key(&c, |&(c1, _)| c1)
    }
}

/// Unicode property query resolved against the installed OS repertoire.
#[derive(Debug)]
pub enum ClassQuery<'a> {
    OneLetter(char),
    Binary(&'a str),
    ByValue {
        property_name: &'a str,
        property_value: &'a str,
    },
}
fn norm(s: &str) -> alloc::string::String {
    s.chars()
        .filter(|c| !matches!(c, '_' | '-' | ' '))
        .map(|c| c.to_ascii_lowercase())
        .collect()
}
fn query(property: &str, value: &str) -> Result<hir::ClassUnicode, Error> {
    let (property, value) = if matches!(norm(property).as_str(), "gc" | "generalcategory")
        && matches!(norm(value).as_str(), "any" | "ascii" | "assigned")
    {
        (value, "")
    } else {
        (property, value)
    };
    if norm(property) == "age" {
        let version =
            susi_os_unicode::unicode_version().map_err(|_| Error::PropertyValueNotFound)?;
        let numbers: Option<alloc::vec::Vec<u8>> =
            value.split('.').map(|s| s.parse::<u8>().ok()).collect();
        let Some(numbers) = numbers else {
            return Err(Error::PropertyValueNotFound);
        };
        if numbers.len() != 2
            || numbers[0] == 0
            || [numbers[0], numbers[1]] > [version[0], version[1]]
            || !susi_os_unicode::supports_age([numbers[0], numbers[1]])
                .map_err(|_| Error::PropertyValueNotFound)?
        {
            return Err(Error::PropertyValueNotFound);
        }
    }
    let ranges = susi_os_unicode::ranges(property, value).map_err(|_| {
        if value.is_empty() {
            Error::PropertyNotFound
        } else {
            Error::PropertyValueNotFound
        }
    })?;
    Ok(hir_class(&ranges))
}
pub fn class(query_value: ClassQuery<'_>) -> Result<hir::ClassUnicode, Error> {
    match query_value {
        ClassQuery::OneLetter(c) => class(ClassQuery::Binary(&c.to_string())),
        ClassQuery::Binary(name) => {
            let normalized = norm(name);
            if cfg!(feature = "unicode-gencat")
                && matches!(normalized.as_str(), "any" | "ascii" | "assigned")
            {
                return query(name, "");
            }
            if cfg!(feature = "unicode-perl") && normalized == "digit" {
                return query("gc", "Nd");
            }
            // ICU resolves binary properties, general category aliases and script
            // aliases. Gate each family explicitly rather than widening disabled features.
            if cfg!(feature = "unicode-gencat") {
                if let Ok(c) = query("gc", name) {
                    return Ok(c);
                }
            }
            if cfg!(feature = "unicode-script") {
                if let Ok(c) = query("sc", name) {
                    return Ok(c);
                }
            }
            if cfg!(feature = "unicode-bool") {
                return query(name, "");
            }
            Err(Error::PropertyNotFound)
        }
        ClassQuery::ByValue {
            property_name,
            property_value,
        } => {
            let enabled = match norm(property_name).as_str() {
                "gc" | "generalcategory" => cfg!(feature = "unicode-gencat"),
                "sc" | "script" | "scx" | "scriptextensions" => cfg!(feature = "unicode-script"),
                "age" => cfg!(feature = "unicode-age"),
                "gcb" | "graphemeclusterbreak" | "wb" | "wordbreak" | "sb" | "sentencebreak" => {
                    cfg!(feature = "unicode-segment")
                }
                _ => false,
            };
            if !enabled {
                return Err(Error::PropertyNotFound);
            }
            query(property_name, property_value)
        }
    }
}
pub fn hir_class(ranges: &[(char, char)]) -> hir::ClassUnicode {
    hir::ClassUnicode::new(
        ranges
            .iter()
            .map(|&(a, b)| hir::ClassUnicodeRange::new(a, b)),
    )
}
pub fn perl_word() -> Result<hir::ClassUnicode, Error> {
    if !cfg!(feature = "unicode-perl") {
        return Err(Error::PerlClassNotFound);
    }
    let mut class = hir::ClassUnicode::empty();
    for (p, v) in [
        ("Alphabetic", ""),
        ("gc", "M"),
        ("gc", "Nd"),
        ("gc", "Pc"),
        ("Join_Control", ""),
    ] {
        class.union(&query(p, v)?);
    }
    Ok(class)
}
pub fn perl_space() -> Result<hir::ClassUnicode, Error> {
    if !cfg!(any(feature = "unicode-perl", feature = "unicode-bool")) {
        return Err(Error::PerlClassNotFound);
    }
    query("White_Space", "")
}
pub fn perl_digit() -> Result<hir::ClassUnicode, Error> {
    if !cfg!(any(feature = "unicode-perl", feature = "unicode-gencat")) {
        return Err(Error::PerlClassNotFound);
    }
    query("gc", "Nd")
}
pub fn is_word_character(c: char) -> Result<bool, UnicodeWordError> {
    #[cfg(not(feature = "std"))]
    {
        let _ = c;
        return Err(UnicodeWordError(()));
    }
    #[cfg(feature = "std")]
    {
        static WORD: std::sync::OnceLock<Result<hir::ClassUnicode, UnicodeWordError>> =
            std::sync::OnceLock::new();
        let class = WORD
            .get_or_init(|| perl_word().map_err(|_| UnicodeWordError(())))
            .as_ref()
            .map_err(|_| UnicodeWordError(()))?;
        Ok(class
            .ranges()
            .binary_search_by(|r| {
                if c < r.start() {
                    core::cmp::Ordering::Greater
                } else if c > r.end() {
                    core::cmp::Ordering::Less
                } else {
                    core::cmp::Ordering::Equal
                }
            })
            .is_ok())
    }
}
#[cfg(all(test, feature = "unicode", feature = "std"))]
mod os_tests {
    use super::*;
    #[test]
    fn os_properties_and_rejection() {
        assert!(class(ClassQuery::ByValue {
            property_name: "scx",
            property_value: "Greek"
        })
        .is_ok());
        assert!(class(ClassQuery::ByValue {
            property_name: "age",
            property_value: "3.0"
        })
        .is_ok());
        assert!(class(ClassQuery::Binary("Not_A_Property")).is_err());
        assert!(is_word_character('λ').unwrap());
        assert!(!is_word_character('!').unwrap());
    }
    #[test]
    fn simple_fold_equivalence() {
        let mut f = SimpleCaseFolder::new().unwrap();
        assert_eq!(f.mapping('K'), &['k', 'K']);
        assert_eq!(f.mapping('k'), &['K', 'K']);
        assert_eq!(f.mapping('K'), &['K', 'k']);
    }
}
