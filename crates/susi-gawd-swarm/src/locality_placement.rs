//! Federated task placement by data locality (VC-201-039).
//!
//! Scores *eligible* peers — residency is a hard gate, not a weight: a
//! dataset marked local-only is never moved, and a task bound to it can
//! only be scored on hosts inside its permitted set. Among eligible
//! peers the score prefers verified capability, on-host data, and
//! available resources so work lands next to its data instead of
//! dragging the data to the work.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Where a dataset is allowed to be consumed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Residency {
    /// Only the listed hosts may touch it — it never leaves them.
    LocalOnly { permitted_hosts: BTreeSet<String> },
    /// Any host in the named region/zone.
    Region(String),
    /// No constraint.
    Anywhere,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlacementTask {
    /// Dataset the task must reach.
    pub dataset_id: String,
    pub residency: Residency,
    /// Capabilities the peer must have *verified* (e.g. `model:llama`).
    pub needs_caps: Vec<String>,
    pub cpu: f64,
    pub gpu_mem_gb: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PeerInfo {
    pub node_id: String,
    /// Datasets already resident on this peer.
    pub datasets: BTreeSet<String>,
    /// Capabilities verified by signed handshake — self-reported
    /// strings do not count.
    pub verified_caps: BTreeSet<String>,
    /// Region/zone this peer belongs to.
    pub region: String,
    pub cpu_free: f64,
    pub gpu_mem_free_gb: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Placement {
    pub node_id: String,
    /// Higher is better; exact weights are the contract below.
    pub score: f64,
}

/// Every refusal kept for the placement audit trail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacementRejection {
    pub node_id: String,
    pub reason: &'static str,
}

fn residency_allows(task: &PlacementTask, peer: &PeerInfo) -> bool {
    match &task.residency {
        Residency::LocalOnly { permitted_hosts } => {
            // Data stays put: only permitted hosts score at all, and
            // the peer must already hold the dataset — it is never
            // copied to satisfy placement.
            permitted_hosts.contains(&peer.node_id) && peer.datasets.contains(&task.dataset_id)
        }
        Residency::Region(region) => peer.region == *region,
        Residency::Anywhere => true,
    }
}

fn reject_reason(task: &PlacementTask, peer: &PeerInfo) -> Option<&'static str> {
    if !residency_allows(task, peer) {
        return Some("residency constraint");
    }
    if !task
        .needs_caps
        .iter()
        .all(|c| peer.verified_caps.contains(c))
    {
        return Some("missing verified capability");
    }
    if peer.cpu_free < task.cpu || peer.gpu_mem_free_gb < task.gpu_mem_gb {
        return Some("insufficient resources");
    }
    None
}

/// Score one eligible peer. Weights: verified capability coverage is
/// mandatory (gated above); on-host data is the dominant signal (100),
/// then resource headroom (cpu free / need, gpu free / need capped).
fn score(task: &PlacementTask, peer: &PeerInfo) -> f64 {
    let mut s = 0.0;
    if peer.datasets.contains(&task.dataset_id) {
        s += 100.0; // data locality dominates — no data movement
    }
    if task.cpu > 0.0 {
        s += (peer.cpu_free / task.cpu).min(4.0) * 10.0;
    }
    if task.gpu_mem_gb > 0.0 {
        s += (peer.gpu_mem_free_gb / task.gpu_mem_gb).min(4.0) * 10.0;
    }
    s
}

/// Pick the best eligible peer, or `None` with every rejection reason.
#[must_use]
pub fn place(
    task: &PlacementTask,
    peers: &[PeerInfo],
) -> (Option<Placement>, Vec<PlacementRejection>) {
    let mut best: Option<Placement> = None;
    let mut rejections = Vec::new();
    for peer in peers {
        match reject_reason(task, peer) {
            Some(reason) => rejections.push(PlacementRejection {
                node_id: peer.node_id.clone(),
                reason,
            }),
            None => {
                let s = score(task, peer);
                let better = match &best {
                    Some(b) => s > b.score,
                    None => true,
                };
                if better {
                    best = Some(Placement {
                        node_id: peer.node_id.clone(),
                        score: s,
                    });
                }
            }
        }
    }
    (best, rejections)
}

#[cfg(test)]
mod inline_tests {
    use super::*;

