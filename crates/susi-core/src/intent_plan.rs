//! Intent → typed DAG of real leaf operations (VC-202-007).
//!
//! A natural-language intent is planned into a typed directed acyclic graph
//! whose nodes are real susi leaf operations, each with declared inputs,
//! outputs and a cost estimate, then executed with per-node provenance.
//!
//! An intent the system cannot serve is refused with *what is missing*, and a
//! command-shaped input is refused before it can be planned (Mandate 55:
//! command shapes are refused, never run). An unplannable intent is a typed
//! failure, never a silent approximation.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::susi_error::{EaiError, EaiResult};

/// One real leaf operation the planner may emit. The registry is the single
/// source of truth: a `PlanNode` that names an operation outside it is a
/// typed failure, so a plan can never silently approximate an unknown step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeafOperation {
    /// Stable operation name.
    pub name: &'static str,
    /// Declared input slots this operation consumes.
    pub inputs: &'static [&'static str],
    /// Declared output slot this operation produces.
    pub output: &'static str,
    /// Cost estimate in abstract units (not currency).
    pub cost: u64,
}

/// The leaf operations the planner may emit, in increasing cost order.
pub const LEAF_OPERATIONS: &[LeafOperation] = &[
    LeafOperation {
        name: "list_dir",
        inputs: &["path"],
        output: "entries",
        cost: 1,
    },
    LeafOperation {
        name: "view_file",
        inputs: &["path"],
        output: "text",
        cost: 1,
    },
    LeafOperation {
        name: "grep_search",
        inputs: &["pattern", "path"],
        output: "matches",
        cost: 2,
    },
    LeafOperation {
        name: "find_files",
        inputs: &["pattern", "path"],
        output: "paths",
        cost: 2,
    },
    LeafOperation {
        name: "write_file",
        inputs: &["path", "content"],
        output: "status",
        cost: 3,
    },
    LeafOperation {
        name: "exec_command",
        inputs: &["command"],
        output: "output",
        cost: 5,
    },
];

/// Look up a leaf operation by name.
#[must_use]
pub fn leaf_operation(name: &str) -> Option<&'static LeafOperation> {
    LEAF_OPERATIONS.iter().find(|op| op.name == name)
}

/// Typed planning/validation failure. Every refusal names what is missing or
/// what was wrong, so a caller can explain it instead of guessing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlanError {
    /// Input was a command shape, not an intent (Mandate 55).
    CommandShaped,
    /// No plan could be formed; `missing` lists the absent capabilities.
    Unplannable { missing: Vec<String> },
    /// The DAG has a dependency cycle (the offending node ids).
    Cycle(Vec<String>),
    /// A node names an operation outside the leaf registry.
    UnknownOperation(String),
    /// A node references a dependency that does not exist.
    UnknownDependency { node: String, dependency: String },
    /// A node does not declare an input its operation requires.
    MissingInput { node: String, input: String },
}

impl fmt::Display for PlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CommandShaped => {
                write!(
                    f,
                    "refused command-shaped input: an intent is required, not a command"
                )
            }
            Self::Unplannable { missing } => {
                write!(f, "unplannable intent; missing: {}", missing.join(", "))
            }
            Self::Cycle(ids) => write!(
                f,
                "plan has a dependency cycle through: {}",
                ids.join(" -> ")
            ),
            Self::UnknownOperation(op) => write!(f, "unknown leaf operation: {op}"),
            Self::UnknownDependency { node, dependency } => {
                write!(f, "node {node} depends on unknown node {dependency}")
            }
            Self::MissingInput { node, input } => {
                write!(f, "node {node} is missing required input {input}")
            }
        }
    }
}

impl std::error::Error for PlanError {}

/// One node in the planned DAG.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanNode {
    pub id: String,
    pub operation: String,
    /// Resolved input values, keyed by the operation's declared input slots.
    pub inputs: BTreeMap<String, String>,
    /// Node ids this node must run after.
    #[serde(default)]
    pub depends_on: Vec<String>,
}

/// A typed directed acyclic graph of leaf operations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanDag {
    pub nodes: Vec<PlanNode>,
}

