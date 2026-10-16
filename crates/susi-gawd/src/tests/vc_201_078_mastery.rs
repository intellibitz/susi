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

/// Mastery target: production caller integration in GAWD plane handler wires
/// workspace boundary enforcement for tenant requests, rejecting sibling-prefix,
/// dotdot, and foreign workspace escapes, and preventing unauthenticated access.
#[test]
fn vc_201_078_mastery_plane_handler_tenant_solve_workspace_boundary() {
    let tenant_id = "tenant_solve_ws";
    let token = "secret_solve_token";
    crate::tenant_isolation::with_global(|iso| {
        iso.unregister(tenant_id);
        iso.register_with_token(
            TenantContext {
                tenant_id: tenant_id.into(),
                workspace_root: "/t/isolated".into(),
                memory_ns: "mem-iso".into(),
                tool_grants: ["read".into()].into_iter().collect(),
                quota: TenantQuota {
                    max_queue: 2,
                    budget_usd_cents: 100,
                },
            },
            token,
        )
        .unwrap();
    });

    // Foreign workspace is rejected by GAWD_SOLVE even with valid token
    let foreign = crate::plane_handler::handle_topic(
        susi_core::plane_bus::topics::GAWD_SOLVE,
        serde_json::json!({
            "intent": "test mission",
            "workspace": "/t/foreign",
            "tenant_id": tenant_id,
            "auth_token": token,
        }),
    )
    .unwrap();
    let err = foreign.get("error").and_then(|v| v.as_str()).unwrap_or("");
    assert!(
        err.contains("cross-tenant workspace access denied"),
        "expected cross-tenant denial, got: {err}"
    );

    // Sibling prefix escape (/t/isolated-evil) is rejected
    let sibling = crate::plane_handler::handle_topic(
        susi_core::plane_bus::topics::GAWD_SOLVE,
        serde_json::json!({
            "intent": "test mission",
            "workspace": "/t/isolated-evil/sub",
            "tenant_id": tenant_id,
            "auth_token": token,
        }),
    )
    .unwrap();
    let err = sibling.get("error").and_then(|v| v.as_str()).unwrap_or("");
    assert!(
        err.contains("cross-tenant workspace access denied"),
        "expected sibling prefix denial, got: {err}"
    );

    // Dotdot escape (/t/isolated/../other) is rejected
    let dotdot = crate::plane_handler::handle_topic(
        susi_core::plane_bus::topics::GAWD_SOLVE,
        serde_json::json!({
            "intent": "test mission",
            "workspace": "/t/isolated/../other",
            "tenant_id": tenant_id,
            "auth_token": token,
        }),
    )
    .unwrap();
    let err = dotdot.get("error").and_then(|v| v.as_str()).unwrap_or("");
    assert!(
        err.contains("cross-tenant workspace access denied"),
        "expected dotdot escape denial, got: {err}"
    );

    // Accessing tenant workspace without tenant_id or auth_token is denied
    let unauth = crate::plane_handler::handle_topic(
        susi_core::plane_bus::topics::GAWD_SOLVE,
        serde_json::json!({
            "intent": "test mission",
            "workspace": "/t/isolated/secrets",
        }),
    )
    .unwrap();
    let err = unauth.get("error").and_then(|v| v.as_str()).unwrap_or("");
    assert!(
        err.contains("workspace is tenant-isolated"),
        "expected tenant isolation denial, got: {err}"
    );

    // Supplying wrong auth_token is denied
    let wrong_token = crate::plane_handler::handle_topic(
        susi_core::plane_bus::topics::GAWD_SOLVE,
        serde_json::json!({
            "intent": "test mission",
            "workspace": "/t/isolated",
            "tenant_id": tenant_id,
            "auth_token": "wrong_password",
        }),
    )
    .unwrap();
    let err = wrong_token
        .get("error")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(
        err.contains("tenant authentication failed"),
        "expected auth failure, got: {err}"
    );
}

