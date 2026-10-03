//! Mastery verification for VC-201-007: synthesized reflex intent
//! correctness — compile-and-probe with independent fixtures.
//!
//! The cited tests exercise the verdict strings. The distinguishing
//! property is that probing actually *executes or compiles* the reflex
//! and checks its behavior — not that the source text contains the
//! expected token.

use crate::reflex_intent::{probe_reflex, IntentFixture, ReflexVerdict};

fn fixture(input: &str, expect: &str) -> IntentFixture {
    IntentFixture {
        input: input.into(),
        expect_action: expect.into(),
    }
}

/// Falsification: nothing is compiled or run. A reflex whose only mention
/// of the expected action is in a *comment* — a reflex that panics on any
/// input — is Accepted, because probing is a substring scan.
#[test]
fn vc_201_007_mastery_comment_only_action_accepts() {
    let f = fixture("list dirs", "list_dirs");
    let code = "// ACTION: list_dirs\nfn run(_input) { panic!(\"no behavior\") }";
    assert_eq!(
        probe_reflex(code, &f),
        ReflexVerdict::Accept,
        "a reflex that does nothing but name the action in a comment is accepted"
    );
}

/// Falsification: the signature-echo check is string matching, trivially
/// evaded. An executable that returns the goal verbatim passes if it also
/// embeds the action token — 'return input' spelled as 'return(input)' or
/// via a variable alias dodges the check.
#[test]
fn vc_201_007_mastery_signature_echo_evades_string_matching() {
    let f = fixture("list dirs", "list_dirs");
    // Same behavior — echo the goal — but spelled so the scan misses it,
    // with the action token planted in a string literal.
    let code = "const MARK: &str = \"ACTION: list_dirs\"; fn run(i){ return(i) }";
    assert_eq!(
        probe_reflex(code, &f),
        ReflexVerdict::Accept,
        "a verbatim echo reflex spelled differently is accepted"
    );
}

/// Falsification: 'malformed-input checks' reject the *fixture*, never
/// probe the reflex. A reflex that panics on malformed input is never
/// exercised with one — the gate only refuses when the fixture itself is
/// malformed, so reflex robustness is untested.
#[test]
fn vc_201_007_mastery_malformed_check_never_reaches_the_reflex() {
    // Same reflex, two fixtures: a malformed fixture is rejected before
    // the reflex is even looked at — and a reflex that would crash on
    // malformed input is never tried against one.
    let fragile = "const OUT: &str = \"ACTION: x\"; fn run(i){ parse(i).unwrap() }";
    assert_eq!(
        probe_reflex(fragile, &fixture("\0", "x")),
        ReflexVerdict::RejectMalformed
    );
    assert_eq!(
        probe_reflex(fragile, &fixture("clean", "x")),
        ReflexVerdict::Accept,
        "the reflex's malformed-input behavior is never probed"
    );
}

/// What does hold: a reflex lacking the action token is rejected, and a
/// literal echo_goal marker is caught.
#[test]
fn vc_201_007_mastery_missing_token_and_literal_echo_reject() {
    let f = fixture("list dirs", "list_dirs");
    assert_eq!(
        probe_reflex("fn run(_){ nothing() }", &f),
        ReflexVerdict::RejectWrongOutput
    );
    assert_eq!(
        probe_reflex("fn run(i){ echo_goal(i) }", &f),
        ReflexVerdict::RejectSignatureEcho
    );
}
