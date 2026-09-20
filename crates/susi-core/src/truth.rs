use crate::evidence::{EvidenceRecord, EvidenceSource};
use crate::registry::CapabilityRegistry;
use std::path::Path;
use std::sync::OnceLock;

use susi_error::{EaiError, EaiResult};

pub struct SusiTruthAgent;

impl SusiTruthAgent {
    pub fn verify_swarm_reality(
        goal: &str,
        tool_name: &str,
        result: &str,
        workspace: &Path,
    ) -> EaiResult<String> {
        Self::verify_mission_reality(goal, tool_name, result, workspace)
    }

    /// Checks a tool result's claims against the workspace on disk.
    pub fn verify_mission_reality(
        _goal: &str,
        _tool_name: &str,
        result: &str,
        workspace: &Path,
    ) -> EaiResult<String> {
        let mut violations = Vec::new();

        // Rather than switching on tool name, we look for phrases in the
        // result text that imply a filesystem write ("Wrote to ", "Saved to ").

        // Check: did a claimed file write actually happen?
        if result.contains("Wrote to ") || result.contains("Saved to ") {
            let mut found_path = false;
            let parts: Vec<&str> = result.split([' ', '[', ']']).collect();
            for part in parts {
                let path_candidate =
                    part.trim_matches(|c| c == '.' || c == ':' || c == '[' || c == ']');
                if (path_candidate.contains('/') || path_candidate.contains('.'))
                    && !path_candidate.is_empty()
                {
                    let target_path = workspace.join(path_candidate);
                    found_path = true;
                    if !target_path.exists() {
                        violations.push(format!("Reality Mismatch: Resource '{}' reported as written but does not exist in workspace.", path_candidate));
                    } else if let Ok(m) = target_path.metadata() {
                        if m.len() == 0 && !result.to_lowercase().contains("empty") {
                            violations.push(format!("Reality Mismatch: Resource '{}' exists but is empty (0 bytes). Result claimed success.", path_candidate));
                        }
                    }
                    break;
                }
            }
            if !found_path && (result.contains("Wrote to") || result.contains("Saved to")) {
                violations.push("Reality Mismatch: Tool reported writing a file but no valid path could be extracted for verification.".to_string());
            }
        }

        if !violations.is_empty() {
            let error_msg = format!("TRUTH_VIOLATION: {}\nSTRUCTURED_FEEDBACK: Please grounded your response in the physical workspace state. Ensure files are actually written before reporting success.", violations.join(" | "));
            return Err(EaiError::governance(error_msg));
        }

        Ok(result.to_string())
    }
}

pub struct TruthTransformer;

impl TruthTransformer {
    /// Legacy entrypoint for raw text strings.
    pub fn verify_mission_reality(
        goal: &str,
        tool_name: &str,
        result: &str,
        workspace: &Path,
    ) -> EaiResult<String> {
        SusiTruthAgent::verify_mission_reality(goal, tool_name, result, workspace)
    }

    /// Dual pipeline for swarm finals: physical reality check, then semantic
    /// cross-examination via discovered CapabilityRegistry providers.
    pub fn verify_mission_with_cross_examine(
        goal: &str,
        tool_name: &str,
        result: &str,
        workspace: &Path,
    ) -> EaiResult<String> {
        let verified = Self::verify_mission_reality(goal, tool_name, result, workspace)?;
        let record = Self::mission_evidence_record(goal, tool_name, &verified);
        Self::cross_examine_sync(&record, workspace)?;
        Ok(verified)
    }

    /// Build an AgentObservation evidence record from a mission result string.
    pub fn mission_evidence_record(goal: &str, agent_id: &str, result: &str) -> EvidenceRecord {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        EvidenceRecord::new(
            agent_id.to_string(),
            1.0,
            now,
            crate::evidence::Claim {
                subject: goal.chars().take(120).collect(),
                predicate: "mission_result".to_string(),
                value: result.chars().take(240).collect(),
            },
            EvidenceSource::AgentObservation {
                observation: result.chars().take(500).collect(),
                reasoning_trace: result.to_string(),
            },
            0.9,
        )
    }

