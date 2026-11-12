//! `distill lint`: label hygiene for a commit range (T-GROKLABEL-1).
//!
//! Reports, deterministically and without changing anything:
//! - commits with no `Task:` trailer;
//! - trailers naming a task with no record at the base revision;
//! - tasks whose commits landed in the range while the record is still open
//!   (`in_progress`, `open`, `claimed`, ...) or merely implemented;
//! - commits touching files outside the declared scopes of their tasks;
//! - commits and merges the Git host created instead of Susi (`hostmerge`):
//!   every merge happens in Susi, the host is storage only.
//!
//! `Task: LOCAL-<name>` (the native launcher's migration label, AGENTS.md
//! step 5) counts as labelled but has no record, so it is reported as a
//! distillation gap, not as a missing trailer. A task's scopes come from its
//! record, or, when the record has none (records written by `susi tasks
//! close` drop them), from its claim ref `refs/claims/<id>` if present.
//!
//! Records are read at `--base` (default: the range end) through Git, never
//! from the working tree. One `git log` pass plus one record read; bounded.
use super::{index, is_oid, Git, Record, Result, Status, MAX_LISTED, TASK_DIR};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub const LINT_SCHEMA: &str = "susi-distill-lint/v1";
const MAX_COMMITS: usize = 50_000;
const RS: char = '\u{1e}';
const US: char = '\u{1f}';
const GS: char = '\u{1d}';

const LINT_USAGE: &str = "usage: cargo run --offline --locked -p xtask --bin distill -- lint --range <from>..<to> | --range <to> [--base <rev>] [--out <file>] [--json] [--strict] [--no-claims] [--pull]\n";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Commit {
    pub oid: String,
    pub subject: String,
    /// Well-formed `Task:` ids (`T-...`), sorted and deduplicated.
    pub tasks: Vec<String>,
    /// `Task: LOCAL-<name>` launcher labels.
    pub local: Vec<String>,
    /// `Task:` values that are neither.
    pub malformed: Vec<String>,
    pub files: Vec<String>,
}

/// Split `Task:` trailer values: one per trailer, or several separated by
/// commas or whitespace.
pub fn split_trailers(raw: &str) -> (Vec<String>, Vec<String>, Vec<String>) {
    let mut tasks = BTreeSet::new();
    let mut local = BTreeSet::new();
    let mut malformed = BTreeSet::new();
    for v in raw
        .split(|c: char| c == GS || c == ',' || c.is_whitespace())
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        if v.len() > 2 && v.starts_with("T-") && v.bytes().all(|b| b.is_ascii_graphic()) {
            tasks.insert(v.to_owned());
        } else if v.len() > 6 && v.starts_with("LOCAL-") && v.bytes().all(|b| b.is_ascii_graphic())
        {
            local.insert(v.to_owned());
        } else {
            malformed.insert(v.to_owned());
        }
    }
    (
        tasks.into_iter().collect(),
        local.into_iter().collect(),
        malformed.into_iter().collect(),
    )
}

/// Parse `git log --name-only` output in the format `lint_log` asks for.
pub fn parse_log(text: &str) -> Vec<Commit> {
    let mut commits = Vec::new();
    for chunk in text.split(RS).filter(|c| !c.trim().is_empty()) {
        let mut parts = chunk.splitn(4, US);
        let oid = parts.next().unwrap_or_default().trim().to_owned();
        let subject = parts.next().unwrap_or_default().to_owned();
        let trailers = parts.next().unwrap_or_default();
        let files_text = parts.next().unwrap_or_default();
        if !is_oid(&oid) {
            continue;
        }
        let (tasks, local, malformed) = split_trailers(trailers);
        let files: BTreeSet<String> = files_text
            .lines()
            .map(str::trim_end)
            .filter(|l| !l.is_empty())
            .map(str::to_owned)
            .collect();
        commits.push(Commit {
            oid,
            subject,
            tasks,
            local,
            malformed,
            files: files.into_iter().collect(),
        });
    }
    commits
}

/// Whether `scope` covers `file`: equal, a directory prefix, or a glob whose
/// literal prefix (before the first `*`, `?` or `[`) is a prefix of `file`.
pub fn covers(scope: &str, file: &str) -> bool {
    let scope = scope.trim_end_matches('/');
    if scope.is_empty() || scope == "." {
        return true;
    }
    if let Some(at) = scope.find(['*', '?', '[']) {
        return file.starts_with(&scope[..at]);
    }
    file == scope || file.strip_prefix(scope).is_some_and(|r| r.starts_with('/'))
}

