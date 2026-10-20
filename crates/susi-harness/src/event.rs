//! The live event feed the cockpit renders.
//!
//! One [`Event`] per action — a model call, a tool call, a git action, a CI
//! result, a decision — with a human-readable `summary` and an optional cost.
//! The feed is append-only and ordered by `seq`; a cockpit renders it and never
//! becomes a second source of truth.

use crate::money::MicroUsd;
use serde::{Deserialize, Serialize};

/// What kind of action an event records.
///
/// The panel groups by kind; the supervisor never infers state from the feed —
/// the ledger and mission records are the facts, the feed is the narrative.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// A mission was accepted and queued.
    MissionStarted,
    /// A mission was paused by the operator or by credit exhaustion.
    MissionPaused,
    /// A paused mission resumed.
    MissionResumed,
    /// A mission reached its acceptance.
    MissionFinished,
    /// The capability ladder was exhausted without acceptance.
    MissionStuck,
    /// A task was handed to an executor.
    TaskDispatched,
    /// A task settled (accepted, rejected or cancelled).
    TaskFinished,
    /// A model call started or finished.
    ModelCall,
    /// An executor ran a tool.
    ToolCall,
    /// A git action: worktree, claim, commit, push, pull request, merge.
    GitAction,
    /// A CI result.
    CiStatus,
    /// The supervisor needs a decision from the operator.
    DecisionRequested,
    /// The operator answered a decision.
    DecisionResolved,
    /// A provider balance crossed its low-water mark.
    CreditLow,
    /// A provider balance recovered after a top-up.
    CreditRestored,
}

/// One row of the live feed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    /// Monotonic sequence number within this supervisor.
    pub seq: u64,
    /// Milliseconds since the Unix epoch.
    pub at_unix_ms: u64,
    /// Mission this event belongs to, when it is mission-scoped.
    pub mission: Option<String>,
    /// Task this event belongs to, when it is task-scoped.
    pub task: Option<String>,
    /// What kind of action this is.
    pub kind: EventKind,
    /// What happened, in the operator's language — never code.
    pub summary: String,
    /// What it cost, when the action spent money.
    pub cost: Option<MicroUsd>,
}

impl Event {
    /// Start an event at `seq`/`at_unix_ms` with a summary.
    #[must_use]
    pub fn new(seq: u64, at_unix_ms: u64, kind: EventKind, summary: impl Into<String>) -> Self {
        Self {
            seq,
            at_unix_ms,
            mission: None,
            task: None,
            kind,
            summary: summary.into(),
            cost: None,
        }
    }

    /// Attach the mission this event belongs to.
    #[must_use]
    pub fn with_mission(mut self, mission: impl Into<String>) -> Self {
        self.mission = Some(mission.into());
        self
    }

    /// Attach the task this event belongs to.
    #[must_use]
    pub fn with_task(mut self, task: impl Into<String>) -> Self {
        self.task = Some(task.into());
        self
    }

    /// Attach the cost this action incurred.
    #[must_use]
    pub fn with_cost(mut self, cost: MicroUsd) -> Self {
        self.cost = Some(cost);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_event_carries_scope_and_cost() {
        let event = Event::new(
            7,
            1_700_000_000_000,
            EventKind::ModelCall,
            "deepseek-flash · 91% cached",
        )
        .with_mission("VC-202-025")
        .with_task("T-1")
        .with_cost(MicroUsd::from_micros(3_000));
        assert_eq!(event.mission.as_deref(), Some("VC-202-025"));
        assert_eq!(event.task.as_deref(), Some("T-1"));
        assert_eq!(event.cost, Some(MicroUsd::from_micros(3_000)));
    }

    #[test]
    fn events_round_trip_as_json() {
        let event = Event::new(1, 42, EventKind::DecisionRequested, "Task needs a decision")
            .with_mission("m");
        let json = serde_json::to_string(&event).unwrap();
        let back: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(back, event);
    }

    #[test]
    fn kind_serializes_snake_case() {
        let json = serde_json::to_string(&EventKind::CreditLow).unwrap();
        assert_eq!(json, "\"credit_low\"");
    }
}
