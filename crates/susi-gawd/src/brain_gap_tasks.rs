//! Convert evidence-backed capability gaps into susi task records
//! (T-CODEX-30 / VC-201-014).
//!
//! A brain-audited [`Gap`](crate::brain_gap_audit::Gap) becomes a real
//! `.agents/tasks/T-*.json` record only after validation: namespaced id,
//! supported acceptance command (`cargo test` with a nonzero-name filter
//! — never arbitrary shell), resolvable dependencies without cycles, and
//! an existing roadmap vector or a schema-valid new-vector proposal.
//!
//! The model may propose tests and titles; it can never fabricate a
//! passing receipt or a `closed` field — drafts have no completion state
//! by construction and validation strips nothing into existence.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::brain_gap_audit::{Gap, GapStatus, Impact};

/// Namespaced task id: `T-<AGENT>-<n>`.
fn valid_task_id(id: &str) -> bool {
    let parts: Vec<&str> = id.split('-').collect();
    if parts.len() != 3 || parts[0] != "T" {
        return false;
    }
    let agent_ok = !parts[1].is_empty()
        && parts[1]
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit());
    agent_ok && !parts[2].is_empty() && parts[2].chars().all(|c| c.is_ascii_digit())
}

/// A proposed new roadmap vector for a genuinely uncovered capability.
/// Fields match `.agents/schemas/roadmap.schema.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorProposal {
    /// `VC-<area>-<seq>` id.
    pub id: String,
    /// Vector type (e.g. `capability`).
    #[serde(rename = "type")]
    pub vtype: String,
    /// Vector area label.
    pub vector: String,
    /// The promised behavior this vector tracks.
    pub mastery_target: String,
    /// Human-readable status — starts OPEN.
    pub progress: String,
    /// P1..P3.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
    /// Vector ids this depends on.
    #[serde(default)]
    pub depends_on: Vec<String>,
}

/// A task draft produced from a verified gap. There is deliberately no
/// `closed`/`receipt` field — the draft cannot carry fabricated
/// completion state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskDraft {
    /// `T-<AGENT>-<n>` namespaced id.
    pub id: String,
    /// Short imperative title.
    pub title: String,
    /// Scoped implementation goal citing the evidence.
    pub goal: String,
    /// `s|m|l`.
    pub size: String,
    /// Dependency task ids (must already exist, no cycles).
    pub deps: Vec<String>,
    /// Acceptance command — validated to a nonzero-test cargo invocation.
    pub accept: AcceptCmd,
    /// Roadmap vector id this task advances.
    pub roadmap: String,
    /// Minting agent identity.
    pub created_by: String,
    /// Unix creation time.
    pub created_unix: u64,
}

/// The accept block — an argv array, never a shell string.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcceptCmd {
    /// Argument vector, e.g. `["cargo","test","-p","susi-gawd","my_test","--locked"]`.
    pub cmd: Vec<String>,
}

/// Why a draft/proposal was rejected before publication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DraftReject {
    /// Id doesn't match `T-<AGENT>-<n>`.
    BadId(String),
    /// A required field is empty.
    MissingField(&'static str),
    /// Acceptance cmd isn't a `cargo test …` invocation.
    UnsupportedCommand(String),
    /// The acceptance filter would run zero tests.
    ZeroTestFilter,
    /// Dependency doesn't resolve to a known task.
    UnresolvedDep(String),
    /// Adding the edge would close a cycle.
    Cycle(String),
    /// Roadmap vector is neither known nor proposed.
    UnknownVector(String),
    /// A task with this id already exists.
    DuplicateId(String),
    /// Malformed model output.
    Malformed(String),
}

