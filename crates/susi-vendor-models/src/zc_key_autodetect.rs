//! Infer vendor from a pasted API key prefix.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyGuess {
    pub vendor: String,
}

#[must_use]
pub fn autodetect_vendor(key: &str) -> Option<KeyGuess> {
    let k = key.trim();
    if k.starts_with("sk-ant-") {
        Some(KeyGuess {
            vendor: "anthropic".into(),
        })
    } else if k.starts_with("sk-") {
        Some(KeyGuess {
            vendor: "openai".into(),
        })
    } else if k.starts_with("gsk_") {
        Some(KeyGuess {
            vendor: "groq".into(),
        })
    } else if k.starts_with("AIza") {
        Some(KeyGuess {
            vendor: "google".into(),
        })
    } else {
        None
    }
}

#[cfg(test)]
mod zc_key_autodetect_tests {
    use super::*;

    #[test]
    fn zc_key_autodetect_infers_vendor_from_prefix() {
        assert_eq!(autodetect_vendor("sk-ant-xxx").unwrap().vendor, "anthropic");
        assert_eq!(autodetect_vendor("sk-xxx").unwrap().vendor, "openai");
        assert_eq!(autodetect_vendor("gsk_xxx").unwrap().vendor, "groq");
        assert!(autodetect_vendor("nope").is_none());
    }
}