/// Mastery target: production caller integration in GAWD plane handler wires
/// tenant queue concurrency limits, rejecting requests when max_queue is reached.
#[test]
fn vc_201_078_mastery_plane_handler_tenant_solve_queue_concurrency() {
    let tenant_id = "tenant_solve_queue";
    let token = "secret_queue_token";
    crate::tenant_isolation::with_global(|iso| {
        iso.unregister(tenant_id);
        iso.register_with_token(
            TenantContext {
                tenant_id: tenant_id.into(),
                workspace_root: "/t/queue_ws".into(),
                memory_ns: "mem-queue".into(),
                tool_grants: BTreeSet::new(),
                quota: TenantQuota {
                    max_queue: 1,
                    budget_usd_cents: 50,
                },
            },
            token,
        )
        .unwrap();
        // Fill the slot
        assert_eq!(iso.enqueue(tenant_id), AccessVerdict::Allowed);
    });

    // Next solve request with tenant_id must be rejected for quota exhausted
    let rejected = crate::plane_handler::handle_topic(
        susi_core::plane_bus::topics::GAWD_SOLVE,
        serde_json::json!({
            "intent": "test mission",
            "workspace": "/t/queue_ws",
            "tenant_id": tenant_id,
            "auth_token": token,
        }),
    )
    .unwrap();
    let err = rejected.get("error").and_then(|v| v.as_str()).unwrap_or("");
    assert!(
        err.contains("tenant queue quota exhausted"),
        "expected queue quota exhaustion, got: {err}"
    );

    // Drain the slot
    crate::tenant_isolation::with_global(|iso| {
        assert_eq!(iso.dequeue(tenant_id), AccessVerdict::Allowed);
    });
}

/// Mastery target: production caller integration in GAWD plane handler wires
/// tenant tool grants into audit actions, enforcing tool grant boundaries.
#[test]
fn vc_201_078_mastery_plane_handler_tenant_tool_grant_audit() {
    let tenant_id = "tenant_tool_grant";
    let token = "secret_tool_token";
    crate::tenant_isolation::with_global(|iso| {
        iso.unregister(tenant_id);
        iso.register_with_token(
            TenantContext {
                tenant_id: tenant_id.into(),
                workspace_root: "/t/tool_ws".into(),
                memory_ns: "mem-tool".into(),
                tool_grants: ["permitted_reader".into()].into_iter().collect(),
                quota: TenantQuota {
                    max_queue: 5,
                    budget_usd_cents: 100,
                },
            },
            token,
        )
        .unwrap();
    });

    // Ungranted tool is denied by GAWD_AUDIT_ACTION
    let ungranted = crate::plane_handler::handle_topic(
        susi_core::plane_bus::topics::GAWD_AUDIT_ACTION,
        serde_json::json!({
            "tool": "unauthorized_exec",
            "detail": "cargo run",
            "workspace": "/t/tool_ws",
            "tenant_id": tenant_id,
            "auth_token": token,
        }),
    )
    .unwrap();
    let err = ungranted
        .get("error")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(
        err.contains("tenant tool grant denied"),
        "expected tool grant denial, got: {err}"
    );

    // Cross-tenant workspace audit action is denied
    let foreign_audit = crate::plane_handler::handle_topic(
        susi_core::plane_bus::topics::GAWD_AUDIT_ACTION,
        serde_json::json!({
            "tool": "permitted_reader",
            "detail": "read secret",
            "workspace": "/foreign/ws",
            "tenant_id": tenant_id,
            "auth_token": token,
        }),
    )
    .unwrap();
    let err = foreign_audit
        .get("error")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(
        err.contains("cross-tenant audit workspace access denied"),
        "expected workspace denial, got: {err}"
    );
}

