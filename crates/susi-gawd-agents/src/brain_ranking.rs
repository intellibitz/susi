//! Comparable verified ranking of cloud brains per task class
//! (T-CODEX-25 / VC-201-011).
//!
//! Evidence model:
//! - Capability is ranked from **held-out evaluation** and **verified
//!   production outcomes** — never from `SelfReported` provenance, never
//!   from model name, price or vendor claims.
//! - Evidence is keyed by `(task_class, model, version)`: a new version
//!   inherits nothing — it starts unproven until its own evidence lands.
//! - Three capability dimensions are scored distinctly: `Correctness`,
//!   `ToolReliability`, `ContextCapability`. Missing dimensions count as
//!   *unproven* (0.5), not as good or bad.
//! - Confidence shrinks a dimension's score toward 0.5 by
//!   `n/(n+K)`: a single lucky sample never outranks a well-measured
//!   model. Stale evidence (older than `stale_secs`) is ignored.
//! - Availability/cost/latency are NOT capability — they gate through the
//!   caller's `is_working` predicate (a failed or unfunded model can never
//!   lead, whatever its history) and are excluded from the score.
//! - Ordering is deterministic: score desc, then confidence desc, then
//!   model+version lexicographic.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Confidence shrinkage constant — samples needed to fully trust a mean.
const CONFIDENCE_K: u32 = 4;
/// Neutral score for an unmeasured dimension.
const NEUTRAL: f64 = 0.5;
/// Dimension weights for the composite capability score.
const W_CORRECTNESS: f64 = 0.5;
const W_TOOL: f64 = 0.3;
const W_CONTEXT: f64 = 0.2;
/// Max evidence entries per (class, model, version) record.
const MAX_ENTRIES: usize = 64;

/// Which capability a measurement covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum EvalDimension {
    /// Answer correctness on held-out tasks.
    Correctness,
    /// Tool-use / function-calling reliability.
    ToolReliability,
    /// Effective context capability (long-context tasks).
    ContextCapability,
}

/// Where a measurement came from. `SelfReported` is stored for audit but
/// contributes **zero** to ranking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EvalProvenance {
    /// Authorized held-out evaluation run.
    HeldOut,
    /// Gate-verified production outcome (acceptance/clippy/tests).
    VerifiedOutcome,
    /// Provider or model self-report — never trusted for ranking.
    SelfReported,
}

/// One capability measurement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalEvidence {
    /// Task class (`coding`, `math`, `reasoning`, …).
    pub task_class: String,
    /// Model id.
    pub model: String,
    /// Model version/snapshot — evidence is version-scoped.
    pub version: String,
    /// Which capability was measured.
    pub dimension: EvalDimension,
    /// Measured score 0..=1 on the held-out/verified workload.
    pub score: f64,
    /// Number of samples behind `score`.
    pub samples: u32,
    /// When observed (unix secs).
    pub observed_unix: u64,
    /// Evidence provenance.
    pub provenance: EvalProvenance,
}

/// A model's ranked position for a task class.
#[derive(Debug, Clone, PartialEq)]
pub struct RankedBrain {
    /// Model id.
    pub model: String,
    /// Version ranked.
    pub version: String,
    /// Composite capability score 0..1 (confidence-shrunk).
    pub score: f64,
    /// Overall confidence 0..1 (coverage + sample volume).
    pub confidence: f64,
    /// Evidence entries counted.
    pub evidence_n: u32,
    /// Currently eligible to lead (working + authorized).
    pub working: bool,
}

/// Persistent ranker: one JSON file per (task_class, model, version)
/// record — union-mergeable shared state.
pub struct BrainRankStore {
    dir: PathBuf,
    /// (task_class, model, version) -> evidence
    records: BTreeMap<(String, String, String), Vec<EvalEvidence>>,
    /// Evidence older than this many secs is ignored.
    pub stale_secs: u64,
}

