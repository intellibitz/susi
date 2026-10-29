//! A graded user-intent ladder, run at start-up within a budget
//! (VC-201-001 / T-DEEPSEEK-200).
//!
//! The ladder is a versioned artifact — `.agents/intent-ladder.json`,
//! schema `susi.intent_ladder/v1` — of real user intents from trivial to
//! extreme. Each intent carries an *independently checkable* outcome
//! (a file the run must leave, or text it must print) and its own cost
//! and wall-clock budget, so a pass is evidence the harness can verify
//! rather than the runner's own say-so.
//!
//! A run is always hermetic: every intent executes in a throwaway
//! workspace with a scratch `SUSI_HOME`, an environment scrubbed of
//! provider credentials, and `SUSI_OFFLINE` set — an automatic run of
//! extreme intents that touches the user's environment is a foot-gun,
//! not a self-test.
//!
//! Two policies share [`run_slice`]: the **start-up slice** (a few
//! low-grade intents under tight totals — `after susi starts`) and the
//! **full ladder** (every intent, on demand via `susi admin
//! intent-ladder --run` and on the daily schedule in `Maintenance::
//! tick`). Both write the same durable scorecard:
//! `<home>/intent-ladder-scorecard.json` records, per intent, pass /
//! fail / partial / skipped, what it cost, which worker ran it, and
//! where it stopped — reproducible per ladder revision.

use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use susi_error::{EaiError, EaiResult};

/// Ladder schema this binary understands.
pub const LADDER_SCHEMA: &str = "susi.intent_ladder/v1";
/// Scorecard schema — versioned independently of the ladder.
pub const SCORECARD_SCHEMA: &str = "susi.intent_scorecard/v1";
/// The ladder's canonical path inside the repository.
pub const LADDER_PATH: &str = ".agents/intent-ladder.json";
/// The durable scorecard, under the susi home the run reports into.
pub const SCORECARD_FILE: &str = "intent-ladder-scorecard.json";

/// The bundled ladder — the revision every installed binary carries;
/// a repo working copy with a newer revision wins via [`load_ladder`].
const BUNDLED_LADDER: &str = include_str!("../../../.agents/intent-ladder.json");

/// The start-up slice: a few low-grade intents under tight totals.
pub const SLICE_MAX_INTENTS: usize = 3;
pub const SLICE_MAX_SECONDS: u64 = 600;
/// The scheduled full-ladder cadence — daily, env-overridable.
pub const SCHEDULE_DEFAULT_SECS: u64 = 86_400;
/// A full run's wall-clock ceiling regardless of per-intent budgets.
pub const FULL_MAX_SECONDS: u64 = 7_200;

/// One rung on the ladder: how much of susi the intent exercises.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Grade {
    /// A single local action — no reasoning needed.
    Trivial,
    /// One file or one fact; a warmed substrate should manage it.
    Easy,
    /// Multi-step local work — read, transform, write, verify.
    Moderate,
    /// Repair or synthesis a model must reason through.
    Hard,
    /// Orchestrated multi-stage work — the ladder's ceiling.
    Extreme,
}

impl Grade {
    pub fn name(self) -> &'static str {
        match self {
            Grade::Trivial => "trivial",
            Grade::Easy => "easy",
            Grade::Moderate => "moderate",
            Grade::Hard => "hard",
            Grade::Extreme => "extreme",
        }
    }
}

/// How an outcome is checked *independently* — the harness inspects
/// the throwaway workspace and captured output, never the runner's
/// own report of success.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Check {
    /// The run must leave `<workspace>/<path>` containing `needle`.
    /// `path` must be relative and inside the workspace — hermeticity
    /// is enforced at parse, not trusted later.
    FileContains { path: String, needle: String },
    /// The run's captured output must contain `needle`.
    StdoutMarker { needle: String },
}

/// One intent's own budget — the ceiling it runs under regardless of
/// the slice's totals. `micros == 0` means "not priced" (the run still
/// records what it spent).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Budget {
    pub seconds: u64,
    #[serde(default)]
    pub micros: u64,
}

/// A ladder intent: the request text, its grade, its independent check
/// and its budget.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Intent {
    pub id: String,
    pub grade: Grade,
    /// The user-facing request exactly as a user would write it.
    pub intent: String,
    pub check: Check,
    pub budget: Budget,
}

/// The versioned ladder document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IntentLadder {
    pub schema: String,
    pub revision: u64,
    pub authored_by: String,
    pub rationale: String,
    pub intents: Vec<Intent>,
}

fn check_path_safe(path: &str) -> bool {
    let p = Path::new(path);
    !path.is_empty()
        && p.is_relative()
        && p.components().all(|c| {
            matches!(c, std::path::Component::Normal(_))
                && !c.as_os_str().to_string_lossy().starts_with('.')
        })
}

