//! T-DEEPSEEK-104 / VC-202-008: susi authors its own roadmap and queue.
//!
//! The contract: a batch `{rationale, vectors, tasks}` lands roadmap
//! vectors and queue tasks through the same gates a human's edits meet —
//! `valid_vector_id`, the roadmap's type/priority vocabulary, `add`'s
//! acceptance/dependency/title rules — plus a rationale requirement and a
//! per-author bound, because a system that can add work but not justify
//! it is a runaway. Authored records carry `authored` so the review
//! surface can list them apart from hand-written work.
use crate::admin::tasks::{self, AuthoredSpec, NewTask, NewVector};
use std::path::PathBuf;

const ROADMAP: &str = r#"{
  "description": "test roadmap",
  "schema": 1,
  "strategic_priority": "x",
  "target_version": "0",
  "title": "t",
  "vectors": [
    {
      "depends_on": [],
      "id": "VC-900-001",
      "mastery_target": "the existing vector",
      "priority": "P1",
      "progress": "PARTIAL: seeded",
      "type": "OPERATIONS",
      "vector": "existing capability"
    }
  ],
  "version": 0
}
"#;

fn ws(tag: &str) -> PathBuf {
    let root =
        std::env::temp_dir().join(format!("susi-self-authored-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join(".agents/tasks")).unwrap();
    std::fs::write(root.join(".agents/roadmap.json"), ROADMAP).unwrap();
    root
}

fn task(title: &str, roadmap: Option<&str>) -> NewTask {
    NewTask {
        title: title.into(),
        goal: "why this work exists".into(),
        size: "s".into(),
        deps: vec![],
        accept: vec![
            "cargo".into(),
            "nextest".into(),
            "run".into(),
            "--locked".into(),
            "-E".into(),
            "test(self_authored_backlog)".into(),
        ],
        roadmap: roadmap.map(str::to_string),
    }
}

fn vector(id: &str) -> NewVector {
    NewVector {
        id: id.into(),
        priority: "P1".into(),
        vector: "authored capability".into(),
        mastery_target: "what mastery looks like".into(),
        vtype: "OPERATIONS".into(),
        depends_on: vec!["VC-900-001".into()],
    }
}

fn spec(rationale: &str, vectors: Vec<NewVector>, tasks: Vec<NewTask>) -> AuthoredSpec {
    AuthoredSpec {
        rationale: rationale.into(),
        vectors,
        tasks,
    }
}

fn open_task_files(root: &std::path::Path) -> usize {
    std::fs::read_dir(root.join(".agents/tasks"))
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .path()
                .extension()
                .is_some_and(|x| x == "json")
        })
        .count()
}

#[test]
fn self_authored_backlog_authors_vectors_and_tasks_through_the_same_gates() {
    let root = ws("happy");
    let report = tasks::author(
        &root,
        "deepseek",
        &spec(
            "the audit found a capability the roadmap does not name",
            vec![vector("VC-900-002")],
            vec![task("first authored task", Some("VC-900-002"))],
        ),
    )
    .unwrap();
    assert_eq!(report.vectors, vec!["VC-900-002"]);
    assert_eq!(report.tasks, vec!["T-DEEPSEEK-1"]);

    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(root.join(".agents/roadmap.json")).unwrap())
            .unwrap();
    let vectors = doc["vectors"].as_array().unwrap();
    assert_eq!(vectors.len(), 2);
    let authored = vectors
        .iter()
        .find(|v| v["id"] == "VC-900-002")
        .expect("the authored vector is on the roadmap");
    // Same fields, same "planned and queued" opening a hand-written entry carries.
    assert_eq!(
        authored.as_object().unwrap().keys().collect::<Vec<_>>(),
        vec![
            "depends_on",
            "id",
            "mastery_target",
            "priority",
            "progress",
            "type",
            "vector"
        ]
    );
    assert_eq!(authored["depends_on"], serde_json::json!(["VC-900-001"]));
    assert!(
        authored["progress"]
            .as_str()
            .unwrap()
            .starts_with("PARTIAL: planned and queued"),
        "an authored vector opens with the standard narrative"
    );

    let t: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join(".agents/tasks/T-DEEPSEEK-1.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(t["authored"], serde_json::json!(true));
    assert_eq!(t["created_by"], "DEEPSEEK");
    assert_eq!(t["roadmap"], "VC-900-002");
}

