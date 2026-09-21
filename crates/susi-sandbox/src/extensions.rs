//! Extension packs: vendor opinions outside protocol-generic core.
//!
//! Zero-config lifecycle:
//! - **Create/seed** — `ensure_extensions_substrate` materializes
//!   `~/.susi/extensions/default/` from the bundled pack on first use.
//! - **Load** — packs under `~/.susi/extensions/<id>/` are discovered and
//!   marked loaded in `state.json` (default pack always loaded after seed).
//! - **Unload** — deactivate a pack (keeps files); falls back to `default`.
//!
//! Host overrides still win per-file. `SUSI_EXTENSION_PACK` forces the active id.

use parking_lot::Mutex;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Active extension pack (id + host root).
#[derive(Debug, Clone)]
pub struct ExtensionPack {
    pub id: String,
    /// Host directory (`~/.susi/extensions/<id>`).
    pub root: PathBuf,
}

/// Pack status for listing / CLI.
#[derive(Debug, Clone, Serialize)]
pub struct PackStatus {
    pub id: String,
    pub name: String,
    pub version: String,
    pub seeded: bool,
    pub loaded: bool,
    pub active: bool,
    pub root: PathBuf,
}

/// Pack manifest (`manifest.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    /// Logical file name → path relative to the pack root.
    #[serde(default)]
    pub files: BTreeMap<String, String>,
}

/// Durable substrate state (`~/.susi/extensions/state.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ExtensionsState {
    #[serde(default = "default_pack_id")]
    active: String,
    #[serde(default)]
    loaded: Vec<String>,
    /// Packs the user unloaded — not auto-loaded on discover until `load_pack`.
    #[serde(default)]
    unloaded: Vec<String>,
}

fn default_pack_id() -> String {
    DEFAULT_PACK_ID.to_string()
}

impl Default for ExtensionsState {
    fn default() -> Self {
        Self {
            active: DEFAULT_PACK_ID.to_string(),
            loaded: vec![DEFAULT_PACK_ID.to_string()],
            unloaded: Vec::new(),
        }
    }
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
const BUNDLED_CODING_MODELS: &str = include_str!("../../../config/coding-models.json");
const BUNDLED_EXECUTION_AGENTS: &str = include_str!("../../../config/execution-agents.json");
const BUNDLED_AGENT_ENGINES: &str = include_str!("../../../config/agent-engines.json");
const BUNDLED_LEADING_MCP: &str = include_str!("../../../config/leading-mcp.json");
const BUNDLED_MODELS_CATALOG: &str = include_str!("../../../config/models.catalog.default.json");
const BUNDLED_CONFIG_DEFAULT: &str = include_str!("../../../config/config.default.json");

static CACHE_GEN: AtomicU64 = AtomicU64::new(0);
static CLOUD_VENDOR_CACHE: Mutex<Option<(u64, Vec<CloudVendorEntry>)>> = Mutex::new(None);

fn extensions_root() -> PathBuf {
    susi_paths::SusiDirs::config_dir().join("extensions")
}

fn state_path() -> PathBuf {
    extensions_root().join("state.json")
}

fn private_dir(path: &Path) -> Result<(), String> {
    std::fs::create_dir_all(path).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
    }
    Ok(())
}

fn write_private_file(path: &Path, contents: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        private_dir(parent)?;
    }
    std::fs::write(path, contents).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn read_state() -> ExtensionsState {
    let path = state_path();
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
        Err(_) => ExtensionsState::default(),
    }
}

fn write_state(state: &ExtensionsState) -> Result<(), String> {
    let text = serde_json::to_string_pretty(state).map_err(|e| e.to_string())?;
    write_private_file(&state_path(), &format!("{text}\n"))
}

