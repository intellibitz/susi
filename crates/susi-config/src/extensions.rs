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
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
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
    pub quarantined: bool,
    pub root: PathBuf,
}

/// Pack manifest (`manifest.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    /// Host API major compatibility (`1` ↔ host major `1`). Default `1`.
    #[serde(default = "default_api_version", alias = "apiVersion")]
    pub api_version: String,
    #[serde(default)]
    pub description: String,
    /// Declared capability ids (e.g. `catalog.coding_models`).
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Required host capabilities (hard fail if missing).
    #[serde(default)]
    pub requires: Vec<String>,
    /// Optional host capabilities (warn / degrade if missing).
    #[serde(default)]
    pub optional: Vec<String>,
    /// Declared permissions — enforced: unknown strings and `files` entries
    /// escaping the pack root without `filesystem.read` are rejected at load.
    #[serde(default)]
    pub permissions: Vec<String>,
    /// Logical file name → path relative to the pack root.
    #[serde(default)]
    pub files: BTreeMap<String, String>,
    /// SHA-256 bindings for managed files. Values may be raw hexadecimal or
    /// prefixed with `sha256:`.
    #[serde(default)]
    pub checksums: BTreeMap<String, String>,
    /// Optional Ed25519 signatures over raw managed-file bytes.
    #[serde(default)]
    pub signatures: BTreeMap<String, String>,
    #[serde(default)]
    pub signer: Option<String>,
    /// Mandate 35: preserve unknown pack-manifest keys.
    #[serde(flatten, default)]
    pub extra: HashMap<String, serde_json::Value>,
}

fn default_api_version() -> String {
    "1".into()
}

/// Host API major this binary supports for extension packs.
pub const HOST_PACK_API_MAJOR: u32 = 1;

/// Permission vocabulary the host can enforce. Unknown strings are rejected —
/// a permission the host cannot enforce must never be silently granted.
pub const KNOWN_PERMISSIONS: &[&str] = &[
    "filesystem.read",
    "filesystem.write",
    "network.egress",
    "process.exec",
];

/// True when `rel` resolves outside the pack root: absolute paths always
/// escape; a `..` that climbs above the root escapes (e.g. the bundled default
/// pack's `../../coding-models.json`, which is permitted only because it
/// declares `filesystem.read`).
fn path_escapes_pack_root(rel: &str) -> bool {
    let path = Path::new(rel);
    if path.is_absolute() {
        return true;
    }
    let mut depth: u32 = 0;
    for comp in path.components() {
        match comp {
            std::path::Component::ParentDir => {
                if depth == 0 {
                    return true;
                }
                depth -= 1;
            }
            std::path::Component::Normal(_) => depth += 1,
            std::path::Component::Prefix(_)
            | std::path::Component::RootDir
            | std::path::Component::CurDir => {}
        }
    }
    false
}

