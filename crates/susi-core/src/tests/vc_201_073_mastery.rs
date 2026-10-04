//! Mastery verification for VC-201-073: classification labels
//! (local-only / workspace-private / approved-cloud) propagated through
//! every egress boundary; indirect delegation cannot strip a label.

use crate::prompt_secret_scan::{
    scan_prompt_classified, scan_prompt_secrets, ClassificationLabel, PromptScanPolicy,
};

/// Verification: classification labels exist and are enforced. A workspace-private
/// or local-only payload is refused from external egress even when carrying no credential.
#[test]
fn vc_201_073_mastery_private_nonsecret_payload_refused() {
    let payload = "workspace-private mission plan: replace engine weights";
    let r = scan_prompt_classified(
        payload,
        ClassificationLabel::WorkspacePrivate,
        &["sk-".to_string()],
        [],
        PromptScanPolicy::Block,
    );
    assert!(!r.is_clean(), "workspace-private payload must have hit");
    assert!(r.blocked(), "private content must be blocked from egress");
    assert_eq!(
        r.egress_text(),
        None,
        "private content must be refused egress"
    );

    // Also automatic inference when labeled in payload
    let r2 = scan_prompt_secrets(payload, &["sk-".to_string()], [], PromptScanPolicy::Block);
    assert!(r2.blocked(), "inferred workspace-private must be blocked");
    assert_eq!(r2.egress_text(), None);
}

/// Verification: held env credentials under DATABASE_PASSWORD and short tokens (<8 chars)
/// are detected and blocked.
#[test]
fn vc_201_073_mastery_env_secret_outside_name_list_caught() {
    let text = "connect with password swordfish-very-secret";
    let env = [(
        "DATABASE_PASSWORD".to_string(),
        "swordfish-very-secret".to_string(),
    )];
    let r = scan_prompt_secrets(text, &[], env, PromptScanPolicy::Block);
    assert!(!r.is_clean(), "held password secret must be caught");
    assert!(r.blocked(), "held password secret must be blocked");
    assert_eq!(r.egress_text(), None);

    // Short token (<8 chars) is also caught
    let env2 = [("MY_API_KEY".to_string(), "abc1234".to_string())];
    let r2 = scan_prompt_secrets("use abc1234", &[], env2, PromptScanPolicy::Block);
    assert!(!r2.is_clean(), "short token must be caught");
    assert!(r2.blocked(), "short token must be blocked");
    assert_eq!(r2.egress_text(), None);
}

/// Verification: character-spaced obfuscated secrets are detected and blocked.
#[test]
fn vc_201_073_mastery_obfuscated_secret_caught() {
    let env = [("MY_API_KEY".to_string(), "sk-abc123secret".to_string())];
    // Same secret, character-separated.
    let obfuscated = "s k - a b c 1 2 3 s e c r e t";
    let r = scan_prompt_secrets(obfuscated, &[], env, PromptScanPolicy::Block);
    assert!(!r.is_clean(), "obfuscated secret must be detected");
    assert!(r.blocked(), "obfuscated secret must be blocked");
    assert_eq!(r.egress_text(), None);
}

/// Verification: default policy is Block (fail-closed refusal). A secret-bearing prompt
/// refuses egress rather than silently shipping scrubbed text.
#[test]
fn vc_201_073_mastery_default_policy_refuses_egress() {
    let env = [("MY_API_KEY".to_string(), "sk-abc123secret".to_string())];
    let text = "call with sk-abc123secret attached";
    let r = scan_prompt_secrets(text, &[], env, PromptScanPolicy::default());
    assert!(!r.is_clean(), "secret must be found");
    assert!(
        r.blocked(),
        "default policy must refuse egress (fail-closed)"
    );
    assert_eq!(
        r.egress_text(),
        None,
        "default policy must not ship content"
    );
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
