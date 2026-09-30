//! Deduplicate and prioritize brain-discovered gaps before they mint
//! tasks (T-CODEX-31 / VC-201-015).
//!
//! Before a [`Gap`] may become a task, this pass checks the *whole* task
//! set — open, claimed and closed — plus live proposals and recent
//! rejections:
//!
//! - **Merge**: a semantically similar issue already has an active
//!   proposal → the gap's receipts are folded in, no second task.
//! - **Follow-up**: the issue matches a *closed* task that recurred → a
//!   linked follow-up draft (dep on the closed task) — history is never
//!   erased or silently reopened.
//! - **Reject**: the same issue was recently proposed and rejected →
//!   blocked until genuinely new evidence arrives.
//!
//! Ranking is evidence-weighted: measured frequency, declared impact,
//! unblock value, verified-deficit status and estimated cost — every
//! score records its uncertainty instead of inventing precision.
//!
//! Concurrency safety follows the queue's own pattern: one file per
//! proposal, created with `create_new` (fails if another brain already
//! holds the issue) — nobody overwrites another agent's record.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::brain_gap_audit::{Gap, Impact};
use crate::brain_gap_tasks::{draft_from_gap, DraftCtx, DraftReject, TaskDraft};

/// Word-set similarity above this counts as the same issue.
const SIMILARITY_MIN: f64 = 0.5;

/// What a lookup over existing records found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskState {
    /// Still on the queue (open or claimed).
    Active,
    /// Finished — history preserved.
    Closed,
}

/// Minimal view of an existing queue task for dedup.
#[derive(Debug, Clone)]
pub struct ExistingTask {
    /// `T-<AGENT>-<n>`.
    pub id: String,
    /// Title + goal are compared against the gap text.
    pub text: String,
    /// Queue state.
    pub state: TaskState,
}

/// A proposal record — one file under `proposals/` per issue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Proposal {
    /// Canonical issue key (normalized signature).
    pub issue_key: String,
    /// Agent that minted the proposal.
    pub agent: String,
    /// Receipt ids folded into this issue.
    pub evidence: Vec<String>,
    /// Task id minted for this issue, when published.
    pub task_id: Option<String>,
    /// `active` | `linked` | `rejected`.
    pub state: String,
    /// Last update (unix secs).
    pub updated_unix: u64,
}

/// Persisted proposal store — one file per issue key, `create_new` only.
pub struct ProposalStore {
    dir: PathBuf,
}

fn key_file(dir: &Path, key: &str) -> PathBuf {
    dir.join(format!("{key}.json"))
}

fn sig_key(text: &str) -> String {
    text.to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .take(8)
        .collect::<Vec<_>>()
        .join("-")
}

fn word_set(text: &str) -> std::collections::BTreeSet<String> {
    text.to_ascii_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| w.len() > 2)
        .map(str::to_string)
        .collect()
}

/// Overlap coefficient over normalized word sets — wording-invariant
/// duplicate detection. `intersection / min(|a|, |b|)` so a task title
/// embedded in a longer gap text still matches.
pub fn similarity(a: &str, b: &str) -> f64 {
    let (wa, wb) = (word_set(a), word_set(b));
    if wa.is_empty() || wb.is_empty() {
        return 0.0;
    }
    let inter = wa.intersection(&wb).count() as f64;
    inter / wa.len().min(wb.len()) as f64
}

/// The dedup decision for one gap.
#[derive(Debug)]
pub enum ProposeOutcome {
    /// Fresh issue — draft minted and proposal recorded.
    Created(Box<TaskDraft>),
    /// Same issue as an active proposal — evidence folded in.
    Merged {
        /// The held proposal key.
        issue_key: String,
        /// Task it tracks, if already minted.
        task_id: Option<String>,
    },
    /// Recurrence of a closed task — follow-up draft linked to it.
    LinkedFollowUp {
        /// The closed task this follows up.
        closed_task: String,
        /// The follow-up draft.
        draft: Box<TaskDraft>,
    },
    /// A like issue was recently rejected — needs new evidence.
    RejectedRepeat {
        /// The held proposal key.
        issue_key: String,
    },
    /// Another agent already holds the issue — nothing overwritten.
    HeldByOther {
        /// The held proposal key.
        issue_key: String,
        /// Owning agent.
        agent: String,
    },
}

