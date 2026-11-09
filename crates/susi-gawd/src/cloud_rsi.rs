//! Cloud-backed self-development proposals (T-CODEX-16 / VC-201-012).
//!
//! Working cloud models analyze REDACTED repository evidence — failure
//! receipts, user feedback, roadmap gaps, repo facts — and return structured
//! improvement proposals. The model is a suggester, never an authority:
//!
//! - Evidence is redacted before it leaves the process (secrets, env creds).
//! - Every proposal must cite evidence that was actually supplied —
//!   `evidence` entries that match no observed fact are rejected as
//!   fabricated. Model output is a *suggestion*; facts are input.
//! - Accepted proposals become namespaced task drafts linked to a VALID
//!   roadmap vector, deduplicated against existing task titles.
//! - Dispatch goes through the production path (`cloud_failover::run`):
//!   availability evidence, task-class ranking, residency grants, spend
//!   policy, bounded attempts. Insufficient credit falls back to a
//!   permitted alternative; total blockage is an honest failure, never a
//!   fabricated proposal.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use susi_config::cloud_eligibility::InferenceResult;
use susi_error::redact::{mask_env_credentials, redact_patterns};
use susi_gawd_agents::cloud_intent::{Candidate, IntentConstraints};
use susi_gawd_swarm::cloud_failover::{
    run, AttemptOutcome, FailoverBudget, FailoverResult, Runner, Stores,
};

// ---------------------------------------------------------------- evidence

/// Observed inputs — the ONLY facts a proposal may cite. Model text that
/// invents supporting evidence is rejected.
#[derive(Debug, Clone, Default)]
pub struct RsiEvidence {
    /// Redacted failure receipts (e.g. "T-9: p2/mL HTTP 429 retry-after").
    pub failure_receipts: Vec<String>,
    /// Observed user feedback lines.
    pub user_feedback: Vec<String>,
    /// Open roadmap vector ids that are declared gaps.
    pub roadmap_gaps: Vec<String>,
    /// Repository facts (coverage gaps, audit findings, measured perf).
    pub repo_facts: Vec<String>,
}

impl RsiEvidence {
    /// All observed facts, normalized — the support set for citation checks.
    fn facts(&self) -> BTreeSet<String> {
        [
            self.failure_receipts.as_slice(),
            self.user_feedback.as_slice(),
            self.roadmap_gaps.as_slice(),
            self.repo_facts.as_slice(),
        ]
        .concat()
        .iter()
        .map(|f| normalize(f))
        .collect()
    }

    /// Render the analysis prompt with all secret material redacted.
    /// `secrets` are literal strings (tokens, keys) that must never appear.
    #[must_use]
    pub fn prompt(&self, secrets: &[String]) -> String {
        let section = |name: &str, items: &[String]| {
            if items.is_empty() {
                return String::new();
            }
            format!("{name}:\n{}\n", items.join("\n"))
        };
        let raw = format!(
            concat!(
                "Analyze the following observed SUSI evidence and propose concrete, ",
                "deduplicated improvements. Reply with ONLY JSON of shape\n",
                "{{\"proposals\":[{{\"title\",\"behavior\",\"benefit\",\"risks\":[..],",
                "\"acceptance\":[\"argv\",\"..\"],\"evidence\":[\"..\"],\"roadmap\":\"VC-...\"}}]}}\n",
                "Rules: evidence[] entries must quote supplied facts verbatim; ",
                "roadmap must be an existing vector id; acceptance must be an ",
                "executable command.\n\n",
                "{}{}{}{}"
            ),
            section("failure receipts", &self.failure_receipts),
            section("user feedback", &self.user_feedback),
            section("roadmap gaps", &self.roadmap_gaps),
            section("repository facts", &self.repo_facts),
        );
        redact_patterns(secrets, &mask_env_credentials(&raw))
    }
}

// ---------------------------------------------------------------- proposal

/// A validated improvement proposal — a model SUGGESTION backed by observed
/// facts, never a completed-work claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proposal {
    /// Short task title.
    pub title: String,
    /// Affected behavior (what changes for the user/system).
    pub behavior: String,
    /// Measurable benefit (what improves and how we'd know).
    pub benefit: String,
    /// Declared risks.
    pub risks: Vec<String>,
    /// Executable acceptance command argv.
    pub acceptance: Vec<String>,
    /// Facts from the supplied evidence supporting this proposal.
    pub evidence: Vec<String>,
    /// Roadmap vector id this advances.
    pub roadmap: String,
}

/// Why a model suggestion was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RejectReason {
    /// Unparseable model output or missing required fields.
    Malformed(String),
    /// Cited evidence that was never supplied — fabricated support.
    UnsupportedEvidence(String),
    /// Roadmap vector id does not exist.
    UnknownVector(String),
    /// Missing executable acceptance plan.
    NoAcceptance,
    /// Duplicate of an existing or earlier-proposed task.
    Duplicate,
    /// Repeats a rejection recorded in experiment memory (VC-201-019)
    /// without citing the prior proposal id or a supplied failure receipt.
    PriorRejection(String),
}

/// What a rejection looked like (suggestion kept for audit, marked refused).
#[derive(Debug, Clone)]
pub struct Rejected {
    /// Raw title (or "(unparseable)").
    pub title: String,
    /// Why refused.
    pub reason: RejectReason,
}

