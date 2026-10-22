//! The mission-control view of an agent graph.
//!
//! The graph writes every handoff to its bus as JSON lines; this module turns
//! that file into what a cockpit renders: the handoffs in order, the roles that
//! produced them, and the reviewer's verdicts. It also assembles a mission's
//! [`Replay`], one deterministic timeline over what the harness already
//! journals — the event feed, the ledger and the graph bus. Everything here is
//! read-only and recomputed on every call, so the cockpit never becomes a
//! second source of truth.

use crate::event::{Event, EventKind};
use crate::ledger::{LedgerEntry, Outcome};
use crate::money::MicroUsd;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::Path;

/// How many of the newest steps a replay keeps.
const REPLAY_CAP: usize = 200;

/// One handoff read from the bus.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GraphMessage {
    /// Sending role.
    pub from: String,
    /// Receiving role.
    pub to: String,
    /// plan, work, review, approval or rejection.
    pub kind: String,
    /// Payload as JSON text.
    pub payload: String,
}

/// The graph as a mission-control surface sees it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct GraphView {
    /// Handoffs, in the order they happened.
    pub messages: Vec<GraphMessage>,
    /// Roles that produced a handoff, ordered by name.
    pub roles: Vec<String>,
    /// Approvals the reviewer recorded.
    pub approvals: usize,
    /// Rejections the reviewer recorded.
    pub rejections: usize,
    /// The reviewer's latest verdict (`approval` or `rejection`).
    pub verdict: Option<String>,
}

impl GraphView {
    /// Read a bus file, skipping any line that is not a handoff.
    #[must_use]
    pub fn load(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        let mut view = Self::default();
        let mut roles = BTreeSet::new();
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            let Ok(value) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let from = value
                .get("from")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let to = value
                .get("to")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let kind = value
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            match kind.as_str() {
                "approval" => {
                    view.approvals += 1;
                    view.verdict = Some(kind.clone());
                }
                "rejection" => {
                    view.rejections += 1;
                    view.verdict = Some(kind.clone());
                }
                _ => {}
            }
            roles.insert(from.clone());
            view.messages.push(GraphMessage {
                from,
                to,
                kind,
                payload: value
                    .get("payload")
                    .map(ToString::to_string)
                    .unwrap_or_default(),
            });
        }
        view.roles = roles.into_iter().collect();
        view
    }

    /// Whether the bus held no handoffs.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// Whether the reviewer's latest verdict was acceptance.
    #[must_use]
    pub fn approved(&self) -> bool {
        self.verdict.as_deref() == Some("approval")
    }
}

/// One step of a mission's replay timeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReplayEvent {
    /// When the step happened, milliseconds since the Unix epoch.
    pub at_unix_ms: u64,
    /// `event:<kind>`, `ledger:<model>` or `graph:<from>-><to>:<kind>`.
    pub kind: String,
    /// What the step says: an event summary, a ledger task and outcome, or the
    /// graph message's payload.
    pub detail: String,
    /// What the step cost, in micro-USD. Events carry the cost they recorded,
    /// a ledger step carries its settled cost, and a graph handoff is free to
    /// replay because its spend is in the ledger.
    pub cost_micro_usd: u64,
}

/// A mission's replay: every journalled step in one deterministic order.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Replay {
    /// Steps, oldest first, at most the newest 200.
    pub events: Vec<ReplayEvent>,
}

