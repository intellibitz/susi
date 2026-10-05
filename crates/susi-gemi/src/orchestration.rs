//! Primary orchestrates secondaries (VC-202-022 / T-DEEPSEEK-122).
//!
//! A mission decomposes into goals ([`MissionPlanner`]); the elected
//! primary serves them — but when another fully-working worker can serve a
//! goal cheaper, the primary delegates it. The decision is the ranking's
//! own: each goal is classified, candidates are ranked for that class, and
//! the first working, floor-meeting, cap-holding secondary whose marginal
//! cost does not exceed the primary's gets the call. Fan-out is bounded
//! twice: a secondary takes at most its remaining rate allowance of
//! delegated calls, and a delegation is never priced above doing the work
//! on the primary.
//!
//! Every assignment keeps provenance — goal, class, serving worker, price,
//! and why a goal was not delegated — and the finished topology is appended
//! to the orchestrations journal, one JSON line per mission, so *who did
//! what, on which model, at what cost* is reconstructible from the trace.
//! The synthesis call runs on the primary: the final answer is the
//! primary's own, built on the delegated results.

use crate::engines::brain::{self, Ranked, TaskClass};
use crate::engines::cost::Budget;
use crate::primary_election;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// The ranking view the assignment consults per class — production feeds
/// `brain::rank_with_quota` over the unified headroom; tests inject a
/// deterministic rank.
pub type RankedFor<'a> = dyn Fn(TaskClass) -> Vec<Ranked> + 'a;

/// The rate-headroom view: provider → `(remaining, allowance)`, `None`
/// for an unbounded worker. Shared shape with
/// [`crate::key_arbitration::HeadroomFn`].
pub type HeadroomView<'a> = dyn Fn(&str) -> Option<(u64, u64)> + 'a;

/// The pinned-generation primitive: `(provider, prompt)` → the worker's
/// answer. Production supplies
/// [`crate::engines::runtime::GemiEngine::generate_reasoning_deep_with_model`];
/// tests inject fakes.
pub type PinnedDispatch<'a> = dyn Fn(&str, &str) -> Result<String, String> + 'a;

/// Why a goal stayed with the primary instead of going to a secondary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum HeldBack {
    /// No other fully-working candidate exists for this class.
    NoSecondary,
    /// Working secondaries exist but all are capped out right now.
    SecondaryCapped { candidate: String },
    /// A secondary could serve but its remaining rate allowance is spent
    /// on already-assigned delegations — the fan-out bound.
    SecondaryRateBound { candidate: String },
    /// Delegating would cost more than doing the work on the primary —
    /// the cost bound on fan-out.
    SecondaryPricier { candidate: String },
}

/// One goal's routing record — the provenance each delegated result keeps.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Delegation {
    pub goal: String,
    /// The task class the goal was classified into — the class the
    /// secondary was chosen for.
    pub class: String,
    /// The worker that served the goal: a secondary when `delegated`,
    /// the primary itself otherwise.
    pub provider: String,
    pub delegated: bool,
    /// Scarcity-adjusted marginal cost the rank priced this call at;
    /// `None` when the worker is unpriced.
    pub effective_cost_usd: Option<f64>,
    /// Why the goal stayed with the primary when it did.
    pub why_not_delegated: Option<HeldBack>,
    /// The call's outcome as the caller saw it.
    pub ok: bool,
    /// The answer the worker returned. Recorded in memory for synthesis;
    /// the journal keeps the topology, not payloads.
    #[serde(skip)]
    pub answer: Option<String>,
}

/// The recorded shape of one orchestrated mission — who did what, on
/// which model, at what cost. Appended to the journal, one line per run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Topology {
    pub unix: u64,
    /// Task class the mission itself classified into — the class the
    /// primary was elected for.
    pub mission_class: String,
    /// The elected primary — winner of [`primary_election::elect`] for the
    /// mission class, so the orchestrator can never disagree with the
    /// election the scout journals.
    pub primary: String,
    pub delegations: Vec<Delegation>,
    /// Number of goals a secondary actually served — the realized fan-out.
    pub fanned_out: usize,
    pub synthesis_ok: bool,
}

