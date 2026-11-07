//! The self-build lifecycle ledger.
//!
//! `scripts/check-rsi-scorecard.py` reads `.agents/rsi-loop.jsonl` for the
//! facts git cannot supply: what a task cost and what went wrong. The harness
//! is the component that knows both — the gateway prices every call and the
//! supervisor sees every failed attempt — so it writes the events.
//!
//! The schema is the scorecard's:
//!
//! ```json
//! {"task":"T-AGENT-1","phase":"merged","at_unix":1710000300,"cost":0.0123,"cost_unit":"usd"}
//! {"task":"T-AGENT-1","phase":"defect","at_unix":1710000200,"outcome":"not-accepted","detail":"…"}
//! ```
//!
//! Cost crosses to the report as a dollar number because that is the unit the
//! scorecard sums; the harness keeps integer micro-USD everywhere else and
//! converts once, here, at the reporting boundary. `released` and `promoted`
//! are deliberately not written: those are release actions, and inventing them
//! would turn a missing phase into a fabricated success.

use crate::ledger::read_entries_reporting;
use crate::money::MicroUsd;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use susi_error::{EaiError, EaiResult};

/// Where the scorecard looks for lifecycle events by default.
pub const DEFAULT_LIFECYCLE_PATH: &str = ".agents/rsi-loop.jsonl";

/// One lifecycle event, in the scorecard's schema.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LifecycleEvent {
    /// Task id this event belongs to.
    pub task: String,
    /// proposed, implemented, gated, merged, released, promoted or defect.
    pub phase: String,
    /// Seconds since the Unix epoch.
    pub at_unix: u64,
    /// Cost, when this event carries one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
    /// The unit of `cost`; the harness always reports `usd`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_unit: Option<String>,
    /// For a defect: failed, not-accepted, …
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    /// For a defect: what happened, in the operator's language.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl LifecycleEvent {
    /// Start an event for `task` at `at_unix`.
    #[must_use]
    pub fn new(task: impl Into<String>, phase: impl Into<String>, at_unix: u64) -> Self {
        Self {
            task: task.into(),
            phase: phase.into(),
            at_unix,
            cost: None,
            cost_unit: None,
            outcome: None,
            detail: None,
        }
    }

    /// The `merged` event, carrying what the task cost.
    #[must_use]
    pub fn merged(task: impl Into<String>, at_unix: u64, cost: MicroUsd) -> Self {
        Self::new(task, "merged", at_unix).with_cost(cost)
    }

    /// A defect event, carrying its outcome and detail.
    #[must_use]
    pub fn defect(
        task: impl Into<String>,
        at_unix: u64,
        outcome: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self::new(task, "defect", at_unix)
            .with_outcome(outcome)
            .with_detail(detail)
    }

    /// Attach the cost the scorecard sums.
    #[must_use]
    #[allow(clippy::cast_precision_loss)] // reporting only: a dollar amount is far below 2^53
    pub fn with_cost(mut self, cost: MicroUsd) -> Self {
        self.cost = Some(cost.micros() as f64 / 1_000_000.0);
        self.cost_unit = Some("usd".to_string());
        self
    }

    /// Attach a defect outcome.
    #[must_use]
    pub fn with_outcome(mut self, outcome: impl Into<String>) -> Self {
        self.outcome = Some(outcome.into());
        self
    }

    /// Attach a human-readable detail.
    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    /// Verify structural invariants of a [`LifecycleEvent`].
    ///
    /// # Errors
    ///
    /// Returns a human-readable error if `task` or `phase` is blank, `at_unix`
    /// is zero, `cost` and `cost_unit` are inconsistently populated or invalid,
    /// `outcome` or `detail` is blank when present, or a `"defect"` event has
    /// no `outcome`.
    pub fn verify_event_invariants(&self) -> Result<(), String> {
        if self.task.trim().is_empty() {
            return Err("LifecycleEvent.task must not be empty".to_string());
        }
        if self.phase.trim().is_empty() {
            return Err("LifecycleEvent.phase must not be empty".to_string());
        }
        if self.at_unix == 0 {
            return Err("LifecycleEvent.at_unix must be greater than zero".to_string());
        }
        match (&self.cost, &self.cost_unit) {
            (Some(cost), Some(unit)) => {
                if !cost.is_finite() || *cost < 0.0 {
                    return Err("LifecycleEvent.cost must be finite and non-negative".to_string());
                }
                if unit.trim() != "usd" {
                    return Err("LifecycleEvent.cost_unit must be 'usd'".to_string());
                }
            }
            (Some(_), None) | (None, Some(_)) => {
                return Err(
                    "LifecycleEvent.cost and cost_unit must both be present or both absent"
                        .to_string(),
                );
            }
            (None, None) => {}
        }
        if let Some(ref outcome) = self.outcome {
            if outcome.trim().is_empty() {
                return Err("LifecycleEvent.outcome must not be blank when present".to_string());
            }
        }
        if let Some(ref detail) = self.detail {
            if detail.trim().is_empty() {
                return Err("LifecycleEvent.detail must not be blank when present".to_string());
            }
        }
        if self.phase == "defect" && self.outcome.is_none() {
            return Err("LifecycleEvent in 'defect' phase must carry an outcome".to_string());
        }
        Ok(())
    }
}

