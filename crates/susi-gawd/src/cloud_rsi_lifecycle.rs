//! Persisted lifecycle for the cloud-assisted self-development loop
//! (T-CODEX-19 / VC-201-015).
//!
//! One record per improvement candidate, from proposal to promotion or
//! rejection: `Proposed → Claimed → Implementing → Implemented → Reviewing
//! → Validating → Promoted | Rejected`, with `Cancelled` reachable from any
//! live phase. The record tracks lineage (parent attempt), model and
//! REDACTED credential references (fingerprints only — never key material),
//! receipts, per-phase checkpoints, and cumulative tokens/spend/time that
//! persist across retries, workers and descendant attempts.
//!
//! Safety invariants:
//! - `Promoted` is unreachable without recorded, all-passing gate results.
//! - Every mutation is fenced: a stale worker's write is refused.
//! - Restart reloads records and rolls back transient phases
//!   (`Implementing`/`Reviewing`/`Validating`) to the last stable
//!   checkpoint — only safe work resumes; partial edits are preserved.
//! - Spend caps are enforced on accumulate; nothing buys credit or
//!   rotates keys on its own.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Lifecycle phase — `*` phases are transient: on restart they roll back
/// to the last stable checkpoint because their work is not atomic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    /// Model proposal minted, not yet claimed.
    Proposed,
    /// Claimed in the shared queue.
    Claimed,
    /// Worker is editing (transient — resumes as Claimed).
    Implementing,
    /// Edits applied and locally verified (stable).
    Implemented,
    /// Independent review running (transient — resumes as Implemented).
    Reviewing,
    /// Gates/baseline checks running (transient — resumes as Implemented).
    Validating,
    /// Passed everything — ready for PR promotion (terminal).
    Promoted,
    /// Rejected with recorded reason (terminal).
    Rejected,
    /// Cancelled — terminal, no further transitions.
    Cancelled,
}

impl Phase {
    /// Transient phases are rolled back to a stable phase on reload.
    fn resume_target(self) -> Self {
        match self {
            Phase::Implementing => Phase::Claimed,
            Phase::Reviewing | Phase::Validating => Phase::Implemented,
            stable @ (Phase::Proposed
            | Phase::Claimed
            | Phase::Implemented
            | Phase::Promoted
            | Phase::Rejected
            | Phase::Cancelled) => stable,
        }
    }
    fn is_terminal(self) -> bool {
        matches!(self, Phase::Promoted | Phase::Rejected | Phase::Cancelled)
    }
}

/// Allowed forward transitions (rejection/cancellation handled separately).
fn allowed(from: Phase, to: Phase) -> bool {
    matches!(
        (from, to),
        (Phase::Proposed, Phase::Claimed)
            | (Phase::Claimed, Phase::Implementing)
            | (Phase::Implementing, Phase::Implemented)
            | (Phase::Implemented, Phase::Reviewing)
            | (Phase::Reviewing, Phase::Validating)
            | (Phase::Reviewing, Phase::Implemented) // re-review after fix
            | (Phase::Validating, Phase::Promoted)
    )
}

/// A phase-boundary checkpoint — every transition appends one.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    /// Phase entered.
    pub phase: Phase,
    /// Fence that authored it.
    pub fence: u64,
    /// Worker identity.
    pub worker: String,
    /// Wall time (unix secs, injected clock domain).
    pub at_unix: u64,
    /// Freeform note (e.g. gate summary).
    pub note: String,
}

/// One improvement candidate's durable record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateRecord {
    /// Cycle id.
    pub id: String,
    /// Queue task id this cycle implements.
    pub task_id: String,
    /// Parent candidate id when this is a retry/descendant.
    pub parent: Option<String>,
    /// Current phase (post-resume adjustment on load).
    pub phase: Phase,
    /// Fencing token — bumps on reassignment.
    pub fence: u64,
    /// Worker holding the fence.
    pub worker: String,
    /// Opaque model ids that touched this candidate (implementer, reviewers).
    pub model_refs: Vec<String>,
    /// Redacted credential fingerprints — never raw keys.
    pub cred_fps: Vec<String>,
    /// Tool receipts recorded so far.
    pub receipts: Vec<String>,
    /// Checkpoint log.
    pub checkpoints: Vec<Checkpoint>,
    /// Preserved partial edits (survive restart and reassignment).
    pub partial_edits: Vec<String>,
    /// Recorded gate results — Promoted requires nonempty + all true.
    pub gates: Vec<(String, bool)>,
    /// Cumulative tokens consumed across attempts.
    pub tokens: u64,
    /// Cumulative spend (micros) committed against this candidate.
    pub spend_micros: u64,
    /// Cumulative wall ms spent.
    pub elapsed_ms: u64,
}

