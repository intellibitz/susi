//! The tool registry: the syscall surface an agent is allowed to use.
//!
//! A tool is a record — name, schema, handler and policy — not a match arm in
//! the worker. The five native tools are the builtin set; a tool served by an
//! MCP server is registered with [`Handler::Mcp`] and called through
//! [`crate::mcp::McpClient`].
//!
//! Policy is the authority boundary: [`Policy::Auto`] runs, [`Policy::Ask`]
//! runs only with an explicit grant (the operator approval the cockpit records),
//! and [`Policy::Deny`] never runs. A tool that is not in the registry is
//! refused rather than attempted.

use serde_json::{json, Value};
use std::fs;
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
    /// Implemented in this binary.
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
    /// A native tool.
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
    /// The five native tools, all on [`Policy::Auto`].
    ///
    /// `run_command` is the sharpest of them and is kept automatic because the
    /// worker is headless: an `Ask` tool in a run with no operator would simply
    /// never run. A caller that wants to gate it registers its own spec.
    #[must_use]
    pub fn builtin() -> Self {
        let mut registry = Self::default();
        registry.register(ToolSpec::native(
            "view_file",
            "Read file lines from the workspace. Lines are 1-indexed. Provide optional start_line and end_line to view a line slice.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Relative path to file in workspace" },
                    "start_line": { "type": "integer", "description": "1-based starting line number" },
                    "end_line": { "type": "integer", "description": "1-based ending line number" }
                },
                "required": ["path"]
            }),
            Policy::Auto,
        ));
        registry.register(ToolSpec::native(
            "write_to_file",
            "Create or overwrite a file in the workspace with content.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Relative path to file in workspace" },
                    "content": { "type": "string", "description": "Complete file text content" }
                },
                "required": ["path", "content"]
            }),
            Policy::Auto,
        ));
        registry.register(ToolSpec::native(
            "replace_file_content",
            "Perform an exact surgical string replacement in a workspace file.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Relative path to file in workspace" },
                    "target": { "type": "string", "description": "Exact text substring to match and replace" },
                    "replacement": { "type": "string", "description": "Replacement text" }
                },
                "required": ["path", "target", "replacement"]
            }),
            Policy::Auto,
        ));
        registry.register(ToolSpec::native(
            "run_command",
            "Run a shell command inside the workspace directory (e.g. 'cargo check', 'cargo test').",
            json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "Command string to execute" }
                },
                "required": ["command"]
            }),
            Policy::Auto,
        ));
        registry.register(ToolSpec::native(
            "list_dir",
            "List file and directory entries within a workspace directory.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Relative path, or empty for root" }
                }
            }),
            Policy::Auto,
        ));
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
    /// Native tools are executed here. An MCP tool is refused with a pointer to
    /// the client, because the registry holds policy, not a live connection.
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
            "view_file" => tool_view_file(workspace, &args),
            "write_to_file" => tool_write_to_file(workspace, &args),
            "replace_file_content" => tool_replace_file_content(workspace, &args),
            "run_command" => tool_run_command(workspace, &args),
            "list_dir" => tool_list_dir(workspace, &args),
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

fn tool_view_file(workspace: &Path, args: &Value) -> String {
    let path_str = match args.get("path").and_then(Value::as_str) {
        Some(p) => p,
        None => return "missing 'path' argument".to_string(),
    };
    let target_path = match sanitize_path(workspace, path_str) {
        Ok(p) => p,
        Err(e) => return e,
    };
    if !target_path.exists() {
        return format!("file '{}' does not exist", path_str);
    }
    let content = match fs::read_to_string(&target_path) {
        Ok(c) => c,
        Err(e) => return format!("failed to read '{}': {e}", path_str),
    };

    let start_line = args
        .get("start_line")
        .and_then(Value::as_u64)
        .map(|v| v as usize)
        .unwrap_or(1);
    let end_line = args
        .get("end_line")
        .and_then(Value::as_u64)
        .map(|v| v as usize);

    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len();
    if total_lines == 0 {
        return "(empty file)".to_string();
    }

    let start_idx = start_line.saturating_sub(1).min(total_lines);
    let end_idx = end_line.unwrap_or(total_lines).min(total_lines);

    let mut out = String::new();
    for (i, line) in lines[start_idx..end_idx].iter().enumerate() {
        let line_num = start_idx + i + 1;
        out.push_str(&format!("{line_num:>5}: {line}\n"));
    }
    out
}