/// The marginal price of the next call on a ranked worker, for the
/// delegation cost bound: the ranking's scarcity-adjusted cost when the
/// worker is priced, and the billing-mode marginal when it is not — a
/// subscription or free-tier worker under its cap is free at the margin;
/// an unpriced metered worker cannot prove it is cheaper, so it is not.
fn marginal_cost(r: &Ranked) -> f64 {
    if let Some(c) = r.effective_cost_usd {
        return c;
    }
    match r.billing {
        "subscription" | "free-tier" if r.quota_remaining != Some(0.0) => 0.0,
        _ => f64::MAX,
    }
}

/// Assign each goal a serving worker. `ranked_for` ranks the candidate
/// set per class (production feeds `brain::rank_with_quota` with the
/// unified headroom view; tests inject a deterministic one); `headroom`
/// reports `(remaining, allowance)` rate headroom, `None` for unbounded
/// workers. `None` when no class elects a primary — the caller keeps its
/// existing path rather than orchestrating nothing.
#[must_use]
pub fn assign(
    ranked_for: &RankedFor<'_>,
    headroom: &HeadroomView<'_>,
    mission_class: TaskClass,
    goals: &[String],
    now: u64,
) -> Option<Topology> {
    let election = primary_election::elect(&ranked_for(mission_class), mission_class, now)?;
    let primary = election.primary.clone();
    // Rate bound on fan-out: a secondary accepts at most its remaining
    // allowance of delegated calls; unbounded workers have no count.
    let mut spent: BTreeMap<String, u64> = BTreeMap::new();
    let delegations = goals
        .iter()
        .map(|goal| {
            let class = TaskClass::classify(goal);
            let ranked = ranked_for(class);
            // The cost bound is class-relative: a delegation may not be
            // priced above the primary doing *this* goal's work — an
            // equally-priced secondary delegates (fan-out shares the
            // load); a pricier one stays home.
            let primary_cost = ranked
                .iter()
                .find(|r| r.provider == primary)
                .map(marginal_cost)
                .unwrap_or(f64::MAX);
            let mut held = HeldBack::NoSecondary;
            let mut secondary: Option<&Ranked> = None;
            for r in &ranked {
                if r.provider == primary || r.unfit || !r.meets_floor {
                    continue;
                }
                if brain_quota_exhausted(r) {
                    held = HeldBack::SecondaryCapped {
                        candidate: r.provider.clone(),
                    };
                    continue;
                }
                if let Some((remaining, _)) = headroom(&r.provider) {
                    if *spent.get(&r.provider).unwrap_or(&0) >= remaining {
                        held = HeldBack::SecondaryRateBound {
                            candidate: r.provider.clone(),
                        };
                        continue;
                    }
                }
                if marginal_cost(r) > primary_cost {
                    held = HeldBack::SecondaryPricier {
                        candidate: r.provider.clone(),
                    };
                    continue;
                }
                secondary = Some(r);
                break;
            }
            match secondary {
                Some(r) => {
                    *spent.entry(r.provider.clone()).or_insert(0) += 1;
                    Delegation {
                        goal: goal.clone(),
                        class: class.label().to_string(),
                        provider: r.provider.clone(),
                        delegated: true,
                        effective_cost_usd: r.effective_cost_usd,
                        why_not_delegated: None,
                        ok: false,
                        answer: None,
                    }
                }
                None => Delegation {
                    goal: goal.clone(),
                    class: class.label().to_string(),
                    provider: primary.clone(),
                    delegated: false,
                    effective_cost_usd: ranked
                        .iter()
                        .find(|r| r.provider == primary)
                        .and_then(|r| r.effective_cost_usd),
                    why_not_delegated: Some(held),
                    ok: false,
                    answer: None,
                },
            }
        })
        .collect::<Vec<_>>();
    let fanned_out = delegations.iter().filter(|d| d.delegated).count();
    Some(Topology {
        unix: now,
        mission_class: mission_class.label().to_string(),
        primary,
        delegations,
        fanned_out,
        synthesis_ok: false,
    })
}

fn brain_quota_exhausted(r: &Ranked) -> bool {
    r.quota_remaining == Some(0.0) || r.effective_cost_usd.is_some_and(|c| c >= f64::MAX)
}

/// Run the assigned topology: dispatch each goal to its worker, then ask
/// the primary to synthesize the final answer from the results. `dispatch`
/// is the pinned-generation primitive — production supplies
/// [`GemiEngine::generate_reasoning_deep_with_model`]; tests inject fakes.
/// The returned topology carries the realized outcomes and the synthesis
/// flag; the returned string is the primary's own final answer.
#[allow(clippy::too_many_arguments)] // the views and dispatch are the
                                     // injection surface — bundling them
                                     // would only rename the six slots
