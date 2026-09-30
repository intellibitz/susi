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