impl IntentLadder {
    /// Parse *and* verify — schema, revision, unique ids, non-empty
    /// fields, safe check paths, positive budgets. A malformed rung is
    /// an error at load, never a surprise mid-run.
    pub fn from_json(text: &str) -> EaiResult<Self> {
        let ladder: IntentLadder = serde_json::from_str(text)?;
        if ladder.schema != LADDER_SCHEMA {
            return Err(EaiError::config(format!(
                "intent ladder schema `{}` — this binary understands {LADDER_SCHEMA}",
                ladder.schema
            )));
        }
        if ladder.revision == 0 {
            return Err(EaiError::config("intent ladder revision must be >= 1"));
        }
        if ladder.rationale.trim().is_empty() {
            return Err(EaiError::config(
                "an authored ladder names the analysis that produced it — the rationale is required",
            ));
        }
        let mut ids = std::collections::BTreeSet::new();
        for i in &ladder.intents {
            if !ids.insert(i.id.as_str()) {
                return Err(EaiError::config(format!("duplicate intent id `{}`", i.id)));
            }
            if i.id.trim().is_empty() || i.intent.trim().is_empty() {
                return Err(EaiError::config("every intent needs an id and a request"));
            }
            if i.budget.seconds == 0 {
                return Err(EaiError::config(format!(
                    "intent {} needs a positive wall-clock budget",
                    i.id
                )));
            }
            match &i.check {
                Check::FileContains { path, needle } => {
                    if !check_path_safe(path) {
                        return Err(EaiError::config(format!(
                            "intent {} check path `{path}` must stay inside the throwaway \
                             workspace (relative, no `..`, no hidden segments)",
                            i.id
                        )));
                    }
                    if needle.is_empty() {
                        return Err(EaiError::config(format!(
                            "intent {} check needs a non-empty needle",
                            i.id
                        )));
                    }
                }
                Check::StdoutMarker { needle } => {
                    if needle.is_empty() {
                        return Err(EaiError::config(format!(
                            "intent {} check needs a non-empty needle",
                            i.id
                        )));
                    }
                }
            }
        }
        Ok(ladder)
    }

    /// The bundled ladder — always loadable or the build is broken.
    #[allow(clippy::expect_used)] // compiled-in asset verified by the test
                                  // suite — a parse failure is a build bug,
                                  // never a runtime condition
    pub fn bundled() -> Self {
        Self::from_json(BUNDLED_LADDER).expect("bundled intent ladder failed verification")
    }
}

/// Resolve the ladder: a repo working copy whose revision is at least
/// the bundled one wins; otherwise the compiled-in ladder. Returns the
/// ladder plus where it came from (for the scorecard's provenance).
pub fn load_ladder(repo_root: &Path) -> EaiResult<(IntentLadder, String)> {
    let file = repo_root.join(LADDER_PATH);
    let bundled = IntentLadder::bundled();
    match std::fs::read_to_string(&file) {
        Ok(text) => match IntentLadder::from_json(&text) {
            Ok(l) if l.revision >= bundled.revision => Ok((l, file.display().to_string())),
            Ok(l) => Ok((
                bundled,
                format!("bundled (file revision {} is older)", l.revision),
            )),
            Err(e) => Err(EaiError::config(format!(
                "{}: {e} — a broken ladder file is not silently bundled over",
                file.display()
            ))),
        },
        Err(_) => Ok((bundled, "bundled".into())),
    }
}

/// Which rungs a run covers and the totals it may spend.
#[derive(Debug, Clone, Copy)]
pub struct SlicePolicy {
    /// Rungs attempted, in ladder order.
    pub max_intents: usize,
    /// Highest grade the slice reaches.
    pub max_grade: Grade,
    /// Wall-clock ceiling across the whole slice.
    pub total_seconds: u64,
    /// Spend ceiling across the whole slice; 0 = uncapped.
    pub total_micros: u64,
    /// True for the full ladder: every intent regardless of grade.
    pub full_ladder: bool,
}

impl SlicePolicy {
    /// The after-startup slice — small, low-grade, cheap.
    pub fn startup() -> Self {
        Self {
            max_intents: SLICE_MAX_INTENTS,
            max_grade: Grade::Easy,
            total_seconds: SLICE_MAX_SECONDS,
            total_micros: 0,
            full_ladder: false,
        }
    }
    /// The on-demand / scheduled run — every rung, wall-clock bounded.
    pub fn full() -> Self {
        Self {
            max_intents: usize::MAX,
            max_grade: Grade::Extreme,
            total_seconds: FULL_MAX_SECONDS,
            total_micros: 0,
            full_ladder: true,
        }
    }

    /// The rungs this policy selects, in ladder order.
    pub fn plan<'a>(&self, ladder: &'a IntentLadder) -> Vec<&'a Intent> {
        ladder
            .intents
            .iter()
            .filter(|i| self.full_ladder || i.grade <= self.max_grade)
            .take(self.max_intents)
            .collect()
    }
}

/// Where a rung's check looks for its outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// The run finished and the check was evaluated.
    Completed,
    /// The rung hit its own wall-clock budget — partial, not pass/fail.
    Timeout,
    /// The slice ceiling stopped the rung before it started.
    SliceBudget,
}

/// What the runner produced — exit state, bounded output tail,
/// measured cost and the worker identity that served it.
#[derive(Debug, Clone)]
pub struct RunOutcome {
    pub exit_ok: bool,
    /// Captured output tail (bounded), for `stdout_marker` checks.
    pub output: String,
    pub seconds: u64,
    pub micros: u64,
    /// Which worker ran it — the binary@pid a process runner reports,
    /// or the seat a swarm runner would record.
    pub worker: String,
    pub timed_out: bool,
}

/// The execution seam: production spawns the susi binary hermetically;
/// tests inject outcomes. One seam keeps the scorecard honest — every
/// kind of run flows through the same check + accounting.
pub trait Runner {
    /// Run one intent under `env` within `budget`. The runner MUST NOT
    /// block past `budget.seconds` by a wide margin — `run_slice`
    /// records `timed_out` outcomes as partial.
    fn run(&self, intent: &Intent, env: &RunEnv, budget: Budget) -> RunOutcome;
}

/// The throwaway an intent runs in.
#[derive(Debug, Clone)]
pub struct RunEnv {
    /// Scratch `SUSI_HOME` — created fresh per intent, deleted after.
    pub home: PathBuf,
    /// Throwaway workspace the intent works in.
    pub workspace: PathBuf,
}