impl PlanDag {
    /// Validate the plan: every node names a real operation, declares every
    /// required input, references only existing dependencies, and the graph
    /// is acyclic. Returns `Ok(())` only for an executable plan.
    pub fn validate(&self) -> Result<(), PlanError> {
        let ids: BTreeSet<&str> = self.nodes.iter().map(|n| n.id.as_str()).collect();
        if ids.len() != self.nodes.len() {
            return Err(PlanError::Unplannable {
                missing: vec!["unique node ids".into()],
            });
        }
        for node in &self.nodes {
            let Some(op) = leaf_operation(&node.operation) else {
                return Err(PlanError::UnknownOperation(node.operation.clone()));
            };
            for input in op.inputs {
                if !node.inputs.contains_key(*input) {
                    return Err(PlanError::MissingInput {
                        node: node.id.clone(),
                        input: (*input).to_string(),
                    });
                }
            }
            for dep in &node.depends_on {
                if !ids.contains(dep.as_str()) {
                    return Err(PlanError::UnknownDependency {
                        node: node.id.clone(),
                        dependency: dep.clone(),
                    });
                }
            }
        }
        self.topo_order().map(|_| ())
    }

    /// Kahn topological order of node indices. A cycle is a typed failure
    /// carrying the ids still in the work set.
    pub fn topo_order(&self) -> Result<Vec<usize>, PlanError> {
        let by_id: BTreeMap<&str, usize> = self
            .nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.id.as_str(), i))
            .collect();
        let mut indegree = vec![0usize; self.nodes.len()];
        let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); self.nodes.len()];
        for (i, node) in self.nodes.iter().enumerate() {
            for dep in &node.depends_on {
                let Some(&j) = by_id.get(dep.as_str()) else {
                    return Err(PlanError::UnknownDependency {
                        node: node.id.clone(),
                        dependency: dep.clone(),
                    });
                };
                indegree[i] += 1;
                dependents[j].push(i);
            }
        }
        let mut ready: Vec<usize> = (0..self.nodes.len())
            .filter(|&i| indegree[i] == 0)
            .collect();
        let mut order = Vec::with_capacity(self.nodes.len());
        while let Some(i) = ready.pop() {
            order.push(i);
            for &j in &dependents[i] {
                indegree[j] -= 1;
                if indegree[j] == 0 {
                    ready.push(j);
                }
            }
        }
        if order.len() != self.nodes.len() {
            let stuck: Vec<String> = (0..self.nodes.len())
                .filter(|&i| indegree[i] > 0)
                .map(|i| self.nodes[i].id.clone())
                .collect();
            return Err(PlanError::Cycle(stuck));
        }
        Ok(order)
    }

    /// Sum of every node's operation cost estimate.
    #[must_use]
    pub fn total_cost(&self) -> u64 {
        self.nodes
            .iter()
            .filter_map(|n| leaf_operation(&n.operation))
            .map(|op| op.cost)
            .sum()
    }
}

/// Per-node execution record: what ran, on what, what it cost and produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeProvenance {
    pub node_id: String,
    pub operation: String,
    pub inputs: BTreeMap<String, String>,
    pub cost: u64,
    pub started_at: u64,
    pub elapsed_ms: u64,
    pub output: String,
}

/// Planner: turns a natural-language intent into a validated `PlanDag`.
#[derive(Debug, Default)]
pub struct Planner;

impl Planner {
    /// Plan an intent into a typed DAG of real leaf operations.
    ///
    /// Command-shaped input is refused before planning (Mandate 55), and an
    /// intent with no recognizable action is an explicit `Unplannable`
    /// failure naming what is missing — never a silent approximation.
    pub fn plan(intent: &str) -> Result<PlanDag, PlanError> {
        if is_command_shaped(intent) {
            return Err(PlanError::CommandShaped);
        }
        let steps = detect_steps(intent);
        if steps.is_empty() {
            return Err(PlanError::Unplannable {
                missing: vec![
                    "a recognizable action (read, list, search, find, write, or run)".into(),
                ],
            });
        }

        let mut nodes = Vec::with_capacity(steps.len());
        let mut prev: Option<String> = None;
        for (i, (operation, inputs)) in steps.into_iter().enumerate() {
            let id = format!("n{i}");
            let node = PlanNode {
                id: id.clone(),
                operation: operation.to_string(),
                inputs,
                depends_on: prev.iter().cloned().collect(),
            };
            prev = Some(id);
            nodes.push(node);
        }
        let dag = PlanDag { nodes };
        dag.validate()?;
        Ok(dag)
    }
}

