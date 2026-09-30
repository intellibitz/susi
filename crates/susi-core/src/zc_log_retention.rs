//! Bound audit/trace/cooldown/brain files by size and age with no settings.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetentionPolicy {
    pub max_bytes: u64,
    pub max_age_secs: u64,
}

/// Default retention for a named log class.
#[must_use]
pub fn retention_for(kind: &str) -> RetentionPolicy {
    match kind {
        "audit" => RetentionPolicy {
            max_bytes: 64 * 1024 * 1024,
            max_age_secs: 30 * 24 * 3600,
        },
        "trace" => RetentionPolicy {
            max_bytes: 256 * 1024 * 1024,
            max_age_secs: 7 * 24 * 3600,
        },
        "cooldown" => RetentionPolicy {
            max_bytes: 4 * 1024 * 1024,
            max_age_secs: 24 * 3600,
        },
        "brain" => RetentionPolicy {
            max_bytes: 32 * 1024 * 1024,
            max_age_secs: 90 * 24 * 3600,
        },
        _ => RetentionPolicy {
            max_bytes: 16 * 1024 * 1024,
            max_age_secs: 14 * 24 * 3600,
        },
    }
}

/// Whether a file should be rotated away.
#[must_use]
pub fn should_rotate(kind: &str, size_bytes: u64, age_secs: u64) -> bool {
    let p = retention_for(kind);
    size_bytes > p.max_bytes || age_secs > p.max_age_secs
}

#[cfg(test)]
mod zc_log_retention_tests {
    use super::*;

    #[test]
    fn zc_log_retention_bounds_by_size_and_age() {
        assert!(!should_rotate("audit", 1024, 60));
        assert!(should_rotate("audit", 65 * 1024 * 1024, 0));
        assert!(should_rotate("trace", 0, 8 * 24 * 3600));
        assert!(retention_for("brain").max_age_secs > retention_for("cooldown").max_age_secs);
    }
}