/// Per-intent scorecard row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScorecardEntry {
    pub id: String,
    pub grade: String,
    /// pass | fail | partial | skipped
    pub verdict: String,
    pub seconds: u64,
    pub micros: u64,
    pub worker: String,
    /// Where it stopped: completed | timeout | slice-budget.
    pub stopped: String,
    /// What the independent check saw (empty for skipped rungs).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
}

/// The durable, reproducible scorecard.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Scorecard {
    pub schema: String,
    pub ladder_revision: u64,
    pub ladder_source: String,
    pub full_ladder: bool,
    pub started_unix: u64,
    pub entries: Vec<ScorecardEntry>,
}

pub fn scorecard_path(home: &Path) -> PathBuf {
    home.join(SCORECARD_FILE)
}

/// Read the last scorecard a run left in `home`, if any.
pub fn read_scorecard(home: &Path) -> Option<Scorecard> {
    serde_json::from_str(&std::fs::read_to_string(scorecard_path(home)).ok()?).ok()
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Evaluate a rung's independent check against the throwaway the run
/// left behind — never against the runner's own claim.
fn evaluate(check: &Check, env: &RunEnv, outcome: &RunOutcome) -> (bool, String) {
    match check {
        Check::FileContains { path, needle } => {
            let file = env.workspace.join(path);
            match std::fs::read_to_string(&file) {
                Ok(text) if text.contains(needle.as_str()) => {
                    (true, format!("{} contains the marker", path))
                }
                Ok(_) => (false, format!("{path} exists but lacks the marker")),
                Err(_) => (false, format!("{path} was never produced")),
            }
        }
        Check::StdoutMarker { needle } => {
            if outcome.output.contains(needle.as_str()) {
                (true, "the marker is in the output".into())
            } else {
                (false, "the marker is not in the output".into())
            }
        }
    }
}

/// Where a run's throwaways live and where its scorecard lands.
///
/// Hermeticity is the caller's contract: `runs_root` must be throwaway
/// (`std::env::temp_dir()` descendants), `state_dir` the susi home the
/// run reports into.
#[derive(Debug, Clone, Copy)]
pub struct RunDirs<'a> {
    pub state_dir: &'a Path,
    pub runs_root: &'a Path,
}

/// Run `policy` over `ladder`: each planned intent gets a scratch
/// `SUSI_HOME` and a throwaway workspace under `runs_root`, executes
/// through `runner`, and is checked independently. The durable
/// scorecard lands in `state_dir`.
pub fn run_slice<R: Runner>(
    dirs: &RunDirs<'_>,
    ladder: &IntentLadder,
    ladder_source: &str,
    policy: &SlicePolicy,
    runner: &R,
) -> Scorecard {
    let RunDirs {
        state_dir,
        runs_root,
    } = *dirs;
    let plan = policy.plan(ladder);
    let started = Instant::now();
    let mut spent_micros: u64 = 0;
    let mut entries = Vec::new();

    for intent in plan {
        // Slice ceilings stop a rung before it starts — recorded, not silent.
        if started.elapsed().as_secs() >= policy.total_seconds
            || (policy.total_micros > 0 && spent_micros >= policy.total_micros)
        {
            entries.push(ScorecardEntry {
                id: intent.id.clone(),
                grade: intent.grade.name().into(),
                verdict: "skipped".into(),
                seconds: 0,
                micros: 0,
                worker: String::new(),
                stopped: "slice-budget".into(),
                detail: "the slice ceiling was already spent".into(),
            });
            continue;
        }

        let env = RunEnv {
            home: runs_root.join(format!("{}-home", intent.id)),
            workspace: runs_root.join(format!("{}-ws", intent.id)),
        };
        let _ = std::fs::remove_dir_all(&env.home);
        let _ = std::fs::remove_dir_all(&env.workspace);
        let _ = std::fs::create_dir_all(&env.home);
        let _ = std::fs::create_dir_all(&env.workspace);

        // Per-intent budget is the smaller of its own and the slice's
        // remaining wall-clock — the slice ceiling is always enforced.
        let budget = Budget {
            seconds: intent
                .budget
                .seconds
                .min(
                    policy
                        .total_seconds
                        .saturating_sub(started.elapsed().as_secs()),
                )
                .max(1),
            micros: intent.budget.micros,
        };
        let outcome = runner.run(intent, &env, budget);
        spent_micros = spent_micros.saturating_add(outcome.micros);

        let (verdict, stopped, detail) = if outcome.timed_out {
            (
                "partial",
                "timeout",
                format!("hit its {}s budget without finishing", budget.seconds),
            )
        } else {
            let (ok, detail) = evaluate(&intent.check, &env, &outcome);
            (
                if ok && outcome.exit_ok {
                    "pass"
                } else {
                    "fail"
                },
                "completed",
                detail,
            )
        };

        entries.push(ScorecardEntry {
            id: intent.id.clone(),
            grade: intent.grade.name().into(),
            verdict: verdict.into(),
            seconds: outcome.seconds,
            micros: outcome.micros,
            worker: outcome.worker.clone(),
            stopped: stopped.into(),
            detail,
        });

        // The throwaway is thrown away — nothing an intent did can outlive its rung.
        let _ = std::fs::remove_dir_all(&env.home);
        let _ = std::fs::remove_dir_all(&env.workspace);
    }
    let _ = std::fs::remove_dir(runs_root);

    let card = Scorecard {
        schema: SCORECARD_SCHEMA.into(),
        ladder_revision: ladder.revision,
        ladder_source: ladder_source.into(),
        full_ladder: policy.full_ladder,
        started_unix: now_unix(),
        entries,
    };
    if let Ok(body) = serde_json::to_vec_pretty(&card) {
        let _ = std::fs::create_dir_all(state_dir);
        let _ = susi_config::atomic_write_bytes(&scorecard_path(state_dir), &body);
    }
    card
}

