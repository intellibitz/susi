//! Detectable facts should not require env vars.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetectableEnv {
    pub name: &'static str,
    pub detection: &'static str,
}

pub const DETECTABLE_ENVS: &[DetectableEnv] = &[
    DetectableEnv {
        name: "SUSI_CPU_COUNT",
        detection: "std::thread::available_parallelism",
    },
    DetectableEnv {
        name: "SUSI_HOSTNAME",
        detection: "gethostname",
    },
    DetectableEnv {
        name: "SUSI_ARCH",
        detection: "std::env::consts::ARCH",
    },
];

#[must_use]
pub fn should_replace_with_detection(name: &str) -> bool {
    DETECTABLE_ENVS.iter().any(|e| e.name == name)
}

#[cfg(test)]
mod zc_env_detectable_tests {
    use super::*;

    #[test]
    fn zc_env_detectable_lists_facts_not_secrets() {
        assert!(should_replace_with_detection("SUSI_CPU_COUNT"));
        assert!(should_replace_with_detection("SUSI_ARCH"));
        assert!(!should_replace_with_detection("OPENAI_API_KEY"));
        assert!(!DETECTABLE_ENVS.is_empty());
    }
}