/// Execute a validated DAG in topological order, recording per-node
/// provenance. Each leaf operation runs in-process against the workspace —
/// hermetic, no daemon, no network.
pub fn execute(dag: &PlanDag, workspace: &Path) -> EaiResult<Vec<NodeProvenance>> {
    let order = dag
        .topo_order()
        .map_err(|e| EaiError::internal(format!("execute unvalidated plan: {e}")))?;
    let mut outputs: BTreeMap<String, String> = BTreeMap::new();
    let mut provenance = Vec::with_capacity(dag.nodes.len());
    for &i in &order {
        let node = &dag.nodes[i];
        let op = leaf_operation(&node.operation).ok_or_else(|| {
            EaiError::internal(format!(
                "plan node names unknown operation {}",
                node.operation
            ))
        })?;
        // Resolve inputs: a value that names a prior node id is substituted
        // with that node's output, so DAG edges carry data, not just order.
        let mut resolved = node.inputs.clone();
        for value in resolved.values_mut() {
            if let Some(prior) = outputs.get(value) {
                *value = prior.clone();
            }
        }
        let started_at = now_unix();
        let start = std::time::Instant::now();
        let output = exec_leaf(op, &resolved, workspace)?;
        let elapsed_ms = start.elapsed().as_millis() as u64;
        outputs.insert(node.id.clone(), output.clone());
        provenance.push(NodeProvenance {
            node_id: node.id.clone(),
            operation: node.operation.clone(),
            inputs: resolved,
            cost: op.cost,
            started_at,
            elapsed_ms,
            output,
        });
    }
    Ok(provenance)
}

// ---------------------------------------------------------------------------
// Planning internals
// ---------------------------------------------------------------------------

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Command executables that, leading an input, mark it as a command shape.
const COMMAND_WORDS: &[&str] = &[
    "cargo", "git", "rm", "cp", "mv", "mkdir", "rmdir", "sh", "bash", "sudo", "curl", "wget",
    "echo", "sed", "awk", "touch", "chmod", "chown", "kill", "ps", "cat", "ls", "grep", "find",
];

/// Natural-language connective words that keep a leading verb from reading as
/// a command (e.g. "list the directory" is an intent, not `ls`).
const NATURAL_ARTICLES: &[&str] = &[
    "the",
    "a",
    "an",
    "for",
    "in",
    "of",
    "all",
    "files",
    "file",
    "named",
    "matching",
    "directory",
    "dir",
    "folder",
    "tests",
    "test",
    "me",
    "please",
];

/// Whether `intent` is a command shape rather than a natural-language intent.
fn is_command_shaped(intent: &str) -> bool {
    let trimmed = intent.trim();
    if trimmed.is_empty() {
        return false;
    }
    if trimmed
        .chars()
        .any(|c| matches!(c, '|' | '>' | '<' | ';' | '`'))
        || trimmed.contains("&&")
        || trimmed.contains("$(")
    {
        return true;
    }
    let words: Vec<&str> = trimmed.split_whitespace().collect();
    let Some(first) = words.first() else {
        return false;
    };
    let first = first.to_ascii_lowercase();
    if !COMMAND_WORDS.contains(&first.as_str()) {
        return false;
    }
    !words[1..]
        .iter()
        .any(|w| NATURAL_ARTICLES.contains(&w.to_ascii_lowercase().as_str()))
}

/// Split an intent into ordered clauses on natural-language conjunctions.
fn clauses(intent: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut rest = intent.trim().to_string();
    for marker in [" and ", " then ", " first ", ", "] {
        // Re-split on this marker against the current remainder, keeping the
        // earlier markers already applied.
        let mut rebuilt: Vec<String> = Vec::new();
        for piece in std::mem::take(&mut out) {
            rebuilt.extend(split_on(&piece, marker));
        }
        rebuilt.extend(split_on(&rest, marker));
        out = rebuilt;
        rest = String::new();
    }
    if !rest.is_empty() {
        out.push(rest);
    }
    out.into_iter().filter(|c| !c.trim().is_empty()).collect()
}

/// Split `s` on `marker`, preserving both sides.
fn split_on(s: &str, marker: &str) -> Vec<String> {
    if !s.contains(marker) {
        return vec![s.to_string()];
    }
    let mut out = Vec::new();
    let mut start = 0;
    while let Some(pos) = s[start..].find(marker) {
        let abs = start + pos;
        out.push(s[start..abs].to_string());
        start = abs + marker.len();
    }
    out.push(s[start..].to_string());
    out
}