/// Where a repo working copy would hold the ladder — the process cwd, for a
/// CLI caller working in a checkout (a cwd that is not a repo falls through
/// to the bundled revision). The daemon never uses it: it has no repository
/// and resolves the ladder from the substrate home it owns.
pub fn repo_root() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// The scheduled full-ladder cadence — daily, `SUSI_INTENT_LADDER_
/// INTERVAL_SECS` overrides.
pub fn schedule_secs() -> u64 {
    std::env::var("SUSI_INTENT_LADDER_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|s| *s > 0)
        .unwrap_or(SCHEDULE_DEFAULT_SECS)
}

/// The production runner: spawn the susi binary itself with the intent
/// as the request — the same `susi <intent>` path a user hits — inside
/// a scrubbed environment. `env_clear` + an explicit minimal env means
/// no provider credential survives into the run; `SUSI_OFFLINE` keeps
/// even a configured-path fetch off; the scratch `SUSI_HOME` has no
/// consent grants, so egress is triple-gated.
pub struct ProcessRunner {
    pub susi_bin: PathBuf,
}

impl ProcessRunner {
    /// The runner for the binary that is running — the daemon and the
    /// CLI both spawn their own `susi`.
    pub fn this_binary() -> EaiResult<Self> {
        Ok(Self {
            susi_bin: std::env::current_exe()
                .map_err(|e| EaiError::io(format!("current_exe: {e}")))?,
        })
    }
}

/// Output captured per run is bounded — a pathological run can't grow
/// the scorecard.
const OUTPUT_CAP: usize = 8 * 1024;

impl Runner for ProcessRunner {
    fn run(&self, intent: &Intent, env: &RunEnv, budget: Budget) -> RunOutcome {
        let worker = format!("{}@spawn", self.susi_bin.display());
        let started = Instant::now();
        let out_file = env.workspace.join(".intent-output.txt");
        #[cfg(windows)]
        let mut cmd = if self
            .susi_bin
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("sh"))
        {
            let mut command = std::process::Command::new("sh");
            command.arg(&self.susi_bin);
            command
        } else {
            std::process::Command::new(&self.susi_bin)
        };
        #[cfg(not(windows))]
        let mut cmd = std::process::Command::new(&self.susi_bin);
        cmd.arg(&intent.intent)
            .current_dir(&env.workspace)
            .env_clear()
            // The minimum a child needs: PATH to find helpers, HOME +
            // SUSI_HOME pointed at the scratch, OFFLINE as the third
            // egress gate.
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &env.home)
            .env("SUSI_HOME", &env.home)
            .env("SUSI_OFFLINE", "1")
            .env("SUSI_HERMETIC_FORBIDDEN", &env.home)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        if let Ok(f) = std::fs::File::create(&out_file) {
            cmd.stdout(std::process::Stdio::from(f));
        } else {
            cmd.stdout(std::process::Stdio::null());
        }

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                return RunOutcome {
                    exit_ok: false,
                    output: format!("spawn failed: {e}"),
                    seconds: 0,
                    micros: 0,
                    worker,
                    timed_out: false,
                };
            }
        };
        let deadline = Duration::from_secs(budget.seconds);
        let (mut timed_out, mut exit_ok) = (false, false);
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    exit_ok = status.success();
                    break;
                }
                Ok(None) => {
                    if started.elapsed() > deadline {
                        timed_out = true;
                        let _ = child.kill();
                        let _ = child.wait();
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(_) => break,
            }
        }
        let output = std::fs::File::open(&out_file)
            .ok()
            .map(|mut f| {
                let mut buf = Vec::new();
                let _ = f.by_ref().take(OUTPUT_CAP as u64).read_to_end(&mut buf);
                String::from_utf8_lossy(&buf).into_owned()
            })
            .unwrap_or_default();
        RunOutcome {
            exit_ok,
            output,
            seconds: started.elapsed().as_secs(),
            micros: 0,
            worker: format!("{}@{}", self.susi_bin.display(), child.id()),
            timed_out,
        }
    }
}

/// After-startup slice: the bounded low-grade run, scorecard into
/// `home`. Called once at boot from the runtime-admin loop.
pub fn startup_slice(home: &Path, repo_root: &Path) -> EaiResult<Scorecard> {
    let (ladder, source) = load_ladder(repo_root)?;
    let runner = ProcessRunner::this_binary()?;
    let runs_root = std::env::temp_dir().join(format!("susi-intent-slice-{}", std::process::id()));
    Ok(run_slice(
        &RunDirs {
            state_dir: home,
            runs_root: &runs_root,
        },
        &ladder,
        &source,
        &SlicePolicy::startup(),
        &runner,
    ))
}

/// The full ladder on demand or on the schedule — same hermeticity,
/// every rung.
pub fn full_run(home: &Path, repo_root: &Path) -> EaiResult<Scorecard> {
    let (ladder, source) = load_ladder(repo_root)?;
    let runner = ProcessRunner::this_binary()?;
    let runs_root = std::env::temp_dir().join(format!("susi-intent-full-{}", std::process::id()));
    Ok(run_slice(
        &RunDirs {
            state_dir: home,
            runs_root: &runs_root,
        },
        &ladder,
        &source,
        &SlicePolicy::full(),
        &runner,
    ))
}

/// A check as evidence prose — what was expected, for the task record.
pub fn describe_check(check: &Check) -> String {
    match check {
        Check::FileContains { path, needle } => {
            format!("a file `{path}` containing `{needle}`")
        }
        Check::StdoutMarker { needle } => format!("output containing `{needle}`"),
    }
}

