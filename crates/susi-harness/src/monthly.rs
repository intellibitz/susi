//! Resolve monthly providers before giving a single CLI the prompt.

use super::{
    build_invocation, default_search_dirs, drain, find_in_dirs, monthly_auto, reap, run_piped_with,
    with_model, CliAuth, CliCommand, CliSubRunner, CliToolKind, ExecutorConfig, PipedRun,
    PollTiming, RunOutcome, DRAIN_GRACE,
};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const DEFAULT: &str = "codex,claude,gemini,antigravity";

fn preferences(raw: &str) -> Result<Vec<CliToolKind>, String> {
    let mut kinds = Vec::new();
    for name in raw.split(',') {
        let kind = CliToolKind::from_name(name)
            .ok_or_else(|| format!("unknown monthly provider '{name}'"))?;
        if !kinds.contains(&kind) {
            kinds.push(kind);
        }
    }
    Ok(kinds)
}

/// Preserve the native user's auth location. A monthly provider never inherits
/// the host's API keys, alternate endpoints, cloud auth toggles or Susi overlay.
pub(super) fn session_environment(command: &mut Command) {
    crate::workload::clean_environment(command);
    command.env("NO_BROWSER", "1");
    for name in [
        "HOME",
        "USERPROFILE",
        "APPDATA",
        "LOCALAPPDATA",
        "CODEX_HOME",
        "CLAUDE_CONFIG_DIR",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_RUNTIME_DIR",
        "DBUS_SESSION_BUS_ADDRESS",
        "GEMINI_CLI_SYSTEM_SETTINGS_PATH",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
}

/// Gemini's model list is built into the CLI. For a named Google-login
/// selection, require the native enterprise policy that rejects API key/Vertex
/// in noninteractive mode. Never write, substitute or relax the system policy.
fn google_login_enforced() -> bool {
    let path = std::env::var_os("GEMINI_CLI_SYSTEM_SETTINGS_PATH")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            if cfg!(windows) {
                r"C:\ProgramData\gemini-cli\settings.json".into()
            } else if cfg!(target_os = "macos") {
                "/Library/Application Support/GeminiCli/settings.json".into()
            } else {
                "/etc/gemini-cli/settings.json".into()
            }
        });
    if !path.is_absolute() {
        return false;
    }
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .is_some_and(|v| {
            v.pointer("/security/auth/enforcedType")
                .and_then(serde_json::Value::as_str)
                == Some("oauth-personal")
        })
}

fn subscription_login(kind: CliToolKind, stdout: &str, stderr: &str) -> bool {
    match kind {
        CliToolKind::Codex => stdout.lines().chain(stderr.lines()).any(|line| {
            ["logged in using chatgpt", "logged in with chatgpt"]
                .iter()
                .any(|status| line.trim().eq_ignore_ascii_case(status))
        }),
        CliToolKind::Claude => {
            serde_json::from_str::<serde_json::Value>(stdout).is_ok_and(|value| {
                value["loggedIn"] == true
                    && value["authMethod"] == "claude.ai"
                    && matches!(
                        value["subscriptionType"].as_str(),
                        Some("pro" | "max" | "team" | "enterprise")
                    )
            })
        }
        CliToolKind::Gemini
        | CliToolKind::Antigravity
        | CliToolKind::Qwen
        | CliToolKind::Aider
        | CliToolKind::OpenCode => false,
    }
}

