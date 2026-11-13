//! The adaptive dual-mode runner: PAYG BYOK inside, subscription CLIs outside.
//!
//! The harness spends money two ways, and a mission should not care which:
//!
//! - **PAYG BYOK** ([`ExecutionMode::PaygByok`]): the native worker drives the
//!   metering gateway with the operator's own API keys. Every call is priced
//!   and recorded in the ledger.
//! - **Monthly-subscription CLI** ([`ExecutionMode::MonthlySubCli`]): an
//!   installed coding CLI (Claude Code, Codex, Gemini, Antigravity, Qwen,
//!   Aider, OpenCode) runs the turn on a flat monthly plan. Its marginal PAYG
//!   spend is `$0.00`.
//!
//! [`AdaptiveRunner`] chooses between them per turn under an
//! [`AdaptiveStrategy`] and fails over when one side is out of quota, out of
//! credit, missing or simply failed. [`SusiSocketEndpoints`] names the Susi AI
//! OS Socket the harness sits on: the GMCP HTTP tool socket, the Gemi
//! OpenAI-compatible completion socket and the Gawd A2A socket.
//!
//! The CLI path pipes the prompt on stdin rather than passing it as an
//! argument, so Windows `CommandLineToArgvW` never truncates a multi-line
//! prompt or mangles embedded quotes, and drains stdout and stderr on helper
//! threads so a chatty child can never fill a pipe and stall the turn.

use crate::executor::{ExecutorConfig, NativeWorker, RunOutcome, Runner};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ExitStatus, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use susi_error::EaiResult;

/// Environment variable selecting the [`AdaptiveStrategy`].
pub const ADAPTIVE_ENV: &str = "SUSI_HARNESS_ADAPTIVE";

/// Environment variable naming the CLI spec PAYG fails over to.
pub const FALLBACK_CLI_ENV: &str = "SUSI_HARNESS_FALLBACK_CLI";

/// Environment variable naming the PAYG model a CLI turn fails over to.
pub const FALLBACK_PAYG_ENV: &str = "SUSI_HARNESS_FALLBACK_PAYG";

/// Presence opts this process into Argon-only Antigravity routing, even if the
/// value is empty or misspelled. Remove the variable to disable the policy.
pub const ANTIGRAVITY_ARGON_ONLY_ENV: &str = "SUSI_HARNESS_ANTIGRAVITY_ARGON_ONLY";

/// Exact model pinned by the opt-in Antigravity policy.
pub const ANTIGRAVITY_ARGON_MODEL: &str = "gemini-4-argon-high";

fn argon_model(submodel: Option<&str>) -> Result<&'static str, String> {
    match submodel {
        None | Some(ANTIGRAVITY_ARGON_MODEL) => Ok(ANTIGRAVITY_ARGON_MODEL),
        Some(other) => Err(format!(
            "Antigravity Argon-only policy refuses model '{other}'; requires {ANTIGRAVITY_ARGON_MODEL}"
        )),
    }
}

/// The prompt flag argument for CLIs that take the real prompt on stdin.
const STDIN_PROMPT: &str = "Execute the instructions provided on stdin.";

/// How often a running CLI child is polled for exit.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// How long captured output is awaited after the child itself exited.
const DRAIN_GRACE: Duration = Duration::from_secs(2);

/// Upper bound on captured bytes per stream, so a runaway child cannot
/// exhaust memory.
const MAX_CAPTURE_BYTES: usize = 16 * 1024 * 1024;

/// Characters of output quoted in a failure message.
const FAILURE_TAIL_CHARS: usize = 2_000;

/// Which billing plane ran (or would run) a turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExecutionMode {
    /// Pay-as-you-go with the operator's own API keys, metered by the gateway.
    PaygByok,
    /// A monthly-subscription coding CLI installed on this machine.
    MonthlySubCli,
}

impl ExecutionMode {
    /// Stable lowercase name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PaygByok => "payg-byok",
            Self::MonthlySubCli => "monthly-sub-cli",
        }
    }
}

/// How [`AdaptiveRunner`] orders and fails over between the two planes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum AdaptiveStrategy {
    /// A `cli:`/`sub:` model spec runs on the CLI, anything else on PAYG; each
    /// fails over only when the matching fallback is configured.
    #[default]
    Auto,
    /// Subscription CLI first, PAYG when the CLI fails or times out.
    PreferCliSubThenPayg,
    /// PAYG first, subscription CLI when PAYG fails.
    PreferPaygThenCliSub,
    /// PAYG only; never shell out.
    PaygOnly,
    /// Subscription CLI only; never spend PAYG money.
    CliSubOnly,
}

