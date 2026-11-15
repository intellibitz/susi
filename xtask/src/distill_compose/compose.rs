use super::*;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub(in crate::distill_compose) fn normalize_scope(scope: &str) -> Result<String> {
    let trimmed = scope.trim().replace('\\', "/");
    let clean = trimmed.trim_end_matches('/');
    if clean.is_empty()
        || clean.starts_with('/')
        || clean.chars().any(char::is_control)
        || clean
            .split('/')
            .any(|seg| seg.is_empty() || seg == "." || seg == "..")
    {
        return Err(format!("invalid scope path '{scope}'"));
    }
    Ok(clean.to_owned())
}

pub(in crate::distill_compose) fn is_forbidden_confidential_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let file_name = lower.rsplit('/').next().unwrap_or(&lower);
    file_name == ".env"
        || file_name.starts_with(".env.")
        || file_name == "credentials.json"
        || Path::new(file_name)
            .extension()
            .is_some_and(|ext| ext == "pem" || ext == "key" || ext == "p12")
}

pub(in crate::distill_compose) fn scope_matches_path(path: &str, scope: &str) -> bool {
    if path.eq_ignore_ascii_case(scope) {
        return true;
    }
    path.len() > scope.len()
        && path.as_bytes()[scope.len()] == b'/'
        && path[..scope.len()].eq_ignore_ascii_case(scope)
}

pub(in crate::distill_compose) fn path_within_scopes(
    path: &str,
    scopes: &[String],
    manifest_path: &str,
) -> bool {
    if path.eq_ignore_ascii_case(manifest_path) {
        return true;
    }
    scopes.iter().any(|scope| scope_matches_path(path, scope))
}

pub(in crate::distill_compose) fn spdx_allowed(expr: &str) -> bool {
    const ALLOWED: [&str; 4] = ["Apache-2.0", "MIT", "BSD-2-Clause", "BSD-3-Clause"];
    let cleaned = expr.replace(['(', ')'], " ");
    let tokens: Vec<&str> = cleaned.split_whitespace().collect();
    if tokens.is_empty() {
        return false;
    }
    tokens
        .iter()
        .all(|tok| matches!(*tok, "AND" | "OR") || ALLOWED.contains(tok))
        && tokens.iter().any(|tok| ALLOWED.contains(tok))
}

pub(in crate::distill_compose) fn load_feature_manifest(
    git: &Git<'_>,
    root: &Path,
    spec: &str,
) -> Result<FeatureManifest> {
    let bytes = if let Some((rev, path)) = spec.split_once(':') {
        if !rev.is_empty() && !Path::new(spec).is_absolute() && !rev.ends_with('\\') {
            let commit = git.resolve_commit(rev)?;
            let object = format!("{commit}:{path}");
            git.bytes(&["show", &object], None)?
        } else {
            fs::read(root.join(spec))
                .map_err(|e| format!("cannot read feature manifest {spec}: {e}"))?
        }
    } else {
        fs::read(root.join(spec))
            .map_err(|e| format!("cannot read feature manifest {spec}: {e}"))?
    };
    let val: Value = serde_json::from_slice(&bytes)
        .map_err(|e| format!("invalid feature manifest JSON {spec}: {e}"))?;
    if val.get("schema").and_then(Value::as_str) != Some(FEATURE_SCHEMA) {
        return Err(format!(
            "feature manifest must declare schema {FEATURE_SCHEMA}"
        ));
    }
    let id = val
        .get("id")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| "feature manifest missing string 'id'".to_owned())?
        .to_owned();
    let title = val
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or(&id)
        .to_owned();
    let license = val
        .get("license")
        .and_then(Value::as_str)
        .unwrap_or("Apache-2.0")
        .to_owned();
    if !spdx_allowed(&license) {
        return Err(format!(
            "disallowed_license: feature {id} declares non-allowlisted SPDX expression '{license}'"
        ));
    }
    let tasks = json_string_vec(&val, "tasks", true)?;
    if tasks.is_empty() {
        return Err(format!(
            "feature manifest {id} must list at least one task in 'tasks'"
        ));
    }
    let deps = json_string_vec(&val, "deps", false)?;
    let enabled_capabilities = json_string_vec(&val, "enabled_capabilities", false)?;
    let disabled_capabilities = json_string_vec(&val, "disabled_capabilities", false)?;
    let destination_branch = val
        .get("destination_branch")
        .and_then(Value::as_str)
        .map(str::to_owned);
    Ok(FeatureManifest {
        id,
        title,
        tasks,
        deps,
        destination_branch,
        license,
        enabled_capabilities,
        disabled_capabilities,
    })
}

pub(in crate::distill_compose) fn json_string_vec(
    val: &Value,
    field: &str,
    required: bool,
) -> Result<Vec<String>> {
    let Some(raw) = val.get(field) else {
        return if required {
            Err(format!("missing required array field '{field}'"))
        } else {
            Ok(Vec::new())
        };
    };
    let arr = raw
        .as_array()
        .ok_or_else(|| format!("field '{field}' must be an array of strings"))?;
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let s = item
            .as_str()
            .ok_or_else(|| format!("field '{field}' contains non-string element"))?;
        out.push(s.to_owned());
    }
    Ok(out)
}

pub(in crate::distill_compose) struct PlanManifestSpec {
    pub(in crate::distill_compose) sha256: String,
    pub(in crate::distill_compose) base_commit: Option<String>,
    pub(in crate::distill_compose) task_ids: Vec<String>,
    pub(in crate::distill_compose) functional_only: bool,
}

