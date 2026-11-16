//! The tool registry: the syscall surface an agent is allowed to use.
//!
//! A tool is a record — name, schema, handler and policy — not a match arm in
//! the worker. The builtin set declares local and installed capabilities. A
//! tool served by an MCP server is registered with [`Handler::Mcp`] and called through
//! [`crate::mcp::McpClient`].
//!
//! Policy is the authority boundary: [`Policy::Auto`] runs, [`Policy::Ask`]
//! runs only with an explicit grant (the operator approval the cockpit records),
//! and [`Policy::Deny`] never runs. A tool that is not in the registry is
//! refused rather than attempted.

use serde_json::{json, Value};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How much authority a tool carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    /// Runs without asking.
    Auto,
    /// Runs only when the caller holds an explicit grant for it.
    Ask,
    /// Never runs.
    Deny,
}

/// Who executes the tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handler {
    /// A built-in local or installed capability.
    Native,
    /// Served by an MCP server.
    Mcp,
}

/// One tool the agent may call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolSpec {
    /// Function name the model calls.
    pub name: String,
    /// Human-readable description, sent to the model.
    pub description: String,
    /// JSON schema of the arguments.
    pub parameters: Value,
    /// Who runs it.
    pub handler: Handler,
    /// What authority it carries.
    pub policy: Policy,
}

impl ToolSpec {
    /// A built-in tool contract.
    #[must_use]
    pub fn native(name: &str, description: &str, parameters: Value, policy: Policy) -> Self {
        Self {
            name: name.to_string(),
            description: description.to_string(),
            parameters,
            handler: Handler::Native,
            policy,
        }
    }

    /// A tool served by an MCP server.
    #[must_use]
    pub fn mcp(name: &str, description: &str, parameters: Value, policy: Policy) -> Self {
        Self {
            name: name.to_string(),
            description: description.to_string(),
            parameters,
            handler: Handler::Mcp,
            policy,
        }
    }

    fn declaration(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": self.name,
                "description": self.description,
                "parameters": self.parameters,
            }
        })
    }
}

/// The tools a worker may call.
#[derive(Debug, Clone, Default)]
pub struct Registry {
    tools: Vec<ToolSpec>,
}

impl Registry {
    /// The six built-in tool contracts, all on [`Policy::Auto`].
    ///
    /// `run_command` is the sharpest of them and is kept automatic because the
    /// worker is headless: an `Ask` tool in a run with no operator would simply
    /// never run. A caller that wants to gate it registers its own spec.
    #[must_use]
    pub fn builtin() -> Self {
        let mut registry = Self::default();
        let mut register = |name, description, properties: Value, required: &[&str]| {
            let mut schema = json!({"type": "object", "properties": properties});
            if !required.is_empty() {
                schema["required"] = json!(required);
            }
            registry.register(ToolSpec::native(name, description, schema, Policy::Auto));
        };
        register(
            "view_file",
            "Read file lines from the workspace. Lines are 1-indexed. Provide optional start_line and end_line to view a line slice.",
            json!({
                "path": { "type": "string", "description": "Relative path to file in workspace" },
                "start_line": { "type": "integer", "description": "1-based starting line number" },
                "end_line": { "type": "integer", "description": "1-based ending line number" }
            }),
            &["path"],
        );
        register(
            "write_to_file",
            "Create a new file in the workspace with content. Refuses a path that already exists: add to an existing file with append_to_file, or change part of it with replace_file_content.",
            json!({
                "path": { "type": "string", "description": "Relative path to a file that does not exist yet" },
                "content": { "type": "string", "description": "Complete text of the new file" }
            }),
            &["path", "content"],
        );
        register(
            "append_to_file",
            "Append content to the end of a workspace file (a newline is inserted first when the file does not end in one); creates the file when it is missing. The way to add a function or test to a large file.",
            json!({
                "path": { "type": "string", "description": "Relative path to file in workspace" },
                "content": { "type": "string", "description": "Text to add at the end of the file" }
            }),
            &["path", "content"],
        );
        register(
            "replace_file_content",
            "Perform an exact surgical string replacement in a workspace file.",
            json!({
                "path": { "type": "string", "description": "Relative path to file in workspace" },
                "target": { "type": "string", "description": "Exact text substring to match and replace" },
                "replacement": { "type": "string", "description": "Replacement text" }
            }),
            &["path", "target", "replacement"],
        );
        register(
            "run_command",
            "Run a shell command inside the workspace directory (e.g. 'cargo check', 'cargo test').",
            json!({
                "command": { "type": "string", "description": "Command string to execute" }
            }),
            &["command"],
        );
        register(
            "list_dir",
            "List file and directory entries within a workspace directory.",
            json!({
                "path": { "type": "string", "description": "Relative path, or empty for root" }
            }),
            &[],
        );
        registry
    }

