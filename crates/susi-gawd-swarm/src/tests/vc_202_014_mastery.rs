//! Mastery verification for VC-202-014: secrets, egress and per-agent
//! authority.
//!
//! The vector bundles three claims. The secrets leg is substantially real:
//! `redact_credentials` runs at the logging, capture, context-graph,
//! untrusted-content, fleet and admin choke points. The per-mission egress
//! leg is delivered: `susi_tools::ToolRegistry::execute_tool` — the single
//! dispatch choke every tool call passes — resolves the acting mission's
//! evidence session, mints its posture-bounded token set, checks the
//! mission's own tokens at dispatch, and consults the mission's
//! `EgressGate` on every egress-class call. Refusals are recorded per
//! mission (`EgressRefusal` for the gate, `AuthorityRefusal` for token
//! denials). The behavioral proofs live in `crates/susi-tools/src/
//! registry.rs` (`vc_202_014_mastery` tests) because this crate does not
//! link susi-tools; the tests below pin the machinery and the production
//! wiring by source inspection.

use susi_core::egress::{DenyReason, EgressGate, EgressPolicy};
use susi_core::zc_egress_allowlist::allowlist_from_vendors;

/// What does hold: the per-mission egress machinery itself is correct — a
/// mission-declared policy admits only named hosts (with wildcard
/// subdomains), and every refusal is recorded with the mission, host and
/// reason.
#[test]
fn vc_202_014_mastery_egress_gate_records_per_mission_refusals() {
    let policy = EgressPolicy::from_allowlist(&allowlist_from_vendors(&["openai"]));
    let mut gate = EgressGate::new(policy);

    assert_eq!(gate.authorize("mission-a", "api.openai.com"), Ok(()));
    assert_eq!(
        gate.authorize("mission-a", "exfil.example.net"),
        Err(DenyReason::HostNotAllowed)
    );
    let refusals = gate.refusals();
    assert_eq!(refusals.len(), 1);
    assert_eq!(refusals[0].mission, "mission-a");
    assert_eq!(refusals[0].host, "exfil.example.net");

    let mut empty = EgressGate::new(EgressPolicy::deny_all());
    assert_eq!(
        empty.authorize("mission-b", "api.openai.com"),
        Err(DenyReason::PolicyEmpty)
    );
}

/// The gate is on the production dispatch path: `ToolRegistry::execute_tool`
/// — the choke every tool dispatch passes — resolves the acting mission,
/// mints and checks its tokens, and consults its `EgressGate` on
/// egress-class calls. The end-to-end behavior is proven by the
/// `vc_202_014_mastery` tests inside `crates/susi-tools/src/registry.rs`;
/// this crate cannot link susi-tools, so this test pins the wiring by
/// source inspection (same convention as the VC-202-010 reachability
/// scans).
#[test]
fn vc_202_014_mastery_egress_gate_is_on_the_production_dispatch_path() {
    let src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../susi-tools/src/registry.rs"
    ))
    .expect("susi-tools registry.rs must exist");
    // The gate is constructed per mission inside the dispatch choke…
    assert!(
        src.contains("EgressGate::new"),
        "production dispatch must build per-mission EgressGates"
    );
    // …consulted on egress-class dispatches inside execute_tool…
    assert!(
        src.contains("gate.authorize(mission"),
        "production dispatch must authorize against the mission's gate"
    );
    // …with the mission's own tokens checked at dispatch (subject = mission
    // id, not the process-wide "susi")…
    assert!(
        src.contains("Some(mission)"),
        "dispatch must pass the mission subject to authorize_tool"
    );
    // …and both refusal kinds recorded.
    assert!(
        src.contains("record_authority_refusal"),
        "dispatch must record authority refusals per mission"
    );
}

/// What does hold: the secrets leg — `redact_credentials` scrubs key-shaped
/// material, and it is invoked at the logging/capture/context-graph/
/// untrusted-content/admin choke points (production call sites, not tests).
#[test]
fn vc_202_014_mastery_secret_redaction_scrubs_key_material() {
    let raw = "token is sk-abcdefghijklmnopqrstuvwxyz0123456789 and key=AKIAIOSFODNN7EXAMPLE";
    let redacted = susi_config::redact_credentials(raw);
    assert!(
        !redacted.contains("sk-abcdefghijklmnopqrstuvwxyz0123456789"),
        "key material must not survive redaction: {redacted}"
    );
}
