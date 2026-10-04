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