    /// Add a tool, replacing one with the same name.
    pub fn register(&mut self, spec: ToolSpec) {
        self.tools.retain(|known| known.name != spec.name);
        self.tools.push(spec);
    }

    /// Every registered tool, in registration order.
    #[must_use]
    pub fn tools(&self) -> &[ToolSpec] {
        &self.tools
    }

    /// Find a tool by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&ToolSpec> {
        self.tools.iter().find(|tool| tool.name == name)
    }

    /// The OpenAI-compatible `tools` array for a completion request.
    #[must_use]
    pub fn declarations(&self) -> Value {
        Value::Array(
            self.tools
                .iter()
                .map(ToolSpec::declaration)
                .collect::<Vec<_>>(),
        )
    }

    /// Whether a tool may run for this caller.
    ///
    /// # Errors
    ///
    /// Returns a refusal for an unknown tool, a denied tool, or an `Ask` tool
    /// without a matching grant.
    pub fn authorize(&self, name: &str, grants: &[String]) -> Result<&ToolSpec, String> {
        let spec = self
            .get(name)
            .ok_or_else(|| format!("unknown tool '{name}'"))?;
        match spec.policy {
            Policy::Auto => Ok(spec),
            Policy::Ask if grants.iter().any(|grant| grant == name) => Ok(spec),
            Policy::Ask => Err(format!(
                "tool '{name}' needs operator approval; grant it or add it to the run's approvals"
            )),
            Policy::Deny => Err(format!("tool '{name}' is denied by policy")),
        }
    }

    /// Authorize and run a tool with JSON arguments.
    ///
    /// Built-in filesystem contracts resolve an installed MCP provider after
    /// authorization and argument parsing. Explicit MCP registrations require
    /// the caller's live client; command execution retains its local policy.
    #[must_use]
    pub fn execute(
        &self,
        workspace: &Path,
        name: &str,
        args_json: &str,
        grants: &[String],
    ) -> String {
        if let Err(refusal) = self.authorize(name, grants) {
            return refusal;
        }
        match self.get(name).map(|spec| spec.handler) {
            Some(Handler::Mcp) => {
                return format!("tool '{name}' is served by MCP; call it through an McpClient")
            }
            Some(Handler::Native) => {}
            None => return format!("unknown tool '{name}'"),
        }
        let args: Value = match serde_json::from_str(args_json) {
            Ok(value) => value,
            Err(error) => return format!("error parsing tool arguments: {error}"),
        };
        match name {
            "view_file"
            | "write_to_file"
            | "append_to_file"
            | "replace_file_content"
            | "list_dir" => {
                let native_path = match susi_paths::native_path::encode(workspace) {
                    Ok(path) => path,
                    Err(error) => return format!("tool '{name}' workspace: {error}"),
                };
                let tool = format!("worker_{name}");
                match crate::tool_backend::try_call_text_in(
                    &tool,
                    &json!({"workspace": native_path, "arguments": args}),
                    Path::new("."),
                ) {
                    Ok(Some(text)) => Ok(text),
                    Ok(None) if name == "list_dir" => return standalone_list_dir(workspace, &args),
                    Ok(None) => Err(crate::tool_backend::no_provider(&tool, 0)),
                    Err(error) => Err(error),
                }
                .unwrap_or_else(|error| format!("tool '{name}' provider failed: {error}"))
            }
            "run_command" => tool_run_command(workspace, &args),
            other => format!("unknown tool '{other}'"),
        }
    }
}

