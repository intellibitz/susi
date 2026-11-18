//! VC-200-002: a synthesized reflex registers only after its intent's own
//! checks pass in the sandbox — execution alone is not correctness.

use crate::reflex_verify::{parse_intent_spec, run_checks, IntentCheck, IntentSpec, InvariantSpec};

fn spec() -> IntentSpec {
    IntentSpec {
        fixtures: vec![
            IntentCheck {
                input: "2".into(),
                expected: Some("4".into()),
                expect_contains: None,
            },
            IntentCheck {
                input: "7".into(),
                expected: Some("14".into()),
                expect_contains: None,
            },
        ],
        invariants: Vec::new(),
    }
}

/// A "double the number" runner — the sandbox stand-in.
fn double(input: &str) -> Result<String, String> {
    input
        .trim()
        .parse::<i64>()
        .map(|n| (n * 2).to_string())
        .map_err(|_| "not a number".into())
}

#[test]
fn vc_200_002_passing_fixtures_permit_registration() {
    assert!(run_checks(&spec(), &double).is_ok());
}

#[test]
fn vc_200_002_wrong_output_keeps_reflex_unregistered() {
    // A reflex that adds instead of doubling executes fine — it is
    // still rejected because the intent's expected output differs.
    let wrong = |input: &str| -> Result<String, String> {
        input
            .trim()
            .parse::<i64>()
            .map(|n| (n + 1).to_string())
            .map_err(|_| "not a number".into())
    };
    let err = run_checks(&spec(), &wrong).unwrap_err();
    assert!(
        err.contains("expected"),
        "evidence names the mismatch: {err}"
    );
}

#[test]
fn vc_200_002_execution_failure_is_a_failed_check() {
    let crash = |_: &str| -> Result<String, String> { Err("trap".into()) };
    assert!(run_checks(&spec(), &crash).is_err());
}

#[test]
fn vc_200_002_metamorphic_invariant_runs_when_declared() {
    let mut s = spec();
    s.invariants = vec![InvariantSpec::WhitespaceNormalize {
        a: "2".into(),
        b: "  2  ".into(),
    }];
    assert!(run_checks(&s, &double).is_ok());
    // A whitespace-sensitive reflex violates the metamorphic property.
    let echo = |input: &str| -> Result<String, String> { Ok(input.to_string()) };
    assert!(run_checks(&s, &echo).is_err());
}

#[test]
fn vc_200_002_output_allowlist_bounds_every_observed_output() {
    let mut s = spec();
    s.invariants = vec![InvariantSpec::OutputAllowlist {
        allowed: vec!["4".into(), "14".into()],
    }];
    assert!(run_checks(&s, &double).is_ok());
    s.invariants = vec![InvariantSpec::OutputAllowlist {
        allowed: vec!["4".into()],
    }];
    assert!(run_checks(&s, &double).is_err(), "14 not allowlisted");
}

#[test]
fn vc_200_002_unverifiable_spec_is_rejected_at_parse() {
    // Empty spec: nothing to verify against → unverifiable → unregistered.
    assert!(parse_intent_spec(r#"{"fixtures": [], "invariants": []}"#).is_none());
    // Fixture with no expectation verifies nothing.
    assert!(parse_intent_spec(r#"{"fixtures": [{"input": "x"}], "invariants": []}"#).is_none());
    // Malformed JSON never becomes a spec.
    assert!(parse_intent_spec("not json").is_none());
    // A usable spec parses.
    let spec = parse_intent_spec(
        r#"{"fixtures": [{"input": "1", "expect_contains": "2"}], "invariants": []}"#,
    );
    assert!(spec.is_some());
}