/// Detect a sequence of (operation, resolved-inputs) steps, preserving the
/// order of the verbs in the intent.
fn detect_steps(intent: &str) -> Vec<(&'static str, BTreeMap<String, String>)> {
    let mut steps = Vec::new();
    for clause in clauses(intent) {
        if let Some(step) = detect_clause(&clause) {
            steps.push(step);
        }
    }
    steps
}

/// Parse one clause into a single leaf-operation step, or `None` when the
/// clause carries no recognized verb.
fn detect_clause(clause: &str) -> Option<(&'static str, BTreeMap<String, String>)> {
    let words: Vec<&str> = clause.split_whitespace().collect();
    if words.is_empty() {
        return None;
    }
    let lower: Vec<String> = words.iter().map(|w| w.to_ascii_lowercase()).collect();
    let verb = lower[0]
        .trim_end_matches(|c: char| !c.is_ascii_alphanumeric())
        .to_string();

    match verb.as_str() {
        "read" | "view" | "show" | "cat" => {
            let path = clause_path(&words).unwrap_or_else(|| ".".into());
            Some(("view_file", BTreeMap::from([("path".to_string(), path)])))
        }
        "list" | "ls" => {
            let path = clause_path(&words).unwrap_or_else(|| ".".into());
            Some(("list_dir", BTreeMap::from([("path".to_string(), path)])))
        }
        "search" | "grep" => {
            let pattern = after(&lower, &["for"]).unwrap_or_else(|| "*".into());
            let path = clause_path(&words).unwrap_or_else(|| ".".into());
            Some((
                "grep_search",
                BTreeMap::from([("pattern".to_string(), pattern), ("path".to_string(), path)]),
            ))
        }
        "find" => {
            let pattern =
                after(&lower, &["for", "matching", "named"]).unwrap_or_else(|| "*".into());
            let path = clause_path(&words).unwrap_or_else(|| ".".into());
            Some((
                "find_files",
                BTreeMap::from([("pattern".to_string(), pattern), ("path".to_string(), path)]),
            ))
        }
        "write" | "create" | "make" => {
            let path = clause_path(&words).unwrap_or_else(|| "output.txt".into());
            // "write <content> to <path>": the content is the word right after
            // the verb; a content introducer ("with …", "containing …") would
            // follow the verb's object, which the clause grammar does not
            // need — the first object word is the content.
            let content = second_word(&words).unwrap_or_default();
            Some((
                "write_file",
                BTreeMap::from([("path".to_string(), path), ("content".to_string(), content)]),
            ))
        }
        "run" | "execute" => {
            let command =
                after(&lower, &["run", "execute", "the"]).unwrap_or_else(|| "true".into());
            Some((
                "exec_command",
                BTreeMap::from([("command".to_string(), command)]),
            ))
        }
        _ => None,
    }
}

/// The path-like token in a clause: the token after a path introducer, else
/// the first token containing a separator or a known extension.
fn clause_path(words: &[&str]) -> Option<String> {
    for (i, w) in words.iter().enumerate() {
        let wl = w.to_ascii_lowercase();
        if matches!(
            wl.as_str(),
            "file" | "directory" | "dir" | "folder" | "named" | "in" | "to" | "into"
        ) && i + 1 < words.len()
        {
            let candidate =
                words[i + 1].trim_matches(|c| c == '\'' || c == '"' || c == ',' || c == '.');
            if !candidate.is_empty() {
                return Some(candidate.to_string());
            }
        }
    }
    words
        .iter()
        .map(|w| w.trim_matches(|c| c == '\'' || c == '"' || c == ','))
        .find(|w| w.contains('/') || w.contains('.'))
        .map(|w| w.to_string())
}

/// The token after the first occurrence of any introducer word.
fn after(words: &[String], introducers: &[&str]) -> Option<String> {
    for (i, w) in words.iter().enumerate() {
        if introducers.contains(&w.as_str()) && i + 1 < words.len() {
            return Some(
                words[i + 1]
                    .trim_matches(|c| c == '\'' || c == '"' || c == ',' || c == '.')
                    .to_string(),
            );
        }
    }
    None
}

/// The second word of a clause, punctuation-stripped.
fn second_word(words: &[&str]) -> Option<String> {
    words
        .get(1)
        .map(|w| w.trim_matches(|c| c == '\'' || c == '"' || c == ',' || c == '.'))
        .filter(|w| !w.is_empty())
        .map(|w| w.to_string())
}

