//! Registers every CoreTools handler into a fresh ToolRegistry.

use susi_tools::{MetaCategory, ToolRegistry};

use super::core::CoreTools;

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
        CoreTools::status,
    );
    ToolRegistry::register_meta_tool(
        registry,
        "identity",
        "SUSI substrate identity report",
        MetaCategory::SystemPrimitive,
        CoreTools::identity,
    );
    ToolRegistry::register_meta_tool(
        registry,
        "sovereign_dashboard",
        "Report on autonomous invisible work performed by the substrate",
        MetaCategory::SystemPrimitive,
        CoreTools::sovereign_dashboard,
    );
    ToolRegistry::register_meta_tool(
        registry,
        "bloat_audit",
        "Recursively audit src/ and target/ for bloat and hardcoded secrets, rayon-parallel across all cores",
        MetaCategory::SystemPrimitive,
        CoreTools::bloat_audit,
    );
    ToolRegistry::register_meta_tool(
        registry,
        "distill_genome",
        "Distill the hard-compiled genome into the Tier 2 reasoning model",
        MetaCategory::SystemPrimitive,
        CoreTools::distill_genome,
    );
    ToolRegistry::register_meta_tool(
        registry,
        "self_validate",
        "Execute autonomous substrate self-validation",
        MetaCategory::SystemPrimitive,
        CoreTools::self_validate,
    );
    ToolRegistry::register_meta_tool(
        registry,
        "list_models",
        "List available model substrates",
        MetaCategory::SystemPrimitive,
        CoreTools::list_models,
    );
    ToolRegistry::register_meta_tool(
        registry,
        "select_model",
        "Select or override active model substrate",
        MetaCategory::SystemPrimitive,
        CoreTools::select_model,
    );
    ToolRegistry::register_meta_tool(
        registry,
        "scout_model",
        "Scout or install model substrate",
        MetaCategory::SystemPrimitive,
        CoreTools::scout_model,
    );
    ToolRegistry::register_meta_tool(
        registry,
        "train_reflexes",
        "Manually trigger native neural reflex distillation",
        MetaCategory::SystemPrimitive,
        CoreTools::train_reflexes,
    );
    ToolRegistry::register_meta_tool(
        registry,
        "read_file",
        "Read file content in workspace",
        MetaCategory::WorkspaceIo,
        CoreTools::read_file,
    );
    ToolRegistry::register_meta_tool(
        registry,
        "write_file",
        "Write content to workspace file",
        MetaCategory::WorkspaceIo,
        CoreTools::write_file,
    );
    ToolRegistry::register_meta_tool(
        registry,
        "exec_command",
        "Execute command in workspace",
        MetaCategory::WorkspaceIo,
        CoreTools::exec_command,
    );
    ToolRegistry::register_meta_tool(
        registry,
        "agents_list",
        "List managed external executors and setup readiness",
        MetaCategory::IntelligenceBridge,
        CoreTools::agents_list,
    );
    ToolRegistry::register_meta_tool(
        registry,
        "agents_run",
        "Launch external task with agent and prompt",
        MetaCategory::IntelligenceBridge,
        CoreTools::agents_run,
    );
    ToolRegistry::register_meta_tool(
        registry,
        "agents_tasks",
        "List durable external tasks",
        MetaCategory::IntelligenceBridge,
        CoreTools::agents_tasks,
    );
    ToolRegistry::register_meta_tool(
        registry,
        "agents_status",
        "Inspect external task_id; optional refresh",
        MetaCategory::IntelligenceBridge,
        CoreTools::agents_status,
    );
    ToolRegistry::register_meta_tool(
        registry,
        "agents_cancel",
        "Cancel external task_id",
        MetaCategory::IntelligenceBridge,
        CoreTools::agents_cancel,
    );
    ToolRegistry::register_meta_tool(
        registry,
        "agents_logs",
        "Read external task_id output; optional stderr",
        MetaCategory::IntelligenceBridge,
        CoreTools::agents_logs,
    );
    ToolRegistry::register_meta_tool(
        registry,
        "agents_send",
        "Send message to cloud task_id",
        MetaCategory::IntelligenceBridge,
        CoreTools::agents_send,
    );
    ToolRegistry::register_meta_tool(
        registry,
        "coding_models_list",
        "List top coding/agent models and readiness",
        MetaCategory::IntelligenceBridge,
        CoreTools::coding_models_list,
    );
    ToolRegistry::register_meta_tool(
        registry,
        "coding_models_prefer",
        "Prefer coding model id for routing",
        MetaCategory::IntelligenceBridge,
        CoreTools::coding_models_prefer,
    );
    ToolRegistry::register_meta_tool(
        registry,
        "frameworks_list",
        "List managed agent frameworks and setup readiness",
        MetaCategory::IntelligenceBridge,
        CoreTools::frameworks_list,
    );
    ToolRegistry::register_meta_tool(
        registry,
        "frameworks_run",
        "Launch framework task with engine and prompt",
        MetaCategory::IntelligenceBridge,
        CoreTools::frameworks_run,
    );
    ToolRegistry::register_meta_tool(
        registry,
        "frameworks_tasks",
        "List durable framework tasks",
        MetaCategory::IntelligenceBridge,
        CoreTools::frameworks_tasks,
    );
    ToolRegistry::register_meta_tool(
        registry,
        "frameworks_status",
        "Inspect framework task_id",
        MetaCategory::IntelligenceBridge,
        CoreTools::frameworks_status,
    );
    ToolRegistry::register_meta_tool(
        registry,
        "frameworks_cancel",
        "Cancel framework task_id",
        MetaCategory::IntelligenceBridge,
        CoreTools::frameworks_cancel,
    );
    ToolRegistry::register_meta_tool(
        registry,
        "frameworks_logs",
        "Read framework task_id output; optional stderr",
        MetaCategory::IntelligenceBridge,
        CoreTools::frameworks_logs,
    );
    ToolRegistry::register_meta_tool(
        registry,
        "tasks_list",
        "List active and historical swarm tasks with liveness telemetry",
        MetaCategory::SystemPrimitive,
        CoreTools::tasks_list,
    );
    ToolRegistry::register_meta_tool(
        registry,
        "tasks_pause",
        "Pause a running task by task_id",
        MetaCategory::SystemPrimitive,
        CoreTools::tasks_pause,
    );
    ToolRegistry::register_meta_tool(
        registry,
        "tasks_resume",
        "Resume a paused task by task_id",
        MetaCategory::SystemPrimitive,
        CoreTools::tasks_resume,
    );
    ToolRegistry::register_meta_tool(
        registry,
        "tasks_kill",
        "Kill a running or stalled task by task_id",
        MetaCategory::SystemPrimitive,
        CoreTools::tasks_kill,
    );
    ToolRegistry::register_meta_tool(
        registry,
        "mcp_registry",
        "Interrogate global MCP registry and benchmark servers",
        MetaCategory::McpProxy,
        CoreTools::mcp_registry,
    );
    ToolRegistry::register_meta_tool(
        registry,
        "mcp_configure",
        "Configure external MCP server",
        MetaCategory::McpProxy,
        CoreTools::mcp_configure,
    );
    ToolRegistry::register_meta_tool(
        registry,
        "leading_mcp_list",
        "List top MCP servers and readiness",
        MetaCategory::McpProxy,
        CoreTools::leading_mcp_list,
    );
    ToolRegistry::register_meta_tool(
        registry,
        "leading_mcp_enable",
        "Enable leading MCP server into mcp_config",
        MetaCategory::McpProxy,
        CoreTools::leading_mcp_enable,
    );
    ToolRegistry::register_meta_tool(
        registry,
        "leading_mcp_disable",
        "Disable leading MCP server from mcp_config",
        MetaCategory::McpProxy,
        CoreTools::leading_mcp_disable,
    );
    ToolRegistry::register_meta_tool(
        registry,
        "agent_register",
        "Dynamically register a new agent profile",
        MetaCategory::IntelligenceBridge,
        CoreTools::agent_register,
    );
    ToolRegistry::register_meta_tool(
        registry,
        "reason",
        "Execute swarm reasoning substrate",
        MetaCategory::SystemPrimitive,
        CoreTools::reason,
    );
    ToolRegistry::register_meta_tool(
        registry,
        "susi_solve",
        "Solve a natural-language intent via the SUSI swarm substrate",
        MetaCategory::SystemPrimitive,
        CoreTools::susi_solve,
    );
    ToolRegistry::register_meta_tool(
        registry,
        "power_reason",
        "Delegate complex reasoning to Power-Tier MCP remotes",
        MetaCategory::IntelligenceBridge,
        CoreTools::power_reason,
    );
    ToolRegistry::register_meta_tool(
        registry,
        "meta_scout_agents",
        "Discover agent capabilities from connected remotes",
        MetaCategory::IntelligenceBridge,
        CoreTools::meta_scout_agents,
    );
    ToolRegistry::register_meta_tool(
        registry,
        "meta_rank_agents",
        "Report current agent expertise hierarchy",
        MetaCategory::IntelligenceBridge,
        CoreTools::meta_rank_agents,
    );

    // SPECIALIST TOOLBOXES: Type 1 (Coding) & Type 2 (Assistant)
    #[cfg(feature = "tools-rich")]
    ToolRegistry::register_meta_tool(
        registry,
        "ast_analyze",
        "Structural AST code analysis via tree-sitter",
        MetaCategory::CodingSpecialist,
        CoreTools::ast_analyze,
    );
    #[cfg(feature = "tools-rich")]
    ToolRegistry::register_meta_tool(
        registry,
        "semantic_search",
        "Fast embedded search via tantivy",
        MetaCategory::CodingSpecialist,
        CoreTools::semantic_search,
    );
    ToolRegistry::register_meta_tool(
        registry,
        "sandbox_exec",
        "Isolated Docker execution via bollard",
        MetaCategory::CodingSpecialist,
        CoreTools::sandbox_exec,
    );
    #[cfg(feature = "tools-rich")]
    ToolRegistry::register_meta_tool(
        registry,
        "browser_automate",
        "DOM access and web automation via headless_chrome",
        MetaCategory::AssistantSpecialist,
        CoreTools::browser_automate,
    );
    #[cfg(feature = "tools-rich")]
    ToolRegistry::register_meta_tool(
        registry,
        "rag_query",
        "Semantic memory retrieval via Qdrant/FastEmbed",
        MetaCategory::AssistantSpecialist,
        CoreTools::rag_query,
    );
    ToolRegistry::register_meta_tool(
        registry,
        "audio_transcribe",
        "Production-grade transcription substrate",
        MetaCategory::AssistantSpecialist,
        CoreTools::audio_transcribe,
    );

    // DYNAMIC DISCOVERY: Synthesized Native Reflexes
    crate::reflexes::register_synthesized_reflexes(registry);

    // Zero-Config Auto-Link: Ensure essential MCP tools are mapped (Non-Blocking Mandate)
    std::thread::spawn(|| {
        ToolRegistry::auto_link_essential_mcp_servers();
    });
}
