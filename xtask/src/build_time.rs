//! `cargo xtask build-time`: the lean-core compile budget, measured.
//!
//! Cold-builds the `susi` host binary (dev profile, offline, locked) into a
//! fresh target directory with `--timings`, then reads cargo's own unit data
//! out of the HTML report and prints what the budget in
//! `docs/lean-core.md` is pinned on: wall clock, rustc CPU-seconds (all
//! units; workspace and third-party apart), the critical path, the serial
//! tail (wall seconds with fewer than four units compiling) and the heaviest
//! units. One JSON line per run is appended to `.agents/build-time.jsonl`
//! so the number is a record, not a claim.
//!
//! Budgets are optional: `--budget-wall 60` and `--budget-cpu 1200` make the
//! command fail when the measurement exceeds them, so the same command can
//! be a gate once the numbers are under control.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// One cargo unit (a crate build, a build script, a test target) as the
/// timings report records it.
#[derive(Debug, Clone)]
struct Unit {
    index: usize,
    name: String,
    target: String,
    start: f64,
    duration: f64,
    unblocked: Vec<usize>,
}

impl Unit {
    fn end(&self) -> f64 {
        self.start + self.duration
    }

    fn label(&self) -> String {
        if self.target == "lib" || self.target.is_empty() {
            self.name.clone()
        } else {
            format!("{} ({})", self.name, self.target)
        }
    }
}

/// What one measurement found.
#[derive(Debug)]
pub struct Measurement {
    /// Package built.
    pub package: String,
    /// Wall clock of the cargo invocation.
    pub wall_s: f64,
    /// Sum of every unit's duration.
    pub cpu_s: f64,
    /// CPU-seconds of workspace units (names starting with `susi` or `xtask`).
    pub workspace_s: f64,
    /// CPU-seconds of everything else.
    pub third_party_s: f64,
    /// Units compiled.
    pub units: usize,
    /// Critical path, first to last, as `(label, duration)`.
    pub critical_path: Vec<(String, f64)>,
    /// Wall seconds with fewer than four units compiling.
    pub serial_tail_s: f64,
    /// Heaviest units by CPU-seconds, summed per crate name.
    pub heaviest: Vec<(String, f64)>,
    /// Logical CPUs on this host.
    pub threads: usize,
}

/// Parsed command line.
struct Options {
    package: String,
    budget_wall: Option<f64>,
    budget_cpu: Option<f64>,
    keep_target: bool,
}

fn parse(args: &[String]) -> Result<Options> {
    let mut options = Options {
        package: "susi".to_string(),
        budget_wall: None,
        budget_cpu: None,
        keep_target: false,
    };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--package" | "-p" => {
                options.package = it.next().ok_or("--package needs a value")?.clone();
            }
            "--budget-wall" => {
                options.budget_wall =
                    Some(it.next().ok_or("--budget-wall needs seconds")?.parse()?);
            }
            "--budget-cpu" => {
                options.budget_cpu = Some(it.next().ok_or("--budget-cpu needs seconds")?.parse()?);
            }
            "--keep-target" => options.keep_target = true,
            other => return Err(format!("build-time: unknown argument {other}").into()),
        }
    }
    Ok(options)
}

/// Run the measurement from the workspace root and print the report.
pub fn run(args: &[String]) -> Result<()> {
    let options = parse(args)?;
    let root = workspace_root()?;
    let target_dir = root.join("target").join("build-time");
    if target_dir.exists() {
        std::fs::remove_dir_all(&target_dir)?;
    }
    let measurement = measure(&root, &target_dir, &options.package)?;
    if !options.keep_target {
        let _ = std::fs::remove_dir_all(&target_dir);
    }
    print_report(&measurement);
    record(&root, &measurement)?;
    let mut over = Vec::new();
    if let Some(budget) = options.budget_wall {
        if measurement.wall_s > budget {
            over.push(format!(
                "wall {:.0} s > budget {budget:.0} s",
                measurement.wall_s
            ));
        }
    }
    if let Some(budget) = options.budget_cpu {
        if measurement.cpu_s > budget {
            over.push(format!(
                "cpu {:.0} s > budget {budget:.0} s",
                measurement.cpu_s
            ));
        }
    }
    if over.is_empty() {
        Ok(())
    } else {
        Err(format!("build-time: over budget: {}", over.join("; ")).into())
    }
}

