use crate::reflex_intent::{probe_reflex, IntentFixture, ReflexVerdict};

#[test]
fn vc_201_007_rejects_signature_echo() {
    let f = IntentFixture {
        input: "list dirs".into(),
        expect_action: "list_dirs".into(),
    };
    assert_eq!(
        probe_reflex("fn run(input){ echo_goal(input) }", &f),
        ReflexVerdict::RejectSignatureEcho
    );
}

#[test]
fn vc_201_007_rejects_malformed_input() {
    let f = IntentFixture {
        input: "\0".into(),
        expect_action: "x".into(),
    };
    assert_eq!(
        probe_reflex("ACTION: x", &f),
        ReflexVerdict::RejectMalformed
    );
}

#[test]
fn vc_201_007_accepts_independent_fixture_match() {
    let f = IntentFixture {
        input: "list dirs".into(),
        expect_action: "list_dirs".into(),
    };
    assert_eq!(
        probe_reflex("const OUT = \"ACTION: list_dirs\";", &f),
        ReflexVerdict::Accept
    );
}
