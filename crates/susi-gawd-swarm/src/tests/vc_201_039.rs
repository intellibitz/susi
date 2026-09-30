use crate::locality_placement::{place, PeerInfo, PlacementTask, Residency};
use std::collections::BTreeSet;

fn peer(id: &str, datasets: &[&str], region: &str, cpu: f64, gpu: f64) -> PeerInfo {
    PeerInfo {
        node_id: id.into(),
        datasets: datasets.iter().map(|d| d.to_string()).collect(),
        verified_caps: BTreeSet::from(["model:llama".to_string()]),
        region: region.into(),
        cpu_free: cpu,
        gpu_mem_free_gb: gpu,
    }
}

fn task(residency: Residency) -> PlacementTask {
    PlacementTask {
        dataset_id: "corpus-7".into(),
        residency,
        needs_caps: vec!["model:llama".into()],
        cpu: 4.0,
        gpu_mem_gb: 8.0,
    }
}

#[test]
fn vc_201_039_local_only_dataset_never_moves_off_permitted_hosts() {
    let t = task(Residency::LocalOnly {
        permitted_hosts: BTreeSet::from(["h1".to_string(), "h2".to_string()]),
    });
    let peers = vec![
        // Permitted but does not hold the data — still refused (no move).
        peer("h1", &[], "us", 64.0, 80.0),
        // Permitted AND holds the dataset — the only legal host.
        peer("h2", &["corpus-7"], "us", 8.0, 10.0),
        // Holds the dataset but not permitted — refused.
        peer("h3", &["corpus-7"], "us", 64.0, 80.0),
    ];
    let (best, rejections) = place(&t, &peers);
    assert_eq!(best.map(|p| p.node_id), Some("h2".to_string()));
    assert_eq!(rejections.len(), 2);
    assert!(rejections
        .iter()
        .all(|r| r.reason == "residency constraint"));
}

#[test]
fn vc_201_039_locality_score_prefers_data_on_host() {
    let t = task(Residency::Anywhere);
    let peers = vec![
        peer("far", &[], "us", 64.0, 80.0),          // richer but no data
        peer("near", &["corpus-7"], "us", 8.0, 9.0), // holds the dataset
    ];
    let (best, _) = place(&t, &peers);
    assert_eq!(
        best.map(|p| p.node_id),
        Some("near".to_string()),
        "data locality dominates resource richness"
    );
}

#[test]
fn vc_201_039_region_residency_and_resource_gates() {
    let t = task(Residency::Region("eu".into()));
    let peers = vec![
        peer("us-node", &["corpus-7"], "us", 64.0, 80.0),
        peer("eu-small", &["corpus-7"], "eu", 2.0, 4.0), // under-resourced
        peer("eu-fit", &["corpus-7"], "eu", 16.0, 16.0),
    ];
    let (best, rejections) = place(&t, &peers);
    assert_eq!(best.map(|p| p.node_id), Some("eu-fit".to_string()));
    assert!(rejections
        .iter()
        .any(|r| r.node_id == "us-node" && r.reason == "residency constraint"));
    assert!(rejections
        .iter()
        .any(|r| r.node_id == "eu-small" && r.reason == "insufficient resources"));
}

#[test]
fn vc_201_039_unverified_capability_is_rejected_not_scored() {
    let t = task(Residency::Anywhere);
    let mut capable = peer("cap", &["corpus-7"], "us", 16.0, 16.0);
    capable.verified_caps = BTreeSet::new(); // self-reported does not count
    let peers = vec![capable, peer("ok", &["corpus-7"], "us", 16.0, 16.0)];
    let (best, rejections) = place(&t, &peers);
    assert_eq!(best.map(|p| p.node_id), Some("ok".to_string()));
    assert!(rejections
        .iter()
        .any(|r| r.reason == "missing verified capability"));
}