/// Safely resolve a path within the workspace root.
///
/// # Errors
///
/// Absolute paths and `..` traversal are refused.
pub fn sanitize_path(workspace: &Path, rel: &str) -> Result<PathBuf, String> {
    let clean_rel = Path::new(rel);
    for component in clean_rel.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => {
                return Err("absolute paths are not permitted; provide relative paths".to_string());
            }
            Component::ParentDir => {
                return Err("path traversal ('..') is not permitted".to_string());
            }
            Component::CurDir | Component::Normal(_) => {}
        }
    }
    Ok(workspace.join(clean_rel))
}

fn standalone_list_dir(workspace: &Path, args: &Value) -> String {
    let path = args["path"].as_str().unwrap_or_default().trim();
    let directory = match sanitize_path(workspace, path) {
        Ok(directory) => directory,
        Err(error) => return error,
    };
    if !directory.exists() {
        return format!("directory '{path}' does not exist");
    }
    let contents = match std::fs::read_dir(directory) {
        Ok(contents) => contents,
        Err(error) => return format!("failed to read directory: {error}"),
    };
    let mut names = Vec::new();
    for entry in contents.flatten() {
        let mut name = entry.file_name().to_string_lossy().into_owned();
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            name.push('/');
        }
        names.push(name);
    }
    names.sort();
    if names.is_empty() {
        "(empty directory)".into()
    } else {
        names.join("\n")
    }
}

fn tool_run_command(workspace: &Path, args: &Value) -> String {
    let cmd_str = match args.get("command").and_then(Value::as_str) {
        Some(c) => c,
        None => return "missing 'command' argument".to_string(),
    };
    if runs_susi(cmd_str) {
        return "refused: the harness owns `susi` commands (claim, close, finish, sync). Do not run \
them — leave the finished changes in the working tree and summarize."
            .to_string();
    }
    run_shell(workspace, cmd_str, &withheld_variables())
}

/// The variables a worker's command never inherits: every provider key the
/// registry (overlay included) names. The command's output goes to a model,
/// which may be another company's, and `env` or `printenv` once handed it
/// every key in the harness's environment.
fn withheld_variables() -> Vec<&'static str> {
    let mut vars = crate::fleet::key_names(crate::providers::Registry::builtin());
    for extra in [
        "SUSI_VAULT_KEY",
        "SUSI_VAULT_KEY_FILE",
        "SUSI_VAULT_PASSPHRASE",
        "SUSI_VAULT_PASSPHRASE_FILE",
        "SUSI_VAULT_KDBX_PASSPHRASE",
        "SUSI_VAULT_KDBX_PASSPHRASE_FILE",
        "SUSI_KDBX_PASSPHRASE",
        "SUSI_KDBX_PASSPHRASE_FILE",
        "SUSI_COMMERCIAL_SECRET",
        "SUSI_CUSTOMER_LICENSE_SIGNING_KEY",
        "SUSI_CUSTOMER_LICENSE_TOKEN",
        "SUSI_HOST_TOKEN",
        "SUSI_CELL_TOKEN",
        "SUSI_TURN_USER",
        "SUSI_TURN_PASS",
        "ANTHROPIC_WORKSPACE_ID",
        "GH_TOKEN",
        "GITHUB_TOKEN",
        "GITHUB_PERSONAL_ACCESS_TOKEN",
        "HF_TOKEN",
        "HUGGINGFACE_HUB_TOKEN",
        "GROQ_API_KEY",
        "MISTRAL_API_KEY",
        "XAI_API_KEY",
        "ZHIPU_API_KEY",
        "ZAI_API_KEY",
        "FIREWORKS_API_KEY",
        "TOGETHER_API_KEY",
        "CEREBRAS_API_KEY",
        "COHERE_API_KEY",
        "PERPLEXITY_API_KEY",
        "MINIMAX_API_KEY",
        "LITELLM_MASTER_KEY",
        "OPENCLAW_GATEWAY_TOKEN",
        "BRAVE_API_KEY",
        "TAVILY_API_KEY",
        "CONTEXT7_API_KEY",
        "SENTRY_ACCESS_TOKEN",
        "SENTRY_AUTH_TOKEN",
        "AWS_SECRET_ACCESS_KEY",
        "AWS_SESSION_TOKEN",
        "AZURE_CLIENT_SECRET",
    ] {
        if !vars.contains(&extra) {
            vars.push(extra);
        }
    }
    vars
}

