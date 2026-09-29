use crate::capability_contract::{evaluate, EntryPoint, LayerDecision, LayerVerdict};

#[test]
fn vc_201_071_conflicting_rules_deny_consistently() {
    let layers = [
        LayerDecision {
            layer: "mac".into(),
            verdict: LayerVerdict::Allow,
        },
        LayerDecision {
            layer: "capability".into(),
            verdict: LayerVerdict::Deny,
        },
    ];
    for ep in [EntryPoint::Tool, EntryPoint::Peer, EntryPoint::Model] {
        assert_eq!(evaluate(ep, &layers), LayerVerdict::Deny);
    }
}

#[test]
fn vc_201_071_all_allow_permits() {
    let layers = [LayerDecision {
        layer: "mac".into(),
        verdict: LayerVerdict::Allow,
    }];
    assert_eq!(evaluate(EntryPoint::Tool, &layers), LayerVerdict::Allow);
}

#[test]
fn vc_201_071_empty_layers_deny() {
    assert_eq!(evaluate(EntryPoint::Model, &[]), LayerVerdict::Deny);
}