pub(in crate::distill_compose) fn load_plan_manifest(
    root: &Path,
    path: &Path,
) -> Result<PlanManifestSpec> {
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    let bytes = fs::read(&resolved)
        .map_err(|e| format!("cannot read --plan-manifest {}: {e}", resolved.display()))?;
    let val: Value = serde_json::from_slice(&bytes)
        .map_err(|e| format!("invalid --plan-manifest JSON {}: {e}", resolved.display()))?;
    if val.get("schema").and_then(Value::as_str) != Some(PLAN_SCHEMA) {
        return Err(format!("--plan-manifest must declare schema {PLAN_SCHEMA}"));
    }
    let base_commit = val
        .get("base")
        .and_then(|b| b.get("commit"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    if let Some(oid) = &base_commit {
        valid_oid(oid)?;
    }
    let functional_only = val
        .get("functional_only")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let has_skipped_non_functional = val
        .get("skipped_non_functional_tasks")
        .and_then(Value::as_array)
        .is_some_and(|arr| !arr.is_empty());
    let mut task_ids = Vec::new();
    if let Some(inc) = val.get("included").and_then(Value::as_array) {
        for entry in inc {
            if let Some(id) = entry.get("id").and_then(Value::as_str) {
                if !id.trim().is_empty() {
                    task_ids.push(id.trim().to_owned());
                }
            }
        }
    }
    if task_ids.is_empty() && !(functional_only && has_skipped_non_functional) {
        task_ids = json_string_vec(&val, "requested", false)?;
    }
    if task_ids.is_empty() && !(functional_only && has_skipped_non_functional) {
        return Err("--plan-manifest contains no included or requested task IDs".to_owned());
    }
    Ok(PlanManifestSpec {
        sha256: sha256_hex(&bytes),
        base_commit,
        task_ids,
        functional_only,
    })
}

pub(in crate::distill_compose) struct LoadedTasks {
    pub(in crate::distill_compose) valid: BTreeMap<String, TaskManifest>,
    pub(in crate::distill_compose) invalid: BTreeMap<String, String>,
    pub(in crate::distill_compose) done_deps: BTreeMap<String, Vec<String>>,
}

impl LoadedTasks {
    pub(in crate::distill_compose) fn deps_of(&self, key: &str) -> Option<&[String]> {
        self.valid
            .get(key)
            .map(|m| m.deps.as_slice())
            .or_else(|| self.done_deps.get(key).map(Vec::as_slice))
    }
}

pub(in crate::distill_compose) fn parse_closed_done_deps(
    bytes: &[u8],
) -> Option<(String, Vec<String>)> {
    let val: Value = serde_json::from_slice(bytes).ok()?;
    let id = val.get("id")?.as_str()?.trim();
    if id.is_empty() {
        return None;
    }
    let mut deps = json_string_vec(&val, "deps", false).unwrap_or_default();
    if deps.is_empty() {
        deps = json_string_vec(&val, "dependencies", false).unwrap_or_default();
    }
    Some((id.to_ascii_uppercase(), deps))
}

pub(in crate::distill_compose) fn load_tasks_from_commit(
    git: &Git<'_>,
    commit: &str,
) -> Result<LoadedTasks> {
    let entries: Vec<(String, String)> = git
        .tree_blobs(commit, ".agents/tasks")?
        .into_iter()
        .filter(|(path, _)| {
            path.ends_with(".json")
                && (path
                    .strip_prefix(".agents/tasks/")
                    .is_some_and(|rest| !rest.contains('/'))
                    || path
                        .strip_prefix(".agents/tasks/done/")
                        .is_some_and(|rest| !rest.contains('/'))
                    || path
                        .strip_prefix(".agents/tasks/in-progress/")
                        .is_some_and(|rest| !rest.contains('/')))
        })
        .collect();
    let oids: Vec<String> = entries.iter().map(|(_, oid)| oid.clone()).collect();
    let payloads = git.read_blobs(&oids)?;

    let mut valid: BTreeMap<String, TaskManifest> = BTreeMap::new();
    let mut invalid: BTreeMap<String, String> = BTreeMap::new();
    let mut done_deps: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for ((path, _), bytes) in entries.into_iter().zip(payloads) {
        let is_done = path.starts_with(".agents/tasks/done/");
        let stem = Path::new(&path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_uppercase();
        if is_done {
            if let Some((done_key, deps)) = parse_closed_done_deps(&bytes) {
                done_deps.insert(done_key, deps);
            }
        }
        match parse_task_manifest(&path, &bytes) {
            Ok(task) => {
                if valid.contains_key(&task.key) || invalid.contains_key(&task.key) {
                    valid.remove(&task.key);
                    invalid.insert(
                        task.key.clone(),
                        format!("duplicate task identifier across .agents/tasks records ({path})"),
                    );
                } else {
                    valid.insert(task.key.clone(), task);
                }
            }
            Err(err) => {
                if !stem.is_empty() {
                    invalid.insert(stem, format!("{path}: {err}"));
                }
            }
        }
    }
    Ok(LoadedTasks {
        valid,
        invalid,
        done_deps,
    })
}

pub(in crate::distill_compose) fn parse_task_manifest(
    path: &str,
    bytes: &[u8],
) -> Result<TaskManifest> {
    let val: Value = serde_json::from_slice(bytes).map_err(|e| format!("malformed JSON: {e}"))?;
    let id = val
        .get("id")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| "missing non-empty string 'id'".to_owned())?
        .trim()
        .to_owned();
    let key = id.to_ascii_uppercase();
    let title = val
        .get("title")
        .and_then(Value::as_str)
        .or_else(|| val.get("goal").and_then(Value::as_str))
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| "missing non-empty 'title' or 'goal'".to_owned())?
        .to_owned();
    let goal = val
        .get("goal")
        .and_then(Value::as_str)
        .unwrap_or(&title)
        .to_owned();
    let owner = val
        .get("created_by")
        .and_then(Value::as_str)
        .or_else(|| val.get("native_worker").and_then(Value::as_str))
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| "missing 'created_by'".to_owned())?
        .to_owned();
    let mut deps = json_string_vec(&val, "deps", false)?;
    if deps.is_empty() {
        deps = json_string_vec(&val, "dependencies", false)?;
    }
    let mut raw_scopes = json_string_vec(&val, "scopes", false)?;
    if raw_scopes.is_empty() {
        raw_scopes = json_string_vec(&val, "scope", false)?;
    }
    if raw_scopes.is_empty() {
        return Err("missing_scopes: task manifest must declare non-empty 'scopes'".to_owned());
    }
    let mut scopes = Vec::with_capacity(raw_scopes.len());
    for s in &raw_scopes {
        scopes.push(normalize_scope(s)?);
    }
    let accept_obj = val
        .get("accept")
        .ok_or_else(|| "missing 'accept.cmd'".to_owned())?;
    let accept_cmd = json_string_vec(accept_obj, "cmd", true)?;
    if accept_cmd.is_empty() {
        return Err("accept.cmd must not be empty".to_owned());
    }
    let source_commits = json_string_vec(&val, "source_commits", false)?;
    for oid in &source_commits {
        valid_oid(oid)?;
    }
    Ok(TaskManifest {
        id,
        key,
        path: path.to_owned(),
        title,
        goal,
        owner,
        deps,
        scopes,
        accept_cmd,
        source_commits,
    })
}

pub(in crate::distill_compose) struct ClosurePlan {
    pub(in crate::distill_compose) order: Vec<String>,
    pub(in crate::distill_compose) cycle_errors: BTreeMap<String, String>,
}

pub(in crate::distill_compose) struct ClosureWalk {
    pub(in crate::distill_compose) state: BTreeMap<String, VisitState>,
    pub(in crate::distill_compose) stack: Vec<String>,
    pub(in crate::distill_compose) order: Vec<String>,
    pub(in crate::distill_compose) cycle_errors: BTreeMap<String, String>,
}

pub(in crate::distill_compose) fn compute_closure(
    roots: &[String],
    tasks: &LoadedTasks,
    commit_rank: &BTreeMap<String, usize>,
    closure_leaves: &BTreeSet<String>,
) -> ClosurePlan {
    let mut walk = ClosureWalk {
        state: BTreeMap::new(),
        stack: Vec::new(),
        order: Vec::new(),
        cycle_errors: BTreeMap::new(),
    };

    for root in roots {
        let key = root.trim().to_ascii_uppercase();
        if !key.is_empty() {
            dfs_closure(&key, tasks, closure_leaves, &mut walk);
        }
    }

    // Preserve dependency-before-dependent ordering while sorting independent
    // ready tasks deterministically by earliest commit rank in base..from, then key.
    let ordered = deterministic_toposort(
        &walk.order,
        tasks,
        commit_rank,
        &walk.cycle_errors,
        closure_leaves,
    );
    ClosurePlan {
        order: ordered,
        cycle_errors: walk.cycle_errors,
    }
}