/// Validate a pack manifest for load. Returns `Ok(())` or a human error.
/// Incompatible `api_version` majors fail; missing optional deps do not.
/// Permissions are enforced: unknown permission strings and `files` entries
/// that escape the pack root without declaring `filesystem.read` are rejected.
pub fn validate_manifest(manifest: &ExtensionManifest) -> Result<(), String> {
    if manifest.id.trim().is_empty() {
        return Err("pack manifest id must not be empty".into());
    }
    let major = manifest
        .api_version
        .split('.')
        .next()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);
    if major != HOST_PACK_API_MAJOR {
        return Err(format!(
            "pack `{}` api_version {} incompatible with host major {}",
            manifest.id, manifest.api_version, HOST_PACK_API_MAJOR
        ));
    }
    for perm in &manifest.permissions {
        if perm.trim().is_empty() {
            return Err(format!(
                "pack `{}` has an empty permissions entry",
                manifest.id
            ));
        }
        if !KNOWN_PERMISSIONS.contains(&perm.as_str()) {
            return Err(format!(
                "pack `{}` declares unknown permission `{perm}` (known: {})",
                manifest.id,
                KNOWN_PERMISSIONS.join(", ")
            ));
        }
    }
    for logical in manifest.checksums.keys().chain(manifest.signatures.keys()) {
        if !manifest.files.contains_key(logical) {
            return Err(format!(
                "pack `{}` binds unknown file `{logical}`",
                manifest.id
            ));
        }
    }
    for (logical, checksum) in &manifest.checksums {
        if !is_sha256_digest(checksum) {
            return Err(format!(
                "pack `{}` has invalid SHA-256 checksum for `{logical}`",
                manifest.id
            ));
        }
    }
    if !manifest.signatures.is_empty() {
        let Some(signer) = manifest.signer.as_deref() else {
            return Err(format!(
                "pack `{}` declares signatures without a signer",
                manifest.id
            ));
        };
        if decode_fixed_hex::<32>(signer).is_none() {
            return Err(format!(
                "pack `{}` declares an invalid Ed25519 signer",
                manifest.id
            ));
        }
        for (logical, signature) in &manifest.signatures {
            if decode_fixed_hex::<64>(signature).is_none() {
                return Err(format!(
                    "pack `{}` declares an invalid signature for `{logical}`",
                    manifest.id
                ));
            }
        }
    }
    let can_read_outside = manifest.permissions.iter().any(|p| p == "filesystem.read");
    for (logical, rel) in &manifest.files {
        if Path::new(rel).is_absolute() {
            return Err(format!(
                "pack `{id}` maps `{logical}` to an absolute path `{rel}` — pack files must be relative to the pack root",
                id = manifest.id
            ));
        }
        if path_escapes_pack_root(rel) && !can_read_outside {
            return Err(format!(
                "pack `{id}` maps `{logical}` outside its root (`{rel}`) without declaring `filesystem.read`",
                id = manifest.id
            ));
        }
    }
    Ok(())
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
    /// Digests pinned when a pack is first admitted or explicitly re-admitted.
    #[serde(default)]
    trusted_digests: BTreeMap<String, BTreeMap<String, String>>,
    /// Signer keys are trust-on-first-use and require explicit re-admission to
    /// rotate. The manifest alone cannot silently change identity.
    #[serde(default)]
    trusted_signers: BTreeMap<String, String>,
    /// Pack id → logical file (or `manifest.json`) → quarantine reason.
    #[serde(default)]
    quarantined: BTreeMap<String, BTreeMap<String, String>>,
    /// Mandate 35: preserve unknown state keys on write-back.
    #[serde(flatten, default)]
    extra: HashMap<String, serde_json::Value>,
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
            trusted_digests: BTreeMap::new(),
            trusted_signers: BTreeMap::new(),
            quarantined: BTreeMap::new(),
            extra: HashMap::new(),
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

impl CloudVendorEntry {
    /// Resolve this vendor's credential at use time through the scoped key
    /// path, trying `api_key_env` then each `api_key_env_alts` entry in
    /// order — every field names a `key://` ref or bare `ENV_NAME`, never
    /// the secret body. `Ok(None)` means no credential is configured;
    /// `Err` means references were configured but none resolved.
    pub fn resolve_api_key(
        &self,
        global_dir: &std::path::Path,
    ) -> crate::susi_error::EaiResult<Option<crate::key_scope::ScopedSecret>> {
        let mut last_err = None;
        for name in std::iter::once(&self.api_key_env).chain(self.api_key_env_alts.iter()) {
            if name.trim().is_empty() {
                continue;
            }
            match crate::credential::resolve_api_key_env(name, global_dir) {
                Ok(found @ Some(_)) => return Ok(found),
                Ok(None) => {}
                Err(e) => last_err = Some(e),
            }
        }
        if let Some(e) = last_err {
            return Err(e);
        }
        Ok(None)
    }
}

const DEFAULT_PACK_ID: &str = "default";
const BUNDLED_MANIFEST: &str = include_str!("../../../config/extensions/default/manifest.json");
pub(crate) const BUNDLED_CLOUD_VENDORS: &str =
    include_str!("../../../config/extensions/default/cloud-vendors.json");
