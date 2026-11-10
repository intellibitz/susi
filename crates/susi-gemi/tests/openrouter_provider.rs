#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // Test assertions.
//! `provider()` follows `doctor()`. Both resolve the OpenRouter key and the
//! endpoint from the process environment and the config under HOME, so this
//! test owns its process (an integration test binary of its own) and an
//! isolated home with a fixed test key. As an inline unit test it raced
//! sibling tests that moved HOME between the two calls, and it read the
//! operator's real key store.
use susi_gemi::engines::http_provider::InferenceProtocol;
use susi_gemi::openrouter_ext::provider;
use susi_gemi_models::openrouter::{OpenRouterManager, API_BASE};

#[test]
fn provider_reflects_manager_doctor_state_and_formats_provider_name() {
    let home = tempfile::tempdir().unwrap();
    let mut env = susi_paths::test_env::EnvGuard::isolated();
    env.set("HOME", home.path())
        .set("SUSI_HOME", home.path().join(".susi"))
        .set("OPENROUTER_API_KEY", "sk-or-test-not-a-real-key");
    let manager = OpenRouterManager::new().expect("manager init");
    match manager.doctor() {
        Ok(_) => {
            let built = provider(&manager).expect("provider when doctor succeeds");
            assert_eq!(built.api_base, API_BASE);
            assert!(built.name.starts_with("openrouter-"));
            assert_eq!(built.protocol, InferenceProtocol::OpenAiChat);
        }
        Err(_) => assert!(provider(&manager).is_err()),
    }
    // Without a key, doctor and provider both refuse.
    env.remove("OPENROUTER_API_KEY");
    assert!(manager.doctor().is_err());
    assert!(provider(&manager).is_err());
}
