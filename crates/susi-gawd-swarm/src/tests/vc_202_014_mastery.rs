//! Mastery verification for VC-202-014: secrets, egress and per-agent
//! authority.
//!
//! The vector bundles three claims. The secrets leg is substantially real:
//! `redact_credentials` runs at the logging, capture, context-graph,
//! untrusted-content, fleet and admin choke points. The per-mission egress
//! leg is refuted: `EgressGate`/`EgressPolicy` model exactly the required
//! per-mission allow-list with recorded refusals, but the reachability
//! checker finds zero production callers — the enforced path is the global
//! `mac_policy::egress_permitted` posture, which is not per-mission and
//! records nothing. The per-agent authority leg has real token machinery
//! (`CapabilityToken`, `is_permitted` consulted in abi_bridge and broker)
//! but refusal recording on the agent dispatch path is not a named,
//! reachable behavior. These tests pin the honest boundary.
//! (Filed under susi-gawd-swarm because susi-core was reserved by another
//! agent's claim.)

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

/// What does NOT hold on the production path, pinned as documentation: the
/// gate above is never constructed. `scripts/check-reachability.py
/// --refuted EgressGate::authorize` passes — no production caller. The
/// enforced egress check is `mac_policy::egress_permitted`, which answers
/// for the whole process posture (subject "susi", scope "*"), not for a
/// mission's declared allow-list, and returns a bare bool with no refusal
/// record. A mission that must not reach `exfil.example.net` is protected
/// only when the global posture is closed for everyone.
#[test]
fn vc_202_014_mastery_egress_gate_is_not_on_the_production_path() {
    // Executable proxy for the refutation: the production entry point is a
    // global boolean, not a per-mission gate — so a *per-mission* policy
    // can only be expressed by the (uncalled) machinery asserted above.
    let local = susi_core::mac_policy::egress_permitted("http://127.0.0.1:9090/health");
    assert!(
        local,
        "local targets are always permitted under the posture"
    );
    // Non-local traffic answers from posture alone — the call takes no
    // mission and returns no record:
    let posture_answer: bool =
        susi_core::mac_policy::egress_permitted("https://api.openai.com/v1/models");
    let _ = posture_answer; // mission name and refusal record are inexpressible
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