const BUNDLED_CODING_MODELS: &str = include_str!("../../../config/coding-models.json");
const BUNDLED_EXECUTION_AGENTS: &str = include_str!("../../../config/execution-agents.json");
const BUNDLED_AGENT_ENGINES: &str = include_str!("../../../config/agent-engines.json");
const BUNDLED_LEADING_MCP: &str = include_str!("../../../config/leading-mcp.json");
const BUNDLED_MODELS_CATALOG: &str = include_str!("../../../config/models.catalog.default.json");
const BUNDLED_CONFIG_DEFAULT: &str = include_str!("../../../config/config.default.json");
const BUNDLED_OPENROUTER_MODELS: &str = include_str!("../../../config/openrouter-models.json");
const BUNDLED_OPEN_WEIGHT_MODELS: &str = include_str!("../../../config/open-weight-models.json");
const BUNDLED_FRONTIER_MODELS: &str = include_str!("../../../config/frontier-models.json");

static CACHE_GEN: AtomicU64 = AtomicU64::new(0);
static CLOUD_VENDOR_CACHE: Mutex<Option<(u64, Vec<CloudVendorEntry>)>> = Mutex::new(None);

pub(crate) fn extensions_root() -> PathBuf {
    susi_paths::SusiDirs::config_dir().join("extensions")
}

pub(crate) fn state_path() -> PathBuf {
    extensions_root().join("state.json")
}

pub(crate) fn private_dir(path: &Path) -> Result<(), String> {
    super::json_util::create_private_dir(path).map_err(|e| e.to_string())
}

/// Owner-only file write that readers never observe torn or, under a
/// permissive umask, momentarily world-readable.
pub(crate) fn write_private_file(path: &Path, contents: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        private_dir(parent)?;
    }
    super::json_util::atomic_write_bytes(path, contents.as_bytes()).map_err(|e| e.to_string())
}

/// Pack state. Absent is the default state; damaged or unreadable is an
/// error. Reading it as default made the next write reset the operator's
/// active pack and re-load every pack they had unloaded.
fn read_state() -> Result<ExtensionsState, String> {
    let path = state_path();
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            serde_json::from_str(&text).map_err(|e| format!("damaged {}: {e}", path.display()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ExtensionsState::default()),
        Err(e) => Err(format!("unreadable {}: {e}", path.display())),
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
        "openrouter-models.json",
        "open-weight-models.json",
        "frontier-models.json",
    ] {
        files.insert(name.to_string(), name.to_string());
    }
    let checksums = files
        .keys()
        .filter_map(|name| {
            bundled_bytes_for(name)
                .map(|contents| (name.clone(), sha256_digest(contents.as_bytes())))
        })
        .collect();
    ExtensionManifest {
        id: DEFAULT_PACK_ID.to_string(),
        name: "SUSI Default Extension Pack".into(),
        version: "0.1.0".into(),
        api_version: default_api_version(),
        description: "Auto-seeded vendor opinions. Edit files here or drop additional packs under ~/.susi/extensions/<id>/.".into(),
        capabilities: vec![
            "catalog.cloud_vendors".into(),
            "catalog.coding_models".into(),
            "catalog.mcp".into(),
        ],
        requires: Vec::new(),
        optional: Vec::new(),
        permissions: vec!["filesystem.read".into()],
        files,
        checksums,
        signatures: BTreeMap::new(),
        signer: None,
        extra: HashMap::new(),
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
        "openrouter-models.json" => Some(BUNDLED_OPENROUTER_MODELS),
        "open-weight-models.json" => Some(BUNDLED_OPEN_WEIGHT_MODELS),
        "frontier-models.json" => Some(BUNDLED_FRONTIER_MODELS),
        _ => None,
    }
}