    /// Primary entrypoint for the Verification Pipeline:
    /// Takes a structured EvidenceRecord from a capability/agent and verifies
    /// its cryptographic signature and grounding against the physical workspace.
    pub fn verify_evidence(record: &EvidenceRecord, workspace: &Path) -> EaiResult<()> {
        if !record.verify_reality(workspace) {
            let error_msg = format!(
                "TRUTH_VIOLATION: Evidence record from agent '{}' failed reality check against workspace.\nCLAIM: {} {} {}",
                record.agent_id, record.claim.subject, record.claim.predicate, record.claim.value
            );
            return Err(EaiError::governance(error_msg));
        }
        Ok(())
    }

    /// Sync bridge for swarm/DAG callers into async [`cross_examine`], using the
    /// process-wide CapabilityRegistry populated by zero-config discovery.
    pub fn cross_examine_sync(record: &EvidenceRecord, workspace: &Path) -> EaiResult<()> {
        Self::cross_examine_blocking(record, CapabilityRegistry::global(), workspace)
    }

    fn verifier_runtime() -> Option<&'static tokio::runtime::Runtime> {
        static RT: OnceLock<std::io::Result<tokio::runtime::Runtime>> = OnceLock::new();
        RT.get_or_init(tokio::runtime::Runtime::new).as_ref().ok()
    }

    pub fn cross_examine_blocking(
        record: &EvidenceRecord,
        registry: &CapabilityRegistry,
        workspace: &Path,
    ) -> EaiResult<()> {
        let Some(runtime) = Self::verifier_runtime() else {
            // No runtime available: keep physical check only.
            return Self::verify_evidence(record, workspace);
        };
        runtime.block_on(Self::cross_examine(record, registry, workspace))
    }

    fn select_verifier_provider(
        registry: &CapabilityRegistry,
    ) -> Option<std::sync::Arc<dyn crate::provider::Provider>> {
        let mut names: Vec<String> = registry
            .list_providers()
            .into_iter()
            // Candle delegates into GemiEngine and would recurse under swarm load.
            .filter(|n| n != "Candle (Local)")
            .collect();
        if names.is_empty() {
            return None;
        }
        names.sort_by_key(|n| {
            let lower = n.to_ascii_lowercase();
            let rank = if lower.contains("sglang") {
                0u8
            } else if lower.contains("vllm") {
                1
            } else if lower.contains("ollama") {
                2
            } else {
                10
            };
            (rank, n.clone())
        });
        names.into_iter().find_map(|n| registry.get_provider(&n))
    }

    /// Ultimate Epistemic Validator: Universal Hallucination Detector
    /// Dispatches to deterministic rule-engines for physical claims, and calls
    /// a 'verifier' Model Provider via the CapabilityRegistry for semantic claims.
    pub async fn cross_examine(
        record: &EvidenceRecord,
        registry: &CapabilityRegistry,
        workspace: &Path,
    ) -> EaiResult<()> {
        // 1. Physical / Deterministic Grounding (File existences, MCP responses)
        if !record.verify_reality(workspace) {
            let error_msg = format!(
                "TRUTH_VIOLATION [DETERMINISTIC]: Evidence record from '{}' failed physical reality check.\nCLAIM: {} {} {}",
                record.agent_id, record.claim.subject, record.claim.predicate, record.claim.value
            );
            return Err(EaiError::governance(error_msg));
        }

        // 2. Semantic Cross-Examination for abstract or complex agent observations
        if let EvidenceSource::AgentObservation {
            observation: _,
            reasoning_trace,
        } = &record.source
        {
            let Some(verifier_provider) = Self::select_verifier_provider(registry) else {
                // Can't run semantic checks without a provider, pass by default
                return Ok(());
            };

            let prompt = format!(
                "You are the SUSI Truth Transformer, an epistemic validator designed to detect hallucinations, lies, and empty claims.\n\nEVIDENCE TRACE:\n{}\n\nAGENT CLAIM TO VERIFY:\n{} {} {}\n\nAssess if the reasoning explicitly supports the claim without hallucinating unobserved facts. If it is a hallucination or an empty claim, respond with ONLY the word 'HALLUCINATION'. If it is factually grounded, respond with ONLY the word 'VERIFIED'.",
                reasoning_trace, record.claim.subject, record.claim.predicate, record.claim.value
            );

            let verdict = verifier_provider.generate(&prompt).await?;
            if verdict.trim().to_uppercase().contains("HALLUCINATION") {
                let error_msg = format!(
                    "TRUTH_VIOLATION [SEMANTIC]: Verification engine '{}' detected a hallucinated or empty claim from agent '{}'.\nCLAIM: {} {} {}",
                    verifier_provider.name(), record.agent_id, record.claim.subject, record.claim.predicate, record.claim.value
                );
                return Err(EaiError::governance(error_msg));
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evidence::{Claim, EvidenceRecord, EvidenceSource};
    use crate::provider::{BoxFuture, Provider};
    use sha2::{Digest, Sha256};

    #[test]
    fn test_verify_evidence_pipeline() {
        let tmp = std::env::temp_dir().join("susi_test_verify_evidence");
        let _ = std::fs::create_dir_all(&tmp);
        let file_path = tmp.join("evidence.txt");
        let content = b"verified reality";
        std::fs::write(&file_path, content).unwrap();

        let mut hasher = Sha256::new();
        hasher.update(content);
        let hash = hex::encode(hasher.finalize());

        let record = EvidenceRecord::new(
            "agent-007".to_string(),
            1.0,
            1234567890,
            Claim {
                subject: "evidence.txt".to_string(),
                predicate: "contains".to_string(),
                value: "verified reality".to_string(),
            },
            EvidenceSource::File {
                path: std::path::PathBuf::from("evidence.txt"),
                hash,
            },
            0.95,
        );

        assert!(TruthTransformer::verify_evidence(&record, &tmp).is_ok());

        // Test failure on nonexistent file
        let bad_record = EvidenceRecord::new(
            "agent-007".to_string(),
            1.0,
            1234567890,
            Claim {
                subject: "missing.txt".to_string(),
                predicate: "exists".to_string(),
                value: "true".to_string(),
            },
            EvidenceSource::File {
                path: std::path::PathBuf::from("missing.txt"),
                hash: "fakehash".to_string(),
            },
            0.95,
        );

        let err = TruthTransformer::verify_evidence(&bad_record, &tmp).unwrap_err();
        assert!(err.to_string().contains("TRUTH_VIOLATION"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_mcp_empty_claim_is_rejected_deterministically() {
        let tmp = std::env::temp_dir().join("susi_test_mcp_reality");
        let _ = std::fs::create_dir_all(&tmp);

        // An agent hallucinates that it fetched a web page but actually got an MCP error
        let bad_mcp_record = EvidenceRecord::new(
            "web-agent".to_string(),
            1.0,
            1234567890,
            Claim {
                subject: "web_search".to_string(),
                predicate: "found".to_string(),
                value: "React documentation".to_string(),
            },
            EvidenceSource::McpTool {
                tool_name: "brave_search".to_string(),
                raw_response: "Error: API rate limit exceeded".to_string(),
            },
            0.95,
        );

        let err = TruthTransformer::verify_evidence(&bad_mcp_record, &tmp).unwrap_err();
        assert!(err.to_string().contains("TRUTH_VIOLATION"));

        // A valid MCP claim
        let good_mcp_record = EvidenceRecord::new(
            "web-agent".to_string(),
            1.0,
            1234567890,
            Claim {
                subject: "web_search".to_string(),
                predicate: "found".to_string(),
                value: "Rust documentation".to_string(),
            },
            EvidenceSource::McpTool {
                tool_name: "brave_search".to_string(),
                raw_response: "{ results: ['Rust is a systems language...'] }".to_string(),
            },
            0.95,
        );

        assert!(TruthTransformer::verify_evidence(&good_mcp_record, &tmp).is_ok());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    struct MockTruthProvider {
        detect_hallucination: bool,
    }

    impl Provider for MockTruthProvider {
        fn name(&self) -> &str {
            "Mock Verifier"
        }
        fn is_healthy(&self) -> BoxFuture<'_, EaiResult<bool>> {
            Box::pin(async { Ok(true) })
        }
        fn generate(&self, _prompt: &str) -> BoxFuture<'_, EaiResult<String>> {
            let res = if self.detect_hallucination {
                "HALLUCINATION"
            } else {
                "VERIFIED"
            };
            Box::pin(async move { Ok(res.to_string()) })
        }
        fn embed(&self, _text: &str) -> BoxFuture<'_, EaiResult<Vec<f32>>> {
            Box::pin(async { Ok(vec![]) })
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    #[tokio::test]
    async fn test_semantic_cross_examination() {
        let registry = CapabilityRegistry::new();
        // Register a provider that WILL flag hallucinations
        registry.register_provider(MockTruthProvider {
            detect_hallucination: true,
        });

        let tmp = std::env::temp_dir().join("susi_test_semantic");
        let _ = std::fs::create_dir_all(&tmp);

        let hallucinated_record = EvidenceRecord::new(
            "rogue-agent".to_string(),
            1.0,
            1234567890,
            Claim {
                subject: "Quantum Gravity".to_string(),
                predicate: "solved by".to_string(),
                value: "the susi framework".to_string(),
            },
            EvidenceSource::AgentObservation {
                observation: "I read a blog post".to_string(),
                reasoning_trace:
                    "susi has a truth transformer, therefore it solved quantum gravity.".to_string(),
            },
            0.95,
        );

        let result = TruthTransformer::cross_examine(&hallucinated_record, &registry, &tmp).await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("TRUTH_VIOLATION [SEMANTIC]"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_verify_mission_with_cross_examine_passes_without_providers() {
        let tmp = std::env::temp_dir().join("susi_test_dual_pipeline_no_provider");
        let _ = std::fs::create_dir_all(&tmp);
        let out = TruthTransformer::verify_mission_with_cross_examine(
            "list files",
            "SUSI_SOLVE",
            "Here is a safe plan to list files.",
            &tmp,
        )
        .unwrap();
        assert!(out.contains("list files"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_cross_examine_blocking_flags_hallucination() {
        let registry = CapabilityRegistry::new();
        registry.register_provider(MockTruthProvider {
            detect_hallucination: true,
        });
        let tmp = std::env::temp_dir().join("susi_test_cross_examine_blocking");
        let _ = std::fs::create_dir_all(&tmp);
        let record = TruthTransformer::mission_evidence_record(
            "prove P=NP",
            "rogue",
            "I invented a polynomial-time algorithm for SAT in my head.",
        );
        let err = TruthTransformer::cross_examine_blocking(&record, &registry, &tmp).unwrap_err();
        assert!(err.to_string().contains("TRUTH_VIOLATION [SEMANTIC]"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_select_verifier_skips_candle() {
        let registry = CapabilityRegistry::new();
        registry.register_provider(MockTruthProvider {
            detect_hallucination: false,
        });
        // Re-register under Candle name via a thin wrapper isn't needed —
        // empty non-candle set with only Candle should yield None.
        let candle_only = CapabilityRegistry::new();
        struct CandleNamed;
        impl Provider for CandleNamed {
            fn name(&self) -> &str {
                "Candle (Local)"
            }
            fn is_healthy(&self) -> BoxFuture<'_, EaiResult<bool>> {
                Box::pin(async { Ok(true) })
            }
            fn generate(&self, _: &str) -> BoxFuture<'_, EaiResult<String>> {
                Box::pin(async { Ok("VERIFIED".into()) })
            }
            fn embed(&self, _: &str) -> BoxFuture<'_, EaiResult<Vec<f32>>> {
                Box::pin(async { Ok(vec![]) })
            }
            fn as_any(&self) -> &dyn std::any::Any {
                self
            }
        }
        candle_only.register_provider(CandleNamed);
        assert!(TruthTransformer::select_verifier_provider(&candle_only).is_none());
        assert!(TruthTransformer::select_verifier_provider(&registry).is_some());
    }
}
