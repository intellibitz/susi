//! Classify `SUSI_*` environment variables for zero-config debt reporting.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvClass {
    Internal,
    Test,
    UserFacing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvVarClass {
    pub name: &'static str,
    pub class: EnvClass,
}

/// Known SUSI_* variables and their audience.
pub const ENV_REGISTRY: &[EnvVarClass] = &[
    EnvVarClass {
        name: "SUSI_HOME",
        class: EnvClass::Internal,
    },
    EnvVarClass {
        name: "SUSI_XDG",
        class: EnvClass::Test,
    },
    EnvVarClass {
        name: "SUSI_CONFIG_PORT",
        class: EnvClass::Internal,
    },
    EnvVarClass {
        name: "SUSI_ALLOW_PRIMARY",
        class: EnvClass::Test,
    },
    EnvVarClass {
        name: "SUSI_ALLOW_MAIN",
        class: EnvClass::Test,
    },
    EnvVarClass {
        name: "OPENAI_API_KEY",
        class: EnvClass::UserFacing,
    },
    EnvVarClass {
        name: "ANTHROPIC_API_KEY",
        class: EnvClass::UserFacing,
    },
];

#[must_use]
pub fn classify(name: &str) -> Option<EnvClass> {
    ENV_REGISTRY
        .iter()
        .find(|e| e.name == name)
        .map(|e| e.class)
}

#[must_use]
pub fn user_facing_names() -> Vec<&'static str> {
    ENV_REGISTRY
        .iter()
        .filter(|e| matches!(e.class, EnvClass::UserFacing))
        .map(|e| e.name)
        .collect()
}

#[cfg(test)]
mod zc_env_classify_tests {
    use super::*;

    #[test]
    fn zc_env_classify_partitions_internal_test_user() {
        assert_eq!(classify("SUSI_HOME"), Some(EnvClass::Internal));
        assert_eq!(classify("SUSI_XDG"), Some(EnvClass::Test));
        assert_eq!(classify("OPENAI_API_KEY"), Some(EnvClass::UserFacing));
        assert!(user_facing_names().contains(&"ANTHROPIC_API_KEY"));
        assert!(classify("UNKNOWN_VAR").is_none());
    }
}
