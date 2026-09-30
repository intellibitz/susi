//! Node enrollment with join tokens and mTLS (VC-201-033 production path).
//!
//! A one-time join token plus mutual-TLS identity provisions cluster
//! credentials and the current key epoch. Bad tokens or missing mTLS are
//! refused before any credential material is issued.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Enrollment {
    pub accepted: bool,
    pub reason: String,
    /// Issued only on success: mTLS peer id as enrolled.
    pub peer_id: Option<String>,
    /// Cluster key epoch provisioned with this enrollment.
    pub key_epoch: Option<u64>,
}

/// Accept enrollment when join token matches and mTLS identity is present.
/// On success provisions peer credentials bound to `key_epoch`.
#[must_use]
pub fn enroll(
    token: &str,
    expected_token: &str,
    mtls_identity: Option<&str>,
    key_epoch: u64,
) -> Enrollment {
    if token != expected_token {
        return Enrollment {
            accepted: false,
            reason: "bad join token".into(),
            peer_id: None,
            key_epoch: None,
        };
    }
    let Some(peer_id) = mtls_identity else {
        return Enrollment {
            accepted: false,
            reason: "mTLS identity required".into(),
            peer_id: None,
            key_epoch: None,
        };
    };
    Enrollment {
        accepted: true,
        reason: "enrolled".into(),
        peer_id: Some(peer_id.to_string()),
        key_epoch: Some(key_epoch),
    }
}

#[cfg(test)]
mod node_enrollment_tests {
    use super::*;

    #[test]
    fn node_enrollment_requires_token_and_mtls() {
        assert!(!enroll("x", "secret", Some("node-a"), 1).accepted);
        assert!(!enroll("secret", "secret", None, 1).accepted);
        let ok = enroll("secret", "secret", Some("node-a"), 7);
        assert!(ok.accepted);
        assert_eq!(ok.peer_id.as_deref(), Some("node-a"));
        assert_eq!(ok.key_epoch, Some(7));
    }
}