impl AdaptiveStrategy {
    /// Parse a strategy name (`auto`, `cli-first`, `payg-first`, `payg-only`,
    /// `cli-only`). Case and surrounding whitespace are ignored; `_` is
    /// accepted for `-`.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        let normalized = value.trim().to_ascii_lowercase().replace('_', "-");
        match normalized.as_str() {
            "auto" | "" => Some(Self::Auto),
            "cli-first" | "sub-first" | "prefer-cli" => Some(Self::PreferCliSubThenPayg),
            "payg-first" | "byok-first" | "prefer-payg" => Some(Self::PreferPaygThenCliSub),
            "payg-only" | "byok-only" | "payg" => Some(Self::PaygOnly),
            "cli-only" | "sub-only" | "cli" => Some(Self::CliSubOnly),
            _ => None,
        }
    }

    /// The strategy named by [`ADAPTIVE_ENV`], [`AdaptiveStrategy::Auto`] when
    /// unset or unrecognised.
    #[must_use]
    pub fn from_env() -> Self {
        std::env::var(ADAPTIVE_ENV)
            .ok()
            .and_then(|value| Self::parse(&value))
            .unwrap_or_default()
    }

    /// Stable name, the inverse of [`AdaptiveStrategy::parse`].
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::PreferCliSubThenPayg => "cli-first",
            Self::PreferPaygThenCliSub => "payg-first",
            Self::PaygOnly => "payg-only",
            Self::CliSubOnly => "cli-only",
        }
    }
}

/// A monthly-subscription coding CLI the harness can drive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CliToolKind {
    /// Anthropic Claude Code (`claude`).
    Claude,
    /// OpenAI Codex CLI (`codex`).
    Codex,
    /// Google Gemini CLI (`gemini`).
    Gemini,
    /// Google Antigravity (`agy`).
    Antigravity,
    /// Qwen Code (`qwen`).
    Qwen,
    /// Aider (`aider`).
    Aider,
    /// OpenCode (`opencode`).
    OpenCode,
}

impl CliToolKind {
    /// Every supported CLI, in discovery preference order.
    pub const ALL: [Self; 7] = [
        Self::Claude,
        Self::Codex,
        Self::Gemini,
        Self::Antigravity,
        Self::Qwen,
        Self::Aider,
        Self::OpenCode,
    ];

    /// Stable lowercase name, as used in a `cli:<name>` spec.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Gemini => "gemini",
            Self::Antigravity => "antigravity",
            Self::Qwen => "qwen",
            Self::Aider => "aider",
            Self::OpenCode => "opencode",
        }
    }

    /// The tool a name or alias refers to.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "claude" | "claude-code" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            "gemini" | "gemini-cli" => Some(Self::Gemini),
            "agy" | "antigravity" => Some(Self::Antigravity),
            "qwen" | "qwen-code" => Some(Self::Qwen),
            "aider" => Some(Self::Aider),
            "opencode" | "open-code" => Some(Self::OpenCode),
            _ => None,
        }
    }

    /// Parse a CLI model spec: `cli:<tool>` or `sub:<tool>`, optionally
    /// followed by `:<submodel>` (`cli:claude:opus`). Any other model id —
    /// a PAYG catalogue id such as `deepseek-flash` — is `None`.
    #[must_use]
    pub fn parse_spec(model: &str) -> Option<(Self, Option<String>)> {
        let trimmed = model.trim();
        let (prefix, rest) = trimmed.split_once(':')?;
        if !(prefix.eq_ignore_ascii_case("cli") || prefix.eq_ignore_ascii_case("sub")) {
            return None;
        }
        let (tool, submodel) = match rest.split_once(':') {
            Some((tool, submodel)) => (tool, Some(submodel.trim())),
            None => (rest, None),
        };
        let kind = Self::from_name(tool)?;
        let submodel = submodel
            .filter(|submodel| !submodel.is_empty())
            .map(str::to_string);
        Some((kind, submodel))
    }

    /// Program names this tool installs under, in lookup order.
    #[must_use]
    pub fn candidates(self) -> Vec<&'static str> {
        build_invocation(self, None).0
    }
}

