//! First-run local model chosen and fetched for this hardware.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FirstModel {
    pub id: String,
    pub reason: String,
}

/// Pick a first-run local model from free RAM.
#[must_use]
pub fn choose_first_model(free_ram_gb: u32) -> FirstModel {
    if free_ram_gb >= 32 {
        FirstModel {
            id: "qwen2.5-14b-instruct-q4".into(),
            reason: "fits comfortably in 32GB+".into(),
        }
    } else if free_ram_gb >= 16 {
        FirstModel {
            id: "qwen2.5-7b-instruct-q4".into(),
            reason: "fits in 16GB".into(),
        }
    } else {
        FirstModel {
            id: "qwen2.5-1.5b-instruct-q4".into(),
            reason: "low-memory default".into(),
        }
    }
}

#[cfg(test)]
mod zc_first_model_tests {
    use super::*;

    #[test]
    fn zc_first_model_scales_with_ram() {
        assert!(choose_first_model(8).id.contains("1.5b"));
        assert!(choose_first_model(16).id.contains("7b"));
        assert!(choose_first_model(64).id.contains("14b"));
    }
}
