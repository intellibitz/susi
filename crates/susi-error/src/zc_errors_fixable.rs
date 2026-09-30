//! Every user-facing error carries its fix command.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FixableError {
    pub message: String,
    pub fix_cmd: String,
}

/// Attach a actionable fix command to a user-facing error.
#[must_use]
pub fn with_fix(message: &str, fix_cmd: &str) -> FixableError {
    FixableError {
        message: message.into(),
        fix_cmd: fix_cmd.into(),
    }
}

#[cfg(test)]
mod zc_errors_fixable_tests {
    use super::*;

    #[test]
    fn zc_errors_fixable_carries_fix_command() {
        let e = with_fix("daemon not running", "susi start");
        assert!(e.message.contains("daemon"));
        assert_eq!(e.fix_cmd, "susi start");
    }
}
