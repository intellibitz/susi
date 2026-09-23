//! Bounded recovery of failed missions using discovered cloud providers.

use crate::ama::SusiMissionReport;
use crate::amas::A2AMessage;
use serde::Deserialize;
use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;
use susi_core::evidence::{Claim, EvidenceRecord, EvidenceSource};
use susi_core::registry::CapabilityRegistry;
use susi_core::truth::TruthTransformer;
use susi_error::{EaiError, EaiResult};
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
        other => other.to_string(),
    }
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
pub(crate) fn recover(report: &mut SusiMissionReport, workspace: &Path) {
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
    susi_core::plane_bus::gemi::register_configured_cloud_endpoints();
    let order = susi_core::plane_bus::gemi::cloud_failover_order();
    let providers: Vec<String> = order
        .get("order")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
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
) -> EaiResult<(String, EvidenceRecord)> {
    let answer_text = answer_text(&answer.answer);
    if !matches!(answer.status, CompletionStatus::Complete) {
        return Err(EaiError::inference(answer_text));
    }
    // Crown gate: citations resolve from the ledger; if receipts exist and
    // were not cited, fail — never promote narrative over captured evidence.
    if let Some(resolved) =
        susi_core::capture::EvidenceSession::verify_answer(&answer_text, workspace)
    {
        let rendered = resolved?;
        if !susi_gawd_agents::accountability::is_usable(&rendered) {
            return Err(EaiError::inference(
                "Provider cited receipts that resolve to unusable evidence",
            ));
        }
        susi_core::plane_bus::gemi::GemiEngine::verify_axiomatic_alignment(&rendered, workspace)
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
    susi_core::plane_bus::gemi::GemiEngine::verify_axiomatic_alignment(&answer_text, workspace)
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
) {
    if !eligible(report) {
        return;
    }
    let context = SecurityDetector::redact(&serde_json::to_string(report).unwrap_or_default());
    let context: String = context.chars().take(24_000).collect();
    // Receipts captured during the failed mission are the only evidence a
    // recovery answer is allowed to stand on.
    let context = format!(
        "{context}{}",
        susi_core::capture::EvidenceSession::evidence_prompt_for(workspace)
    );
    let mut attempted = HashSet::new();
    for name in providers {
        if !attempted.insert(name.clone()) {
            continue;
        }
        eprintln!("[FAILOVER] Trying cloud {name}");
        let prompt = recovery_prompt(&report.goal, &context);
        let result = tokio::time::timeout(timeout, async {
            let provider = registry
                .get_provider(&name)
                .ok_or_else(|| EaiError::inference("Provider no longer available"))?;
            let raw = provider.generate(&prompt).await?;
            let answer = parse_recovery_answer(&raw)?;
            verify_recovery_answer(report, &name, answer, &context, registry, workspace).await
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
                record_attempt(report, &name, "CLOUD_ATTEMPT_FAILED", error.to_string())
            }
            Err(_) => record_attempt(
                report,
                &name,
                "CLOUD_ATTEMPT_FAILED",
                "Provider attempt timed out".into(),
            ),
        }
    }

    // Last resort: local GGUF / llamacpp path (skips cloud escalation).
    if !fallback_local {
        report.final_answer.push_str(&format!(
            "\nFailover exhausted {} cloud provider(s); mission remains failed. See attempt details in the mission trace.",
            attempted.len()
        ));
        return;
    }

    eprintln!("[FAILOVER] Trying local inference");
    let prompt = recovery_prompt(&report.goal, &context);
    let workspace_owned = workspace.to_path_buf();
    let local = tokio::time::timeout(timeout, async {
        let raw = tokio::task::spawn_blocking(move || {
            susi_core::plane_bus::gemi::GemiEngine::generate_reasoning_deep_with_model(
                &prompt,
                &workspace_owned,
                "local",
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
        let answer = parse_recovery_answer(&raw)?;
        verify_recovery_answer(report, "local", answer, &context, registry, workspace).await
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
        attempted.len()
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use susi_core::provider::{BoxFuture, Provider};

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
        fn is_healthy(&self) -> BoxFuture<'_, EaiResult<bool>> {
            Box::pin(async { Ok(true) })
        }
        fn generate(&self, prompt: &str) -> BoxFuture<'_, EaiResult<String>> {
            let prompt = prompt.to_string();
            Box::pin(async move {
                if matches!(self.reply, Reply::Error) {
                    self.calls.lock().unwrap().push(self.name.into());
                    return Err(EaiError::process("HTTP 402 Payment Required"));
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
        fn embed(&self, _: &str) -> BoxFuture<'_, EaiResult<Vec<f32>>> {
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
        )
        .await;
        assert!(calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn citation_answers_complete_recovery_through_the_ledger() {
        let ws = TempWorkspace::new();
        let session =
            susi_core::capture::EvidenceSession::new("Explain the observed result", &ws.0, |s| {
                s.to_string()
            })
            .unwrap();
        let _activation = susi_core::capture::EvidenceSession::activate(&session);
        susi_core::capture::EvidenceSession::capture_call(
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