/// Build a draft from a verified gap. `accept_cmd` is the proposed
/// verification argv — validated later by [`validate_draft`]. Gaps still
/// at `Hypothesis` cannot mint tasks (no receipts → nothing actionable).
pub fn draft_from_gap(
    gap: &Gap,
    seq: u32,
    agent: &str,
    roadmap: &str,
    accept_cmd: Vec<String>,
    deps: Vec<String>,
    now: u64,
) -> Result<TaskDraft, DraftReject> {
    if gap.status == GapStatus::Hypothesis {
        return Err(DraftReject::Malformed(
            "hypothesis gaps have no receipts; promote evidence first".into(),
        ));
    }
    let size = match gap.impact {
        Impact::High => "l",
        Impact::Medium => "m",
        Impact::Low => "s",
    }
    .to_string();
    let goal = format!(
        "Close verified gap {} ({}): expected `{}` but observed `{}`. Trigger: {}. Evidence: {}.",
        gap.id,
        gap.title,
        gap.expected,
        gap.observed,
        gap.trigger.as_deref().unwrap_or("not yet isolated"),
        gap.receipts.join(", "),
    );
    Ok(TaskDraft {
        id: format!("T-{agent}-{seq}"),
        title: format!("Close gap: {}", gap.title),
        goal,
        size,
        deps,
        accept: AcceptCmd { cmd: accept_cmd },
        roadmap: roadmap.to_string(),
        created_by: agent.to_string(),
        created_unix: now,
    })
}

/// The acceptance command must be `cargo test` with `-p <crate>`, a
/// nonempty test-name filter and `--locked` — the argv form guarantees
/// no shell interpretation. `cargo test` alone would run everything
/// (zero specificity) so a name filter is required.
fn validate_accept(cmd: &[String]) -> Result<(), DraftReject> {
    let joined = cmd.join(" ");
    if cmd.len() < 2 || cmd[0] != "cargo" || cmd[1] != "test" {
        return Err(DraftReject::UnsupportedCommand(joined));
    }
    let has_p = cmd.iter().any(|a| a == "-p" || a.starts_with("-p="));
    let has_locked = cmd.iter().any(|a| a == "--locked");
    // The name filter: first positional arg that isn't a flag or a
    // value of `-p`.
    let mut positional = Vec::new();
    let mut skip_next = false;
    for a in cmd.iter().skip(2) {
        if skip_next {
            skip_next = false;
            continue;
        }
        if a == "-p" || a == "--package" {
            skip_next = true;
            continue;
        }
        if a.starts_with('-') {
            continue;
        }
        positional.push(a.clone());
    }
    if !has_p || !has_locked {
        return Err(DraftReject::UnsupportedCommand(joined));
    }
    if positional.is_empty() {
        return Err(DraftReject::ZeroTestFilter);
    }
    Ok(())
}

/// Depth-first cycle check over the task graph including the draft edge.
fn closes_cycle(draft_id: &str, graph: &BTreeMap<String, Vec<String>>) -> Option<String> {
    let mut stack: Vec<String> = graph.get(draft_id).cloned().unwrap_or_default();
    let mut seen = BTreeSet::new();
    while let Some(n) = stack.pop() {
        if n == draft_id {
            return Some(n);
        }
        if seen.insert(n.clone()) {
            stack.extend(graph.get(&n).cloned().unwrap_or_default());
        }
    }
    None
}

/// Validate a draft against the live task set and roadmap vectors.
/// `task_graph` maps task id → its declared deps (from published files).
pub fn validate_draft(
    draft: &TaskDraft,
    known_ids: &BTreeSet<String>,
    task_graph: &BTreeMap<String, Vec<String>>,
    known_vectors: &BTreeSet<String>,
    proposed_vectors: &BTreeSet<String>,
) -> Result<(), DraftReject> {
    if !valid_task_id(&draft.id) {
        return Err(DraftReject::BadId(draft.id.clone()));
    }
    if known_ids.contains(&draft.id) {
        return Err(DraftReject::DuplicateId(draft.id.clone()));
    }
    if draft.title.trim().is_empty() {
        return Err(DraftReject::MissingField("title"));
    }
    if draft.goal.trim().is_empty() {
        return Err(DraftReject::MissingField("goal"));
    }
    if !matches!(draft.size.as_str(), "s" | "m" | "l") {
        return Err(DraftReject::MissingField("size"));
    }
    validate_accept(&draft.accept.cmd)?;
    for d in &draft.deps {
        if !known_ids.contains(d) {
            return Err(DraftReject::UnresolvedDep(d.clone()));
        }
    }
    // Cycle check including the draft's own edges.
    let mut g = task_graph.clone();
    g.insert(draft.id.clone(), draft.deps.clone());
    if let Some(c) = closes_cycle(&draft.id, &g) {
        return Err(DraftReject::Cycle(c));
    }
    if !known_vectors.contains(&draft.roadmap) && !proposed_vectors.contains(&draft.roadmap) {
        return Err(DraftReject::UnknownVector(draft.roadmap.clone()));
    }
    Ok(())
}

