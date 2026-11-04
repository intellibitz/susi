//! Wiring: durable mission resume CLI view from MissionDag (T-INTELLIBITZ-12).

use crate::cancel_propagate::{CancelToken, Descendant, WorkerKind};
use crate::dag::MissionDag;
use crate::mission_resume::{MissionSummary, NodeView};

#[test]
fn mission_resume_cli_wiring() {
    let mut dag = MissionDag::new("partial swarm outcome");
    let _ = dag.push_node("child", "depends on root", vec![0]);

    // Root still running; child blocked — CLI must not claim full success.
    let view = dag.resume_cli_view("mission-partial");
    assert_eq!(view.summary(), MissionSummary::Resumable);
    assert!(!view.reports_full_success());
    let status = MissionDag::resume_cli_status(&view);
    assert!(
        status.contains("partial") || status.contains("resumable"),
        "status={status}"
    );
    assert!(!status.contains("fully complete"));
    assert_eq!(view.nodes["n0"].view, NodeView::Running);
    assert!(view.nodes["n0"].resumable);
    assert_eq!(view.nodes["n1"].view, NodeView::Blocked);
    assert!(view.nodes["n1"].resumable);

    // Complete root only — child becomes the resume target.
    dag.nodes[0].completed = true;
    let view2 = dag.resume_cli_view("mission-partial");
    assert!(!view2.reports_full_success());
    let targets = view2.resume_targets();
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].id, "n1");
    let status2 = MissionDag::resume_cli_status(&view2);
    assert!(status2.contains("n1"), "status2={status2}");

    // Cancelled worker is not resumable and still not full success.
    dag.register_cancel_worker(1, WorkerKind::Local, true, 9_999);
    if let Some(tok) = dag.cancel.token.as_mut() {
        tok.cancel();
    } else {
        dag.cancel.set_token(CancelToken {
            root_id: "n1".into(),
            deadline_unix: 9_999,
            cancelled: true,
        });
        dag.cancel.register(Descendant {
            id: "n1".into(),
            kind: WorkerKind::Local,
            cancellable: true,
            cancel_scope: None,
            signal: None,
        });
    }
    let view3 = dag.resume_cli_view("mission-partial");
    assert_eq!(view3.nodes["n1"].view, NodeView::Cancelled);
    assert!(!view3.nodes["n1"].resumable);
    assert!(!view3.reports_full_success());

    // All completed → full success status.
    dag.nodes[1].completed = true;
    dag.cancel.token = None;
    let done = dag.resume_cli_view("mission-done");
    assert!(done.reports_full_success());
    assert_eq!(done.summary(), MissionSummary::FullyComplete);
    assert!(MissionDag::resume_cli_status(&done).contains("fully complete"));
}

#[test]
fn mission_resume_cli_wiring_uncertain_side_effect_marks_node_uncertain_and_resumable() {
    let mut dag = MissionDag::new("uncertain side effect");
    let first = dag.record_side_effect(0, "exec_command", None);
    let after_crash = dag.record_side_effect(0, "exec_command", Some(&first));
    assert!(after_crash.uncertain_external);

    let view = dag.resume_cli_view("mission-uncertain");
    assert_eq!(view.nodes["n0"].view, NodeView::Uncertain);
    assert!(view.nodes["n0"].resumable);
    assert_eq!(view.summary(), MissionSummary::Resumable);
    assert!(!view.reports_full_success());
    let status = MissionDag::resume_cli_status(&view);
    assert!(status.contains("n0"), "status={status}");
}

#[test]
fn mission_resume_cli_wiring_multi_branch_dag_resume_targets_and_status() {
    let mut dag = MissionDag::new("multi-branch");
    let b1 = dag.push_node("b1", "branch 1", vec![0]);
    let b2 = dag.push_node("b2", "branch 2", vec![0]);
    assert_eq!((b1, b2), (1, 2));

    dag.nodes[0].completed = true;
    let view = dag.resume_cli_view("mission-branches");
    assert_eq!(view.nodes["n0"].view, NodeView::Completed);
    assert!(!view.nodes["n0"].resumable);
    assert_eq!(view.nodes["n1"].view, NodeView::Running);
    assert_eq!(view.nodes["n2"].view, NodeView::Running);

    let targets: Vec<&str> = view
        .resume_targets()
        .iter()
        .map(|t| t.id.as_str())
        .collect();
    assert_eq!(targets, vec!["n1", "n2"]);
    let status = MissionDag::resume_cli_status(&view);
    assert!(status.contains("n1, n2"), "status={status}");
}

#[test]
fn mission_resume_cli_wiring_partial_non_resumable_cancelled_summary() {
    let mut dag = MissionDag::new("cancelled only");
    dag.register_cancel_worker(0, WorkerKind::Local, true, 9_999);
    if let Some(tok) = dag.cancel.token.as_mut() {
        tok.cancel();
    }

    let view = dag.resume_cli_view("mission-cancelled");
    assert_eq!(view.nodes["n0"].view, NodeView::Cancelled);
    assert!(!view.nodes["n0"].resumable);
    assert_eq!(view.summary(), MissionSummary::Partial);
    assert!(view.resume_targets().is_empty());
    let status = MissionDag::resume_cli_status(&view);
    assert!(status.contains("no resumable nodes"), "status={status}");
}

#[test]
fn mission_resume_cli_wiring_single_completed_root_has_empty_resume_targets() {
    let mut dag = MissionDag::new("single root done");
    dag.nodes[0].completed = true;
    let view = dag.resume_cli_view("mission-single-done");
    assert!(view.reports_full_success());
    assert_eq!(view.summary(), MissionSummary::FullyComplete);
    assert!(view.resume_targets().is_empty());
    assert_eq!(view.nodes["n0"].view, NodeView::Completed);
    assert!(!view.nodes["n0"].resumable);
}
