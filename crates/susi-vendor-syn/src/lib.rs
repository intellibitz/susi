#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

//! # susi-vendor-syn
//!
//! The one crate that links `syn` for runtime Rust analysis. Three calls,
//! no syn types across the boundary: [`is_valid_rust`], [`analyze`] (the
//! bloat auditor's per-file metrics) and [`outline`] (top-level items for
//! the `ast_analyze` tool).

use syn::visit::{self, Visit};

/// True when `src` parses as a Rust source file.
#[must_use]
pub fn is_valid_rust(src: &str) -> bool {
    syn::parse_file(src).is_ok()
}

/// Production-code metrics of one Rust file. Test-only code (`#[test]`,
/// `#[cfg(test)]` modules, impls and functions) is excluded: the mandates
/// exempt tests from the unwrap/expect rules.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RustMetrics {
    pub functions: usize,
    pub oversized_functions: usize,
    pub unwrap_calls: usize,
    pub expect_calls: usize,
    pub clone_calls: usize,
    pub unsafe_blocks: usize,
}

/// Parse `src` and count its metrics; a function with more than
/// `max_statements` top-level statements is oversized. `None` when `src`
/// does not parse.
#[must_use]
pub fn analyze(src: &str, max_statements: usize) -> Option<RustMetrics> {
    let ast = syn::parse_file(src).ok()?;
    let mut v = MetricsVisitor {
        max_statements,
        m: RustMetrics::default(),
    };
    v.visit_file(&ast);
    Some(v.m)
}

/// Kind of a top-level item in [`outline`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    Function,
    Struct,
    Enum,
    Other,
}

/// Top-level items of `src` in order, with names for functions / structs /
/// enums (empty for [`ItemKind::Other`]). `None` when `src` does not parse.
#[must_use]
pub fn outline(src: &str) -> Option<Vec<(ItemKind, String)>> {
    let file = syn::parse_file(src).ok()?;
    Some(
        file.items
            .iter()
            .map(|item| {
                // Only the headline kinds are named; every other syn::Item
                // variant (and future ones) is reported as `Other`.
                #[allow(clippy::wildcard_enum_match_arm)]
                match item {
                    syn::Item::Fn(f) => (ItemKind::Function, f.sig.ident.to_string()),
                    syn::Item::Struct(s) => (ItemKind::Struct, s.ident.to_string()),
                    syn::Item::Enum(e) => (ItemKind::Enum, e.ident.to_string()),
                    _ => (ItemKind::Other, String::new()),
                }
            })
            .collect(),
    )
}

/// Whether a cfg expression can only be enabled in a test build.
fn cfg_requires_test(meta: &syn::Meta) -> bool {
    match meta {
        syn::Meta::Path(path) => path.is_ident("test"),
        syn::Meta::List(list) => {
            let Some(operator) = list.path.get_ident().map(ToString::to_string) else {
                return false;
            };
            let Ok(items) = list.parse_args_with(
                syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
            ) else {
                return false;
            };
            match operator.as_str() {
                "all" => items.iter().any(cfg_requires_test),
                "any" => !items.is_empty() && items.iter().all(cfg_requires_test),
                _ => false,
            }
        }
        syn::Meta::NameValue(_) => false,
    }
}

fn is_test_only(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("test")
            || (attr.path().is_ident("cfg")
                && attr
                    .parse_args::<syn::Meta>()
                    .is_ok_and(|meta| cfg_requires_test(&meta)))
    })
}

struct MetricsVisitor {
    max_statements: usize,
    m: RustMetrics,
}

impl MetricsVisitor {
    fn count_fn(&mut self, stmts: usize) {
        self.m.functions += 1;
        if stmts > self.max_statements {
            self.m.oversized_functions += 1;
        }
    }
}

impl<'ast> Visit<'ast> for MetricsVisitor {
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        if !is_test_only(&node.attrs) {
            visit::visit_item_mod(self, node);
        }
    }

    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        if !is_test_only(&node.attrs) {
            visit::visit_item_impl(self, node);
        }
    }

    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        if is_test_only(&node.attrs) {
            return;
        }
        self.count_fn(node.block.stmts.len());
        visit::visit_item_fn(self, node);
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        if is_test_only(&node.attrs) {
            return;
        }
        self.count_fn(node.block.stmts.len());
        visit::visit_impl_item_fn(self, node);
    }

    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        match node.method.to_string().as_str() {
            "unwrap" => self.m.unwrap_calls += 1,
            "expect" => self.m.expect_calls += 1,
            "clone" => self.m.clone_calls += 1,
            _ => {}
        }
        visit::visit_expr_method_call(self, node);
    }

    fn visit_expr_unsafe(&mut self, node: &'ast syn::ExprUnsafe) {
        self.m.unsafe_blocks += 1;
        visit::visit_expr_unsafe(self, node);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_code_is_exempt_including_impls() {
        let src = r#"
            fn prod() { x.unwrap(); }
            #[cfg(test)] mod t { fn f() { y.unwrap(); } }
            #[cfg(test)] impl S { fn g() { z.unwrap(); } }
            #[cfg(all(test, feature = "audit-tests"))] impl S { fn j() { q.unwrap(); } }
            #[cfg(any(test, feature = "audit-tests"))] fn maybe_prod() { m.unwrap(); }
            #[cfg(not(test))] fn not_test() { n.unwrap(); }
            impl S { #[test] fn h() { w.unwrap(); } fn k() { v.expect("e"); } }
        "#;
        let m = analyze(src, 60).unwrap();
        assert_eq!((m.functions, m.unwrap_calls, m.expect_calls), (4, 3, 1));
    }

    #[test]
    fn outline_names_headline_items() {
        let o = outline("fn a() {} struct B; enum C {} const D: u8 = 0;").unwrap();
        assert_eq!(o[0], (ItemKind::Function, "a".into()));
        assert_eq!(o[3].0, ItemKind::Other);
        assert!(!is_valid_rust("fn ("));
    }
}
