//! Installed model catalogs share verified pack loading and first-match ordering.
//! Each catalog defines which names it accepts and how stored overrides apply.

use serde::de::DeserializeOwned;
use std::{
    fs,
    path::{Path, PathBuf},
};
use susi_error::{eai_bail as bail, EaiResult, ResultExt};

pub(crate) struct Catalog<T> {
    pub file: &'static str,
    pub bundled: &'static str,
    pub matches: fn(&T, &str) -> bool,
    pub unknown: fn(&str) -> String,
}

impl<T: DeserializeOwned> Catalog<T> {
    pub(crate) fn rows(&self) -> Vec<T> {
        crate::susi_sandbox::extensions::load_json_or_bundled(self.file, self.bundled)
    }

    pub(crate) fn first(&self, id: &str) -> EaiResult<T> {
        self.rows()
            .into_iter()
            .find(|row| (self.matches)(row, id))
            .with_context(|| (self.unknown)(id))
    }
}

/// Bind each public manager's existing Rust name and constructor to its catalog.
macro_rules! local_catalog {
    ($manager:ident, $constructor:ident, $entry:ty, $directory:literal, $file:literal, $unknown:literal,
     |$row:ident, $id:ident| $matches:expr) => {
        #[derive(Clone)]
        pub struct $manager {
            root: std::path::PathBuf,
        }

        impl $manager {
            const CATALOG: crate::catalog::Catalog<$entry> = crate::catalog::Catalog {
                file: $file,
                bundled: include_str!(concat!("../../../config/", $file)),
                matches: |$row, $id| $matches,
                unknown: |id| format!($unknown, id = id),
            };

            pub fn new() -> susi_error::EaiResult<Self> {
                Ok(Self::$constructor(
                    susi_paths::SusiDirs::config_dir().join($directory),
                ))
            }

            pub fn $constructor(root: std::path::PathBuf) -> Self {
                Self { root }
            }

            pub fn catalog() -> susi_error::EaiResult<Vec<$entry>> {
                Ok(Self::CATALOG.rows())
            }

            pub fn definition($id: &str) -> susi_error::EaiResult<$entry> {
                Self::CATALOG.first($id)
            }
        }
    };
}
pub(crate) use local_catalog;

/// Coding and frontier retain distinct public override and endpoint types.
/// Their storage format, field validation and endpoint resolution are identical.
macro_rules! cloud_model_controls {
    ($manager:ident, $model:ident, $over:ident, $endpoint:ident, $family:literal) => {
        #[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
        pub struct $over {
            pub engine: Option<String>,
            pub model: Option<String>,
            pub protocol_type: Option<String>,
            pub api_key_env: Option<String>,
            /// Mandate 35: retain unknown fields in an override document.
            #[serde(flatten, default)]
            pub extra: std::collections::HashMap<String, serde_json::Value>,
        }

        #[derive(Debug, Clone)]
        pub struct $endpoint {
            pub def: $model,
            pub api_base: String,
            pub protocol_type: String,
            pub api_key_env: String,
            pub api_key: String,
        }

        impl $model {
            fn with_stored_override(
                mut self,
                root: &std::path::Path,
            ) -> susi_error::EaiResult<Self> {
                let invalid = concat!("invalid ", $family, "-model override");
                if let Some(over) = crate::catalog::read_override::<$over>(root, &self.id, invalid)?
                {
                    crate::catalog::apply_override(&mut self.engine, over.engine, false);
                    crate::catalog::apply_override(&mut self.model, over.model, false);
                    crate::catalog::apply_override(
                        &mut self.protocol_type,
                        over.protocol_type,
                        true,
                    );
                    crate::catalog::apply_override(&mut self.api_key_env, over.api_key_env, true);
                }
                self.validate()?;
                Ok(self)
            }

            fn validate(&self) -> susi_error::EaiResult<()> {
                crate::catalog::validate_model(
                    [&self.id, &self.engine, &self.model],
                    &self.api_key_env,
                    $family,
                )
            }
        }

        impl $manager {
            pub fn reset(&self, id: &str) -> susi_error::EaiResult<$model> {
                let def = Self::definition(id)?;
                crate::susi_config::clear_json_override(&self.root, &def.id)?;
                self.effective(&def.id)
            }

            pub fn resolve_endpoint(&self, id: &str) -> susi_error::EaiResult<$endpoint> {
                let def = self.effective(id)?;
                self.preflight(&def.id)?;
                let (endpoint, api_key) = crate::catalog::resolve_endpoint(
                    &def.engine,
                    &def.protocol_type,
                    &def.api_key_env,
                )?;
                Ok($endpoint {
                    def,
                    api_base: endpoint.api_base.trim().to_string(),
                    protocol_type: endpoint.protocol_type,
                    api_key_env: endpoint.api_key_env,
                    api_key,
                })
            }
        }
    };
}
pub(crate) use cloud_model_controls;