/// A minted task draft matching `.agents/tasks/*.json` shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MintedTask {
    /// Namespaced id suggestion (`T-<AGENT>-<n>`).
    pub id: String,
    /// Title.
    pub title: String,
    /// Goal text combining behavior + benefit.
    pub goal: String,
    /// Executable acceptance command.
    pub accept: AcceptCmd,
    /// Linked roadmap vector.
    pub roadmap: String,
    /// Declared risks carried into the task body.
    pub risks: Vec<String>,
    /// Minting agent namespace.
    pub created_by: String,
}

/// Acceptance command record (mirrors the queue's `accept.cmd`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptCmd {
    /// Argv.
    pub cmd: Vec<String>,
}

// ------------------------------------------------------------------ parse

/// Extract the JSON object from model output (tolerates code fences and
/// leading prose by slicing the outermost braces).
fn extract_json(text: &str) -> Result<String, RejectReason> {
    let start = text.find('{');
    let end = text.rfind('}');
    match (start, end) {
        (Some(s), Some(e)) if e > s => Ok(text[s..=e].to_string()),
        _ => Err(RejectReason::Malformed("no JSON object in output".into())),
    }
}

/// Case/punctuation-insensitive normalization for dedup and citation checks.
fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .flat_map(|c| c.to_lowercase())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Validate one raw proposal against supplied facts and valid vectors.
fn validate(
    p: Proposal,
    facts: &BTreeSet<String>,
    vectors: &BTreeSet<String>,
) -> Result<Proposal, RejectReason> {
    if p.title.trim().is_empty() || p.behavior.trim().is_empty() || p.benefit.trim().is_empty() {
        return Err(RejectReason::Malformed(
            "empty title/behavior/benefit".into(),
        ));
    }
    if p.acceptance.is_empty() {
        return Err(RejectReason::NoAcceptance);
    }
    if !vectors.contains(&p.roadmap) {
        return Err(RejectReason::UnknownVector(p.roadmap));
    }
    // Every cited fact must have been supplied — a citation that matches no
    // observed evidence is fabrication.
    for ev in &p.evidence {
        let n = normalize(ev);
        let supported = facts.iter().any(|f| f.contains(&n) || n.contains(f));
        if !supported {
            return Err(RejectReason::UnsupportedEvidence(ev.clone()));
        }
    }
    Ok(p)
}

/// The experiment-memory identity of a cloud proposal. The two stores
/// share no natural key, so the mapping is explicit: the proposal's
/// trimmed title is its id (a rejected eval candidate named like the
/// proposal title is the same experiment), its trimmed behavior is the
/// cause, and its first cited evidence line is the counterexample.
#[must_use]
pub fn proposal_memory_entry(p: &Proposal) -> crate::experiment_memory::ExperimentMemoryEntry {
    crate::experiment_memory::ExperimentMemoryEntry {
        proposal_id: p.title.trim().to_string(),
        cause: p.behavior.trim().to_string(),
        counterexample: p.evidence.first().cloned(),
        lineage: vec![p.roadmap.clone()],
    }
}

/// Refuse a proposal repeating a recorded rejection (VC-201-019). It must
/// either cite the prior proposal's id among its `evidence` (addressing
/// it), or cite at least one supplied failure receipt — verifiable new
/// failure evidence, since `validate` already proved every citation was
/// observed. Repo facts and roadmap gaps alone do not reopen a rejection.
pub fn check_proposal_memory(
    memory: &crate::experiment_memory::ExperimentMemory,
    p: &Proposal,
    evidence: &RsiEvidence,
) -> Result<(), RejectReason> {
    let receipts: BTreeSet<String> = evidence
        .failure_receipts
        .iter()
        .map(|r| normalize(r))
        .collect();
    let fresh: Vec<String> = p
        .evidence
        .iter()
        .filter(|ev| receipts.contains(&normalize(ev)))
        .cloned()
        .collect();
    if memory.may_resubmit(&proposal_memory_entry(p), &p.evidence, &fresh) {
        Ok(())
    } else {
        Err(RejectReason::PriorRejection(p.title.clone()))
    }
}

/// Mint namespaced task drafts, deduplicated against `existing_titles` and
/// within the batch (normalized title+behavior).
fn mint(
    proposals: &[Proposal],
    agent: &str,
    next_seq: u64,
    existing_titles: &BTreeSet<String>,
    rejected: &mut Vec<Rejected>,
) -> Vec<MintedTask> {
    let mut seen: BTreeSet<String> = existing_titles.clone();
    let mut out = Vec::new();
    let mut seq = next_seq;
    for p in proposals {
        let key = format!("{}|{}", normalize(&p.title), normalize(&p.behavior));
        if !seen.insert(key.clone()) {
            rejected.push(Rejected {
                title: p.title.clone(),
                reason: RejectReason::Duplicate,
            });
            continue;
        }
        seq += 1;
        out.push(MintedTask {
            id: format!("T-{}-{seq}", agent.to_uppercase()),
            title: p.title.clone(),
            goal: format!("{}\n\nBenefit: {}", p.behavior, p.benefit),
            accept: AcceptCmd {
                cmd: p.acceptance.clone(),
            },
            roadmap: p.roadmap.clone(),
            risks: p.risks.clone(),
            created_by: agent.to_uppercase(),
        });
    }
    out
}

// --------------------------------------------------------------- dispatch

/// The cloud model seam — production wraps a provider call; tests inject
/// scripted behavior per candidate.
pub trait ProposalModel: Send + Sync {
    /// Produce raw proposal JSON for `prompt` on candidate `c`.
    fn propose(&self, c: &Candidate, prompt: &str) -> AttemptOutcome;
}

struct ProposalRunner<'a> {
    backend: &'a dyn ProposalModel,
    candidates: &'a [Candidate],
    prompt: String,
}

