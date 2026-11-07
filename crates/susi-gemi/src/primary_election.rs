//! Primary election (VC-202-022 / T-DEEPSEEK-121).
//!
//! The cascade picks per request; the *primary* is the recorded answer to
//! "which model holds this task class right now" — elected only from the
//! fully-working candidates: available (`!unfit`, health) **and** capable
//! (`meets_floor`) for the class, chosen by the ranking's own order —
//! expected cost per verified outcome. Every election journals why: the
//! winner's measured evidence and the reason each other candidate lost —
//! unfit, below floor, or simply pricier per verified outcome.
//!
//! Re-election is not a special path: the scheduled scout re-runs
//! [`elect_all`] over fresh evidence every cycle, so a provider that dies
//! (`FailureKind` recorded by probe outcomes), a price that moves
//! (catalogue diff), or a capability that changes elects a new primary on
//! the next run — and a single fully-working candidate is primary by
//! default, not by exception.

use crate::engines::brain::{self, Ranked, TaskClass};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Why a candidate was not elected — the counter-evidence an election owes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum Rejection {
    /// Failing right now — health is checked before capability or price.
    Unfit {
        provider: String,
        /// `Kind x streak` of the most recent failure inside the window.
        last_failure: Option<String>,
    },
    /// Declared capability below the class floor — ineligible however cheap.
    BelowFloor {
        provider: String,
        capability: String,
    },
    /// Fully working but beaten by the winner's cost per verified outcome.
    Pricier {
        provider: String,
        cost_per_outcome_usd: Option<f64>,
    },
}

/// One election: the primary for one task class and the evidence for the
/// choice — what the winner measured, how large the fully-working field
/// was, and why every other candidate lost.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Election {
    pub class: String,
    pub unix: u64,
    pub primary: String,
    /// The evidence the choice stands on — the winner's ranking inputs.
    pub cost_per_outcome_usd: Option<f64>,
    pub success_rate: Option<f32>,
    pub samples: u32,
    /// Fully-working candidates the winner was elected over — `1` means a
    /// sole working candidate, primary by default rather than exception.
    pub field: usize,
    pub rejected: Vec<Rejection>,
}

/// Elect the primary for `class` from one ranking. `ranked` must be in the
/// order [`brain::rank`] produces — the same order the cascade consults,
/// cheapest cost per verified outcome first — so the election can never
/// disagree with what actually routes.
#[must_use]
pub fn elect(ranked: &[Ranked], class: TaskClass, now: u64) -> Option<Election> {
    let mut working = Vec::new();
    let mut rejected = Vec::new();
    for r in ranked {
        if r.unfit {
            rejected.push(Rejection::Unfit {
                provider: r.provider.clone(),
                last_failure: r
                    .last_failure
                    .map(|(kind, streak)| format!("{kind:?} x{streak}")),
            });
        } else if !r.meets_floor {
            rejected.push(Rejection::BelowFloor {
                provider: r.provider.clone(),
                capability: r.capability.to_string(),
            });
        } else {
            working.push(r);
        }
    }
    let winner = working.first()?;
    for r in &working[1..] {
        rejected.push(Rejection::Pricier {
            provider: r.provider.clone(),
            cost_per_outcome_usd: r.cost_per_outcome_usd,
        });
    }
    Some(Election {
        class: class.label().to_string(),
        unix: now,
        primary: winner.provider.clone(),
        cost_per_outcome_usd: winner.cost_per_outcome_usd,
        success_rate: winner.success_rate,
        samples: winner.samples,
        field: working.len(),
        rejected,
    })
}

/// Elect the primary for every task class over the live brain evidence —
/// the candidate set is the caller's (registered providers ∪ evidence).
#[must_use]
pub fn elect_all(names: &[String], now: u64) -> Vec<Election> {
    TaskClass::ALL
        .iter()
        .filter_map(|class| elect(&brain::rank(names, *class), *class, now))
        .collect()
}