pub(in crate::distill_compose) fn dfs_closure(
    key: &str,
    tasks: &LoadedTasks,
    closure_leaves: &BTreeSet<String>,
    walk: &mut ClosureWalk,
) {
    if walk.stack.len() >= MAX_CLOSURE_DEPTH {
        walk.cycle_errors.insert(
            key.to_owned(),
            format!("dependency depth exceeded limit {MAX_CLOSURE_DEPTH}"),
        );
        return;
    }
    match walk.state.get(key).copied() {
        Some(VisitState::Visited) => return,
        Some(VisitState::Visiting) => {
            let pos = walk.stack.iter().position(|k| k == key).unwrap_or(0);
            let mut chain: Vec<String> = walk.stack[pos..].to_vec();
            chain.push(key.to_owned());
            let detail = format!("cycle detected: {}", chain.join(" -> "));
            for member in &walk.stack[pos..] {
                walk.cycle_errors.insert(member.clone(), detail.clone());
            }
            return;
        }
        None => {}
    }

    walk.state.insert(key.to_owned(), VisitState::Visiting);
    walk.stack.push(key.to_owned());

    if !closure_leaves.contains(key) {
        if let Some(raw_deps) = tasks.deps_of(key) {
            let mut sorted_deps: Vec<String> = raw_deps
                .iter()
                .map(|d| d.trim().to_ascii_uppercase())
                .filter(|d| !d.is_empty())
                .collect();
            sorted_deps.sort();
            sorted_deps.dedup();
            for dep_key in sorted_deps {
                dfs_closure(&dep_key, tasks, closure_leaves, walk);
            }
        }
    }

    let _ = walk.stack.pop();
    walk.state.insert(key.to_owned(), VisitState::Visited);
    walk.order.push(key.to_owned());
}

pub(in crate::distill_compose) fn deterministic_toposort(
    visited_order: &[String],
    tasks: &LoadedTasks,
    commit_rank: &BTreeMap<String, usize>,
    cycle_errors: &BTreeMap<String, String>,
    closure_leaves: &BTreeSet<String>,
) -> Vec<String> {
    let nodes: BTreeSet<String> = visited_order.iter().cloned().collect();
    let mut indegree: BTreeMap<String, usize> = BTreeMap::new();
    let mut dependents: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for key in &nodes {
        indegree.insert(key.clone(), 0);
    }
    for key in &nodes {
        if cycle_errors.contains_key(key) || closure_leaves.contains(key) {
            continue;
        }
        if let Some(raw_deps) = tasks.deps_of(key) {
            let mut deps: BTreeSet<String> = BTreeSet::new();
            for dep in raw_deps {
                let dep_key = dep.trim().to_ascii_uppercase();
                if nodes.contains(&dep_key)
                    && dep_key != *key
                    && !cycle_errors.contains_key(&dep_key)
                {
                    deps.insert(dep_key);
                }
            }
            indegree.insert(key.clone(), deps.len());
            for dep_key in deps {
                dependents.entry(dep_key).or_default().push(key.clone());
            }
        }
    }

    let mut ready: BTreeSet<(usize, String)> = BTreeSet::new();
    for (key, deg) in &indegree {
        if *deg == 0 {
            let rank = commit_rank.get(key).copied().unwrap_or(usize::MAX);
            ready.insert((rank, key.clone()));
        }
    }

    let mut out = Vec::with_capacity(nodes.len());
    while let Some((_, current)) = ready.pop_first() {
        out.push(current.clone());
        if let Some(children) = dependents.get(&current) {
            for child in children {
                if let Some(deg) = indegree.get_mut(child) {
                    *deg = deg.saturating_sub(1);
                    if *deg == 0 {
                        let rank = commit_rank.get(child).copied().unwrap_or(usize::MAX);
                        ready.insert((rank, child.clone()));
                    }
                }
            }
        }
    }
    for key in visited_order {
        if !out.contains(key) {
            out.push(key.clone());
        }
    }
    out
}

pub(in crate::distill_compose) fn attribute_task_commits(
    git: &Git<'_>,
    base_commit: &str,
    from_commit: &str,
    task: &TaskManifest,
    stream_commits: &[CommitInfo],
) -> Result<Vec<CommitInfo>> {
    if !task.source_commits.is_empty() {
        let mut pinned = Vec::with_capacity(task.source_commits.len());
        for oid in &task.source_commits {
            if !git.ancestor(oid, from_commit)? {
                return Err(format!(
                    "pinned source_commit {oid} for {} is not an ancestor of stream {from_commit}",
                    task.id
                ));
            }
            if git.ancestor(oid, base_commit)? {
                continue;
            }
            pinned.push(git.inspect_commit(oid)?);
        }
        return Ok(pinned);
    }
    let matched: Vec<CommitInfo> = stream_commits
        .iter()
        .filter(|commit| {
            commit.task_trailers.iter().any(|t| t == &task.key)
                || commit.changed_paths.iter().any(|p| p == &task.path)
        })
        .cloned()
        .collect();
    Ok(matched)
}

pub(in crate::distill_compose) type ReasonErr = (&'static str, String);

pub(in crate::distill_compose) struct ComposeCursor<'a> {
    pub(in crate::distill_compose) base_commit: &'a str,
    pub(in crate::distill_compose) current_commit: &'a str,
    pub(in crate::distill_compose) current_tree: &'a str,
}

pub(in crate::distill_compose) struct ComposedStep {
    pub(in crate::distill_compose) commit: String,
    pub(in crate::distill_compose) tree: String,
    pub(in crate::distill_compose) changed_files: Vec<String>,
}

pub(in crate::distill_compose) fn audit_task_scopes(
    task: &TaskManifest,
    commits: &[CommitInfo],
) -> std::result::Result<(), ReasonErr> {
    for commit in commits {
        for path in &commit.changed_paths {
            if is_forbidden_confidential_path(path) {
                return Err((
                    "confidentiality_violation",
                    format!(
                        "commit {} touches forbidden credential/key path '{path}'",
                        commit.oid
                    ),
                ));
            }
            if !path_within_scopes(path, &task.scopes, &task.path) {
                return Err((
                    "out_of_scope_diff",
                    format!(
                        "commit {} modifies '{path}' outside declared scopes {:?}",
                        commit.oid, task.scopes
                    ),
                ));
            }
        }
    }
    Ok(())
}

pub(in crate::distill_compose) fn compose_task_onto(
    git: &Git<'_>,
    cursor: &ComposeCursor<'_>,
    task: &TaskManifest,
    commits: &[CommitInfo],
) -> Result<std::result::Result<ComposedStep, ReasonErr>> {
    let mut step_commit = cursor.current_commit.to_owned();
    let mut step_tree = cursor.current_tree.to_owned();

    for commit in commits {
        let merged_tree = match git.merge_three_way(&commit.parent, &step_commit, &commit.oid)? {
            ThreeWayMerge::Clean(tree) => tree,
            ThreeWayMerge::Conflict(conflict_msg) => {
                return Ok(Err(("merge_conflict", conflict_msg)));
            }
        };
        if merged_tree != step_tree {
            let delta_paths = git.diff_paths(&step_tree, &merged_tree)?;
            for path in &delta_paths {
                if !path_within_scopes(path, &task.scopes, &task.path) {
                    return Ok(Err((
                        "out_of_scope_diff",
                        format!(
                            "3-way composition of {} leaked out-of-scope path '{path}'",
                            commit.oid
                        ),
                    )));
                }
            }
            let step_msg = format!("distill-step {}: {}\n", task.id, commit.subject);
            step_commit = git.commit_tree(&merged_tree, &step_commit, &step_msg)?;
            step_tree = merged_tree;
        }
    }

    let changed_files = git.diff_paths(cursor.current_tree, &step_tree)?;
    let source_list: Vec<&str> = commits.iter().map(|c| c.oid.as_str()).collect();
    let message = format!(
        "Distill task {}: {}\n\nUnqualified distilled candidate. Full exact-tree release gates remain required.\n\nDistilled-Task: {}\nSource-Commits: {}\nDistill-Base: {}\n",
        task.id,
        task.title,
        task.id,
        source_list.join(","),
        cursor.base_commit
    );
    let final_task_commit = git.commit_tree(&step_tree, cursor.current_commit, &message)?;
    Ok(Ok(ComposedStep {
        commit: final_task_commit,
        tree: step_tree,
        changed_files,
    }))
}