/// Host manifest for seeded packs: all files live next to the manifest (no `../../`).
fn host_seed_manifest() -> ExtensionManifest {
    let mut files = BTreeMap::new();
    for name in [
        "cloud-vendors.json",
        "coding-models.json",
        "execution-agents.json",
        "agent-engines.json",
        "leading-mcp.json",
        "models.catalog.default.json",
        "config.default.json",
    ] {
        files.insert(name.to_string(), name.to_string());
    }
    ExtensionManifest {
        id: DEFAULT_PACK_ID.to_string(),
        name: "SUSI Default Extension Pack".into(),
        version: "0.1.0".into(),
        description: "Auto-seeded vendor opinions. Edit files here or drop additional packs under ~/.susi/extensions/<id>/.".into(),
        files,
    }
}

fn bundled_bytes_for(name: &str) -> Option<&'static str> {
    match name {
        "manifest.json" => Some(BUNDLED_MANIFEST),
        "cloud-vendors.json" => Some(BUNDLED_CLOUD_VENDORS),
        "coding-models.json" => Some(BUNDLED_CODING_MODELS),
        "execution-agents.json" => Some(BUNDLED_EXECUTION_AGENTS),
        "agent-engines.json" => Some(BUNDLED_AGENT_ENGINES),
        "leading-mcp.json" => Some(BUNDLED_LEADING_MCP),
        "models.catalog.default.json" => Some(BUNDLED_MODELS_CATALOG),
        "config.default.json" => Some(BUNDLED_CONFIG_DEFAULT),
        _ => None,
    }
}

/// Seed the default pack onto the host if missing (idempotent, never overwrites).
pub fn seed_default_pack() -> Result<PathBuf, String> {
    let root = extensions_root().join(DEFAULT_PACK_ID);
    private_dir(&root)?;
    let host_manifest = host_seed_manifest();
    let manifest_path = root.join("manifest.json");
    if !manifest_path.is_file() {
        let text = serde_json::to_string_pretty(&host_manifest).map_err(|e| e.to_string())?;
        write_private_file(&manifest_path, &format!("{text}\n"))?;
    }
    for name in host_manifest.files.keys() {
        let dest = root.join(name);
        if dest.is_file() {
            continue;
        }
        let Some(bytes) = bundled_bytes_for(name) else {
            continue;
        };
        write_private_file(&dest, bytes)?;
    }
    Ok(root)
}

/// Zero-config entry: create extensions root, seed default, ensure state, return active pack.
pub fn ensure_extensions_substrate() -> Result<ExtensionPack, String> {
    private_dir(&extensions_root())?;
    seed_default_pack()?;
    let mut state = read_state();
    if !state.loaded.iter().any(|id| id == DEFAULT_PACK_ID) {
        state.loaded.push(DEFAULT_PACK_ID.to_string());
    }
    // Discover other pack dirs and auto-load them (zero-config), unless unloaded.
    if let Ok(entries) = std::fs::read_dir(extensions_root()) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let Some(id) = path.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            if id == "state.json" || id.starts_with('.') {
                continue;
            }
            if !path.join("manifest.json").is_file() {
                continue;
            }
            if state.unloaded.iter().any(|x| x == id) {
                continue;
            }
            if !state.loaded.iter().any(|x| x == id) {
                state.loaded.push(id.to_string());
            }
        }
    }
    if state.active.trim().is_empty() {
        state.active = DEFAULT_PACK_ID.to_string();
    }
    // Env override always wins for active id.
    if let Ok(forced) = std::env::var("SUSI_EXTENSION_PACK") {
        let forced = forced.trim();
        if !forced.is_empty() {
            state.active = forced.to_string();
            if !state.loaded.iter().any(|x| x == forced) {
                state.loaded.push(forced.to_string());
            }
        }
    }
    // If active pack dir is missing (and not default), fall back.
    let active_root = extensions_root().join(&state.active);
    if state.active != DEFAULT_PACK_ID && !active_root.join("manifest.json").is_file() {
        state.active = DEFAULT_PACK_ID.to_string();
    }
    write_state(&state)?;
    Ok(ExtensionPack {
        id: state.active.clone(),
        root: extensions_root().join(&state.active),
    })
}