/// One ballot: the elections a single scout run produced, appended to the
/// elections journal — one JSON line per run, so a re-election is reviewable
/// against the evidence that caused it, not just logged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ballot {
    pub unix: u64,
    pub elections: Vec<Election>,
}

/// Append one ballot to the elections journal — one JSON line per run.
pub fn append(journal: &Path, ballot: &Ballot) -> std::io::Result<()> {
    use std::io::Write as _;
    if let Some(parent) = journal.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let line = serde_json::to_string(ballot)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(journal)?;
    writeln!(f, "{line}")
}

/// Read the last recorded ballot — who holds each class and on what
/// evidence. A missing or corrupt journal is just "no election yet".
#[must_use]
pub fn load_last(journal: &Path) -> Option<Ballot> {
    let text = std::fs::read_to_string(journal).ok()?;
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<Ballot>(l).ok())
        .next_back()
}

#[cfg(test)]
mod inline_tests {
    use super::*;

    fn candidate(provider: &str, meets_floor: bool, unfit: bool, cost: Option<f64>) -> Ranked {
        Ranked {
            provider: provider.to_string(),
            meets_floor,
            capability: if meets_floor { "coding" } else { "basic" },
            unfit,
            last_failure: None,
            score: 0.8,
            cost_tier: "low",
            expected_cost_usd: cost,
            cost_per_outcome_usd: cost,
            billing: "metered",
            quota_remaining: Some(1.0),
            effective_cost_usd: cost,
            samples: 5,
            success_rate: Some(0.9),
            avg_latency_ms: Some(100),
        }
    }

    #[test]
    fn elect_returns_none_when_no_candidate_is_working_and_meets_floor() {
        let ranked = vec![
            candidate("unfit-one", true, true, Some(0.01)),
            candidate("weak-one", false, false, Some(0.001)),
        ];
        assert!(elect(&ranked, TaskClass::Code, 100).is_none());
    }

    #[test]
    fn elect_picks_first_working_candidate_and_records_all_rejection_reasons() {
        let ranked = vec![
            candidate("unfit-first", true, true, Some(0.001)),
            candidate("below-floor", false, false, Some(0.002)),
            candidate("winner", true, false, Some(0.01)),
            candidate("runner-up", true, false, Some(0.02)),
        ];
        let election = elect(&ranked, TaskClass::Code, 500).unwrap();
        assert_eq!(election.primary, "winner");
        assert_eq!(election.field, 2);
        assert_eq!(election.rejected.len(), 3);
        assert!(
            matches!(&election.rejected[0], Rejection::Unfit { provider, .. } if provider == "unfit-first")
        );
        assert!(
            matches!(&election.rejected[1], Rejection::BelowFloor { provider, .. } if provider == "below-floor")
        );
        assert!(
            matches!(&election.rejected[2], Rejection::Pricier { provider, .. } if provider == "runner-up")
        );
    }

    #[test]
    fn single_working_candidate_is_elected_with_field_one() {
        let ranked = vec![candidate("solo", true, false, Some(0.05))];
        let election = elect(&ranked, TaskClass::Reasoning, 777).unwrap();
        assert_eq!(election.primary, "solo");
        assert_eq!(election.field, 1);
        assert!(election.rejected.is_empty());
    }

    #[test]
    fn append_and_load_last_roundtrip_latest_ballot() {
        let path = std::env::temp_dir().join(format!(
            "susi-election-inline-{}-{}.jsonl",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_file(&path);
        assert!(load_last(&path).is_none());

        let b1 = Ballot {
            unix: 100,
            elections: Vec::new(),
        };
        let b2 = Ballot {
            unix: 200,
            elections: Vec::new(),
        };
        append(&path, &b1).unwrap();
        append(&path, &b2).unwrap();
        assert_eq!(load_last(&path).unwrap().unix, 200);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn elect_all_with_empty_candidates_returns_empty_vec() {
        assert!(elect_all(&[], 1_000).is_empty());
        assert!(elect(&[], TaskClass::Chat, 1_000).is_none());
    }
}