pub(in crate::distill_compose) struct DetachedVerifyWorktree<'a> {
    pub(in crate::distill_compose) git: &'a Git<'a>,
    pub(in crate::distill_compose) temp_dir: PathBuf,
    pub(in crate::distill_compose) wt_dir: PathBuf,
}

impl Drop for DetachedVerifyWorktree<'_> {
    fn drop(&mut self) {
        if let Some(wt_str) = self.wt_dir.to_str() {
            let _ = self
                .git
                .output(&["worktree", "remove", "--force", wt_str], None);
        }
        let _ = fs::remove_dir_all(&self.temp_dir);
    }
}

/// Toolchain homes an accept command keeps when `HOME` is replaced.
///
/// Cargo and rustup default to `$HOME/.cargo` and `$HOME/.rustup`, so the
/// scratch `HOME` below would hide the registry cache and the toolchains, and
/// every `--offline` accept command would fail to resolve its first crate.
/// Both are immutable shared caches (AGENTS.md permits sharing those): an
/// explicit non-empty value is kept, otherwise the caller's default is pinned.
pub(in crate::distill_compose) fn toolchain_homes(
    get: impl Fn(&str) -> Option<std::ffi::OsString>,
) -> Vec<(&'static str, std::ffi::OsString)> {
    let home = get("HOME")
        .filter(|value| !value.is_empty())
        .or_else(|| get("USERPROFILE").filter(|value| !value.is_empty()));
    [("CARGO_HOME", ".cargo"), ("RUSTUP_HOME", ".rustup")]
        .into_iter()
        .filter_map(|(key, default_dir)| {
            get(key)
                .filter(|value| !value.is_empty())
                .or_else(|| {
                    home.as_ref()
                        .map(|home| Path::new(home).join(default_dir).into_os_string())
                })
                .map(|value| (key, value))
        })
        .collect()
}

// Acceptance runs under a disposable home. Host compiler-cache wrappers may
// depend on real user config (sccache panics on Windows without it). Match the
// native local-workflow/release isolation without changing host Cargo settings.
// Empty overrides, rather than env_remove, also suppress Cargo config wrappers.
pub(in crate::distill_compose) fn isolate_acceptance_compiler_wrappers(command: &mut Command) {
    for key in [
        "RUSTC_WRAPPER",
        "RUSTC_WORKSPACE_WRAPPER",
        "CARGO_BUILD_RUSTC_WRAPPER",
        "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
    ] {
        command.env(key, "");
    }
}

#[cfg(test)]
#[test]
fn acceptance_disables_host_wrappers_without_mutating_parent_environment() {
    let keys = [
        "RUSTC_WRAPPER",
        "RUSTC_WORKSPACE_WRAPPER",
        "CARGO_BUILD_RUSTC_WRAPPER",
        "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
    ];
    let before: Vec<_> = keys.iter().map(std::env::var_os).collect();
    let mut command = Command::new("cargo");
    for key in keys {
        command.env(key, "host-wrapper-that-requires-user-config");
    }
    isolate_acceptance_compiler_wrappers(&mut command);
    for key in keys {
        assert_eq!(
            command
                .get_envs()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| value),
            Some(Some(std::ffi::OsStr::new("")))
        );
    }
    assert_eq!(
        before,
        keys.iter().map(std::env::var_os).collect::<Vec<_>>()
    );
}