/// How to invoke one CLI headlessly: `(candidate_program_names, args,
/// pipe_prompt_on_stdin)`.
///
/// Every tool takes the real prompt on stdin, so argument quoting can never
/// truncate it.
#[must_use]
pub fn build_invocation(
    kind: CliToolKind,
    submodel: Option<&str>,
) -> (Vec<&'static str>, Vec<String>, bool) {
    let owned = |args: &[&str]| args.iter().map(|arg| (*arg).to_string()).collect();
    let (candidates, mut args, model_flag): (Vec<&'static str>, Vec<String>, Option<&str>) =
        match kind {
            CliToolKind::Claude => (
                vec!["claude", "claude.exe"],
                owned(&["--dangerously-skip-permissions", "-p", STDIN_PROMPT]),
                Some("--model"),
            ),
            CliToolKind::Codex => (
                vec!["codex", "codex.exe"],
                owned(&[
                    "exec",
                    "--dangerously-bypass-approvals-and-sandbox",
                    "--skip-git-repo-check",
                    "-",
                ]),
                Some("-m"),
            ),
            CliToolKind::Gemini => (
                vec!["gemini", "gemini.cmd", "gemini.exe"],
                owned(&["--yolo", "-p", STDIN_PROMPT]),
                Some("-m"),
            ),
            CliToolKind::Antigravity => (
                vec!["agy", "agy.exe", "antigravity"],
                // agy uses --model (not the Gemini CLI's -m/--yolo contract).
                // Its print timer is disabled; the harness task budget remains
                // authoritative and is separate from polling for completion.
                owned(&["--sandbox", "--print-timeout", "0", "--print", STDIN_PROMPT]),
                Some("--model"),
            ),
            CliToolKind::Qwen => (
                vec!["qwen", "qwen.cmd", "qwen.exe"],
                owned(&["--yolo", "-p", STDIN_PROMPT]),
                None,
            ),
            CliToolKind::Aider => (
                vec!["aider", "aider.exe"],
                owned(&["--yes-always", "--no-auto-commits", "--message-file", "-"]),
                None,
            ),
            CliToolKind::OpenCode => (vec!["opencode", "opencode.exe"], owned(&["run"]), None),
        };
    if let (Some(flag), Some(submodel)) = (model_flag, submodel) {
        args.push(flag.to_string());
        args.push(submodel.to_string());
    }
    (candidates, args, true)
}

/// A CLI found on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredCliTool {
    /// Which tool.
    pub kind: CliToolKind,
    /// The executable that was found.
    pub path: PathBuf,
}

/// Find each supported CLI in `dirs`, first directory wins per tool.
///
/// The result lists at most one entry per [`CliToolKind`], in
/// [`CliToolKind::ALL`] order.
#[must_use]
pub fn discover_cli_tools_in(dirs: &[PathBuf]) -> Vec<DiscoveredCliTool> {
    CliToolKind::ALL
        .iter()
        .filter_map(|kind| {
            find_in_dirs(*kind, dirs).map(|path| DiscoveredCliTool { kind: *kind, path })
        })
        .collect()
}

fn find_in_dirs(kind: CliToolKind, dirs: &[PathBuf]) -> Option<PathBuf> {
    let candidates = kind.candidates();
    dirs.iter().find_map(|dir| {
        candidates
            .iter()
            .map(|name| dir.join(name))
            .find(|path| path.is_file())
    })
}

/// The directories [`discover_cli_tools`] scans: `PATH`, then the standard
/// per-user install locations.
#[must_use]
pub fn default_search_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        let local = PathBuf::from(local);
        dirs.push(local.join("agy").join("bin"));
        dirs.push(local.join("claude-nolitellm"));
        dirs.push(local.join("Programs").join("claude"));
    }
    if let Some(roaming) = std::env::var_os("APPDATA") {
        dirs.push(PathBuf::from(roaming).join("npm"));
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    if let Some(home) = home {
        let home = PathBuf::from(home);
        dirs.push(home.join(".local").join("bin"));
        dirs.push(home.join(".cargo").join("bin"));
        dirs.push(home.join(".claude").join("local"));
    }
    let mut unique: Vec<PathBuf> = Vec::with_capacity(dirs.len());
    for dir in dirs {
        if !dir.as_os_str().is_empty() && !unique.contains(&dir) {
            unique.push(dir);
        }
    }
    unique
}

/// Find the supported CLIs installed on this machine.
#[must_use]
pub fn discover_cli_tools() -> Vec<DiscoveredCliTool> {
    discover_cli_tools_in(&default_search_dirs())
}

fn contains_any(text: &str, needles: &[&str]) -> bool {
    let lower = text.to_ascii_lowercase();
    needles.iter().any(|needle| lower.contains(needle))
}

/// Whether a CLI's output says its subscription session, usage window or rate
/// limit is spent.
#[must_use]
pub fn is_subscription_quota_exhausted(text: &str) -> bool {
    contains_any(
        text,
        &[
            "rate limit",
            "rate-limit",
            "usage limit",
            "quota exceeded",
            "too many requests",
            "429",
            "hit your limit",
            "resets at",
        ],
    )
}