fn sha256_digest(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

fn is_sha256_digest(value: &str) -> bool {
    let hex_value = value.strip_prefix("sha256:").unwrap_or(value);
    hex_value.len() == 64 && hex::decode(hex_value).is_ok()
}

fn decode_fixed_hex<const N: usize>(value: &str) -> Option<[u8; N]> {
    let bytes = hex::decode(value).ok()?;
    bytes.try_into().ok()
}

fn artifact_digests(
    root: &Path,
    manifest: &ExtensionManifest,
) -> Result<BTreeMap<String, String>, String> {
    let mut digests = BTreeMap::new();
    let manifest_path = root.join("manifest.json");
    let manifest_bytes = std::fs::read(&manifest_path)
        .map_err(|e| format!("pack `{}` manifest is unavailable: {e}", manifest.id))?;
    digests.insert("manifest.json".to_string(), sha256_digest(&manifest_bytes));
    for (logical, rel) in &manifest.files {
        let path = root.join(rel);
        let bytes = std::fs::read(&path).map_err(|e| {
            format!(
                "pack `{}` artifact `{logical}` is unavailable at {}: {e}",
                manifest.id,
                path.display()
            )
        })?;
        digests.insert(logical.clone(), sha256_digest(&bytes));
    }
    Ok(digests)
}

fn verify_signature(
    manifest: &ExtensionManifest,
    logical: &str,
    bytes: &[u8],
) -> Result<(), String> {
    let Some(signature_hex) = manifest.signatures.get(logical) else {
        return Ok(());
    };
    let signer = manifest
        .signer
        .as_deref()
        .and_then(decode_fixed_hex::<32>)
        .ok_or_else(|| format!("pack `{}` has no valid signer", manifest.id))?;
    let signature = decode_fixed_hex::<64>(signature_hex).ok_or_else(|| {
        format!(
            "pack `{}` has invalid signature for `{logical}`",
            manifest.id
        )
    })?;
    let verifying_key = ed25519_dalek::VerifyingKey::from_bytes(&signer)
        .map_err(|e| format!("pack `{}` signer is invalid: {e}", manifest.id))?;
    let signature = ed25519_dalek::Signature::from_bytes(&signature);
    verifying_key
        .verify_strict(bytes, &signature)
        .map_err(|_| format!("pack `{}` signature mismatch for `{logical}`", manifest.id))
}

fn verify_pack_integrity(
    root: &Path,
    manifest: &ExtensionManifest,
    state: &ExtensionsState,
) -> Result<BTreeMap<String, String>, String> {
    if let Some(files) = state.quarantined.get(&manifest.id) {
        if !files.is_empty() {
            return Err(format!(
                "pack `{}` is quarantined; explicitly re-admit it after inspection",
                manifest.id
            ));
        }
    }
    let digests = artifact_digests(root, manifest)?;
    if let Some(trusted) = state.trusted_signers.get(&manifest.id) {
        if manifest.signer.as_deref() != Some(trusted.as_str()) {
            return Err(format!("pack `{}` signer changed", manifest.id));
        }
    }
    for (logical, expected) in &manifest.checksums {
        let actual = digests.get(logical).ok_or_else(|| {
            format!(
                "pack `{}` checksum target `{logical}` is missing",
                manifest.id
            )
        })?;
        if actual != &normalize_checksum(expected) {
            return Err(format!(
                "pack `{}` checksum mismatch for `{logical}`",
                manifest.id
            ));
        }
    }
    if let Some(trusted) = state.trusted_digests.get(&manifest.id) {
        for (logical, expected) in trusted {
            let actual = digests.get(logical).ok_or_else(|| {
                format!(
                    "pack `{}` trusted artifact `{logical}` is missing",
                    manifest.id
                )
            })?;
            if actual != expected {
                return Err(format!(
                    "pack `{}` trusted digest mismatch for `{logical}`",
                    manifest.id
                ));
            }
        }
    }
    for (logical, rel) in &manifest.files {
        let bytes = std::fs::read(root.join(rel)).map_err(|e| {
            format!(
                "pack `{}` artifact `{logical}` is unavailable: {e}",
                manifest.id
            )
        })?;
        verify_signature(manifest, logical, &bytes)?;
    }
    Ok(digests)
}

fn normalize_checksum(value: &str) -> String {
    let hex_value = value.strip_prefix("sha256:").unwrap_or(value);
    format!("sha256:{}", hex_value.to_ascii_lowercase())
}

/// Seed the default pack onto the host. Missing files are created from
/// the bundled catalog. Existing files are preserved as operator-owned bytes;
/// integrity admission decides whether changed files may be served.
pub fn seed_default_pack() -> Result<PathBuf, String> {
    use sha2::{Digest, Sha256};
    let root = extensions_root().join(DEFAULT_PACK_ID);
    private_dir(&root)?;
    let host_manifest = host_seed_manifest();
    let digests_path = root.join(".bundled-digests.json");
    let mut next = BTreeMap::new();
    let digest_of = |bytes: &[u8]| hex::encode(Sha256::digest(bytes));

    let sync_file = |name: &str,
                     dest: &Path,
                     bundled: &str,
                     next: &mut BTreeMap<String, String>|
     -> Result<(), String> {
        let bundled_digest = digest_of(bundled.as_bytes());
        if !dest.is_file() {
            write_private_file(dest, bundled)?;
            next.insert(name.to_string(), bundled_digest);
            return Ok(());
        }
        let Ok(current) = std::fs::read_to_string(dest) else {
            next.insert(name.to_string(), bundled_digest);
            return Ok(());
        };
        let current_digest = digest_of(current.as_bytes());
        // Existing bytes are operator-owned once they differ from the
        // bundled source. Never rewrite them based on a mutable digest ledger:
        // doing so would turn a tamper into a trusted replacement on the next
        // substrate start. Integrity admission below decides whether the pack
        // may be served.
        next.insert(name.to_string(), current_digest);
        Ok(())
    };

    let manifest_text =
        serde_json::to_string_pretty(&host_manifest).map_err(|e| e.to_string())? + "\n";
    sync_file(
        "manifest.json",
        &root.join("manifest.json"),
        &manifest_text,
        &mut next,
    )?;

    for name in host_manifest.files.keys() {
        let Some(bytes) = bundled_bytes_for(name) else {
            continue;
        };
        sync_file(name, &root.join(name), bytes, &mut next)?;
    }
    let digests_json = serde_json::to_string_pretty(&next).map_err(|e| e.to_string())?;
    write_private_file(&digests_path, &format!("{digests_json}\n"))?;
    Ok(root)
}

/// Zero-config entry: create extensions root, seed default, ensure state, return active pack.
pub fn ensure_extensions_substrate() -> Result<ExtensionPack, String> {
    private_dir(&extensions_root())?;
    seed_default_pack()?;
    let mut state = match read_state() {
        Ok(state) => state,
        Err(e) => {
            // Zero-config must not brick on it, and must not overwrite it:
            // run on the seeded default pack until the operator repairs it.
            eprintln!("[extensions] {e}; using the default pack without rewriting state");
            return Ok(ExtensionPack {
                id: DEFAULT_PACK_ID.to_string(),
                root: extensions_root().join(DEFAULT_PACK_ID),
            });
        }
    };
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
            let discovered = manifest_for(id);
            if let Err(e) = validate_manifest(&discovered) {
                eprintln!(
                    "[extensions] skipping pack `{id}`: {e} (optional failure; core continues)"
                );
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
    let state = read_state().unwrap_or_default();
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
            quarantined: state
                .quarantined
                .get(id)
                .is_some_and(|files| !files.is_empty()),
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
#[allow(clippy::expect_used)]
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
        // Mandate 42: safe - seed_default_pack() just returned Ok, meaning it
        // wrote (or confirmed) the default pack's manifest on disk; list_packs()
        // reads that same directory synchronously with no intervening mutation,
        // so the just-seeded id is guaranteed to be present in its output.
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
            api_version: default_api_version(),
            description: format!("Host extension pack `{id}`."),
            capabilities: Vec::new(),
            requires: Vec::new(),
            optional: Vec::new(),
            permissions: Vec::new(),
            files: BTreeMap::new(),
            checksums: BTreeMap::new(),
            signatures: BTreeMap::new(),
            signer: None,
            extra: HashMap::new(),
        };
        let text = serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?;
        write_private_file(&manifest_path, &format!("{text}\n"))?;
    }
    invalidate_extension_caches();
    // Mandate 42: safe - the manifest for `id` was just written above (or
    // already existed), and list_packs() reads that same directory
    // synchronously with no intervening mutation, so `id` is guaranteed to
    // be present in its output.
    Ok(list_packs()
        .into_iter()
        .find(|p| p.id == id)
        .expect("created pack must appear in list"))
}

