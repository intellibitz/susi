//! Reasoning-grade capability floor for estate operations
//! (T-DEEPSEEK-162, VC-202-024).
//!
//! Planning a multi-step change to a live ecosystem — ordering restarts,
//! preserving a warm cache, spending the last of a quota — is reasoning
//! work, so `TaskClass::EstateOps` declares a Reasoning floor and the
//! routing ladder reads it before price: the cheapest model is not
//! eligible however cheap it is, while cost still decides between the
//! qualified.

use crate::engines::brain::{Store, TaskClass};
use crate::engines::capability::ModelCapability;
use crate::engines::cost::Budget;

fn names(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn estate_operations_capability_floor_is_reasoning_grade() {
    assert_eq!(
        TaskClass::EstateOps.capability_floor(),
        ModelCapability::Reasoning,
        "estate operations declare a reasoning-grade floor"
    );
    // The class is data beside the class — labeled, serialized, in ALL.
    assert_eq!(TaskClass::EstateOps.label(), "estate_ops");
    assert!(TaskClass::ALL.contains(&TaskClass::EstateOps));
}

#[test]
fn estate_operations_capability_floor_reached_by_the_requires_token() {
    // The production bridge a dispatch carries: requires:"estate" on a
    // routed call must classify into the floored class, not fall through
    // to the chat floor.
    assert_eq!(
        TaskClass::from_requires(Some("estate")),
        TaskClass::EstateOps
    );
    assert_eq!(
        TaskClass::from_requires(Some("estate_ops")),
        TaskClass::EstateOps
    );
    // No text a user types classifies into estate ops by accident — the
    // class is assigned, not inferred.
    assert_ne!(
        TaskClass::classify("please restart the model server when idle"),
        TaskClass::EstateOps
    );
}

#[test]
fn estate_operations_capability_floor_the_cheapest_model_is_not_eligible() {
    // haiku is a low-cost Basic model; opus is a high-cost Reasoning one.
    // Under the frugal dial cost pressure is at its strongest — and it
    // still cannot put the cheap weak model first for estate work.
    let s = Store::default();
    let r = s.rank_with_budget(
        &names(&["anthropic-haiku", "anthropic-opus"]),
        TaskClass::EstateOps,
        Budget::Low,
    );
    assert_eq!(r[0].provider, "anthropic-opus");
    assert!(r[0].meets_floor);
    assert_eq!(r[1].provider, "anthropic-haiku");
    assert!(
        !r[1].meets_floor,
        "however cheap, a below-floor model is not eligible"
    );

    // The below-floor rung is kept as the last resort, never silently
    // dropped — the ladder still reaches it if nothing qualifies.
    let solo = s.rank(&names(&["anthropic-haiku"]), TaskClass::EstateOps);
    assert_eq!(solo.len(), 1);
    assert!(!solo[0].meets_floor);
}

#[test]
fn estate_operations_capability_floor_cost_still_decides_the_qualified() {
    // Both candidates meet the floor; the cheaper qualified brain leads.
    let s = Store::default();
    let r = s.rank_with_budget(
        &names(&["anthropic-opus", "groq-llama-70b"]),
        TaskClass::EstateOps,
        Budget::Low,
    );
    assert!(r[0].meets_floor && r[1].meets_floor);
    assert_eq!(
        r[0].provider,
        "groq-llama-70b",
        "cost decides between qualified brains: {:?}",
        r.iter()
            .map(|x| (x.provider.as_str(), x.score))
            .collect::<Vec<_>>()
    );
}

#[test]
fn estate_operations_capability_floor_carries_through_the_surfaces() {
    // The per-class surfaces the ladder feeds — leaders, the capability
    // matrix, elections — read the class from ALL, so estate ops is a row
    // in each rather than a special case.
    let leaders = crate::scout_schedule::current_leaders(&names(&["anthropic-opus"]));
    assert!(
        leaders.contains_key("estate_ops"),
        "the leaders table carries the class: {leaders:?}"
    );
    assert_eq!(leaders["estate_ops"].as_deref(), Some("anthropic-opus"));
}

#[test]
fn estate_operations_capability_floor_wiring_is_on_the_production_path() {
    // The floor must be the data the shared ladder reads — no parallel
    // estate-only ranking that could drift.
    let brain = include_str!("../engines/brain.rs");
    assert!(
        brain.contains("EstateOps") && brain.contains("Self::Reasoning | Self::EstateOps"),
        "the class and its floor live beside TaskClass"
    );
    assert!(
        brain.contains(r#"Some("estate") | Some("estate_ops")"#),
        "the requires token maps into the class"
    );
    let cost = include_str!("../engines/cost.rs");
    assert!(
        cost.contains("TaskClass::EstateOps"),
        "cost scores the class on the same ladder"
    );
}