/// Whether a PAYG failure says the key was refused or the account is out of
/// credit.
#[must_use]
pub fn is_payg_credit_or_key_exhausted(text: &str) -> bool {
    contains_any(
        text,
        &[
            "insufficient_quota",
            "insufficient balance",
            "insufficient credit",
            "no credit",
            "billing",
            "none of ",
            "api key",
            "401",
            "402",
        ],
    )
}

/// One exact program invocation, bypassing catalogue resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliCommand {
    /// Executable path or name.
    pub program: PathBuf,
    /// Arguments.
    pub args: Vec<String>,
    /// Whether the prompt is written to the child's stdin.
    pub pipe_prompt_on_stdin: bool,
}

/// Runs one turn on an installed monthly-subscription CLI.
///
/// The tool is chosen from `config.model` when it is a `cli:`/`sub:` spec,
/// otherwise [`CliSubRunner::default_kind`], otherwise the first tool
/// discovered on this machine. A tool that is not installed is a
/// [`RunOutcome::Failed`], so an [`AdaptiveRunner`] can fail over to PAYG.
#[derive(Debug, Clone)]
pub struct CliSubRunner {
    /// The tool used when the model is not a CLI spec.
    pub default_kind: Option<CliToolKind>,
    /// Directories searched for the tool; `None` uses [`default_search_dirs`].
    pub search_dirs: Option<Vec<PathBuf>>,
    /// An exact command that replaces catalogue resolution entirely.
    pub command_override: Option<CliCommand>,
    /// How often the child is polled for exit.
    pub poll_interval: Duration,
    /// Pin Antigravity to Argon High and reject conflicting model selections.
    pub antigravity_argon_only: bool,
}

impl Default for CliSubRunner {
    fn default() -> Self {
        Self {
            default_kind: None,
            search_dirs: None,
            command_override: None,
            poll_interval: DEFAULT_POLL_INTERVAL,
            antigravity_argon_only: false,
        }
    }
}

impl CliSubRunner {
    /// A runner that defaults to `kind` when the model is not a CLI spec.
    #[must_use]
    pub fn with_default_kind(kind: CliToolKind) -> Self {
        Self {
            default_kind: Some(kind),
            ..Self::default()
        }
    }

    /// A runner that always executes exactly `command`.
    #[must_use]
    pub fn with_command(command: CliCommand) -> Self {
        Self {
            command_override: Some(command),
            ..Self::default()
        }
    }

    /// A runner that searches only `dirs` for its tools.
    #[must_use]
    pub fn with_search_dirs(mut self, dirs: Vec<PathBuf>) -> Self {
        self.search_dirs = Some(dirs);
        self
    }

    /// The command this runner would execute for `model`, or the reason it
    /// cannot run one.
    ///
    /// # Errors
    ///
    /// Returns a human-readable reason when no tool is named, configured or
    /// installed.
    pub fn resolve(&self, model: &str) -> Result<Vec<CliCommand>, String> {
        if let Some(command) = &self.command_override {
            if self.antigravity_argon_only {
                return Err("Argon-only policy refuses an unverified command override".into());
            }
            return Ok(vec![command.clone()]);
        }
        let dirs = self.search_dirs.clone().unwrap_or_else(default_search_dirs);
        let (kind, submodel) = match CliToolKind::parse_spec(model) {
            Some(spec) => spec,
            None => match self.default_kind {
                Some(kind) => (kind, None),
                None => match discover_cli_tools_in(&dirs).into_iter().next() {
                    Some(tool) => (tool.kind, None),
                    None => {
                        return Err(format!(
                            "no subscription CLI tool for model '{model}': none is configured \
                             and none of {} is installed",
                            CliToolKind::ALL
                                .iter()
                                .map(CliToolKind::as_str)
                                .collect::<Vec<_>>()
                                .join(", ")
                        ))
                    }
                },
            },
        };
        let submodel = if self.antigravity_argon_only && kind == CliToolKind::Antigravity {
            Some(argon_model(submodel.as_deref())?.to_string())
        } else {
            submodel
        };
        let (candidates, args, pipe_prompt_on_stdin) = build_invocation(kind, submodel.as_deref());
        let command = |program: PathBuf| CliCommand {
            program,
            args: args.clone(),
            pipe_prompt_on_stdin,
        };
        if let Some(path) = find_in_dirs(kind, &dirs) {
            return Ok(vec![command(path)]);
        }
        // An explicit search list is authoritative: a tool absent from it is
        // not installed, so the turn fails fast instead of asking `PATH`.
        if self.search_dirs.is_some() {
            return Err(format!(
                "subscription CLI '{}' is not installed (looked for {})",
                kind.as_str(),
                candidates.join(", ")
            ));
        }
        Ok(candidates
            .into_iter()
            .map(|name| command(PathBuf::from(name)))
            .collect())
    }
}

