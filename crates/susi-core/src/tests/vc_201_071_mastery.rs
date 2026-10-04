//! Mastery verification for VC-201-071: one authoritative evaluation
//! contract mapping MacPolicy and daemon CapabilityPolicy decisions,
//! denying consistently across tool, peer, and model entry points.

use crate::capability_contract::{evaluate, EntryPoint, LayerDecision, LayerVerdict};
use crate::mac_policy::{actions, MacPolicy, PrivacyMode};

fn layer(name: &str, v: LayerVerdict) -> LayerDecision {
    LayerDecision {
        layer: name.into(),
        verdict: v,
    }
}

/// Verification: fabricated layers from unrecognized names are rejected.
/// The contract requires known authoritative policy layers.
#[test]
fn vc_201_071_mastery_fabricated_layer_denied() {
    let forged = [layer("not-a-real-policy-layer", LayerVerdict::Allow)];
    assert_eq!(evaluate(EntryPoint::Tool, &forged), LayerVerdict::Deny);
    assert_eq!(evaluate(EntryPoint::Peer, &forged), LayerVerdict::Deny);
    assert_eq!(evaluate(EntryPoint::Model, &forged), LayerVerdict::Deny);
}

/// Verification: the mandatory access control (mac) layer cannot be omitted.
/// Even if an auxiliary layer claims Allow, omitting the mac layer denies.
#[test]
fn vc_201_071_mastery_missing_mac_layer_denied() {
    let layers = [layer("capability", LayerVerdict::Allow)];
    assert_eq!(evaluate(EntryPoint::Tool, &layers), LayerVerdict::Deny);
    assert_eq!(evaluate(EntryPoint::Peer, &layers), LayerVerdict::Deny);
    assert_eq!(evaluate(EntryPoint::Model, &layers), LayerVerdict::Deny);
}

/// Verification: EntryPoint semantics are distinguished and enforced.
/// An entry-point restriction for Model denies Model access while allowing Tool access.
#[test]
fn vc_201_071_mastery_entry_point_enforced() {
    let layers = [
        layer("mac", LayerVerdict::Allow),
        layer("capability", LayerVerdict::Allow),
        LayerDecision::for_entry_point(EntryPoint::Model, LayerVerdict::Deny),
    ];
    assert_eq!(evaluate(EntryPoint::Tool, &layers), LayerVerdict::Allow);
    assert_eq!(evaluate(EntryPoint::Peer, &layers), LayerVerdict::Allow);
    assert_eq!(evaluate(EntryPoint::Model, &layers), LayerVerdict::Deny);
}

/// Verification: Real policy mapping from MacPolicy and CapabilityToken.
#[test]
fn vc_201_071_mastery_derived_from_mac_and_token() {
    let key = [42u8; 32];
    let policy = MacPolicy::new(key, PrivacyMode::Balanced, false);

    // Granted action in policy
    let token = policy.grant("worker-1", actions::PROCESS_EXEC, "/bin/echo", Some(3600));

    // Valid derivation from MAC policy
    let mac_dec = LayerDecision::from_mac(&policy, "worker-1", actions::PROCESS_EXEC, "/bin/echo");
    assert_eq!(mac_dec.verdict, LayerVerdict::Allow);

    // Valid derivation from verified token
    let token_dec = LayerDecision::from_token(&policy, &token);
    assert_eq!(token_dec.verdict, LayerVerdict::Allow);

    let layers = [mac_dec, token_dec];
    assert_eq!(evaluate(EntryPoint::Tool, &layers), LayerVerdict::Allow);

    // Tampered token fails derivation
    let mut tampered = token.clone();
    tampered.action = actions::NETWORK_EGRESS.into();
    let tampered_dec = LayerDecision::from_token(&policy, &tampered);
    assert_eq!(tampered_dec.verdict, LayerVerdict::Deny);

    let failing_layers = [
        LayerDecision::from_mac(&policy, "worker-1", actions::PROCESS_EXEC, "/bin/echo"),
        tampered_dec,
    ];
    assert_eq!(
        evaluate(EntryPoint::Tool, &failing_layers),
        LayerVerdict::Deny
    );
}

/// Verification: any Deny wins at every entry point, and an empty layer set denies.
#[test]
fn vc_201_071_mastery_deny_wins_and_empty_denies() {
    for ep in [EntryPoint::Tool, EntryPoint::Peer, EntryPoint::Model] {
        let layers = [
            layer("mac", LayerVerdict::Allow),
            layer("capability", LayerVerdict::Deny),
        ];
        assert_eq!(evaluate(ep, &layers), LayerVerdict::Deny);
        assert_eq!(evaluate(ep, &[]), LayerVerdict::Deny);
    }
}
