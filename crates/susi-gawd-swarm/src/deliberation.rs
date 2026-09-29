//! Plan search — deliberate before dispatch.
//!
//! `solve_autonomous` used to commit to the first decomposition the model
//! returned. Now a mission generates several candidate plans at different
//! step budgets and phrasings, scores each against goal coverage, risk
//! vocabulary, and verifiability, executes the best, and — for read-scope
//! goals only — falls through to the next candidate on failure (a mutating
//! plan that failed mid-way reports what it cannot undo rather than
//! re-running mutations under a second guess). Mutate/SelfExtend intents
//! additionally require consensus: two independent candidates must agree on
//! the approach or the mission declines multi-step autonomy in favor of the
//! single-step path.

use crate::susi_core::manifold::{IntentManifold, RiskProfile, ScopeOfImpact};

/// One candidate plan plus why it scored as it did — the rejected
/// candidates' scores and rationales are part of the mission record, not
/// discarded.
#[derive(Debug, Clone)]
pub struct CandidatePlan {
    pub steps: Vec<String>,
    pub score: f32,
    pub rationale: String,
}

/// The outcome of plan search, ready for execution order.
#[derive(Debug, Clone)]
pub struct Deliberation {
    /// Candidates sorted best-first. Always non-empty on return.
    pub candidates: Vec<CandidatePlan>,
    /// Whether this intent's scope/risk demands inter-candidate agreement.
    pub consensus_required: bool,
    /// `Some(similarity)` when the two best candidates agree; `None` when no
    /// pair reached the threshold.
    pub consensus: Option<f32>,
}

/// Two candidates agree when their step-token Jaccard clears this bar.
const CONSENSUS_THRESHOLD: f32 = 0.35;

/// Tokens that mark a step as carrying real-world risk.
const RISK_TOKENS: &[&str] = &[
    "rm",
    "delete",
    "drop",
    "force",
    "overwrite",
    "kill",
    "format",
    "truncate",
    "chmod",
    "chown",
    "push",
    "deploy",
    "remove",
];

/// Tokens marking a step whose output can be checked by a contract.
const VERIFIABLE_TOKENS: &[&str] = &[
    "test", "verify", "check", "assert", "diff", "cargo", "probe", "validate",
];

/// Jaccard similarity over whitespace-token sets — a cheap, explainable
/// measure of whether two plans describe the same approach.
/// How much two plans agree — on the work *and* the order. The set
/// component asks whether the plans contain the same tokens at all; the
/// positional component compares step-vs-step at each index. Consensus
/// takes the *minimum*: "read config, write result" and "write result,
/// read config" share every token but are different plans, and a
/// mutating plan executed backwards must never count as agreement.
pub fn plan_similarity(a: &[String], b: &[String]) -> f32 {
    let bag = |steps: &[String]| {
        steps
            .iter()
            .flat_map(|s| s.split_whitespace().map(|t| t.to_lowercase()))
            .collect::<std::collections::BTreeSet<_>>()
    };
    let step_tokens = |s: &str| {
        s.split_whitespace()
            .map(|t| t.to_lowercase())
            .collect::<std::collections::BTreeSet<_>>()
    };
    let jaccard = |x: &std::collections::BTreeSet<String>,
                   y: &std::collections::BTreeSet<String>| {
        let u = x.union(y).count();
        if u == 0 {
            1.0
        } else {
            x.intersection(y).count() as f32 / u as f32
        }
    };
    let (ta, tb) = (bag(a), bag(b));
    let set_sim = if ta.is_empty() && tb.is_empty() {
        1.0
    } else {
        jaccard(&ta, &tb)
    };
    let positional = a
        .iter()
        .zip(b.iter())
        .map(|(sa, sb)| jaccard(&step_tokens(sa), &step_tokens(sb)))
        .sum::<f32>()
        / a.len().max(b.len()).max(1) as f32;
    set_sim.min(positional)
}

/// Score a candidate plan: goal-token coverage earns, risk vocabulary and
/// historically-failed tool mentions cost, verifiable steps earn a bonus,
/// and overshooting the step budget costs. Pure — no model calls — so
/// scores are reproducible and testable.
pub fn score_plan(goal: &str, steps: &[String], max_steps: u32) -> (f32, String) {
    score_plan_weighted(goal, steps, max_steps, &std::collections::BTreeSet::new())
}