/// Load (activate + mark loaded) a pack. Seeds `default` if needed.
#[allow(clippy::expect_used)]
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
    let manifest = manifest_for(id);
    if let Err(e) = validate_manifest(&manifest) {
        // Optional / third-party packs must not brick the substrate: refuse
        // this pack only, leave active pack unchanged.
        return Err(format!("pack `{id}` rejected: {e}"));
    }
    let mut state = read_state()?;
    let digests = match verify_pack_integrity(&root, &manifest, &state) {
        Ok(digests) => digests,
        Err(reason) => {
            state
                .quarantined
                .entry(id.to_string())
                .or_default()
                .insert("pack".to_string(), reason.clone());
            write_state(&state)?;
            return Err(format!("pack `{id}` quarantined: {reason}"));
        }
    };
    state.trusted_digests.insert(id.to_string(), digests);
    if let Some(signer) = manifest.signer.as_ref() {
        state.trusted_signers.insert(id.to_string(), signer.clone());
    }
    state.quarantined.remove(id);
    state.unloaded.retain(|x| x != id);
    if !state.loaded.iter().any(|x| x == id) {
        state.loaded.push(id.to_string());
    }
    state.active = id.to_string();
    write_state(&state)?;
    invalidate_extension_caches();
    // Mandate 42: safe - the early return above already guarantees
    // `root.join("manifest.json")` exists for `id` before this point, and
    // list_packs() reads that same directory synchronously with no
    // intervening mutation, so `id` is guaranteed to be present in its output.
    Ok(list_packs()
        .into_iter()
        .find(|p| p.id == id)
        .expect("loaded pack must appear in list"))
}

