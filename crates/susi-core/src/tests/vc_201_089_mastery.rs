//! Mastery verification for VC-201-089: sandbox and roll back adapter
//! upgrades.
//!
//! The cited tests show a staged candidate switching in on success and the
//! active revision surviving failure. The distinguishing properties are
//! that the contract checks are real (not a caller-supplied flag), the
//! sandbox cannot be bypassed, and an upgrade is actually an upgrade.

use crate::adapter_upgrade::{AdapterRevision, AdapterUpgrade, ContractResults, SwitchVerdict};

fn rev(id: &str, version: u32) -> AdapterRevision {
    AdapterRevision {
        id: id.into(),
        version,
    }
}

fn passing() -> ContractResults {
    ContractResults {
        checks: vec![("compat".into(), true), ("abi".into(), true)],
    }
}

/// Fixed: `switch_if_contracts_pass` now takes real named contract-check
/// results, not a caller-supplied boolean — an empty result set is not
/// evidence of anything passing and is refused.
#[test]
fn vc_201_089_mastery_contract_checks_are_a_caller_supplied_flag() {
    let mut u = AdapterUpgrade::new();
    u.install_active(rev("v1", 1)).unwrap();
    u.stage(rev("v2", 2));
    assert_eq!(
        u.switch_if_contracts_pass(&ContractResults::default()),
        SwitchVerdict::RejectedContractFailure,
        "an empty contract-result set must not pass vacuously"
    );
    u.stage(rev("v2", 2));
    assert_eq!(
        u.switch_if_contracts_pass(&passing()),
        SwitchVerdict::Switched
    );
    assert_eq!(u.active().map(|a| a.id.as_str()), Some("v2"));
}

/// Fixed: `install_active` now refuses to replace an already-installed
/// active revision — an unchecked revision can no longer bypass stage +
/// contracts + rollback to become live.
#[test]
fn vc_201_089_mastery_install_active_bypasses_the_sandbox() {
    let mut u = AdapterUpgrade::new();
    u.install_active(rev("v1", 1)).unwrap();
    u.bind_session("s1");
    let err = u
        .install_active(rev("hostile", 99))
        .expect_err("must refuse to replace a live active revision directly");
    assert!(err.contains("v1"), "{err}");
    assert_eq!(
        u.active().map(|a| a.id.as_str()),
        Some("v1"),
        "the unchecked revision must not have replaced the live one"
    );
}

/// Fixed: the staged revision's version is now consulted — a downgrade
/// is refused with its own verdict instead of switching in as if it were
/// an upgrade.
#[test]
fn vc_201_089_mastery_downgrade_switches_in_as_an_upgrade() {
    let mut u = AdapterUpgrade::new();
    u.install_active(rev("v2", 2)).unwrap();
    u.stage(rev("v1", 1));
    assert_eq!(
        u.switch_if_contracts_pass(&passing()),
        SwitchVerdict::RejectedDowngrade,
        "a staged revision older than the active one must be refused"
    );
    assert_eq!(
        u.active().map(|a| a.version),
        Some(2),
        "the newer active revision must survive a downgrade attempt"
    );
}

/// Fixed: switching with nothing staged now has its own verdict, distinct
/// from a contract failure.
#[test]
fn vc_201_089_mastery_switch_with_nothing_staged_is_indistinguishable() {
    let mut u = AdapterUpgrade::new();
    u.install_active(rev("v1", 1)).unwrap();
    assert_eq!(
        u.switch_if_contracts_pass(&passing()),
        SwitchVerdict::NoCandidateStaged,
        "no candidate staged must be distinguishable from a contract failure"
    );
}

/// What does hold: a failed contract result preserves the active
/// revision, clears the staged candidate, and keeps in-flight session
/// ownership.
#[test]
fn vc_201_089_mastery_failed_flag_preserves_active_and_sessions() {
    let mut u = AdapterUpgrade::new();
    u.install_active(rev("v1", 1)).unwrap();
    u.bind_session("s1");
    u.stage(rev("v2", 2));
    let failing = ContractResults {
        checks: vec![("compat".into(), false)],
    };
    assert_eq!(
        u.switch_if_contracts_pass(&failing),
        SwitchVerdict::RejectedContractFailure
    );
    assert_eq!(u.active().map(|a| a.id.as_str()), Some("v1"));
    assert!(u.staged().is_none());
    assert_eq!(u.session_owner("s1"), Some("v1"));
}