/// The task's own record files are always in scope for its commits.
fn own_record(id: &str, file: &str) -> bool {
    let Some(rest) = file
        .strip_prefix(TASK_DIR)
        .and_then(|r| r.strip_prefix('/'))
    else {
        return false;
    };
    let name = rest
        .strip_prefix("done/")
        .or_else(|| rest.strip_prefix("in-progress/"))
        .unwrap_or(rest);
    name.strip_suffix(".json")
        .is_some_and(|stem| stem.eq_ignore_ascii_case(id))
}

fn short(oid: &str) -> &str {
    oid.get(..12).unwrap_or(oid)
}

fn capped<T: Clone>(items: &[T]) -> Vec<T> {
    items.iter().take(MAX_LISTED).cloned().collect()
}

/// Pure lint over already-read facts. `commits` are in Git's topo order;
/// `claim_scopes` maps a task id to the scopes its claim ref declares.
pub fn lint(
    commits: &[Commit],
    records: &[Record],
    claim_scopes: &BTreeMap<String, Vec<String>>,
) -> Value {
    let idx = index(records);
    let mut local_only = Vec::new();
    let mut local_labels: BTreeSet<&str> = BTreeSet::new();
    let mut scoped_by_claim: BTreeSet<&str> = BTreeSet::new();
    let mut untasked = Vec::new();
    let mut malformed = Vec::new();
    let mut unknown: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut landed: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut out_of_scope = Vec::new();
    let mut unscoped: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut files_outside = 0usize;
    for c in commits {
        if !c.malformed.is_empty() {
            malformed.push(json!({"commit": c.oid, "values": c.malformed}));
        }
        local_labels.extend(c.local.iter().map(String::as_str));
        if c.tasks.is_empty() {
            if c.local.is_empty() {
                untasked.push(json!({"commit": c.oid, "subject": c.subject}));
            } else {
                local_only.push(json!({"commit": c.oid, "subject": c.subject, "labels": c.local}));
            }
            continue;
        }
        let mut scopes: Vec<&str> = Vec::new();
        let mut evidence: BTreeSet<&str> = BTreeSet::new();
        let mut known = 0usize;
        let mut any_scoped = false;
        for t in &c.tasks {
            landed.entry(t.clone()).or_default().push(c.oid.clone());
            match idx.get(t.as_str()) {
                None => unknown.entry(t.clone()).or_default().push(c.oid.clone()),
                Some(r) => {
                    known += 1;
                    let declared: &[String] = if r.record.scopes.is_empty() {
                        match claim_scopes.get(t.as_str()) {
                            Some(cs) if !cs.is_empty() => {
                                scoped_by_claim.insert(t.as_str());
                                cs
                            }
                            Some(_) | None => &[],
                        }
                    } else {
                        &r.record.scopes
                    };
                    if declared.is_empty() {
                        unscoped.entry(t.clone()).or_default().push(c.oid.clone());
                    } else {
                        any_scoped = true;
                    }
                    scopes.extend(declared.iter().map(String::as_str));
                    evidence.extend(r.record.evidence.iter().map(String::as_str));
                }
            }
        }
        if known == 0 || !any_scoped {
            continue; // reported as unknown or unscoped instead
        }
        let outside: Vec<&String> = c
            .files
            .iter()
            .filter(|f| {
                !c.tasks.iter().any(|t| own_record(t, f))
                    && !evidence.contains(f.as_str())
                    && !scopes.iter().any(|s| covers(s, f))
            })
            .collect();
        if !outside.is_empty() {
            files_outside += outside.len();
            out_of_scope.push(json!({
                "commit": c.oid,
                "subject": c.subject,
                "tasks": c.tasks,
                "files": outside.iter().take(MAX_LISTED).collect::<Vec<_>>(),
                "file_count": outside.len(),
            }));
        }
    }
    let mut still_open = Vec::new();
    let mut implemented = Vec::new();
    for (task, oids) in &landed {
        let Some(r) = idx.get(task.as_str()) else {
            continue;
        };
        let entry = json!({
            "task": task,
            "record": r.record.path,
            "status": if r.record.raw_status.is_empty() {
                "(no status field)"
            } else {
                r.record.raw_status.as_str()
            },
            "title": r.record.title,
            "commits": oids.len(),
            "latest_commit": oids.first(),
        });
        match r.record.status {
            Status::Open => still_open.push(entry),
            Status::Implemented => implemented.push(entry),
            Status::Closed | Status::Abandoned => {}
        }
    }
    let unknown_list: Vec<Value> = unknown
        .iter()
        .map(|(t, o)| json!({"task": t, "commits": o.len(), "latest_commit": o.first()}))
        .collect();
    let unscoped_list: Vec<Value> = unscoped
        .iter()
        .map(|(t, o)| json!({"task": t, "commits": o.len()}))
        .collect();
    let tasked = commits.len() - untasked.len() - local_only.len();
    json!({
        "summary": {
            "commits": commits.len(),
            "commits_with_task_trailer": tasked,
            "commits_without_task_trailer": untasked.len(),
            "commits_with_local_label_only": local_only.len(),
            "local_labels": local_labels.len(),
            "commits_with_malformed_trailer": malformed.len(),
            "tasks_named": landed.len(),
            "tasks_without_record": unknown_list.len(),
            "tasks_landed_but_record_open": still_open.len(),
            "tasks_landed_but_record_implemented_not_closed": implemented.len(),
            "tasks_without_scopes": unscoped_list.len(),
            "tasks_scoped_by_claim_ref_only": scoped_by_claim.len(),
            "commits_touching_files_outside_task_scopes": out_of_scope.len(),
            "files_outside_task_scopes": files_outside,
            "findings": untasked.len() + malformed.len() + unknown_list.len()
                + still_open.len() + out_of_scope.len() + unscoped_list.len(),
        },
        "commits_without_task_trailer": capped(&untasked),
        "commits_with_local_label_only": capped(&local_only),
        "commits_with_malformed_trailer": capped(&malformed),
        "tasks_without_record": capped(&unknown_list),
        "tasks_landed_but_record_open": still_open,
        "tasks_landed_but_record_implemented_not_closed": implemented,
        "tasks_without_scopes": capped(&unscoped_list),
        "commits_touching_files_outside_task_scopes": capped(&out_of_scope),
    })
}

