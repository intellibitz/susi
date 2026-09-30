//! Publish and explain brain-generated gap tasks (T-CODEX-33 /
//! VC-201-017).
//!
//! Every task minted from a gap carries a lineage record: the gap id,
//! the evidence receipts behind it, the assigned roadmap vector, the
//! priority rationale, its dependencies, and which discovery brain
//! produced it — all redacted (opaque credential ids only).
//!
//! Publication state is explicit and ordered:
//! `Draft → RecordedLocal → Published → Claimed → Delivered`.
//! Discovery never closes implementation work — a record only reaches
//! `Delivered` with gate receipts attached.
//!
//! Records are one file per task id under `publications/`, created with
//! `create_new` — concurrent brains can't clobber each other, and a
//! failed publish retries cleanly (nothing partial is left live).

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::brain_gap_audit::Gap;
use crate::brain_gap_tasks::{validate_draft, DraftReject, TaskDraft};

/// Ordered publication lifecycle. Transitions only move forward.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum PublishState {
    /// Minted, not yet validated/persisted.
    Draft,
    /// Validated and written to the local queue.
    RecordedLocal,
    /// Pushed on a branch / PR opened — visible to other agents.
    Published,
    /// Claimed by an agent for implementation.
    Claimed,
    /// Acceptance passed; task closed with receipts.
    Delivered,
}

/// Why a state transition was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransitionError {
    /// Only forward moves are legal (`{from} → {to}` tried).
    BackwardOrSkip {
        from: PublishState,
        to: PublishState,
    },
    /// `Delivered` requires gate receipts — never self-reported.
    MissingReceipts,
}

/// The lineage record for one gap-minted task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GapTaskRecord {
    /// Task id (`T-<AGENT>-<n>`).
    pub task_id: String,
    /// Gap this task answers.
    pub gap_id: String,
    /// Receipt ids the gap rests on.
    pub evidence: Vec<String>,
    /// Roadmap vector assigned.
    pub vector: String,
    /// Why this priority/order — the ranking drivers.
    pub rationale: Vec<String>,
    /// Dependency task ids.
    pub deps: Vec<String>,
    /// Opaque id of the discovery brain (`provider/model/fp8`).
    pub discovery_brain: Option<String>,
    /// Current publication state.
    pub state: PublishState,
    /// Gate receipts attached at delivery (never fabricated).
    #[serde(default)]
    pub gate_receipts: Vec<String>,
    /// State history `(state, unix)`.
    #[serde(default)]
    pub history: Vec<(PublishState, u64)>,
}

/// A store of lineage records — one file per task id.
pub struct PublicationLog {
    dir: PathBuf,
}

impl PublicationLog {
    /// Open/create the log at `dir`.
    #[must_use]
    pub fn load(dir: PathBuf) -> Self {
        Self { dir }
    }

    fn path(&self, task_id: &str) -> PathBuf {
        self.dir.join(format!("{task_id}.pub.json"))
    }

