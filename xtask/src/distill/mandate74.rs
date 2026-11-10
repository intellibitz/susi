//! CEO Mandate 74 distillation automation (`docs/ceo-mandate-74.md` §§3, 5, 10):
//!
//! 1. **Disjoint-scope lane partitioning (`partition_disjoint_lanes`)**: splits
//!    distilled task closures into deterministic parallel lanes whose file scopes
//!    are pairwise disjoint while keeping overlapping or dependent tasks ordered
//!    inside the same lane.
//! 2. **Stable Git patch-id computation (`compute_commit_patch_id`)**: computes
//!    `git patch-id --stable` for composed commits so release picks can detect
//!    already-applied patches across branches without host coordination.
//! 3. **`susi-harvest-manifest-v1` schema, Apache-2.0 §4 obligation audit, and
//!    commit trailer verification (`verify_harvest_tree_and_range`)**: enforces
//!    license allowlists at the pinned commit, modification notices, NOTICE
//!    attribution, and `Harvest:` / `Origin:` / `Origin-License:` trailer binding.
//! 4. **Quarantine upstream intake (`quarantine_fetch`)**: fetches an untrusted
//!    upstream commit into `refs/susi/upstream/<slug>/<sha>` with hooks, tags,
//!    and submodules disabled so zero upstream code executes during intake.
#![forbid(unsafe_code)]

use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