/// Invalidate in-process pack caches after load/unload/seed.
pub fn invalidate_extension_caches() {
    CACHE_GEN.fetch_add(1, Ordering::SeqCst);
    *CLOUD_VENDOR_CACHE.lock() = None;
}

/// List installed / discoverable packs.
pub fn list_packs() -> Vec<PackStatus> {
    let _ = ensure_extensions_substrate();
    let state = read_state();
    let mut packs = Vec::new();
    let mut seen = std::collections::HashSet::new();

    let mut push_pack = |id: &str| {
        if !seen.insert(id.to_string()) {
            return;
        }
        let root = extensions_root().join(id);
        let manifest = manifest_for(id);
        packs.push(PackStatus {
            id: id.to_string(),
            name: manifest.name,
            version: manifest.version,
            seeded: root.join("manifest.json").is_file(),
            loaded: state.loaded.iter().any(|x| x == id),
            active: state.active == id,
            root,
        });
    };

    push_pack(DEFAULT_PACK_ID);
    for id in &state.loaded {
        push_pack(id);
    }
    if let Ok(entries) = std::fs::read_dir(extensions_root()) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Some(id) = path.file_name().and_then(|s| s.to_str()) {
                    if path.join("manifest.json").is_file() {
                        push_pack(id);
                    }
                }
            }
        }
    }
    packs.sort_by(|a, b| a.id.cmp(&b.id));
    packs
}

/// Create a minimal pack shell under `~/.susi/extensions/<id>/` (idempotent).
/// Does not activate; call [`load_pack`] to make it active.
pub fn create_pack(id: &str) -> Result<PackStatus, String> {
    let id = id.trim();
    if id.is_empty() {
        return Err("pack id must not be empty".into());
    }
    if id.contains('/') || id.contains('\\') || id == "." || id == ".." || id.starts_with('.') {
        return Err(format!("invalid pack id `{id}`"));
    }
    ensure_extensions_substrate()?;
    if id == DEFAULT_PACK_ID {
        seed_default_pack()?;
        return Ok(list_packs()
            .into_iter()
            .find(|p| p.id == DEFAULT_PACK_ID)
            .expect("default pack"));
    }
    let root = extensions_root().join(id);
    private_dir(&root)?;
    let manifest_path = root.join("manifest.json");
    if !manifest_path.is_file() {
        let manifest = ExtensionManifest {
            id: id.to_string(),
            name: id.to_string(),
            version: "0.0.1".into(),
            description: format!("Host extension pack `{id}`."),
            files: BTreeMap::new(),
        };
        let text = serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?;
        write_private_file(&manifest_path, &format!("{text}\n"))?;
    }
    invalidate_extension_caches();
    Ok(list_packs()
        .into_iter()
        .find(|p| p.id == id)
        .expect("created pack must appear in list"))
}

/// Load (activate + mark loaded) a pack. Seeds `default` if needed.
pub fn load_pack(id: &str) -> Result<PackStatus, String> {
    let id = id.trim();
    if id.is_empty() {
        return Err("pack id must not be empty".into());
    }
    ensure_extensions_substrate()?;
    if id == DEFAULT_PACK_ID {
        seed_default_pack()?;
    }
    let root = extensions_root().join(id);
    if !root.join("manifest.json").is_file() {
        return Err(format!(
            "pack `{id}` not found under {} — drop a manifest.json there or use `default`",
            extensions_root().display()
        ));
    }
    let mut state = read_state();
    state.unloaded.retain(|x| x != id);
    if !state.loaded.iter().any(|x| x == id) {
        state.loaded.push(id.to_string());
    }
    state.active = id.to_string();
    write_state(&state)?;
    invalidate_extension_caches();
    Ok(list_packs()
        .into_iter()
        .find(|p| p.id == id)
        .expect("loaded pack must appear in list"))
}