/// Re-run one named intent hermetically and return its scorecard entry —
/// the acceptance a repair task's close runs: the task is done when the
/// intent itself passes, not when somebody says so.
///
/// # Errors
/// [`EaiError::config`] when the ladder holds no such intent.
pub fn verify_intent<R: Runner>(
    repo_root: &Path,
    id: &str,
    runner: &R,
) -> EaiResult<ScorecardEntry> {
    let (ladder, source) = load_ladder(repo_root)?;
    let Some(intent) = ladder.intents.iter().find(|i| i.id == id) else {
        return Err(EaiError::config(format!(
            "no intent `{id}` on the ladder ({source})"
        )));
    };
    let runs_root =
        std::env::temp_dir().join(format!("susi-intent-verify-{id}-{}", std::process::id()));
    let env = RunEnv {
        home: runs_root.join("home"),
        workspace: runs_root.join("ws"),
    };
    let _ = std::fs::remove_dir_all(&env.home);
    let _ = std::fs::remove_dir_all(&env.workspace);
    let _ = std::fs::create_dir_all(&env.home);
    let _ = std::fs::create_dir_all(&env.workspace);
    let outcome = runner.run(intent, &env, intent.budget);
    let entry = if outcome.timed_out {
        ScorecardEntry {
            id: id.into(),
            grade: intent.grade.name().into(),
            verdict: "partial".into(),
            seconds: outcome.seconds,
            micros: outcome.micros,
            worker: outcome.worker.clone(),
            stopped: "timeout".into(),
            detail: format!(
                "hit its {}s budget without finishing",
                intent.budget.seconds
            ),
        }
    } else {
        let (ok, detail) = evaluate(&intent.check, &env, &outcome);
        ScorecardEntry {
            id: id.into(),
            grade: intent.grade.name().into(),
            verdict: if ok && outcome.exit_ok {
                "pass"
            } else {
                "fail"
            }
            .into(),
            seconds: outcome.seconds,
            micros: outcome.micros,
            worker: outcome.worker.clone(),
            stopped: "completed".into(),
            detail,
        }
    };
    let _ = std::fs::remove_dir_all(&env.home);
    let _ = std::fs::remove_dir_all(&env.workspace);
    let _ = std::fs::remove_dir(&runs_root);
    Ok(entry)
}

/// Repair tasks authored per scorecard pass — the ladder cannot flood
/// the queue faster than agents drain it, on top of the per-author bound
/// `tasks::author` already enforces.
pub const GAP_TASKS_PER_RUN: usize = 2;

/// Turn each ladder failure into a specific, evidence-backed task
/// (T-DEEPSEEK-201): the task names the intent, what was expected
/// against what happened, and its acceptance is the failing intent
/// itself — closing it requires `--verify` to pass, which makes the
/// corpus a growing regression suite.
///
/// Deduplication: an open task or roadmap vector already naming the
/// intent suppresses a duplicate; a closed one does not — an intent that
/// regressed after repair earns a fresh task. Failures are triaged by
/// grade (an extreme failure outranks a trivial one) and capped at
/// [`GAP_TASKS_PER_RUN`] per scorecard.
///
/// Outside a repository — a daemon cwd that is not a checkout — this is
/// a deliberate no-op: it must not invent `.agents/` state in an
/// arbitrary directory. The daemon never calls it: it records failures in
/// the host outbox ([`record_gaps`]), and an agent turns one into a task in
/// its own checkout ([`adopt_gap`]). The substrate home is refused
/// outright, even if it were laid out like a repository.
pub fn gap_tasks(
    ws: &Path,
    card: &Scorecard,
    agent: &str,
) -> EaiResult<crate::admin::tasks::AuthoredReport> {
    use crate::admin::tasks;
    let empty = tasks::AuthoredReport {
        vectors: vec![],
        tasks: vec![],
    };
    if !ws.join(".agents/roadmap.json").is_file() || !ws.join(".agents/tasks").is_dir() {
        return Ok(empty);
    }
    if same_dir(ws, &susi_paths::SusiDirs::substrate_home()) {
        return Ok(empty);
    }
    let (ladder, _) = load_ladder(ws)?;
    let open = tasks::list_open(ws);
    let vectors = tasks::roadmap_vectors(ws).unwrap_or_default();
    let already_known = |id: &str| {
        open.iter().any(|t| {
            t.title.contains(id) || t.goal.contains(id) || t.accept.cmd.join(" ").contains(id)
        }) || vectors
            .iter()
            .any(|v| v.title.contains(id) || v.progress.contains(id))
    };

    let failures = failing_rungs(&ladder, card);
    let new_tasks: Vec<tasks::NewTask> = failures
        .iter()
        .filter(|(i, _)| !already_known(&i.id))
        .take(GAP_TASKS_PER_RUN)
        .map(|(i, e)| repair_task(i, e, card.ladder_revision))
        .collect();
    tasks::author(
        ws,
        agent,
        &tasks::AuthoredSpec {
            rationale: format!(
                "intent ladder revision {} scorecard: {} failing rung(s) become repair tasks",
                card.ladder_revision,
                failures.len()
            ),
            vectors: vec![],
            tasks: new_tasks,
        },
    )
}

/// Failing (fail or partial) scorecard rows that name a rung on `ladder`,
/// triaged by observed impact: the higher the failed grade, the more
/// capability the failure reports.
fn failing_rungs<'a>(
    ladder: &'a IntentLadder,
    card: &'a Scorecard,
) -> Vec<(&'a Intent, &'a ScorecardEntry)> {
    let mut failures: Vec<(&Intent, &ScorecardEntry)> = card
        .entries
        .iter()
        .filter(|e| e.verdict == "fail" || e.verdict == "partial")
        .filter_map(|e| ladder.intents.iter().find(|i| i.id == e.id).map(|i| (i, e)))
        .collect();
    failures.sort_by_key(|(i, _)| std::cmp::Reverse(i.grade));
    failures
}