impl Runner for ProposalRunner<'_> {
    fn attempt(&mut self, index: usize, _remaining_ms: u64) -> AttemptOutcome {
        let Some(c) = self.candidates.get(index) else {
            return AttemptOutcome::PreDispatch(InferenceResult::Failed {
                status: None,
                body_snippet: "no such candidate".into(),
                retry_after_secs: None,
            });
        };
        self.backend.propose(c, &self.prompt)
    }
}

/// Policy knobs for one proposal round.
pub struct ProposalPolicy {
    /// Valid roadmap vector ids.
    pub valid_vectors: BTreeSet<String>,
    /// Normalized titles of tasks that already exist.
    pub existing_titles: BTreeSet<String>,
    /// Agent namespace for minted ids.
    pub agent: String,
    /// Next free task sequence number for this agent.
    pub next_seq: u64,
    /// Durable experiment memory (`ExperimentMemory::load(workspace)`);
    /// when present, every validated proposal must pass
    /// [`check_proposal_memory`] before it can be minted.
    pub experiment_memory: Option<crate::experiment_memory::ExperimentMemory>,
}

/// Everything a proposal round produced.
pub struct ProposalRound {
    /// Dispatch audit trail from the production path.
    pub failover: FailoverResult,
    /// The redacted prompt actually sent (for audit).
    pub prompt: String,
    /// Validated, deduplicated proposals — model suggestions only.
    pub proposals: Vec<Proposal>,
    /// Refused suggestions with reasons.
    pub rejected: Vec<Rejected>,
    /// Namespaced task drafts ready to enqueue.
    pub tasks: Vec<MintedTask>,
}

/// Run one evidence-backed proposal round through production dispatch.
/// Insufficient credit, lockouts and outages fail over per policy; a fully
/// blocked round returns an empty proposal set with the real stop reason —
/// never a fabricated proposal.
#[allow(clippy::too_many_arguments)] // every parameter is an injected seam
pub fn run_proposals(
    intent: &IntentConstraints,
    candidates: &[Candidate],
    stores: &mut Stores<'_>,
    budget: FailoverBudget,
    backend: &dyn ProposalModel,
    evidence: &RsiEvidence,
    secrets: &[String],
    policy: &ProposalPolicy,
) -> ProposalRound {
    let prompt = evidence.prompt(secrets);
    let mut runner = ProposalRunner {
        backend,
        candidates,
        prompt: prompt.clone(),
    };
    let failover = run(intent, candidates, stores, budget, &mut runner);
    let mut round = ProposalRound {
        failover,
        prompt,
        proposals: Vec::new(),
        rejected: Vec::new(),
        tasks: Vec::new(),
    };
    let Some(raw) = round.failover.output.clone() else {
        return round; // honest blockage — stop reason carries the why
    };
    let parsed: Result<Proposals, RejectReason> = extract_json(&raw)
        .and_then(|j| serde_json::from_str(&j).map_err(|e| RejectReason::Malformed(e.to_string())));
    let parsed = match parsed {
        Ok(p) => p,
        Err(reason) => {
            round.rejected.push(Rejected {
                title: "(unparseable)".into(),
                reason,
            });
            return round;
        }
    };
    let facts = evidence.facts();
    for p in parsed.proposals {
        let checked = validate(p, &facts, &policy.valid_vectors).and_then(|v| {
            match policy.experiment_memory.as_ref() {
                Some(memory) => check_proposal_memory(memory, &v, evidence).map(|()| v),
                None => Ok(v),
            }
        });
        match checked {
            Ok(v) => round.proposals.push(v),
            Err(reason) => {
                let title = reason_title(&reason);
                round.rejected.push(Rejected { title, reason });
            }
        }
    }
    round.tasks = mint(
        &round.proposals,
        &policy.agent,
        policy.next_seq,
        &policy.existing_titles,
        &mut round.rejected,
    );
    round
}

fn reason_title(r: &RejectReason) -> String {
    match r {
        RejectReason::Malformed(t) => format!("malformed: {t}"),
        RejectReason::UnsupportedEvidence(e) => format!("unsupported: {e}"),
        RejectReason::UnknownVector(v) => format!("unknown vector {v}"),
        RejectReason::NoAcceptance => "no acceptance plan".into(),
        RejectReason::Duplicate => "duplicate".into(),
        RejectReason::PriorRejection(t) => format!("repeats recorded rejection: {t}"),
    }
}

#[derive(Deserialize)]
struct Proposals {
    proposals: Vec<Proposal>,
}

// ============================================================ implementation
//
// The implementation loop (T-CODEX-17 / VC-201-013: a cloud model's patch
// plan applied and verified through the patch cycle by the parallel roadmap
// agents, with capability gaps delegated over A2A) moved to Susi Commercial
// Apps' Frontier Lab with those agents (T-SWARMMOVE-1). It reaches this
// core's patch cycle through the GMCP `apply_patch_cycle` tool.

use std::path::Path;

// ============================================================== verification
//
// T-CODEX-18 / VC-201-014: independent review + measured validation of a
// candidate change. Two separations are enforced structurally:
//
// - PROVENANCE: the reviewing model must differ from the implementer — a
//   self-review is refused before it runs.
// - AUTHORITY: reviewer approval is advisory only. A change verifies only
//   when the exact acceptance command AND the workspace gates (fmt, clippy,
//   test) pass AND measured behavior does not regress beyond tolerance AND
//   the claimed tool receipts match what actually ran — fabricated receipts
//   and missing checks are rejections, not warnings.

