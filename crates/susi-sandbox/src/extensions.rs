//! Extension packs: vendor opinions outside protocol-generic core.
//!
//! Bundled default pack lives at `config/extensions/default/`. Host overrides
//! win from `~/.susi/extensions/<pack-id>/` (or `SUSI_EXTENSION_PACK`).

use serde::de::DeserializeOwned;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Active extension pack (id + host override root).
#[derive(Debug, Clone)]
pub struct ExtensionPack {
    pub id: String,
    /// Host override directory (`~/.susi/extensions/<id>`). May not exist yet.
    pub root: PathBuf,
}

/// Pack manifest (`manifest.json`).
#[derive(Debug, Clone, Deserialize)]
pub struct ExtensionManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    /// Logical file name → path relative to the pack root (may point at `../../…`
    /// for catalogs that still live under `config/` for one release).
    #[serde(default)]
    pub files: std::collections::BTreeMap<String, String>,
}

/// One cloud vendor entry from pack `cloud-vendors.json`.
#[derive(Debug, Clone, Deserialize)]
pub struct CloudVendorEntry {
    pub id: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub api_key_env: String,
    #[serde(default)]
    pub api_key_env_alts: Vec<String>,
}

const DEFAULT_PACK_ID: &str = "default";
const BUNDLED_MANIFEST: &str = include_str!("../../../config/extensions/default/manifest.json");
const BUNDLED_CLOUD_VENDORS: &str =
    include_str!("../../../config/extensions/default/cloud-vendors.json");

/// Resolve the active pack. Default id is `default`; override with
/// `SUSI_EXTENSION_PACK`. Host root is always `config_dir()/extensions/<id>`.
pub fn active_pack() -> ExtensionPack {
    let id = std::env::var("SUSI_EXTENSION_PACK")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_PACK_ID.to_string());
    let root = susi_paths::SusiDirs::config_dir()
        .join("extensions")
        .join(&id);
    ExtensionPack { id, root }
}

/// Host pack file path when present and readable as a file.
/// Prefer this over bundled `include_str!` content.
pub fn pack_file(name: &str) -> Option<PathBuf> {
    let pack = active_pack();
    resolve_pack_path(&pack, name)
}

fn resolve_pack_path(pack: &ExtensionPack, name: &str) -> Option<PathBuf> {
    // Direct file under host pack root.
    let direct = pack.root.join(name);
    if direct.is_file() {
        return Some(direct);
    }
    // Manifest may remap logical names to relative paths (including ../../ catalogs).
    if let Some(rel) = manifest_for(&pack.id).files.get(name).map(|s| s.as_str()) {
        let mapped = pack.root.join(rel);
        if mapped.is_file() {
            return Some(mapped);
        }
        // When the pack root is empty (no host override), resolve relative to
        // the bundled pack's conceptual root only when running from a source
        // checkout is not available — callers must use bundled include_str.
        let _ = mapped;
    }
    None
}

/// Load JSON from the host pack file when present; otherwise parse `bundled`.
pub fn load_json_or_bundled<T: DeserializeOwned>(pack_relative: &str, bundled: &str) -> T {
    if let Some(path) = pack_file(pack_relative) {
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(value) = serde_json::from_str(&text) {
                return value;
            }
        }
    }
    serde_json::from_str(bundled).unwrap_or_else(|e| {
        panic!("bundled extension pack file `{pack_relative}` must be valid JSON: {e}")
    })
}

/// Bundled default-pack manifest (compile-time).
pub fn bundled_manifest() -> ExtensionManifest {
    serde_json::from_str(BUNDLED_MANIFEST)
        .expect("bundled config/extensions/default/manifest.json must be valid JSON")
}

/// Manifest for a pack id: host file wins, else bundled default when id is `default`.
pub fn manifest_for(pack_id: &str) -> ExtensionManifest {
    let root = susi_paths::SusiDirs::config_dir()
        .join("extensions")
        .join(pack_id);
    let host = root.join("manifest.json");
    if host.is_file() {
        if let Ok(text) = std::fs::read_to_string(&host) {
            if let Ok(m) = serde_json::from_str::<ExtensionManifest>(&text) {
                return m;
            }
        }
    }
    if pack_id == DEFAULT_PACK_ID {
        return bundled_manifest();
    }
    // Unknown custom pack without host manifest: empty shell with that id.
    ExtensionManifest {
        id: pack_id.to_string(),
        name: pack_id.to_string(),
        version: "0.0.0".into(),
        description: String::new(),
        files: Default::default(),
    }
}

/// Manifest for the active pack.
pub fn active_manifest() -> ExtensionManifest {
    manifest_for(&active_pack().id)
}

/// Cloud vendors from the active pack (host override or bundled default).
pub fn load_cloud_vendors() -> Vec<CloudVendorEntry> {
    static CACHED: OnceLock<Vec<CloudVendorEntry>> = OnceLock::new();
    // Cache only the bundled parse for the common path; host overrides still
    // refresh when SUSI_EXTENSION_PACK / host files change by re-reading when
    // a host file is present.
    if pack_file("cloud-vendors.json").is_some() {
        return load_json_or_bundled("cloud-vendors.json", BUNDLED_CLOUD_VENDORS);
    }
    CACHED
        .get_or_init(|| load_json_or_bundled("cloud-vendors.json", BUNDLED_CLOUD_VENDORS))
        .clone()
}

/// Bundled cloud-vendors JSON (for tests / explicit include_str callers).
pub fn bundled_cloud_vendors_json() -> &'static str {
    BUNDLED_CLOUD_VENDORS
}

/// Resolve a logical catalog name via the active manifest's `files` map.
pub fn catalog_relative_path(logical_name: &str) -> Option<String> {
    active_manifest().files.get(logical_name).cloned()
}

/// Whether `path` is under an extension pack root (host overrides).
pub fn is_pack_override_path(path: &Path) -> bool {
    let pack = active_pack();
    path.starts_with(&pack.root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_manifest_is_default_pack() {
        let m = bundled_manifest();
        assert_eq!(m.id, "default");
        assert!(m.files.contains_key("cloud-vendors.json"));
        assert!(m.files.contains_key("coding-models.json"));
    }

    #[test]
    fn bundled_cloud_vendors_parse() {
        let vendors: Vec<CloudVendorEntry> =
            serde_json::from_str(BUNDLED_CLOUD_VENDORS).expect("parse");
        assert!(vendors.len() >= 10);
        let openai = vendors.iter().find(|v| v.id == "openai").expect("openai");
        assert_eq!(openai.api_key_env, "OPENAI_API_KEY");
        let kimi = vendors.iter().find(|v| v.id == "kimi").expect("kimi");
        assert!(kimi.aliases.iter().any(|a| a == "moonshot"));
    }

    #[test]
    fn load_json_or_bundled_uses_bundled_when_no_host_file() {
        let vendors: Vec<CloudVendorEntry> =
            load_json_or_bundled("cloud-vendors.json", BUNDLED_CLOUD_VENDORS);
        assert!(!vendors.is_empty());
    }

    #[test]
    fn active_pack_defaults_to_default_id() {
        // Do not clear SUSI_EXTENSION_PACK if the environment already set it
        // for a concurrent test; only assert shape when unset.
        if std::env::var_os("SUSI_EXTENSION_PACK").is_none() {
            let pack = active_pack();
            assert_eq!(pack.id, "default");
            assert!(
                pack.root.ends_with(Path::new("extensions").join("default")),
                "expected …/extensions/default, got {}",
                pack.root.display()
            );
        }
    }
}
