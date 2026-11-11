// GMCP Universal Client: Bridges SUSI to Industry Protocol Standard MCP Servers
// 100% Rust implementation for Meta-Orchestrated Multi-Server Substrates

use serde_json::json;
use std::collections::HashMap;

use crate::config::{GlobalMcpEntry, McpConfig, McpServerConfig};
use crate::hooks::hooks;
use crate::types::McpTool;
use std::{fs, path::PathBuf};

pub struct GmcpClient;

impl GmcpClient {
    pub fn get_config_path() -> PathBuf {
        let susi_dir = susi_paths::SusiDirs::config_dir();
        if !susi_dir.exists() {
            let _ = fs::create_dir_all(&susi_dir);
        }
        susi_dir.join("mcp_config.json")
    }

    /// List all externally configured tools via dynamic mcp_config.json.
    /// Names are wildcards (`server:*`) — callers that dispatch tools use
    /// [`Self::list_managed_tools`], which substitutes live `server:tool` names.
    pub fn list_external_tools() -> Vec<McpTool> {
        wildcard_tools()
    }

    /// Tools the swarm can call, without waiting on any external server.
    ///
    /// A fresh probe cache answers directly. Otherwise the last probe for the
    /// current `mcp_config.json` (or, before any probe, one `server:*`
    /// wildcard per server) answers now and a single background probe
    /// refreshes the cache. Live probing spawns every configured server and
    /// waited up to 12 s, which made the first MCP `tools/list` on a fresh
    /// host take 10.3 s.
    pub fn list_managed_tools() -> Vec<McpTool> {
        if cfg!(test) {
            return Self::list_external_tools();
        }
        if let Some(cached) = read_live_cache() {
            return cached;
        }
        refresh_live_cache_in_background();
        read_stale_live_cache().unwrap_or_else(wildcard_tools)
    }

    /// Tools the swarm can call. Live `server:tool` names replace `server:*`
    /// for every server that answers `tools/list`. Unreachable servers stay
    /// as a wildcard so they remain visible. The probe is cached against
    /// `mcp_config.json`'s mtime so a tools/list does not respawn every server.
    /// Blocks on the probe; dispatch paths that must resolve a concrete
    /// `server:tool` name use this.
    pub fn list_managed_tools_blocking() -> Vec<McpTool> {
        if cfg!(test) {
            return Self::list_external_tools();
        }
        if let Some(cached) = read_live_cache() {
            return cached;
        }
        let live = Self::discover_live_tools();
        let mut seen = std::collections::BTreeSet::new();
        let mut tools = Vec::new();
        for (server, tool) in &live {
            seen.insert(server.as_str());
            tools.push(tool.clone());
        }
        for wild in wildcard_tools() {
            let server = wild
                .name
                .split_once(':')
                .map(|(name, _)| name)
                .unwrap_or("");
            if !seen.contains(server) {
                tools.push(wild);
            }
        }
        if !live.is_empty() {
            let _ = write_live_cache(&tools);
        }
        tools
    }

