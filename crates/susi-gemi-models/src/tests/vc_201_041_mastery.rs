//! Vector VC-201-041 mastery tests.
//!
//! Vector: Publish an observed local AI resource inventory.
//! Mastery target: Unify actual CPU, RAM, GPU, VRAM, disk, model, and runtime
//! observations with timestamps and provenance; unavailable probes are unknown
//! rather than zero or fabricated readiness.

use crate::resource_inventory::{Observation, ProbeInputs, ProbeProvenance, ResourceInventory};

#[test]
fn vc_201_041_mastery_unknown_probe_is_not_zero() {
    let inv = ResourceInventory::from_probes(ProbeInputs {
        cpu_cores: Some(4),
        ram_bytes: None,
        gpu_name: None,
        vram_bytes: None,
        disk_free_bytes: None,
        models: None,
        runtimes: None,
        now_unix: Some(1790959800),
    });

    assert_eq!(inv.captured_unix, 1790959800);
    // Unavailable RAM/VRAM probes must report Observation::Unknown, never 0.
    assert!(inv.ram_bytes.is_unknown());
    assert_eq!(inv.ram_bytes.value_ref(), None);
    assert!(inv.vram_bytes.is_unknown());
    assert_eq!(inv.vram_bytes.value_ref(), None);
    assert!(inv.disk_free_bytes.is_unknown());
    assert_eq!(inv.disk_free_bytes.value_ref(), None);

    // Critical readiness fields that are unknown must not report ready.
    assert!(inv.has_unknown_readiness());
}

#[test]
fn vc_201_041_mastery_provenance_and_timestamps_present() {
    let t = 1790959800;
    let inv = ResourceInventory::from_probes(ProbeInputs {
        cpu_cores: Some(8),
        ram_bytes: None,
        gpu_name: None,
        vram_bytes: None,
        disk_free_bytes: Some(100_000_000_000),
        models: Some(vec!["qwen-2.5-coder-7b".into()]),
        runtimes: None,
        now_unix: Some(t),
    });

    // Observed fields must carry observed timestamp and Observed provenance
    match &inv.cpu_cores {
        Observation::Value {
            observed_unix,
            provenance,
            value,
        } => {
            assert_eq!(*observed_unix, t);
            assert_eq!(*value, 8);
            assert!(matches!(provenance, ProbeProvenance::Observed { .. }));
        }
        Observation::Unknown { .. } => panic!("cpu_cores should be observed"),
    }

    // Unavailable fields must carry timestamp and Unavailable provenance with diagnostic reason
    match &inv.gpu_name {
        Observation::Unknown {
            observed_unix,
            provenance,
        } => {
            assert_eq!(*observed_unix, t);
            match provenance {
                ProbeProvenance::Unavailable { reason } => {
                    assert!(!reason.is_empty());
                }
                ProbeProvenance::Observed { .. } => panic!("gpu_name should be unavailable"),
            }
        }
        Observation::Value { .. } => panic!("gpu_name should be unknown"),
    }
}

#[test]
fn vc_201_041_mastery_complete_probes_clear_unknown_readiness_and_preserve_models_runtimes() {
    let inv = ResourceInventory::from_probes(ProbeInputs {
        cpu_cores: Some(16),
        ram_bytes: Some(64_000_000_000),
        gpu_name: Some("RTX 4090".into()),
        vram_bytes: Some(24_000_000_000),
        disk_free_bytes: Some(500_000_000_000),
        models: Some(vec!["m1".into()]),
        runtimes: Some(vec!["ollama".into()]),
        now_unix: Some(1790959900),
    });

    assert!(!inv.has_unknown_readiness());
    assert_eq!(inv.models.value_ref(), Some(&vec!["m1".to_string()]));
    assert_eq!(inv.runtimes.value_ref(), Some(&vec!["ollama".to_string()]));
}

#[test]
fn vc_201_041_mastery_serde_json_roundtrip_preserves_observed_and_unknown_variants() {
    let inv = ResourceInventory::from_probes(ProbeInputs {
        cpu_cores: Some(8),
        ram_bytes: Some(32_000_000_000),
        gpu_name: None,
        vram_bytes: None,
        disk_free_bytes: Some(100_000_000_000),
        models: Some(vec!["qwen-2.5".into()]),
        runtimes: None,
        now_unix: Some(1790959950),
    });

    let serialized = serde_json::to_string(&inv).expect("serialize ResourceInventory");
    let decoded: ResourceInventory =
        serde_json::from_str(&serialized).expect("deserialize ResourceInventory");
    assert_eq!(decoded, inv);
}

#[test]
fn vc_201_041_mastery_missing_single_critical_probe_flags_unknown_readiness() {
    let inv = ResourceInventory::from_probes(ProbeInputs {
        cpu_cores: Some(16),
        ram_bytes: Some(64_000_000_000),
        gpu_name: Some("RTX 4090".into()),
        vram_bytes: Some(24_000_000_000),
        disk_free_bytes: None,
        models: Some(vec!["m1".into()]),
        runtimes: Some(vec!["ollama".into()]),
        now_unix: Some(1790960000),
    });
    assert!(inv.disk_free_bytes.is_unknown());
    assert!(inv.has_unknown_readiness());
}
