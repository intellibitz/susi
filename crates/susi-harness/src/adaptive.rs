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
use std::process::{Child, Command, ExitStatus, Stdio};
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
                owned(&["exec", "--full-auto", "-"]),
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
}

impl Default for CliSubRunner {
    fn default() -> Self {
        Self {
            default_kind: None,
            search_dirs: None,
            command_override: None,
            poll_interval: DEFAULT_POLL_INTERVAL,
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
    let mut command = Command::new(program);
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
    /// [`ADAPTIVE_ENV`], [`FALLBACK_CLI_ENV`] and [`FALLBACK_PAYG_ENV`].
    #[must_use]
    pub fn from_env() -> Self {
        let read = |name: &str| {
            std::env::var(name)
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        };
        Self {
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

impl<P: Runner, C: Runner> AdaptiveRunner<P, C> {
    /// A runner over the two planes with no fallback models.
    pub fn new(payg_runner: P, cli_runner: C, strategy: AdaptiveStrategy) -> Self {
        Self {
            payg_runner,
            cli_runner,
            strategy,
            fallback_cli_model: None,
            fallback_payg_model: None,
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
            ExecutionMode::MonthlySubCli => self.cli_runner.run(config),
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
mod tests {
    use super::*;

    fn config(model: &str) -> ExecutorConfig {
        ExecutorConfig {
            gateway_url: "http://127.0.0.1:1".into(),
            model: model.into(),
            workspace: std::env::temp_dir(),
            state_dir: std::env::temp_dir().join("susi-adaptive-test-state"),
            mission: "adaptive".into(),
            task: "route-1".into(),
            attempt: 1,
            timeout: Duration::from_secs(30),
            prompt: "say OK".into(),
        }
    }

    /// A runner that records the model of every turn it is handed and answers
    /// with a fixed outcome.
    struct Scripted {
        outcome: RunOutcome,
        seen: Mutex<Vec<String>>,
    }

    impl Scripted {
        fn new(outcome: RunOutcome) -> Self {
            Self {
                outcome,
                seen: Mutex::new(Vec::new()),
            }
        }

        fn ok(text: &str) -> Self {
            Self::new(RunOutcome::Succeeded {
                final_text: text.into(),
                reported_cost_usd: None,
            })
        }

        fn failed(message: &str) -> Self {
            Self::new(RunOutcome::Failed {
                message: message.into(),
            })
        }

        fn seen(&self) -> Vec<String> {
            self.seen.lock().unwrap().clone()
        }
    }

    impl Runner for Scripted {
        fn run(&self, config: &ExecutorConfig) -> EaiResult<RunOutcome> {
            self.seen.lock().unwrap().push(config.model.clone());
            Ok(self.outcome.clone())
        }
    }

    fn text(outcome: &RunOutcome) -> &str {
        match outcome {
            RunOutcome::Succeeded { final_text, .. } => final_text,
            other => panic!("expected success, got {other:?}"),
        }
    }

    fn system32(program: &str) -> PathBuf {
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        PathBuf::from(root).join("System32").join(program)
    }

    #[test]
    fn strategies_parse_from_their_env_names_and_default_to_auto() {
        assert_eq!(
            AdaptiveStrategy::parse("auto"),
            Some(AdaptiveStrategy::Auto)
        );
        assert_eq!(
            AdaptiveStrategy::parse(" CLI-first "),
            Some(AdaptiveStrategy::PreferCliSubThenPayg)
        );
        assert_eq!(
            AdaptiveStrategy::parse("payg_first"),
            Some(AdaptiveStrategy::PreferPaygThenCliSub)
        );
        assert_eq!(
            AdaptiveStrategy::parse("payg-only"),
            Some(AdaptiveStrategy::PaygOnly)
        );
        assert_eq!(
            AdaptiveStrategy::parse("cli-only"),
            Some(AdaptiveStrategy::CliSubOnly)
        );
        assert_eq!(AdaptiveStrategy::parse("sideways"), None);
        assert_eq!(AdaptiveStrategy::default(), AdaptiveStrategy::Auto);
        for strategy in [
            AdaptiveStrategy::Auto,
            AdaptiveStrategy::PreferCliSubThenPayg,
            AdaptiveStrategy::PreferPaygThenCliSub,
            AdaptiveStrategy::PaygOnly,
            AdaptiveStrategy::CliSubOnly,
        ] {
            assert_eq!(AdaptiveStrategy::parse(strategy.as_str()), Some(strategy));
        }
    }

    #[test]
    fn cli_specs_parse_with_prefix_tool_and_optional_submodel() {
        let cases = [
            ("cli:claude", CliToolKind::Claude, None),
            ("cli:claude:opus", CliToolKind::Claude, Some("opus")),
            ("sub:codex", CliToolKind::Codex, None),
            ("cli:gemini", CliToolKind::Gemini, None),
            ("cli:agy", CliToolKind::Antigravity, None),
            ("cli:qwen", CliToolKind::Qwen, None),
            ("cli:aider", CliToolKind::Aider, None),
            ("cli:opencode", CliToolKind::OpenCode, None),
            (
                "SUB:Gemini:gemini-2.5-pro",
                CliToolKind::Gemini,
                Some("gemini-2.5-pro"),
            ),
        ];
        for (spec, kind, submodel) in cases {
            assert_eq!(
                CliToolKind::parse_spec(spec),
                Some((kind, submodel.map(str::to_string))),
                "{spec}"
            );
        }
        for not_cli in ["deepseek-flash", "openai:gpt-5", "cli:", "cli:vim", ""] {
            assert_eq!(CliToolKind::parse_spec(not_cli), None, "{not_cli}");
        }
        assert_eq!(
            CliToolKind::parse_spec("cli:claude:"),
            Some((CliToolKind::Claude, None))
        );
        for kind in CliToolKind::ALL {
            assert_eq!(CliToolKind::from_name(kind.as_str()), Some(kind));
        }
    }

    #[test]
    fn invocations_pipe_the_prompt_on_stdin_for_every_tool() {
        let strings = |args: &[&str]| args.iter().map(|a| (*a).to_string()).collect::<Vec<_>>();

        let (programs, args, pipe) = build_invocation(CliToolKind::Claude, None);
        assert_eq!(programs, ["claude", "claude.exe"]);
        assert_eq!(
            args,
            strings(&["--dangerously-skip-permissions", "-p", STDIN_PROMPT])
        );
        assert!(pipe);
        let (_, args, _) = build_invocation(CliToolKind::Claude, Some("opus"));
        assert_eq!(
            args,
            strings(&[
                "--dangerously-skip-permissions",
                "-p",
                STDIN_PROMPT,
                "--model",
                "opus"
            ])
        );

        let (programs, args, _) = build_invocation(CliToolKind::Codex, None);
        assert_eq!(programs, ["codex", "codex.exe"]);
        assert_eq!(args, strings(&["exec", "--full-auto", "-"]));
        let (_, args, _) = build_invocation(CliToolKind::Codex, Some("o3"));
        assert_eq!(args, strings(&["exec", "--full-auto", "-", "-m", "o3"]));

        let (programs, args, _) = build_invocation(CliToolKind::Gemini, Some("flash"));
        assert_eq!(programs, ["gemini", "gemini.cmd", "gemini.exe"]);
        assert_eq!(
            args,
            strings(&["--yolo", "-p", STDIN_PROMPT, "-m", "flash"])
        );

        let (programs, args, _) =
            build_invocation(CliToolKind::Antigravity, Some("gemini-4-argon-high"));
        assert_eq!(programs, ["agy", "agy.exe", "antigravity"]);
        assert_eq!(
            args,
            strings(&[
                "--sandbox",
                "--print-timeout",
                "0",
                "--print",
                STDIN_PROMPT,
                "--model",
                "gemini-4-argon-high"
            ])
        );

        let (programs, args, _) = build_invocation(CliToolKind::Qwen, None);
        assert_eq!(programs, ["qwen", "qwen.cmd", "qwen.exe"]);
        assert_eq!(args, strings(&["--yolo", "-p", STDIN_PROMPT]));

        let (programs, args, _) = build_invocation(CliToolKind::Aider, None);
        assert_eq!(programs, ["aider", "aider.exe"]);
        assert_eq!(
            args,
            strings(&["--yes-always", "--no-auto-commits", "--message-file", "-"])
        );

        let (programs, args, _) = build_invocation(CliToolKind::OpenCode, None);
        assert_eq!(programs, ["opencode", "opencode.exe"]);
        assert_eq!(args, strings(&["run"]));

        for kind in CliToolKind::ALL {
            assert!(build_invocation(kind, None).2, "{kind:?} must pipe stdin");
        }
    }

    #[test]
    fn discovery_finds_mock_binaries_and_the_first_directory_wins() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        std::fs::write(first.path().join("claude.exe"), b"").unwrap();
        std::fs::write(first.path().join("gemini.cmd"), b"").unwrap();
        std::fs::write(second.path().join("claude"), b"").unwrap();
        std::fs::write(second.path().join("agy"), b"").unwrap();
        // A directory named like a tool is not a tool.
        std::fs::create_dir(second.path().join("codex")).unwrap();

        let dirs = vec![first.path().to_path_buf(), second.path().to_path_buf()];
        let found = discover_cli_tools_in(&dirs);
        let kinds: Vec<CliToolKind> = found.iter().map(|tool| tool.kind).collect();
        assert_eq!(
            kinds,
            [
                CliToolKind::Claude,
                CliToolKind::Gemini,
                CliToolKind::Antigravity
            ]
        );
        assert_eq!(found[0].path, first.path().join("claude.exe"));
        assert_eq!(found[1].path, first.path().join("gemini.cmd"));
        assert_eq!(found[2].path, second.path().join("agy"));
        assert!(discover_cli_tools_in(&[]).is_empty());
        assert!(!default_search_dirs().is_empty());
        // The live scan never panics, whatever this machine has installed.
        let _ = discover_cli_tools();
    }

    #[test]
    fn antigravity_resolves_the_requested_model_without_permission_bypass() {
        let installed = tempfile::tempdir().unwrap();
        std::fs::write(installed.path().join("agy.exe"), b"").unwrap();
        let runner = CliSubRunner::default().with_search_dirs(vec![installed.path().to_path_buf()]);
        let resolved = runner
            .resolve("cli:antigravity:gemini-4-argon-high")
            .unwrap();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].program, installed.path().join("agy.exe"));
        assert!(resolved[0]
            .args
            .windows(2)
            .any(|pair| pair == ["--model", "gemini-4-argon-high"]));
        assert!(resolved[0]
            .args
            .windows(2)
            .any(|pair| pair == ["--print-timeout", "0"]));
        assert!(resolved[0].args.iter().any(|arg| arg == "--sandbox"));
        assert!(!resolved[0]
            .args
            .iter()
            .any(|arg| arg == "--yolo" || arg == "--dangerously-skip-permissions"));
        // The unspecified case does not invent or override the user's model.
        let (_, args, _) = build_invocation(CliToolKind::Antigravity, None);
        assert!(!args.iter().any(|arg| arg == "--model"));
    }

    #[test]
    fn default_discovery_includes_antigravity_local_installation() {
        // Read-only environment inspection avoids racing parallel tests.
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            let expected = PathBuf::from(local).join("agy").join("bin");
            assert!(default_search_dirs().contains(&expected));
        }
    }

    #[test]
    fn quota_and_credit_classifiers_recognise_provider_refusals() {
        for quota in [
            "Claude AI usage limit reached|1730000000",
            "You have hit your limit - resets at 5pm",
            "429 Too Many Requests",
            "Rate limit exceeded for this session",
            "Quota exceeded for quota metric",
        ] {
            assert!(is_subscription_quota_exhausted(quota), "{quota}");
        }
        assert!(!is_subscription_quota_exhausted("compiled 12 crates"));

        for credit in [
            "{\"error\":{\"code\":\"insufficient_quota\"}}",
            "Insufficient Balance",
            "HTTP 402 Payment Required",
            "401 Unauthorized: invalid API key",
            "none of the configured providers accepted the request",
            "check your billing details",
            "no credit remaining",
        ] {
            assert!(is_payg_credit_or_key_exhausted(credit), "{credit}");
        }
        assert!(!is_payg_credit_or_key_exhausted("all tests passed"));
    }

    #[test]
    fn payg_only_and_cli_only_never_touch_the_other_plane() {
        let runner = AdaptiveRunner::new(
            Scripted::failed("insufficient_quota"),
            Scripted::ok("cli"),
            AdaptiveStrategy::PaygOnly,
        )
        .with_fallback_cli_model("cli:claude");
        let outcome = runner.run(&config("deepseek-flash")).unwrap();
        assert!(matches!(outcome, RunOutcome::Failed { .. }));
        assert!(runner.cli_runner.seen().is_empty());

        let runner = AdaptiveRunner::new(
            Scripted::ok("payg"),
            Scripted::failed("usage limit reached"),
            AdaptiveStrategy::CliSubOnly,
        )
        .with_fallback_payg_model("deepseek-flash");
        let outcome = runner.run(&config("cli:claude")).unwrap();
        assert!(matches!(outcome, RunOutcome::Failed { .. }));
        assert!(runner.payg_runner.seen().is_empty());
        assert_eq!(runner.cli_runner.seen(), ["cli:claude"]);
    }

    #[test]
    fn cli_first_fails_over_to_payg_on_quota_exhaustion_with_the_fallback_model() {
        let runner = AdaptiveRunner::new(
            Scripted::ok("from payg"),
            Scripted::failed("Claude AI usage limit reached; resets at 5pm"),
            AdaptiveStrategy::PreferCliSubThenPayg,
        )
        .with_fallback_payg_model("deepseek-flash");
        let outcome = runner.run(&config("cli:claude:opus")).unwrap();
        assert_eq!(text(&outcome), "from payg");
        assert_eq!(runner.cli_runner.seen(), ["cli:claude:opus"]);
        assert_eq!(runner.payg_runner.seen(), ["deepseek-flash"]);
    }

    #[test]
    fn cli_first_fails_over_on_timeout_and_keeps_a_payg_model_as_is() {
        let runner = AdaptiveRunner::new(
            Scripted::ok("from payg"),
            Scripted::new(RunOutcome::TimedOut),
            AdaptiveStrategy::PreferCliSubThenPayg,
        );
        let outcome = runner.run(&config("glm-4.6")).unwrap();
        assert_eq!(text(&outcome), "from payg");
        assert_eq!(runner.payg_runner.seen(), ["glm-4.6"]);

        // A cli: spec with no PAYG fallback has nowhere to go.
        let runner = AdaptiveRunner::new(
            Scripted::ok("from payg"),
            Scripted::new(RunOutcome::TimedOut),
            AdaptiveStrategy::PreferCliSubThenPayg,
        );
        assert_eq!(
            runner.run(&config("cli:codex")).unwrap(),
            RunOutcome::TimedOut
        );
        assert!(runner.payg_runner.seen().is_empty());
    }

    #[test]
    fn cli_first_success_never_spends_payg_money() {
        let runner = AdaptiveRunner::new(
            Scripted::ok("from payg"),
            Scripted::ok("from cli"),
            AdaptiveStrategy::PreferCliSubThenPayg,
        )
        .with_fallback_payg_model("deepseek-flash");
        assert_eq!(
            text(&runner.run(&config("cli:claude")).unwrap()),
            "from cli"
        );
        assert!(runner.payg_runner.seen().is_empty());
    }

    #[test]
    fn payg_first_fails_over_to_the_cli_on_a_credit_refusal() {
        let runner = AdaptiveRunner::new(
            Scripted::failed("HTTP 402: insufficient balance"),
            Scripted::ok("from cli"),
            AdaptiveStrategy::PreferPaygThenCliSub,
        )
        .with_fallback_cli_model("cli:codex");
        let outcome = runner.run(&config("deepseek-flash")).unwrap();
        assert_eq!(text(&outcome), "from cli");
        assert_eq!(runner.payg_runner.seen(), ["deepseek-flash"]);
        assert_eq!(runner.cli_runner.seen(), ["cli:codex"]);
    }

    #[test]
    fn payg_first_does_not_fail_over_on_a_payg_timeout() {
        let runner = AdaptiveRunner::new(
            Scripted::new(RunOutcome::TimedOut),
            Scripted::ok("from cli"),
            AdaptiveStrategy::PreferPaygThenCliSub,
        )
        .with_fallback_cli_model("cli:codex");
        assert_eq!(
            runner.run(&config("deepseek-flash")).unwrap(),
            RunOutcome::TimedOut
        );
        assert!(runner.cli_runner.seen().is_empty());
    }

    #[test]
    fn payg_first_with_a_cli_spec_runs_payg_only_through_the_fallback_model() {
        let runner = AdaptiveRunner::new(
            Scripted::failed("401 invalid api key"),
            Scripted::ok("from cli"),
            AdaptiveStrategy::PreferPaygThenCliSub,
        )
        .with_fallback_payg_model("deepseek-flash");
        assert_eq!(
            text(&runner.run(&config("cli:gemini")).unwrap()),
            "from cli"
        );
        assert_eq!(runner.payg_runner.seen(), ["deepseek-flash"]);
        assert_eq!(runner.cli_runner.seen(), ["cli:gemini"]);

        let runner = AdaptiveRunner::new(
            Scripted::ok("from payg"),
            Scripted::ok("from cli"),
            AdaptiveStrategy::PreferPaygThenCliSub,
        );
        assert_eq!(
            text(&runner.run(&config("cli:gemini")).unwrap()),
            "from cli"
        );
        assert!(runner.payg_runner.seen().is_empty());
    }

    #[test]
    fn auto_routes_by_model_spec_and_fails_over_only_when_configured() {
        // A cli: spec runs on the CLI and fails over to the PAYG fallback.
        let runner = AdaptiveRunner::new(
            Scripted::ok("from payg"),
            Scripted::failed("You have hit your limit"),
            AdaptiveStrategy::Auto,
        )
        .with_fallback_payg_model("deepseek-flash");
        assert_eq!(
            text(&runner.run(&config("cli:claude")).unwrap()),
            "from payg"
        );
        assert_eq!(runner.payg_runner.seen(), ["deepseek-flash"]);

        // Without the fallback the CLI verdict stands unchanged.
        let runner = AdaptiveRunner::new(
            Scripted::ok("from payg"),
            Scripted::failed("You have hit your limit"),
            AdaptiveStrategy::Auto,
        );
        assert_eq!(
            runner.run(&config("cli:claude")).unwrap(),
            RunOutcome::Failed {
                message: "You have hit your limit".into()
            }
        );
        assert!(runner.payg_runner.seen().is_empty());

        // A PAYG id runs on PAYG and fails over to the CLI fallback.
        let runner = AdaptiveRunner::new(
            Scripted::failed("insufficient_quota"),
            Scripted::ok("from cli"),
            AdaptiveStrategy::Auto,
        )
        .with_fallback_cli_model("cli:claude:sonnet");
        assert_eq!(
            text(&runner.run(&config("deepseek-flash")).unwrap()),
            "from cli"
        );
        assert_eq!(runner.cli_runner.seen(), ["cli:claude:sonnet"]);

        // Without the fallback a PAYG id never shells out.
        let runner = AdaptiveRunner::new(
            Scripted::failed("insufficient_quota"),
            Scripted::ok("from cli"),
            AdaptiveStrategy::Auto,
        );
        assert!(matches!(
            runner.run(&config("deepseek-flash")).unwrap(),
            RunOutcome::Failed { .. }
        ));
        assert!(runner.cli_runner.seen().is_empty());
    }

    #[test]
    fn a_double_failure_reports_both_planes() {
        let runner = AdaptiveRunner::new(
            Scripted::failed("insufficient_quota"),
            Scripted::failed("usage limit reached"),
            AdaptiveStrategy::PreferPaygThenCliSub,
        )
        .with_fallback_cli_model("cli:claude");
        let RunOutcome::Failed { message } = runner.run(&config("deepseek-flash")).unwrap() else {
            panic!("expected failure");
        };
        assert!(
            message.contains("payg-byok (deepseek-flash) failed: insufficient_quota"),
            "{message}"
        );
        assert!(
            message.contains("monthly-sub-cli (cli:claude) failed: usage limit reached"),
            "{message}"
        );
    }

    #[test]
    fn a_missing_cli_tool_is_a_failed_turn_that_fails_over_to_payg() {
        let empty = tempfile::tempdir().unwrap();
        let cli = CliSubRunner::default().with_search_dirs(vec![empty.path().to_path_buf()]);
        let outcome = cli.run(&config("cli:claude")).unwrap();
        let RunOutcome::Failed { message } = &outcome else {
            panic!("expected failure, got {outcome:?}");
        };
        assert!(message.contains("claude"), "{message}");
        assert!(matches!(
            cli.run(&config("deepseek-flash")).unwrap(),
            RunOutcome::Failed { .. }
        ));

        let runner = AdaptiveRunner::new(Scripted::ok("from payg"), cli, AdaptiveStrategy::Auto)
            .with_fallback_payg_model("deepseek-flash");
        assert_eq!(
            text(&runner.run(&config("cli:claude")).unwrap()),
            "from payg"
        );
        assert_eq!(runner.payg_runner.seen(), ["deepseek-flash"]);
    }

    #[test]
    fn cli_resolution_prefers_the_spec_then_the_default_then_discovery() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("codex"), b"").unwrap();
        std::fs::write(dir.path().join("claude"), b"").unwrap();
        let dirs = vec![dir.path().to_path_buf()];

        let discovered = CliSubRunner::default().with_search_dirs(dirs.clone());
        let commands = discovered.resolve("deepseek-flash").unwrap();
        assert_eq!(commands[0].program, dir.path().join("claude"));

        let defaulted =
            CliSubRunner::with_default_kind(CliToolKind::Codex).with_search_dirs(dirs.clone());
        let commands = defaulted.resolve("deepseek-flash").unwrap();
        assert_eq!(commands[0].program, dir.path().join("codex"));
        assert_eq!(commands[0].args, ["exec", "--full-auto", "-"]);

        let commands = defaulted.resolve("cli:claude:opus").unwrap();
        assert_eq!(commands[0].program, dir.path().join("claude"));
        assert_eq!(commands[0].args.last().map(String::as_str), Some("opus"));

        // With no explicit search list an absent tool falls back to PATH
        // lookup over every candidate name.
        let path_lookup = CliSubRunner::with_default_kind(CliToolKind::Gemini);
        if find_in_dirs(CliToolKind::Gemini, &default_search_dirs()).is_none() {
            let programs: Vec<PathBuf> = path_lookup
                .resolve("x")
                .unwrap()
                .into_iter()
                .map(|command| command.program)
                .collect();
            assert_eq!(
                programs,
                ["gemini", "gemini.cmd", "gemini.exe"].map(PathBuf::from)
            );
        }
    }

    /// A command on every platform that copies stdin to stdout (sorted).
    fn sort_command() -> CliCommand {
        let program = if cfg!(windows) {
            system32("sort.exe")
        } else {
            PathBuf::from("sort")
        };
        CliCommand {
            program,
            args: Vec::new(),
            pipe_prompt_on_stdin: true,
        }
    }

    /// A command on every platform that runs for about 30 seconds.
    fn slow_command() -> (PathBuf, Vec<String>) {
        if cfg!(windows) {
            (
                system32("PING.EXE"),
                vec!["-n".into(), "30".into(), "127.0.0.1".into()],
            )
        } else {
            (PathBuf::from("sleep"), vec!["30".into()])
        }
    }

    #[test]
    fn cli_runner_pipes_a_multi_line_quoted_prompt_on_stdin_at_zero_payg_cost() {
        let prompt = "line one says \"hello world\"\n\
                      line two has 'single' quotes and a \\backslash\n\
                      line three has %PATH% $HOME & | < > ^ chars\n";
        let runner = CliSubRunner::with_command(sort_command());
        let mut cfg = config("cli:claude");
        cfg.prompt = prompt.to_string();
        let outcome = runner.run(&cfg).unwrap();
        let RunOutcome::Succeeded {
            final_text,
            reported_cost_usd,
        } = outcome
        else {
            panic!("expected success, got {outcome:?}");
        };
        assert_eq!(reported_cost_usd, Some(0.0));
        for line in prompt.lines() {
            assert!(
                final_text.lines().any(|out| out.trim_end() == line),
                "missing {line:?} in {final_text:?}"
            );
        }
    }

    #[test]
    fn cli_runner_drains_output_larger_than_a_pipe_buffer() {
        let prompt: String = (0..6_000)
            .map(|n| format!("row {n:05} padding padding padding\n"))
            .collect();
        assert!(prompt.len() > 128 * 1024);
        let command = sort_command();
        let outcome = run_piped(
            &command,
            &std::env::temp_dir(),
            &prompt,
            Duration::from_secs(60),
            DEFAULT_POLL_INTERVAL,
        );
        let PipedRun::Finished(RunOutcome::Succeeded { final_text, .. }) = outcome else {
            panic!("expected success, got {outcome:?}");
        };
        assert_eq!(final_text.lines().count(), 6_000);
    }

    #[test]
    fn cli_runner_kills_a_child_that_outlives_its_timeout() {
        let (program, args) = slow_command();
        let runner = CliSubRunner::with_command(CliCommand {
            program,
            args,
            pipe_prompt_on_stdin: true,
        });
        let mut cfg = config("cli:claude");
        cfg.timeout = Duration::from_millis(400);
        let started = Instant::now();
        assert_eq!(runner.run(&cfg).unwrap(), RunOutcome::TimedOut);
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "timeout took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_non_zero_exit_is_a_failed_turn_quoting_the_output() {
        let (program, args): (PathBuf, Vec<String>) = if cfg!(windows) {
            (
                system32("cmd.exe"),
                vec![
                    "/D".into(),
                    "/C".into(),
                    "echo Claude usage limit reached 1>&2 & exit 3".into(),
                ],
            )
        } else {
            (
                PathBuf::from("sh"),
                vec![
                    "-c".into(),
                    "echo Claude usage limit reached 1>&2; exit 3".into(),
                ],
            )
        };
        let runner = CliSubRunner::with_command(CliCommand {
            program,
            args,
            pipe_prompt_on_stdin: false,
        });
        let RunOutcome::Failed { message } = runner.run(&config("cli:claude")).unwrap() else {
            panic!("expected failure");
        };
        assert!(message.contains("code 3"), "{message}");
        assert!(message.contains("usage limit reached"), "{message}");
        assert!(
            message.contains("[subscription quota exhausted]"),
            "{message}"
        );
    }

    #[test]
    fn output_survives_a_grandchild_that_holds_the_pipe_open() {
        let (program, args): (PathBuf, Vec<String>) = if cfg!(windows) {
            (
                system32("cmd.exe"),
                vec![
                    "/D".into(),
                    "/C".into(),
                    "start /B PING -n 8 127.0.0.1 >NUL & echo visible".into(),
                ],
            )
        } else {
            (
                PathBuf::from("sh"),
                vec!["-c".into(), "sleep 8 & echo visible".into()],
            )
        };
        let runner = CliSubRunner::with_command(CliCommand {
            program,
            args,
            pipe_prompt_on_stdin: false,
        });
        let started = Instant::now();
        let outcome = runner.run(&config("cli:claude")).unwrap();
        let RunOutcome::Succeeded { final_text, .. } = outcome else {
            panic!("expected success, got {outcome:?}");
        };
        assert!(final_text.contains("visible"), "{final_text:?}");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_missing_workspace_is_a_failed_turn_not_the_harness_cwd() {
        let mut cfg = config("cli:claude");
        cfg.workspace = std::env::temp_dir().join("susi-adaptive-no-such-workspace-dir");
        let runner = CliSubRunner::with_command(sort_command());
        let RunOutcome::Failed { message } = runner.run(&cfg).unwrap() else {
            panic!("expected failure");
        };
        assert!(message.contains("workspace"), "{message}");
    }

    #[test]
    fn a_program_that_does_not_exist_is_not_found_not_an_error() {
        let missing = CliCommand {
            program: PathBuf::from("susi-definitely-not-installed-cli"),
            args: Vec::new(),
            pipe_prompt_on_stdin: true,
        };
        let outcome = run_piped(
            &missing,
            &std::env::temp_dir(),
            "x",
            Duration::from_secs(5),
            DEFAULT_POLL_INTERVAL,
        );
        assert_eq!(outcome, PipedRun::NotFound);
        let runner = CliSubRunner::with_command(missing);
        assert!(matches!(
            runner.run(&config("cli:claude")).unwrap(),
            RunOutcome::Failed { .. }
        ));
    }

    #[test]
    fn failure_tails_are_bounded_on_a_char_boundary() {
        let long = "\u{e9}".repeat(FAILURE_TAIL_CHARS + 50);
        assert_eq!(tail(&long).chars().count(), FAILURE_TAIL_CHARS);
        assert_eq!(tail("short"), "short");
    }

    #[test]
    fn socket_endpoints_default_to_the_susi_os_ports_and_shift_by_offset() {
        let default = SusiSocketEndpoints::default();
        assert_eq!(default.gmcp_http_url, "http://127.0.0.1:9090");
        assert_eq!(
            default.gemi_openai_url,
            "http://127.0.0.1:9091/v1/chat/completions"
        );
        assert_eq!(default.gawd_a2a_url, "http://127.0.0.1:9094");
        let shifted = SusiSocketEndpoints::from_port_offset(100);
        assert_eq!(shifted.gmcp_http_url, "http://127.0.0.1:9190");
        assert_eq!(
            shifted.gemi_openai_url,
            "http://127.0.0.1:9191/v1/chat/completions"
        );
        assert_eq!(shifted.gawd_a2a_url, "http://127.0.0.1:9194");
        let saturated = SusiSocketEndpoints::from_port_offset(u16::MAX);
        assert_eq!(saturated.gmcp_http_url, "http://127.0.0.1:65535");
    }

    #[test]
    fn execution_modes_have_stable_names_and_the_default_runner_is_auto() {
        assert_eq!(ExecutionMode::PaygByok.as_str(), "payg-byok");
        assert_eq!(ExecutionMode::MonthlySubCli.as_str(), "monthly-sub-cli");
        let runner = AdaptiveRunner::default();
        assert_eq!(runner.strategy, AdaptiveStrategy::Auto);
        assert_eq!(
            runner.plan("cli:claude").primary,
            (ExecutionMode::MonthlySubCli, "cli:claude".to_string())
        );
        assert_eq!(
            runner.plan("deepseek-flash").primary,
            (ExecutionMode::PaygByok, "deepseek-flash".to_string())
        );
    }
}
