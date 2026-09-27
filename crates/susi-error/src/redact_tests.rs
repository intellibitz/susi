//! Tests for `redact` — kept out of the shared source so mounting crates
//! never compile them.

#[cfg(test)]
mod redact_tests {
    use crate::redact::*;

    #[test]
    fn test_redact_masks_token_not_just_prefix() {
        let patterns = vec!["sk-".to_string()];
        let redacted = redact_patterns(&patterns, "token=sk-proj12345abcXYZ rest of log");
        assert!(!redacted.contains("proj12345"));
        assert!(redacted.contains("[REDACTED]"));
        assert!(redacted.contains("rest of log"));
    }

    #[test]
    fn ordinary_words_containing_a_prefix_are_not_secrets() {
        let patterns = vec!["sk-".to_string(), "AIza".to_string()];
        let text = "write a risk-assessment for the task-queue on disk-backed storage";
        assert_eq!(redact_patterns(&patterns, text), text);
        assert_eq!(contains_secret_pattern(&patterns, text), None);
        let leaked = "key=sk-live123 and (AIzaSyX1)";
        assert_eq!(
            redact_patterns(&patterns, leaked),
            "key=[REDACTED] and ([REDACTED])"
        );
        assert_eq!(
            contains_secret_pattern(&patterns, leaked),
            Some("sk-".into())
        );
    }

    #[test]
    fn test_empty_pattern_is_skipped() {
        let patterns = vec![String::new(), "secret".to_string()];
        let redacted = redact_patterns(&patterns, "the secret123 value");
        assert!(redacted.contains("[REDACTED]"));
    }
}

#[cfg(test)]
mod redact_prop_tests {
    use crate::redact::redact_patterns;
    use proptest::prelude::*;

    proptest! {
        // Property: a pattern followed by token-shaped characters is always
        // redacted — no secret suffix survives in the output.
        #[test]
        fn pattern_tail_never_survives(
            pat in "[a-z]{2,6}-",
            tail in "[A-Za-z0-9_-]{1,16}",
            tail2 in "[A-Za-z0-9_-]{0,8}",
        ) {
            let input = format!("x {pat}{tail} y {pat}{tail2} z");
            let out = redact_patterns(std::slice::from_ref(&pat), &input);
            let needle1 = format!("{pat}{tail}");
            let needle2 = format!("{pat}{tail2}");
            prop_assert!(!out.contains(&needle1));
            prop_assert!(!out.contains(&needle2));
        }

        // Property: redaction is idempotent for patterns that cannot appear
        // inside the "[REDACTED]" marker itself.
        #[test]
        fn redaction_is_idempotent(
            pat in "[a-z]{2,6}-",
            text in "[ -~]{0,200}",
        ) {
            let once = redact_patterns(std::slice::from_ref(&pat), &text);
            let twice = redact_patterns(&[pat], &once);
            prop_assert_eq!(once, twice);
        }
    }
}
