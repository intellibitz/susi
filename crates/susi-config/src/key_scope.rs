//! Scoped key references: configs name a secret by scope, never paste it.
//!
//! Adapters and pack JSON refer to `key://scope/name`; resolution reads
//! `cloud.env` / process env / the host key file at use time and never
//! writes the secret into another file.

use crate::cloud_env::{cloud_env_path, env_or_cloud_env, parse_env_file};
use crate::susi_error::{EaiError, EaiResult};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

/// Where a scoped secret lives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyScope {
    /// Process / `cloud.env` variable (e.g. `OPENAI_API_KEY`).
    Env,
    /// Named entry inside `cloud.env` under an explicit alias.
    CloudEnv,
    /// Host file under the config dir (e.g. `api_token`), never copied out.
    HostFile,
}

/// A reference that configs and adapters may store — never the secret body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyRef {
    pub scope: KeyScope,
    /// Env var name, cloud.env alias, or host-relative file name.
    pub name: String,
}

impl KeyRef {
    /// Parse `key://env/OPENAI_API_KEY`, `key://cloud/HF_TOKEN`, or
    /// `key://host/api_token`. Bare `ENV_NAME` is treated as `key://env/…`.
    pub fn parse(raw: &str) -> EaiResult<Self> {
        let raw = raw.trim();
        if let Some(rest) = raw.strip_prefix("key://") {
            let (scope, name) = rest
                .split_once('/')
                .ok_or_else(|| EaiError::config("key ref must be key://<scope>/<name>"))?;
            let name = name.trim();
            if name.is_empty() || name.contains('/') {
                return Err(EaiError::config(
                    "key ref name must be a single path segment",
                ));
            }
            let scope = match scope {
                "env" => KeyScope::Env,
                "cloud" => KeyScope::CloudEnv,
                "host" => KeyScope::HostFile,
                other => {
                    return Err(EaiError::config(format!(
                        "unknown key scope `{other}` (want env|cloud|host)"
                    )));
                }
            };
            return Ok(Self {
                scope,
                name: name.to_string(),
            });
        }
        if raw.is_empty() || raw.contains('/') || raw.contains(' ') {
            return Err(EaiError::config(
                "bare key ref must be a non-empty ENV_NAME without spaces",
            ));
        }
        Ok(Self {
            scope: KeyScope::Env,
            name: raw.to_string(),
        })
    }

    #[must_use]
    pub fn uri(&self) -> String {
        let scope = match self.scope {
            KeyScope::Env => "env",
            KeyScope::CloudEnv => "cloud",
            KeyScope::HostFile => "host",
        };
        format!("key://{scope}/{}", self.name)
    }
}

/// Resolved secret held only in memory for the call site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopedSecret {
    pub reference: KeyRef,
    pub value: String,
}

impl ScopedSecret {
    /// Redacted display — never the body.
    #[must_use]
    pub fn render(&self) -> String {
        format!("{} = [redacted]", self.reference.uri())
    }
}

/// Resolve a key reference at use time. Does not write the secret anywhere.
pub fn resolve_key_ref(reference: &KeyRef, global_dir: &Path) -> EaiResult<ScopedSecret> {
    let value = match reference.scope {
        KeyScope::Env => env_or_cloud_env(&reference.name).map_err(|_| {
            EaiError::config(format!(
                "secret {} not set in process env or cloud.env",
                reference.uri()
            ))
        })?,
        KeyScope::CloudEnv => {
            let path = if global_dir.join("cloud.env").is_file() {
                global_dir.join("cloud.env")
            } else {
                cloud_env_path()
            };
            let text = fs::read_to_string(&path).map_err(|_| {
                EaiError::config(format!(
                    "cloud.env missing while resolving {}",
                    reference.uri()
                ))
            })?;
            parse_env_file(&text)
                .into_iter()
                .find(|(k, _)| k == &reference.name)
                .map(|(_, v)| v)
                .ok_or_else(|| {
                    EaiError::config(format!("alias {} not found in cloud.env", reference.uri()))
                })?
        }
        KeyScope::HostFile => {
            let path = global_dir.join(&reference.name);
            let text = fs::read_to_string(&path).map_err(|e| {
                EaiError::config(format!("host file {} unreadable: {e}", reference.uri()))
            })?;
            let trimmed = text.trim().to_string();
            if trimmed.is_empty() {
                return Err(EaiError::config(format!(
                    "host file {} is empty",
                    reference.uri()
                )));
            }
            trimmed
        }
    };
    Ok(ScopedSecret {
        reference: reference.clone(),
        value,
    })
}