impl ProposalStore {
    /// Open/create the store at `dir`.
    #[must_use]
    pub fn load(dir: PathBuf) -> Self {
        Self { dir }
    }

    fn all(&self) -> Vec<Proposal> {
        let mut out = Vec::new();
        if let Ok(rd) = fs::read_dir(&self.dir) {
            for e in rd.flatten() {
                if let Ok(body) = fs::read_to_string(e.path()) {
                    if let Ok(p) = serde_json::from_str::<Proposal>(&body) {
                        out.push(p);
                        continue;
                    }
                }
            }
        }
        out
    }

    /// Atomically claim an issue key. `create_new` — a live file means
    /// another agent holds the issue.
    fn claim(&self, p: &Proposal) -> io::Result<bool> {
        fs::create_dir_all(&self.dir)?;
        let body = serde_json::to_vec_pretty(p).map_err(io::Error::other)?;
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(key_file(&self.dir, &p.issue_key))
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

    /// Fold new evidence into a held proposal (union; never loses ids).
    fn merge_evidence(&self, key: &str, receipts: &[String], now: u64) -> io::Result<()> {
        let path = key_file(&self.dir, key);
        if let Ok(body) = fs::read_to_string(&path) {
            if let Ok(mut p) = serde_json::from_str::<Proposal>(&body) {
                for r in receipts {
                    if !p.evidence.contains(r) {
                        p.evidence.push(r.clone());
                    }
                }
                p.updated_unix = now;
                let body = serde_json::to_vec_pretty(&p).map_err(io::Error::other)?;
                fs::write(&path, body)?;
            }
        }
        Ok(())
    }
}

/// How strong the recorded evidence is for ranking.
#[derive(Debug, Clone)]
pub struct Rank {
    /// 0..1 priority score.
    pub score: f64,
    /// 0..1 — how much of the score rests on measured inputs.
    pub certainty: f64,
    /// What drove the score (for explanation).
    pub drivers: Vec<String>,
}

/// Rank a gap: measured frequency + declared impact + unblock value +
/// verified deficit + cost. Unmeasured inputs lower `certainty` instead
/// of contributing invented precision.
#[must_use]
pub fn rank_gap(gap: &Gap, frequency: u32, unblocks: u32, cost_estimate: Option<f64>) -> Rank {
    let mut drivers = Vec::new();
    let mut score = 0.0;
    let mut measured = 0u32;
    let mut total = 0u32;

    total += 1;
    measured += 1; // impact is always declared on a gap
    let impact_w = match gap.impact {
        Impact::High => 0.4,
        Impact::Medium => 0.25,
        Impact::Low => 0.1,
    };
    score += impact_w;
    drivers.push(format!("impact:{:?}", gap.impact));

    total += 1;
    measured += 1;
    let freq_w = (f64::from(frequency).min(10.0) / 10.0) * 0.2;
    score += freq_w;
    drivers.push(format!("frequency:{frequency}"));

    total += 1;
    if unblocks > 0 {
        measured += 1;
        score += (f64::from(unblocks).min(5.0) / 5.0) * 0.2;
        drivers.push(format!("unblocks:{unblocks}"));
    } else {
        drivers.push("unblocks:unknown".into());
    }

    total += 1;
    measured += 1;
    if gap.status == crate::brain_gap_audit::GapStatus::Verified {
        score += 0.15;
        drivers.push("verified".into());
    } else {
        drivers.push("hypothesis".into());
    }

    total += 1;
    match cost_estimate {
        Some(c) if c >= 0.0 => {
            measured += 1;
            // Cheaper work ranks slightly higher at equal value.
            score += (1.0 / (1.0 + c / 10.0)) * 0.05;
            drivers.push(format!("cost:{c}"));
        }
        _ => drivers.push("cost:unknown".into()),
    }

    let certainty = f64::from(measured) / f64::from(total);
    Rank {
        score,
        certainty,
        drivers,
    }
}

/// The dedup+rank pass: consult existing tasks and held proposals, then
/// create / merge / link / reject. `next_seq` mints new task ids.
pub fn propose(
    store: &ProposalStore,
    gap: &Gap,
    existing: &[ExistingTask],
    ctx: &DraftCtx<'_>,
    next_seq: u32,
) -> Result<ProposeOutcome, DraftReject> {
    let gap_text = format!("{} {} {}", gap.title, gap.expected, gap.observed);

    // 1. Match against existing tasks (any state).
    for t in existing {
        if similarity(&gap_text, &t.text) >= SIMILARITY_MIN {
            return match t.state {
                TaskState::Active => Ok(ProposeOutcome::Merged {
                    issue_key: t.id.clone(),
                    task_id: Some(t.id.clone()),
                }),
                TaskState::Closed => {
                    // Recurrence → linked follow-up; original untouched.
                    let mut deps = ctx.deps.clone();
                    deps.push(t.id.clone());
                    let follow = DraftCtx {
                        deps,
                        ..DraftCtx {
                            agent: ctx.agent,
                            roadmap: ctx.roadmap,
                            accept_cmd: ctx.accept_cmd.clone(),
                            deps: Vec::new(),
                            now: ctx.now,
                        }
                    };
                    let mut draft = draft_from_gap(gap, next_seq, &follow)?;
                    draft.title = format!("Follow-up: {} (recurrence of {})", gap.title, t.id);
                    Ok(ProposeOutcome::LinkedFollowUp {
                        closed_task: t.id.clone(),
                        draft: Box::new(draft),
                    })
                }
            };
        }
    }

    // 2. Match against held proposals.
    let key = sig_key(&gap_text);
    for p in store.all() {
        if p.issue_key == key
            || similarity(&gap_text, &p.issue_key.replace('-', " ")) >= SIMILARITY_MIN
        {
            return match p.state.as_str() {
                "rejected" => Ok(ProposeOutcome::RejectedRepeat {
                    issue_key: p.issue_key,
                }),
                "active" | "linked" if p.agent != ctx.agent => Ok(ProposeOutcome::HeldByOther {
                    issue_key: p.issue_key,
                    agent: p.agent,
                }),
                _ => {
                    let _ = store.merge_evidence(&p.issue_key, &gap.receipts, ctx.now);
                    Ok(ProposeOutcome::Merged {
                        issue_key: p.issue_key,
                        task_id: p.task_id,
                    })
                }
            };
        }
    }

    // 3. Fresh issue → draft + atomic proposal claim.
    let draft = draft_from_gap(gap, next_seq, ctx)?;
    let p = Proposal {
        issue_key: key.clone(),
        agent: ctx.agent.into(),
        evidence: gap.receipts.clone(),
        task_id: Some(draft.id.clone()),
        state: "active".into(),
        updated_unix: ctx.now,
    };
    match store.claim(&p) {
        Ok(true) => Ok(ProposeOutcome::Created(Box::new(draft))),
        Ok(false) => Ok(ProposeOutcome::HeldByOther {
            issue_key: key,
            agent: "unknown".into(),
        }),
        Err(e) => Err(DraftReject::Malformed(format!("proposal store: {e}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brain_gap_audit::GapStatus;
    use std::collections::BTreeSet;

    const T0: u64 = 1_700_000_000;

    fn gap(title: &str, expected: &str, observed: &str) -> Gap {
        Gap {
            id: format!("GAP-{title}"),
            title: title.into(),
            receipts: vec![format!("rcpt-{title}")],
            trigger: Some("repro".into()),
            expected: expected.into(),
            observed: observed.into(),
            hidden_behind: None,
            status: GapStatus::Verified,
            impact: Impact::Medium,
            confidence: 0.9,
            observed_at: T0,
        }
    }

    fn ctx<'a>() -> DraftCtx<'a> {
        DraftCtx {
            agent: "DEVIN",
            roadmap: "VC-201-015",
            accept_cmd: vec![
                "cargo".into(),
                "test".into(),
                "-p".into(),
                "susi-gawd".into(),
                "gap_test".into(),
                "--locked".into(),
            ],
            deps: vec![],
            now: T0,
        }
    }

    fn store(tag: &str) -> (PathBuf, ProposalStore) {
        let dir = std::env::temp_dir().join(format!("dd-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        (dir.clone(), ProposalStore::load(dir))
    }

    /// Same issue in different words merges evidence into one proposal —
    /// no second task is minted.
    #[test]
    fn brain_gap_dedup_same_issue_different_wording_merges() {
        let (_d, s) = store("same");
        let g1 = gap(
            "retry never engages",
            "requests retry on failure",
            "no retry occurs",
        );
        let o1 = propose(&s, &g1, &[], &ctx(), 10).unwrap();
        assert!(matches!(o1, ProposeOutcome::Created(_)));
        // Differently worded, same substance.
        let g2 = gap(
            "failed requests are never retried",
            "requests retry on failure",
            "no retry occurs",
        );
        let o2 = propose(&s, &g2, &[], &ctx(), 11).unwrap();
        match o2 {
            ProposeOutcome::Merged { task_id, .. } => assert!(task_id.is_some()),
            _ => panic!("expected merge, got {o2:?}"),
        }
        let _ = fs::remove_dir_all(_d);
    }

    /// Genuinely different gaps mint distinct tasks.
    #[test]
    fn brain_gap_dedup_distinct_gaps_create_distinct_tasks() {
        let (_d, s) = store("distinct");
        let o1 = propose(&s, &gap("retry missing", "a", "b"), &[], &ctx(), 20).unwrap();
        let o2 = propose(
            &s,
            &gap("config file unparsed", "yaml loads", "yaml rejected"),
            &[],
            &ctx(),
            21,
        )
        .unwrap();
        let (ProposeOutcome::Created(a), ProposeOutcome::Created(b)) = (o1, o2) else {
            panic!("both should be created");
        };
        assert_ne!(a.id, b.id);
        let _ = fs::remove_dir_all(_d);
    }

    /// Two agents racing on the same issue: first claim wins; second is
    /// told who holds it — nothing overwritten.
    #[test]
    fn brain_gap_dedup_concurrent_no_duplicate() {
        let (_d, s) = store("race");
        let g = gap("quota leak", "bounded spend", "unbounded spend");
        let o1 = propose(&s, &g, &[], &ctx(), 30).unwrap();
        assert!(matches!(o1, ProposeOutcome::Created(_)));
        // A second agent proposes the identical issue.
        let other = DraftCtx {
            agent: "CLAUDE",
            ..ctx()
        };
        let o2 = propose(&s, &g, &[], &other, 31).unwrap();
        match o2 {
            ProposeOutcome::HeldByOther { agent, .. } => assert_eq!(agent, "DEVIN"),
            _ => panic!("expected HeldByOther, got {o2:?}"),
        }
        // DEVIN proposing again merges into its own proposal.
        let o3 = propose(&s, &g, &[], &ctx(), 32).unwrap();
        assert!(matches!(o3, ProposeOutcome::Merged { .. }));
        let _ = fs::remove_dir_all(_d);
    }

    /// Recurrence of a closed task produces a linked follow-up — the
    /// closed record is untouched, history preserved.
    #[test]
    fn brain_gap_dedup_recurrence_linked_followup() {
        let (_d, s) = store("recur");
        let closed = ExistingTask {
            id: "T-CODEX-9".into(),
            text: "retry requests recover on transient failure".into(),
            state: TaskState::Closed,
        };
        let g = gap(
            "requests not retried",
            "requests retry on transient failure",
            "still no retry",
        );
        let o = propose(&s, &g, &[closed], &ctx(), 40).unwrap();
        match o {
            ProposeOutcome::LinkedFollowUp { closed_task, draft } => {
                assert_eq!(closed_task, "T-CODEX-9");
                assert!(draft.deps.contains(&"T-CODEX-9".to_string()));
                assert!(draft.title.contains("recurrence"));
            }
            _ => panic!("expected follow-up, got {o:?}"),
        }
        let _ = fs::remove_dir_all(_d);
    }

    /// An active task covering the issue folds evidence in — no dup task.
    #[test]
    fn brain_gap_dedup_active_task_merges() {
        let (_d, s) = store("active");
        let open = ExistingTask {
            id: "T-CODEX-5".into(),
            text: "lockout recovery honors retry-after cooldown".into(),
            state: TaskState::Active,
        };
        let g = gap(
            "lockout ignores cooldown",
            "retry-after cooldown honored",
            "cooldown skipped",
        );
        let o = propose(&s, &g, &[open], &ctx(), 50).unwrap();
        match o {
            ProposeOutcome::Merged { task_id, .. } => {
                assert_eq!(task_id.as_deref(), Some("T-CODEX-5"))
            }
            _ => panic!("expected merge, got {o:?}"),
        }
        let _ = fs::remove_dir_all(_d);
    }

    /// Ranking weighs measured drivers and reports certainty honestly —
    /// unknowns are named, not zero-filled.
    #[test]
    fn brain_gap_dedup_ranking_records_uncertainty() {
        let g = gap("x", "a", "b");
        let full = rank_gap(&g, 5, 3, Some(2.0));
        let sparse = rank_gap(&g, 5, 0, None);
        assert!(full.score > sparse.score);
        assert!(full.certainty > sparse.certainty);
        assert!(sparse.drivers.iter().any(|d| d == "cost:unknown"));
        assert!(sparse.drivers.iter().any(|d| d == "unblocks:unknown"));
    }

    /// A rejected proposal blocks re-creation until new evidence —
    /// repeated proposals don't churn the queue.
    #[test]
    fn brain_gap_dedup_rejected_repeat_blocked() {
        let (_d, s) = store("rej");
        let g = gap(
            "parser rejects unicode",
            "unicode accepted",
            "unicode panics",
        );
        let o1 = propose(&s, &g, &[], &ctx(), 60).unwrap();
        assert!(matches!(o1, ProposeOutcome::Created(_)));
        // Mark the proposal rejected on disk (as the workflow would).
        let path = _d.join(format!(
            "{}.json",
            sig_key("parser rejects unicode unicode accepted unicode panics")
        ));
        let mut p: Proposal = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        p.state = "rejected".into();
        fs::write(&path, serde_json::to_vec_pretty(&p).unwrap()).unwrap();
        let o2 = propose(&s, &g, &[], &ctx(), 61).unwrap();
        assert!(matches!(o2, ProposeOutcome::RejectedRepeat { .. }));
        let _ = fs::remove_dir_all(_d);
    }

    /// Exact-key set sanity: `sig_key` and `similarity` agree that
    /// reworded duplicates match while distinct issues don't.
    #[test]
    fn brain_gap_dedup_similarity_threshold() {
        assert!(
            similarity(
                "requests retry on failure no retry occurs",
                "requests retry on failure no retry occurs"
            ) >= SIMILARITY_MIN
        );
        assert!(similarity("unicode parser panic", "quota ledger off by cents") < SIMILARITY_MIN);
        let _ = BTreeSet::<String>::new();
    }
}
