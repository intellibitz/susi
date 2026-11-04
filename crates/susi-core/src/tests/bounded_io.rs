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

#[test]
fn json_body_cap_constant_is_sixteen_megabytes_and_zero_cap_accepts_only_empty_errors() {
    assert_eq!(crate::bounded_io::JSON_BODY_CAP, 16 * 1024 * 1024);
    assert_eq!(
        json_capped::<serde_json::Value>(&b"null"[..], 4).unwrap(),
        serde_json::Value::Null
    );
    assert_eq!(
        json_capped::<serde_json::Value>(&b"null"[..], 0),
        Err("response body exceeds 0 bytes".to_string())
    );
}

#[test]
fn malformed_json_within_cap_returns_serde_parse_error_not_cap_exceeded() {
    let err = json_capped::<serde_json::Value>(&b"{invalid-json"[..], 64).unwrap_err();
    assert!(!err.contains("exceeds"), "{err}");
}

#[test]
fn io_error_from_reader_propagates_cleanly() {
    struct FailingReader;

    impl std::io::Read for FailingReader {
        fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "socket reset",
            ))
        }
    }

    let err = json_capped::<serde_json::Value>(FailingReader, 128).unwrap_err();
    assert!(err.contains("socket reset"), "{err}");
}

#[test]
fn json_capped_accepts_payload_whose_byte_length_equals_exact_cap() {
    let payload = br#"{"ok":true}"#;
    let parsed: serde_json::Value =
        json_capped(&payload[..], payload.len() as u64).expect("exact cap should succeed");
    assert_eq!(parsed["ok"], serde_json::Value::Bool(true));
}
