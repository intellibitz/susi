//! VC-201-033 node enrollment (token plus mTLS). The suite's identity revocation tests
//! moved with identity_revoke to Susi Commercial Apps (susi-fleet-command).
use crate::node_enrollment::enroll;

#[test]
fn vc_201_033_enrollment_requires_token_and_mtls() {
    assert!(!enroll("x", "secret", Some("node-a"), 1).accepted);
    assert!(!enroll("secret", "secret", None, 1).accepted);
    let ok = enroll("secret", "secret", Some("node-a"), 1);
    assert!(ok.accepted);
    assert_eq!(ok.peer_id.as_deref(), Some("node-a"));
    assert_eq!(ok.key_epoch, Some(1));
}

/// Production enrollment path: valid production token plus mTLS identity
/// provisions peer credentials and the cluster key epoch.
#[test]
fn node_enrollment_production() {
    let mut dag = crate::dag::MissionDag::new("enroll production");
    let prod = dag.enroll_node(
        "prod-token-2026",
        "prod-token-2026",
        Some("node-prod-1"),
        42,
    );
    assert!(prod.accepted);
    assert_eq!(prod.reason, "enrolled");
    assert_eq!(prod.peer_id.as_deref(), Some("node-prod-1"));
    assert_eq!(prod.key_epoch, Some(42));
    assert!(dag.roster.0.contains("node-prod-1"));

    // Wrong token rejected even with mTLS — roster unchanged.
    let before = dag.roster.clone();
    let bad = dag.enroll_node("wrong", "prod-token-2026", Some("node-prod-1"), 42);
    assert!(!bad.accepted);
    assert!(bad.peer_id.is_none());
    assert_eq!(dag.roster, before);

    // No mTLS rejected even with correct token.
    let no_tls = dag.enroll_node("prod-token-2026", "prod-token-2026", None, 42);
    assert!(!no_tls.accepted);
    assert!(no_tls.key_epoch.is_none());
}