/// `score_plan` plus history weighting: every step-token naming a tool that
/// only ever appeared on failed similar missions costs, and every token
/// naming a tool that only ever appeared on *successful* ones earns.
pub fn score_plan_weighted(
    goal: &str,
    steps: &[String],
    max_steps: u32,
    failed_tools: &std::collections::BTreeSet<String>,
) -> (f32, String) {
    let counts: std::collections::BTreeMap<String, u32> =
        failed_tools.iter().map(|t| (t.clone(), 1)).collect();
    score_plan_with_history(
        goal,
        steps,
        max_steps,
        &counts,
        &std::collections::BTreeSet::new(),
    )
}

/// Full history-weighted scoring: each failed-history mention costs
/// 0.10 per past failed mission it appeared on (capped at 3), each proven
/// mention earns 0.05 (capped at 4) — repetition matters, stuffing does
/// not.
pub fn score_plan_with_history(
    goal: &str,
    steps: &[String],
    max_steps: u32,
    failed_tools: &std::collections::BTreeMap<String, u32>,
    proven_tools: &std::collections::BTreeSet<String>,
) -> (f32, String) {
    if steps.is_empty() {
        return (0.0, "empty plan".into());
    }
    let goal_tokens: std::collections::BTreeSet<String> =
        crate::amas::tokenize_goal(goal).into_iter().collect();
    let step_tokens: std::collections::BTreeSet<String> = steps
        .iter()
        .flat_map(|s| crate::amas::tokenize_goal(s))
        .collect();
    let coverage = if goal_tokens.is_empty() {
        0.5
    } else {
        goal_tokens.intersection(&step_tokens).count() as f32 / goal_tokens.len() as f32
    };

    let risk_hits = steps
        .iter()
        .flat_map(|s| s.split_whitespace().map(|t| t.to_lowercase()))
        .filter(|t| RISK_TOKENS.contains(&t.trim_matches(|c: char| !c.is_alphanumeric())))
        .count();
    let verifiable = steps
        .iter()
        .flat_map(|s| s.split_whitespace().map(|t| t.to_lowercase()))
        .filter(|t| VERIFIABLE_TOKENS.contains(&t.trim_matches(|c: char| !c.is_alphanumeric())))
        .count();
    let over_budget = steps.len().saturating_sub(max_steps as usize);
    let step_words = |t: &String| {
        t.split_whitespace()
            .map(|w| w.to_lowercase())
            .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_string())
            .collect::<Vec<_>>()
    };
    // Penalty scales with how often the tool failed on similar missions —
    // three past failures dock 0.30 per mention, not 0.10.
    let history_penalty: u32 = steps
        .iter()
        .flat_map(step_words)
        .filter_map(|t| failed_tools.get(t.as_str()))
        .map(|count| (*count).min(3))
        .sum();
    let history_hits = steps
        .iter()
        .flat_map(step_words)
        .filter(|t| failed_tools.contains_key(t.as_str()))
        .count();
    let proven_hits = steps
        .iter()
        .flat_map(step_words)
        .filter(|t| proven_tools.contains(t))
        .count();

    let score =
        (0.4 + 0.5 * coverage + 0.05 * verifiable.min(4) as f32 + 0.05 * proven_hits.min(4) as f32
            - 0.15 * risk_hits as f32
            - 0.1 * history_penalty as f32
            - 0.1 * over_budget as f32)
            .clamp(0.0, 1.0);
    let rationale = format!(
        "coverage={coverage:.2} risk_tokens={risk_hits} failed_history_tools={history_hits} \
         failure_penalty={history_penalty} proven_tools={proven_hits} \
         verifiable_steps={verifiable} steps={} over_budget={over_budget}",
        steps.len()
    );
    (score, rationale)
}

/// Does this intent's manifold profile demand that candidates agree before
/// a multi-step plan may execute? Mutation and self-extension do.
pub fn consensus_required(manifold: &IntentManifold) -> bool {
    matches!(
        manifold.scope_of_impact,
        ScopeOfImpact::Mutate | ScopeOfImpact::SelfExtend
    ) || manifold.risk_profile >= RiskProfile::High
}

