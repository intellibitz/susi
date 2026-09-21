//! Pluggable external coding-agent peers (Claude Code, Cursor, Codex, Devin, OpenHands).
//!
//! Drivers are config-declared; when the binary/API key is missing the agent
//! returns UNAVAILABLE rather than inventing work. Successful invokes are
//! captured into the live EvidenceSession ledger.

use crate::agents::{GawdAgent, MissionBlackboard};
use crate::security::SecurityDetector;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};
use susi_core::capture::EvidenceSession;
use susi_core::registry::{AgentCapability, CapabilityRegistry};
use susi_error::{EaiError, EaiResult};
use susi_sandbox::manager::{ExternalPeerAgentSpec, SusiConfig};

/// Resolve the live driver binary for a peer spec.
pub fn resolve_driver(spec: &ExternalPeerAgentSpec) -> Option<PathBuf> {
    if !spec.command.trim().is_empty() {
        if which_bin(spec.command.trim()).is_some() {
            return Some(PathBuf::from(spec.command.trim()));
        }
    }
    for bin in &spec.detect_bins {
        if let Some(path) = which_bin(bin) {
            return Some(path);
        }
    }
    None
}

fn which_bin(name: &str) -> Option<PathBuf> {
    if name.contains('/') || name.contains('\\') {
        let p = PathBuf::from(name);
        return p.exists().then_some(p);
    }
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
        #[cfg(windows)]
        {
            let with_exe = dir.join(format!("{name}.exe"));
            if with_exe.is_file() {
                return Some(with_exe);
            }
        }
    }
    None
}

fn api_key_ready(spec: &ExternalPeerAgentSpec) -> bool {
    match &spec.api_key_env {
        None => true,
        Some(env_name) if env_name.trim().is_empty() => true,
        Some(env_name) => std::env::var_os(env_name).is_some_and(|v| !v.is_empty()),
    }
}

fn render_args(spec: &ExternalPeerAgentSpec, goal: &str, workspace: &Path) -> Vec<String> {
    let ws = workspace.display().to_string();
    spec.args
        .iter()
        .map(|a| a.replace("{goal}", goal).replace("{workspace}", &ws))
        .collect()
}

fn run_peer_process(
    bin: &Path,
    args: &[String],
    workspace: &Path,
    timeout: Duration,
) -> EaiResult<String> {
    let bin = bin.to_path_buf();
    let bin_label = bin.display().to_string();
    let args = args.to_vec();
    let workspace = workspace.to_path_buf();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = Command::new(&bin)
            .args(&args)
            .current_dir(&workspace)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output();
        let _ = tx.send(result);
    });
    match rx.recv_timeout(timeout) {
        Ok(Ok(output)) => {
            let mut text = String::new();
            if !output.stdout.is_empty() {
                text.push_str(&String::from_utf8_lossy(&output.stdout));
            }
            if !output.stderr.is_empty() {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&String::from_utf8_lossy(&output.stderr));
            }
            let text = SecurityDetector::redact(&text);
            if output.status.success() {
                Ok(text)
            } else {
                Err(EaiError::process(format!(
                    "external peer exited {}: {text}",
                    output.status
                )))
            }
        }
        Ok(Err(e)) => Err(EaiError::process(format!(
            "external peer spawn failed: {e}"
        ))),
        Err(_) => Err(EaiError::process(format!(
            "external peer timed out after {}s ({bin_label})",
            timeout.as_secs()
        ))),
    }
}

pub struct ExternalPeerAgent {
    pub spec: ExternalPeerAgentSpec,
}

impl GawdAgent for ExternalPeerAgent {
    fn name(&self) -> String {
        self.spec.name.clone()
    }

    fn rank(&self) -> f32 {
        0.92
    }

    fn execute(
        &self,
        goal: &str,
        workspace: &Path,
        blackboard: &MissionBlackboard,
    ) -> EaiResult<String> {
        if !api_key_ready(&self.spec) {
            let env = self.spec.api_key_env.clone().unwrap_or_default();
            let msg = format!(
                "[UNAVAILABLE] {}: required env `{env}` is not set. Install/configure the peer driver to plug it into susi.",
                self.spec.name
            );
            blackboard.insert(self.name(), msg.clone());
            return Err(EaiError::governance(msg));
        }

        let Some(bin) = resolve_driver(&self.spec) else {
            let bins = self.spec.detect_bins.join(", ");
            let msg = format!(
                "[UNAVAILABLE] {}: no driver on PATH (tried: {bins}). Install the peer CLI to plug it into susi.",
                self.spec.name
            );
            blackboard.insert(self.name(), msg.clone());
            return Err(EaiError::governance(msg));
        };

        let args = render_args(&self.spec, goal, workspace);
        let timeout = Duration::from_secs(if self.spec.timeout_secs == 0 {
            600
        } else {
            self.spec.timeout_secs
        });
        let tool = format!("external_peer:{}", self.spec.name);
        let arguments = serde_json::json!({
            "bin": bin.display().to_string(),
            "args": args,
            "goal": goal,
        });

        let result = EvidenceSession::capture_call(&tool, &arguments, workspace, || {
            run_peer_process(&bin, &args, workspace, timeout)
        })?;

        let rendered = format!(
            "[{}]: peer driver `{}` completed.\n{}",
            self.spec.name,
            bin.display(),
            result
        );
        blackboard.insert(self.name(), rendered.clone());
        Ok(rendered)
    }
}