    /// Record a draft — `create_new` so a concurrent record for the same
    /// task id is a clean refusal, never an overwrite.
    pub fn record(&self, r: &GapTaskRecord) -> io::Result<bool> {
        fs::create_dir_all(&self.dir)?;
        let body = serde_json::to_vec_pretty(r).map_err(io::Error::other)?;
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(self.path(&r.task_id))
        {
            Ok(mut f) => {
                use std::io::Write;
                f.write_all(&body)?;
                Ok(true)
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Read back a record.
    #[must_use]
    pub fn get(&self, task_id: &str) -> Option<GapTaskRecord> {
        fs::read_to_string(self.path(task_id))
            .ok()
            .and_then(|b| serde_json::from_str(&b).ok())
    }

    /// Persist a state transition (atomic tmp+rename).
    pub fn update(&self, r: &GapTaskRecord) -> io::Result<()> {
        let body = serde_json::to_vec_pretty(r).map_err(io::Error::other)?;
        let tmp = self.dir.join(format!(".{}.pub.json.tmp", r.task_id));
        fs::write(&tmp, body)?;
        fs::rename(&tmp, self.path(&r.task_id))
    }
}

/// Build the lineage record for a validated draft.
#[must_use]
pub fn lineage(
    draft: &TaskDraft,
    gap: &Gap,
    brain_opaque: Option<&str>,
    rationale: Vec<String>,
    now: u64,
) -> GapTaskRecord {
    GapTaskRecord {
        task_id: draft.id.clone(),
        gap_id: gap.id.clone(),
        evidence: gap.receipts.clone(),
        vector: draft.roadmap.clone(),
        rationale,
        deps: draft.deps.clone(),
        discovery_brain: brain_opaque.map(str::to_string),
        state: PublishState::Draft,
        gate_receipts: Vec::new(),
        history: vec![(PublishState::Draft, now)],
    }
}

/// Advance a record's state. `Delivered` requires gate receipts.
pub fn transition(
    r: &mut GapTaskRecord,
    to: PublishState,
    gate_receipts: &[String],
    now: u64,
) -> Result<(), TransitionError> {
    // Strictly stepwise: no backward moves, no skipping a state.
    let next = match r.state {
        PublishState::Draft => PublishState::RecordedLocal,
        PublishState::RecordedLocal => PublishState::Published,
        PublishState::Published => PublishState::Claimed,
        PublishState::Claimed => PublishState::Delivered,
        PublishState::Delivered => PublishState::Delivered,
    };
    if to != next || to == r.state {
        return Err(TransitionError::BackwardOrSkip { from: r.state, to });
    }
    if to == PublishState::Delivered && gate_receipts.is_empty() {
        return Err(TransitionError::MissingReceipts);
    }
    if to == PublishState::Delivered {
        r.gate_receipts = gate_receipts.to_vec();
    }
    r.state = to;
    r.history.push((to, now));
    Ok(())
}

/// The queue context needed to validate a draft before recording.
pub struct QueueKnowledge<'a> {
    /// All known task ids.
    pub known_ids: &'a BTreeSet<String>,
    /// Task id → deps graph.
    pub task_graph: &'a std::collections::BTreeMap<String, Vec<String>>,
    /// Known roadmap vector ids.
    pub known_vectors: &'a BTreeSet<String>,
}

/// Validate + record: the full Draft→RecordedLocal hop. A failed
/// validation or write leaves nothing behind — the caller may retry.
pub fn record_draft(
    log: &PublicationLog,
    draft: &TaskDraft,
    gap: &Gap,
    brain_opaque: Option<&str>,
    rationale: Vec<String>,
    now: u64,
    queue: &QueueKnowledge<'_>,
) -> Result<GapTaskRecord, DraftReject> {
    validate_draft(
        draft,
        queue.known_ids,
        queue.task_graph,
        queue.known_vectors,
        &BTreeSet::new(),
    )?;
    let mut r = lineage(draft, gap, brain_opaque, rationale, now);
    if !log
        .record(&r)
        .map_err(|e| DraftReject::Malformed(format!("publication log: {e}")))?
    {
        return Err(DraftReject::DuplicateId(draft.id.clone()));
    }
    r.state = PublishState::RecordedLocal;
    r.history.push((PublishState::RecordedLocal, now));
    log.update(&r)
        .map_err(|e| DraftReject::Malformed(format!("publication log: {e}")))?;
    Ok(r)
}

/// Human-readable explanation — what gap, what evidence, why this
/// priority, who discovered it, where it stands. Redacted by
/// construction (opaque brain ids, receipt ids only).
#[must_use]
pub fn explain(r: &GapTaskRecord) -> String {
    let mut s = format!(
        "{} — {:?} — answers gap {} (evidence: {})\n  vector {} · deps [{}] · discovered by {}\n  rationale: {}\n  history:",
        r.task_id,
        r.state,
        r.gap_id,
        r.evidence.join(", "),
        r.vector,
        r.deps.join(", "),
        r.discovery_brain.as_deref().unwrap_or("none"),
        r.rationale.join(", "),
    );
    for (st, at) in &r.history {
        s.push_str(&format!(" {st:?}@{at}"));
    }
    susi_error::redact::mask_env_credentials(&s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brain_gap_audit::{GapStatus, Impact};
    use crate::brain_gap_tasks::{draft_from_gap, DraftCtx};
    use std::collections::BTreeMap;

    const T0: u64 = 1_700_000_000;

    fn gap() -> Gap {
        Gap {
            id: "GAP-retry".into(),
            title: "retry path never engaged".into(),
            receipts: vec!["test_retry".into()],
            trigger: Some("repro".into()),
            expected: "retries".into(),
            observed: "no retry".into(),
            hidden_behind: None,
            status: GapStatus::Verified,
            impact: Impact::High,
            confidence: 0.9,
            observed_at: T0,
        }
    }

    fn draft(id_seq: u32) -> TaskDraft {
        draft_from_gap(
            &gap(),
            id_seq,
            &DraftCtx {
                agent: "DEVIN",
                roadmap: "VC-201-017",
                accept_cmd: vec![
                    "cargo".into(),
                    "test".into(),
                    "-p".into(),
                    "susi-gawd".into(),
                    "gap_fix".into(),
                    "--locked".into(),
                ],
                deps: vec!["T-CODEX-1".into()],
                now: T0,
            },
        )
        .unwrap()
    }

    type Known = (
        BTreeSet<String>,
        BTreeMap<String, Vec<String>>,
        BTreeSet<String>,
    );

    fn queue<'a>(k: &'a Known) -> QueueKnowledge<'a> {
        QueueKnowledge {
            known_ids: &k.0,
            task_graph: &k.1,
            known_vectors: &k.2,
        }
    }

    fn known() -> Known {
        (
            ["T-CODEX-1".to_string()].into_iter().collect(),
            BTreeMap::new(),
            ["VC-201-017".to_string()].into_iter().collect(),
        )
    }

    /// The lineage record exposes the full evidence chain — gap, receipts,
    /// vector, rationale, deps, discovery brain — with no secrets.
    #[test]
    fn brain_gap_publication_lineage_exposes_evidence_chain() {
        let dir = std::env::temp_dir().join(format!("pb-1-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let log = PublicationLog::load(dir.clone());
        let k = known();
        let d = draft(70);
        let r = record_draft(
            &log,
            &d,
            &gap(),
            Some("acme/m-brain/deadbeef"),
            vec!["impact:High".into(), "verified".into()],
            T0,
            &queue(&k),
        )
        .unwrap();
        assert_eq!(r.state, PublishState::RecordedLocal);
        assert_eq!(r.gap_id, "GAP-retry");
        assert_eq!(r.evidence, vec!["test_retry"]);
        assert_eq!(r.vector, "VC-201-017");
        assert_eq!(r.discovery_brain.as_deref(), Some("acme/m-brain/deadbeef"));
        let text = explain(&r);
        assert!(text.contains("T-DEVIN-70"));
        assert!(text.contains("test_retry"));
        assert!(text.contains("acme/m-brain/deadbeef"));
        assert!(!text.contains("sk-"));
        let _ = fs::remove_dir_all(&dir);
    }

    /// States move forward only; delivery needs gate receipts; discovery
    /// alone never marks work done.
    #[test]
    fn brain_gap_publication_states_progress_in_order() {
        let mut r = lineage(&draft(71), &gap(), None, vec![], T0);
        assert_eq!(r.state, PublishState::Draft);
        // Draft → Claimed skips states — rejected.
        assert!(matches!(
            transition(&mut r, PublishState::Claimed, &[], T0),
            Err(TransitionError::BackwardOrSkip { .. })
        ));
        // Walk the chain in order.
        transition(&mut r, PublishState::RecordedLocal, &[], T0).unwrap();
        transition(&mut r, PublishState::Published, &[], T0).unwrap();
        transition(&mut r, PublishState::Claimed, &[], T0).unwrap();
        assert!(matches!(
            transition(&mut r, PublishState::RecordedLocal, &[], T0),
            Err(TransitionError::BackwardOrSkip { .. })
        ));
        // Delivered needs gate receipts.
        assert!(matches!(
            transition(&mut r, PublishState::Delivered, &[], T0),
            Err(TransitionError::MissingReceipts)
        ));
        transition(&mut r, PublishState::Delivered, &["accept:pass".into()], T0).unwrap();
        assert_eq!(r.state, PublishState::Delivered);
        assert_eq!(r.gate_receipts, vec!["accept:pass"]);
    }

    /// Two concurrent records for the same task id: first wins, second
    /// is a clean refusal — no overwrite, no torn state.
    #[test]
    fn brain_gap_publication_concurrent_records_no_clobber() {
        let dir = std::env::temp_dir().join(format!("pb-3-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let log = PublicationLog::load(dir.clone());
        let k = known();
        let r1 = record_draft(
            &log,
            &draft(72),
            &gap(),
            Some("a/m/11111111"),
            vec![],
            T0,
            &queue(&k),
        )
        .unwrap();
        // A different brain records the same id — refused.
        let res = record_draft(
            &log,
            &draft(72),
            &gap(),
            Some("b/m/22222222"),
            vec![],
            T0,
            &queue(&k),
        );
        assert!(matches!(res, Err(DraftReject::DuplicateId(_))));
        // The original record is untouched.
        let back = log.get("T-DEVIN-72").unwrap();
        assert_eq!(back.discovery_brain.as_deref(), Some("a/m/11111111"));
        let _ = fs::remove_dir_all(&dir);
        let _ = r1;
    }

    /// A failed publish (unwritable dir) leaves no partial record; the
    /// retry on a healthy dir succeeds.
    #[test]
    fn brain_gap_publication_failure_recovery() {
        let dir = std::env::temp_dir().join(format!("pb-4-{}", std::process::id()));
        let blocked = dir.join("nonexistent-parent").join("x").join("log");
        // A path under a file, not a dir — writes fail.
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("nonexistent-parent"), "file").unwrap();
        let bad_log = PublicationLog::load(blocked);
        let k = known();
        let res = record_draft(&bad_log, &draft(73), &gap(), None, vec![], T0, &queue(&k));
        assert!(res.is_err());
        // Retry on a healthy dir works and records cleanly.
        let good = PublicationLog::load(dir.join("log"));
        let r = record_draft(&good, &draft(73), &gap(), None, vec![], T0, &queue(&k)).unwrap();
        assert_eq!(r.state, PublishState::RecordedLocal);
        let _ = fs::remove_dir_all(&dir);
    }

    /// Invalid schema/deps still refuse at publication time — the lineage
    /// step doesn't bypass task validation.
    #[test]
    fn brain_gap_publication_invalid_rejected() {
        let dir = std::env::temp_dir().join(format!("pb-5-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let log = PublicationLog::load(dir.clone());
        let k = known();
        let mut d = draft(74);
        d.deps = vec!["T-GHOST-9".into()];
        let res = record_draft(&log, &d, &gap(), None, vec![], T0, &queue(&k));
        assert!(matches!(res, Err(DraftReject::UnresolvedDep(_))));
        assert!(log.get("T-DEVIN-74").is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    /// The minted task file has no completion state — publication is not
    /// delivery; only gate receipts can move it to Delivered.
    #[test]
    fn brain_gap_publication_discovery_never_closes_work() {
        let dir = std::env::temp_dir().join(format!("pb-6-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let log = PublicationLog::load(dir.clone());
        let k = known();
        let r = record_draft(&log, &draft(75), &gap(), None, vec![], T0, &queue(&k)).unwrap();
        let task_json = serde_json::to_string(&draft(75)).unwrap();
        assert!(!task_json.contains("\"closed\""));
        assert_ne!(r.state, PublishState::Delivered);
        let _ = fs::remove_dir_all(&dir);
    }
}
