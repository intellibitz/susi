//! Plane-bus handler for all `gawd.*` topics.

use crate::susi_core::audit_export::{AuditAction, AuditChokePoint, AuditOutcome, AuditSink};
use crate::susi_core::plane_bus::topics;
use crate::susi_core::plane_bus::{PlaneBus, PlaneHandler};
use crate::susi_error::EaiResult;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;

pub struct GawdPlaneHandler;

fn workspace_path(payload: &Value) -> PathBuf {
    payload
        .get("workspace")
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Run `mission` as the user's own request: like `susi <goal>` in process
/// (`SusiMasterAgent::solve_stream_report`), it may start a stopped local
/// engine on demand. The permission is thread-scoped and the mission runs
/// on this thread, so it covers the whole solve and ends with it.
fn as_user_request<T>(mission: impl FnOnce() -> T) -> T {
    let _user_request = crate::susi_core::provider::OnDemandEngineStart::allow();
    mission()
}

/// The `mission_run` answer: the final text as the CLI prints it (wrapped
/// and redacted), the mission status and the CLI exit code derived from
/// the report (`SusiMissionReport::exit_code`).
fn mission_outcome(report: &crate::ama::SusiMissionReport) -> Value {
    json!({
        "text": report.final_answer_for_sink(),
        "status": report.status,
        "exit_code": if report.is_success() { 0 } else { 1 },
    })
}

fn eai_to_string<T>(r: EaiResult<T>) -> Result<T, String> {
    r.map_err(|e| e.to_string())
}

struct SignedAuditSink {
    workspace: PathBuf,
}

impl AuditSink for SignedAuditSink {
    fn append(&self, action: &AuditAction) -> EaiResult<()> {
        let details = serde_json::to_string(action).map_err(|error| {
            crate::susi_error::EaiError::protocol(format!("serialize audit action: {error}"))
        })?;
        crate::susi_sandbox::manager::SusiAuditLogger::log_event(
            &self.workspace,
            "AUDIT_ACTION",
            &details,
        );
        Ok(())
    }
}

fn persist_audit_action(action: &AuditAction, workspace: &std::path::Path) -> Result<(), String> {
    AuditChokePoint::new(SignedAuditSink {
        workspace: workspace.to_path_buf(),
    })
    .dispatch(action)
    .map_err(|error| error.to_string())
}

/// Multi-tenant isolation, quotas and per-tenant billing are organisation-scale
/// capabilities provided by Susi commercial apps, not this single-host substrate.
/// Requests that name a tenant fail closed instead of running untenanted.
const MULTI_TENANT_COMMERCIAL: &str =
    "multi-tenant isolation is a Susi commercial capability; this host serves a single owner";

impl PlaneHandler for GawdPlaneHandler {
    fn handle(&self, topic: &str, payload: Value) -> Result<Value, String> {
        crate::init_hooks();
        match topic {
            topics::GAWD_SOLVE => {
                let intent = payload.get("intent").and_then(|v| v.as_str()).unwrap_or("");
                let ws = workspace_path(&payload);
                let version = payload
                    .get("version")
                    .and_then(|v| v.as_str())
                    .unwrap_or(env!("CARGO_PKG_VERSION"));
                let model = payload.get("model").and_then(|v| v.as_str());
                let generative = payload
                    .get("generative")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let requires = payload.get("requires").and_then(|v| v.as_str());
                let chat = payload.get("chat").cloned();
                if payload.get("tenant_id").is_some() {
                    return Ok(json!({ "error": MULTI_TENANT_COMMERCIAL }));
                }
                let text = crate::susi_core::plane_bus::with_watched(&payload, || {
                    let master = crate::ama::SusiMasterAgent::new();
                    if generative {
                        master.solve_clean_generative_with_requires_chat(
                            intent, &ws, version, model, requires, chat,
                        )
                    } else {
                        master.solve_clean_with_model(intent, &ws, version, model)
                    }
                });
                Ok(json!({ "text": text }))
            }
            topics::GAWD_MISSION_RUN => {
                let intent = payload.get("intent").and_then(|v| v.as_str()).unwrap_or("");
                if intent.trim().is_empty() {
                    return Ok(json!({ "error": "intent is required" }));
                }
                if payload.get("tenant_id").is_some() {
                    return Ok(json!({ "error": MULTI_TENANT_COMMERCIAL }));
                }
                let ws = workspace_path(&payload);
                let version = payload
                    .get("version")
                    .and_then(|v| v.as_str())
                    .unwrap_or(env!("CARGO_PKG_VERSION"));
                let model = payload.get("model").and_then(|v| v.as_str());
                // A watched forwarded goal streams its progress lines back and
                // observes cooperative cancellation at phase boundaries.
                let report = crate::susi_core::plane_bus::with_watched(&payload, || {
                    as_user_request(|| {
                        crate::ama::SusiMasterAgent::new().solve_stream_report_with_model(
                            intent,
                            &ws,
                            version,
                            model,
                            &|_| {},
                        )
                    })
                });
                Ok(mission_outcome(&report))
            }
            topics::GAWD_PLAN_AUTONOMOUS => {
                let goal = payload.get("goal").and_then(|v| v.as_str()).unwrap_or("");
                if goal.trim().is_empty() {
                    return Ok(json!({ "error": "goal is required" }));
                }
                let ws = workspace_path(&payload);
                let version = payload
                    .get("version")
                    .and_then(|v| v.as_str())
                    .unwrap_or(env!("CARGO_PKG_VERSION"));
                let max_steps = payload
                    .get("max_steps")
                    .and_then(|v| v.as_u64())
                    .and_then(|n| u32::try_from(n).ok())
                    .unwrap_or(3);
                match crate::ama::SusiMasterAgent::new()
                    .solve_autonomous(goal, &ws, version, max_steps)
                {
                    Ok(report) => Ok(report.plan_summary()),
                    Err(e) => Ok(json!({ "error": e.to_string() })),
                }
            }
            topics::GAWD_PULSE_INGEST => {
                let intent = payload.get("intent").and_then(|v| v.as_str()).unwrap_or("");
                let ws = workspace_path(&payload);
                match crate::compliance::SusiAdmin::ingest_natural_intent(&ws, intent) {
                    Ok(text) => Ok(json!({ "text": text })),
                    Err(e) => Ok(json!({ "error": e.to_string() })),
                }
            }
            topics::GAWD_AUDIT_COMPLIANCE => {
                let ws = workspace_path(&payload);
                match crate::compliance::SusiAdmin::audit_compliance(&ws, Some("push")) {
                    Ok(text) => Ok(json!({ "text": text })),
                    Err(e) => Ok(json!({ "error": format!("Compliance audit failed: {e}") })),
                }
            }
            topics::GAWD_AUDIT_EVIDENCE_EXPORT => {
                let field = |key: &str| {
                    payload
                        .get(key)
                        .and_then(|v| v.as_str())
                        .map(std::path::PathBuf::from)
                };
                let (Some(input), Some(output)) = (field("input"), field("output")) else {
                    return Ok(json!({ "error": "input and output are required" }));
                };
                match crate::audit_evidence::export_to_file(&input, &output) {
                    Ok(segments) => Ok(json!({
                        "segments": segments,
                        "output": output.display().to_string(),
                    })),
                    Err(e) => Ok(json!({ "error": e })),
                }
            }
            topics::GAWD_SUBSTRATE_REPORT => {
                let ws = workspace_path(&payload);
                match crate::ama::SusiMasterAgent::new().generate_substrate_report(&ws) {
                    Ok(text) => Ok(json!({ "text": text })),
                    Err(e) => Ok(json!({
                        "error": format!("Failed to generate substrate report: {e}")
                    })),
                }
            }
            topics::GAWD_SANITIZE => {
                let input = payload.get("input").and_then(|v| v.as_str()).unwrap_or("");
                match crate::ama::SusiMasterAgent::sanitize_input(input) {
                    Ok(text) => Ok(json!({ "text": text })),
                    Err(e) => Ok(json!({ "error": e.to_string() })),
                }
            }
            topics::GAWD_AUDIT_ACTION => {
                let tool = payload.get("tool").and_then(|v| v.as_str()).unwrap_or("");
                let detail = payload.get("detail").and_then(|v| v.as_str()).unwrap_or("");
                let ws = workspace_path(&payload);
                if payload.get("tenant_id").is_some() {
                    return Ok(json!({ "error": MULTI_TENANT_COMMERCIAL }));
                }

                let mut action = payload
                    .get("action")
                    .cloned()
                    .map(serde_json::from_value::<AuditAction>)
                    .transpose()
                    .map_err(|error| format!("invalid structured audit action: {error}"))?
                    .map_or_else(
                        || AuditAction::from_legacy(tool, detail, AuditOutcome::Succeeded),
                        Ok,
                    )
                    .map_err(|error| error.to_string())?;
                let gate_result = crate::safety::SafetyDetector::audit_action(tool, detail, &ws)
                    .and_then(|()| {
                        crate::security::SecurityDetector::audit_action(tool, detail, &ws)
                    });
                if let Err(error) = gate_result {
                    action.outcome = AuditOutcome::Denied;
                    persist_audit_action(&action, &ws)?;
                    return Ok(json!({ "error": error.to_string() }));
                }
                persist_audit_action(&action, &ws)?;
                Ok(json!({ "ok": true }))
            }
            topics::GAWD_PATCH => {
                let ws = workspace_path(&payload);
                let trust = payload
                    .get("trust_level")
                    .and_then(|v| v.as_str())
                    .unwrap_or("manual");
                let request: crate::patch_cycle::PatchRequest = if payload.get("files").is_some() {
                    serde_json::from_value(payload.clone())
                        .map_err(|e| format!("invalid patch payload: {e}"))?
                } else {
                    let request_json = payload
                        .get("request_json")
                        .and_then(|v| v.as_str())
                        .ok_or("request_json or files required")?;
                    serde_json::from_str(request_json)
                        .map_err(|e| format!("invalid patch request JSON: {e}"))?
                };
                match crate::patch_cycle::apply_patch_cycle(&ws, &request, trust) {
                    Ok(outcome) => serde_json::to_value(outcome).map_err(|e| e.to_string()),
                    Err(e) => Ok(json!({ "error": e.to_string() })),
                }
            }
            topics::GAWD_BLOAT => {
                let ws = workspace_path(&payload);
                let report = eai_to_string(crate::bloat_audit::BloatAuditor::audit_workspace(&ws))?;
                Ok(json!({
                    "text": crate::bloat_audit::BloatAuditor::render_report(&report)
                }))
            }
            topics::GAWD_IDENTITY => {
                let ws = workspace_path(&payload);
                Ok(json!({ "text": susi_gawd_agents::self_core::identity_report(&ws) }))
            }
            "gawd.auth.mint_aic" => {
                // Phase 3: BYOA (Bring Your Own Agent) Identity Registration
                let agent_id = payload
                    .get("agent_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("anonymous_agent");
                let requested_caps = payload
                    .get("requested_capabilities")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();

                let caps: Vec<String> = requested_caps
                    .into_iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect();
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let expires_at = now + 3600;

                // Mock cryptographic signing for Phase 1.
                let aic = json!({
                    "agent_id": agent_id,
                    "capabilities": caps,
                    "issued_at": now,
                    "expires_at": expires_at,
                    "token": format!("aic_{}_{}", agent_id, expires_at),
                });

                Ok(json!({ "certificate": aic }))
            }
            topics::GAWD_REASON_AUDIT => {
                let ws = workspace_path(&payload);
                let text = eai_to_string(
                    crate::reason_trainer::ReasoningTrainer::audit_reasoning_substrate(&ws),
                )?;
                Ok(json!({ "text": text }))
            }
            topics::GAWD_SELF_VALIDATE => {
                let ws = workspace_path(&payload);
                let text = eai_to_string(
                    crate::self_validation::execute_autonomous_self_validation(&ws),
                )?;
                Ok(json!({ "text": text }))
            }
            topics::GAWD_TRAIN_REFLEX => {
                let ws = workspace_path(&payload);
                let text = eai_to_string(crate::reflex_trainer::ReflexTrainer::force_train(&ws))?;
                Ok(json!({ "text": text }))
            }
            topics::GAWD_CAPABILITY_GAP => {
                let name = payload.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let ws = workspace_path(&payload);
                use crate::reflex_synth::ReflexSynthesizer;
                let outcome = ReflexSynthesizer::synthesize_capability(name, &ws);
                let text = ReflexSynthesizer::gap_report(name, &outcome);
                Ok(json!({ "text": text }))
            }
            topics::GAWD_LOCK_BROADCAST => {
                let resource_id = payload
                    .get("resource_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                Ok(json!({
                    "ok": crate::amas::SusiSupervisor::broadcast_lock_request(resource_id)
                }))
            }
            topics::GAWD_SCHEDULER_RECENT => {
                let limit = payload.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize;
                let mut decisions =
                    susi_gawd_agents::scheduler::MissionScheduler::recent_decisions();
                decisions.truncate(limit);
                serde_json::to_value(decisions).map_err(|e| e.to_string())
            }
            topics::GAWD_CLUSTER_PEERS => {
                // Verified roster for commit-ledger anti-entropy: only
                // active Local/Explicit members are useful replication
                // targets — a Discovered node's address is untrusted.
                let peers: Vec<Value> = crate::amas::SusiSupervisor::list_cluster_nodes()
                    .into_iter()
                    .filter(|n| {
                        n.is_active
                            && matches!(
                                n.admission,
                                crate::amas::PeerAdmission::Local
                                    | crate::amas::PeerAdmission::Explicit
                            )
                    })
                    .map(|n| {
                        json!({ "node_id": n.node_id, "address": n.address,
                                "trust_score": n.trust_score })
                    })
                    .collect();
                Ok(json!({ "peers": peers }))
            }
            // Literal like `gawd.auth.mint_aic` — the topics module lives
            // in susi-core; this arm is the swarm mission rollback
            // surface (T-DEEPSEEK-94).
            "gawd.mission.rollback" => {
                let ws = workspace_path(&payload);
                let mission_id = payload
                    .get("mission_id")
                    .and_then(|v| v.as_str())
                    .ok_or("mission_id required")?;
                let snapshot = payload.get("snapshot").and_then(|v| v.as_str());
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                match susi_gawd_swarm::mission_resume::rollback_mission(
                    &ws, mission_id, snapshot, now,
                ) {
                    Ok((view, line)) => Ok(json!({
                        "status": line,
                        "mission_id": view.mission_id,
                        "summary": format!("{:?}", view.summary()),
                        "snapshots": susi_gawd_swarm::mission_persist::PersistedMission::list_snapshots(&ws, mission_id),
                    })),
                    Err(e) => Ok(json!({ "error": e.to_string() })),
                }
            }
            topic if topic.starts_with("gawd.tenant.") => {
                Ok(json!({ "error": MULTI_TENANT_COMMERCIAL }))
            }
            other => Err(format!("gawd handler: unhandled topic '{other}'")),
        }
    }
}

