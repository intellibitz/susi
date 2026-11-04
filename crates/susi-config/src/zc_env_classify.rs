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
        name: "SUSI_PORT_OFFSET",
        class: EnvClass::Internal,
    },
    EnvVarClass {
        name: "SUSI_API_TOKEN",
        class: EnvClass::Internal,
    },
    EnvVarClass {
        name: "SUSI_TOKEN_FILE",
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
        name: "SUSI_PATH_CONSENT",
        class: EnvClass::UserFacing,
    },
    EnvVarClass {
        name: "OPENAI_API_KEY",
        class: EnvClass::UserFacing,
    },
    EnvVarClass {
        name: "ANTHROPIC_API_KEY",
        class: EnvClass::UserFacing,
    },
    EnvVarClass {
        name: "GEMINI_API_KEY",
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
pub fn internal_names() -> Vec<&'static str> {
    ENV_REGISTRY
        .iter()
        .filter(|e| matches!(e.class, EnvClass::Internal))
        .map(|e| e.name)
        .collect()
}

#[must_use]
pub fn test_only_names() -> Vec<&'static str> {
    ENV_REGISTRY
        .iter()
        .filter(|e| matches!(e.class, EnvClass::Test))
        .map(|e| e.name)
        .collect()
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
    use std::collections::HashSet;

    #[test]
    fn zc_env_classify_partitions_internal_test_user() {
        assert_eq!(classify("SUSI_HOME"), Some(EnvClass::Internal));
        assert_eq!(classify("SUSI_PORT_OFFSET"), Some(EnvClass::Internal));
        assert_eq!(classify("SUSI_API_TOKEN"), Some(EnvClass::Internal));
        assert_eq!(classify("SUSI_TOKEN_FILE"), Some(EnvClass::Internal));
        assert_eq!(classify("SUSI_XDG"), Some(EnvClass::Test));
        assert_eq!(classify("SUSI_PATH_CONSENT"), Some(EnvClass::UserFacing));
        assert_eq!(classify("OPENAI_API_KEY"), Some(EnvClass::UserFacing));
        assert_eq!(classify("GEMINI_API_KEY"), Some(EnvClass::UserFacing));

        let internal = internal_names();
        assert!(internal.contains(&"SUSI_PORT_OFFSET"));
        assert!(internal.contains(&"SUSI_API_TOKEN"));
        assert!(internal.contains(&"SUSI_TOKEN_FILE"));

        let test_only = test_only_names();
        assert!(test_only.contains(&"SUSI_XDG"));
        assert!(test_only.contains(&"SUSI_ALLOW_PRIMARY"));
        assert!(test_only.contains(&"SUSI_ALLOW_MAIN"));

        let user_facing = user_facing_names();
        assert!(user_facing.contains(&"ANTHROPIC_API_KEY"));
        assert!(user_facing.contains(&"SUSI_PATH_CONSENT"));
        assert!(user_facing.contains(&"GEMINI_API_KEY"));

        assert!(classify("UNKNOWN_VAR").is_none());
    }

    #[test]
    fn zc_env_registry_entries_are_unique_non_empty_and_exhaustively_partitioned() {
        let mut seen = HashSet::new();
        for entry in ENV_REGISTRY {
            assert!(!entry.name.trim().is_empty());
            assert!(
                seen.insert(entry.name),
                "duplicate ENV_REGISTRY entry: {}",
                entry.name
            );
        }
        assert_eq!(
            internal_names().len() + test_only_names().len() + user_facing_names().len(),
            ENV_REGISTRY.len()
        );
    }

    #[test]
    fn classify_is_case_sensitive_and_env_class_serializes_to_snake_case() {
        assert_eq!(classify("susi_home"), None);
        assert_eq!(classify("openai_api_key"), None);
        assert_eq!(classify(""), None);

        for (class, expected_wire) in [
            (EnvClass::Internal, "\"internal\""),
            (EnvClass::Test, "\"test\""),
            (EnvClass::UserFacing, "\"user_facing\""),
        ] {
            let encoded = serde_json::to_string(&class).expect("serialize EnvClass");
            assert_eq!(encoded, expected_wire);
            let decoded: EnvClass = serde_json::from_str(&encoded).expect("deserialize EnvClass");
            assert_eq!(decoded, class);
        }

        let sample = EnvVarClass {
            name: "SUSI_HOME",
            class: EnvClass::Internal,
        };
        let sample_val = serde_json::to_value(&sample).expect("serialize EnvVarClass");
        assert_eq!(sample_val["name"], "SUSI_HOME");
        assert_eq!(sample_val["class"], "internal");
    }

    #[test]
    fn every_registered_var_roundtrips_through_classify_and_matches_its_partition_helper() {
        let internal: HashSet<&str> = internal_names().into_iter().collect();
        let test_only: HashSet<&str> = test_only_names().into_iter().collect();
        let user_facing: HashSet<&str> = user_facing_names().into_iter().collect();

        for entry in ENV_REGISTRY {
            assert_eq!(classify(entry.name), Some(entry.class));
            assert_eq!(
                internal.contains(entry.name),
                entry.class == EnvClass::Internal
            );
            assert_eq!(
                test_only.contains(entry.name),
                entry.class == EnvClass::Test
            );
            assert_eq!(
                user_facing.contains(entry.name),
                entry.class == EnvClass::UserFacing
            );
        }
    }
}
