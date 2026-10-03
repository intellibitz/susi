//! Mastery verification for VC-201-075: per-reflex grants derived from
//! the parent mission — filesystem, network, memory, fuel, and time —
//! such that generated code cannot acquire permissions.

use crate::wasm_grants::GrantSet;

/// Falsification: 'cannot acquire permissions' is unenforced — `caps` is
/// a public BTreeSet. A reflex can grant itself anything at any time;
/// attenuation is a constructor convention, not a boundary.
#[test]
fn vc_201_075_mastery_reflex_can_self_grant() {
    let host = GrantSet::host_baseline();
    let mut wasm = host.attenuate_for_wasm();
    assert!(!wasm.allows("net.loopback"));
    wasm.caps.insert("net.loopback".to_string());
    wasm.caps.insert("fs.write".to_string());
    wasm.caps.insert("admin".to_string());
    assert!(wasm.allows("net.loopback"));
    assert!(wasm.allows("fs.write"));
    assert!(wasm.allows("admin"));
}

/// Falsification: grants are not derived from the parent mission — they
/// are intersected with one hardcoded list. Two missions with entirely
/// different needs get identical reflex grants; the mission cannot widen
/// OR narrow what a reflex receives beyond the fixed {fs.read, clock}.
#[test]
fn vc_201_075_mastery_not_derived_from_parent() {
    // A parent mission that legitimately has network access still
    // produces a reflex whose grants are the same fixed intersection —
    // derivation would make reflex grants a per-mission value.
    let mut parent = GrantSet::host_baseline();
    parent.caps.insert("net.loopback".into());
    parent.caps.insert("custom.cap".into());
    let wasm = parent.attenuate_for_wasm();
    assert_eq!(wasm.caps.len(), 2, "every parent yields the same grants");
}

/// Falsification: memory, fuel, and time dimensions do not exist —
/// GrantSet carries only a set of strings. There is nothing to bound
/// fuel/time/memory, so 'bounded execution' grants cannot be expressed,
/// let alone derived.
#[test]
fn vc_201_075_mastery_no_resource_dimensions() {
    let wasm = GrantSet::host_baseline().attenuate_for_wasm();
    // No memory/fuel/time field exists; 'grants' for them can be
    // smuggled as strings, and none is enforced.
    assert!(!wasm.allows("fuel.unlimited"));
    assert!(!wasm.allows("memory.unbounded"));
    // …but they can be granted by mutation (see self_grant test) —
    // the struct has no notion of these dimensions at all.
}

/// What holds: the nominal baseline intersects correctly — wasm drops
/// net.loopback and gains nothing beyond the parent's set.
#[test]
fn vc_201_075_mastery_nominal_attenuation_holds() {
    let host = GrantSet::host_baseline();
    let wasm = host.attenuate_for_wasm();
    assert!(wasm.allows("fs.read"));
    assert!(wasm.allows("clock"));
    assert!(!wasm.allows("net.loopback"));
    // And attenuation never widens beyond what the parent had.
    let thin = GrantSet {
        caps: ["clock".to_string()].into_iter().collect(),
    };
    assert_eq!(thin.attenuate_for_wasm().caps.len(), 1);
}
