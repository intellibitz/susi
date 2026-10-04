//! Mastery verification for VC-201-075: per-reflex grants derived from
//! the parent mission — filesystem, network, memory, fuel, and time —
//! such that generated code cannot acquire permissions.

use std::collections::BTreeSet;

use crate::wasm_grants::{GrantSet, ResourceLimits};

/// Containment is enforced: `caps` and `limits` are private.
/// A reflex cannot mutate or widen its grant set to self-grant permissions.
#[test]
fn vc_201_075_mastery_reflex_cannot_self_grant() {
    let host = GrantSet::host_baseline();
    let wasm = host.attenuate_for_wasm();

    assert!(!wasm.allows("net.loopback"));
    assert!(!wasm.allows("fs.write"));
    assert!(!wasm.allows("admin"));

    // Caps returned by reference are immutable.
    let caps = wasm.caps();
    assert!(!caps.contains("admin"));
    assert!(!caps.contains("net.loopback"));
    assert!(!caps.contains("fs.write"));
}

/// Derivation from parent mission: reflex grants are derived from the
/// parent's grant set. A child reflex can never acquire capabilities
/// that the parent mission does not hold.
#[test]
fn vc_201_075_mastery_derived_from_parent_mission() {
    let mut parent_caps = BTreeSet::new();
    parent_caps.insert("fs.read".to_string());
    parent_caps.insert("custom.sensor".to_string());

    let parent_limits = ResourceLimits {
        max_memory_bytes: 32 * 1024 * 1024,
        max_fuel: 50_000,
        timeout_millis: 2_000,
    };
    let parent = GrantSet::from_caps_and_limits(parent_caps, parent_limits);

    // Reflex requests sensor and attempts privilege escalation (admin, net.loopback).
    let mut requested_caps = BTreeSet::new();
    requested_caps.insert("custom.sensor".to_string());
    requested_caps.insert("admin".to_string());
    requested_caps.insert("net.loopback".to_string());

    let child = parent.derive_for_reflex(&requested_caps, ResourceLimits::default());

    // Only custom.sensor is granted; unauthorized escalated caps are stripped out.
    assert!(child.allows("custom.sensor"));
    assert!(!child.allows("admin"));
    assert!(!child.allows("net.loopback"));
    assert_eq!(child.caps().len(), 1);
}

/// Bounded execution dimensions: memory, fuel, and time are tracked and
/// clamped to the parent's limits during reflex derivation.
#[test]
fn vc_201_075_mastery_resource_limits_clamped_and_enforced() {
    let parent_limits = ResourceLimits {
        max_memory_bytes: 10 * 1024 * 1024,
        max_fuel: 20_000,
        timeout_millis: 1_000,
    };
    let parent = GrantSet::from_caps_and_limits(BTreeSet::new(), parent_limits);

    // Child requests limits exceeding parent.
    let excessive_limits = ResourceLimits {
        max_memory_bytes: 50 * 1024 * 1024,
        max_fuel: 100_000,
        timeout_millis: 5_000,
    };
    let child = parent.derive_for_reflex(&BTreeSet::new(), excessive_limits);

    // Clamped strictly to parent's bounds.
    let limits = child.limits();
    assert_eq!(limits.max_memory_bytes, 10 * 1024 * 1024);
    assert_eq!(limits.max_fuel, 20_000);
    assert_eq!(limits.timeout_millis, 1_000);

    // Enforcement checks
    assert!(child.allows_resources(10 * 1024 * 1024, 20_000, 1_000));
    assert!(!child.allows_resources(10 * 1024 * 1024 + 1, 20_000, 1_000));
    assert!(!child.allows_resources(10 * 1024 * 1024, 20_001, 1_000));
    assert!(!child.allows_resources(10 * 1024 * 1024, 20_000, 1_001));
}

/// Nominal attenuation holds: wasm receives at most {fs.read, clock},
/// and cannot exceed what the parent held.
#[test]
fn vc_201_075_mastery_nominal_attenuation_holds() {
    let host = GrantSet::host_baseline();
    let wasm = host.attenuate_for_wasm();

    assert!(wasm.allows("fs.read"));
    assert!(wasm.allows("clock"));
    assert!(!wasm.allows("net.loopback"));

    // If parent only has clock, wasm gets only clock (never gains fs.read).
    let thin = GrantSet::from_caps_and_limits(
        BTreeSet::from(["clock".to_string()]),
        ResourceLimits::default(),
    );
    let thin_wasm = thin.attenuate_for_wasm();
    assert_eq!(thin_wasm.caps().len(), 1);
    assert!(thin_wasm.allows("clock"));
    assert!(!thin_wasm.allows("fs.read"));
}
