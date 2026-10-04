//! Mastery checks for VC-201-078: enforce per-tenant workspace paths,
//! memory namespaces, tool grants, queues, and budgets.
//!
//! Every test name starts `vc_201_078_mastery_` so the vector's evidence is
//! enumerable with `cargo test -p susi-gawd vc_201_078`.

use crate::tenant_isolation::{AccessVerdict, TenantContext, TenantIsolation, TenantQuota};
use std::collections::BTreeSet;

fn iso() -> TenantIsolation {
    let mut i = TenantIsolation::new();
    i.register(TenantContext {
        tenant_id: "alpha".into(),
        workspace_root: "/t/alpha".into(),
        memory_ns: "mem-alpha".into(),
        tool_grants: ["read".into()].into_iter().collect::<BTreeSet<_>>(),
        quota: TenantQuota {
            max_queue: 2,
            budget_usd_cents: 100,
        },
    })
    .unwrap();
    i
}

/// Fixed: cross-tenant workspace isolation now resolves the path and
/// requires an exact match or a `/`-bounded nesting under the root, so a
/// sibling directory that merely shares the string prefix
/// (`/t/alpha-evil`) is no longer admitted as inside `/t/alpha`.
#[test]
fn vc_201_078_mastery_sibling_prefix_escape() {
    let i = iso();
    assert_eq!(
        i.read_path("alpha", "/t/alpha-evil/secrets"),
        AccessVerdict::CrossTenantDenied,
        "a sibling sharing only a string prefix must not read as inside the workspace"
    );
}

/// Fixed: the path is resolved lexically (`.`/`..` collapsed) before the
/// boundary check, so `/t/alpha/../beta/secrets` — which resolves to a
/// foreign workspace — is denied instead of matching the raw prefix.
#[test]
fn vc_201_078_mastery_dotdot_escape() {
    let i = iso();
    assert_eq!(
        i.read_path("alpha", "/t/alpha/../beta/secrets"),
        AccessVerdict::CrossTenantDenied,
        "a dotdot that resolves outside the workspace must be denied"
    );
}

/// Fixed: `register` now refuses to rebind an already-registered
/// tenant_id — any caller swapping another tenant's root/grants/quota by
/// re-registering is rejected, not silently applied.
#[test]
fn vc_201_078_mastery_reregister_swaps_identity_silently() {
    let mut i = iso();
    let err = i
        .register(TenantContext {
            tenant_id: "alpha".into(),
            workspace_root: "/t/beta".into(),
            memory_ns: "mem-beta".into(),
            tool_grants: ["admin".into()].into_iter().collect(),
            quota: TenantQuota {
                max_queue: 999,
                budget_usd_cents: u64::MAX,
            },
        })
        .expect_err("re-registering an existing tenant_id must be refused");
    assert!(err.contains("alpha"), "{err}");
    // The original identity is untouched.
    assert_eq!(
        i.read_path("alpha", "/t/beta/x"),
        AccessVerdict::CrossTenantDenied
    );
    assert_eq!(i.use_tool("alpha", "admin"), AccessVerdict::ToolDenied);
}

/// Fixed: `dequeue` releases a slot, so `max_queue` is a live bound rather
/// than a permanent, monotone wedge.
#[test]
fn vc_201_078_mastery_queue_never_drains() {
    let mut i = iso();
    assert_eq!(i.enqueue("alpha"), AccessVerdict::Allowed);
    assert_eq!(i.enqueue("alpha"), AccessVerdict::Allowed);
    assert_eq!(i.enqueue("alpha"), AccessVerdict::QuotaExhausted);
    assert_eq!(i.dequeue("alpha"), AccessVerdict::Allowed);
    assert_eq!(
        i.enqueue("alpha"),
        AccessVerdict::Allowed,
        "a released slot must be reusable"
    );
}

/// Holds: foreign namespace reads are denied.
#[test]
fn vc_201_078_mastery_foreign_memory_ns_denied_holds() {
    let i = iso();
    assert_eq!(
        i.read_memory("alpha", "mem-beta"),
        AccessVerdict::CrossTenantDenied
    );
}

/// Holds: unknown tenants are denied everywhere.
#[test]
fn vc_201_078_mastery_unknown_tenant_denied_holds() {
    let mut i = iso();
    assert_eq!(
        i.read_path("ghost", "/t/alpha"),
        AccessVerdict::CrossTenantDenied
    );
    assert_eq!(i.enqueue("ghost"), AccessVerdict::CrossTenantDenied);
}

/// Holds: ungranted tools are denied; budget overflow saturates.
#[test]
fn vc_201_078_mastery_tool_and_budget_bounds_hold() {
    let mut i = iso();
    assert_eq!(i.use_tool("alpha", "admin"), AccessVerdict::ToolDenied);
    assert_eq!(i.charge("alpha", 150), AccessVerdict::QuotaExhausted);
    assert_eq!(i.charge("alpha", 100), AccessVerdict::Allowed);
    assert_eq!(i.charge("alpha", 1), AccessVerdict::QuotaExhausted);
}

/// New: `unregister` is the sanctioned way to free a tenant_id — after it,
/// re-registering with a new context succeeds and the tenant's prior queue
/// and spend do not leak into the new registration.
#[test]
fn vc_201_078_mastery_unregister_then_reregister_is_clean() {
    let mut i = iso();
    assert_eq!(i.enqueue("alpha"), AccessVerdict::Allowed);
    assert_eq!(i.charge("alpha", 100), AccessVerdict::Allowed);
    i.unregister("alpha");
    i.register(TenantContext {
        tenant_id: "alpha".into(),
        workspace_root: "/t/alpha2".into(),
        memory_ns: "mem-alpha2".into(),
        tool_grants: BTreeSet::new(),
        quota: TenantQuota {
            max_queue: 1,
            budget_usd_cents: 10,
        },
    })
    .unwrap();
    assert_eq!(i.enqueue("alpha"), AccessVerdict::Allowed);
    assert_eq!(
        i.charge("alpha", 10),
        AccessVerdict::Allowed,
        "old spend must not carry over to the fresh registration"
    );
}