/// Fold host-created commits into a lint report: a summary count that adds to
/// `findings`, and the listed commits (merges first).
pub fn add_host_findings(
    report: &mut Value,
    host_created: &[super::hostmerge::HostCommit],
    hosts: &BTreeSet<String>,
) {
    let mut list: Vec<Value> = host_created
        .iter()
        .map(|c| {
            json!({
                "commit": c.oid,
                "merge": c.merge,
                "committer": format!("{} <{}>", c.committer, c.email),
                "subject": c.subject,
            })
        })
        .collect();
    list.sort_by_key(|v| !v["merge"].as_bool().unwrap_or(false));
    let merges = host_created.iter().filter(|c| c.merge).count();
    if let Some(sum) = report["summary"].as_object_mut() {
        sum.insert(
            "commits_created_by_git_host".into(),
            json!(host_created.len()),
        );
        sum.insert("merges_created_by_git_host".into(), json!(merges));
        let findings = sum.get("findings").and_then(Value::as_u64).unwrap_or(0);
        sum.insert(
            "findings".into(),
            json!(findings + host_created.len() as u64),
        );
    }
    if let Some(obj) = report.as_object_mut() {
        obj.insert("commits_created_by_git_host".into(), json!(capped(&list)));
        obj.insert("git_host_committer_identities".into(), json!(hosts));
    }
}

/// CEO Mandate 78 took effect at this Unix time; a task record created
/// earlier is not asked for a reuse decision.
pub const REUSE_EFFECTIVE_UNIX: u64 = 1_794_480_600;

/// Why a task record states no usable reuse decision (CEO Mandate 78), when
/// it should: the record was created after the mandate, is of size `m` or
/// larger, and its `reuse` object is missing, names no known decision, or
/// leaves out what its decision needs. `None` when nothing is owed.
pub fn reuse_gap(record: &Value) -> Option<&'static str> {
    if record["created_unix"].as_u64().unwrap_or(0) < REUSE_EFFECTIVE_UNIX {
        return None;
    }
    let size = record["size"].as_str().unwrap_or("").to_ascii_lowercase();
    if matches!(size.as_str(), "xs" | "s") {
        return None;
    }
    let reuse = &record["reuse"];
    let stated = |key: &str| {
        reuse[key]
            .as_str()
            .is_some_and(|text| !text.trim().is_empty())
    };
    match reuse["decision"].as_str() {
        None => Some("no reuse decision"),
        Some("existing" | "dependency") => None,
        Some("harvest") if stated("source") => None,
        Some("harvest") => Some("a harvest names no source"),
        Some("build") if stated("reason") => None,
        Some("build") => Some("a build gives no reason"),
        Some(_) => Some("unknown reuse decision"),
    }
}