/// Publish one task record — one file, atomic write (tmp + rename),
/// never touching other records.
pub fn publish(tasks_dir: &Path, draft: &TaskDraft) -> io::Result<PathBuf> {
    fs::create_dir_all(tasks_dir)?;
    let body = serde_json::to_string_pretty(draft).map_err(io::Error::other)?;
    let path = tasks_dir.join(format!("{}.json", draft.id));
    let tmp = tasks_dir.join(format!(".{}.json.tmp", draft.id));
    fs::write(&tmp, body)?;
    fs::rename(&tmp, &path)?;
    Ok(path)
}

/// Propose a schema-valid new vector for a gap no existing vector covers.
#[must_use]
pub fn propose_vector(gap: &Gap, vector_area: &str, seq: u32) -> VectorProposal {
    VectorProposal {
        id: format!("VC-{vector_area}-{seq:03}"),
        vtype: "capability".into(),
        vector: vector_area.to_string(),
        mastery_target: format!("{}: expected `{}`", gap.title, gap.expected),
        progress: format!(
            "OPEN: discovered by brain gap audit ({}), evidence {}",
            gap.id,
            gap.receipts.join(", ")
        ),
        priority: Some(
            match gap.impact {
                Impact::High => "P1",
                Impact::Medium => "P2",
                Impact::Low => "P3",
            }
            .into(),
        ),
        depends_on: Vec::new(),
    }
}

/// Parse the model's proposed tasks (a JSON array of `{title, accept_cmd,
/// deps, roadmap}` objects). Malformed output fails — nothing partial is
/// published.
#[derive(Debug, Deserialize)]
struct ProposedTask {
    title: String,
    #[serde(default)]
    goal: Option<String>,
    #[serde(default)]
    size: Option<String>,
    #[serde(default)]
    deps: Vec<String>,
    accept_cmd: Vec<String>,
    roadmap: String,
}

