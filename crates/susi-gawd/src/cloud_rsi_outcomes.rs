//! Verified-outcome learning for the cloud-assisted self-development loop
//! (T-CODEX-20 / VC-201-019).
//!
//! Feeds measured outcomes back into the evidence-ranked brain: per
//! (task-class, model) the ledger records promoted vs rejected patches,
//! exact gate/test receipts, regression metrics, latency and cost,
//! reviewer disagreement, and — kept strictly distinct — operational
//! availability failures (5xx/timeout/lockout).
//!
//! Invariants:
//! - `Promoted` outcomes are recorded only with a complete all-passing
//!   gate record and real receipts — self-reported success earns nothing.
//! - Rejection counterexamples persist; a repeated proposal from the same
//!   model on the same task class must reference them or bring fresh
//!   evidence, otherwise it is flagged `Stale`.
//! - Availability failures never lower a model's quality ranking — they
//!   only feed the nonworking-model exclusion at assignment time.
//! - Rankings never tune against held-out checks: only recorded,
//!   gate-verified outcomes move a score.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// How a candidate attempt concluded.
#[derive(Debug, Clone)]
pub enum Outcome {
    /// Gates all passed, work promoted — requires receipts + gates.
    Promoted {
        /// Candidate id for lineage.
        candidate_id: String,
        /// Test/tool receipts proving the gates ran.
        receipts: Vec<String>,
        /// Recorded gate results; all must pass.
        gates: Vec<(String, bool)>,
        /// Observed regression vs baseline (percent, may be negative).
        regression_pct: Option<f64>,
        /// Resource cost of the attempt.
        usage: OutcomeUsage,
    },
    /// Gates or review rejected the candidate — a persisted counterexample.
    Rejected {
        /// Candidate id.
        candidate_id: String,
        /// Rejection reason (recorded verbatim — future proposals must
        /// address it or bring fresh evidence).
        reason: String,
        /// Receipts backing the rejection.
        receipts: Vec<String>,
        /// Resource cost of the attempt.
        usage: OutcomeUsage,
    },
    /// Operational failure (5xx, timeout, lockout) — NOT a quality signal.
    AvailabilityFailure {
        /// What went operationally wrong.
        kind: String,
    },
    /// Independent reviewer contradicted the implementer's claim.
    ReviewerDisagreement {
        /// Reviewer model id.
        reviewer: String,
        /// Candidate id under dispute.
        candidate_id: String,
    },
}

/// Resource usage attached to an outcome.
#[derive(Debug, Clone, Default)]
pub struct OutcomeUsage {
    /// Tokens consumed.
    pub tokens: u64,
    /// Spend (micros).
    pub spend_micros: u64,
    /// Latency (ms).
    pub latency_ms: u64,
}

/// A persisted rejection counterexample.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Counterexample {
    /// Candidate that was rejected.
    pub candidate_id: String,
    /// Recorded rejection reason.
    pub reason: String,
    /// Receipts backing it.
    pub receipts: Vec<String>,
    /// When recorded (unix secs).
    pub at_unix: u64,
}

/// Per (task-class, model) verified-outcome record.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OutcomeRecord {
    /// Task class key.
    pub task_class: String,
    /// Opaque model id.
    pub model: String,
    /// Verified promotions.
    pub accepted: u32,
    /// Verified rejections.
    pub rejected: u32,
    /// Operational failures — kept separate from quality.
    pub avail_failures: u32,
    /// Reviewer disagreements involving this model's work.
    pub reviewer_disagreements: u32,
    /// Sum of per-sample regression percent (for averaging).
    pub regression_sum: f64,
    /// Samples carrying a regression measurement.
    pub regression_n: u32,
    /// Cumulative cost.
    pub tokens: u64,
    /// Cumulative spend (micros).
    pub spend_micros: u64,
    /// Cumulative latency (ms).
    pub latency_ms: u64,
    /// Persisted rejection counterexamples (bounded).
    pub counterexamples: Vec<Counterexample>,
}

