//! The OpenClaw executor.
//!
//! One headless agent turn, run to a deadline and attributed to a mission and
//! task. The run's generated OpenClaw configuration points at the metering
//! gateway with a per-run key (`susi:<mission>:<task>:<attempt>`), so every
//! model call the agent makes is priced and attributed without teaching
//! OpenClaw a new header.
//!
//! Two operational facts this module encodes, both observed on a real run:
//!
//! - A successful OpenClaw exec can still exit non-zero (its SQLite cleanup
//!   races its own writers), so the verdict is the `--json` envelope, never the
//!   exit code.
//! - The Store alias of `openclaw` runs sandboxed and cannot read a workspace
//!   outside its package; the caller passes the npm shim explicitly.

use serde_json::{json, Value};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use susi_error::{EaiError, EaiResult};

/// One headless agent turn.
pub struct ExecutorConfig {
    /// The OpenClaw executable (the npm shim, not the sandboxed Store alias).
    pub program: PathBuf,
    /// Gateway base URL, e.g. `http://127.0.0.1:9185`.
    pub gateway_url: String,
    /// Model id as the gateway's catalogue knows it, e.g. `deepseek-flash`.
    pub model: String,
    /// Working directory the agent reads and writes.
    pub workspace: PathBuf,
    /// Isolated OpenClaw state directory for this run.
    pub state_dir: PathBuf,
    /// Mission this run belongs to.
    pub mission: String,
    /// Task this run belongs to.
    pub task: String,
    /// 1-based attempt number.
    pub attempt: u32,
    /// Wall-clock deadline.
    pub timeout: Duration,
    /// Kill the turn when the workspace has not changed for this long.
    ///
    /// With a ledger, the clock starts at the task's first priced call, so a
    /// slow agent startup is not mistaken for a stall. Without one, it starts
    /// when the process does.
    pub stall_after: Option<Duration>,
    /// Ledger the gateway writes, used to see call activity while the
    /// workspace stays untouched.
    pub ledger_path: Option<PathBuf>,
    /// The instruction handed to the agent.
    pub prompt: String,
}

/// How one agent turn ended.
#[derive(Debug, Clone, PartialEq)]
pub enum RunOutcome {
    /// The envelope reported success.
    Succeeded {
        /// The agent's final text.
        final_text: String,
        /// OpenClaw's own cost estimate, informational only.
        reported_cost_usd: Option<f64>,
    },
    /// The envelope reported failure.
    Failed {
        /// The agent's reason.
        message: String,
    },
    /// The wall clock expired; the process tree was killed.
    TimedOut,
    /// The workspace stopped changing mid-turn; the process tree was killed.
    Stalled {
        /// Why and for how long.
        message: String,
    },
}