    fn sample_peer(
        node_id: &str,
        region: &str,
        datasets: &[&str],
        caps: &[&str],
        (cpu_free, gpu_free): (f64, f64),
    ) -> PeerInfo {
        PeerInfo {
            node_id: node_id.into(),
            datasets: datasets.iter().map(|s| (*s).to_string()).collect(),
            verified_caps: caps.iter().map(|s| (*s).to_string()).collect(),
            region: region.into(),
            cpu_free,
            gpu_mem_free_gb: gpu_free,
        }
    }

    #[test]
    fn local_only_residency_requires_permitted_host_and_resident_dataset() {
        let task = PlacementTask {
            dataset_id: "ds-secret".into(),
            residency: Residency::LocalOnly {
                permitted_hosts: BTreeSet::from(["node-a".to_string(), "node-b".to_string()]),
            },
            needs_caps: vec![],
            cpu: 1.0,
            gpu_mem_gb: 0.0,
        };
        let peers = vec![
            sample_peer("node-a", "us", &[], &[], (4.0, 0.0)),
            sample_peer("node-c", "us", &["ds-secret"], &[], (4.0, 0.0)),
            sample_peer("node-b", "us", &["ds-secret"], &[], (4.0, 0.0)),
        ];
        let (best, rejections) = place(&task, &peers);
        assert_eq!(best.unwrap().node_id, "node-b");
        assert_eq!(rejections.len(), 2);
        assert!(rejections
            .iter()
            .all(|r| r.reason == "residency constraint"));
    }

    #[test]
    fn place_records_specific_rejection_reasons_for_region_capability_and_resources() {
        let task = PlacementTask {
            dataset_id: "ds-1".into(),
            residency: Residency::Region("eu-west".into()),
            needs_caps: vec!["model:qwen".into()],
            cpu: 4.0,
            gpu_mem_gb: 8.0,
        };
        let peers = vec![
            sample_peer(
                "p-wrong-region",
                "us-east",
                &["ds-1"],
                &["model:qwen"],
                (8.0, 16.0),
            ),
            sample_peer("p-no-cap", "eu-west", &["ds-1"], &[], (8.0, 16.0)),
            sample_peer(
                "p-low-gpu",
                "eu-west",
                &["ds-1"],
                &["model:qwen"],
                (8.0, 4.0),
            ),
        ];
        let (best, rejections) = place(&task, &peers);
        assert!(best.is_none());
        assert_eq!(rejections[0].reason, "residency constraint");
        assert_eq!(rejections[1].reason, "missing verified capability");
        assert_eq!(rejections[2].reason, "insufficient resources");
    }

    #[test]
    fn locality_bonus_prefers_peer_holding_dataset_over_higher_raw_resources() {
        let task = PlacementTask {
            dataset_id: "ds-hot".into(),
            residency: Residency::Anywhere,
            needs_caps: vec!["tool:rg".into()],
            cpu: 2.0,
            gpu_mem_gb: 4.0,
        };
        let peers = vec![
            sample_peer("remote-big", "us", &[], &["tool:rg"], (16.0, 32.0)),
            sample_peer("local-small", "us", &["ds-hot"], &["tool:rg"], (2.0, 4.0)),
        ];
        let (best, rejections) = place(&task, &peers);
        assert!(rejections.is_empty());
        let winner = best.unwrap();
        assert_eq!(winner.node_id, "local-small");
        assert!(winner.score >= 120.0);
    }

    #[test]
    fn resource_headroom_caps_at_four_times_requirement_when_scoring_peers() {
        let task = PlacementTask {
            dataset_id: "ds-none".into(),
            residency: Residency::Anywhere,
            needs_caps: vec![],
            cpu: 2.0,
            gpu_mem_gb: 0.0,
        };
        let p_4x = sample_peer("p-4x", "us", &[], &[], (8.0, 0.0));
        let p_10x = sample_peer("p-10x", "us", &[], &[], (20.0, 0.0));
        let (best, _) = place(&task, &[p_4x, p_10x]);
        let chosen = best.unwrap();
        assert_eq!(chosen.node_id, "p-4x");
        assert!((chosen.score - 40.0).abs() < f64::EPSILON);
    }

    #[test]
    fn place_returns_none_and_empty_rejections_when_peer_list_is_empty() {
        let task = PlacementTask {
            dataset_id: "ds-empty".into(),
            residency: Residency::Anywhere,
            needs_caps: vec![],
            cpu: 1.0,
            gpu_mem_gb: 2.0,
        };
        let (best, rejections) = place(&task, &[]);
        assert!(best.is_none());
        assert!(rejections.is_empty());
    }
}