/// The repair task one failing rung becomes — the same title, evidence and
/// `--verify` acceptance whether it is authored straight from a scorecard
/// or adopted later from the host outbox.
fn repair_task(
    i: &Intent,
    e: &ScorecardEntry,
    ladder_revision: u64,
) -> crate::admin::tasks::NewTask {
    crate::admin::tasks::NewTask {
        title: format!("Restore intent {} ({})", i.id, i.grade.name()),
        goal: format!(
            "Restoring the {} intent `{}` from ladder revision {}.\n\
             Expected: {}.\n\
             Observed: {} — verdict {}, stopped {}, worker {}, {}s and {}µs.\n\
             Close requires the intent itself to pass: \
             `susi admin intent-ladder --verify {}`.",
            i.grade.name(),
            i.id,
            ladder_revision,
            describe_check(&i.check),
            e.detail,
            e.verdict,
            e.stopped,
            e.worker,
            e.seconds,
            e.micros,
            i.id,
        ),
        size: "m".into(),
        deps: vec![],
        accept: vec![
            "susi".into(),
            "admin".into(),
            "intent-ladder".into(),
            "--verify".into(),
            i.id.clone(),
        ],
        roadmap: None,
    }
}

fn same_dir(a: &Path, b: &Path) -> bool {
    a == b
        || matches!(
            (std::fs::canonicalize(a), std::fs::canonicalize(b)),
            (Ok(x), Ok(y)) if x == y
        )
}

/// The host outbox of failing rungs, under the susi home.
pub const GAPS_FILE: &str = "intent-gaps.json";
/// Outbox schema — versioned independently of the scorecard.
pub const GAPS_SCHEMA: &str = "susi.intent_gaps/v1";
/// Unadopted gaps the outbox holds at most: like [`GAP_TASKS_PER_RUN`],
/// the ladder cannot queue work faster than agents adopt it.
pub const GAPS_PENDING_MAX: usize = 16;
/// How long an adoption holds a gap whose rung keeps failing. Agent
/// worktrees are routinely discarded and their task files routinely never
/// merge; an adoption that has not closed the rung within this bound is
/// offered again, so a failing rung never drops out of every `susi tasks
/// list` for good. A checkout that already holds the adopted task (it
/// merged) re-links to it instead of authoring a duplicate.
pub const GAP_ADOPTION_TTL_SECS: u64 = 24 * 60 * 60;
/// Lock name (`<home>/intent-gaps.lock`) serialising every
/// read-modify-write of the outbox across processes: the daemon records,
/// CLIs in any number of worktrees adopt.
const GAPS_LOCK: &str = "intent-gaps";

/// One failing rung awaiting (or past) adoption as a repair task.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IntentGap {
    pub intent: String,
    pub grade: String,
    pub ladder_revision: u64,
    /// The independent check, as evidence prose.
    pub expected: String,
    pub verdict: String,
    pub stopped: String,
    pub worker: String,
    pub seconds: u64,
    pub micros: u64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
    pub first_seen_unix: u64,
    pub last_seen_unix: u64,
    /// Scorecards that failed this rung since it entered the outbox.
    pub failures: u32,
    /// The task id it became, once an agent adopted it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adopted: Option<String>,
    /// When it was adopted (unix seconds).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub adopted_unix: u64,
    /// The checkout that adopted it — where its task file was authored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adopted_in: Option<PathBuf>,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

impl IntentGap {
    /// What an agent runs, in its own checkout, to turn this gap into a task.
    #[must_use]
    pub fn adopt_command(&self) -> String {
        format!("susi admin intent-ladder --adopt-gap {}", self.intent)
    }

    /// Whether an agent may adopt this gap at `now`: nobody has, or the
    /// adoption no longer stands — older than [`GAP_ADOPTION_TTL_SECS`]
    /// while the rung kept failing, or the checkout that adopted it is gone
    /// (a discarded worktree takes its unmerged task file with it).
    #[must_use]
    pub fn is_pending_at(&self, now: u64) -> bool {
        if self.adopted.is_none() {
            return true;
        }
        now.saturating_sub(self.adopted_unix) >= GAP_ADOPTION_TTL_SECS
            || self.adopted_in.as_ref().is_some_and(|ws| !ws.is_dir())
    }

    /// The scorecard row the gap was recorded from.
    fn entry(&self) -> ScorecardEntry {
        ScorecardEntry {
            id: self.intent.clone(),
            grade: self.grade.clone(),
            verdict: self.verdict.clone(),
            seconds: self.seconds,
            micros: self.micros,
            worker: self.worker.clone(),
            stopped: self.stopped.clone(),
            detail: self.detail.clone(),
        }
    }

    fn observe(&mut self, e: &ScorecardEntry, ladder_revision: u64, now: u64) {
        self.verdict = e.verdict.clone();
        self.stopped = e.stopped.clone();
        self.worker = e.worker.clone();
        self.seconds = e.seconds;
        self.micros = e.micros;
        self.detail = e.detail.clone();
        self.ladder_revision = ladder_revision;
        self.last_seen_unix = now;
        self.failures = self.failures.saturating_add(1);
    }

    fn adopt(&mut self, task: &str, ws: &Path, now: u64) {
        self.adopted = Some(task.to_string());
        self.adopted_unix = now;
        self.adopted_in = Some(std::fs::canonicalize(ws).unwrap_or_else(|_| ws.to_path_buf()));
    }
}

/// `<home>/intent-gaps.json`: what the daemon found failing, for agents to
/// adopt. The daemon records gaps and never authors task files; an
/// adoption only marks the gap it turned into a task.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GapOutbox {
    pub schema: String,
    pub updated_unix: u64,
    pub gaps: Vec<IntentGap>,
}

