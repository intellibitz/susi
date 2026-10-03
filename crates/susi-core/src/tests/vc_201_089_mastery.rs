//! Mastery verification for VC-201-089: sandbox and roll back adapter
//! upgrades.
//!
//! The cited tests show a staged candidate switching in on success and the
//! active revision surviving failure. The distinguishing properties are
//! that the contract checks are real (not a caller-supplied flag), the
//! sandbox cannot be bypassed, and an upgrade is actually an upgrade.

use crate::adapter_upgrade::{AdapterRevision, AdapterUpgrade, SwitchVerdict};

fn rev(id: &str, version: u32) -> AdapterRevision {
    AdapterRevision {
        id: id.into(),
        version,
    }
}

/// Falsification: "run contract checks" never happens — the switch verdict
/// depends entirely on a caller-supplied boolean. Nothing executes
/// contracts, so 'switch only after success' is 'switch when told true'.
/// The same staged revision switches or preserves purely on the flag.
#[test]
fn vc_201_089_mastery_contract_checks_are_a_caller_supplied_flag() {
    let mut u = AdapterUpgrade::new();
    u.install_active(rev("v1", 1));
    u.stage(rev("v2", 2));
    // The caller just asserts contracts passed — none ran.
    assert_eq!(u.switch_if_contracts_pass(true), SwitchVerdict::Switched);
    assert_eq!(u.active().map(|a| a.id.as_str()), Some("v2"));
}

/// Falsification: the sandbox is bypassable. `install_active` installs any
/// revision directly as live with no staging, no contract path, no
/// rollback point — so "install beside the active one and switch only
/// after success" is not the only way a revision becomes active.
#[test]
fn vc_201_089_mastery_install_active_bypasses_the_sandbox() {
    let mut u = AdapterUpgrade::new();
    u.install_active(rev("v1", 1));
    u.bind_session("s1");
    // An unchecked revision replaces the live one outright.
    u.install_active(rev("hostile", 99));
    assert_eq!(u.active().map(|a| a.id.as_str()), Some("hostile"));
}

/// Falsification: nothing enforces that an upgrade is newer. A *downgrade*
/// — staging an older revision — switches in just the same. The version
/// field is carried but never consulted.
#[test]
fn vc_201_089_mastery_downgrade_switches_in_as_an_upgrade() {
    let mut u = AdapterUpgrade::new();
    u.install_active(rev("v2", 2));
    u.stage(rev("v1", 1));
    assert_eq!(u.switch_if_contracts_pass(true), SwitchVerdict::Switched);
    assert_eq!(
        u.active().map(|a| a.version),
        Some(1),
        "an older revision replaced the newer active one"
    );
}

/// Falsification: switch with contracts passing but nothing staged quietly
/// reports PreservedActive — indistinguishable from a contract failure,
/// so an operator cannot tell "no candidate" from "checks failed".
#[test]
fn vc_201_089_mastery_switch_with_nothing_staged_is_indistinguishable() {
    let mut u = AdapterUpgrade::new();
    u.install_active(rev("v1", 1));
    assert_eq!(
        u.switch_if_contracts_pass(true),
        SwitchVerdict::PreservedActive,
        "no candidate staged yet the verdict mirrors contract failure"
    );
}

/// What does hold: a failed contract flag preserves the active revision,
/// clears the staged candidate, and keeps in-flight session ownership.
#[test]
fn vc_201_089_mastery_failed_flag_preserves_active_and_sessions() {
    let mut u = AdapterUpgrade::new();
    u.install_active(rev("v1", 1));
    u.bind_session("s1");
    u.stage(rev("v2", 2));
    assert_eq!(
        u.switch_if_contracts_pass(false),
        SwitchVerdict::PreservedActive
    );
    assert_eq!(u.active().map(|a| a.id.as_str()), Some("v1"));
    assert!(u.staged().is_none());
    assert_eq!(u.session_owner("s1"), Some("v1"));
}
