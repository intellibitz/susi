//! Plan-only release distillation (T-GROKDISTILL-1).
//!
//! Reads `.agents/tasks/**.json` at a Git revision (never the working tree),
//! takes the dependency closure of the requested task ids, checks status,
//! provenance and evidence, reports scope overlaps, and emits a deterministic
//! included / deferred / rejected manifest. It composes no branch, runs no
//! acceptance command and qualifies nothing: those steps belong to the
//! release tooling that consumes this plan (see docs/distillation.md).
//! Bounded: record count, record size and Git output are capped; no network.
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};

#[path = "distill/hostmerge.rs"]
pub mod hostmerge;
#[path = "distill/lint.rs"]
mod lint;

type Result<T> = std::result::Result<T, String>;

pub const SCHEMA: &str = "susi-distill-plan/v1";
const TASK_DIR: &str = ".agents/tasks";
const MAX_RECORDS: usize = 50_000;
const MAX_RECORD_BYTES: usize = 1 << 20;
const MAX_GIT_OUTPUT: usize = 256 << 20;
const MAX_REQUESTED: usize = 10_000;
const MAX_LISTED: usize = 200;
const NOT_IMPLEMENTED: &str = "Plan only. Not implemented here: branch composition (merge or cherry-pick onto a release branch), conflict resolution, accept.cmd execution, gate runs, qualification, receipts and tag publication.";

/// Where a record lives; the derived order is the precedence order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Location {
    Done,
    Open,
    InProgress,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Closed,
    Implemented,
    Open,
    Abandoned,
}

