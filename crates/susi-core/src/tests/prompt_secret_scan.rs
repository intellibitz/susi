//! Corpus tests for prompt secret scanning (`prompt_secret_scan_*`).

use crate::prompt_secret_scan::{scan_prompt_secrets, PromptScanPolicy};

fn patterns() -> Vec<String> {
    vec![
        "sk-".into(),
        "ghp_".into(),
        "AIza".into(),
        "xoxb-".into(),
        "github_pat_".into(),
        "-----BEGIN RSA PRIVATE KEY-----".into(),
    ]
}

#[test]
fn prompt_secret_scan_positive_corpus_detects_vendor_prefixes() {
    let positives = [
        "Authorization: Bearer sk-live-abc123XYZ",
        "export TOKEN=ghp_abcdefghijklmnopqrstuvwxyz012345",
        "key=AIzaSyA-test-key-value-here",
        "slack xoxb-1234567890-token",
        "pat github_pat_11AAAABBBBCCCC",
        "-----BEGIN RSA PRIVATE KEY-----\nMIIE...",
    ];
    for text in positives {
        let r = scan_prompt_secrets(
            text,
            &patterns(),
            None::<(String, String)>,
            PromptScanPolicy::Redact,
        );
        assert!(
            !r.is_clean(),
            "expected hit in positive corpus item: {text}"
        );
        assert!(r.redacted.contains("[REDACTED]") || r.redacted != text);
        assert_eq!(r.egress_text(), Some(r.redacted.as_str()));
    }
}

#[test]
fn prompt_secret_scan_negative_corpus_keeps_benign_text() {
    let negatives = [
        "write a risk-assessment for the task-queue",
        "please ask about disks and skillets",
        "normal mission goal: list_directory in src/",
        "ghpages deploy is fine",
    ];
    for text in negatives {
        let r = scan_prompt_secrets(
            text,
            &patterns(),
            None::<(String, String)>,
            PromptScanPolicy::Block,
        );
        assert!(r.is_clean(), "false positive on: {text} → {:?}", r.hits);
        assert_eq!(r.egress_text(), Some(text));
    }
}

#[test]
fn prompt_secret_scan_block_policy_refuses_egress() {
    let text = "leak sk-prod-secret-value-here in the prompt";
    let r = scan_prompt_secrets(
        text,
        &patterns(),
        Vec::<(String, String)>::new(),
        PromptScanPolicy::Block,
    );
    assert!(r.blocked());
    assert!(r.egress_text().is_none());
}

#[test]
fn prompt_secret_scan_env_held_secret_is_detected() {
    let held = "supersecretvalue99".to_string();
    let text = format!("please use {held} against the API");
    let env = vec![("OPENAI_API_KEY".to_string(), held.clone())];
    let r = scan_prompt_secrets(&text, &patterns(), env, PromptScanPolicy::Redact);
    assert!(!r.is_clean());
    assert!(!r.redacted.contains(&held));
    assert!(r.hits.iter().any(|h| h.kind.contains("OPENAI_API_KEY")));
}
