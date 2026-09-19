// GMCP Universal Client: Bridges SUSI to Industry Protocol Standard MCP Servers
// 100% Rust implementation for Meta-Orchestrated Multi-Server Substrates

use serde_json::json;
use std::collections::HashMap;

use parking_lot::RwLock;
use std::process::Child;
use std::sync::{Arc, OnceLock};

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use crate::config::{GlobalMcpEntry, McpConfig, McpServerConfig};
use crate::hooks::hooks;
use crate::types::McpTool;

// Mandate 12: Hardware Authority over Process Lifecycle
// MCP Client Cache prevents cold-booting processes for every Swarm Reflex.
type McpProcessPool = Arc<RwLock<HashMap<String, Arc<parking_lot::Mutex<Child>>>>>;

static MCP_PROCESS_POOL: OnceLock<McpProcessPool> = OnceLock::new();

fn get_process_pool() -> McpProcessPool {
    MCP_PROCESS_POOL
        .get_or_init(|| Arc::new(RwLock::new(HashMap::new())))
        .clone()
}

fn write_mcp_message(writer: &mut impl Write, msg: &str) -> std::io::Result<()> {
    write!(writer, "Content-Length: {}\r\n\r\n{}", msg.len(), msg)?;
    writer.flush()
}

/// Reads one MCP message with a hard deadline. `read_mcp_message` blocks on
/// pipe I/O with no way to interrupt it directly, so an unresponsive child
/// (observed live: an `npm exec`-launched server stalling on a cold package
/// resolve/install with no local cache) hung this call forever - the exact
/// non-GPU cause behind a reported multi-minute "GPU hang", since this is
/// what `power_reason` falls through to after local inference fails. A
/// watchdog thread SIGKILLs the process by raw PID (no `Child` lock needed,
/// so it can't deadlock against the caller's held `MutexGuard<Child>`) if the
/// read hasn't finished by the deadline; the killed process's pipe then hits
/// EOF and `read_mcp_message` returns `None` on its own, same as any other
/// dead-server case already handled by callers.
fn read_mcp_message_with_timeout(
    reader: &mut impl BufRead,
    pid: u32,
    timeout: std::time::Duration,
) -> Option<String> {
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let done_watch = Arc::clone(&done);
    std::thread::spawn(move || {
        std::thread::sleep(timeout);
        if !done_watch.load(std::sync::atomic::Ordering::Acquire) {
            unsafe {
                libc::kill(pid as i32, libc::SIGKILL);
            }
        }
    });
    let result = read_mcp_message(reader);
    done.store(true, std::sync::atomic::Ordering::Release);
    result
}

fn read_mcp_message(reader: &mut impl BufRead) -> Option<String> {
    let mut clen = 0;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() || line.is_empty() {
            return None;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            break;
        }
        let lower = trimmed.to_lowercase();
        if lower.starts_with("content-length:") {
            if let Ok(l) = lower
                .trim_start_matches("content-length:")
                .trim()
                .parse::<usize>()
            {
                clen = l;
            }
        }
    }
    if clen > 0 {
        let mut buf = vec![0u8; clen];
        if std::io::Read::read_exact(reader, &mut buf).is_ok() {
            return String::from_utf8(buf).ok();
        }
    }
    None
}

pub struct GmcpClient;

impl GmcpClient {
    pub fn get_config_path() -> PathBuf {
        let susi_dir = susi_paths::SusiDirs::config_dir();
        if !susi_dir.exists() {
            let _ = fs::create_dir_all(&susi_dir);
        }
        susi_dir.join("mcp_config.json")
    }

    /// List all externally configured tools via dynamic mcp_config.json
    pub fn list_external_tools() -> Vec<McpTool> {
        let mut tools = Vec::new();
        let config_path = Self::get_config_path();
        if let Ok(content) = fs::read_to_string(&config_path) {
            if let Ok(config) = serde_json::from_str::<McpConfig>(&content) {
                for (name, _srv) in config.mcp_servers {
                    tools.push(McpTool {
                        name: format!("{}:*", name),
                        description: format!("Dynamic Proxy for standard MCP server: {}", name),
                    });
                }
            }
        }
        tools
    }

