//! Classification of legacy text outcomes. Passing this filter is not proof
//! that a claim is true; it only makes the output eligible for synthesis.

use std::sync::OnceLock;

pub fn is_failure(output: &str) -> bool {
    let upper = output.to_uppercase();
    // Bracket / multi-word markers: substring match is exact enough.
    const PHRASES: &[&str] = &[
        "[FAIL]",
        "[FAILED]",
        "[CAPABILITY_GAP]",
        "[GOVERNANCE_BLOCK]",
        "[STALLED]",
        "[DAG_EXECUTION_FAILED]",
        "AGENT EXECUTION FAILED",
        "TRUTH_VIOLATION",
        "TRUTH_UNVERIFIED",
        "REALITY VIOLATION",
        "AXIOMATIC VIOLATION",
        "UNGROUNDED CLAIMS DETECTED",
    ];
    if PHRASES.iter().any(|marker| upper.contains(marker)) {
        return true;
    }
    // Single-token markers must be word-bounded — otherwise a verified
    // `Cargo.toml` comment about "test failures" poisons a native read.
    static WORD_FAIL: OnceLock<regex::Regex> = OnceLock::new();
    #[allow(clippy::expect_used)] // static pattern; compile failure is a build bug
    let word_fail = WORD_FAIL
        .get_or_init(|| regex::Regex::new(r"\bFAILURE\b").expect("static failure-token pattern"));
    word_fail.is_match(&upper)
}

pub fn is_usable(output: &str) -> bool {
    !output.trim().is_empty() && !is_failure(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn execution_failures_are_ineligible_regardless_of_case() {
        for output in [
            "Agent Execution Failed: unavailable",
            "[stalled] cancelled",
            "[GOVERNANCE_BLOCK] veto",
            "[CAPABILITY_GAP] missing",
            "failure: timeout",
            "[DAG_EXECUTION_FAILED] error",
            "TRUTH_VIOLATION: nonexistent file",
            "TRUTH_UNVERIFIED: no verifier",
        ] {
            assert!(is_failure(output), "{output}");
            assert!(!is_usable(output));
        }
        assert!(!is_usable("  \n"));
        assert!(is_usable("Measured 8 available CPU cores."));
    }

    #[test]
    fn test_failures_comment_is_not_a_mission_failure() {
        let cargo_comment = "# panic backtraces and test failures still get file:line";
        assert!(!is_failure(cargo_comment), "{cargo_comment}");
        assert!(is_usable(cargo_comment));
        assert!(is_failure("FAILURE: boom"));
        assert!(is_failure("mission FAILURE reported"));
    }
}
