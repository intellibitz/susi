//! D-SP-2 / Step 1 user-response gate: the declared millisecond budgets a
//! user's first interactions must meet.
//!
//! It times `susi --version`, `susi --help` and `susi status` (daemon down,
//! then daemon up), and a piped first goal (daemon down, then up) to its first
//! output and to completion. Budgets are absolute milliseconds, judged on the
//! optimized executable that ships (`--susi`); see `docs/step1-acceptance.md`
//! for their rationale. The cost of starting a program that does nothing (this
//! binary re-executed with `--floor-probe`) is measured and reported as host
//! context only: on one host it moved between 6.7 and 9.6 ms, so a multiple of
//! it passed and failed the same binary on consecutive runs. Each case runs
//! once untimed (file cache), then `--runs` times; the median is judged.
//!
//! The goal is a local, tool-free read of a nonce file under `local_only` and
//! `SUSI_OFFLINE=1`, with provider credentials and endpoints removed; every
//! run must answer with the nonce. Nothing is printed before the answer, so
//! its first output is also the goal's acknowledgment. Model-backed answers
//! are outside this gate.
//!
//! Every child runs from an empty directory with an isolated `SUSI_HOME` and
//! a free `SUSI_PORT_OFFSET`, so the daemon this gate starts never touches the
//! host's own instance or its ports. The gate stops only the daemon it
//! started (`susi stop` under that home): when a daemon already runs under
//! that home (for example `--home` pointing at a live instance), it skips the
//! daemon-up cases, reports an error and never starts or stops anything. With
//! `--home` the goal cases are skipped: they write a privacy setting into the
//! instance home, which only the gate's own home may receive.
//!
//! Output: one JSON object on stdout (`susi-startup-gate-v2`); progress on
//! stderr. Exit 0 when every measured case is within budget, 1 when one is
//! not, 2 when the gate could not run.
//!
//! ```text
//! startup-gate [--susi PATH] [--home DIR] [--runs N] [--skip-daemon-up] [--keep-home]
//! ```
#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use std::net::{Ipv4Addr, TcpListener, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const SCHEMA: &str = "susi-startup-gate-v2";
/// The hidden mode that does nothing: the process-spawn floor (context only).
const FLOOR_PROBE: &str = "--floor-probe";
const DEFAULT_RUNS: usize = 5;
/// `--version` and `--help` may cost at most this many milliseconds.
const VERSION_HELP_BUDGET_MS: f64 = 30.0;
/// `status` may cost at most this many milliseconds, daemon up or down.
const STATUS_BUDGET_MS: f64 = 50.0;
/// A piped first goal's first output (its acknowledgment) may take at most
/// this many milliseconds, daemon up or down.
const GOAL_FIRST_OUTPUT_BUDGET_MS: f64 = 600.0;
/// A piped first goal may take at most this many milliseconds to complete.
const GOAL_COMPLETION_BUDGET_MS: f64 = 750.0;
/// The goal the gate pipes in; it reads `proof.txt` in the goal directory.
const PROOF_GOAL: &str = "Read proof.txt and return its exact contents.";
/// Every host-contract, gateway, harness, leaf-service and cell port an
/// instance binds, before its `SUSI_PORT_OFFSET` is added.
const INSTANCE_PORTS: [u16; 17] = [
    9090, 9091, 9092, 9093, 9094, 9095, 9180, 9181, 18080, 18081, 18082, 18083, 18084, 19090,
    19091, 19092, 19093,
];
/// Offsets the gate may pick for its daemon: above the development
/// allocator's range (1000 + slot*100, at most 40900), low enough that the
/// highest instance port stays below 65536, each one checked free before use.
const OFFSET_FIRST: u16 = 41_100;
const OFFSET_STEP: u16 = 100;
const OFFSET_CANDIDATES: u16 = 50;

/// Which daemon state a case needs.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Daemon {
    Any,
    Down,
    Up,
}

#[derive(Clone, Copy, Debug)]
struct Case {
    name: &'static str,
    args: &'static [&'static str],
    daemon: Daemon,
    budget_ms: f64,
}