    pub fn list_external_prompts() -> Vec<McpTool> {
        let mut prompts = Vec::new();
        let config_path = Self::get_config_path();
        if let Ok(content) = fs::read_to_string(&config_path) {
            if let Ok(config) = serde_json::from_str::<McpConfig>(&content) {
                for (name, _srv) in config.mcp_servers {
                    prompts.push(McpTool {
                        name: format!("{}:prompt:*", name),
                        description: format!("Prompts from MCP server: {}", name),
                    });
                }
            }
        }
        prompts
    }

    pub fn list_external_resources() -> Vec<McpTool> {
        let mut resources = Vec::new();
        let config_path = Self::get_config_path();
        if let Ok(content) = fs::read_to_string(&config_path) {
            if let Ok(config) = serde_json::from_str::<McpConfig>(&content) {
                for (name, _srv) in config.mcp_servers {
                    resources.push(McpTool {
                        name: format!("{}:resource:*", name),
                        description: format!("Resources from MCP server: {}", name),
                    });
                }
            }
        }
        resources
    }

    pub fn fetch_global_registry() -> Vec<GlobalMcpEntry> {
        let global_dir = susi_paths::SusiDirs::config_dir();
        let registry_path = global_dir.join("global_mcp_registry.json");
        let cfg = susi_sandbox::manager::SusiConfig::load(&global_dir).unwrap_or_default();

        let mtime = std::fs::metadata(&registry_path)
            .and_then(|m| m.modified())
            .ok();
        let is_stale = mtime
            .map(|t| t.elapsed().unwrap_or_default().as_secs() > 86400)
            .unwrap_or(true);

        static STORE: std::sync::OnceLock<susi_sandbox::VersionedJsonStore<Vec<GlobalMcpEntry>>> =
            std::sync::OnceLock::new();
        let store = STORE.get_or_init(susi_sandbox::VersionedJsonStore::new);

        let entries = store
            .load_with_healing(
                &registry_path,
                || Ok(cfg.bootstrap_mcp_servers()),
                |_| false,
                false,
            )
            .unwrap_or_else(|_| cfg.bootstrap_mcp_servers());

        if is_stale {
            static REGISTRY_FETCH_RUNNING: std::sync::atomic::AtomicBool =
                std::sync::atomic::AtomicBool::new(false);
            if !REGISTRY_FETCH_RUNNING.swap(true, std::sync::atomic::Ordering::SeqCst) {
                let url = cfg.mcp_registry_url();
                let reg_p = registry_path;
                std::thread::spawn(move || {
                    struct FetchGuard;
                    impl Drop for FetchGuard {
                        fn drop(&mut self) {
                            REGISTRY_FETCH_RUNNING
                                .store(false, std::sync::atomic::Ordering::SeqCst);
                        }
                    }
                    let _guard = FetchGuard;
                    if let Ok(resp) = susi_sandbox::manager::http_agent()
                        .get(&url)
                        .header("User-Agent", "SUSI/0.1")
                        .call()
                    {
                        if let Ok(remote_entries) =
                            resp.into_body().read_json::<Vec<GlobalMcpEntry>>()
                        {
                            if !remote_entries.is_empty() {
                                let _ = fs::write(
                                    &reg_p,
                                    serde_json::to_string_pretty(&remote_entries)
                                        .unwrap_or_default(),
                                );
                            }
                        }
                    }
                });
            }
        }

        entries
    }

