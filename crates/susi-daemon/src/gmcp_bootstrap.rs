//! Registers every CoreTools handler into a fresh ToolRegistry.

use susi_gmcp::tools::CoreTools;
use susi_tools::{MetaCategory, ToolRegistry};

type CoreToolHandler =
    fn(&serde_json::Value, &std::path::Path) -> susi_tools::susi_error::EaiResult<String>;

struct CoreTool {
    name: &'static str,
    description: &'static str,
    category: MetaCategory,
    handler: CoreToolHandler,
}

// The public name is the handler's identifier, so the dispatch target cannot
// drift independently of the catalog. All handlers share one registration
// implementation rather than a generic instantiation per adapter closure.
macro_rules! core_tools {
    ($($(#[$attr:meta])* $([$definition:ident])? $handler:ident: $category:ident => $description:literal;)*) => {
        const CORE_TOOLS: &[CoreTool] = &[
            $($(#[$attr])* CoreTool {
                name: stringify!($handler),
                description: $description,
                category: MetaCategory::$category,
                handler: CoreTools::$handler,
            },)*
        ];
    };
}

susi_gmcp::gmcp_catalog_groups!(core_tools, [
    status: SystemPrimitive => "SUSI Substrate status report";
    ledger_verify: SystemPrimitive => "Verify the pinned GMCP normative requirements ledger (VC-203-001)";
    identity: SystemPrimitive => "SUSI substrate identity report";
    sovereign_dashboard: SystemPrimitive => "Report on autonomous invisible work performed by the substrate";
    bloat_audit: SystemPrimitive => "Recursively audit every Rust source in the workspace (production code) and target/ for bloat and hardcoded secrets, thread-parallel across all cores";
    distill_genome: SystemPrimitive => "Distill the hard-compiled genome into the Tier 2 reasoning model";
    self_validate: SystemPrimitive => "Execute autonomous substrate self-validation";
    list_models: SystemPrimitive => "List available model substrates";
    select_model: SystemPrimitive => "Select or override active model substrate";
    inference_placement: SystemPrimitive => "Explain the live local-versus-cloud inference placement decision";
    scout_model: SystemPrimitive => "Scout or install model substrate";
    swarm_schedule: SystemPrimitive => "Show recent mission scheduler decisions";
    train_reflexes: SystemPrimitive => "Manually trigger native neural reflex distillation";
    read_file: WorkspaceIo => "Read file content in workspace";
    write_file: WorkspaceIo => "Write content to workspace file";
    exec_command: WorkspaceIo => "Execute command in workspace";
    os_services: SystemPrimitive => "List or restart the substrate's leaf services (supervised process table)";
    os_ps: SystemPrimitive => "List host processes from /proc (pid, name, RSS), sorted by memory";
    os_sysinfo: SystemPrimitive => "Host kernel/uptime/load/memory summary from /proc";
    os_kill: WorkspaceIo => "Signal a supervised substrate service pid (TERM/KILL only, fails closed otherwise)";
    os_logs: SystemPrimitive => "Tail a supervised leaf service's log file";
    commit_record: WorkspaceIo => "Accept a cluster-key-signed swarm quorum commit record into the local ledger";
    commit_records: WorkspaceIo => "Batch intake of replicated commit records — one ledger pass for anti-entropy pushes";
    commit_log: SystemPrimitive => "List quorum commit records in the local ledger, newest first";
    commit_log_fetch: SystemPrimitive => "Return commit-ledger records for peer anti-entropy pulls";
    cluster_rekey_stage: WorkspaceIo => "Stage a next-epoch cluster key delivered with its committed cluster_rekey record";
    cluster_rekey_commit: WorkspaceIo => "Apply a committed cluster_rekey_activate record — activates the staged key";
    member_propose: WorkspaceIo => "Leader-only member_add proposal — seals and replicates a roster delta";
    member_endorse: WorkspaceIo => "Endorse a sealed privileged record — joint-consensus vote for roster/config deltas";
    cluster_status: WorkspaceIo => "Read-only consensus snapshot — term, leader, ledger heads, key epoch, roster counts";
    a2a_delegate: IntelligenceBridge => "Delegate a task to a remote A2A agent endpoint (roster-gated: loopback or verified cluster members)";
    delegate_to_ide: IntelligenceBridge => "Delegate a complex task to an overarching IDE Agent like Antigravity";
    verify_model_download_agent: SystemPrimitive => "Verify model download agent, network status, and model provisioning";
], [
    brain_pareto: IntelligenceBridge => "Show the 3D Pareto frontier, Wilson 95% lower confidence bounds, and Bayesian UCB1 exploration-exploitation indices across providers";

    // Zero-UI plan B4 groups 2, 6, 11 and 12: CLI control-plane commands
    // and the runtime model / placement tools that replaced the 9091 runtime
    // routes, reachable over the standards socket.
    inference_reset_provider: WorkspaceIo => "Re-admit a repaired inference provider after routing quarantine";
    operator_snapshot: SystemPrimitive => "Unified operator state for the server workspace (same document as `susi top --json`)";
    extensions_list: SystemPrimitive => "List installed extension packs";
    extensions_status: SystemPrimitive => "Active extension pack, its root, and every installed pack";
    extensions_validate: SystemPrimitive => "Check a pack directory against the host's admission rules (reads no host state)";
    extensions_load: WorkspaceIo => "Verify and activate an installed extension pack";
    extensions_unload: WorkspaceIo => "Deactivate an extension pack (files kept)";
]);

// No essential-MCP auto-link: core never configures or launches a
// third-party MCP server on its own (zero-UI plan A8).

/// Populates a freshly created `ToolRegistry` with every concrete tool this
/// engine provides. `ToolRegistry::global()` calls this through the daemon's
/// `EngineHooks::bootstrap_tools`; the registry mechanism stays in `susi-tools`.
pub fn bootstrap_registry(registry: &ToolRegistry) {
    for tool in CORE_TOOLS {
        ToolRegistry::register_meta_tool(
            registry,
            tool.name,
            tool.description,
            tool.category,
            tool.handler,
        );
    }
}

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod inline_tests {
    use super::*;

    type ExpectedTool = (&'static str, &'static str, MetaCategory, CoreToolHandler);
    const EXPECTED_TOOLS: &[ExpectedTool] = include!("../tests/fixtures/gmcp_bootstrap_catalog.rs");

    fn fresh_registry() -> ToolRegistry {
        let registry = ToolRegistry {
            tools: Default::default(),
        };
        bootstrap_registry(&registry);
        registry
    }

    #[test]
    fn bootstrap_matches_pristine_catalog_and_handler_mapping() {
        assert_eq!(
            EXPECTED_TOOLS.len(),
            if cfg!(feature = "tools-rich") {
                204
            } else {
                199
            }
        );
        assert_eq!(CORE_TOOLS.len(), EXPECTED_TOOLS.len());
        let registry = fresh_registry();
        let tools = registry.tools.read();
        assert_eq!(tools.len(), EXPECTED_TOOLS.len());
        let capabilities = susi_core::registry::CapabilityRegistry::global();

        // Zip preserves registration order; the fixture independently pins
        // the original name, exact description, category and callable target.
        for (descriptor, &(name, description, category, handler)) in
            CORE_TOOLS.iter().zip(EXPECTED_TOOLS)
        {
            assert_eq!(
                (descriptor.name, descriptor.description, descriptor.category),
                (name, description, category),
            );
            assert!(
                std::ptr::fn_addr_eq(descriptor.handler, handler),
                "handler for {name}"
            );
            let tool = tools.get(name).unwrap();
            assert_eq!(tool.name(), name);
            assert_eq!(tool.description(), description);
            let capability = capabilities.get_tool(name).unwrap();
            assert_eq!(capability.name(), name);
            assert_eq!(capability.description(), description);
        }
    }

    #[test]
    fn registered_handlers_preserve_success_and_error_results() {
        let registry = fresh_registry();
        let tools = registry.tools.read();
        let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        // Read a checked-in file: no writes, process execution or inference.
        let arg = serde_json::json!({"path": "Cargo.toml"});
        let expected = CoreTools::read_file(&arg, workspace).unwrap();
        assert_eq!(
            tools["read_file"].execute(&arg, workspace).unwrap(),
            expected
        );

        // These validations fail before any tool side effects. Compare both
        // the shared error contract and the exact caller-visible message.
        let cases: &[(&str, CoreToolHandler, serde_json::Value, &str)] = &[
            (
                "select_model",
                CoreTools::select_model,
                serde_json::json!({}),
                "Config",
            ),
            (
                "inference_placement",
                CoreTools::inference_placement,
                serde_json::json!(false),
                "Config",
            ),
            (
                "write_file",
                CoreTools::write_file,
                serde_json::json!({}),
                "Protocol",
            ),
            (
                "exec_command",
                CoreTools::exec_command,
                serde_json::json!(""),
                "Protocol",
            ),
            (
                "os_kill",
                CoreTools::os_kill,
                serde_json::json!({}),
                "Protocol",
            ),
            (
                "tx_commit",
                CoreTools::tx_commit,
                serde_json::json!({}),
                "Governance",
            ),
            (
                "tx_abort",
                CoreTools::tx_abort,
                serde_json::json!({}),
                "Governance",
            ),
            (
                "read_file",
                CoreTools::read_file,
                serde_json::json!({"path": "../outside"}),
                "Filesystem",
            ),
        ];
        for &(name, handler, ref arg, kind) in cases {
            let expected = handler(arg, workspace).unwrap_err();
            let actual = tools[name].execute(arg, workspace).unwrap_err();
            assert_eq!(actual.kind_name(), kind, "{name}");
            assert_eq!(actual.code(), expected.code(), "{name}");
            assert_eq!(actual.retryable(), expected.retryable(), "{name}");
            assert_eq!(actual.to_string(), expected.to_string(), "{name}");
        }
    }

    #[test]
    fn bootstrap_registry_registers_core_system_primitives() {
        let registry = ToolRegistry::global();
        bootstrap_registry(registry);
        for name in ["status", "ledger_verify", "identity", "self_validate"] {
            assert!(
                registry.tools.read().contains_key(name),
                "expected core primitive tool {name} to be registered"
            );
        }
    }

    #[test]
    fn bootstrap_registry_registers_group_11_extension_tools() {
        let registry = ToolRegistry::global();
        bootstrap_registry(registry);
        for name in [
            "extensions_list",
            "extensions_status",
            "extensions_validate",
            "extensions_load",
            "extensions_unload",
            "extensions_create",
        ] {
            assert!(
                registry.tools.read().contains_key(name),
                "expected extension tool {name} to be registered"
            );
        }
    }

    #[test]
    fn bootstrap_registry_registers_runtime_model_and_placement_tools() {
        let registry = ToolRegistry::global();
        bootstrap_registry(registry);
        for name in [
            "runtime_models_list",
            "runtime_models_load",
            "runtime_models_unload",
            "placement_decision",
            "inference_placement",
        ] {
            assert!(
                registry.tools.read().contains_key(name),
                "expected runtime/placement tool {name} to be registered"
            );
        }
    }

    #[test]
    fn bootstrap_registry_registers_zero_config_and_lifecycle_tools() {
        let registry = ToolRegistry::global();
        bootstrap_registry(registry);
        for name in [
            "auto_status",
            "auto_prime",
            "deep_scan",
            "config_reload",
            "daemon_shutdown",
            "intent_ladder",
        ] {
            assert!(
                registry.tools.read().contains_key(name),
                "expected zero-config/lifecycle tool {name} to be registered"
            );
        }
    }

    #[test]
    fn bootstrap_registry_registers_workspace_io_and_transaction_tools() {
        let registry = ToolRegistry::global();
        bootstrap_registry(registry);
        for name in [
            "read_file",
            "write_file",
            "exec_command",
            "tx_begin",
            "tx_commit",
            "tx_abort",
        ] {
            assert!(
                registry.tools.read().contains_key(name),
                "expected workspace/transaction tool {name} to be registered"
            );
        }
    }
}