/// The command cases the gate judges, in the order it runs them.
const CASES: [Case; 4] = [
    Case {
        name: "version",
        args: &["--version"],
        daemon: Daemon::Any,
        budget_ms: VERSION_HELP_BUDGET_MS,
    },
    Case {
        name: "help",
        args: &["--help"],
        daemon: Daemon::Any,
        budget_ms: VERSION_HELP_BUDGET_MS,
    },
    Case {
        name: "status_daemon_down",
        args: &["status"],
        daemon: Daemon::Down,
        budget_ms: STATUS_BUDGET_MS,
    },
    Case {
        name: "status_daemon_up",
        args: &["status"],
        daemon: Daemon::Up,
        budget_ms: STATUS_BUDGET_MS,
    },
];

/// A piped first goal, timed to its first output and to completion.
#[derive(Clone, Copy, Debug)]
struct GoalCase {
    first_output: Case,
    completion: Case,
}

const fn goal_case(
    first_output: &'static str,
    completion: &'static str,
    daemon: Daemon,
) -> GoalCase {
    GoalCase {
        first_output: Case {
            name: first_output,
            args: &[],
            daemon,
            budget_ms: GOAL_FIRST_OUTPUT_BUDGET_MS,
        },
        completion: Case {
            name: completion,
            args: &[],
            daemon,
            budget_ms: GOAL_COMPLETION_BUDGET_MS,
        },
    }
}

/// The goal cases, daemon down (before `susi start`) and up.
const GOAL_CASES: [GoalCase; 2] = [
    goal_case(
        "goal_first_output_daemon_down",
        "goal_completion_daemon_down",
        Daemon::Down,
    ),
    goal_case(
        "goal_first_output_daemon_up",
        "goal_completion_daemon_up",
        Daemon::Up,
    ),
];

/// One judged case.
#[derive(Clone, Debug, PartialEq)]
struct Verdict {
    name: &'static str,
    args: &'static [&'static str],
    samples_ms: Vec<f64>,
    median_ms: f64,
    limit_ms: f64,
    pass: bool,
}

/// Median of the samples; the mean of the middle two for an even count.
fn median(samples: &[f64]) -> Option<f64> {
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    let mid = sorted.len() / 2;
    match sorted.len() {
        0 => None,
        n if n % 2 == 1 => sorted.get(mid).copied(),
        _ => Some((sorted.get(mid - 1)? + sorted.get(mid)?) / 2.0),
    }
}

/// Judge one case's samples against its budget. A median exactly at the
/// limit passes; `None` when there are no samples.
fn judge(case: &Case, samples: Vec<f64>) -> Option<Verdict> {
    let median_ms = median(&samples)?;
    Some(Verdict {
        name: case.name,
        args: case.args,
        samples_ms: samples,
        median_ms,
        limit_ms: case.budget_ms,
        pass: median_ms <= case.budget_ms,
    })
}

/// The gate passes only when every judged case passed and none failed to run.
fn overall_pass(verdicts: &[Verdict], errors: &[String]) -> bool {
    !verdicts.is_empty() && errors.is_empty() && verdicts.iter().all(|v| v.pass)
}

fn round(ms: f64) -> f64 {
    (ms * 100.0).round() / 100.0
}

struct Report<'a> {
    susi: &'a Path,
    runs: usize,
    floor_samples_ms: &'a [f64],
    floor_ms: f64,
    port_offset: Option<u16>,
    verdicts: &'a [Verdict],
    skipped: &'a [String],
    errors: &'a [String],
    daemon: serde_json::Value,
}

fn report_json(report: &Report) -> serde_json::Value {
    let cases: Vec<serde_json::Value> = report
        .verdicts
        .iter()
        .map(|v| {
            serde_json::json!({
                "name": v.name,
                "args": v.args,
                "median_ms": round(v.median_ms),
                "samples_ms": v.samples_ms.iter().copied().map(round).collect::<Vec<_>>(),
                "limit_ms": round(v.limit_ms),
                "pass": v.pass,
            })
        })
        .collect();
    serde_json::json!({
        "schema": SCHEMA,
        "susi": report.susi,
        "host": { "os": std::env::consts::OS, "arch": std::env::consts::ARCH },
        "runs": report.runs,
        "floor": {
            "probe": FLOOR_PROBE,
            "median_ms": round(report.floor_ms),
            "samples_ms": report.floor_samples_ms.iter().copied().map(round).collect::<Vec<_>>(),
        },
        "budgets": {
            "version_help_max_ms": VERSION_HELP_BUDGET_MS,
            "status_max_ms": STATUS_BUDGET_MS,
            "goal_first_output_max_ms": GOAL_FIRST_OUTPUT_BUDGET_MS,
            "goal_completion_max_ms": GOAL_COMPLETION_BUDGET_MS,
        },
        "goal": PROOF_GOAL,
        "port_offset": report.port_offset,
        "daemon": report.daemon,
        "cases": cases,
        "skipped": report.skipped,
        "errors": report.errors,
        "pass": overall_pass(report.verdicts, report.errors),
    })
}