/// Unload a pack (deactivate; files kept). Cannot unload the last remaining pack —
/// falls back to seeded `default` still loaded.
pub fn unload_pack(id: &str) -> Result<PackStatus, String> {
    let id = id.trim();
    if id.is_empty() {
        return Err("pack id must not be empty".into());
    }
    ensure_extensions_substrate()?;
    let mut state = read_state();
    state.loaded.retain(|x| x != id);
    if !state.unloaded.iter().any(|x| x == id) {
        state.unloaded.push(id.to_string());
    }
    if state.loaded.is_empty() {
        seed_default_pack()?;
        state.loaded.push(DEFAULT_PACK_ID.to_string());
        state.unloaded.retain(|x| x != DEFAULT_PACK_ID);
    }
    if state.active == id {
        state.active = state
            .loaded
            .iter()
            .find(|x| x.as_str() == DEFAULT_PACK_ID)
            .cloned()
            .unwrap_or_else(|| state.loaded[0].clone());
    }
    write_state(&state)?;
    invalidate_extension_caches();
    Ok(PackStatus {
        id: id.to_string(),
        name: id.to_string(),
        version: String::new(),
        seeded: extensions_root().join(id).join("manifest.json").is_file(),
        loaded: false,
        active: false,
        root: extensions_root().join(id),
    })
}

/// Resolve the active pack (auto-seeds substrate first).
pub fn active_pack() -> ExtensionPack {
    match ensure_extensions_substrate() {
        Ok(pack) => {
            if let Ok(forced) = std::env::var("SUSI_EXTENSION_PACK") {
                let forced = forced.trim();
                if !forced.is_empty() {
                    return ExtensionPack {
                        id: forced.to_string(),
                        root: extensions_root().join(forced),
                    };
                }
            }
            pack
        }
        Err(_) => ExtensionPack {
            id: DEFAULT_PACK_ID.to_string(),
            root: extensions_root().join(DEFAULT_PACK_ID),
        },
    }
}

/// Host pack file path when present and readable as a file.
pub fn pack_file(name: &str) -> Option<PathBuf> {
    let _ = ensure_extensions_substrate();
    let pack = active_pack();
    resolve_pack_path(&pack, name)
}

fn resolve_pack_path(pack: &ExtensionPack, name: &str) -> Option<PathBuf> {
    let direct = pack.root.join(name);
    if direct.is_file() {
        return Some(direct);
    }
    if let Some(rel) = manifest_for(&pack.id).files.get(name).map(|s| s.as_str()) {
        let mapped = pack.root.join(rel);
        if mapped.is_file() {
            return Some(mapped);
        }
    }
    None
}

