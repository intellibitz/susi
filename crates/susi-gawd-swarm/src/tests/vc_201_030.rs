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