#[test]
fn self_authored_backlog_vector_gates_reject_each_bad_shape() {
    let root = ws("vector-gates");
    let before = std::fs::read_to_string(root.join(".agents/roadmap.json")).unwrap();
    for (tag, v) in [
        ("bad id", vector("banana")),
        ("already on the roadmap", vector("VC-900-001")),
        ("priority outside P0–P2", {
            let mut v = vector("VC-900-010");
            v.priority = "P9".into();
            v
        }),
        ("type outside the roadmap vocabulary", {
            let mut v = vector("VC-900-011");
            v.vtype = "STUFF".into();
            v
        }),
        ("dep on an unknown vector", {
            let mut v = vector("VC-900-012");
            v.depends_on = vec!["VC-999-999".into()];
            v
        }),
        ("dep on itself", {
            let mut v = vector("VC-900-013");
            v.depends_on = vec!["VC-900-013".into()];
            v
        }),
    ] {
        let err = tasks::author(&root, "deepseek", &spec("justified", vec![v], vec![]))
            .expect_err(&format!("{tag} must be refused"));
        assert_eq!(
            std::fs::read_to_string(root.join(".agents/roadmap.json")).unwrap(),
            before,
            "a refused vector leaves the roadmap byte-identical ({err})"
        );
    }
}

#[test]
fn self_authored_backlog_bound_refuses_a_runaway() {
    let root = ws("bound");
    let batch: Vec<NewTask> = (0..tasks::AUTHORED_OPEN_MAX)
        .map(|i| task(&format!("authored {i}"), None))
        .collect();
    tasks::author(&root, "deepseek", &spec("justified", vec![], batch)).unwrap();
    let err = tasks::author(
        &root,
        "deepseek",
        &spec("justified", vec![], vec![task("one too many", None)]),
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("bound"),
        "the refusal names the bound: {err}"
    );
    // The bound counts authored work — hand-written tasks are not capped.
    tasks::add(&root, "deepseek", task("a human added this", None)).unwrap();
    // Another agent's authored budget is untouched.
    tasks::author(
        &root,
        "codex",
        &spec("justified", vec![], vec![task("codex authored", None)]),
    )
    .unwrap();
}

#[test]
fn self_authored_backlog_task_gates_are_the_human_ones() {
    let root = ws("task-gates");
    for (tag, t) in [
        ("empty title", {
            let mut t = task("   ", None);
            t.title = "   ".into();
            t
        }),
        ("size outside s/m/l", {
            let mut t = task("bad size", None);
            t.size = "xl".into();
            t
        }),
        ("acceptance that is not an allow-listed program", {
            let mut t = task("bad accept", None);
            t.accept = vec!["rm".into(), "-rf".into(), "/".into()];
            t
        }),
        ("a shell-quoted acceptance argument", {
            let mut t = task("quoted", None);
            t.accept.push("'test(foo)'".into());
            t
        }),
        ("dep on a task that does not exist", {
            let mut t = task("bad dep", None);
            t.deps = vec!["T-NOPE-1".into()];
            t
        }),
        ("roadmap link to a vector that does not exist", {
            let mut t = task("bad vector", None);
            t.roadmap = Some("VC-999-999".into());
            t
        }),
    ] {
        tasks::author(&root, "deepseek", &spec("justified", vec![], vec![t]))
            .expect_err(&format!("{tag} must be refused"));
        assert_eq!(open_task_files(&root), 0, "{tag} wrote nothing");
    }
}

#[test]
fn self_authored_backlog_a_refused_batch_writes_nothing() {
    let root = ws("atomic");
    let before = std::fs::read_to_string(root.join(".agents/roadmap.json")).unwrap();

    // An empty rationale is refused before anything lands.
    tasks::author(
        &root,
        "deepseek",
        &spec("   ", vec![vector("VC-900-002")], vec![task("t", None)]),
    )
    .unwrap_err();
    // One id named twice in a batch is a refused batch, not a first-write.
    tasks::author(
        &root,
        "deepseek",
        &spec(
            "justified",
            vec![vector("VC-900-003"), vector("VC-900-003")],
            vec![],
        ),
    )
    .unwrap_err();
    // A good vector beside a task whose gates fail: the batch is checked as
    // a whole, so a half-written batch is never a state.
    tasks::author(
        &root,
        "deepseek",
        &spec(
            "justified",
            vec![vector("VC-900-004")],
            vec![{
                let mut t = task("bad", None);
                t.accept = vec![];
                t
            }],
        ),
    )
    .unwrap_err();
    let after = std::fs::read_to_string(root.join(".agents/roadmap.json")).unwrap();
    assert_eq!(
        after, before,
        "a refused batch leaves the roadmap untouched"
    );
    assert_eq!(open_task_files(&root), 0, "the refused task wrote nothing");
}