pub const HARVEST_SCHEMA: &str = "susi-harvest-manifest-v1";
pub const HARVEST_AUDIT_SCHEMA: &str = "susi-harvest-audit-v1";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaneTaskInput {
    pub task_id: String,
    pub deps: Vec<String>,
    pub scopes: Vec<String>,
    pub changed_files: Vec<String>,
    pub patch_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DisjointLane {
    pub lane_id: String,
    pub tasks: Vec<String>,
    pub scopes: Vec<String>,
    pub changed_files: Vec<String>,
    pub patch_ids: BTreeMap<String, String>,
}

impl DisjointLane {
    pub fn to_json(&self) -> Value {
        json!({
            "lane_id": self.lane_id,
            "tasks": self.tasks,
            "scopes": self.scopes,
            "changed_files": self.changed_files,
            "patch_ids": self.patch_ids,
        })
    }
}

fn is_task_record_path(path: &str) -> bool {
    let norm = path.trim().replace('\\', "/");
    let lower = norm.to_ascii_lowercase();
    lower.starts_with(".agents/tasks/") && lower.ends_with(".json")
}

fn paths_overlap(a: &str, b: &str) -> bool {
    if a.eq_ignore_ascii_case(b) {
        return true;
    }
    if a.len() > b.len() && a.as_bytes()[b.len()] == b'/' && a[..b.len()].eq_ignore_ascii_case(b) {
        return true;
    }
    if b.len() > a.len() && b.as_bytes()[a.len()] == b'/' && b[..a.len()].eq_ignore_ascii_case(a) {
        return true;
    }
    false
}

fn effective_conflict_paths(item: &LaneTaskInput) -> Vec<String> {
    let mut set = BTreeSet::new();
    for p in item.scopes.iter().chain(item.changed_files.iter()) {
        let clean = p.trim().replace('\\', "/");
        let clean = clean.trim_end_matches('/');
        if !clean.is_empty() && !is_task_record_path(clean) {
            set.insert(clean.to_owned());
        }
    }
    set.into_iter().collect()
}

fn uf_find(parent: &mut [usize], mut idx: usize) -> usize {
    while parent[idx] != idx {
        parent[idx] = parent[parent[idx]];
        idx = parent[idx];
    }
    idx
}

fn uf_union(parent: &mut [usize], a: usize, b: usize) {
    let ra = uf_find(parent, a);
    let rb = uf_find(parent, b);
    if ra != rb {
        if ra < rb {
            parent[rb] = ra;
        } else {
            parent[ra] = rb;
        }
    }
}

/// Splits ordered tasks into minimal conflict-free lanes with pairwise disjoint
/// non-task-record file scopes while keeping dependent or overlapping tasks
/// inside the same lane in their original closure order.
pub fn partition_disjoint_lanes(items: &[LaneTaskInput]) -> Vec<DisjointLane> {
    if items.is_empty() {
        return Vec::new();
    }
    let n = items.len();
    let mut parent: Vec<usize> = (0..n).collect();
    let mut key_to_idx: BTreeMap<String, usize> = BTreeMap::new();
    let mut conflict_paths: Vec<Vec<String>> = Vec::with_capacity(n);

    for (i, item) in items.iter().enumerate() {
        key_to_idx.insert(item.task_id.trim().to_ascii_uppercase(), i);
        conflict_paths.push(effective_conflict_paths(item));
    }

    for (i, item) in items.iter().enumerate() {
        for dep in &item.deps {
            let dep_key = dep.trim().to_ascii_uppercase();
            if let Some(&dep_idx) = key_to_idx.get(&dep_key) {
                uf_union(&mut parent, i, dep_idx);
            }
        }
    }

    for i in 0..n {
        for j in (i + 1)..n {
            if uf_find(&mut parent, i) == uf_find(&mut parent, j) {
                continue;
            }
            let overlaps = conflict_paths[i]
                .iter()
                .any(|pa| conflict_paths[j].iter().any(|pb| paths_overlap(pa, pb)));
            if overlaps {
                uf_union(&mut parent, i, j);
            }
        }
    }

    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for idx in 0..n {
        let root = uf_find(&mut parent, idx);
        groups.entry(root).or_default().push(idx);
    }

    let mut ordered_groups: Vec<Vec<usize>> = groups.into_values().collect();
    ordered_groups.sort_by_key(|members| members.first().copied().unwrap_or(usize::MAX));

    let mut lanes = Vec::with_capacity(ordered_groups.len());
    for (lane_num, members) in ordered_groups.into_iter().enumerate() {
        let mut tasks = Vec::with_capacity(members.len());
        let mut scopes = BTreeSet::new();
        let mut changed_files = BTreeSet::new();
        let mut patch_ids = BTreeMap::new();
        for idx in members {
            let item = &items[idx];
            tasks.push(item.task_id.clone());
            for s in &conflict_paths[idx] {
                scopes.insert(s.clone());
            }
            for f in &item.changed_files {
                let clean = f.trim().replace('\\', "/");
                if !clean.is_empty() && !is_task_record_path(&clean) {
                    changed_files.insert(clean);
                }
            }
            if let Some(ref pid) = item.patch_id {
                patch_ids.insert(item.task_id.clone(), pid.clone());
            }
        }
        lanes.push(DisjointLane {
            lane_id: format!("lane-{:02}", lane_num + 1),
            tasks,
            scopes: scopes.into_iter().collect(),
            changed_files: changed_files.into_iter().collect(),
            patch_ids,
        });
    }
    lanes
}

fn is_hex_oid(s: &str) -> bool {
    matches!(s.len(), 40 | 64) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn git_cmd_base(root: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.current_dir(root)
        .arg("--literal-pathspecs")
        .arg("--no-replace-objects")
        .env("GIT_TERMINAL_PROMPT", "0");
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_GRAFT_FILE",
    ] {
        cmd.env_remove(key);
    }
    cmd
}

/// Computes `git patch-id --stable` for a single commit's diff (`git show --pretty=format: <oid>`).
pub fn compute_commit_patch_id(root: &Path, commit_oid: &str) -> Result<Option<String>, String> {
    if !is_hex_oid(commit_oid) {
        return Err(format!("invalid commit OID for patch-id: {commit_oid}"));
    }
    let show_out = git_cmd_base(root)
        .args(["show", "--pretty=format:", "--patch", commit_oid])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("cannot run git show for patch-id: {e}"))?;
    if !show_out.status.success() {
        return Err(format!(
            "git show --patch {commit_oid} failed ({})",
            show_out.status
        ));
    }
    if show_out.stdout.is_empty() {
        return Ok(None);
    }
    let mut child = git_cmd_base(root)
        .args(["patch-id", "--stable"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot spawn git patch-id: {e}"))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "git patch-id stdin unavailable".to_owned())?;
    let patch_bytes = show_out.stdout;
    let pid_out = std::thread::scope(|scope| -> Result<std::process::Output, String> {
        let writer = scope.spawn(move || stdin.write_all(&patch_bytes));
        let out = child
            .wait_with_output()
            .map_err(|e| format!("cannot wait on git patch-id: {e}"))?;
        let wres = writer
            .join()
            .map_err(|_| "git patch-id writer thread panicked".to_owned())?;
        if out.status.success() {
            wres.map_err(|e| format!("cannot pipe diff to git patch-id: {e}"))?;
        }
        Ok(out)
    })?;
    if !pid_out.status.success() {
        return Err(format!("git patch-id --stable failed ({})", pid_out.status));
    }
    let stdout = String::from_utf8_lossy(&pid_out.stdout);
    let first_tok = stdout
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().next())
        .filter(|tok| is_hex_oid(tok))
        .map(str::to_owned);
    Ok(first_tok)
}

/// Checks whether an SPDX license expression is allowlisted under CEO Mandate 68 & Mandate 74 §5.1.
pub fn harvest_spdx_allowed(expr: &str) -> bool {
    let trimmed = expr.trim();
    if trimmed.is_empty() {
        return false;
    }
    // Normalize `Apache-2.0 WITH LLVM-exception` into a single token before splitting.
    let normalized = trimmed.replace(
        "Apache-2.0 WITH LLVM-exception",
        "Apache-2.0-WITH-LLVM-exception",
    );
    let cleaned = normalized.replace(['(', ')'], " ");
    let tokens: Vec<&str> = cleaned.split_whitespace().collect();
    if tokens.is_empty() {
        return false;
    }
    const ALLOWED: [&str; 5] = [
        "Apache-2.0",
        "Apache-2.0-WITH-LLVM-exception",
        "MIT",
        "BSD-2-Clause",
        "BSD-3-Clause",
    ];
    tokens
        .iter()
        .all(|tok| matches!(*tok, "AND" | "OR") || ALLOWED.contains(tok))
        && tokens.iter().any(|tok| ALLOWED.contains(tok))
}

fn normalize_rel_path(raw: &str) -> Result<String, String> {
    let norm = raw.trim().replace('\\', "/");
    let clean = norm.trim_end_matches('/');
    if clean.is_empty()
        || clean.starts_with('/')
        || clean.chars().any(char::is_control)
        || clean
            .split('/')
            .any(|seg| seg.is_empty() || seg == "." || seg == "..")
    {
        return Err(format!("invalid relative path '{raw}'"));
    }
    Ok(clean.to_owned())
}

fn valid_harvest_id(id: &str) -> bool {
    let Some(rest) = id.strip_prefix("H-") else {
        return false;
    };
    let mut parts = rest.rsplitn(2, '-');
    let Some(num) = parts.next() else {
        return false;
    };
    let Some(agent) = parts.next() else {
        return false;
    };
    !agent.is_empty()
        && !num.is_empty()
        && num.bytes().all(|b| b.is_ascii_digit())
        && agent
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn valid_source_url(url: &str) -> bool {
    let trimmed = url.trim();
    if trimmed.is_empty() || trimmed.chars().any(char::is_control) || trimmed.contains(' ') {
        return false;
    }
    // Reject embedded user:password@ credentials in http(s) URLs.
    if let Some(after_scheme) = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .or_else(|| trimmed.strip_prefix("ssh://"))
        .or_else(|| trimmed.strip_prefix("git://"))
    {
        let authority = after_scheme.split('/').next().unwrap_or("");
        if authority.is_empty() || authority.contains('@') {
            return false;
        }
        return true;
    }
    false
}

#[derive(Clone, Debug)]
pub struct HarvestManifest {
    pub id: String,
    pub source_url: String,
    pub source_commit: String,
    pub source_paths: Vec<String>,
    pub destination_paths: Vec<String>,
    pub spdx_license: String,
    pub license_sha256: String,
    pub notice_sha256: Option<String>,
    pub copyright_holders: Vec<String>,
    pub notice_text: Option<String>,
    pub import_mode: String,
    pub modified: bool,
    pub modification_summary: String,
    pub agent: String,
    pub date: String,
    pub task_id: String,
    pub feature_id: Option<String>,
}

impl HarvestManifest {
    pub fn to_summary_json(&self) -> Value {
        json!({
            "id": self.id,
            "source_url": self.source_url,
            "source_commit": self.source_commit,
            "source_paths": self.source_paths,
            "destination_paths": self.destination_paths,
            "spdx_license": self.spdx_license,
            "license_sha256": self.license_sha256,
            "notice_sha256": self.notice_sha256,
            "copyright_holders": self.copyright_holders,
            "import_mode": self.import_mode,
            "modified": self.modified,
            "modification_summary": self.modification_summary,
            "agent": self.agent,
            "date": self.date,
            "task_id": self.task_id,
            "feature_id": self.feature_id,
        })
    }
}

fn req_str(val: &Value, key: &str) -> Result<String, String> {
    val.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| format!("missing non-empty string field '{key}'"))
}

fn req_str_vec(val: &Value, key: &str) -> Result<Vec<String>, String> {
    let arr = val
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("missing string array field '{key}'"))?;
    if arr.is_empty() {
        return Err(format!("array field '{key}' must not be empty"));
    }
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let s = item
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| format!("array field '{key}' contains empty or non-string element"))?;
        out.push(s.to_owned());
    }
    Ok(out)
}

