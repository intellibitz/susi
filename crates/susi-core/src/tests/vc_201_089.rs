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
