//! EOS token ids taken from model metadata.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EosTokens {
    pub ids: Vec<u32>,
    pub source: String,
}

/// Prefer metadata-declared EOS ids; fall back to a family hint only when empty.
#[must_use]
pub fn eos_from_metadata(family: &str, metadata_ids: &[u32]) -> EosTokens {
    if !metadata_ids.is_empty() {
        return EosTokens {
            ids: metadata_ids.to_vec(),
            source: "metadata".into(),
        };
    }
    let ids = match family.to_ascii_lowercase().as_str() {
        f if f.contains("llama") => vec![2],
        f if f.contains("qwen") => vec![151_643],
        _ => vec![0],
    };
    EosTokens {
        ids,
        source: "family_hint".into(),
    }
}

#[cfg(test)]
mod zc_eos_tokens_tests {
    use super::*;

    #[test]
    fn zc_eos_tokens_prefer_model_metadata() {
        let from_meta = eos_from_metadata("llama", &[128_001, 128_009]);
        assert_eq!(from_meta.source, "metadata");
        assert_eq!(from_meta.ids, vec![128_001, 128_009]);
        let hint = eos_from_metadata("llama-3", &[]);
        assert_eq!(hint.source, "family_hint");
        assert_eq!(hint.ids, vec![2]);
    }
}
