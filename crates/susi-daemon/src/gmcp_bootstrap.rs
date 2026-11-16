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
    ($($(#[$attr:meta])* $handler:ident: $category:ident => $description:literal;)*) => {
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

core_tools! {
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
    agents_list: IntelligenceBridge => "List managed external executors and setup readiness";
    agents_run: IntelligenceBridge => "Launch external task with agent and prompt";
    agents_tasks: IntelligenceBridge => "List durable external tasks";
    agents_status: IntelligenceBridge => "Inspect external task_id; optional refresh";
    agents_cancel: IntelligenceBridge => "Cancel external task_id";
    agents_logs: IntelligenceBridge => "Read external task_id output; optional stderr";
    agents_send: IntelligenceBridge => "Send message to cloud task_id";
    coding_models_list: IntelligenceBridge => "List top coding/agent models and readiness";
    coding_models_prefer: IntelligenceBridge => "Prefer coding model id for routing";
    frameworks_list: IntelligenceBridge => "List managed agent frameworks and setup readiness";
    frameworks_run: IntelligenceBridge => "Launch framework task with engine and prompt";
    frameworks_tasks: IntelligenceBridge => "List durable framework tasks";
    frameworks_status: IntelligenceBridge => "Inspect framework task_id";
    frameworks_cancel: IntelligenceBridge => "Cancel framework task_id";
    frameworks_logs: IntelligenceBridge => "Read framework task_id output; optional stderr";
    tasks_list: SystemPrimitive => "List active and historical swarm tasks with liveness telemetry";
    tasks_pause: SystemPrimitive => "Pause a running task by task_id";
    tasks_resume: SystemPrimitive => "Resume a paused task by task_id";
    tasks_kill: SystemPrimitive => "Kill a running or stalled task by task_id";
    mcp_registry: McpProxy => "Interrogate global MCP registry and benchmark servers";
    mcp_configure: McpProxy => "Configure external MCP server";
    leading_mcp_list: McpProxy => "List top MCP servers and readiness";
    leading_mcp_enable: McpProxy => "Enable leading MCP server into mcp_config";
    leading_mcp_disable: McpProxy => "Disable leading MCP server from mcp_config";
    agent_register: IntelligenceBridge => "Dynamically register a new agent profile";
    reason: SystemPrimitive => "Execute swarm reasoning substrate";
    susi_solve: SystemPrimitive => "Solve a natural-language intent via the SUSI swarm substrate";
    mission_run: SystemPrimitive => "Run a goal as `susi <goal>` does and report its status and exit code";
    power_reason: IntelligenceBridge => "Delegate complex reasoning to Power-Tier MCP remotes";
    meta_scout_agents: IntelligenceBridge => "Discover agent capabilities from connected remotes";
    meta_rank_agents: IntelligenceBridge => "Report current agent expertise hierarchy";

    // SPECIALIST TOOLBOXES: Type 1 (Coding) & Type 2 (Assistant)
    #[cfg(feature = "tools-rich")]
    ast_analyze: CodingSpecialist => "Structural AST analysis of Rust source (syn); other languages report size only";
    #[cfg(feature = "tools-rich")]
    semantic_search: CodingSpecialist => "Unified BM25 recall over .susi stores and workspace files";
    sandbox_exec: CodingSpecialist => "Isolated Docker execution via bollard";
    #[cfg(feature = "tools-rich")]
    browser_automate: AssistantSpecialist => "DOM access and web automation via headless Chrome";
    #[cfg(feature = "tools-rich")]
    rag_query: AssistantSpecialist => "Vector recall over the unified .susi index (local fastembed)";
    context_graph_query: SystemPrimitive => "Query the Universal Context Graph for the current workspace or a specific node id";
    context_graph_ingest: SystemPrimitive => "Ingest an external context event into the Universal Context Graph";
    context_graph_compact: SystemPrimitive => "Compact the append-only Universal Context Graph log";
    ipc_grant: SystemPrimitive => "Grant an inter-app permission scope";
    ipc_request: SystemPrimitive => "Request an inter-app permission (opens negotiation)";
    ipc_negotiate: SystemPrimitive => "Approve or deny a pending permission request";
    ipc_send: SystemPrimitive => "Send a broker message (requires dispatch grant unless from=susi)";
    ipc_receive: SystemPrimitive => "Receive broker messages for an identity";
    host_telemetry: SystemPrimitive => "Sample host thermal, battery, and load telemetry";
    apply_patch_cycle: WorkspaceIo => "Apply a workspace-confined patch then run tests; revert on failure";
    privacy_status: SystemPrimitive => "Report privacy mode and cryptographic capability grants";
    privacy_consent_egress: SystemPrimitive => "Grant time-limited network egress / cloud inference consent";
    intent_advertise: SystemPrimitive => "Advertise a provider capability on the semantic intent bus";
    intent_need: SystemPrimitive => "Publish a need and match providers on the semantic intent bus";
    #[cfg(feature = "tools-rich")]
    ambient_pulse: SystemPrimitive => "Scan workspace, record FS changes into context graph, refresh vector index";
    tx_begin: WorkspaceIo => "Begin a multi-agent transaction snapshotting listed files";
    tx_commit: WorkspaceIo => "Commit an open multi-agent transaction";
    tx_abort: WorkspaceIo => "Abort a transaction and restore snapshotted files";
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
    plan_run: WorkspaceIo => "Solve a goal with the autonomous planning loop in the server workspace";
    intent_ingest: WorkspaceIo => "Ingest a natural-language intent into the server workspace evidence ledger";
    blackboard_show: SystemPrimitive => "Show the last mission blackboard (credential-redacted)";
    blackboard_post: WorkspaceIo => "Post a key-value update to the shared blackboard and Susi Native Board";
    board_list: SystemPrimitive => "List collaborative items on the Susi Native Board";
    board_check: SystemPrimitive => "Check the Susi Native Board for open stops and unanswered requests";
    board_post: WorkspaceIo => "Post an item onto the Susi Native Board";
    runtime_models_list: SystemPrimitive => "List models loaded in the inference cache";
    runtime_models_load: WorkspaceIo => "Load a model into the inference cache";
    runtime_models_unload: WorkspaceIo => "Evict a model from the inference cache";
    placement_decision: SystemPrimitive => "Look up an audited inference-placement decision by id";
    context_graph_stats: SystemPrimitive => "Universal Context Graph node and edge counts by kind";
    context_graph_show: SystemPrimitive => "Most recent Universal Context Graph events for a workspace";

    // Zero-UI plan B4 groups 4, 5, 8 and 9: onboarding, assurance, context
    // and IPC, and the OS view, reachable over the standards socket.
    ipc_pending: SystemPrimitive => "List pending inter-app permission requests";
    ipc_check: SystemPrimitive => "Check whether an identity holds an inter-app permission scope";
    ipc_revoke: WorkspaceIo => "Revoke an inter-app permission scope from an identity";
    ipc_list: SystemPrimitive => "List the inter-app permission grants an identity holds";
    privacy_mode: WorkspaceIo => "Set the privacy mode (balanced, local_only = this machine and its local network, host_only = this machine alone, open)";
    privacy_grant: WorkspaceIo => "Grant a cryptographic capability token";
    privacy_revoke: WorkspaceIo => "Revoke a capability grant";
    intent_list: SystemPrimitive => "List active intent-bus providers and needs";
    ambient_start: WorkspaceIo => "Start the background ambient indexer for the server workspace";
    tx_list: SystemPrimitive => "List open multi-agent transactions in the server workspace";
    context_graph_sample_os: WorkspaceIo => "Sample OS context into the Universal Context Graph";
    audit_verify: SystemPrimitive => "Verify the server workspace's tamper-evident audit chain";
    audit_query: SystemPrimitive => "Query redacted action history from the audit chain";
    compliance_audit: SystemPrimitive => "Run the deterministic compliance audit";
    audit_evidence_export: WorkspaceIo => "Export redacted audit segments in the verifiable format";
    substrate_report: SystemPrimitive => "Compiled-constants substrate report";
    substrate_status: SystemPrimitive => "Pluggable / Sandbox / Governance / Host-contract / Reflexes status";
    crown_verify: SystemPrimitive => "Verify every Tier S USP";
    clean: WorkspaceIo => "Remove SUSI-owned residue and report bytes freed";
    setup_plan: SystemPrimitive => "Detect the host and return the zero-config setup plan";
    setup_apply: WorkspaceIo => "Apply the zero-config setup plan (existing settings preserved)";
    ecosystem_scan: SystemPrimitive => "Scan (or return the last scan of) the local AI ecosystem";
    ecosystem_start: WorkspaceIo => "Start an installed local AI engine";
    ecosystem_kb: SystemPrimitive => "Query the cloud-AI-ecosystem knowledge base";
    ecosystem_explain: SystemPrimitive => "Explain a knowledge-base component, standard or protocol";
    os_provision: WorkspaceIo => "Explicit cloud provisioning: probe, list or apply";
    os_capabilities: WorkspaceIo => "Discover or invoke typed capability-bus capabilities";
    services_start: WorkspaceIo => "Clear an operator stop on a supervised leaf service";
    services_stop: WorkspaceIo => "Stop a supervised leaf service and hold it down";
    services_restart: WorkspaceIo => "Restart a supervised leaf service";
    commits_audit: SystemPrimitive => "Whole-ledger consistency check of the commit log";
    commits_replay: SystemPrimitive => "Consensus view the commit ledger replays to";
    commits_show: SystemPrimitive => "Show one commit record by epoch prefix";
    commits_sync: WorkspaceIo => "Commit-ledger anti-entropy with verified peers";
    commits_compact: WorkspaceIo => "Compact the commit ledger into a snapshot plus archive";
    peers_probe: SystemPrimitive => "Cryptographically verify a peer host without admitting it";
    peers_list: SystemPrimitive => "List the verified cluster roster, leader and banned members";
    peers_status: SystemPrimitive => "Every rostered member's cluster status side by side";

    // Zero-UI plan B4 group 3: providers, keys (presence only) and the
    // brain, reachable over the standards socket.
    provider_list: SystemPrimitive => "List a provider control plane's curated models and readiness";
    provider_doctor: SystemPrimitive => "Check a provider control plane's readiness (no paid call)";
    provider_setup: SystemPrimitive => "Show a provider model's setup instructions and effective wiring";
    provider_configure: WorkspaceIo => "Store a host override for a provider model";
    provider_reset: WorkspaceIo => "Drop a provider model's host override";
    provider_pull: WorkspaceIo => "Pull an open-weight model's weights through Ollama";
    provider_prefer: WorkspaceIo => "Prefer or clear a provider model for routing";
    provider_probe: SystemPrimitive => "Probe a model endpoint with a short completion (cloud probes are paid)";
    provider_models: SystemPrimitive => "List OpenRouter routes or the live models its key reaches";
    keys_status: SystemPrimitive => "Report which cloud vendor keys are set (presence only, never values)";
    keys_prefer: WorkspaceIo => "Pin, clear or report the preferred cloud vendor";
    keys_check: SystemPrimitive => "Check each vendor key against its model list (values never returned)";
    keys_models: SystemPrimitive => "List the models a vendor's valid key reaches";
    brain_status: SystemPrimitive => "Evidence-ranked providers for every task class";
    brain_budget: WorkspaceIo => "Set or report the budget dial and shared spend policy";
    brain_classify: SystemPrimitive => "Show which task class a prompt routes as";
    brain_explain: SystemPrimitive => "Explain the brain's routing decision for a prompt";
    brain_export: SystemPrimitive => "Export the brain evidence bundle";
    brain_import: WorkspaceIo => "Merge a brain evidence bundle";
    brain_matrix: SystemPrimitive => "Show the measured model capability matrix";
    brain_cache: SystemPrimitive => "Show the measured prompt-cache hit rate and saving";
    brain_reset: WorkspaceIo => "Forget every recorded brain outcome";
    brain_bench: WorkspaceIo => "Run the fixed prompt set against the fake engine and record evidence";
    brain_probe: WorkspaceIo => "Probe a registered provider with the scout benchmark";
    benchmark_run: SystemPrimitive => "Measure local (and configured cloud) inference latency and throughput";
    verify_download_agent: SystemPrimitive => "Verify network status and the hardware-selected model ladder";

    // Zero-UI plan B4 group 7: adapter lifecycle for agents, frameworks,
    // coding models and MCP servers.
    adapter_doctor: SystemPrimitive => "Check agent, framework, coding-model or MCP adapter readiness";
    adapter_setup: SystemPrimitive => "Show an adapter's setup instructions and effective configuration";
    adapter_configure: WorkspaceIo => "Store a host adapter override (verified upgrade for agents and frameworks)";
    adapter_reset: WorkspaceIo => "Drop a host adapter override and restore the bundled adapter";
    agents_retry: WorkspaceIo => "Run a finished external agent task again with its original prompt";
    frameworks_retry: WorkspaceIo => "Run a finished agent-framework task again with its original prompt";
    mcp_add: WorkspaceIo => "Admit an MCP server into the registry and hot-plug it";

    // Zero-UI plan B4 group 1: the self-build harness.
    harness_status: SystemPrimitive => "Harness API snapshot for a ledger: spend, tasks, workers, warnings";
    harness_budget: SystemPrimitive => "One-line harness budget report and top-up alerts";
    harness_models: SystemPrimitive => "Harness provider registry, drift check, discovery or keyed models";
    harness_plan: WorkspaceIo => "Plan a batch of dependency-ready queue tasks";
    harness_author: WorkspaceIo => "Author a plan for a roadmap vector through the metered gateway";
    harness_run: WorkspaceIo => "Start running a harness plan under the budget gate";
    harness_runs: SystemPrimitive => "Harness missions this daemon started and their outcomes";

    // Zero-UI plan B4 groups 2, 4, 9, 10 and 13: host views, maintenance
    // and daemon lifecycle.
    operator_status: SystemPrimitive => "Measured executable identity and substrate counts (as `susi status --json`)";
    operator_review: SystemPrimitive => "Daemon liveness and reclaimable OS cache (as `susi review`)";
    os_ecosystem: SystemPrimitive => "Local AI ecosystem: hardware, runtimes, models, adapters, MCP servers, AI processes";
    os_status: SystemPrimitive => "Substrate status: consensus, ledger, services, peers, endpoints, storage";
    os_runtime: SystemPrimitive => "Live AI processes by owner, managed-model residency and GPU memory";
    model_uninstall: WorkspaceIo => "Remove one SUSI-managed model file (refused while loaded or selected)";
    peers_add: WorkspaceIo => "Join a peer by signed handshake and commit its admission";
    peers_remove: WorkspaceIo => "Evict and ban a verified peer, committed cluster-wide";
    peers_unban: WorkspaceIo => "Lift a peer ban, committed cluster-wide";
    peers_rekey: WorkspaceIo => "Rotate the cluster key across every verified member";
    daemon_shutdown: WorkspaceIo => "Stop this daemon instance after replying";
    extensions_create: WorkspaceIo => "Create an empty extension pack shell (not activated)";
    auto_status: SystemPrimitive => "Zero-config substrate readiness snapshot";
    auto_prime: WorkspaceIo => "Re-run zero-config auto-prime (never enables MCP)";
    deep_scan: WorkspaceIo => "Scan the home directory for model files and register them";
    config_reload: WorkspaceIo => "Reload config.json and report what is in effect";
    provider_contract: SystemPrimitive => "Provider contract fixture checks (credentialed half opt-in)";
    intent_ladder: WorkspaceIo => "Intent ladder scorecard, runs, verification, gap outbox and adoption";

    // No essential-MCP auto-link: core never configures or launches a
    // third-party MCP server on its own (zero-UI plan A8).
}

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
