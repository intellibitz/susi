#![allow(missing_docs)] // integration test crate: no public API to document
#![allow(clippy::expect_used)] // the fixture catalog is intentionally checked in

use serde_json::json;
use susi_core::capability_bus::{
    CapabilityBus, CapabilityDescriptor, CapabilityError, CapabilityInvocation, CapabilityRequest,
    CapabilitySurface,
};
use susi_core::registry::{CapabilityRegistry, Tool};

#[test]
fn capability_bus() {
    let mut bus = CapabilityBus::new();
    for (id, surface) in [
        ("a2a.run", CapabilitySurface::A2a),
        ("mcp.tool", CapabilitySurface::Mcp),
        ("http.fetch", CapabilitySurface::Http),
        ("adapter.embed", CapabilitySurface::Adapter),
    ] {
        bus.register(CapabilityDescriptor::new(
            id,
            surface,
            "invoke",
            "invoke:capability",
            100,
            50,
        ))
        .expect("fixture descriptor must be valid");
    }
    let request = CapabilityRequest::new(
        "http.fetch",
        "mission-7",
        ["invoke:capability".to_string()],
        80,
        40,
    );
    let receipt = bus
        .dispatch(&request, |_| {
            Ok(CapabilityInvocation {
                output: json!({"status": "ok"}),
                elapsed_ms: 20,
                cost_micros: 30,
            })
        })
        .expect("authorized capability must dispatch");
    assert_eq!(receipt.surface, CapabilitySurface::Http);
    assert_eq!(receipt.subject, "mission-7");
    assert_eq!(receipt.cost_micros, 30);
    assert_eq!(bus.discover().len(), 4);
}

/// VC-202-012 mastery: the bus is not just an in-memory catalog exercised
/// by its own fixture — `CapabilityRegistry::dispatch_capability` is a
/// real production caller that discovers a live registered tool through
/// `typed_bus()` and invokes it (already MAC-enforced by `register_tool`)
/// under the bus's permission/timeout/cost contract, reachable from
/// `susi os capabilities`.
#[test]
fn vc_202_012_mastery() {
    struct Echo;
    impl Tool for Echo {
        fn name(&self) -> &str {
            "echo"
        }
        fn description(&self) -> &str {
            "echoes its args back"
        }
        fn execute(
            &self,
            args: &serde_json::Value,
            _workspace: &std::path::Path,
        ) -> susi_core::susi_error::EaiResult<String> {
            Ok(args.to_string())
        }
    }

    let registry = CapabilityRegistry::new();
    registry.register_tool(Echo);
    let workspace = std::env::temp_dir();

    // Discovery: the tool surfaces on the Mcp surface without any bespoke
    // wiring — it is data in the registry, not code in a new place.
    let discovered = registry.typed_bus().discover();
    assert!(discovered
        .iter()
        .any(|d| d.id == "echo" && d.surface == CapabilitySurface::Mcp));

    // Dispatch: a real invocation of the real tool, permission-checked,
    // timed, and cost-attributed through the shared contract.
    let allowed = CapabilityRequest::new(
        "echo",
        "operator",
        ["capability:tool".to_string()],
        30_000,
        u64::MAX,
    );
    let receipt = registry
        .dispatch_capability(&allowed, &json!({"ping": "pong"}), &workspace)
        .expect("a permitted, in-budget dispatch must succeed");
    assert_eq!(receipt.capability_id, "echo");
    assert_eq!(receipt.surface, CapabilitySurface::Mcp);
    assert_eq!(receipt.output, json!({"ping": "pong"}).to_string());

    // Permission is enforced on the real path, not bypassed by it.
    let unpermitted = CapabilityRequest::new("echo", "operator", [], 30_000, u64::MAX);
    assert!(matches!(
        registry.dispatch_capability(&unpermitted, &json!({}), &workspace),
        Err(CapabilityError::PermissionDenied(_))
    ));

    // A zero timeout refuses before the tool ever runs.
    let no_time = CapabilityRequest::new(
        "echo",
        "operator",
        ["capability:tool".to_string()],
        0,
        u64::MAX,
    );
    assert!(matches!(
        registry.dispatch_capability(&no_time, &json!({}), &workspace),
        Err(CapabilityError::TimeoutExceeded { .. })
    ));

    // An unregistered id is refused, not silently ignored.
    let missing = CapabilityRequest::new(
        "missing",
        "operator",
        ["capability:tool".to_string()],
        30_000,
        u64::MAX,
    );
    assert!(matches!(
        registry.dispatch_capability(&missing, &json!({}), &workspace),
        Err(CapabilityError::UnknownCapability(_))
    ));
}

#[test]
fn capability_bus_rejects_invalid_and_duplicate_descriptors() {
    let mut bus = CapabilityBus::new();
    let empty_id = CapabilityDescriptor::new("", CapabilitySurface::Http, "op", "perm", 100, 10);
    assert!(matches!(
        bus.register(empty_id),
        Err(CapabilityError::InvalidDescriptor(_))
    ));

    let zero_timeout =
        CapabilityDescriptor::new("http.op", CapabilitySurface::Http, "op", "perm", 0, 10);
    assert!(matches!(
        bus.register(zero_timeout),
        Err(CapabilityError::InvalidDescriptor(_))
    ));

    let valid =
        CapabilityDescriptor::new("http.op", CapabilitySurface::Http, "op", "perm", 100, 10);
    bus.register(valid.clone())
        .expect("first registration succeeds");
    assert!(matches!(
        bus.register(valid),
        Err(CapabilityError::InvalidDescriptor(_))
    ));
}

#[test]
fn capability_bus_enforces_budget_ceiling_and_surface_filtering() {
    let mut bus = CapabilityBus::new();
    bus.register(CapabilityDescriptor::new(
        "a2a.delegate",
        CapabilitySurface::A2a,
        "delegate",
        "perm:a2a",
        500,
        100,
    ))
    .expect("register a2a");
    bus.register(CapabilityDescriptor::new(
        "mcp.search",
        CapabilitySurface::Mcp,
        "search",
        "perm:mcp",
        500,
        100,
    ))
    .expect("register mcp");

    let a2a_only = bus.discover_surface(CapabilitySurface::A2a);
    assert_eq!(a2a_only.len(), 1);
    assert_eq!(a2a_only[0].id, "a2a.delegate");

    let over_budget_req =
        CapabilityRequest::new("a2a.delegate", "agent-1", ["perm:a2a".to_string()], 500, 25);
    let err = bus
        .dispatch(&over_budget_req, |_| {
            Ok(CapabilityInvocation {
                output: json!({}),
                elapsed_ms: 10,
                cost_micros: 50,
            })
        })
        .expect_err("cost exceeding caller budget must fail");
    assert_eq!(
        err,
        CapabilityError::BudgetExceeded {
            cost_micros: 50,
            limit_micros: 25
        }
    );
}
