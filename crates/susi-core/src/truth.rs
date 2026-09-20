// Checks tool-reported results against actual workspace state (e.g. a
// claimed file write that never happened) before a mission treats the
// result as fact.

use crate::evidence::EvidenceSource;
use crate::registry::CapabilityRegistry;
use std::path::Path;

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

    /// Primary entrypoint for the Verification Pipeline:
    /// Takes a structured EvidenceRecord from a capability/agent and verifies
    /// its cryptographic signature and grounding against the physical workspace.
    pub fn verify_evidence(
        record: &crate::evidence::EvidenceRecord,
        workspace: &Path,
    ) -> EaiResult<()> {
        if !record.verify_reality(workspace) {
            let error_msg = format!(
                "TRUTH_VIOLATION: Evidence record from agent '{}' failed reality check against workspace.\nCLAIM: {} {} {}",
                record.agent_id, record.claim.subject, record.claim.predicate, record.claim.value
            );
            return Err(EaiError::governance(error_msg));
        }
        Ok(())
    }

    /// Ultimate Epistemic Validator: Universal Hallucination Detector
    /// Dispatches to deterministic rule-engines for physical claims, and calls
    /// a 'verifier' Model Provider via the CapabilityRegistry for semantic claims.
    pub async fn cross_examine(
        record: &crate::evidence::EvidenceRecord,
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
            // Find an external provider to critique the agent's logic
            // Prefers "sglang" or "vllm" if they exist as they are fast structured verifiers, else uses whatever is registered
            let mut verifier_provider = None;
            for name in registry.list_providers() {
                if name.contains("sglang") || name.contains("vllm") || name.contains("ollama") {
                    verifier_provider = registry.get_provider(&name);
                    break;
                }
            }

            // Fallback to the first available provider
            let verifier_provider = match verifier_provider {
                Some(p) => p,
                None => {
                    let providers = registry.list_providers();
                    if providers.is_empty() {
                        // Can't run semantic checks without a provider, pass by default
                        return Ok(());
                    }
                    match registry.get_provider(&providers[0]) {
                        Some(p) => p,
                        None => return Ok(()), // raced out of registry; skip semantic check
                    }
                }
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
}