pub(in crate::distill_compose) fn verify_composed_accept_commands(
    git: &Git<'_>,
    final_commit: &str,
    included: &[IncludedEntry],
) -> Result<Vec<Value>> {
    valid_oid(final_commit)?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let temp_dir = std::env::temp_dir().join(format!(
        "susi-distill-verify-{}-{nonce}",
        std::process::id()
    ));
    let wt_dir = temp_dir.join("candidate");
    let home_dir = temp_dir.join("home");
    let runtime_dir = temp_dir.join("runtime");
    fs::create_dir_all(&home_dir)
        .and_then(|()| fs::create_dir_all(&runtime_dir))
        .map_err(|e| format!("cannot prepare hermetic accept-verify dirs: {e}"))?;

    let wt_str = wt_dir
        .to_str()
        .ok_or_else(|| "non-UTF-8 temporary worktree path".to_owned())?
        .to_owned();
    let guard = DetachedVerifyWorktree {
        git,
        temp_dir,
        wt_dir: wt_dir.clone(),
    };
    git.text(&["worktree", "add", "--detach", &wt_str, final_commit])?;

    let mut records = Vec::with_capacity(included.len());
    for entry in included {
        let Some((program, cmd_args)) = entry.accept_cmd.split_first() else {
            return Err(format!(
                "accept_verification_failed: {} has empty accept.cmd",
                entry.task_id
            ));
        };
        let mut command = Command::new(program);
        command
            .args(cmd_args)
            .current_dir(&guard.wt_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("HOME", &home_dir)
            .env("USERPROFILE", &home_dir)
            .env("SUSI_HOME", home_dir.join(".susi"))
            .env("XDG_RUNTIME_DIR", &runtime_dir)
            .env("TMPDIR", &runtime_dir)
            .env("TEMP", &runtime_dir)
            .env("TMP", &runtime_dir)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("SUSI_NO_REGISTRY_ENV", "1");
        isolate_acceptance_compiler_wrappers(&mut command);
        for (key, value) in toolchain_homes(|key| std::env::var_os(key)) {
            command.env(key, value);
        }
        for secret_key in [
            "GITHUB_TOKEN",
            "GH_TOKEN",
            "HF_TOKEN",
            "OPENAI_API_KEY",
            "ANTHROPIC_API_KEY",
            "GEMINI_API_KEY",
            "OPENROUTER_API_KEY",
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
        ] {
            command.env_remove(secret_key);
        }
        let output = command.output().map_err(|e| {
            format!(
                "accept_verification_failed: cannot execute {:?} for {}: {e}",
                entry.accept_cmd, entry.task_id
            )
        })?;
        let mut combined = output.stdout;
        combined.extend_from_slice(&output.stderr);
        let output_sha256 = sha256_hex(&combined);
        let code = output.status.code().unwrap_or(-1);
        if !output.status.success() {
            let preview = String::from_utf8_lossy(&combined);
            let tail: String = preview
                .chars()
                .rev()
                .take(400)
                .collect::<String>()
                .chars()
                .rev()
                .collect();
            return Err(format!(
                "accept_verification_failed: task {} command {:?} exited with {} ({})",
                entry.task_id,
                entry.accept_cmd,
                code,
                tail.trim()
            ));
        }
        records.push(json!({
            "task_id": entry.task_id,
            "cmd": entry.accept_cmd,
            "exit_code": code,
            "passed": true,
            "output_sha256": output_sha256,
        }));
    }
    Ok(records)
}

pub(in crate::distill_compose) struct DistillOutcome {
    pub(in crate::distill_compose) receipt: Value,
    pub(in crate::distill_compose) receipt_bytes: Vec<u8>,
    pub(in crate::distill_compose) receipt_sha256: String,
    pub(in crate::distill_compose) final_commit: String,
    pub(in crate::distill_compose) final_tree: String,
    pub(in crate::distill_compose) branch_ref: String,
    pub(in crate::distill_compose) has_deferred_or_rejected: bool,
}

pub(in crate::distill_compose) fn execute_distill(
    git: &Git<'_>,
    root: &Path,
    args: &DistillArgs,
) -> Result<DistillOutcome> {
    git.require_original_history()?;
    let plan_spec = match &args.plan_manifest {
        Some(path) => Some(load_plan_manifest(root, path)?),
        None => None,
    };
    let effective_from = args
        .from_rev
        .as_deref()
        .or_else(|| plan_spec.as_ref().and_then(|p| p.base_commit.as_deref()))
        .unwrap_or("HEAD");

    if args.pull {
        git.pull_explicit_remote_ref(&args.base_rev)?;
    }
    let base_commit = if args.pull && args.base_rev == "origin/main" {
        let _ = git.pull_and_reconcile_source("HEAD");
        git.resolve_commit(&args.base_rev)?
    } else {
        git.resolve_commit(&args.base_rev)?
    };
    let base_tree = git.tree(&base_commit)?;
    let from_commit = if args.pull {
        git.pull_and_reconcile_source(effective_from)?
    } else {
        git.resolve_commit(effective_from)?
    };
    let from_tree = git.tree(&from_commit)?;

    let feature = match &args.feature_spec {
        Some(spec) => Some(load_feature_manifest(git, root, spec)?),
        None => None,
    };

    let functional_only =
        args.functional_only || plan_spec.as_ref().is_some_and(|p| p.functional_only);
    let mut requested: Vec<String> = args.selected.clone();
    if let Some(range_spec) = &args.tasks_from_range {
        requested.extend(discover_tasks_in_range(git, range_spec, functional_only)?);
    }
    if let Some(plan) = &plan_spec {
        requested.extend(plan.task_ids.clone());
    }
    if let Some(feat) = &feature {
        requested.extend(feat.tasks.clone());
        requested.extend(feat.deps.clone());
    }

    let from_tasks = load_tasks_from_commit(git, &from_commit)?;
    let base_tasks = load_tasks_from_commit(git, &base_commit)?;
    let distilled_in_base = git.distilled_into_base(&from_commit, &base_commit)?;
    let stream_commits: Vec<CommitInfo> = git
        .non_merge_commits(&base_commit, &from_commit)?
        .into_iter()
        .filter(|commit| !distilled_in_base.covers_commit(commit))
        .collect();
    let base_reason = |key: &str| -> (&'static str, &'static str) {
        if distilled_in_base.tasks.contains(key) {
            (
                "already_distilled_in_base",
                "was distilled into base by an earlier distillation (Distilled-Task/Source-Commits)",
            )
        } else {
            ("already_in_base", "is already present in base")
        }
    };

    if args.mode == CommandMode::Orchestrate && requested.is_empty() {
        let mut seen = BTreeSet::new();
        for commit in &stream_commits {
            for trailer in &commit.task_trailers {
                if functional_only && trailer.starts_with("LOCAL-") {
                    continue;
                }
                if seen.insert(trailer.clone()) {
                    requested.push(trailer.clone());
                }
            }
            if functional_only {
                for path in &commit.changed_paths {
                    if let Some(id) = functional::task_id_from_manifest_path(path) {
                        if seen.insert(id.clone()) {
                            requested.push(id);
                        }
                    }
                }
            }
        }
    }

    let mut commit_rank: BTreeMap<String, usize> = BTreeMap::new();
    for (idx, commit) in stream_commits.iter().enumerate() {
        for trailer in &commit.task_trailers {
            commit_rank.entry(trailer.clone()).or_insert(idx);
        }
        for (key, manifest) in &from_tasks.valid {
            if commit.changed_paths.iter().any(|p| p == &manifest.path) {
                commit_rank.entry(key.clone()).or_insert(idx);
            }
        }
    }

    // Merges happen in Susi; the Git host is storage only. Work whose commits
    // the host created itself (web merge, web edit, API commit) is refused
    // unless the distilling agent adopts each commit by full id, on record.
    let host_identities = hostmerge::identities(root)?;
    let relands = hostmerge::relands(root, &from_commit, &host_identities)?;
    let (relanded_in_stream, mut host_in_stream): (Vec<_>, Vec<_>) = hostmerge::scan_range(
        root,
        &format!("{base_commit}..{from_commit}"),
        &host_identities,
    )?
    .into_iter()
    .partition(|c| relands.valid.contains_key(&c.oid));
    if functional_only {
        let non_func_stream_oids: BTreeSet<&str> = stream_commits
            .iter()
            .filter(|c| !functional::is_functional_commit(&c.subject, &c.changed_paths))
            .map(|c| c.oid.as_str())
            .collect();
        host_in_stream.retain(|c| !non_func_stream_oids.contains(c.oid.as_str()));
    }

    let base_satisfied_leaves = classify_base_satisfied_leaves(&BaseSatisfiedScan {
        git,
        base_commit: &base_commit,
        from_commit: &from_commit,
        from_tasks: &from_tasks,
        base_tasks: &base_tasks,
        distilled_in_base: &distilled_in_base,
        stream_commits: &stream_commits,
        relanded_valid: &relands.valid,
        functional_only,
    });

    let non_functional_leaves = if functional_only {
        classify_non_functional_leaves(&NonFunctionalScan {
            is_commercial_repo: is_commercial_checkout(root),
            requested: &requested,
            from_tasks: &from_tasks,
            base_tasks: &base_tasks,
            distilled_in_base: &distilled_in_base,
            stream_commits: &stream_commits,
            relanded_valid: &relands.valid,
        })
    } else {
        BTreeSet::new()
    };

    let closure_leaves: BTreeSet<String> = base_satisfied_leaves
        .union(&non_functional_leaves)
        .cloned()
        .collect();

    let closure = compute_closure(&requested, &from_tasks, &commit_rank, &closure_leaves);

    let mut outcomes: BTreeMap<String, TaskOutcome> = BTreeMap::new();
    let mut included: Vec<IncludedEntry> = Vec::new();
    let mut satisfied_in_base: Vec<BaseSatisfiedEntry> = Vec::new();
    let mut skipped_non_functional_set: BTreeSet<String> = BTreeSet::new();
    let mut deferred: Vec<DecisionEntry> = Vec::new();
    let mut rejected: Vec<DecisionEntry> = Vec::new();

    let mut current_commit = base_commit.clone();
    let mut current_tree = base_tree.clone();
    let mut adopted_host_commits: BTreeMap<String, String> = BTreeMap::new();

    for key in &closure.order {
        if let Some(cycle_detail) = closure.cycle_errors.get(key) {
            let label = from_tasks
                .valid
                .get(key)
                .map_or_else(|| key.clone(), |t| t.id.clone());
            let deps = from_tasks
                .valid
                .get(key)
                .map_or_else(Vec::new, |t| t.deps.clone());
            rejected.push(DecisionEntry {
                task_id: label,
                source_commits: Vec::new(),
                deps,
                reason_code: "cyclic_dependency",
                detail: cycle_detail.clone(),
            });
            outcomes.insert(key.clone(), TaskOutcome::Rejected);
            continue;
        }

        if base_tasks.done_deps.contains_key(key)
            && !has_done_task_stream_commits(key, &stream_commits, &relands.valid, functional_only)
        {
            let (reason_code, how) = base_reason(key);
            satisfied_in_base.push(BaseSatisfiedEntry {
                task_id: key.clone(),
                reason_code,
                detail: format!(
                    "closed task record for {key} {how} {base_commit} with zero newer commits"
                ),
            });
            outcomes.insert(key.clone(), TaskOutcome::SatisfiedInBase);
            continue;
        }

        if non_functional_leaves.contains(key) {
            let label = from_tasks
                .valid
                .get(key)
                .map_or_else(|| key.clone(), |t| t.id.clone());
            skipped_non_functional_set.insert(label);
            outcomes.insert(key.clone(), TaskOutcome::SkippedNonFunctional);
            continue;
        }

        if let Some(inv_detail) = from_tasks.invalid.get(key) {
            let reason = if inv_detail.contains("missing_scopes") {
                "missing_scopes"
            } else {
                "invalid_manifest"
            };
            rejected.push(DecisionEntry {
                task_id: key.clone(),
                source_commits: Vec::new(),
                deps: Vec::new(),
                reason_code: reason,
                detail: inv_detail.clone(),
            });
            outcomes.insert(key.clone(), TaskOutcome::Rejected);
            continue;
        }

        let Some(task) = from_tasks.valid.get(key) else {
            if base_tasks.valid.contains_key(key) || distilled_in_base.tasks.contains(key) {
                let (reason_code, how) = base_reason(key);
                satisfied_in_base.push(BaseSatisfiedEntry {
                    task_id: key.clone(),
                    reason_code,
                    detail: format!("task {key} {how} {base_commit} with zero newer commits"),
                });
                outcomes.insert(key.clone(), TaskOutcome::SatisfiedInBase);
            } else {
                rejected.push(DecisionEntry {
                    task_id: key.clone(),
                    source_commits: Vec::new(),
                    deps: Vec::new(),
                    reason_code: "missing_manifest",
                    detail: format!("no .agents/tasks manifest found for {key} in {from_commit}"),
                });
                outcomes.insert(key.clone(), TaskOutcome::Rejected);
            }
            continue;
        };

        let attributed_res =
            attribute_task_commits(git, &base_commit, &from_commit, task, &stream_commits);
        if let Ok(raw_list) = &attributed_res {
            let mut filtered: Vec<&CommitInfo> = raw_list
                .iter()
                .filter(|commit| !distilled_in_base.covers_commit(commit))
                .filter(|commit| !relands.valid.contains_key(&commit.oid))
                .collect();
            let had_raw_attributed = !filtered.is_empty();
            if functional_only {
                filtered.retain(|commit| {
                    functional::is_functional_commit(&commit.subject, &commit.changed_paths)
                });
            }
            if filtered.is_empty() {
                if base_tasks.valid.contains_key(key)
                    || base_tasks.done_deps.contains_key(key)
                    || distilled_in_base.tasks.contains(key)
                {
                    let (reason_code, how) = base_reason(key);
                    satisfied_in_base.push(BaseSatisfiedEntry {
                        task_id: task.id.clone(),
                        reason_code,
                        detail: format!(
                            "task {} {how} {base_commit} with zero newer commits",
                            task.id
                        ),
                    });
                    outcomes.insert(key.clone(), TaskOutcome::SatisfiedInBase);
                    continue;
                }
                if functional_only
                    && (had_raw_attributed
                        || (!task.scopes.is_empty()
                            && task
                                .scopes
                                .iter()
                                .all(|s| !functional::is_functional_path(s))))
                {
                    skipped_non_functional_set.insert(task.id.clone());
                    outcomes.insert(key.clone(), TaskOutcome::SkippedNonFunctional);
                    continue;
                }
            }
        }

        if let Some((dep_reason, dep_detail, is_reject)) = check_dep_outcomes(task, &outcomes) {
            let entry = DecisionEntry {
                task_id: task.id.clone(),
                source_commits: Vec::new(),
                deps: task.deps.clone(),
                reason_code: dep_reason,
                detail: dep_detail,
            };
            if is_reject {
                rejected.push(entry);
                outcomes.insert(key.clone(), TaskOutcome::Rejected);
            } else {
                deferred.push(entry);
                outcomes.insert(key.clone(), TaskOutcome::Deferred);
            }
            continue;
        }

        let mut attributed = match attributed_res {
            Ok(mut list) => {
                list.retain(|commit| !distilled_in_base.covers_commit(commit));
                // A host commit re-landed through Susi is replaced by its replay.
                list.retain(|commit| !relands.valid.contains_key(&commit.oid));
                list
            }
            Err(err) => {
                rejected.push(DecisionEntry {
                    task_id: task.id.clone(),
                    source_commits: task.source_commits.clone(),
                    deps: task.deps.clone(),
                    reason_code: "invalid_source_commit",
                    detail: err,
                });
                outcomes.insert(key.clone(), TaskOutcome::Rejected);
                continue;
            }
        };
        if functional_only {
            attributed.retain(|commit| {
                functional::is_functional_commit(&commit.subject, &commit.changed_paths)
            });
        }

        if attributed.is_empty() {
            deferred.push(DecisionEntry {
                task_id: task.id.clone(),
                source_commits: Vec::new(),
                deps: task.deps.clone(),
                reason_code: "unresolved_commit",
                detail: format!(
                    "no non-merge commits in {base_commit}..{from_commit} matched Task: {}",
                    task.id
                ),
            });
            outcomes.insert(key.clone(), TaskOutcome::Deferred);
            continue;
        }

        let commit_oids: Vec<String> = attributed.iter().map(|c| c.oid.clone()).collect();
        if let Err((code, detail)) = audit_task_scopes(task, &attributed) {
            rejected.push(DecisionEntry {
                task_id: task.id.clone(),
                source_commits: commit_oids,
                deps: task.deps.clone(),
                reason_code: code,
                detail,
            });
            outcomes.insert(key.clone(), TaskOutcome::Rejected);
            continue;
        }

        let attributed_oids: BTreeSet<String> = commit_oids.iter().cloned().collect();
        let host_made = hostmerge::among(root, &attributed_oids, &host_identities)?;
        let refused: Vec<String> = host_made
            .values()
            .filter(|c| !args.adopt_host_commits.contains(&c.oid))
            .map(|c| format!("{} ({} <{}>)", c.oid, c.committer, c.email))
            .collect();
        if !refused.is_empty() {
            rejected.push(DecisionEntry {
                task_id: task.id.clone(),
                source_commits: commit_oids,
                deps: task.deps.clone(),
                reason_code: "merged_outside_susi",
                detail: format!(
                    "commit(s) created by the Git host, not by a Susi merge: {}; re-land them through Susi or adopt each with --adopt-host-commit <full-id>",
                    refused.join(", ")
                ),
            });
            outcomes.insert(key.clone(), TaskOutcome::Rejected);
            continue;
        }
        for c in host_made.values() {
            adopted_host_commits.insert(
                c.oid.clone(),
                format!("{} <{}> for {}", c.committer, c.email, task.id),
            );
        }

        let cursor = ComposeCursor {
            base_commit: &base_commit,
            current_commit: &current_commit,
            current_tree: &current_tree,
        };
        match compose_task_onto(git, &cursor, task, &attributed)? {
            Ok(composed) => {
                let patch_id = mandate74::compute_commit_patch_id(root, &composed.commit)?;
                current_commit = composed.commit.clone();
                current_tree = composed.tree.clone();
                included.push(IncludedEntry {
                    task_id: task.id.clone(),
                    title: task.title.clone(),
                    goal: task.goal.clone(),
                    owner: task.owner.clone(),
                    deps: task.deps.clone(),
                    scopes: task.scopes.clone(),
                    accept_cmd: task.accept_cmd.clone(),
                    source_commits: commit_oids,
                    composed_commit: composed.commit,
                    composed_tree: composed.tree,
                    patch_id,
                    changed_files: composed.changed_files,
                });
                outcomes.insert(key.clone(), TaskOutcome::Included);
            }
            Err((code, detail)) => {
                let entry = DecisionEntry {
                    task_id: task.id.clone(),
                    source_commits: commit_oids,
                    deps: task.deps.clone(),
                    reason_code: code,
                    detail,
                };
                if code == "out_of_scope_diff" {
                    rejected.push(entry);
                    outcomes.insert(key.clone(), TaskOutcome::Rejected);
                } else {
                    deferred.push(entry);
                    outcomes.insert(key.clone(), TaskOutcome::Deferred);
                }
            }
        }
    }

    let branch_ref = resolve_branch_ref(args, feature.as_ref())?;
    let endpoint_digest = if args.pull {
        git.endpoint_optional().ok().flatten().map(|e| e.digest)
    } else {
        git.endpoint_optional()?.map(|e| e.digest)
    };
    let lane_inputs: Vec<mandate74::LaneTaskInput> = included
        .iter()
        .map(|i| mandate74::LaneTaskInput {
            task_id: i.task_id.clone(),
            deps: i.deps.clone(),
            scopes: i.scopes.clone(),
            changed_files: i.changed_files.clone(),
            patch_id: i.patch_id.clone(),
        })
        .collect();
    let lanes = mandate74::partition_disjoint_lanes(&lane_inputs);
    let selected_source_commits: Vec<String> = included
        .iter()
        .flat_map(|i| i.source_commits.iter().cloned())
        .collect();
    let harvest_audit =
        mandate74::verify_harvest_for_commits(root, &current_commit, &selected_source_commits)?;
    if !harvest_audit.passed && (args.strict || args.release_candidate) {
        return Err(format!(
            "disallowed_or_unattributed_harvest: {}",
            harvest_audit.findings.join("; ")
        ));
    }
    let has_deferred_or_rejected = !deferred.is_empty() || !rejected.is_empty();
    let accept_verification = if args.verify_accept && !(args.strict && has_deferred_or_rejected) {
        Some(verify_composed_accept_commands(
            git,
            &current_commit,
            &included,
        )?)
    } else {
        None
    };
    let qualification_status = if accept_verification.is_some() {
        "accept_verified_candidate"
    } else {
        "unqualified_candidate"
    };

    let skipped_non_functional_tasks: Vec<String> =
        skipped_non_functional_set.into_iter().collect();

    let mut body = json!({
        "schema": RECEIPT_SCHEMA,
        "name": args.name,
        "mode": match args.mode {
            CommandMode::Plan => "plan",
            CommandMode::Compose => "compose",
            CommandMode::Orchestrate => "orchestrate",
            CommandMode::Publish => "publish",
            CommandMode::Status => "status",
        },
        "functional_only": functional_only,
        "skipped_non_functional_tasks": skipped_non_functional_tasks,
        "qualification_status": qualification_status,
        "base_commit": base_commit,
        "base_tree": base_tree,
        "source_stream_commit": from_commit,
        "source_stream_tree": from_tree,
        "branch_ref": branch_ref,
        "snapshot_tag_ref": format!("refs/tags/susi-distill/{}", args.name),
        "endpoint_digest": endpoint_digest,
        "plan_manifest_sha256": plan_spec.as_ref().map(|p| &p.sha256),
        "requested_tasks": requested,
        "closure_order": closure.order,
        "lanes": lanes.iter().map(mandate74::DisjointLane::to_json).collect::<Vec<_>>(),
        "harvest_audit": harvest_audit.to_json(),
        "feature": feature.as_ref().map(|f| json!({
            "id": f.id,
            "title": f.title,
            "license": f.license,
            "destination_branch": f.destination_branch,
            "enabled_capabilities": f.enabled_capabilities,
            "disabled_capabilities": f.disabled_capabilities,
        })),
        "accept_verification": accept_verification,
        "included": included.iter().map(|i| json!({
            "task_id": i.task_id,
            "title": i.title,
            "goal": i.goal,
            "owner": i.owner,
            "deps": i.deps,
            "scopes": i.scopes,
            "accept_cmd": i.accept_cmd,
            "source_commits": i.source_commits,
            "composed_commit": i.composed_commit,
            "composed_tree": i.composed_tree,
            "patch_id": i.patch_id,
            "changed_files": i.changed_files,
            "reason_code": "selected_and_composed",
        })).collect::<Vec<_>>(),
        "satisfied_in_base": satisfied_in_base.iter().map(|b| json!({
            "task_id": b.task_id,
            "reason_code": b.reason_code,
            "detail": b.detail,
        })).collect::<Vec<_>>(),
        "deferred": deferred.iter().map(render_decision).collect::<Vec<_>>(),
        "rejected": rejected.iter().map(render_decision).collect::<Vec<_>>(),
        "git_host_identities": host_identities,
        "git_host_created_in_stream": host_in_stream.iter().map(|c| json!({
            "commit": c.oid,
            "merge": c.merge,
            "committer": format!("{} <{}>", c.committer, c.email),
            "subject": c.subject,
        })).collect::<Vec<_>>(),
        "adopted_host_commits": adopted_host_commits,
        "relanded_host_commits_in_stream": relanded_in_stream
            .iter()
            .map(|c| (c.oid.clone(), relands.valid.get(&c.oid).cloned()))
            .collect::<BTreeMap<_, _>>(),
        "reland_claims_not_honoured": relands.invalid.iter().map(|(by, named, why)| json!({
            "commit": by,
            "names": named,
            "reason": why,
        })).collect::<Vec<_>>(),
        "final_commit": current_commit,
        "final_tree": current_tree,
    });

    let canonical_payload =
        serde_json::to_vec(&body).map_err(|e| format!("cannot serialize receipt: {e}"))?;
    let receipt_sha256 = sha256_hex(&canonical_payload);
    if let Some(map) = body.as_object_mut() {
        map.insert("receipt_sha256".to_owned(), json!(receipt_sha256));
    }
    let mut receipt_bytes =
        serde_json::to_vec_pretty(&body).map_err(|e| format!("cannot format receipt: {e}"))?;
    receipt_bytes.push(b'\n');

    Ok(DistillOutcome {
        receipt: body,
        receipt_bytes,
        receipt_sha256,
        final_commit: current_commit,
        final_tree: current_tree,
        branch_ref,
        has_deferred_or_rejected,
    })
}

pub(in crate::distill_compose) fn render_decision(d: &DecisionEntry) -> Value {
    json!({
        "task_id": d.task_id,
        "source_commits": d.source_commits,
        "deps": d.deps,
        "reason_code": d.reason_code,
        "detail": d.detail,
    })
}

pub(in crate::distill_compose) fn resolve_branch_ref(
    args: &DistillArgs,
    feature: Option<&FeatureManifest>,
) -> Result<String> {
    if args.release_candidate {
        return Ok(format!("refs/heads/release/{}", args.name));
    }
    if let Some(dest) = feature.and_then(|f| f.destination_branch.as_deref()) {
        let clean = normalize_scope(dest)?;
        if let Some(stripped) = clean.strip_prefix("refs/heads/") {
            return Ok(format!("refs/heads/{stripped}"));
        }
        return Ok(format!("refs/heads/{clean}"));
    }
    Ok(format!("refs/heads/distill/{}", args.name))
}

pub(in crate::distill_compose) fn is_commercial_checkout(root: &Path) -> bool {
    let manifest = root.join("Cargo.toml");
    let Ok(text) = fs::read_to_string(manifest) else {
        return false;
    };
    text.lines()
        .any(|line| line.trim() == "name = \"susi-commercial\"")
}

pub(in crate::distill_compose) fn has_done_task_stream_commits(
    key: &str,
    stream_commits: &[CommitInfo],
    relanded_valid: &BTreeMap<String, String>,
    functional_only: bool,
) -> bool {
    let done_path = format!(".agents/tasks/done/{key}.json");
    stream_commits
        .iter()
        .filter(|c| !relanded_valid.contains_key(&c.oid))
        .any(|c| {
            (!functional_only || functional::is_functional_commit(&c.subject, &c.changed_paths))
                && (c.task_trailers.iter().any(|t| t == key)
                    || c.changed_paths
                        .iter()
                        .any(|p| p.eq_ignore_ascii_case(&done_path)))
        })
}

pub(in crate::distill_compose) struct BaseSatisfiedScan<'a> {
    pub(in crate::distill_compose) git: &'a Git<'a>,
    pub(in crate::distill_compose) base_commit: &'a str,
    pub(in crate::distill_compose) from_commit: &'a str,
    pub(in crate::distill_compose) from_tasks: &'a LoadedTasks,
    pub(in crate::distill_compose) base_tasks: &'a LoadedTasks,
    pub(in crate::distill_compose) distilled_in_base: &'a DistilledInBase,
    pub(in crate::distill_compose) stream_commits: &'a [CommitInfo],
    pub(in crate::distill_compose) relanded_valid: &'a BTreeMap<String, String>,
    pub(in crate::distill_compose) functional_only: bool,
}