pub(crate) fn read_override<T: DeserializeOwned>(
    root: &Path,
    id: &str,
    error: &str,
) -> EaiResult<Option<T>> {
    fs::read(root.join(format!("{id}.json")))
        .ok()
        .map(|bytes| serde_json::from_slice(&bytes).context(error))
        .transpose()
}

pub(crate) fn preferred_path(root: &Path) -> PathBuf {
    root.join("preferred_model.txt")
}

pub(crate) fn apply_override(target: &mut String, value: Option<String>, allow_empty: bool) {
    if let Some(value) = value.filter(|value| allow_empty || !value.trim().is_empty()) {
        *target = value;
    }
}

pub(crate) fn preferred(path: &Path) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

pub(crate) fn clear_preferred(path: &Path, cleared: &str, absent: &str) -> EaiResult<String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(cleared.into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(absent.into()),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn validate_model(required: [&str; 3], key_env: &str, family: &str) -> EaiResult<()> {
    if required.iter().any(|value| value.trim().is_empty()) {
        bail!("{family} model id, engine, and model must be nonempty");
    }
    if !key_env
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        bail!("api_key_env must name an environment variable");
    }
    Ok(())
}

/// `require_remote_key` preserves coding models' stricter readiness policy:
/// frontier models permit remote endpoints that declare no credential variable.
pub(crate) fn preflight(
    engine: &str,
    key_env: &str,
    require_remote_key: bool,
) -> EaiResult<&'static str> {
    let endpoint = crate::cloud::endpoint_named(engine)
        .with_context(|| format!("inference endpoint '{engine}' is not configured"))?;
    if endpoint.api_base.trim().is_empty() {
        bail!("endpoint '{engine}' has an empty api_base");
    }
    let key_env = if key_env.is_empty() {
        &endpoint.api_key_env
    } else {
        key_env
    };
    let key = crate::cloud::resolve_api_key(key_env, engine);
    let remote = crate::cloud::is_remote_cloud(endpoint.api_base.trim());
    if remote && (require_remote_key || !key_env.is_empty()) && key.is_empty() {
        bail!("set {key_env} (or `susi keys set {engine}`) for cloud access");
    }
    Ok(if key_env.is_empty() || !remote {
        "not required (local)"
    } else {
        "present"
    })
}

pub(crate) fn resolve_endpoint(
    engine: &str,
    protocol: &str,
    key_env: &str,
) -> EaiResult<(susi_config::InferenceEndpointItem, String)> {
    let mut endpoint =
        crate::cloud::endpoint_named(engine).context("endpoint missing after preflight")?;
    if !key_env.is_empty() {
        endpoint.api_key_env = key_env.to_owned();
    }
    if !protocol.is_empty() {
        endpoint.protocol_type = protocol.to_owned();
    }
    let key = crate::cloud::resolve_api_key(&endpoint.api_key_env, engine);
    Ok((endpoint, key))
}