fn status(program: &Path, kind: CliToolKind, workspace: &Path, deadline: Instant) -> bool {
    let args: &[&str] = match kind {
        CliToolKind::Codex => &["login", "status"],
        CliToolKind::Claude => &["auth", "status"],
        CliToolKind::Gemini
        | CliToolKind::Antigravity
        | CliToolKind::Qwen
        | CliToolKind::Aider
        | CliToolKind::OpenCode => return false,
    };
    let mut cmd = susi_paths::console::background_command(program);
    session_environment(&mut cmd);
    let Ok(mut child) = cmd
        .args(args)
        .current_dir(workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    else {
        return false;
    };
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let deadline = deadline.min(Instant::now() + Duration::from_secs(4));
    loop {
        match child.try_wait() {
            Ok(Some(result)) => {
                let grace = deadline.min(Instant::now() + DRAIN_GRACE);
                return result.success()
                    && subscription_login(kind, &stdout.collect(grace), &stderr.collect(grace));
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                reap(&mut child);
                return false;
            }
        }
    }
}

impl CliSubRunner {
    pub(super) fn run_monthly(&self, config: &ExecutorConfig) -> RunOutcome {
        let failed = |message: String| RunOutcome::Failed { message };
        if self.command_override.is_some() {
            return failed("monthly authorization requires a native provider executable".into());
        }
        let (kinds, submodel) = if monthly_auto(&config.model) {
            let raw =
                std::env::var("SUSI_HARNESS_MONTHLY_PREFER").unwrap_or_else(|_| DEFAULT.into());
            match preferences(&raw) {
                Ok(kinds) => (kinds, None),
                Err(e) => return failed(e),
            }
        } else if let Some((kind, submodel)) = CliToolKind::parse_spec(&config.model) {
            (vec![kind], submodel)
        } else {
            return failed("unknown monthly provider".into());
        };
        let dirs = self
            .search_dirs
            .clone()
            .unwrap_or_else(default_search_dirs)
            .into_iter()
            .filter(|p| p.is_absolute())
            .collect::<Vec<_>>();
        let workload = crate::workload::enabled();
        let google_pin =
            !monthly_auto(&config.model) && !workload && kinds == [CliToolKind::Gemini];
        if google_pin && !google_login_enforced() {
            return failed(
                "monthly:gemini requires the native system settings to enforce oauth-personal"
                    .into(),
            );
        }
        let deadline = Instant::now() + config.timeout;
        let mut reasons = Vec::new();
        for kind in kinds {
            if Instant::now() >= deadline {
                return RunOutcome::TimedOut;
            }
            let why = if workload && kind != CliToolKind::Codex {
                "no managed subscription adapter"
            } else if !google_pin && !matches!(kind, CliToolKind::Codex | CliToolKind::Claude) {
                "no verifiable headless subscription status"
            } else if let Some(program) = find_in_dirs(kind, &dirs) {
                if workload {
                    let name = submodel
                        .as_ref()
                        .map_or_else(|| "cli:codex".to_owned(), |m| format!("cli:codex:{m}"));
                    return self.run_workload_subscription(&with_model(config, &name));
                }
                if google_pin || status(&program, kind, &config.workspace, deadline) {
                    let (_, mut args, pipe_prompt_on_stdin) =
                        build_invocation(kind, submodel.as_deref());
                    match kind {
                        CliToolKind::Codex => {
                            args.splice(
                                1..1,
                                [
                                    "--ignore-user-config".into(),
                                    "-c".into(),
                                    "model_provider=\"openai\"".into(),
                                ],
                            );
                        }
                        CliToolKind::Claude => {
                            args.extend(["--setting-sources".into(), String::new()]);
                        }
                        CliToolKind::Gemini
                        | CliToolKind::Antigravity
                        | CliToolKind::Qwen
                        | CliToolKind::Aider
                        | CliToolKind::OpenCode => {}
                    }
                    let invocation = CliCommand {
                        program,
                        args,
                        pipe_prompt_on_stdin,
                    };
                    return match run_piped_with(
                        &invocation,
                        &config.workspace,
                        &config.prompt,
                        PollTiming {
                            timeout: deadline.saturating_duration_since(Instant::now()),
                            interval: self.poll_interval,
                        },
                        CliAuth::LocalPlan,
                    ) {
                        PipedRun::Finished(outcome) => outcome,
                        PipedRun::NotFound => {
                            failed(format!("{} disappeared before dispatch", kind.as_str()))
                        }
                    };
                }
                "native subscription login was not confirmed"
            } else {
                "executable unavailable"
            };
            reasons.push(format!("{}: {why}", kind.as_str()));
        }
        failed(format!(
            "no ready monthly provider ({})",
            reasons.join(", ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn auto_dispatch_requires_native_subscription_status() {
        assert!(subscription_login(
            CliToolKind::Codex,
            "",
            "Logged in using ChatGPT"
        ));
        assert!(!subscription_login(
            CliToolKind::Codex,
            "Logged in using an API key",
            ""
        ));
        assert!(subscription_login(
            CliToolKind::Claude,
            r#"{"loggedIn":true,"authMethod":"claude.ai","subscriptionType":"team"}"#,
            ""
        ));
        assert!(!subscription_login(
            CliToolKind::Claude,
            r#"{"loggedIn":true,"authMethod":"api_key","subscriptionType":"team"}"#,
            ""
        ));
        assert!(!subscription_login(CliToolKind::Gemini, "logged in", ""));
        assert_eq!(
            preferences("claude,codex,claude").unwrap(),
            vec![CliToolKind::Claude, CliToolKind::Codex]
        );
        assert!(preferences("claude,typo").is_err());
    }
}
