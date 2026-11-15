//! CoreTools handlers for substrate diagnostics, governance, patch cycles, transactions, and reasoning.

use std::fs;
use std::path::Path;

use super::{remote_result_is_error, CoreTools};
use crate::susi_core::context_graph::ContextGraph;
use crate::susi_core::plane_bus::gemi::{sample_telemetry, HardwareProfiler};
use crate::susi_core::plane_bus::{gawd, gawd_hooks};
use crate::susi_error::{EaiError, EaiResult};
use crate::tool_registry::GmcpClient;
use crate::tools::args;

impl CoreTools {
    crate::tool_def!(status_tool_attr {
        name: "status",
        description: "SUSI Substrate status report",
        input_schema: args::no_args(),
    });
    pub fn status(_arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let hardware = HardwareProfiler::get_profile();
        let mut out = format!("SUSI Engine Version: {}\n", env!("CARGO_PKG_VERSION"));
        out.push_str(&format!(
            "System Environment: {} CPUs | RAM: {}GB | {}\n",
            hardware.cpus, hardware.ram_gb, hardware.gpu_info
        ));

        // Report Background Provisioning Progress
        let progress_file = susi_paths::SusiDirs::data_dir().join("download_progress.json");
        if progress_file.exists() {
            if let Ok(content) = fs::read_to_string(&progress_file) {
                if let Ok(progress) = serde_json::from_str::<serde_json::Value>(&content) {
                    if progress.get("status").and_then(|v| v.as_str()) == Some("IN_PROGRESS") {
                        out.push_str("\n[SUBSTRATE PROVISIONING ACTIVE]\n");
                        if let Some(name) = progress.get("model_name").and_then(|v| v.as_str()) {
                            out.push_str(&format!("- Target: {name}\n"));
                        }
                        let pct = progress
                            .get("percentage")
                            .and_then(|v| v.as_f64())
                            .unwrap_or(0.0);
                        let downloaded = progress
                            .get("bytes_downloaded")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0) as f32
                            / 1e9;
                        let expected = progress
                            .get("expected_bytes")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0) as f32
                            / 1e9;
                        out.push_str(&format!(
                            "- Progress: {pct:.2}% ({downloaded:.2}GB / {expected:.2}GB)\n"
                        ));
                    }
                }
            }
        }

        out.push_str("\nStatus: Operational.\n");
        Ok(out)
    }

    crate::tool_def!(ledger_verify_tool_attr {
        name: "ledger_verify",
        description: "Verify the pinned GMCP normative requirements ledger (VC-203-001): vendored corpus hashes, verbatim clauses, complete row classification; drift=true checks the pinned upstreams instead, as `susi mcp ledger [--drift]`. Args: optional drift, refetched, workspace. Optional workspace: the absolute directory the CLI would run in, admitted by the MAC policy (default: the server workspace).",
        input_schema: args::typed::<args::LedgerVerify>(),
    });
    pub fn ledger_verify(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        // The CLI runs this in its working directory; the socket
        // takes an optional, MAC-policy-admitted `workspace`.
        let workspace =
            &crate::tools::forward::policy_workspace(arg, "workspace", workspace, false)?;
        let report = if arg.get("drift").and_then(|v| v.as_bool()) == Some(true) {
            let refetched = match arg.get("refetched").and_then(|v| v.as_str()) {
                Some(dir) if Path::new(dir).is_absolute() => Some(Path::new(dir)),
                Some(_) => {
                    return Err(EaiError::config(
                        "refetched must be an absolute path (the socket has no caller working directory)",
                    ));
                }
                None => None,
            };
            crate::requirements_ledger::drift_report_json(workspace, refetched)?
        } else {
            crate::requirements_ledger::verify_report_json(workspace)?
        };
        serde_json::to_string_pretty(&report)
            .map_err(|e| EaiError::process(format!("ledger report encode: {e}")))
    }

    crate::tool_def!(sovereign_dashboard_tool_attr {
        name: "sovereign_dashboard",
        description: "Report on autonomous invisible work performed by the substrate",
        input_schema: args::no_args(),
    });
    pub fn sovereign_dashboard(_arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let log_content =
            crate::susi_sandbox::manager::SusiAuditLogger::read_audit_log(workspace, 100);
        let mut report = "# SUSI Sovereign Dashboard - Invisible Work Audit\n\n".to_string();

        // Count what the audit log actually records, by event type. The
        // dashboard used to count fixed categories (self-heals, security
        // hardening, bloat rejections, memory distillations) that no code
        // ever logs, so it reported invented zeros — or false hits when an
        // unrelated entry's text merely contained one of those words.
        let mut by_type: std::collections::BTreeMap<String, usize> =
            std::collections::BTreeMap::new();
        for line in log_content.lines() {
            if let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) {
                if let Some(kind) = entry["type"].as_str() {
                    *by_type.entry(kind.to_string()).or_default() += 1;
                }
            }
        }
        let mut counts: Vec<(String, usize)> = by_type.into_iter().collect();
        counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        if counts.is_empty() {
            report.push_str("- No audited substrate activity in the last 100 entries.\n\n");
        } else {
            report.push_str("### Audited activity by event type (last 100 entries):\n");
            for (kind, count) in &counts {
                report.push_str(&format!("- **{kind}**: {count}\n"));
            }
            report.push('\n');
        }

        report.push_str("### Recent Autonomous Activity Trace:\n");
        for line in log_content.lines().rev().take(10) {
            if let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) {
                let ts = entry["ts"].as_u64().unwrap_or(0);
                let ev_type = entry["type"].as_str().unwrap_or("INFO");
                let details = entry["details"].as_str().unwrap_or("");
                report.push_str(&format!("- [{}] **{}**: {}\n", ts, ev_type, details));
            }
        }

        Ok(report)
    }

    crate::tool_def!(bloat_audit_tool_attr {
        name: "bloat_audit",
        description: "Recursively audit every Rust source under the workspace (AST-based; target/, dot directories and test-only trees skipped) and target/ (build artifact size) for bloat and hardcoded secrets, on std worker threads across all cores. Optional workspace: the absolute directory the CLI would run in, admitted by the MAC policy (default: the server workspace).",
        input_schema: args::typed::<args::WorkspaceView>(),
    });
    pub fn bloat_audit(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        // The CLI runs this in its working directory; the socket
        // takes an optional, MAC-policy-admitted `workspace`.
        let workspace =
            &crate::tools::forward::policy_workspace(arg, "workspace", workspace, false)?;
        gawd::bloat_audit(workspace).map_err(EaiError::governance)
    }

    crate::tool_def!(identity_tool_attr {
        name: "identity",
        description: "SUSI substrate identity report",
        input_schema: args::no_args(),
    });
    pub fn identity(_arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        gawd::identity_report(workspace).map_err(EaiError::governance)
    }

    crate::tool_def!(distill_genome_tool_attr {
        name: "distill_genome",
        description: "Distill the compiled genome into the workspace experience buffer (indexed by semantic search)",
        input_schema: args::no_args(),
    });
    pub fn distill_genome(_arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        gawd::audit_reasoning_substrate(workspace)
            .map(|report| format!("# Genome Distillation\n\n{report}"))
            .map_err(EaiError::governance)
    }

    crate::tool_def!(self_validate_tool_attr {
        name: "self_validate",
        description: "Report the hardware profile and tensor device, and check the compiled genome",
        input_schema: args::no_args(),
    });
    pub fn self_validate(_arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        // A failure is an error, not an Ok string: tool receipts treat Ok
        // output as a successful, citable result.
        match gawd::self_validate(workspace) {
            Ok(report) => Ok(format!(
                "# Substrate Self-Validation Successful\n\n{}",
                report
            )),
            Err(e) => Err(EaiError::governance(format!(
                "Substrate self-validation failed: {}",
                e
            ))),
        }
    }

    crate::tool_def!(reason_tool_attr {
        name: "reason",
        description: "Execute swarm reasoning substrate",
        input_schema: args::typed::<args::ReasonPrompt>(),
    });
    pub fn reason(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let arg_s = if let Some(s) = arg.as_str() {
            s.to_string()
        } else {
            arg.to_string()
        };
        // Untrusted Input Boundary (Mandate 41): `reason` is a second front
        // door into the same reasoning substrate `susi_solve` guards with
        // `SusiMasterAgent::sanitize_input` (reached via `EngineHooks`), and
        // it's also the exact verb LAN peers use to dispatch mission intent
        // (`SusiSupervisor::dispatch_peer_task`, susi-gawd-swarm/amas.rs).
        // Apply the same length/injection-pattern check and the same
        // governance detectors every other action-capable tool call gets
        // (see `exec_command` above) before the raw prompt ever reaches
        // the model.
        let sanitized = gawd_hooks::sanitize_input(&arg_s)?;
        gawd_hooks::audit_action("reason", &sanitized, workspace)?;
        // When the mission captured real tool calls, this reasoning call must
        // answer by citing those receipts — free narrative cannot certify.
        let prompt = format!(
            "{sanitized}{}",
            crate::susi_core::capture::EvidenceSession::evidence_prompt_for(workspace)
        );
        Ok(
            crate::susi_core::plane_bus::gemi::GemiEngine::generate_reasoning_deep(
                &prompt, workspace,
            ),
        )
    }

    crate::tool_def!(susi_solve_tool_attr {
        name: "susi_solve",
        description: "Solve a natural-language intent via the SUSI swarm substrate. Optional workspace: the absolute directory the CLI would run in, admitted by the MAC policy (default: the server workspace).",
        input_schema: args::typed::<args::SusiSolve>(),
    });
    /// Full swarm solve via SusiMasterAgent. Accepts a plain string, or an
    /// object with `intent` / `input` / `prompt` — never the raw JSON Display
    /// of the whole arguments blob prefixed with the tool name.
    pub fn susi_solve(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        // The CLI runs this in its working directory; the socket
        // takes an optional, MAC-policy-admitted `workspace`.
        let workspace =
            &crate::tools::forward::policy_workspace(arg, "workspace", workspace, true)?;
        let intent = if let Some(s) = arg.as_str() {
            s.to_string()
        } else if let Some(s) = arg
            .get("intent")
            .or_else(|| arg.get("input"))
            .or_else(|| arg.get("prompt"))
            .and_then(|v| v.as_str())
        {
            s.to_string()
        } else {
            arg.to_string()
        };
        if intent.trim().is_empty() {
            return Err(EaiError::protocol(
                "Usage: susi_solve with string intent or {intent|input|prompt}",
            ));
        }
        Ok(gawd::solve_mission(
            &intent,
            workspace,
            env!("CARGO_PKG_VERSION"),
        ))
    }

    crate::tool_def!(mission_run_tool_attr {
        name: "mission_run",
        description: "Run a goal as `susi <goal>` does: a stopped local engine may be started on demand, and the result is one JSON document {text, status, exit_code} (exit_code 0 only for SUCCESS or COMPLETE). Optional workspace: the absolute directory the CLI would run in, admitted by the MAC policy (default: the server workspace). Optional model: an advisory model hint.",
        input_schema: args::typed::<args::MissionRun>(),
    });
    /// The CLI's own mission (`susi <goal>`) run in the daemon. Unlike
    /// `susi_solve`, it may start a stopped local engine on demand (as the
    /// in-process goal does) and answers with one JSON document
    /// `{text, status, exit_code}`, so a forwarding client takes its exit
    /// code from the mission status rather than from the answer text.
    pub fn mission_run(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let workspace =
            &crate::tools::forward::policy_workspace(arg, "workspace", workspace, true)?;
        let goal = arg
            .as_str()
            .or_else(|| arg.get("goal").and_then(|v| v.as_str()))
            .unwrap_or("");
        if goal.trim().is_empty() {
            return Err(EaiError::protocol(
                "Usage: mission_run {goal, workspace?, model?}",
            ));
        }
        let model = arg.get("model").and_then(|v| v.as_str());
        let outcome = gawd::run_mission(goal, workspace, env!("CARGO_PKG_VERSION"), model);
        if let Some(error) = outcome.get("error").and_then(|v| v.as_str()) {
            return Err(EaiError::protocol(format!("mission_run: {error}")));
        }
        serde_json::to_string(&outcome).map_err(|e| EaiError::protocol(format!("mission_run: {e}")))
    }

    crate::tool_def!(power_reason_tool_attr {
        name: "power_reason",
        description: "Delegate complex reasoning to Power-Tier MCP remotes",
        input_schema: args::typed::<args::ReasonPrompt>(),
    });
    pub fn power_reason(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let arg_s = if let Some(s) = arg.as_str() {
            s.to_string()
        } else {
            arg.to_string()
        };
        if arg_s.trim().is_empty() {
            return Err(EaiError::protocol("Usage: power_reason <complex_intent>"));
        }

        let remotes = GmcpClient::scout_reasoning_remotes();
        if let Some(best_remote) = remotes.first() {
            let res = GmcpClient::execute_external_tool(best_remote, "reason", &arg_s)?;
            if !remote_result_is_error(&res) {
                return Ok(res);
            }
        }

        Err(EaiError::protocol(
            "No Power-Tier reasoning remotes configured or available. SUSI local reasoning active.",
        ))
    }

    crate::tool_def!(host_telemetry_tool_attr {
        name: "host_telemetry",
        description: "Sample host thermal, battery, and load telemetry",
        input_schema: args::no_args(),
    });
    pub fn host_telemetry(_arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let snapshot = sample_telemetry();
        if let Ok(snap) =
            serde_json::from_value::<crate::susi_core::TelemetrySnapshot>(snapshot.clone())
        {
            ContextGraph::global().record_telemetry(&snap, Some(workspace));
        }
        Ok(serde_json::to_string_pretty(&snapshot).unwrap_or_else(|_| "{}".into()))
    }

    crate::tool_def!(apply_patch_cycle_tool_attr {
        name: "apply_patch_cycle",
        description: "Apply a workspace-confined patch then run tests; revert on failure. Args: files[{path,old,new}], optional test_command, auto_apply, description. Optional workspace: the absolute directory the CLI would run in, admitted by the MAC policy (default: the server workspace).",
        input_schema: args::typed::<args::ApplyPatchCycle>(),
    });
    pub fn apply_patch_cycle(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        // The CLI runs this in its working directory; the socket
        // takes an optional, MAC-policy-admitted `workspace`.
        let workspace =
            &crate::tools::forward::policy_workspace(arg, "workspace", workspace, true)?;
        if arg
            .get("files")
            .and_then(|f| f.as_array())
            .is_none_or(|a| a.is_empty())
        {
            return Err(EaiError::governance("apply_patch_cycle requires files[]"));
        }
        let cfg = crate::susi_sandbox::manager::SusiConfig::load_global_arc().unwrap_or_default();
        // `workspace` chose where the cycle runs; it is not part of the patch.
        let mut request = arg.clone();
        if let Some(map) = request.as_object_mut() {
            map.remove("workspace");
        }
        let request_json = serde_json::to_string(&request)
            .map_err(|e| EaiError::governance(format!("serialize patch request: {e}")))?;
        gawd_hooks::apply_patch_cycle(workspace, &request_json, &cfg.trust_level())
    }

    crate::tool_def!(privacy_status_tool_attr {
        name: "privacy_status",
        description: "Report privacy mode, its network scope (local_only = this machine and its local network; host_only = this machine alone, loopback), mandatory sandbox, and cryptographic capability grants",
        input_schema: args::no_args(),
    });
    pub fn privacy_status(_arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        Ok(serde_json::to_string_pretty(
            &crate::susi_core::mac_policy::MacPolicy::global().status_json(),
        )
        .unwrap_or_else(|_| "{}".into()))
    }

    crate::tool_def!(privacy_consent_egress_tool_attr {
        name: "privacy_consent_egress",
        description: "Grant time-limited network.egress (+ cloud.inference under local_only; refused under host_only). Args: optional subject, optional ttl_secs.",
        input_schema: args::typed::<args::PrivacyConsentEgress>(),
    });
    pub fn privacy_consent_egress(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let subject = arg
            .get("subject")
            .and_then(|v| v.as_str())
            .unwrap_or("susi");
        let ttl = arg.get("ttl_secs").and_then(|v| v.as_u64()).unwrap_or(3600);
        let tokens =
            crate::susi_core::mac_policy::MacPolicy::global().consent_egress(subject, ttl)?;
        Ok(serde_json::to_string_pretty(&serde_json::json!({
            "consented": true,
            "tokens": tokens,
        }))
        .unwrap_or_else(|_| "{}".into()))
    }

    crate::tool_def!(intent_advertise_tool_attr {
        name: "intent_advertise",
        description: "Advertise a provider capability. Args: from, intent, optional payload.",
        input_schema: args::typed::<args::IntentAdvertise>(),
    });
    pub fn intent_advertise(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let from = arg.get("from").and_then(|v| v.as_str()).unwrap_or("agent");
        let intent = arg
            .get("intent")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EaiError::governance("intent_advertise requires intent"))?;
        let payload = arg
            .get("payload")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let msg = crate::susi_core::intent_bus::IntentBus::global()
            .advertise(from, intent, payload, None);
        Ok(serde_json::to_string_pretty(&msg).unwrap_or_else(|_| "{}".into()))
    }

    crate::tool_def!(intent_need_tool_attr {
        name: "intent_need",
        description:
            "Publish a need and match providers. Args: from, intent, optional min_score, limit.",
        input_schema: args::typed::<args::IntentNeed>(),
    });
    pub fn intent_need(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let from = arg
            .get("from")
            .and_then(|v| v.as_str())
            .unwrap_or("planner");
        let intent = arg
            .get("intent")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EaiError::governance("intent_need requires intent"))?;
        let min_score = arg.get("min_score").and_then(|v| v.as_f64()).unwrap_or(0.2) as f32;
        let limit = arg.get("limit").and_then(|v| v.as_u64()).unwrap_or(5) as usize;
        let (need, matches) = crate::susi_core::intent_bus::IntentBus::global().need(
            from,
            intent,
            arg.get("payload")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
            None,
            min_score,
            limit,
        );
        Ok(serde_json::to_string_pretty(&serde_json::json!({
            "need": need,
            "matches": matches,
        }))
        .unwrap_or_else(|_| "{}".into()))
    }

    #[cfg(feature = "tools-rich")]
    crate::tool_def!(ambient_pulse_tool_attr {
        name: "ambient_pulse",
        description: "Scan workspace FS changes into context graph and refresh semantic index. Optional workspace: the absolute directory the CLI would run in, admitted by the MAC policy (default: the server workspace).",
        input_schema: args::typed::<args::WorkspaceView>(),
    });
    #[cfg(feature = "tools-rich")]
    pub fn ambient_pulse(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        // The CLI runs this in its working directory; the socket
        // takes an optional, MAC-policy-admitted `workspace`.
        let workspace =
            &crate::tools::forward::policy_workspace(arg, "workspace", workspace, true)?;
        let indexed = super::semantic_index::SemanticIndex::refresh(workspace).unwrap_or(0);
        ContextGraph::global().record_external_context(
            "ambient_pulse",
            "manual ambient pulse",
            &serde_json::json!({"docs_indexed": indexed}),
            Some(workspace),
            None,
        );
        Ok(serde_json::to_string_pretty(&serde_json::json!({
            "docs_indexed": indexed,
        }))
        .unwrap_or_else(|_| "{}".into()))
    }

    crate::tool_def!(tx_begin_tool_attr {
        name: "tx_begin",
        description: "Begin multi-agent transaction. Args: description, files (array of relative paths). Optional workspace: the absolute directory the CLI would run in, admitted by the MAC policy (default: the server workspace).",
        input_schema: args::typed::<args::TxBegin>(),
    });
    pub fn tx_begin(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        // The CLI runs this in its working directory; the socket
        // takes an optional, MAC-policy-admitted `workspace`.
        let workspace =
            &crate::tools::forward::policy_workspace(arg, "workspace", workspace, true)?;
        let description = arg
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("agent tx");
        let files: Vec<String> = arg
            .get("files")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        let tx = crate::susi_core::agent_tx::TxManager::global().begin(
            workspace,
            description,
            &files,
            Default::default(),
        )?;
        Ok(serde_json::to_string_pretty(&tx).unwrap_or_else(|_| "{}".into()))
    }

    crate::tool_def!(tx_commit_tool_attr {
        name: "tx_commit",
        description: "Commit open transaction. Args: id.",
        input_schema: args::typed::<args::TxId>(),
    });
    pub fn tx_commit(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let id = arg
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EaiError::governance("tx_commit requires id"))?;
        let txm = crate::susi_core::agent_tx::TxManager::global();
        // A transaction begun by another process (e.g. `susi tx begin`)
        // exists only in the durable journal until hydrated.
        txm.hydrate(workspace);
        let tx = txm.commit(id, workspace)?;
        Ok(serde_json::to_string_pretty(&tx).unwrap_or_else(|_| "{}".into()))
    }

    crate::tool_def!(tx_abort_tool_attr {
        name: "tx_abort",
        description: "Abort transaction and restore files. Args: id.",
        input_schema: args::typed::<args::TxId>(),
    });
    pub fn tx_abort(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let id = arg
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EaiError::governance("tx_abort requires id"))?;
        let txm = crate::susi_core::agent_tx::TxManager::global();
        txm.hydrate(workspace);
        let tx = txm.abort(id, workspace)?;
        Ok(serde_json::to_string_pretty(&tx).unwrap_or_else(|_| "{}".into()))
    }

    crate::tool_def!(delegate_to_ide_tool_attr {
        name: "delegate_to_ide",
        description: "Delegate a complex, ambiguous, or IDE-level task to an overarching IDE Agent like Antigravity. Use this when the swarm encounters a problem requiring a full IDE environment or human interaction.",
        input_schema: args::typed::<args::DelegateToIde>(),
    });
    pub fn delegate_to_ide(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let goal = arg
            .get("goal")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EaiError::protocol("goal is required"))?;
        let context = arg.get("context").and_then(|v| v.as_str()).unwrap_or("");

        let delegations_dir = susi_paths::SusiDirs::data_dir().join("delegations");
        std::fs::create_dir_all(&delegations_dir)
            .map_err(|e| EaiError::filesystem(e.to_string()))?;

        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let req_path = delegations_dir.join(format!("ide_request_{ts}.md"));
        let res_path = delegations_dir.join(format!("ide_response_{ts}.md"));

        let content = format!("# IDE Delegation Request\n\n## Goal\n{}\n\n## Context\n{}\n\nWrite your response to: {}", goal, context, res_path.display());
        std::fs::write(&req_path, content).map_err(|e| EaiError::filesystem(e.to_string()))?;

        Ok(format!(
            "Delegation request written to {}. The Swarm will await the IDE's response.",
            req_path.display()
        ))
    }

    crate::tool_def!(brain_pareto_tool_attr {
        name: "brain_pareto",
        description: "Show the 3D Pareto frontier, Wilson 95% lower confidence bounds, and Bayesian UCB1 exploration-exploitation indices across providers",
        input_schema: args::typed::<args::BrainPareto>(),
    });
    pub fn brain_pareto(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let class = arg
            .get("class")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("code");
        let resp = crate::susi_core::plane_bus::PlaneBus::global()
            .request("gemi.brain.pareto", serde_json::json!({ "class": class }))
            .map_err(EaiError::process)?;
        serde_json::to_string_pretty(&resp).map_err(|e| EaiError::protocol(e.to_string()))
    }
}