impl Runner for CliSubRunner {
    fn run(&self, config: &ExecutorConfig) -> EaiResult<RunOutcome> {
        let commands = match self.resolve(&config.model) {
            Ok(commands) => commands,
            Err(message) => return Ok(RunOutcome::Failed { message }),
        };
        let mut not_found = Vec::new();
        for command in &commands {
            match run_piped(
                command,
                &config.workspace,
                &config.prompt,
                config.timeout,
                self.poll_interval,
            ) {
                PipedRun::Finished(outcome) => return Ok(outcome),
                PipedRun::NotFound => not_found.push(command.program.display().to_string()),
            }
        }
        Ok(RunOutcome::Failed {
            message: format!(
                "subscription CLI is not installed (tried {})",
                not_found.join(", ")
            ),
        })
    }
}

/// How one [`run_piped`] call ended.
#[derive(Debug, Clone, PartialEq)]
pub enum PipedRun {
    /// The program ran (or could not be spawned for a reason other than
    /// absence); the turn's verdict.
    Finished(RunOutcome),
    /// The program does not exist, so the next candidate may be tried.
    NotFound,
}

/// Spawn `invocation` in `workspace`, pipe `prompt` on stdin when asked, drain
/// stdout and stderr concurrently and wait at most `timeout`.
///
/// Exit 0 is [`RunOutcome::Succeeded`] with the trimmed stdout as final text
/// and `$0.00` reported PAYG cost; a non-zero exit is [`RunOutcome::Failed`]
/// quoting the output's tail; the deadline is [`RunOutcome::TimedOut`], after
/// the child has been killed and reaped.
#[must_use]
pub fn run_piped(
    invocation: &CliCommand,
    workspace: &Path,
    prompt: &str,
    timeout: Duration,
    poll_interval: Duration,
) -> PipedRun {
    let program = invocation.program.as_path();
    let args = invocation.args.as_slice();
    let pipe_prompt_on_stdin = invocation.pipe_prompt_on_stdin;
    // A headless agent with every permission granted must never fall back to
    // whatever directory the harness happens to be running in.
    if !workspace.is_dir() {
        return PipedRun::Finished(RunOutcome::Failed {
            message: format!(
                "workspace {} is not a directory; refusing to run {}",
                workspace.display(),
                program.display()
            ),
        });
    }
    let mut command = susi_paths::console::background_command(program);
    command
        .args(args)
        .stdin(if pipe_prompt_on_stdin {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .current_dir(workspace);
    for (k, v) in susi_config::cloud_env_overlay() {
        if std::env::var_os(&k).is_none() {
            command.env(k, v);
        }
    }
    susi_paths::console::apply_non_interactive_env(&mut command);
    command.env("SUSI_UNATTENDED", "1");
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) if error.kind() == ErrorKind::NotFound => return PipedRun::NotFound,
        Err(error) => {
            return PipedRun::Finished(RunOutcome::Failed {
                message: format!("spawn {}: {error}", program.display()),
            })
        }
    };

    if let Some(mut stdin) = child.stdin.take() {
        let prompt = prompt.to_string();
        // The writer owns stdin and drops it when done, which closes the pipe
        // so the child sees end-of-input. A child that exits without reading
        // breaks the pipe; that is its choice, not a harness fault.
        thread::spawn(move || {
            let _ = stdin.write_all(prompt.as_bytes());
            let _ = stdin.flush();
        });
    }
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => {
                reap(&mut child);
                return PipedRun::Finished(RunOutcome::Failed {
                    message: format!("wait {}: {error}", program.display()),
                });
            }
        }
        let now = Instant::now();
        if now >= deadline {
            // Readers are not joined after a kill: a grandchild can inherit
            // the pipe and hold it open long after the child is gone.
            reap(&mut child);
            return PipedRun::Finished(RunOutcome::TimedOut);
        }
        thread::sleep(poll_interval.min(deadline.saturating_duration_since(now)));
    };
    // A grandchild that inherited the pipes can hold them open after the
    // child exits; whatever arrived within the grace window is kept.
    let grace = Instant::now() + DRAIN_GRACE;
    let stdout = stdout.collect(grace);
    let stderr = stderr.collect(grace);
    PipedRun::Finished(verdict(program, status, &stdout, &stderr))
}

