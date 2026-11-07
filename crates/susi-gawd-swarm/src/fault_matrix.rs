//! Multi-node federation fault matrix (VC-201-040).
//!
//! Drives the `ChaosNetwork` election/commit model plus the real
//! `IdentityStore` quorum gate through the failure catalogue: partition,
//! clock skew (term drift), duplicate delivery, stale keys, failed
//! rekeys, mixed versions, and crash recovery. Every scenario archives
//! its step trace into the returned report, and guarantees the matrix
//! does *not* provide are enumerated explicitly — a silent gap is
//! worse than a documented one.

use crate::identity_revoke::{IdentityStore, KeyMaterial};
use crate::swarm_chaos::{ChaosNetwork, ChaosNode, CommitRecord};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FaultKind {
    Partition,
    ClockSkew,
    DuplicateDelivery,
    StaleKey,
    FailedRekey,
    MixedVersion,
    CrashRecovery,
}

impl FaultKind {
    /// Verifies that the fault kind variant is well-formed.
    ///
    /// # Errors
    ///
    /// Returns `Ok(())` for all defined variants.
    pub fn verify_kind_invariants(&self) -> Result<(), String> {
        match self {
            Self::Partition
            | Self::ClockSkew
            | Self::DuplicateDelivery
            | Self::StaleKey
            | Self::FailedRekey
            | Self::MixedVersion
            | Self::CrashRecovery => Ok(()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioResult {
    pub kind: FaultKind,
    pub injected: String,
    pub expected_guard: String,
    pub passed: bool,
    /// Ordered steps — archived so a failed guard reproduces offline.
    pub trace: Vec<String>,
}

impl ScenarioResult {
    /// Verifies invariants of a fault matrix scenario result.
    ///
    /// # Errors
    ///
    /// Returns a descriptive error string if `kind` fails invariants, if `injected` or
    /// `expected_guard` is blank, or if `trace` is empty or contains blank step entries.
    pub fn verify_scenario_invariants(&self) -> Result<(), String> {
        self.kind.verify_kind_invariants()?;
        if self.injected.trim().is_empty() {
            return Err("ScenarioResult.injected must not be blank".to_string());
        }
        if self.expected_guard.trim().is_empty() {
            return Err("ScenarioResult.expected_guard must not be blank".to_string());
        }
        if self.trace.is_empty() {
            return Err("ScenarioResult.trace must not be empty".to_string());
        }
        if self.trace.iter().any(|step| step.trim().is_empty()) {
            return Err("ScenarioResult.trace must not contain blank entries".to_string());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatrixReport {
    pub scenarios: Vec<ScenarioResult>,
    /// Failure modes the federation explicitly does NOT survive —
    /// verified facts, not aspirations.
    pub unsupported: Vec<String>,
}

impl MatrixReport {
    /// Verifies invariants of a federation fault matrix report.
    ///
    /// # Errors
    ///
    /// Returns a descriptive error string if `scenarios` is empty, if any scenario fails
    /// [`ScenarioResult::verify_scenario_invariants`], or if `unsupported` is empty or contains
    /// blank entries.
    pub fn verify_report_invariants(&self) -> Result<(), String> {
        if self.scenarios.is_empty() {
            return Err("MatrixReport.scenarios must not be empty".to_string());
        }
        for scenario in &self.scenarios {
            scenario.verify_scenario_invariants()?;
        }
        if self.unsupported.is_empty() {
            return Err("MatrixReport.unsupported must not be empty".to_string());
        }
        if self.unsupported.iter().any(|u| u.trim().is_empty()) {
            return Err("MatrixReport.unsupported must not contain blank entries".to_string());
        }
        Ok(())
    }

    #[must_use]
    pub fn all_passed(&self) -> bool {
        self.scenarios.iter().all(|s| s.passed)
    }
}

/// Write the report (with traces) as JSON for archival.
pub fn save_report(path: &Path, report: &MatrixReport) -> std::io::Result<()> {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    let json = serde_json::to_string_pretty(report)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(path, json)
}

fn three_node_net() -> (ChaosNetwork, Vec<(String, f32)>) {
    let mut net = ChaosNetwork::default();
    for id in ["a", "b", "c"] {
        net.add(id);
    }
    let roster: Vec<(String, f32)> = vec![("a".into(), 1.0), ("b".into(), 0.9), ("c".into(), 0.8)];
    (net, roster)
}

fn elect_a(net: &mut ChaosNetwork, roster: &[(String, f32)]) {
    for n in net.nodes.values_mut() {
        n.elect(roster);
    }
}

fn scenario_partition() -> ScenarioResult {
    let mut trace = Vec::new();
    let (mut net, roster) = three_node_net();
    elect_a(&mut net, &roster);
    net.partition("c", true);
    trace.push("leader=a, c partitioned".into());
    let rec = net.nodes["a"].propose_commit(1, "x");
    let Some(rec) = rec else {
        return ScenarioResult {
            kind: FaultKind::Partition,
            injected: "leader elected then c partitioned".into(),
            expected_guard: "c misses the commit, converges after heal".into(),
            passed: false,
            trace,
        };
    };
    net.broadcast("a", rec.clone(), false);
    let missed = !net.nodes["c"].log.contains(&rec);
    net.partition("c", false);
    net.flush_delayed();
    let converged = net.nodes["c"].log.contains(&rec);
    trace.push(format!(
        "missed_during_partition={missed} converged_after_heal={converged}"
    ));
    ScenarioResult {
        kind: FaultKind::Partition,
        injected: "partition drops delivery; heal flushes delayed records".into(),
        expected_guard: "no commit lost across a healed partition".into(),
        passed: missed && converged && net.assert_no_split_brain_commit().is_ok(),
        trace,
    }
}

fn scenario_clock_skew() -> ScenarioResult {
    // A skewed node's term runs ahead — stale-term records must be
    // rejected even when they look current to a slow node.
    let mut trace = Vec::new();
    let (mut net, roster) = three_node_net();
    elect_a(&mut net, &roster);
    if let Some(n) = net.nodes.get_mut("b") {
        n.term = 99;
    }
    trace.push("b term skewed to 99".into());
    let rec = CommitRecord {
        term: 1,
        leader: "a".into(),
        seq: 1,
        payload: "x".into(),
    };
    let accepted = net
        .nodes
        .get_mut("b")
        .map(|n| n.accept(rec))
        .unwrap_or(false);
    trace.push(format!("stale-term record accepted={accepted}"));
    ScenarioResult {
        kind: FaultKind::ClockSkew,
        injected: "node term ahead of cluster".into(),
        expected_guard: "record under an older term is refused".into(),
        passed: !accepted,
        trace,
    }
}

fn scenario_duplicate() -> ScenarioResult {
    let mut trace = Vec::new();
    let (mut net, roster) = three_node_net();
    elect_a(&mut net, &roster);
    let rec = net.nodes["a"]
        .propose_commit(1, "x")
        .unwrap_or_else(|| CommitRecord {
            term: 0,
            leader: "a".into(),
            seq: 0,
            payload: String::new(),
        });
    net.broadcast("a", rec.clone(), true);
    let b_count = net.nodes["b"]
        .log
        .iter()
        .filter(|r| r.seq == rec.seq && r.term == rec.term)
        .count();
    trace.push(format!("b holds {b_count} copies after duplicate delivery"));
    ScenarioResult {
        kind: FaultKind::DuplicateDelivery,
        injected: "same record delivered twice".into(),
        expected_guard: "accept is idempotent — one copy lands".into(),
        passed: b_count == 1 && net.assert_no_split_brain_commit().is_ok(),
        trace,
    }
}

fn scenario_stale_key() -> ScenarioResult {
    // A record signed under a retired key id must be refused after
    // rotation even though the signature is well-formed.
    let mut trace = Vec::new();
    let mut store = IdentityStore::default();
    let electorate = BTreeSet::from(["a".to_string(), "b".to_string(), "c".to_string()]);
    let quorum = BTreeSet::from(["a".to_string(), "b".to_string()]);
    let _ = store.rotate(
        "p1",
        KeyMaterial {
            peer_id: "p1".into(),
            key_id: "k-old".into(),
            public: "pk-old".into(),
        },
        &quorum,
        &electorate,
    );
    let _ = store.rotate(
        "p1",
        KeyMaterial {
            peer_id: "p1".into(),
            key_id: "k-new".into(),
            public: "pk-new".into(),
        },
        &quorum,
        &electorate,
    );
    let stale_ok = store.credential_ok_after_restart("k-old");
    let current_ok = store.credential_ok_after_restart("k-new");
    trace.push(format!(
        "after rotation: k-old ok={stale_ok}, k-new ok={current_ok}"
    ));
    ScenarioResult {
        kind: FaultKind::StaleKey,
        injected: "credential under retired key id presented".into(),
        expected_guard: "only the current key id admits".into(),
        passed: !stale_ok && current_ok,
        trace,
    }
}

fn scenario_failed_rekey() -> ScenarioResult {
    // Rotation without quorum must not take effect — the old key
    // remains authoritative.
    let mut trace = Vec::new();
    let mut store = IdentityStore::default();
    let electorate = BTreeSet::from(["a".to_string(), "b".to_string(), "c".to_string()]);
    let _ = store.rotate(
        "p1",
        KeyMaterial {
            peer_id: "p1".into(),
            key_id: "k-old".into(),
            public: "pk-old".into(),
        },
        &BTreeSet::from(["a".to_string(), "b".to_string()]),
        &electorate,
    );
    let failed = store.rotate(
        "p1",
        KeyMaterial {
            peer_id: "p1".into(),
            key_id: "k-usurper".into(),
            public: "pk-usurper".into(),
        },
        &BTreeSet::from(["a".to_string()]), // 1 of 3 — below quorum
        &electorate,
    );
    let usurper_ok = store.credential_ok_after_restart("k-usurper");
    let old_ok = store.credential_ok_after_restart("k-old");
    trace.push(format!(
        "failed rekey: rotate err={}, usurper ok={usurper_ok}, old ok={old_ok}",
        failed.is_err()
    ));
    ScenarioResult {
        kind: FaultKind::FailedRekey,
        injected: "rotation attempted with a below-quorum voter set".into(),
        expected_guard: "rotation refused; prior key still authoritative".into(),
        passed: failed.is_err() && !usurper_ok && old_ok,
        trace,
    }
}

fn scenario_mixed_version() -> ScenarioResult {
    // An old-version node ignores record kinds it cannot parse instead
    // of crashing — it still converges on the kinds it knows.
    let mut trace = Vec::new();
    let (mut net, roster) = three_node_net();
    elect_a(&mut net, &roster);
    // A v2-only record lands at a v1 node: modeled as a payload the
    // node cannot interpret — it must skip it and keep the log valid.
    let rec = net.nodes["a"]
        .propose_commit(1, "v2:federated-snapshot-ref")
        .unwrap_or_else(|| CommitRecord {
            term: 0,
            leader: "a".into(),
            seq: 0,
            payload: String::new(),
        });
    net.broadcast("a", rec.clone(), false);
    let held = net.nodes["b"].log.contains(&rec);
    trace.push(format!("v1 node stored opaque record={held}"));
    // Opaque records append without interpretation — the v1 node does
    // not act on them, but convergence on record *presence* holds so a
    // v2 peer can later interpret them. Version negotiation before
    // action is an enumerated gap (see `unsupported`).
    ScenarioResult {
        kind: FaultKind::MixedVersion,
        injected: "record whose payload a v1 node cannot interpret".into(),
        expected_guard: "node ignores semantics, keeps log consistent".into(),
        passed: held && net.assert_no_split_brain_commit().is_ok(),
        trace,
    }
}

fn scenario_crash_recovery() -> ScenarioResult {
    // Crash = volatile state lost, durable log kept. A restarted node
    // resumes empty term/leader but retains its log and re-converges.
    let mut trace = Vec::new();
    let (mut net, roster) = three_node_net();
    elect_a(&mut net, &roster);
    if let Some(rec) = net.nodes["a"].propose_commit(1, "x") {
        net.broadcast("a", rec.clone(), false);
        // b "restarts": fresh volatile state over the same log.
        let saved_log = net.nodes["b"].log.clone();
        let mut fresh = ChaosNode::new("b");
        fresh.log = saved_log.clone();
        net.nodes.insert("b".into(), fresh);
        trace.push(format!(
            "b restarted: term={} leader='{}' log_len={}",
            net.nodes["b"].term,
            net.nodes["b"].leader,
            saved_log.len()
        ));
        if let Some(rec2) = net.nodes["a"].propose_commit(2, "y") {
            net.broadcast("a", rec2.clone(), false);
            let converged = net.nodes["b"].log.contains(&rec2) && net.nodes["b"].log.contains(&rec);
            ScenarioResult {
                kind: FaultKind::CrashRecovery,
                injected: "restart wipes volatile state; log persists".into(),
                expected_guard: "restarted node keeps history and reconverges".into(),
                passed: converged,
                trace,
            }
        } else {
            ScenarioResult {
                kind: FaultKind::CrashRecovery,
                injected: "restart".into(),
                expected_guard: "reconverges".into(),
                passed: false,
                trace,
            }
        }
    } else {
        ScenarioResult {
            kind: FaultKind::CrashRecovery,
            injected: "restart".into(),
            expected_guard: "reconverges".into(),
            passed: false,
            trace,
        }
    }
}

/// Run the whole matrix. Every scenario names its injected fault and
/// the guard expected to hold — a red row means a real gap, and the
/// `unsupported` list must stay honest rather than claiming coverage.
#[must_use]
pub fn run_matrix() -> MatrixReport {
    MatrixReport {
        scenarios: vec![
            scenario_partition(),
            scenario_clock_skew(),
            scenario_duplicate(),
            scenario_stale_key(),
            scenario_failed_rekey(),
            scenario_mixed_version(),
            scenario_crash_recovery(),
        ],
        unsupported: vec![
            // Delivery to a peer partitioned past the delayed-queue
            // bound is lost — catch-up repair, not the push path, heals it.
            "delivery to a node partitioned beyond the delayed-queue bound".into(),
            // A leaked *current* key forges valid signatures — detection
            // is behavioral (divergent term), not cryptographic.
            "forgery under a compromised current cluster key".into(),
            // Opaque-record presence converges, but a v1 node takes no
            // semantic action on v2 records — no cross-version execution.
            "semantic action on records newer than the node's version".into(),
        ],
    }
}

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod inline_tests {
    use super::*;

    #[test]
    fn run_matrix_executes_all_seven_fault_scenarios_and_passes_all_guards() {
        let report = run_matrix();
        assert_eq!(report.scenarios.len(), 7);
        assert!(report.all_passed());
        for scenario in &report.scenarios {
            assert!(scenario.passed);
            assert!(!scenario.trace.is_empty());
            assert!(!scenario.expected_guard.is_empty());
        }
    }

    #[test]
    fn run_matrix_enumerates_non_empty_unsupported_failure_modes() {
        let report = run_matrix();
        assert_eq!(report.unsupported.len(), 3);
        assert!(report
            .unsupported
            .iter()
            .any(|u| u.contains("compromised current cluster key")));
    }

    #[test]
    fn matrix_report_all_passed_returns_false_when_any_scenario_fails() {
        let mut report = run_matrix();
        assert!(report.all_passed());
        report.scenarios[0].passed = false;
        assert!(!report.all_passed());
    }

    #[test]
    fn save_report_writes_pretty_json_archive_to_nested_path() {
        let tmp = std::env::temp_dir()
            .join(format!("susi_fault_matrix_{}", std::process::id()))
            .join("nested")
            .join("report.json");
        let _ = std::fs::remove_dir_all(tmp.parent().unwrap().parent().unwrap());

        let report = run_matrix();
        save_report(&tmp, &report).expect("save report");
        let raw = std::fs::read_to_string(&tmp).expect("read saved report");
        let decoded: MatrixReport = serde_json::from_str(&raw).expect("parse saved report");
        assert_eq!(decoded.scenarios.len(), report.scenarios.len());
        assert!(decoded.all_passed());

        let _ = std::fs::remove_dir_all(tmp.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn run_matrix_covers_every_fault_kind_variant_in_expected_order() {
        let report = run_matrix();
        let kinds: Vec<FaultKind> = report.scenarios.into_iter().map(|s| s.kind).collect();
        assert_eq!(
            kinds,
            vec![
                FaultKind::Partition,
                FaultKind::ClockSkew,
                FaultKind::DuplicateDelivery,
                FaultKind::StaleKey,
                FaultKind::FailedRekey,
                FaultKind::MixedVersion,
                FaultKind::CrashRecovery,
            ]
        );
    }

    #[test]
    fn fault_kind_and_scenario_result_verify_swarm_wave14_invariants() {
        for kind in [
            FaultKind::Partition,
            FaultKind::ClockSkew,
            FaultKind::DuplicateDelivery,
            FaultKind::StaleKey,
            FaultKind::FailedRekey,
            FaultKind::MixedVersion,
            FaultKind::CrashRecovery,
        ] {
            assert!(kind.verify_kind_invariants().is_ok());
        }

        let valid = ScenarioResult {
            kind: FaultKind::Partition,
            injected: "partition c".into(),
            expected_guard: "converges after heal".into(),
            passed: true,
            trace: vec!["step 1".into(), "step 2".into()],
        };
        assert!(valid.verify_scenario_invariants().is_ok());

        let blank_injected = ScenarioResult {
            injected: "   ".into(),
            ..valid.clone()
        };
        assert!(blank_injected.verify_scenario_invariants().is_err());

        let blank_guard = ScenarioResult {
            expected_guard: String::new(),
            ..valid.clone()
        };
        assert!(blank_guard.verify_scenario_invariants().is_err());

        let empty_trace = ScenarioResult {
            trace: Vec::new(),
            ..valid.clone()
        };
        assert!(empty_trace.verify_scenario_invariants().is_err());

        let blank_trace_entry = ScenarioResult {
            trace: vec!["step 1".into(), "   ".into()],
            ..valid
        };
        assert!(blank_trace_entry.verify_scenario_invariants().is_err());
    }

    #[test]
    fn matrix_report_verify_swarm_wave14_invariants() {
        let report = run_matrix();
        assert!(report.verify_report_invariants().is_ok());

        let empty_scenarios = MatrixReport {
            scenarios: Vec::new(),
            ..report.clone()
        };
        assert!(empty_scenarios.verify_report_invariants().is_err());

        let empty_unsupported = MatrixReport {
            unsupported: Vec::new(),
            ..report.clone()
        };
        assert!(empty_unsupported.verify_report_invariants().is_err());

        let blank_unsupported = MatrixReport {
            unsupported: vec!["valid".into(), "   ".into()],
            ..report
        };
        assert!(blank_unsupported.verify_report_invariants().is_err());
    }
}
