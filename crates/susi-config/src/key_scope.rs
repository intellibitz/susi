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
    /// Entry inside the AEAD-sealed `vault.sealed.json` (`key://vault/<name>` or `key://vault/<scope>/<name>`).
    Vault,
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
            if name.is_empty() {
                return Err(EaiError::config("key ref name must not be empty"));
            }
            let scope = match scope {
                "env" => KeyScope::Env,
                "cloud" => KeyScope::CloudEnv,
                "host" => KeyScope::HostFile,
                "vault" => KeyScope::Vault,
                other => {
                    return Err(EaiError::config(format!(
                        "unknown key scope `{other}` (want env|cloud|host|vault)"
                    )));
                }
            };
            let valid_name = if scope == KeyScope::Vault {
                !name.starts_with('/')
                    && !name.ends_with('/')
                    && !name.contains("..")
                    && name.split('/').count() <= 2
                    && name.split('/').all(|seg| !seg.trim().is_empty())
            } else {
                !name.contains('/')
            };
            if !valid_name {
                return Err(EaiError::config(
                    "key ref name must be a single path segment (or <scope>/<name> for vault)",
                ));
            }
            return Ok(Self {
                scope,
                name: name.to_string(),
            });
        }
        // A bare ref must be a real environment-variable name; anything
        // else — including embedded secret material, which this shape
        // refuses by construction — is not a reference.
        let mut chars = raw.chars();
        let env_name = chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !env_name {
            return Err(EaiError::config(
                "bare key ref must be an ENV_NAME like OPENAI_API_KEY",
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
            KeyScope::Vault => "vault",
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
            if let Some(found) = super::cloud_env::load_cloud_env_from(global_dir)
                .into_iter()
                .find(|(k, _)| k == &reference.name)
                .map(|(_, v)| v)
            {
                found
            } else {
                let path = if global_dir.join("cloud.env").is_file() {
                    global_dir.join("cloud.env")
                } else {
                    cloud_env_path()
                };
                let text = fs::read_to_string(&path).map_err(|_| {
                    EaiError::config(format!(
                        "cloud.env/sealed vault missing while resolving {}",
                        reference.uri()
                    ))
                })?;
                parse_env_file(&text)
                    .into_iter()
                    .find(|(k, _)| k == &reference.name)
                    .map(|(_, v)| v)
                    .ok_or_else(|| {
                        EaiError::config(format!(
                            "alias {} not found in cloud.env or sealed vault",
                            reference.uri()
                        ))
                    })?
            }
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
        KeyScope::Vault => {
            let vault = super::vault_store::SealedVault::open(global_dir)
                .map_err(EaiError::config)?
                .ok_or_else(|| {
                    EaiError::config(format!(
                        "sealed vault missing while resolving {}",
                        reference.uri()
                    ))
                })?;
            vault
                .lookup_any(&reference.name)
                .map(str::to_string)
                .ok_or_else(|| {
                    EaiError::config(format!(
                        "entry {} not found in sealed vault",
                        reference.uri()
                    ))
                })?
        }
    };
    Ok(ScopedSecret {
        reference: reference.clone(),
        value,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn key_ref_parse_uri_and_redacted_render_contract() {
        let env_ref = KeyRef::parse("key://env/OPENAI_API_KEY").unwrap();
        assert_eq!(
            env_ref,
            KeyRef {
                scope: KeyScope::Env,
                name: "OPENAI_API_KEY".into()
            }
        );
        assert_eq!(env_ref.uri(), "key://env/OPENAI_API_KEY");

        let bare_ref = KeyRef::parse("OPENAI_API_KEY").unwrap();
        assert_eq!(bare_ref, env_ref);

        let cloud_ref = KeyRef::parse("key://cloud/HF_TOKEN").unwrap();
        assert_eq!(cloud_ref.scope, KeyScope::CloudEnv);
        assert_eq!(cloud_ref.uri(), "key://cloud/HF_TOKEN");

        let host_ref = KeyRef::parse("key://host/api_token").unwrap();
        assert_eq!(host_ref.scope, KeyScope::HostFile);
        assert_eq!(host_ref.uri(), "key://host/api_token");

        assert!(KeyRef::parse("key://env").is_err());
        assert!(KeyRef::parse("key://env/").is_err());
        assert!(KeyRef::parse("key://host/sub/token").is_err());
        assert!(KeyRef::parse("key://unknown/token").is_err());
        let vault_ref = KeyRef::parse("key://vault/commercial/SUSI_COMMERCIAL_SECRET").unwrap();
        assert_eq!(vault_ref.scope, KeyScope::Vault);
        assert_eq!(
            vault_ref.uri(),
            "key://vault/commercial/SUSI_COMMERCIAL_SECRET"
        );
        assert!(KeyRef::parse("key://vault/a/b/c").is_err());
        assert!(KeyRef::parse("sk-live-secret-body").is_err());
        assert!(KeyRef::parse("123_BAD").is_err());

        let scoped = ScopedSecret {
            reference: host_ref,
            value: "top-secret-bearer".into(),
        };
        assert_eq!(scoped.render(), "key://host/api_token = [redacted]");
        assert!(!scoped.render().contains("top-secret-bearer"));
    }

    #[test]
    fn resolve_key_ref_reads_cloud_env_and_host_file_in_hermetic_tempdir() {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "susi_key_scope_{}_{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("cloud.env"), "HF_TOKEN=\"hf_test_token_123\"\n").unwrap();
        fs::write(dir.join("api_token"), "  host_bearer_xyz  \n").unwrap();
        fs::write(dir.join("empty_token"), "   \n").unwrap();

        let cloud_resolved =
            resolve_key_ref(&KeyRef::parse("key://cloud/HF_TOKEN").unwrap(), &dir).unwrap();
        assert_eq!(cloud_resolved.value, "hf_test_token_123");

        let host_resolved =
            resolve_key_ref(&KeyRef::parse("key://host/api_token").unwrap(), &dir).unwrap();
        assert_eq!(host_resolved.value, "host_bearer_xyz");

        // Missing cloud alias, missing host file, and whitespace-only host file fail.
        assert!(resolve_key_ref(&KeyRef::parse("key://cloud/MISSING_KEY").unwrap(), &dir).is_err());
        assert!(resolve_key_ref(&KeyRef::parse("key://host/no_such_file").unwrap(), &dir).is_err());
        assert!(resolve_key_ref(&KeyRef::parse("key://host/empty_token").unwrap(), &dir).is_err());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_key_ref_env_scope_and_key_ref_serde_roundtrip() {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "susi_key_scope_env_{}_{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();

        let _guard = crate::env_test_lock();
        const VAR_NAME: &str = "SUSI_TEST_SCOPE_ENV_1104";
        let prev = std::env::var_os(VAR_NAME);
        std::env::set_var(VAR_NAME, "env-scope-secret-1104");

        let env_ref = KeyRef::parse("key://env/SUSI_TEST_SCOPE_ENV_1104").unwrap();
        let resolved = resolve_key_ref(&env_ref, &dir).expect("resolve env key ref when set");
        assert_eq!(resolved.value, "env-scope-secret-1104");

        std::env::remove_var(VAR_NAME);
        let unset_err = resolve_key_ref(&env_ref, &dir)
            .expect_err("unset env key ref must fail")
            .to_string();
        assert!(unset_err.contains("not set in process env or cloud.env"));

        if let Some(v) = prev {
            std::env::set_var(VAR_NAME, v);
        }
        drop(_guard);

        for (scope, expected_wire) in [
            (KeyScope::Env, "\"env\""),
            (KeyScope::CloudEnv, "\"cloud_env\""),
            (KeyScope::HostFile, "\"host_file\""),
            (KeyScope::Vault, "\"vault\""),
        ] {
            let scope_json = serde_json::to_string(&scope).expect("serialize KeyScope");
            assert_eq!(scope_json, expected_wire);
            let scope_back: KeyScope =
                serde_json::from_str(&scope_json).expect("deserialize KeyScope");
            assert_eq!(scope_back, scope);

            let kr = KeyRef {
                scope,
                name: "sample_key".into(),
            };
            let kr_json = serde_json::to_string(&kr).expect("serialize KeyRef");
            let kr_back: KeyRef = serde_json::from_str(&kr_json).expect("deserialize KeyRef");
            assert_eq!(kr_back, kr);
        }

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn key_ref_parse_trims_surrounding_whitespace_and_accepts_leading_underscore_bare_names() {
        let trimmed = KeyRef::parse("  key://cloud/  ANTHROPIC_KEY  ").unwrap();
        assert_eq!(trimmed.scope, KeyScope::CloudEnv);
        assert_eq!(trimmed.name, "ANTHROPIC_KEY");
        assert_eq!(trimmed.uri(), "key://cloud/ANTHROPIC_KEY");

        let underscore = KeyRef::parse("  _SUSI_INTERNAL_KEY_2  ").unwrap();
        assert_eq!(underscore.scope, KeyScope::Env);
        assert_eq!(underscore.name, "_SUSI_INTERNAL_KEY_2");

        for invalid in ["", "   ", "BAD-HYPHEN", "bad/slash"] {
            assert!(
                KeyRef::parse(invalid).is_err(),
                "expected invalid key ref `{invalid}` to be rejected"
            );
        }
    }

    #[test]
    fn key_ref_uri_formats_all_scopes_and_roundtrips_parse() {
        for (uri, scope, name) in [
            ("key://env/OPENAI_API_KEY", KeyScope::Env, "OPENAI_API_KEY"),
            (
                "key://cloud/GEMINI_API_KEY",
                KeyScope::CloudEnv,
                "GEMINI_API_KEY",
            ),
            ("key://host/api_token", KeyScope::HostFile, "api_token"),
            ("key://vault/token", KeyScope::Vault, "token"),
            (
                "key://vault/commercial/SUSI_COMMERCIAL_SECRET",
                KeyScope::Vault,
                "commercial/SUSI_COMMERCIAL_SECRET",
            ),
        ] {
            let kr = KeyRef::parse(uri).unwrap();
            assert_eq!(kr.scope, scope);
            assert_eq!(kr.name, name);
            assert_eq!(kr.uri(), uri);
        }
    }

    #[test]
    fn resolve_key_ref_reads_sealed_vault_entries() {
        let _guard = crate::env_test_lock();
        let dir = std::env::temp_dir().join(format!("susi_key_scope_vault_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let prev = std::env::var_os(super::super::vault_store::VAULT_KEY_ENV);
        std::env::remove_var(super::super::vault_store::VAULT_KEY_ENV);

        super::super::vault_store::enroll_master_key(&dir, Some("keyfile"), None).unwrap();
        let mut vault = super::super::vault_store::SealedVault::open_or_init(&dir).unwrap();
        vault.set("commercial", "SUSI_COMMERCIAL_SECRET", "sealed-comm-val");
        vault.save(&dir).unwrap();

        let kr = KeyRef::parse("key://vault/commercial/SUSI_COMMERCIAL_SECRET").unwrap();
        let resolved = resolve_key_ref(&kr, &dir).unwrap();
        assert_eq!(resolved.value, "sealed-comm-val");

        if let Some(v) = prev {
            std::env::set_var(super::super::vault_store::VAULT_KEY_ENV, v);
        }
        let _ = fs::remove_dir_all(&dir);
    }
}