/// Mastery target: plane bus exposes gawd.tenant.* topics for lifecycle,
/// memory namespace isolation, tool grants, queue operations, and budget charging
/// with explicit authorization and unregister protection.
#[test]
fn vc_201_078_mastery_plane_bus_tenant_topics_lifecycle_and_boundaries() {
    let tenant_id = "tenant_bus_lifecycle";
    let token = "bus_secret_pass";

    // 1. Register via plane bus with auth_token
    let reg_res = crate::plane_handler::handle_topic(
        "gawd.tenant.register",
        serde_json::json!({
            "tenant_id": tenant_id,
            "auth_token": token,
            "workspace_root": "/t/bus_ws",
            "memory_ns": "ns-bus-alpha",
            "tool_grants": ["tool_a"],
            "quota": {
                "max_queue": 2,
                "budget_usd_cents": 75,
            }
        }),
    )
    .unwrap();
    assert_eq!(reg_res.get("ok").and_then(|v| v.as_bool()), Some(true));

    // 2. Re-registering existing tenant fails
    let re_reg = crate::plane_handler::handle_topic(
        "gawd.tenant.register",
        serde_json::json!({
            "tenant_id": tenant_id,
            "auth_token": token,
            "workspace_root": "/t/bus_ws_other",
            "memory_ns": "ns-bus-beta",
            "tool_grants": [],
            "quota": {
                "max_queue": 1,
                "budget_usd_cents": 10,
            }
        }),
    )
    .unwrap();
    assert!(re_reg.get("error").is_some());

    // 3. Memory namespace isolation requires auth_token
    let mem_unauth = crate::plane_handler::handle_topic(
        "gawd.tenant.read_memory",
        serde_json::json!({ "tenant_id": tenant_id, "ns": "ns-bus-alpha" }),
    )
    .unwrap();
    assert!(mem_unauth.get("error").is_some());

    let mem_own = crate::plane_handler::handle_topic(
        "gawd.tenant.read_memory",
        serde_json::json!({ "tenant_id": tenant_id, "auth_token": token, "ns": "ns-bus-alpha" }),
    )
    .unwrap();
    assert_eq!(mem_own.get("allowed").and_then(|v| v.as_bool()), Some(true));

    let mem_foreign = crate::plane_handler::handle_topic(
        "gawd.tenant.read_memory",
        serde_json::json!({ "tenant_id": tenant_id, "auth_token": token, "ns": "ns-bus-foreign" }),
    )
    .unwrap();
    assert_eq!(
        mem_foreign.get("allowed").and_then(|v| v.as_bool()),
        Some(false)
    );
    assert_eq!(
        mem_foreign.get("verdict").and_then(|v| v.as_str()),
        Some("CrossTenantDenied")
    );

    // 4. Tool grant boundary
    let tool_allowed = crate::plane_handler::handle_topic(
        "gawd.tenant.use_tool",
        serde_json::json!({ "tenant_id": tenant_id, "tool": "tool_a" }),
    )
    .unwrap();
    assert_eq!(
        tool_allowed.get("allowed").and_then(|v| v.as_bool()),
        Some(true)
    );

    let tool_denied = crate::plane_handler::handle_topic(
        "gawd.tenant.use_tool",
        serde_json::json!({ "tenant_id": tenant_id, "tool": "tool_b" }),
    )
    .unwrap();
    assert_eq!(
        tool_denied.get("allowed").and_then(|v| v.as_bool()),
        Some(false)
    );

    // 5. Budget charge requires auth_token and saturates
    let charge_unauth = crate::plane_handler::handle_topic(
        "gawd.tenant.charge",
        serde_json::json!({ "tenant_id": tenant_id, "cents": 50 }),
    )
    .unwrap();
    assert!(charge_unauth.get("error").is_some());

    let charge_ok = crate::plane_handler::handle_topic(
        "gawd.tenant.charge",
        serde_json::json!({ "tenant_id": tenant_id, "auth_token": token, "cents": 50 }),
    )
    .unwrap();
    assert_eq!(
        charge_ok.get("allowed").and_then(|v| v.as_bool()),
        Some(true)
    );

    let charge_exceeded = crate::plane_handler::handle_topic(
        "gawd.tenant.charge",
        serde_json::json!({ "tenant_id": tenant_id, "auth_token": token, "cents": 30 }),
    )
    .unwrap();
    assert_eq!(
        charge_exceeded.get("allowed").and_then(|v| v.as_bool()),
        Some(false)
    );
    assert_eq!(
        charge_exceeded.get("verdict").and_then(|v| v.as_str()),
        Some("QuotaExhausted")
    );

    // 6. Status inspection
    let status = crate::plane_handler::handle_topic(
        "gawd.tenant.status",
        serde_json::json!({ "tenant_id": tenant_id }),
    )
    .unwrap();
    assert_eq!(
        status.get("registered").and_then(|v| v.as_bool()),
        Some(true)
    );
    assert_eq!(status.get("spent_cents").and_then(|v| v.as_u64()), Some(50));

    // 7. Unregister without token or wrong token is rejected
    let unreg_bad = crate::plane_handler::handle_topic(
        "gawd.tenant.unregister",
        serde_json::json!({ "tenant_id": tenant_id, "auth_token": "wrong" }),
    )
    .unwrap();
    assert!(unreg_bad.get("error").is_some());

    // 8. Unregister with valid auth_token succeeds and clears state
    let unreg_ok = crate::plane_handler::handle_topic(
        "gawd.tenant.unregister",
        serde_json::json!({ "tenant_id": tenant_id, "auth_token": token }),
    )
    .unwrap();
    assert_eq!(unreg_ok.get("ok").and_then(|v| v.as_bool()), Some(true));

    let status_after = crate::plane_handler::handle_topic(
        "gawd.tenant.status",
        serde_json::json!({ "tenant_id": tenant_id }),
    )
    .unwrap();
    assert_eq!(
        status_after.get("registered").and_then(|v| v.as_bool()),
        Some(false)
    );
}
