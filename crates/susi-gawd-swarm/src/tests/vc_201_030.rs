use crate::mission_resume::{DagNodeView, MissionSummary, MissionView, NodeView};

#[test]
fn vc_201_030_partial_never_reports_full_success() {
    let mut m = MissionView::new("m1");
    m.upsert(DagNodeView {
        id: "n1".into(),
        view: NodeView::Completed,
        resumable: false,
        output: Some("ok".into()),
    });
    m.upsert(DagNodeView {
        id: "n2".into(),
        view: NodeView::Running,
        resumable: true,
        output: None,
    });
    assert_eq!(m.summary(), MissionSummary::Resumable);
    assert!(!m.reports_full_success());
    let targets = m.resume_targets();
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].id, "n2");
}

#[test]
fn vc_201_030_cancelled_uncertain_views() {
    let mut m = MissionView::new("m2");
    m.upsert(DagNodeView {
        id: "c".into(),
        view: NodeView::Cancelled,
        resumable: false,
        output: None,
    });
    m.upsert(DagNodeView {
        id: "u".into(),
        view: NodeView::Uncertain,
        resumable: true,
        output: None,
    });
    m.upsert(DagNodeView {
        id: "b".into(),
        view: NodeView::Blocked,
        resumable: true,
        output: None,
    });
    assert!(!m.reports_full_success());
    assert_eq!(m.resume_targets().len(), 2);
}

#[test]
fn vc_201_030_all_completed_is_full_success() {
    let mut m = MissionView::new("m3");
    m.upsert(DagNodeView {
        id: "n".into(),
        view: NodeView::Completed,
        resumable: false,
        output: Some("done".into()),
    });
    assert_eq!(m.summary(), MissionSummary::FullyComplete);
    assert!(m.reports_full_success());
}

/// Wiring test: durable mission resume CLI for partial swarm outcomes.
///
/// A mission with mixed terminal states reports Resumable (not
/// Partial), never Full success; resume_targets only returns
/// blocked/running/uncertain nodes so a CLI resume drill never
/// re-runs completed work.
#[test]
fn mission_resume_cli_wiring() {
    let mut m = MissionView::new("partial-mission");
    m.upsert(DagNodeView {
        id: "done".into(),
        view: NodeView::Completed,
        resumable: false,
        output: Some("ok".into()),
    });
    m.upsert(DagNodeView {
        id: "blocked".into(),
        view: NodeView::Blocked,
        resumable: true,
        output: None,
    });
    m.upsert(DagNodeView {
        id: "running".into(),
        view: NodeView::Running,
        resumable: true,
        output: None,
    });
    // Mixed states with resumable work => Resumable, never Full success.
    assert_eq!(m.summary(), MissionSummary::Resumable);
    assert!(!m.reports_full_success());
    // Only blocked + running are resume targets; completed excluded.
    let targets = m.resume_targets();
    assert_eq!(targets.len(), 2);
    assert!(targets.iter().any(|n| n.id == "blocked"));
    assert!(targets.iter().any(|n| n.id == "running"));
}