/// Register config-declared external peers into the native agent factory table
/// and CapabilityRegistry catalog.
pub fn register_external_peer_factories(registry: &susi_core::registry::DynamicServiceRegistry) {
    let specs = SusiConfig::load_global()
        .unwrap_or_default()
        .external_peer_agents();
    let caps = CapabilityRegistry::global();
    for spec in specs {
        if spec.name.trim().is_empty() {
            continue;
        }
        caps.register_agent_capability(AgentCapability {
            name: spec.name.clone(),
            description: spec.description.clone(),
            is_core: false,
        });
        let name = spec.name.clone();
        let spec_for_factory = spec.clone();
        registry.register_factory(name, move || {
            Arc::new(Arc::new(ExternalPeerAgent {
                spec: spec_for_factory.clone(),
            }) as Arc<dyn GawdAgent>)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::HighDensityContextStore;

    #[test]
    fn unavailable_when_driver_missing() {
        let agent = ExternalPeerAgent {
            spec: ExternalPeerAgentSpec {
                name: "MissingPeerAgent".into(),
                description: "test".into(),
                detect_bins: vec!["susi-definitely-missing-peer-bin-xyz".into()],
                command: String::new(),
                args: vec!["{goal}".into()],
                timeout_secs: 5,
                api_key_env: None,
            },
        };
        let board: MissionBlackboard = Arc::new(HighDensityContextStore::new(8));
        let err = agent
            .execute("do work", Path::new("."), &board)
            .unwrap_err();
        assert!(err.to_string().contains("UNAVAILABLE"));
    }

    #[test]
    fn unavailable_when_api_key_missing() {
        let agent = ExternalPeerAgent {
            spec: ExternalPeerAgentSpec {
                name: "KeyGatedPeer".into(),
                description: "test".into(),
                detect_bins: vec!["echo".into()],
                command: "echo".into(),
                args: vec!["{goal}".into()],
                timeout_secs: 5,
                api_key_env: Some("SUSI_TEST_PEER_KEY_NOT_SET_EVER".into()),
            },
        };
        let board: MissionBlackboard = Arc::new(HighDensityContextStore::new(8));
        let err = agent
            .execute("do work", Path::new("."), &board)
            .unwrap_err();
        assert!(err.to_string().contains("UNAVAILABLE"));
        assert!(err.to_string().contains("SUSI_TEST_PEER_KEY_NOT_SET_EVER"));
    }

    #[test]
    fn echo_driver_captures_output() {
        let echo = which_bin("echo").expect("echo must exist on PATH for this test");
        let agent = ExternalPeerAgent {
            spec: ExternalPeerAgentSpec {
                name: "EchoPeerAgent".into(),
                description: "test".into(),
                detect_bins: vec!["echo".into()],
                command: echo.display().to_string(),
                args: vec!["peer-ok:{goal}".into()],
                timeout_secs: 5,
                api_key_env: None,
            },
        };
        let board: MissionBlackboard = Arc::new(HighDensityContextStore::new(8));
        let ws = std::env::temp_dir().join(format!(
            "susi_peer_echo_{}_{}",
            std::process::id(),
            Instant::now().elapsed().as_nanos()
        ));
        let _ = std::fs::create_dir_all(&ws);
        let session =
            EvidenceSession::new("peer test", &ws, SecurityDetector::redact).expect("session");
        let _activation = EvidenceSession::activate(&session);
        let res = agent.execute("hello", &ws, &board).expect("echo peer");
        assert!(res.contains("peer-ok:hello"), "{res}");
        assert!(!session.receipts().is_empty());
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn bundled_peers_are_five_named_leaders() {
        let specs = SusiConfig::default().external_peer_agents();
        let names: Vec<_> = specs.iter().map(|s| s.name.as_str()).collect();
        for expected in [
            "ClaudeCodeAgent",
            "CursorAgent",
            "CodexAgent",
            "DevinAgent",
            "OpenHandsAgent",
        ] {
            assert!(names.contains(&expected), "missing {expected} in {names:?}");
        }
    }

    #[test]
    fn peer_factories_instantiate() {
        let agent = crate::agents::instantiate_native_agent("ClaudeCodeAgent")
            .expect("ClaudeCodeAgent factory must register");
        assert_eq!(agent.name(), "ClaudeCodeAgent");
        let agent = crate::agents::instantiate_native_agent("OpenHandsAgent")
            .expect("OpenHandsAgent factory must register");
        assert_eq!(agent.name(), "OpenHandsAgent");
    }
}
