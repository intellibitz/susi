//! Mastery verification for VC-201-073: classification labels
//! (local-only / workspace-private / approved-cloud) propagated through
//! every egress boundary; indirect delegation cannot strip a label.

use crate::prompt_secret_scan::{scan_prompt_secrets, PromptScanPolicy};

/// Falsification: there are no classification labels at all. The scan
/// keys on secret-shaped content only — a workspace-private payload
/// carrying no credential is clean and egresses, whatever 'label' a
/// producer would have put on it.
#[test]
fn vc_201_073_mastery_private_nonsecret_payload_egresses() {
    // A local-only artifact's content — no credential shape.
    let payload = "workspace-private mission plan: replace engine weights";
    let r = scan_prompt_secrets(payload, &["sk-".to_string()], [], PromptScanPolicy::Block);
    assert!(r.is_clean());
    assert_eq!(r.egress_text(), Some(payload), "private content egressed");
}

/// Falsification: the env-held detector only matches vars named
/// *_API_KEY, *_TOKEN or *_SECRET with length >= 8. A credential held in
/// DATABASE_PASSWORD or a 7-char token value is invisible — the scan
/// reports clean while the secret ships.
#[test]
fn vc_201_073_mastery_env_secret_outside_name_list_evades() {
    let text = "connect with password swordfish-very-secret";
    let env = [(
        "DATABASE_PASSWORD".to_string(),
        "swordfish-very-secret".to_string(),
    )];
    let r = scan_prompt_secrets(text, &[], env, PromptScanPolicy::Block);
    assert!(r.is_clean(), "a held secret under another name is missed");
    // And a short token value (<8 chars) is skipped by the length gate.
    let env2 = [("MY_API_KEY".to_string(), "abc1234".to_string())];
    let r2 = scan_prompt_secrets("use abc1234", &[], env2, PromptScanPolicy::Block);
    assert!(r2.is_clean());
}

/// Falsification: verbatim matching — a trivially obfuscated secret
/// (spaced, base64) does not match the recorded pattern and egresses.
#[test]
fn vc_201_073_mastery_obfuscated_secret_evades() {
    let env = [("MY_API_KEY".to_string(), "sk-abc123secret".to_string())];
    // Same secret, character-separated.
    let obfuscated = "s k - a b c 1 2 3 s e c r e t";
    let r = scan_prompt_secrets(obfuscated, &[], env, PromptScanPolicy::Block);
    assert!(r.is_clean());
}

/// Falsification: 'egress refusal' is opt-in — the DEFAULT policy is
/// Redact, and egress_text then returns the scrubbed payload: the send
/// proceeds with secret-free text rather than being refused. Indirect
/// paths that forget Block still egress.
#[test]
fn vc_201_073_mastery_default_policy_ships_content() {
    let env = [("MY_API_KEY".to_string(), "sk-abc123secret".to_string())];
    let text = "call with sk-abc123secret attached";
    let r = scan_prompt_secrets(text, &[], env, PromptScanPolicy::default());
    assert!(!r.is_clean());
    assert!(!r.blocked(), "default policy does not refuse egress");
    assert!(r.egress_text().is_some(), "scrubbed content is still sent");
}

/// What holds: a known-prefix secret under Block refuses egress, and a
/// held *_API_KEY value embedded in text is caught verbatim.
#[test]
fn vc_201_073_mastery_verbatim_block_holds() {
    let env = [("MY_API_KEY".to_string(), "sk-abc123secret".to_string())];
    let r = scan_prompt_secrets("key is sk-abc123secret", &[], env, PromptScanPolicy::Block);
    assert!(!r.is_clean());
    assert!(r.blocked());
    assert!(r.egress_text().is_none());
}
