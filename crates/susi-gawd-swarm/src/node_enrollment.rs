//! Node enrollment with join tokens and mTLS.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Enrollment {
    pub accepted: bool,
    pub reason: String,
}

/// Accept enrollment when join token matches and mTLS identity is present.
#[must_use]
pub fn enroll(token: &str, expected_token: &str, mtls_identity: Option<&str>) -> Enrollment {
    if token != expected_token {
        return Enrollment {
            accepted: false,
            reason: "bad join token".into(),
        };
    }
    if mtls_identity.is_none() {
        return Enrollment {
            accepted: false,
            reason: "mTLS identity required".into(),
        };
    }
    Enrollment {
        accepted: true,
        reason: "enrolled".into(),
    }
}

#[cfg(test)]
mod node_enrollment_tests {
    use super::*;

    #[test]
    fn node_enrollment_requires_token_and_mtls() {
        assert!(!enroll("x", "secret", Some("node-a")).accepted);
        assert!(!enroll("secret", "secret", None).accepted);
        assert!(enroll("secret", "secret", Some("node-a")).accepted);
    }
}