/// Work evidence to append under a live fence.
#[derive(Debug, Default)]
pub struct WorkRecord {
    /// Tool receipts recorded this step.
    pub receipts: Vec<String>,
    /// Opaque model id that produced the work.
    pub model: Option<String>,
    /// Redacted credential fingerprint (never key material).
    pub cred_fp: Option<String>,
    /// Partial edits to preserve across restart/reassignment.
    pub edits: Vec<String>,
}

/// Resource consumption to accumulate under a live fence.
#[derive(Debug, Default)]
pub struct Usage {
    /// Tokens consumed.
    pub tokens: u64,
    /// Spend committed (micros).
    pub spend_micros: u64,
    /// Wall time spent (ms).
    pub elapsed_ms: u64,
}

/// Why a mutation was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LifecycleError {
    /// Unknown candidate.
    Missing,
    /// Presented fence isn't current — stale writer.
    StaleFence,
    /// Record is terminal.
    Terminal,
    /// Illegal phase transition.
    BadTransition,
    /// Promotion without a complete, all-passing gate record.
    GatesIncomplete,
    /// Accumulate would exceed the candidate's spend cap.
    OverBudget { cap: u64, would_be: u64 },
}

/// Durable store: one JSON file per record (Mandate 51 — records union on
/// merge, never overwritten wholesale).
pub struct LifecycleStore {
    dir: PathBuf,
    records: BTreeMap<String, CandidateRecord>,
    /// Per-candidate spend cap (micros); `u64::MAX` = uncapped.
    pub spend_cap_micros: u64,
    clock: fn() -> u64,
}

impl LifecycleStore {
    /// Open (or create) the store rooted at `dir`, reloading live records.
    /// Transient phases roll back to their last stable checkpoint.
    #[must_use]
    pub fn load(dir: PathBuf, spend_cap_micros: u64, clock: fn() -> u64) -> Self {
        let mut records = BTreeMap::new();
        if let Ok(rd) = fs::read_dir(&dir) {
            for e in rd.flatten() {
                if e.path().extension().and_then(|x| x.to_str()) != Some("json") {
                    continue;
                }
                if let Ok(body) = fs::read_to_string(e.path()) {
                    if let Ok(mut rec) = serde_json::from_str::<CandidateRecord>(&body) {
                        rec.phase = rec.phase.resume_target();
                        records.insert(rec.id.clone(), rec);
                    }
                }
            }
        }
        Self {
            dir,
            records,
            spend_cap_micros,
            clock,
        }
    }

    fn persist(&self, rec: &CandidateRecord) {
        let _ = fs::create_dir_all(&self.dir);
        if let Ok(body) = serde_json::to_string_pretty(rec) {
            let _ = fs::write(self.dir.join(format!("{}.json", rec.id)), body);
        }
    }