pub(in crate::distill_compose) fn classify_base_satisfied_leaves(
    scan: &BaseSatisfiedScan<'_>,
) -> BTreeSet<String> {
    let BaseSatisfiedScan {
        git,
        base_commit,
        from_commit,
        from_tasks,
        base_tasks,
        distilled_in_base,
        stream_commits,
        relanded_valid,
        functional_only,
    } = *scan;
    let mut candidates: BTreeSet<String> = BTreeSet::new();
    candidates.extend(base_tasks.valid.keys().cloned());
    candidates.extend(base_tasks.done_deps.keys().cloned());
    candidates.extend(distilled_in_base.tasks.iter().cloned());

    let mut leaves = BTreeSet::new();
    for key in candidates {
        if base_tasks.done_deps.contains_key(&key)
            && !has_done_task_stream_commits(&key, stream_commits, relanded_valid, functional_only)
        {
            leaves.insert(key);
            continue;
        }
        if from_tasks.invalid.contains_key(&key) {
            continue;
        }
        if let Some(task) = from_tasks.valid.get(&key) {
            if let Ok(mut list) =
                attribute_task_commits(git, base_commit, from_commit, task, stream_commits)
            {
                list.retain(|commit| !distilled_in_base.covers_commit(commit));
                list.retain(|commit| !relanded_valid.contains_key(&commit.oid));
                if functional_only {
                    list.retain(|commit| {
                        functional::is_functional_commit(&commit.subject, &commit.changed_paths)
                    });
                }
                if list.is_empty() {
                    leaves.insert(key);
                }
            }
        } else if base_tasks.valid.contains_key(&key) || distilled_in_base.tasks.contains(&key) {
            leaves.insert(key);
        }
    }
    leaves
}

