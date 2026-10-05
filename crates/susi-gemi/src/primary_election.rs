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