/// Explicitly re-admit a quarantined pack after its files have been inspected.
/// Re-admission pins the current bytes and is the only path that clears a
/// quarantine or rotates a pinned signer.
pub fn readmit_pack(id: &str) -> Result<PackStatus, String> {
    let id = id.trim();
    if id.is_empty() {
        return Err("pack id must not be empty".into());
    }
    ensure_extensions_substrate()?;
    let root = extensions_root().join(id);
    let manifest = manifest_for(id);
    validate_manifest(&manifest).map_err(|e| format!("pack `{id}` rejected: {e}"))?;
    let digests = artifact_digests(&root, &manifest)?;
    for (logical, rel) in &manifest.files {
        let bytes = std::fs::read(root.join(rel))
            .map_err(|e| format!("pack `{id}` artifact `{logical}` is unavailable: {e}"))?;
        verify_signature(&manifest, logical, &bytes)?;
    }
    let mut state = read_state()?;
    state.trusted_digests.insert(id.to_string(), digests);
    if let Some(signer) = manifest.signer {
        state.trusted_signers.insert(id.to_string(), signer);
    } else {
        state.trusted_signers.remove(id);
    }
    state.quarantined.remove(id);
    write_state(&state)?;
    invalidate_extension_caches();
    load_pack(id)
}