pub(in crate::distill_compose) struct NonFunctionalScan<'a> {
    pub(in crate::distill_compose) is_commercial_repo: bool,
    pub(in crate::distill_compose) requested: &'a [String],
    pub(in crate::distill_compose) from_tasks: &'a LoadedTasks,
    pub(in crate::distill_compose) base_tasks: &'a LoadedTasks,
    pub(in crate::distill_compose) distilled_in_base: &'a DistilledInBase,
    pub(in crate::distill_compose) stream_commits: &'a [CommitInfo],
    pub(in crate::distill_compose) relanded_valid: &'a BTreeMap<String, String>,
}

pub(in crate::distill_compose) fn classify_non_functional_leaves(
    scan: &NonFunctionalScan<'_>,
) -> BTreeSet<String> {
    let NonFunctionalScan {
        is_commercial_repo,
        requested,
        from_tasks,
        base_tasks,
        distilled_in_base,
        stream_commits,
        relanded_valid,
    } = *scan;
    let mut requested_upper: BTreeSet<String> = BTreeSet::new();
    let mut candidates: BTreeSet<String> = BTreeSet::new();
    for r in requested {
        let k = r.trim().to_ascii_uppercase();
        if !k.is_empty() {
            requested_upper.insert(k.clone());
            candidates.insert(k);
        }
    }
    candidates.extend(from_tasks.valid.keys().cloned());
    candidates.extend(from_tasks.invalid.keys().cloned());
    for task in from_tasks.valid.values() {
        for dep in &task.deps {
            let dep_key = dep.trim().to_ascii_uppercase();
            if !dep_key.is_empty() {
                candidates.insert(dep_key);
            }
        }
    }
    for commit in stream_commits {
        for t in &commit.task_trailers {
            candidates.insert(t.clone());
        }
        for p in &commit.changed_paths {
            if let Some(id) = functional::task_id_from_manifest_path(p) {
                candidates.insert(id);
            }
        }
    }

    let mut leaves = BTreeSet::new();
    for key in candidates {
        if base_tasks.valid.contains_key(&key)
            || base_tasks.done_deps.contains_key(&key)
            || distilled_in_base.tasks.contains(&key)
        {
            continue;
        }
        if let Some(task) = from_tasks.valid.get(&key) {
            if !task.source_commits.is_empty() {
                continue;
            }
        }
        let task_path = from_tasks.valid.get(&key).map(|t| t.path.as_str());
        let matched: Vec<&CommitInfo> = stream_commits
            .iter()
            .filter(|c| !relanded_valid.contains_key(&c.oid))
            .filter(|c| {
                c.task_trailers.iter().any(|t| t == &key)
                    || task_path.is_some_and(|tp| c.changed_paths.iter().any(|p| p == tp))
                    || c.changed_paths.iter().any(|p| {
                        functional::task_id_from_manifest_path(p).as_deref() == Some(key.as_str())
                    })
            })
            .collect();
        if !matched.is_empty() {
            if matched
                .iter()
                .all(|c| !functional::is_functional_commit(&c.subject, &c.changed_paths))
            {
                leaves.insert(key);
            }
        } else if let Some(task) = from_tasks.valid.get(&key) {
            if !task.scopes.is_empty()
                && task
                    .scopes
                    .iter()
                    .all(|s| !functional::is_functional_path(s))
            {
                leaves.insert(key);
            }
        } else if !is_commercial_repo
            && !requested_upper.contains(&key)
            && !from_tasks.invalid.contains_key(&key)
            && functional::is_cross_repo_commercial_task_id(&key)
        {
            leaves.insert(key);
        }
    }
    leaves
}

