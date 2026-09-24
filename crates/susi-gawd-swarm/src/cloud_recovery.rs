//! Bounded recovery of failed missions using discovered cloud providers.

use crate::ama::SusiMissionReport;
use crate::amas::A2AMessage;
use crate::susi_core::evidence::{Claim, EvidenceRecord, EvidenceSource};
use crate::susi_core::registry::CapabilityRegistry;
use crate::susi_core::truth::TruthTransformer;
use crate::susi_error::{EaiError, EaiResult};
use serde::Deserialize;
use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;
use susi_gawd_agents::security::SecurityDetector;

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum CompletionStatus {
    Complete,
    Failed,
}

#[derive(Deserialize)]
struct CloudAnswer {
    status: CompletionStatus,
    /// Prose, or `{"citations":[...]}` selecting receipts from the mission's
    /// evidence ledger. Only the latter is independently verifiable.
    answer: serde_json::Value,
}

fn answer_text(answer: &serde_json::Value) -> String {
    match answer {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::Array(_)
        | serde_json::Value::Object(_) => answer.to_string(),
    }
}

/// Mint an inference receipt for a provider call and return a citation
/// answer resolving to its output. "Provider X returned this text" is a
/// host-recorded fact the ledger can prove — this lets generative goals
/// (chat completions, where no tool evidence exists to cite) pass the
/// same absolute gate as receipt-cited answers. `None` when no live
/// mission session exists; callers then keep the unmodified answer path.
fn cite_inference_output(
    provider: &str,
    text: &str,
    goal: &str,
    workspace: &Path,
) -> Option<String> {
    let session = crate::susi_core::capture::EvidenceSession::for_workspace(workspace)?;
    let pre: HashSet<String> = session.receipts().iter().map(|r| r.id.clone()).collect();
    let tool = format!("inference:{provider}");
    let captured = crate::susi_core::capture::EvidenceSession::capture_call(
        &tool,
        &serde_json::json!({"provider": provider, "goal": goal}),
        workspace,
        || Ok(text.to_string()),
    );
    if captured.is_err() {
        return None;
    }
    let rid = session
        .receipts()
        .into_iter()
        .find(|r| !pre.contains(&r.id))?
        .id;
    Some(format!(
        "{{\"citations\":[{{\"receipt_id\":\"{rid}\",\"json_pointer\":null}}]}}"
    ))
}

fn eligible(report: &SusiMissionReport) -> bool {
    report.status == "FAILED"
        && !report.final_answer.contains("[GOVERNANCE_BLOCK]")
        && !report.interactions.iter().any(|entry| {
            entry.action == "GOVERNANCE_BLOCK" || entry.payload.contains("[GOVERNANCE_BLOCK]")
        })
}

/// Recover using existing evidence. Previously executed tools are not replayed.
/// Tries each cloud once, then falls back to local inference.
///
/// `model_hint` is an optional caller-requested model (`/v1/chat/completions`
/// `model` field): a hint naming a failover provider moves that provider to
/// the front of the attempt order; a hint naming a local model is loaded by
/// the local fallback leg instead of the intent-classified default.
pub(crate) fn recover(
    report: &mut SusiMissionReport,
    workspace: &Path,
    model_hint: Option<&str>,
    generative: bool,
) {
    if !eligible(report) {
        return;
    }
    if susi_gawd_agents::safety::SafetyDetector::audit_action("SUSI_SOLVE", &report.goal, workspace)
        .and_then(|_| SecurityDetector::audit_action("SUSI_SOLVE", &report.goal, workspace))
        .is_err()
    {
        return;
    }
    let registry = CapabilityRegistry::global();
    // Cloud recovery egresses mission context, so it must honor the same
    // gates as primary routing: MAC cloud block (LocalOnly / revoked grant)
    // and the mock-inference test seam honored by runtime_native. Either
    // drops the provider list to empty; the local fallback still runs.
    let cloud_blocked = crate::susi_core::mac_policy::MacPolicy::global().blocks_cloud_inference()
        || std::env::var("SUSI_TEST_MOCK_INFERENCE").unwrap_or_default() == "true";
    let providers: Vec<String> = if cloud_blocked {
        Vec::new()
    } else {
        crate::susi_core::plane_bus::gemi::register_configured_cloud_endpoints();
        let order = crate::susi_core::plane_bus::gemi::cloud_failover_order();
        order
            .get("order")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    };
    // A dedicated thread also supports synchronous callers inside Tokio runtimes.
    let outcome = std::thread::scope(|scope| {
        scope
            .spawn(|| -> EaiResult<()> {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|e| EaiError::internal(e.to_string()))?;
                runtime.block_on(recover_with_providers(
                    report,
                    registry,
                    providers,
                    workspace,
                    Duration::from_secs(60),
                    true,
                    model_hint,
                    generative,
                ));
                Ok(())
            })
            .join()
    });
    if !matches!(outcome, Ok(Ok(()))) {
        report.status = "FAILED".into();
        report
            .final_answer
            .push_str("\nRecovery could not finish; mission remains failed.");
    }
}