fn verdict(program: &Path, status: ExitStatus, stdout: &str, stderr: &str) -> RunOutcome {
    if status.success() {
        return RunOutcome::Succeeded {
            final_text: stdout.trim().to_string(),
            reported_cost_usd: Some(0.0),
        };
    }
    let code = status
        .code()
        .map_or_else(|| "a signal".to_string(), |code| format!("code {code}"));
    let mut message = format!("{} exited with {code}", program.display());
    for (label, text) in [("stderr", stderr), ("stdout", stdout)] {
        let text = text.trim();
        if !text.is_empty() {
            message.push_str(&format!("; {label}: {}", tail(text)));
        }
    }
    if is_subscription_quota_exhausted(&message) {
        message.push_str(" [subscription quota exhausted]");
    }
    RunOutcome::Failed { message }
}

fn tail(text: &str) -> &str {
    let count = text.chars().count();
    if count <= FAILURE_TAIL_CHARS {
        return text;
    }
    let skip = count.saturating_sub(FAILURE_TAIL_CHARS);
    text.char_indices()
        .nth(skip)
        .map_or(text, |(index, _)| &text[index..])
}

fn reap(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Output read from one child stream so far, appended as it arrives.
struct Capture {
    bytes: Arc<Mutex<Vec<u8>>>,
    done: mpsc::Receiver<()>,
}

impl Capture {
    /// Wait until the stream ends or `grace` passes, then snapshot what was
    /// read. A stream still held open by a grandchild yields its partial
    /// output instead of nothing.
    fn collect(&self, grace: Instant) -> String {
        let _ = self
            .done
            .recv_timeout(grace.saturating_duration_since(Instant::now()));
        let bytes = self
            .bytes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

fn drain<R: Read + Send + 'static>(stream: Option<R>) -> Capture {
    let (sender, done) = mpsc::channel();
    let bytes = Arc::new(Mutex::new(Vec::new()));
    match stream {
        Some(mut stream) => {
            let sink = Arc::clone(&bytes);
            thread::spawn(move || {
                let mut buffer = [0_u8; 8 * 1024];
                loop {
                    match stream.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(read) => {
                            let mut captured = sink
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            let room = MAX_CAPTURE_BYTES.saturating_sub(captured.len());
                            captured.extend_from_slice(&buffer[..read.min(room)]);
                        }
                        Err(error) if error.kind() == ErrorKind::Interrupted => {}
                        Err(_) => break,
                    }
                }
                let _ = sender.send(());
            });
        }
        None => {
            let _ = sender.send(());
        }
    }
    Capture { bytes, done }
}

/// The route a turn takes: a primary plane and, optionally, a failover.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdaptivePlan {
    /// The plane tried first, with the model it is given.
    pub primary: (ExecutionMode, String),
    /// The plane tried when the primary fails, with the model it is given.
    pub failover: Option<(ExecutionMode, String)>,
}

/// Runs each turn on PAYG BYOK or a subscription CLI under an
/// [`AdaptiveStrategy`], failing over between them.
///
/// A CLI turn fails over on [`RunOutcome::Failed`] or [`RunOutcome::TimedOut`]
/// (quota spent, tool missing, tool hung); a PAYG turn fails over on
/// [`RunOutcome::Failed`] (key refused, credit spent, provider down). An
/// operational [`susi_error::EaiError`] is never masked by a failover.
#[derive(Debug, Clone)]
pub struct AdaptiveRunner<P: Runner = NativeWorker, C: Runner = CliSubRunner> {
    /// The PAYG BYOK runner.
    pub payg_runner: P,
    /// The subscription CLI runner.
    pub cli_runner: C,
    /// Ordering and failover policy.
    pub strategy: AdaptiveStrategy,
    /// The CLI spec PAYG fails over to, e.g. `cli:claude`.
    pub fallback_cli_model: Option<String>,
    /// The PAYG model a `cli:` turn fails over to, e.g. `deepseek-flash`.
    pub fallback_payg_model: Option<String>,
    /// Restrict Antigravity routes to Argon High, with no PAYG rerouting.
    pub antigravity_argon_only: bool,
}

impl Default for AdaptiveRunner<NativeWorker, CliSubRunner> {
    fn default() -> Self {
        Self::new(
            NativeWorker,
            CliSubRunner::default(),
            AdaptiveStrategy::Auto,
        )
    }
}