/// Seconds since the Unix epoch.
#[must_use]
pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// Append one event as a JSON line.
///
/// # Errors
///
/// Returns `EaiError::config` for a blank task or phase, and `EaiError::io`
/// when the event cannot be encoded or the file cannot be opened.
pub fn append(path: &Path, event: &LifecycleEvent) -> EaiResult<()> {
    if event.task.trim().is_empty() {
        return Err(EaiError::config("lifecycle event has no task id"));
    }
    if event.phase.trim().is_empty() {
        return Err(EaiError::config("lifecycle event has no phase"));
    }
    let line = serde_json::to_string(event)
        .map_err(|error| EaiError::io(format!("encode lifecycle event: {error}")))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            EaiError::io(format!(
                "create lifecycle dir {}: {error}",
                parent.display()
            ))
        })?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| EaiError::io(format!("open lifecycle log {}: {error}", path.display())))?;
    writeln!(file, "{line}")
        .map_err(|error| EaiError::io(format!("append lifecycle log {}: {error}", path.display())))
}

/// Read every event; a missing log is empty.
///
/// # Errors
///
/// Returns `EaiError::io` when a line cannot be decoded.
pub fn read(path: &Path) -> EaiResult<Vec<LifecycleEvent>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(EaiError::io(format!(
                "read lifecycle log {}: {error}",
                path.display()
            )))
        }
    };
    let mut events = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let event = serde_json::from_str(line).map_err(|error| {
            EaiError::io(format!(
                "lifecycle log {} line {}: {error}",
                path.display(),
                index + 1
            ))
        })?;
        events.push(event);
    }
    Ok(events)
}