pub fn orchestrate(
    mission: &str,
    goals: &[String],
    ranked_for: &RankedFor<'_>,
    headroom: &HeadroomView<'_>,
    dispatch: &PinnedDispatch<'_>,
    now: u64,
) -> Result<(Topology, String), String> {
    let mission_class = TaskClass::classify(mission);
    let mut topology = assign(ranked_for, headroom, mission_class, goals, now)
        .ok_or_else(|| "no fully-working primary for this mission".to_string())?;
    for d in &mut topology.delegations {
        match dispatch(&d.provider, &d.goal) {
            Ok(text) => {
                d.ok = true;
                d.answer = Some(text);
            }
            Err(e) => {
                d.ok = false;
                d.answer = Some(format!("[delegation failed: {e}]"));
            }
        }
    }
    let mut brief = format!("Mission: {mission}\n\nResults to synthesize into one final answer:\n");
    for d in &topology.delegations {
        brief.push_str(&format!(
            "- Goal ({}): {}\n  Answer: {}\n",
            d.class,
            d.goal,
            d.answer.as_deref().unwrap_or("(no answer)")
        ));
    }
    let synthesis = dispatch(&topology.primary.clone(), &brief);
    topology.synthesis_ok = synthesis.is_ok();
    let answer = synthesis.unwrap_or_else(|_| {
        topology
            .delegations
            .iter()
            .filter_map(|d| d.answer.clone())
            .collect::<Vec<_>>()
            .join("\n")
    });
    Ok((topology, answer))
}

/// The production path: plan the mission, rank the registered candidate
/// set with the unified headroom view, bound fan-out by the arbiter's rate
/// headroom, dispatch each goal through the pinned-generation cascade,
/// synthesize on the primary, and journal the topology.
pub fn orchestrate_mission(
    mission: &str,
    workspace: &Path,
    journal: &Path,
) -> Result<(Topology, String), String> {
    let plan = crate::engines::runtime::MissionPlanner::plan_mission(mission, workspace)
        .map_err(|e| e.to_string())?;
    let registry = crate::susi_core::registry::CapabilityRegistry::global();
    let names = crate::engines::routing::InferenceRouter::cloud_failover_order_for(
        registry,
        TaskClass::Chat,
    );
    let ranked_for = |class: TaskClass| {
        brain::rank_with_quota(
            &names,
            class,
            Budget::from_env(),
            &crate::worker::remaining_fraction_unified,
        )
    };
    let ws = workspace.to_path_buf();
    let dispatch = |provider: &str, prompt: &str| -> Result<String, String> {
        let text = crate::engines::runtime::GemiEngine::generate_reasoning_deep_with_model(
            prompt, &ws, provider,
        );
        if text.trim().is_empty()
            || crate::engines::runtime::GemiEngine::looks_like_error_text(&text)
        {
            Err(text)
        } else {
            Ok(text)
        }
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (topology, answer) = orchestrate(
        mission,
        &plan.goals,
        &ranked_for,
        &crate::worker::headroom,
        &dispatch,
        now,
    )?;
    if let Err(e) = append(journal, &topology) {
        tracing::warn!("orchestration journal append failed: {e}");
    }
    Ok((topology, answer))
}

/// Where the journal lives by default — sibling of the brain scout and
/// election journals.
#[must_use]
pub fn default_journal_path() -> std::path::PathBuf {
    susi_paths::SusiDirs::config_dir().join("orchestrations.jsonl")
}

/// Append one topology to the orchestrations journal — one JSON line per
/// mission, so who did what on which model at what cost is reconstructible
/// from the trace.
pub fn append(journal: &Path, topology: &Topology) -> std::io::Result<()> {
    use std::io::Write as _;
    if let Some(parent) = journal.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let line = serde_json::to_string(topology)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(journal)?;
    writeln!(f, "{line}")
}

/// Read the last recorded topology — the most recent mission's who-did-what.
#[must_use]
pub fn load_last(journal: &Path) -> Option<Topology> {
    let text = std::fs::read_to_string(journal).ok()?;
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<Topology>(l).ok())
        .next_back()
}
