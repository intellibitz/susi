use crate::capability_bus::{
    CapabilityBus, CapabilityDescriptor, CapabilityError, CapabilityInvocation, CapabilityRequest,
    CapabilitySurface,
};
use serde_json::json;

fn catalog() -> CapabilityBus {
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
        .expect("test catalog is valid");
    }
    bus
}

#[test]
fn capability_bus_discovers_all_surfaces() {
    let bus = catalog();
    assert_eq!(bus.discover().len(), 4);
    for surface in [
        CapabilitySurface::A2a,
        CapabilitySurface::Mcp,
        CapabilitySurface::Http,
        CapabilitySurface::Adapter,
    ] {
        assert_eq!(bus.discover_surface(surface).len(), 1);
    }
}

#[test]
fn capability_bus_authorizes_and_attributes_cost() {
    let bus = catalog();
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
        .expect("authorized invocation must produce a receipt");
    assert_eq!(receipt.subject, "mission-7");
    assert_eq!(receipt.surface, CapabilitySurface::Http);
    assert_eq!(receipt.cost_micros, 30);
    assert_eq!(receipt.output["status"], "ok");
}

#[test]
fn capability_bus_refuses_permission_timeout_and_budget_violations() {
    let bus = catalog();
    let denied = CapabilityRequest::new("mcp.tool", "agent", [], 80, 40);
    assert!(matches!(
        bus.dispatch(&denied, |_| unreachable!()),
        Err(CapabilityError::PermissionDenied(_))
    ));

    let request = CapabilityRequest::new(
        "adapter.embed",
        "agent",
        ["invoke:capability".to_string()],
        80,
        40,
    );
    assert!(matches!(
        bus.dispatch(&request, |_| {
            Ok(CapabilityInvocation {
                output: json!(null),
                elapsed_ms: 81,
                cost_micros: 1,
            })
        }),
        Err(CapabilityError::TimeoutExceeded { .. })
    ));
    assert!(matches!(
        bus.dispatch(&request, |_| {
            Ok(CapabilityInvocation {
                output: json!(null),
                elapsed_ms: 1,
                cost_micros: 41,
            })
        }),
        Err(CapabilityError::BudgetExceeded { .. })
    ));
}
