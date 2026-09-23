//! Registers every CoreTools handler into a fresh ToolRegistry.

use susi_tools::{MetaCategory, ToolRegistry};

use susi_gmcp::tools::CoreTools;

/// Vendored-error boundary: `CoreTools` handlers return susi-gmcp's vendored
/// `EaiError` while `ToolRegistry` expects susi-tools' vendored `EaiError`.
/// `rewrap` preserves the error kind across the boundary.
fn adapt<F>(
    f: F,
) -> impl Fn(&serde_json::Value, &std::path::Path) -> susi_tools::susi_error::EaiResult<String>
+ Send
+ Sync
+ 'static
where
    F: Fn(&serde_json::Value, &std::path::Path) -> Result<String, susi_gmcp::susi_error::EaiError>
        + Send
        + Sync
        + 'static,
{
    move |v, p| f(v, p).map_err(|e| susi_tools::susi_error::rewrap(e.kind_name(), e.to_string()))
}

/// Populates a freshly created `ToolRegistry` with every concrete tool this
/// engine provides - `susi_tools::ToolRegistry::global()` calls this via the
/// `EngineHooks::bootstrap_tools` hook below, since the registry mechanism
/// itself lives in `susi-tools` but the concrete `CoreTools::*` handlers
/// (and their gemi/gawd dependencies) stay here in `gmcp`.
pub fn bootstrap_registry(registry: &ToolRegistry) {
    ToolRegistry::register_meta_tool(
        registry,
        "status",
        "SUSI Substrate status report",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::status),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "identity",
        "SUSI substrate identity report",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::identity),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "sovereign_dashboard",
        "Report on autonomous invisible work performed by the substrate",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::sovereign_dashboard),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "bloat_audit",
        "Recursively audit src/ and target/ for bloat and hardcoded secrets, rayon-parallel across all cores",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::bloat_audit),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "distill_genome",
        "Distill the hard-compiled genome into the Tier 2 reasoning model",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::distill_genome),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "self_validate",
        "Execute autonomous substrate self-validation",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::self_validate),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "list_models",
        "List available model substrates",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::list_models),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "select_model",
        "Select or override active model substrate",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::select_model),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "scout_model",
        "Scout or install model substrate",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::scout_model),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "swarm_schedule",
        "Show recent mission scheduler decisions",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::swarm_schedule),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "train_reflexes",
        "Manually trigger native neural reflex distillation",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::train_reflexes),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "read_file",
        "Read file content in workspace",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::read_file),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "write_file",
        "Write content to workspace file",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::write_file),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "exec_command",
        "Execute command in workspace",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::exec_command),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "os_services",
        "List or restart the substrate's leaf services (supervised process table)",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::os_services),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "os_ps",
        "List host processes from /proc (pid, name, RSS), sorted by memory",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::os_ps),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "os_sysinfo",
        "Host kernel/uptime/load/memory summary from /proc",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::os_sysinfo),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "os_kill",
        "Signal a supervised substrate service pid (TERM/KILL only, fails closed otherwise)",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::os_kill),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "agents_list",
        "List managed external executors and setup readiness",
        MetaCategory::IntelligenceBridge,
        adapt(CoreTools::agents_list),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "agents_run",
        "Launch external task with agent and prompt",
        MetaCategory::IntelligenceBridge,
        adapt(CoreTools::agents_run),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "agents_tasks",
        "List durable external tasks",
        MetaCategory::IntelligenceBridge,
        adapt(CoreTools::agents_tasks),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "agents_status",
        "Inspect external task_id; optional refresh",
        MetaCategory::IntelligenceBridge,
        adapt(CoreTools::agents_status),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "agents_cancel",
        "Cancel external task_id",
        MetaCategory::IntelligenceBridge,
        adapt(CoreTools::agents_cancel),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "agents_logs",
        "Read external task_id output; optional stderr",
        MetaCategory::IntelligenceBridge,
        adapt(CoreTools::agents_logs),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "agents_send",
        "Send message to cloud task_id",
        MetaCategory::IntelligenceBridge,
        adapt(CoreTools::agents_send),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "coding_models_list",
        "List top coding/agent models and readiness",
        MetaCategory::IntelligenceBridge,
        adapt(CoreTools::coding_models_list),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "coding_models_prefer",
        "Prefer coding model id for routing",
        MetaCategory::IntelligenceBridge,
        adapt(CoreTools::coding_models_prefer),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "frameworks_list",
        "List managed agent frameworks and setup readiness",
        MetaCategory::IntelligenceBridge,
        adapt(CoreTools::frameworks_list),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "frameworks_run",
        "Launch framework task with engine and prompt",
        MetaCategory::IntelligenceBridge,
        adapt(CoreTools::frameworks_run),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "frameworks_tasks",
        "List durable framework tasks",
        MetaCategory::IntelligenceBridge,
        adapt(CoreTools::frameworks_tasks),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "frameworks_status",
        "Inspect framework task_id",
        MetaCategory::IntelligenceBridge,
        adapt(CoreTools::frameworks_status),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "frameworks_cancel",
        "Cancel framework task_id",
        MetaCategory::IntelligenceBridge,
        adapt(CoreTools::frameworks_cancel),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "frameworks_logs",
        "Read framework task_id output; optional stderr",
        MetaCategory::IntelligenceBridge,
        adapt(CoreTools::frameworks_logs),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "tasks_list",
        "List active and historical swarm tasks with liveness telemetry",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::tasks_list),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "tasks_pause",
        "Pause a running task by task_id",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::tasks_pause),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "tasks_resume",
        "Resume a paused task by task_id",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::tasks_resume),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "tasks_kill",
        "Kill a running or stalled task by task_id",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::tasks_kill),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "mcp_registry",
        "Interrogate global MCP registry and benchmark servers",
        MetaCategory::McpProxy,
        adapt(CoreTools::mcp_registry),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "mcp_configure",
        "Configure external MCP server",
        MetaCategory::McpProxy,
        adapt(CoreTools::mcp_configure),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "leading_mcp_list",
        "List top MCP servers and readiness",
        MetaCategory::McpProxy,
        adapt(CoreTools::leading_mcp_list),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "leading_mcp_enable",
        "Enable leading MCP server into mcp_config",
        MetaCategory::McpProxy,
        adapt(CoreTools::leading_mcp_enable),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "leading_mcp_disable",
        "Disable leading MCP server from mcp_config",
        MetaCategory::McpProxy,
        adapt(CoreTools::leading_mcp_disable),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "agent_register",
        "Dynamically register a new agent profile",
        MetaCategory::IntelligenceBridge,
        adapt(CoreTools::agent_register),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "reason",
        "Execute swarm reasoning substrate",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::reason),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "susi_solve",
        "Solve a natural-language intent via the SUSI swarm substrate",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::susi_solve),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "power_reason",
        "Delegate complex reasoning to Power-Tier MCP remotes",
        MetaCategory::IntelligenceBridge,
        adapt(CoreTools::power_reason),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "meta_scout_agents",
        "Discover agent capabilities from connected remotes",
        MetaCategory::IntelligenceBridge,
        adapt(CoreTools::meta_scout_agents),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "meta_rank_agents",
        "Report current agent expertise hierarchy",
        MetaCategory::IntelligenceBridge,
        adapt(CoreTools::meta_rank_agents),
    );

    // SPECIALIST TOOLBOXES: Type 1 (Coding) & Type 2 (Assistant)
    #[cfg(feature = "tools-rich")]
    ToolRegistry::register_meta_tool(
        registry,
        "ast_analyze",
        "Structural AST code analysis via tree-sitter",
        MetaCategory::CodingSpecialist,
        adapt(CoreTools::ast_analyze),
    );
    #[cfg(feature = "tools-rich")]
    ToolRegistry::register_meta_tool(
        registry,
        "semantic_search",
        "Unified BM25 recall over .susi stores and workspace files",
        MetaCategory::CodingSpecialist,
        adapt(CoreTools::semantic_search),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "sandbox_exec",
        "Isolated Docker execution via bollard",
        MetaCategory::CodingSpecialist,
        adapt(CoreTools::sandbox_exec),
    );
    #[cfg(feature = "tools-rich")]
    ToolRegistry::register_meta_tool(
        registry,
        "browser_automate",
        "DOM access and web automation via headless_chrome",
        MetaCategory::AssistantSpecialist,
        adapt(CoreTools::browser_automate),
    );
    #[cfg(feature = "tools-rich")]
    ToolRegistry::register_meta_tool(
        registry,
        "rag_query",
        "Vector recall over the unified .susi index (local fastembed)",
        MetaCategory::AssistantSpecialist,
        adapt(CoreTools::rag_query),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "context_graph_query",
        "Query the Universal Context Graph for the current workspace or a specific node id",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::context_graph_query),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "context_graph_ingest",
        "Ingest an external context event into the Universal Context Graph",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::context_graph_ingest),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "context_graph_compact",
        "Compact the append-only Universal Context Graph log",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::context_graph_compact),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "ipc_grant",
        "Grant an inter-app permission scope",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::ipc_grant),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "ipc_request",
        "Request an inter-app permission (opens negotiation)",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::ipc_request),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "ipc_negotiate",
        "Approve or deny a pending permission request",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::ipc_negotiate),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "ipc_send",
        "Send a broker message (requires dispatch grant unless from=susi)",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::ipc_send),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "ipc_receive",
        "Receive broker messages for an identity",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::ipc_receive),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "host_telemetry",
        "Sample host thermal, battery, and load telemetry",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::host_telemetry),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "apply_patch_cycle",
        "Apply a workspace-confined patch then run tests; revert on failure",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::apply_patch_cycle),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "privacy_status",
        "Report privacy mode and cryptographic capability grants",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::privacy_status),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "privacy_consent_egress",
        "Grant time-limited network egress / cloud inference consent",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::privacy_consent_egress),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "intent_advertise",
        "Advertise a provider capability on the semantic intent bus",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::intent_advertise),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "intent_need",
        "Publish a need and match providers on the semantic intent bus",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::intent_need),
    );
    #[cfg(feature = "tools-rich")]
    ToolRegistry::register_meta_tool(
        registry,
        "ambient_pulse",
        "Scan workspace, record FS changes into context graph, refresh vector index",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::ambient_pulse),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "tx_begin",
        "Begin a multi-agent transaction snapshotting listed files",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::tx_begin),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "tx_commit",
        "Commit an open multi-agent transaction",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::tx_commit),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "tx_abort",
        "Abort a transaction and restore snapshotted files",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::tx_abort),
    );

    // DYNAMIC DISCOVERY: Synthesized Native Reflexes
    susi_gmcp::reflexes::register_synthesized_reflexes();

    // Zero-Config Auto-Link: Ensure essential MCP tools are mapped (Non-Blocking Mandate)
    std::thread::spawn(|| {
        ToolRegistry::auto_link_essential_mcp_servers();
    });
}