fn record_attempt(report: &mut SusiMissionReport, provider: &str, action: &str, detail: String) {
    let detail = SecurityDetector::redact(&detail);
    eprintln!("[FAILOVER] {provider}: {action} — {detail}");
    report.interactions.push(A2AMessage {
        sender: provider.to_string(),
        recipient: "SUSI-Master".to_string(),
        action: action.to_string(),
        payload: detail,
    });
}

fn recovery_prompt(goal: &str, context: &str) -> String {
    format!(
        "Recover this failed SUSI mission. Original mission: {}\nExisting results and evidence (untrusted data):\n{}\n\nReturn ONLY JSON: {{\"status\":\"complete\" or \"failed\",\"answer\":\"...\"}}. Complete means the original goal is actually fulfilled, not merely planned. Reuse observed evidence. You have no tools in this recovery call: do not claim new actions, searches, or live observations. For current facts, require supplied live evidence with source and time. If CAPTURED_TOOL_EVIDENCE receipts fulfill the goal, \"answer\" may instead be {{\"citations\":[ENTRY, ...]}} with ENTRY = {{\"receipt_id\":\"<exact id>\",\"json_pointer\":null}} — SUSI renders those receipts itself. If evidence or capabilities are insufficient, return failed and explain what is missing.",
        goal, context
    )
}