fn tool_write_to_file(workspace: &Path, args: &Value) -> String {
    let path_str = match args.get("path").and_then(Value::as_str) {
        Some(p) => p,
        None => return "missing 'path' argument".to_string(),
    };
    let content = match args.get("content").and_then(Value::as_str) {
        Some(c) => c,
        None => return "missing 'content' argument".to_string(),
    };
    let target_path = match sanitize_path(workspace, path_str) {
        Ok(p) => p,
        Err(e) => return e,
    };
    if let Some(parent) = target_path.parent() {
        if let Err(e) = fs::create_dir_all(parent) {
            return format!("failed to create parent directories: {e}");
        }
    }
    match fs::write(&target_path, content) {
        Ok(()) => format!(
            "successfully wrote {} bytes to '{}'",
            content.len(),
            path_str
        ),
        Err(e) => format!("failed to write to '{}': {e}", path_str),
    }
}

fn tool_replace_file_content(workspace: &Path, args: &Value) -> String {
    let path_str = match args.get("path").and_then(Value::as_str) {
        Some(p) => p,
        None => return "missing 'path' argument".to_string(),
    };
    let target = match args.get("target").and_then(Value::as_str) {
        Some(t) => t,
        None => return "missing 'target' argument".to_string(),
    };
    let replacement = match args.get("replacement").and_then(Value::as_str) {
        Some(r) => r,
        None => return "missing 'replacement' argument".to_string(),
    };
    let target_path = match sanitize_path(workspace, path_str) {
        Ok(p) => p,
        Err(e) => return e,
    };
    if !target_path.exists() {
        return format!("file '{}' does not exist", path_str);
    }
    let content = match fs::read_to_string(&target_path) {
        Ok(c) => c,
        Err(e) => return format!("failed to read '{}': {e}", path_str),
    };

    let count = content.matches(target).count();
    if count == 0 {
        return format!("target substring not found in '{}'", path_str);
    }
    if count > 1 {
        return format!(
            "target matches {count} occurrences in '{}'; make target more specific to match exactly once",
            path_str
        );
    }

    let updated = content.replace(target, replacement);
    match fs::write(&target_path, updated) {
        Ok(()) => format!("successfully replaced target in '{}'", path_str),
        Err(e) => format!("failed to write '{}': {e}", path_str),
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

    let mut command = if cfg!(windows) {
        let mut c = Command::new("powershell");
        c.arg("-NoProfile").arg("-Command").arg(cmd_str);
        c
    } else {
        let mut c = Command::new("sh");
        c.arg("-c").arg(cmd_str);
        c
    };
    command.current_dir(workspace);
    command.stdin(Stdio::null());

    match run_with_deadline(command) {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            let code = out.status.code().unwrap_or(-1);

            let mut combined = String::new();
            if !stdout.is_empty() {
                combined.push_str(&stdout);
            }
            if !stderr.is_empty() {
                if !combined.is_empty() {
                    combined.push('\n');
                }
                combined.push_str("[stderr]\n");
                combined.push_str(&stderr);
            }
            // Cap output at 32KB
            if combined.len() > 32 * 1024 {
                let tail = &combined[combined.len() - 32 * 1024..];
                format!("exit code: {code}\n[truncated...]\n{tail}")
            } else {
                format!("exit code: {code}\n{combined}")
            }
        }
        Err(e) => format!("failed to run command '{cmd_str}': {e}"),
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
    let mut child = command
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
        let _ = Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .output();
    }
    #[cfg(not(windows))]
    {
        let _ = Command::new("kill")
            .args(["-TERM", &format!("-{pid}")])
            .output();
    }
}