/// Add Mandate 78's advice to a lint report: the landed tasks whose record
/// states no reuse decision. Advice only: it is not counted in `findings`, so
/// it never fails `--strict`.
pub fn add_reuse_advice(report: &mut Value, gaps: &[(String, &'static str)]) {
    let listed: Vec<Value> = gaps
        .iter()
        .map(|(task, why)| json!({"task": task, "why": why}))
        .collect();
    if let Some(sum) = report["summary"].as_object_mut() {
        sum.insert(
            "advisory_tasks_without_reuse_decision".into(),
            json!(listed.len()),
        );
    }
    if let Some(obj) = report.as_object_mut() {
        obj.insert(
            "tasks_without_reuse_decision".into(),
            json!(capped(&listed)),
        );
    }
}

pub fn render_lint_text(report: &Value) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    let sum = &report["summary"];
    let _ = writeln!(
        s,
        "distill lint {} (records at {} {})",
        report["range"].as_str().unwrap_or_default(),
        report["base"]["rev"].as_str().unwrap_or_default(),
        short(report["base"]["commit"].as_str().unwrap_or_default()),
    );
    if let Some(m) = sum.as_object() {
        for (k, v) in m {
            let _ = writeln!(s, "  {k}: {v}");
        }
    }
    let section = |s: &mut String, key: &str, line: &dyn Fn(&Value) -> String| {
        let Some(items) = report[key].as_array().filter(|a| !a.is_empty()) else {
            return;
        };
        let _ = writeln!(s, "{key}:");
        for item in items.iter().take(50) {
            let _ = writeln!(s, "  {}", line(item));
        }
        if items.len() > 50 {
            let _ = writeln!(s, "  ... {} more in --json", items.len() - 50);
        }
    };
    let oid = |v: &Value, k: &str| short(v[k].as_str().unwrap_or_default()).to_owned();
    section(&mut s, "commits_created_by_git_host", &|v| {
        format!(
            "{} {}{} by {}",
            oid(v, "commit"),
            if v["merge"].as_bool().unwrap_or(false) {
                "[merge] "
            } else {
                ""
            },
            v["subject"].as_str().unwrap_or_default(),
            v["committer"].as_str().unwrap_or_default()
        )
    });
    section(&mut s, "tasks_landed_but_record_open", &|v| {
        format!(
            "{} [{}] {} commit(s), {}",
            v["task"].as_str().unwrap_or_default(),
            v["status"].as_str().unwrap_or_default(),
            v["commits"],
            v["record"].as_str().unwrap_or_default()
        )
    });
    section(&mut s, "tasks_without_record", &|v| {
        format!(
            "{} {} commit(s), latest {}",
            v["task"].as_str().unwrap_or_default(),
            v["commits"],
            oid(v, "latest_commit")
        )
    });
    section(&mut s, "tasks_without_scopes", &|v| {
        format!(
            "{} {} commit(s)",
            v["task"].as_str().unwrap_or_default(),
            v["commits"]
        )
    });
    section(&mut s, "commits_touching_files_outside_task_scopes", &|v| {
        format!(
            "{} {} {} file(s) outside {}",
            oid(v, "commit"),
            v["subject"].as_str().unwrap_or_default(),
            v["file_count"],
            v["tasks"]
        )
    });
    section(&mut s, "commits_without_task_trailer", &|v| {
        format!(
            "{} {}",
            oid(v, "commit"),
            v["subject"].as_str().unwrap_or_default()
        )
    });
    section(&mut s, "commits_with_local_label_only", &|v| {
        format!(
            "{} {} {}",
            oid(v, "commit"),
            v["labels"],
            v["subject"].as_str().unwrap_or_default()
        )
    });
    section(&mut s, "commits_with_malformed_trailer", &|v| {
        format!("{} {}", oid(v, "commit"), v["values"])
    });
    s
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct LintArgs {
    pub range: String,
    pub base: Option<String>,
    pub out: Option<String>,
    pub json: bool,
    pub strict: bool,
    pub no_claims: bool,
    pub pull: bool,
}

pub fn parse_lint_args(args: &[String]) -> Result<LintArgs> {
    let mut a = LintArgs::default();
    let mut range = None;
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let mut value = |name: &str| {
            it.next()
                .cloned()
                .filter(|v| !v.starts_with("--"))
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match flag.as_str() {
            "--range" => range = Some(value("--range")?),
            "--base" => a.base = Some(value("--base")?),
            "--out" => a.out = Some(value("--out")?),
            "--json" => a.json = true,
            "--strict" => a.strict = true,
            "--no-claims" => a.no_claims = true,
            "--pull" => a.pull = true,
            "help" | "--help" | "-h" => return Err(LINT_USAGE.trim_end().into()),
            other => return Err(format!("unknown option {other}\n{LINT_USAGE}")),
        }
    }
    a.range = range.ok_or_else(|| format!("give --range\n{LINT_USAGE}"))?;
    Ok(a)
}

fn lint_log(git: &Git<'_>, from: Option<&str>, to: &str) -> Result<Vec<Commit>> {
    let range = match from {
        Some(f) => format!("{f}..{to}"),
        None => to.to_owned(),
    };
    let count = git.ok(&["rev-list", "--count", "--no-merges", &range])?;
    let count: usize = String::from_utf8_lossy(&count)
        .trim()
        .parse()
        .map_err(|_| "bad rev-list count".to_owned())?;
    if count > MAX_COMMITS {
        return Err(format!(
            "{count} commits in range exceed the {MAX_COMMITS} limit; narrow --range"
        ));
    }
    let out = git.ok(&[
        "log",
        "--no-merges",
        "--topo-order",
        "--no-renames",
        "--name-only",
        "--format=%x1e%H%x1f%s%x1f%(trailers:key=Task,key=Distilled-Task,valueonly,separator=%x1d)%x1f",
        &range,
        "--",
    ])?;
    Ok(parse_log(&String::from_utf8_lossy(&out)))
}

/// Scopes declared by local claim refs (`refs/claims/<id>` blobs holding the
/// claim JSON). Unreadable or foreign blobs are skipped.
fn claim_scopes(git: &Git<'_>) -> Result<BTreeMap<String, Vec<String>>> {
    let listing = git.ok(&[
        "for-each-ref",
        "--format=%(objectname) %(objecttype) %(refname)",
        "refs/claims/",
    ])?;
    let oids: Vec<String> = String::from_utf8_lossy(&listing)
        .lines()
        .filter_map(|l| {
            let mut p = l.split(' ');
            let (oid, kind) = (p.next()?, p.next()?);
            (kind == "blob" && is_oid(oid)).then(|| oid.to_owned())
        })
        .take(super::MAX_RECORDS)
        .collect();
    let mut out = BTreeMap::new();
    if oids.is_empty() {
        return Ok(out);
    }
    let input: Vec<u8> = oids
        .iter()
        .flat_map(|o| format!("{o}\n").into_bytes())
        .collect();
    let (code, body) = git.raw(&["cat-file", "--batch"], Some(input))?;
    if code != 0 {
        return Err("git cat-file --batch failed for claim refs".into());
    }
    let mut at = 0usize;
    for _ in &oids {
        let Some(nl) = body[at..].iter().position(|b| *b == b'\n') else {
            break;
        };
        let header = String::from_utf8_lossy(&body[at..at + nl]).to_string();
        at += nl + 1;
        let Some(size) = header
            .split(' ')
            .nth(2)
            .and_then(|n| n.parse::<usize>().ok())
        else {
            continue;
        };
        let Some(end) = at.checked_add(size).filter(|e| *e <= body.len()) else {
            break;
        };
        if let Ok(v) = serde_json::from_slice::<Value>(&body[at..end]) {
            if let Some(task) = v.get("task").and_then(Value::as_str) {
                let scopes: BTreeSet<String> = super::strings(v.get("scopes"))
                    .iter()
                    .filter_map(|s| super::norm_path(s))
                    .collect();
                out.insert(task.to_owned(), scopes.into_iter().collect());
            }
        }
        at = end + 1;
    }
    Ok(out)
}

pub fn run_lint(root: &Path, args: &[String]) -> Result<String> {
    let a = parse_lint_args(args)?;
    let git = Git { root };
    if a.pull {
        let _ = git.pull_origin_optional()?;
    }
    let (from, to) = match a.range.split_once("..") {
        Some((f, t)) if !f.is_empty() && !t.is_empty() && !t.starts_with('.') => {
            (Some(f.to_owned()), t.to_owned())
        }
        Some(_) => return Err("range must be <from>..<to> or <to>".into()),
        None => (None, a.range.clone()),
    };
    let to_commit = git.resolve(&to)?;
    let from_commit = from.as_deref().map(|f| git.resolve(f)).transpose()?;
    let base_rev = a.base.clone().unwrap_or_else(|| to.clone());
    let base = git.resolve(&base_rev)?;
    let commits = lint_log(&git, from_commit.as_deref(), &to_commit)?;
    let (records, unreadable) = git.records(&base)?;
    let claims = if a.no_claims {
        BTreeMap::new()
    } else {
        claim_scopes(&git)?
    };
    let mut report = lint(&commits, &records, &claims);
    // Mandate 78: which landed tasks state no reuse decision (advice only).
    let landed: BTreeSet<&str> = commits
        .iter()
        .flat_map(|c| c.tasks.iter().map(String::as_str))
        .collect();
    let mut reuse_gaps = Vec::new();
    for record in records.iter().filter(|r| landed.contains(r.id.as_str())) {
        let (code, bytes) = git.raw(&["show", &format!("{base}:{}", record.path)], None)?;
        if code != 0 {
            continue;
        }
        if let Ok(raw) = serde_json::from_slice::<Value>(&bytes) {
            if let Some(why) = reuse_gap(&raw) {
                reuse_gaps.push((record.id.clone(), why));
            }
        }
    }
    add_reuse_advice(&mut report, &reuse_gaps);
    let scan_range = match &from_commit {
        Some(f) => format!("{f}..{to_commit}"),
        None => to_commit.clone(),
    };
    let hosts = super::hostmerge::identities(root)?;
    let mut host_created = super::hostmerge::scan_range(root, &scan_range, &hosts)?;
    let relands = super::hostmerge::relands(root, &to_commit, &hosts)?;
    let relanded: Vec<Value> = host_created
        .iter()
        .filter_map(|c| {
            relands
                .valid
                .get(&c.oid)
                .map(|by| json!({"commit": c.oid, "merge": c.merge, "relanded_by": by}))
        })
        .collect();
    host_created.retain(|c| !relands.valid.contains_key(&c.oid));
    add_host_findings(&mut report, &host_created, &hosts);
    let invalid: Vec<Value> = relands
        .invalid
        .iter()
        .map(|(by, named, why)| json!({"commit": by, "names": named, "reason": why}))
        .collect();
    if let Some(sum) = report["summary"].as_object_mut() {
        sum.insert("host_commits_relanded".into(), json!(relanded.len()));
        sum.insert("reland_claims_not_honoured".into(), json!(invalid.len()));
        let findings = sum.get("findings").and_then(Value::as_u64).unwrap_or(0);
        sum.insert("findings".into(), json!(findings + invalid.len() as u64));
    }
    if let Some(obj) = report.as_object_mut() {
        obj.insert("host_commits_relanded".into(), json!(capped(&relanded)));
        obj.insert("reland_claims_not_honoured".into(), json!(capped(&invalid)));
    }
    if let Some(obj) = report.as_object_mut() {
        obj.insert("schema".into(), json!(LINT_SCHEMA));
        obj.insert("range".into(), json!(a.range));
        obj.insert(
            "range_commits".into(),
            json!({"from": from_commit, "to": to_commit}),
        );
        obj.insert("base".into(), json!({"rev": base_rev, "commit": base}));
        obj.insert("unreadable_records".into(), json!(unreadable));
        obj.insert("claim_refs_read".into(), json!(claims.len()));
        obj.insert(
            "rules".into(),
            json!([
                "every commit carries a `Task: <id>` trailer",
                "every task record declares scopes; commits stay inside them (own record and declared evidence exempt)",
                "close the record when its work lands",
                "every commit and merge is created by an agent's local Git or Susi tooling, never by the Git host (merges happen in Susi; the host is storage only)",
                "advice, not a finding (CEO Mandate 78): a task of size m or larger states its reuse decision in its record's `reuse` object"
            ]),
        );
    }
    let pretty = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())? + "\n";
    if let Some(out) = &a.out {
        std::fs::write(out, pretty.as_bytes()).map_err(|e| format!("cannot write {out}: {e}"))?;
    }
    let text = if a.json {
        pretty
    } else {
        render_lint_text(&report)
    };
    let findings = report["summary"]["findings"].as_u64().unwrap_or(0);
    if a.strict && findings > 0 {
        return Err(format!("{findings} label finding(s) (--strict)\n{text}"));
    }
    Ok(text)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::super::{parse_record, run};
    use super::*;

    fn rec(path: &str, body: &str) -> Record {
        parse_record(path, body.as_bytes()).unwrap().unwrap()
    }

    fn commit(n: u8, tasks: &[&str], files: &[&str]) -> Commit {
        Commit {
            oid: format!("{:040x}", n),
            subject: format!("c{n}"),
            tasks: tasks.iter().map(|s| (*s).to_owned()).collect(),
            local: Vec::new(),
            malformed: Vec::new(),
            files: files.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    fn records() -> Vec<Record> {
        vec![
            rec(
                ".agents/tasks/done/T-A-1.json",
                r#"{"id":"T-A-1","status":"done","scopes":["src/a"]}"#,
            ),
            rec(
                ".agents/tasks/T-A-1.json",
                r#"{"id":"T-A-1","status":"in_progress","scopes":["src/a"]}"#,
            ),
            rec(
                ".agents/tasks/T-B-1.json",
                r#"{"id":"T-B-1","status":"in_progress","scopes":["src/b.rs","docs/*.md"],"evidence":[".agents/evidence/b.json"]}"#,
            ),
            rec(
                ".agents/tasks/T-C-1.json",
                r#"{"id":"T-C-1","status":"implemented"}"#,
            ),
        ]
    }

    #[test]
    fn trailers_split_on_commas_and_whitespace_and_flag_junk() {
        let (t, l, m) = split_trailers("T-B-1, T-A-1\u{1d}T-A-1 fixes\u{1d}LOCAL-x");
        assert_eq!(t, vec!["T-A-1", "T-B-1"]);
        assert_eq!(l, vec!["LOCAL-x"]);
        assert_eq!(m, vec!["fixes"]);
    }

    #[test]
    fn scope_coverage_handles_dirs_files_and_globs() {
        assert!(covers("src/a", "src/a/x.rs"));
        assert!(covers("src/a/", "src/a/x.rs"));
        assert!(!covers("src/a", "src/ab.rs"));
        assert!(covers("src/b.rs", "src/b.rs"));
        assert!(covers("docs/*.md", "docs/x.md"));
        assert!(!covers("docs/*.md", "src/x.md"));
        assert!(own_record("T-A-1", ".agents/tasks/done/t-a-1.json"));
        assert!(!own_record("T-A-1", ".agents/tasks/T-A-10.json"));
    }

    #[test]
    fn lint_reports_each_finding_kind() {
        let commits = vec![
            commit(1, &[], &["x"]),
            commit(2, &["T-A-1"], &["src/a/x.rs", "README"]),
            commit(
                3,
                &["T-B-1"],
                &[
                    "src/b.rs",
                    "docs/b.md",
                    ".agents/tasks/T-B-1.json",
                    ".agents/evidence/b.json",
                ],
            ),
            commit(4, &["T-NOPE-1"], &["y"]),
            commit(5, &["T-C-1"], &["z"]),
            Commit {
                local: vec!["LOCAL-n".into()],
                ..commit(6, &[], &["w"])
            },
        ];
        let r = lint(&commits, &records(), &BTreeMap::new());
        let s = &r["summary"];
        assert_eq!(s["commits"], 6);
        assert_eq!(s["commits_with_local_label_only"], 1);
        assert_eq!(s["commits_with_task_trailer"], 4);
        assert_eq!(s["commits_without_task_trailer"], 1);
        assert_eq!(s["tasks_without_record"], 1);
        // T-A-1 is closed (done/ wins), T-B-1 is open, T-C-1 implemented.
        assert_eq!(s["tasks_landed_but_record_open"], 1);
        assert_eq!(r["tasks_landed_but_record_open"][0]["task"], "T-B-1");
        assert_eq!(s["tasks_landed_but_record_implemented_not_closed"], 1);
        assert_eq!(s["tasks_without_scopes"], 1);
        assert_eq!(s["commits_touching_files_outside_task_scopes"], 1);
        assert_eq!(
            r["commits_touching_files_outside_task_scopes"][0]["files"][0],
            "README"
        );
        assert_eq!(s["findings"], 5);
        // A claim ref supplies scopes the record lacks.
        let claims = BTreeMap::from([("T-C-1".to_owned(), vec!["z".to_owned()])]);
        let r = lint(&commits, &records(), &claims);
        assert_eq!(r["summary"]["tasks_without_scopes"], 0);
        assert_eq!(r["summary"]["tasks_scoped_by_claim_ref_only"], 1);
        assert_eq!(r["summary"]["findings"], 4);
    }

    #[test]
    fn lint_is_deterministic() {
        let commits = vec![commit(2, &["T-B-1", "T-A-1"], &["q"]), commit(1, &[], &[])];
        let a = serde_json::to_string(&lint(&commits, &records(), &BTreeMap::new())).unwrap();
        let mut recs = records();
        recs.reverse();
        let b = serde_json::to_string(&lint(&commits, &recs, &BTreeMap::new())).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn log_output_parses_with_files_and_trailers() {
        let o1 = "a".repeat(40);
        let o2 = "b".repeat(40);
        let text = format!(
            "\u{1e}{o1}\u{1f}feat: x\u{1f}T-A-1\u{1d}T-B-1\u{1f}\n\nsrc/a/x.rs\nREADME\n\u{1e}{o2}\u{1f}chore\u{1f}\u{1f}\n"
        );
        let c = parse_log(&text);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].tasks, vec!["T-A-1", "T-B-1"]);
        assert_eq!(c[0].files, vec!["README", "src/a/x.rs"]);
        assert!(c[1].tasks.is_empty() && c[1].files.is_empty());
    }

    #[test]
    fn reuse_decisions_are_advice_and_never_findings() {
        let after = REUSE_EFFECTIVE_UNIX + 1;
        let record = |size: &str, created: u64, reuse: Value| json!({"size": size, "created_unix": created, "reuse": reuse});
        // Not owed: older records and small tasks.
        assert_eq!(
            reuse_gap(&record("l", REUSE_EFFECTIVE_UNIX - 1, Value::Null)),
            None
        );
        assert_eq!(reuse_gap(&record("S", after, Value::Null)), None);
        assert_eq!(reuse_gap(&record("xs", after, Value::Null)), None);
        // Owed and stated.
        for ok in [
            json!({"decision": "existing"}),
            json!({"decision": "dependency"}),
            json!({"decision": "harvest", "source": "https://example.invalid/x@0123abc"}),
            json!({"decision": "build", "reason": "no Apache-2.0 project implements the GMCP task plane"}),
        ] {
            assert_eq!(reuse_gap(&record("m", after, ok.clone())), None, "{ok}");
        }
        // Owed and missing or incomplete.
        assert_eq!(
            reuse_gap(&record("M", after, Value::Null)),
            Some("no reuse decision")
        );
        assert_eq!(
            reuse_gap(&json!({"created_unix": after})),
            Some("no reuse decision")
        );
        assert_eq!(
            reuse_gap(&record(
                "l",
                after,
                json!({"decision": "build", "reason": " "})
            )),
            Some("a build gives no reason")
        );
        assert_eq!(
            reuse_gap(&record("l", after, json!({"decision": "harvest"}))),
            Some("a harvest names no source")
        );
        assert_eq!(
            reuse_gap(&record("l", after, json!({"decision": "copied"}))),
            Some("unknown reuse decision")
        );
        // Advice is reported beside the findings and never added to them.
        let mut report = json!({"summary": {"findings": 0}});
        add_reuse_advice(&mut report, &[("T-A-1".to_owned(), "no reuse decision")]);
        assert_eq!(report["summary"]["findings"], 0);
        assert_eq!(
            report["summary"]["advisory_tasks_without_reuse_decision"],
            1
        );
        assert_eq!(report["tasks_without_reuse_decision"][0]["task"], "T-A-1");
    }

    #[test]
    fn lint_arguments_are_strict() {
        let s = |v: &[&str]| v.iter().map(|x| (*x).to_owned()).collect::<Vec<_>>();
        assert!(parse_lint_args(&s(&[])).is_err());
        assert!(parse_lint_args(&s(&["--range"])).is_err());
        assert!(parse_lint_args(&s(&["--range", "a..b", "--bogus"])).is_err());
        let a = parse_lint_args(&s(&["--range", "a..b", "--strict", "--json"])).unwrap();
        assert!(a.strict && a.json && a.range == "a..b");
    }

    #[test]
    fn real_git_lint_reads_history_and_strict_fails() {
        let dir = std::env::temp_dir().join(format!("susi-distill-lint-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let g = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(args)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_AUTHOR_NAME", "fixture")
                .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
                .env("GIT_COMMITTER_NAME", "fixture")
                .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}");
            String::from_utf8_lossy(&out.stdout).trim().to_owned()
        };
        let w = |p: &str, b: &str| {
            let p = dir.join(p);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, b).unwrap();
        };
        g(&["init", "-q", "-b", "main"]);
        w("README", "r\n");
        g(&["add", "."]);
        g(&["commit", "-qm", "base"]);
        let start = g(&["rev-parse", "HEAD"]);
        w(
            ".agents/tasks/T-L-1.json",
            r#"{"id":"T-L-1","status":"in_progress","scopes":["l.txt"]}"#,
        );
        w("l.txt", "l\n");
        w("stray.txt", "s\n");
        g(&["add", "."]);
        g(&["commit", "-qm", "l\n\nTask: T-L-1"]);
        w("u.txt", "u\n");
        g(&["add", "."]);
        g(&["commit", "-qm", "unlabelled"]);
        let s = |v: &[&str]| v.iter().map(|x| (*x).to_owned()).collect::<Vec<_>>();
        w(
            "claim.json",
            r#"{"task":"T-L-1","scopes":["l.txt","stray.txt"]}"#,
        );
        let blob = g(&["hash-object", "-w", "claim.json"]);
        std::fs::remove_file(dir.join("claim.json")).unwrap();
        g(&["update-ref", "refs/claims/T-L-1", &blob]);
        let args = s(&["lint", "--range", &format!("{start}..main"), "--json"]);
        let text = run(&dir, &args).unwrap();
        let r: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(r["schema"], LINT_SCHEMA);
        assert_eq!(r["summary"]["commits"], 2, "{text}");
        assert_eq!(r["summary"]["commits_without_task_trailer"], 1);
        assert_eq!(r["summary"]["tasks_landed_but_record_open"], 1);
        // The record's own scopes win over the claim ref.
        assert_eq!(r["claim_refs_read"], 1);
        assert_eq!(r["summary"]["tasks_scoped_by_claim_ref_only"], 0);
        assert_eq!(
            r["commits_touching_files_outside_task_scopes"][0]["files"][0],
            "stray.txt"
        );
        assert_eq!(
            run(&dir, &args).unwrap(),
            text,
            "repeat run must be identical"
        );
        let full = run(&dir, &s(&["lint", "--range", "main", "--json"])).unwrap();
        let f: Value = serde_json::from_str(&full).unwrap();
        assert_eq!(f["summary"]["commits"], 3);
        let strict = run(&dir, &s(&["lint", "--range", "main", "--strict"]));
        assert!(strict.unwrap_err().contains("--strict"));
        assert!(run(&dir, &s(&["lint", "--range", "..main"])).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