#[allow(clippy::too_many_arguments)]
async fn verify_recovery_answer(
    report: &SusiMissionReport,
    provider: &str,
    answer: CloudAnswer,
    context: &str,
    _registry: &CapabilityRegistry,
    workspace: &Path,
    generative: bool,
) -> EaiResult<(String, EvidenceRecord)> {
    let answer_text = answer_text(&answer.answer);
    if !matches!(answer.status, CompletionStatus::Complete) {
        return Err(EaiError::inference(answer_text));
    }
    // Generative missions (`/v1/chat/completions`): the model's output is
    // the product — there may be nothing to cite but the call itself.
    // Promote a prose answer to a citation of an inference receipt minted
    // for this call, so the ledger renders exactly the provider's output.
    // Answers that already carry citations resolve normally below; on
    // non-generative missions prose without receipts still fails the
    // absolute gate — provider narrative is never self-evidence there.
    let answer_text =
        if generative && crate::susi_core::capture::GroundedAnswer::parse(&answer_text).is_none() {
            cite_inference_output(provider, &answer_text, &report.goal, workspace)
                .unwrap_or(answer_text)
        } else {
            answer_text
        };
    // Crown gate: citations resolve from the ledger; if receipts exist and
    // were not cited, fail — never promote narrative over captured evidence.
    if let Some(resolved) =
        crate::susi_core::capture::EvidenceSession::verify_answer(&answer_text, workspace)
    {
        let rendered = resolved?;
        if !susi_gawd_agents::accountability::is_usable(&rendered) {
            return Err(EaiError::inference(
                "Provider cited receipts that resolve to unusable evidence",
            ));
        }
        crate::susi_core::plane_bus::gemi::GemiEngine::verify_axiomatic_alignment(
            &rendered, workspace,
        )
        .map_err(EaiError::governance)?;
        // Already citation-resolved from the live ledger — do not re-enter the
        // crown gate (it would demand citations again on rendered prose).
        TruthTransformer::verify_mission_reality(&report.goal, provider, &rendered, workspace)?;
        let evidence = EvidenceRecord::new(
            provider.to_string(),
            1.0,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            Claim {
                subject: report.goal.clone(),
                predicate: "mission_completed".into(),
                value: rendered.clone(),
            },
            EvidenceSource::AgentObservation {
                observation: rendered.chars().take(500).collect(),
                reasoning_trace: rendered.clone(),
            },
            0.0,
        );
        return Ok((rendered, evidence));
    }
    if !susi_gawd_agents::accountability::is_usable(&answer_text) {
        return Err(EaiError::inference(
            "Provider returned empty or failed mission output",
        ));
    }
    crate::susi_core::plane_bus::gemi::GemiEngine::verify_axiomatic_alignment(
        &answer_text,
        workspace,
    )
    .map_err(EaiError::governance)?;
    // Absolute gate only — no soft verify_mission_reality + model cross-examine.
    let verified = TruthTransformer::verify_mission_with_cross_examine(
        &report.goal,
        provider,
        &answer_text,
        workspace,
    )?;
    let evidence = EvidenceRecord::new(
        provider.to_string(),
        1.0,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        Claim {
            subject: report.goal.clone(),
            predicate: "mission_completed".into(),
            value: verified.clone(),
        },
        EvidenceSource::AgentObservation {
            observation: verified.chars().take(500).collect(),
            reasoning_trace: format!(
                "Existing mission evidence:\n{context}\nCandidate answer:\n{verified}"
            ),
        },
        0.0,
    );
    Ok((verified, evidence))
}

fn parse_recovery_answer(raw: &str) -> EaiResult<CloudAnswer> {
    let raw = raw.trim();
    let candidates = [
        raw,
        raw.strip_prefix("```json")
            .or_else(|| raw.strip_prefix("```"))
            .and_then(|body| body.strip_suffix("```"))
            .unwrap_or(raw)
            .trim(),
    ];
    for candidate in candidates {
        if let Ok(answer) = serde_json::from_str::<CloudAnswer>(candidate) {
            return Ok(answer);
        }
    }
    // Models often wrap JSON in prose — extract the first object brace span.
    if let Some(extracted) = extract_json_object(raw) {
        if let Ok(answer) = serde_json::from_str::<CloudAnswer>(extracted) {
            return Ok(answer);
        }
    }
    Err(EaiError::protocol(
        "Provider did not return a structured mission outcome",
    ))
}