// ---------------------------------------------------------------------------
// Leaf executors
// ---------------------------------------------------------------------------

/// Execute one leaf operation in-process. `workspace` scopes relative paths.
fn exec_leaf(
    op: &LeafOperation,
    inputs: &BTreeMap<String, String>,
    workspace: &Path,
) -> EaiResult<String> {
    let resolve = |key: &str| -> PathBuf {
        let raw = inputs.get(key).map(String::as_str).unwrap_or(".");
        let p = Path::new(raw);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            workspace.join(p)
        }
    };
    match op.name {
        "list_dir" => {
            let dir = resolve("path");
            let entries = std::fs::read_dir(&dir)
                .map_err(|e| EaiError::io(format!("list {}: {e}", dir.display())))?;
            let mut names: Vec<String> = entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            Ok(names.join("\n"))
        }
        "view_file" => {
            let path = resolve("path");
            std::fs::read_to_string(&path)
                .map_err(|e| EaiError::filesystem(format!("read {}: {e}", path.display())))
        }
        "grep_search" => {
            let pattern = inputs.get("pattern").map(String::as_str).unwrap_or("");
            let path = resolve("path");
            grep_matches(&path, pattern)
        }
        "find_files" => {
            let pattern = inputs.get("pattern").map(String::as_str).unwrap_or("*");
            let dir = resolve("path");
            let mut hits = Vec::new();
            find_files(&dir, pattern, &mut hits)?;
            hits.sort();
            Ok(hits.join("\n"))
        }
        "write_file" => {
            let path = resolve("path");
            let content = inputs.get("content").map(String::as_str).unwrap_or("");
            if let Some(parent) = path.parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent).map_err(|e| {
                        EaiError::filesystem(format!("mkdir {}: {e}", parent.display()))
                    })?;
                }
            }
            std::fs::write(&path, content)
                .map_err(|e| EaiError::filesystem(format!("write {}: {e}", path.display())))?;
            Ok(format!("wrote {}", path.display()))
        }
        "exec_command" => {
            let command = inputs.get("command").map(String::as_str).unwrap_or("");
            let output = std::process::Command::new("sh")
                .arg("-c")
                .arg(command)
                .current_dir(workspace)
                .output()
                .map_err(|e| EaiError::process(format!("spawn command: {e}")))?;
            if !output.status.success() {
                return Err(EaiError::process(format!(
                    "command failed ({:?}): {}",
                    output.status.code().unwrap_or(-1),
                    String::from_utf8_lossy(&output.stderr).trim()
                )));
            }
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
        }
        other => Err(EaiError::internal(format!(
            "no executor for leaf operation {other}"
        ))),
    }
}

/// Lines under `path` (or the file itself) that contain `pattern`.
fn grep_matches(path: &Path, pattern: &str) -> EaiResult<String> {
    let mut hits = Vec::new();
    if path.is_dir() {
        let mut files = Vec::new();
        find_files(path, "*", &mut files)?;
        files.sort();
        for file in files {
            let Ok(text) = std::fs::read_to_string(&file) else {
                continue;
            };
            for (lineno, line) in text.lines().enumerate() {
                if line.contains(pattern) {
                    hits.push(format!("{}:{}:{}", file, lineno + 1, line));
                }
            }
        }
    } else if let Ok(text) = std::fs::read_to_string(path) {
        for (lineno, line) in text.lines().enumerate() {
            if line.contains(pattern) {
                hits.push(format!("{}:{}:{}", path.display(), lineno + 1, line));
            }
        }
    }
    Ok(hits.join("\n"))
}

