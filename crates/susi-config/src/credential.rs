//! Credentials stored in config are scoped references, never embedded
//! bodies (VC-201-066).
//!
//! [`SusiConfig::credential_ref`] parses the stored value into a
//! `secret://` or `key://` reference — embedded plaintext fails the parse,
//! so it can never leave the config boundary — and the resolvers hand back
//! a secret only through the scoped paths: [`resolve_key_ref`] for
//! `key://`/`ENV_NAME` refs (process env → `cloud.env` → host file) and an
//! issued [`ScopeBoundary`] for `secret://` refs. Provider `api_key_env`
//! fields resolve the same way via [`resolve_api_key_env`].

use crate::key_scope::{resolve_key_ref, KeyRef, ScopedSecret};
use crate::secret_ref::{ResolvedSecret, ScopeBoundary, SecretRef};
use crate::susi_error::{EaiError, EaiResult};
use crate::SusiConfig;
use std::path::Path;

/// The reference stored under a credential key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialRef {
    /// `secret://scope/name` — resolves only through a `ScopeBoundary`
    /// the vault owner issued for that scope.
    Scoped(SecretRef),
    /// `key://env|cloud|host/<name>` or a bare `ENV_NAME` — resolves
    /// through [`resolve_key_ref`].
    Key(KeyRef),
}

impl CredentialRef {
    /// Reference form for logs/exports — never the body.
    #[must_use]
    pub fn display(&self) -> String {
        match self {
            Self::Scoped(r) => r.display(),
            Self::Key(r) => r.uri(),
        }
    }
}

impl SusiConfig {
    /// The scoped reference stored under `key`. Embedded plaintext is
    /// refused: a credential that is not a `secret://`/`key://` reference
    /// or a bare `ENV_NAME` can never be read back as a value.
    pub fn credential_ref(&self, key: &str) -> EaiResult<CredentialRef> {
        let raw = self
            .get::<String>(key)
            .ok_or_else(|| EaiError::config(format!("no credential stored under `{key}`")))?;
        let raw = raw.trim();
        if let Some(r) = SecretRef::parse(raw) {
            return Ok(CredentialRef::Scoped(r));
        }
        KeyRef::parse(raw).map(CredentialRef::Key).map_err(|_| {
            EaiError::config(format!(
                "credential `{key}` embeds a value; store secret://scope/name, \
                 key://scope/name, or an ENV_NAME"
            ))
        })
    }

    /// Resolve a `key://`/`ENV_NAME` credential through the scoped key
    /// path (process env → `cloud.env` → host file). `secret://` refs
    /// have no unscoped resolution — they need `resolve_scoped_credential`.
    pub fn resolve_credential(&self, key: &str, global_dir: &Path) -> EaiResult<ScopedSecret> {
        match self.credential_ref(key)? {
            CredentialRef::Key(r) => resolve_key_ref(&r, global_dir),
            CredentialRef::Scoped(r) => Err(EaiError::config(format!(
                "credential `{key}` is {} — resolve it through an issued ScopeBoundary",
                r.display()
            ))),
        }
    }

    /// Resolve a `secret://` credential through a boundary the vault
    /// owner issued for its scope — there is no
    /// resolve-with-an-asserted-scope path.
    pub fn resolve_scoped_credential(
        &self,
        key: &str,
        boundary: &ScopeBoundary,
    ) -> EaiResult<ResolvedSecret> {
        match self.credential_ref(key)? {
            CredentialRef::Scoped(r) => boundary.resolve(&r).map_err(EaiError::config),
            CredentialRef::Key(r) => Err(EaiError::config(format!(
                "credential `{key}` is {} not a secret:// ref — use resolve_credential",
                r.uri()
            ))),
        }
    }
}

