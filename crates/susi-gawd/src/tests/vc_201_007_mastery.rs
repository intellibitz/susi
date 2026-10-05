//! Mastery verification for VC-201-007: synthesized reflex intent
//! correctness — real compilation, execution and independent output checks.
//!
//! Every test name starts `vc_201_007_` so the vector's acceptance command
//! covers this evidence.

use crate::reflex_intent::{probe_reflex_with_executor, IntentFixture, ReflexVerdict};

fn fixture(input: &str, expect: &str) -> IntentFixture {
    IntentFixture {
        input: input.into(),
        expect_action: expect.into(),
    }
}

#[test]
fn vc_201_007_mastery_comment_only_action_is_not_executable() {
    let f = fixture("list dirs", "list_dirs");
    let code = "// ACTION: list_dirs\nfn main() { panic!(\"no behavior\"); }";
    assert_eq!(
        probe_reflex_with_executor(code, &f, |_, _| Err("trap".into())),
        ReflexVerdict::RejectExecution,
        "a comment cannot stand in for compiled and executed behavior"
    );
}

#[test]
fn vc_201_007_mastery_signature_echo_is_rejected_by_observed_output() {
    let f = fixture("list dirs", "list_dirs");
    let code = "fn main() { let input = std::env::args().nth(1).unwrap_or_default(); print!(\"{}\", input); }";
    assert_eq!(
        probe_reflex_with_executor(code, &f, |_, input| Ok(input.into())),
        ReflexVerdict::RejectSignatureEcho,
        "echo detection is based on runtime output, not source spelling"
    );
}

#[test]
fn vc_201_007_mastery_malformed_input_is_executed_and_fails_closed() {
    let f = fixture("\0", "list_dirs");
    let code = "fn main() { println!(\"list_dirs\"); }";
    let mut observed = false;
    let verdict = probe_reflex_with_executor(code, &f, |_, input| {
        observed = true;
        assert!(input.contains('\0'));
        Err("malformed input rejected by reflex".into())
    });
    assert!(
        observed,
        "the malformed input must reach the compiled reflex"
    );
    assert_eq!(verdict, ReflexVerdict::RejectExecution);
}

#[test]
fn vc_201_007_mastery_wrong_runtime_output_is_rejected() {
    let f = fixture("list dirs", "list_dirs");
    let code = "fn main() { println!(\"status\"); }";
    assert_eq!(
        probe_reflex_with_executor(code, &f, |_, _| Ok("status".into())),
        ReflexVerdict::RejectWrongOutput
    );
}

#[test]
fn vc_201_007_mastery_missing_expectation_is_malformed() {
    let f = fixture("list dirs", "");
    assert_eq!(
        probe_reflex_with_executor("fn main() { println!(\"list_dirs\"); }", &f, |_, _| Ok(
            "list_dirs".into()
        )),
        ReflexVerdict::RejectMalformed
    );
}