fn workspace_root() -> Result<PathBuf> {
    let manifest = std::env::var_os("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir()?);
    let mut probe = manifest.as_path();
    loop {
        let candidate = probe.join("Cargo.toml");
        if candidate.exists() {
            let text = std::fs::read_to_string(&candidate)?;
            if text.contains("[workspace]") {
                return Ok(probe.to_path_buf());
            }
        }
        probe = probe
            .parent()
            .ok_or("build-time: no workspace Cargo.toml above the current directory")?;
    }
}

/// Cold-build `package` into `target_dir` and read the timings report.
pub fn measure(root: &Path, target_dir: &Path, package: &str) -> Result<Measurement> {
    let started = Instant::now();
    let status = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string()))
        .current_dir(root)
        .args([
            "build",
            "-p",
            package,
            "--timings",
            "--offline",
            "--locked",
            "--target-dir",
        ])
        .arg(target_dir)
        .env_remove("RUSTC_WRAPPER")
        .status()?;
    let wall_s = started.elapsed().as_secs_f64();
    if !status.success() {
        return Err(format!("build-time: cargo build -p {package} failed ({status})").into());
    }
    let report = target_dir.join("cargo-timings").join("cargo-timing.html");
    let html = std::fs::read_to_string(&report)
        .map_err(|e| format!("build-time: {}: {e}", report.display()))?;
    let units = parse_units(&html)?;
    Ok(summarize(package, wall_s, units))
}

/// The `UNIT_DATA` array cargo embeds in its timings page.
fn parse_units(html: &str) -> Result<Vec<Unit>> {
    let start = html
        .find("const UNIT_DATA = ")
        .ok_or("build-time: no UNIT_DATA in the timings report")?
        + "const UNIT_DATA = ".len();
    let rest = &html[start..];
    let end = rest
        .find("];")
        .ok_or("build-time: unterminated UNIT_DATA")?;
    let json: serde_json::Value = serde_json::from_str(&rest[..=end])?;
    let array = json
        .as_array()
        .ok_or("build-time: UNIT_DATA is not an array")?;
    let mut units = Vec::with_capacity(array.len());
    for item in array {
        let indices = |key: &str| -> Vec<usize> {
            item.get(key)
                .and_then(serde_json::Value::as_array)
                .map(|v| {
                    v.iter()
                        .filter_map(serde_json::Value::as_u64)
                        .filter_map(|n| usize::try_from(n).ok())
                        .collect()
                })
                .unwrap_or_default()
        };
        let mut unblocked = indices("unblocked_units");
        unblocked.extend(indices("unblocked_rmeta_units"));
        units.push(Unit {
            index: item
                .get("i")
                .and_then(serde_json::Value::as_u64)
                .and_then(|n| usize::try_from(n).ok())
                .ok_or("build-time: unit without index")?,
            name: item
                .get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("?")
                .to_string(),
            target: item
                .get("target")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string(),
            start: item
                .get("start")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(0.0),
            duration: item
                .get("duration")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(0.0),
            unblocked,
        });
    }
    Ok(units)
}

fn is_workspace(name: &str) -> bool {
    name.starts_with("susi") || name == "xtask"
}

fn summarize(package: &str, wall_s: f64, units: Vec<Unit>) -> Measurement {
    let cpu_s: f64 = units.iter().map(|u| u.duration).sum();
    let workspace_s: f64 = units
        .iter()
        .filter(|u| is_workspace(&u.name))
        .map(|u| u.duration)
        .sum();
    // Predecessors: the units whose completion unblocked each unit.
    let mut predecessors: std::collections::HashMap<usize, Vec<usize>> =
        std::collections::HashMap::new();
    for unit in &units {
        for &next in &unit.unblocked {
            predecessors.entry(next).or_default().push(unit.index);
        }
    }
    let by_index: std::collections::HashMap<usize, &Unit> =
        units.iter().map(|u| (u.index, u)).collect();
    let mut critical_path = Vec::new();
    if let Some(last) = units.iter().max_by(|a, b| {
        a.end()
            .partial_cmp(&b.end())
            .unwrap_or(std::cmp::Ordering::Equal)
    }) {
        let mut seen = std::collections::HashSet::new();
        let mut current = last;
        loop {
            if !seen.insert(current.index) {
                break;
            }
            critical_path.push((current.label(), current.duration));
            let Some(previous) = predecessors.get(&current.index).and_then(|ps| {
                ps.iter()
                    .filter_map(|p| by_index.get(p).copied())
                    .max_by(|a, b| {
                        a.end()
                            .partial_cmp(&b.end())
                            .unwrap_or(std::cmp::Ordering::Equal)
                    })
            }) else {
                break;
            };
            current = previous;
        }
        critical_path.reverse();
    }
    // Serial tail: wall time with fewer than four units running.
    let mut events: Vec<(f64, i32)> = Vec::with_capacity(units.len() * 2);
    for unit in &units {
        events.push((unit.start, 1));
        events.push((unit.end(), -1));
    }
    events.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let (mut running, mut last_t, mut serial_tail_s) = (0i32, 0.0f64, 0.0f64);
    for (t, delta) in events {
        if running < 4 {
            serial_tail_s += t - last_t;
        }
        running += delta;
        last_t = t;
    }
    let mut per_crate: std::collections::HashMap<&str, f64> = std::collections::HashMap::new();
    for unit in &units {
        *per_crate.entry(unit.name.as_str()).or_insert(0.0) += unit.duration;
    }
    let mut heaviest: Vec<(String, f64)> = per_crate
        .into_iter()
        .map(|(n, d)| (n.to_string(), d))
        .collect();
    heaviest.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    heaviest.truncate(20);
    Measurement {
        package: package.to_string(),
        wall_s,
        cpu_s,
        workspace_s,
        third_party_s: cpu_s - workspace_s,
        units: units.len(),
        critical_path,
        serial_tail_s,
        heaviest,
        threads: std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1),
    }
}

