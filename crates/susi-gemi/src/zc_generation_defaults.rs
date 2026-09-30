//! Sampling and repeat-penalty defaults per model family.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GenerationDefaults {
    pub repeat_penalty: f32,
    pub repeat_last_n: u32,
    pub max_generation_tokens: u32,
}

/// Family-table defaults instead of a single global constant.
#[must_use]
pub fn defaults_for_family(family: &str) -> GenerationDefaults {
    let f = family.to_ascii_lowercase();
    if f.contains("code") || f.contains("coder") {
        return GenerationDefaults {
            repeat_penalty: 1.05,
            repeat_last_n: 128,
            max_generation_tokens: 8192,
        };
    }
    if f.contains("instruct") || f.contains("chat") {
        return GenerationDefaults {
            repeat_penalty: 1.1,
            repeat_last_n: 64,
            max_generation_tokens: 4096,
        };
    }
    if f.contains("llama") {
        return GenerationDefaults {
            repeat_penalty: 1.08,
            repeat_last_n: 64,
            max_generation_tokens: 4096,
        };
    }
    GenerationDefaults {
        repeat_penalty: 1.1,
        repeat_last_n: 64,
        max_generation_tokens: 2048,
    }
}

#[cfg(test)]
mod zc_generation_defaults_tests {
    use super::*;

    #[test]
    fn zc_generation_defaults_per_family() {
        let code = defaults_for_family("qwen2.5-coder");
        let chat = defaults_for_family("llama-3-instruct");
        assert!(code.max_generation_tokens > chat.max_generation_tokens);
        assert!((code.repeat_penalty - 1.05).abs() < f32::EPSILON);
        assert_eq!(defaults_for_family("unknown").max_generation_tokens, 2048);
    }
}