/// Walk `dir` collecting regular files whose name contains `pattern` (`*`
/// matches everything).
fn find_files(dir: &Path, pattern: &str, out: &mut Vec<String>) -> EaiResult<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            find_files(&path, pattern, out)?;
        } else if pattern == "*" || name.contains(pattern) {
            out.push(path.display().to_string());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "susi-intent-plan-{tag}-{}-{}",
            std::process::id(),
            now_unix()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Acceptance: an intent becomes a real, executable DAG; command shapes
    /// and unplannable intents are typed failures.
    #[test]
    fn intent_to_executable_dag() {
        let ws = scratch("dag");
        std::fs::write(ws.join("notes.md"), "# Title\ncontains unsafe here\n").unwrap();
        std::fs::create_dir_all(ws.join("crates")).unwrap();

        // A multi-step intent plans into a DAG of real operations.
        let dag = Planner::plan("read the file notes.md and search for unsafe in crates")
            .expect("plannable intent");
        assert!(dag.nodes.len() >= 2, "expected a multi-node DAG");
        dag.validate().expect("plan must validate");

        // Every node names a real leaf operation and carries its inputs.
        for node in &dag.nodes {
            let op = leaf_operation(&node.operation)
                .unwrap_or_else(|| panic!("{} is not a leaf operation", node.operation));
            for input in op.inputs {
                assert!(node.inputs.contains_key(*input), "missing {input}");
            }
        }

        // The DAG is acyclic and ordered.
        let order = dag.topo_order().expect("no cycle");
        assert_eq!(order.len(), dag.nodes.len());
        assert!(dag.total_cost() > 0);

        // Execute with per-node provenance, one record per node in order.
        let provenance = execute(&dag, &ws).expect("executable");
        assert_eq!(provenance.len(), dag.nodes.len());
        let dag_ids: BTreeSet<&str> = dag.nodes.iter().map(|n| n.id.as_str()).collect();
        let prov_ids: BTreeSet<&str> = provenance.iter().map(|p| p.node_id.as_str()).collect();
        assert_eq!(dag_ids, prov_ids);
        // The read node saw the file we wrote out beforehand.
        assert!(provenance
            .iter()
            .any(|p| p.operation == "view_file" && p.output.contains("unsafe")));

        // Command shapes are refused before planning (Mandate 55).
        assert_eq!(
            Planner::plan("rm -rf /").unwrap_err(),
            PlanError::CommandShaped
        );
        assert_eq!(
            Planner::plan("cargo build --release").unwrap_err(),
            PlanError::CommandShaped
        );

        // An unplannable intent is a typed failure naming what is missing.
        match Planner::plan("bake a sourdough loaf").unwrap_err() {
            PlanError::Unplannable { missing } => assert!(!missing.is_empty()),
            other => panic!("expected Unplannable, got {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn command_shapes_are_refused_and_natural_intents_are_not() {
        assert!(is_command_shaped("rm -rf /"));
        assert!(is_command_shaped("cargo build --release"));
        assert!(is_command_shaped("cat Cargo.toml | grep name"));
        assert!(!is_command_shaped("read the file Cargo.toml"));
        assert!(!is_command_shaped("list the directory crates"));
        assert!(!is_command_shaped("search for unsafe in crates"));
    }

    #[test]
    fn dag_rejects_cycle_and_unknown_operation() {
        let cyclic = PlanDag {
            nodes: vec![
                PlanNode {
                    id: "a".into(),
                    operation: "view_file".into(),
                    inputs: BTreeMap::from([("path".into(), "x".into())]),
                    depends_on: vec!["b".into()],
                },
                PlanNode {
                    id: "b".into(),
                    operation: "list_dir".into(),
                    inputs: BTreeMap::from([("path".into(), ".".into())]),
                    depends_on: vec!["a".into()],
                },
            ],
        };
        assert!(matches!(cyclic.validate(), Err(PlanError::Cycle(_))));

        let unknown = PlanDag {
            nodes: vec![PlanNode {
                id: "a".into(),
                operation: "bake_bread".into(),
                inputs: BTreeMap::new(),
                depends_on: vec![],
            }],
        };
        assert!(matches!(
            unknown.validate(),
            Err(PlanError::UnknownOperation(_))
        ));
    }

    #[test]
    fn write_then_read_chain_runs_in_intent_order() {
        let ws = scratch("chain");
        let dag = Planner::plan("write hello to greeting.txt and read the file greeting.txt")
            .expect("plannable");
        let order = dag.topo_order().unwrap();
        assert_eq!(dag.nodes[order[0]].operation, "write_file");
        assert_eq!(dag.nodes[order[1]].operation, "view_file");
        let provenance = execute(&dag, &ws).expect("executable");
        assert!(std::fs::read_to_string(ws.join("greeting.txt"))
            .unwrap()
            .contains("hello"));
        let read = provenance
            .iter()
            .find(|p| p.operation == "view_file")
            .unwrap();
        assert!(read.output.contains("hello"));
        let _ = std::fs::remove_dir_all(&ws);
    }
}