/// Convert parsed proposals into drafts at sequence `seq` onward.
pub fn parse_proposals(
    body: &str,
    seq0: u32,
    agent: &str,
    now: u64,
) -> Result<Vec<TaskDraft>, DraftReject> {
    let raw: Vec<ProposedTask> = serde_json::from_str(body)
        .map_err(|e| DraftReject::Malformed(format!("not a task array: {e}")))?;
    let mut out = Vec::with_capacity(raw.len());
    for (i, p) in raw.into_iter().enumerate() {
        out.push(TaskDraft {
            id: format!("T-{agent}-{}", seq0 + i as u32),
            title: p.title,
            goal: p.goal.unwrap_or_default(),
            size: p.size.unwrap_or_else(|| "m".into()),
            deps: p.deps,
            accept: AcceptCmd { cmd: p.accept_cmd },
            roadmap: p.roadmap,
            created_by: agent.into(),
            created_unix: now,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: u64 = 1_700_000_000;

    fn gap() -> Gap {
        Gap {
            id: "GAP-retry".into(),
            title: "retry path never engaged".into(),
            receipts: vec!["test_retry".into()],
            trigger: Some("send two requests".into()),
            expected: "retries recover".into(),
            observed: "no retry attempted".into(),
            hidden_behind: None,
            status: GapStatus::Verified,
            impact: Impact::High,
            confidence: 0.9,
            observed_at: T0,
        }
    }

    fn accept() -> Vec<String> {
        vec![
            "cargo".into(),
            "test".into(),
            "-p".into(),
            "susi-gawd".into(),
            "retry_engages".into(),
            "--locked".into(),
        ]
    }

    fn sets() -> (
        BTreeSet<String>,
        BTreeMap<String, Vec<String>>,
        BTreeSet<String>,
    ) {
        let ids: BTreeSet<String> = ["T-CODEX-1", "T-CODEX-2"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let graph: BTreeMap<String, Vec<String>> =
            [("T-CODEX-2".to_string(), vec!["T-CODEX-1".to_string()])]
                .into_iter()
                .collect();
        let vectors: BTreeSet<String> = ["VC-201-014"].iter().map(|s| s.to_string()).collect();
        (ids, graph, vectors)
    }

    /// A verified gap mints a valid task record on disk — namespaced id,
    /// evidence-citing goal, supported nonzero-test acceptance command.
    #[test]
    fn brain_gap_tasks_gap_becomes_published_task() {
        let dir = std::env::temp_dir().join(format!("gt-1-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let (ids, graph, vectors) = sets();
        let d = draft_from_gap(
            &gap(),
            40,
            "DEVIN",
            "VC-201-014",
            accept(),
            vec!["T-CODEX-1".into()],
            T0,
        )
        .unwrap();
        validate_draft(&d, &ids, &graph, &vectors, &BTreeSet::new()).unwrap();
        let p = publish(&dir, &d).unwrap();
        let back: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(back["id"], "T-DEVIN-40");
        assert_eq!(back["roadmap"], "VC-201-014");
        assert_eq!(back["accept"]["cmd"][3], "susi-gawd");
        assert_eq!(back["accept"]["cmd"][4], "retry_engages");
        assert!(
            back.get("closed").is_none(),
            "drafts never carry completion"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// Arbitrary shell and non-cargo commands are refused; `cargo test`
    /// without a filter is refused (would not be a nonzero-test target).
    #[test]
    fn brain_gap_tasks_unsupported_commands_rejected() {
        let (ids, graph, vectors) = sets();
        let g = gap();
        for bad in [
            vec!["sh".into(), "-c".into(), "true".into()],
            vec!["cargo".into(), "build".into()],
            vec![
                "cargo".into(),
                "test".into(),
                "-p".into(),
                "susi-gawd".into(),
                "--locked".into(),
            ],
        ] {
            let d = draft_from_gap(&g, 41, "DEVIN", "VC-201-014", bad, vec![], T0).unwrap();
            assert!(
                validate_draft(&d, &ids, &graph, &vectors, &BTreeSet::new()).is_err(),
                "{:?} must reject",
                d.accept.cmd
            );
        }
    }

    /// Hypothesis gaps cannot mint tasks — model claims without receipts
    /// produce nothing actionable.
    #[test]
    fn brain_gap_tasks_hypothesis_cannot_publish() {
        let mut g = gap();
        g.status = GapStatus::Hypothesis;
        assert!(matches!(
            draft_from_gap(&g, 42, "DEVIN", "VC-201-014", accept(), vec![], T0),
            Err(DraftReject::Malformed(_))
        ));
    }

    /// Unresolved deps and dependency cycles are rejected before publish.
    #[test]
    fn brain_gap_tasks_deps_and_cycles_rejected() {
        let (ids, mut graph, vectors) = sets();
        let g = gap();
        // Missing dep.
        let d = draft_from_gap(
            &g,
            43,
            "DEVIN",
            "VC-201-014",
            accept(),
            vec!["T-GHOST-9".into()],
            T0,
        )
        .unwrap();
        assert!(matches!(
            validate_draft(&d, &ids, &graph, &vectors, &BTreeSet::new()),
            Err(DraftReject::UnresolvedDep(_))
        ));
        // Cycle: T-DEVIN-44 deps T-CODEX-2 while T-CODEX-2 deps T-DEVIN-44.
        graph.insert("T-CODEX-2".into(), vec!["T-DEVIN-44".into()]);
        let d2 = draft_from_gap(
            &g,
            44,
            "DEVIN",
            "VC-201-014",
            accept(),
            vec!["T-CODEX-2".into()],
            T0,
        )
        .unwrap();
        assert!(matches!(
            validate_draft(&d2, &ids, &graph, &vectors, &BTreeSet::new()),
            Err(DraftReject::Cycle(_))
        ));
    }

    /// Malformed model output never reaches the queue.
    #[test]
    fn brain_gap_tasks_malformed_output_rejected() {
        assert!(matches!(
            parse_proposals("not json at all", 50, "DEVIN", T0),
            Err(DraftReject::Malformed(_))
        ));
        // Valid JSON, wrong shape (object not array).
        assert!(matches!(
            parse_proposals("{\"title\":\"x\"}", 50, "DEVIN", T0),
            Err(DraftReject::Malformed(_))
        ));
    }

    /// A genuinely uncovered capability produces a schema-valid vector
    /// proposal the task can link to.
    #[test]
    fn brain_gap_tasks_new_vector_proposal_is_valid() {
        let v = propose_vector(&gap(), "202", 7);
        assert!(v.id.starts_with("VC-"));
        assert_eq!(v.vtype, "capability");
        assert!(!v.mastery_target.is_empty());
        assert!(v.progress.starts_with("OPEN"));
        let json = serde_json::to_value(&v).unwrap();
        for k in ["id", "type", "vector", "mastery_target", "progress"] {
            assert!(json.get(k).is_some(), "missing {k}");
        }
        // Task may link to the proposed vector once registered.
        let (ids, graph, vectors) = sets();
        let proposed: BTreeSet<String> = [v.id.clone()].into_iter().collect();
        let d = draft_from_gap(&gap(), 45, "DEVIN", &v.id, accept(), vec![], T0).unwrap();
        validate_draft(&d, &ids, &graph, &vectors, &proposed).unwrap();
        // Without the proposal registered, the same draft must reject.
        let d2 = draft_from_gap(&gap(), 46, "DEVIN", "VC-999-999", accept(), vec![], T0).unwrap();
        assert!(matches!(
            validate_draft(&d2, &ids, &graph, &vectors, &BTreeSet::new()),
            Err(DraftReject::UnknownVector(_))
        ));
    }

    /// Publishing preserves every existing task file — union semantics.
    #[test]
    fn brain_gap_tasks_existing_records_preserved() {
        let dir = std::env::temp_dir().join(format!("gt-7-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let existing = dir.join("T-OTHER-1.json");
        fs::write(&existing, "{\"id\":\"T-OTHER-1\"}").unwrap();
        let d = draft_from_gap(&gap(), 47, "DEVIN", "VC-201-014", accept(), vec![], T0).unwrap();
        publish(&dir, &d).unwrap();
        assert_eq!(
            fs::read_to_string(&existing).unwrap(),
            "{\"id\":\"T-OTHER-1\"}"
        );
        assert!(dir.join("T-DEVIN-47.json").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    /// Well-formed brain proposals parse into drafts; dup ids rejected.
    #[test]
    fn brain_gap_tasks_proposals_parse_and_dedupe() {
        let body = r#"[{
            "title":"Retry the failed request once",
            "goal":"wire retry policy",
            "size":"m",
            "deps":["T-CODEX-1"],
            "accept_cmd":["cargo","test","-p","susi-gawd","retry","--locked"],
            "roadmap":"VC-201-014"
        }]"#;
        let ds = parse_proposals(body, 60, "DEVIN", T0).unwrap();
        assert_eq!(ds.len(), 1);
        assert_eq!(ds[0].id, "T-DEVIN-60");
        let (ids, graph, vectors) = sets();
        validate_draft(&ds[0], &ids, &graph, &vectors, &BTreeSet::new()).unwrap();
        // Same id again → duplicate.
        let mut ids2 = ids.clone();
        ids2.insert("T-DEVIN-60".into());
        assert!(matches!(
            validate_draft(&ds[0], &ids2, &graph, &vectors, &BTreeSet::new()),
            Err(DraftReject::DuplicateId(_))
        ));
    }
}
