use super::*;
use serde_json::json;

#[test]
fn caller_file_paths_are_absolute_resolved_and_typed() {
    let keys = [("out", true), ("file", false)];
    assert_eq!(policy_paths(&json!({}), &keys).unwrap(), json!({}));
    let refused = policy_paths(&json!({"out": "rel.json"}), &keys).unwrap_err();
    assert!(refused.to_string().contains("absolute"), "{refused}");
    assert!(policy_paths(&json!({"file": 3}), &keys).is_err());
    let dir = std::env::temp_dir().join(format!("susi-forward-path-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join("new").join("out.json");
    let resolved = policy_paths(&json!({"out": out.display().to_string()}), &keys).unwrap();
    let text = resolved["out"].as_str().unwrap();
    assert!(Path::new(text).ends_with(Path::new("new").join("out.json")));
    assert_eq!(
        merged(json!({"a": 1, "b": 1}), json!({"b": 2})),
        json!({"a": 1, "b": 2})
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn payload_copies_set_arguments_and_refuses_wrong_types() {
    let keys = [
        ("name", Kind::Str),
        ("live", Kind::Bool),
        ("limit", Kind::U64),
        ("doc", Kind::Object),
        ("args", Kind::StrArray),
    ];
    let sent = payload(
        "act",
        &json!({"name": "n", "live": true, "limit": 3, "doc": {}, "args": ["a"],
            "ignored": 1, "absent": null}),
        &keys,
        json!({"workspace": "/w"}),
    )
    .unwrap();
    assert_eq!(
        sent,
        json!({"action": "act", "name": "n", "live": true, "limit": 3, "doc": {},
            "args": ["a"], "workspace": "/w"})
    );
    for bad in [
        json!({"name": 1}),
        json!({"live": "yes"}),
        json!({"limit": -1}),
        json!({"doc": []}),
        json!({"args": [1]}),
    ] {
        assert!(payload("act", &bad, &keys, json!({})).is_err(), "{bad}");
    }
}

#[test]
fn workspace_defaults_to_the_server_and_admits_granted_directories() {
    let server = std::env::temp_dir();
    assert_eq!(
        policy_workspace(&json!({}), "workspace", &server, false).unwrap(),
        server
    );
    assert!(policy_workspace(&json!({"workspace": 3}), "workspace", &server, false).is_err());
    assert!(
        policy_workspace(&json!({"workspace": "rel"}), "workspace", &server, false)
            .unwrap_err()
            .to_string()
            .contains("absolute")
    );
    let dir = std::env::temp_dir().join(format!("susi-forward-ws-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let resolved = policy_workspace(
        &json!({"workspace": dir.display().to_string()}),
        "workspace",
        &server,
        true,
    )
    .unwrap();
    assert_eq!(
        resolved.canonicalize().unwrap(),
        dir.canonicalize().unwrap()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn forward_and_send_format_pretty_json_and_propagate_call_errors() {
    fn ok_call(v: Value) -> Result<Value, String> {
        Ok(json!({ "received": v }))
    }
    fn err_call(_v: Value) -> Result<Value, String> {
        Err("plane rejected call".into())
    }

    let pretty = forward(
        ok_call,
        "ping",
        &json!({"name": "alpha"}),
        &[("name", Kind::Str)],
        json!({"extra": true}),
    )
    .unwrap();
    let decoded: Value = serde_json::from_str(&pretty).unwrap();
    assert_eq!(decoded["received"]["action"], "ping");
    assert_eq!(decoded["received"]["name"], "alpha");
    assert_eq!(decoded["received"]["extra"], true);

    let err = send(err_call, json!({"action": "ping"})).unwrap_err();
    assert!(err.to_string().contains("plane rejected call"));
}

#[test]
fn kind_describe_and_admits_cover_all_five_json_argument_shapes() {
    assert_eq!(Kind::Str.describe(), "a string");
    assert_eq!(Kind::Bool.describe(), "a boolean");
    assert_eq!(Kind::U64.describe(), "a non-negative integer");
    assert_eq!(Kind::Object.describe(), "an object");
    assert_eq!(Kind::StrArray.describe(), "an array of strings");

    assert!(Kind::StrArray.admits(&json!([])));
    assert!(Kind::StrArray.admits(&json!(["a", "b"])));
    assert!(!Kind::StrArray.admits(&json!(["a", 2])));
}

#[test]
fn forwarded_tools_match_pristine_metadata() {
    use crate::mcp::model::Tool;
    use crate::tools::{args, CoreTools};
    use std::borrow::Cow;

    // Captured from pristine 6a262d346 before forwarding consolidation.
    // Names/descriptions/schema expressions are independent of the new macro
    // declarations; compare every emitted Tool field, including absent metadata.
    macro_rules! check {
        ($attribute:ident, $name:literal, $description:literal, $schema:expr) => {
            assert_eq!(
                CoreTools::$attribute(),
                Tool::new_with_raw($name, Some(Cow::Borrowed($description)), $schema),
                "{}",
                $name,
            );
        };
    }
    check!(adapter_doctor_tool_attr, "adapter_doctor",
        "Readiness rows for agent, framework, coding-model or MCP adapters (nothing installed, no paid call), as `susi <agents|frameworks|models|mcp> doctor`. Args: kind, optional id",
        args::typed::<args::AdapterDoctor>());
    check!(adapter_setup_tool_attr, "adapter_setup",
        "Setup instructions and the effective adapter, as `susi <agents|frameworks|models|mcp> setup`. Args: kind, id",
        args::typed::<args::AdapterId>());
    check!(adapter_configure_tool_attr, "adapter_configure",
        "Store a host adapter override (agent and framework adapters pass the verified upgrade), as `susi <agents|frameworks|models|mcp> configure`. Args: kind, id, and adapter (object) or file (absolute path)",
        args::typed::<args::AdapterConfigure>());
    check!(adapter_reset_tool_attr, "adapter_reset",
        "Drop a host adapter override and return the bundled adapter, as `susi <agents|frameworks|models|mcp> reset`. Args: kind, id",
        args::typed::<args::AdapterId>());
    check!(agents_retry_tool_attr, "agents_retry",
        "Run a finished external agent task again with its original prompt (refused while it is active or unknown), as `susi agents retry`. Args: task_id, optional workspace",
        args::typed::<args::TaskRetry>());
    check!(frameworks_retry_tool_attr, "frameworks_retry",
        "Run a finished agent-framework task again with its original prompt, as `susi frameworks retry`. Args: task_id, optional workspace",
        args::typed::<args::TaskRetry>());
    check!(mcp_add_tool_attr, "mcp_add",
        "Admit an MCP server (stdio command or streamable HTTP URL) into the registry and hot-plug it, as `susi mcp-add`. Args: name, command_or_url, optional args",
        args::typed::<args::McpAdd>());
    check!(agents_status_tool_attr, "agents_status",
        "Inspect an external task; refresh=true recovers cloud status after worker loss. Optional workspace: the absolute directory the CLI would run in, admitted by the MAC policy (default: the server workspace).",
        args::typed::<args::ExternalTaskStatus>());
    check!(agents_cancel_tool_attr, "agents_cancel",
        "Request cancellation of a managed external agent task. Optional workspace: the absolute directory the CLI would run in, admitted by the MAC policy (default: the server workspace).",
        args::typed::<args::ExternalTaskId>());
    check!(agents_logs_tool_attr, "agents_logs",
        "Read the last 64 KiB of external agent output; stderr=true selects errors. Optional workspace: the absolute directory the CLI would run in, admitted by the MAC policy (default: the server workspace).",
        args::typed::<args::ExternalTaskLogs>());
    check!(agents_send_tool_attr, "agents_send",
        "Send a follow-up message to an existing cloud agent task. Optional workspace: the absolute directory the CLI would run in, admitted by the MAC policy (default: the server workspace).",
        args::typed::<args::ExternalTaskSend>());
    check!(frameworks_status_tool_attr, "frameworks_status",
        "Inspect an agent-framework task. Optional workspace: the absolute directory the CLI would run in, admitted by the MAC policy (default: the server workspace).",
        args::typed::<args::ExternalTaskStatus>());
    check!(frameworks_cancel_tool_attr, "frameworks_cancel",
        "Request cancellation of a managed agent-framework task. Optional workspace: the absolute directory the CLI would run in, admitted by the MAC policy (default: the server workspace).",
        args::typed::<args::ExternalTaskId>());
    check!(frameworks_logs_tool_attr, "frameworks_logs",
        "Read the last 64 KiB of agent-framework output; stderr=true selects errors. Optional workspace: the absolute directory the CLI would run in, admitted by the MAC policy (default: the server workspace).",
        args::typed::<args::ExternalTaskLogs>());
    check!(
        tasks_pause_tool_attr,
        "tasks_pause",
        "Pause a running task by task_id",
        args::typed::<args::SwarmTaskId>()
    );
    check!(
        tasks_resume_tool_attr,
        "tasks_resume",
        "Resume a paused task by task_id",
        args::typed::<args::SwarmTaskId>()
    );
    check!(
        tasks_kill_tool_attr,
        "tasks_kill",
        "Kill a running or stalled task by task_id",
        args::typed::<args::SwarmTaskId>()
    );
    check!(harness_status_tool_attr, "harness_status",
        "The harness API snapshot (spend, tasks, workers, warnings) for a ledger, with a run's state and plan when given, as `susi harness status`. Args: ledger, optional events, state, plan (absolute paths)",
        args::typed::<args::HarnessStatus>());
    check!(harness_budget_tool_attr, "harness_budget",
        "The one-line harness budget report and any top-up alerts, as `susi harness budget`. Args: optional ledger, repo (absolute)",
        args::typed::<args::HarnessBudget>());
    check!(harness_models_tool_attr, "harness_models",
        "The harness provider registry (registry, no network), its live drift check (verify), the models keyed providers add (discover) or the keyed models by cost (available), as `susi harness models`. Args: optional mode, provider, repo",
        args::typed::<args::HarnessModels>());
    check!(harness_plan_tool_attr, "harness_plan",
        "Plan a batch of dependency-ready queue tasks and write the plan, as `susi harness plan`. Args: repo, out (absolute), optional mission, limit, workers, roadmap, gateway",
        args::typed::<args::HarnessPlan>());
    check!(harness_author_tool_attr, "harness_author",
        "Author a plan for a roadmap vector through the metered gateway, validate it and write it, as `susi harness author`. Args: repo, gateway, vector, out, optional model, roadmap, max_tasks, workers",
        args::typed::<args::HarnessAuthor>());
    check!(harness_runs_tool_attr, "harness_runs",
        "The harness missions this daemon started: running or finished, with notes, the report and the budget line",
        args::no_args());
    check!(operator_status_tool_attr, "operator_status",
        "The executable's measured identity (sha256), daemon, local model, missions in flight, resumable missions, approvals and the scheduled queue, as `susi status --json`; with detail true, the whole `susi status` view (missions with prompts, resumable DAG nodes, rollback snapshots, brain leaders and unfit providers, routing, spend, approvals). Args: optional workspace, detail",
        args::typed::<args::OperatorStatus>());
    check!(
        operator_review_tool_attr,
        "operator_review",
        "Daemon liveness and reclaimable OS cache, as `susi review`. Args: optional workspace",
        args::typed::<args::WorkspaceView>()
    );
    check!(os_ecosystem_tool_attr, "os_ecosystem",
        "The local AI ecosystem: hardware, runtimes, models, agent and framework readiness, MCP servers and running AI processes, as `susi os ecosystem --json`. Args: optional workspace",
        args::typed::<args::WorkspaceView>());
    check!(os_status_tool_attr, "os_status",
        "Substrate status: consensus, ledger, services, peers, endpoints, storage and warnings, as `susi os status --json`",
        args::no_args());
    check!(os_runtime_tool_attr, "os_runtime",
        "Live AI processes by owner (SUSI or external), managed models with daemon residency, and per-process GPU memory, as `susi os runtime --json`",
        args::no_args());
    check!(model_uninstall_tool_attr, "model_uninstall",
        "Remove one SUSI-managed .gguf model (refused while loaded or selected), as `susi os manage model uninstall`. Args: name",
        args::typed::<args::ModelUninstall>());
    check!(daemon_shutdown_tool_attr, "daemon_shutdown",
        "Stop this daemon instance after replying (the call `susi stop` makes); starting it again stays with the CLI",
        args::no_args());
    check!(auto_status_tool_attr, "auto_status",
        "Zero-config substrate readiness: packs, leading MCP, preferred coding model, agent and framework readiness, last prime, as `susi auto status`. Args: optional workspace",
        args::typed::<args::WorkspaceView>());
    check!(auto_prime_tool_attr, "auto_prime",
        "Re-run auto-prime now (models and installed peers; never enables MCP), as `susi auto prime`",
        args::no_args());
    check!(config_reload_tool_attr, "config_reload",
        "Reload config.json and report the engine, model, model-ladder steps and MCP bootstrap servers in effect, as `susi admin reload`",
        args::no_args());
    check!(provider_contract_tool_attr, "provider_contract",
        "Provider contract fixture checks, plus the read-only credentialed claims when SUSI_PROVIDER_PROBE=1, as `susi admin provider-contract`",
        args::no_args());
    check!(provider_list_tool_attr, "provider_list",
        "Curated models (routes) of a provider control plane and their readiness, as `susi <openweight|frontier|openrouter> list`. Args: vendor",
        args::typed::<args::ProviderVendor>());
    check!(provider_doctor_tool_attr, "provider_doctor",
        "Readiness documents of a provider control plane (no paid call), as `susi <vendor> doctor`. Args: vendor, optional model, live (openrouter: list live model ids)",
        args::typed::<args::ProviderDoctor>());
    check!(provider_setup_tool_attr, "provider_setup",
        "Setup instructions and effective wiring, as `susi <vendor> setup`. Args: vendor, model (openweight and frontier)",
        args::typed::<args::ProviderSetup>());
    check!(provider_configure_tool_attr, "provider_configure",
        "Store a host override for a model, as `susi <openweight|frontier> configure`. Args: vendor, model, and override (object) or file (absolute path)",
        args::typed::<args::ProviderConfigure>());
    check!(provider_reset_tool_attr, "provider_reset",
        "Drop a model's host override and return the bundled entry, as `susi <openweight|frontier> reset`. Args: vendor, model",
        args::typed::<args::ProviderModel>());
    check!(provider_pull_tool_attr, "provider_pull",
        "Pull an open-weight model's weights through Ollama, as `susi openweight pull`. Args: model",
        args::typed::<args::ProviderPull>());
    check!(provider_prefer_tool_attr, "provider_prefer",
        "Prefer (or clear) a provider model for routing, as `susi <vendor> prefer`. Args: vendor, model, clear",
        args::typed::<args::ProviderPrefer>());
    check!(provider_probe_tool_attr, "provider_probe",
        "Short completion through a model's endpoint (cloud probes are paid), as `susi <vendor> probe` and `susi models probe` (vendor coding). Args: vendor, model, optional prompt",
        args::typed::<args::ProviderProbe>());
    check!(provider_models_tool_attr, "provider_models",
        "OpenRouter's curated routes, or the live model ids its key reaches, as `susi openrouter models`. Args: optional live, limit",
        args::typed::<args::ProviderModels>());
    check!(keys_status_tool_attr, "keys_status",
        "Which cloud vendor keys are set (presence only, never values), the routing preference and the key file, as `susi keys list`",
        args::no_args());
    check!(keys_prefer_tool_attr, "keys_prefer",
        "Pin, clear or report the preferred cloud vendor, as `susi keys prefer`. Args: optional vendor, clear",
        args::typed::<args::KeysPrefer>());
    check!(keys_check_tool_attr, "keys_check",
        "Each key's state against its vendor's model list (key values never returned), as `susi keys check`. Args: optional vendor",
        args::typed::<args::KeysCheck>());
    check!(
        keys_models_tool_attr,
        "keys_models",
        "The models a vendor's valid key reaches, as `susi keys models`. Args: vendor",
        args::typed::<args::KeysModels>()
    );
    check!(brain_status_tool_attr, "brain_status",
        "Evidence-ranked providers per task class, as `susi brain status --json`. Args: optional top, class",
        args::typed::<args::BrainStatus>());
    check!(brain_budget_tool_attr, "brain_budget",
        "Set or report the budget dial and shared spend policy, as `susi brain budget`. Args: optional level, free_only, allow_paid (explicit spend authorization), max_spend_microusd, max_tokens, max_requests, max_parallel, new_window",
        args::typed::<args::BrainBudget>());
    check!(
        brain_classify_tool_attr,
        "brain_classify",
        "The task class a prompt routes as, as `susi brain classify`. Args: prompt",
        args::typed::<args::BrainPrompt>()
    );
    check!(
        brain_explain_tool_attr,
        "brain_explain",
        "The brain's routing decision for a prompt, as `susi brain explain`. Args: prompt",
        args::typed::<args::BrainPrompt>()
    );
    check!(brain_export_tool_attr, "brain_export",
        "The brain evidence bundle (or write it to an absolute path), as `susi brain export`. Args: optional out",
        args::typed::<args::BrainExport>());
    check!(
        brain_import_tool_attr,
        "brain_import",
        "Merge a brain evidence bundle from an absolute path, as `susi brain import`. Args: file",
        args::typed::<args::BrainImport>()
    );
    check!(brain_matrix_tool_attr, "brain_matrix",
        "The measured capability matrix (success, latency and cost per verified outcome), as `susi brain matrix`",
        args::no_args());
    check!(
        brain_cache_tool_attr,
        "brain_cache",
        "The measured prompt-cache hit rate and estimated saving, as `susi brain cache`",
        args::no_args()
    );
    check!(
        brain_reset_tool_attr,
        "brain_reset",
        "Forget every recorded brain outcome, as `susi brain reset`",
        args::no_args()
    );
    check!(brain_bench_tool_attr, "brain_bench",
        "Run the fixed prompt set against the fake engine and record JSONL evidence, as `susi brain bench`. Args: optional out (absolute)",
        args::typed::<args::BrainBench>());
    check!(brain_probe_tool_attr, "brain_probe",
        "Probe a registered provider with the scout benchmark and record the verified outcomes, as `susi brain probe`. Args: provider, optional no_record",
        args::typed::<args::BrainProbe>());
    check!(verify_download_agent_tool_attr, "verify_download_agent",
        "Network status and the hardware-selected model ladder's provisioning steps (all_verified is the verdict), as `susi verify-download-agent`; verification only, it never starts a download",
        args::no_args());
}

/// An invalid scope or file must stop the call before unrelated payload errors.
#[test]
fn operator_admission_preserves_failure_order() {
    let _home = crate::test_isolation::enter_isolated_home();
    use crate::tools::CoreTools;
    let server = std::env::temp_dir();
    let message = |result: EaiResult<String>| result.unwrap_err().to_string();
    assert!(message(CoreTools::harness_plan(
        &json!({"out":false, "limit":"invalid"}),
        &server
    ))
    .contains("repo is required"));
    assert!(message(CoreTools::provider_configure(
        &json!({"file":false, "vendor":false, "model":false}),
        &server
    ))
    .contains("file must be a string"));
    assert!(message(CoreTools::harness_status(
        &json!({"events":false, "ledger":false}),
        &server
    ))
    .contains("ledger must be a string"));
    assert!(message(CoreTools::harness_run(
        &json!({"susi":false, "repo":false, "plan":false}),
        &server
    ))
    .contains("susi cannot be chosen over the socket"));
}
