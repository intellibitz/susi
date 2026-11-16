//! Catalog sections whose tool membership and order are shared by the MCP
//! wire definitions and daemon registration. Each consumer supplies the two
//! sections that historically have different ordering and chooses its own
//! metadata from each shared row. Feature gates expand in the consumer.

#[doc(hidden)]
#[macro_export]
macro_rules! gmcp_catalog_groups {
    ($collect:ident, [$($head:tt)*], [$($between:tt)*]) => {
        $crate::gmcp_catalog_groups! { @rows registry $collect, [$($head)*], [$($between)*] }
    };
    // Preserve the exported registry callback's original tokens, including its
    // caller-supplied sections. Schema consumers also receive wire metadata.
    (@registry $collect:ident; $( [$($prefix:tt)*] {
        $($(#[$attr:meta])* [$definition:ident $(, $schema:expr => $wire:literal)?]
          $handler:ident: $category:ident => $summary:literal;)*
    } )*) => {
        $collect! { $( $($prefix)* $($(#[$attr])* [$definition] $handler: $category => $summary;)* )* }
    };
    (@wire $collect:ident; $( [$($prefix:tt)*] { $($rows:tt)* } )*) => {
        $collect! { $( $($prefix)* $($rows)* )* }
    };
    (@rows $view:ident $collect:ident, [$($head:tt)*], [$($between:tt)*]) => {
        $crate::gmcp_catalog_groups! {
            @$view $collect;
            [$($head)*] {
            [agents_list_tool_attr, no_args() => "List managed external executors and local setup readiness"]
            agents_list: IntelligenceBridge => "List managed external executors and setup readiness";
            [agents_run_tool_attr, typed::<AgentsRun>() => "Launch an external agent task; returns a durable task ID for status, logs and cancellation. Optional workspace: the absolute directory the CLI would run in, admitted by the MAC policy (default: the server workspace)."]
            agents_run: IntelligenceBridge => "Launch external task with agent and prompt";
            [agents_tasks_tool_attr, typed::<WorkspaceView>() => "List persisted external agent tasks for this workspace. Optional workspace: the absolute directory the CLI would run in, admitted by the MAC policy (default: the server workspace)."]
            agents_tasks: IntelligenceBridge => "List durable external tasks";
            [agents_status_tool_attr] agents_status: IntelligenceBridge => "Inspect external task_id; optional refresh";
            [agents_cancel_tool_attr] agents_cancel: IntelligenceBridge => "Cancel external task_id";
            [agents_logs_tool_attr] agents_logs: IntelligenceBridge => "Read external task_id output; optional stderr";
            [agents_send_tool_attr] agents_send: IntelligenceBridge => "Send message to cloud task_id";
            [coding_models_list_tool_attr, no_args() => "List top developer/agent models and local setup readiness"]
            coding_models_list: IntelligenceBridge => "List top coding/agent models and readiness";
            [coding_models_prefer_tool_attr, typed::<CodingModelsPrefer>() => "Prefer a coding/agent model id for subsequent routing"]
            coding_models_prefer: IntelligenceBridge => "Prefer coding model id for routing";
            [frameworks_list_tool_attr, no_args() => "List managed agent frameworks/engines and local setup readiness"]
            frameworks_list: IntelligenceBridge => "List managed agent frameworks and setup readiness";
            [frameworks_run_tool_attr, typed::<FrameworksRun>() => "Launch an agent-framework task; returns a durable task ID. Optional workspace: the absolute directory the CLI would run in, admitted by the MAC policy (default: the server workspace)."]
            frameworks_run: IntelligenceBridge => "Launch framework task with engine and prompt";
            [frameworks_tasks_tool_attr, typed::<WorkspaceView>() => "List persisted agent-framework tasks for this workspace. Optional workspace: the absolute directory the CLI would run in, admitted by the MAC policy (default: the server workspace)."]
            frameworks_tasks: IntelligenceBridge => "List durable framework tasks";
            [frameworks_status_tool_attr] frameworks_status: IntelligenceBridge => "Inspect framework task_id";
            [frameworks_cancel_tool_attr] frameworks_cancel: IntelligenceBridge => "Cancel framework task_id";
            [frameworks_logs_tool_attr] frameworks_logs: IntelligenceBridge => "Read framework task_id output; optional stderr";
            [tasks_list_tool_attr, no_args() => "List active and historical swarm tasks with liveness telemetry"]
            tasks_list: SystemPrimitive => "List active and historical swarm tasks with liveness telemetry";
            [tasks_pause_tool_attr] tasks_pause: SystemPrimitive => "Pause a running task by task_id";
            [tasks_resume_tool_attr] tasks_resume: SystemPrimitive => "Resume a paused task by task_id";
            [tasks_kill_tool_attr] tasks_kill: SystemPrimitive => "Kill a running or stalled task by task_id";
            [mcp_registry_tool_attr, no_args() => "Interrogate global MCP registry and benchmark servers"]
            mcp_registry: McpProxy => "Interrogate global MCP registry and benchmark servers";
            [mcp_configure_tool_attr, typed::<McpConfigure>() => "Configure external MCP server"]
            mcp_configure: McpProxy => "Configure external MCP server";
            [leading_mcp_list_tool_attr, no_args() => "List top MCP tool servers and enablement/readiness"]
            leading_mcp_list: McpProxy => "List top MCP servers and readiness";
            [leading_mcp_enable_tool_attr, typed::<LeadingMcpServer>() => "Enable a leading MCP server into ~/.susi/mcp_config.json"]
            leading_mcp_enable: McpProxy => "Enable leading MCP server into mcp_config";
            [leading_mcp_disable_tool_attr, typed::<LeadingMcpServer>() => "Disable a leading MCP server from ~/.susi/mcp_config.json"]
            leading_mcp_disable: McpProxy => "Disable leading MCP server from mcp_config";
            [agent_register_tool_attr, typed::<AgentRegister>() => "Dynamically register a new agent profile"]
            agent_register: IntelligenceBridge => "Dynamically register a new agent profile";
            [reason_tool_attr, typed::<ReasonPrompt>() => "Execute swarm reasoning substrate"]
            reason: SystemPrimitive => "Execute swarm reasoning substrate";
            [susi_solve_tool_attr, typed::<SusiSolve>() => "Solve a natural-language intent via the SUSI swarm substrate. Optional workspace: the absolute directory the CLI would run in, admitted by the MAC policy (default: the server workspace)."]
            susi_solve: SystemPrimitive => "Solve a natural-language intent via the SUSI swarm substrate";
            [mission_run_tool_attr, typed::<MissionRun>() => "Run a goal as `susi <goal>` does: a stopped local engine may be started on demand, and the result is one JSON document {text, status, exit_code} (exit_code 0 only for SUCCESS or COMPLETE). Optional workspace: the absolute directory the CLI would run in, admitted by the MAC policy (default: the server workspace). Optional model: an advisory model hint."]
            mission_run: SystemPrimitive => "Run a goal as `susi <goal>` does and report its status and exit code";
            [power_reason_tool_attr, typed::<ReasonPrompt>() => "Delegate complex reasoning to Power-Tier MCP remotes"]
            power_reason: IntelligenceBridge => "Delegate complex reasoning to Power-Tier MCP remotes";
            [meta_scout_agents_tool_attr, no_args() => "Discover agent capabilities from connected remotes"]
            meta_scout_agents: IntelligenceBridge => "Discover agent capabilities from connected remotes";
            [meta_rank_agents_tool_attr, no_args() => "Report current agent expertise hierarchy"]
            meta_rank_agents: IntelligenceBridge => "Report current agent expertise hierarchy";

            // SPECIALIST TOOLBOXES: Type 1 (Coding) & Type 2 (Assistant)
            #[cfg(feature = "tools-rich")]
            [ast_analyze_tool_attr, typed::<AstAnalyze>() => "Structural AST analysis of Rust source (syn); other languages report size only"]
            ast_analyze: CodingSpecialist => "Structural AST analysis of Rust source (syn); other languages report size only";
            #[cfg(feature = "tools-rich")]
            [semantic_search_tool_attr, typed::<Query>() => "Unified BM25 recall over .susi memory, experience, audit log, and workspace files"]
            semantic_search: CodingSpecialist => "Unified BM25 recall over .susi stores and workspace files";
            [sandbox_exec_tool_attr, typed::<SandboxExec>() => "Isolated Docker execution via bollard"]
            sandbox_exec: CodingSpecialist => "Isolated Docker execution via bollard";
            #[cfg(feature = "tools-rich")]
            [browser_automate_tool_attr, typed::<BrowserAutomate>() => "DOM access and web automation via headless Chrome"]
            browser_automate: AssistantSpecialist => "DOM access and web automation via headless Chrome";
            #[cfg(feature = "tools-rich")]
            [rag_query_tool_attr, typed::<Query>() => "Vector recall over the unified .susi index via local fastembed cosine similarity"]
            rag_query: AssistantSpecialist => "Vector recall over the unified .susi index (local fastembed)";
            [context_graph_query_tool_attr, typed::<ContextGraphQuery>() => "Query the Universal Context Graph for the current workspace or a specific node id"]
            context_graph_query: SystemPrimitive => "Query the Universal Context Graph for the current workspace or a specific node id";
            [context_graph_ingest_tool_attr, typed::<ContextGraphIngest>() => "Ingest an external context event into the Universal Context Graph. Args: source (string), label (string), payload (object), optional workspace, optional user."]
            context_graph_ingest: SystemPrimitive => "Ingest an external context event into the Universal Context Graph";
            [context_graph_compact_tool_attr, no_args() => "Compact the append-only Universal Context Graph log"]
            context_graph_compact: SystemPrimitive => "Compact the append-only Universal Context Graph log";
            [ipc_grant_tool_attr, typed::<IpcGrant>() => "Grant an inter-app permission scope. Args: grantor, grantee, resource, action, optional ttl_secs."]
            ipc_grant: SystemPrimitive => "Grant an inter-app permission scope";
            [ipc_request_tool_attr, typed::<IpcRequest>() => "Request an inter-app permission (opens negotiation). Args: requester, resource, action, optional grantor, optional ttl_secs."]
            ipc_request: SystemPrimitive => "Request an inter-app permission (opens negotiation)";
            [ipc_negotiate_tool_attr, typed::<IpcNegotiate>() => "Approve or deny a pending permission request. Args: request_id, actor, approve (bool)."]
            ipc_negotiate: SystemPrimitive => "Approve or deny a pending permission request";
            [ipc_send_tool_attr, typed::<IpcSend>() => "Send a broker message (requires dispatch grant unless from=susi). Args: from, to, topic, payload."]
            ipc_send: SystemPrimitive => "Send a broker message (requires dispatch grant unless from=susi)";
            [ipc_receive_tool_attr, typed::<IpcReceive>() => "Receive broker messages for an identity. Args: recipient, optional limit."]
            ipc_receive: SystemPrimitive => "Receive broker messages for an identity";
            [host_telemetry_tool_attr, no_args() => "Sample host thermal, battery, and load telemetry"]
            host_telemetry: SystemPrimitive => "Sample host thermal, battery, and load telemetry";
            [apply_patch_cycle_tool_attr, typed::<ApplyPatchCycle>() => "Apply a workspace-confined patch then run tests; revert on failure. Args: files[{path,old,new}], optional test_command, auto_apply, description. Optional workspace: the absolute directory the CLI would run in, admitted by the MAC policy (default: the server workspace)."]
            apply_patch_cycle: WorkspaceIo => "Apply a workspace-confined patch then run tests; revert on failure";
            [privacy_status_tool_attr, no_args() => "Report privacy mode, its network scope (local_only = this machine and its local network; host_only = this machine alone, loopback), mandatory sandbox, and cryptographic capability grants"]
            privacy_status: SystemPrimitive => "Report privacy mode and cryptographic capability grants";
            [privacy_consent_egress_tool_attr, typed::<PrivacyConsentEgress>() => "Grant time-limited network.egress (+ cloud.inference under local_only; refused under host_only). Args: optional subject, optional ttl_secs."]
            privacy_consent_egress: SystemPrimitive => "Grant time-limited network egress / cloud inference consent";
            [intent_advertise_tool_attr, typed::<IntentAdvertise>() => "Advertise a provider capability. Args: from, intent, optional payload."]
            intent_advertise: SystemPrimitive => "Advertise a provider capability on the semantic intent bus";
            [intent_need_tool_attr, typed::<IntentNeed>() => "Publish a need and match providers. Args: from, intent, optional min_score, limit."]
            intent_need: SystemPrimitive => "Publish a need and match providers on the semantic intent bus";
            #[cfg(feature = "tools-rich")]
            [ambient_pulse_tool_attr, typed::<WorkspaceView>() => "Scan workspace FS changes into context graph and refresh semantic index. Optional workspace: the absolute directory the CLI would run in, admitted by the MAC policy (default: the server workspace)."]
            ambient_pulse: SystemPrimitive => "Scan workspace, record FS changes into context graph, refresh vector index";
            [tx_begin_tool_attr, typed::<TxBegin>() => "Begin multi-agent transaction. Args: description, files (array of relative paths). Optional workspace: the absolute directory the CLI would run in, admitted by the MAC policy (default: the server workspace)."]
            tx_begin: WorkspaceIo => "Begin a multi-agent transaction snapshotting listed files";
            [tx_commit_tool_attr, typed::<TxId>() => "Commit open transaction. Args: id."]
            tx_commit: WorkspaceIo => "Commit an open multi-agent transaction";
            [tx_abort_tool_attr, typed::<TxId>() => "Abort transaction and restore files. Args: id."]
            tx_abort: WorkspaceIo => "Abort a transaction and restore snapshotted files";

            } [$($between)*] {
            [plan_run_tool_attr, typed::<PlanRun>() => "Solve a goal with the autonomous planning loop in the server workspace, as `susi plan`; returns status, final_answer and agents. Args: {goal, optional max_steps (default 3)}. Optional workspace: the absolute directory the CLI would run in, admitted by the MAC policy (default: the server workspace)."]
            plan_run: WorkspaceIo => "Solve a goal with the autonomous planning loop in the server workspace";
            [intent_ingest_tool_attr, typed::<IntentIngest>() => "Ingest a natural-language intent into the server workspace's evidence ledger, as `susi pulse`. Args: {intent}. Optional workspace: the absolute directory the CLI would run in, admitted by the MAC policy (default: the server workspace)."]
            intent_ingest: WorkspaceIo => "Ingest a natural-language intent into the server workspace evidence ledger";
            [blackboard_show_tool_attr, typed::<WorkspaceView>() => "The last mission blackboard (swarm shared state, credential-redacted) in the server workspace, as `susi blackboard show`. Optional workspace: the absolute directory the CLI would run in, admitted by the MAC policy (default: the server workspace)."]
            blackboard_show: SystemPrimitive => "Show the last mission blackboard (credential-redacted)";
            [blackboard_post_tool_attr, typed::<BlackboardPost>() => "Post a key-value update to the shared blackboard and mirror it onto the Susi Native Board by default"]
            blackboard_post: WorkspaceIo => "Post a key-value update to the shared blackboard and Susi Native Board";
            [board_list_tool_attr, typed::<BoardList>() => "List collaborative items on the Susi Native Board"]
            board_list: SystemPrimitive => "List collaborative items on the Susi Native Board";
            [board_check_tool_attr, typed::<BoardCheck>() => "Check the Susi Native Board for integrity, open stops, and unanswered requests (fail-closed)"]
            board_check: SystemPrimitive => "Check the Susi Native Board for open stops and unanswered requests";
            [board_post_tool_attr, typed::<BoardPost>() => "Post a collaborative coordination item to the Susi Native Board"]
            board_post: WorkspaceIo => "Post an item onto the Susi Native Board";
            [runtime_models_list_tool_attr, no_args() => "Models loaded in the inference cache: the daemon-loaded document `susi os runtime` reports"]
            runtime_models_list: SystemPrimitive => "List models loaded in the inference cache";
            [runtime_models_load_tool_attr, typed::<RuntimeModel>() => "Load a model into the inference cache, as `susi os manage model load`. Args: {model}"]
            runtime_models_load: WorkspaceIo => "Load a model into the inference cache";
            [runtime_models_unload_tool_attr, typed::<RuntimeModel>() => "Evict a model from the inference cache, as `susi os manage model unload`. Args: {model}"]
            runtime_models_unload: WorkspaceIo => "Evict a model from the inference cache";
            [placement_decision_tool_attr, typed::<PlacementDecision>() => "The audited inference-placement decision with this id, plus its audit entry hash (the id a completion returns in X-Susi-Placement-Id). Args: {decision_id}"]
            placement_decision: SystemPrimitive => "Look up an audited inference-placement decision by id";
            [context_graph_stats_tool_attr, no_args() => "Universal Context Graph node and edge counts by kind: the same document as `susi context-graph stats` and 9091 GET /context-graph/stats"]
            context_graph_stats: SystemPrimitive => "Universal Context Graph node and edge counts by kind";
            [context_graph_show_tool_attr, typed::<ContextGraphShow>() => "The most recent Universal Context Graph events for a workspace: the same document as `susi context-graph show`. Args: optional limit (default 20), optional absolute workspace (default: the server workspace)"]
            context_graph_show: SystemPrimitive => "Most recent Universal Context Graph events for a workspace";

            // Zero-UI plan B4 groups 4, 5, 8 and 9: onboarding, assurance, context
            // and IPC, and the OS view, reachable over the standards socket.
            [ipc_pending_tool_attr, typed::<IpcPending>() => "Pending inter-app permission requests, as `susi broker pending`. Args: optional grantor"]
            ipc_pending: SystemPrimitive => "List pending inter-app permission requests";
            [ipc_check_tool_attr, typed::<IpcScope>() => "Whether an identity holds an inter-app permission scope, as `susi broker check`. Args: grantee, resource, action"]
            ipc_check: SystemPrimitive => "Check whether an identity holds an inter-app permission scope";
            [ipc_revoke_tool_attr, typed::<IpcScope>() => "Revoke an inter-app permission scope from an identity, as `susi broker revoke`. Args: grantee, resource, action"]
            ipc_revoke: WorkspaceIo => "Revoke an inter-app permission scope from an identity";
            [ipc_list_tool_attr, typed::<IpcList>() => "Inter-app permission grants held by an identity, as `susi broker list`. Args: grantee"]
            ipc_list: SystemPrimitive => "List the inter-app permission grants an identity holds";
            [privacy_mode_tool_attr, typed::<PrivacyMode>() => "Set the privacy mode (persisted; local_only keeps traffic on this machine and its local network, host_only on this machine alone (loopback; no LAN, discovery, gossip or off-host peers); both pin inference routing to local), as `susi privacy mode`. Args: mode (balanced, local_only, host_only, open)"]
            privacy_mode: WorkspaceIo => "Set the privacy mode (balanced, local_only = this machine and its local network, host_only = this machine alone, open)";
            [privacy_grant_tool_attr, typed::<PrivacyGrant>() => "Grant a cryptographic capability token, as `susi privacy grant`. Args: action, optional subject (default susi), resource (default *), ttl_secs"]
            privacy_grant: WorkspaceIo => "Grant a cryptographic capability token";
            [privacy_revoke_tool_attr, typed::<PrivacyRevoke>() => "Revoke a capability grant, as `susi privacy revoke`. Args: action, optional subject (default susi), resource (default *)"]
            privacy_revoke: WorkspaceIo => "Revoke a capability grant";
            [intent_list_tool_attr, no_args() => "Active intent-bus providers and needs, as `susi intent list`"]
            intent_list: SystemPrimitive => "List active intent-bus providers and needs";
            [ambient_start_tool_attr, typed::<WorkspaceView>() => "Start the background ambient indexer for the server workspace, as `susi ambient start`; reports started, already_running or refused_internal_path. Optional workspace: the absolute directory the CLI would run in, admitted by the MAC policy (default: the server workspace)."]
            ambient_start: WorkspaceIo => "Start the background ambient indexer for the server workspace";
            [tx_list_tool_attr, typed::<WorkspaceView>() => "Open multi-agent transactions in the server workspace, as `susi tx list`. Optional workspace: the absolute directory the CLI would run in, admitted by the MAC policy (default: the server workspace)."]
            tx_list: SystemPrimitive => "List open multi-agent transactions in the server workspace";
            [context_graph_sample_os_tool_attr, typed::<ContextSampleOs>() => "Sample OS context (process names; active window on Linux) into the Universal Context Graph, as `susi context-graph sample-os`. Args: optional user, optional absolute workspace to tag the records with (default: the server workspace)"]
            context_graph_sample_os: WorkspaceIo => "Sample OS context into the Universal Context Graph";
            [audit_verify_tool_attr, typed::<AuditVerify>() => "Verify every record of the server workspace's tamper-evident audit chain, as `susi audit verify`. Args: optional since (Unix seconds; counts entries at or after it, the full chain is checked). Optional workspace: the absolute directory the CLI would run in, admitted by the MAC policy (default: the server workspace)."]
            audit_verify: SystemPrimitive => "Verify the server workspace's tamper-evident audit chain";
            [audit_query_tool_attr, typed::<AuditQuery>() => "Redacted action history from the server workspace's audit chain, as `susi audit query --json`. Args: optional actor, mission, kind, since, until. Optional workspace: the absolute directory the CLI would run in, admitted by the MAC policy (default: the server workspace)."]
            audit_query: SystemPrimitive => "Query redacted action history from the audit chain";
            [compliance_audit_tool_attr, typed::<WorkspaceView>() => "The deterministic compliance audit of the server workspace, as `susi audit` and `susi admin audit`. Optional workspace: the absolute directory the CLI would run in, admitted by the MAC policy (default: the server workspace)."]
            compliance_audit: SystemPrimitive => "Run the deterministic compliance audit";
            [audit_evidence_export_tool_attr, typed::<AuditEvidenceExport>() => "Wrap redacted audit segments in the independently verifiable export format, as `susi admin audit-evidence export`. Args: input, output (absolute paths)"]
            audit_evidence_export: WorkspaceIo => "Export redacted audit segments in the verifiable format";
            [substrate_report_tool_attr, typed::<WorkspaceView>() => "The compiled-constants substrate report (version, model, axioms, topology), as `susi substrate-report`. Optional workspace: the absolute directory the CLI would run in, admitted by the MAC policy (default: the server workspace)."]
            substrate_report: SystemPrimitive => "Compiled-constants substrate report";
            [substrate_status_tool_attr, typed::<WorkspaceView>() => "Pluggable / Sandbox / Governance / Host-contract / Reflexes status: the same document as `susi substrate status`. Optional workspace: the absolute directory the CLI would run in, admitted by the MAC policy (default: the server workspace)."]
            substrate_status: SystemPrimitive => "Pluggable / Sandbox / Governance / Host-contract / Reflexes status";
            [crown_verify_tool_attr, typed::<WorkspaceView>() => "Verify every Tier S USP: the same document as `susi crown verify|status` (all_critical_hold says whether verify would pass). Optional workspace: the absolute directory the CLI would run in, admitted by the MAC policy (default: the server workspace)."]
            crown_verify: SystemPrimitive => "Verify every Tier S USP";
            [clean_tool_attr, no_args() => "Remove SUSI-owned residue (never host or other apps' caches) and report bytes freed, as `susi os clean` / `susi os-clean`"]
            clean: WorkspaceIo => "Remove SUSI-owned residue and report bytes freed";
            [setup_plan_tool_attr, no_args() => "Detect hardware, engines, key presence (names only) and agents, and return the zero-config setup plan: the same document as `susi setup plan --json`"]
            setup_plan: SystemPrimitive => "Detect the host and return the zero-config setup plan";
            [setup_apply_tool_attr, typed::<SetupApply>() => "Apply the zero-config setup plan (existing settings preserved): the same document as `susi setup apply --json`. Args: optional cloud (grant cloud API key consent)"]
            setup_apply: WorkspaceIo => "Apply the zero-config setup plan (existing settings preserved)";
            [ecosystem_scan_tool_attr, typed::<EcosystemScan>() => "Scan hardware and installed local AI engines, runtimes and model stores now, as `susi ecosystem scan`; with cached=true return the last persisted scan, as `susi ecosystem status`. Args: optional cached"]
            ecosystem_scan: SystemPrimitive => "Scan (or return the last scan of) the local AI ecosystem";
            [ecosystem_start_tool_attr, typed::<EcosystemStart>() => "Start an installed local AI engine that needs no further input (e.g. ollama, local-ai), as `susi ecosystem start`. Args: id"]
            ecosystem_start: WorkspaceIo => "Start an installed local AI engine";
            [ecosystem_kb_tool_attr, typed::<EcosystemKb>() => "Query the cloud-AI-ecosystem knowledge base, as `susi ecosystem kb list|show|search|check`. Args: optional action (default list), id, query, kind, capability, vendor, dir (absolute)"]
            ecosystem_kb: SystemPrimitive => "Query the cloud-AI-ecosystem knowledge base";
            [ecosystem_explain_tool_attr, typed::<EcosystemExplain>() => "Explain a component, standard or protocol end to end (versions, implementers, capabilities, susi profile), as `susi ecosystem explain`. Args: id, optional dir (absolute)"]
            ecosystem_explain: SystemPrimitive => "Explain a knowledge-base component, standard or protocol";
            [os_provision_tool_attr, typed::<OsProvision>() => "Explicit cloud provisioning, as `susi os provision probe|list|apply --json`. Args: optional action (default probe), kind (list/apply), manifest (apply; absolute path)"]
            os_provision: WorkspaceIo => "Explicit cloud provisioning: probe, list or apply";
            [os_capabilities_tool_attr, typed::<OsCapabilities>() => "The typed capability bus, as `susi os capabilities discover|invoke|delegate --json`. Args: optional action (default discover); invoke: id, permission, optional subject (default operator), args, timeout_ms (default 30000), budget_micros (default unlimited), delegation (a host-signed susi-delegation/v1 envelope: the call nests under it and cannot exceed it); delegate: the same fields, returns a host-signed envelope for subject to exercise id under delegation (or a fresh root). Optional workspace: the absolute directory the CLI would run in, admitted by the MAC policy (default: the server workspace)."]
            os_capabilities: WorkspaceIo => "Discover or invoke typed capability-bus capabilities";
            [services_start_tool_attr] services_start: WorkspaceIo => "Clear an operator stop on a supervised leaf service";
            [services_stop_tool_attr] services_stop: WorkspaceIo => "Stop a supervised leaf service and hold it down";
            [services_restart_tool_attr] services_restart: WorkspaceIo => "Restart a supervised leaf service";
            [commits_audit_tool_attr, no_args() => "Whole-ledger consistency check of the replicated commit log (signatures, gaps, equivocation, non-leader commits): the same document as `susi commits audit --json`"]
            commits_audit: SystemPrimitive => "Whole-ledger consistency check of the commit log";
            [commits_replay_tool_attr, no_args() => "The consensus view the commit ledger replays to (term, leader, roster, high-water marks): the same document as `susi commits replay --json`"]
            commits_replay: SystemPrimitive => "Consensus view the commit ledger replays to";
            [commits_show_tool_attr, typed::<CommitsShow>() => "One commit record (live or archived) by unique epoch prefix, with its signature verdicts: the same document as `susi commits show --json`. Args: epoch"]
            commits_show: SystemPrimitive => "Show one commit record by epoch prefix";
            [commits_sync_tool_attr, no_args() => "Bidirectional commit-ledger anti-entropy with every verified remote peer, as `susi commits sync`; returns per-peer and total counts"]
            commits_sync: WorkspaceIo => "Commit-ledger anti-entropy with verified peers";
            [commits_compact_tool_attr, no_args() => "Fold the commit ledger into a snapshot and archive pre-snapshot records, as `susi commits compact`"]
            commits_compact: WorkspaceIo => "Compact the commit ledger into a snapshot plus archive";
            [peers_probe_tool_attr, typed::<PeersProbe>() => "Verify a host cryptographically (signed ping/pong, then its MCP endpoint) without admitting it, as `susi peers probe`. Args: host (optionally host:port), optional port"]
            peers_probe: SystemPrimitive => "Cryptographically verify a peer host without admitting it";
            [peers_list_tool_attr, no_args() => "This node's identity, eviction state, elected leader, and every rostered and banned cluster member: the same document as `susi peers list --json`"]
            peers_list: SystemPrimitive => "List the verified cluster roster, leader and banned members";
            [peers_status_tool_attr, no_args() => "Every rostered member's cluster_status (unreachable members as error rows) plus this node's own, sorted by node: the same rows as `susi peers status --json`"]
            peers_status: SystemPrimitive => "Every rostered member's cluster status side by side";

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
            [benchmark_run_tool_attr, no_args() => "Measure real local inference latency and tokens/sec (and a configured cloud endpoint), as `susi benchmark`"]
            benchmark_run: SystemPrimitive => "Measure local (and configured cloud) inference latency and throughput";
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
            [harness_run_tool_attr, typed::<HarnessRun>() => "Start running a plan under the budget gate on a daemon thread (prepare in parallel, land in order); returns the mission and its state file, as `susi harness run`. Needs process.exec: the run drives this daemon's own susi binary (the CLI's --susi stays local). Args: repo, plan, gateway, and the CLI's other optional flags"]
            harness_run: WorkspaceIo => "Start running a harness plan under the budget gate";
            [harness_runs_tool_attr] harness_runs: SystemPrimitive => "Harness missions this daemon started and their outcomes";

            // Zero-UI plan B4 groups 2, 4, 9, 10 and 13: host views, maintenance
            // and daemon lifecycle.
            [operator_status_tool_attr] operator_status: SystemPrimitive => "Measured executable identity and substrate counts (as `susi status --json`)";
            [operator_review_tool_attr] operator_review: SystemPrimitive => "Daemon liveness and reclaimable OS cache (as `susi review`)";
            [os_ecosystem_tool_attr] os_ecosystem: SystemPrimitive => "Local AI ecosystem: hardware, runtimes, models, adapters, MCP servers, AI processes";
            [os_status_tool_attr] os_status: SystemPrimitive => "Substrate status: consensus, ledger, services, peers, endpoints, storage";
            [os_runtime_tool_attr] os_runtime: SystemPrimitive => "Live AI processes by owner, managed-model residency and GPU memory";
            [model_uninstall_tool_attr] model_uninstall: WorkspaceIo => "Remove one SUSI-managed model file (refused while loaded or selected)";
            [peers_add_tool_attr, typed::<PeersAdd>() => "Join a peer by signed handshake and commit its admission to the replicated ledger, as `susi peers add`. Args: host, optional port"]
            peers_add: WorkspaceIo => "Join a peer by signed handshake and commit its admission";
            [peers_remove_tool_attr, typed::<PeerName>() => "Evict a verified peer (revoke trust and ban re-verification), committed cluster-wide, as `susi peers remove`. Args: peer"]
            peers_remove: WorkspaceIo => "Evict and ban a verified peer, committed cluster-wide";
            [peers_unban_tool_attr, typed::<PeerName>() => "Lift a ban so an evicted peer can re-verify, committed cluster-wide, as `susi peers unban`. Args: peer"]
            peers_unban: WorkspaceIo => "Lift a peer ban, committed cluster-wide";
            [peers_rekey_tool_attr, typed::<PeersRekey>() => "Rotate the cluster key across every verified member, two-phase (no key material is returned), as `susi peers rekey`. Args: optional force"]
            peers_rekey: WorkspaceIo => "Rotate the cluster key across every verified member";
            [daemon_shutdown_tool_attr] daemon_shutdown: WorkspaceIo => "Stop this daemon instance after replying";
            [extensions_create_tool_attr, typed::<ExtensionPackId>() => "Create an empty extension pack shell under the host extensions root (not activated), as `susi extensions create`. Args: id"]
            extensions_create: WorkspaceIo => "Create an empty extension pack shell (not activated)";
            [auto_status_tool_attr] auto_status: SystemPrimitive => "Zero-config substrate readiness snapshot";
            [auto_prime_tool_attr] auto_prime: WorkspaceIo => "Re-run zero-config auto-prime (never enables MCP)";
            [deep_scan_tool_attr, no_args() => "Scan the home directory for model files and register them, as `susi deep-scan`"]
            deep_scan: WorkspaceIo => "Scan the home directory for model files and register them";
            [config_reload_tool_attr] config_reload: WorkspaceIo => "Reload config.json and report what is in effect";
            [provider_contract_tool_attr] provider_contract: SystemPrimitive => "Provider contract fixture checks (credentialed half opt-in)";
            [intent_ladder_tool_attr, typed::<IntentLadderArgs>() => "The intent ladder: last scorecard, a full or start-up-slice run, one intent's verification, or the failing-rung outbox, as `susi admin intent-ladder`. Mode adopt_gap is refused: adopting a gap as a repair task moved to Susi Commercial Apps (`susi-commercial admin intent-ladder --adopt-gap <id>`). Args: optional mode, id, workspace"]
            intent_ladder: WorkspaceIo => "Intent ladder scorecard, runs, verification, gap outbox and adoption";
            }
        }
    };
}