/// `api_key_env` fields across the spec types: `Some(ScopedSecret)` when
/// a reference is configured and resolves, `Ok(None)` when the field is
/// empty, `Err` when the named reference cannot be resolved.
pub(crate) fn resolve_api_key_env(
    api_key_env: &str,
    global_dir: &Path,
) -> EaiResult<Option<ScopedSecret>> {
    if api_key_env.trim().is_empty() {
        return Ok(None);
    }
    resolve_key_ref(&KeyRef::parse(api_key_env)?, global_dir).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret_ref::SecretVault;
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn credential_ref_parses_scoped_and_key_refs_and_refuses_embedded_plaintext() {
        let mut cfg = SusiConfig::default();
        cfg.settings
            .insert("scoped_cred".into(), json!("secret://deploy/openai_key"));
        cfg.settings
            .insert("cloud_cred".into(), json!("key://cloud/HF_TOKEN"));
        cfg.settings
            .insert("env_cred".into(), json!("OPENAI_API_KEY"));
        cfg.settings
            .insert("host_cred".into(), json!("key://host/api_token"));
        cfg.settings
            .insert("plaintext_cred".into(), json!("sk-live-plaintext-123"));

        let scoped = cfg.credential_ref("scoped_cred").unwrap();
        assert_eq!(scoped.display(), "secret://deploy/openai_key");
        assert!(matches!(scoped, CredentialRef::Scoped(_)));

        let cloud = cfg.credential_ref("cloud_cred").unwrap();
        assert_eq!(cloud.display(), "key://cloud/HF_TOKEN");
        assert!(matches!(cloud, CredentialRef::Key(_)));

        let env = cfg.credential_ref("env_cred").unwrap();
        assert_eq!(env.display(), "key://env/OPENAI_API_KEY");

        assert!(cfg.credential_ref("missing_cred").is_err());
        let err = cfg
            .credential_ref("plaintext_cred")
            .unwrap_err()
            .to_string();
        assert!(err.contains("embeds a value"));
    }

    #[test]
    fn resolve_credential_and_resolve_scoped_credential_enforce_boundary_separation() {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "susi_cred_bound_{}_{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("cloud.env"), "HF_TOKEN=\"hf_cloud_secret\"\n").unwrap();
        std::fs::write(dir.join("api_token"), "host_file_secret\n").unwrap();

        let mut cfg = SusiConfig::default();
        cfg.settings
            .insert("scoped_cred".into(), json!("secret://deploy/openai_key"));
        cfg.settings
            .insert("cloud_cred".into(), json!("key://cloud/HF_TOKEN"));
        cfg.settings
            .insert("host_cred".into(), json!("key://host/api_token"));

        let mut vault = SecretVault::default();
        vault.put("deploy", "openai_key", "vault_deploy_secret");
        let deploy_boundary = vault.issue_boundary("deploy");
        let billing_boundary = vault.issue_boundary("billing");

        // Key-scoped credentials resolve via resolve_credential and refuse resolve_scoped_credential.
        assert_eq!(
            cfg.resolve_credential("cloud_cred", &dir).unwrap().value,
            "hf_cloud_secret"
        );
        assert_eq!(
            cfg.resolve_credential("host_cred", &dir).unwrap().value,
            "host_file_secret"
        );
        assert!(cfg
            .resolve_scoped_credential("cloud_cred", &deploy_boundary)
            .is_err());

        // Vault-scoped credentials resolve only via an issued ScopeBoundary for that scope.
        assert!(cfg.resolve_credential("scoped_cred", &dir).is_err());
        assert_eq!(
            cfg.resolve_scoped_credential("scoped_cred", &deploy_boundary)
                .unwrap()
                .expose(),
            "vault_deploy_secret"
        );
        assert!(cfg
            .resolve_scoped_credential("scoped_cred", &billing_boundary)
            .is_err());

        // resolve_api_key_env contract: empty -> Ok(None), valid ref -> Ok(Some), missing -> Err.
        assert_eq!(resolve_api_key_env("   ", &dir).unwrap(), None);
        assert_eq!(
            resolve_api_key_env("key://host/api_token", &dir)
                .unwrap()
                .unwrap()
                .value,
            "host_file_secret"
        );
        assert!(resolve_api_key_env("key://host/missing_file", &dir).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
