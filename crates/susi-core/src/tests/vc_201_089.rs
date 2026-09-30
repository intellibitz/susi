use crate::adapter_upgrade::{AdapterRevision, AdapterUpgrade, SwitchVerdict};

#[test]
fn vc_201_089_failed_upgrade_preserves_active_and_sessions() {
    let mut u = AdapterUpgrade::new();
    u.install_active(AdapterRevision {
        id: "v1".into(),
        version: 1,
    });
    u.bind_session("s1");
    u.stage(AdapterRevision {
        id: "v2".into(),
        version: 2,
    });
    assert_eq!(
        u.switch_if_contracts_pass(false),
        SwitchVerdict::PreservedActive
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
    });
    u.stage(AdapterRevision {
        id: "v2".into(),
        version: 2,
    });
    assert_eq!(u.switch_if_contracts_pass(true), SwitchVerdict::Switched);
    assert_eq!(u.active().map(|a| a.id.as_str()), Some("v2"));
}
