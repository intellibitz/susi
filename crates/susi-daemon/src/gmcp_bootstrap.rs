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
    move |v, p| f(v, p)
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
        "ledger_verify",
        "Verify the pinned GMCP normative requirements ledger (VC-203-001)",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::ledger_verify),
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
        "Recursively audit src/ and target/ for bloat and hardcoded secrets, thread-parallel across all cores",
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
        "inference_placement",
        "Explain the live local-versus-cloud inference placement decision",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::inference_placement),
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
        "os_logs",
        "Tail a supervised leaf service's log file",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::os_logs),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "commit_record",
        "Accept a cluster-key-signed swarm quorum commit record into the local ledger",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::commit_record),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "commit_records",
        "Batch intake of replicated commit records — one ledger pass for anti-entropy pushes",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::commit_records),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "commit_log",
        "List quorum commit records in the local ledger, newest first",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::commit_log),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "commit_log_fetch",
        "Return commit-ledger records for peer anti-entropy pulls",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::commit_log_fetch),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "cluster_rekey_stage",
        "Stage a next-epoch cluster key delivered with its committed cluster_rekey record",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::cluster_rekey_stage),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "cluster_rekey_commit",
        "Apply a committed cluster_rekey_activate record — activates the staged key",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::cluster_rekey_commit),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "member_propose",
        "Leader-only member_add proposal — seals and replicates a roster delta",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::member_propose),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "member_endorse",
        "Endorse a sealed privileged record — joint-consensus vote for roster/config deltas",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::member_endorse),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "cluster_status",
        "Read-only consensus snapshot — term, leader, ledger heads, key epoch, roster counts",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::cluster_status),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "a2a_delegate",
        "Delegate a task to a remote A2A agent endpoint (roster-gated: loopback or verified cluster members)",
        MetaCategory::IntelligenceBridge,
        adapt(CoreTools::a2a_delegate),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "delegate_to_ide",
        "Delegate a complex task to an overarching IDE Agent like Antigravity",
        MetaCategory::IntelligenceBridge,
        adapt(CoreTools::delegate_to_ide),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "verify_model_download_agent",
        "Verify model download agent, network status, and model provisioning",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::verify_model_download_agent),
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
        "mission_run",
        "Run a goal as `susi <goal>` does and report its status and exit code",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::mission_run),
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
        "Structural AST analysis of Rust source (syn); other languages report size only",
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
        "DOM access and web automation via headless Chrome",
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
    ToolRegistry::register_meta_tool(
        registry,
        "brain_pareto",
        "Show the 3D Pareto frontier, Wilson 95% lower confidence bounds, and Bayesian UCB1 exploration-exploitation indices across providers",
        MetaCategory::IntelligenceBridge,
        adapt(CoreTools::brain_pareto),
    );

    // Zero-UI plan B4 groups 2, 6, 11 and 12: CLI control-plane commands
    // and the runtime model / placement tools that replaced the 9091 runtime
    // routes, reachable over the standards socket.
    ToolRegistry::register_meta_tool(
        registry,
        "inference_reset_provider",
        "Re-admit a repaired inference provider after routing quarantine",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::inference_reset_provider),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "operator_snapshot",
        "Unified operator state for the server workspace (same document as `susi top --json`)",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::operator_snapshot),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "extensions_list",
        "List installed extension packs",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::extensions_list),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "extensions_status",
        "Active extension pack, its root, and every installed pack",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::extensions_status),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "extensions_validate",
        "Check a pack directory against the host's admission rules (reads no host state)",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::extensions_validate),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "extensions_load",
        "Verify and activate an installed extension pack",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::extensions_load),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "extensions_unload",
        "Deactivate an extension pack (files kept)",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::extensions_unload),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "plan_run",
        "Solve a goal with the autonomous planning loop in the server workspace",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::plan_run),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "intent_ingest",
        "Ingest a natural-language intent into the server workspace evidence ledger",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::intent_ingest),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "blackboard_show",
        "Show the last mission blackboard (credential-redacted)",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::blackboard_show),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "runtime_models_list",
        "List models loaded in the inference cache",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::runtime_models_list),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "runtime_models_load",
        "Load a model into the inference cache",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::runtime_models_load),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "runtime_models_unload",
        "Evict a model from the inference cache",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::runtime_models_unload),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "placement_decision",
        "Look up an audited inference-placement decision by id",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::placement_decision),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "context_graph_stats",
        "Universal Context Graph node and edge counts by kind",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::context_graph_stats),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "context_graph_show",
        "Most recent Universal Context Graph events for a workspace",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::context_graph_show),
    );

    // Zero-UI plan B4 groups 4, 5, 8 and 9: onboarding, assurance, context
    // and IPC, and the OS view, reachable over the standards socket.
    ToolRegistry::register_meta_tool(
        registry,
        "ipc_pending",
        "List pending inter-app permission requests",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::ipc_pending),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "ipc_check",
        "Check whether an identity holds an inter-app permission scope",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::ipc_check),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "ipc_revoke",
        "Revoke an inter-app permission scope from an identity",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::ipc_revoke),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "ipc_list",
        "List the inter-app permission grants an identity holds",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::ipc_list),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "privacy_mode",
        "Set the privacy mode (balanced, local_only, open)",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::privacy_mode),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "privacy_grant",
        "Grant a cryptographic capability token",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::privacy_grant),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "privacy_revoke",
        "Revoke a capability grant",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::privacy_revoke),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "intent_list",
        "List active intent-bus providers and needs",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::intent_list),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "ambient_start",
        "Start the background ambient indexer for the server workspace",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::ambient_start),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "tx_list",
        "List open multi-agent transactions in the server workspace",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::tx_list),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "context_graph_sample_os",
        "Sample OS context into the Universal Context Graph",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::context_graph_sample_os),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "audit_verify",
        "Verify the server workspace's tamper-evident audit chain",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::audit_verify),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "audit_query",
        "Query redacted action history from the audit chain",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::audit_query),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "compliance_audit",
        "Run the deterministic compliance audit",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::compliance_audit),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "audit_evidence_export",
        "Export redacted audit segments in the verifiable format",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::audit_evidence_export),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "substrate_report",
        "Compiled-constants substrate report",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::substrate_report),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "substrate_status",
        "Pluggable / Sandbox / Governance / Host-contract / Reflexes status",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::substrate_status),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "crown_verify",
        "Verify every Tier S USP",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::crown_verify),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "clean",
        "Remove SUSI-owned residue and report bytes freed",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::clean),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "setup_plan",
        "Detect the host and return the zero-config setup plan",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::setup_plan),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "setup_apply",
        "Apply the zero-config setup plan (existing settings preserved)",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::setup_apply),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "ecosystem_scan",
        "Scan (or return the last scan of) the local AI ecosystem",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::ecosystem_scan),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "ecosystem_start",
        "Start an installed local AI engine",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::ecosystem_start),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "ecosystem_kb",
        "Query the cloud-AI-ecosystem knowledge base",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::ecosystem_kb),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "ecosystem_explain",
        "Explain a knowledge-base component, standard or protocol",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::ecosystem_explain),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "os_provision",
        "Explicit cloud provisioning: probe, list or apply",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::os_provision),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "os_capabilities",
        "Discover or invoke typed capability-bus capabilities",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::os_capabilities),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "services_start",
        "Clear an operator stop on a supervised leaf service",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::services_start),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "services_stop",
        "Stop a supervised leaf service and hold it down",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::services_stop),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "services_restart",
        "Restart a supervised leaf service",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::services_restart),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "commits_audit",
        "Whole-ledger consistency check of the commit log",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::commits_audit),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "commits_replay",
        "Consensus view the commit ledger replays to",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::commits_replay),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "commits_show",
        "Show one commit record by epoch prefix",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::commits_show),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "commits_sync",
        "Commit-ledger anti-entropy with verified peers",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::commits_sync),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "commits_compact",
        "Compact the commit ledger into a snapshot plus archive",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::commits_compact),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "peers_probe",
        "Cryptographically verify a peer host without admitting it",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::peers_probe),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "peers_list",
        "List the verified cluster roster, leader and banned members",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::peers_list),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "peers_status",
        "Every rostered member's cluster status side by side",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::peers_status),
    );

    // Zero-UI plan B4 group 3: providers, keys (presence only) and the
    // brain, reachable over the standards socket.
    ToolRegistry::register_meta_tool(
        registry,
        "provider_list",
        "List a provider control plane's curated models and readiness",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::provider_list),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "provider_doctor",
        "Check a provider control plane's readiness (no paid call)",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::provider_doctor),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "provider_setup",
        "Show a provider model's setup instructions and effective wiring",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::provider_setup),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "provider_configure",
        "Store a host override for a provider model",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::provider_configure),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "provider_reset",
        "Drop a provider model's host override",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::provider_reset),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "provider_pull",
        "Pull an open-weight model's weights through Ollama",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::provider_pull),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "provider_prefer",
        "Prefer or clear a provider model for routing",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::provider_prefer),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "provider_probe",
        "Probe a model endpoint with a short completion (cloud probes are paid)",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::provider_probe),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "provider_models",
        "List OpenRouter routes or the live models its key reaches",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::provider_models),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "keys_status",
        "Report which cloud vendor keys are set (presence only, never values)",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::keys_status),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "keys_prefer",
        "Pin, clear or report the preferred cloud vendor",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::keys_prefer),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "keys_check",
        "Check each vendor key against its model list (values never returned)",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::keys_check),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "keys_models",
        "List the models a vendor's valid key reaches",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::keys_models),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "brain_status",
        "Evidence-ranked providers for every task class",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::brain_status),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "brain_budget",
        "Set or report the budget dial and shared spend policy",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::brain_budget),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "brain_classify",
        "Show which task class a prompt routes as",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::brain_classify),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "brain_explain",
        "Explain the brain's routing decision for a prompt",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::brain_explain),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "brain_export",
        "Export the brain evidence bundle",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::brain_export),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "brain_import",
        "Merge a brain evidence bundle",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::brain_import),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "brain_matrix",
        "Show the measured model capability matrix",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::brain_matrix),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "brain_cache",
        "Show the measured prompt-cache hit rate and saving",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::brain_cache),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "brain_reset",
        "Forget every recorded brain outcome",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::brain_reset),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "brain_bench",
        "Run the fixed prompt set against the fake engine and record evidence",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::brain_bench),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "brain_probe",
        "Probe a registered provider with the scout benchmark",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::brain_probe),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "benchmark_run",
        "Measure local (and configured cloud) inference latency and throughput",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::benchmark_run),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "verify_download_agent",
        "Verify network status and the hardware-selected model ladder",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::verify_download_agent),
    );

    // Zero-UI plan B4 group 7: adapter lifecycle for agents, frameworks,
    // coding models and MCP servers.
    ToolRegistry::register_meta_tool(
        registry,
        "adapter_doctor",
        "Check agent, framework, coding-model or MCP adapter readiness",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::adapter_doctor),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "adapter_setup",
        "Show an adapter's setup instructions and effective configuration",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::adapter_setup),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "adapter_configure",
        "Store a host adapter override (verified upgrade for agents and frameworks)",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::adapter_configure),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "adapter_reset",
        "Drop a host adapter override and restore the bundled adapter",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::adapter_reset),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "agents_retry",
        "Run a finished external agent task again with its original prompt",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::agents_retry),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "frameworks_retry",
        "Run a finished agent-framework task again with its original prompt",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::frameworks_retry),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "mcp_add",
        "Admit an MCP server into the registry and hot-plug it",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::mcp_add),
    );

    // Zero-UI plan B4 group 1: the self-build harness.
    ToolRegistry::register_meta_tool(
        registry,
        "harness_status",
        "Harness API snapshot for a ledger: spend, tasks, workers, warnings",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::harness_status),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "harness_budget",
        "One-line harness budget report and top-up alerts",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::harness_budget),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "harness_models",
        "Harness provider registry, drift check, discovery or keyed models",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::harness_models),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "harness_plan",
        "Plan a batch of dependency-ready queue tasks",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::harness_plan),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "harness_author",
        "Author a plan for a roadmap vector through the metered gateway",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::harness_author),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "harness_run",
        "Start running a harness plan under the budget gate",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::harness_run),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "harness_runs",
        "Harness missions this daemon started and their outcomes",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::harness_runs),
    );

    // Zero-UI plan B4 groups 2, 4, 9, 10 and 13: host views, maintenance
    // and daemon lifecycle.
    ToolRegistry::register_meta_tool(
        registry,
        "operator_status",
        "Measured executable identity and substrate counts (as `susi status --json`)",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::operator_status),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "operator_review",
        "Daemon liveness and reclaimable OS cache (as `susi review`)",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::operator_review),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "os_ecosystem",
        "Local AI ecosystem: hardware, runtimes, models, adapters, MCP servers, AI processes",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::os_ecosystem),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "os_status",
        "Substrate status: consensus, ledger, services, peers, endpoints, storage",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::os_status),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "os_runtime",
        "Live AI processes by owner, managed-model residency and GPU memory",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::os_runtime),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "model_uninstall",
        "Remove one SUSI-managed model file (refused while loaded or selected)",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::model_uninstall),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "peers_add",
        "Join a peer by signed handshake and commit its admission",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::peers_add),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "peers_remove",
        "Evict and ban a verified peer, committed cluster-wide",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::peers_remove),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "peers_unban",
        "Lift a peer ban, committed cluster-wide",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::peers_unban),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "peers_rekey",
        "Rotate the cluster key across every verified member",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::peers_rekey),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "daemon_shutdown",
        "Stop this daemon instance after replying",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::daemon_shutdown),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "extensions_create",
        "Create an empty extension pack shell (not activated)",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::extensions_create),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "auto_status",
        "Zero-config substrate readiness snapshot",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::auto_status),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "auto_prime",
        "Re-run zero-config auto-prime (never enables MCP)",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::auto_prime),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "deep_scan",
        "Scan the home directory for model files and register them",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::deep_scan),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "config_reload",
        "Reload config.json and report what is in effect",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::config_reload),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "provider_contract",
        "Provider contract fixture checks (credentialed half opt-in)",
        MetaCategory::SystemPrimitive,
        adapt(CoreTools::provider_contract),
    );
    ToolRegistry::register_meta_tool(
        registry,
        "intent_ladder",
        "Intent ladder scorecard, runs, verification, gap outbox and adoption",
        MetaCategory::WorkspaceIo,
        adapt(CoreTools::intent_ladder),
    );

    // No essential-MCP auto-link: core never configures or launches a
    // third-party MCP server on its own (zero-UI plan A8).
}

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod inline_tests {
    use super::*;

    #[test]
    fn bootstrap_registry_registers_core_system_primitives() {
        let registry = ToolRegistry::global();
        bootstrap_registry(registry);
        for name in ["status", "ledger_verify", "identity", "self_validate"] {
            assert!(
                registry.tools.contains_key(name),
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
                registry.tools.contains_key(name),
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
                registry.tools.contains_key(name),
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
                registry.tools.contains_key(name),
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
                registry.tools.contains_key(name),
                "expected workspace/transaction tool {name} to be registered"
            );
        }
    }
}