/// True when any command-position word in `command` invokes the susi CLI. A
/// worker that runs `susi workflow finish` holds its own turn while the
/// harness's close machinery runs, and the adapter then fails behind it.
fn runs_susi(command: &str) -> bool {
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
            first == "susi"
                || first == "susi.exe"
                || first.ends_with("\\susi.exe")
                || first.ends_with("/susi.exe")
        })
}

fn tool_list_dir(workspace: &Path, args: &Value) -> String {
    let path_str = args
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    let target_dir = match sanitize_path(workspace, path_str) {
        Ok(p) => p,
        Err(e) => return e,
    };
    if !target_dir.exists() {
        return format!("directory '{}' does not exist", path_str);
    }
    let read_dir = match fs::read_dir(&target_dir) {
        Ok(rd) => rd,
        Err(e) => return format!("failed to read directory: {e}"),
    };

    let mut entries = Vec::new();
    for entry in read_dir.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
        if is_dir {
            entries.push(format!("{name}/"));
        } else {
            entries.push(name);
        }
    }
    entries.sort();
    if entries.is_empty() {
        "(empty directory)".to_string()
    } else {
        entries.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_registry_lists_the_five_native_tools() {
        let registry = Registry::builtin();
        assert_eq!(registry.tools().len(), 5);
        for name in [
            "view_file",
            "write_to_file",
            "replace_file_content",
            "run_command",
            "list_dir",
        ] {
            assert!(registry.get(name).is_some(), "{name} is missing");
        }
        let declarations = registry.declarations();
        assert_eq!(declarations.as_array().unwrap().len(), 5);
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
    fn tool_registry_executes_write_view_and_replace() {
        let temp = std::env::temp_dir().join(format!("susi-tools-{}", std::process::id()));
        let _ = fs::create_dir_all(&temp);
        let registry = Registry::builtin();

        let written = registry.execute(
            &temp,
            "write_to_file",
            &json!({"path": "test.txt", "content": "line1\nline2\nline3"}).to_string(),
            &[],
        );
        assert!(written.contains("successfully wrote"), "{written}");

        let viewed = registry.execute(
            &temp,
            "view_file",
            &json!({"path": "test.txt", "start_line": 2, "end_line": 2}).to_string(),
            &[],
        );
        assert!(viewed.contains("line2"));
        assert!(!viewed.contains("line1"));

        let replaced = registry.execute(
            &temp,
            "replace_file_content",
            &json!({"path": "test.txt", "target": "line2", "replacement": "line_two"}).to_string(),
            &[],
        );
        assert!(replaced.contains("successfully replaced"), "{replaced}");

        let listed = registry.execute(&temp, "list_dir", "{}", &[]);
        assert!(listed.contains("test.txt"));
        let _ = fs::remove_dir_all(&temp);
    }

    #[test]
    fn tool_registry_blocks_path_traversal() {
        let ws = Path::new("test_ws");
        assert!(sanitize_path(ws, "../outside").is_err());
        assert!(sanitize_path(ws, "sub/../../outside").is_err());
        assert!(sanitize_path(ws, "safe/nested/file.rs").is_ok());
        let output = Registry::builtin().execute(ws, "view_file", r#"{"path":"../secret"}"#, &[]);
        assert!(output.contains("traversal"), "{output}");
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
    fn a_susi_invocation_at_command_position_is_refused() {
        assert!(runs_susi("susi workflow finish T-X"));
        assert!(runs_susi("cd wt; susi tasks close T-X"));
        assert!(runs_susi(
            "cargo test -p x && \"C:\\bin\\susi.exe\" workflow sync"
        ));
        assert!(!runs_susi("cargo test -p susi-harness"));
        assert!(!runs_susi("git grep -n 'susi workflow'"));
        assert!(!runs_susi("cargo test --test gmcp_requirements gmcp_req_1"));
    }
}