/// Run `cmd_str` in the worker's shell, in `workspace`, without the
/// `withheld` variables, and report its exit code and output for the model.
fn run_shell(workspace: &Path, cmd_str: &str, withheld: &[&str]) -> String {
    let mut command = if cfg!(windows) {
        let mut c = Command::new("powershell");
        c.args(["-NoProfile", "-NonInteractive", "-Command", cmd_str]);
        c
    } else {
        let mut c = Command::new("sh");
        c.arg("-c").arg(cmd_str);
        c
    };
    command.current_dir(workspace);
    if crate::workload::enabled() {
        crate::workload::clean_environment(&mut command);
    }
    command.stdin(Stdio::null());
    // Every command a worker runs carries the guard variable: the CLI refuses
    // harness-owned subcommands under it, so a wrapper or an argv split (which
    // the text refusal cannot see) cannot run the harness's own close path.
    command.env("SUSI_WORKER_GUARD", "1");
    for name in withheld {
        command.env_remove(name);
    }

    match run_with_deadline(command) {
        Ok(out) => command_report(
            out.status.code().unwrap_or(-1),
            &String::from_utf8_lossy(&out.stdout),
            &String::from_utf8_lossy(&out.stderr),
        ),
        Err(e) => crate::secrets::redact(&format!("failed to run command '{cmd_str}': {e}")),
    }
}

/// Most of a command's output a tool result carries: the last 32 KB.
const OUTPUT_LIMIT: usize = 32 * 1024;

/// The tool result for a finished command: its exit code, then its output
/// with every key masked ([`crate::secrets::redact`]), cut to its last
/// [`OUTPUT_LIMIT`] bytes.
///
/// A command reaches more than the environment (`cat ~/.susi/.env`), so the
/// output is masked too, whole and before the cut, which could otherwise
/// leave the tail of a key unmatched. The cut lands on a character boundary:
/// a byte cut inside an em dash or a U+FFFD in long cargo output panicked.
fn command_report(code: i32, stdout: &str, stderr: &str) -> String {
    let mut combined = String::new();
    if !stdout.is_empty() {
        combined.push_str(stdout);
    }
    if !stderr.is_empty() {
        if !combined.is_empty() {
            combined.push('\n');
        }
        combined.push_str("[stderr]\n");
        combined.push_str(stderr);
    }
    // Read every provider key first, so one this process has not used yet (in
    // a `.env` file or the Windows registry) is masked by its value, not only
    // when it has a provider's usual shape.
    for name in withheld_variables() {
        let _ = crate::secrets::is_set(name);
    }
    let combined = crate::secrets::redact(&combined);
    if combined.len() > OUTPUT_LIMIT {
        let mut start = combined.len() - OUTPUT_LIMIT;
        while !combined.is_char_boundary(start) {
            start += 1;
        }
        format!("exit code: {code}\n[truncated...]\n{}", &combined[start..])
    } else {
        format!("exit code: {code}\n{combined}")
    }
}

