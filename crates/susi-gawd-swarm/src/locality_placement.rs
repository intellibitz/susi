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

impl Residency {
    /// Verifies that residency constraints contain valid, non-empty host or region identifiers.
    ///
    /// # Errors
    ///
    /// Returns a descriptive error string if `LocalOnly` has an empty `permitted_hosts` set
    /// or contains a blank host name, or if `Region` is blank.
    pub fn verify_residency_invariants(&self) -> Result<(), String> {
        match self {
            Self::LocalOnly { permitted_hosts } => {
                if permitted_hosts.is_empty() {
                    return Err(
                        "Residency::LocalOnly permitted_hosts must not be empty".to_string()
                    );
                }
                if permitted_hosts.iter().any(|h| h.trim().is_empty()) {
                    return Err(
                        "Residency::LocalOnly permitted_hosts must not contain blank hosts"
                            .to_string(),
                    );
                }
                Ok(())
            }
            Self::Region(r) => {
                if r.trim().is_empty() {
                    return Err("Residency::Region must not be blank".to_string());
                }
                Ok(())
            }
            Self::Anywhere => Ok(()),
        }
    }
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

impl PlacementTask {
    /// Verifies dataset identity, residency invariants, capability requirements, and resource bounds.
    ///
    /// # Errors
    ///
    /// Returns a descriptive error string if `dataset_id` is blank, `residency` is invalid,
    /// `needs_caps` contains blank entries, or `cpu`/`gpu_mem_gb` is non-finite or negative.
    pub fn verify_task_invariants(&self) -> Result<(), String> {
        if self.dataset_id.trim().is_empty() {
            return Err("PlacementTask dataset_id must not be blank".to_string());
        }
        self.residency.verify_residency_invariants()?;
        if self.needs_caps.iter().any(|c| c.trim().is_empty()) {
            return Err("PlacementTask needs_caps must not contain blank entries".to_string());
        }
        if !self.cpu.is_finite() || self.cpu < 0.0 {
            return Err("PlacementTask cpu must be finite and >= 0.0".to_string());
        }
        if !self.gpu_mem_gb.is_finite() || self.gpu_mem_gb < 0.0 {
            return Err("PlacementTask gpu_mem_gb must be finite and >= 0.0".to_string());
        }
        Ok(())
    }
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

impl PeerInfo {
    /// Verifies peer identity, region, resident datasets, verified capabilities, and free capacity bounds.
    ///
    /// # Errors
    ///
    /// Returns a descriptive error string if `node_id` or `region` is blank, `datasets` or
    /// `verified_caps` contains a blank entry, or `cpu_free`/`gpu_mem_free_gb` is non-finite or negative.
    pub fn verify_peer_invariants(&self) -> Result<(), String> {
        if self.node_id.trim().is_empty() {
            return Err("PeerInfo node_id must not be blank".to_string());
        }
        if self.region.trim().is_empty() {
            return Err("PeerInfo region must not be blank".to_string());
        }
        if self.datasets.iter().any(|d| d.trim().is_empty()) {
            return Err("PeerInfo datasets must not contain blank entries".to_string());
        }
        if self.verified_caps.iter().any(|c| c.trim().is_empty()) {
            return Err("PeerInfo verified_caps must not contain blank entries".to_string());
        }
        if !self.cpu_free.is_finite() || self.cpu_free < 0.0 {
            return Err("PeerInfo cpu_free must be finite and >= 0.0".to_string());
        }
        if !self.gpu_mem_free_gb.is_finite() || self.gpu_mem_free_gb < 0.0 {
            return Err("PeerInfo gpu_mem_free_gb must be finite and >= 0.0".to_string());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Placement {
    pub node_id: String,
    /// Higher is better; exact weights are the contract below.
    pub score: f64,
}

impl Placement {
    /// Verifies that the selected placement has a non-empty `node_id` and a finite non-negative score.
    ///
    /// # Errors
    ///
    /// Returns a descriptive error string if `node_id` is blank or `score` is non-finite or negative.
    pub fn verify_placement_invariants(&self) -> Result<(), String> {
        if self.node_id.trim().is_empty() {
            return Err("Placement node_id must not be blank".to_string());
        }
        if !self.score.is_finite() || self.score < 0.0 {
            return Err("Placement score must be finite and >= 0.0".to_string());
        }
        Ok(())
    }
}

/// Every refusal kept for the placement audit trail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacementRejection {
    pub node_id: String,
    pub reason: &'static str,
}

impl PlacementRejection {
    /// Verifies that the rejection record has both a non-empty `node_id` and a non-empty `reason`.
    ///
    /// # Errors
    ///
    /// Returns a descriptive error string if `node_id` or `reason` is blank.
    pub fn verify_rejection_invariants(&self) -> Result<(), String> {
        if self.node_id.trim().is_empty() {
            return Err("PlacementRejection node_id must not be blank".to_string());
        }
        if self.reason.trim().is_empty() {
            return Err("PlacementRejection reason must not be blank".to_string());
        }
        Ok(())
    }
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

    #[test]
    fn residency_invariants_verify_valid_and_reject_corrupt_states() {
        assert!(Residency::Anywhere.verify_residency_invariants().is_ok());
        assert!(Residency::Region("eu-west".into())
            .verify_residency_invariants()
            .is_ok());
        assert!(Residency::Region("   ".into())
            .verify_residency_invariants()
            .is_err());

        let valid_local = Residency::LocalOnly {
            permitted_hosts: BTreeSet::from(["host-1".to_string()]),
        };
        assert!(valid_local.verify_residency_invariants().is_ok());

        let empty_local = Residency::LocalOnly {
            permitted_hosts: BTreeSet::new(),
        };
        assert!(empty_local.verify_residency_invariants().is_err());

        let blank_host_local = Residency::LocalOnly {
            permitted_hosts: BTreeSet::from(["host-1".to_string(), "  ".to_string()]),
        };
        assert!(blank_host_local.verify_residency_invariants().is_err());
    }

    #[test]
    fn placement_task_invariants_verify_valid_and_reject_corrupt_states() {
        let valid = PlacementTask {
            dataset_id: "ds-1".into(),
            residency: Residency::Region("us-east".into()),
            needs_caps: vec!["cap:a".into()],
            cpu: 2.0,
            gpu_mem_gb: 0.0,
        };
        assert!(valid.verify_task_invariants().is_ok());

        let mut bad_ds = valid.clone();
        bad_ds.dataset_id = " ".into();
        assert!(bad_ds.verify_task_invariants().is_err());

        let mut bad_residency = valid.clone();
        bad_residency.residency = Residency::Region(String::new());
        assert!(bad_residency.verify_task_invariants().is_err());

        let mut bad_cap = valid.clone();
        bad_cap.needs_caps.push("  ".into());
        assert!(bad_cap.verify_task_invariants().is_err());

        let mut bad_cpu = valid.clone();
        bad_cpu.cpu = -1.0;
        assert!(bad_cpu.verify_task_invariants().is_err());

        let mut nan_gpu = valid;
        nan_gpu.gpu_mem_gb = f64::NAN;
        assert!(nan_gpu.verify_task_invariants().is_err());
    }

    #[test]
    fn peer_info_invariants_verify_valid_and_reject_corrupt_states() {
        let valid = sample_peer("node-1", "us-west", &["ds-1"], &["cap:1"], (4.0, 8.0));
        assert!(valid.verify_peer_invariants().is_ok());

        let mut bad_id = valid.clone();
        bad_id.node_id = "  ".into();
        assert!(bad_id.verify_peer_invariants().is_err());

        let mut bad_region = valid.clone();
        bad_region.region = String::new();
        assert!(bad_region.verify_peer_invariants().is_err());

        let mut bad_ds = valid.clone();
        bad_ds.datasets.insert(" ".into());
        assert!(bad_ds.verify_peer_invariants().is_err());

        let mut bad_cap = valid.clone();
        bad_cap.verified_caps.insert(String::new());
        assert!(bad_cap.verify_peer_invariants().is_err());

        let mut bad_cpu = valid.clone();
        bad_cpu.cpu_free = f64::INFINITY;
        assert!(bad_cpu.verify_peer_invariants().is_err());

        let mut bad_gpu = valid;
        bad_gpu.gpu_mem_free_gb = -0.5;
        assert!(bad_gpu.verify_peer_invariants().is_err());
    }

    #[test]
    fn placement_and_rejection_invariants_verify_valid_and_reject_corrupt_states() {
        let valid_placement = Placement {
            node_id: "node-1".into(),
            score: 120.0,
        };
        assert!(valid_placement.verify_placement_invariants().is_ok());

        let bad_placement_id = Placement {
            node_id: " ".into(),
            score: 10.0,
        };
        assert!(bad_placement_id.verify_placement_invariants().is_err());

        let bad_placement_score = Placement {
            node_id: "node-1".into(),
            score: -1.0,
        };
        assert!(bad_placement_score.verify_placement_invariants().is_err());

        let valid_rejection = PlacementRejection {
            node_id: "node-2".into(),
            reason: "residency constraint",
        };
        assert!(valid_rejection.verify_rejection_invariants().is_ok());

        let bad_rejection_id = PlacementRejection {
            node_id: String::new(),
            reason: "residency constraint",
        };
        assert!(bad_rejection_id.verify_rejection_invariants().is_err());

        let bad_rejection_reason = PlacementRejection {
            node_id: "node-2".into(),
            reason: "   ",
        };
        assert!(bad_rejection_reason.verify_rejection_invariants().is_err());
    }
}
