//! Mastery checks for VC-201-086: probe admitted MCP servers for
//! initialization, discovery, cancellation, timeouts, malformed responses,
//! and credential isolation; compatibility status records tested behavior
//! rather than catalog membership.
//!
//! Every test name starts `vc_201_086_mastery_` so the vector's evidence is
//! enumerable with `cargo test -p susi-gmcp vc_201_086`.

use crate::mcp_profile::McpSpecProfile;

/// "Compatibility status records tested behavior rather than catalog
/// membership": `registry_metadata` is a hardcoded literal asserting
/// `supports_list_changed` — no probe ran, no behavior was tested.
#[test]
fn vc_201_086_mastery_compatibility_is_static_catalog_claim() {
    let p = McpSpecProfile::default_profile();
    assert_eq!(p.registry_metadata["supports_list_changed"], true);
    // The value exists before any server was contacted — it can never
    // reflect tested behavior.
}

/// The profile's version list is stale relative to the wire: the real
/// client (`susi_core::mcp_client`) speaks 2025-11-25 and 2025-06-18, yet
/// the catalog carries neither — so "negotiation" against a real current
/// peer reports no common revision.
#[test]
fn vc_201_086_mastery_catalog_versions_miss_the_current_wire() {
    let mut p = McpSpecProfile::default_profile();
    assert_eq!(
        p.negotiate(&["2025-11-25"]),
        None,
        "catalog knows only 2024-11-05 / 2025-03-26 — a peer speaking the\n\
         current wire revision negotiates to nothing"
    );
}

/// The probe surface the claim requires — initialization, discovery,
/// cancellation, timeouts, malformed responses, credential isolation — has
/// no representation: the profile type carries exactly versions, negotiated
/// and registry_metadata. No probe function exists in the crate.
#[test]
fn vc_201_086_mastery_no_probe_surface_exists() {
    let p = McpSpecProfile::default_profile();
    let json = serde_json::to_value(&p).unwrap();
    let keys: std::collections::BTreeSet<String> =
        json.as_object().unwrap().keys().cloned().collect();
    assert_eq!(
        keys,
        ["versions", "negotiated", "registry_metadata"]
            .into_iter()
            .map(String::from)
            .collect()
    );
}

/// `negotiate` is a pure list intersection — no initialize request is
/// issued, no timeout applies, malformed input is impossible to represent.
/// It "succeeds" over a peer that was never contacted.
#[test]
fn vc_201_086_mastery_negotiate_is_pure_list_intersection() {
    let mut p = McpSpecProfile::default_profile();
    // A fabricated peer list — nothing was contacted — still "negotiates".
    assert_eq!(p.negotiate(&["2024-11-05"]), Some("2024-11-05".into()));
    assert_eq!(p.negotiated.as_deref(), Some("2024-11-05"));
}

/// Holds: newest common revision wins when the catalog actually overlaps.
#[test]
fn vc_201_086_mastery_newest_common_wins_holds() {
    let mut p = McpSpecProfile::default_profile();
    assert_eq!(
        p.negotiate(&["2024-11-05", "2025-03-26"]),
        Some("2025-03-26".into())
    );
}