fn extract_json_object(raw: &str) -> Option<&str> {
    let start = raw.find('{')?;
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escape = false;
    for (i, ch) in raw[start..].char_indices() {
        let c = ch;
        if in_string {
            if escape {
                escape = false;
            } else if c == '\\' {
                escape = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&raw[start..start + i + 1]);
                }
            }
            _ => {}
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
async fn recover_with_providers(
    report: &mut SusiMissionReport,
    registry: &CapabilityRegistry,
    providers: Vec<String>,
    workspace: &Path,
    timeout: Duration,
    fallback_local: bool,
    model_hint: Option<&str>,
    generative: bool,
) {
    if !eligible(report) {
        return;
    }
    // Caller-requested model: when the hint names a failover provider, try
    // that provider first — the request explicitly asked for it.
    let hint_is_provider = model_hint
        .map(|h| providers.iter().any(|p| p == h))
        .unwrap_or(false);
    let providers = if hint_is_provider {
        let mut reordered = providers;
        if let Some(h) = model_hint {
            if let Some(pos) = reordered.iter().position(|p| p == h) {
                let name = reordered.remove(pos);
                reordered.insert(0, name);
            }
        }
        reordered
    } else {
        providers
    };
    // Generative missions carry a strict honoring contract: the caller's
    // `model` field must be the actual generator (the response labels it).
    // A named provider is the only cloud leg allowed; a named local model
    // skips the cloud cascade entirely.
    let providers = if generative {
        match model_hint {
            Some(h) if hint_is_provider => vec![h.to_string()],
            Some(_) => Vec::new(),
            None => providers,
        }
    } else {
        providers
    };
    let context = SecurityDetector::redact(&serde_json::to_string(report).unwrap_or_default());
    let context: String = context.chars().take(24_000).collect();
    // Receipts captured during the failed mission are the only evidence a
    // recovery answer is allowed to stand on.
    let context = format!(
        "{context}{}",
        crate::susi_core::capture::EvidenceSession::evidence_prompt_for(workspace)
    );
    let mut attempted = HashSet::new();
    let mut ghosts = 0usize;
    for name in providers {
        if !attempted.insert(name.clone()) {
            continue;
        }
        // Resolve before logging: a name the failover order enumerated
        // but no registry can serve (no cap, no wired topic) is a ghost,
        // not a provider — skip it without a "Trying" line.
        let Some(provider) = registry.get_provider(&name) else {
            ghosts += 1;
            continue;
        };
        eprintln!("[FAILOVER] Trying cloud {name}");
        // Generative missions send the goal verbatim — the recovery-JSON
        // envelope biases evidence-free chat goals toward "failed" and
        // demands strict JSON small local models cannot reliably emit.
        let prompt = if generative {
            report.goal.clone()
        } else {
            recovery_prompt(&report.goal, &context)
        };
        let result = tokio::time::timeout(timeout, async {
            let raw = provider.generate(&prompt).await?;
            let answer = if generative {
                CloudAnswer {
                    status: CompletionStatus::Complete,
                    answer: serde_json::Value::String(raw),
                }
            } else {
                parse_recovery_answer(&raw)?
            };
            verify_recovery_answer(
                report, &name, answer, &context, registry, workspace, generative,
            )
            .await
        })
        .await;
        match result {
            Ok(Ok((answer, evidence))) => {
                record_attempt(
                    report,
                    &name,
                    "CLOUD_ATTEMPT_VERIFIED",
                    evidence.render_for_gemi(),
                );
                report.status = "COMPLETE".into();
                report.final_answer = answer;
                return;
            }
            Ok(Err(error)) => {
                // A registry ghost — a provider name the failover order
                // enumerated but no live process serves (stale cap file,
                // dead owner, unswept rendezvous) — is not a provider
                // attempt. A failed mission otherwise logs hundreds of
                // instant "no handler" failures (observed: 547 ghost
                // providers from a dead rediscovery sweep), drowning the
                // real attempts in the report.
                let msg = error.to_string();
                if msg.contains("no handler for topic") || msg.contains("no longer available") {
                    ghosts += 1;
                } else {
                    record_attempt(report, &name, "CLOUD_ATTEMPT_FAILED", msg)
                }
            }
            Err(_) => record_attempt(
                report,
                &name,
                "CLOUD_ATTEMPT_FAILED",
                "Provider attempt timed out".into(),
            ),
        }
    }
    if ghosts > 0 {
        eprintln!("[FAILOVER] skipped {ghosts} ghost provider registration(s) — no live owner");
    }

    // Last resort: local GGUF / llamacpp path (skips cloud escalation).
    // Generative missions that named a provider must not silently
    // substitute a different local model — the response is labeled with
    // the requested model, so the fallback would mislabel the generator.
    if !fallback_local || (generative && hint_is_provider) {
        report.final_answer.push_str(&format!(
            "\nFailover exhausted {} cloud provider(s); mission remains failed. See attempt details in the mission trace.",
            attempted.len().saturating_sub(ghosts)
        ));
        return;
    }

    eprintln!("[FAILOVER] Trying local inference");
    let prompt = if generative {
        report.goal.clone()
    } else {
        recovery_prompt(&report.goal, &context)
    };
    let workspace_owned = workspace.to_path_buf();
    // A provider-name hint was already served by the cloud loop above; only
    // a non-provider hint is a local model name to load here.
    let local_model = if hint_is_provider { None } else { model_hint };
    let local_model_name = local_model.unwrap_or("local").to_string();
    let local = tokio::time::timeout(timeout, async {
        let raw = tokio::task::spawn_blocking(move || {
            crate::susi_core::plane_bus::gemi::GemiEngine::generate_reasoning_deep_with_model(
                &prompt,
                &workspace_owned,
                &local_model_name,
            )
        })
        .await
        .map_err(|e| EaiError::internal(format!("local recovery worker failed: {e}")))?;
        if raw.trim().is_empty() || raw.contains("[FAIL]") {
            return Err(EaiError::inference(format!(
                "Local inference produced no usable recovery answer: {}",
                raw.chars().take(200).collect::<String>()
            )));
        }
        let answer = if generative {
            CloudAnswer {
                status: CompletionStatus::Complete,
                answer: serde_json::Value::String(raw),
            }
        } else {
            parse_recovery_answer(&raw)?
        };
        verify_recovery_answer(
            report, "local", answer, &context, registry, workspace, generative,
        )
        .await
    })
    .await;

    match local {
        Ok(Ok((answer, evidence))) => {
            record_attempt(
                report,
                "local",
                "LOCAL_ATTEMPT_VERIFIED",
                evidence.render_for_gemi(),
            );
            report.status = "COMPLETE".into();
            report.final_answer = answer;
            return;
        }
        Ok(Err(error)) => {
            record_attempt(report, "local", "LOCAL_ATTEMPT_FAILED", error.to_string())
        }
        Err(_) => record_attempt(
            report,
            "local",
            "LOCAL_ATTEMPT_FAILED",
            "Local recovery attempt timed out".into(),
        ),
    }

    report.final_answer.push_str(&format!(
        "\nFailover exhausted {} cloud provider(s) and local inference; mission remains failed. See attempt details in the mission trace.",
        attempted.len().saturating_sub(ghosts)
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::susi_core::provider::{BoxFuture, Provider};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};

    /// The real `gemi.infer.verify` handler is a fast risk-pattern guard
    /// wired by the daemon composition root; in this unit-test binary the
    /// bus is unwired, so the governance check fails every recovery attempt
    /// before the truth gates under test ever run. Stub it as a pass-through
    /// (what the guard returns for text without risk patterns).
    fn wire_verify_stub() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            struct VerifyStub;
            impl crate::susi_core::plane_bus::PlaneHandler for VerifyStub {
                fn handle(
                    &self,
                    _topic: &str,
                    payload: serde_json::Value,
                ) -> Result<serde_json::Value, String> {
                    let text = payload
                        .get("text")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    Ok(serde_json::json!({ "text": text }))
                }
            }
            crate::susi_core::plane_bus::PlaneBus::global().register(
                crate::susi_core::plane_bus::topics::GEMI_INFER_VERIFY,
                Arc::new(VerifyStub),
            );
        });
    }

    struct TempWorkspace(std::path::PathBuf);
    impl TempWorkspace {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "susi-recovery-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for TempWorkspace {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[derive(Clone, Copy)]
    enum Reply {
        Error,
        Text(&'static str),
        /// Answer by citing the first receipt id advertised in the prompt.
        Cite,
        Hang,
    }
    struct MockProvider {
        name: &'static str,
        reply: Reply,
        calls: Arc<Mutex<Vec<String>>>,
    }
    impl Provider for MockProvider {
        fn name(&self) -> &str {
            self.name
        }
        fn is_healthy(&self) -> BoxFuture<'_, crate::susi_core::susi_error::EaiResult<bool>> {
            Box::pin(async { Ok(true) })
        }
        fn generate(
            &self,
            prompt: &str,
        ) -> BoxFuture<'_, crate::susi_core::susi_error::EaiResult<String>> {
            let prompt = prompt.to_string();
            Box::pin(async move {
                if matches!(self.reply, Reply::Error) {
                    self.calls.lock().unwrap().push(self.name.into());
                    return Err(crate::susi_core::susi_error::EaiError::process(
                        "HTTP 402 Payment Required",
                    ));
                }
                // Model review prompts are never absolute proof — ignore them.
                if prompt.starts_with("You are the SUSI Truth Transformer") {
                    return Ok("VERIFIED".into());
                }
                self.calls.lock().unwrap().push(self.name.into());
                match self.reply {
                    Reply::Text(text) => Ok(text.into()),
                    Reply::Cite => {
                        let Some(start) = prompt.find("\"id\":\"") else {
                            return Ok(
                                r#"{"status":"failed","answer":"no receipts advertised"}"#.into()
                            );
                        };
                        let rest = &prompt[start + "\"id\":\"".len()..];
                        let id = rest.split('"').next().unwrap_or("");
                        Ok(format!(
                            r#"{{"status":"complete","answer":{{"citations":[{{"receipt_id":"{id}"}}]}}}}"#
                        ))
                    }
                    Reply::Hang => std::future::pending().await,
                    Reply::Error => unreachable!(),
                }
            })
        }
        fn embed(
            &self,
            _: &str,
        ) -> BoxFuture<'_, crate::susi_core::susi_error::EaiResult<Vec<f32>>> {
            Box::pin(async { Ok(vec![]) })
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }
    fn report() -> SusiMissionReport {
        SusiMissionReport {
            goal: "Explain the observed result".into(),
            status: "FAILED".into(),
            agents: vec![],
            interactions: vec![],
            final_answer: "Original provider unavailable".into(),
        }
    }
    fn providers(
        entries: &[(&'static str, Reply)],
    ) -> (CapabilityRegistry, Arc<Mutex<Vec<String>>>) {
        let registry = CapabilityRegistry::new();
        let calls = Arc::new(Mutex::new(vec![]));
        for &(name, reply) in entries {
            registry.register_provider(MockProvider {
                name,
                reply,
                calls: Arc::clone(&calls),
            });
        }
        (registry, calls)
    }
    const COMPLETE: &str = r#"{"status":"complete","answer":"The observed result is available."}"#;

    #[tokio::test]
    async fn model_hint_moves_named_provider_to_front() {
        let ws = TempWorkspace::new();
        let (registry, calls) = providers(&[
            ("a-first", Reply::Error),
            ("b-hinted", Reply::Text(COMPLETE)),
        ]);
        let mut report = report();
        recover_with_providers(
            &mut report,
            &registry,
            vec!["a-first".into(), "b-hinted".into()],
            &ws.0,
            Duration::from_secs(1),
            false,
            Some("b-hinted"),
            false,
        )
        .await;
        // The hinted provider ran first even though it was listed last.
        assert_eq!(
            calls.lock().unwrap().first().map(String::as_str),
            Some("b-hinted")
        );
    }

    #[tokio::test]
    async fn model_agreement_without_evidence_never_completes_recovery() {
        let ws = TempWorkspace::new();
        let (registry, calls) = providers(&[
            ("a-down", Reply::Error),
            ("b-working", Reply::Text(COMPLETE)),
            ("c-unused", Reply::Text(COMPLETE)),
        ]);
        let mut report = report();
        recover_with_providers(
            &mut report,
            &registry,
            vec!["a-down".into(), "b-working".into(), "c-unused".into()],
            &ws.0,
            Duration::from_secs(1),
            false,
            None,
            false,
        )
        .await;
        assert!(!report.is_success());
        assert_eq!(*calls.lock().unwrap(), ["a-down", "b-working", "c-unused"]);
        assert!(report
            .interactions
            .iter()
            .any(|entry| entry.action == "CLOUD_ATTEMPT_FAILED" && entry.payload.contains("402")));
        assert!(report
            .interactions
            .iter()
            .all(|entry| entry.action != "CLOUD_ATTEMPT_VERIFIED"));
        assert!(report.final_answer.contains("mission remains failed"));
    }

    #[tokio::test]
    async fn failed_malformed_and_ungrounded_answers_advance_to_next_provider() {
        wire_verify_stub();
        let ws = TempWorkspace::new();
        let (registry, calls) = providers(&[
            (
                "a-missing",
                Reply::Text(r#"{"status":"failed","answer":"No live weather evidence"}"#),
            ),
            ("b-malformed", Reply::Text("MISSION COMPLETE")),
            (
                "c-ungrounded",
                Reply::Text(r#"{"status":"complete","answer":"ungrounded answer"}"#),
            ),
            ("d-good", Reply::Text(COMPLETE)),
        ]);
        let mut report = report();
        recover_with_providers(
            &mut report,
            &registry,
            vec![
                "a-missing".into(),
                "b-malformed".into(),
                "c-ungrounded".into(),
                "d-good".into(),
            ],
            &ws.0,
            Duration::from_secs(1),
            false,
            None,
            false,
        )
        .await;
        assert!(!report.is_success());
        assert_eq!(calls.lock().unwrap().len(), 4);
        assert!(report.interactions.iter().any(|entry| {
            entry.sender == "c-ungrounded" && entry.payload.contains("TRUTH_UNVERIFIED")
        }));
        assert!(report.interactions.iter().any(|entry| {
            entry.sender == "d-good" && entry.payload.contains("no absolute evidence")
        }));
    }

    #[tokio::test]
    async fn exhausted_providers_remain_failed_and_duplicates_are_not_retried() {
        let ws = TempWorkspace::new();
        let (registry, calls) = providers(&[("down", Reply::Error)]);
        let mut report = report();
        recover_with_providers(
            &mut report,
            &registry,
            vec!["down".into(), "down".into()],
            &ws.0,
            Duration::from_secs(1),
            false,
            None,
            false,
        )
        .await;
        assert!(!report.is_success());
        assert_eq!(*calls.lock().unwrap(), ["down"]);
        assert!(report.final_answer.contains("exhausted 1 cloud provider"));
        assert_eq!(report.exit_code(), std::process::ExitCode::FAILURE);
    }

    #[tokio::test]
    async fn timed_out_attempt_advances_to_working_provider() {
        let ws = TempWorkspace::new();
        let (registry, calls) =
            providers(&[("a-hung", Reply::Hang), ("b-good", Reply::Text(COMPLETE))]);
        let mut report = report();
        recover_with_providers(
            &mut report,
            &registry,
            vec!["a-hung".into(), "b-good".into()],
            &ws.0,
            Duration::from_millis(20),
            false,
            None,
            false,
        )
        .await;
        assert!(!report.is_success());
        assert_eq!(*calls.lock().unwrap(), ["a-hung", "b-good"]);
        assert!(report.interactions[0].payload.contains("timed out"));
    }

    #[tokio::test]
    async fn successful_and_governance_blocked_missions_are_not_retried() {
        let ws = TempWorkspace::new();
        let (registry, calls) = providers(&[("unused", Reply::Text(COMPLETE))]);
        for status in ["COMPLETE", "BLOCKED", "ABORTED"] {
            let mut report = report();
            report.status = status.into();
            recover_with_providers(
                &mut report,
                &registry,
                vec!["unused".into()],
                &ws.0,
                Duration::from_secs(1),
                false,
                None,
                false,
            )
            .await;
            assert_eq!(report.status, status);
        }
        let mut report = report();
        report.final_answer = "[GOVERNANCE_BLOCK] Forbidden action".into();
        recover_with_providers(
            &mut report,
            &registry,
            vec!["unused".into()],
            &ws.0,
            Duration::from_secs(1),
            false,
            None,
            false,
        )
        .await;
        assert!(calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn citation_answers_complete_recovery_through_the_ledger() {
        wire_verify_stub();
        let ws = TempWorkspace::new();
        let session = crate::susi_core::capture::EvidenceSession::new(
            "Explain the observed result",
            &ws.0,
            |s| s.to_string(),
        )
        .unwrap();
        let _activation = crate::susi_core::capture::EvidenceSession::activate(&session);
        crate::susi_core::capture::EvidenceSession::capture_call(
            "open_meteo_weather",
            &serde_json::json!({"place": "Chennai"}),
            &ws.0,
            || Ok("24.8C overcast observed".to_string()),
        )
        .unwrap();

        let (registry, calls) = providers(&[("a-cites", Reply::Cite)]);
        let mut report = report();
        recover_with_providers(
            &mut report,
            &registry,
            vec!["a-cites".into()],
            &ws.0,
            Duration::from_secs(1),
            false,
            None,
            false,
        )
        .await;
        assert!(report.is_success(), "{}", report.final_answer);
        assert!(report.final_answer.contains("24.8C overcast observed"));
        assert!(report
            .interactions
            .iter()
            .any(|entry| entry.action == "CLOUD_ATTEMPT_VERIFIED"));
        assert_eq!(*calls.lock().unwrap(), ["a-cites"]);
    }

    /// Generative missions (`/v1/chat/completions`) produce no tool receipts
    /// to cite — the provider's output is the product. The recovery gate
    /// accepts it via a self-cited inference receipt minted for the call.
    #[tokio::test]
    async fn generative_mission_self_cites_inference_receipt() {
        wire_verify_stub();
        let ws = TempWorkspace::new();
        let session =
            crate::susi_core::capture::EvidenceSession::new("Reply with exactly: ok", &ws.0, |s| {
                s.to_string()
            })
            .unwrap();
        let _activation = crate::susi_core::capture::EvidenceSession::activate(&session);

        let (registry, calls) = providers(&[("a-gen", Reply::Text(COMPLETE))]);
        let mut report = report();
        recover_with_providers(
            &mut report,
            &registry,
            vec!["a-gen".into()],
            &ws.0,
            Duration::from_secs(1),
            false,
            None,
            true, // generative
        )
        .await;
        assert!(report.is_success(), "{}", report.final_answer);
        assert!(report
            .final_answer
            .contains("The observed result is available."));
        assert_eq!(*calls.lock().unwrap(), ["a-gen"]);
    }

    /// The promotion is scoped to generative missions: the same prose
    /// answer on a normal mission still fails the absolute gate.
    #[tokio::test]
    async fn non_generative_prose_answer_still_fails_without_receipts() {
        wire_verify_stub();
        let ws = TempWorkspace::new();
        let session = crate::susi_core::capture::EvidenceSession::new(
            "Explain the observed result",
            &ws.0,
            |s| s.to_string(),
        )
        .unwrap();
        let _activation = crate::susi_core::capture::EvidenceSession::activate(&session);

        let (registry, _calls) = providers(&[("a-prose", Reply::Text(COMPLETE))]);
        let mut report = report();
        recover_with_providers(
            &mut report,
            &registry,
            vec!["a-prose".into()],
            &ws.0,
            Duration::from_secs(1),
            false,
            None,
            false,
        )
        .await;
        assert!(!report.is_success(), "{}", report.final_answer);
    }

    #[test]
    fn parse_recovery_answer_extracts_json_from_prose() {
        let raw = "Sure — here you go:\n{\"status\":\"failed\",\"answer\":\"No live weather evidence\"}\nHope that helps.";
        let parsed = parse_recovery_answer(raw).expect("should extract JSON object");
        assert!(matches!(parsed.status, CompletionStatus::Failed));
        assert!(answer_text(&parsed.answer).contains("No live weather"));
    }

    #[test]
    fn parse_recovery_answer_accepts_fenced_json() {
        let raw = "```json\n{\"status\":\"complete\",\"answer\":\"ok\"}\n```";
        let parsed = parse_recovery_answer(raw).unwrap();
        assert!(matches!(parsed.status, CompletionStatus::Complete));
        assert_eq!(answer_text(&parsed.answer), "ok");
    }
}