pub fn register() {
    PlaneBus::global().register_prefix("gawd.", Arc::new(GawdPlaneHandler));
}

/// Direct entry point to dispatch a topic through the GAWD plane handler.
pub fn handle_topic(topic: &str, payload: Value) -> Result<Value, String> {
    GawdPlaneHandler.handle(topic, payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tenant_scoped_requests_fail_closed_in_the_open_substrate() {
        for (topic, payload) in [
            ("gawd.tenant.register", json!({ "tenant_id": "acme" })),
            ("gawd.tenant.status", json!({ "tenant_id": "acme" })),
            (
                topics::GAWD_SOLVE,
                json!({ "intent": "x", "tenant_id": "acme" }),
            ),
            (
                topics::GAWD_MISSION_RUN,
                json!({ "intent": "x", "tenant_id": "acme" }),
            ),
            (
                topics::GAWD_AUDIT_ACTION,
                json!({ "tool": "t", "tenant_id": "acme" }),
            ),
        ] {
            let out = handle_topic(topic, payload).expect("handled");
            assert_eq!(out["error"], MULTI_TENANT_COMMERCIAL, "{topic}");
        }
    }

    /// The forwarded goal gets the same on-demand engine start the
    /// in-process goal gets, for exactly as long as the mission runs.
    #[test]
    fn mission_run_allows_on_demand_engine_start_only_while_the_mission_runs() {
        use crate::susi_core::provider::OnDemandEngineStart;
        assert!(!OnDemandEngineStart::allowed());
        assert!(as_user_request(OnDemandEngineStart::allowed));
        assert!(!OnDemandEngineStart::allowed());
    }

    #[test]
    fn mission_outcome_exit_code_follows_the_status_not_the_prose() {
        let report = |status: &str, answer: &str| crate::ama::SusiMissionReport {
            goal: "g".into(),
            status: status.into(),
            agents: Vec::new(),
            interactions: Vec::new(),
            final_answer: answer.into(),
            plan: None,
        };
        for status in ["SUCCESS", "COMPLETE"] {
            let out = mission_outcome(&report(status, "Error: looks like a failure"));
            assert_eq!(out["exit_code"], 0, "{status}");
            assert_eq!(out["status"], status);
            let text = out["text"].as_str().unwrap_or_default();
            assert!(text.contains("Error: looks like a failure"), "{text}");
        }
        for status in ["FAILED", "BLOCKED", "PARTIAL"] {
            assert_eq!(
                mission_outcome(&report(status, "done"))["exit_code"],
                1,
                "{status}"
            );
        }
        // The text is the redacted form the CLI prints, never the raw answer.
        let secret = report(
            "SUCCESS",
            "key sk-ant-api03-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        );
        let printed = secret.final_answer_for_sink();
        assert_eq!(mission_outcome(&secret)["text"], printed);
    }

    #[test]
    fn mission_run_requires_an_intent() {
        for payload in [json!({}), json!({ "intent": "  " })] {
            let out = handle_topic(topics::GAWD_MISSION_RUN, payload).expect("handled");
            assert_eq!(out["error"], "intent is required");
        }
    }

    #[test]
    fn unknown_gawd_topic_returns_unhandled_topic_error() {
        let err = handle_topic("gawd.nonexistent.topic", json!({}))
            .expect_err("unknown topic must return Err");
        assert!(err.contains("unhandled topic"));
    }

    #[test]
    fn register_wires_gawd_prefix_on_global_plane_bus_idempotently() {
        register();
        register();
        let res = PlaneBus::global()
            .request(
                "gawd.auth.mint_aic",
                json!({
                    "agent_id": "test_agent",
                    "requested_capabilities": ["code", "verify"]
                }),
            )
            .expect("plane bus request");
        assert_eq!(res["certificate"]["agent_id"], "test_agent");
    }

    #[test]
    fn non_tenant_readonly_topics_succeed_without_commercial_gate() {
        let recent = handle_topic(topics::GAWD_SCHEDULER_RECENT, json!({ "limit": 5 }))
            .expect("scheduler recent");
        assert!(recent.is_array());

        let peers = handle_topic(topics::GAWD_CLUSTER_PEERS, json!({})).expect("cluster peers");
        assert!(peers.get("peers").and_then(Value::as_array).is_some());
    }
}