fn print_report(m: &Measurement) {
    println!(
        "build-time: cold `cargo build -p {}` on {} threads",
        m.package, m.threads
    );
    println!("  wall            {:8.1} s", m.wall_s);
    println!("  rustc cpu       {:8.1} s  ({} units)", m.cpu_s, m.units);
    println!("    workspace     {:8.1} s", m.workspace_s);
    println!("    third-party   {:8.1} s", m.third_party_s);
    println!(
        "  serial tail     {:8.1} s  (fewer than 4 units compiling)",
        m.serial_tail_s
    );
    let path_s: f64 = m.critical_path.iter().map(|(_, d)| d).sum();
    println!(
        "  critical path   {:8.1} s  ({} units)",
        path_s,
        m.critical_path.len()
    );
    for (label, duration) in &m.critical_path {
        println!("    {duration:6.1} s  {label}");
    }
    println!("  heaviest crates (cpu-seconds)");
    for (name, duration) in &m.heaviest {
        println!("    {duration:6.1} s  {name}");
    }
}

/// Append one JSON line to `.agents/build-time.jsonl`.
fn record(root: &Path, m: &Measurement) -> Result<()> {
    let commit = Command::new("git")
        .current_dir(root)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    let recorded_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let line = serde_json::json!({
        "recorded_unix": recorded_unix,
        "commit": commit,
        "package": m.package,
        "host_os": std::env::consts::OS,
        "threads": m.threads,
        "wall_s": round1(m.wall_s),
        "cpu_s": round1(m.cpu_s),
        "workspace_s": round1(m.workspace_s),
        "third_party_s": round1(m.third_party_s),
        "units": m.units,
        "serial_tail_s": round1(m.serial_tail_s),
        "critical_path_s": round1(m.critical_path.iter().map(|(_, d)| d).sum()),
        "critical_path": m.critical_path.iter().map(|(l, _)| l.clone()).collect::<Vec<_>>(),
        "heaviest": m.heaviest.iter().map(|(n, d)| serde_json::json!({"crate": n, "cpu_s": round1(*d)})).collect::<Vec<_>>(),
    });
    let dir = root.join(".agents");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("build-time.jsonl");
    let mut text = std::fs::read_to_string(&path).unwrap_or_default();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&serde_json::to_string(&line)?);
    text.push('\n');
    std::fs::write(&path, text)?;
    println!("build-time: recorded in {}", path.display());
    Ok(())
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<html><script>
const UNIT_DATA = [
{"i":0,"name":"syn","version":"2.0","mode":"run-custom-build","target":"build-script","start":0.0,"duration":2.0,"unblocked_units":[1],"unblocked_rmeta_units":[]},
{"i":1,"name":"syn","version":"2.0","mode":"todo","target":"lib","start":2.0,"duration":10.0,"unblocked_units":[2],"unblocked_rmeta_units":[]},
{"i":2,"name":"susi-core","version":"0.1","mode":"todo","target":"lib","start":12.0,"duration":5.0,"unblocked_units":[3],"unblocked_rmeta_units":[]},
{"i":3,"name":"susi","version":"0.1","mode":"todo","target":"bin","start":17.0,"duration":3.0,"unblocked_units":[],"unblocked_rmeta_units":[]},
{"i":4,"name":"regex","version":"1.0","mode":"todo","target":"lib","start":0.0,"duration":6.0,"unblocked_units":[],"unblocked_rmeta_units":[]}
];
const CONCURRENCY_DATA = [];
</script></html>"#;

    #[test]
    fn units_critical_path_and_split_are_read_from_the_report() {
        let units = parse_units(SAMPLE).unwrap_or_default();
        assert_eq!(units.len(), 5);
        let m = summarize("susi", 20.5, units);
        assert!((m.cpu_s - 26.0).abs() < 1e-9);
        assert!((m.workspace_s - 8.0).abs() < 1e-9);
        assert!((m.third_party_s - 18.0).abs() < 1e-9);
        let labels: Vec<&str> = m.critical_path.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(
            labels,
            ["syn (build-script)", "syn", "susi-core", "susi (bin)"],
            "{labels:?}"
        );
        // Never more than 2 units run at once here, so the whole wall is "tail".
        assert!((m.serial_tail_s - 20.0).abs() < 1e-9, "{}", m.serial_tail_s);
        assert_eq!(m.heaviest[0].0, "syn");
        assert!((m.heaviest[0].1 - 12.0).abs() < 1e-9);
    }

    #[test]
    fn arguments_parse_and_reject_unknown_flags() {
        let o = parse(&["--budget-wall".into(), "60".into(), "--keep-target".into()])
            .ok()
            .filter(|o| o.budget_wall == Some(60.0) && o.keep_target && o.package == "susi");
        assert!(o.is_some());
        assert!(parse(&["--bogus".into()]).is_err());
    }
}
