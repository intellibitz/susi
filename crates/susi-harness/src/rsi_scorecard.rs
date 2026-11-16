//! Pure-Rust RSI loop scorecard over `.agents/tasks`, git history,
//! merge attestations (`refs/merged/<id>`), and `.agents/rsi-loop.jsonl`.

use crate::git_governor::{cat_file_batch_json, run_git};
use crate::lifecycle::DEFAULT_LIFECYCLE_PATH;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;
use std::path::Path;
use susi_error::{EaiError, EaiResult};

const PHASES: [&str; 6] = [
    "proposed",
    "implemented",
    "gated",
    "merged",
    "released",
    "promoted",
];

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).map_or("", str::trim)
}

fn pick<'a>(v: &'a Value, a: &str, b: &str) -> &'a str {
    let x = s(v, a);
    if x.is_empty() {
        s(v, b)
    } else {
        x
    }
}

fn u(v: Option<&Value>) -> Option<u64> {
    let v = v?;
    v.as_u64()
        .or_else(|| v.as_i64().and_then(|n| u64::try_from(n).ok()))
        .or_else(|| v.as_str()?.trim().parse().ok())
}

fn git_text(root: &Path, args: &[&str]) -> String {
    match run_git(root, args, None, &[]) {
        Ok(o) if o.ok() => String::from_utf8_lossy(&o.stdout).into_owned(),
        _ => String::new(),
    }
}

#[must_use]
pub fn score_task(t: &Value, evs: &[Value], imp: Option<u64>, att: Option<&Value>) -> Value {
    let (mut obs, mut cost, mut unit, mut defects) = (BTreeMap::new(), 0.0, None::<String>, 0);
    for ev in evs {
        let ph = s(ev, "phase").to_ascii_lowercase();
        let ts = u(ev.get("at_unix").or_else(|| ev.get("timestamp")));
        if let Some(v) = ts.filter(|_| PHASES.contains(&ph.as_str())) {
            obs.entry(ph.clone()).or_insert(v);
        }
        let oc = pick(ev, "outcome", "status").to_ascii_lowercase();
        if ph == "defect" || matches!(oc.as_str(), "failed" | "failure" | "defect") {
            defects += 1;
        }
        let cv = ev
            .get("cost")
            .and_then(|v| v.as_f64().or_else(|| v.as_str()?.trim().parse().ok()));
        if let Some(c) = cv.filter(|c| c.is_finite() && *c >= 0.0) {
            cost += c;
            let cu = match s(ev, "cost_unit") {
                "" => "units",
                x => x,
            };
            unit = Some(match unit {
                None => cu.into(),
                Some(u) if u == cu => u,
                _ => "mixed".into(),
            });
        }
    }
    defects += t["defects"].as_array().map_or(0, Vec::len);
    for (k, v) in [
        ("proposed", u(t.get("created_unix"))),
        ("implemented", imp),
        ("gated", u(t["closed"].get("at_unix"))),
        ("merged", u(att.and_then(|a| a.get("merged_unix")))),
    ] {
        if let Some(ts) = v {
            obs.entry(k.into()).or_insert(ts);
        }
    }
    let miss: Vec<_> = PHASES
        .iter()
        .filter(|&&p| !obs.contains_key(p))
        .copied()
        .collect();
    let ord: Vec<_> = PHASES
        .iter()
        .filter_map(|&p| obs.get(p).map(|&v| (p, v)))
        .collect();
    let probs: Vec<_> = ord
        .windows(2)
        .filter_map(|w| match w {
            [(l, lt), (r, rt)] if rt < lt => Some(format!("{r} ({rt}) precedes {l} ({lt})")),
            _ => None,
        })
        .collect();
    let span = ord
        .first()
        .zip(ord.last())
        .and_then(|((_, lo), (_, hi))| hi.checked_sub(*lo));
    let elapsed = probs.is_empty().then_some(span).flatten();
    let st = if !probs.is_empty() {
        "invalid"
    } else if miss.is_empty() {
        "complete"
    } else {
        "in_progress"
    };
    let phases: BTreeMap<_, _> = PHASES.iter().map(|&p| (p, obs.get(p).copied())).collect();
    json!({
        "task": s(t, "id"), "title": s(t, "title"), "status": st, "phases": phases,
        "missing": miss, "stop_at": miss.first().copied(), "elapsed_seconds": elapsed,
        "cost": unit.map(|u| json!({ "value": cost, "unit": u })), "defects": defects, "problems": probs,
    })
}

