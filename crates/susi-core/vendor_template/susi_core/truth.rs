use crate::susi_core::evidence::{EvidenceAssessment, EvidenceRecord, EvidenceSource};
use crate::susi_core::registry::CapabilityRegistry;
use std::path::Path;
use std::sync::OnceLock;

use crate::susi_error::{EaiError, EaiResult};

pub struct SusiTruthAgent;

impl SusiTruthAgent {
    /// Preflight for explicit write claims, not proof of mission completion.
    /// Existence alone does not establish who wrote a file or what changed.
    #[allow(clippy::expect_used)]
    pub fn verify_mission_reality(
        _goal: &str,
        _tool_name: &str,
        result: &str,
        workspace: &Path,
    ) -> EaiResult<String> {
        if result.trim().is_empty() {
            return Err(EaiError::governance("TRUTH_UNVERIFIED: empty result"));
        }
        let mut violations = Vec::new();

        // Inspect every explicit write claim, rather than the first path-looking
        // word anywhere in the result. Quoting supports paths containing spaces.
        static WRITES: OnceLock<regex::Regex> = OnceLock::new();
        // Mandate 42: safe - the pattern is a compile-time string literal,
        // not runtime input; its validity doesn't depend on any value that
        // varies between calls, so a failure here would be caught by any
        // test run, never a runtime condition.
        let writes = WRITES.get_or_init(|| {
            regex::Regex::new(
                r#"(?i)\b(?:wrote to|saved to)(?:[ \t]+(?:`([^`]+)`|"([^"]+)"|'([^']+)'|([^\s]+)))?"#,
            )
            .expect("static write-claim pattern")
        });
        for capture in writes.captures_iter(result) {
            let candidate = (1..=4).find_map(|index| capture.get(index));
            let Some(candidate) = candidate else {
                violations.push("Reality Mismatch: write claim has no verifiable path".to_string());
                continue;
            };
            let path = if capture.get(4).is_some() {
                candidate.as_str().trim_end_matches(['.', ',', ';'])
            } else {
                candidate.as_str()
            };
            match crate::susi_core::evidence::confined_file(workspace, Path::new(path)) {
                Some(_) => {}
                _ => violations.push(format!(
                    "Reality Mismatch: Resource '{}' reported as written but is not an existing regular file.", path
                )),
            }
        }

        if !violations.is_empty() {
            let error_msg = format!("TRUTH_VIOLATION: {}\nSTRUCTURED_FEEDBACK: Please ground your response in the physical workspace state. Ensure files are actually written before reporting success.", violations.join(" | "));
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

    /// Absolute-truth gate for mission finals.
    /// Only a live ledger citation answer can pass here. Compiled binary reads
    /// and native system receipts bypass this entrypoint in the AMA. Model
    /// review is never proof of fact and is not consulted.
    pub fn verify_mission_with_cross_examine(
        goal: &str,
        tool_name: &str,
        result: &str,
        workspace: &Path,
    ) -> EaiResult<String> {
        if let Some(resolved) = crate::susi_core::capture::EvidenceSession::verify_answer(result, workspace) {
            let rendered = resolved?;
            // Resolved ledger text can still claim writes — check the workspace.
            return Self::verify_mission_reality(goal, tool_name, &rendered, workspace)
                .map(|_| rendered);
        }
        Err(EaiError::governance(
            "TRUTH_UNVERIFIED: no absolute evidence — cite live tool receipts, or use a compiled/native verified read path",
        ))
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
            crate::susi_core::evidence::Claim {
                subject: goal.to_string(),
                predicate: "mission_completed".to_string(),
                value: result.to_string(),
            },
            EvidenceSource::AgentObservation {
                observation: result.chars().take(500).collect(),
                reasoning_trace: result.to_string(),
            },
            0.0,
        )
    }

    /// Primary entrypoint for the Verification Pipeline:
    /// Takes a structured EvidenceRecord from a capability/agent and verifies
    /// its integrity checksum and source checks against the physical workspace.
    /// Observation integrity alone does not establish factual truth.
    pub fn verify_evidence(record: &EvidenceRecord, workspace: &Path) -> EaiResult<()> {
        match record.assess(workspace) {
            EvidenceAssessment::Verified => Ok(()),
            EvidenceAssessment::Unverified(reason) => {
                Err(EaiError::governance(format!("TRUTH_UNVERIFIED: {reason}")))
            }
            EvidenceAssessment::Rejected(reason) => {
                Err(EaiError::governance(format!("TRUTH_VIOLATION: {reason}")))
            }
        }
    }

    /// Verify every claim; empty bundles never establish truth. Unsupported or
    /// contradictory records are retained in the report, not dropped or voted out.
    pub fn assess_evidence_bundle(
        records: &[EvidenceRecord],
        workspace: &Path,
    ) -> Vec<EvidenceAssessment> {
        if records.is_empty() {
            return vec![EvidenceAssessment::Unverified(
                "no evidence supplied".into(),
            )];
        }
        records
            .iter()
            .map(|record| record.assess(workspace))
            .collect()
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
            return Err(EaiError::governance(
                "TRUTH_UNVERIFIED: verifier runtime unavailable",
            ));
        };
        // Synchronous tool handlers can also be invoked from a Tokio runtime.
        // Enter the dedicated runtime on another thread to avoid nested block_on.
        if tokio::runtime::Handle::try_current().is_ok() {
            std::thread::scope(|scope| {
                scope
                    .spawn(|| runtime.block_on(Self::cross_examine(record, registry, workspace)))
                    .join()
                    .map_err(|_| {
                        EaiError::governance("TRUTH_UNVERIFIED: verifier worker panicked")
                    })?
            })
        } else {
            runtime.block_on(Self::cross_examine(record, registry, workspace))
        }
    }

    #[cfg(test)]
    fn verifier_providers(
        registry: &CapabilityRegistry,
    ) -> Vec<std::sync::Arc<dyn crate::susi_core::provider::Provider>> {
        let mut names: Vec<String> = registry
            .list_providers()
            .into_iter()
            // Candle delegates into GemiEngine and would recurse under swarm load.
            .filter(|n| n != "Candle (Local)")
            .collect();
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
        names
            .into_iter()
            .filter_map(|n| registry.get_provider(&n))
            .collect()
    }

    /// Absolute assessment only. `EvidenceAssessment::Verified` sources
    /// (workspace file re-reads, live tool receipts) pass. Everything else —
    /// including model narrative review — is TRUTH_UNVERIFIED. A model verdict
    /// is never independent proof of a fact.
    pub async fn cross_examine(
        record: &EvidenceRecord,
        _registry: &CapabilityRegistry,
        workspace: &Path,
    ) -> EaiResult<()> {
        match record.assess(workspace) {
            EvidenceAssessment::Verified => return Ok(()),
            EvidenceAssessment::Rejected(reason) => {
                return Err(EaiError::governance(format!(
                    "TRUTH_VIOLATION [DETERMINISTIC]: {reason}"
                )))
            }
            EvidenceAssessment::Unverified(_) => {}
        }

        if let EvidenceSource::AgentObservation {
            observation: _,
            reasoning_trace,
        } = &record.source
        {
            // Citation answers resolve from the live ledger. Narrative never.
            if let Some(resolved) =
                crate::susi_core::capture::EvidenceSession::verify_answer(reasoning_trace, workspace)
            {
                return resolved.map(|_| ());
            }
            return Err(EaiError::governance(
                "TRUTH_UNVERIFIED: agent narrative is not absolute evidence; cite live tool receipts",
            ));
        }

        Self::verify_evidence(record, workspace)
    }
}

