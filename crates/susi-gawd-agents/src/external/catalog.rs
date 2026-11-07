use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use susi_error::{eai_bail as bail, EaiResult as Result, ResultExt as Context};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogKind {
    /// Coding executors admitted under an Apache-2.0 or permissive license.
    Execution,
    /// Top agent frameworks / engines (LangGraph, CrewAI, …).
    Framework,
}

impl CatalogKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Execution => "agent",
            Self::Framework => "framework",
        }
    }

    pub fn list_hint(self) -> &'static str {
        match self {
            Self::Execution => "`susi agents list`",
            Self::Framework => "`susi frameworks list`",
        }
    }

    pub fn config_subdir(self) -> &'static str {
        match self {
            Self::Execution => "execution-agents",
            Self::Framework => "agent-engines",
        }
    }

    pub fn run_subdir(self) -> &'static str {
        match self {
            Self::Execution => "execution-agents",
            Self::Framework => "agent-engines",
        }
    }

    /// Logical file name of this catalog inside an extension pack.
    pub fn pack_file(self) -> &'static str {
        match self {
            Self::Execution => "execution-agents.json",
            Self::Framework => "agent-engines.json",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Adapter {
    Command {
        program: String,
        args: Vec<String>,
    },
    /// Remote A2A-compliant agent, discovered from `<url>/.well-known/agent-card.json`.
    A2a {
        url: String,
        /// Env var holding a bearer token, when the remote agent requires one.
        #[serde(default)]
        token_env: Option<String>,
    },
    /// Agent Client Protocol agent spoken to over stdio (JSON-RPC).
    Acp {
        program: String,
        args: Vec<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDefinition {
    pub id: String,
    pub name: String,
    pub peer_name: String,
    /// User-supplied catalog order, not a measured quality score.
    pub rank: u8,
    pub documentation: String,
    pub adapter: Adapter,
    /// Mandate 35: preserve unknown catalog keys from extension packs.
    #[serde(flatten, default)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

/// The installed catalog. Core bundles no vendor agents (zero-UI plan A9):
/// entries come from the active extension pack's `execution-agents.json` /
/// `agent-engines.json`, then from adapter files the host (or the commercial
/// desktop) wrote as `<config_dir>/<execution-agents|agent-engines>/<id>.json`
/// for ids the pack does not define. With neither, the catalog is empty.
pub fn catalog(kind: CatalogKind) -> Result<Vec<AgentDefinition>> {
    catalog_with_host_dir(
        kind,
        &susi_paths::SusiDirs::config_dir().join(kind.config_subdir()),
    )
}

/// [`catalog`] reading host adapter files from `host_dir`.
pub fn catalog_with_host_dir(kind: CatalogKind, host_dir: &Path) -> Result<Vec<AgentDefinition>> {
    let mut entries: Vec<AgentDefinition> =
        crate::susi_sandbox::extensions::load_json_or_bundled(kind.pack_file(), "[]");
    for id in crate::susi_sandbox::extensions::host_adapter_ids(host_dir) {
        if entries.iter().any(|a| a.id == id || a.peer_name == id) {
            continue;
        }
        let Ok(bytes) = std::fs::read(host_dir.join(format!("{id}.json"))) else {
            continue;
        };
        let Ok(adapter) = serde_json::from_slice::<Adapter>(&bytes) else {
            continue;
        };
        entries.push(AgentDefinition {
            id: id.clone(),
            name: id.clone(),
            peer_name: id,
            // Host-written adapters rank after every pack entry.
            rank: u8::MAX,
            documentation: String::new(),
            adapter,
            extra: std::collections::HashMap::new(),
        });
    }
    Ok(entries)
}

pub fn definition(kind: CatalogKind, id: &str) -> Result<AgentDefinition> {
    catalog(kind)?
        .into_iter()
        .find(|a| a.id == id || a.peer_name == id)
        .with_context(|| format!("unknown {} {id}; use {}", kind.label(), kind.list_hint()))
}

/// Resolve a managed peer name against executors first, then frameworks.
pub fn resolve_managed(id: &str) -> Result<(CatalogKind, AgentDefinition)> {
    if let Ok(def) = definition(CatalogKind::Execution, id) {
        return Ok((CatalogKind::Execution, def));
    }
    definition(CatalogKind::Framework, id).map(|def| (CatalogKind::Framework, def))
}

impl Adapter {
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Command { program, args } => {
                if program.trim().is_empty()
                    || program.contains('\0')
                    || args.iter().any(|a| a.contains('\0'))
                {
                    bail!("command and arguments must be nonempty executable / NUL-free argv");
                }
            }
            Self::A2a { url, token_env } => {
                if !(url.starts_with("https://") || url.starts_with("http://"))
                    || url.contains('\0')
                {
                    bail!("A2A url must be an http(s) URL");
                }
                if token_env.as_deref().is_some_and(|e| {
                    e.is_empty() || !e.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                }) {
                    bail!("token_env must name an environment variable, not contain a credential");
                }
            }
            Self::Acp { program, args } => {
                if program.trim().is_empty()
                    || program.contains('\0')
                    || args.iter().any(|a| a.contains('\0'))
                {
                    bail!("ACP command and arguments must be nonempty executable / NUL-free argv");
                }
            }
        }
        Ok(())
    }

    /// Local preflight only. Presence of a binary/package does not prove vendor authentication.
    pub fn preflight(&self) -> Result<String> {
        self.validate()?;
        match self {
            Self::Command { program, .. } => {
                resolve_program(program)
                    .with_context(|| format!("missing executable: {program}"))?;
                Ok("executable found; vendor login/permissions must be configured".into())
            }
            Self::A2a { url, token_env } => {
                if let Some(env) = token_env {
                    if crate::susi_config::env_or_cloud_env(env)
                        .unwrap_or_default()
                        .trim()
                        .is_empty()
                    {
                        bail!("set {env} for the remote A2A agent's bearer token");
                    }
                }
                Ok(format!(
                    "A2A endpoint {url}; agent card is fetched at execution"
                ))
            }
            Self::Acp { program, .. } => {
                resolve_program(program)
                    .with_context(|| format!("missing ACP executable: {program}"))?;
                Ok(
                    "ACP executable found; agent auth is checked by the initialize handshake"
                        .into(),
                )
            }
        }
    }

    pub fn is_cloud(&self) -> bool {
        matches!(self, Self::A2a { .. })
    }
}

pub fn resolve_program(program: &str) -> Option<PathBuf> {
    fn executable(p: &Path) -> bool {
        if !p.is_file() {
            return false;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            p.metadata()
                .is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
        }
        #[cfg(not(unix))]
        true
    }
    let p = Path::new(program);
    if p.components().count() > 1 {
        return executable(p).then(|| p.to_path_buf());
    }
    for dir in std::env::split_paths(&std::env::var_os("PATH")?) {
        // Never silently discover a repo-supplied binary via a relative PATH entry.
        if !dir.is_absolute() {
            continue;
        }
        let p = dir.join(program);
        if executable(&p) {
            return Some(p);
        }
        #[cfg(windows)]
        for ext in ["exe", "cmd", "bat"] {
            let p = dir.join(format!("{program}.{ext}"));
            if executable(&p) {
                return Some(p);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_kind_metadata_matches_execution_and_framework_contracts() {
        assert_eq!(CatalogKind::Execution.label(), "agent");
        assert_eq!(CatalogKind::Framework.label(), "framework");
        assert_eq!(CatalogKind::Execution.list_hint(), "`susi agents list`");
        assert_eq!(CatalogKind::Framework.list_hint(), "`susi frameworks list`");
        assert_eq!(CatalogKind::Execution.config_subdir(), "execution-agents");
        assert_eq!(CatalogKind::Framework.config_subdir(), "agent-engines");
        assert_eq!(CatalogKind::Execution.run_subdir(), "execution-agents");
        assert_eq!(CatalogKind::Framework.run_subdir(), "agent-engines");
        assert_eq!(CatalogKind::Execution.pack_file(), "execution-agents.json");
        assert_eq!(CatalogKind::Framework.pack_file(), "agent-engines.json");
    }

    #[test]
    fn adapter_validate_and_is_cloud_check_command_acp_and_a2a() {
        let valid_cmd = Adapter::Command {
            program: "aider".into(),
            args: vec!["--yes".into(), "--message".into()],
        };
        assert!(valid_cmd.validate().is_ok());
        assert!(!valid_cmd.is_cloud());

        let valid_acp = Adapter::Acp {
            program: "codex-acp".into(),
            args: vec!["--stdio".into()],
        };
        assert!(valid_acp.validate().is_ok());
        assert!(!valid_acp.is_cloud());

        let valid_a2a_http = Adapter::A2a {
            url: "http://127.0.0.1:8080/agent".into(),
            token_env: None,
        };
        assert!(valid_a2a_http.validate().is_ok());
        assert!(valid_a2a_http.is_cloud());

        let valid_a2a_https = Adapter::A2a {
            url: "https://agents.example.com/a2a".into(),
            token_env: Some("A2A_BEARER_TOKEN_1".into()),
        };
        assert!(valid_a2a_https.validate().is_ok());
        assert!(valid_a2a_https.is_cloud());

        for bad in [
            Adapter::Command {
                program: "   ".into(),
                args: vec![],
            },
            Adapter::Command {
                program: "bad\0bin".into(),
                args: vec![],
            },
            Adapter::Command {
                program: "ok".into(),
                args: vec!["arg\0nul".into()],
            },
            Adapter::Acp {
                program: String::new(),
                args: vec![],
            },
            Adapter::Acp {
                program: "acp\0bin".into(),
                args: vec![],
            },
            Adapter::Acp {
                program: "acp".into(),
                args: vec!["bad\0arg".into()],
            },
            Adapter::A2a {
                url: "ftp://example.com/agent".into(),
                token_env: None,
            },
            Adapter::A2a {
                url: "https://example.com/\0bad".into(),
                token_env: None,
            },
            Adapter::A2a {
                url: "https://example.com/a2a".into(),
                token_env: Some(String::new()),
            },
            Adapter::A2a {
                url: "https://example.com/a2a".into(),
                token_env: Some("Bearer secret".into()),
            },
            Adapter::A2a {
                url: "https://example.com/a2a".into(),
                token_env: Some("KEY=val".into()),
            },
        ] {
            assert!(bad.validate().is_err(), "expected invalid adapter: {bad:?}");
        }
    }

    #[test]
    fn catalog_with_host_dir_loads_valid_host_adapters_and_skips_malformed_json() {
        let temp_dir = std::env::temp_dir().join(format!(
            "susi-catalog-host-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp_dir).unwrap();

        std::fs::write(
            temp_dir.join("custom-exec.json"),
            r#"{"kind":"command","program":"custom-agent","args":["--run"]}"#,
        )
        .unwrap();
        std::fs::write(temp_dir.join("broken.json"), b"{not valid json").unwrap();

        let entries = catalog_with_host_dir(CatalogKind::Execution, &temp_dir).unwrap();
        let _ = std::fs::remove_dir_all(&temp_dir);

        let custom = entries
            .iter()
            .find(|entry| entry.id == "custom-exec")
            .expect("custom-exec host adapter should be loaded");
        assert_eq!(custom.name, "custom-exec");
        assert_eq!(custom.peer_name, "custom-exec");
        assert_eq!(custom.rank, u8::MAX);
        assert!(matches!(
            &custom.adapter,
            Adapter::Command { program, args }
                if program == "custom-agent" && args == &vec!["--run".to_string()]
        ));
        assert!(!entries.iter().any(|entry| entry.id == "broken"));
    }

    #[test]
    fn catalog_with_host_dir_loads_framework_acp_and_a2a_entries_and_skips_invalid_files() {
        let temp_dir = std::env::temp_dir().join(format!(
            "susi-catalog-framework-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp_dir).unwrap();

        std::fs::write(
            temp_dir.join("langgraph.json"),
            r#"{"kind":"acp","program":"langgraph-acp","args":["--stdio"]}"#,
        )
        .unwrap();
        std::fs::write(
            temp_dir.join("autogen.json"),
            r#"{"kind":"a2a","url":"https://example.com/a2a","token_env":"AUTOGEN_TOKEN"}"#,
        )
        .unwrap();
        std::fs::write(
            temp_dir.join("bad.json"),
            r#"{"kind":"unknown_adapter_kind"}"#,
        )
        .unwrap();
        std::fs::write(temp_dir.join("README.txt"), b"ignore non-json files").unwrap();

        let entries = catalog_with_host_dir(CatalogKind::Framework, &temp_dir).unwrap();
        let _ = std::fs::remove_dir_all(&temp_dir);

        let autogen = entries
            .iter()
            .find(|e| e.id == "autogen")
            .expect("autogen entry loaded");
        assert!(autogen.adapter.validate().is_ok());
        assert!(autogen.adapter.is_cloud());

        let langgraph = entries
            .iter()
            .find(|e| e.id == "langgraph")
            .expect("langgraph entry loaded");
        assert!(langgraph.adapter.validate().is_ok());
        assert!(!langgraph.adapter.is_cloud());

        assert!(!entries.iter().any(|e| e.id == "bad"));
        assert!(!entries.iter().any(|e| e.id == "README"));
        assert!(entries
            .iter()
            .any(|e| e.id.eq_ignore_ascii_case("LANGGRAPH")));
        assert!(!entries
            .iter()
            .any(|e| e.id.eq_ignore_ascii_case("nonexistent")));
    }

    #[test]
    fn resolve_program_rejects_missing_executable_and_a2a_preflight_succeeds_without_token_env() {
        assert!(resolve_program("/nonexistent/susi/missing-agent-binary").is_none());
        let a2a = Adapter::A2a {
            url: "https://example.com/a2a".into(),
            token_env: None,
        };
        let msg = a2a
            .preflight()
            .expect("a2a without token_env should pass preflight");
        assert!(msg.contains("https://example.com/a2a"));
    }
}