/// Generate up to `budgets.len()` candidates via `generate(budget)`, score
/// each, sort best-first, and measure consensus between the top two.
/// `generate` is injected so the search is testable without inference —
/// production passes `SusiMasterAgent::plan_steps` with a budget.
/// Retrieved history the scorer weighs: tools only ever seen on failed
/// similar missions penalize a candidate; tools only ever seen on
/// successful ones reward it.
#[derive(Debug, Clone, Default)]
pub struct HistorySignals {
    /// Tool → count of failed similar missions it appeared on; repeated
    /// failures weigh more in scoring.
    pub failed: std::collections::BTreeMap<String, u32>,
    pub proven: std::collections::BTreeSet<String>,
    /// Texts of the steps that actually aborted similar failed missions —
    /// a candidate echoing one gets docked.
    pub failed_steps: Vec<String>,
    /// Texts of steps that ran on similar *verified-success* missions —
    /// a candidate echoing one earns a bonus.
    pub proven_steps: Vec<String>,
}

pub fn deliberate(
    goal: &str,
    manifold: &IntentManifold,
    budgets: &[u32],
    history: &HistorySignals,
    mut generate: impl FnMut(u32) -> Vec<String>,
) -> Deliberation {
    let gated = consensus_required(manifold);
    let doomed: Vec<std::collections::BTreeSet<String>> = history
        .failed_steps
        .iter()
        .map(|s| crate::amas::tokenize_goal(s).into_iter().collect())
        .collect();
    let winning: Vec<std::collections::BTreeSet<String>> = history
        .proven_steps
        .iter()
        .map(|s| crate::amas::tokenize_goal(s).into_iter().collect())
        .collect();
    let mut candidates: Vec<CandidatePlan> = budgets
        .iter()
        .map(|&budget| {
            let steps = generate(budget);
            let (mut score, mut rationale) =
                score_plan_with_history(goal, &steps, budget, &history.failed, &history.proven);
            // A mutating/high-risk plan with zero verifiable steps asks to
            // be trusted blind — real cost on the score, not just a flag.
            if gated
                && !steps.is_empty()
                && !steps.iter().any(|s| {
                    s.split_whitespace().map(|t| t.to_lowercase()).any(|t| {
                        VERIFIABLE_TOKENS.contains(&t.trim_matches(|c: char| !c.is_alphanumeric()))
                    })
                })
            {
                score = (score - 0.2).max(0.0);
                rationale.push_str(" unverified_mutation_penalty=-0.20");
            }
            // A step that echoes the step that aborted a similar mission
            // before is a repeating failure pattern, not a coincidence.
            if !doomed.is_empty() {
                let echoes = steps
                    .iter()
                    .map(|s| {
                        crate::amas::tokenize_goal(s)
                            .into_iter()
                            .collect::<std::collections::BTreeSet<_>>()
                    })
                    .filter(|toks| {
                        !toks.is_empty()
                            && doomed.iter().any(|d| {
                                let inter = toks.intersection(d).count() as f32;
                                inter / toks.union(d).count() as f32 >= 0.6
                            })
                    })
                    .count();
                if echoes > 0 {
                    score = (score - 0.15 * echoes.min(2) as f32).max(0.0);
                    rationale.push_str(&format!(" doomed_step_echoes={echoes}"));
                }
            }
            // Mirror of the doomed-step penalty: a step matching one that
            // ran on a verified success repeats a move that worked before.
            if !winning.is_empty() {
                let proven = steps
                    .iter()
                    .map(|s| {
                        crate::amas::tokenize_goal(s)
                            .into_iter()
                            .collect::<std::collections::BTreeSet<_>>()
                    })
                    .filter(|toks| {
                        !toks.is_empty()
                            && winning.iter().any(|w| {
                                let inter = toks.intersection(w).count() as f32;
                                inter / toks.union(w).count() as f32 >= 0.6
                            })
                    })
                    .count();
                if proven > 0 {
                    score = (score + 0.05 * proven.min(3) as f32).min(1.0);
                    rationale.push_str(&format!(" proven_step_matches={proven}"));
                }
            }
            CandidatePlan {
                steps,
                score,
                rationale,
            }
        })
        .filter(|c| !c.steps.is_empty())
        .collect();
    candidates.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            // Equal scores: the cheaper plan (fewer steps) wins — same
            // expected outcome, less to go wrong.
            .then(a.steps.len().cmp(&b.steps.len()))
    });

    // Consensus is measured before dedup: two budgets producing the
    // identical decomposition is the *strongest* agreement signal, and
    // collapsing them first would hide it.
    let consensus = if candidates.len() >= 2 {
        let sim = plan_similarity(&candidates[0].steps, &candidates[1].steps);
        (sim >= CONSENSUS_THRESHOLD).then_some(sim)
    } else {
        None
    };

    // Two budgets can produce the identical decomposition — execution
    // keeps only the best-scored copy (post-sort first occurrence).
    let mut seen = std::collections::BTreeSet::new();
    candidates.retain(|c| seen.insert(c.steps.join("\u{1f}")));
    if candidates.is_empty() {
        // Decomposition produced nothing usable anywhere — the goal itself
        // is the conservative plan, identical to the old fallback.
        candidates.push(CandidatePlan {
            steps: vec![goal.to_string()],
            score: 0.4,
            rationale: "fallback: goal as single step".into(),
        });
    }

    Deliberation {
        candidates,
        consensus_required: consensus_required(manifold),
        consensus,
    }
}