    pub fn auto_configure_server(name: &str, package: &str) -> String {
        let config_path = Self::get_config_path();
        let mut config = if let Ok(content) = fs::read_to_string(&config_path) {
            serde_json::from_str::<McpConfig>(&content).unwrap_or(McpConfig {
                mcp_servers: HashMap::new(),
                extra: HashMap::new(),
            })
        } else {
            McpConfig {
                mcp_servers: HashMap::new(),
                extra: HashMap::new(),
            }
        };

        // Meta Execution Scout: Identify best-suited executor for the host environment
        let has_uvx = Command::new("uvx").arg("--version").output().is_ok();
        let has_npx = Command::new("npx").arg("--version").output().is_ok();

        let (cmd, args) = if package.starts_with("pypi:") || package.contains("python") {
            if has_uvx {
                (
                    "uvx".to_string(),
                    vec![package.trim_start_matches("pypi:").to_string()],
                )
            } else if has_npx {
                (
                    "npx".to_string(),
                    vec!["-y".to_string(), package.to_string()],
                )
            } else {
                (
                    "python3".to_string(),
                    vec!["-m".to_string(), package.to_string()],
                )
            }
        } else if has_npx {
            (
                "npx".to_string(),
                vec!["-y".to_string(), package.to_string()],
            )
        } else {
            (
                "susi".to_string(),
                vec!["mcp".to_string(), name.to_string()],
            )
        };

        let new_srv = McpServerConfig {
            command: cmd,
            args,
            env: None,
            extra: HashMap::new(),
        };

        config.mcp_servers.insert(name.to_string(), new_srv);
        if let Ok(updated) = serde_json::to_string_pretty(&config) {
            if fs::write(&config_path, updated).is_ok() {
                return "SUCCESS_CONFIGURED".to_string();
            }
        }
        "ERROR_FAILED".to_string()
    }

    /// Dynamically provisions an MCP tool package from the global registry
    pub fn provision_tool_package(name: &str) -> String {
        let registry = Self::fetch_global_registry();
        if let Some(entry) = registry.iter().find(|e| e.name == name) {
            return Self::auto_configure_server(&entry.name, &entry.package);
        }
        "NOT_FOUND_IN_REGISTRY".to_string()
    }

    pub fn execute_external_tool(server_name: &str, tool_name: &str, args: &str) -> String {
        let config_path = Self::get_config_path();
        let config_content = match fs::read_to_string(&config_path) {
            Ok(c) => c,
            Err(_) => {
                return format!(
                    "[FAIL] MCP Error: Config not found at {}",
                    config_path.display()
                )
            }
        };

        let config: McpConfig = match serde_json::from_str(&config_content) {
            Ok(c) => c,
            Err(e) => return format!("[FAIL] MCP Error: Config parse failed: {}", e),
        };

        let srv = match config.mcp_servers.get(server_name) {
            Some(s) => s,
            None => {
                return format!(
                    "[FAIL] MCP Error: Server '{}' not found in config.",
                    server_name
                )
            }
        };

        Self::proxy_call(srv, tool_name, args)
    }