/// Deadline for one worker command. A test the worker wrote can hang; the
/// mission must not hang with it. Overridable for tests and for a slow gate.
fn command_limit() -> Duration {
    std::env::var("SUSI_TOOL_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
        .map_or_else(|| Duration::from_secs(600), Duration::from_secs)
}

/// Run a command with a deadline, killing its whole process tree on expiry so a
/// hung child cannot strand the worker.
fn run_with_deadline(command: Command) -> Result<std::process::Output, String> {
    run_with_limit(command, command_limit())
}

/// The deadline itself, so a test can prove a hung command returns instead of
/// blocking the worker.
fn run_with_limit(mut command: Command, limit: Duration) -> Result<std::process::Output, String> {
    let mut child = susi_paths::console::without_window(&mut command)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let deadline = Instant::now() + limit;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                return child.wait_with_output().map_err(|e| e.to_string());
            }
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Ok(None) => {
                kill_tree(child.id());
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "timed out after {}s; the process tree was killed — narrow the command or raise SUSI_TOOL_TIMEOUT_SECS",
                    limit.as_secs()
                ));
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// Kill the command and every descendant it spawned.
fn kill_tree(pid: u32) {
    #[cfg(windows)]
    {
        let _ = susi_paths::console::background_command("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .output();
    }
    #[cfg(not(windows))]
    {
        let _ = Command::new("kill").args(kill_tree_args(pid)).output();
    }
}

/// The `kill` arguments that signal the process group `pid` leads. The `--`
/// is load-bearing: without it procps-ng reads `-<pid>` as an option and
/// takes its first digit as the target, so a pid beginning with 1 became
/// `kill(-1, SIGTERM)` and signalled every process the user owns.
#[cfg(not(windows))]
fn kill_tree_args(pid: u32) -> [String; 3] {
    ["-TERM".to_string(), "--".to_string(), format!("-{pid}")]
}

/// True when any command-position word in `command` invokes the susi CLI. A
/// worker that runs `susi workflow finish` holds its own turn while the
/// harness's close machinery runs, and the adapter then fails behind it.
fn runs_susi(command: &str) -> bool {
    // A wrapper (`powershell -Command`, `cmd /c`) hides the binary from a
    // command-position check, so the harness-owned subcommands are refused by
    // name wherever they appear.
    let lowered = command.to_ascii_lowercase();
    if [
        "workflow finish",
        "workflow sync",
        "workflow start",
        "tasks claim",
        "tasks close",
        "tasks release",
    ]
    .iter()
    .any(|phrase| lowered.contains(phrase))
    {
        return true;
    }
    command
        .split([';', '|', '&', '\n', '\r'])
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .any(|part| {
            let first = part
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .trim_matches('"')
                .to_ascii_lowercase();
            let name = first.rsplit(['\\', '/']).next().unwrap_or(first.as_str());
            let stem = name.strip_suffix(".exe").unwrap_or(name);
            // The harness binary may be renamed (`susi12.exe`, `susi-lab.exe`):
            // any stem of `susi` plus a version or lab suffix is the same CLI.
            stem == "susi"
                || stem.strip_prefix("susi").is_some_and(|rest| {
                    !rest.is_empty()
                        && rest
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
                })
        })
}

#[cfg(all(test, windows))]
#[path = "tests/tools_window.rs"]
mod tools_window;

#[cfg(test)]
mod tests {
    use super::*;

    /// A group id passed to `kill` without `--` is parsed as an option by
    /// procps-ng, which then signals its first digit: `-TERM -1234` became
    /// `kill(-1, SIGTERM)`, every process the user owns.
    #[cfg(not(windows))]
    #[test]
    fn kill_tree_separates_the_group_id_from_the_options() {
        for pid in [1_u32, 19, 1234, 16330, 21406] {
            assert_eq!(
                kill_tree_args(pid),
                ["-TERM".to_string(), "--".to_string(), format!("-{pid}")]
            );
        }
    }

    #[test]
    fn tool_registry_lists_the_six_builtin_contracts() {
        let registry = Registry::builtin();
        assert_eq!(registry.tools().len(), 6);
        for name in [
            "view_file",
            "write_to_file",
            "append_to_file",
            "replace_file_content",
            "run_command",
            "list_dir",
        ] {
            assert!(registry.get(name).is_some(), "{name} is missing");
        }
        let declarations = registry.declarations();
        assert_eq!(declarations.as_array().unwrap().len(), 6);
        assert_eq!(declarations[0]["type"], "function");
        assert!(declarations[0]["function"]["parameters"]["properties"]["path"].is_object());
    }

    #[test]
    fn tool_registry_asks_for_approval_and_denies() {
        let mut registry = Registry::builtin();
        registry.register(ToolSpec::native(
            "deploy",
            "Ship it.",
            json!({"type": "object"}),
            Policy::Ask,
        ));
        registry.register(ToolSpec::native(
            "wipe",
            "No.",
            json!({"type": "object"}),
            Policy::Deny,
        ));
        assert!(registry.authorize("deploy", &[]).is_err());
        assert!(registry
            .authorize("deploy", &["deploy".to_string()])
            .is_ok());
        assert!(registry.authorize("wipe", &["wipe".to_string()]).is_err());
        assert!(registry.authorize("nope", &[]).is_err());
    }

    #[test]
    fn tool_registry_refuses_an_mcp_tool_without_a_client() {
        let mut registry = Registry::builtin();
        registry.register(ToolSpec::mcp(
            "web_search",
            "Search the web.",
            json!({"type": "object"}),
            Policy::Auto,
        ));
        let output = registry.execute(Path::new("."), "web_search", "{}", &[]);
        assert!(output.contains("McpClient"), "{output}");
    }

    #[test]
    fn filesystem_authorization_precedes_parsing_and_provider_dispatch() {
        for name in [
            "view_file",
            "write_to_file",
            "append_to_file",
            "replace_file_content",
            "list_dir",
        ] {
            let mut registry = Registry::builtin();
            for policy in [Policy::Ask, Policy::Deny] {
                registry.register(ToolSpec::native(name, "Fixture", json!({}), policy));
                let output = registry.execute(Path::new(""), name, "{", &[]);
                assert_eq!(output, registry.authorize(name, &[]).unwrap_err());
            }
            registry.register(ToolSpec::native(name, "Fixture", json!({}), Policy::Ask));
            let expected = format!(
                "error parsing tool arguments: {}",
                serde_json::from_str::<Value>("{").unwrap_err()
            );
            assert_eq!(
                registry.execute(Path::new(""), name, "{", &[name.to_owned()]),
                expected
            );
            registry.register(ToolSpec::native(name, "Fixture", json!({}), Policy::Auto));
            assert_eq!(registry.execute(Path::new(""), name, "{", &[]), expected);
            registry.register(ToolSpec::mcp(name, "Fixture", json!({}), Policy::Auto));
            assert_eq!(
                registry.execute(Path::new(""), name, "{", &[]),
                format!("tool '{name}' is served by MCP; call it through an McpClient")
            );
        }
    }

    #[test]
    fn tool_registry_blocks_path_traversal() {
        let ws = Path::new("test_ws");
        assert!(sanitize_path(ws, "../outside").is_err());
        assert!(sanitize_path(ws, "sub/../../outside").is_err());
        assert!(sanitize_path(ws, "safe/nested/file.rs").is_ok());
    }

    #[test]
    fn tool_registry_refuses_an_unknown_tool() {
        let output = Registry::builtin().execute(Path::new("."), "format_disk", "{}", &[]);
        assert!(output.contains("unknown tool"), "{output}");
    }
    #[test]
    fn a_hung_command_returns_a_timeout_instead_of_blocking() {
        let mut command = if cfg!(windows) {
            let mut c = Command::new("powershell");
            c.args(["-NoProfile", "-Command", "Start-Sleep -Seconds 30"]);
            c
        } else {
            let mut c = Command::new("sh");
            c.args(["-c", "sleep 30"]);
            c
        };
        command.stdin(Stdio::null());
        let started = Instant::now();
        let result = run_with_limit(command, Duration::from_millis(700));
        let elapsed = started.elapsed();
        assert!(result.is_err(), "a hung command must not return Ok");
        assert!(
            result.unwrap_err().contains("timed out"),
            "the worker must be told it timed out"
        );
        assert!(
            elapsed < Duration::from_secs(20),
            "the deadline must end the wait, not the sleep: {elapsed:?}"
        );
    }
    #[test]
    fn every_provider_key_variable_is_withheld_from_a_command() {
        let withheld = withheld_variables();
        for name in [
            "OPENAI_API_KEY",
            "ANTHROPIC_API_KEY",
            "GEMINI_API_KEY",
            "GOOGLE_API_KEY",
            "DEEPSEEK_API_KEY",
            "MOONSHOT_API_KEY",
            "KIMI_API_KEY",
            "OPENROUTER_API_KEY",
            "SUSI_VAULT_KEY",
            "SUSI_HOST_TOKEN",
            "SUSI_TURN_PASS",
            "GH_TOKEN",
            "HF_TOKEN",
        ] {
            assert!(withheld.contains(&name), "{name} missing from {withheld:?}");
        }
    }

    #[test]
    fn a_command_never_inherits_a_withheld_key_variable() {
        // A name of its own, so no other test's view of the keys changes.
        let name = "SUSI_TEST_TOOLS_WITHHELD_0001";
        std::env::set_var(name, "withheld-value-0001");
        let print = if cfg!(windows) {
            format!("Write-Output ('[' + $env:{name} + ']')")
        } else {
            format!("printf '[%s]\\n' \"${name}\"")
        };
        let workspace = susi_paths::tempfile::tempdir().unwrap();
        let inherited = run_shell(workspace.path(), &print, &[]);
        let withheld = run_shell(workspace.path(), &print, &[name]);
        std::env::remove_var(name);
        assert!(
            inherited.contains("[withheld-value-0001]"),
            "the shell sees the environment it is given: {inherited}"
        );
        assert!(withheld.contains("[]"), "{withheld}");
        assert!(!withheld.contains("withheld-value-0001"), "{withheld}");
    }

    #[test]
    fn command_output_is_masked_before_the_model_sees_it() {
        let report = command_report(
            0,
            "OPENAI_API_KEY=sk-proj-test0123456789abcdef\n",
            "GEMINI_API_KEY='AIzaSyTest0123456789abcdef'",
        );
        assert!(!report.contains("sk-proj-test"), "{report}");
        assert!(!report.contains("AIzaSyTest"), "{report}");
        assert!(report.contains("OPENAI_API_KEY=<redacted>"), "{report}");
        assert!(report.starts_with("exit code: 0\n"), "{report}");
        // Ordinary output, file names included, is left as it is.
        let plain = command_report(1, "test task-runner-tests ... ok\n", "");
        assert_eq!(plain, "exit code: 1\ntest task-runner-tests ... ok\n");
    }

    #[test]
    fn long_output_is_cut_on_a_character_boundary() {
        // 40,001 bytes: the last 32 KB start inside a two-byte character,
        // where a byte cut panicked.
        let stdout = format!("{}x", "é".repeat(20_000));
        assert!(!stdout.is_char_boundary(stdout.len() - OUTPUT_LIMIT));
        let report = command_report(0, &stdout, "");
        let tail = report
            .strip_prefix("exit code: 0\n[truncated...]\n")
            .expect("long output is marked as cut");
        assert!(tail.len() <= OUTPUT_LIMIT, "{}", tail.len());
        assert!(tail.ends_with("éx"));
    }

    #[test]
    fn a_susi_invocation_at_command_position_is_refused() {
        assert!(runs_susi("susi workflow finish T-X"));
        assert!(runs_susi("cd wt; susi tasks close T-X"));
        assert!(runs_susi(
            "cargo test -p x && \"C:\\bin\\susi.exe\" workflow sync"
        ));
        assert!(runs_susi(
            "powershell -NoProfile -Command \"out/roadmap/susi13.exe workflow finish T-X\""
        ));
        assert!(runs_susi("cmd /c \"susi tasks close T-X\""));
        assert!(runs_susi("/out/roadmap/susi12.exe workflow finish T-X"));
        assert!(runs_susi("susi-lab tasks close T-X"));
        assert!(!runs_susi("cargo test -p susi-harness"));
        assert!(!runs_susi("git grep -n 'susi workflow'"));
        assert!(!runs_susi("cargo test --test gmcp_requirements gmcp_req_1"));
    }
}