impl Status {
    fn label(self) -> &'static str {
        match self {
            Status::Closed => "closed",
            Status::Implemented => "implemented",
            Status::Open => "open",
            Status::Abandoned => "abandoned",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub id: String,
    pub path: String,
    pub location: Location,
    pub status: Status,
    pub raw_status: String,
    pub deps: Vec<String>,
    pub scopes: Vec<String>,
    pub accept: Vec<String>,
    pub closed_commit: Option<String>,
    pub evidence: Vec<String>,
    pub title: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Included,
    Deferred,
    Rejected,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Kind::Included => "included",
            Kind::Deferred => "deferred",
            Kind::Rejected => "rejected",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    pub kind: Kind,
    pub reasons: BTreeSet<String>,
    pub notes: BTreeSet<String>,
    pub commits: Vec<String>,
    pub record: Option<Record>,
    pub requested: bool,
}

/// Facts observed in Git for the base revision. Filled by the Git layer,
/// supplied directly by tests.
#[derive(Clone, Debug, Default)]
pub struct Facts {
    /// Task id -> commits in base history whose message has a `Task:` trailer.
    pub trailer_commits: BTreeMap<String, BTreeSet<String>>,
    /// Closed-record commits verified to be ancestors of (or equal to) base.
    pub reachable: BTreeSet<String>,
    /// Evidence paths verified to exist in the base tree.
    pub existing_paths: BTreeSet<String>,
    /// Commits of the planned tasks that the Git host created instead of Susi
    /// (commit id -> `committer <email>`), see `hostmerge`.
    pub host_created: BTreeMap<String, String>,
    /// Host-created commits verifiably re-landed through Susi (host -> replay).
    pub relanded: BTreeMap<String, String>,
    /// Re-land claims that did not verify, as warnings.
    pub reland_warnings: BTreeSet<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conflict {
    pub kind: &'static str,
    pub a: String,
    pub b: String,
    pub paths: Vec<String>,
    pub resolution: &'static str,
}

#[derive(Clone, Debug)]
pub struct Plan {
    pub requested: Vec<String>,
    pub outcomes: BTreeMap<String, Outcome>,
    pub conflicts: Vec<Conflict>,
    pub warnings: BTreeSet<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Defer a task whose scope overlaps a task outside the selection that is
    /// already in base history (composition without the rest of base).
    pub isolate: bool,
    /// Host-created commits the planning agent explicitly adopts (full ids).
    /// Each adoption is listed in the manifest.
    pub adopt_host_commits: BTreeSet<String>,
}

// ---------------------------------------------------------------- parsing

fn location_of(path: &str) -> Option<Location> {
    let rest = path.strip_prefix(TASK_DIR)?.strip_prefix('/')?;
    if !rest.ends_with(".json") {
        return None;
    }
    if let Some(name) = rest.strip_prefix("done/") {
        return (!name.contains('/')).then_some(Location::Done);
    }
    if let Some(name) = rest.strip_prefix("in-progress/") {
        return (!name.contains('/')).then_some(Location::InProgress);
    }
    (!rest.contains('/')).then_some(Location::Open)
}

fn norm_status(text: &str) -> String {
    text.trim().to_ascii_lowercase().replace('-', "_")
}

fn classify(location: Location, closed: bool, labels: &[String]) -> Status {
    if location == Location::Done || closed {
        return Status::Closed;
    }
    let has = |set: &[&str]| labels.iter().any(|l| set.contains(&l.as_str()));
    if has(&["done", "closed", "completed"]) {
        Status::Closed
    } else if has(&[
        "failed",
        "canceled",
        "cancelled",
        "abandoned",
        "rejected",
        "retired",
    ]) {
        Status::Abandoned
    } else if has(&[
        "implemented",
        "verified",
        "implemented_pending_integration",
        "verified_pending_integration",
        "ready_for_integration",
        "implemented_pending_integration_and_production_qualification",
    ]) {
        Status::Implemented
    } else {
        Status::Open
    }
}

fn strings(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| match item {
                Value::String(s) => Some(s.clone()),
                Value::Object(o) => o
                    .get("task")
                    .or_else(|| o.get("id"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                Value::Null | Value::Bool(_) | Value::Number(_) | Value::Array(_) => None,
            })
            .collect(),
        Some(Value::Null | Value::Bool(_) | Value::Number(_) | Value::Object(_)) | None => {
            Vec::new()
        }
    }
}

fn norm_path(path: &str) -> Option<String> {
    let p = path.trim().trim_start_matches("./").trim_end_matches('/');
    (!p.is_empty() && !p.contains(char::is_whitespace)).then(|| p.to_owned())
}

fn is_oid(text: &str) -> bool {
    matches!(text.len(), 40 | 64) && text.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Parse one record. `Ok(None)` means the path is not a task record.
pub fn parse_record(path: &str, bytes: &[u8]) -> Result<Option<Record>> {
    let Some(location) = location_of(path) else {
        return Ok(None);
    };
    let value: Value = serde_json::from_slice(bytes).map_err(|e| format!("{path}: {e}"))?;
    let Some(obj) = value.as_object() else {
        return Err(format!("{path}: record is not a JSON object"));
    };
    let id = obj
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("{path}: missing id"))?
        .to_owned();
    let labels: Vec<String> = ["status", "state"]
        .iter()
        .filter_map(|k| obj.get(*k).and_then(Value::as_str))
        .map(norm_status)
        .collect();
    let closed_commit = obj
        .get("closed")
        .and_then(|c| c.get("commit"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let closed = obj.get("closed").is_some_and(Value::is_object);
    let mut deps: BTreeSet<String> = strings(obj.get("deps")).into_iter().collect();
    deps.extend(strings(obj.get("dependencies")));
    deps.retain(|d| !d.trim().is_empty() && d != &id);
    let mut self_dep = strings(obj.get("deps"));
    self_dep.extend(strings(obj.get("dependencies")));
    let mut scopes: BTreeSet<String> = strings(obj.get("scopes"))
        .iter()
        .filter_map(|s| norm_path(s))
        .collect();
    scopes.extend(
        strings(obj.get("scope"))
            .iter()
            .filter_map(|s| norm_path(s)),
    );
    let accept = match obj.get("accept") {
        Some(Value::Object(a)) => strings(a.get("cmd")),
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Null | Value::Bool(_) | Value::Number(_) | Value::Array(_)) | None => {
            Vec::new()
        }
    };
    let evidence = strings(obj.get("evidence"))
        .iter()
        .filter_map(|s| norm_path(s))
        .filter(|s| s.contains('/') && !s.contains(".."))
        .collect();
    let mut record = Record {
        status: classify(location, closed, &labels),
        raw_status: labels.join("/"),
        deps: deps.into_iter().collect(),
        scopes: scopes.into_iter().collect(),
        accept,
        closed_commit,
        evidence,
        title: obj
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        path: path.to_owned(),
        location,
        id,
    };
    if self_dep.iter().any(|d| d == &record.id) {
        // A self-dependency is a one-node cycle; keep it visible.
        record.deps.push(record.id.clone());
        record.deps.sort();
    }
    Ok(Some(record))
}

// --------------------------------------------------------------- planning

struct Resolved<'a> {
    record: &'a Record,
    superseded: Vec<&'a str>,
    ambiguous: Vec<&'a str>,
}

fn same_meaning(a: &Record, b: &Record) -> bool {
    a.status == b.status
        && a.deps == b.deps
        && a.scopes == b.scopes
        && a.closed_commit == b.closed_commit
}

fn index(records: &[Record]) -> BTreeMap<&str, Resolved<'_>> {
    let mut by_id: BTreeMap<&str, Vec<&Record>> = BTreeMap::new();
    for r in records {
        by_id.entry(r.id.as_str()).or_default().push(r);
    }
    by_id
        .into_iter()
        .filter_map(|(id, mut rs)| {
            rs.sort_by(|a, b| (a.location, &a.path).cmp(&(b.location, &b.path)));
            let top = *rs.first()?;
            let mut superseded = Vec::new();
            let mut ambiguous = Vec::new();
            for r in rs.iter().skip(1) {
                if r.location == top.location && !same_meaning(r, top) {
                    ambiguous.push(r.path.as_str());
                } else {
                    superseded.push(r.path.as_str());
                }
            }
            if !ambiguous.is_empty() {
                ambiguous.insert(0, top.path.as_str());
            }
            Some((
                id,
                Resolved {
                    record: top,
                    superseded,
                    ambiguous,
                },
            ))
        })
        .collect()
}

fn overlap(a: &[String], b: &[String]) -> Vec<String> {
    let mut out = BTreeSet::new();
    for p in a {
        for q in b {
            if p == q || p.starts_with(&format!("{q}/")) {
                out.insert(p.clone());
            } else if q.starts_with(&format!("{p}/")) {
                out.insert(q.clone());
            }
        }
    }
    out.into_iter().collect()
}

/// Nodes that lie on a dependency cycle (strongly connected components of
/// size > 1, or a self edge), computed iteratively (Kosaraju).
fn cycle_members(
    nodes: &BTreeSet<String>,
    edges: &BTreeMap<String, Vec<String>>,
) -> BTreeMap<String, Vec<String>> {
    let succ = |n: &str| edges.get(n).map(Vec::as_slice).unwrap_or_default();
    let mut order = Vec::new();
    let mut seen = BTreeSet::new();
    for start in nodes {
        if !seen.insert(start.clone()) {
            continue;
        }
        let mut stack = vec![(start.clone(), 0usize)];
        while let Some((node, i)) = stack.pop() {
            let next = succ(&node).get(i).cloned();
            match next {
                Some(n) => {
                    stack.push((node, i + 1));
                    if nodes.contains(&n) && seen.insert(n.clone()) {
                        stack.push((n, 0));
                    }
                }
                None => order.push(node),
            }
        }
    }
    let mut reverse: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (from, tos) in edges {
        for to in tos {
            if nodes.contains(to) && nodes.contains(from) {
                reverse.entry(to.as_str()).or_default().push(from.as_str());
            }
        }
    }
    let mut assigned = BTreeSet::new();
    let mut out = BTreeMap::new();
    for root in order.iter().rev() {
        if !assigned.insert(root.as_str()) {
            continue;
        }
        let mut component = vec![root.clone()];
        let mut stack = vec![root.as_str()];
        while let Some(node) = stack.pop() {
            for prev in reverse.get(node).map(Vec::as_slice).unwrap_or_default() {
                if assigned.insert(prev) {
                    component.push((*prev).to_owned());
                    stack.push(prev);
                }
            }
        }
        component.sort();
        let self_edge = component.len() == 1 && succ(&component[0]).contains(&component[0]);
        if component.len() > 1 || self_edge {
            for member in &component {
                out.insert(member.clone(), component.clone());
            }
        }
    }
    out
}

/// Ids reachable through deps from the requested ids (unknown ids included
/// as leaves), bounded by MAX_RECORDS.
fn closure(requested: &[String], idx: &BTreeMap<&str, Resolved<'_>>) -> Result<BTreeSet<String>> {
    let mut out = BTreeSet::new();
    let mut queue: VecDeque<String> = requested.iter().cloned().collect();
    while let Some(id) = queue.pop_front() {
        if !out.insert(id.clone()) {
            continue;
        }
        if out.len() > MAX_RECORDS {
            return Err("dependency closure exceeds the record limit".into());
        }
        if let Some(r) = idx.get(id.as_str()) {
            queue.extend(r.record.deps.iter().filter(|d| !out.contains(*d)).cloned());
        }
    }
    Ok(out)
}

fn intrinsic(
    id: &str,
    requested: bool,
    idx: &BTreeMap<&str, Resolved<'_>>,
    cycles: &BTreeMap<String, Vec<String>>,
    facts: &Facts,
) -> Outcome {
    let mut out = Outcome {
        kind: Kind::Included,
        reasons: BTreeSet::new(),
        notes: BTreeSet::new(),
        commits: Vec::new(),
        record: None,
        requested,
    };
    let Some(resolved) = idx.get(id) else {
        out.kind = Kind::Rejected;
        out.reasons.insert(if requested {
            "unknown_id: no task record at base".to_owned()
        } else {
            "unknown_id: referenced as a dependency, no task record at base".to_owned()
        });
        return out;
    };
    let r = resolved.record;
    out.record = Some(r.clone());
    for path in &resolved.superseded {
        out.notes.insert(format!("superseded_record: {path}"));
    }
    if !resolved.ambiguous.is_empty() {
        out.kind = Kind::Rejected;
        out.reasons.insert(format!(
            "ambiguous_record: {}",
            resolved.ambiguous.join(", ")
        ));
    }
    if let Some(members) = cycles.get(id) {
        out.kind = Kind::Rejected;
        out.reasons
            .insert(format!("dependency_cycle: {}", members.join(" <-> ")));
    }
    match r.status {
        Status::Abandoned => {
            out.kind = Kind::Rejected;
            out.reasons
                .insert(format!("abandoned_status: {}", r.raw_status));
        }
        Status::Open => {
            out.kind = out.kind.max(Kind::Deferred);
            let raw = if r.raw_status.is_empty() {
                "none"
            } else {
                r.raw_status.as_str()
            };
            out.reasons
                .insert(format!("open_status: {raw} (record in {})", r.path));
        }
        Status::Implemented => {
            out.notes
                .insert(format!("status {} (implemented, not closed)", r.raw_status));
        }
        Status::Closed => {}
    }
    let mut commits: BTreeSet<String> = facts.trailer_commits.get(id).cloned().unwrap_or_default();
    if let Some(c) = &r.closed_commit {
        if facts.reachable.contains(c) {
            commits.insert(c.clone());
        } else {
            out.notes
                .insert(format!("closed.commit {c} is not an ancestor of base"));
        }
    }
    if commits.is_empty() && matches!(r.status, Status::Closed | Status::Implemented) {
        out.kind = out.kind.max(Kind::Deferred);
        out.reasons.insert(
            "no_provenance_in_base: neither closed.commit nor a `Task:` trailer commit is in base history".into(),
        );
    }
    out.commits = commits.into_iter().collect();
    for e in &r.evidence {
        if !facts.existing_paths.contains(e) {
            out.kind = out.kind.max(Kind::Deferred);
            out.reasons.insert(format!("missing_evidence: {e}"));
        }
    }
    if r.scopes.is_empty() {
        out.notes
            .insert("no_scope: overlaps cannot be checked".into());
    }
    out
}

/// Merges happen in Susi: a task whose provenance commits the Git host created
/// is rejected unless the planning agent adopted each such commit by full id.
fn host_provenance(out: &mut Outcome, facts: &Facts, adopted: &BTreeSet<String>) {
    for c in &out.commits {
        if let Some(by) = facts.relanded.get(c) {
            out.notes.insert(format!(
                "relanded_host_commit: {c} re-landed through Susi by {by}"
            ));
            continue;
        }
        let Some(who) = facts.host_created.get(c) else {
            continue;
        };
        if adopted.contains(c) {
            out.notes.insert(format!(
                "adopted_host_commit: {c} ({who}), adopted by the planning agent"
            ));
        } else {
            out.kind = Kind::Rejected;
            out.reasons.insert(format!(
                "merged_outside_susi: {c} was created by the Git host ({who}), not by a Susi merge; re-land it through Susi or adopt it with --adopt-host-commit {c}"
            ));
        }
    }
}

fn propagate(outcomes: &mut BTreeMap<String, Outcome>) {
    for _ in 0..=outcomes.len() {
        let snapshot: BTreeMap<String, Kind> =
            outcomes.iter().map(|(k, o)| (k.clone(), o.kind)).collect();
        let unknown: BTreeSet<String> = outcomes
            .iter()
            .filter(|(_, o)| o.record.is_none())
            .map(|(k, _)| k.clone())
            .collect();
        let mut changed = false;
        for (id, out) in outcomes.iter_mut() {
            let Some(record) = &out.record else { continue };
            for dep in record.deps.clone() {
                if &dep == id {
                    continue;
                }
                let (kind, reason) = match snapshot.get(&dep) {
                    Some(Kind::Included) | None => continue,
                    Some(Kind::Rejected) if unknown.contains(&dep) => {
                        (Kind::Rejected, format!("missing_dependency: {dep}"))
                    }
                    Some(Kind::Rejected) => (Kind::Rejected, format!("dependency_rejected: {dep}")),
                    Some(Kind::Deferred) => (
                        Kind::Deferred,
                        format!("blocked_by_dependency: {dep} (deferred)"),
                    ),
                };
                if out.reasons.insert(reason) || kind > out.kind {
                    changed = true;
                }
                out.kind = out.kind.max(kind);
            }
        }
        if !changed {
            break;
        }
    }
}

fn depends_on(a: &str, b: &str, idx: &BTreeMap<&str, Resolved<'_>>) -> bool {
    let mut seen = BTreeSet::new();
    let mut stack = vec![a];
    while let Some(n) = stack.pop() {
        if !seen.insert(n) || seen.len() > MAX_RECORDS {
            continue;
        }
        if let Some(r) = idx.get(n) {
            for d in &r.record.deps {
                if d == b {
                    return true;
                }
                stack.push(d.as_str());
            }
        }
    }
    false
}

/// Compute the plan. Pure: all repository facts come in through `facts`.
pub fn plan(
    records: &[Record],
    requested: &[String],
    facts: &Facts,
    options: &Options,
) -> Result<Plan> {
    if requested.is_empty() {
        return Err("no task ids requested".into());
    }
    if requested.len() > MAX_REQUESTED {
        return Err("too many requested task ids".into());
    }
    if records.len() > MAX_RECORDS {
        return Err("too many task records".into());
    }
    let requested: Vec<String> = requested
        .iter()
        .map(|s| s.trim().to_owned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let idx = index(records);
    let nodes = closure(&requested, &idx)?;
    let edges: BTreeMap<String, Vec<String>> = nodes
        .iter()
        .filter_map(|n| {
            idx.get(n.as_str())
                .map(|r| (n.clone(), r.record.deps.clone()))
        })
        .collect();
    let cycles = cycle_members(&nodes, &edges);
    let wanted: BTreeSet<&String> = requested.iter().collect();
    let mut outcomes: BTreeMap<String, Outcome> = nodes
        .iter()
        .map(|id| {
            (
                id.clone(),
                intrinsic(id, wanted.contains(id), &idx, &cycles, facts),
            )
        })
        .collect();
    for o in outcomes.values_mut() {
        host_provenance(o, facts, &options.adopt_host_commits);
    }
    propagate(&mut outcomes);

    let mut conflicts = Vec::new();
    let included: Vec<(&String, &Record)> = outcomes
        .iter()
        .filter(|(_, o)| o.kind == Kind::Included)
        .filter_map(|(id, o)| o.record.as_ref().map(|r| (id, r)))
        .collect();
    for (i, (a, ra)) in included.iter().enumerate() {
        for (b, rb) in included.iter().skip(i + 1) {
            let paths = overlap(&ra.scopes, &rb.scopes);
            if paths.is_empty() || depends_on(a, b, &idx) || depends_on(b, a, &idx) {
                continue;
            }
            conflicts.push(Conflict {
                kind: "unordered_overlap",
                a: (*a).clone(),
                b: (*b).clone(),
                paths,
                resolution: "ordered_by_base_history (both have provenance in base)",
            });
        }
    }
    let mut entangled: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (id, resolved) in &idx {
        if nodes.contains(*id) || resolved.record.status == Status::Abandoned {
            continue;
        }
        let in_base =
            resolved.record.location == Location::Done || facts.trailer_commits.contains_key(*id);
        if !in_base {
            continue;
        }
        for (a, ra) in &included {
            let paths = overlap(&ra.scopes, &resolved.record.scopes);
            if !paths.is_empty() {
                entangled
                    .entry((*a).clone())
                    .or_default()
                    .insert((*id).to_owned());
                if conflicts
                    .iter()
                    .filter(|c| c.kind == "entangled_unselected")
                    .count()
                    < MAX_LISTED
                {
                    conflicts.push(Conflict {
                        kind: "entangled_unselected",
                        a: (*a).clone(),
                        b: (*id).to_owned(),
                        paths,
                        resolution: if options.isolate {
                            "deferred (--isolate)"
                        } else {
                            "none needed when composing from base history; required when composing without it"
                        },
                    });
                }
            }
        }
    }
    if options.isolate {
        for (a, others) in &entangled {
            if let Some(o) = outcomes.get_mut(a) {
                o.kind = o.kind.max(Kind::Deferred);
                let list: Vec<&str> = others.iter().map(String::as_str).take(5).collect();
                o.reasons.insert(format!(
                    "scope_conflict: overlaps {} unselected task(s) already in base, e.g. {}",
                    others.len(),
                    list.join(", ")
                ));
            }
        }
        propagate(&mut outcomes);
    }
    conflicts.sort_by(|x, y| (x.kind, &x.a, &x.b).cmp(&(y.kind, &y.a, &y.b)));
    let mut warnings = BTreeSet::new();
    let unscoped = outcomes
        .values()
        .filter(|o| {
            o.kind == Kind::Included && o.record.as_ref().is_some_and(|r| r.scopes.is_empty())
        })
        .count();
    if unscoped > 0 {
        warnings.insert(format!(
            "{unscoped} included task(s) declare no scope, so their overlaps cannot be checked"
        ));
    }
    let adopted: BTreeSet<&str> = outcomes
        .values()
        .flat_map(|o| o.commits.iter())
        .filter(|c| facts.host_created.contains_key(*c) && options.adopt_host_commits.contains(*c))
        .map(String::as_str)
        .collect();
    if !adopted.is_empty() {
        warnings.insert(format!(
            "{} host-created commit(s) adopted by the planning agent instead of re-landed through Susi: {}",
            adopted.len(),
            adopted.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }
    warnings.extend(facts.reland_warnings.iter().cloned());
    let entangled_total: usize = entangled.values().map(BTreeSet::len).sum();
    if entangled_total > 0 {
        warnings.insert(format!(
            "{} included task(s) overlap {} scope pair(s) with tasks outside the selection; listed up to {MAX_LISTED}",
            entangled.len(),
            entangled_total
        ));
    }
    Ok(Plan {
        requested,
        outcomes,
        conflicts,
        warnings,
    })
}

// ------------------------------------------------------------ comparison

#[derive(Clone, Debug, Default)]
pub struct RangeFacts {
    pub from: String,
    pub to: String,
    pub commits: Vec<(String, Vec<String>)>,
    pub files: Vec<String>,
    /// Commits (of included tasks) that are ancestors of `from`.
    pub before_from: BTreeSet<String>,
}

pub fn range_tasks(range: &RangeFacts) -> Vec<String> {
    range
        .commits
        .iter()
        .flat_map(|(_, t)| t.iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

pub fn compare(plan: &Plan, range: &RangeFacts) -> Value {
    let from_range: BTreeSet<String> = range_tasks(range).into_iter().collect();
    let kind_of = |id: &String| plan.outcomes.get(id).map(|o| o.kind);
    let range_not_included: Vec<Value> = from_range
        .iter()
        .filter(|id| kind_of(id) != Some(Kind::Included))
        .map(|id| {
            let o = plan.outcomes.get(id);
            json!({"id": id, "outcome": o.map(|o| o.kind.label()).unwrap_or("absent"),
                   "reasons": o.map(|o| o.reasons.iter().cloned().collect::<Vec<_>>()).unwrap_or_default()})
        })
        .collect();
    let mut extra_already = Vec::new();
    let mut extra_new = Vec::new();
    for (id, o) in &plan.outcomes {
        if o.kind != Kind::Included || from_range.contains(id) {
            continue;
        }
        if !o.commits.is_empty() && o.commits.iter().all(|c| range.before_from.contains(c)) {
            extra_already.push(id.clone());
        } else {
            extra_new.push(id.clone());
        }
    }
    let scopes: Vec<&String> = plan
        .outcomes
        .values()
        .filter(|o| o.kind == Kind::Included)
        .filter_map(|o| o.record.as_ref())
        .flat_map(|r| r.scopes.iter())
        .collect();
    let covered = |f: &String| {
        scopes
            .iter()
            .any(|s| f == *s || f.starts_with(&format!("{s}/")))
    };
    let outside: Vec<&String> = range.files.iter().filter(|f| !covered(f)).collect();
    let untasked: Vec<&String> = range
        .commits
        .iter()
        .filter(|(_, t)| t.is_empty())
        .map(|(c, _)| c)
        .collect();
    json!({
        "range": format!("{}..{}", range.from, range.to),
        "non_merge_commits": range.commits.len(),
        "commits_without_task_trailer": untasked.len(),
        "commits_without_task_trailer_list": untasked.iter().take(MAX_LISTED).collect::<Vec<_>>(),
        "tasks_named_in_range": from_range.len(),
        "range_tasks_not_included": range_not_included,
        "closure_tasks_outside_range_already_before_start": extra_already,
        "closure_tasks_outside_range_not_before_start": extra_new,
        "files_changed": range.files.len(),
        "files_changed_outside_included_scopes": outside.len(),
        "files_changed_outside_included_scopes_list": outside.iter().take(MAX_LISTED).collect::<Vec<_>>(),
    })
}

// ------------------------------------------------------------- rendering

fn outcome_json(id: &str, o: &Outcome) -> Value {
    let r = o.record.as_ref();
    json!({
        "id": id,
        "requested": o.requested,
        "status": r.map(|r| r.status.label()),
        "raw_status": r.map(|r| r.raw_status.clone()),
        "record": r.map(|r| r.path.clone()),
        "title": r.map(|r| r.title.clone()),
        "deps": r.map(|r| r.deps.clone()).unwrap_or_default(),
        "scopes": r.map(|r| r.scopes.clone()).unwrap_or_default(),
        "accept_cmd": r.map(|r| r.accept.clone()).unwrap_or_default(),
        "commits": o.commits,
        "reasons": o.reasons.iter().collect::<Vec<_>>(),
        "notes": o.notes.iter().collect::<Vec<_>>(),
    })
}

pub fn manifest(
    plan: &Plan,
    base_rev: &str,
    base_commit: &str,
    unreadable: &[String],
    comparison: Option<Value>,
) -> Value {
    let mut groups: BTreeMap<Kind, Vec<Value>> = BTreeMap::new();
    for (id, o) in &plan.outcomes {
        groups.entry(o.kind).or_default().push(outcome_json(id, o));
    }
    let take = |k: Kind, groups: &mut BTreeMap<Kind, Vec<Value>>| {
        Value::Array(groups.remove(&k).unwrap_or_default())
    };
    let mut m = Map::new();
    m.insert("schema".into(), json!(SCHEMA));
    m.insert("plan_only".into(), json!(true));
    m.insert(
        "base".into(),
        json!({"rev": base_rev, "commit": base_commit}),
    );
    m.insert("requested".into(), json!(plan.requested));
    m.insert("closure_size".into(), json!(plan.outcomes.len()));
    m.insert(
        "summary".into(),
        json!({
            "included": plan.outcomes.values().filter(|o| o.kind == Kind::Included).count(),
            "deferred": plan.outcomes.values().filter(|o| o.kind == Kind::Deferred).count(),
            "rejected": plan.outcomes.values().filter(|o| o.kind == Kind::Rejected).count(),
            "conflicts": plan.conflicts.len(),
        }),
    );
    m.insert("included".into(), take(Kind::Included, &mut groups));
    m.insert("deferred".into(), take(Kind::Deferred, &mut groups));
    m.insert("rejected".into(), take(Kind::Rejected, &mut groups));
    m.insert(
        "conflicts".into(),
        Value::Array(plan.conflicts.iter().map(|c| json!({"kind": c.kind, "a": c.a, "b": c.b, "paths": c.paths, "resolution": c.resolution})).collect()),
    );
    m.insert("warnings".into(), json!(plan.warnings));
    m.insert("unreadable_records".into(), json!(unreadable));
    if let Some(c) = comparison {
        m.insert("range_comparison".into(), c);
    }
    m.insert("not_implemented".into(), json!(NOT_IMPLEMENTED));
    Value::Object(m)
}

pub fn render_text(manifest: &Value) -> String {
    let mut s = String::new();
    let get = |k: &str| manifest.get(k).cloned().unwrap_or(Value::Null);
    let base = get("base");
    s.push_str("Distillation plan (plan only; no branch, ref or tree was changed)\n");
    s.push_str(&format!(
        "base: {} = {}\nrequested: {}  closure: {}\n",
        base.get("rev").and_then(Value::as_str).unwrap_or(""),
        base.get("commit").and_then(Value::as_str).unwrap_or(""),
        get("requested").as_array().map(Vec::len).unwrap_or(0),
        get("closure_size")
    ));
    for group in ["included", "deferred", "rejected"] {
        let items = get(group);
        let items = items.as_array().cloned().unwrap_or_default();
        s.push_str(&format!("\n{} ({})\n", group.to_uppercase(), items.len()));
        for item in &items {
            let id = item.get("id").and_then(Value::as_str).unwrap_or("");
            let status = item
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let reasons: Vec<&str> = item
                .get("reasons")
                .and_then(Value::as_array)
                .map(|r| r.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            let commits = item
                .get("commits")
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or(0);
            let detail = if reasons.is_empty() {
                format!("{commits} commit(s)")
            } else {
                reasons.join("; ")
            };
            s.push_str(&format!("  {id:<34} {status:<12} {detail}\n"));
        }
    }
    let conflicts = get("conflicts");
    let conflicts = conflicts.as_array().cloned().unwrap_or_default();
    s.push_str(&format!("\nCONFLICTS ({})\n", conflicts.len()));
    for c in conflicts.iter().take(50) {
        s.push_str(&format!(
            "  {} {} ~ {}: {}\n",
            c.get("kind").and_then(Value::as_str).unwrap_or(""),
            c.get("a").and_then(Value::as_str).unwrap_or(""),
            c.get("b").and_then(Value::as_str).unwrap_or(""),
            c.get("paths")
                .and_then(Value::as_array)
                .map(|p| p
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", "))
                .unwrap_or_default()
        ));
    }
    if conflicts.len() > 50 {
        s.push_str(&format!(
            "  ... {} more in the JSON manifest\n",
            conflicts.len() - 50
        ));
    }
    if let Some(w) = get("warnings").as_array() {
        for line in w.iter().filter_map(Value::as_str) {
            s.push_str(&format!("warning: {line}\n"));
        }
    }
    if let Some(c) = manifest.get("range_comparison") {
        s.push_str("\nRANGE COMPARISON\n");
        for key in [
            "range",
            "non_merge_commits",
            "commits_without_task_trailer",
            "tasks_named_in_range",
            "files_changed",
            "files_changed_outside_included_scopes",
        ] {
            s.push_str(&format!(
                "  {key}: {}\n",
                c.get(key).cloned().unwrap_or(Value::Null)
            ));
        }
        for key in [
            "range_tasks_not_included",
            "closure_tasks_outside_range_already_before_start",
            "closure_tasks_outside_range_not_before_start",
        ] {
            let n = c
                .get(key)
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or(0);
            s.push_str(&format!("  {key}: {n}\n"));
        }
    }
    s.push_str(&format!("\n{NOT_IMPLEMENTED}\n"));
    s
}

// -------------------------------------------------------------------- git

struct Git<'a> {
    root: &'a Path,
}

impl Git<'_> {
    fn raw(&self, args: &[&str], input: Option<Vec<u8>>) -> Result<(i32, Vec<u8>)> {
        let mut child = Command::new("git")
            .arg("-C")
            .arg(self.root)
            .args(["-c", "core.quotePath=false"])
            .args(args)
            .env("GIT_NO_REPLACE_OBJECTS", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("LC_ALL", "C")
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("cannot run git: {e}"))?;
        let writer = match (input, child.stdin.take()) {
            (Some(bytes), Some(mut stdin)) => {
                Some(std::thread::spawn(move || stdin.write_all(&bytes)))
            }
            (Some(_), None) | (None, _) => None,
        };
        let mut out = Vec::new();
        if let Some(stdout) = child.stdout.take() {
            stdout
                .take(MAX_GIT_OUTPUT as u64 + 1)
                .read_to_end(&mut out)
                .map_err(|e| format!("git read: {e}"))?;
        }
        if out.len() > MAX_GIT_OUTPUT {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "git {} output exceeds limit",
                args.first().unwrap_or(&"")
            ));
        }
        if let Some(w) = writer {
            w.join()
                .map_err(|_| "git stdin writer failed".to_owned())?
                .map_err(|e| format!("git write: {e}"))?;
        }
        let status = child.wait().map_err(|e| format!("git wait: {e}"))?;
        Ok((status.code().unwrap_or(-1), out))
    }

    fn ok(&self, args: &[&str]) -> Result<Vec<u8>> {
        match self.raw(args, None)? {
            (0, out) => Ok(out),
            (code, _) => Err(format!("git {} failed ({code})", args.join(" "))),
        }
    }

    fn resolve(&self, rev: &str) -> Result<String> {
        if rev.is_empty() || rev.starts_with('-') || rev.contains(char::is_whitespace) {
            return Err(format!("invalid revision {rev:?}"));
        }
        let out = self
            .ok(&[
                "rev-parse",
                "--verify",
                "--quiet",
                "--end-of-options",
                &format!("{rev}^{{commit}}"),
            ])
            .map_err(|_| format!("unknown revision {rev}"))?;
        let oid = String::from_utf8_lossy(&out).trim().to_owned();
        if is_oid(&oid) {
            Ok(oid)
        } else {
            Err(format!("unexpected rev-parse output for {rev}"))
        }
    }

    fn records(&self, commit: &str) -> Result<(Vec<Record>, Vec<String>)> {
        let listing = self.ok(&["ls-tree", "-r", "-z", "--full-tree", commit, "--", TASK_DIR])?;
        let mut entries = Vec::new();
        for item in listing.split(|b| *b == 0).filter(|i| !i.is_empty()) {
            let text = String::from_utf8_lossy(item);
            let Some((meta, path)) = text.split_once('\t') else {
                continue;
            };
            let parts: Vec<&str> = meta.split(' ').collect();
            if parts.len() == 3
                && parts[1] == "blob"
                && location_of(path).is_some()
                && is_oid(parts[2])
            {
                entries.push((parts[2].to_owned(), path.to_owned()));
            }
        }
        if entries.len() > MAX_RECORDS {
            return Err("too many task records".into());
        }
        let input: Vec<u8> = entries
            .iter()
            .flat_map(|(oid, _)| format!("{oid}\n").into_bytes())
            .collect();
        let (code, out) = self.raw(&["cat-file", "--batch"], Some(input))?;
        if code != 0 {
            return Err("git cat-file --batch failed".into());
        }
        let mut records = Vec::new();
        let mut unreadable = Vec::new();
        let mut at = 0usize;
        for (oid, path) in &entries {
            let Some(nl) = out[at..].iter().position(|b| *b == b'\n') else {
                return Err("truncated cat-file output".into());
            };
            let header = String::from_utf8_lossy(&out[at..at + nl]).to_string();
            at += nl + 1;
            let fields: Vec<&str> = header.split(' ').collect();
            if fields.len() != 3 || fields[0] != oid || fields[1] != "blob" {
                return Err(format!("unexpected cat-file header for {path}"));
            }
            let size: usize = fields[2]
                .parse()
                .map_err(|_| "bad cat-file size".to_owned())?;
            let end = at
                .checked_add(size)
                .filter(|e| *e < out.len())
                .ok_or("truncated cat-file body")?;
            let body = &out[at..end];
            at = end + 1;
            if size > MAX_RECORD_BYTES {
                unreadable.push(format!("{path}: larger than {MAX_RECORD_BYTES} bytes"));
                continue;
            }
            match parse_record(path, body) {
                Ok(Some(r)) => records.push(r),
                Ok(None) => {}
                Err(e) => unreadable.push(e),
            }
        }
        Ok((records, unreadable))
    }

    fn trailer_log(&self, args: &[&str]) -> Result<Vec<(String, Vec<String>)>> {
        let mut full = vec![
            "log",
            "-z",
            "--format=%H%x1f%(trailers:key=Task,valueonly,separator=%x1e)",
        ];
        full.extend_from_slice(args);
        let out = self.ok(&full)?;
        let mut commits = Vec::new();
        for item in out.split(|b| *b == 0) {
            let text = String::from_utf8_lossy(item);
            let text = text.trim_matches('\n');
            if text.is_empty() {
                continue;
            }
            let (oid, trailers) = text.split_once('\u{1f}').unwrap_or((text, ""));
            let tasks: BTreeSet<String> = trailers
                .split(['\u{1e}', '\n'])
                .map(str::trim)
                .filter(|t| t.starts_with("T-") && !t.contains(char::is_whitespace))
                .map(str::to_owned)
                .collect();
            if is_oid(oid.trim()) {
                commits.push((oid.trim().to_owned(), tasks.into_iter().collect()));
            }
        }
        Ok(commits)
    }

    fn is_ancestor(&self, commit: &str, of: &str) -> Result<bool> {
        if !is_oid(commit) {
            return Ok(false);
        }
        match self
            .raw(&["merge-base", "--is-ancestor", commit, of], None)?
            .0
        {
            0 => Ok(true),
            _ => Ok(false),
        }
    }

    fn path_exists(&self, commit: &str, path: &str) -> Result<bool> {
        if path.starts_with('-') || path.contains("..") {
            return Ok(false);
        }
        Ok(self
            .raw(&["cat-file", "-e", &format!("{commit}:{path}")], None)?
            .0
            == 0)
    }
}

/// Facts for exactly the records the plan can reach (closure is recomputed
/// cheaply here so the Git layer checks only what it must).
fn gather(git: &Git<'_>, base: &str, records: &[Record], requested: &[String]) -> Result<Facts> {
    let mut facts = Facts::default();
    for (oid, tasks) in git.trailer_log(&[base])? {
        for t in tasks {
            facts
                .trailer_commits
                .entry(t)
                .or_default()
                .insert(oid.clone());
        }
    }
    let idx = index(records);
    let nodes = closure(requested, &idx)?;
    let mut planned: BTreeSet<String> = BTreeSet::new();
    for id in &nodes {
        if let Some(c) = facts.trailer_commits.get(id) {
            planned.extend(c.iter().cloned());
        }
    }
    for id in nodes {
        let Some(r) = idx.get(id.as_str()) else {
            continue;
        };
        if let Some(c) = &r.record.closed_commit {
            if git.is_ancestor(c, base)? {
                facts.reachable.insert(c.clone());
                planned.insert(c.clone());
            }
        }
        for e in &r.record.evidence {
            if git.path_exists(base, e)? {
                facts.existing_paths.insert(e.clone());
            }
        }
    }
    let hosts = hostmerge::identities(git.root)?;
    for (oid, c) in hostmerge::among(git.root, &planned, &hosts)? {
        facts
            .host_created
            .insert(oid, format!("{} <{}>", c.committer, c.email));
    }
    let relands = hostmerge::relands(git.root, base, &hosts)?;
    for (host, by) in relands.valid {
        if facts.host_created.remove(&host).is_some() {
            facts.relanded.insert(host, by);
        }
    }
    for (by, named, why) in relands.invalid {
        facts.reland_warnings.insert(format!(
            "re-land claim in {by} for {named} not honoured: {why}"
        ));
    }
    Ok(facts)
}

// -------------------------------------------------------------------- cli

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Args {
    pub tasks: Vec<String>,
    pub range: Option<String>,
    pub base: Option<String>,
    pub out: Option<String>,
    pub json: bool,
    pub isolate: bool,
    pub adopt_host_commits: Vec<String>,
}

const USAGE: &str = "usage: cargo run --offline --locked -p xtask --bin distill -- plan [--tasks <id>...] [--tasks-from-range <from>..<to>] [--base <rev>] [--out <file>] [--json] [--isolate] [--adopt-host-commit <full-commit-id>]...\n       cargo run --offline --locked -p xtask --bin distill -- lint --range <from>..<to> | --range <to> [--base <rev>] [--out <file>] [--json] [--strict]\n";

pub fn parse_args(args: &[String]) -> Result<Args> {
    let mut it = args.iter().peekable();
    match it.next().map(String::as_str) {
        Some("plan") => {}
        Some("help" | "--help" | "-h") | None => return Err(USAGE.trim_end().into()),
        Some(other) => return Err(format!("unknown command {other}\n{USAGE}")),
    }
    let mut a = Args::default();
    while let Some(flag) = it.next() {
        let mut value = |name: &str| {
            it.next()
                .cloned()
                .filter(|v| !v.starts_with("--"))
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match flag.as_str() {
            "--tasks" => {
                let mut any = false;
                while let Some(v) = it.peek() {
                    if v.starts_with("--") {
                        break;
                    }
                    a.tasks.extend(
                        v.split(|c: char| c == ',' || c.is_whitespace())
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(str::to_owned),
                    );
                    any = true;
                    it.next();
                }
                if !any {
                    return Err("--tasks needs at least one id".into());
                }
            }
            "--tasks-from-range" => a.range = Some(value("--tasks-from-range")?),
            "--base" => a.base = Some(value("--base")?),
            "--out" => a.out = Some(value("--out")?),
            "--json" => a.json = true,
            "--isolate" => a.isolate = true,
            "--adopt-host-commit" => a.adopt_host_commits.push(value("--adopt-host-commit")?),
            other => return Err(format!("unknown option {other}\n{USAGE}")),
        }
    }
    if a.tasks.is_empty() && a.range.is_none() {
        return Err(format!("give --tasks or --tasks-from-range\n{USAGE}"));
    }
    Ok(a)
}

pub fn run(root: &Path, args: &[String]) -> Result<String> {
    if args.first().map(String::as_str) == Some("lint") {
        return lint::run_lint(root, &args[1..]);
    }
    let a = parse_args(args)?;
    let git = Git { root };
    let mut range = None;
    let mut requested = a.tasks.clone();
    let mut default_base = "HEAD".to_owned();
    if let Some(spec) = &a.range {
        let (from, to) = spec
            .split_once("..")
            .filter(|(f, t)| !f.is_empty() && !t.is_empty() && !t.starts_with('.'))
            .ok_or("range must be <from>..<to>")?;
        let (fc, tc) = (git.resolve(from)?, git.resolve(to)?);
        let commits = git.trailer_log(&["--no-merges", &format!("{fc}..{tc}")])?;
        let files_raw = git.ok(&["diff", "--name-only", "-z", &fc, &tc])?;
        let files = files_raw
            .split(|b| *b == 0)
            .filter(|f| !f.is_empty())
            .map(|f| String::from_utf8_lossy(f).to_string())
            .collect();
        let rf = RangeFacts {
            from: from.to_owned(),
            to: to.to_owned(),
            commits,
            files,
            before_from: BTreeSet::new(),
        };
        requested.extend(range_tasks(&rf));
        default_base = to.to_owned();
        range = Some((rf, fc));
    }
    let base_rev = a.base.clone().unwrap_or(default_base);
    let base = git.resolve(&base_rev)?;
    let (records, unreadable) = git.records(&base)?;
    let facts = gather(&git, &base, &records, &requested)?;
    let plan = plan(
        &records,
        &requested,
        &facts,
        &Options {
            isolate: a.isolate,
            adopt_host_commits: hostmerge::parse_adoptions(&a.adopt_host_commits)?,
        },
    )?;
    let comparison = match range {
        Some((mut rf, from_commit)) => {
            for o in plan.outcomes.values().filter(|o| o.kind == Kind::Included) {
                for c in &o.commits {
                    if git.is_ancestor(c, &from_commit)? {
                        rf.before_from.insert(c.clone());
                    }
                }
            }
            Some(compare(&plan, &rf))
        }
        None => None,
    };
    let m = manifest(&plan, &base_rev, &base, &unreadable, comparison);
    let pretty = serde_json::to_string_pretty(&m).map_err(|e| e.to_string())? + "\n";
    if let Some(out) = &a.out {
        std::fs::write(out, pretty.as_bytes()).map_err(|e| format!("cannot write {out}: {e}"))?;
    }
    Ok(if a.json { pretty } else { render_text(&m) })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    const C1: &str = "1111111111111111111111111111111111111111";
    const C2: &str = "2222222222222222222222222222222222222222";

    fn rec(id: &str, loc: Location, status: Status, deps: &[&str], scopes: &[&str]) -> Record {
        Record {
            id: id.into(),
            path: format!("{TASK_DIR}/{id}.json"),
            location: loc,
            status,
            raw_status: status.label().into(),
            deps: deps.iter().map(|s| s.to_string()).collect(),
            scopes: scopes.iter().map(|s| s.to_string()).collect(),
            accept: vec!["cargo".into(), "test".into()],
            closed_commit: None,
            evidence: Vec::new(),
            title: id.into(),
        }
    }

    fn closed(id: &str, deps: &[&str], scopes: &[&str]) -> Record {
        rec(id, Location::Done, Status::Closed, deps, scopes)
    }

    fn facts_for(ids: &[&str]) -> Facts {
        let mut f = Facts::default();
        for (i, id) in ids.iter().enumerate() {
            f.trailer_commits
                .entry(id.to_string())
                .or_default()
                .insert(format!("{:040x}", i + 1));
        }
        f
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn kind(p: &Plan, id: &str) -> Kind {
        p.outcomes[id].kind
    }

    fn has_reason(p: &Plan, id: &str, prefix: &str) -> bool {
        p.outcomes[id].reasons.iter().any(|r| r.starts_with(prefix))
    }

    #[test]
    fn closure_pulls_transitive_dependencies() {
        let records = vec![
            closed("T-A-1", &["T-B-1"], &["a"]),
            closed("T-B-1", &["T-C-1"], &["b"]),
            closed("T-C-1", &[], &["c"]),
            closed("T-X-1", &[], &["x"]),
        ];
        let p = plan(
            &records,
            &ids(&["T-A-1"]),
            &facts_for(&["T-A-1", "T-B-1", "T-C-1", "T-X-1"]),
            &Options::default(),
        )
        .unwrap();
        assert_eq!(
            p.outcomes.keys().cloned().collect::<Vec<_>>(),
            ids(&["T-A-1", "T-B-1", "T-C-1"])
        );
        assert!(p.outcomes.values().all(|o| o.kind == Kind::Included));
        assert!(p.warnings.is_empty(), "{:?}", p.warnings);
        assert!(p.outcomes["T-A-1"].requested && !p.outcomes["T-C-1"].requested);
    }

    #[test]
    fn cycles_are_rejected_with_their_dependents() {
        let records = vec![
            closed("T-A-1", &["T-B-1"], &[]),
            closed("T-B-1", &["T-C-1"], &[]),
            closed("T-C-1", &["T-B-1"], &[]),
            closed("T-S-1", &["T-S-1"], &[]),
        ];
        let p = plan(
            &records,
            &ids(&["T-A-1", "T-S-1"]),
            &facts_for(&["T-A-1", "T-B-1", "T-C-1", "T-S-1"]),
            &Options::default(),
        )
        .unwrap();
        assert_eq!(kind(&p, "T-B-1"), Kind::Rejected);
        assert!(has_reason(&p, "T-B-1", "dependency_cycle: T-B-1 <-> T-C-1"));
        assert!(
            p.warnings.is_empty(),
            "rejected tasks are not counted: {:?}",
            p.warnings
        );
        assert!(has_reason(&p, "T-C-1", "dependency_cycle"));
        assert!(has_reason(&p, "T-A-1", "dependency_rejected: T-B-1"));
        assert!(!has_reason(&p, "T-A-1", "dependency_cycle"));
        assert!(has_reason(&p, "T-S-1", "dependency_cycle: T-S-1"));
    }

    #[test]
    fn unknown_ids_and_missing_dependencies_are_rejected() {
        let records = vec![closed("T-A-1", &["T-GONE-1"], &[])];
        let p = plan(
            &records,
            &ids(&["T-A-1", "T-NOPE-1"]),
            &facts_for(&["T-A-1"]),
            &Options::default(),
        )
        .unwrap();
        assert!(has_reason(&p, "T-NOPE-1", "unknown_id: no task record"));
        assert!(has_reason(
            &p,
            "T-GONE-1",
            "unknown_id: referenced as a dependency"
        ));
        assert!(has_reason(&p, "T-A-1", "missing_dependency: T-GONE-1"));
        assert_eq!(kind(&p, "T-A-1"), Kind::Rejected);
    }

    #[test]
    fn open_work_is_deferred_and_blocks_dependents() {
        let records = vec![
            closed("T-A-1", &["T-B-1"], &[]),
            rec("T-B-1", Location::Open, Status::Open, &[], &[]),
            rec("T-F-1", Location::Open, Status::Abandoned, &[], &[]),
            closed("T-G-1", &["T-F-1"], &[]),
        ];
        let p = plan(
            &records,
            &ids(&["T-A-1", "T-G-1"]),
            &facts_for(&["T-A-1", "T-B-1", "T-G-1"]),
            &Options::default(),
        )
        .unwrap();
        assert!(has_reason(&p, "T-B-1", "open_status"));
        assert_eq!(kind(&p, "T-B-1"), Kind::Deferred);
        assert!(has_reason(&p, "T-A-1", "blocked_by_dependency: T-B-1"));
        assert_eq!(kind(&p, "T-A-1"), Kind::Deferred);
        assert!(has_reason(&p, "T-F-1", "abandoned_status"));
        assert_eq!(kind(&p, "T-G-1"), Kind::Rejected);
    }

    #[test]
    fn provenance_and_evidence_are_required() {
        let mut a = closed("T-A-1", &[], &[]);
        a.closed_commit = Some(C1.into());
        let mut b = closed("T-B-1", &[], &[]);
        b.closed_commit = Some(C2.into());
        let mut c = rec("T-C-1", Location::Open, Status::Implemented, &[], &[]);
        c.evidence = vec![".agents/evidence/c.json".into()];
        let mut facts = Facts::default();
        facts.reachable.insert(C1.into());
        facts
            .trailer_commits
            .entry("T-C-1".into())
            .or_default()
            .insert(C2.into());
        let p = plan(
            &[a, b, c],
            &ids(&["T-A-1", "T-B-1", "T-C-1"]),
            &facts,
            &Options::default(),
        )
        .unwrap();
        assert_eq!(kind(&p, "T-A-1"), Kind::Included);
        assert_eq!(p.outcomes["T-A-1"].commits, vec![C1.to_string()]);
        assert!(has_reason(&p, "T-B-1", "no_provenance_in_base"));
        assert!(p.outcomes["T-B-1"]
            .notes
            .iter()
            .any(|n| n.contains("not an ancestor")));
        assert!(has_reason(
            &p,
            "T-C-1",
            "missing_evidence: .agents/evidence/c.json"
        ));
        facts
            .existing_paths
            .insert(".agents/evidence/c.json".into());
        let c = rec("T-C-1", Location::Open, Status::Implemented, &[], &[]);
        let p = plan(&[c], &ids(&["T-C-1"]), &facts, &Options::default()).unwrap();
        assert_eq!(kind(&p, "T-C-1"), Kind::Included);
    }

    #[test]
    fn overlaps_are_reported_and_isolation_defers_entangled_tasks() {
        let records = vec![
            closed("T-A-1", &[], &["src/x.rs", "docs"]),
            closed("T-B-1", &[], &["docs/a.md"]),
            closed("T-C-1", &["T-A-1"], &["src/x.rs"]),
            closed("T-U-1", &[], &["src"]),
        ];
        let f = facts_for(&["T-A-1", "T-B-1", "T-C-1", "T-U-1"]);
        let p = plan(
            &records,
            &ids(&["T-A-1", "T-B-1", "T-C-1"]),
            &f,
            &Options::default(),
        )
        .unwrap();
        let unordered: Vec<_> = p
            .conflicts
            .iter()
            .filter(|c| c.kind == "unordered_overlap")
            .collect();
        assert_eq!(unordered.len(), 1, "{:?}", p.conflicts);
        assert_eq!(
            (unordered[0].a.as_str(), unordered[0].b.as_str()),
            ("T-A-1", "T-B-1")
        );
        assert_eq!(unordered[0].paths, vec!["docs/a.md".to_string()]);
        assert!(p
            .conflicts
            .iter()
            .any(|c| c.kind == "entangled_unselected" && c.b == "T-U-1"));
        assert!(p.outcomes.values().all(|o| o.kind == Kind::Included));
        let p = plan(
            &records,
            &ids(&["T-A-1", "T-B-1", "T-C-1"]),
            &f,
            &Options {
                isolate: true,
                ..Options::default()
            },
        )
        .unwrap();
        assert!(has_reason(&p, "T-A-1", "scope_conflict"));
        assert!(has_reason(&p, "T-C-1", "scope_conflict"));
        assert_eq!(kind(&p, "T-B-1"), Kind::Included);
    }

    #[test]
    fn manifest_is_deterministic_regardless_of_input_order() {
        let mut records = vec![
            closed("T-A-1", &["T-B-1", "T-C-1"], &["a", "s"]),
            closed("T-B-1", &["T-C-1"], &["s"]),
            closed("T-C-1", &[], &["c"]),
            rec("T-D-1", Location::Open, Status::Open, &[], &["s"]),
        ];
        let f = facts_for(&["T-A-1", "T-B-1", "T-C-1", "T-D-1"]);
        let render = |rs: &[Record], req: &[&str]| {
            let p = plan(rs, &ids(req), &f, &Options::default()).unwrap();
            serde_json::to_string(&manifest(&p, "HEAD", C1, &[], None)).unwrap()
        };
        let first = render(&records, &["T-A-1", "T-D-1"]);
        records.reverse();
        assert_eq!(first, render(&records, &["T-D-1", "T-A-1", "T-A-1"]));
        let p = plan(&records, &ids(&["T-A-1"]), &f, &Options::default()).unwrap();
        assert_eq!(
            render_text(&manifest(&p, "HEAD", C1, &[], None)),
            render_text(&manifest(&p, "HEAD", C1, &[], None))
        );
    }

    #[test]
    fn duplicate_records_prefer_done_and_reject_ambiguity() {
        let done = closed("T-A-1", &[], &[]);
        let mut open = rec("T-A-1", Location::Open, Status::Open, &[], &[]);
        open.path = format!("{TASK_DIR}/t-a-1.json");
        let one = rec("T-B-1", Location::Open, Status::Implemented, &[], &["x"]);
        let mut two = rec("T-B-1", Location::Open, Status::Implemented, &[], &["y"]);
        two.path = format!("{TASK_DIR}/t-b-1.json");
        let p = plan(
            &[open, done, one, two],
            &ids(&["T-A-1", "T-B-1"]),
            &facts_for(&["T-A-1", "T-B-1"]),
            &Options::default(),
        )
        .unwrap();
        assert_eq!(kind(&p, "T-A-1"), Kind::Included);
        assert!(p.outcomes["T-A-1"]
            .notes
            .iter()
            .any(|n| n.starts_with("superseded_record")));
        assert!(has_reason(&p, "T-B-1", "ambiguous_record"));
        assert!(
            p.warnings
                .iter()
                .any(|w| w.starts_with("1 included task(s) declare no scope")),
            "{:?}",
            p.warnings
        );
    }

    #[test]
    fn records_parse_every_historical_shape() {
        let body = br#"{"id":"T-A-1","title":"t","deps":["T-B-1","T-A-1"],"dependencies":[{"task":"T-C-1"},"T-D-1"],
            "scope":"docs/x.md","scopes":["./src/","a b"],"status":"implemented-pending-integration",
            "accept":{"cmd":["cargo","test"]},"evidence":".agents/evidence/e.json"}"#;
        let r = parse_record(".agents/tasks/T-A-1.json", body)
            .unwrap()
            .unwrap();
        assert_eq!(r.deps, ids(&["T-A-1", "T-B-1", "T-C-1", "T-D-1"]));
        assert_eq!(r.scopes, ids(&["docs/x.md", "src"]));
        assert_eq!(r.status, Status::Implemented);
        assert_eq!(r.evidence, ids(&[".agents/evidence/e.json"]));
        let done = parse_record(
            ".agents/tasks/done/T-Z-1.json",
            br#"{"id":"T-Z-1","status":"failed","closed":{"commit":"x"}}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(done.status, Status::Closed);
        let open = parse_record(
            ".agents/tasks/T-Y-1.json",
            br#"{"id":"T-Y-1","state":"in_progress"}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(open.status, Status::Open);
        assert!(parse_record(".agents/tasks/notes.md", b"x")
            .unwrap()
            .is_none());
        assert!(parse_record(".agents/tasks/sub/dir/T.json", b"{}")
            .unwrap()
            .is_none());
        assert!(parse_record(".agents/tasks/T-Q-1.json", b"[1]").is_err());
        assert!(parse_record(".agents/tasks/T-Q-1.json", br#"{"title":"no id"}"#).is_err());
    }

    #[test]
    fn arguments_are_strict() {
        let a = parse_args(&ids(&[
            "plan",
            "--tasks",
            "T-A-1",
            "T-B-1,T-C-1 T-D-1",
            "--base",
            "v1",
            "--out",
            "o.json",
            "--json",
        ]))
        .unwrap();
        assert_eq!(a.tasks, ids(&["T-A-1", "T-B-1", "T-C-1", "T-D-1"]));
        assert_eq!(
            (a.base.as_deref(), a.out.as_deref(), a.json),
            (Some("v1"), Some("o.json"), true)
        );
        assert!(parse_args(&ids(&["plan"])).is_err());
        assert!(parse_args(&ids(&["plan", "--tasks"])).is_err());
        assert!(parse_args(&ids(&["plan", "--tasks", "T-A-1", "--base"])).is_err());
        assert!(parse_args(&ids(&["plan", "--tasks", "T-A-1", "--force"])).is_err());
        assert!(parse_args(&ids(&["compose"])).is_err());
        assert!(plan(&[], &[], &Facts::default(), &Options::default()).is_err());
    }

    #[test]
    fn manifest_matches_the_published_feature_schema() {
        let schema: Value =
            serde_json::from_str(include_str!("../../.agents/schemas/feature.schema.json"))
                .unwrap();
        let records = vec![
            closed("T-A-1", &["T-B-1"], &["a"]),
            rec("T-B-1", Location::Open, Status::Open, &[], &[]),
        ];
        let p = plan(
            &records,
            &ids(&["T-A-1", "T-Z-1"]),
            &facts_for(&["T-A-1"]),
            &Options::default(),
        )
        .unwrap();
        let m = manifest(&p, "HEAD", C1, &[], None);
        let obj = m.as_object().unwrap();
        let props = schema["properties"].as_object().unwrap();
        for key in schema["required"].as_array().unwrap() {
            assert!(obj.contains_key(key.as_str().unwrap()), "missing {key}");
        }
        assert!(
            obj.keys().all(|k| props.contains_key(k)),
            "undeclared key in {:?}",
            obj.keys()
        );
        let outcome = &schema["$defs"]["outcome"];
        let required: Vec<&str> = outcome["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        for group in ["included", "deferred", "rejected"] {
            for item in m[group].as_array().unwrap() {
                let keys: Vec<&str> = item
                    .as_object()
                    .unwrap()
                    .keys()
                    .map(String::as_str)
                    .collect();
                assert_eq!(
                    keys.iter().copied().collect::<BTreeSet<_>>(),
                    required.iter().copied().collect::<BTreeSet<_>>()
                );
            }
        }
        assert_eq!(m["schema"], schema["properties"]["schema"]["const"]);
    }

    struct Fixture(std::path::PathBuf);

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "fixture")
            .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
            .env("GIT_COMMITTER_NAME", "fixture")
            .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    fn write(dir: &Path, path: &str, body: &str) {
        let p = dir.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    #[test]
    fn real_git_reads_the_revision_not_the_working_tree() {
        let dir =
            std::env::temp_dir().join(format!("susi-distill-{}-{}", std::process::id(), line!()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let fixture = Fixture(dir.clone());
        git(&dir, &["init", "-q", "-b", "main"]);
        write(&dir, "README", "base\n");
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-qm", "base"]);
        let start = git(&dir, &["rev-parse", "HEAD"]);
        write(
            &dir,
            ".agents/tasks/T-B-1.json",
            r#"{"id":"T-B-1","title":"b","status":"implemented","deps":[],"scopes":["b.txt"],"accept":{"cmd":["cargo","test"]}}"#,
        );
        write(&dir, "b.txt", "b\n");
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-qm", "b\n\nTask: T-B-1"]);
        write(
            &dir,
            ".agents/tasks/T-A-1.json",
            r#"{"id":"T-A-1","title":"a","status":"implemented","deps":["T-B-1"],"scopes":["a.txt"],"accept":{"cmd":["cargo","test"]}}"#,
        );
        write(&dir, "a.txt", "a\n");
        write(&dir, "stray.txt", "s\n");
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-qm", "a\n\nTask: T-A-1"]);
        git(&dir, &["tag", "v1"]);
        // Working-tree edits must not influence the plan.
        write(
            &dir,
            ".agents/tasks/T-A-1.json",
            r#"{"id":"T-A-1","status":"in_progress","deps":["T-NOPE-1"]}"#,
        );
        let out_file = dir.join("plan.json");
        let args = ids(&[
            "plan",
            "--tasks-from-range",
            &format!("{start}..v1"),
            "--out",
            out_file.to_str().unwrap(),
            "--json",
        ]);
        let text = run(&dir, &args).unwrap();
        let m: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(std::fs::read_to_string(&out_file).unwrap(), text);
        assert_eq!(m["summary"]["included"], 2, "{text}");
        assert_eq!(m["base"]["rev"], "v1");
        let c = &m["range_comparison"];
        assert_eq!(c["tasks_named_in_range"], 2);
        assert_eq!(c["commits_without_task_trailer"], 0);
        assert_eq!(c["files_changed_outside_included_scopes"], 3, "{c}");
        assert_eq!(
            run(&dir, &args).unwrap(),
            text,
            "repeat run must be identical"
        );
        let status = git(&dir, &["status", "--porcelain"]);
        assert!(
            status.contains("T-A-1.json") && !status.contains("refs"),
            "{status}"
        );
        assert_eq!(
            git(&dir, &["for-each-ref", "--format=%(refname)"]),
            "refs/heads/main\nrefs/tags/v1"
        );
        assert!(run(&dir, &ids(&["plan", "--tasks", "T-A-1", "--base", "-x"])).is_err());
        drop(fixture);
    }

    /// A real repository: the host writes one task commit and one merge;
    /// lint reports both, plan refuses the task, adoption admits it on record.
    #[test]
    fn real_git_host_created_work_is_a_lint_finding_and_a_plan_rejection() {
        use serde_json::Value;
        let dir = std::env::temp_dir().join(format!("susi-hostmerge-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let g = |host: bool, args: &[&str]| {
            let (name, email) = if host {
                ("GitHub", "noreply@github.com")
            } else {
                ("fixture", "fixture@example.invalid")
            };
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(args)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_AUTHOR_NAME", "fixture")
                .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
                .env("GIT_COMMITTER_NAME", name)
                .env("GIT_COMMITTER_EMAIL", email)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {out:?}");
            String::from_utf8_lossy(&out.stdout).trim().to_owned()
        };
        let w = |p: &str, b: &str| {
            let p = dir.join(p);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, b).unwrap();
        };
        g(false, &["init", "-q", "-b", "main"]);
        w("README", "r\n");
        g(false, &["add", "."]);
        g(false, &["commit", "-qm", "base"]);
        let start = g(false, &["rev-parse", "HEAD"]);
        w(
            ".agents/tasks/done/T-H-1.json",
            r#"{"id":"T-H-1","status":"done","scopes":["h.txt"]}"#,
        );
        w("h.txt", "h\n");
        g(false, &["add", "."]);
        g(true, &["commit", "-qm", "api commit\n\nTask: T-H-1"]);
        let api = g(false, &["rev-parse", "HEAD"]);
        g(false, &["checkout", "-qb", "side", &start]);
        w(
            ".agents/tasks/done/T-L-1.json",
            r#"{"id":"T-L-1","status":"done","scopes":["l.txt"]}"#,
        );
        w("l.txt", "l\n");
        g(false, &["add", "."]);
        g(false, &["commit", "-qm", "local work\n\nTask: T-L-1"]);
        g(false, &["checkout", "-q", "main"]);
        g(
            true,
            &[
                "merge",
                "-q",
                "--no-ff",
                "-m",
                "Merge pull request #1",
                "side",
            ],
        );
        let s = |v: &[&str]| v.iter().map(|x| (*x).to_owned()).collect::<Vec<_>>();

        let lint: Value = serde_json::from_str(
            &run(
                &dir,
                &s(&[
                    "lint",
                    "--range",
                    &format!("{start}..main"),
                    "--json",
                    "--no-claims",
                ]),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(lint["summary"]["commits_created_by_git_host"], 2, "{lint}");
        assert_eq!(lint["summary"]["merges_created_by_git_host"], 1);
        assert_eq!(lint["commits_created_by_git_host"][0]["merge"], true);
        let strict = run(
            &dir,
            &s(&[
                "lint",
                "--range",
                &format!("{start}..main"),
                "--strict",
                "--no-claims",
            ]),
        );
        assert!(strict
            .unwrap_err()
            .contains("by GitHub <noreply@github.com>"));

        let plan = |extra: &[&str]| -> Value {
            let mut args = s(&[
                "plan", "--tasks", "T-H-1", "T-L-1", "--base", "main", "--json",
            ]);
            args.extend(s(extra));
            serde_json::from_str(&run(&dir, &args).unwrap()).unwrap()
        };
        let refused = plan(&[]);
        assert_eq!(refused["summary"]["rejected"], 1, "{refused}");
        assert_eq!(refused["rejected"][0]["id"], "T-H-1");
        let reasons = refused["rejected"][0]["reasons"].to_string();
        assert!(reasons.contains("merged_outside_susi"), "{reasons}");
        // The task whose own commit is local stays included: the host merge
        // that carried it is a lint finding, not a property of the task.
        assert_eq!(refused["included"][0]["id"], "T-L-1");

        let adopted = plan(&["--adopt-host-commit", &api]);
        assert_eq!(adopted["summary"]["rejected"], 0, "{adopted}");
        assert!(adopted["warnings"].to_string().contains(&api));
        assert!(run(
            &dir,
            &s(&[
                "plan",
                "--tasks",
                "T-H-1",
                "--adopt-host-commit",
                &api[..12]
            ]),
        )
        .is_err());

        // A forged re-land (different content) is reported, never honoured.
        let host_merge = g(false, &["rev-parse", "main"]);
        let parent = g(false, &["rev-parse", &format!("{api}^")]);
        let parent_tree = g(false, &["rev-parse", &format!("{parent}^{{tree}}")]);
        let forged_msg = format!("forged\n\nTask: T-H-1\nRelands: {api}");
        let forged = g(
            false,
            &[
                "commit-tree",
                &parent_tree,
                "-p",
                &parent,
                "-m",
                &forged_msg,
            ],
        );
        let tree = g(false, &["rev-parse", "main^{tree}"]);
        let joined = g(
            false,
            &[
                "commit-tree",
                &tree,
                "-p",
                "main",
                "-p",
                &forged,
                "-m",
                "join forged",
            ],
        );
        g(false, &["update-ref", "refs/heads/main", &joined]);
        let lint_range = format!("{start}..main");
        let lint_json = |dir: &std::path::Path| -> Value {
            let args = s(&["lint", "--range", &lint_range, "--json", "--no-claims"]);
            serde_json::from_str(&run(dir, &args).unwrap()).unwrap()
        };
        let forged_lint = lint_json(&dir);
        assert_eq!(
            forged_lint["summary"]["reland_claims_not_honoured"], 1,
            "{forged_lint}"
        );
        assert_eq!(forged_lint["summary"]["host_commits_relanded"], 0);
        assert_eq!(plan(&[])["summary"]["rejected"], 1);

        // A genuine replay (identical change, local committer) plus a Susi
        // merge naming the host merge re-lands both.
        let api_tree = g(false, &["rev-parse", &format!("{api}^{{tree}}")]);
        let replay_msg = format!("api commit\n\nTask: T-H-1\nRelands: {api}");
        let replay = g(
            false,
            &["commit-tree", &api_tree, "-p", &parent, "-m", &replay_msg],
        );
        let merge_msg = format!("Susi re-land\n\nRelands: {host_merge}");
        let susi_merge = g(
            false,
            &[
                "commit-tree",
                &tree,
                "-p",
                "main",
                "-p",
                &replay,
                "-m",
                &merge_msg,
            ],
        );
        g(false, &["update-ref", "refs/heads/main", &susi_merge]);
        let relanded = lint_json(&dir);
        assert_eq!(
            relanded["summary"]["host_commits_relanded"], 2,
            "{relanded}"
        );
        assert_eq!(relanded["summary"]["commits_created_by_git_host"], 0);
        let ok = plan(&[]);
        assert_eq!(ok["summary"]["rejected"], 0, "{ok}");
        assert!(ok["included"].to_string().contains("relanded_host_commit"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