/// The evidence a reviewer and the gates judge: real diff, the task's own
/// requirements, the receipts the implementer claims, and the preserved
/// baseline to compare against.
#[derive(Debug, Clone)]
pub struct ReviewPacket {
    /// Task id under review.
    pub task_id: String,
    /// The actual diff under review (`git diff` output).
    pub diff: String,
    /// Acceptance argv the work must leave passing.
    pub accept: Vec<String>,
    /// Opaque id of the model that produced the change.
    pub implementer: String,
    /// Receipts the implementer claims it ran (command + exit code lines).
    pub claimed_receipts: Vec<String>,
    /// Receipts actually recorded by the tool loop.
    pub actual_receipts: Vec<String>,
    /// Preserved baseline metrics (name → value).
    pub baseline: BTreeMap<String, f64>,
    /// Metrics measured on the held-out workload for this change.
    pub measured: BTreeMap<String, f64>,
}

/// The independent reviewer's verdict — advisory, parsed from model output
/// `{"verdict":"approve"|"reject","reasons":[..]}`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ReviewVerdict {
    /// "approve" or "reject".
    pub verdict: String,
    /// Reviewer's reasons.
    #[serde(default)]
    pub reasons: Vec<String>,
}

/// Independent review seam — production wraps a provider call on a
/// DIFFERENT model than the implementer.
pub trait ReviewModel: Send + Sync {
    /// Critique `packet` on candidate `c`; Success payload is the verdict JSON.
    fn review(&self, c: &Candidate, packet: &ReviewPacket) -> AttemptOutcome;
}

/// Executes one verification gate in the candidate worktree.
pub trait GateRunner: Send + Sync {
    /// Returns true iff the gate passed (exit 0).
    fn run_gate(&self, worktree: &Path, name: &str, argv: &[String]) -> bool;
}

/// Policy for the verification stage.
pub struct VerifyPolicy {
    /// Gates to run in order (name, argv). The exact task acceptance
    /// command is always run first, in addition to these.
    pub gates: Vec<(String, Vec<String>)>,
    /// Allowed regression percent on any measured metric vs baseline.
    pub max_regress_pct: f64,
}

/// Final verification outcome.
#[derive(Debug)]
pub struct Verification {
    /// True only when: independent reviewer approved AND every gate passed
    /// AND no regression AND no fabricated receipts.
    pub verified: bool,
    /// Reviewer verdict (None when review never ran — e.g. self-approval
    /// refused or all reviewers blocked).
    pub review: Option<ReviewVerdict>,
    /// The reviewer's opaque id (must differ from the implementer).
    pub reviewer: Option<String>,
    /// Per-gate results, in run order.
    pub gates: Vec<(String, bool)>,
    /// Metrics that regressed beyond tolerance.
    pub regressions: Vec<String>,
    /// Claimed receipts with no matching actual execution.
    pub fabricated: Vec<String>,
    /// Dispatch audit trail of the reviewer call.
    pub failover: Option<FailoverResult>,
    /// Human-readable rejection reasons, if not verified.
    pub reasons: Vec<String>,
}

struct ReviewRunner<'a> {
    backend: &'a dyn ReviewModel,
    candidates: &'a [Candidate],
    packet: &'a ReviewPacket,
}
impl Runner for ReviewRunner<'_> {
    fn attempt(&mut self, index: usize, _r: u64) -> AttemptOutcome {
        let Some(c) = self.candidates.get(index) else {
            return AttemptOutcome::PreDispatch(InferenceResult::Failed {
                status: None,
                body_snippet: "no such candidate".into(),
                retry_after_secs: None,
            });
        };
        self.backend.review(c, self.packet)
    }
}

