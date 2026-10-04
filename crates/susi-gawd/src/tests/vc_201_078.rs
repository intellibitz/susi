use crate::tenant_isolation::{AccessVerdict, TenantContext, TenantIsolation, TenantQuota};
use std::collections::BTreeSet;

#[test]
fn vc_201_078_cross_tenant_path_and_memory_denied() {
    let mut iso = TenantIsolation::new();
    iso.register(TenantContext {
        tenant_id: "t1".into(),
        workspace_root: "/ws/t1".into(),
        memory_ns: "ns-t1".into(),
        tool_grants: BTreeSet::from(["exec".into()]),
        quota: TenantQuota {
            max_queue: 2,
            budget_usd_cents: 100,
        },
    })
    .unwrap();
    iso.register(TenantContext {
        tenant_id: "t2".into(),
        workspace_root: "/ws/t2".into(),
        memory_ns: "ns-t2".into(),
        tool_grants: BTreeSet::from(["net".into()]),
        quota: TenantQuota {
            max_queue: 2,
            budget_usd_cents: 100,
        },
    })
    .unwrap();
    assert_eq!(iso.read_path("t1", "/ws/t1/a.rs"), AccessVerdict::Allowed);
    assert_eq!(
        iso.read_path("t1", "/ws/t2/secret"),
        AccessVerdict::CrossTenantDenied
    );
    assert_eq!(
        iso.read_memory("t1", "ns-t2"),
        AccessVerdict::CrossTenantDenied
    );
    assert_eq!(iso.use_tool("t1", "net"), AccessVerdict::ToolDenied);
}

#[test]
fn vc_201_078_quota_exhaustion_boundary() {
    let mut iso = TenantIsolation::new();
    iso.register(TenantContext {
        tenant_id: "t1".into(),
        workspace_root: "/ws/t1".into(),
        memory_ns: "ns".into(),
        tool_grants: BTreeSet::new(),
        quota: TenantQuota {
            max_queue: 1,
            budget_usd_cents: 50,
        },
    })
    .unwrap();
    assert_eq!(iso.enqueue("t1"), AccessVerdict::Allowed);
    assert_eq!(iso.enqueue("t1"), AccessVerdict::QuotaExhausted);
    assert_eq!(iso.charge("t1", 40), AccessVerdict::Allowed);
    assert_eq!(iso.charge("t1", 20), AccessVerdict::QuotaExhausted);
}
