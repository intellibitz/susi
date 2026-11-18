use super::*;
use crate::ledger::Dispatch;
use crate::pricing::{ModelPrice, TokenUsage};

fn write_bus(dir: &Path) -> std::path::PathBuf {
    let p = dir.join("graph.jsonl");
    let data = "{\"from\":\"planner\",\"to\":\"coders\",\"kind\":\"plan\",\"payload\":{\"summary\":\"p\"}}\n\
         {\"from\":\"coder-a\",\"to\":\"reviewer\",\"kind\":\"work\",\"payload\":{}}\n\
         not json\n\
         {\"from\":\"reviewer\",\"to\":\"coders\",\"kind\":\"rejection\",\"payload\":{\"summary\":\"no\"}}\n";
    std::fs::write(&p, data).unwrap();
    p
}

#[test]
fn mission_control_reads_handoffs_and_counts_verdicts() {
    let dir = susi_paths::tempfile::tempdir().unwrap();
    let v = GraphView::load(&write_bus(dir.path()));
    assert_eq!(v.messages.len(), 3);
    assert_eq!(v.roles, vec!["coder-a", "planner", "reviewer"]);
    assert_eq!((v.approvals, v.rejections, v.approved()), (0, 1, false));
    assert_eq!(v.verdict.as_deref(), Some("rejection"));
    assert_eq!(v.messages[0].kind, "plan");
    assert_eq!(v.messages[0].payload, "{\"summary\":\"p\"}");
}

#[test]
fn mission_control_missing_bus_is_empty() {
    let dir = susi_paths::tempfile::tempdir().unwrap();
    let v = GraphView::load(&dir.path().join("absent.jsonl"));
    assert!(v.is_empty() && v.roles.is_empty() && v.verdict.is_none());
}

#[test]
fn mission_control_approval_sets_the_verdict() {
    let dir = susi_paths::tempfile::tempdir().unwrap();
    let p = dir.path().join("graph.jsonl");
    let line = "{\"from\":\"reviewer\",\"to\":\"coders\",\"kind\":\"approval\",\"payload\":{}}\n";
    std::fs::write(&p, line).unwrap();
    let v = GraphView::load(&p);
    assert_eq!(v.approvals, 1);
    assert!(v.approved() && v.roles == vec!["reviewer"]);
}

fn event(seq: u64, at: u64, m: &str, k: EventKind, s: &str) -> Event {
    Event::new(seq, at, k, s).with_mission(m)
}

fn ledger_entry(m: &str, t: &str, md: &str, end: u64, micros: u64) -> LedgerEntry {
    LedgerEntry::settle(Dispatch {
        mission: m.into(),
        task: t.into(),
        attempt: 1,
        model: md.into(),
        provider: "deepseek".into(),
        usage: TokenUsage::new(1_000_000, 0, 0),
        price: ModelPrice {
            input_miss: MicroUsd::from_micros(micros),
            input_hit: MicroUsd::ZERO,
            output: MicroUsd::ZERO,
        },
        started_unix_ms: end.saturating_sub(1),
        ended_unix_ms: end,
        outcome: Outcome::Completed,
    })
}

fn graph(msgs: &[(&str, &str, &str)]) -> GraphView {
    GraphView {
        messages: msgs
            .iter()
            .map(|(f, t, k)| GraphMessage {
                from: (*f).into(),
                to: (*t).into(),
                kind: (*k).into(),
                payload: "{}".into(),
            })
            .collect(),
        ..GraphView::default()
    }
}

fn count_kind(r: &Replay, pfx: &str) -> usize {
    r.events.iter().filter(|s| s.kind.starts_with(pfx)).count()
}

#[test]
fn replay_orders_steps_by_time_then_kind_and_detail() {
    let events = vec![
        event(2, 200, "m", EventKind::ToolCall, "b"),
        event(1, 100, "m", EventKind::MissionStarted, "a"),
        event(3, 200, "m", EventKind::GitAction, "c"),
    ];
    let entries = vec![ledger_entry("m", "t-1", "deepseek-flash", 150, 7)];
    let r = Replay::build(&events, &entries, &GraphView::default(), "m");
    let ts: Vec<_> = r.events.iter().map(|s| s.at_unix_ms).collect();
    assert_eq!(ts, vec![100, 150, 200, 200]);
    assert_eq!(r.events[2].kind, "event:git_action");
    assert_eq!(r.events[3].kind, "event:tool_call");
    assert_eq!(r.events[1].kind, "ledger:deepseek-flash");
    assert_eq!(r.events[1].cost_micro_usd, 7);
    assert_eq!(r.events[1].detail, "t-1 attempt 1 · completed");
    assert_eq!(r.events[0].cost_micro_usd, 0);
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
    let g = graph(&[("planner", "coders", "plan")]);
    let f = Replay::build(&events, &entries, &g, "m-1");
    assert_eq!(count_kind(&f, "event:"), 1);
    assert_eq!(count_kind(&f, "ledger:"), 1);
    assert_eq!(count_kind(&f, "graph:"), 1);
    let all = Replay::build(&events, &entries, &g, "");
    assert_eq!(count_kind(&all, "event:"), 3);
    assert_eq!(count_kind(&all, "ledger:"), 2);
}

#[test]
fn replay_keeps_only_the_newest_two_hundred_steps() {
    let ev: Vec<_> = (1..=205)
        .map(|i| event(i, i, "m", EventKind::ModelCall, &format!("s {i}")))
        .collect();
    let r = Replay::build(&ev, &[], &GraphView::default(), "m");
    assert_eq!(r.events.len(), 200);
    assert_eq!(r.events.first().unwrap().at_unix_ms, 6);
    assert_eq!(r.events.last().unwrap().at_unix_ms, 205);
}

#[test]
fn replay_stamps_graph_steps_from_the_missions_first_event() {
    let events = vec![
        event(1, 1_000, "m", EventKind::MissionStarted, "start"),
        event(2, 5_000, "m", EventKind::MissionFinished, "finish"),
    ];
    let g = graph(&[
        ("planner", "coders", "plan"),
        ("reviewer", "planner", "approval"),
    ]);
    let r = Replay::build(&events, &[], &g, "m");
    let gs: Vec<_> = r
        .events
        .iter()
        .filter(|s| s.kind.starts_with("graph:"))
        .collect();
    assert_eq!(gs.len(), 2);
    assert_eq!((gs[0].at_unix_ms, gs[1].at_unix_ms), (1_000, 1_001));
    assert_eq!(gs[0].kind, "graph:planner->coders:plan");
    assert_eq!(gs[1].kind, "graph:reviewer->planner:approval");
    assert_eq!(gs[0].cost_micro_usd, 0);
    assert_eq!(r.events[0].kind, "event:mission_started");
    assert_eq!(r.events[1].kind, "graph:planner->coders:plan");
    let bare = Replay::build(&[], &[], &g, "m");
    assert_eq!(
        (bare.events[0].at_unix_ms, bare.events[1].at_unix_ms),
        (0, 1)
    );
}
