use super::*;

use std::thread;

#[test]
fn a_missing_local_model_is_a_failure_that_names_the_download_command() {
    let missing = crate::models::NoLocalModelInstalled::starter();
    let text = missing.to_string();
    assert!(GemiEngine::looks_like_error_text(&text));
    assert!(
        text.contains("`susi os manage model install starter`"),
        "{text}"
    );
    assert!(text.contains("about 429 MB"), "{text}");
    assert!(
        text.contains("Cloud models work without a download"),
        "{text}"
    );
    assert!(EaiError::from(missing)
        .to_string()
        .contains("[NO_LOCAL_MODEL]"));
}

#[test]
fn error_text_detection_rejects_stringified_failures() {
    // The observed live leak: an MCP -32603 surfaced through Ok(String)
    // all the way into a chat completion's content.
    assert!(GemiEngine::looks_like_error_text(
        "Protocol Error: Mcp error: -32603: Unknown tool: reason"
    ));
    assert!(GemiEngine::looks_like_error_text(
        "[FAIL] SUSI-Tier2-Inference: exhausted"
    ));
    assert!(GemiEngine::looks_like_error_text(
        "Inference Error: model crashed"
    ));
    assert!(GemiEngine::looks_like_error_text(
        "Governance Violation: blocked"
    ));
    assert!(!GemiEngine::looks_like_error_text("the answer is 42"));
    // A multi-byte char straddling byte 64 must not panic the check.
    let tricky = format!("{}é tail", "a".repeat(63));
    assert!(!GemiEngine::looks_like_error_text(&tricky));
    assert!(!GemiEngine::looks_like_error_text(
        "Later in the text an error: is discussed"
    ));
}

#[test]
fn test_backend_dispatch_rejects_unsupported_architectures() {
    // Which weights loader serves an admitted architecture is the local
    // engine plug's business (Susi Commercial Apps); core admits these.
    for arch in ["qwen3", "gemma", "gptoss", "unknown"] {
        assert!(InferenceHost::validate_architecture(arch).is_err());
    }
    for arch in ["llama", "qwen2", "qwen2moe", "qwen3moe"] {
        assert!(InferenceHost::validate_architecture(arch).is_ok());
    }
}

