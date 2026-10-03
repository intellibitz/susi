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
    });
    i
}

/// Cross-tenant workspace isolation is a lexical `starts_with` — a sibling
/// directory sharing the prefix (`/t/alpha-evil`) reads as inside alpha's
/// workspace. Another tenant's tree is reachable without crossing a
/// boundary.
#[test]
fn vc_201_078_mastery_sibling_prefix_escape() {
    let i = iso();
    assert_eq!(
        i.read_path("alpha", "/t/alpha-evil/secrets"),
        AccessVerdict::Allowed,
        "lexical prefix admits a foreign sibling workspace"
    );
}

/// `..` never normalizes under `starts_with` — `/t/alpha/../beta` resolves
/// to a foreign workspace yet is Allowed.
#[test]
fn vc_201_078_mastery_dotdot_escape() {
    let i = iso();
    assert_eq!(
        i.read_path("alpha", "/t/alpha/../beta/secrets"),
        AccessVerdict::Allowed,
        "dotdot inside the prefix escapes the workspace"
    );
}

/// `register` silently replaces an existing tenant's context — any caller
/// can rebind `alpha` to a different root/grants/quota with no ownership or
/// lease check. Identity is mutable by overwrite.
#[test]
fn vc_201_078_mastery_reregister_swaps_identity_silently() {
    let mut i = iso();
    i.register(TenantContext {
        tenant_id: "alpha".into(),
        workspace_root: "/t/beta".into(),
        memory_ns: "mem-beta".into(),
        tool_grants: ["admin".into()].into_iter().collect(),
        quota: TenantQuota {
            max_queue: 999,
            budget_usd_cents: u64::MAX,
        },
    });
    assert_eq!(i.read_path("alpha", "/t/beta/x"), AccessVerdict::Allowed);
    assert_eq!(i.use_tool("alpha", "admin"), AccessVerdict::Allowed);
}

/// The queue never drains: `enqueue` only increments — there is no
/// dequeue/complete — so `max_queue` is a permanent, monotone wedge, not a
/// live concurrency bound.
#[test]
fn vc_201_078_mastery_queue_never_drains() {
    let mut i = iso();
    assert_eq!(i.enqueue("alpha"), AccessVerdict::Allowed);
    assert_eq!(i.enqueue("alpha"), AccessVerdict::Allowed);
    assert_eq!(i.enqueue("alpha"), AccessVerdict::QuotaExhausted);
    // No API exists to release the slots — "alpha" is wedged forever.
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