impl Replay {
    /// Assemble the replay for `mission`, or the whole journal when it is
    /// empty.
    ///
    /// Three journals contribute steps. An event is stamped by its own
    /// `at_unix_ms`. A ledger row is stamped when it settled
    /// (`ended_unix_ms`), because that is the moment the spend exists. The
    /// graph bus has no clock at all, so its handoffs are stamped from an
    /// ordinal anchored at the mission's first event: the first handoff sits at
    /// that event's millisecond, each later one a millisecond after it, and at
    /// `0, 1, …` when the mission has no events. This keeps the handoffs inside
    /// the mission's time window without inventing wall-clock times, and makes
    /// the same journal always replay to the same timeline.
    ///
    /// Steps are sorted by time, then by `kind` and `detail` so identical
    /// timestamps have a stable order, and only the newest 200 are kept.
    #[must_use]
    pub fn build(
        events: &[Event],
        entries: &[LedgerEntry],
        graph: &GraphView,
        mission: &str,
    ) -> Self {
        let window_start = events
            .iter()
            .filter(|event| in_mission(event.mission.as_deref(), mission))
            .map(|event| event.at_unix_ms)
            .min()
            .unwrap_or(0);
        let mut steps = Vec::new();
        for event in events
            .iter()
            .filter(|event| in_mission(event.mission.as_deref(), mission))
        {
            steps.push(ReplayEvent {
                at_unix_ms: event.at_unix_ms,
                kind: format!("event:{}", event_kind(event.kind)),
                detail: event.summary.clone(),
                cost_micro_usd: event.cost.map_or(0, MicroUsd::micros),
            });
        }
        for entry in entries
            .iter()
            .filter(|entry| in_mission(Some(entry.mission.as_str()), mission))
        {
            steps.push(ReplayEvent {
                at_unix_ms: entry.ended_unix_ms,
                kind: format!("ledger:{}", entry.model),
                detail: format!(
                    "{} attempt {} · {}",
                    entry.task,
                    entry.attempt,
                    outcome_label(&entry.outcome)
                ),
                cost_micro_usd: entry.cost.micros(),
            });
        }
        let mut at = window_start;
        for message in &graph.messages {
            steps.push(ReplayEvent {
                at_unix_ms: at,
                kind: format!("graph:{}->{}:{}", message.from, message.to, message.kind),
                detail: message.payload.clone(),
                cost_micro_usd: 0,
            });
            at = at.saturating_add(1);
        }
        steps.sort_by(|left, right| {
            (left.at_unix_ms, &left.kind, &left.detail).cmp(&(
                right.at_unix_ms,
                &right.kind,
                &right.detail,
            ))
        });
        if steps.len() > REPLAY_CAP {
            steps = steps.split_off(steps.len() - REPLAY_CAP);
        }
        Self { events: steps }
    }
}

/// Whether a record belongs to the replay's mission.
fn in_mission(owner: Option<&str>, mission: &str) -> bool {
    mission.is_empty() || owner == Some(mission)
}

/// The wire name of an event kind (`mission_started`, …), as the feed writes
/// it, falling back to the variant name if the enum ever stops serializing.
fn event_kind(kind: EventKind) -> String {
    serde_json::to_value(kind)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| format!("{kind:?}"))
}

