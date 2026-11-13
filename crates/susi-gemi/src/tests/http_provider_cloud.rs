use super::*;
use crate::susi_core::registry::CapabilityRegistry;

#[test]
fn upsert_and_register_roundtrip_in_temp_cloud_env() {
    let _guard = crate::engines::env_test_lock();
    let isolated = IsolatedCloudHome::new("susi_key_reg");
    // A key in the developer's shell env would (correctly) win over
    // cloud.env; clear it for the duration of the test.
    let prev_key = std::env::var_os("DEEPSEEK_API_KEY");
    unsafe {
        std::env::remove_var("DEEPSEEK_API_KEY");
    }

    let msg = register_api_key("deepseek", "sk-test-deepseek").unwrap();
    assert!(msg.contains("DEEPSEEK_API_KEY"));
    let path = cloud_env_path();
    assert!(
        path.starts_with(&isolated.dir),
        "cloud.env must land under isolated home, got {}",
        path.display()
    );
    assert!(path.exists());
    let body = std::fs::read_to_string(&path).unwrap();
    assert!(body.contains("DEEPSEEK_API_KEY=sk-test-deepseek"));
    // Visible to lookups, never copied into the process environment.
    assert_eq!(
        crate::susi_config::env_or_cloud_env("DEEPSEEK_API_KEY").unwrap(),
        "sk-test-deepseek"
    );
    assert!(std::env::var_os("DEEPSEEK_API_KEY").is_none());

    remove_api_key("deepseek").unwrap();
    let body = std::fs::read_to_string(cloud_env_path()).unwrap_or_default();
    assert!(!body.contains("DEEPSEEK_API_KEY"));
    assert!(crate::susi_config::env_or_cloud_env("DEEPSEEK_API_KEY").is_err());
    if let Some(v) = prev_key {
        unsafe {
            std::env::set_var("DEEPSEEK_API_KEY", v);
        }
    }
    drop(isolated);
}

#[test]
fn register_configured_respects_api_key_presence() {
    let _guard = crate::engines::env_test_lock();
    let _isolated = IsolatedCloudHome::new("susi_key_cfg");
    unsafe {
        std::env::remove_var("OPENAI_API_KEY");
        std::env::remove_var("ANTHROPIC_API_KEY");
        std::env::remove_var("GEMINI_API_KEY");
        std::env::remove_var("GOOGLE_API_KEY");
        std::env::remove_var("DEEPSEEK_API_KEY");
        std::env::remove_var("MOONSHOT_API_KEY");
        std::env::remove_var("KIMI_API_KEY");
        std::env::remove_var("MINIMAX_API_KEY");
        std::env::remove_var("OPENROUTER_API_KEY");
    }

    let registry = CapabilityRegistry::new();
    register_configured_cloud_endpoints(&registry);
    let cloudish: Vec<_> = registry
        .list_providers()
        .into_iter()
        .filter(|n| is_bundled_cloud_prefix(n))
        .collect();
    assert!(
        cloudish.is_empty(),
        "must not register cloud providers without keys: {:?}",
        cloudish
    );

    unsafe {
        std::env::set_var("OPENAI_API_KEY", "sk-unit-test");
        std::env::set_var("DEEPSEEK_API_KEY", "sk-deepseek-test");
    }
    register_configured_cloud_endpoints(&registry);
    let providers = registry.list_providers();
    assert!(
        providers.iter().any(|n| n.starts_with("openai-")),
        "expected openai-* provider, got {:?}",
        providers
    );
    assert!(
        providers.iter().any(|n| n.starts_with("deepseek-")),
        "expected deepseek-* OpenAI-compat provider, got {:?}",
        providers
    );
    let openai = providers.iter().find(|n| n.starts_with("openai-")).unwrap();
    let p = registry.get_provider(openai).unwrap();
    let http = p.as_any().downcast_ref::<HttpProvider>().unwrap();
    assert_eq!(http.protocol, InferenceProtocol::OpenAiChat);
    assert_eq!(http.api_key, "sk-unit-test");
    assert!(!http.model.is_empty());
    unsafe {
        std::env::remove_var("OPENAI_API_KEY");
        std::env::remove_var("DEEPSEEK_API_KEY");
    }
}

/// Redirect HOME + XDG_* so cloud.env never reads the developer machine.
/// Also scrubs `SUSI_HOME`/`SUSI_PORT_OFFSET`, which outrank HOME/XDG.
struct IsolatedCloudHome {
    dir: std::path::PathBuf,
    env: Option<susi_paths::test_env::EnvGuard>,
}

impl IsolatedCloudHome {
    fn new(prefix: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("{}_{}", prefix, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let xdg_config = dir.join("config");
        let xdg_data = dir.join("data");
        std::fs::create_dir_all(&xdg_config).unwrap();
        std::fs::create_dir_all(&xdg_data).unwrap();
        let mut env = susi_paths::test_env::EnvGuard::isolated();
        env.set("HOME", &dir);
        env.set("XDG_CONFIG_HOME", &xdg_config);
        env.set("XDG_DATA_HOME", &xdg_data);
        // Provider URLs are independent of HOME. The test's fake keys must
        // never be sent to an inherited developer proxy or custom endpoint.
        for name in [
            "OPENAI_API_BASE",
            "OPENAI_BASE_URL",
            "OPENAI_API_BASE_URL",
            "DEEPSEEK_API_BASE",
            "DEEPSEEK_BASE_URL",
            "DEEPSEEK_API_BASE_URL",
        ] {
            env.remove(name);
        }
        Self {
            dir,
            env: Some(env),
        }
    }
}

impl Drop for IsolatedCloudHome {
    fn drop(&mut self) {
        self.env = None;
        std::env::remove_var("DEEPSEEK_API_KEY");
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn is_bundled_cloud_prefix(n: &str) -> bool {
    n.starts_with("openai-")
        || n.starts_with("anthropic-")
        || n.starts_with("googlegemini-")
        || n.starts_with("deepseek-")
        || n.starts_with("kimi-")
        || n.starts_with("minimax-")
        || n.starts_with("openrouter-")
}