#[test]
fn self_authored_backlog_review_surface_marks_authorship() {
    let root = ws("review");
    tasks::author(
        &root,
        "deepseek",
        &spec("justified", vec![], vec![task("authored", None)]),
    )
    .unwrap();
    tasks::add(&root, "deepseek", task("hand written", None)).unwrap();
    let open = tasks::list_open(&root);
    let authored: Vec<_> = open.iter().filter(|t| t.authored).collect();
    assert_eq!(authored.len(), 1);
    assert_eq!(authored[0].title, "authored");
    // Hand-written records stay byte-compatible — no `authored` key at all.
    let hand = std::fs::read_to_string(root.join(".agents/tasks/T-DEEPSEEK-2.json")).unwrap();
    assert!(!hand.contains("\"authored\""), "{hand}");
}

#[test]
fn self_authored_backlog_analysis_authors_unqueued_verification() {
    let root = ws("analysis");
    // VC-900-002 is unqueued; VC-900-003 already claims mastery; VC-900-001
    // gets queued by a hand-written task below.
    std::fs::write(
        root.join(".agents/roadmap.json"),
        r#"{
  "vectors": [
    {"depends_on": [], "id": "VC-900-001", "mastery_target": "first", "priority": "P1",
     "progress": "PARTIAL: seeded", "type": "OPERATIONS", "vector": "first capability"},
    {"depends_on": [], "id": "VC-900-002", "mastery_target": "the gap's mastery bar",
     "priority": "P1", "progress": "PARTIAL: nothing queued", "type": "OPERATIONS",
     "vector": "the gap"},
    {"depends_on": [], "id": "VC-900-003", "mastery_target": "done", "priority": "P1",
     "progress": "DELIVERED: verified", "type": "OPERATIONS", "vector": "claimed"}
  ]
}"#,
    )
    .unwrap();
    tasks::add(&root, "deepseek", task("queued work", Some("VC-900-001"))).unwrap();

    let report = tasks::author_unqueued(&root, "deepseek").unwrap();
    assert_eq!(
        report.tasks.len(),
        1,
        "only the unqueued vector is authored"
    );
    let open = tasks::list_open(&root);
    let drafted = open.iter().find(|t| t.authored).expect("drafted task");
    assert_eq!(drafted.title, "Verify mastery: the gap");
    assert_eq!(drafted.goal, "the gap's mastery bar");
    assert_eq!(drafted.roadmap.as_deref(), Some("VC-900-002"));
    assert_eq!(
        drafted.accept.cmd.last().unwrap(),
        "test(vc_900_002_mastery)"
    );
    assert!(drafted.authored);
    // The hand-written task was not re-drafted; the delivered vector was skipped.
    assert_eq!(open.len(), 2);
}

#[test]
fn self_authored_backlog_wiring_is_on_the_cli_path() {
    let src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../src/cli/tasks_cli.rs"
    ))
    .unwrap();
    for needle in [
        "TaskCommands::Author",
        "TaskCommands::Authored",
        "tasks::author(",
        "tasks::author_unqueued(",
        "unqueued: bool",
        "t.authored",
    ] {
        assert!(src.contains(needle), "tasks_cli.rs is missing `{needle}`");
    }
    let lib = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/admin/tasks.rs"))
        .unwrap();
    for needle in [
        "pub const AUTHORED_OPEN_MAX",
        "pub fn author(",
        "fn add_inner(ws: &Path, agent: &str, new: NewTask, authored: bool)",
    ] {
        assert!(lib.contains(needle), "admin/tasks.rs is missing `{needle}`");
    }
}
