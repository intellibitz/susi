use crate::wasm_grants::{GrantSet, ResourceLimits};
use std::collections::BTreeSet;

#[test]
fn vc_201_075_wasm_cannot_gain_host_network_or_write() {
    let host = GrantSet::host_baseline();
    let wasm = host.attenuate_for_wasm();
    assert!(wasm.allows("fs.read"));
    assert!(wasm.allows("clock"));
    assert!(!wasm.allows("net.loopback"));
    assert!(!wasm.allows("fs.write"));
}

#[test]
fn vc_201_075_derive_for_reflex_clamps_limits_and_intersects_capabilities() {
    let host = GrantSet::host_baseline();
    let caps = BTreeSet::from([
        "fs.read".to_string(),
        "net.external".to_string(),
        "root.exec".to_string(),
    ]);
    let oversized = ResourceLimits {
        max_memory_bytes: u64::MAX,
        max_fuel: u64::MAX,
        timeout_millis: u64::MAX,
    };
    let child = host.derive_for_reflex(&caps, oversized);
    assert!(child.allows("fs.read"));
    assert!(!child.allows("net.external"));
    assert!(!child.allows("root.exec"));
    assert_eq!(child.limits(), host.limits());
}

#[test]
fn vc_201_075_allows_all_and_allows_resources_enforce_strict_subset_and_bounds() {
    let wasm = GrantSet::host_baseline().attenuate_for_wasm();
    assert!(wasm.allows_all(&BTreeSet::from([
        "fs.read".to_string(),
        "clock".to_string()
    ])));
    assert!(!wasm.allows_all(&BTreeSet::from([
        "fs.read".to_string(),
        "net.loopback".to_string()
    ])));
    assert!(wasm.allows_resources(16 * 1024 * 1024, 100_000, 5_000));
    assert!(!wasm.allows_resources(16 * 1024 * 1024 + 1, 100_000, 5_000));
    assert!(!wasm.allows_resources(16 * 1024 * 1024, 100_001, 5_000));
    assert!(!wasm.allows_resources(16 * 1024 * 1024, 100_000, 5_001));
}

#[test]
fn vc_201_075_from_caps_and_limits_preserves_custom_grant_set() {
    let custom = GrantSet::from_caps_and_limits(
        BTreeSet::from(["custom.cap".to_string()]),
        ResourceLimits {
            max_memory_bytes: 1024,
            max_fuel: 50,
            timeout_millis: 10,
        },
    );
    assert_eq!(custom.caps().len(), 1);
    assert!(custom.allows("custom.cap"));
    assert_eq!(custom.limits().max_fuel, 50);
}
