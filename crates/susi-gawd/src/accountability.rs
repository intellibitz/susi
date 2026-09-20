//! Classification of legacy text outcomes. Passing this filter is not proof
//! that a claim is true; it only makes the output eligible for synthesis.

pub(crate) fn is_failure(output: &str) -> bool {
    let upper = output.to_uppercase();
    [
        "FAILURE",
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
    ]
    .iter()
    .any(|marker| upper.contains(marker))
}

pub(crate) fn is_usable(output: &str) -> bool {
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
}