/// How a settled attempt ended, in one word for the timeline.
fn outcome_label(outcome: &Outcome) -> &'static str {
    match outcome {
        Outcome::Completed => "completed",
        Outcome::Accepted => "accepted",
        Outcome::ModelFailed { .. } => "model failed",
        Outcome::Rejected { .. } => "rejected",
        Outcome::Cancelled => "cancelled",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::Dispatch;
    use crate::pricing::{ModelPrice, TokenUsage};

    fn write_bus(dir: &Path) -> std::path::PathBuf {
        let path = dir.join("graph.jsonl");
        std::fs::write(
            &path,
            "{\"from\":\"planner\",\"to\":\"coders\",\"kind\":\"plan\",\"payload\":{\"summary\":\"p\"}}\n\
             {\"from\":\"coder-a\",\"to\":\"reviewer\",\"kind\":\"work\",\"payload\":{}}\n\
             not json\n\
             {\"from\":\"reviewer\",\"to\":\"coders\",\"kind\":\"rejection\",\"payload\":{\"summary\":\"no\"}}\n",
        )
        .unwrap();
        path
    }

    #[test]
    fn mission_control_reads_handoffs_and_counts_verdicts() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_bus(dir.path());
        let view = GraphView::load(&path);
        assert_eq!(view.messages.len(), 3);
        assert_eq!(view.roles, vec!["coder-a", "planner", "reviewer"]);
        assert_eq!(view.approvals, 0);
        assert_eq!(view.rejections, 1);
        assert_eq!(view.verdict.as_deref(), Some("rejection"));
        assert!(!view.approved());
        assert_eq!(view.messages[0].kind, "plan");
        assert_eq!(view.messages[0].payload, "{\"summary\":\"p\"}");
    }

    #[test]
    fn mission_control_missing_bus_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let view = GraphView::load(&dir.path().join("absent.jsonl"));
        assert!(view.is_empty());
        assert!(view.roles.is_empty());
        assert!(view.verdict.is_none());
    }

    #[test]
    fn mission_control_approval_sets_the_verdict() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graph.jsonl");
        std::fs::write(
            &path,
            "{\"from\":\"reviewer\",\"to\":\"coders\",\"kind\":\"approval\",\"payload\":{}}\n",
        )
        .unwrap();
        let view = GraphView::load(&path);
        assert_eq!(view.approvals, 1);
        assert!(view.approved());
        assert_eq!(view.roles, vec!["reviewer"]);
    }

    fn event(seq: u64, at: u64, mission: &str, kind: EventKind, summary: &str) -> Event {
        Event::new(seq, at, kind, summary).with_mission(mission)
    }

    fn ledger_entry(
        mission: &str,
        task: &str,
        model: &str,
        ended_unix_ms: u64,
        micros: u64,
    ) -> LedgerEntry {
        LedgerEntry::settle(Dispatch {
            mission: mission.into(),
            task: task.into(),
            attempt: 1,
            model: model.into(),
            provider: "deepseek".into(),
            usage: TokenUsage::new(1_000_000, 0, 0),
            price: ModelPrice {
                input_miss: MicroUsd::from_micros(micros),
                input_hit: MicroUsd::ZERO,
                output: MicroUsd::ZERO,
            },
            started_unix_ms: ended_unix_ms.saturating_sub(1),
            ended_unix_ms,
            outcome: Outcome::Completed,
        })
    }

    fn graph(messages: &[(&str, &str, &str)]) -> GraphView {
        GraphView {
            messages: messages
                .iter()
                .map(|(from, to, kind)| GraphMessage {
                    from: (*from).to_string(),
                    to: (*to).to_string(),
                    kind: (*kind).to_string(),
                    payload: "{}".to_string(),
                })
                .collect(),
            roles: Vec::new(),
            approvals: 0,
            rejections: 0,
            verdict: None,
        }
    }

    #[test]
    fn replay_orders_steps_by_time_then_kind_and_detail() {
        let events = vec![
            event(2, 200, "m", EventKind::ToolCall, "b"),
            event(1, 100, "m", EventKind::MissionStarted, "a"),
            event(3, 200, "m", EventKind::GitAction, "c"),
        ];
        let entries = vec![ledger_entry("m", "t-1", "deepseek-flash", 150, 7)];
        let replay = Replay::build(&events, &entries, &GraphView::default(), "m");
        assert_eq!(
            replay
                .events
                .iter()
                .map(|step| step.at_unix_ms)
                .collect::<Vec<_>>(),
            vec![100, 150, 200, 200]
        );
        // Two steps share 200 ms: `kind` is the tiebreaker, not arrival order.
        assert_eq!(replay.events[2].kind, "event:git_action");
        assert_eq!(replay.events[3].kind, "event:tool_call");
        // The ledger step settled at its end and carries its cost.
        assert_eq!(replay.events[1].kind, "ledger:deepseek-flash");
        assert_eq!(replay.events[1].cost_micro_usd, 7);
        assert_eq!(replay.events[1].detail, "t-1 attempt 1 · completed");
        // An event's own cost travels with it, and a free event is zero.
        assert_eq!(replay.events[0].cost_micro_usd, 0);
    }

    #[test]
    fn replay_filters_to_one_mission_but_keeps_the_whole_journal_unfocused() {
        let events = vec![
            event(1, 10, "m-1", EventKind::MissionStarted, "one"),
            event(2, 20, "m-2", EventKind::MissionStarted, "two"),
            Event::new(3, 30, EventKind::ControlReceived, "unscoped"),
        ];
        let entries = vec![
            ledger_entry("m-1", "t-1", "flash", 11, 3),
            ledger_entry("m-2", "t-2", "flash", 21, 4),
        ];
        let graph = graph(&[("planner", "coders", "plan")]);

        let focused = Replay::build(&events, &entries, &graph, "m-1");
        assert_eq!(
            focused
                .events
                .iter()
                .filter(|step| step.kind.starts_with("event:"))
                .count(),
            1
        );
        assert_eq!(
            focused
                .events
                .iter()
                .filter(|step| step.kind.starts_with("ledger:"))
                .count(),
            1
        );
        assert_eq!(
            focused
                .events
                .iter()
                .filter(|step| step.kind.starts_with("graph:"))
                .count(),
            1
        );

        // No focus means the whole journal, including unscoped events.
        let all = Replay::build(&events, &entries, &graph, "");
        assert_eq!(
            all.events
                .iter()
                .filter(|step| step.kind.starts_with("event:"))
                .count(),
            3
        );
        assert_eq!(
            all.events
                .iter()
                .filter(|step| step.kind.starts_with("ledger:"))
                .count(),
            2
        );
    }

    #[test]
    fn replay_keeps_only_the_newest_two_hundred_steps() {
        let events: Vec<Event> = (1..=205)
            .map(|index| {
                event(
                    index,
                    index,
                    "m",
                    EventKind::ModelCall,
                    &format!("step {index}"),
                )
            })
            .collect();
        let replay = Replay::build(&events, &[], &GraphView::default(), "m");
        assert_eq!(replay.events.len(), 200);
        assert_eq!(replay.events.first().unwrap().at_unix_ms, 6);
        assert_eq!(replay.events.last().unwrap().at_unix_ms, 205);
    }

    #[test]
    fn replay_stamps_graph_steps_from_the_missions_first_event() {
        let events = vec![
            event(1, 1_000, "m", EventKind::MissionStarted, "start"),
            event(2, 5_000, "m", EventKind::MissionFinished, "finish"),
        ];
        let graph = graph(&[
            ("planner", "coders", "plan"),
            ("reviewer", "planner", "approval"),
        ]);
        let replay = Replay::build(&events, &[], &graph, "m");
        let graph_steps: Vec<&ReplayEvent> = replay
            .events
            .iter()
            .filter(|step| step.kind.starts_with("graph:"))
            .collect();
        assert_eq!(graph_steps.len(), 2);
        assert_eq!(graph_steps[0].at_unix_ms, 1_000);
        assert_eq!(graph_steps[1].at_unix_ms, 1_001);
        assert_eq!(graph_steps[0].kind, "graph:planner->coders:plan");
        assert_eq!(graph_steps[1].kind, "graph:reviewer->planner:approval");
        assert_eq!(graph_steps[0].cost_micro_usd, 0);
        // At the anchor millisecond the event wins the `kind` tiebreak.
        assert_eq!(replay.events[0].kind, "event:mission_started");
        assert_eq!(replay.events[1].kind, "graph:planner->coders:plan");

        // A mission with no events has no window, so the ordinal starts at 0.
        let bare = Replay::build(&[], &[], &graph, "m");
        assert_eq!(bare.events[0].at_unix_ms, 0);
        assert_eq!(bare.events[1].at_unix_ms, 1);
    }
}
