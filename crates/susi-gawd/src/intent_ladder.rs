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

/// Where a repo working copy would hold the ladder — the process cwd.
/// A daemon started anywhere still resolves correctly: a cwd that is
/// not a repo simply falls through to the bundled revision.
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
/// arbitrary directory.
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

    // Triage by observed impact: the higher the failed grade, the more
    // capability the failure reports.
    let mut failures: Vec<(&Intent, &ScorecardEntry)> = card
        .entries
        .iter()
        .filter(|e| e.verdict == "fail" || e.verdict == "partial")
        .filter_map(|e| ladder.intents.iter().find(|i| i.id == e.id).map(|i| (i, e)))
        .collect();
    failures.sort_by_key(|(i, _)| std::cmp::Reverse(i.grade));

    let new_tasks: Vec<tasks::NewTask> = failures
        .iter()
        .filter(|(i, _)| !already_known(&i.id))
        .take(GAP_TASKS_PER_RUN)
        .map(|(i, e)| tasks::NewTask {
            title: format!("Restore intent {} ({})", i.id, i.grade.name()),
            goal: format!(
                "Restoring the {} intent `{}` from ladder revision {}.\n\
                 Expected: {}.\n\
                 Observed: {} — verdict {}, stopped {}, worker {}, {}s and {}µs.\n\
                 Close requires the intent itself to pass: \
                 `susi admin intent-ladder --verify {}`.",
                i.grade.name(),
                i.id,
                card.ladder_revision,
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
        })
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
