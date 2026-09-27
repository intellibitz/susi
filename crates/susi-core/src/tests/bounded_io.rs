//! `bounded_io` tests — kept out of the canonical source so crates that
//! `#[path]`-mount it never compile or run them.

use crate::bounded_io::json_capped;

#[test]
fn parses_within_the_cap_and_refuses_past_it() {
    let body = br#"{"models":[{"name":"a"}]}"#;
    let v: serde_json::Value = json_capped(&body[..], body.len() as u64).unwrap();
    assert_eq!(v["models"][0]["name"], "a");
    let err = json_capped::<serde_json::Value>(&body[..], body.len() as u64 - 1).unwrap_err();
    assert!(err.contains("exceeds"), "{err}");
    // An endless stream is cut at the cap, not read to exhaustion.
    let endless = std::io::repeat(b' ');
    assert!(json_capped::<serde_json::Value>(endless, 1024).is_err());
}