/// Run one headless turn, killing the process tree at the deadline.
///
/// # Errors
///
/// Returns `EaiError::io` when the state directory or config cannot be written,
/// and `EaiError::process` when the program cannot be spawned or waited on.
pub fn run(config: &ExecutorConfig) -> EaiResult<RunOutcome> {
    if config.program == Path::new("native")
        || config.program == Path::new("susi")
        || config.program == Path::new("internal")
    {
        return run_native(config);
    }
    std::fs::create_dir_all(&config.state_dir).map_err(|error| {
        EaiError::io(format!(
            "create state dir {}: {error}",
            config.state_dir.display()
        ))
    })?;
    let config_path = config.state_dir.join("openclaw.json");
    let document = serde_json::to_string_pretty(&openclaw_config(config))
        .map_err(|error| EaiError::internal(format!("render openclaw config: {error}")))?;
    std::fs::write(&config_path, document)
        .map_err(|error| EaiError::io(format!("write {}: {error}", config_path.display())))?;
    // A prompt with newlines cannot be passed as an argument to a `.cmd`
    // shim on Windows (the spawn is refused as batch-invalid), so it travels
    // as a file; that also keeps long prompts off the command line.
    let prompt_path = config.state_dir.join("prompt.md");
    std::fs::write(&prompt_path, &config.prompt)
        .map_err(|error| EaiError::io(format!("write {}: {error}", prompt_path.display())))?;

    let mut command = agent_command(config, &config_path, &prompt_path);
    let mut child = command.spawn().map_err(|error| {
        EaiError::process(format!("spawn {}: {error}", config.program.display()))
    })?;
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());

    // A grace period over the agent's own deadline, so its own timeout can
    // fire first and report why; past this we kill the tree ourselves.
    let deadline = Instant::now() + config.timeout + Duration::from_secs(30);
    let mut stall_after = config.stall_after;
    let mut last_write: Option<Instant> = None;
    let mut calls_seen: usize = 0;
    // Without a ledger there is no call clock; start one when the process does.
    let mut first_call = config.ledger_path.is_none().then(Instant::now);
    let mut next_progress_check = Instant::now() + Duration::from_secs(10);
    loop {
        match child.try_wait() {
            Ok(Some(_status)) => break,
            Ok(None) => {}
            Err(error) => {
                kill_tree(&mut child);
                return Err(EaiError::process(format!("wait for agent: {error}")));
            }
        }
        let now = Instant::now();
        if now >= deadline {
            kill_tree(&mut child);
            return Ok(RunOutcome::TimedOut);
        }
        if let Some(limit) = stall_after {
            if now >= next_progress_check {
                next_progress_check = now + Duration::from_secs(10);
                match workspace_changed(&config.workspace) {
                    Some(true) => {
                        last_write = Some(now);
                    }
                    Some(false) => {
                        if let Some(path) = &config.ledger_path {
                            if let Some(calls) = ledger_calls(path, &config.mission, &config.task) {
                                if calls > calls_seen {
                                    calls_seen = calls;
                                    if first_call.is_none() {
                                        first_call = Some(now);
                                    }
                                }
                            }
                        }
                        if let Some(started) = last_write.or(first_call) {
                            let quiet = now.duration_since(started);
                            if quiet > limit {
                                kill_tree(&mut child);
                                return Ok(RunOutcome::Stalled {
                                    message: format!(
                                        "no workspace change for {}s after {} call(s); the turn was stopped",
                                        quiet.as_secs(),
                                        calls_seen
                                    ),
                                });
                            }
                        }
                    }
                    // Not a git worktree: a non-change cannot be proven, so
                    // the guard disables itself for this turn.
                    None => stall_after = None,
                }
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    let out = String::from_utf8_lossy(&stdout.join().unwrap_or_default()).into_owned();
    let err = String::from_utf8_lossy(&stderr.join().unwrap_or_default()).into_owned();
    // Keep the envelope and the log beside the prompt: when a run fails, the
    // reason has to be diagnosable without replaying it.
    let _ = std::fs::write(config.state_dir.join("result.json"), &out);
    let _ = std::fs::write(config.state_dir.join("stderr.log"), &err);
    Ok(parse_envelope(&out).unwrap_or_else(|| RunOutcome::Failed {
        message: format!(
            "no JSON envelope from openclaw; stderr tail: {}",
            tail(&err)
        ),
    }))
}

/// A thing that can execute one turn.
///
/// The scheduler depends on this trait, not on OpenClaw, so a mission can be
/// tested without spawning a real agent.
pub trait Runner: Sync {
    /// Run one turn.
    ///
    /// # Errors
    ///
    /// Returns the executor's operational error (spawn, I/O), not an agent
    /// failure — those are [`RunOutcome`] values.
    fn run(&self, config: &ExecutorConfig) -> EaiResult<RunOutcome>;
}

/// The real OpenClaw runner.
pub struct OpenClaw;

impl Runner for OpenClaw {
    fn run(&self, config: &ExecutorConfig) -> EaiResult<RunOutcome> {
        run(config)
    }
}

/// The native in-process Rust autonomous coding worker.
///
/// Drives turn-by-turn completion through the metering gateway and executes
/// workspace operations directly, removing any dependency on Node.js or OpenClaw.
pub struct NativeWorker;

impl Runner for NativeWorker {
    fn run(&self, config: &ExecutorConfig) -> EaiResult<RunOutcome> {
        run_native(config)
    }
}

/// Run one turn using the native in-process worker.
pub fn run_native(config: &ExecutorConfig) -> EaiResult<RunOutcome> {
    crate::worker::run_agent_loop(config, 25)
}

/// Build the headless turn, pinning the config and state directory.
///
/// The environment variables alone are not enough: a running OpenClaw
/// gateway or tray can keep resolving models from the ambient config, and then
/// the `harness` provider this run just declared does not exist. `--config`
/// pins the run to the file the harness wrote. `--state-dir` keeps the run's
/// SQLite state in the directory we chose instead of a temporary one whose
/// cleanup races its own writers.
fn agent_command(config: &ExecutorConfig, config_path: &Path, prompt_path: &Path) -> Command {
    let mut command = Command::new(resolve_program(&config.program));
    command
        .arg("agent")
        .arg("exec")
        .arg("--model")
        .arg(format!("harness/{}", config.model))
        .arg("--cwd")
        .arg(&config.workspace)
        .arg("--config")
        .arg(config_path)
        .arg("--state-dir")
        .arg(&config.state_dir)
        .arg("--json")
        .arg("--timeout")
        .arg(config.timeout.as_secs().to_string())
        .arg("--message-file")
        .arg(prompt_path)
        .env("OPENCLAW_STATE_DIR", &config.state_dir)
        .env("OPENCLAW_CONFIG_PATH", config_path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn drain(mut pipe: Option<impl Read + Send + 'static>) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buffer = Vec::new();
        if let Some(pipe) = pipe.as_mut() {
            let _ = pipe.read_to_end(&mut buffer);
        }
        buffer
    })
}

/// The `--json` envelope, pointing at the gateway and carrying the run's key.
fn openclaw_config(config: &ExecutorConfig) -> Value {
    let model_ref = format!("harness/{}", config.model);
    let mut allowed = serde_json::Map::new();
    allowed.insert(model_ref.clone(), json!({}));
    json!({
        "models": {
            "mode": "merge",
            "providers": {
                "harness": {
                    "baseUrl": format!("{}/v1", config.gateway_url.trim_end_matches('/')),
                    "api": "openai-completions",
                    "apiKey": format!("susi:{}:{}:{}", config.mission, config.task, config.attempt),
                    "models": [{
                        "id": config.model,
                        "name": format!("{} via SUSI harness", config.model),
                        "reasoning": true,
                        "input": ["text"],
                        "cost": {
                            "input": 0.0,
                            "output": 0.0,
                            "cacheRead": 0.0,
                            "cacheWrite": 0.0
                        },
                        "contextWindow": 1_000_000,
                        "maxTokens": 8192,
                        "compat": {
                            "supportsUsageInStreaming": true,
                            "maxTokensField": "max_tokens"
                        }
                    }]
                }
            }
        },
        "agents": {
            "defaults": {
                "model": { "primary": model_ref },
                "models": Value::Object(allowed)
            }
        }
    })
}

fn parse_envelope(stdout: &str) -> Option<RunOutcome> {
    let trimmed = stdout.trim();
    let value = serde_json::from_str::<Value>(trimmed).ok().or_else(|| {
        let start = trimmed.find('{')?;
        serde_json::from_str::<Value>(&trimmed[start..]).ok()
    })?;
    let ok = value.get("ok").and_then(Value::as_bool).unwrap_or(false);
    let turns = value
        .get("assistantTurns")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    // A successful turn can still report `ok: false` because OpenClaw's own
    // SQLite cleanup raced its writers. That is not an agent failure: the turn
    // ran (turns > 0) and the only error is the cleanup. Any other error is
    // real.
    let cleanup_only = value
        .get("error")
        .and_then(|error| error.get("kind"))
        .and_then(Value::as_str)
        == Some("cleanup");
    if ok || (cleanup_only && turns > 0) {
        Some(RunOutcome::Succeeded {
            final_text: value
                .get("final")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            reported_cost_usd: value.get("costUsd").and_then(Value::as_f64),
        })
    } else {
        Some(RunOutcome::Failed {
            message: value
                .get("error")
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("openclaw reported failure")
                .to_string(),
        })
    }
}

fn tail(text: &str) -> String {
    const LIMIT: usize = 800;
    if text.len() <= LIMIT {
        return text.to_string();
    }
    let start = text.len() - LIMIT;
    text.get(start..).unwrap_or(text).to_string()
}

/// Resolve the agent program.
///
/// A bare `openclaw` resolves to the npm shim when it exists. The WindowsApps
/// alias is the sandboxed Store build: it spawns a persistent session host and
/// a sandbox broker for the tool call, and the worktree the harness handed it
/// is outside that sandbox — runs that looked successful made no repository
/// change and left state locks behind. An explicit path is never rewritten.
fn resolve_program(program: &Path) -> PathBuf {
    if program != Path::new("openclaw") {
        return program.to_path_buf();
    }
    let Some(appdata) = std::env::var_os("APPDATA") else {
        return program.to_path_buf();
    };
    let shim = PathBuf::from(appdata).join("npm").join("openclaw.cmd");
    if shim.is_file() {
        shim
    } else {
        program.to_path_buf()
    }
}

/// Whether the workspace has uncommitted or untracked changes.
///
/// `None` means the question cannot be answered (not a git checkout, git
/// missing), and the stall guard must not guess.
fn workspace_changed(workspace: &Path) -> Option<bool> {
    let output = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(["status", "--porcelain"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(!String::from_utf8_lossy(&output.stdout).trim().is_empty())
}

/// Priced calls already recorded for one mission and task.
///
/// `None` means the ledger cannot be read; the guard then has no call clock and
/// must not invent one.
fn ledger_calls(path: &Path, mission: &str, task: &str) -> Option<usize> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut calls = 0;
    for line in text.lines() {
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if entry.get("mission").and_then(Value::as_str) == Some(mission)
            && entry.get("task").and_then(Value::as_str) == Some(task)
        {
            calls += 1;
        }
    }
    Some(calls)
}

#[cfg(windows)]
fn kill_tree(child: &mut Child) {
    let pid = child.id().to_string();
    let _ = Command::new("taskkill")
        .args(["/PID", pid.as_str(), "/T", "/F"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = child.wait();
}

#[cfg(unix)]
fn kill_tree(child: &mut Child) {
    let pid = child.id().to_string();
    let _ = Command::new("kill")
        .args(["-s", "KILL", &format!("-{pid}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(not(any(windows, unix)))]
fn kill_tree(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> ExecutorConfig {
        ExecutorConfig {
            program: PathBuf::from("openclaw"),
            gateway_url: "http://127.0.0.1:9185".into(),
            model: "deepseek-flash".into(),
            workspace: PathBuf::from("scratch"),
            state_dir: PathBuf::from("state"),
            mission: "pilot-2".into(),
            task: "exec-1".into(),
            attempt: 3,
            timeout: Duration::from_secs(60),
            stall_after: None,
            ledger_path: None,
            prompt: "say OK".into(),
        }
    }

    #[test]
    fn config_points_at_the_gateway_and_tags_the_run() {
        let value = openclaw_config(&config());
        let provider = &value["models"]["providers"]["harness"];
        assert_eq!(provider["baseUrl"], "http://127.0.0.1:9185/v1");
        assert_eq!(provider["api"], "openai-completions");
        assert_eq!(provider["apiKey"], "susi:pilot-2:exec-1:3");
        assert_eq!(provider["models"][0]["id"], "deepseek-flash");
        assert_eq!(
            value["agents"]["defaults"]["model"]["primary"],
            "harness/deepseek-flash"
        );
        assert!(value["agents"]["defaults"]["models"]["harness/deepseek-flash"].is_object());
    }

    #[test]
    fn the_command_pins_the_generated_config_and_state_dir() {
        let config = config();
        let command = agent_command(
            &config,
            Path::new("state/openclaw.json"),
            Path::new("state/prompt.md"),
        );
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(args
            .windows(2)
            .any(|pair| pair[0] == "--config" && pair[1] == "state/openclaw.json"));
        assert!(args
            .windows(2)
            .any(|pair| pair[0] == "--state-dir" && pair[1] == "state"));
        assert!(args
            .windows(2)
            .any(|pair| pair[0] == "--message-file" && pair[1] == "state/prompt.md"));
    }

    #[test]
    fn a_trailing_base_url_does_not_double_the_slash() {
        let mut config = config();
        config.gateway_url = "http://127.0.0.1:9185/".into();
        let value = openclaw_config(&config);
        assert_eq!(
            value["models"]["providers"]["harness"]["baseUrl"],
            "http://127.0.0.1:9185/v1"
        );
    }

    #[test]
    fn the_envelope_is_the_verdict_not_the_exit_code() {
        let success = r#"{
            "ok": true,
            "status": "ok",
            "final": "OK",
            "costUsd": 0.001606,
            "error": { "message": "cleanup failed: EBUSY" }
        }"#;
        match parse_envelope(success) {
            Some(RunOutcome::Succeeded {
                final_text,
                reported_cost_usd,
            }) => {
                assert_eq!(final_text, "OK");
                let cost = reported_cost_usd.unwrap_or_default();
                assert!((cost - 0.001_606).abs() < 1e-9, "cost was {cost}");
            }
            other => panic!("unexpected envelope: {other:?}"),
        }

        let failure = r#"{"ok": false, "error": {"message": "no model"}}"#;
        assert_eq!(
            parse_envelope(failure),
            Some(RunOutcome::Failed {
                message: "no model".into()
            })
        );
    }

    #[test]
    fn a_cleanup_error_does_not_fail_a_turn_that_ran() {
        let envelope = r#"{
            "ok": false,
            "status": "error",
            "final": "OK",
            "assistantTurns": 3,
            "costUsd": 0.0016,
            "error": { "kind": "cleanup", "message": "Agent exec cleanup failed: EBUSY" }
        }"#;
        assert!(matches!(
            parse_envelope(envelope),
            Some(RunOutcome::Succeeded { .. })
        ));

        let nothing_ran = r#"{
            "ok": false,
            "assistantTurns": 0,
            "error": { "kind": "cleanup", "message": "nothing ran" }
        }"#;
        assert!(matches!(
            parse_envelope(nothing_ran),
            Some(RunOutcome::Failed { .. })
        ));
    }

    #[test]
    fn a_log_prefix_does_not_hide_the_envelope() {
        let text = "some log line\n{\n  \"ok\": true,\n  \"final\": \"done\"\n}";
        assert!(matches!(
            parse_envelope(text),
            Some(RunOutcome::Succeeded { .. })
        ));
    }

    #[test]
    fn no_envelope_is_none() {
        assert_eq!(parse_envelope("plain text only"), None);
    }

    #[test]
    fn a_workspace_change_is_only_provable_in_a_git_checkout() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(workspace_changed(dir.path()), None);
        let initialized = Command::new("git")
            .args(["init", "-q"])
            .current_dir(dir.path())
            .status();
        if !initialized.map(|status| status.success()).unwrap_or(false) {
            return; // git is not available in this test environment
        }
        assert_eq!(workspace_changed(dir.path()), Some(false));
        std::fs::write(dir.path().join("written.txt"), "work").unwrap();
        assert_eq!(workspace_changed(dir.path()), Some(true));
    }

    #[test]
    fn an_explicit_program_is_never_rewritten() {
        let explicit = Path::new("C:/tools/openclaw-custom.exe");
        assert_eq!(resolve_program(explicit), explicit);
    }

    #[test]
    fn ledger_calls_counts_only_that_task() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.jsonl");
        std::fs::write(
            &path,
            "{\"mission\":\"m\",\"task\":\"t\"}\n{\"mission\":\"m\",\"task\":\"t\"}\n\
             {\"mission\":\"m\",\"task\":\"x\"}\nnot json\n",
        )
        .unwrap();
        assert_eq!(ledger_calls(&path, "m", "t"), Some(2));
        assert_eq!(ledger_calls(&dir.path().join("absent"), "m", "t"), None);
    }

    #[test]
    fn native_worker_implements_runner() {
        let runner = NativeWorker;
        let mut cfg = config();
        let test_state =
            std::env::temp_dir().join(format!("susi-worker-test-{}", std::process::id()));
        cfg.state_dir = test_state.clone();
        cfg.program = PathBuf::from("native");
        // Non-existent gateway should fail cleanly with a message rather than crashing
        cfg.gateway_url = "http://127.0.0.1:1".into();
        let outcome = runner.run(&cfg).unwrap();
        match outcome {
            RunOutcome::Failed { message } => {
                assert!(
                    message.contains("gateway call failed")
                        || message.contains("connection refused")
                );
            }
            other => panic!("expected failed outcome, got {other:?}"),
        }
    }
}