pub fn parse_harvest_manifest(path: &str, bytes: &[u8]) -> Result<HarvestManifest, String> {
    let val: Value =
        serde_json::from_slice(bytes).map_err(|e| format!("{path}: invalid JSON: {e}"))?;
    if val.get("schema").and_then(Value::as_str) != Some(HARVEST_SCHEMA) {
        return Err(format!("{path}: schema must equal '{HARVEST_SCHEMA}'"));
    }
    let id = req_str(&val, "id").map_err(|e| format!("{path}: {e}"))?;
    if !valid_harvest_id(&id) {
        return Err(format!(
            "{path}: invalid harvest id '{id}' (expected H-<AGENT>-<N>)"
        ));
    }
    let source_url = req_str(&val, "source_url").map_err(|e| format!("{path}: {e}"))?;
    if !valid_source_url(&source_url) {
        return Err(format!(
            "{path}: invalid or credential-bearing source_url '{source_url}'"
        ));
    }
    let source_commit = req_str(&val, "source_commit").map_err(|e| format!("{path}: {e}"))?;
    if !is_hex_oid(&source_commit) {
        return Err(format!(
            "{path}: source_commit must be a full 40- or 64-hex commit SHA"
        ));
    }
    // Support both canonical `local-harvest` `files[]` (`{source_path, dest_path, ...}`)
    // and explicit `source_paths` / `destination_paths` arrays.
    let (raw_src, raw_dst) = if let Some(files) = val.get("files").and_then(Value::as_array) {
        if files.is_empty() {
            return Err(format!("{path}: array field 'files' must not be empty"));
        }
        let mut srcs = Vec::with_capacity(files.len());
        let mut dsts = Vec::with_capacity(files.len());
        for (idx, item) in files.iter().enumerate() {
            let sp =
                req_str(item, "source_path").map_err(|e| format!("{path}: files[{idx}]: {e}"))?;
            let dp =
                req_str(item, "dest_path").map_err(|e| format!("{path}: files[{idx}]: {e}"))?;
            srcs.push(sp);
            dsts.push(dp);
        }
        (srcs, dsts)
    } else {
        let s = req_str_vec(&val, "source_paths").map_err(|e| format!("{path}: {e}"))?;
        let d = req_str_vec(&val, "destination_paths").map_err(|e| format!("{path}: {e}"))?;
        (s, d)
    };
    let mut source_paths = Vec::with_capacity(raw_src.len());
    for sp in &raw_src {
        source_paths.push(normalize_rel_path(sp).map_err(|e| format!("{path}: {e}"))?);
    }
    let mut destination_paths = Vec::with_capacity(raw_dst.len());
    for dp in &raw_dst {
        destination_paths.push(normalize_rel_path(dp).map_err(|e| format!("{path}: {e}"))?);
    }
    let spdx_license = req_str(&val, "license")
        .or_else(|_| req_str(&val, "spdx_license"))
        .map_err(|e| format!("{path}: {e}"))?;
    if !harvest_spdx_allowed(&spdx_license) {
        return Err(format!(
            "{path}: disallowed_license: SPDX expression '{spdx_license}' is forbidden by Mandate 74 §5.1"
        ));
    }
    let license_sha256 = if let Ok(explicit) = req_str(&val, "license_sha256") {
        if !is_sha256_hex(&explicit) {
            return Err(format!(
                "{path}: license_sha256 must be a 64-character hex SHA-256 digest"
            ));
        }
        explicit
    } else if let Some(ev_arr) = val.get("license_evidence").and_then(Value::as_array) {
        let first_sha = ev_arr
            .iter()
            .filter(|ev| ev.get("kind").and_then(Value::as_str) == Some("license"))
            .find_map(|ev| ev.get("sha256").and_then(Value::as_str))
            .or_else(|| {
                ev_arr
                    .first()
                    .and_then(|ev| ev.get("sha256").and_then(Value::as_str))
            })
            .map(str::trim)
            .filter(|s| is_sha256_hex(s))
            .map(str::to_owned);
        first_sha.unwrap_or_else(|| "0".repeat(64))
    } else {
        return Err(format!(
            "{path}: missing 'license_sha256' or 'license_evidence[].sha256'"
        ));
    };
    let notice_sha256 = match val.get("notice_sha256").and_then(Value::as_str) {
        Some(s) if !s.trim().is_empty() => {
            let trimmed = s.trim();
            if !is_sha256_hex(trimmed) {
                return Err(format!(
                    "{path}: notice_sha256 must be a 64-character hex SHA-256 digest"
                ));
            }
            Some(trimmed.to_owned())
        }
        _ => None,
    };
    let copyright_holders = if val.get("copyright_lines").is_some() {
        val.get("copyright_lines")
            .and_then(Value::as_array)
            .ok_or_else(|| format!("{path}: 'copyright_lines' must be an array"))?
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect()
    } else {
        req_str_vec(&val, "copyright_holders").map_err(|e| format!("{path}: {e}"))?
    };
    let notice_text = val
        .get("notice_text")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let import_mode = req_str(&val, "mode")
        .or_else(|_| req_str(&val, "import_mode"))
        .map_err(|e| format!("{path}: {e}"))?;
    if !matches!(
        import_mode.as_str(),
        "transplant" | "port" | "cherry-pick" | "format-patch" | "source-transplant" | "rust-port"
    ) {
        return Err(format!(
            "{path}: mode '{import_mode}' must be transplant, port, cherry-pick, format-patch, source-transplant or rust-port"
        ));
    }
    let modification_summary = val
        .get("modifications")
        .and_then(Value::as_str)
        .map(str::trim)
        .map(str::to_owned)
        .or_else(|| req_str(&val, "modification_summary").ok())
        .ok_or_else(|| format!("{path}: missing 'modifications' or 'modification_summary'"))?;
    let default_modified =
        matches!(import_mode.as_str(), "port" | "rust-port") || !modification_summary.is_empty();
    let modified = val
        .get("modified")
        .and_then(Value::as_bool)
        .unwrap_or(default_modified);
    let agent = req_str(&val, "harvested_by")
        .or_else(|_| req_str(&val, "agent"))
        .map_err(|e| format!("{path}: {e}"))?;
    let date = if let Some(u) = val.get("harvested_unix").and_then(Value::as_u64) {
        u.to_string()
    } else {
        req_str(&val, "date").map_err(|e| format!("{path}: {e}"))?
    };
    let task_id = req_str(&val, "task")
        .or_else(|_| req_str(&val, "task_id"))
        .map_err(|e| format!("{path}: {e}"))?;
    let feature_id = val
        .get("feature")
        .or_else(|| val.get("feature_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);

    Ok(HarvestManifest {
        id,
        source_url,
        source_commit,
        source_paths,
        destination_paths,
        spdx_license,
        license_sha256,
        notice_sha256,
        copyright_holders,
        notice_text,
        import_mode,
        modified,
        modification_summary,
        agent,
        date,
        task_id,
        feature_id,
    })
}

enum TreeReader<'a> {
    Workdir(&'a Path),
    GitRev { root: &'a Path, rev: &'a str },
}

type RawManifestList = Vec<(String, Vec<u8>)>;

impl TreeReader<'_> {
    fn read_blob(&self, rel_path: &str) -> Option<Vec<u8>> {
        match self {
            TreeReader::Workdir(root) => fs::read(root.join(rel_path)).ok(),
            TreeReader::GitRev { root, rev } => {
                let spec = format!("{rev}:{rel_path}");
                let out = git_cmd_base(root)
                    .args(["cat-file", "blob", &spec])
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .output()
                    .ok()?;
                if out.status.success() {
                    Some(out.stdout)
                } else {
                    None
                }
            }
        }
    }

    fn list_harvest_manifests(&self) -> Result<RawManifestList, String> {
        match self {
            TreeReader::Workdir(root) => {
                let dir = root.join(".agents/harvest");
                if !dir.exists() {
                    return Ok(Vec::new());
                }
                let mut entries = Vec::new();
                let rd =
                    fs::read_dir(&dir).map_err(|e| format!("cannot read .agents/harvest: {e}"))?;
                for entry in rd {
                    let entry = entry.map_err(|e| e.to_string())?;
                    let path = entry.path();
                    if path.extension().is_some_and(|ext| ext == "json") {
                        let name = entry.file_name().to_string_lossy().into_owned();
                        let rel = format!(".agents/harvest/{name}");
                        let bytes = fs::read(&path)
                            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
                        entries.push((rel, bytes));
                    }
                }
                entries.sort_by(|a, b| a.0.cmp(&b.0));
                Ok(entries)
            }
            TreeReader::GitRev { root, rev } => {
                let out = git_cmd_base(root)
                    .args(["ls-tree", "-r", "-z", "--name-only", rev, ".agents/harvest"])
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .output()
                    .map_err(|e| format!("cannot run git ls-tree .agents/harvest: {e}"))?;
                if !out.status.success() || out.stdout.is_empty() {
                    return Ok(Vec::new());
                }
                let mut paths: Vec<String> = String::from_utf8_lossy(&out.stdout)
                    .split('\0')
                    .filter(|p| p.ends_with(".json"))
                    .map(str::to_owned)
                    .collect();
                paths.sort();
                let mut res = Vec::with_capacity(paths.len());
                for rel in paths {
                    if let Some(bytes) = self.read_blob(&rel) {
                        res.push((rel, bytes));
                    }
                }
                Ok(res)
            }
        }
    }
}

fn verify_manifest_obligations(
    reader: &TreeReader<'_>,
    manifest_path: &str,
    m: &HarvestManifest,
) -> Vec<String> {
    let mut findings = Vec::new();
    let has_license_file = reader.read_blob("LICENSE").is_some()
        || reader.read_blob("LICENSES/Apache-2.0.txt").is_some();
    if !has_license_file {
        findings.push(format!(
            "{manifest_path} ({}) Apache-2.0 §4(a): repository root must ship LICENSE or LICENSES/Apache-2.0.txt",
            m.id
        ));
    }

    for dst in &m.destination_paths {
        match reader.read_blob(dst) {
            None => findings.push(format!(
                "{manifest_path} ({}) destination path '{dst}' does not exist in tree",
                m.id
            )),
            Some(bytes) => {
                if m.modified {
                    let text = String::from_utf8_lossy(&bytes);
                    let has_notice = text
                        .lines()
                        .any(|line| line.to_ascii_lowercase().contains("modified by"));
                    if !has_notice {
                        findings.push(format!(
                            "{manifest_path} ({}) Apache-2.0 §4(b): modified destination '{dst}' lacks prominent 'Modified by' notice",
                            m.id
                        ));
                    }
                }
            }
        }
    }

    if let Some(ref notice_snippet) = m.notice_text {
        let notice_doc = reader
            .read_blob("THIRD_PARTY_NOTICES")
            .or_else(|| reader.read_blob("NOTICE"));
        match notice_doc {
            None => findings.push(format!(
                "{manifest_path} ({}) Apache-2.0 §4(d): upstream notice_text requires NOTICE or THIRD_PARTY_NOTICES in tree",
                m.id
            )),
            Some(bytes) => {
                let text = String::from_utf8_lossy(&bytes);
                if !text.contains(notice_snippet.as_str()) {
                    findings.push(format!(
                        "{manifest_path} ({}) Apache-2.0 §4(d): NOTICE / THIRD_PARTY_NOTICES does not include recorded notice_text attribution",
                        m.id
                    ));
                }
            }
        }
    }
    findings
}

fn extract_trailer_values(body: &str, key_lower: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in body.lines() {
        if let Some((k, v)) = line.split_once(':') {
            if k.trim().eq_ignore_ascii_case(key_lower) {
                let val = v.trim();
                if !val.is_empty() {
                    out.push(val.to_owned());
                }
            }
        }
    }
    out
}

fn parse_harvest_commit_records(
    raw: &str,
    known_manifests: &BTreeMap<String, HarvestManifest>,
) -> (usize, Vec<String>) {
    let mut harvest_commits = 0usize;
    let mut findings = Vec::new();

    for rec in raw.split('\u{1e}') {
        let rec = rec.trim_matches(['\n', '\r']);
        if rec.is_empty() {
            continue;
        }
        let mut parts = rec.splitn(3, '\u{1f}');
        let Some(oid) = parts.next() else { continue };
        let subject = parts.next().unwrap_or("");
        let body = parts.next().unwrap_or("");
        let harvests = extract_trailer_values(body, "harvest");
        let origins = extract_trailer_values(body, "origin");
        let licenses = extract_trailer_values(body, "origin-license");

        if harvests.is_empty() && origins.is_empty() {
            continue;
        }
        harvest_commits += 1;

        if harvests.is_empty() {
            findings.push(format!(
                "commit {oid} ('{subject}') carries Origin: trailer without Harvest: H-<AGENT>-<N> trailer"
            ));
        }
        if origins.is_empty() {
            findings.push(format!(
                "commit {oid} ('{subject}') carries Harvest: trailer without Origin: <url>@<sha> trailer"
            ));
        }
        if licenses.is_empty() {
            findings.push(format!(
                "commit {oid} ('{subject}') carries Harvest:/Origin: trailer without Origin-License: trailer"
            ));
        } else {
            for lic in &licenses {
                if !harvest_spdx_allowed(lic) {
                    findings.push(format!(
                        "commit {oid} ('{subject}') declares disallowed Origin-License '{lic}'"
                    ));
                }
            }
        }
        for hid in &harvests {
            let key = hid.to_ascii_uppercase();
            if !known_manifests.contains_key(&key) {
                findings.push(format!(
                    "commit {oid} ('{subject}') references Harvest: {hid}, which has no valid manifest under .agents/harvest/"
                ));
            }
        }
    }
    (harvest_commits, findings)
}

fn scan_commit_range_harvest_trailers(
    root: &Path,
    range: &str,
    known_manifests: &BTreeMap<String, HarvestManifest>,
) -> Result<(usize, Vec<String>), String> {
    let out = git_cmd_base(root)
        .args(["log", "--no-merges", "--format=%x1e%H%x1f%s%x1f%B", range])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("cannot scan commit range {range}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git log {range} failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let raw = String::from_utf8_lossy(&out.stdout);
    Ok(parse_harvest_commit_records(&raw, known_manifests))
}

fn scan_explicit_commits_harvest_trailers(
    root: &Path,
    oids: &[String],
    known_manifests: &BTreeMap<String, HarvestManifest>,
) -> Result<(usize, Vec<String>), String> {
    if oids.is_empty() {
        return Ok((0, Vec::new()));
    }
    let mut cmd = git_cmd_base(root);
    cmd.args(["log", "--no-walk", "--format=%x1e%H%x1f%s%x1f%B"]);
    for oid in oids {
        cmd.arg(oid);
    }
    let out = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("cannot inspect source commits for harvest trailers: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git log --no-walk failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let raw = String::from_utf8_lossy(&out.stdout);
    Ok(parse_harvest_commit_records(&raw, known_manifests))
}

#[derive(Clone, Debug)]
pub struct HarvestAuditReport {
    pub passed: bool,
    pub manifests_verified: usize,
    pub harvest_commits_checked: usize,
    pub manifest_ids: Vec<String>,
    pub manifests: Vec<Value>,
    pub findings: Vec<String>,
}

impl HarvestAuditReport {
    pub fn to_json(&self) -> Value {
        json!({
            "schema": HARVEST_AUDIT_SCHEMA,
            "passed": self.passed,
            "manifests_verified": self.manifests_verified,
            "harvest_commits_checked": self.harvest_commits_checked,
            "manifest_ids": self.manifest_ids,
            "manifests": self.manifests,
            "findings": self.findings,
        })
    }
}

type LoadedManifests = (BTreeMap<String, HarvestManifest>, Vec<String>);

fn load_and_verify_manifests(reader: &TreeReader<'_>) -> Result<LoadedManifests, String> {
    let raw_manifests = reader.list_harvest_manifests()?;
    let mut findings = Vec::new();
    let mut known: BTreeMap<String, HarvestManifest> = BTreeMap::new();

    for (rel_path, bytes) in &raw_manifests {
        match parse_harvest_manifest(rel_path, bytes) {
            Ok(m) => {
                let key = m.id.to_ascii_uppercase();
                let expected_stem = Path::new(rel_path)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("");
                if !expected_stem.eq_ignore_ascii_case(&m.id) {
                    findings.push(format!(
                        "{rel_path}: filename stem '{expected_stem}' must match manifest id '{}'",
                        m.id
                    ));
                }
                findings.extend(verify_manifest_obligations(reader, rel_path, &m));
                if let std::collections::btree_map::Entry::Vacant(slot) = known.entry(key) {
                    slot.insert(m);
                } else {
                    findings.push(format!("{rel_path}: duplicate harvest id '{}'", m.id));
                }
            }
            Err(err) => findings.push(err),
        }
    }
    Ok((known, findings))
}

/// Audits `.agents/harvest/*.json` manifests and Apache-2.0 §4 obligations in
/// either the working tree (`rev = None`) or a Git revision (`rev = Some(oid)`),
/// plus optional commit-range trailer binding (`range = Some("A..B")`).
pub fn verify_harvest_tree_and_range(
    root: &Path,
    rev: Option<&str>,
    range: Option<&str>,
) -> Result<HarvestAuditReport, String> {
    let reader = match rev {
        Some(r) => TreeReader::GitRev { root, rev: r },
        None => TreeReader::Workdir(root),
    };
    let (known, mut findings) = load_and_verify_manifests(&reader)?;

    let harvest_commits_checked = if let Some(rng) = range {
        let (count, trailer_findings) = scan_commit_range_harvest_trailers(root, rng, &known)?;
        findings.extend(trailer_findings);
        count
    } else {
        0
    };

    let manifest_ids: Vec<String> = known.values().map(|m| m.id.clone()).collect();
    let manifests: Vec<Value> = known
        .values()
        .map(HarvestManifest::to_summary_json)
        .collect();
    Ok(HarvestAuditReport {
        passed: findings.is_empty(),
        manifests_verified: known.len(),
        harvest_commits_checked,
        manifest_ids,
        manifests,
        findings,
    })
}

/// Audits `.agents/harvest/*.json` in `composed_rev` together with the exact
/// `source_commit_oids` selected into a distillation candidate.
pub fn verify_harvest_for_commits(
    root: &Path,
    composed_rev: &str,
    source_commit_oids: &[String],
) -> Result<HarvestAuditReport, String> {
    let reader = TreeReader::GitRev {
        root,
        rev: composed_rev,
    };
    let (known, mut findings) = load_and_verify_manifests(&reader)?;

    let (harvest_commits_checked, trailer_findings) =
        scan_explicit_commits_harvest_trailers(root, source_commit_oids, &known)?;
    findings.extend(trailer_findings);

    let manifest_ids: Vec<String> = known.values().map(|m| m.id.clone()).collect();
    let manifests: Vec<Value> = known
        .values()
        .map(HarvestManifest::to_summary_json)
        .collect();
    Ok(HarvestAuditReport {
        passed: findings.is_empty(),
        manifests_verified: known.len(),
        harvest_commits_checked,
        manifest_ids,
        manifests,
        findings,
    })
}

fn valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= 64
        && !slug.starts_with(['-', '.'])
        && !slug.ends_with(['-', '.'])
        && !slug.contains("..")
        && slug.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'.' | b'_')
        })
}

