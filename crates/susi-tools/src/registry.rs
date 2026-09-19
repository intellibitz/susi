use dashmap::DashMap;
use std::path::Path;
use std::sync::{Arc, OnceLock};
use susi_error::EaiResult;

use crate::client::GmcpClient;
use crate::hooks::hooks;
use crate::types::{McpTool, MetaCategory, MetaTool, SusiTool};

pub struct ToolRegistry {
    pub tools: DashMap<String, Arc<dyn SusiTool>>,
    pub locks: DashMap<String, u64>,
}

impl ToolRegistry {
    pub fn global() -> &'static Self {
        static REGISTRY: OnceLock<ToolRegistry> = OnceLock::new();
        REGISTRY.get_or_init(|| {
            let registry = ToolRegistry {
                tools: DashMap::new(),
                locks: DashMap::new(),
            };
            hooks().bootstrap_tools(&registry);
            registry
        })
    }

    /// Public so `EngineHooks::bootstrap_tools` implementations (in `gmcp`,
    /// where the concrete `CoreTools::*` handlers live) can register them.
    pub fn register_meta_tool<F>(
        registry: &ToolRegistry,
        name: &str,
        desc: &str,
        category: MetaCategory,
        handler: F,
    ) where
        F: Fn(&serde_json::Value, &Path) -> EaiResult<String> + Send + Sync + 'static,
    {
        let tool = MetaTool {
            tool_name: name.to_string(),
            tool_desc: desc.to_string(),
            category,
            handler: Arc::new(handler),
        };
        registry.tools.insert(name.to_string(), Arc::new(tool));
    }

    pub fn list_tools() -> Vec<McpTool> {
        let registry = Self::global();
        let mut tools: Vec<McpTool> = registry
            .tools
            .iter()
            .map(|r| McpTool {
                name: r.key().clone(),
                description: r.value().description(),
            })
            .collect();

        tools.extend(GmcpClient::list_external_tools());

        let reflex_dir = susi_paths::SusiDirs::data_dir().join("reflexes");
        if let Ok(entries) = std::fs::read_dir(&reflex_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|ext| ext == "wasm") {
                    if let Ok(name) = entry.file_name().into_string() {
                        tools.push(McpTool {
                            name: format!("reflex_{}", name.replace(".wasm", "")),
                            description: "Dynamic Wasm neural reflex tool".to_string(),
                        });
                    }
                }
            }
        }

        tools.sort_by(|a, b| a.name.cmp(&b.name));
        tools.dedup_by(|a, b| a.name == b.name);
        tools
    }

    pub fn exists(name: &str) -> bool {
        let registry = Self::global();
        if registry.tools.contains_key(name) {
            return true;
        }
        let lower_name = name.to_lowercase();
        if lower_name.contains(':')
            || lower_name.starts_with("ext_")
            || lower_name.starts_with("reflex_")
        {
            return Self::list_tools()
                .iter()
                .any(|t| t.name == name || t.name.starts_with(name));
        }
        false
    }

    pub fn execute_tool(name: &str, arg: &serde_json::Value, workspace: &Path) -> String {
        if name.contains(':') && !name.starts_with("ext_") {
            let parts: Vec<&str> = name.splitn(2, ':').collect();
            let arg_str = if let Some(s) = arg.as_str() {
                s.to_string()
            } else {
                arg.to_string()
            };
            return GmcpClient::execute_external_tool(parts[0], parts[1], &arg_str);
        }

        if name.starts_with("reflex_") {
            let wasm_name = format!("{}.wasm", name.trim_start_matches("reflex_"));
            let wasm_path = susi_paths::SusiDirs::data_dir()
                .join("reflexes")
                .join(wasm_name);
            if wasm_path.exists() {
                let arg_str = if let Some(s) = arg.as_str() {
                    s.to_string()
                } else {
                    arg.to_string()
                };
                match susi_native::wasm::WasmHost::execute_reflex(&wasm_path, &arg_str) {
                    Ok(res) => return res,
                    Err(e) => return format!("Reflex Error: {}", e),
                }
            }
        }

        let registry = Self::global();
        if let Some(tool) = registry.tools.get(name) {
            match tool.execute(arg, workspace) {
                Ok(res) => res,
                Err(e) => format!("{}", e),
            }
        } else {
            // Self-Healing Protocol: Attempt autonomous resolution
            if let Ok(provisioned_res) = Self::resolve_capability_gap(name, workspace) {
                if provisioned_res == "SUCCESS_CONFIGURED" {
                    return format!("[RECOVERY] Capability '{}' was missing and autonomously provisioned. Please retry the mission.", name);
                }
            }
            format!("[CAPABILITY_GAP] Tool '{}' missing from Meta-Substrate. Report to Substrate Swarm for native evolution.", name)
        }
    }

    /// Autonomous Capability Resolution
    pub fn resolve_capability_gap(name: &str, workspace: &Path) -> EaiResult<String> {
        let server_name = name.split(':').next().unwrap_or(name);

        // Proactive Semantic Scout (Tier 1 Hardening)
        // If the tool name isn't an exact match, we search for semantic overlaps in the registry
        let registry = GmcpClient::fetch_global_registry();
        if let Some(entry) = registry
            .iter()
            .find(|e| e.name == server_name || e.description.to_lowercase().contains(server_name))
        {
            return Ok(GmcpClient::auto_configure_server(
                &entry.name,
                &entry.package,
            ));
        }

        let res = GmcpClient::provision_tool_package(server_name);
        if res != "NOT_FOUND_IN_REGISTRY" {
            return Ok(res);
        }

        // VC-200-002 (ROADMAP.md): last-resort autonomous hot-patch - delegated
        // to the engine hooks (was a direct call to
        // gawd::reflex_synth::ReflexSynthesizer::synthesize_wasm_reflex; see
        // hooks.rs for why this crate can't depend on gawd directly).
        hooks().resolve_capability_gap(server_name, workspace)
    }

    pub fn acquire_meta_lock(resource_id: &str) -> bool {
        if !Self::acquire_local_lock(resource_id) {
            return false;
        }

        // Distributed Resource Sovereignty: Broadcast to peers
        if !hooks().broadcast_lock_request(resource_id) {
            Self::release_meta_lock(resource_id);
            return false;
        }

        true
    }

    pub fn acquire_local_lock(resource_id: &str) -> bool {
        let registry = Self::global();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        if let Some(timestamp) = registry.locks.get(resource_id) {
            // Lease-Based Timed Locks (300s TTL)
            if now - *timestamp < 300 {
                return false;
            }
        }
        registry.locks.insert(resource_id.to_string(), now);
        true
    }

    pub fn release_meta_lock(resource_id: &str) {
        let registry = Self::global();
        registry.locks.remove(resource_id);
    }

    /// Zero-Config Autonomous Tool Linking
    pub fn auto_link_essential_mcp_servers() {
        let registry = GmcpClient::fetch_global_registry();
        let config_path = GmcpClient::get_config_path();

        let config_exists = config_path.exists();
        let mut essential_found = false;

        if config_exists {
            if let Ok(content) = std::fs::read_to_string(&config_path) {
                if let Ok(config) = serde_json::from_str::<crate::config::McpConfig>(&content) {
                    essential_found = !config.mcp_servers.is_empty();
                }
            }
        }

        if !essential_found {
            if std::env::var("SUSI_VERBOSE").is_ok() {
                eprintln!(
                    "[GMCP] No external tools configured. Auto-linking essential substrates..."
                );
            }
            let essentials = [
                "brave_search",
                "filesystem",
                "google_search",
                "github",
                "google_maps",
            ];
            for e in essentials {
                if let Some(entry) = registry.iter().find(|r| r.name == e) {
                    let _res = GmcpClient::auto_configure_server(&entry.name, &entry.package);
                }
            }
        }
    }
}
