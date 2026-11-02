//! The forbid-only metric examines crate attributes, not item grammar or
//! unsafe-function counts. Tokenize the entire file so legacy item syntax
//! (notably pre-dyn Fn aliases) cannot silently remove a measured file.

use syn::parse::{Parse, ParseStream};

struct CrateAttributes(Vec<syn::Attribute>);

impl Parse for CrateAttributes {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let attrs = input.call(syn::Attribute::parse_inner)?;
        input.step(|cursor| {
            let mut rest = *cursor;
            while let Some((_, next)) = rest.token_tree() {
                rest = next;
            }
            Ok(((), rest))
        })?;
        Ok(Self(attrs))
    }
}

/// Detect an unconditional crate-root `forbid(unsafe_code)` declaration.
/// The whole source must tokenize and all initial inner attributes must parse.
/// Conditional attributes, `deny`, item attributes and macro text do not count.
/// This deliberately does not certify item grammar or count unsafe functions.
pub fn crate_forbids_unsafe_checked(source: &str) -> Result<bool, String> {
    let mut session = super::ClassificationSession::begin();
    unicode_ident::clear_classification_error();
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    let source = if let Some(after_bang) = source.strip_prefix("#!") {
        if attribute_follows(after_bang)? {
            source
        } else {
            source.split_once('\n').map_or("", |(_, rest)| rest)
        }
    } else {
        source
    };
    let parsed = syn::parse_str::<CrateAttributes>(source);
    let classification_error = unicode_ident::take_classification_error();
    session.finish()?;
    if let Some(error) = classification_error {
        return Err(format!(
            "Rust identifier classification unavailable: {error}"
        ));
    }
    let parsed = parsed.map_err(|error| format!("crate attribute/tokenization error: {error}"))?;
    let mut forbids = false;
    for attr in parsed.0 {
        if !attr.path().is_ident("forbid") {
            continue;
        }
        let entries = attr
            .parse_args_with(
                syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
            )
            .map_err(|error| format!("malformed forbid attribute: {error}"))?;
        for entry in entries {
            match entry {
                syn::Meta::Path(path) => forbids |= path.is_ident("unsafe_code"),
                syn::Meta::NameValue(value) if value.path.is_ident("reason") => {
                    if !matches!(
                        value.value,
                        syn::Expr::Lit(syn::ExprLit {
                            lit: syn::Lit::Str(_),
                            ..
                        })
                    ) {
                        return Err("forbid reason must be a string".into());
                    }
                }
                syn::Meta::NameValue(_) | syn::Meta::List(_) => {
                    return Err("malformed forbid lint entry".into());
                }
            }
        }
    }
    Ok(forbids)
}

// Rust permits whitespace/comments between #! and the opening bracket. The
// only preprocessing is a real shebang/BOM; source files are never rewritten.
fn attribute_follows(mut tail: &str) -> Result<bool, String> {
    loop {
        tail = tail.trim_start();
        if let Some(comment) = tail.strip_prefix("//") {
            tail = comment.split_once('\n').map_or("", |(_, rest)| rest);
        } else if tail.starts_with("/*") {
            let bytes = tail.as_bytes();
            let mut depth = 1usize;
            let mut offset = 2usize;
            while depth != 0 && offset + 1 < bytes.len() {
                match &bytes[offset..offset + 2] {
                    b"/*" => {
                        depth += 1;
                        offset += 2;
                    }
                    b"*/" => {
                        depth -= 1;
                        offset += 2;
                    }
                    _ => offset += 1,
                }
            }
            if depth != 0 {
                return Err("unterminated comment after crate attribute marker".into());
            }
            tail = &tail[offset..];
        } else {
            return Ok(tail.starts_with('['));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_bare_fn_alias_remains_measurable() {
        let legacy = "#[allow(unknown_lints, bare_trait_objects)]\ntype Action = Fn(&siginfo_t) + Send + Sync;";
        assert_eq!(crate_forbids_unsafe_checked(legacy), Ok(false));
        assert_eq!(
            crate_forbids_unsafe_checked(&format!("#![forbid(unsafe_code)]\n{legacy}")),
            Ok(true)
        );
    }

    #[test]
    fn only_unconditional_crate_attributes_count() {
        for source in [
            "#![deny(unsafe_code)]",
            "#![warn(unsafe_code)]",
            "#![cfg_attr(feature = \"strict\", forbid(unsafe_code))]",
            "#[forbid(unsafe_code)] fn f() {}",
            "fn f() { let s = r#\"#![forbid(unsafe_code)]\"#; }",
            "// #![forbid(unsafe_code)]\nmod x { #![forbid(unsafe_code)] }",
        ] {
            assert_eq!(crate_forbids_unsafe_checked(source), Ok(false), "{source}");
        }
        assert_eq!(
            crate_forbids_unsafe_checked("#![forbid(unused, unsafe_code, reason = \"boundary\")]"),
            Ok(true)
        );
    }

    #[test]
    fn malformed_source_and_attributes_fail_closed() {
        for source in [
            "fn f( {",
            "/* never closed",
            "#![forbid(unsafe_code = false)]",
            "#![forbid(unsafe_code, reason = 7)]",
            "#![forbid]",
            "#! /* never closed",
        ] {
            assert!(crate_forbids_unsafe_checked(source).is_err(), "{source}");
        }
    }

    #[test]
    fn shebang_bom_and_nested_comments_preserve_attributes() {
        for source in [
            "\u{feff}#![forbid(unsafe_code)]",
            "#!/usr/bin/env rust-script\n#![forbid(unsafe_code)]",
            "#! /* outer /* inner */ end */ [forbid(unsafe_code)]",
        ] {
            assert_eq!(crate_forbids_unsafe_checked(source), Ok(true), "{source}");
        }
    }
}