fn file_name(task_class: &str, model: &str, version: &str) -> String {
    let raw = format!("{task_class}__{model}__{version}");
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

impl BrainRankStore {
    /// Open (or create) the store at `dir`.
    #[must_use]
    pub fn load(dir: PathBuf, stale_secs: u64) -> Self {
        let mut records: BTreeMap<(String, String, String), Vec<EvalEvidence>> = BTreeMap::new();
        if let Ok(rd) = fs::read_dir(&dir) {
            for e in rd.flatten() {
                if e.path().extension().and_then(|x| x.to_str()) != Some("json") {
                    continue;
                }
                if let Ok(body) = fs::read_to_string(e.path()) {
                    if let Ok(v) = serde_json::from_str::<Vec<EvalEvidence>>(&body) {
                        if let Some(first) = v.first() {
                            records.insert(
                                (
                                    first.task_class.clone(),
                                    first.model.clone(),
                                    first.version.clone(),
                                ),
                                v,
                            );
                        }
                    }
                }
            }
        }
        Self {
            dir,
            records,
            stale_secs,
        }
    }

    fn persist(&self, ev: &EvalEvidence) {
        let _ = fs::create_dir_all(&self.dir);
        let k = (ev.task_class.clone(), ev.model.clone(), ev.version.clone());
        if let Some(v) = self.records.get(&k) {
            if let Ok(body) = serde_json::to_string_pretty(v) {
                let _ = fs::write(
                    self.dir
                        .join(file_name(&ev.task_class, &ev.model, &ev.version)),
                    body,
                );
            }
        }
    }

    /// Record one measurement. Self-reported evidence is kept for audit
    /// but never affects ranking.
    pub fn record(&mut self, ev: EvalEvidence) {
        let k = (ev.task_class.clone(), ev.model.clone(), ev.version.clone());
        let v = self.records.entry(k).or_default();
        v.push(ev.clone());
        v.truncate(MAX_ENTRIES);
        self.persist(&ev);
    }

    /// Freshness + provenance gate for an entry at `now`.
    fn countable(&self, ev: &EvalEvidence, now: u64) -> bool {
        match ev.provenance {
            EvalProvenance::SelfReported => false,
            EvalProvenance::HeldOut | EvalProvenance::VerifiedOutcome => {
                now.saturating_sub(ev.observed_unix) <= self.stale_secs
            }
        }
    }

    /// Confidence-shrunk score for one dimension of one model/version.
    fn dim_score(
        &self,
        task_class: &str,
        pair: &(String, String),
        dim: EvalDimension,
        now: u64,
    ) -> (f64, u32) {
        let Some(v) = self
            .records
            .get(&(task_class.to_string(), pair.0.clone(), pair.1.clone()))
        else {
            return (NEUTRAL, 0);
        };
        let mut sum = 0.0;
        let mut n = 0u32;
        for e in v
            .iter()
            .filter(|e| e.dimension == dim && self.countable(e, now))
        {
            // weight each observation by its sample count
            sum += e.score.clamp(0.0, 1.0) * f64::from(e.samples.max(1));
            n += e.samples.max(1);
        }
        if n == 0 {
            return (NEUTRAL, 0);
        }
        let mean = sum / f64::from(n);
        let shrink = f64::from(n) / f64::from(n + CONFIDENCE_K);
        (NEUTRAL + (mean - NEUTRAL) * shrink, n)
    }

    /// Rank `candidates` for `task_class`. `is_working` is the availability
    /// gate (fresh eligibility + quota); nonworking models sort last.
    #[must_use]
    pub fn rank(
        &self,
        task_class: &str,
        candidates: &[(String, String)],
        is_working: &dyn Fn(&str, &str) -> bool,
        now: u64,
    ) -> Vec<RankedBrain> {
        let mut out: Vec<RankedBrain> = candidates
            .iter()
            .map(|pair| {
                let (model, version) = pair;
                let (c, n_c) = self.dim_score(task_class, pair, EvalDimension::Correctness, now);
                let (t, n_t) =
                    self.dim_score(task_class, pair, EvalDimension::ToolReliability, now);
                let (x, n_x) =
                    self.dim_score(task_class, pair, EvalDimension::ContextCapability, now);
                let score = c * W_CORRECTNESS + t * W_TOOL + x * W_CONTEXT;
                let measured = n_c + n_t + n_x;
                // Confidence: dimension coverage (0..3) folded with volume.
                let dims_measured = u32::from(n_c > 0) + u32::from(n_t > 0) + u32::from(n_x > 0);
                let confidence = (f64::from(dims_measured) / 3.0)
                    * (f64::from(measured) / f64::from(measured + CONFIDENCE_K));
                RankedBrain {
                    model: model.clone(),
                    version: version.clone(),
                    score,
                    confidence,
                    evidence_n: measured,
                    working: is_working(model, version),
                }
            })
            .collect();
        out.sort_by(|a, b| {
            // Working models always lead; then score desc; then confidence
            // desc; then a deterministic name+version tiebreak.
            b.working
                .cmp(&a.working)
                .then(
                    b.score
                        .partial_cmp(&a.score)
                        .unwrap_or(std::cmp::Ordering::Equal),
                )
                .then(
                    b.confidence
                        .partial_cmp(&a.confidence)
                        .unwrap_or(std::cmp::Ordering::Equal),
                )
                .then_with(|| a.model.cmp(&b.model).then(a.version.cmp(&b.version)))
        });
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: u64 = 1_700_000_000;

    fn store(tag: &str) -> (PathBuf, BrainRankStore) {
        let dir = std::env::temp_dir().join(format!("br-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        (dir.clone(), BrainRankStore::load(dir, 86_400))
    }

    fn ev(model: &str, version: &str, dim: EvalDimension, score: f64, n: u32) -> EvalEvidence {
        EvalEvidence {
            task_class: "reasoning".into(),
            model: model.into(),
            version: version.into(),
            dimension: dim,
            score,
            samples: n,
            observed_unix: T0,
            provenance: EvalProvenance::HeldOut,
        }
    }

    fn candidates(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(m, v)| (m.to_string(), v.to_string()))
            .collect()
    }

    /// Verified held-out evidence outranks a totally unknown model.
    #[test]
    fn powerful_cloud_brain_ranking_verified_beats_unknown() {
        let (_d, mut s) = store("ver");
        s.record(ev("m-proven", "v1", EvalDimension::Correctness, 0.9, 40));
        s.record(ev(
            "m-proven",
            "v1",
            EvalDimension::ToolReliability,
            0.85,
            40,
        ));
        s.record(ev(
            "m-proven",
            "v1",
            EvalDimension::ContextCapability,
            0.8,
            40,
        ));
        let r = s.rank(
            "reasoning",
            &candidates(&[("m-proven", "v1"), ("m-mystery", "v9")]),
            &|_, _| true,
            T0 + 60,
        );
        assert_eq!(r[0].model, "m-proven");
        assert!(r[0].score > r[1].score);
        assert_eq!(r[1].evidence_n, 0);
        let _ = fs::remove_dir_all(_d);
    }

    /// Self-reported scores never move the ranking.
    #[test]
    fn powerful_cloud_brain_ranking_self_reported_never_counts() {
        let (_d, mut s) = store("self");
        let mut boast = ev("m-boast", "v1", EvalDimension::Correctness, 1.0, 1000);
        boast.provenance = EvalProvenance::SelfReported;
        s.record(boast);
        s.record(ev("m-honest", "v1", EvalDimension::Correctness, 0.8, 50));
        let r = s.rank(
            "reasoning",
            &candidates(&[("m-boast", "v1"), ("m-honest", "v1")]),
            &|_, _| true,
            T0 + 60,
        );
        assert_eq!(r[0].model, "m-honest");
        // Boast's evidence_n counts only trusted entries.
        assert_eq!(r[1].score, NEUTRAL);
        let _ = fs::remove_dir_all(_d);
    }

    /// A new model version inherits nothing — it starts unproven.
    #[test]
    fn powerful_cloud_brain_ranking_version_change_resets_evidence() {
        let (_d, mut s) = store("ver2");
        s.record(ev("m-x", "v1", EvalDimension::Correctness, 0.95, 100));
        s.record(ev("m-x", "v1", EvalDimension::ToolReliability, 0.95, 100));
        s.record(ev("m-x", "v1", EvalDimension::ContextCapability, 0.95, 100));
        let r = s.rank(
            "reasoning",
            &candidates(&[("m-x", "v2"), ("m-y", "v1")]),
            &|_, _| true,
            T0 + 60,
        );
        // m-x v2 has zero evidence → neutral 0.5; m-y unproven also 0.5,
        // deterministic name tiebreak — m-x before m-y.
        assert_eq!(r[0].score, NEUTRAL);
        // Now give v2 real evidence — it must earn its rank.
        s.record(ev("m-x", "v2", EvalDimension::Correctness, 0.99, 60));
        s.record(ev("m-x", "v2", EvalDimension::ToolReliability, 0.99, 60));
        s.record(ev("m-x", "v2", EvalDimension::ContextCapability, 0.99, 60));
        let r2 = s.rank(
            "reasoning",
            &candidates(&[("m-x", "v2"), ("m-x", "v1")]),
            &|_, _| true,
            T0 + 60,
        );
        assert_eq!(r2[0].version, "v2");
        let _ = fs::remove_dir_all(_d);
    }

    /// Task-class-specific evidence: strong at math doesn't imply strong
    /// at coding.
    #[test]
    fn powerful_cloud_brain_ranking_is_task_class_specific() {
        let (_d, mut s) = store("class");
        let mut m = ev("m-math", "v1", EvalDimension::Correctness, 0.95, 50);
        m.task_class = "math".into();
        s.record(m);
        s.record(ev("m-code", "v1", EvalDimension::Correctness, 0.9, 50));
        let r = s.rank(
            "reasoning",
            &candidates(&[("m-math", "v1"), ("m-code", "v1")]),
            &|_, _| true,
            T0 + 60,
        );
        assert_eq!(r[0].model, "m-code");
        let _ = fs::remove_dir_all(_d);
    }

    /// Stale evidence is ignored — a model whose only evidence is old
    /// falls back to unproven.
    #[test]
    fn powerful_cloud_brain_ranking_stale_evidence_ignored() {
        let (_d, mut s) = store("stale");
        let mut old = ev("m-old", "v1", EvalDimension::Correctness, 0.99, 80);
        old.observed_unix = T0 - 100_000; // beyond 86_400s stale horizon
        s.record(old);
        s.record(ev("m-fresh", "v1", EvalDimension::Correctness, 0.7, 40));
        let r = s.rank(
            "reasoning",
            &candidates(&[("m-old", "v1"), ("m-fresh", "v1")]),
            &|_, _| true,
            T0,
        );
        assert_eq!(r[0].model, "m-fresh");
        let _ = fs::remove_dir_all(_d);
    }

    /// A currently failing/unfunded model never leads, whatever its
    /// historical superiority.
    #[test]
    fn powerful_cloud_brain_ranking_current_failure_excludes_leadership() {
        let (_d, mut s) = store("down");
        s.record(ev("m-king", "v1", EvalDimension::Correctness, 0.99, 100));
        s.record(ev(
            "m-king",
            "v1",
            EvalDimension::ToolReliability,
            0.99,
            100,
        ));
        s.record(ev(
            "m-king",
            "v1",
            EvalDimension::ContextCapability,
            0.99,
            100,
        ));
        s.record(ev("m-plain", "v1", EvalDimension::Correctness, 0.6, 40));
        let r = s.rank(
            "reasoning",
            &candidates(&[("m-king", "v1"), ("m-plain", "v1")]),
            &|m, _| m != "m-king", // king is unfunded right now
            T0 + 60,
        );
        assert_eq!(r[0].model, "m-plain");
        assert!(!r.iter().find(|b| b.model == "m-king").unwrap().working);
        let _ = fs::remove_dir_all(_d);
    }

    /// One lucky sample never outranks a well-measured model — confidence
    /// shrinkage keeps single-sample claims honest.
    #[test]
    fn powerful_cloud_brain_ranking_low_confidence_shrinks() {
        let (_d, mut s) = store("conf");
        s.record(ev("m-lucky", "v1", EvalDimension::Correctness, 1.0, 1));
        s.record(ev("m-lucky", "v1", EvalDimension::ToolReliability, 1.0, 1));
        s.record(ev(
            "m-lucky",
            "v1",
            EvalDimension::ContextCapability,
            1.0,
            1,
        ));
        s.record(ev("m-solid", "v1", EvalDimension::Correctness, 0.9, 60));
        s.record(ev("m-solid", "v1", EvalDimension::ToolReliability, 0.9, 60));
        s.record(ev(
            "m-solid",
            "v1",
            EvalDimension::ContextCapability,
            0.9,
            60,
        ));
        let r = s.rank(
            "reasoning",
            &candidates(&[("m-lucky", "v1"), ("m-solid", "v1")]),
            &|_, _| true,
            T0 + 60,
        );
        assert_eq!(r[0].model, "m-solid");
        assert!(r[1].confidence < r[0].confidence);
        let _ = fs::remove_dir_all(_d);
    }

    /// Deterministic tiebreak: identical evidence → name ordering.
    #[test]
    fn powerful_cloud_brain_ranking_deterministic_tiebreak() {
        let (_d, s) = store("tie");
        let r = s.rank(
            "reasoning",
            &candidates(&[("m-b", "v1"), ("m-a", "v1")]),
            &|_, _| true,
            T0,
        );
        assert_eq!(r[0].model, "m-a");
        let _ = fs::remove_dir_all(_d);
    }
}

// ---- Wave 13 (T-CLAUDE-411) structural invariant verifiers ----

impl EvalEvidence {
    /// One measurement is well formed: it names the task class, model and version,
    /// its score is finite in `0..=1`, and it rests on at least one sample.
    pub fn verify_evidence_invariants(&self) -> Result<(), String> {
        if self.task_class.trim().is_empty()
            || self.model.trim().is_empty()
            || self.version.trim().is_empty()
        {
            return Err("EvalEvidence task_class, model and version must not be empty".to_string());
        }
        if !self.score.is_finite() || !(0.0..=1.0).contains(&self.score) {
            return Err(format!(
                "EvalEvidence score {} must be finite in 0..=1",
                self.score
            ));
        }
        if self.samples == 0 {
            return Err("EvalEvidence must rest on at least one sample".to_string());
        }
        Ok(())
    }
}

impl RankedBrain {
    /// A ranked model names its model and version and carries a finite score and
    /// confidence, each in `0..=1`.
    pub fn verify_ranked_invariants(&self) -> Result<(), String> {
        if self.model.trim().is_empty() || self.version.trim().is_empty() {
            return Err("RankedBrain model and version must not be empty".to_string());
        }
        for (label, value) in [("score", self.score), ("confidence", self.confidence)] {
            if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                return Err(format!(
                    "RankedBrain {label} {value} must be finite in 0..=1"
                ));
            }
        }
        Ok(())
    }
}

/// A ranking is well formed: every entry verifies, the order is score-descending
/// then confidence-descending (the deterministic order the ranker emits), and no
/// model appears twice.
pub fn verify_ranking_invariants(ranked: &[RankedBrain]) -> Result<(), String> {
    for r in ranked {
        r.verify_ranked_invariants()?;
    }
    for pair in ranked.windows(2) {
        let (a, b) = (&pair[0], &pair[1]);
        let ordered = a.score > b.score || (a.score == b.score && a.confidence >= b.confidence);
        if !ordered {
            return Err(format!(
                "ranking is not score/confidence-descending at `{}` before `{}`",
                a.model, b.model
            ));
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    for r in ranked {
        if !seen.insert(r.model.as_str()) {
            return Err(format!("model `{}` is ranked more than once", r.model));
        }
    }
    Ok(())
}

#[cfg(test)]
mod wave13_verifier_tests {
    use super::*;

    #[test]
    fn evidence_ranked_and_ranking_order_verifiers_reject_corrupt_state() {
        let ev = EvalEvidence {
            task_class: "coding".to_string(),
            model: "m".to_string(),
            version: "v1".to_string(),
            dimension: EvalDimension::Correctness,
            score: 0.8,
            samples: 12,
            observed_unix: 1_700_000_000,
            provenance: EvalProvenance::VerifiedOutcome,
        };
        assert_eq!(ev.verify_evidence_invariants(), Ok(()));
        let mut out = ev.clone();
        out.score = 1.5;
        assert!(out.verify_evidence_invariants().is_err());
        let mut no_samples = ev;
        no_samples.samples = 0;
        assert!(no_samples.verify_evidence_invariants().is_err());

        let rb = |model: &str, score: f64, confidence: f64| RankedBrain {
            model: model.to_string(),
            version: "v1".to_string(),
            score,
            confidence,
            evidence_n: 4,
            working: true,
        };
        assert_eq!(rb("m", 0.7, 0.6).verify_ranked_invariants(), Ok(()));
        assert!(rb("m", 0.7, 1.4).verify_ranked_invariants().is_err());

        assert_eq!(
            verify_ranking_invariants(&[rb("a", 0.9, 0.8), rb("b", 0.9, 0.5), rb("c", 0.2, 0.9)]),
            Ok(())
        );
        assert!(verify_ranking_invariants(&[rb("a", 0.2, 0.9), rb("b", 0.9, 0.5)]).is_err());
        assert!(verify_ranking_invariants(&[rb("a", 0.9, 0.8), rb("a", 0.5, 0.5)]).is_err());
    }
}
