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

    #[test]
    fn enroll_reports_exact_failure_reasons_and_clears_credentials() {
        let bad_token = enroll("wrong", "expected", None, 42);
        assert!(!bad_token.accepted);
        assert_eq!(bad_token.reason, "bad join token");
        assert_eq!(bad_token.peer_id, None);
        assert_eq!(bad_token.key_epoch, None);

        let missing_mtls = enroll("expected", "expected", None, 42);
        assert!(!missing_mtls.accepted);
        assert_eq!(missing_mtls.reason, "mTLS identity required");
        assert_eq!(missing_mtls.peer_id, None);
        assert_eq!(missing_mtls.key_epoch, None);
    }

    #[test]
    fn enroll_preserves_boundary_key_epochs_and_peer_id() {
        for epoch in [0, 1, u64::MAX] {
            let res = enroll("tok", "tok", Some("peer-xyz"), epoch);
            assert!(res.accepted);
            assert_eq!(res.reason, "enrolled");
            assert_eq!(res.peer_id.as_deref(), Some("peer-xyz"));
            assert_eq!(res.key_epoch, Some(epoch));
        }
    }

    #[test]
    fn enrollment_serde_json_roundtrip_for_accepted_and_rejected_states() {
        let accepted = enroll("t", "t", Some("node-1"), 9);
        let rejected = enroll("bad", "t", Some("node-1"), 9);
        for item in [accepted, rejected] {
            let encoded = serde_json::to_string(&item).unwrap();
            let decoded: Enrollment = serde_json::from_str(&encoded).unwrap();
            assert_eq!(decoded, item.clone());
            assert!(!format!("{item:?}").is_empty());
        }
    }

    #[test]
    fn enroll_rejects_empty_token_mismatch_and_prioritizes_token_check_over_mtls() {
        let empty_mismatch = enroll("", "secret-token", Some("node-1"), 3);
        assert!(!empty_mismatch.accepted);
        assert_eq!(empty_mismatch.reason, "bad join token");
        assert!(empty_mismatch.peer_id.is_none());
        assert!(empty_mismatch.key_epoch.is_none());
    }
}
