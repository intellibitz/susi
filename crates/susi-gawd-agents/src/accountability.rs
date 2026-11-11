//! Classification of legacy text outcomes. Passing this filter is not proof
//! that a claim is true; it only makes the output eligible for synthesis.

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
    // Word chars are alphanumerics and `_`, as in `susi_core::pattern`.
    let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
    upper.match_indices("FAILURE").any(|(at, token)| {
        !word(upper[..at].chars().next_back()) && !word(upper[at + token.len()..].chars().next())
    })
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
        assert!(is_failure("mission FAILURE reported") && is_failure("(FAILURE)"));
        assert!(!is_failure("_FAILURE FAILURE_ 2FAILURE FAILURES"));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // Test-only module: unwrap/expect/panic are how these tests assert.
mod contract_tests {
    use super::*;

    const PHRASES: [&str; 12] = [
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

    #[test]
    fn every_failure_marker_is_detected_anywhere_in_the_text_in_any_case() {
        for marker in PHRASES {
            for text in [
                marker.to_string(),
                format!("prefix {marker} suffix"),
                format!("line one\n{}\nline three", marker.to_lowercase()),
                format!("{}{}", marker.to_lowercase(), " trailing"),
            ] {
                assert!(is_failure(&text), "{marker}: {text:?}");
                assert!(!is_usable(&text), "{marker}: {text:?}");
            }
        }
    }

    #[test]
    fn the_failure_token_is_word_bounded() {
        for failing in [
            "FAILURE",
            "failure",
            "Failure.",
            "(failure)",
            "a failure occurred",
            "failure-mode analysis",
            "result: failure\nnext line",
        ] {
            assert!(is_failure(failing), "{failing:?}");
        }
        for benign in [
            "failures",
            "test failures were reviewed",
            "subfailure",
            "nonfailure",
            "failure_mode",
            "unfailure",
        ] {
            assert!(!is_failure(benign), "{benign:?}");
            assert!(is_usable(benign), "{benign:?}");
        }
    }

    #[test]
    fn plain_fail_words_and_empty_text_are_not_failures() {
        // Only the listed markers and the bounded token count: this filter is
        // not a general sentiment check.
        for text in ["", "test failed", "it will fail later", "[FAILING]", "[OK]"] {
            assert!(!is_failure(text), "{text:?}");
        }
    }

    #[test]
    fn usable_output_is_non_blank_and_not_a_failure() {
        for blank in ["", " ", "\n", " \t\r\n "] {
            assert!(!is_usable(blank), "{blank:?}");
        }
        assert!(is_usable("Measured 8 available CPU cores."));
        assert!(is_usable("  padded but real  "));
        assert!(is_usable("✓ vérifié — 数据"));
        assert!(!is_usable("   [STALLED]   "));
    }
}
