// Checks tool-reported results against actual workspace state (e.g. a
// claimed file write that never happened) before a mission treats the
// result as fact.

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
            // A violation found here (claimed file write that doesn't exist,
            // or exists empty) is never overridable. It used to be bypassable
            // by a "[CONVERGENCE_SCORE: >= 0.85]" marker in the result string
            // (see amas.rs's AGENT_SUCCESS_RATIO, formerly CONVERGENCE_SCORE),
            // but that score only measures whether other agents' output text
            // avoided the substrings "FAILURE"/"GAP" — it says nothing about
            // whether the claimed file exists. That let a real missing-file
            // violation get waved through as "(Verified via Epistemic
            // Delegation)" just because unrelated agents didn't say the word
            // "failure". Don't reintroduce a bypass here.
            let error_msg = format!("TRUTH_VIOLATION: {}\nSTRUCTURED_FEEDBACK: Please grounded your response in the physical workspace state. Ensure files are actually written before reporting success.", violations.join(" | "));
            return Err(EaiError::governance(error_msg));
        }

        Ok(result.to_string())
    }
}

pub struct TruthTransformer;

impl TruthTransformer {
    pub fn verify_mission_reality(
        goal: &str,
        tool_name: &str,
        result: &str,
        workspace: &Path,
    ) -> EaiResult<String> {
        SusiTruthAgent::verify_mission_reality(goal, tool_name, result, workspace)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_claimed_write_to_nonexistent_file_is_a_violation() {
        let tmp = std::env::temp_dir().join("susi_test_truth_nonexistent");
        let _ = std::fs::create_dir_all(&tmp);
        let result = "Wrote to output.txt successfully.";
        assert!(SusiTruthAgent::verify_mission_reality("goal", "tool", result, &tmp).is_err());
    }

    /// Regression: a claimed file write that doesn't exist used to be
    /// overridable by a `[CONVERGENCE_SCORE: >= 0.85]` marker in the same
    /// result string — a score that only reflects whether unrelated agents'
    /// output avoided the words "FAILURE"/"GAP", not whether the file
    /// actually existed. Must not bypass the check again, under this or any
    /// other score-shaped marker.
    #[test]
    fn test_high_convergence_score_no_longer_bypasses_a_real_violation() {
        let tmp = std::env::temp_dir().join("susi_test_truth_no_bypass");
        let _ = std::fs::create_dir_all(&tmp);
        let result = "Wrote to output.txt successfully.\n\n[CONVERGENCE_SCORE: 1.00]";
        let err = SusiTruthAgent::verify_mission_reality("goal", "tool", result, &tmp)
            .expect_err("a fabricated score must not override a verified missing-file violation");
        assert!(err.to_string().contains("TRUTH_VIOLATION"));
    }

    #[test]
    fn test_claimed_write_to_real_nonempty_file_passes() {
        let tmp = std::env::temp_dir().join("susi_test_truth_real_file");
        let _ = std::fs::create_dir_all(&tmp);
        std::fs::write(tmp.join("output.txt"), b"real content").unwrap();
        let result = "Wrote to output.txt successfully.";
        assert!(SusiTruthAgent::verify_mission_reality("goal", "tool", result, &tmp).is_ok());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