/// Quarantine upstream fetch (`docs/ceo-mandate-74.md` §5.2 item 1): fetches a
/// pinned commit SHA into `refs/susi/upstream/<slug>/<sha>` with tags,
/// submodules, and hooks disabled so zero upstream code executes.
pub fn quarantine_fetch(
    root: &Path,
    url: &str,
    sha: &str,
    slug: &str,
    json_output: bool,
) -> Result<String, String> {
    if !valid_source_url(url) && !Path::new(url).exists() {
        return Err(format!(
            "invalid or credential-bearing upstream URL '{url}'"
        ));
    }
    if !is_hex_oid(sha) {
        return Err("quarantine-fetch requires a full 40- or 64-hex commit SHA".to_owned());
    }
    if !valid_slug(slug) {
        return Err(format!(
            "invalid --slug '{slug}' (expected lowercase [a-z0-9._-], 1..=64 chars)"
        ));
    }
    let common_out = git_cmd_base(root)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .output()
        .map_err(|e| format!("cannot resolve git-common-dir: {e}"))?;
    if !common_out.status.success() {
        return Err("cannot resolve git-common-dir".to_owned());
    }
    let common_dir = String::from_utf8_lossy(&common_out.stdout)
        .trim()
        .to_owned();
    let empty_hooks = Path::new(&common_dir)
        .join("susi-distill")
        .join("empty-hooks");
    fs::create_dir_all(&empty_hooks)
        .map_err(|e| format!("cannot create quarantine empty-hooks dir: {e}"))?;

    let target_ref = format!("refs/susi/upstream/{slug}/{sha}");
    let refspec = format!("{sha}:{target_ref}");
    let hooks_cfg = format!("core.hooksPath={}", empty_hooks.display());
    let out = git_cmd_base(root)
        .args([
            "-c",
            &hooks_cfg,
            "fetch",
            "--no-tags",
            "--no-write-fetch-head",
            "--recurse-submodules=no",
            "--",
            url,
            &refspec,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("cannot run quarantine git fetch: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "quarantine fetch failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let verify_out = git_cmd_base(root)
        .args(["rev-parse", "--verify", &format!("{target_ref}^{{commit}}")])
        .output()
        .map_err(|e| format!("cannot verify quarantined ref: {e}"))?;
    let resolved = String::from_utf8_lossy(&verify_out.stdout)
        .trim()
        .to_owned();
    if !verify_out.status.success() || resolved != sha {
        return Err(format!(
            "quarantine ref {target_ref} resolved to '{resolved}', expected '{sha}'"
        ));
    }
    if json_output {
        return serde_json::to_string_pretty(&json!({
            "schema": "susi-quarantine-fetch-v1",
            "status": "quarantined",
            "slug": slug,
            "commit": sha,
            "quarantine_ref": target_ref,
            "hooks_disabled": true,
        }))
        .map_err(|e| e.to_string());
    }
    Ok(format!(
        "Quarantined upstream commit {sha} at {target_ref} (hooks/tags/submodules disabled)."
    ))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn disjoint_scopes_split_into_independent_lanes_ignoring_task_self_records() {
        let items = vec![
            LaneTaskInput {
                task_id: "T-ALPHA-1".to_owned(),
                deps: vec![],
                scopes: vec![
                    ".agents/tasks/T-ALPHA-1.json".to_owned(),
                    "crates/susi-gmcp/src/protocol.rs".to_owned(),
                ],
                changed_files: vec![
                    ".agents/tasks/done/T-ALPHA-1.json".to_owned(),
                    "crates/susi-gmcp/src/protocol.rs".to_owned(),
                ],
                patch_id: Some("aaaa1111aaaa1111aaaa1111aaaa1111aaaa1111".to_owned()),
            },
            LaneTaskInput {
                task_id: "T-BETA-1".to_owned(),
                deps: vec![],
                scopes: vec![
                    ".agents/tasks/T-BETA-1.json".to_owned(),
                    "crates/susi-gemi/src/router.rs".to_owned(),
                ],
                changed_files: vec![
                    ".agents/tasks/done/T-BETA-1.json".to_owned(),
                    "crates/susi-gemi/src/router.rs".to_owned(),
                ],
                patch_id: Some("bbbb2222bbbb2222bbbb2222bbbb2222bbbb2222".to_owned()),
            },
            LaneTaskInput {
                task_id: "T-ALPHA-2".to_owned(),
                deps: vec![],
                scopes: vec![
                    ".agents/tasks/T-ALPHA-2.json".to_owned(),
                    "crates/susi-gmcp".to_owned(),
                ],
                changed_files: vec!["crates/susi-gmcp/src/server.rs".to_owned()],
                patch_id: None,
            },
        ];

        let lanes = partition_disjoint_lanes(&items);
        assert_eq!(lanes.len(), 2);
        assert_eq!(lanes[0].lane_id, "lane-01");
        assert_eq!(lanes[0].tasks, vec!["T-ALPHA-1", "T-ALPHA-2"]);
        assert_eq!(lanes[1].lane_id, "lane-02");
        assert_eq!(lanes[1].tasks, vec!["T-BETA-1"]);
    }

    #[test]
    fn explicit_dependency_merges_disjoint_file_tasks_into_single_ordered_lane() {
        let items = vec![
            LaneTaskInput {
                task_id: "T-BASE-1".to_owned(),
                deps: vec![],
                scopes: vec!["crates/susi-paths".to_owned()],
                changed_files: vec!["crates/susi-paths/src/lib.rs".to_owned()],
                patch_id: None,
            },
            LaneTaskInput {
                task_id: "T-CONSUMER-1".to_owned(),
                deps: vec!["T-BASE-1".to_owned()],
                scopes: vec!["crates/susi-harness".to_owned()],
                changed_files: vec!["crates/susi-harness/src/lib.rs".to_owned()],
                patch_id: None,
            },
        ];
        let lanes = partition_disjoint_lanes(&items);
        assert_eq!(lanes.len(), 1);
        assert_eq!(lanes[0].tasks, vec!["T-BASE-1", "T-CONSUMER-1"]);
    }

    #[test]
    fn harvest_spdx_allowlist_accepts_apache2_variants_and_rejects_copyleft_or_custom() {
        assert!(harvest_spdx_allowed("Apache-2.0"));
        assert!(harvest_spdx_allowed("Apache-2.0 OR MIT"));
        assert!(harvest_spdx_allowed("MIT OR Apache-2.0"));
        assert!(harvest_spdx_allowed("Apache-2.0 WITH LLVM-exception"));
        assert!(harvest_spdx_allowed("BSD-3-Clause AND MIT"));

        assert!(!harvest_spdx_allowed(""));
        assert!(!harvest_spdx_allowed("GPL-3.0-only"));
        assert!(!harvest_spdx_allowed("AGPL-3.0-or-later"));
        assert!(!harvest_spdx_allowed("MPL-2.0"));
        assert!(!harvest_spdx_allowed("Apache-2.0 OR GPL-2.0-only"));
        assert!(!harvest_spdx_allowed("BUSL-1.1"));
        assert!(!harvest_spdx_allowed("SSPL-1.0"));
        assert!(!harvest_spdx_allowed("UNLICENSED"));
    }

    #[test]
    fn harvest_manifest_parser_enforces_apache2_section4_and_rejects_bad_inputs() {
        let tmp =
            std::env::temp_dir().join(format!("susi-mandate74-harvest-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join(".agents/harvest")).unwrap();
        fs::create_dir_all(tmp.join("crates/demo/src")).unwrap();
        fs::write(tmp.join("LICENSE"), "Apache License 2.0\n").unwrap();
        fs::write(
            tmp.join("THIRD_PARTY_NOTICES"),
            "Portions Copyright 2026 Example Project Contributors.\n",
        )
        .unwrap();
        fs::write(
            tmp.join("crates/demo/src/ported.rs"),
            "// Modified by Susi contributors, 2026-11-11: adapted to std-only.\npub fn demo() {}\n",
        )
        .unwrap();
        let manifest = json!({
            "schema": HARVEST_SCHEMA,
            "id": "H-CARBON-1",
            "source_url": "https://codeberg.org/example/project.git",
            "source_commit": "0123456789abcdef0123456789abcdef01234567",
            "source_paths": ["src/ported.rs"],
            "destination_paths": ["crates/demo/src/ported.rs"],
            "spdx_license": "Apache-2.0",
            "license_sha256": "c71d239df91726fc519c6eb72d318ec65820627232b2f796219e87dcf35d0ab4",
            "copyright_holders": ["Example Project Contributors"],
            "notice_text": "Portions Copyright 2026 Example Project Contributors.",
            "import_mode": "source-transplant",
            "modified": true,
            "modification_summary": "Adapted to std-only.",
            "agent": "carbon-cachyos-distill",
            "date": "2026-11-11",
            "task_id": "T-CARBONDISTILL-2"
        });
        fs::write(
            tmp.join(".agents/harvest/H-CARBON-1.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();

        let ok_report = verify_harvest_tree_and_range(&tmp, None, None).unwrap();
        assert!(ok_report.passed, "{:?}", ok_report.findings);
        assert_eq!(ok_report.manifests_verified, 1);

        // Strip modification notice -> Apache-2.0 §4(b) failure
        fs::write(tmp.join("crates/demo/src/ported.rs"), "pub fn demo() {}\n").unwrap();
        let bad_report = verify_harvest_tree_and_range(&tmp, None, None).unwrap();
        assert!(!bad_report.passed);
        assert!(bad_report.findings[0].contains("Apache-2.0 §4(b)"));

        let _ = fs::remove_dir_all(&tmp);
    }
}