#[must_use]
pub fn score(root: &Path, sel: Option<&BTreeSet<String>>, ev_path: Option<&Path>) -> Value {
    let mut tmap = BTreeMap::new();
    for rel in [".agents/tasks", ".agents/tasks/done"] {
        for e in std::fs::read_dir(root.join(rel))
            .into_iter()
            .flatten()
            .flatten()
        {
            let p = e.path();
            if p.extension() == Some("json".as_ref()) {
                let raw = std::fs::read_to_string(&p).ok();
                if let Some(v) = raw.and_then(|r| serde_json::from_str::<Value>(&r).ok()) {
                    let tid = s(&v, "id");
                    if !tid.is_empty() {
                        tmap.insert(tid.to_owned(), v);
                    }
                }
            }
        }
    }
    let mut tasks: Vec<_> = tmap.into_values().collect();
    if let Some(f) = sel.filter(|s| !s.is_empty()) {
        tasks.retain(|t| f.contains(s(t, "id")));
    }
    let def = root.join(DEFAULT_LIFECYCLE_PATH);
    let evp = ev_path.or_else(|| def.is_file().then_some(def.as_path()));
    let (mut by_t, mut probs) = (HashMap::<String, Vec<Value>>::new(), Vec::new());
    if let Some(p) = evp.filter(|p| p.is_file()) {
        let label = p.display();
        match std::fs::read_to_string(p) {
            Err(e) => probs.push(format!("cannot read {label}: {e}")),
            Ok(txt) => {
                for (i, ln) in txt
                    .lines()
                    .enumerate()
                    .filter(|(_, l)| !l.trim().is_empty())
                {
                    let no = i + 1;
                    match serde_json::from_str::<Value>(ln) {
                        Ok(ev) if ev.is_object() => {
                            let tid = pick(&ev, "task", "task_id");
                            if !tid.is_empty() {
                                by_t.entry(tid.into()).or_default().push(ev);
                            }
                        }
                        Ok(_) => probs.push(format!("{label}:{no}: not object")),
                        Err(e) => probs.push(format!("{label}:{no}: {e}")),
                    }
                }
            }
        }
    }
    let wanted: BTreeSet<String> = tasks.iter().map(|t| s(t, "id").into()).collect();
    let mut commits = HashMap::new();
    let log_args = ["log", "--all", "--format=%ct%x1f%B%x1e", "--reverse"];
    let log = if wanted.is_empty() {
        String::new()
    } else {
        git_text(root, &log_args)
    };
    for rec in log.split('\x1e') {
        let Some((t, body)) = rec.split_once('\x1f') else {
            continue;
        };
        let Ok(ts) = t.trim().parse::<u64>() else {
            continue;
        };
        for l in body
            .lines()
            .filter_map(|l| l.strip_prefix("Task:").map(str::trim))
        {
            if wanted.contains(l) {
                commits.entry(l.to_owned()).or_insert(ts);
            }
        }
    }
    let ref_args = ["for-each-ref", "--format=%(refname)", "refs/merged"];
    let refs = git_text(root, &ref_args);
    let hits: Vec<_> = refs
        .lines()
        .filter_map(|l| {
            l.trim()
                .strip_prefix("refs/merged/")
                .filter(|t| wanted.contains(*t))
        })
        .collect();
    let specs: Vec<_> = hits.iter().map(|t| format!("refs/merged/{t}")).collect();
    let blobs = cat_file_batch_json(root, &specs).unwrap_or_default();
    let atts: HashMap<_, _> = hits
        .into_iter()
        .zip(blobs)
        .filter_map(|(t, v)| Some((t, v.filter(Value::is_object)?)))
        .collect();
    let rows: Vec<_> = tasks
        .iter()
        .map(|t| {
            let tid = s(t, "id");
            let evs = by_t.get(tid).map_or(&[][..], Vec::as_slice);
            score_task(t, evs, commits.get(tid).copied(), atts.get(tid))
        })
        .collect();
    json!({ "phases": PHASES, "tasks": rows, "problems": probs })
}

pub fn run(
    root: &Path,
    ev: &Path,
    tasks: &[String],
    json: bool,
    req_done: bool,
) -> EaiResult<String> {
    let sel: BTreeSet<String> = tasks.iter().cloned().collect();
    let r = score(
        root,
        (!sel.is_empty()).then_some(&sel),
        Some(&root.join(ev)),
    );
    let n = r["problems"].as_array().map_or(0, Vec::len);
    if n > 0 {
        return Err(EaiError::config(format!(
            "scorecard reported {n} problem(s)"
        )));
    }
    if req_done
        && r["tasks"]
            .as_array()
            .is_some_and(|a| a.iter().any(|x| x["status"] != "complete"))
    {
        return Err(EaiError::config("scorecard requires all tasks complete"));
    }
    if json {
        return serde_json::to_string_pretty(&r)
            .map(|s| format!("{s}\n"))
            .map_err(|e| EaiError::io(e.to_string()));
    }
    let mut t = String::from("RSI self-build scorecard\ntask                         status       elapsed   cost              defects  stop\n---------------------------  -----------  --------  ----------------  -------  --------\n");
    let rows = r["tasks"].as_array().map_or(&[][..], Vec::as_slice);
    for row in rows {
        let dur = match row["elapsed_seconds"].as_u64() {
            None => "—".into(),
            Some(v) if v >= 3600 => format!("{}h {:02}m", v / 3600, (v % 3600) / 60),
            Some(v) if v >= 60 => format!("{}m {:02}s", v / 60, v % 60),
            Some(v) => format!("{v}s"),
        };
        let c = &row["cost"];
        let cost = c
            .is_object()
            .then(|| format!("{} {}", c["value"], s(c, "unit")));
        let (cost, col) = (
            cost.as_deref().unwrap_or("—"),
            s(row, "task").chars().take(27).collect::<String>(),
        );
        let stop = Some(s(row, "stop_at"))
            .filter(|x| !x.is_empty())
            .unwrap_or("—");
        let (st, df) = (s(row, "status"), row["defects"].as_u64().unwrap_or(0));
        let _ = writeln!(
            t,
            "{col:<27}  {st:<11}  {dur:>8}  {cost:<16}  {df:>7}  {stop}"
        );
    }
    if rows.is_empty() {
        t.push_str("(no task records found)\n");
    }
    Ok(t)
}

#[cfg(test)]
#[path = "tests/rsi_scorecard.rs"]
mod tests;