/// What one mission's task cost, read from the priced ledger.
///
/// A missing ledger means the ledger path was not shared with the harness; the
/// cost is reported as zero rather than guessed. An unreadable line costs only
/// itself ([`read_entries_reporting`]): the strict reader failed the whole
/// file on one torn line, and every merged task's cost then read $0.
#[must_use]
pub fn task_cost(ledger_path: &Path, mission: &str, task: &str) -> MicroUsd {
    let entries = read_entries_reporting(ledger_path);
    entries
        .iter()
        .filter(|entry| entry.mission == mission && entry.task == task)
        .fold(MicroUsd::ZERO, |sum, entry| {
            sum.checked_add(entry.cost)
                .unwrap_or(MicroUsd::from_micros(u64::MAX))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::{append_entry, Dispatch, LedgerEntry, Outcome};
    use crate::pricing::{ModelPrice, TokenUsage};

    #[test]
    fn events_round_trip_and_cost_is_dollars() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".agents").join("rsi-loop.jsonl");
        append(
            &path,
            &LifecycleEvent::merged("T-AGENT-1", 100, MicroUsd::from_micros(250_000)),
        )
        .unwrap();
        append(
            &path,
            &LifecycleEvent::defect("T-AGENT-1", 90, "not-accepted", "acceptance failed"),
        )
        .unwrap();
        let events = read(&path).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].phase, "merged");
        assert_eq!(events[0].cost, Some(0.25));
        assert_eq!(events[0].cost_unit.as_deref(), Some("usd"));
        assert_eq!(events[1].outcome.as_deref(), Some("not-accepted"));
        assert_eq!(events[1].detail.as_deref(), Some("acceptance failed"));
    }

    #[test]
    fn a_missing_log_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read(&dir.path().join("absent.jsonl")).unwrap().is_empty());
    }

    #[test]
    fn a_blank_task_or_phase_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rsi-loop.jsonl");
        assert!(append(&path, &LifecycleEvent::new("", "merged", 1)).is_err());
        assert!(append(&path, &LifecycleEvent::new("T-1", "", 1)).is_err());
    }

    #[test]
    fn task_cost_sums_only_that_missions_task() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = dir.path().join("ledger.jsonl");
        let entry = |mission: &str, task: &str, micros: u64| {
            LedgerEntry::settle(Dispatch {
                mission: mission.into(),
                task: task.into(),
                attempt: 1,
                model: "deepseek-flash".into(),
                provider: "deepseek".into(),
                usage: TokenUsage::new(1_000_000, 0, 0),
                price: ModelPrice {
                    input_miss: MicroUsd::from_micros(micros),
                    input_hit: MicroUsd::ZERO,
                    output: MicroUsd::ZERO,
                },
                started_unix_ms: 0,
                ended_unix_ms: 1,
                outcome: Outcome::Completed,
            })
        };
        append_entry(&ledger, &entry("m-1", "t-1", 100)).unwrap();
        append_entry(&ledger, &entry("m-1", "t-1", 50)).unwrap();
        append_entry(&ledger, &entry("m-1", "t-2", 900)).unwrap();
        append_entry(&ledger, &entry("m-2", "t-1", 700)).unwrap();
        assert_eq!(task_cost(&ledger, "m-1", "t-1"), MicroUsd::from_micros(150));
    }

    #[test]
    fn now_unix_is_positive_and_read_skips_blank_lines_or_errors_on_corrupt_json() {
        assert!(now_unix() > 0);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lifecycle.jsonl");
        let evt = LifecycleEvent::new("T-2", "claimed", 123)
            .with_outcome("ok")
            .with_detail("detail");
        let line = serde_json::to_string(&evt).unwrap();
        std::fs::write(&path, format!("\n   \n{line}\n")).unwrap();
        assert_eq!(read(&path).unwrap(), vec![evt]);

        std::fs::write(&path, format!("{line}\n{{corrupt\n")).unwrap();
        let err = read(&path).unwrap_err().to_string();
        assert!(err.contains("line 2:"), "unexpected error: {err}");
    }

    #[test]
    fn lifecycle_event_verify_invariants_reject_corrupt_state() {
        let valid_merged =
            LifecycleEvent::merged("T-AGENT-1", 1_710_000_300, MicroUsd::from_micros(250_000));
        assert!(valid_merged.verify_event_invariants().is_ok());

        let valid_defect =
            LifecycleEvent::defect("T-AGENT-1", 1_710_000_200, "not-accepted", "gate failed");
        assert!(valid_defect.verify_event_invariants().is_ok());

        let blank_task = LifecycleEvent::new("  ", "merged", 100);
        assert!(blank_task.verify_event_invariants().is_err());

        let blank_phase = LifecycleEvent::new("T-1", " ", 100);
        assert!(blank_phase.verify_event_invariants().is_err());

        let zero_time = LifecycleEvent::new("T-1", "proposed", 0);
        assert!(zero_time.verify_event_invariants().is_err());

        let cost_without_unit = LifecycleEvent {
            cost: Some(0.10),
            cost_unit: None,
            ..LifecycleEvent::new("T-1", "merged", 100)
        };
        assert!(cost_without_unit.verify_event_invariants().is_err());

        let neg_cost = LifecycleEvent {
            cost: Some(-0.01),
            cost_unit: Some("usd".to_string()),
            ..LifecycleEvent::new("T-1", "merged", 100)
        };
        assert!(neg_cost.verify_event_invariants().is_err());

        let bad_unit = LifecycleEvent {
            cost: Some(0.10),
            cost_unit: Some("eur".to_string()),
            ..LifecycleEvent::new("T-1", "merged", 100)
        };
        assert!(bad_unit.verify_event_invariants().is_err());

        let defect_missing_outcome = LifecycleEvent::new("T-1", "defect", 100);
        assert!(defect_missing_outcome.verify_event_invariants().is_err());

        let blank_outcome = LifecycleEvent::new("T-1", "defect", 100).with_outcome("   ");
        assert!(blank_outcome.verify_event_invariants().is_err());

        let blank_detail = LifecycleEvent::new("T-1", "proposed", 100).with_detail("  ");
        assert!(blank_detail.verify_event_invariants().is_err());
    }
}
