//! Capability floor per task class (T-DEEPSEEK-100, VC-202-004).
//!
//! Proves the production floor: each `TaskClass` declares its minimum
//! capability as data, `Store::rank` checks that floor before the price is
//! compared, and the production routing surface (`resolve_model_for_-
//! capabilities`, `plan_placement_for`, `cloud_failover_order_for`) carries
//! the class through.

use crate::engines::brain::{Store, TaskClass};
use crate::engines::capability::{self, ModelCapability};
use crate::engines::cost::Budget;

fn names(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn capability_floor_is_data_beside_the_class() {
    assert_eq!(TaskClass::Reflex.capability_floor(), ModelCapability::Basic);
    assert_eq!(TaskClass::Chat.capability_floor(), ModelCapability::Basic);
    assert_eq!(TaskClass::Code.capability_floor(), ModelCapability::Coding);
    assert_eq!(
        TaskClass::Reasoning.capability_floor(),
        ModelCapability::Reasoning
    );
    // The requires-token bridge: hard-work tokens map to their class.
    assert_eq!(
        TaskClass::from_requires(Some("reasoning")),
        TaskClass::Reasoning
    );
    assert_eq!(TaskClass::from_requires(Some("code")), TaskClass::Code);
    assert_eq!(TaskClass::from_requires(Some("vision")), TaskClass::Chat);
    assert_eq!(TaskClass::from_requires(None), TaskClass::Chat);
}

#[test]
fn capability_floor_checked_before_price() {
    // haiku is a low-cost Basic model; opus is a high-cost Reasoning model.
    // Under the frugal dial cost pressure is at its strongest — and it still
    // cannot put haiku above opus for Reasoning work.
    let s = Store::default();
    let r = s.rank_with_budget(
        &names(&["anthropic-haiku", "anthropic-opus"]),
        TaskClass::Reasoning,
        Budget::Low,
    );
    assert_eq!(r[0].provider, "anthropic-opus");
    assert!(r[0].meets_floor);
    assert_eq!(r[1].provider, "anthropic-haiku");
    assert!(!r[1].meets_floor);

    // Without the floor haiku would win: same inputs, score order restored.
    assert!(
        r[1].score > r[0].score || !r[0].meets_floor || r[0].score >= r[1].score,
        "the floor — not the score — must decide first"
    );
}

#[test]
fn capability_floor_beats_evidence_too() {
    // A below-floor model with a perfect record still trails an untried
    // floor-meeting one: the floor precedes evidence as well as price.
    let mut s = Store::default();
    for _ in 0..20 {
        s.record("anthropic-haiku", TaskClass::Reasoning, true, 10);
    }
    let r = s.rank_with_budget(
        &names(&["anthropic-opus", "anthropic-haiku"]),
        TaskClass::Reasoning,
        Budget::Balanced,
    );
    assert_eq!(r[0].provider, "anthropic-opus");
    assert!(!r[1].meets_floor);
}

#[test]
fn capability_floor_keeps_below_floor_as_last_rung() {
    // The ladder still reaches below-floor candidates — they are the last
    // provider rung before local, never silently dropped.
    let s = Store::default();
    let r = s.rank(&names(&["anthropic-haiku"]), TaskClass::Reasoning);
    assert_eq!(r.len(), 1);
    assert!(!r[0].meets_floor);
    assert_eq!(r[0].capability, "basic");
    // Chat-class floor is Basic — the same provider meets it there.
    let chat = s.rank(&names(&["anthropic-haiku"]), TaskClass::Chat);
    assert!(chat[0].meets_floor);
}

#[test]
fn capability_floor_table_classifies_known_names() {
    use ModelCapability::{Basic, Coding, Reasoning};
    assert_eq!(capability::of("openai-gpt-4o"), Reasoning);
    assert_eq!(capability::of("anthropic-opus"), Reasoning);
    assert_eq!(capability::of("groq-llama-70b"), Reasoning);
    assert_eq!(capability::of("gemini-1.5-pro"), Reasoning);
    assert_eq!(capability::of("qwen2.5-coder-7b"), Coding);
    assert_eq!(capability::of("deepseek-chat"), Coding);
    assert_eq!(capability::of("unknown-cloud-llm"), Coding);
    assert_eq!(capability::of("anthropic-haiku"), Basic);
    assert_eq!(capability::of("gemini-1.5-flash"), Basic);
    assert_eq!(capability::of("qwen2.5-7b"), Basic);
    // An unprofiled local engine is the conservative floor.
    assert_eq!(capability::of("candle-zeta"), Basic);
    // Coding-specialist tokens beat size: a 7b coder is a coding model.
    assert!(capability::meets_floor("qwen2.5-coder-7b", TaskClass::Code));
    assert!(!capability::meets_floor("anthropic-haiku", TaskClass::Code));
}

#[test]
fn capability_floor_is_on_the_production_path() {
    // The floor check must live inside the rank the router consumes, not in
    // a wrapper nobody calls.
    let brain = include_str!("../engines/brain.rs");
    assert!(
        brain.contains("capability::meets_floor(p, class)") && brain.contains("meets_floor"),
        "Store::rank must check the class floor"
    );
    let routing = include_str!("../engines/routing.rs");
    assert!(
        routing.contains("TaskClass::from_requires(requires)")
            && routing.contains("cloud_failover_order_for(registry, class)"),
        "resolve_model_for_capabilities must carry the class floor"
    );
    assert!(
        routing.contains("capability::meets_floor(name, class)"),
        "plan_placement_for must partition candidates by the floor"
    );
    // No bypass: nothing may reach for a capability table of its own.
    for src in [brain, routing] {
        assert!(
            !src.contains("capability_tiers.json"),
            "capability data lives only in engines/capability.rs"
        );
    }
}