impl AdaptiveRunner<NativeWorker, CliSubRunner> {
    /// The production runner configured from the environment:
    /// [`ADAPTIVE_ENV`], [`FALLBACK_CLI_ENV`], [`FALLBACK_PAYG_ENV`] and
    /// the process-scoped [`ANTIGRAVITY_ARGON_ONLY_ENV`] opt-in.
    #[must_use]
    pub fn from_env() -> Self {
        let read = |name: &str| {
            std::env::var(name)
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        };
        let argon_only = std::env::var_os(ANTIGRAVITY_ARGON_ONLY_ENV).is_some();
        Self {
            antigravity_argon_only: argon_only,
            cli_runner: CliSubRunner {
                antigravity_argon_only: argon_only,
                ..CliSubRunner::default()
            },
            fallback_cli_model: read(FALLBACK_CLI_ENV),
            fallback_payg_model: read(FALLBACK_PAYG_ENV),
            ..Self::new(
                NativeWorker,
                CliSubRunner::default(),
                AdaptiveStrategy::from_env(),
            )
        }
    }
}

impl<P: Runner> AdaptiveRunner<P, CliSubRunner> {
    /// Enable Argon-only routing and CLI validation together. This builder is
    /// intentionally limited to the concrete CLI runner: a generic runner may
    /// ignore model selection or execute an unrelated command override.
    #[must_use]
    pub fn with_antigravity_argon_only(mut self) -> Self {
        self.antigravity_argon_only = true;
        self.cli_runner.antigravity_argon_only = true;
        self
    }
}

impl<P: Runner, C: Runner> AdaptiveRunner<P, C> {
    /// A runner over the two planes with no fallback models.
    pub fn new(payg_runner: P, cli_runner: C, strategy: AdaptiveStrategy) -> Self {
        Self {
            payg_runner,
            cli_runner,
            strategy,
            fallback_cli_model: None,
            fallback_payg_model: None,
            antigravity_argon_only: false,
        }
    }

    /// Set the CLI spec PAYG fails over to.
    #[must_use]
    pub fn with_fallback_cli_model(mut self, model: impl Into<String>) -> Self {
        self.fallback_cli_model = Some(model.into());
        self
    }

    /// Set the PAYG model a CLI turn fails over to.
    #[must_use]
    pub fn with_fallback_payg_model(mut self, model: impl Into<String>) -> Self {
        self.fallback_payg_model = Some(model.into());
        self
    }

    /// The route a turn for `model` takes under this runner's strategy.
    ///
    /// A PAYG plane is never handed a `cli:` spec: when the turn's model is
    /// one, PAYG runs [`AdaptiveRunner::fallback_payg_model`] or not at all.
    /// A CLI failover runs [`AdaptiveRunner::fallback_cli_model`] when set,
    /// otherwise the turn's own model (resolved by the CLI runner's default).
    #[must_use]
    pub fn plan(&self, model: &str) -> AdaptivePlan {
        let is_cli_spec = CliToolKind::parse_spec(model).is_some();
        let cli = |model: &str| (ExecutionMode::MonthlySubCli, model.to_string());
        let payg = |model: &str| (ExecutionMode::PaygByok, model.to_string());
        let cli_failover = || cli(self.fallback_cli_model.as_deref().unwrap_or(model));
        // The PAYG model for this turn: the model itself unless it is a CLI
        // spec, in which case only a configured fallback can run on PAYG.
        let payg_model = if is_cli_spec {
            self.fallback_payg_model.as_deref()
        } else {
            Some(model)
        };
        let only = |primary| AdaptivePlan {
            primary,
            failover: None,
        };
        // A restricted Antigravity request cannot be silently converted to a
        // PAYG request, even by payg-first/payg-only or an installed fallback.
        if self.antigravity_argon_only
            && matches!(
                CliToolKind::parse_spec(model),
                Some((CliToolKind::Antigravity, _))
            )
        {
            return only(cli(model));
        }
        match self.strategy {
            AdaptiveStrategy::PaygOnly => only(payg(payg_model.unwrap_or(model))),
            AdaptiveStrategy::CliSubOnly => only(cli(model)),
            AdaptiveStrategy::PreferCliSubThenPayg => AdaptivePlan {
                primary: cli(model),
                failover: payg_model.map(payg),
            },
            AdaptiveStrategy::PreferPaygThenCliSub => match payg_model {
                Some(payg_model) => AdaptivePlan {
                    primary: payg(payg_model),
                    failover: Some(if is_cli_spec {
                        cli(model)
                    } else {
                        cli_failover()
                    }),
                },
                None => only(cli(model)),
            },
            AdaptiveStrategy::Auto => {
                if is_cli_spec {
                    AdaptivePlan {
                        primary: cli(model),
                        failover: payg_model.map(payg),
                    }
                } else {
                    AdaptivePlan {
                        primary: payg(model),
                        failover: self.fallback_cli_model.as_deref().map(cli),
                    }
                }
            }
        }
    }