impl OutcomeRecord {
    /// Verified quality score in [0,1] with Laplace smoothing; None when
    /// there is no quality evidence at all.
    #[must_use]
    pub fn quality(&self) -> Option<f64> {
        let n = self.accepted + self.rejected;
        if n == 0 {
            return None;
        }
        Some(f64::from(self.accepted + 1) / f64::from(n + 2))
    }
    /// Mean recorded regression percent over measured samples.
    #[must_use]
    pub fn mean_regression(&self) -> Option<f64> {
        (self.regression_n > 0).then(|| self.regression_sum / f64::from(self.regression_n))
    }
}

/// Why an outcome was refused.
#[derive(Debug, Clone, PartialEq)]
pub enum OutcomeError {
    /// Promoted without complete all-passing gates or receipts —
    /// self-reported success earns no credit.
    Unverified,
}

/// Check verdict for a repeated proposal from a model that has
/// counterexamples on file for this task class.
#[derive(Debug, Clone, PartialEq)]
pub enum ProposalCheck {
    /// No counterexamples on file — proceed.
    Fresh,
    /// Counterexamples exist and the proposal addresses them or brings
    /// fresh evidence — proceed.
    Addressed,
    /// Counterexamples exist and the proposal references none of them and
    /// cites no fresh receipts — must address prior failures first.
    Stale,
}

const MAX_COUNTEREXAMPLES: usize = 32;

/// Durable ledger: one JSON file per (task-class, model) record
/// (Mandate 51 — union on merge, never wholesale overwrite).
pub struct OutcomeLedger {
    dir: PathBuf,
    records: BTreeMap<(String, String), OutcomeRecord>,
    clock: fn() -> u64,
}

fn key(task_class: &str, model: &str) -> (String, String) {
    (task_class.to_string(), model.to_string())
}

fn file_name(task_class: &str, model: &str) -> String {
    let raw = format!("{task_class}__{model}");
    let safe: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{safe}.json")
}

impl OutcomeLedger {
    /// Open (or create) the ledger rooted at `dir`.
    #[must_use]
    pub fn load(dir: PathBuf, clock: fn() -> u64) -> Self {
        let mut records = BTreeMap::new();
        if let Ok(rd) = fs::read_dir(&dir) {
            for e in rd.flatten() {
                if e.path().extension().and_then(|x| x.to_str()) != Some("json") {
                    continue;
                }
                if let Ok(body) = fs::read_to_string(e.path()) {
                    if let Ok(rec) = serde_json::from_str::<OutcomeRecord>(&body) {
                        records.insert(key(&rec.task_class, &rec.model), rec);
                    }
                }
            }
        }
        Self {
            dir,
            records,
            clock,
        }
    }

    fn persist(&self, rec: &OutcomeRecord) {
        let _ = fs::create_dir_all(&self.dir);
        if let Ok(body) = serde_json::to_string_pretty(rec) {
            let _ = fs::write(self.dir.join(file_name(&rec.task_class, &rec.model)), body);
        }
    }