#[derive(Debug, PartialEq)]
struct Options {
    susi: PathBuf,
    home: Option<PathBuf>,
    runs: usize,
    daemon_up: bool,
    keep_home: bool,
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

fn default_susi() -> PathBuf {
    let exe = if cfg!(windows) { "susi.exe" } else { "susi" };
    repo_root().join("target").join("release").join(exe)
}

fn parse_args(args: &[String]) -> Result<Options, String> {
    let mut options = Options {
        susi: default_susi(),
        home: None,
        runs: DEFAULT_RUNS,
        daemon_up: true,
        keep_home: false,
    };
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let mut value = |flag: &str| {
            args.next()
                .cloned()
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        match arg.as_str() {
            "--susi" => options.susi = PathBuf::from(value("--susi")?),
            "--home" => options.home = Some(PathBuf::from(value("--home")?)),
            "--runs" => {
                let runs = value("--runs")?;
                options.runs = runs
                    .parse::<usize>()
                    .ok()
                    .filter(|runs| (1..=101).contains(runs))
                    .ok_or_else(|| format!("--runs must be 1..=101, got {runs}"))?;
            }
            "--skip-daemon-up" => options.daemon_up = false,
            "--keep-home" => options.keep_home = true,
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(options)
}

fn log(message: &str) {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    eprintln!("[startup-gate {secs}] {message}");
}

/// The isolated instance every child runs in.
struct Instance {
    root: PathBuf,
    susi_home: PathBuf,
    cwd: PathBuf,
    /// The goal's workspace and its nonce, when the gate owns the home.
    goal: Option<(PathBuf, String)>,
    port_offset: u16,
}

impl Instance {
    fn command(&self, exe: &Path, args: &[&str]) -> Command {
        let mut command = Command::new(exe);
        command
            .args(args)
            .current_dir(&self.cwd)
            .env("SUSI_HOME", &self.susi_home)
            .env("SUSI_NO_REGISTRY_ENV", "1")
            .env("SUSI_PORT_OFFSET", self.port_offset.to_string())
            .env_remove("SUSI_VERBOSE")
            .env_remove("SUSI_BOOT_TRACE")
            .env_remove("RUST_LOG")
            .stdin(Stdio::null());
        command
    }

    /// `susi` with the goal on piped stdin, offline, in the goal workspace,
    /// with provider credentials and endpoints removed.
    fn goal_command(&self, exe: &Path, workspace: &Path) -> Command {
        let mut command = self.command(exe, &[]);
        for (key, _) in std::env::vars_os() {
            let name = key.to_string_lossy().to_ascii_uppercase();
            if name.ends_with("_API_KEY")
                || name.ends_with("_API_BASE")
                || name.ends_with("_BASE_URL")
            {
                command.env_remove(key);
            }
        }
        command
            .current_dir(workspace)
            .env("SUSI_OFFLINE", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        command
    }
}

/// Wall time of one piped goal to its first stdout byte and to exit, in
/// milliseconds. A run that fails or does not answer with `nonce` is an
/// error, never a sample.
fn time_goal(mut command: Command, nonce: &str) -> Result<(f64, f64), String> {
    use std::io::{Read, Write};
    let started = Instant::now();
    let mut child = command.spawn().map_err(|e| format!("spawn failed: {e}"))?;
    let mut stdout = child.stdout.take().ok_or("goal stdout was not piped")?;
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut first = None;
        let mut chunk = [0_u8; 4096];
        while let Ok(read) = stdout.read(&mut chunk) {
            if read == 0 {
                break;
            }
            first.get_or_insert_with(|| started.elapsed());
            bytes.extend_from_slice(chunk.get(..read).unwrap_or_default());
        }
        (bytes, first)
    });
    if let Some(mut stdin) = child.stdin.take() {
        writeln!(stdin, "{PROOF_GOAL}").map_err(|e| format!("write goal: {e}"))?;
    }
    let status = child.wait().map_err(|e| format!("wait failed: {e}"))?;
    let completion = started.elapsed();
    let (bytes, first) = reader.join().map_err(|_| "goal stdout reader panicked")?;
    if !status.success() {
        return Err(format!("goal exited with {status}"));
    }
    if !String::from_utf8_lossy(&bytes).contains(nonce) {
        return Err("goal answer did not contain the proof nonce".to_string());
    }
    let first = first.ok_or("goal wrote no output")?;
    Ok((
        first.as_secs_f64() * 1000.0,
        completion.as_secs_f64() * 1000.0,
    ))
}

fn ports_free(offset: u16) -> bool {
    INSTANCE_PORTS.iter().all(|base| {
        base.checked_add(offset).is_some_and(|port| {
            TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_ok()
                && UdpSocket::bind((Ipv4Addr::LOCALHOST, port)).is_ok()
        })
    })
}

fn offset_candidates(start: u16) -> impl Iterator<Item = u16> {
    (0..OFFSET_CANDIDATES).filter_map(move |i| {
        let slot = (start + i) % OFFSET_CANDIDATES;
        OFFSET_FIRST.checked_add(slot.checked_mul(OFFSET_STEP)?)
    })
}

fn pick_port_offset() -> Option<u16> {
    let start = u16::try_from(std::process::id() % u32::from(OFFSET_CANDIDATES)).unwrap_or(0);
    offset_candidates(start).find(|offset| ports_free(*offset))
}

/// Wall time of one run, in milliseconds. A child that fails is an error,
/// never a sample.
fn time_once(mut command: Command) -> Result<f64, String> {
    command.stdout(Stdio::null()).stderr(Stdio::null());
    let started = Instant::now();
    let status = command.status().map_err(|e| format!("spawn failed: {e}"))?;
    let elapsed = started.elapsed();
    if !status.success() {
        return Err(format!("exited with {status}"));
    }
    Ok(elapsed.as_secs_f64() * 1000.0)
}

/// One untimed warm-up run, then `runs` timed ones.
fn sample(runs: usize, mut make: impl FnMut() -> Command) -> Result<Vec<f64>, String> {
    time_once(make())?;
    (0..runs).map(|_| time_once(make())).collect()
}

/// `daemon.running` as `susi status --json` reports it.
fn daemon_running(instance: &Instance, susi: &Path) -> Result<bool, String> {
    let output = instance
        .command(susi, &["status", "--json"])
        .output()
        .map_err(|e| format!("status --json spawn failed: {e}"))?;
    if !output.status.success() {
        return Err(format!("status --json exited with {}", output.status));
    }
    let value: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("status --json is not JSON: {e}"))?;
    value["daemon"]["running"]
        .as_bool()
        .ok_or_else(|| "status --json has no daemon.running".to_string())
}

fn run_lifecycle(instance: &Instance, susi: &Path, verb: &str) -> serde_json::Value {
    let started = Instant::now();
    let result = instance.command(susi, &[verb]).output();
    let elapsed_ms = round(started.elapsed().as_secs_f64() * 1000.0);
    match result {
        Ok(output) => serde_json::json!({
            "ok": output.status.success(),
            "exit": output.status.code(),
            "elapsed_ms": elapsed_ms,
        }),
        Err(error) => serde_json::json!({
            "ok": false,
            "error": error.to_string(),
            "elapsed_ms": elapsed_ms,
        }),
    }
}

struct Outcome {
    verdicts: Vec<Verdict>,
    skipped: Vec<String>,
    errors: Vec<String>,
    daemon: serde_json::Value,
}

fn record(case: &Case, samples: Vec<f64>, outcome: &mut Outcome) {
    match judge(case, samples) {
        Some(verdict) => {
            log(&format!(
                "{}: median {:.1} ms, limit {:.1} ms, {}",
                verdict.name,
                verdict.median_ms,
                verdict.limit_ms,
                if verdict.pass { "pass" } else { "FAIL" }
            ));
            outcome.verdicts.push(verdict);
        }
        None => outcome
            .errors
            .push(format!("{}: no usable samples", case.name)),
    }
}

fn measure_case(case: &Case, instance: &Instance, options: &Options, outcome: &mut Outcome) {
    log(&format!("timing {} ({})", case.name, case.args.join(" ")));
    match sample(options.runs, || instance.command(&options.susi, case.args)) {
        Ok(samples) => record(case, samples, outcome),
        Err(error) => outcome.errors.push(format!("{}: {error}", case.name)),
    }
}

/// One untimed warm-up goal, then `--runs` timed ones, judged as two cases.
fn measure_goal(goal: &GoalCase, instance: &Instance, options: &Options, outcome: &mut Outcome) {
    let Some((workspace, nonce)) = &instance.goal else {
        outcome.skipped.extend([
            goal.first_output.name.to_string(),
            goal.completion.name.to_string(),
        ]);
        return;
    };
    log(&format!("timing {} (piped goal)", goal.completion.name));
    let runs: Result<Vec<(f64, f64)>, String> = (0..=options.runs)
        .map(|_| time_goal(instance.goal_command(&options.susi, workspace), nonce))
        .collect();
    match runs {
        Ok(runs) => {
            let timed = runs.get(1..).unwrap_or_default();
            record(
                &goal.first_output,
                timed.iter().map(|(first, _)| *first).collect(),
                outcome,
            );
            record(
                &goal.completion,
                timed.iter().map(|(_, done)| *done).collect(),
                outcome,
            );
        }
        Err(error) => outcome
            .errors
            .push(format!("{}: {error}", goal.completion.name)),
    }
}

fn expect_daemon(
    instance: &Instance,
    options: &Options,
    want: bool,
    outcome: &mut Outcome,
) -> bool {
    match daemon_running(instance, &options.susi) {
        Ok(running) if running == want => true,
        Ok(running) => {
            outcome.errors.push(format!(
                "expected daemon running={want} for this instance, status reports {running}"
            ));
            false
        }
        Err(error) => {
            outcome.errors.push(error);
            false
        }
    }
}

fn measure_all(instance: &Instance, options: &Options) -> Outcome {
    let mut outcome = Outcome {
        verdicts: Vec::new(),
        skipped: Vec::new(),
        errors: Vec::new(),
        daemon: serde_json::json!({ "started_by_gate": false }),
    };
    for case in CASES.iter().filter(|c| c.daemon != Daemon::Up) {
        if case.daemon == Daemon::Down && !expect_daemon(instance, options, false, &mut outcome) {
            continue;
        }
        measure_case(case, instance, options, &mut outcome);
    }
    for goal in GOAL_CASES
        .iter()
        .filter(|g| g.completion.daemon == Daemon::Down)
    {
        if expect_daemon(instance, options, false, &mut outcome) {
            measure_goal(goal, instance, options, &mut outcome);
        }
    }
    let up_cases: Vec<&Case> = CASES.iter().filter(|c| c.daemon == Daemon::Up).collect();
    let up_goals: Vec<&GoalCase> = GOAL_CASES
        .iter()
        .filter(|g| g.completion.daemon == Daemon::Up)
        .collect();
    let skip_up = |outcome: &mut Outcome| {
        outcome
            .skipped
            .extend(up_cases.iter().map(|c| c.name.to_string()));
        outcome.skipped.extend(up_goals.iter().flat_map(|g| {
            [
                g.first_output.name.to_string(),
                g.completion.name.to_string(),
            ]
        }));
    };
    if !options.daemon_up {
        skip_up(&mut outcome);
        return outcome;
    }
    // Record the state before `susi start`: only a daemon this gate's own
    // start brought up is this gate's to stop.
    if let Err(error) = daemon_up_precheck(daemon_running(instance, &options.susi)) {
        log(&format!("not starting a daemon: {error}"));
        outcome.errors.push(error);
        skip_up(&mut outcome);
        return outcome;
    }
    log("starting an isolated daemon (susi start)");
    let start = run_lifecycle(instance, &options.susi, "start");
    let started = start["ok"].as_bool().unwrap_or(false);
    if started && expect_daemon(instance, options, true, &mut outcome) {
        for case in &up_cases {
            measure_case(case, instance, options, &mut outcome);
        }
        for goal in &up_goals {
            measure_goal(goal, instance, options, &mut outcome);
        }
    } else if !started {
        outcome.errors.push(format!("susi start failed: {start}"));
    }
    // The precheck saw no daemon, so whatever runs now is this gate's own:
    // stop it, even after a failed start, since a daemon still booting holds
    // this instance's lock.
    log("stopping the isolated daemon (susi stop)");
    let stop = run_lifecycle(instance, &options.susi, "stop");
    if !stop["ok"].as_bool().unwrap_or(false) {
        outcome.errors.push(format!("susi stop failed: {stop}"));
    }
    outcome.daemon = serde_json::json!({
        "started_by_gate": started,
        "start": start,
        "stop": stop,
    });
    outcome
}

/// Whether the gate may start (and later stop) a daemon, given the state
/// `status --json` reported before `susi start`. Only a confirmed
/// "not running" qualifies: a daemon already up belongs to someone else, and
/// an unknown state could be one.
fn daemon_up_precheck(running: Result<bool, String>) -> Result<(), String> {
    match running {
        Ok(false) => Ok(()),
        Ok(true) => Err(concat!(
            "a daemon already runs for this SUSI_HOME; the gate did not start it and ",
            "will not stop it, so the daemon-up cases were skipped"
        )
        .to_string()),
        Err(error) => Err(format!(
            "daemon state unknown before start ({error}); the daemon-up cases were skipped"
        )),
    }
}

fn prepare_instance(options: &Options) -> Result<Instance, String> {
    let root = options.home.clone().unwrap_or_else(|| {
        repo_root()
            .join("target")
            .join("live-home")
            .join(format!("startup-gate-{}", std::process::id()))
    });
    let susi_home = root.join("susi-home");
    let cwd = root.join("cwd");
    for dir in [&susi_home, &cwd] {
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    }
    let empty = std::fs::read_dir(&cwd)
        .map_err(|e| format!("read {}: {e}", cwd.display()))?
        .next()
        .is_none();
    if !empty {
        return Err(format!("{} must be empty", cwd.display()));
    }
    let port_offset = pick_port_offset()
        .ok_or("no free SUSI_PORT_OFFSET for an isolated instance".to_string())?;
    // Only the gate's own home may receive the goal's privacy setting.
    let goal = if options.home.is_none() {
        let workspace = root.join("goal");
        let nonce = format!(
            "startup-gate-proof-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        );
        std::fs::create_dir_all(&workspace)
            .and_then(|()| std::fs::write(workspace.join("proof.txt"), &nonce))
            .and_then(|()| std::fs::write(susi_home.join("privacy_mode"), "local_only"))
            .map_err(|e| format!("prepare goal workspace {}: {e}", workspace.display()))?;
        Some((workspace, nonce))
    } else {
        None
    };
    Ok(Instance {
        root,
        susi_home,
        cwd,
        goal,
        port_offset,
    })
}

fn run(options: &Options) -> Result<(serde_json::Value, bool), String> {
    if !options.susi.is_file() {
        return Err(format!(
            "susi binary not found at {} (pass --susi)",
            options.susi.display()
        ));
    }
    let instance = prepare_instance(options)?;
    log(&format!(
        "susi {} | SUSI_HOME {} | SUSI_PORT_OFFSET {}",
        options.susi.display(),
        instance.susi_home.display(),
        instance.port_offset
    ));
    let probe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    log("timing the process-spawn floor");
    let floor_samples = sample(options.runs, || instance.command(&probe, &[FLOOR_PROBE]))
        .map_err(|e| format!("floor probe: {e}"))?;
    let floor_ms = median(&floor_samples).ok_or("floor probe produced no samples")?;
    log(&format!("floor: median {floor_ms:.1} ms"));
    let outcome = measure_all(&instance, options);
    let report = Report {
        susi: &options.susi,
        runs: options.runs,
        floor_samples_ms: &floor_samples,
        floor_ms,
        port_offset: Some(instance.port_offset),
        verdicts: &outcome.verdicts,
        skipped: &outcome.skipped,
        errors: &outcome.errors,
        daemon: outcome.daemon.clone(),
    };
    let pass = overall_pass(&outcome.verdicts, &outcome.errors);
    let stopped_cleanly = outcome.errors.iter().all(|e| !e.starts_with("susi stop"));
    if !options.keep_home && options.home.is_none() && stopped_cleanly {
        // Best effort: the instance lives under target/ and is disposable.
        let _ = std::fs::remove_dir_all(&instance.root);
    }
    Ok((report_json(&report), pass))
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some(FLOOR_PROBE) {
        return ExitCode::SUCCESS;
    }
    let options = match parse_args(&args) {
        Ok(options) => options,
        Err(error) => {
            log(&format!("usage error: {error}"));
            return ExitCode::from(2);
        }
    };
    match run(&options) {
        Ok((report, pass)) => {
            println!(
                "{}",
                serde_json::to_string_pretty(&report).unwrap_or_else(|_| report.to_string())
            );
            if pass {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(error) => {
            log(&format!("gate could not run: {error}"));
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isolated_children_disable_registry_environment() {
        let instance = Instance {
            root: PathBuf::from("private"),
            susi_home: PathBuf::from("private/susi"),
            cwd: PathBuf::from("private/workspace"),
            goal: None,
            port_offset: 400,
        };
        for command in [
            instance.command(Path::new("unused"), &["--version"]),
            instance.goal_command(Path::new("unused"), Path::new("private/workspace")),
        ] {
            assert!(command
                .get_envs()
                .any(|(key, value)| key == "SUSI_NO_REGISTRY_ENV"
                    && value == Some(std::ffi::OsStr::new("1"))));
        }
    }

    fn case(name: &str) -> Case {
        *CASES.iter().find(|c| c.name == name).unwrap()
    }

    #[test]
    fn median_of_odd_even_and_empty_samples() {
        assert_eq!(median(&[30.0, 10.0, 20.0]), Some(20.0));
        assert_eq!(median(&[4.0, 1.0, 3.0, 2.0]), Some(2.5));
        assert_eq!(median(&[7.0]), Some(7.0));
        assert_eq!(median(&[]), None);
        // One slow outlier does not move the median.
        assert_eq!(median(&[10.0, 11.0, 12.0, 13.0, 900.0]), Some(12.0));
    }

    #[test]
    fn version_and_help_are_held_to_thirty_ms() {
        for name in ["version", "help"] {
            let c = case(name);
            assert_eq!(c.budget_ms, 30.0);
            assert!(judge(&c, vec![29.0, 30.0, 31.0]).unwrap().pass);
            assert!(!judge(&c, vec![30.1, 30.2, 35.0]).unwrap().pass);
        }
    }

    #[test]
    fn status_is_held_to_fifty_ms_daemon_up_and_down() {
        let down = case("status_daemon_down");
        let up = case("status_daemon_up");
        assert_eq!((down.daemon, up.daemon), (Daemon::Down, Daemon::Up));
        for c in [down, up] {
            assert_eq!(c.args, ["status"]);
            assert_eq!(c.budget_ms, 50.0);
            assert!(judge(&c, vec![49.0, 50.0, 51.0]).unwrap().pass);
            assert!(!judge(&c, vec![50.1, 50.2, 60.0]).unwrap().pass);
        }
    }

    #[test]
    fn the_goal_is_judged_to_first_output_and_completion_daemon_down_and_up() {
        let [down, up] = GOAL_CASES;
        assert_eq!(
            (down.first_output.daemon, down.completion.daemon),
            (Daemon::Down, Daemon::Down)
        );
        assert_eq!(
            (up.first_output.daemon, up.completion.daemon),
            (Daemon::Up, Daemon::Up)
        );
        for goal in GOAL_CASES {
            assert_eq!(goal.first_output.budget_ms, GOAL_FIRST_OUTPUT_BUDGET_MS);
            assert_eq!(goal.completion.budget_ms, GOAL_COMPLETION_BUDGET_MS);
            assert!(goal.first_output.budget_ms <= goal.completion.budget_ms);
        }
    }

    #[test]
    fn every_required_surface_is_a_case() {
        let names: Vec<&str> = CASES.iter().map(|c| c.name).collect();
        assert_eq!(
            names,
            ["version", "help", "status_daemon_down", "status_daemon_up"]
        );
        let goals: Vec<&str> = GOAL_CASES
            .iter()
            .flat_map(|g| [g.first_output.name, g.completion.name])
            .collect();
        assert_eq!(
            goals,
            [
                "goal_first_output_daemon_down",
                "goal_completion_daemon_down",
                "goal_first_output_daemon_up",
                "goal_completion_daemon_up"
            ]
        );
    }

    #[test]
    fn no_samples_is_not_a_verdict() {
        assert!(judge(&case("version"), Vec::new()).is_none());
    }

    #[test]
    fn the_gate_passes_only_when_every_case_passes_and_nothing_failed_to_run() {
        let pass = judge(&case("version"), vec![10.0]).unwrap();
        let fail = judge(&case("status_daemon_down"), vec![80.0]).unwrap();
        assert!(overall_pass(std::slice::from_ref(&pass), &[]));
        assert!(!overall_pass(&[pass.clone(), fail], &[]));
        assert!(!overall_pass(
            std::slice::from_ref(&pass),
            &["susi stop failed".to_string()]
        ));
        assert!(!overall_pass(&[], &[]), "measuring nothing is not a pass");
    }

    #[test]
    fn report_is_one_json_object_with_the_budgets_and_verdicts() {
        let verdicts = vec![
            judge(&case("version"), vec![12.345, 12.0, 13.0]).unwrap(),
            judge(&case("status_daemon_down"), vec![70.0]).unwrap(),
        ];
        let errors = Vec::new();
        let skipped = vec!["status_daemon_up".to_string()];
        let value = report_json(&Report {
            susi: Path::new("target/release/susi"),
            runs: 3,
            floor_samples_ms: &[8.0, 7.5, 8.5],
            floor_ms: 8.0,
            port_offset: Some(41_100),
            verdicts: &verdicts,
            skipped: &skipped,
            errors: &errors,
            daemon: serde_json::json!({ "started_by_gate": false }),
        });
        assert_eq!(value["schema"], SCHEMA);
        assert_eq!(value["floor"]["median_ms"], 8.0);
        assert_eq!(value["budgets"]["status_max_ms"], 50.0);
        assert_eq!(value["budgets"]["version_help_max_ms"], 30.0);
        assert_eq!(
            value["budgets"]["goal_first_output_max_ms"],
            GOAL_FIRST_OUTPUT_BUDGET_MS
        );
        assert_eq!(
            value["budgets"]["goal_completion_max_ms"],
            GOAL_COMPLETION_BUDGET_MS
        );
        assert_eq!(value["goal"], PROOF_GOAL);
        assert_eq!(value["cases"][0]["name"], "version");
        assert_eq!(value["cases"][0]["limit_ms"], 30.0);
        assert_eq!(value["cases"][0]["median_ms"], 12.35);
        assert_eq!(value["cases"][0]["pass"], true);
        assert_eq!(value["cases"][1]["pass"], false);
        assert_eq!(value["skipped"][0], "status_daemon_up");
        assert_eq!(value["pass"], false);
        let text = serde_json::to_string(&value).unwrap();
        assert!(serde_json::from_str::<serde_json::Value>(&text).is_ok());
    }

    #[test]
    fn arguments_parse_and_reject_unknowns() {
        let args = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let options = parse_args(&args(&[
            "--susi",
            "x/susi",
            "--runs",
            "7",
            "--skip-daemon-up",
            "--home",
            "h",
        ]))
        .unwrap();
        assert_eq!(options.susi, PathBuf::from("x/susi"));
        assert_eq!(options.runs, 7);
        assert!(!options.daemon_up);
        assert_eq!(options.home, Some(PathBuf::from("h")));
        assert_eq!(parse_args(&[]).unwrap().runs, DEFAULT_RUNS);
        assert!(parse_args(&[]).unwrap().daemon_up);
        assert!(parse_args(&args(&["--runs", "0"])).is_err());
        assert!(parse_args(&args(&["--runs"])).is_err());
        assert!(parse_args(&args(&["--bogus"])).is_err());
    }

    #[test]
    fn a_daemon_the_gate_did_not_start_is_never_started_over_or_stopped() {
        assert_eq!(daemon_up_precheck(Ok(false)), Ok(()));
        let preexisting = daemon_up_precheck(Ok(true)).unwrap_err();
        assert!(preexisting.contains("already runs"), "{preexisting}");
        assert!(preexisting.contains("will not stop"), "{preexisting}");
        let unknown = daemon_up_precheck(Err("status --json exited with 1".into())).unwrap_err();
        assert!(unknown.contains("unknown"), "{unknown}");
        // Either refusal is an error, so the gate cannot pass on it.
        let pass = judge(&case("version"), vec![10.0]).unwrap();
        assert!(!overall_pass(std::slice::from_ref(&pass), &[preexisting]));
    }

    #[test]
    fn every_candidate_offset_keeps_all_instance_ports_in_range() {
        assert_eq!(INSTANCE_PORTS.len(), 17);
        assert!(INSTANCE_PORTS.contains(&9180));
        assert!(INSTANCE_PORTS.contains(&9181));
        let offsets: Vec<u16> = offset_candidates(17).collect();
        assert_eq!(offsets.len(), usize::from(OFFSET_CANDIDATES));
        let mut unique = offsets.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), offsets.len());
        for offset in offsets {
            assert!(offset >= OFFSET_FIRST);
            for base in INSTANCE_PORTS {
                assert!(base.checked_add(offset).is_some(), "{base}+{offset}");
            }
        }
    }
}