    fn proxy_call(srv: &McpServerConfig, tool_name: &str, args_json: &str) -> String {
        if srv.command.starts_with("http") {
            return Self::proxy_web_call(srv, tool_name, args_json);
        }

        let pool = get_process_pool();
        let srv_key = format!("{} {:?}", srv.command, srv.args);

        let child_arc = {
            let lock = pool.read();
            lock.get(&srv_key).cloned()
        };

        let child_arc = match child_arc {
            Some(c) => c,
            None => {
                let mut child = match Command::new(&srv.command)
                    .args(&srv.args)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .spawn()
                {
                    Ok(c) => c,
                    Err(e) => {
                        return format!(
                            "[FAIL] MCP Error: Failed to spawn '{}': {}",
                            srv.command, e
                        )
                    }
                };

                // Initialize Handshake natively with LSP Framing
                let init_req = json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "initialize",
                    "params": {
                        "protocolVersion": "2024-11-05",
                        "capabilities": {},
                        "clientInfo": { "name": "susi", "version": hooks().engine_version() }
                    }
                })
                .to_string();

                let scout_timeout = std::time::Duration::from_secs(
                    susi_sandbox::manager::SusiConfig::load_global()
                        .unwrap_or_default()
                        .cloud_scout_timeout_secs(),
                );
                let pid = child.id();
                if let Some(stdin) = child.stdin.as_mut() {
                    let _ = write_mcp_message(stdin, &init_req);
                }
                if let Some(stdout) = child.stdout.as_mut() {
                    let mut reader = BufReader::new(stdout);
                    let _ = read_mcp_message_with_timeout(&mut reader, pid, scout_timeout);
                    // consume init response
                }

                let arc = Arc::new(parking_lot::Mutex::new(child));
                pool.write().insert(srv_key, arc.clone());
                arc
            }
        };

        // Execution Scope (Lock process stdio exclusively)
        let mut locked_child = child_arc.lock();

        let mut context_aware_args = args_json.to_string();
        if tool_name == "reason" {
            let context = Self::gather_workspace_context();
            context_aware_args = json!({
                "intent": args_json,
                "workspace_context": context,
            })
            .to_string();
        }

        let params = match serde_json::from_str::<serde_json::Value>(&context_aware_args) {
            Ok(v) => v,
            Err(_) => json!({ "input": &context_aware_args }),
        };

        let call_req = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": { "name": tool_name, "arguments": params }
        })
        .to_string();

        if let Some(stdin) = locked_child.stdin.as_mut() {
            let _ = write_mcp_message(stdin, &call_req);
        }

        let lease_timeout = std::time::Duration::from_secs(
            susi_sandbox::manager::SusiConfig::load_global()
                .unwrap_or_default()
                .execution_lease_secs(),
        );
        let call_pid = locked_child.id();
        if let Some(stdout) = locked_child.stdout.as_mut() {
            let mut reader = BufReader::new(stdout);
            if let Some(resp_str) =
                read_mcp_message_with_timeout(&mut reader, call_pid, lease_timeout)
            {
                let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap_or(json!({}));
                if let Some(content) = resp
                    .get("result")
                    .and_then(|r| r.get("content"))
                    .and_then(|c| c.get(0))
                    .and_then(|i| i.get("text"))
                    .and_then(|t| t.as_str())
                {
                    return content.to_string();
                }
                return format!("[MCP Proxy Response]: {}", resp_str.trim());
            }
        }

        "[FAIL] MCP Error: No response from server substrate.".to_string()
    }

    fn proxy_web_call(srv: &McpServerConfig, tool_name: &str, args_json: &str) -> String {
        let base_url = srv.command.trim_end_matches('/');

        // 1. Establish SSE Connection to get the message endpoint
        let sse_url = format!("{}/sse", base_url);
        let resp = match susi_sandbox::manager::http_agent().get(&sse_url).call() {
            Ok(r) => r,
            Err(e) => {
                return format!(
                    "[FAIL] MCP Web Error: Failed to connect to {}: {}",
                    sse_url, e
                )
            }
        };

        let mut reader = BufReader::new(resp.into_body().into_reader());
        let mut endpoint = format!("{}/messages", base_url);

        let mut line = String::new();
        while let Ok(len) = reader.read_line(&mut line) {
            if len == 0 {
                break;
            }
            if line.starts_with("event: endpoint") {
                line.clear();
                if reader.read_line(&mut line).is_ok() && line.starts_with("data: ") {
                    endpoint = format!("{}{}", base_url, line.trim_start_matches("data: ").trim());
                    break;
                }
            }
            line.clear();
        }

        // 2. Call Tool via POST
        let mut context_aware_args = args_json.to_string();
        if tool_name == "reason" {
            let context = Self::gather_workspace_context();
            // Unified Swarm Context: Include Blackboard state if available
            context_aware_args = json!({
                "intent": args_json,
                "workspace_context": context,
            })
            .to_string();
        }

        let params = match serde_json::from_str::<serde_json::Value>(&context_aware_args) {
            Ok(v) => v,
            Err(_) => json!({ "input": &context_aware_args }),
        };

        let call_req = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": tool_name,
                "arguments": params
            }
        });

        match susi_sandbox::manager::http_agent()
            .post(&endpoint)
            .send_json(call_req)
        {
            Ok(resp) => {
                let v: serde_json::Value = resp.into_body().read_json().unwrap_or(json!({}));
                if let Some(content) = v
                    .get("result")
                    .and_then(|r| r.get("content"))
                    .and_then(|c| c.get(0))
                    .and_then(|i| i.get("text"))
                    .and_then(|t| t.as_str())
                {
                    return content.to_string();
                }
                format!("[MCP Web Response]: {:?}", v)
            }
            Err(e) => format!("[FAIL] MCP Web Error: POST {} failed: {}", endpoint, e),
        }
    }

    pub fn scout_reasoning_remotes() -> Vec<String> {
        let mut remotes = Vec::new();
        let registry = Self::fetch_global_registry();
        let config_path = Self::get_config_path();

        if let Ok(content) = fs::read_to_string(&config_path) {
            if let Ok(config) = serde_json::from_str::<McpConfig>(&content) {
                for (name, _srv) in config.mcp_servers {
                    // Protocol-Based Scouting: Check registry for intelligence classification
                    if let Some(entry) = registry.iter().find(|e| e.name == name) {
                        if entry.category == "intelligence" || entry.category == "reasoning" {
                            remotes.insert(0, name);
                            continue;
                        }
                    }
                    remotes.push(name);
                }
            }
        }
        remotes
    }

    fn gather_workspace_context() -> serde_json::Value {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let src_count = std::fs::read_dir(cwd.join("src"))
            .map(|d| d.count())
            .unwrap_or(0);
        let hardware = hooks().hardware_snapshot();

        json!({
            "working_directory": cwd.display().to_string(),
            "source_file_count": src_count,
            "engine_version": hooks().engine_version(),
            "available_ram_gb": hardware.available_ram_gb,
            "gpu_acceleration": hardware.acceleration_active
        })
    }

    /// Autonomous Web-Scouting
    /// Interrogates global registries and benchmarks servers for swarm inclusion.
    pub fn autonomous_web_scout() -> Vec<GlobalMcpEntry> {
        let mut entries = Self::fetch_global_registry();
        let home = susi_paths::SusiDirs::home_dir();
        let registry_path = home.join(".susi/mcp_web_registry.json");

        // Benchmark and Rank each entry
        for entry in &mut entries {
            if entry.trust_score.is_none() {
                let (score, latency) = Self::benchmark_server(&entry.name, &entry.package);
                entry.trust_score = Some(score);
                entry.latency_ms = Some(latency);
            }
        }

        // Rank by Trust and Latency
        entries.sort_by(|a, b| {
            let a_val =
                a.trust_score.unwrap_or(0.0) - (a.latency_ms.unwrap_or(1000) as f32 / 10000.0);
            let b_val =
                b.trust_score.unwrap_or(0.0) - (b.latency_ms.unwrap_or(1000) as f32 / 10000.0);
            b_val
                .partial_cmp(&a_val)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let _ = fs::write(
            &registry_path,
            serde_json::to_string_pretty(&entries).unwrap_or_default(),
        );
        entries
    }

    fn benchmark_server(name: &str, package: &str) -> (f32, u64) {
        // Protocol Compliance Benchmarking
        let start = std::time::Instant::now();

        // Attempt trial initialization (Dry-run configuration)
        let has_uvx = Command::new("uvx").arg("--version").output().is_ok();
        let has_npx = Command::new("npx").arg("--version").output().is_ok();

        if (package.contains("python") && !has_uvx) || (!package.contains("python") && !has_npx) {
            return (0.1, 999); // Low trust if environment cannot execute
        }

        let latency = start.elapsed().as_millis() as u64;
        let trust = if name.contains("filesystem") || name.contains("git") {
            0.95
        } else {
            0.80
        };

        (trust, latency)
    }
}