impl Default for GapOutbox {
    fn default() -> Self {
        Self {
            schema: GAPS_SCHEMA.into(),
            updated_unix: 0,
            gaps: Vec::new(),
        }
    }
}

impl GapOutbox {
    /// Gaps an agent may adopt now (see [`IntentGap::is_pending_at`]).
    pub fn pending(&self) -> impl Iterator<Item = &IntentGap> + '_ {
        self.pending_at(now_unix())
    }

    /// Gaps an agent may adopt at `now`.
    pub fn pending_at(&self, now: u64) -> impl Iterator<Item = &IntentGap> + '_ {
        self.gaps.iter().filter(move |g| g.is_pending_at(now))
    }
}

pub fn gaps_path(home: &Path) -> PathBuf {
    home.join(GAPS_FILE)
}

/// The outbox, or an empty one when it is absent or unreadable.
#[must_use]
pub fn read_gaps(home: &Path) -> GapOutbox {
    std::fs::read_to_string(gaps_path(home))
        .ok()
        .and_then(|text| serde_json::from_str::<GapOutbox>(&text).ok())
        .filter(|outbox| outbox.schema == GAPS_SCHEMA)
        .unwrap_or_default()
}

fn write_gaps(home: &Path, outbox: &GapOutbox) -> EaiResult<()> {
    let body = serde_json::to_vec_pretty(outbox)
        .map_err(|e| EaiError::internal(format!("intent gaps: {e}")))?;
    let _ = std::fs::create_dir_all(home);
    susi_config::atomic_write_bytes(&gaps_path(home), &body)
        .map_err(|e| EaiError::io(format!("{}: {e}", gaps_path(home).display())))
}

/// The cross-process outbox lock: the daemon's [`record_gaps`] and every
/// worktree's [`adopt_gap`] read-modify-write the same file, and without it
/// an interleaving loses either the adoption mark (letting a second
/// worktree adopt the same gap) or the daemon's refresh.
fn lock_gaps(home: &Path) -> EaiResult<crate::susi_config::file_lock::FileLock> {
    // Adoption reads the task lists under this lock, which can take seconds
    // on a loaded host; the default ~3 s wait then failed a concurrent record
    // instead of letting it take its turn. Still well under the lock's 60 s
    // staleness threshold, so a wedged holder is not waited on forever.
    let wait = std::time::Duration::from_secs(30);
    crate::susi_config::file_lock::FileLock::acquire_within(home, GAPS_LOCK, wait).ok_or_else(
        || {
            EaiError::io(format!(
                "intent gaps: {} is held by another process",
                home.join(format!("{GAPS_LOCK}.lock")).display()
            ))
        },
    )
}

/// Record a scorecard into the host outbox at `<home>/intent-gaps.json` —
/// the daemon's whole share of VC-201-012. It writes nowhere else: a daemon
/// has no repository, and authoring into whatever directory it happened to
/// start in put untracked `.agents/tasks/T-SUSI-*.json` files into agents'
/// worktrees.
///
/// A failing rung already in the outbox is refreshed (evidence, count),
/// adopted or not — an adoption that stops standing makes it pending again
/// ([`IntentGap::is_pending_at`]); a new one enters triaged by grade, at
/// most [`GAP_TASKS_PER_RUN`] per scorecard and [`GAPS_PENDING_MAX`]
/// pending in all; a rung that passes leaves the outbox, so it heals
/// itself. The ladder is resolved from `home` (its
/// `.agents/intent-ladder.json` override, else the bundled revision) — the
/// same resolution the daemon runs the ladder with.
pub fn record_gaps(home: &Path, card: &Scorecard) -> EaiResult<GapOutbox> {
    let (ladder, _) = load_ladder(home)?;
    let _lock = lock_gaps(home)?;
    let now = now_unix();
    let mut outbox = read_gaps(home);
    let previous = outbox.clone();
    // A rung the current ladder no longer has is never run again, so it can
    // never pass and leave: after a revision renamed or dropped it, its gap
    // stayed pending in every worktree's task list for good. Keyed on the
    // ladder, not the card, so a partial startup-slice card keeps the rest.
    outbox
        .gaps
        .retain(|g| ladder.intents.iter().any(|i| i.id == g.intent));
    for e in &card.entries {
        if e.verdict == "pass" {
            outbox.gaps.retain(|g| g.intent != e.id);
        } else if e.verdict == "fail" || e.verdict == "partial" {
            if let Some(gap) = outbox.gaps.iter_mut().find(|g| g.intent == e.id) {
                gap.observe(e, card.ladder_revision, now);
            }
        }
    }
    let room = GAPS_PENDING_MAX.saturating_sub(outbox.pending_at(now).count());
    let fresh: Vec<IntentGap> = failing_rungs(&ladder, card)
        .into_iter()
        .filter(|(i, _)| !outbox.gaps.iter().any(|g| g.intent == i.id))
        .take(GAP_TASKS_PER_RUN.min(room))
        .map(|(i, e)| IntentGap {
            intent: i.id.clone(),
            grade: i.grade.name().into(),
            ladder_revision: card.ladder_revision,
            expected: describe_check(&i.check),
            verdict: e.verdict.clone(),
            stopped: e.stopped.clone(),
            worker: e.worker.clone(),
            seconds: e.seconds,
            micros: e.micros,
            detail: e.detail.clone(),
            first_seen_unix: now,
            last_seen_unix: now,
            failures: 1,
            adopted: None,
            adopted_unix: 0,
            adopted_in: None,
        })
        .collect();
    outbox.gaps.extend(fresh);
    // Highest grade first, the order an agent should adopt them in.
    let rank = |g: &IntentGap| {
        ladder
            .intents
            .iter()
            .find(|i| i.id == g.intent)
            .map(|i| i.grade)
    };
    outbox
        .gaps
        .sort_by(|a, b| rank(b).cmp(&rank(a)).then_with(|| a.intent.cmp(&b.intent)));
    if outbox != previous || !gaps_path(home).is_file() {
        outbox.schema = GAPS_SCHEMA.into();
        outbox.updated_unix = now;
        write_gaps(home, &outbox)?;
    }
    Ok(outbox)
}

