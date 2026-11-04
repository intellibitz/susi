//! Size-capped body reads for blocking HTTP responses (anything `Read`).
//! A broken or hostile endpoint streaming without end must not exhaust the
//! caller's memory.

use std::io::Read;

/// Default cap for JSON API responses (catalogs, search results, metadata).
pub const JSON_BODY_CAP: u64 = 16 * 1024 * 1024;

/// Deserializes at most `cap` bytes of `body`; a longer body is an error.
pub fn json_capped<T: serde::de::DeserializeOwned>(body: impl Read, cap: u64) -> Result<T, String> {
    let mut buf = Vec::new();
    body.take(cap.saturating_add(1))
        .read_to_end(&mut buf)
        .map_err(|e| e.to_string())?;
    if buf.len() as u64 > cap {
        return Err(format!("response body exceeds {cap} bytes"));
    }
    serde_json::from_slice(&buf).map_err(|e| e.to_string())
}

#[cfg(test)]
mod inline_tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn json_capped_deserializes_payload_within_limit() {
        let body = br#"{"status":"ok","count":3}"#;
        let val: serde_json::Value = json_capped(Cursor::new(body), 1024).unwrap();
        assert_eq!(val["status"], "ok");
        assert_eq!(val["count"], 3);
    }

    #[test]
    fn json_capped_accepts_payload_at_exact_cap_boundary() {
        let body = br#""hello""#;
        let val: String = json_capped(Cursor::new(body), body.len() as u64).unwrap();
        assert_eq!(val, "hello");
    }

    #[test]
    fn json_capped_rejects_payload_exceeding_cap_by_one_byte() {
        let body = br#"{"key":"value"}"#;
        let err = json_capped::<serde_json::Value>(Cursor::new(body), (body.len() - 1) as u64)
            .unwrap_err();
        assert!(err.contains("exceeds"), "{err}");
    }

    #[test]
    fn json_capped_rejects_malformed_json_within_cap() {
        let body = b"{not valid json";
        assert!(json_capped::<serde_json::Value>(Cursor::new(body), 1024).is_err());
        assert_eq!(JSON_BODY_CAP, 16 * 1024 * 1024);
    }

    #[test]
    fn json_capped_deserializes_empty_array_and_nested_objects() {
        let arr: Vec<u32> = json_capped(Cursor::new(b"[]"), 64).unwrap();
        assert!(arr.is_empty());
        let nested: serde_json::Value =
            json_capped(Cursor::new(br#"{"outer":{"inner":[1,2]}}"#), 128).unwrap();
        assert_eq!(nested["outer"]["inner"][1], 2);
    }
}
