//! Tests for resource inventory (`vc_201_041_*`).

use crate::resource_inventory::{Observation, ProbeInputs, ResourceInventory};

#[test]
fn vc_201_041_unknown_probes_are_not_fabricated_zeros() {
    let inv = ResourceInventory::from_probes(ProbeInputs {
        cpu_cores: Some(8),
        ram_bytes: None,
        gpu_name: None,
        vram_bytes: None,
        disk_free_bytes: Some(1_000_000),
        models: None,
        runtimes: Some(vec!["candle".into()]),
        now_unix: Some(42),
    });
    assert_eq!(inv.captured_unix, 42);
    assert_eq!(inv.cpu_cores.value_ref(), Some(&8));
    assert!(inv.ram_bytes.is_unknown());
    assert!(inv.gpu_name.is_unknown());
    assert!(inv.vram_bytes.is_unknown());
    // Critical: unknown must not look like "0 bytes free / ready".
    assert!(matches!(inv.ram_bytes, Observation::Unknown { .. }));
    assert!(inv.has_unknown_readiness());
}

#[test]
fn vc_201_041_full_observation_reports_ready_fields() {
    let inv = ResourceInventory::from_probes(ProbeInputs {
        cpu_cores: Some(16),
        ram_bytes: Some(64 << 30),
        gpu_name: Some("NVIDIA".into()),
        vram_bytes: Some(24 << 30),
        disk_free_bytes: Some(500 << 30),
        models: Some(vec!["qwen".into()]),
        runtimes: Some(vec!["candle".into()]),
        now_unix: Some(99),
    });
    assert!(!inv.has_unknown_readiness());
    assert_eq!(inv.models.value_ref().unwrap().len(), 1);
}

#[test]
fn vc_201_041_provenance_distinguishes_observed_vs_unavailable() {
    let inv = ResourceInventory::from_probes(ProbeInputs {
        cpu_cores: Some(4),
        now_unix: Some(1),
        ..ProbeInputs::default()
    });
    match &inv.cpu_cores {
        Observation::Value { provenance, .. } => {
            let s = format!("{provenance:?}");
            assert!(s.contains("Observed") || s.contains("cpu"));
        }
        Observation::Unknown { .. } => panic!("cpu should be observed"),
    }
    match &inv.gpu_name {
        Observation::Unknown { provenance, .. } => {
            let s = format!("{provenance:?}");
            assert!(s.contains("Unavailable") || s.contains("GPU") || s.contains("gpu"));
        }
        Observation::Value { .. } => panic!("gpu should be unknown"),
    }
}
