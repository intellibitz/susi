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
        // Dual-mount: ToolRegistry dispatch + CapabilityRegistry catalog.
        struct CapTool {
            name: String,
            desc: String,
            handler: crate::types::MetaToolHandler,
        }
        impl susi_core::registry::Tool for CapTool {
            fn name(&self) -> &str {
                &self.name
            }
            fn description(&self) -> &str {
                &self.desc
            }
            fn execute(&self, args: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
                (self.handler)(args, workspace)
            }
        }
        susi_core::registry::CapabilityRegistry::global().register_tool(CapTool {
            name: name.to_string(),
            desc: desc.to_string(),
            handler: tool.handler.clone(),
        });
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

        // Pillar 8: surface live-discovered MCP tools from CapabilityRegistry
        let caps = susi_core::registry::CapabilityRegistry::global();
        for name in caps.list_tools() {
            if let Some(tool) = caps.get_tool(&name) {
                tools.push(McpTool {
                    name,
                    description: tool.description().to_string(),
                });
            }
        }

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
        if susi_core::registry::CapabilityRegistry::global()
            .get_tool(name)
            .is_some()
        {
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
        // Prefer CapabilityRegistry for discovered MCP tools so swarm dispatch
        // uses the same hot-plugged catalog as zero-config bootstrap.
        if let Some(tool) = susi_core::registry::CapabilityRegistry::global().get_tool(name) {
            return match tool.execute(arg, workspace) {
                Ok(res) => res,
                Err(e) => format!("{}", e),
            };
        }

        if name.contains(':') && !name.starts_with("ext_") {
            let parts: Vec<&str> = name.splitn(2, ':').collect();
            let arg_str = if let Some(s) = arg.as_str() {
                s.to_string()
            } else {
                arg.to_string()
            };
            return susi_core::capture::EvidenceSession::capture_call(name, arg, workspace, || {
                GmcpClient::execute_external_tool_result(parts[0], parts[1], &arg_str)
            })
            .unwrap_or_else(|error| error.to_string());
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
                match susi_core::capture::EvidenceSession::capture_call(
                    name,
                    arg,
                    workspace,
                    || susi_native::wasm::WasmHost::execute_untrusted_wasm(&wasm_path, &arg_str),
                ) {
                    Ok(res) => return res,
                    Err(e) => return format!("Reflex Error: {}", e),
                }
            }
        }

        let registry = Self::global();
        let tool = registry
            .tools
            .get(name)
            .map(|entry| Arc::clone(entry.value()));
        if let Some(tool) = tool {
            match susi_core::capture::EvidenceSession::capture_call(name, arg, workspace, || {
                tool.execute(arg, workspace)
            }) {
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
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Self::global().try_acquire_local_lock(resource_id, now)
    }

    fn try_acquire_local_lock(&self, resource_id: &str, now: u64) -> bool {
        const LEASE_SECS: u64 = 300;
        match self.locks.entry(resource_id.to_owned()) {
            dashmap::mapref::entry::Entry::Occupied(mut entry) => {
                // A backwards clock adjustment must not expire an active lease.
                if now.saturating_sub(*entry.get()) < LEASE_SECS {
                    return false;
                }
                entry.insert(now);
            }
            dashmap::mapref::entry::Entry::Vacant(entry) => {
                entry.insert(now);
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> ToolRegistry {
        ToolRegistry {
            tools: DashMap::new(),
            locks: DashMap::new(),
        }
    }

    #[test]
    fn lease_expires_at_boundary_and_tolerates_clock_rollback() {
        let registry = registry();
        assert!(registry.try_acquire_local_lock("resource", 100));
        assert!(!registry.try_acquire_local_lock("resource", 99));
        assert!(!registry.try_acquire_local_lock("resource", 399));
        assert!(registry.try_acquire_local_lock("resource", 400));
        assert!(!registry.try_acquire_local_lock("resource", 400));
        assert!(registry.try_acquire_local_lock("another", 400));
    }

    #[test]
    fn concurrent_callers_have_one_lease_owner() {
        let registry = registry();
        let barrier = std::sync::Barrier::new(16);
        std::thread::scope(|scope| {
            let handles: Vec<_> = (0..16)
                .map(|_| {
                    scope.spawn(|| {
                        barrier.wait();
                        registry.try_acquire_local_lock("resource", 100)
                    })
                })
                .collect();
            let winners = handles
                .into_iter()
                .filter_map(|handle| handle.join().unwrap().then_some(()))
                .count();
            assert_eq!(winners, 1);
        });
    }

    struct MockCapabilityTool;

    impl susi_core::registry::Tool for MockCapabilityTool {
        fn name(&self) -> &str {
            "mock_mcp:echo"
        }
        fn description(&self) -> &str {
            "test tool"
        }
        fn execute(&self, _args: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
            Ok("from-capability-registry".to_string())
        }
    }

    #[test]
    fn execute_tool_routes_discovered_mcp_via_capability_registry() {
        susi_core::registry::CapabilityRegistry::global().register_tool(MockCapabilityTool);
        assert!(ToolRegistry::exists("mock_mcp:echo"));
        let out =
            ToolRegistry::execute_tool("mock_mcp:echo", &serde_json::json!({}), Path::new("."));
        assert_eq!(out, "from-capability-registry");
        let listed = ToolRegistry::list_tools();
        assert!(
            listed.iter().any(|t| t.name == "mock_mcp:echo"),
            "discovered tool must appear in list_tools"
        );
    }
}