/// Verify a candidate change end-to-end. Never called on the installed
/// release instance — `worktree` is the isolated dev workspace.
#[allow(clippy::too_many_arguments)] // every parameter is an injected seam
pub fn verify_candidate(
    intent: &IntentConstraints,
    reviewers: &[Candidate],
    stores: &mut Stores<'_>,
    budget: FailoverBudget,
    model: &dyn ReviewModel,
    gates: &dyn GateRunner,
    packet: &ReviewPacket,
    policy: &VerifyPolicy,
    worktree: &Path,
) -> Verification {
    let mut v = Verification {
        verified: false,
        review: None,
        reviewer: None,
        gates: Vec::new(),
        regressions: Vec::new(),
        fabricated: Vec::new(),
        failover: None,
        reasons: Vec::new(),
    };
    // Fabricated receipts are rejected regardless of everything else.
    let actual: BTreeSet<&String> = packet.actual_receipts.iter().collect();
    v.fabricated = packet
        .claimed_receipts
        .iter()
        .filter(|r| !actual.contains(r))
        .cloned()
        .collect();
    if !v.fabricated.is_empty() {
        v.reasons
            .push(format!("fabricated receipts: {}", v.fabricated.join(", ")));
    }
    // Independent reviewer via production failover — the implementer's
    // model is excluded from the reviewer pool before selection.
    let pool: Vec<Candidate> = reviewers
        .iter()
        .filter(|c| c.opaque_id() != packet.implementer)
        .cloned()
        .collect();
    if pool.is_empty() {
        v.reasons
            .push("self-approval refused: no reviewer distinct from implementer".into());
    } else {
        let mut runner = ReviewRunner {
            backend: model,
            candidates: &pool,
            packet,
        };
        let res = run(intent, &pool, stores, budget, &mut runner);
        let approved = res.output.clone().and_then(|raw| {
            extract_json(&raw)
                .ok()
                .and_then(|j| serde_json::from_str::<ReviewVerdict>(&j).ok())
        });
        v.reviewer = res.winner.map(|i| pool[i].opaque_id());
        match &approved {
            Some(rv) => {
                if rv.verdict == "approve" {
                    v.review = Some(rv.clone());
                } else {
                    v.reasons
                        .push(format!("reviewer rejected: {}", rv.reasons.join("; ")));
                    v.review = Some(rv.clone());
                }
            }
            None => {
                v.reasons.push("no reviewer produced a verdict".into());
            }
        }
        v.failover = Some(res);
    }
    // Gates — the exact task acceptance command first, then the workspace
    // gates. Reviewer approval cannot substitute for a failing gate.
    let mut gate_list: Vec<(String, Vec<String>)> =
        vec![(format!("accept:{}", packet.task_id), packet.accept.clone())];
    gate_list.extend(policy.gates.iter().cloned());
    for (name, argv) in &gate_list {
        let ok = gates.run_gate(worktree, name, argv);
        v.gates.push((name.clone(), ok));
        if !ok {
            v.reasons.push(format!("gate failed: {name}"));
        }
    }
    // Baseline comparison — measured metrics must not regress.
    for (name, base) in &packet.baseline {
        let Some(meas) = packet.measured.get(name) else {
            v.regressions.push(format!("{name}: missing measurement"));
            continue;
        };
        if *base > 0.0 {
            let regress = (base - meas) / base * 100.0;
            if regress > policy.max_regress_pct {
                v.regressions.push(format!(
                    "{name}: {base} -> {meas} ({regress:.1}% regression)"
                ));
            }
        }
    }
    if !v.regressions.is_empty() {
        v.reasons
            .push(format!("regressions: {}", v.regressions.join("; ")));
    }
    v.verified = v.reasons.is_empty() && v.review.as_ref().is_some_and(|r| r.verdict == "approve");
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::Mutex;
    use susi_config::cloud_eligibility::EligibilityStore;
    use susi_config::cloud_quota::QuotaInventory;
    use susi_gawd_agents::cloud_budget::{BudgetLedger, SpendPolicy};
    use susi_gawd_swarm::cloud_failover::FailoverStop;
    use susi_gawd_swarm::cloud_lockout::{now_unix, LockoutPolicy, LockoutTracker};

    const NOW_MS: u64 = 1_700_000_000_000;

    fn cand(key: &str, model: &str, provider: &str, account: &str, cost: f64) -> Candidate {
        Candidate {
            provider: provider.into(),
            api_key: key.into(),
            account: Some(account.into()),
            region: None,
            model: model.into(),
            context_tokens: 128_000,
            modalities: vec![],
            supports_tools: true,
            supports_structured_output: true,
            residency: None,
            est_latency_ms: 10,
            cost_per_mtok: Some(cost),
            quality: BTreeMap::new(),
        }
    }

    struct StoresOwned {
        elig: Mutex<EligibilityStore>,
        quota: QuotaInventory,
        lock: Mutex<LockoutTracker>,
        ledger: BudgetLedger,
    }
    impl StoresOwned {
        fn new() -> Self {
            Self {
                elig: Mutex::new(EligibilityStore::new()),
                quota: QuotaInventory::new(),
                lock: Mutex::new(LockoutTracker::new(LockoutPolicy::default(), now_unix)),
                ledger: BudgetLedger::new(),
            }
        }
        fn stores(&mut self) -> Stores<'_> {
            Stores {
                eligibility: self.elig.get_mut().unwrap_or_else(|e| e.into_inner()),
                quota: &self.quota,
                lockouts: self.lock.get_mut().unwrap_or_else(|e| e.into_inner()),
                ledger: &self.ledger,
            }
        }
    }

    fn budget() -> FailoverBudget {
        FailoverBudget {
            max_attempts: 4,
            deadline_ms: Some(NOW_MS + 60_000),
            spend: SpendPolicy::FreeOnly,
            attempt_estimate_micros: 1_000,
            now_ms: NOW_MS,
        }
    }

    fn evidence() -> RsiEvidence {
        RsiEvidence {
            failure_receipts: vec!["T-9: p2/mL HTTP 429 retry-after 60s".into()],
            user_feedback: vec![],
            roadmap_gaps: vec!["VC-201-012".into()],
            repo_facts: vec!["bloat audit: 3 dead modules in susi-gawd".into()],
        }
    }

    fn policy() -> ProposalPolicy {
        ProposalPolicy {
            valid_vectors: ["VC-201-012", "VC-201-013"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            existing_titles: BTreeSet::new(),
            agent: "devin".into(),
            next_seq: 100,
            experiment_memory: None,
        }
    }

    /// Scripted backend: per-model canned output.
    struct FakeModel {
        out: Mutex<BTreeMap<String, Vec<AttemptOutcome>>>,
        pub seen_prompts: Mutex<Vec<String>>,
    }
    impl FakeModel {
        fn say(model: &str, body: &str) -> Self {
            let mut m = BTreeMap::new();
            m.insert(
                model.to_string(),
                vec![AttemptOutcome::Success(body.to_string())],
            );
            Self {
                out: Mutex::new(m),
                seen_prompts: Mutex::new(Vec::new()),
            }
        }
    }
    impl ProposalModel for FakeModel {
        fn propose(&self, c: &Candidate, prompt: &str) -> AttemptOutcome {
            self.seen_prompts
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(prompt.to_string());
            let mut m = self.out.lock().unwrap_or_else(|e| e.into_inner());
            let entry = m.entry(c.model.clone()).or_default();
            if entry.is_empty() {
                return AttemptOutcome::PreDispatch(InferenceResult::Failed {
                    status: Some(500),
                    body_snippet: "no script".into(),
                    retry_after_secs: None,
                });
            }
            if entry.len() == 1 {
                return entry[0].clone();
            }
            entry.remove(0)
        }
    }

    fn good_proposal_json() -> String {
        serde_json::json!({
            "proposals": [
                {
                    "title": "Remove dead modules from susi-gawd",
                    "behavior": "delete the 3 dead modules flagged by the audit",
                    "benefit": "smaller build, zero dead-code warnings",
                    "risks": ["dead module may still be wired by a feature flag"],
                    "acceptance": ["cargo", "clippy", "-p", "susi-gawd", "--", "-D", "warnings"],
                    "evidence": ["bloat audit: 3 dead modules in susi-gawd"],
                    "roadmap": "VC-201-012"
                }
            ]
        })
        .to_string()
    }

    #[test]
    fn cloud_rsi_proposals_validated_and_minted_as_namespaced_tasks() {
        let mut s = StoresOwned::new();
        let model = FakeModel::say("m1", &good_proposal_json());
        let cs = vec![cand("k1", "m1", "p1", "a1", 0.0)];
        let round = run_proposals(
            &IntentConstraints::default(),
            &cs,
            &mut s.stores(),
            budget(),
            &model,
            &evidence(),
            &[],
            &policy(),
        );
        assert_eq!(round.proposals.len(), 1);
        assert!(round.rejected.is_empty());
        assert_eq!(round.tasks.len(), 1);
        let t = &round.tasks[0];
        assert_eq!(t.id, "T-DEVIN-101");
        assert_eq!(t.roadmap, "VC-201-012");
        assert_eq!(t.created_by, "DEVIN");
        assert_eq!(t.accept.cmd[0], "cargo");
    }

    #[test]
    fn cloud_rsi_proposals_fabricated_evidence_and_bad_vector_rejected() {
        let body = serde_json::json!({
            "proposals": [
                {
                    "title": "Fabricated",
                    "behavior": "fix a thing",
                    "benefit": "it helps",
                    "risks": [],
                    "acceptance": ["cargo", "test"],
                    "evidence": ["a fact the model invented out of nowhere"],
                    "roadmap": "VC-201-012"
                },
                {
                    "title": "BadVector",
                    "behavior": "fix a thing",
                    "benefit": "it helps",
                    "risks": [],
                    "acceptance": ["cargo", "test"],
                    "evidence": ["bloat audit: 3 dead modules in susi-gawd"],
                    "roadmap": "VC-999-001"
                },
                {
                    "title": "NoAccept",
                    "behavior": "fix a thing",
                    "benefit": "it helps",
                    "risks": [],
                    "acceptance": [],
                    "evidence": ["bloat audit: 3 dead modules in susi-gawd"],
                    "roadmap": "VC-201-012"
                }
            ]
        })
        .to_string();
        let mut s = StoresOwned::new();
        let model = FakeModel::say("m1", &body);
        let cs = vec![cand("k1", "m1", "p1", "a1", 0.0)];
        let round = run_proposals(
            &IntentConstraints::default(),
            &cs,
            &mut s.stores(),
            budget(),
            &model,
            &evidence(),
            &[],
            &policy(),
        );
        assert!(round.proposals.is_empty());
        assert_eq!(round.rejected.len(), 3);
        assert!(round
            .rejected
            .iter()
            .any(|r| matches!(r.reason, RejectReason::UnsupportedEvidence(_))));
        assert!(round
            .rejected
            .iter()
            .any(|r| matches!(r.reason, RejectReason::UnknownVector(_))));
        assert!(round
            .rejected
            .iter()
            .any(|r| matches!(r.reason, RejectReason::NoAcceptance)));
        assert!(round.tasks.is_empty());
    }

    #[test]
    fn cloud_rsi_proposals_dedups_existing_and_batch() {
        // The same proposal twice plus one colliding with an existing task.
        let p = Proposal {
            title: "Cleanup pass".into(),
            behavior: "delete the 3 dead modules flagged by the audit".into(),
            benefit: "smaller build".into(),
            risks: vec![],
            acceptance: vec!["cargo".into(), "clippy".into()],
            evidence: vec!["bloat audit: 3 dead modules in susi-gawd".into()],
            roadmap: "VC-201-012".into(),
        };
        let proposals = vec![
            p.clone(),
            p.clone(),
            Proposal {
                title: "Existing work".into(),
                ..p.clone()
            },
        ];
        let mut rejected = Vec::new();
        let existing: BTreeSet<String> = [format!(
            "{}|{}",
            normalize("Existing work"),
            normalize("delete the 3 dead modules flagged by the audit")
        )]
        .into_iter()
        .collect();
        let tasks = mint(&proposals, "devin", 50, &existing, &mut rejected);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, "T-DEVIN-51");
        assert_eq!(rejected.len(), 2);
        assert!(rejected
            .iter()
            .all(|r| matches!(r.reason, RejectReason::Duplicate)));
    }

    #[test]
    fn cloud_rsi_proposals_redacts_secrets_from_prompt() {
        let ev = RsiEvidence {
            failure_receipts: vec!["key sk-SECRET123 saw 401".into()],
            user_feedback: vec![],
            roadmap_gaps: vec![],
            repo_facts: vec!["bloat audit: 3 dead modules in susi-gawd".into()],
        };
        let secrets = vec!["sk-SECRET123".to_string()];
        let prompt = ev.prompt(&secrets);
        assert!(!prompt.contains("sk-SECRET123"), "secret leaked: {prompt}");
        let mut s = StoresOwned::new();
        let model = FakeModel::say("m1", &good_proposal_json());
        let cs = vec![cand("k1", "m1", "p1", "a1", 0.0)];
        let round = run_proposals(
            &IntentConstraints::default(),
            &cs,
            &mut s.stores(),
            budget(),
            &model,
            &ev,
            &secrets,
            &policy(),
        );
        for p in model.seen_prompts.lock().unwrap().iter() {
            assert!(!p.contains("sk-SECRET123"));
        }
        assert_eq!(round.proposals.len(), 1);
    }

    #[test]
    fn cloud_rsi_proposals_credit_denied_falls_back_to_free_model() {
        // Paid model first-ranked (lower latency), but FreeOnly denies it —
        // the free working model answers instead.
        let mut s = StoresOwned::new();
        let model = FakeModel::say("mF", &good_proposal_json());
        let mut paid = cand("kP", "mP", "p1", "a1", 5.0);
        paid.est_latency_ms = 1;
        let cs = vec![paid, cand("kF", "mF", "p2", "a2", 0.0)];
        let round = run_proposals(
            &IntentConstraints::default(),
            &cs,
            &mut s.stores(),
            budget(),
            &model,
            &evidence(),
            &[],
            &policy(),
        );
        assert_eq!(round.proposals.len(), 1);
        // The winner was the free model.
        assert_eq!(round.failover.winner, Some(1));
        assert_eq!(s.ledger.account_exposure("a1"), 0);
    }

    #[test]
    fn cloud_rsi_proposals_total_blockage_is_honest_failure() {
        struct Dead;
        impl ProposalModel for Dead {
            fn propose(&self, _c: &Candidate, _p: &str) -> AttemptOutcome {
                AttemptOutcome::PreDispatch(InferenceResult::Failed {
                    status: Some(503),
                    body_snippet: "outage".into(),
                    retry_after_secs: None,
                })
            }
        }
        let mut s = StoresOwned::new();
        let cs = vec![
            cand("k1", "m1", "p1", "a1", 0.0),
            cand("k2", "m2", "p2", "a2", 0.0),
        ];
        let round = run_proposals(
            &IntentConstraints::default(),
            &cs,
            &mut s.stores(),
            budget(),
            &Dead,
            &evidence(),
            &[],
            &policy(),
        );
        assert!(round.proposals.is_empty());
        assert!(round.tasks.is_empty());
        assert!(round.failover.output.is_none());
        assert!(matches!(
            round.failover.stop,
            Some(FailoverStop::AttemptBudgetExhausted { .. })
        ));
        // Failures were recorded as evidence, not silently swallowed.
        assert!(!s.elig.lock().unwrap().is_empty());
    }

    #[test]
    fn cloud_rsi_proposals_malformed_output_rejected_cleanly() {
        let mut s = StoresOwned::new();
        let model = FakeModel::say("m1", "sorry, I cannot help with that");
        let cs = vec![cand("k1", "m1", "p1", "a1", 0.0)];
        let round = run_proposals(
            &IntentConstraints::default(),
            &cs,
            &mut s.stores(),
            budget(),
            &model,
            &evidence(),
            &[],
            &policy(),
        );
        assert!(round.proposals.is_empty());
        assert_eq!(round.rejected.len(), 1);
        assert!(matches!(
            round.rejected[0].reason,
            RejectReason::Malformed(_)
        ));
    }

    // ------------------------------------------- verification (T-CODEX-18)

    /// Scripted review backend: verdict JSON per model, or a crash.
    struct FakeReview {
        script: Mutex<BTreeMap<String, AttemptOutcome>>,
    }
    impl FakeReview {
        fn approve(model: &str) -> Self {
            Self::with(
                model,
                r#"{"verdict":"approve","reasons":["diff satisfies the task"]}"#,
            )
        }
        fn with(model: &str, body: &str) -> Self {
            let mut m = BTreeMap::new();
            m.insert(model.to_string(), AttemptOutcome::Success(body.to_string()));
            Self {
                script: Mutex::new(m),
            }
        }
        fn dead(_model: &str) -> AttemptOutcome {
            AttemptOutcome::PreDispatch(InferenceResult::Failed {
                status: Some(503),
                body_snippet: "down".into(),
                retry_after_secs: None,
            })
        }
    }
    impl ReviewModel for FakeReview {
        fn review(&self, c: &Candidate, _p: &ReviewPacket) -> AttemptOutcome {
            self.script
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&c.model)
                .cloned()
                .unwrap_or_else(|| FakeReview::dead(&c.model))
        }
    }

    /// Scripted gates: name → pass/fail.
    struct FakeGates(BTreeMap<String, bool>);
    impl GateRunner for FakeGates {
        fn run_gate(&self, _w: &Path, name: &str, _argv: &[String]) -> bool {
            *self.0.get(name).unwrap_or(&true)
        }
    }

    fn impl_cand() -> Candidate {
        cand("kI", "mI", "pI", "aI", 0.0)
    }
    fn rev_cand(tag: &str) -> Candidate {
        cand(
            &format!("k{tag}"),
            &format!("m{tag}"),
            &format!("p{tag}"),
            &format!("a{tag}"),
            0.0,
        )
    }

    fn packet() -> ReviewPacket {
        ReviewPacket {
            task_id: "T-9".into(),
            diff: "diff --git a/f.rs b/f.rs\n+fn f() {}\n".into(),
            accept: vec!["cargo".into(), "test".into()],
            implementer: impl_cand().opaque_id(),
            claimed_receipts: vec!["cargo test -> 0".into()],
            actual_receipts: vec!["cargo test -> 0".into()],
            baseline: [("throughput".into(), 100.0)].into_iter().collect(),
            measured: [("throughput".into(), 102.0)].into_iter().collect(),
        }
    }

    fn verify_policy() -> VerifyPolicy {
        VerifyPolicy {
            gates: vec![
                ("fmt".into(), vec!["cargo".into(), "fmt".into()]),
                ("clippy".into(), vec!["cargo".into(), "clippy".into()]),
            ],
            max_regress_pct: 5.0,
        }
    }

    fn verify_with(
        reviewers: &[Candidate],
        model: &dyn ReviewModel,
        gates: &dyn GateRunner,
        packet: &ReviewPacket,
    ) -> Verification {
        let mut s = StoresOwned::new();
        let wt = std::env::temp_dir();
        verify_candidate(
            &IntentConstraints::default(),
            reviewers,
            &mut s.stores(),
            budget(),
            model,
            gates,
            packet,
            &verify_policy(),
            &wt,
        )
    }

    #[test]
    fn cloud_rsi_verification_independent_approval_plus_gates_verifies() {
        let reviewers = vec![impl_cand(), rev_cand("R1")];
        let v = verify_with(
            &reviewers,
            &FakeReview::approve("mR1"),
            &FakeGates(BTreeMap::new()),
            &packet(),
        );
        assert!(v.verified, "reasons: {:?}", v.reasons);
        assert_eq!(
            v.reviewer.as_deref(),
            Some(rev_cand("R1").opaque_id().as_str())
        );
        assert_ne!(v.reviewer.as_deref(), Some(packet().implementer.as_str()));
        assert_eq!(v.gates.len(), 3, "accept + fmt + clippy all ran");
        assert!(v.gates.iter().all(|(_, ok)| *ok));
    }

    #[test]
    fn cloud_rsi_verification_self_approval_is_refused() {
        // The only "reviewer" available is the implementer itself.
        let reviewers = vec![impl_cand()];
        let v = verify_with(
            &reviewers,
            &FakeReview::approve("mI"),
            &FakeGates(BTreeMap::new()),
            &packet(),
        );
        assert!(!v.verified);
        assert!(v.reasons.iter().any(|r| r.contains("self-approval")));
    }

    #[test]
    fn cloud_rsi_verification_approval_cannot_replace_failing_gates() {
        let mut g = BTreeMap::new();
        g.insert("clippy".to_string(), false);
        let reviewers = vec![rev_cand("R1")];
        let v = verify_with(
            &reviewers,
            &FakeReview::approve("mR1"),
            &FakeGates(g),
            &packet(),
        );
        assert!(!v.verified, "approval alone must not pass");
        assert!(v.reasons.iter().any(|r| r.contains("gate failed: clippy")));
    }

    #[test]
    fn cloud_rsi_verification_fabricated_receipts_are_rejected() {
        let mut p = packet();
        p.claimed_receipts.push("cargo publish -> 0".into()); // never ran
        let reviewers = vec![rev_cand("R1")];
        let v = verify_with(
            &reviewers,
            &FakeReview::approve("mR1"),
            &FakeGates(BTreeMap::new()),
            &p,
        );
        assert!(!v.verified);
        assert_eq!(v.fabricated, vec!["cargo publish -> 0".to_string()]);
    }

    #[test]
    fn cloud_rsi_verification_regression_beyond_tolerance_rejected() {
        let mut p = packet();
        p.measured.insert("throughput".into(), 80.0); // -20%
        let reviewers = vec![rev_cand("R1")];
        let v = verify_with(
            &reviewers,
            &FakeReview::approve("mR1"),
            &FakeGates(BTreeMap::new()),
            &p,
        );
        assert!(!v.verified);
        assert!(v.reasons.iter().any(|r| r.contains("regression")));
    }

    #[test]
    fn cloud_rsi_verification_blocked_reviewer_fails_over_independently() {
        // Reviewer A is down; reviewer B reviews — never the implementer.
        let mut m = BTreeMap::new();
        m.insert("mRA".to_string(), FakeReview::dead("mRA"));
        m.insert(
            "mRB".to_string(),
            AttemptOutcome::Success(r#"{"verdict":"approve","reasons":[]}"#.into()),
        );
        let backend = FakeReview {
            script: Mutex::new(m),
        };
        let reviewers = vec![impl_cand(), rev_cand("RA"), rev_cand("RB")];
        let v = verify_with(&reviewers, &backend, &FakeGates(BTreeMap::new()), &packet());
        assert!(v.verified, "{:?}", v.reasons);
        assert_eq!(
            v.reviewer.as_deref(),
            Some(rev_cand("RB").opaque_id().as_str())
        );
        assert_ne!(v.reviewer.as_deref(), Some(packet().implementer.as_str()));
    }

    #[test]
    fn cloud_rsi_verification_reviewer_rejection_blocks() {
        let reviewers = vec![rev_cand("R1")];
        let v = verify_with(
            &reviewers,
            &FakeReview::with(
                "mR1",
                r#"{"verdict":"reject","reasons":["removes safety check"]}"#,
            ),
            &FakeGates(BTreeMap::new()),
            &packet(),
        );
        assert!(!v.verified);
        assert!(v.reasons.iter().any(|r| r.contains("reviewer rejected")));
    }
}
