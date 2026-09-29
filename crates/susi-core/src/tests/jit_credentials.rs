//! Tests for JIT credentials (`zc_jit_credentials_*`).

use crate::jit_credentials::{
    request_credential_jit, CredentialNeeded, JitCredentialOutcome, JitCredentialRequest,
    CREDENTIAL_NEEDED_EXIT,
};
use std::io::Cursor;

#[test]
fn zc_jit_credentials_already_present_short_circuits() {
    let mut out = Vec::new();
    let mut input = Cursor::new(String::new());
    let req = JitCredentialRequest {
        provider: "openai",
        env_var: "OPENAI_API_KEY",
        stdin_is_tty: true,
    };
    let outcome =
        request_credential_jit(&req, |_| Some("sk-already".into()), &mut out, &mut input).unwrap();
    assert_eq!(
        outcome,
        JitCredentialOutcome::AlreadyPresent("sk-already".into())
    );
    assert!(out.is_empty());
}

#[test]
fn zc_jit_credentials_tty_prompts_once() {
    let mut out = Vec::new();
    let mut input = Cursor::new("sk-from-tty\n");
    let req = JitCredentialRequest {
        provider: "openai",
        env_var: "OPENAI_API_KEY",
        stdin_is_tty: true,
    };
    let outcome = request_credential_jit(&req, |_| None, &mut out, &mut input).unwrap();
    assert_eq!(
        outcome,
        JitCredentialOutcome::SuppliedOnTty("sk-from-tty".into())
    );
    let prompt = String::from_utf8(out).unwrap();
    assert!(prompt.contains("OPENAI_API_KEY"));
    assert!(prompt.contains("openai"));
}

#[test]
fn zc_jit_credentials_non_tty_emits_machine_record_and_exit_code() {
    let mut out = Vec::new();
    let mut input = Cursor::new(String::new());
    let req = JitCredentialRequest {
        provider: "anthropic",
        env_var: "ANTHROPIC_API_KEY",
        stdin_is_tty: false,
    };
    let outcome = request_credential_jit(&req, |_| None, &mut out, &mut input).unwrap();
    let JitCredentialOutcome::NeedRecord(rec) = outcome else {
        panic!("expected NeedRecord");
    };
    assert_eq!(rec.exit_code, CREDENTIAL_NEEDED_EXIT);
    assert_eq!(rec.kind, "credential_needed");
    assert_eq!(rec.env_var, "ANTHROPIC_API_KEY");
    let line = rec.to_json_line();
    let parsed: CredentialNeeded = serde_json::from_str(&line).unwrap();
    assert_eq!(parsed.exit_code, 81);
    assert!(!line.contains("stack"));
    assert!(!line.contains("panic"));
}