    /// Read a record.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&CandidateRecord> {
        self.records.get(id)
    }

    /// All records.
    #[must_use]
    pub fn all(&self) -> Vec<&CandidateRecord> {
        self.records.values().collect()
    }

    /// Begin a candidate cycle (optionally descending from a prior one).
    /// Returns the initial fence.
    pub fn begin(&mut self, id: &str, task_id: &str, worker: &str, parent: Option<String>) -> u64 {
        let rec = CandidateRecord {
            id: id.to_string(),
            task_id: task_id.to_string(),
            parent,
            phase: Phase::Proposed,
            fence: 1,
            worker: worker.to_string(),
            model_refs: Vec::new(),
            cred_fps: Vec::new(),
            receipts: Vec::new(),
            checkpoints: vec![Checkpoint {
                phase: Phase::Proposed,
                fence: 1,
                worker: worker.to_string(),
                at_unix: (self.clock)(),
                note: "proposed".into(),
            }],
            partial_edits: Vec::new(),
            gates: Vec::new(),
            tokens: 0,
            spend_micros: 0,
            elapsed_ms: 0,
        };
        self.persist(&rec);
        self.records.insert(id.to_string(), rec);
        1
    }

    fn checked_mut(
        &mut self,
        id: &str,
        fence: u64,
    ) -> Result<&mut CandidateRecord, LifecycleError> {
        let rec = self.records.get_mut(id).ok_or(LifecycleError::Missing)?;
        if rec.fence != fence {
            return Err(LifecycleError::StaleFence);
        }
        if rec.phase.is_terminal() {
            return Err(LifecycleError::Terminal);
        }
        Ok(rec)
    }

    /// Advance the phase under a live fence. Promotion requires a complete
    /// all-passing gate record — nothing reaches Promoted unverified.
    pub fn advance(
        &mut self,
        id: &str,
        fence: u64,
        to: Phase,
        note: &str,
    ) -> Result<(), LifecycleError> {
        // Borrow-split: validate through a read first (gates gate promotion).
        {
            let rec = self.records.get(id).ok_or(LifecycleError::Missing)?;
            if rec.fence != fence {
                return Err(LifecycleError::StaleFence);
            }
            if rec.phase.is_terminal() {
                return Err(LifecycleError::Terminal);
            }
            match to {
                Phase::Rejected | Phase::Cancelled => {}
                Phase::Promoted => {
                    if !allowed(rec.phase, to) {
                        return Err(LifecycleError::BadTransition);
                    }
                    if rec.gates.is_empty() || rec.gates.iter().any(|(_, ok)| !ok) {
                        return Err(LifecycleError::GatesIncomplete);
                    }
                }
                other @ (Phase::Proposed
                | Phase::Claimed
                | Phase::Implementing
                | Phase::Implemented
                | Phase::Reviewing
                | Phase::Validating) => {
                    if !allowed(rec.phase, other) {
                        return Err(LifecycleError::BadTransition);
                    }
                }
            }
        }
        let clock = self.clock;
        let rec = self.records.get_mut(id).ok_or(LifecycleError::Missing)?;
        rec.phase = to;
        rec.checkpoints.push(Checkpoint {
            phase: to,
            fence,
            worker: rec.worker.clone(),
            at_unix: clock(),
            note: note.to_string(),
        });
        let snap = rec.clone();
        self.persist(&snap);
        Ok(())
    }

    /// Record gate results (called during Validating). Results are
    /// cumulative and durable — a later restart sees them.
    pub fn record_gates(
        &mut self,
        id: &str,
        fence: u64,
        gates: Vec<(String, bool)>,
    ) -> Result<(), LifecycleError> {
        let rec = self.checked_mut(id, fence)?;
        rec.gates.extend(gates);
        let snap = rec.clone();
        self.persist(&snap);
        Ok(())
    }

    /// Append receipts / model+cred refs / partial edits under the fence.
    pub fn record_work(
        &mut self,
        id: &str,
        fence: u64,
        work: WorkRecord,
    ) -> Result<(), LifecycleError> {
        let rec = self.checked_mut(id, fence)?;
        rec.receipts.extend(work.receipts);
        if let Some(m) = work.model {
            if !rec.model_refs.iter().any(|x| x == &m) {
                rec.model_refs.push(m);
            }
        }
        if let Some(fp) = work.cred_fp {
            if !rec.cred_fps.iter().any(|x| x == &fp) {
                rec.cred_fps.push(fp);
            }
        }
        rec.partial_edits.extend(work.edits);
        let snap = rec.clone();
        self.persist(&snap);
        Ok(())
    }

    /// Accumulate tokens/spend/elapsed — refused past the spend cap.
    /// Never buys credit or rotates keys; exhaustion is a typed refusal.
    pub fn accumulate(&mut self, id: &str, fence: u64, usage: Usage) -> Result<(), LifecycleError> {
        let cap = self.spend_cap_micros;
        let rec = self.checked_mut(id, fence)?;
        let would_be = rec.spend_micros.saturating_add(usage.spend_micros);
        if would_be > cap {
            return Err(LifecycleError::OverBudget { cap, would_be });
        }
        rec.tokens = rec.tokens.saturating_add(usage.tokens);
        rec.spend_micros = would_be;
        rec.elapsed_ms = rec.elapsed_ms.saturating_add(usage.elapsed_ms);
        let snap = rec.clone();
        self.persist(&snap);
        Ok(())
    }

    /// Reassign to a new worker — fence bumps, receipts/edits carry over.
    /// Only non-terminal records may be reassigned.
    pub fn reassign(&mut self, id: &str, new_worker: &str) -> Result<u64, LifecycleError> {
        let rec = self.records.get_mut(id).ok_or(LifecycleError::Missing)?;
        if rec.phase.is_terminal() {
            return Err(LifecycleError::Terminal);
        }
        rec.fence += 1;
        rec.worker = new_worker.to_string();
        // Transient phase rolls back for the new worker.
        rec.phase = rec.phase.resume_target();
        let f = rec.fence;
        let snap = rec.clone();
        self.persist(&snap);
        Ok(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> u64 {
        1_700_000_000
    }

    fn store(tag: &str) -> (PathBuf, LifecycleStore) {
        let dir = std::env::temp_dir().join(format!("lc-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        (dir.clone(), LifecycleStore::load(dir, 10_000, t0))
    }

    fn drive_to(store: &mut LifecycleStore, id: &str, f: u64, phase: Phase) {
        for (to, note) in [
            (Phase::Claimed, "claimed"),
            (Phase::Implementing, "editing"),
            (Phase::Implemented, "applied+verified"),
            (Phase::Reviewing, "review"),
            (Phase::Validating, "gates"),
            (Phase::Promoted, "promote"),
        ] {
            if to == Phase::Validating {
                store
                    .record_gates(
                        id,
                        f,
                        vec![("accept".into(), true), ("clippy".into(), true)],
                    )
                    .unwrap();
            }
            if to == phase {
                store.advance(id, f, to, note).unwrap();
                return;
            }
            store.advance(id, f, to, note).unwrap();
        }
    }

    #[test]
    fn cloud_rsi_lifecycle_persists_lineage_across_restart() {
        let (dir, mut s) = store("lineage");
        let f = s.begin("C-1", "T-1", "w1", None);
        s.advance("C-1", f, Phase::Claimed, "claimed").unwrap();
        s.advance("C-1", f, Phase::Implementing, "editing").unwrap();
        s.record_work(
            "C-1",
            f,
            WorkRecord {
                receipts: vec!["git apply -> 0".into()],
                model: Some("p1/m1/ab12cd34".into()),
                cred_fp: Some("fp:ab12cd34".into()),
                edits: vec!["src/f.rs:+fn f()".into()],
            },
        )
        .unwrap();
        s.accumulate(
            "C-1",
            f,
            Usage {
                tokens: 5000,
                spend_micros: 2_000,
                elapsed_ms: 300,
            },
        )
        .unwrap();
        // Descendant attempt inherits lineage.
        let f2 = s.begin("C-2", "T-1", "w2", Some("C-1".into()));
        s.accumulate(
            "C-2",
            f2,
            Usage {
                tokens: 1000,
                spend_micros: 500,
                elapsed_ms: 50,
            },
        )
        .unwrap();

        // Restart.
        let s2 = LifecycleStore::load(dir.clone(), 10_000, t0);
        let r1 = s2.get("C-1").unwrap();
        assert_eq!(
            r1.phase,
            Phase::Claimed,
            "transient Implementing resumes as Claimed"
        );
        assert_eq!(r1.partial_edits, vec!["src/f.rs:+fn f()".to_string()]);
        assert_eq!(r1.model_refs, vec!["p1/m1/ab12cd34".to_string()]);
        assert_eq!(r1.cred_fps, vec!["fp:ab12cd34".to_string()]);
        assert_eq!(r1.tokens, 5000);
        assert_eq!(r1.spend_micros, 2000);
        let r2 = s2.get("C-2").unwrap();
        assert_eq!(r2.parent.as_deref(), Some("C-1"));
        assert_eq!(r2.tokens, 1000);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cloud_rsi_lifecycle_restart_rolls_back_transient_phases() {
        let (dir, mut s) = store("resume");
        let f = s.begin("C-1", "T-1", "w1", None);
        s.advance("C-1", f, Phase::Claimed, "").unwrap();
        s.advance("C-1", f, Phase::Implementing, "").unwrap();
        s.advance("C-1", f, Phase::Implemented, "").unwrap();
        s.advance("C-1", f, Phase::Reviewing, "").unwrap();
        s.advance("C-1", f, Phase::Validating, "").unwrap();
        // "Crash" mid-validation.
        let s2 = LifecycleStore::load(dir.clone(), 10_000, t0);
        assert_eq!(s2.get("C-1").unwrap().phase, Phase::Implemented);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cloud_rsi_lifecycle_stale_fence_refused_after_reassign() {
        let (_d, mut s) = store("fence");
        let f1 = s.begin("C-1", "T-1", "w-old", None);
        s.advance("C-1", f1, Phase::Claimed, "").unwrap();
        s.advance("C-1", f1, Phase::Implementing, "").unwrap();
        let f2 = s.reassign("C-1", "w-new").unwrap();
        assert!(f2 > f1);
        // Old worker's writes are refused.
        assert_eq!(
            s.record_work(
                "C-1",
                f1,
                WorkRecord {
                    receipts: vec!["late".into()],
                    ..Default::default()
                }
            ),
            Err(LifecycleError::StaleFence)
        );
        assert_eq!(
            s.advance("C-1", f1, Phase::Implemented, "stale"),
            Err(LifecycleError::StaleFence)
        );
        // New worker proceeds — rolls back to Claimed to redo the work safely.
        assert_eq!(s.get("C-1").unwrap().phase, Phase::Claimed);
        s.advance("C-1", f2, Phase::Implementing, "").unwrap();
        let _ = std::fs::remove_dir_all(_d);
    }

    #[test]
    fn cloud_rsi_lifecycle_promotion_requires_all_gates_recorded() {
        let (_d, mut s) = store("gates");
        let f = s.begin("C-1", "T-1", "w1", None);
        s.advance("C-1", f, Phase::Claimed, "").unwrap();
        s.advance("C-1", f, Phase::Implementing, "").unwrap();
        s.advance("C-1", f, Phase::Implemented, "").unwrap();
        s.advance("C-1", f, Phase::Reviewing, "").unwrap();
        s.advance("C-1", f, Phase::Validating, "").unwrap();
        // No gates recorded → promotion refused.
        assert_eq!(
            s.advance("C-1", f, Phase::Promoted, ""),
            Err(LifecycleError::GatesIncomplete)
        );
        // A failing gate also refuses.
        s.record_gates("C-1", f, vec![("accept".into(), false)])
            .unwrap();
        assert_eq!(
            s.advance("C-1", f, Phase::Promoted, ""),
            Err(LifecycleError::GatesIncomplete)
        );
        let _ = std::fs::remove_dir_all(_d);

        let (_d2, mut s2) = store("gates2");
        let f = s2.begin("C-2", "T-2", "w1", None);
        drive_to(&mut s2, "C-2", f, Phase::Promoted);
        assert_eq!(s2.get("C-2").unwrap().phase, Phase::Promoted);
        let _ = std::fs::remove_dir_all(_d2);
    }

    #[test]
    fn cloud_rsi_lifecycle_spend_cap_refuses_overspend() {
        let (_d, mut s) = store("cap");
        let f = s.begin("C-1", "T-1", "w1", None);
        s.accumulate(
            "C-1",
            f,
            Usage {
                tokens: 100,
                spend_micros: 9_000,
                elapsed_ms: 0,
            },
        )
        .unwrap();
        assert_eq!(
            s.accumulate(
                "C-1",
                f,
                Usage {
                    tokens: 100,
                    spend_micros: 2_000,
                    elapsed_ms: 0,
                }
            ),
            Err(LifecycleError::OverBudget {
                cap: 10_000,
                would_be: 11_000
            })
        );
        // Refusal is non-destructive — spend stays at 9000.
        assert_eq!(s.get("C-1").unwrap().spend_micros, 9_000);
        let _ = std::fs::remove_dir_all(_d);
    }

    #[test]
    fn cloud_rsi_lifecycle_cancel_and_reject_are_terminal() {
        let (_d, mut s) = store("term");
        let f = s.begin("C-1", "T-1", "w1", None);
        s.advance("C-1", f, Phase::Claimed, "").unwrap();
        s.advance("C-1", f, Phase::Cancelled, "operator cancel")
            .unwrap();
        assert_eq!(
            s.advance("C-1", f, Phase::Implementing, ""),
            Err(LifecycleError::Terminal)
        );
        // Rejection keeps lineage and receipts.
        let f2 = s.begin("C-2", "T-2", "w1", None);
        s.advance("C-2", f2, Phase::Claimed, "").unwrap();
        s.record_work(
            "C-2",
            f2,
            WorkRecord {
                receipts: vec!["r1".into()],
                model: Some("m1".into()),
                ..Default::default()
            },
        )
        .unwrap();
        s.advance("C-2", f2, Phase::Rejected, "verify rejected")
            .unwrap();
        assert_eq!(s.get("C-2").unwrap().receipts, vec!["r1".to_string()]);
        assert_eq!(
            s.advance("C-2", f2, Phase::Proposed, ""),
            Err(LifecycleError::Terminal)
        );
        let _ = std::fs::remove_dir_all(_d);
    }

    #[test]
    fn cloud_rsi_lifecycle_illegal_transitions_refused() {
        let (_d, mut s) = store("trans");
        let f = s.begin("C-1", "T-1", "w1", None);
        // Cannot skip from Proposed to Implemented or Promoted.
        assert_eq!(
            s.advance("C-1", f, Phase::Implemented, ""),
            Err(LifecycleError::BadTransition)
        );
        assert_eq!(
            s.advance("C-1", f, Phase::Promoted, ""),
            Err(LifecycleError::BadTransition)
        );
        let _ = std::fs::remove_dir_all(_d);
    }
}
