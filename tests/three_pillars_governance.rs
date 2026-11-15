//! Governance drift checks for the three-pillar Step 1 contract.
#[test]
fn current_guidance_names_exactly_three_architectural_pillars() {
    for (name, text) in [
        ("AGENTS.md", include_str!("../AGENTS.md")),
        ("README.md", include_str!("../README.md")),
        ("Mandate 74", include_str!("../docs/ceo-mandate-74.md")),
        ("Mandate 75", include_str!("../docs/ceo-mandate-75.md")),
        ("Mandate 79", include_str!("../docs/ceo-mandate-79.md")),
        ("Mandate 81", include_str!("../docs/ceo-mandate-81.md")),
    ] {
        assert!(
            text.contains("three core pillars"),
            "{name}: current directive missing"
        );
        for stale in [
            "five pillars",
            "five Apache-2.0 core pillars",
            "5 open-core pillars",
            "5 primary open-core pillars",
        ] {
            assert!(!text.contains(stale), "{name}: stale taxonomy {stale}");
        }
    }
}
#[test]
fn mandate_keeps_capabilities_cadence_and_release_boundaries() {
    let text = include_str!("../docs/ceo-mandate-80.md");
    for required in [
        "GAWD",
        "GEMI",
        "GMCP",
        "A2A",
        "ACP",
        "Susi Harness",
        "parallel multi-agent workflow",
        "every ten minutes",
        "Step 1",
        "rollback",
        "qualification",
    ] {
        assert!(text.contains(required), "missing {required}");
    }
}
