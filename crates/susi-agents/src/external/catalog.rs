use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Adapter {
    Command { program: String, args: Vec<String> },
    Qwen { python: String },
    Devin { api_key_env: String },
    Manus { api_key_env: String },
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
}

pub fn catalog() -> Result<Vec<AgentDefinition>> {
    serde_json::from_str(include_str!("../../../../config/execution-agents.json"))
        .context("invalid bundled execution-agent catalog")
}

pub fn definition(id: &str) -> Result<AgentDefinition> {
    catalog()?
        .into_iter()
        .find(|a| a.id == id || a.peer_name == id)
        .with_context(|| format!("unknown agent {id}; use `susi agents list`"))
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
            Self::Qwen { python } if python.trim().is_empty() => {
                bail!("python executable is required")
            }
            Self::Devin { api_key_env } | Self::Manus { api_key_env }
                if api_key_env.is_empty()
                    || !api_key_env
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_') =>
            {
                bail!("api_key_env must name an environment variable, not contain a credential");
            }
            Self::Devin { .. } | Self::Manus { .. } | Self::Qwen { .. } => {}
        }
        Ok(())
    }

    /// Local preflight only. Presence of a binary/key does not prove vendor authentication.
    pub fn preflight(&self) -> Result<String> {
        self.validate()?;
        match self {
            Self::Command { program, .. } => {
                resolve_program(program)
                    .with_context(|| format!("missing executable: {program}"))?;
                Ok("executable found; vendor login/permissions must be configured".into())
            }
            Self::Qwen { python } => {
                resolve_program(python).context("Python executable missing")?;
                if std::env::var("SUSI_QWEN_CONFIG")
                    .unwrap_or_default()
                    .is_empty()
                {
                    bail!("set SUSI_QWEN_CONFIG to an absolute JSON configuration file for Qwen-Agent");
                }
                Ok("Python/config present; qwen_agent package and model access checked at execution".into())
            }
            Self::Devin { api_key_env } | Self::Manus { api_key_env } => {
                if std::env::var(api_key_env)
                    .unwrap_or_default()
                    .trim()
                    .is_empty()
                {
                    bail!("set {api_key_env} for cloud API access");
                }
                Ok("credential present; API access checked at execution".into())
            }
        }
    }

    pub fn is_cloud(&self) -> bool {
        matches!(self, Self::Devin { .. } | Self::Manus { .. })
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
