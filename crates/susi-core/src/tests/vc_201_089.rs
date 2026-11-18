use crate::adapter_upgrade::{AdapterRevision, AdapterUpgrade, ContractResults, SwitchVerdict};

fn passing() -> ContractResults {
    ContractResults {
        checks: vec![("compat".into(), true)],
    }
}

fn failing() -> ContractResults {
    ContractResults {
        checks: vec![("compat".into(), false)],
    }
}

#[test]
fn vc_201_089_failed_upgrade_preserves_active_and_sessions() {
    let mut u = AdapterUpgrade::new();
    u.install_active(AdapterRevision {
        id: "v1".into(),
        version: 1,
    })
    .unwrap();
    u.bind_session("s1");
    u.stage(AdapterRevision {
        id: "v2".into(),
        version: 2,
    });
    assert_eq!(
        u.switch_if_contracts_pass(&failing()),
        SwitchVerdict::RejectedContractFailure
    );
    assert_eq!(u.active().map(|a| a.id.as_str()), Some("v1"));
    assert!(u.staged().is_none());
    assert_eq!(u.session_owner("s1"), Some("v1"));
}

#[test]
fn vc_201_089_switch_after_contract_success() {
    let mut u = AdapterUpgrade::new();
    u.install_active(AdapterRevision {
        id: "v1".into(),
        version: 1,
    })
    .unwrap();
    u.stage(AdapterRevision {
        id: "v2".into(),
        version: 2,
    });
    assert_eq!(
        u.switch_if_contracts_pass(&passing()),
        SwitchVerdict::Switched
    );
    assert_eq!(u.active().map(|a| a.id.as_str()), Some("v2"));
}

#[test]
fn vc_201_089_rejects_no_candidate_staged_empty_contracts_and_downgrade_versions() {
    let mut u = AdapterUpgrade::new();
    u.install_active(AdapterRevision {
        id: "v1".into(),
        version: 1,
    })
    .unwrap();

    assert_eq!(
        u.switch_if_contracts_pass(&passing()),
        SwitchVerdict::NoCandidateStaged
    );

    u.stage(AdapterRevision {
        id: "v2".into(),
        version: 2,
    });
    assert_eq!(
        u.switch_if_contracts_pass(&ContractResults::default()),
        SwitchVerdict::RejectedContractFailure
    );

    for bad_version in [1, 0] {
        u.stage(AdapterRevision {
            id: format!("v{bad_version}"),
            version: bad_version,
        });
        assert_eq!(
            u.switch_if_contracts_pass(&passing()),
            SwitchVerdict::RejectedDowngrade
        );
    }
    assert_eq!(u.active().map(|a| a.version), Some(1));
}

#[test]
fn vc_201_089_install_active_refuses_to_overwrite_existing_active_revision() {
    let mut u = AdapterUpgrade::new();
    u.install_active(AdapterRevision {
        id: "a1".into(),
        version: 1,
    })
    .unwrap();
    let err = u
        .install_active(AdapterRevision {
            id: "a2".into(),
            version: 2,
        })
        .expect_err("install_active must refuse overwriting active revision");
    assert!(err.contains("already installed"));
    assert_eq!(u.active().map(|a| a.id.as_str()), Some("a1"));
}

#[test]
fn vc_201_089_fresh_upgrade_has_no_active_or_staged_revision_and_unbound_session() {
    let u = AdapterUpgrade::new();
    assert!(u.active().is_none());
    assert!(u.staged().is_none());
    assert!(u.session_owner("unknown-session").is_none());
}
