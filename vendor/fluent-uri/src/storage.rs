// SPDX-License-Identifier: MIT
// Copyright 2026 Susi contributors.
// Purpose-specific safe text storage. URI Value is sealed to &str and String;
// no generic ownership/container library is needed for those two cases.

pub trait TextReference<'out> {
    fn into_text(self) -> &'out str;
}
impl<'source: 'out, 'out> TextReference<'out> for &'source str {
    fn into_text(self) -> &'out str {
        self
    }
}

pub trait TextStorage {
    type Text<'borrow>: TextReference<'borrow>
    where
        Self: 'borrow;
    fn text(&self) -> Self::Text<'_>;
}
impl<'source> TextStorage for &'source str {
    type Text<'borrow>
        = &'source str
    where
        Self: 'borrow;
    fn text(&self) -> Self::Text<'_> {
        *self
    }
}
#[cfg(feature = "alloc")]
impl TextStorage for alloc::string::String {
    type Text<'borrow>
        = &'borrow str
    where
        Self: 'borrow;
    fn text(&self) -> Self::Text<'_> {
        self.as_str()
    }
}

pub trait TextAt<'borrow, 'out>: TextStorage {
    fn text_at(&'borrow self) -> &'out str;
}
impl<'borrow, 'out, T> TextAt<'borrow, 'out> for T
where
    T: TextStorage + 'borrow,
    T::Text<'borrow>: TextReference<'out>,
{
    fn text_at(&'borrow self) -> &'out str {
        self.text().into_text()
    }
}

#[cfg(test)]
mod tests {
    use crate::{Iri, Uri};
    #[test]
    fn borrowed_components_outlive_the_uri_wrapper() {
        let source = alloc::string::String::from("https://example.test/path?q=value#fragment");
        let (path, query) = {
            let uri = Uri::parse(source.as_str()).unwrap();
            (uri.path(), uri.query().unwrap())
        };
        assert_eq!(path.as_str(), "/path");
        assert_eq!(query.as_str(), "q=value");
    }
    #[test]
    fn owned_and_borrowed_international_text_agree() {
        let source = "https://例え.テスト/路径?搜索=值#片段";
        let borrowed = Iri::parse(source).unwrap();
        let owned = Iri::parse(alloc::string::String::from(source)).unwrap();
        assert_eq!(owned, borrowed);
        assert_eq!(owned.path(), borrowed.path());
        assert_eq!(owned.query(), borrowed.query());
        assert_eq!(owned.fragment(), borrowed.fragment());
        assert_eq!(owned.normalize(), borrowed.normalize());
    }
}
