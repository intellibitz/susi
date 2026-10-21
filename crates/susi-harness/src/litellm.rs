//! LiteLLM as the model proxy.
//!
//! The harness gateway stays the control point: it attributes every call to a
//! mission and task, prices it and writes the ledger. LiteLLM sits behind it
//! and owns the provider layer: one OpenAI-compatible surface over DeepSeek,
//! SiliconFlow, DashScope and Gemini, with response caching, so a repeated
//! request costs nothing and every request still passes through the meter.

use std::collections::BTreeSet;
use std::path::Path;

/// Default config path, relative to the repository root.
pub const DEFAULT_CONFIG: &str = "deploy/litellm/config.yaml";

/// Names referenced as `os.environ/NAME` in a LiteLLM config.
///
/// The launcher warns about each one that is unset instead of starting a proxy
/// whose first call fails with an authentication error.
#[must_use]
pub fn environment_names(text: &str) -> BTreeSet<String> {
    let marker = "os.environ/";
    let mut names = BTreeSet::new();
    for (index, _) in text.match_indices(marker) {
        let rest = &text[index + marker.len()..];
        let name: String = rest
            .chars()
            .take_while(|character| character.is_ascii_alphanumeric() || *character == '_')
            .collect();
        if !name.is_empty() {
            names.insert(name);
        }
    }
    names
}

/// The `litellm` command line for one proxy.
#[must_use]
pub fn argv(config: &Path, host: &str, port: u16) -> Vec<String> {
    vec![
        "--config".to_string(),
        config.display().to_string(),
        "--host".to_string(),
        host.to_string(),
        "--port".to_string(),
        port.to_string(),
    ]
}

/// LiteLLM's liveness endpoint.
#[must_use]
pub fn readiness_url(host: &str, port: u16) -> String {
    format!("http://{host}:{port}/health/liveliness")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_config_names_every_environment_variable_once() {
        let text = r#"
model_list:
  - model_name: chat
    litellm_params:
      api_key: os.environ/DEEPSEEK_API_KEY
  - model_name: qwen
    litellm_params:
      api_key: os.environ/SILICONFLOW_API_KEY
      api_base: https://api.siliconflow.cn/v1
  - model_name: qwen-plus
    litellm_params:
      api_key: os.environ/DASHSCOPE_API_KEY
  - model_name: again
    litellm_params:
      api_key: os.environ/DEEPSEEK_API_KEY
"#;
        let names = environment_names(text);
        assert_eq!(
            names.into_iter().collect::<Vec<_>>(),
            vec![
                "DASHSCOPE_API_KEY".to_string(),
                "DEEPSEEK_API_KEY".to_string(),
                "SILICONFLOW_API_KEY".to_string()
            ]
        );
    }

    #[test]
    fn the_command_line_names_the_config_host_and_port() {
        let argv = argv(Path::new("deploy/litellm/config.yaml"), "127.0.0.1", 4000);
        assert_eq!(
            argv,
            vec![
                "--config".to_string(),
                "deploy/litellm/config.yaml".to_string(),
                "--host".to_string(),
                "127.0.0.1".to_string(),
                "--port".to_string(),
                "4000".to_string()
            ]
        );
    }

    #[test]
    fn readiness_is_checked_on_the_liveness_endpoint() {
        assert_eq!(
            readiness_url("127.0.0.1", 4000),
            "http://127.0.0.1:4000/health/liveliness"
        );
    }

    #[test]
    fn a_config_without_environment_references_names_none() {
        assert!(environment_names("model_list: []").is_empty());
    }
}
