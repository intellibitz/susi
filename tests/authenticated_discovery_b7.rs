#![allow(clippy::unwrap_used)] // Test fixture source splitting must preserve exact input.
//! Generic source-scanner contract. The complete discovery security assertions
//! live with the authority and daemon owners in Commercial Apps' CLI tests.

fn production_prefix(src: &str) -> &str {
    src.split("#[cfg(test)]").next().unwrap_or(src)
}

#[test]
fn production_prefix_strips_cfg_test_suffix_and_preserves_unsplit_input() {
    let sample = "fn prod() {}\n#[cfg(test)]\nmod tests { fn helper() {} }";
    assert_eq!(production_prefix(sample), "fn prod() {}\n");
    assert_eq!(production_prefix("fn only_prod() {}"), "fn only_prod() {}");
}