/// Unload a pack (deactivate; files kept). Cannot unload the last remaining pack —
/// falls back to seeded `default` still loaded.
pub fn unload_pack(id: &str) -> Result<PackStatus, String> {
    let id = id.trim();
    if id.is_empty() {
        return Err("pack id must not be empty".into());
    }
    ensure_extensions_substrate()?;
    let mut state = read_state()?;
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
        quarantined: false,
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
    let mut state = read_state().ok()?;
    let manifest = manifest_for(&pack.id);
    if validate_manifest(&manifest).is_err() {
        return None;
    }
    if let Err(reason) = verify_pack_integrity(&pack.root, &manifest, &state) {
        state
            .quarantined
            .entry(pack.id.clone())
            .or_default()
            .insert("pack".to_string(), reason);
        let _ = write_state(&state);
        return None;
    }
    resolve_pack_path(&pack, name)
}

pub(crate) fn resolve_pack_path(pack: &ExtensionPack, name: &str) -> Option<PathBuf> {
    // Runtime jail (defense in depth with validate_manifest): logical names and
    // `files` targets stay inside the pack root unless the manifest declares
    // `filesystem.read`.
    if !path_escapes_pack_root(name) {
        let direct = pack.root.join(name);
        if direct.is_file() {
            return Some(direct);
        }
    }
    let manifest = manifest_for(&pack.id);
    if let Some(rel) = manifest.files.get(name).map(|s| s.as_str()) {
        let escapes = path_escapes_pack_root(rel);
        let can_read_outside = manifest.permissions.iter().any(|p| p == "filesystem.read");
        if Path::new(rel).is_absolute() || (escapes && !can_read_outside) {
            eprintln!(
                "[extensions] pack `{}` denied `{name}` (`{rel}` escapes pack root without `filesystem.read`)",
                pack.id
            );
            return None;
        }
        let mapped = pack.root.join(rel);
        if mapped.is_file() {
            return Some(mapped);
        }
    }
    None
}

/// Load JSON from the host pack file when present; otherwise parse `bundled`.
#[allow(clippy::panic)]
pub fn load_json_or_bundled<T: DeserializeOwned>(pack_relative: &str, bundled: &str) -> T {
    let _ = ensure_extensions_substrate();
    if let Some(path) = pack_file(pack_relative) {
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(value) = serde_json::from_str(&text) {
                return value;
            }
        }
    }
    // Mandate 42: safe - `bundled` is always a `&'static str` produced by an
    // include_str! at the call site (compiled into the binary), not a
    // user-editable runtime file, same pattern as sandbox/manager.rs's
    // bundled-default `.expect()` calls. Parsing either always succeeds or
    // always fails for a given binary - a failure is a build/packaging bug
    // caught by any test run, never a runtime condition that varies between
    // calls.
    serde_json::from_str(bundled).unwrap_or_else(|e| {
        panic!("bundled extension pack file `{pack_relative}` must be valid JSON: {e}")
    })
}

/// Bundled default-pack manifest (compile-time source tree layout).
#[allow(clippy::expect_used)]
pub fn bundled_manifest() -> ExtensionManifest {
    // Mandate 42: safe - BUNDLED_MANIFEST is compiled in via include_str!,
    // see the comment on load_json_or_bundled above.
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
        api_version: default_api_version(),
        description: String::new(),
        capabilities: Vec::new(),
        requires: Vec::new(),
        optional: Vec::new(),
        permissions: Vec::new(),
        files: Default::default(),
        checksums: BTreeMap::new(),
        signatures: BTreeMap::new(),
        signer: None,
        extra: HashMap::new(),
    }
}

/// Cloud vendors from the active pack (host override or bundled default).
pub fn load_cloud_vendors() -> Vec<CloudVendorEntry> {
    let _ = ensure_extensions_substrate();
    let generation = CACHE_GEN.load(Ordering::SeqCst);
    {
        let cache = CLOUD_VENDOR_CACHE.lock();
        if let Some((cached_gen, vendors)) = cache.as_ref() {
            if *cached_gen == generation {
                return vendors.clone();
            }
        }
    }
    let vendors: Vec<CloudVendorEntry> =
        load_json_or_bundled("cloud-vendors.json", BUNDLED_CLOUD_VENDORS);
    *CLOUD_VENDOR_CACHE.lock() = Some((generation, vendors.clone()));
    vendors
}