/// What [`adopt_gap`] did.
#[derive(Debug, Clone, PartialEq)]
pub struct GapAdoption {
    /// The gap as the outbox now records it.
    pub gap: IntentGap,
    /// Tasks authored into the checkout — empty when it re-linked a task
    /// it already held, or authored nothing.
    pub authored: crate::admin::tasks::AuthoredReport,
    /// The open task the gap is now adopted by: freshly authored, or one the
    /// checkout already held for the intent (an earlier adoption that
    /// merged). `None` when nothing was authored or linked.
    pub task: Option<String>,
}

/// Adopt one outbox gap as a repair task in the repository `ws` — the
/// agent's own checkout, where it commits the task through the normal gate.
/// The task is authored by [`gap_tasks`] from the gap's recorded evidence,
/// so it carries the same title, goal and `--verify` acceptance as one
/// authored straight from the scorecard, and the same dedupe and bounds
/// apply. Once a task lands the gap is marked adopted with its id and this
/// checkout, so no other live worktree adopts it again.
///
/// A gap whose adoption no longer stands ([`IntentGap::is_pending_at`]) is
/// adopted afresh, and so is one whose adopted task this checkout has
/// already closed while the rung fails again (a regression after repair),
/// or one this very checkout adopted and has since dropped (its task is
/// neither open nor closed here).
/// If the checkout already holds an open task naming the intent, the gap is
/// re-linked to that task and nothing new is authored. The whole
/// read-modify-write runs under the outbox lock.
///
/// # Errors
/// [`EaiError::config`] when the outbox holds no such gap or its adoption
/// still stands; errors from [`gap_tasks`] or the lock otherwise.
pub fn adopt_gap(ws: &Path, home: &Path, intent: &str, agent: &str) -> EaiResult<GapAdoption> {
    use crate::admin::tasks;
    let _lock = lock_gaps(home)?;
    let now = now_unix();
    let mut outbox = read_gaps(home);
    let Some(gap) = outbox.gaps.iter_mut().find(|g| g.intent == intent) else {
        return Err(EaiError::config(format!(
            "no intent gap `{intent}` in {}",
            gaps_path(home).display()
        )));
    };
    if let Some(task) = gap.adopted.clone() {
        let done = tasks::list_done(ws);
        let open_here = tasks::list_open(ws).iter().any(|t| t.id == task);
        // Closed here, and the daemon saw the rung fail after the close: a
        // regression after repair earns a fresh task at once.
        let regressed_here = done.iter().any(|t| {
            t.id == task
                && t.closed
                    .as_ref()
                    .is_some_and(|c| gap.last_seen_unix > c.at_unix)
        });
        // This is the adopting checkout, and the task is neither open nor
        // closed in it: the adoption was dropped right here.
        let dropped_here = !open_here
            && !done.iter().any(|t| t.id == task)
            && gap
                .adopted_in
                .as_ref()
                .is_some_and(|adopter| same_dir(adopter, ws));
        if !gap.is_pending_at(now) && !regressed_here && !dropped_here {
            let held_by = if open_here {
                " and that task is open in this checkout".to_string()
            } else {
                gap.adopted_in
                    .as_ref()
                    .map(|p| format!(" in {}", p.display()))
                    .unwrap_or_default()
            };
            return Err(EaiError::config(format!(
                "intent gap `{intent}` was already adopted as {task} {}s ago{held_by}; it is \
                 offered again once {}h pass without the rung passing, or once the adopting \
                 checkout is gone",
                now.saturating_sub(gap.adopted_unix),
                GAP_ADOPTION_TTL_SECS / 3600,
            )));
        }
    }
    // An open task here already names the intent: the earlier adoption
    // merged (or this checkout authored it) — link it, author nothing.
    let names = |t: &tasks::Task| {
        t.title.contains(intent)
            || t.goal.contains(intent)
            || t.accept.cmd.join(" ").contains(intent)
    };
    if let Some(existing) = tasks::list_open(ws).into_iter().find(|t| names(t)) {
        gap.adopt(&existing.id, ws, now);
        let adoption = GapAdoption {
            gap: gap.clone(),
            authored: tasks::AuthoredReport {
                vectors: vec![],
                tasks: vec![],
            },
            task: Some(existing.id),
        };
        outbox.updated_unix = now;
        write_gaps(home, &outbox)?;
        return Ok(adoption);
    }
    let card = Scorecard {
        schema: SCORECARD_SCHEMA.into(),
        ladder_revision: gap.ladder_revision,
        ladder_source: gaps_path(home).display().to_string(),
        full_ladder: false,
        started_unix: gap.last_seen_unix,
        entries: vec![gap.entry()],
    };
    let authored = gap_tasks(ws, &card, agent)?;
    let Some(task) = authored.tasks.first().cloned() else {
        return Ok(GapAdoption {
            gap: gap.clone(),
            authored,
            task: None,
        });
    };
    gap.adopt(&task, ws, now);
    let adoption = GapAdoption {
        gap: gap.clone(),
        authored,
        task: Some(task),
    };
    outbox.updated_unix = now;
    write_gaps(home, &outbox)?;
    Ok(adoption)
}
