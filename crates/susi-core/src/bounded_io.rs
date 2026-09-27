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