/// Load JSON from the host pack file when present; otherwise parse `bundled`.
pub fn load_json_or_bundled<T: DeserializeOwned>(pack_relative: &str, bundled: &str) -> T {
    let _ = ensure_extensions_substrate();
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

/// Bundled default-pack manifest (compile-time source tree layout).
pub fn bundled_manifest() -> ExtensionManifest {
    serde_json::from_str(BUNDLED_MANIFEST)
        .expect("bundled config/extensions/default/manifest.json must be valid JSON")
}

/// Manifest for a pack id: host file wins, else bundled default when id is `default`.
pub fn manifest_for(pack_id: &str) -> ExtensionManifest {
    let root = extensions_root().join(pack_id);
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
    let _ = ensure_extensions_substrate();
    let gen = CACHE_GEN.load(Ordering::SeqCst);
    {
        let cache = CLOUD_VENDOR_CACHE.lock();
        if let Some((cached_gen, vendors)) = cache.as_ref() {
            if *cached_gen == gen {
                return vendors.clone();
            }
        }
    }
    let vendors: Vec<CloudVendorEntry> =
        load_json_or_bundled("cloud-vendors.json", BUNDLED_CLOUD_VENDORS);
    *CLOUD_VENDOR_CACHE.lock() = Some((gen, vendors.clone()));
    vendors
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
    use std::sync::Mutex as StdMutex;

    static ENV_LOCK: StdMutex<()> = StdMutex::new(());

    fn with_temp_home<F: FnOnce()>(f: F) {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = std::env::temp_dir().join(format!(
            "susi_ext_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::create_dir_all(&tmp);
        let prev_home = std::env::var_os("HOME");
        let prev_xdg = std::env::var_os("XDG_CONFIG_HOME");
        let prev_pack = std::env::var_os("SUSI_EXTENSION_PACK");
        let prev_susi_xdg = std::env::var_os("SUSI_XDG");
        unsafe {
            std::env::set_var("HOME", &tmp);
            std::env::set_var("XDG_CONFIG_HOME", tmp.join("config"));
            std::env::set_var("SUSI_XDG", "0");
            std::env::remove_var("SUSI_EXTENSION_PACK");
        }
        invalidate_extension_caches();
        f();
        unsafe {
            match prev_home {
                Some(h) => std::env::set_var("HOME", h),
                None => std::env::remove_var("HOME"),
            }
            match prev_xdg {
                Some(h) => std::env::set_var("XDG_CONFIG_HOME", h),
                None => std::env::remove_var("XDG_CONFIG_HOME"),
            }
            match prev_pack {
                Some(h) => std::env::set_var("SUSI_EXTENSION_PACK", h),
                None => std::env::remove_var("SUSI_EXTENSION_PACK"),
            }
            match prev_susi_xdg {
                Some(h) => std::env::set_var("SUSI_XDG", h),
                None => std::env::remove_var("SUSI_XDG"),
            }
        }
        invalidate_extension_caches();
        let _ = std::fs::remove_dir_all(tmp);
    }

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
    }

    #[test]
    fn load_json_or_bundled_uses_bundled_when_no_host_file() {
        let vendors: Vec<CloudVendorEntry> =
            load_json_or_bundled("cloud-vendors.json", BUNDLED_CLOUD_VENDORS);
        assert!(!vendors.is_empty());
    }

    #[test]
    fn ensure_seeds_default_pack_and_state() {
        with_temp_home(|| {
            let pack = ensure_extensions_substrate().expect("seed");
            assert_eq!(pack.id, "default");
            assert!(pack.root.join("manifest.json").is_file());
            assert!(pack.root.join("cloud-vendors.json").is_file());
            assert!(pack.root.join("coding-models.json").is_file());
            assert!(state_path().is_file());
            let listed = list_packs();
            assert!(listed
                .iter()
                .any(|p| p.id == "default" && p.active && p.loaded));
        });
    }

    #[test]
    fn load_unload_round_trip_custom_pack() {
        with_temp_home(|| {
            ensure_extensions_substrate().unwrap();
            let custom = extensions_root().join("custom");
            private_dir(&custom).unwrap();
            write_private_file(
                &custom.join("manifest.json"),
                r#"{
  "id": "custom",
  "name": "Custom",
  "version": "0.0.1",
  "files": { "cloud-vendors.json": "cloud-vendors.json" }
}"#,
            )
            .unwrap();
            write_private_file(
                &custom.join("cloud-vendors.json"),
                r#"[{"id":"acme","aliases":["acme"],"api_key_env":"ACME_API_KEY"}]"#,
            )
            .unwrap();

            let loaded = load_pack("custom").unwrap();
            assert!(loaded.active && loaded.loaded);
            assert_eq!(active_pack().id, "custom");

            let vendors = load_cloud_vendors();
            assert_eq!(vendors.len(), 1);
            assert_eq!(vendors[0].id, "acme");

            unload_pack("custom").unwrap();
            assert_eq!(active_pack().id, "default");
            let after = list_packs();
            assert!(after
                .iter()
                .any(|p| p.id == "custom" && !p.loaded && !p.active));
        });
    }

    #[test]
    fn create_pack_writes_manifest_shell() {
        with_temp_home(|| {
            let status = create_pack("mine").unwrap();
            assert!(status.seeded);
            assert!(!status.active);
            assert!(extensions_root()
                .join("mine")
                .join("manifest.json")
                .is_file());
            let loaded = load_pack("mine").unwrap();
            assert!(loaded.active);
            unload_pack("mine").unwrap();
            assert_eq!(active_pack().id, "default");
        });
    }
}