pub(in crate::distill_compose) fn check_dep_outcomes(
    task: &TaskManifest,
    outcomes: &BTreeMap<String, TaskOutcome>,
) -> Option<(&'static str, String, bool)> {
    check_dep_list_outcomes(&task.key, &task.deps, outcomes)
}

pub(in crate::distill_compose) fn check_dep_list_outcomes(
    self_key: &str,
    deps: &[String],
    outcomes: &BTreeMap<String, TaskOutcome>,
) -> Option<(&'static str, String, bool)> {
    for dep in deps {
        let dep_key = dep.trim().to_ascii_uppercase();
        if dep_key.is_empty() || dep_key == self_key {
            continue;
        }
        match outcomes.get(&dep_key).copied() {
            Some(
                TaskOutcome::Included
                | TaskOutcome::SatisfiedInBase
                | TaskOutcome::SkippedNonFunctional,
            ) => {}
            Some(TaskOutcome::Rejected) => {
                return Some((
                    "dependency_rejected",
                    format!("prerequisite task {dep} was rejected"),
                    true,
                ));
            }
            Some(TaskOutcome::Deferred) | None => {
                return Some((
                    "dependency_deferred",
                    format!("prerequisite task {dep} was deferred"),
                    false,
                ));
            }
        }
    }
    None
}
