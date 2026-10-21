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
}

/// Run one headless turn, killing the process tree at the deadline.
///
/// # Errors
///
/// Returns `EaiError::io` when the state directory or config cannot be written,
/// and `EaiError::process` when the program cannot be spawned or waited on.
pub fn run(config: &ExecutorConfig) -> EaiResult<RunOutcome> {
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
    loop {
        match child.try_wait() {
            Ok(Some(_status)) => break,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Ok(None) => {
                kill_tree(&mut child);
                return Ok(RunOutcome::TimedOut);
            }
            Err(error) => {
                kill_tree(&mut child);
                return Err(EaiError::process(format!("wait for agent: {error}")));
            }
        }
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

/// Build the headless turn, pinning the config and state directory.
///
/// The environment variables alone are not enough: a running OpenClaw
/// gateway or tray can keep resolving models from the ambient config, and then
/// the `harness` provider this run just declared does not exist. `--config`
/// pins the run to the file the harness wrote. `--state-dir` keeps the run's
/// SQLite state in the directory we chose instead of a temporary one whose
/// cleanup races its own writers.
fn agent_command(config: &ExecutorConfig, config_path: &Path, prompt_path: &Path) -> Command {
    let mut command = Command::new(&config.program);
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

#[cfg(not(windows))]
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
}
