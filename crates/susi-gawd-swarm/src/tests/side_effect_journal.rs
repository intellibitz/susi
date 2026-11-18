//! Crash-replay safety via a durable intent journal (T-DEVIN-10): intent is
//! recorded before dispatch, a crash leaves it Pending→Uncertain, and replay
//! is refused until the uncertain entry is reconciled — exec_command and
//! apply_patch are never classified idempotent by tool name.

use crate::dag::MissionDag;
use crate::mission_persist::PersistedMission;
use crate::side_effect_journal::{IntentJournal, IntentState};
use crate::side_effects::{classify_tool_action, ActionClass};
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!(
        "side-effect-journal-{tag}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|t| t.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn side_effect_journal_records_intent_before_dispatch() {
    let mut j = IntentJournal::new();
    let id = j.record_intent("n0", "exec_command", "sh -c 'x'", 100);
    assert_eq!(j.get(&id).unwrap().state, IntentState::Pending);
    j.mark_executed(&id);
    assert_eq!(j.get(&id).unwrap().state, IntentState::Executed);
}

#[test]
fn side_effect_journal_crash_blocks_replay_until_reconciled() {
    let mut j = IntentJournal::new();
    let id = j.record_intent("n0", "exec_command", "sh -c 'rm f'", 100);
    // Crash: never marked executed.
    let uncertain = j.reconcile_after_crash();
    assert_eq!(uncertain, vec![id.clone()]);
    assert_eq!(j.get(&id).unwrap().state, IntentState::Uncertain);
    assert!(
        !j.may_replay_node("n0"),
        "uncertain side effect must block replay"
    );
    assert_eq!(j.unreconciled("n0"), vec![id.clone()]);

    j.resolve(&id, true);
    assert_eq!(j.get(&id).unwrap().state, IntentState::Executed);
    assert!(j.may_replay_node("n0"));
}

#[test]
fn side_effect_journal_replay_dedupes_by_args_digest() {
    let mut j = IntentJournal::new();
    let a = j.record_intent("n0", "exec_command", "sh -c 'a'", 100);
    // Same call replays under the same intent — no duplicate side effect.
    assert_eq!(j.record_intent("n0", "exec_command", "sh -c 'a'", 101), a);
    // A different command is a new intent needing its own record.
    let b = j.record_intent("n0", "exec_command", "sh -c 'b'", 102);
    assert_ne!(a, b);
    // Same args under a different node is independent.
    let c = j.record_intent("n1", "exec_command", "sh -c 'a'", 103);
    assert_ne!(a, c);
}

#[test]
fn side_effect_journal_exec_and_patch_are_not_idempotent() {
    assert_eq!(
        classify_tool_action("exec_command"),
        ActionClass::Reconcilable
    );
    assert_eq!(
        classify_tool_action("apply_patch"),
        ActionClass::Reconcilable
    );
    assert_eq!(
        classify_tool_action("write_file"),
        ActionClass::Reconcilable
    );
    assert_eq!(classify_tool_action("read_file"), ActionClass::ReadOnly);
    assert_eq!(
        classify_tool_action("one_shot_pay"),
        ActionClass::NonRetryable
    );
}

#[test]
fn side_effect_journal_survives_mission_crash_and_resume() {
    let dir = temp_dir("resume");
    let mut dag = MissionDag::new("journal mission");
    let _ = dag.push_node("w", "work", vec![0]);
    // Recorded pre-dispatch, then the process "crashes" (never executed).
    let _ = dag.record_dispatch_intent(0, "exec_command", "sh -c 'rm tmp'");
    let persist = dag.to_persisted("m-journal");
    persist.save(&dir).unwrap();

    let loaded = PersistedMission::load(&dir.join("m-journal.json")).unwrap();
    let dag2 = MissionDag::from_persisted(&loaded);
    // Resume reconciles the pending intent to Uncertain and blocks replay.
    assert!(
        !dag2.may_retry_node(0),
        "uncertain intent must block node retry until reconciled"
    );
    let blocking = dag2
        .intent_journal
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .unreconciled("n0");
    assert_eq!(blocking.len(), 1, "{blocking:?}");

    // Reconcile → retry allowed again.
    dag2.intent_journal
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .resolve(&blocking[0], false);
    assert!(dag2.may_retry_node(0));
}

#[test]
fn side_effect_journal_executed_intents_permit_replay() {
    let mut dag = MissionDag::new("m");
    let _ = dag.push_node("w", "work", vec![0]);
    let id = dag.record_dispatch_intent(1, "exec_command", "sh -c 'x'");
    dag.mark_intent_executed(&id);
    // Executed intents don't block; crash reconcile leaves them alone.
    dag.intent_journal
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .reconcile_after_crash();
    assert!(dag.may_retry_node(1));
}