    /// Record a verified outcome. `Promoted` without complete all-passing
    /// gates AND receipts is refused — self-reported success is not
    /// evidence.
    pub fn record(
        &mut self,
        task_class: &str,
        model: &str,
        outcome: Outcome,
    ) -> Result<(), OutcomeError> {
        if let Outcome::Promoted {
            receipts, gates, ..
        } = &outcome
        {
            if receipts.is_empty() || gates.is_empty() || gates.iter().any(|(_, ok)| !ok) {
                return Err(OutcomeError::Unverified);
            }
        }
        let rec = self
            .records
            .entry(key(task_class, model))
            .or_insert_with(|| OutcomeRecord {
                task_class: task_class.to_string(),
                model: model.to_string(),
                ..Default::default()
            });
        match outcome {
            Outcome::Promoted {
                candidate_id: _,
                receipts: _,
                gates: _,
                regression_pct,
                usage,
            } => {
                rec.accepted += 1;
                if let Some(r) = regression_pct {
                    rec.regression_sum += r;
                    rec.regression_n += 1;
                }
                rec.tokens = rec.tokens.saturating_add(usage.tokens);
                rec.spend_micros = rec.spend_micros.saturating_add(usage.spend_micros);
                rec.latency_ms = rec.latency_ms.saturating_add(usage.latency_ms);
            }
            Outcome::Rejected {
                candidate_id,
                reason,
                receipts,
                usage,
            } => {
                rec.rejected += 1;
                rec.tokens = rec.tokens.saturating_add(usage.tokens);
                rec.spend_micros = rec.spend_micros.saturating_add(usage.spend_micros);
                rec.latency_ms = rec.latency_ms.saturating_add(usage.latency_ms);
                rec.counterexamples.push(Counterexample {
                    candidate_id,
                    reason,
                    receipts,
                    at_unix: (self.clock)(),
                });
                // Keep the newest MAX_COUNTEREXAMPLES, not the oldest: a
                // plain `truncate` keeps index 0..cap, which drops every
                // fresh rejection once the cap is reached and the ledger
                // stops learning from anything new.
                if rec.counterexamples.len() > MAX_COUNTEREXAMPLES {
                    let excess = rec.counterexamples.len() - MAX_COUNTEREXAMPLES;
                    rec.counterexamples.drain(0..excess);
                }
            }
            Outcome::AvailabilityFailure { kind: _ } => {
                rec.avail_failures += 1;
            }
            Outcome::ReviewerDisagreement {
                reviewer: _,
                candidate_id: _,
            } => {
                rec.reviewer_disagreements += 1;
            }
        }
        let snap = rec.clone();
        self.persist(&snap);
        Ok(())
    }

    /// Record for a (class, model) pair.
    #[must_use]
    pub fn get(&self, task_class: &str, model: &str) -> Option<&OutcomeRecord> {
        self.records.get(&key(task_class, model))
    }

    /// Rank `candidates` for assignment on `task_class`: currently-working
    /// models first (via `is_working`, fed by the eligibility store), then
    /// verified quality descending — models with no quality evidence sit
    /// between verified-good and verified-bad, tie-broken by mean
    /// regression then total spend.
    #[must_use]
    pub fn rank_for_assignment(
        &self,
        task_class: &str,
        candidates: &[String],
        is_working: &dyn Fn(&str) -> bool,
    ) -> Vec<String> {
        let mut ranked: Vec<String> = candidates.to_vec();
        ranked.sort_by(|a, b| {
            let ra = self.get(task_class, a);
            let rb = self.get(task_class, b);
            let wa = is_working(a);
            let wb = is_working(b);
            // Nonworking models always sort last.
            wb.cmp(&wa).then_with(|| {
                let qa = ra.and_then(|r| r.quality());
                let qb = rb.and_then(|r| r.quality());
                // None (no evidence) sorts between good and bad: treat as 0.5.
                let qa = qa.unwrap_or(0.5);
                let qb = qb.unwrap_or(0.5);
                qb.partial_cmp(&qa)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| {
                        let ma = ra.and_then(|r| r.mean_regression()).unwrap_or(0.0);
                        let mb = rb.and_then(|r| r.mean_regression()).unwrap_or(0.0);
                        ma.partial_cmp(&mb).unwrap_or(std::cmp::Ordering::Equal)
                    })
                    .then_with(|| {
                        ra.map(|r| r.spend_micros)
                            .unwrap_or(0)
                            .cmp(&rb.map(|r| r.spend_micros).unwrap_or(0))
                    })
            })
        });
        ranked
    }

    /// A proposal from `model` on `task_class` that ignores recorded
    /// counterexamples without fresh evidence is `Stale`.
    #[must_use]
    pub fn check_proposal(
        &self,
        task_class: &str,
        model: &str,
        addressed: &[String],
        fresh_receipts: &[String],
    ) -> ProposalCheck {
        let Some(rec) = self.get(task_class, model) else {
            return ProposalCheck::Fresh;
        };
        if rec.counterexamples.is_empty() {
            return ProposalCheck::Fresh;
        }
        let referenced = rec
            .counterexamples
            .iter()
            .any(|c| addressed.iter().any(|a| a == &c.candidate_id));
        // A blank string is not evidence: require at least one
        // non-whitespace receipt, not merely a non-empty list.
        let has_fresh_evidence = fresh_receipts.iter().any(|r| !r.trim().is_empty());
        if referenced || has_fresh_evidence {
            ProposalCheck::Addressed
        } else {
            ProposalCheck::Stale
        }
    }
}