/// True when this deliberation may proceed as a multi-step plan: either no
/// consensus demand, or the top candidates actually agree.
pub fn approved(d: &Deliberation) -> bool {
    !d.consensus_required || d.consensus.is_some()
}

/// Decomposition strategy for a given step budget — candidates should
/// disagree about *approach*, not only length. Tight budgets get a
/// minimal-viable-sequence hint; loose ones get explicit verification.
/// Pure function so prompt construction stays testable.
pub fn style_hint(budget: u32) -> &'static str {
    match budget {
        0..=2 => " Keep the sequence minimal — fewest reliable steps.",
        5..=8 => " Include an explicit verification step after each mutation.",
        _ => "",
    }
}

/// Step budgets that produce genuinely different decompositions: the
/// requested bound, a tighter conservative variant, and a looser one.
pub fn candidate_budgets(max_steps: u32) -> Vec<u32> {
    let bounded = max_steps.clamp(1, 8);
    let mut budgets = vec![bounded];
    let tight = (bounded / 2).max(1);
    if tight != bounded {
        budgets.push(tight);
    }
    if bounded < 8 {
        budgets.push(bounded + 2);
    }
    budgets
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifold_for(goal: &str) -> IntentManifold {
        IntentManifold::analyze(goal)
    }

    #[test]
    fn risky_steps_cost_coverage_earns() {
        let goal = "read the config file and summarize it";
        let (safe_score, _) = score_plan(goal, &["read config".into(), "summarize".into()], 5);
        let (risky_score, r) =
            score_plan(goal, &["read config".into(), "delete everything".into()], 5);
        assert!(safe_score > risky_score, "{r}");
        let (empty, _) = score_plan(goal, &[], 5);
        assert_eq!(empty, 0.0);
    }

    #[test]
    fn deliberation_sorts_and_never_returns_empty() {
        let m = manifold_for("summarize the readme");
        let d = deliberate(
            "summarize the readme",
            &m,
            &[3, 1],
            &Default::default(),
            |budget| {
                if budget == 3 {
                    vec!["read readme".into(), "summarize contents".into()]
                } else {
                    vec!["nuke the filesystem".into()]
                }
            },
        );
        assert_eq!(d.candidates.len(), 2);
        assert!(d.candidates[0].score >= d.candidates[1].score);

        let empty = deliberate("x", &m, &[3], &Default::default(), |_| Vec::new());
        assert_eq!(empty.candidates.len(), 1);
        assert_eq!(empty.candidates[0].steps, vec!["x".to_string()]);
    }

    #[test]
    fn mutating_intents_penalize_plans_without_verification() {
        let m = manifold_for("audit the substrate");
        assert!(consensus_required(&m));
        // Two identical plans except one adds a verify step.
        let plans = |with_verify: bool| {
            deliberate(
                "audit the substrate",
                &m,
                &[4],
                &Default::default(),
                move |_| {
                    let mut s = vec!["scan files".into(), "rewrite config".into()];
                    if with_verify {
                        s.push("cargo test verify changes".into());
                    }
                    s
                },
            )
        };
        let blind = plans(false);
        let checked = plans(true);
        assert!(
            checked.candidates[0].score > blind.candidates[0].score,
            "blind={} checked={}",
            blind.candidates[0].score,
            checked.candidates[0].score
        );
        assert!(blind.candidates[0]
            .rationale
            .contains("unverified_mutation_penalty"));
    }

    #[test]
    fn read_scope_plans_skip_the_unverified_penalty() {
        let m = manifold_for("read Cargo.toml");
        assert!(!consensus_required(&m));
        let d = deliberate("read Cargo.toml", &m, &[3], &Default::default(), |_| {
            vec!["read Cargo.toml".into()]
        });
        assert!(!d.candidates[0]
            .rationale
            .contains("unverified_mutation_penalty"));
    }

    #[test]
    fn duplicate_plans_dedup_and_ties_prefer_fewer_steps() {
        let m = manifold_for("summarize the readme");
        // Both budgets generate the same plan — only one candidate remains.
        let dup = deliberate(
            "summarize the readme",
            &m,
            &[4, 2],
            &Default::default(),
            |_| vec!["read readme".into(), "summarize".into()],
        );
        assert_eq!(dup.candidates.len(), 1);

        // Equal coverage/score: the shorter plan sorts first.
        let tied = deliberate("x y", &m, &[4, 3], &Default::default(), |budget| {
            if budget == 4 {
                vec!["x y".into(), "x y".into(), "x y".into()]
            } else {
                vec!["x y".into()]
            }
        });
        assert_eq!(tied.candidates.len(), 2);
        assert_eq!(tied.candidates[0].steps.len(), 1, "{:?}", tied.candidates);
    }

    #[test]
    fn budgets_span_distinct_strategies() {
        assert!(style_hint(1).contains("minimal"));
        assert!(style_hint(2).contains("minimal"));
        assert!(style_hint(4).is_empty());
        assert!(style_hint(6).contains("verification"));
        assert!(style_hint(8).contains("verification"));
        // The common max_steps=4 case yields three budgets spanning
        // concise, neutral, and verify — real approach diversity.
        let styles: std::collections::BTreeSet<&str> = candidate_budgets(4)
            .iter()
            .map(|&b| style_hint(b))
            .collect();
        assert_eq!(styles.len(), 3, "{styles:?}");
    }

    #[test]
    fn plan_similarity_is_order_aware() {
        let forward = vec![
            "read the config".to_string(),
            "write the result".to_string(),
        ];
        let backward: Vec<String> = forward.iter().rev().cloned().collect();
        // Same vocabulary, opposite order — not agreement.
        let sim = plan_similarity(&forward, &backward);
        assert!(sim < CONSENSUS_THRESHOLD, "reversed plan scored {sim}");
        // Identical plans still agree fully.
        assert_eq!(plan_similarity(&forward, &forward), 1.0);
        // Same-order near-duplicate keeps high agreement.
        let near = vec![
            "read the config".to_string(),
            "write the output".to_string(),
        ];
        assert!(plan_similarity(&forward, &near) >= CONSENSUS_THRESHOLD);
    }

    #[test]
    fn steps_echoing_a_doomed_step_are_docked() {
        let m = manifold_for("read the workspace files");
        let history = HistorySignals {
            failed_steps: vec!["blindly rewrite the config".into()],
            ..Default::default()
        };
        let echoing = deliberate("read the workspace files", &m, &[4], &history, |_| {
            vec!["list files".into(), "blindly rewrite the config".into()]
        });
        let clean = deliberate("read the workspace files", &m, &[4], &history, |_| {
            vec!["list files".into(), "inspect each file".into()]
        });
        assert!(
            echoing.candidates[0].score < clean.candidates[0].score,
            "echo={} clean={}",
            echoing.candidates[0].score,
            clean.candidates[0].score
        );
        assert!(echoing.candidates[0]
            .rationale
            .contains("doomed_step_echoes=1"));
    }

    #[test]
    fn steps_echoing_a_proven_step_earn_a_bonus() {
        let m = manifold_for("read the workspace files");
        let history = HistorySignals {
            proven_steps: vec!["verify the config parses".into()],
            ..Default::default()
        };
        let echoing = deliberate("read the workspace files", &m, &[4], &history, |_| {
            vec!["list files".into(), "verify the config parses".into()]
        });
        let plain = deliberate("read the workspace files", &m, &[4], &history, |_| {
            vec!["list files".into(), "inspect each file".into()]
        });
        assert!(
            echoing.candidates[0].score > plain.candidates[0].score,
            "echo={} plain={}",
            echoing.candidates[0].score,
            plain.candidates[0].score
        );
        assert!(echoing.candidates[0]
            .rationale
            .contains("proven_step_matches=1"));
    }

    #[test]
    fn mutating_goals_require_consensus_reads_do_not() {
        assert!(consensus_required(&manifold_for(
            "audit the substrate state"
        )));
        assert!(!consensus_required(&manifold_for("read Cargo.toml")));
        assert!(consensus_required(&manifold_for(
            "exec command to install pkg"
        )));
    }

    #[test]
    fn consensus_blocks_divergent_plans_on_mutations() {
        let m = manifold_for("audit the substrate");
        let divergent = deliberate(
            "audit the substrate",
            &m,
            &[4, 2],
            &Default::default(),
            |budget| {
                if budget == 4 {
                    vec!["scan all files".into(), "audit findings".into()]
                } else {
                    vec!["wipe directory".into()]
                }
            },
        );
        assert!(divergent.consensus_required);
        assert!(divergent.consensus.is_none());
        assert!(!approved(&divergent));

        let agreeing = deliberate(
            "audit the substrate",
            &m,
            &[4, 2],
            &Default::default(),
            |_| vec!["scan files".into(), "audit scan results".into()],
        );
        // Identical plans from two budgets is maximal agreement.
        assert_eq!(agreeing.consensus, Some(1.0));
        assert!(approved(&agreeing));
    }

    #[test]
    fn failed_history_tools_penalize_but_do_not_veto() {
        let failed: std::collections::BTreeSet<String> =
            ["broken_tool".to_string()].into_iter().collect();
        let (clean, _) = score_plan_weighted("deploy api", &["deploy api".into()], 5, &failed);
        let (tainted, r) = score_plan_weighted(
            "deploy api",
            &["run broken_tool deploy api".into()],
            5,
            &failed,
        );
        assert!(tainted < clean, "{r}");
        assert!(tainted > 0.0);
        assert!(r.contains("failed_history_tools=1"), "{r}");
    }

    #[test]
    fn repeated_failures_dock_harder_than_one_offs() {
        let once: std::collections::BTreeMap<String, u32> =
            [("flaky".to_string(), 1)].into_iter().collect();
        let thrice: std::collections::BTreeMap<String, u32> =
            [("flaky".to_string(), 3)].into_iter().collect();
        let ten: std::collections::BTreeMap<String, u32> =
            [("flaky".to_string(), 10)].into_iter().collect();
        let none = std::collections::BTreeSet::new();
        let steps = vec!["run flaky deploy".to_string()];
        let (s1, r1) = score_plan_with_history("deploy", &steps, 5, &once, &none);
        let (s3, r3) = score_plan_with_history("deploy", &steps, 5, &thrice, &none);
        let (s10, _) = score_plan_with_history("deploy", &steps, 5, &ten, &none);
        assert!(s1 > s3, "{r1} {r3}");
        // Beyond three failures the cap flattens the penalty.
        assert!((s3 - s10).abs() < 0.001, "{s3} {s10}");
        assert!(r3.contains("failure_penalty=3"), "{r3}");
    }

    #[test]
    fn proven_history_tools_reward_but_stay_capped() {
        let proven: std::collections::BTreeSet<String> = ["cargo".to_string(), "git".to_string()]
            .into_iter()
            .collect();
        let none: std::collections::BTreeMap<String, u32> = Default::default();
        let empty_set = std::collections::BTreeSet::new();
        let (plain, _) = score_plan_with_history(
            "build the crate",
            &["build crate".into()],
            5,
            &none,
            &empty_set,
        );
        let (boosted, r) = score_plan_with_history(
            "build the crate",
            &["cargo build crate via git".into()],
            5,
            &none,
            &proven,
        );
        assert!(boosted > plain, "{r}");
        assert!(r.contains("proven_tools=2"), "{r}");
        // Four-plus mentions cap at 0.2 total — keyword stuffing can't win.
        let stuffed = score_plan_with_history(
            "x",
            &["cargo cargo cargo cargo cargo".into()],
            5,
            &none,
            &proven,
        );
        assert!(stuffed.1.contains("proven_tools=5"));
        let (four, _) =
            score_plan_with_history("x", &["cargo cargo cargo cargo".into()], 5, &none, &proven);
        assert!((stuffed.0 - four).abs() < 0.001, "{:?}", stuffed);
    }

    #[test]
    fn similarity_is_token_jaccard() {
        let a = vec!["read the config".to_string()];
        assert_eq!(plan_similarity(&a, &a), 1.0);
        let b = vec!["deploy to production".to_string()];
        assert_eq!(plan_similarity(&a, &b), 0.0);
        let c = vec!["read the config twice".to_string()];
        let sim = plan_similarity(&a, &c);
        assert!(sim > 0.5 && sim < 1.0, "{sim}");
    }

    #[test]
    fn budgets_offer_distinct_granularities() {
        let b = candidate_budgets(5);
        assert!(b.len() >= 2 && b.contains(&5));
        assert_eq!(candidate_budgets(1), vec![1, 3]);
    }
}
