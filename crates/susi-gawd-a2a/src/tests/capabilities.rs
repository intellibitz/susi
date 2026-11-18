//! `GawdCapabilities` unit tests.

use super::*;

#[test]
fn gawd_capabilities_default_orchestrator_and_agent_capabilities_mapping() {
    let def = GawdCapabilities::default();
    assert!(def.streaming);
    assert!(def.push_notifications);

    let orch = GawdCapabilities::orchestrator();
    assert!(orch.streaming);
    assert!(orch.push_notifications);

    let mapped = orch.as_capabilities();
    assert_eq!(mapped.streaming, Some(true));
    assert_eq!(mapped.push_notifications, Some(true));

    let custom = GawdCapabilities {
        streaming: false,
        push_notifications: true,
    };
    let custom_mapped = custom.as_capabilities();
    assert_eq!(custom_mapped.streaming, Some(false));
    assert_eq!(custom_mapped.push_notifications, Some(true));
}

#[test]
fn gawd_capabilities_boolean_matrix_clone_debug_and_unset_history_default() {
    for (streaming, push_notifications) in
        [(false, false), (true, false), (false, true), (true, true)]
    {
        let caps = GawdCapabilities {
            streaming,
            push_notifications,
        };
        let cloned = caps.clone();
        assert_eq!(cloned.streaming, streaming);
        assert_eq!(cloned.push_notifications, push_notifications);

        let dbg = format!("{cloned:?}");
        assert!(dbg.contains("GawdCapabilities"));

        let a2a_caps = cloned.as_capabilities();
        let default_a2a = crate::protocol::AgentCapabilities::default();
        assert_eq!(a2a_caps.streaming, Some(streaming));
        assert_eq!(a2a_caps.push_notifications, Some(push_notifications));
        assert_eq!(
            a2a_caps.extended_agent_card,
            default_a2a.extended_agent_card
        );
    }
}

#[test]
fn gawd_capabilities_serde_roundtrip_and_default_orchestrator_equivalence() {
    let def = GawdCapabilities::default();
    let orch = GawdCapabilities::orchestrator();
    assert_eq!(def.streaming, orch.streaming);
    assert_eq!(def.push_notifications, orch.push_notifications);

    let a2a = orch.as_capabilities();
    let val = serde_json::to_value(&a2a).expect("AgentCapabilities must serialize");
    let decoded: crate::protocol::AgentCapabilities =
        serde_json::from_value(val).expect("AgentCapabilities must deserialize");
    assert_eq!(decoded.streaming, Some(true));
    assert_eq!(decoded.push_notifications, Some(true));
}

#[test]
fn gawd_capabilities_independent_mutation_and_fresh_agent_capabilities_allocation() {
    let mut caps = GawdCapabilities::orchestrator();
    caps.streaming = false;
    let first = caps.as_capabilities();
    caps.push_notifications = false;
    let second = caps.as_capabilities();

    assert_eq!(first.streaming, Some(false));
    assert_eq!(first.push_notifications, Some(true));
    assert_eq!(first.extended_agent_card, None);

    assert_eq!(second.streaming, Some(false));
    assert_eq!(second.push_notifications, Some(false));
    assert_eq!(second.extended_agent_card, None);
}

#[test]
fn gawd_capabilities_as_capabilities_preserves_disabled_flags_through_json_serialization() {
    let disabled = GawdCapabilities {
        streaming: false,
        push_notifications: false,
    };
    let a2a = disabled.as_capabilities();
    let json = serde_json::to_string(&a2a).expect("serialize capabilities");
    let roundtrip: crate::protocol::AgentCapabilities =
        serde_json::from_str(&json).expect("deserialize capabilities");
    assert_eq!(roundtrip.streaming, Some(false));
    assert_eq!(roundtrip.push_notifications, Some(false));
}
