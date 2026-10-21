//! LiteLLM as the model proxy.
//!
//! The harness gateway stays the control point: it attributes every call to a
//! mission and task, prices it and writes the ledger. LiteLLM sits behind it
//! and owns the provider layer: one OpenAI-compatible surface over DeepSeek,
//! SiliconFlow, DashScope and Gemini, with response caching, so a repeated
//! request costs nothing and every request still passes through the meter.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use susi_error::{EaiError, EaiResult};

/// Default config path, relative to the repository root.
pub const DEFAULT_CONFIG: &str = "deploy/litellm/config.yaml";

/// How to start the proxy.
pub struct Launcher {
    /// LiteLLM config file.
    pub config: PathBuf,
    /// Interface to bind.
    pub host: String,
    /// Port to bind.
    pub port: u16,
    /// The `litellm` executable.
    pub program: PathBuf,
    /// Where the log lives.
    pub state_dir: PathBuf,
    /// How long to wait for liveness.
    pub ready_timeout: Duration,
}

impl Launcher {
    /// Environment names the config references but the process does not set.
    ///
    /// # Errors
    ///
    /// Returns `EaiError::io` when the config cannot be read.
    pub fn missing_keys(&self) -> EaiResult<Vec<String>> {
        let text = std::fs::read_to_string(&self.config)
            .map_err(|error| EaiError::io(format!("read {}: {error}", self.config.display())))?;
        Ok(environment_names(&text)
            .into_iter()
            .filter(|name| {
                std::env::var(name)
                    .map(|value| value.trim().is_empty())
                    .unwrap_or(true)
            })
            .collect())
    }

    /// Start the proxy and wait until its liveness endpoint answers.
    ///
    /// The child keeps writing to `<state_dir>/litellm.log`; the caller owns
    /// waiting for it and should keep it alive with its own lifetime.
    ///
    /// # Errors
    ///
    /// Returns `EaiError::io` when the log cannot be created and
    /// `EaiError::process` when the child cannot start or never becomes ready.
    pub fn launch(&self) -> EaiResult<Child> {
        std::fs::create_dir_all(&self.state_dir).map_err(|error| {
            EaiError::io(format!("create {}: {error}", self.state_dir.display()))
        })?;
        let log_path = self.state_dir.join("litellm.log");
        let log = std::fs::File::create(&log_path)
            .map_err(|error| EaiError::io(format!("create {}: {error}", log_path.display())))?;
        let stderr = log
            .try_clone()
            .map_err(|error| EaiError::io(format!("clone {}: {error}", log_path.display())))?;
        let mut child = Command::new(&self.program)
            .args(argv(&self.config, &self.host, self.port))
            // LiteLLM prints model names as UTF-8; without this, a redirected
            // Windows console defaults to cp1252 and startup dies on the first
            // non-ASCII character.
            .env("PYTHONIOENCODING", "utf-8")
            .env("PYTHONUTF8", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(stderr))
            .spawn()
            .map_err(|error| {
                EaiError::process(format!("spawn {}: {error}", self.program.display()))
            })?;
        self.wait_ready(&mut child, &log_path)?;
        Ok(child)
    }

    fn wait_ready(&self, child: &mut Child, log_path: &Path) -> EaiResult<()> {
        let url = readiness_url(&self.host, self.port);
        let deadline = Instant::now() + self.ready_timeout;
        let mut last = String::from("no attempt yet");
        while Instant::now() < deadline {
            if let Some(status) = child
                .try_wait()
                .map_err(|error| EaiError::process(format!("poll litellm: {error}")))?
            {
                return Err(EaiError::process(format!(
                    "litellm exited before it was ready ({status}); see {}",
                    log_path.display()
                )));
            }
            match susi_http_transport::http_call("GET", &url, &[], 2, 0) {
                Ok(response) if (200..300).contains(&response.status) => return Ok(()),
                Ok(response) => last = format!("status {}", response.status),
                Err(error) => last = error,
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        Err(EaiError::process(format!(
            "litellm was not ready after {} s ({last})",
            self.ready_timeout.as_secs()
        )))
    }
}

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

    #[test]
    fn missing_keys_lists_what_the_environment_lacks() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.yaml");
        std::fs::write(
            &config,
            "model_list:\n  - litellm_params:\n      api_key: os.environ/SUSI_HARNESS_TEST_UNSET_KEY\n",
        )
        .unwrap();
        let launcher = Launcher {
            config,
            host: "127.0.0.1".into(),
            port: 0,
            program: "litellm".into(),
            state_dir: dir.path().join("state"),
            ready_timeout: Duration::from_secs(1),
        };
        assert_eq!(
            launcher.missing_keys().unwrap(),
            vec!["SUSI_HARNESS_TEST_UNSET_KEY".to_string()]
        );
    }
}