    /// Live-probe each configured MCP server and return real tool names
    /// (`server:tool`) with descriptions. Unreachable servers are skipped.
    pub fn discover_live_tools() -> Vec<(String, McpTool)> {
        let config_path = Self::get_config_path();
        let content = match fs::read_to_string(&config_path) {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };
        let config: McpConfig = match serde_json::from_str(&content) {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };

        let servers: Vec<(String, McpServerConfig)> = config.mcp_servers.into_iter().collect();
        let (tx, rx) = std::sync::mpsc::channel();
        for (server_name, srv) in servers {
            let tx = tx.clone();
            // Detached on purpose: tools/list must return inside the MCP HTTP
            // read budget. Slow servers stay as wildcards; a later `server:*`
            // call probes that one server.
            let _probe = std::thread::Builder::new()
                .name("mcp-probe".into())
                .spawn(move || {
                    let result = crate::mcp_budget::list_tools(srv);
                    let _ = tx.send((server_name, result));
                });
        }
        drop(tx);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(12);
        let mut discovered = Vec::new();
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            match rx.recv_timeout(remaining) {
                Ok((server_name, Ok(tools))) => {
                    for (tool_name, description) in tools {
                        discovered.push((
                            server_name.clone(),
                            McpTool {
                                name: format!("{server_name}:{tool_name}"),
                                description,
                                input_schema: None,
                            },
                        ));
                    }
                }
                Ok((server_name, Err(e))) => {
                    if std::env::var("SUSI_VERBOSE").is_ok() {
                        eprintln!(
                            "[AUTODISCOVER] MCP server '{server_name}' tool probe skipped: {e}"
                        );
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        discovered
    }

    /// The MCP scout registry, in precedence order: the active extension
    /// pack's `mcp-registry.json`; else an existing host
    /// `global_mcp_registry.json` (an operator override, or a cache an
    /// earlier core wrote); else the host config's own
    /// `bootstrap_mcp_servers`. Core bundles no registry, so with none of
    /// these it is empty.
    ///
    /// Read-only: it never writes `global_mcp_registry.json`, never
    /// refreshes it from a remote URL and never provisions an entry on its
    /// own (zero-UI plan A8) — a server runs only after an explicit
    /// `susi mcp-add` / `susi mcp enable`.
    pub fn fetch_global_registry() -> Vec<GlobalMcpEntry> {
        let from_pack: Vec<GlobalMcpEntry> =
            crate::susi_sandbox::extensions::load_json_or_bundled("mcp-registry.json", "[]");
        if !from_pack.is_empty() {
            return from_pack;
        }
        let config_dir = susi_paths::SusiDirs::config_dir();
        if let Ok(bytes) = fs::read(config_dir.join("global_mcp_registry.json")) {
            if let Ok(entries) = serde_json::from_slice::<Vec<GlobalMcpEntry>>(&bytes) {
                return entries;
            }
        }
        crate::susi_sandbox::manager::SusiConfig::load(&config_dir)
            .unwrap_or_default()
            .bootstrap_mcp_servers::<Vec<GlobalMcpEntry>>()
    }

    /// Load `mcp_config.json` for a read-modify-write. Absent means no
    /// servers yet. A torn or unreadable file is an error: callers must not
    /// rewrite it as an empty config, which would drop every other server.
    pub(crate) fn read_mcp_config(path: &std::path::Path) -> Result<McpConfig, &'static str> {
        match fs::read_to_string(path) {
            Ok(content) => serde_json::from_str(&content).map_err(|_| "ERROR_CONFIG_DAMAGED"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(McpConfig {
                mcp_servers: HashMap::new(),
                extra: HashMap::new(),
            }),
            Err(_) => Err("ERROR_CONFIG_UNREADABLE"),
        }
    }

    /// Open MCP admission: mount any stdio command or HTTP MCP URL into
    /// `~/.susi/mcp_config.json` without requiring a scout preset.
    ///
    /// - `command_or_url` starting with `http://` or `https://` → streamable HTTP MCP
    /// - otherwise → stdio MCP (`command` + optional `args`)
    pub fn admit_mcp_server(name: &str, command_or_url: &str, args: &[String]) -> String {
        let name = name.trim();
        let command_or_url = command_or_url.trim();
        if name.is_empty() || command_or_url.is_empty() {
            return "ERROR_INVALID_ARGS".to_string();
        }
        let config_path = Self::get_config_path();
        let mut config = match Self::read_mcp_config(&config_path) {
            Ok(config) => config,
            Err(code) => return code.to_string(),
        };
        let new_srv = McpServerConfig {
            command: command_or_url.to_string(),
            args: args.to_vec(),
            env: None,
            extra: HashMap::new(),
        };
        config.mcp_servers.insert(name.to_string(), new_srv);
        if let Ok(updated) = serde_json::to_string_pretty(&config) {
            if crate::susi_config::atomic_write_bytes(&config_path, updated.as_bytes()).is_ok() {
                return "SUCCESS_ADMITTED".to_string();
            }
        }
        "ERROR_FAILED".to_string()
    }

    pub fn execute_external_tool(server_name: &str, tool_name: &str, args: &str) -> String {
        Self::execute_external_tool_result(server_name, tool_name, args)
            .unwrap_or_else(|error| format!("[FAIL] MCP: {error}"))
    }

    /// Preserve remote error status for evidence capture; legacy string callers
    /// can still use execute_external_tool above.
    pub fn execute_external_tool_result(
        server_name: &str,
        tool_name: &str,
        args: &str,
    ) -> crate::susi_error::EaiResult<String> {
        let config_content = fs::read_to_string(Self::get_config_path())
            .map_err(|e| crate::susi_error::EaiError::filesystem(e.to_string()))?;
        let config: McpConfig = serde_json::from_str(&config_content)
            .map_err(|e| crate::susi_error::EaiError::protocol(e.to_string()))?;
        let srv = config
            .mcp_servers
            .get(server_name)
            .ok_or_else(|| crate::susi_error::EaiError::protocol("MCP server not configured"))?;
        if tool_name == "*" {
            return match crate::mcp_budget::list_tools(srv.clone()) {
                Ok(tools) if !tools.is_empty() => {
                    let names = tools
                        .iter()
                        .map(|(name, _)| format!("{server_name}:{name}"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    Ok(format!(
                        "wildcard is not a callable tool; use one of: {names}"
                    ))
                }
                Ok(_) => Err(crate::susi_error::EaiError::protocol(format!(
                    "MCP server '{server_name}' advertised no tools"
                ))),
                Err(e) => Err(crate::susi_error::EaiError::protocol(e)),
            };
        }
        let arguments = if tool_name == "reason" {
            json!({"intent": args, "workspace_context": Self::gather_workspace_context()})
        } else {
            serde_json::from_str(args).unwrap_or_else(|_| json!({"input":args}))
        };
        crate::mcp_budget::call(srv.clone(), tool_name.to_owned(), arguments)
            .map_err(crate::susi_error::EaiError::protocol)
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
        // Through SusiDirs, never a hardcoded `~/.susi`: creating that dir
        // on an XDG install would switch later processes to the legacy layout.
        let registry_path = susi_paths::SusiDirs::cache_dir().join("mcp_web_registry.json");

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

        let _ = crate::susi_config::atomic_write_bytes(
            &registry_path,
            serde_json::to_string_pretty(&entries)
                .unwrap_or_default()
                .as_bytes(),
        );
        entries
    }

    fn benchmark_server(name: &str, package: &str) -> (f32, u64) {
        // Protocol Compliance Benchmarking
        let start = std::time::Instant::now();

        // This is launcher availability only, not a server handshake.
        if package_runner(package)
            .and_then(|(runner, _)| crate::leading_mcp::resolve_runner(runner))
            .is_none()
        {
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

fn package_runner(package: &str) -> Option<(&str, &str)> {
    let (runner, package) = if let Some(package) = package.strip_prefix("pypi:") {
        ("uvx", package)
    } else if let Some(package) = package.strip_prefix("npm:") {
        ("npx", package)
    } else if package.starts_with("ghcr.io/") || package.contains(':') {
        // Container/remote transports belong to explicit MCP admission or
        // the leading catalog, never an invented npm invocation.
        return None;
    } else if package.contains("python") {
        ("uvx", package)
    } else {
        ("npx", package)
    };
    (!package.trim().is_empty() && !package.starts_with('-')).then_some((runner, package))
}

const LIVE_CACHE_TTL_SECS: u64 = 600;

#[derive(serde::Serialize, serde::Deserialize)]
struct LiveToolCache {
    config_mtime_secs: u64,
    probed_at_secs: u64,
    tools: Vec<McpTool>,
}

fn unix_secs(time: std::time::SystemTime) -> u64 {
    time.duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn now_secs() -> u64 {
    unix_secs(std::time::SystemTime::now())
}

fn config_mtime_secs() -> Option<u64> {
    fs::metadata(GmcpClient::get_config_path())
        .and_then(|meta| meta.modified())
        .ok()
        .map(unix_secs)
}

fn live_cache_path() -> PathBuf {
    susi_paths::SusiDirs::cache_dir().join("mcp_live_tools.json")
}

fn read_live_cache() -> Option<Vec<McpTool>> {
    let mtime = config_mtime_secs()?;
    let content = fs::read_to_string(live_cache_path()).ok()?;
    let cache: LiveToolCache = serde_json::from_str(&content).ok()?;
    let fresh = cache.config_mtime_secs == mtime
        && now_secs().saturating_sub(cache.probed_at_secs) < LIVE_CACHE_TTL_SECS
        && !cache.tools.is_empty();
    fresh.then_some(cache.tools)
}

/// The last probe for the current `mcp_config.json`, however old.
fn read_stale_live_cache() -> Option<Vec<McpTool>> {
    let mtime = config_mtime_secs()?;
    let content = fs::read_to_string(live_cache_path()).ok()?;
    let cache: LiveToolCache = serde_json::from_str(&content).ok()?;
    (cache.config_mtime_secs == mtime && !cache.tools.is_empty()).then_some(cache.tools)
}

/// At most one background probe per process at a time.
fn refresh_live_cache_in_background() {
    use std::sync::atomic::{AtomicBool, Ordering};
    static REFRESHING: AtomicBool = AtomicBool::new(false);
    if REFRESHING.swap(true, Ordering::AcqRel) {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("mcp-probe-refresh".into())
        .spawn(|| {
            let _ = GmcpClient::list_managed_tools_blocking();
            REFRESHING.store(false, Ordering::Release);
        });
    if spawned.is_err() {
        REFRESHING.store(false, Ordering::Release);
    }
}

fn write_live_cache(tools: &[McpTool]) -> std::io::Result<()> {
    let Some(mtime) = config_mtime_secs() else {
        return Ok(());
    };
    let cache = LiveToolCache {
        config_mtime_secs: mtime,
        probed_at_secs: now_secs(),
        tools: tools.to_vec(),
    };
    let body = serde_json::to_string(&cache).map_err(std::io::Error::other)?;
    crate::susi_config::atomic_write_bytes(&live_cache_path(), body.as_bytes())
}

fn wildcard_tools() -> Vec<McpTool> {
    let mut tools = Vec::new();
    let config_path = GmcpClient::get_config_path();
    if let Ok(content) = fs::read_to_string(&config_path) {
        if let Ok(config) = serde_json::from_str::<McpConfig>(&content) {
            for (name, _srv) in config.mcp_servers {
                tools.push(McpTool {
                    name: format!("{name}:*"),
                    description: format!("Dynamic Proxy for standard MCP server: {name}"),
                    input_schema: None,
                });
            }
        }
    }
    tools
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_runner_selects_only_the_matching_launcher() {
        assert_eq!(
            package_runner("pypi:server-test"),
            Some(("uvx", "server-test"))
        );
        assert_eq!(
            package_runner("npm:@example/python-tools"),
            Some(("npx", "@example/python-tools"))
        );
        // Container/remote transports are never turned into an npm call.
        assert_eq!(package_runner("ghcr.io/github/github-mcp-server"), None);
        assert_eq!(package_runner("npm:-rf"), None);
    }

    /// A registry read writes no `global_mcp_registry.json` (so it never
    /// freezes the registry), and an extension pack installed and loaded
    /// afterwards supplies the rows.
    #[test]
    fn scout_registry_reads_write_nothing_and_follow_the_loaded_pack() {
        let Some(dir) = crate::private_config_fixture() else {
            return;
        };
        let host_file = dir.join("global_mcp_registry.json");
        assert!(GmcpClient::fetch_global_registry().is_empty());
        assert!(!host_file.exists(), "a registry read must write nothing");
        let pack = dir.join("extensions").join("acme");
        fs::create_dir_all(&pack).unwrap();
        fs::write(
            pack.join("manifest.json"),
            r#"{"id":"acme","name":"Acme","version":"0.0.1",
                "files":{"mcp-registry.json":"mcp-registry.json"}}"#,
        )
        .unwrap();
        fs::write(
            pack.join("mcp-registry.json"),
            r#"[{"name":"acme-mcp","description":"Acme","package":"npm:@acme/mcp","category":"reasoning"}]"#,
        )
        .unwrap();
        crate::susi_sandbox::extensions::load_pack("acme").unwrap();
        let names: Vec<String> = GmcpClient::fetch_global_registry()
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, ["acme-mcp"]);
        assert!(!host_file.exists(), "a registry read must write nothing");
    }

    #[test]
    fn core_bundles_no_mcp_scout_registry() {
        // Zero-UI plan A9: the scout registry ships in an extension pack;
        // with none installed and no host-listed bootstrap servers, it is
        // empty rather than a compiled vendor catalog.
        assert!(crate::susi_sandbox::manager::SusiConfig::default()
            .bootstrap_mcp_servers::<Vec<GlobalMcpEntry>>()
            .is_empty());
    }

    #[test]
    fn admit_mcp_server_writes_http_and_stdio() {
        let Some(dir) = crate::private_config_fixture() else {
            return;
        };
        let res = GmcpClient::admit_mcp_server("remote", "http://127.0.0.1:3100/mcp", &[]);
        assert_eq!(res, "SUCCESS_ADMITTED");
        let res2 = GmcpClient::admit_mcp_server(
            "fs",
            "npx",
            &[
                "-y".into(),
                "@modelcontextprotocol/server-filesystem".into(),
            ],
        );
        assert_eq!(res2, "SUCCESS_ADMITTED");
        assert_eq!(GmcpClient::get_config_path(), dir.join("mcp_config.json"));
        let tools = GmcpClient::list_external_tools();
        let names: Vec<_> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.iter().any(|n| n.starts_with("remote:")), "{names:?}");
        assert!(names.iter().any(|n| n.starts_with("fs:")), "{names:?}");
    }

    #[test]
    fn admit_refuses_damaged_config_without_erasing_servers() {
        if crate::private_config_fixture().is_none() {
            return;
        }
        let path = GmcpClient::get_config_path();
        let original = "{\"mcp_servers\":{\"keep\":{\"command\":\"echo\",\"args\":[]}},\"torn\"";
        fs::write(&path, original).unwrap();
        let res = GmcpClient::admit_mcp_server("added", "npx", &["-y".into()]);
        assert_eq!(res, "ERROR_CONFIG_DAMAGED");
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
    }

    #[test]
    fn admit_refuses_unreadable_config_without_replacing_it() {
        if crate::private_config_fixture().is_none() {
            return;
        }
        let path = GmcpClient::get_config_path();
        fs::create_dir(&path).unwrap();
        let res = GmcpClient::admit_mcp_server("added", "npx", &[]);
        assert_eq!(res, "ERROR_CONFIG_UNREADABLE");
        assert!(
            path.is_dir(),
            "unreadable config must not be replaced by a file"
        );
    }
}