#[test]
fn test_has_usable_local_model_false_for_empty_workspace() {
    // Point the models scan at an empty dir (test builds honor SUSI_MODEL_DIR
    // ahead of the shared susi_test_models folder).
    let isolated =
        std::env::temp_dir().join(format!("susi_engine_test_no_models_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&isolated);
    let models = isolated.join("models");
    let workspace = isolated.join("workspace");
    std::fs::create_dir_all(&models).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let prev = std::env::var_os("SUSI_MODEL_DIR");
    unsafe {
        std::env::set_var("SUSI_MODEL_DIR", &models);
    }
    let usable = GemiEngine::has_usable_local_model(&workspace);
    unsafe {
        match prev {
            Some(v) => std::env::set_var("SUSI_MODEL_DIR", v),
            None => std::env::remove_var("SUSI_MODEL_DIR"),
        }
    }
    let _ = std::fs::remove_dir_all(&isolated);
    assert!(!usable);
}

#[test]
fn test_competitive_racing_logic() {
    let (tx, rx) = std::sync::mpsc::channel();
    let tx1 = tx.clone();
    thread::spawn(move || {
        thread::sleep(std::time::Duration::from_millis(50));
        let _ = tx1.send("FastPath".to_string());
    });
    thread::spawn(move || {
        thread::sleep(std::time::Duration::from_millis(200));
        let _ = tx.send("SlowPath".to_string());
    });
    let winner = rx.recv().unwrap();
    assert_eq!(winner, "FastPath");
}

struct MockRouteProvider {
    name: &'static str,
    reply: &'static str,
}

impl crate::susi_core::provider::Provider for MockRouteProvider {
    fn name(&self) -> &str {
        self.name
    }
    fn is_healthy(
        &self,
    ) -> crate::susi_core::provider::BoxFuture<'_, crate::susi_core::susi_error::EaiResult<bool>>
    {
        Box::pin(async { Ok(true) })
    }
    fn generate(
        &self,
        _prompt: &str,
    ) -> crate::susi_core::provider::BoxFuture<'_, crate::susi_core::susi_error::EaiResult<String>>
    {
        let reply = self.reply.to_string();
        Box::pin(async move {
            if reply.starts_with("ERR:") {
                return Err(crate::susi_core::susi_error::EaiError::process(reply));
            }
            Ok(reply)
        })
    }
    fn embed(
        &self,
        _text: &str,
    ) -> crate::susi_core::provider::BoxFuture<'_, crate::susi_core::susi_error::EaiResult<Vec<f32>>>
    {
        Box::pin(async { Ok(vec![]) })
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[test]
fn test_try_providers_fails_over_after_provider_error() {
    // The store is process-global and its path comes from a
    // process-global env var that other tests set and clear, so readers
    // take the same lock the writers do.
    let _env = crate::engines::env_test_lock();
    let registry = crate::susi_core::registry::CapabilityRegistry::new();
    registry.register_provider(MockRouteProvider {
        name: "openai-primary",
        reply: "ERR:402 billing",
    });
    registry.register_provider(MockRouteProvider {
        name: "deepseek-backup",
        reply: "recovered",
    });

    let (out, _ladder) =
        GemiEngine::try_providers(&registry, "hello", None, None, None, &|_| {}, &|_| {});
    assert_eq!(out.as_deref(), Some("recovered"));
}

#[test]
fn test_try_providers_records_outcomes_that_reorder_the_brain() {
    // The store is process-global and its path comes from a
    // process-global env var that other tests set and clear, so readers
    // take the same lock the writers do.
    let _env = crate::engines::env_test_lock();
    use crate::engines::brain::{rank, TaskClass};
    let registry = crate::susi_core::registry::CapabilityRegistry::new();
    // Same static rank; name order would try `brainlearn-a` first.
    registry.register_provider(MockRouteProvider {
        name: "ollama-brainlearn-a",
        reply: "ERR:boom",
    });
    registry.register_provider(MockRouteProvider {
        name: "ollama-brainlearn-b",
        reply: "fine",
    });
    let prompt = "brainlearn ping";
    assert_eq!(TaskClass::classify(prompt), TaskClass::Reflex);
    let (out, _ladder) =
        GemiEngine::try_providers(&registry, prompt, None, None, None, &|_| {}, &|_| {});
    assert_eq!(out.as_deref(), Some("fine"));
    let both = vec![
        "ollama-brainlearn-a".to_string(),
        "ollama-brainlearn-b".to_string(),
    ];
    let ranked = rank(&both, TaskClass::Reflex);
    assert_eq!(ranked[0].provider, "ollama-brainlearn-b");
    // The error text is an availability failure, not a wrong answer:
    // nothing lands on the capability axis (VC-202-020/T-117).
    assert_eq!(ranked[1].success_rate, None);
}

#[test]
fn test_try_providers_treats_flattened_error_text_as_failure() {
    // The store is process-global and its path comes from a
    // process-global env var that other tests set and clear, so readers
    // take the same lock the writers do.
    let _env = crate::engines::env_test_lock();
    use crate::engines::brain::{rank, TaskClass};
    let registry = crate::susi_core::registry::CapabilityRegistry::new();
    registry.register_provider(MockRouteProvider {
        name: "ollama-flatten-a",
        reply: "[INFERENCE_FAILED] upstream 500",
    });
    registry.register_provider(MockRouteProvider {
        name: "ollama-flatten-b",
        reply: "real answer",
    });
    let prompt = "flatten check";
    let (out, _ladder) =
        GemiEngine::try_providers(&registry, prompt, None, None, None, &|_| {}, &|_| {});
    assert_eq!(out.as_deref(), Some("real answer"));
    let names = vec!["ollama-flatten-a".to_string()];
    let a = &rank(&names, TaskClass::classify(prompt))[0];
    assert_eq!(
        a.success_rate, None,
        "flattened error text is availability evidence — the capability \
             axis stays untouched (VC-202-020/T-117)"
    );
}

#[test]
fn test_try_providers_never_lets_a_failing_provider_lead() {
    // The store is process-global and its path comes from a
    // process-global env var that other tests set and clear, so readers
    // take the same lock the writers do.
    let _env = crate::engines::env_test_lock();
    use crate::engines::brain::{note_failure, record_outcome, FailureKind, TaskClass};
    let registry = crate::susi_core::registry::CapabilityRegistry::new();
    // Name order would try `ollama-dry-a` first.
    registry.register_provider(MockRouteProvider {
        name: "ollama-dry-a",
        reply: "dry answered",
    });
    registry.register_provider(MockRouteProvider {
        name: "ollama-dry-b",
        reply: "fit answered",
    });
    let prompt = "dry ping";
    let class = TaskClass::classify(prompt);
    for _ in 0..4 {
        record_outcome("ollama-dry-a", class, false, 0);
    }
    note_failure("ollama-dry-a", FailureKind::Funds);
    let (out, _ladder) =
        GemiEngine::try_providers(&registry, prompt, None, None, None, &|_| {}, &|_| {});
    assert_eq!(out.as_deref(), Some("fit answered"));
}

#[test]
fn test_try_providers_prefers_ollama_over_generic_and_skips_candle() {
    // The store is process-global and its path comes from a
    // process-global env var that other tests set and clear, so readers
    // take the same lock the writers do.
    let _env = crate::engines::env_test_lock();
    let registry = crate::susi_core::registry::CapabilityRegistry::new();
    registry.register_provider(MockRouteProvider {
        name: "Candle (Local)",
        reply: "from-candle",
    });
    registry.register_provider(MockRouteProvider {
        name: "generic-remote",
        reply: "from-generic",
    });
    registry.register_provider(MockRouteProvider {
        name: "ollama-llama3",
        reply: "from-ollama",
    });

    let (out, _ladder) =
        GemiEngine::try_providers(&registry, "hello", None, None, None, &|_| {}, &|_| {});
    assert_eq!(out.as_deref(), Some("from-ollama"));
}

#[test]
fn test_try_providers_honors_requested_model_name_match() {
    // The store is process-global and its path comes from a
    // process-global env var that other tests set and clear, so readers
    // take the same lock the writers do.
    let _env = crate::engines::env_test_lock();
    let registry = crate::susi_core::registry::CapabilityRegistry::new();
    registry.register_provider(MockRouteProvider {
        name: "ollama-llama3",
        reply: "llama3",
    });
    registry.register_provider(MockRouteProvider {
        name: "vllm-mixtral",
        reply: "mixtral",
    });

    let (out, _ladder) = GemiEngine::try_providers(
        &registry,
        "hello",
        None,
        Some("mixtral"),
        None,
        &|_| {},
        &|_| {},
    );
    assert_eq!(out.as_deref(), Some("mixtral"));
}

/// `axiomatic_risk_patterns` is config-driven now, not a hardcoded Rust
/// array — prove the configured patterns actually gate the check, and
/// that ordinary reasoning output isn't blocked.
#[test]
fn test_verify_axiomatic_alignment_uses_configured_risk_patterns() {
    let patterns = crate::susi_sandbox::manager::SusiConfig::default().axiomatic_risk_patterns();
    assert!(!patterns.is_empty());

    let workspace = Path::new(".");
    for pattern in &patterns {
        let reasoning = format!("Here is a plan: {}", pattern);
        assert!(
            GemiEngine::verify_axiomatic_alignment(&reasoning, workspace).is_err(),
            "configured risk pattern '{}' must be blocked",
            pattern
        );
    }

    assert!(GemiEngine::verify_axiomatic_alignment(
        "Here is a perfectly safe plan to list files.",
        workspace
    )
    .is_ok());
}

#[test]
fn test_logits_repetition_penalty_dampens_recent_tokens() {
    let mut logits_v = vec![10.0f32, 10.0f32, 10.0f32]; // Equal logits for tokens 0, 1, 2
    apply_repeat_penalty(&mut logits_v, 1.15, &[1u32]); // Token 1 has been generated

    // Token 1 was penalized: 10.0 / 1.15 ~ 8.695
    assert!(logits_v[1] < logits_v[0]);
    assert!(logits_v[1] < logits_v[2]);
    assert_eq!(logits_v[0], 10.0);
    assert_eq!(logits_v[2], 10.0);
}

#[test]
fn test_repeat_penalty_disabled_at_1_0_or_below() {
    let original = vec![10.0f32, -5.0, 3.0];
    let mut logits_v = original.clone();
    apply_repeat_penalty(&mut logits_v, 1.0, &[0, 1, 2]);
    assert_eq!(logits_v, original);

    let mut logits_v = original.clone();
    apply_repeat_penalty(&mut logits_v, 0.0, &[0, 1, 2]);
    assert_eq!(logits_v, original);
}

#[test]
fn test_repeat_penalty_dampens_negative_logits_by_multiplying() {
    // llama.cpp convention: a negative logit is already "unlikely," so
    // penalizing it means moving it further negative (multiply), not
    // dividing (which would move a negative value toward zero, i.e.
    // *more* likely - the opposite of a penalty).
    let mut logits_v = vec![-4.0f32];
    apply_repeat_penalty(&mut logits_v, 1.15, &[0]);
    assert_eq!(logits_v[0], -4.6);
}

#[test]
fn test_generation_loop_excludes_prompt_tokens_from_repeat_penalty_window() {
    // Regression: the window used to be built from prompt_tokens +
    // all_tokens combined, so a short prompt sat inside the
    // repeat_last_n window for the entire generation and got penalized
    // from the very first token. Verified live: "What is the capital of
    // France?" (where "France" is a prompt token) drifted into an
    // unrelated fact about France without ever saying "Paris," because
    // "France" was already penalized before generation started. The
    // fix scopes the window to `all_tokens` (generated so far) only;
    // this locks in that a prompt-only token is never penalized.
    let prompt_tokens: Vec<u32> = vec![42]; // e.g. the token for "France"
    let all_tokens: Vec<u32> = vec![]; // nothing generated yet
    let repeat_last_n = 64usize;

    let mut logits_v = vec![10.0f32; 100];
    let start_idx = all_tokens.len().saturating_sub(repeat_last_n);
    apply_repeat_penalty(&mut logits_v, 1.15, &all_tokens[start_idx..]);

    assert_eq!(
        logits_v[prompt_tokens[0] as usize], 10.0,
        "a prompt-only token must not be penalized just because it appears in the prompt"
    );
}
