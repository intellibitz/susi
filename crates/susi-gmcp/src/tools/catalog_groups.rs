//! Catalog sections whose tool membership and order are shared by the MCP
//! wire definitions and daemon registration. Each consumer supplies the two
//! sections that historically have different ordering and chooses its own
//! metadata from each shared row. Feature gates expand in the consumer.

#[doc(hidden)]
#[macro_export]
macro_rules! gmcp_catalog_groups {
    ($collect:ident, [$($head:tt)*], [$($between:tt)*]) => {
        $collect! {
            $($head)*
            [agents_list_tool_attr] agents_list: IntelligenceBridge => "List managed external executors and setup readiness";
            [agents_run_tool_attr] agents_run: IntelligenceBridge => "Launch external task with agent and prompt";
            [agents_tasks_tool_attr] agents_tasks: IntelligenceBridge => "List durable external tasks";
            [agents_status_tool_attr] agents_status: IntelligenceBridge => "Inspect external task_id; optional refresh";
            [agents_cancel_tool_attr] agents_cancel: IntelligenceBridge => "Cancel external task_id";
            [agents_logs_tool_attr] agents_logs: IntelligenceBridge => "Read external task_id output; optional stderr";
            [agents_send_tool_attr] agents_send: IntelligenceBridge => "Send message to cloud task_id";
            [coding_models_list_tool_attr] coding_models_list: IntelligenceBridge => "List top coding/agent models and readiness";
            [coding_models_prefer_tool_attr] coding_models_prefer: IntelligenceBridge => "Prefer coding model id for routing";
            [frameworks_list_tool_attr] frameworks_list: IntelligenceBridge => "List managed agent frameworks and setup readiness";
            [frameworks_run_tool_attr] frameworks_run: IntelligenceBridge => "Launch framework task with engine and prompt";
            [frameworks_tasks_tool_attr] frameworks_tasks: IntelligenceBridge => "List durable framework tasks";
            [frameworks_status_tool_attr] frameworks_status: IntelligenceBridge => "Inspect framework task_id";
            [frameworks_cancel_tool_attr] frameworks_cancel: IntelligenceBridge => "Cancel framework task_id";
            [frameworks_logs_tool_attr] frameworks_logs: IntelligenceBridge => "Read framework task_id output; optional stderr";
            [tasks_list_tool_attr] tasks_list: SystemPrimitive => "List active and historical swarm tasks with liveness telemetry";
            [tasks_pause_tool_attr] tasks_pause: SystemPrimitive => "Pause a running task by task_id";
            [tasks_resume_tool_attr] tasks_resume: SystemPrimitive => "Resume a paused task by task_id";
            [tasks_kill_tool_attr] tasks_kill: SystemPrimitive => "Kill a running or stalled task by task_id";
            [mcp_registry_tool_attr] mcp_registry: McpProxy => "Interrogate global MCP registry and benchmark servers";
            [mcp_configure_tool_attr] mcp_configure: McpProxy => "Configure external MCP server";
            [leading_mcp_list_tool_attr] leading_mcp_list: McpProxy => "List top MCP servers and readiness";
            [leading_mcp_enable_tool_attr] leading_mcp_enable: McpProxy => "Enable leading MCP server into mcp_config";
            [leading_mcp_disable_tool_attr] leading_mcp_disable: McpProxy => "Disable leading MCP server from mcp_config";
            [agent_register_tool_attr] agent_register: IntelligenceBridge => "Dynamically register a new agent profile";
            [reason_tool_attr] reason: SystemPrimitive => "Execute swarm reasoning substrate";
            [susi_solve_tool_attr] susi_solve: SystemPrimitive => "Solve a natural-language intent via the SUSI swarm substrate";
            [mission_run_tool_attr] mission_run: SystemPrimitive => "Run a goal as `susi <goal>` does and report its status and exit code";
            [power_reason_tool_attr] power_reason: IntelligenceBridge => "Delegate complex reasoning to Power-Tier MCP remotes";
            [meta_scout_agents_tool_attr] meta_scout_agents: IntelligenceBridge => "Discover agent capabilities from connected remotes";
            [meta_rank_agents_tool_attr] meta_rank_agents: IntelligenceBridge => "Report current agent expertise hierarchy";

            // SPECIALIST TOOLBOXES: Type 1 (Coding) & Type 2 (Assistant)
            #[cfg(feature = "tools-rich")]
            [ast_analyze_tool_attr] ast_analyze: CodingSpecialist => "Structural AST analysis of Rust source (syn); other languages report size only";
            #[cfg(feature = "tools-rich")]
            [semantic_search_tool_attr] semantic_search: CodingSpecialist => "Unified BM25 recall over .susi stores and workspace files";
            [sandbox_exec_tool_attr] sandbox_exec: CodingSpecialist => "Isolated Docker execution via bollard";
            #[cfg(feature = "tools-rich")]
            [browser_automate_tool_attr] browser_automate: AssistantSpecialist => "DOM access and web automation via headless Chrome";
            #[cfg(feature = "tools-rich")]
            [rag_query_tool_attr] rag_query: AssistantSpecialist => "Vector recall over the unified .susi index (local fastembed)";
            [context_graph_query_tool_attr] context_graph_query: SystemPrimitive => "Query the Universal Context Graph for the current workspace or a specific node id";
            [context_graph_ingest_tool_attr] context_graph_ingest: SystemPrimitive => "Ingest an external context event into the Universal Context Graph";
            [context_graph_compact_tool_attr] context_graph_compact: SystemPrimitive => "Compact the append-only Universal Context Graph log";
            [ipc_grant_tool_attr] ipc_grant: SystemPrimitive => "Grant an inter-app permission scope";
            [ipc_request_tool_attr] ipc_request: SystemPrimitive => "Request an inter-app permission (opens negotiation)";
            [ipc_negotiate_tool_attr] ipc_negotiate: SystemPrimitive => "Approve or deny a pending permission request";
            [ipc_send_tool_attr] ipc_send: SystemPrimitive => "Send a broker message (requires dispatch grant unless from=susi)";
            [ipc_receive_tool_attr] ipc_receive: SystemPrimitive => "Receive broker messages for an identity";
            [host_telemetry_tool_attr] host_telemetry: SystemPrimitive => "Sample host thermal, battery, and load telemetry";
            [apply_patch_cycle_tool_attr] apply_patch_cycle: WorkspaceIo => "Apply a workspace-confined patch then run tests; revert on failure";
            [privacy_status_tool_attr] privacy_status: SystemPrimitive => "Report privacy mode and cryptographic capability grants";
            [privacy_consent_egress_tool_attr] privacy_consent_egress: SystemPrimitive => "Grant time-limited network egress / cloud inference consent";
            [intent_advertise_tool_attr] intent_advertise: SystemPrimitive => "Advertise a provider capability on the semantic intent bus";
            [intent_need_tool_attr] intent_need: SystemPrimitive => "Publish a need and match providers on the semantic intent bus";
            #[cfg(feature = "tools-rich")]
            [ambient_pulse_tool_attr] ambient_pulse: SystemPrimitive => "Scan workspace, record FS changes into context graph, refresh vector index";
            [tx_begin_tool_attr] tx_begin: WorkspaceIo => "Begin a multi-agent transaction snapshotting listed files";
            [tx_commit_tool_attr] tx_commit: WorkspaceIo => "Commit an open multi-agent transaction";
            [tx_abort_tool_attr] tx_abort: WorkspaceIo => "Abort a transaction and restore snapshotted files";

            $($between)*
            [plan_run_tool_attr] plan_run: WorkspaceIo => "Solve a goal with the autonomous planning loop in the server workspace";
            [intent_ingest_tool_attr] intent_ingest: WorkspaceIo => "Ingest a natural-language intent into the server workspace evidence ledger";
            [blackboard_show_tool_attr] blackboard_show: SystemPrimitive => "Show the last mission blackboard (credential-redacted)";
            [blackboard_post_tool_attr] blackboard_post: WorkspaceIo => "Post a key-value update to the shared blackboard and Susi Native Board";
            [board_list_tool_attr] board_list: SystemPrimitive => "List collaborative items on the Susi Native Board";
            [board_check_tool_attr] board_check: SystemPrimitive => "Check the Susi Native Board for open stops and unanswered requests";
            [board_post_tool_attr] board_post: WorkspaceIo => "Post an item onto the Susi Native Board";
            [runtime_models_list_tool_attr] runtime_models_list: SystemPrimitive => "List models loaded in the inference cache";
            [runtime_models_load_tool_attr] runtime_models_load: WorkspaceIo => "Load a model into the inference cache";
            [runtime_models_unload_tool_attr] runtime_models_unload: WorkspaceIo => "Evict a model from the inference cache";
            [placement_decision_tool_attr] placement_decision: SystemPrimitive => "Look up an audited inference-placement decision by id";
            [context_graph_stats_tool_attr] context_graph_stats: SystemPrimitive => "Universal Context Graph node and edge counts by kind";
            [context_graph_show_tool_attr] context_graph_show: SystemPrimitive => "Most recent Universal Context Graph events for a workspace";

            // Zero-UI plan B4 groups 4, 5, 8 and 9: onboarding, assurance, context
            // and IPC, and the OS view, reachable over the standards socket.
            [ipc_pending_tool_attr] ipc_pending: SystemPrimitive => "List pending inter-app permission requests";
            [ipc_check_tool_attr] ipc_check: SystemPrimitive => "Check whether an identity holds an inter-app permission scope";
            [ipc_revoke_tool_attr] ipc_revoke: WorkspaceIo => "Revoke an inter-app permission scope from an identity";
            [ipc_list_tool_attr] ipc_list: SystemPrimitive => "List the inter-app permission grants an identity holds";
            [privacy_mode_tool_attr] privacy_mode: WorkspaceIo => "Set the privacy mode (balanced, local_only = this machine and its local network, host_only = this machine alone, open)";
            [privacy_grant_tool_attr] privacy_grant: WorkspaceIo => "Grant a cryptographic capability token";
            [privacy_revoke_tool_attr] privacy_revoke: WorkspaceIo => "Revoke a capability grant";
            [intent_list_tool_attr] intent_list: SystemPrimitive => "List active intent-bus providers and needs";
            [ambient_start_tool_attr] ambient_start: WorkspaceIo => "Start the background ambient indexer for the server workspace";
            [tx_list_tool_attr] tx_list: SystemPrimitive => "List open multi-agent transactions in the server workspace";
            [context_graph_sample_os_tool_attr] context_graph_sample_os: WorkspaceIo => "Sample OS context into the Universal Context Graph";
            [audit_verify_tool_attr] audit_verify: SystemPrimitive => "Verify the server workspace's tamper-evident audit chain";
            [audit_query_tool_attr] audit_query: SystemPrimitive => "Query redacted action history from the audit chain";
            [compliance_audit_tool_attr] compliance_audit: SystemPrimitive => "Run the deterministic compliance audit";
            [audit_evidence_export_tool_attr] audit_evidence_export: WorkspaceIo => "Export redacted audit segments in the verifiable format";
            [substrate_report_tool_attr] substrate_report: SystemPrimitive => "Compiled-constants substrate report";
            [substrate_status_tool_attr] substrate_status: SystemPrimitive => "Pluggable / Sandbox / Governance / Host-contract / Reflexes status";
            [crown_verify_tool_attr] crown_verify: SystemPrimitive => "Verify every Tier S USP";
            [clean_tool_attr] clean: WorkspaceIo => "Remove SUSI-owned residue and report bytes freed";
            [setup_plan_tool_attr] setup_plan: SystemPrimitive => "Detect the host and return the zero-config setup plan";
            [setup_apply_tool_attr] setup_apply: WorkspaceIo => "Apply the zero-config setup plan (existing settings preserved)";
            [ecosystem_scan_tool_attr] ecosystem_scan: SystemPrimitive => "Scan (or return the last scan of) the local AI ecosystem";
            [ecosystem_start_tool_attr] ecosystem_start: WorkspaceIo => "Start an installed local AI engine";
            [ecosystem_kb_tool_attr] ecosystem_kb: SystemPrimitive => "Query the cloud-AI-ecosystem knowledge base";
            [ecosystem_explain_tool_attr] ecosystem_explain: SystemPrimitive => "Explain a knowledge-base component, standard or protocol";
            [os_provision_tool_attr] os_provision: WorkspaceIo => "Explicit cloud provisioning: probe, list or apply";
            [os_capabilities_tool_attr] os_capabilities: WorkspaceIo => "Discover or invoke typed capability-bus capabilities";
            [services_start_tool_attr] services_start: WorkspaceIo => "Clear an operator stop on a supervised leaf service";
            [services_stop_tool_attr] services_stop: WorkspaceIo => "Stop a supervised leaf service and hold it down";
            [services_restart_tool_attr] services_restart: WorkspaceIo => "Restart a supervised leaf service";
            [commits_audit_tool_attr] commits_audit: SystemPrimitive => "Whole-ledger consistency check of the commit log";
            [commits_replay_tool_attr] commits_replay: SystemPrimitive => "Consensus view the commit ledger replays to";
            [commits_show_tool_attr] commits_show: SystemPrimitive => "Show one commit record by epoch prefix";
            [commits_sync_tool_attr] commits_sync: WorkspaceIo => "Commit-ledger anti-entropy with verified peers";
            [commits_compact_tool_attr] commits_compact: WorkspaceIo => "Compact the commit ledger into a snapshot plus archive";
            [peers_probe_tool_attr] peers_probe: SystemPrimitive => "Cryptographically verify a peer host without admitting it";
            [peers_list_tool_attr] peers_list: SystemPrimitive => "List the verified cluster roster, leader and banned members";
            [peers_status_tool_attr] peers_status: SystemPrimitive => "Every rostered member's cluster status side by side";

            // Zero-UI plan B4 group 3: providers, keys (presence only) and the
            // brain, reachable over the standards socket.
            [provider_list_tool_attr] provider_list: SystemPrimitive => "List a provider control plane's curated models and readiness";
            [provider_doctor_tool_attr] provider_doctor: SystemPrimitive => "Check a provider control plane's readiness (no paid call)";
            [provider_setup_tool_attr] provider_setup: SystemPrimitive => "Show a provider model's setup instructions and effective wiring";
            [provider_configure_tool_attr] provider_configure: WorkspaceIo => "Store a host override for a provider model";
            [provider_reset_tool_attr] provider_reset: WorkspaceIo => "Drop a provider model's host override";
            [provider_pull_tool_attr] provider_pull: WorkspaceIo => "Pull an open-weight model's weights through Ollama";
            [provider_prefer_tool_attr] provider_prefer: WorkspaceIo => "Prefer or clear a provider model for routing";
            [provider_probe_tool_attr] provider_probe: SystemPrimitive => "Probe a model endpoint with a short completion (cloud probes are paid)";
            [provider_models_tool_attr] provider_models: SystemPrimitive => "List OpenRouter routes or the live models its key reaches";
            [keys_status_tool_attr] keys_status: SystemPrimitive => "Report which cloud vendor keys are set (presence only, never values)";
            [keys_prefer_tool_attr] keys_prefer: WorkspaceIo => "Pin, clear or report the preferred cloud vendor";
            [keys_check_tool_attr] keys_check: SystemPrimitive => "Check each vendor key against its model list (values never returned)";
            [keys_models_tool_attr] keys_models: SystemPrimitive => "List the models a vendor's valid key reaches";
            [brain_status_tool_attr] brain_status: SystemPrimitive => "Evidence-ranked providers for every task class";
            [brain_budget_tool_attr] brain_budget: WorkspaceIo => "Set or report the budget dial and shared spend policy";
            [brain_classify_tool_attr] brain_classify: SystemPrimitive => "Show which task class a prompt routes as";
            [brain_explain_tool_attr] brain_explain: SystemPrimitive => "Explain the brain's routing decision for a prompt";
            [brain_export_tool_attr] brain_export: SystemPrimitive => "Export the brain evidence bundle";
            [brain_import_tool_attr] brain_import: WorkspaceIo => "Merge a brain evidence bundle";
            [brain_matrix_tool_attr] brain_matrix: SystemPrimitive => "Show the measured model capability matrix";
            [brain_cache_tool_attr] brain_cache: SystemPrimitive => "Show the measured prompt-cache hit rate and saving";
            [brain_reset_tool_attr] brain_reset: WorkspaceIo => "Forget every recorded brain outcome";
            [brain_bench_tool_attr] brain_bench: WorkspaceIo => "Run the fixed prompt set against the fake engine and record evidence";
            [brain_probe_tool_attr] brain_probe: WorkspaceIo => "Probe a registered provider with the scout benchmark";
            [benchmark_run_tool_attr] benchmark_run: SystemPrimitive => "Measure local (and configured cloud) inference latency and throughput";
            [verify_download_agent_tool_attr] verify_download_agent: SystemPrimitive => "Verify network status and the hardware-selected model ladder";

            // Zero-UI plan B4 group 7: adapter lifecycle for agents, frameworks,
            // coding models and MCP servers.
            [adapter_doctor_tool_attr] adapter_doctor: SystemPrimitive => "Check agent, framework, coding-model or MCP adapter readiness";
            [adapter_setup_tool_attr] adapter_setup: SystemPrimitive => "Show an adapter's setup instructions and effective configuration";
            [adapter_configure_tool_attr] adapter_configure: WorkspaceIo => "Store a host adapter override (verified upgrade for agents and frameworks)";
            [adapter_reset_tool_attr] adapter_reset: WorkspaceIo => "Drop a host adapter override and restore the bundled adapter";
            [agents_retry_tool_attr] agents_retry: WorkspaceIo => "Run a finished external agent task again with its original prompt";
            [frameworks_retry_tool_attr] frameworks_retry: WorkspaceIo => "Run a finished agent-framework task again with its original prompt";
            [mcp_add_tool_attr] mcp_add: WorkspaceIo => "Admit an MCP server into the registry and hot-plug it";

            // Zero-UI plan B4 group 1: the self-build harness.
            [harness_status_tool_attr] harness_status: SystemPrimitive => "Harness API snapshot for a ledger: spend, tasks, workers, warnings";
            [harness_budget_tool_attr] harness_budget: SystemPrimitive => "One-line harness budget report and top-up alerts";
            [harness_models_tool_attr] harness_models: SystemPrimitive => "Harness provider registry, drift check, discovery or keyed models";
            [harness_plan_tool_attr] harness_plan: WorkspaceIo => "Plan a batch of dependency-ready queue tasks";
            [harness_author_tool_attr] harness_author: WorkspaceIo => "Author a plan for a roadmap vector through the metered gateway";
            [harness_run_tool_attr] harness_run: WorkspaceIo => "Start running a harness plan under the budget gate";
            [harness_runs_tool_attr] harness_runs: SystemPrimitive => "Harness missions this daemon started and their outcomes";

            // Zero-UI plan B4 groups 2, 4, 9, 10 and 13: host views, maintenance
            // and daemon lifecycle.
            [operator_status_tool_attr] operator_status: SystemPrimitive => "Measured executable identity and substrate counts (as `susi status --json`)";
            [operator_review_tool_attr] operator_review: SystemPrimitive => "Daemon liveness and reclaimable OS cache (as `susi review`)";
            [os_ecosystem_tool_attr] os_ecosystem: SystemPrimitive => "Local AI ecosystem: hardware, runtimes, models, adapters, MCP servers, AI processes";
            [os_status_tool_attr] os_status: SystemPrimitive => "Substrate status: consensus, ledger, services, peers, endpoints, storage";
            [os_runtime_tool_attr] os_runtime: SystemPrimitive => "Live AI processes by owner, managed-model residency and GPU memory";
            [model_uninstall_tool_attr] model_uninstall: WorkspaceIo => "Remove one SUSI-managed model file (refused while loaded or selected)";
            [peers_add_tool_attr] peers_add: WorkspaceIo => "Join a peer by signed handshake and commit its admission";
            [peers_remove_tool_attr] peers_remove: WorkspaceIo => "Evict and ban a verified peer, committed cluster-wide";
            [peers_unban_tool_attr] peers_unban: WorkspaceIo => "Lift a peer ban, committed cluster-wide";
            [peers_rekey_tool_attr] peers_rekey: WorkspaceIo => "Rotate the cluster key across every verified member";
            [daemon_shutdown_tool_attr] daemon_shutdown: WorkspaceIo => "Stop this daemon instance after replying";
            [extensions_create_tool_attr] extensions_create: WorkspaceIo => "Create an empty extension pack shell (not activated)";
            [auto_status_tool_attr] auto_status: SystemPrimitive => "Zero-config substrate readiness snapshot";
            [auto_prime_tool_attr] auto_prime: WorkspaceIo => "Re-run zero-config auto-prime (never enables MCP)";
            [deep_scan_tool_attr] deep_scan: WorkspaceIo => "Scan the home directory for model files and register them";
            [config_reload_tool_attr] config_reload: WorkspaceIo => "Reload config.json and report what is in effect";
            [provider_contract_tool_attr] provider_contract: SystemPrimitive => "Provider contract fixture checks (credentialed half opt-in)";
            [intent_ladder_tool_attr] intent_ladder: WorkspaceIo => "Intent ladder scorecard, runs, verification, gap outbox and adoption";
        }
    };
}
