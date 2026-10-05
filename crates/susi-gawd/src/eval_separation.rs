//! Separate candidate generation from judging (VC-201-005).

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::io::Write;
use std::path::Path;

use crate::eval_contamination::{self, ContaminationReport};
use crate::experiment_lifecycle::{ExperimentLog, ExperimentState};
use crate::rsi_corpus::{CorpusIntegrityError, RsiCorpus};
use crate::scorecard::ImprovementScorecard;
use crate::susi_error::{EaiError, EaiResult};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateArtifact {
    pub id: String,
    /// What the candidate actually produced on the suite's inputs. The
    /// gate judges this, never `expected_results` — a candidate's own
    /// claim about what it expects is not evidence of what it did.
    pub actual_output: BTreeSet<String>,
    /// The candidate's own claim about what it expects to satisfy. Used
    /// only to catch a candidate inventing truth outside the held-out
    /// suite — it never substitutes for `actual_output` in the verdict.
    pub expected_results: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeldOutSuite {
    pub inputs: Vec<String>,
    pub expected: BTreeSet<String>,
}

impl HeldOutSuite {
    /// Build the judge's suite from a corpus's held-out fixtures. The
    /// corpus is verified first: a suite built from a tampered corpus
    /// would judge against inputs the evaluator never approved, so
    /// integrity failure means no suite at all. `evaluator_expected`
    /// values become the expected set — this view stays evaluator-side
    /// (`candidate_view` is what a candidate may see).
    pub fn from_corpus(
        corpus: &crate::rsi_corpus::RsiCorpus,
    ) -> Result<Self, crate::rsi_corpus::CorpusIntegrityError> {
        corpus.verify_integrity()?;
        let held = corpus.held_out();
        Ok(HeldOutSuite {
            inputs: held.iter().map(|f| f.input.clone()).collect(),
            expected: held
                .iter()
                .filter_map(|f| f.evaluator_expected.clone())
                .collect(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromotionGate {
    Pass,
    FailSelfAlteredExpectations,
    /// The candidate's actual output does not cover the held-out truth —
    /// including producing nothing at all.
    FailWrongOutput,
    FailWriteAttempt,
}

/// Evaluator: held-out inputs; candidate write perms unavailable.
///
/// Judges what the candidate actually produced, not what it claims: a
/// candidate inventing self-reported expectations outside the held-out
/// suite is caught first, and only a candidate whose `actual_output`
/// genuinely covers the suite's `expected` set — not an empty or partial
/// run — passes.
pub fn judge(
    candidate: &CandidateArtifact,
    suite: &HeldOutSuite,
    candidate_can_write: bool,
) -> PromotionGate {
    if candidate_can_write {
        return PromotionGate::FailWriteAttempt;
    }
    // Patch altering its own expected results cannot satisfy the gate.
    let invented: BTreeSet<_> = candidate
        .expected_results
        .difference(&suite.expected)
        .cloned()
        .collect();
    if !invented.is_empty() {
        return PromotionGate::FailSelfAlteredExpectations;
    }
    // The gate must judge what the candidate actually produced. A
    // candidate that produced nothing, or whose output does not cover
    // the held-out truth, fails — declaring a correct expectation is not
    // evidence of a correct result.
    if !suite.expected.is_subset(&candidate.actual_output) {
        return PromotionGate::FailWrongOutput;
    }
    PromotionGate::Pass
}

/// One candidate run's access footprint, alongside the candidate itself —
/// what training/memory ids it touched while producing `candidate`, which
/// [`evaluate`] checks for held-out contamination before trusting the
/// gate's verdict as promotable evidence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalRunInput {
    pub candidate: CandidateArtifact,
    pub candidate_can_write: bool,
    #[serde(default)]
    pub training_access: BTreeSet<String>,
    #[serde(default)]
    pub memory_access: BTreeSet<String>,
    /// The evaluator's measured scorecard for this run — the dimensions
    /// the run harness actually observed, judged against the corpus's
    /// predeclared `promotion_spec` when the gate passes. Absent
    /// measurements make no promotion claim: a `Pass` alone advances the
    /// experiment to Evaluated and no further.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measured: Option<ImprovementScorecard>,
}

/// The combined verdict [`evaluate`] records: a contaminated run is
/// refused outright, never conflated with (or allowed to shadow) what the
/// gate itself decided.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvalVerdict {
    Pass,
    FailContaminated,
    FailSelfAlteredExpectations,
    FailWrongOutput,
    FailWriteAttempt,
}

impl From<PromotionGate> for EvalVerdict {
    fn from(gate: PromotionGate) -> Self {
        match gate {
            PromotionGate::Pass => EvalVerdict::Pass,
            PromotionGate::FailSelfAlteredExpectations => EvalVerdict::FailSelfAlteredExpectations,
            PromotionGate::FailWrongOutput => EvalVerdict::FailWrongOutput,
            PromotionGate::FailWriteAttempt => EvalVerdict::FailWriteAttempt,
        }
    }
}

/// The durable record one evaluation run leaves behind: which corpus
/// revision judged which candidate, with what contamination/promotion
/// result, so a verdict is never only an in-memory result a caller could
/// silently drop.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalEvidence {
    pub corpus_revision: String,
    pub candidate_id: String,
    pub contamination: ContaminationReport,
    pub verdict: EvalVerdict,
    pub timestamp_unix: u64,
    /// Where the candidate's durable experiment landed after this run —
    /// `None` only for evidence produced by the pure [`evaluate`] path,
    /// which touches no experiment log.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub experiment_state: Option<ExperimentState>,
}

/// Judge one candidate run against `corpus`'s held-out suite: contamination
/// is checked first — a run that touched held-out content during training
/// or memory access can never pass regardless of what the gate itself
/// decides, since the held-out truth it is judged against is truth it was
/// already exposed to.
pub fn evaluate(
    corpus: &RsiCorpus,
    input: &EvalRunInput,
) -> Result<EvalEvidence, CorpusIntegrityError> {
    let suite = HeldOutSuite::from_corpus(corpus)?;
    let contamination =
        eval_contamination::detect_in_corpus(corpus, &input.training_access, &input.memory_access)?;
    let verdict = if contamination.contaminated {
        EvalVerdict::FailContaminated
    } else {
        judge(&input.candidate, &suite, input.candidate_can_write).into()
    };
    Ok(EvalEvidence {
        corpus_revision: corpus.revision.clone(),
        candidate_id: input.candidate.id.clone(),
        contamination,
        verdict,
        timestamp_unix: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        experiment_state: None,
    })
}

/// Durable experiment-log root, mirroring the evidence store: one record
/// per candidate under `workspace/.susi/experiments/`.
fn experiments_dir(workspace: &Path) -> std::path::PathBuf {
    workspace.join(".susi").join("experiments")
}

/// Advance the candidate's durable experiment through the lifecycle this
/// eval run earned: a fresh candidate is proposed, its judged run is
/// recorded (Proposed -> Isolated -> Evaluated), a failed verdict
/// rejects it, and only a `Pass` carrying the evaluator's measured
/// scorecard may attempt `PromotionReady` — against the corpus's
/// predeclared `promotion_spec`, never a bound the run itself supplied.
/// A terminal state stands: a revised candidate earns a new id rather
/// than reopening a concluded experiment.
fn advance_experiment(
    log: &mut ExperimentLog,
    evidence: &EvalEvidence,
    input: &EvalRunInput,
    corpus: &RsiCorpus,
) -> EaiResult<ExperimentState> {
    let id = evidence.candidate_id.clone();
    let state_of = |log: &ExperimentLog| {
        log.experiments
            .get(&id)
            .map_or(ExperimentState::Proposed, |e| e.state)
    };
    if let Some(state @ (ExperimentState::Rejected | ExperimentState::PromotionReady)) =
        log.experiments.get(&id).map(|e| e.state)
    {
        return Ok(state);
    }
    if !log.experiments.contains_key(&id) {
        log.propose(&id).map_err(EaiError::governance)?;
    }
    if state_of(log) == ExperimentState::Proposed {
        log.transition(&id, ExperimentState::Isolated)
            .map_err(EaiError::governance)?;
    }
    if state_of(log) == ExperimentState::Isolated {
        log.transition(&id, ExperimentState::Evaluated)
            .map_err(EaiError::governance)?;
    }
    match evidence.verdict {
        EvalVerdict::Pass => {
            if let (Some(measured), Some(spec)) =
                (input.measured.as_ref(), corpus.promotion_spec.as_ref())
            {
                // A scorecard that fails the declared spec leaves the
                // experiment Evaluated — recorded, never promoted.
                let _ = log.promote_with_scorecard(&id, measured, spec);
            }
        }
        EvalVerdict::FailContaminated
        | EvalVerdict::FailSelfAlteredExpectations
        | EvalVerdict::FailWrongOutput
        | EvalVerdict::FailWriteAttempt => {
            log.transition(&id, ExperimentState::Rejected)
                .map_err(EaiError::governance)?;
        }
    }
    Ok(state_of(log))
}

/// [`evaluate`], then durably append the evidence as JSONL under
/// `workspace/.susi/eval_evidence.jsonl` — the production record a
/// `susi tasks eval-corpus` run or a scheduled evaluation leaves behind,
/// mirroring how mission traces are recorded (`susi_core::mission_trace`).
pub fn evaluate_and_record(
    workspace: &Path,
    corpus: &RsiCorpus,
    input: &EvalRunInput,
) -> EaiResult<EvalEvidence> {
    let mut evidence = evaluate(corpus, input)
        .map_err(|e| EaiError::governance(format!("rsi corpus eval failed: {e}")))?;
    let mut log = ExperimentLog::load(experiments_dir(workspace));
    evidence.experiment_state = Some(advance_experiment(&mut log, &evidence, input, corpus)?);
    record(workspace, &evidence)?;
    if evidence.experiment_state == Some(ExperimentState::PromotionReady) {
        record_release_candidate(workspace, corpus, &evidence, input)?;
    }
    Ok(evidence)
}

fn record(workspace: &Path, evidence: &EvalEvidence) -> EaiResult<()> {
    let susi_dir = workspace.join(".susi");
    std::fs::create_dir_all(&susi_dir)
        .map_err(|e| EaiError::filesystem(format!("eval evidence dir create failed: {e}")))?;
    let mut line = serde_json::to_vec(evidence)
        .map_err(|e| EaiError::internal(format!("eval evidence serialization failed: {e}")))?;
    line.push(b'\n');
    let _lock = crate::susi_config::file_lock::FileLock::acquire(&susi_dir, "eval_evidence")
        .ok_or_else(|| EaiError::filesystem("eval evidence lock acquisition failed".to_string()))?;
    let path = susi_dir.join("eval_evidence.jsonl");
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| EaiError::filesystem(format!("eval evidence file open failed: {e}")))?;
    file.write_all(&line)
        .map_err(|e| EaiError::filesystem(format!("eval evidence write failed: {e}")))
}

/// Directory holding the reviewable release candidates a PromotionReady
/// experiment produces — the only artifacts `susi release` /
/// `susi-release-sync.sh` may later promote (VC-201-018).
fn release_candidates_dir(workspace: &Path) -> std::path::PathBuf {
    workspace.join(".susi").join("release-candidates")
}

/// Render `id` safe as a single filename component — a candidate id may
/// carry separators that must never become path segments.
fn candidate_file_name(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// The durable artifact left under `.susi/release-candidates/` for a
/// PromotionReady experiment: the `ReleaseCandidate` itself plus the
/// promotion decision the policy reached for it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseCandidateRecord {
    /// The reviewable candidate — `evidence_manifest` carries the full
    /// serialized manifest JSON.
    pub candidate: crate::rsi_promotion::ReleaseCandidate,
    /// `accepted_reviewable` when `decide_promotion` cleared the
    /// SusiRelease path — the manifest records the decision a reviewer
    /// would re-derive, not a silent promotion.
    pub decision: String,
}

/// A PromotionReady experiment produces its reviewable release candidate
/// and evidence manifest here: the manifest binds the eval evidence,
/// corpus revision, measured scorecard, the corpus's predeclared
/// promotion spec and the only allowed promoters, so review needs
/// nothing a caller could fabricate. The `Verification` handed to
/// `produce_release_candidate` is rebuilt from the recorded evidence —
/// `verified` is only ever set on the PromotionReady branch this
/// function is called from.
fn record_release_candidate(
    workspace: &Path,
    corpus: &RsiCorpus,
    evidence: &EvalEvidence,
    input: &EvalRunInput,
) -> EaiResult<()> {
    let verification = crate::cloud_rsi::Verification {
        verified: true,
        review: Some(crate::cloud_rsi::ReviewVerdict {
            verdict: "approve".into(),
            reasons: Vec::new(),
        }),
        reviewer: Some(format!("eval-corpus:{}", corpus.revision)),
        gates: vec![
            ("held_out_eval".to_string(), true),
            ("promotion_spec".to_string(), true),
        ],
        regressions: Vec::new(),
        fabricated: Vec::new(),
        failover: None,
        reasons: Vec::new(),
    };
    let digest = crate::eval_receipt::EvalReceipt::digest_bytes(
        serde_json::to_string(&input.candidate.actual_output)
            .unwrap_or_default()
            .as_bytes(),
    );
    let manifest = serde_json::to_string_pretty(&serde_json::json!({
        "eval_evidence": evidence,
        "corpus_revision": corpus.revision,
        "promotion_spec": corpus.promotion_spec,
        "measured": input.measured,
        "allowed_promoters": crate::rsi_promotion::ALLOWED_PROMOTERS,
    }))
    .unwrap_or_default();
    let candidate = crate::rsi_promotion::produce_release_candidate(
        &verification,
        &evidence.candidate_id,
        &digest,
        &manifest,
    )
    .map_err(|e| EaiError::governance(format!("release candidate refused: {e}")))?;
    let decision =
        match crate::rsi_promotion::decide_promotion(&crate::rsi_promotion::PromotionAttempt {
            candidate: candidate.clone(),
            via: crate::rsi_promotion::PromotionPath::SusiRelease,
        }) {
            crate::rsi_promotion::PromotionDecision::AcceptedReviewable => "accepted_reviewable",
            crate::rsi_promotion::PromotionDecision::Rejected(why) => {
                return Err(EaiError::governance(format!(
                    "promotion decision rejected reviewable candidate: {why}"
                )))
            }
        };
    let dir = release_candidates_dir(workspace);
    std::fs::create_dir_all(&dir)
        .map_err(|e| EaiError::filesystem(format!("release candidate dir create failed: {e}")))?;
    let record = ReleaseCandidateRecord {
        candidate,
        decision: decision.to_string(),
    };
    let bytes = serde_json::to_vec_pretty(&record)
        .map_err(|e| EaiError::internal(format!("release candidate serialization failed: {e}")))?;
    crate::susi_config::atomic_replace_file(
        &dir.join(format!(
            "{}.json",
            candidate_file_name(&evidence.candidate_id)
        )),
        &bytes,
    )
    .map_err(|e| EaiError::filesystem(format!("release candidate write failed: {e}")))
}

/// Read every eval evidence record under `workspace/.susi/`, skipping
/// lines that fail to parse.
#[must_use]
pub fn read_all(workspace: &Path) -> Vec<EvalEvidence> {
    let path = workspace.join(".susi").join("eval_evidence.jsonl");
    let Ok(content) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    content
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rsi_corpus::{
        make_fixture, FixtureClass, FixtureSpec, FixtureSplit, RSI_CORPUS_SCHEMA,
    };

    fn eval_ws(tag: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("susi-rsi-eval-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn sample_corpus(revision: &str) -> RsiCorpus {
        RsiCorpus {
            schema_version: RSI_CORPUS_SCHEMA.into(),
            revision: revision.into(),
            fixtures: vec![
                make_fixture(FixtureSpec {
                    id: "h1",
                    class: FixtureClass::Coding,
                    input: "hidden eval input",
                    split: FixtureSplit::HeldOut,
                    seed: 11,
                    evaluator_expected: Some("rubric".into()),
                }),
                make_fixture(FixtureSpec {
                    id: "t1",
                    class: FixtureClass::Coding,
                    input: "training input",
                    split: FixtureSplit::Train,
                    seed: 12,
                    evaluator_expected: None,
                }),
            ],
            promotion_spec: None,
        }
    }

    /// The production surface: load a corpus, judge a clean candidate run
    /// against its held-out suite, and record the promotion evidence —
    /// then show a run that touched held-out content during training is
    /// refused outright, and both runs persist as durable evidence a
    /// second reader can see without re-running anything.
    #[test]
    fn rsi_corpus_production_eval() {
        let ws = eval_ws("prod");
        let corpus = sample_corpus("rev-9");

        let clean = EvalRunInput {
            candidate: CandidateArtifact {
                id: "cand-pass".into(),
                actual_output: BTreeSet::from(["rubric".to_string()]),
                expected_results: BTreeSet::from(["rubric".to_string()]),
            },
            candidate_can_write: false,
            training_access: BTreeSet::new(),
            memory_access: BTreeSet::new(),
            measured: None,
        };
        let evidence = evaluate_and_record(&ws, &corpus, &clean).expect("clean eval");
        assert_eq!(evidence.verdict, EvalVerdict::Pass);
        assert!(!evidence.contamination.contaminated);
        assert_eq!(evidence.corpus_revision, "rev-9");

        let tainted = EvalRunInput {
            candidate: CandidateArtifact {
                id: "cand-tainted".into(),
                actual_output: BTreeSet::from(["rubric".to_string()]),
                expected_results: BTreeSet::from(["rubric".to_string()]),
            },
            candidate_can_write: false,
            training_access: BTreeSet::from(["h1".to_string()]),
            memory_access: BTreeSet::new(),
            measured: None,
        };
        let evidence2 = evaluate_and_record(&ws, &corpus, &tainted).expect("tainted eval");
        assert_eq!(evidence2.verdict, EvalVerdict::FailContaminated);
        assert!(evidence2.contamination.contaminated);

        // Both runs are durably recorded, not just returned in memory.
        let recorded = read_all(&ws);
        assert_eq!(recorded.len(), 2);
        assert_eq!(recorded[0].candidate_id, "cand-pass");
        assert_eq!(recorded[1].candidate_id, "cand-tainted");

        // A tampered corpus cannot seed an evaluation at all.
        let mut tampered = corpus.clone();
        tampered.fixtures[0].input = "swapped".into();
        assert!(evaluate(&tampered, &clean).is_err());

        let _ = std::fs::remove_dir_all(&ws);
    }
}
