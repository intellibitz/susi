//! Mastery verification for VC-201-071: one authoritative evaluation
//! contract mapping MacPolicy and daemon CapabilityPolicy decisions,
//! denying consistently across tool, peer, and model entry points.

use crate::capability_contract::{evaluate, EntryPoint, LayerDecision, LayerVerdict};

fn layer(name: &str, v: LayerVerdict) -> LayerDecision {
    LayerDecision {
        layer: name.into(),
        verdict: v,
    }
}

/// Falsification: there is no mapping from the real policies. evaluate
/// consumes caller-fabricated LayerDecision values — nothing derives them
/// from MacPolicy/CapabilityToken or the daemon CapabilityPolicy. A call
/// whose ONLY layer is an invented name that says Allow yields Allow:
/// the contract does not require the real policy layers to participate.
#[test]
fn vc_201_071_mastery_fabricated_layer_permits() {
    let forged = [layer("not-a-real-policy-layer", LayerVerdict::Allow)];
    assert_eq!(evaluate(EntryPoint::Tool, &forged), LayerVerdict::Allow);
    assert_eq!(evaluate(EntryPoint::Peer, &forged), LayerVerdict::Allow);
    assert_eq!(evaluate(EntryPoint::Model, &forged), LayerVerdict::Allow);
}

/// Falsification: a real layer can simply be omitted — the contract
/// cannot tell that the mandatory-access layer never answered. The
/// daemon's decision absent, one unrelated Allow layer still permits.
#[test]
fn vc_201_071_mastery_missing_real_layer_still_permits() {
    // mac layer never evaluated the request; only an unrelated Allow.
    let layers = [layer("telemetry", LayerVerdict::Allow)];
    assert_eq!(evaluate(EntryPoint::Tool, &layers), LayerVerdict::Allow);
}

/// Observation (not itself a failure, but shows the entry point is
/// decorative): Tool, Peer, and Model are interchangeable — entry is
/// `let _ = entry` — so 'across tool, peer, and model entry points' is
/// satisfied only because per-entry-point semantics do not exist.
#[test]
fn vc_201_071_mastery_entry_point_is_ignored() {
    let layers = [
        layer("mac", LayerVerdict::Allow),
        layer("cap", LayerVerdict::Allow),
    ];
    let a = evaluate(EntryPoint::Tool, &layers);
    let b = evaluate(EntryPoint::Peer, &layers);
    let c = evaluate(EntryPoint::Model, &layers);
    assert_eq!((a, b), (b, c));
}

/// What holds: any Deny wins at every entry point, and an empty layer
/// set denies.
#[test]
fn vc_201_071_mastery_deny_wins_and_empty_denies() {
    for ep in [EntryPoint::Tool, EntryPoint::Peer, EntryPoint::Model] {
        let layers = [
            layer("mac", LayerVerdict::Allow),
            layer("cap", LayerVerdict::Deny),
        ];
        assert_eq!(evaluate(ep, &layers), LayerVerdict::Deny);
        assert_eq!(evaluate(ep, &[]), LayerVerdict::Deny);
    }
}
