#![allow(missing_docs)] // integration test crate: no public API to document
#![allow(clippy::expect_used)] // the fixture catalog is intentionally checked in

use serde_json::json;
use susi_core::capability_bus::{
    CapabilityBus, CapabilityDescriptor, CapabilityInvocation, CapabilityRequest, CapabilitySurface,
};

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