    fn run_on(&self, mode: ExecutionMode, config: &ExecutorConfig) -> EaiResult<RunOutcome> {
        match mode {
            ExecutionMode::PaygByok => self.payg_runner.run(config),
            ExecutionMode::MonthlySubCli => {
                if self.antigravity_argon_only {
                    if let Some((CliToolKind::Antigravity, model)) =
                        CliToolKind::parse_spec(&config.model)
                    {
                        let model = match argon_model(model.as_deref()) {
                            Ok(model) => model,
                            Err(message) => return Ok(RunOutcome::Failed { message }),
                        };
                        return self
                            .cli_runner
                            .run(&with_model(config, &format!("cli:antigravity:{model}")));
                    }
                }
                self.cli_runner.run(config)
            }
        }
    }
}

fn should_fail_over(mode: ExecutionMode, outcome: &RunOutcome) -> bool {
    match (mode, outcome) {
        (_, RunOutcome::Succeeded { .. }) => false,
        (ExecutionMode::MonthlySubCli, RunOutcome::Failed { .. } | RunOutcome::TimedOut) => true,
        (ExecutionMode::PaygByok, RunOutcome::Failed { .. }) => true,
        (ExecutionMode::PaygByok, RunOutcome::TimedOut) => false,
    }
}

fn describe(outcome: &RunOutcome) -> String {
    match outcome {
        RunOutcome::Succeeded { .. } => "succeeded".to_string(),
        RunOutcome::Failed { message } => message.clone(),
        RunOutcome::TimedOut => "timed out".to_string(),
    }
}

impl<P: Runner, C: Runner> Runner for AdaptiveRunner<P, C> {
    fn run(&self, config: &ExecutorConfig) -> EaiResult<RunOutcome> {
        let plan = self.plan(&config.model);
        let (primary_mode, primary_model) = plan.primary;
        let primary_config = with_model(config, &primary_model);
        let primary = self.run_on(primary_mode, &primary_config)?;
        let Some((failover_mode, failover_model)) = plan.failover else {
            return Ok(primary);
        };
        if !should_fail_over(primary_mode, &primary) {
            return Ok(primary);
        }
        let secondary = self.run_on(failover_mode, &with_model(config, &failover_model))?;
        Ok(match secondary {
            RunOutcome::Failed { message } => RunOutcome::Failed {
                message: format!(
                    "{} ({primary_model}) failed: {}; failover {} ({failover_model}) failed: \
                     {message}",
                    primary_mode.as_str(),
                    describe(&primary),
                    failover_mode.as_str(),
                ),
            },
            other @ (RunOutcome::Succeeded { .. } | RunOutcome::TimedOut) => other,
        })
    }
}

fn with_model(config: &ExecutorConfig, model: &str) -> ExecutorConfig {
    let mut config = config.clone();
    model.clone_into(&mut config.model);
    config
}

/// The Susi AI OS Socket endpoints the harness binds onto.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SusiSocketEndpoints {
    /// The GMCP streamable-HTTP tool socket.
    pub gmcp_http_url: String,
    /// The Gemi OpenAI-compatible chat-completions socket.
    pub gemi_openai_url: String,
    /// The Gawd agent-to-agent socket.
    pub gawd_a2a_url: String,
}

/// Default GMCP HTTP port.
pub const GMCP_HTTP_PORT: u16 = 9090;
/// Default Gemi OpenAI-compatible port.
pub const GEMI_OPENAI_PORT: u16 = 9091;
/// Default Gawd A2A port.
pub const GAWD_A2A_PORT: u16 = 9094;

impl Default for SusiSocketEndpoints {
    fn default() -> Self {
        Self::from_port_offset(0)
    }
}

impl SusiSocketEndpoints {
    /// The loopback endpoints with every port shifted by `offset`, so parallel
    /// Susi instances bind without colliding. Ports saturate at `u16::MAX`.
    #[must_use]
    pub fn from_port_offset(offset: u16) -> Self {
        Self {
            gmcp_http_url: format!("http://127.0.0.1:{}", GMCP_HTTP_PORT.saturating_add(offset)),
            gemi_openai_url: format!(
                "http://127.0.0.1:{}/v1/chat/completions",
                GEMI_OPENAI_PORT.saturating_add(offset)
            ),
            gawd_a2a_url: format!("http://127.0.0.1:{}", GAWD_A2A_PORT.saturating_add(offset)),
        }
    }
}

#[cfg(test)]
#[path = "tests/adaptive.rs"]
mod tests;