/// Production wiring: the outcome ledger is the verified-outcome ranking
/// source for the default brain policy.
impl susi_gawd_agents::cloud_brain_policy::OutcomeRanking for OutcomeLedger {
    fn rank(
        &self,
        task_class: &str,
        models: &[String],
        is_working: &dyn Fn(&str) -> bool,
    ) -> Vec<String> {
        self.rank_for_assignment(task_class, models, is_working)
    }
    fn quality(&self, task_class: &str, model: &str) -> Option<f64> {
        self.get(task_class, model).and_then(|r| r.quality())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> u64 {
        1_700_000_000
    }

    fn ledger(tag: &str) -> (PathBuf, OutcomeLedger) {
        let dir = std::env::temp_dir().join(format!("oc-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        (dir.clone(), OutcomeLedger::load(dir, now))
    }

    fn gates_ok() -> Vec<(String, bool)> {
        vec![("accept".into(), true), ("clippy".into(), true)]
    }

    fn usage() -> OutcomeUsage {
        OutcomeUsage {
            tokens: 100,
            spend_micros: 10,
            latency_ms: 50,
        }
    }

    fn promoted(id: &str) -> Outcome {
        Outcome::Promoted {
            candidate_id: id.into(),
            receipts: vec!["cargo test -> ok".into()],
            gates: gates_ok(),
            regression_pct: Some(-2.0),
            usage: usage(),
        }
    }

    #[test]
    fn cloud_rsi_outcomes_verified_promotion_raises_ranking() {
        let (_d, mut l) = ledger("rank");
        for i in 0..3 {
            l.record("patch", "m-good", promoted(&format!("C-{i}")))
                .unwrap();
        }
        let ranked = l.rank_for_assignment(
            "patch",
            &["m-new".to_string(), "m-good".to_string()],
            &|_| true,
        );
        assert_eq!(ranked[0], "m-good");
        let _ = fs::remove_dir_all(_d);
    }

    #[test]
    fn cloud_rsi_outcomes_self_reported_success_is_refused() {
        let (_d, mut l) = ledger("selfreport");
        // Promoted with no receipts → refused.
        let bad = Outcome::Promoted {
            candidate_id: "C-1".into(),
            receipts: vec![],
            gates: gates_ok(),
            regression_pct: None,
            usage: usage(),
        };
        assert_eq!(l.record("patch", "m1", bad), Err(OutcomeError::Unverified));
        // Promoted with a failing gate → refused.
        let bad2 = Outcome::Promoted {
            candidate_id: "C-2".into(),
            receipts: vec!["t".into()],
            gates: vec![("accept".into(), false)],
            regression_pct: None,
            usage: usage(),
        };
        assert_eq!(l.record("patch", "m1", bad2), Err(OutcomeError::Unverified));
        // Nothing recorded.
        assert!(l.get("patch", "m1").is_none());
        let _ = fs::remove_dir_all(_d);
    }

    #[test]
    fn cloud_rsi_outcomes_availability_is_distinct_from_quality() {
        let (_d, mut l) = ledger("avail");
        l.record("patch", "m-flaky", promoted("C-1")).unwrap();
        l.record(
            "patch",
            "m-flaky",
            Outcome::AvailabilityFailure {
                kind: "http_503".into(),
            },
        )
        .unwrap();
        l.record(
            "patch",
            "m-flaky",
            Outcome::AvailabilityFailure {
                kind: "timeout".into(),
            },
        )
        .unwrap();
        let rec = l.get("patch", "m-flaky").unwrap();
        assert_eq!(rec.avail_failures, 2);
        // Quality unaffected by operational failures.
        assert!(rec.quality().unwrap() > 0.6);
        // But a nonworking model still sorts last at assignment time.
        let ranked = l.rank_for_assignment(
            "patch",
            &["m-flaky".to_string(), "m-solid".to_string()],
            &|m| m != "m-flaky",
        );
        assert_eq!(ranked, vec!["m-solid".to_string(), "m-flaky".to_string()]);
        let _ = fs::remove_dir_all(_d);
    }

    #[test]
    fn cloud_rsi_outcomes_rejected_earns_no_success_and_persists_counterexample() {
        let (dir, mut l) = ledger("reject");
        l.record(
            "patch",
            "m-bad",
            Outcome::Rejected {
                candidate_id: "C-9".into(),
                reason: "broke parser".into(),
                receipts: vec!["cargo test -> FAIL".into()],
                usage: usage(),
            },
        )
        .unwrap();
        let rec = l.get("patch", "m-bad").unwrap();
        assert_eq!(rec.accepted, 0);
        assert_eq!(rec.rejected, 1);
        assert!(rec.quality().unwrap() < 0.5);
        assert_eq!(rec.counterexamples.len(), 1);
        assert_eq!(rec.counterexamples[0].reason, "broke parser");
        // Persisted across restart.
        let l2 = OutcomeLedger::load(dir.clone(), now);
        assert_eq!(l2.get("patch", "m-bad").unwrap().counterexamples.len(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cloud_rsi_outcomes_repeated_proposal_must_address_counterexamples() {
        let (_d, mut l) = ledger("repeat");
        l.record(
            "patch",
            "m1",
            Outcome::Rejected {
                candidate_id: "C-9".into(),
                reason: "broke parser".into(),
                receipts: vec!["r".into()],
                usage: usage(),
            },
        )
        .unwrap();
        // Same idea, no reference, no fresh evidence → Stale.
        assert_eq!(
            l.check_proposal("patch", "m1", &[], &[]),
            ProposalCheck::Stale
        );
        // Referencing the counterexample → Addressed.
        assert_eq!(
            l.check_proposal("patch", "m1", &["C-9".into()], &[]),
            ProposalCheck::Addressed
        );
        // Or bringing fresh evidence → Addressed.
        assert_eq!(
            l.check_proposal("patch", "m1", &[], &["new-trace".into()]),
            ProposalCheck::Addressed
        );
        // A model with no failures → Fresh.
        assert_eq!(
            l.check_proposal("patch", "m2", &[], &[]),
            ProposalCheck::Fresh
        );
        let _ = fs::remove_dir_all(_d);
    }

    #[test]
    fn cloud_rsi_outcomes_reviewer_disagreement_and_metrics_tracked() {
        let (_d, mut l) = ledger("metrics");
        l.record("patch", "m1", promoted("C-1")).unwrap();
        l.record(
            "patch",
            "m1",
            Outcome::ReviewerDisagreement {
                reviewer: "m-rev".into(),
                candidate_id: "C-2".into(),
            },
        )
        .unwrap();
        let rec = l.get("patch", "m1").unwrap();
        assert_eq!(rec.reviewer_disagreements, 1);
        assert_eq!(rec.tokens, 100);
        assert_eq!(rec.spend_micros, 10);
        assert_eq!(rec.latency_ms, 50);
        assert_eq!(rec.mean_regression(), Some(-2.0));
        let _ = fs::remove_dir_all(_d);
    }

    #[test]
    fn cloud_rsi_outcomes_rejected_never_outranks_verified() {
        let (_d, mut l) = ledger("order");
        l.record("patch", "m-win", promoted("C-1")).unwrap();
        l.record(
            "patch",
            "m-lose",
            Outcome::Rejected {
                candidate_id: "C-2".into(),
                reason: "bad".into(),
                receipts: vec!["r".into()],
                usage: usage(),
            },
        )
        .unwrap();
        let ranked = l.rank_for_assignment(
            "patch",
            &["m-lose".to_string(), "m-win".to_string()],
            &|_| true,
        );
        assert_eq!(ranked, vec!["m-win".to_string(), "m-lose".to_string()]);
        let _ = fs::remove_dir_all(_d);
    }
}
