use crate::reflex_intent::{probe_reflex_with_executor, IntentFixture, ReflexVerdict};

fn fixture(input: &str, expect: &str) -> IntentFixture {
    IntentFixture {
        input: input.into(),
        expect_action: expect.into(),
    }
}

#[test]
fn vc_201_007_compiles_and_accepts_matching_runtime_output() {
    let f = fixture("list dirs", "list_dirs");
    let code = "fn main() { println!(\"list_dirs\"); }";
    assert_eq!(
        probe_reflex_with_executor(code, &f, |_, _| Ok("list_dirs\n".into())),
        ReflexVerdict::Accept
    );
}

#[test]
fn vc_201_007_rejects_source_that_does_not_compile() {
    let f = fixture("list dirs", "list_dirs");
    let code = "this is not Rust";
    assert_eq!(
        probe_reflex_with_executor(code, &f, |_, _| Ok("list_dirs".into())),
        ReflexVerdict::RejectCompilation
    );
}

#[test]
fn vc_201_007_runtime_failure_is_not_a_success() {
    let f = fixture("list dirs", "list_dirs");
    let code = "fn main() { panic!(\"no behavior\"); }";
    assert_eq!(
        probe_reflex_with_executor(code, &f, |_, _| Err("trap".into())),
        ReflexVerdict::RejectExecution
    );
}

#[test]
fn vc_201_007_runtime_output_rejects_echo_and_wrong_results() {
    let f = fixture("list dirs", "list_dirs");
    let code = "fn main() { println!(\"runtime\"); }";
    assert_eq!(
        probe_reflex_with_executor(code, &f, |_, input| Ok(input.into())),
        ReflexVerdict::RejectSignatureEcho
    );
    assert_eq!(
        probe_reflex_with_executor(code, &f, |_, _| Ok("status".into())),
        ReflexVerdict::RejectWrongOutput
    );
}

#[test]
fn vc_201_007_malformed_input_reaches_the_executor() {
    let f = fixture("\0", "list_dirs");
    let code = "fn main() { println!(\"list_dirs\"); }";
    assert_eq!(
        probe_reflex_with_executor(code, &f, |_, input| {
            if input.contains('\0') {
                Err("malformed input trap".into())
            } else {
                Ok("list_dirs".into())
            }
        }),
        ReflexVerdict::RejectExecution
    );
}
