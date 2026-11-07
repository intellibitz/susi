//! Extension packs: vendor opinions outside protocol-generic core.
//!
//! Core ships no pack and seeds none (zero-UI plan A9). Launchable vendor
//! catalogs (leading MCP servers, the MCP scout registry, execution agents,
//! agent engines) live in packs that the host or the commercial desktop
//! installs; core only reads them. Lifecycle:
//! - **Install** — a pack is a directory `~/.susi/extensions/<id>/` holding a
//!   `manifest.json` and the files it maps; core never writes one on its own
//!   (`create_pack` writes an empty shell only when asked).
//! - **Load** — packs under `~/.susi/extensions/<id>/` are discovered and
//!   marked loaded in `state.json`; `load_pack` verifies integrity (checksums,
//!   Ed25519 signatures, pinned digests) and activates one.
//! - **Unload** — deactivate a pack (keeps files); with no active pack every
//!   reader falls back to core's compiled defaults (empty for the launchable
//!   catalogs).
//!
//! The active pack's files win per-file. `SUSI_EXTENSION_PACK` forces the
//! active id.

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
    /// The pack is installed: its `manifest.json` is present on the host.
    /// (The field keeps its historical name for output compatibility; core
    /// no longer seeds any pack.)
    pub seeded: bool,
    pub loaded: bool,
    pub active: bool,
    pub quarantined: bool,
    pub root: PathBuf,
}

/// Pack manifest (`manifest.json`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

fn valid_file_component(value: &str) -> bool {
    let stem = value
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    !value.is_empty()
        && !value.ends_with(['.', ' '])
        && !value
            .chars()
            .any(|c| c.is_control() || "<>:\"/\\|?*".contains(c))
        && !matches!(
            stem.as_str(),
            "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
        )
        && !["COM", "LPT"].iter().any(|prefix| {
            stem.strip_prefix(prefix).is_some_and(|n| {
                matches!(
                    n,
                    "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                )
            })
        })
}

fn validate_pack_id(id: &str) -> Result<(), String> {
    if id.starts_with('.') || !valid_file_component(id) {
        return Err(format!("invalid pack id `{id}`"));
    }
    Ok(())
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
/// escape; a `..` that climbs above the root escapes (e.g.
/// `../shared/coding-models.json`, which is permitted only when the pack
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
    validate_pack_id(&manifest.id)?;
    if manifest.name.trim().is_empty() || manifest.version.trim().is_empty() {
        return Err(format!(
            "pack `{}` must declare a name and version",
            manifest.id
        ));
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
        if logical.eq_ignore_ascii_case("manifest.json")
            || !logical.split(['/', '\\']).all(valid_file_component)
        {
            return Err(format!("invalid managed file name `{logical}`"));
        }
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
        if !rel
            .split(['/', '\\'])
            .all(|part| matches!(part, "." | "..") || valid_file_component(part))
        {
            return Err(format!("invalid managed file path `{rel}`"));
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
            // Nothing is installed until the host installs a pack.
            loaded: Vec::new(),
            unloaded: Vec::new(),
            trusted_digests: BTreeMap::new(),
            trusted_signers: BTreeMap::new(),
            quarantined: BTreeMap::new(),
            extra: HashMap::new(),
        }
    }
}

/// One cloud vendor entry from `cloud-vendors.json` (core's compiled
/// `config/cloud-vendors.json`, or the active pack's override).
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

/// Pack id used when no installed pack is active: every reader then serves
/// core's compiled defaults. Core never creates a pack directory for it.
const DEFAULT_PACK_ID: &str = "default";
/// Compiled cloud-vendor aliases (NP-04: core's key resolver depends on
/// them). Plain compiled defaults, not a pack.
pub(crate) const BUNDLED_CLOUD_VENDORS: &str = include_str!("../../../config/cloud-vendors.json");

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

/// [`write_private_file`], skipped when the file already holds exactly
/// `contents`. The zero-config path re-derives the same bookkeeping on every
/// catalog read, and rewriting identical bytes cost a synced temp file and a
/// rename each time — hundreds per CLI command on Windows. What the rewrite
/// did besides is kept: the parent is an owner-only directory, and on Unix
/// the file is owner-only (0600) afterwards.
fn write_private_file_if_changed(path: &Path, contents: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        private_dir(parent)?;
    }
    if std::fs::read(path).is_ok_and(|current| current == contents.as_bytes()) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if std::fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o777 != 0o600) {
                let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
            }
        }
        return Ok(());
    }
    super::json_util::atomic_write_bytes(path, contents.as_bytes()).map_err(|e| e.to_string())
}

/// Pack state. Absent is the default state; damaged or unreadable is an
/// error. Reading it as default made the next write reset the operator's
/// active pack and re-load every pack they had unloaded.
fn read_state() -> Result<ExtensionsState, String> {
    read_state_text().map(|(state, _)| state)
}

/// [`read_state`], with the bytes it was parsed from (`None`: no file).
fn read_state_text() -> Result<(ExtensionsState, Option<String>), String> {
    let path = state_path();
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text)
            .map(|state| (state, Some(text)))
            .map_err(|e| format!("damaged {}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Ok((ExtensionsState::default(), None))
        }
        Err(e) => Err(format!("unreadable {}: {e}", path.display())),
    }
}

/// The bytes `state.json` holds for `state`.
fn render_state(state: &ExtensionsState) -> Result<String, String> {
    let text = serde_json::to_string_pretty(state).map_err(|e| e.to_string())?;
    Ok(format!("{text}\n"))
}

/// Unchanged state is not rewritten: every catalog read re-derives it, and
/// re-records a standing quarantine. Callers hold [`StateLock`] from the
/// read the new state was derived from until this returns.
fn write_state(state: &ExtensionsState) -> Result<(), String> {
    write_private_file_if_changed(&state_path(), &render_state(state)?)
}

/// Name of the cross-process lock on `state.json` (`.state.lock`, next to
/// it). The leading dot keeps it out of the pack namespace: no valid pack id
/// starts with one and discovery skips such entries, so a pack directory can
/// never sit where the lock goes. The substrate stamp leaves its files out:
/// they are not decision inputs.
const STATE_LOCK: &str = ".state";

/// Whether `name`, an entry of the extensions root that is not a
/// directory, is one of the state lock's files: exactly `.state.lock` or
/// the helper's `.state.lock.wait`/`.state.lock.break` markers.
fn is_state_lock_file(name: &str, is_dir: bool) -> bool {
    !is_dir
        && name
            .strip_prefix(STATE_LOCK)
            .is_some_and(|rest| matches!(rest, ".lock" | ".lock.wait" | ".lock.break"))
}

/// Exclusive cross-process hold on `state.json`, taken for every
/// read-derive-write of it.
///
/// Every catalog read re-derives the state, and each lifecycle command
/// (load, unload, re-admit, a quarantine) reads, changes and writes it. With
/// no lock, a reader that took the state just before `load` wrote it wrote
/// its own derivation back afterwards: the load was reported, yet the active
/// pack fell back to `default` and its pinned digests were gone. Holders
/// re-read the state after taking this and write before dropping it; the
/// write itself stays a temp file plus rename, so lock-free readers never see
/// it torn. Acquisition waits a bounded time and reclaims a lock whose holder
/// died ([`crate::file_lock::FileLock`]: pid liveness, then a 60 s age rule).
/// Not reentrant: release it before calling back into a path that may take
/// it (`ensure_extensions_substrate`, `list_packs`, `load_pack`).
struct StateLock {
    held: crate::file_lock::FileLock,
}

impl StateLock {
    /// Only for an existing extensions root: taking the lock never creates
    /// it (core does not create the root on its own).
    fn acquire() -> Result<Self, String> {
        let root = extensions_root();
        if !root.is_dir() {
            return Err(format!(
                "extensions root {} is not a directory",
                root.display()
            ));
        }
        crate::file_lock::FileLock::acquire_within(&root, STATE_LOCK, STATE_LOCK_WAIT)
            .map(|held| Self { held })
            .ok_or_else(|| {
                format!(
                    "extensions state {} is locked by another process; retry",
                    state_path().display()
                )
            })
    }

    /// Run `work`, a critical section that hashes pack files, while the
    /// lock is kept fresh: hashing a large pack on a slow or suspended host
    /// can outlast the 60 s age rule, which would otherwise hand the lock to
    /// a waiter while this holder is still between its read and its write.
    fn hold_while<T>(&self, work: impl FnOnce() -> T) -> T {
        self.held.hold_while(work)
    }
}

/// How long a state writer waits for the lock. Holders work for
/// milliseconds (hashing a pack's files at most), so a longer wait is a
/// wedged holder, which the lock's stale rule reclaims.
const STATE_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// On Unix, `path` (when present) is owner-only (0600) afterwards: what an
/// unchanged write did besides not writing.
fn keep_private_mode(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if std::fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o777 != 0o600) {
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
    }
    #[cfg(not(unix))]
    let _ = path;
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
    let manifest_path = declared_path(root, "manifest.json", false)?;
    let manifest_bytes = std::fs::read(&manifest_path)
        .map_err(|e| format!("pack `{}` manifest is unavailable: {e}", manifest.id))?;
    if !serde_json::from_slice::<ExtensionManifest>(&manifest_bytes)
        .is_ok_and(|current| &current == manifest)
    {
        return Err(format!(
            "pack `{}` manifest changed during admission",
            manifest.id
        ));
    }
    digests.insert("manifest.json".to_string(), sha256_digest(&manifest_bytes));
    for (logical, rel) in &manifest.files {
        let path = declared_path(
            root,
            rel,
            manifest.permissions.iter().any(|p| p == "filesystem.read"),
        )?;
        let bytes = std::fs::read(&path).map_err(|e| {
            format!(
                "pack `{}` artifact `{logical}` is unavailable at {}: {e}",
                manifest.id,
                path.display()
            )
        })?;
        verify_signature(manifest, logical, &bytes)?;
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
    Ok(digests)
}

/// A relative escape must be explicit in the mapping and permitted. A link
/// in a seemingly internal mapping cannot redirect it outside the pack.
fn declared_path(root: &Path, rel: &str, can_read_outside: bool) -> Result<PathBuf, String> {
    let path = root
        .join(rel)
        .canonicalize()
        .map_err(|e| format!("managed path `{rel}` is unavailable: {e}"))?;
    let base = root.canonicalize().map_err(|e| e.to_string())?;
    if !path.is_file()
        || (!path.starts_with(base) && !(can_read_outside && path_escapes_pack_root(rel)))
    {
        return Err(format!("managed path `{rel}` is not an authorized file"));
    }
    Ok(path)
}

fn normalize_checksum(value: &str) -> String {
    let hex_value = value.strip_prefix("sha256:").unwrap_or(value);
    format!("sha256:{}", hex_value.to_ascii_lowercase())
}

/// The last [`ensure_extensions_substrate`] answer, with the
/// [`substrate_stamp`] it was decided under.
static ENSURE_MEMO: Mutex<Option<(String, ExtensionPack)>> = Mutex::new(None);

/// How long after its last modification an entry's stamp is trusted.
/// Filesystem timestamps are coarse (a clock tick on NTFS and Linux, two
/// seconds on FAT): a second write of the same size within one tick leaves
/// size and modification time as they were.
const STAMP_RACY_WINDOW: std::time::Duration = std::time::Duration::from_secs(2);

/// Everything [`ensure_extensions_substrate`] reads, as stat facts taken at
/// `now`: where the extensions root is, the forced pack id, and the name,
/// kind, size, modification time and [`file_node`](crate::file_node) (through
/// a link, as the decision reads it) of every entry in the root and in each
/// directory directly under it — so a file replaced by another of the same
/// size with its mtime restored still changes the stamp.
/// Each pack's `manifest.json` and files and the state file all sit at that
/// depth, so while the stamp is unchanged a fresh decision
/// reads the same bytes, picks the same pack and writes nothing. `None` when
/// a directory cannot be listed, or an entry was modified less than
/// [`STAMP_RACY_WINDOW`] before `now` (or after it): its size and time may
/// not yet show every write.
fn substrate_stamp(now: std::time::SystemTime) -> Option<String> {
    /// One entry's line, and whether it is a directory.
    fn fact(
        name: &str,
        entry: &std::fs::DirEntry,
        now: std::time::SystemTime,
    ) -> Option<(String, bool)> {
        // Stat the path itself, never `DirEntry::metadata`: on Windows that
        // is the directory listing's copy of the facts, which NTFS does not
        // refresh for a write made through another hard link of the file
        // (or while a writer still holds it open). Following a link here
        // also reads what the decision reads.
        let meta = std::fs::metadata(entry.path()).ok()?;
        let modified = meta.modified().ok()?;
        if !now
            .duration_since(modified)
            .is_ok_and(|age| age >= STAMP_RACY_WINDOW)
        {
            return None;
        }
        let nanos = modified
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let line = format!(
            "{name}|{}|{}|{nanos}|{}",
            meta.is_dir(),
            meta.len(),
            super::json_util::file_node(&meta)
        );
        Some((line, meta.is_dir()))
    }
    let root = extensions_root();
    let mut lines = vec![
        format!("root|{}", root.display()),
        format!("forced|{:?}", std::env::var_os("SUSI_EXTENSION_PACK")),
    ];
    for entry in std::fs::read_dir(&root).ok()? {
        let entry = entry.ok()?;
        let name = entry.file_name().to_string_lossy().into_owned();
        // Lock files come and go between the listing and the stat; a
        // directory under any name is a decision input.
        if is_state_lock_file(&name, entry.file_type().is_ok_and(|t| t.is_dir())) {
            continue;
        }
        let (line, is_dir) = fact(&name, &entry, now)?;
        lines.push(line);
        if is_dir {
            for child in std::fs::read_dir(entry.path()).ok()? {
                let child = child.ok()?;
                let child_name = format!("{name}/{}", child.file_name().to_string_lossy());
                lines.push(fact(&child_name, &child, now)?.0);
            }
        }
    }
    lines.sort();
    Some(lines.join("\n"))
}

/// Zero-config entry: discover installed packs, settle state, return the
/// active pack. Never seeds a pack and never creates the extensions root:
/// with nothing installed it answers the `default` id (compiled defaults)
/// and writes nothing.
///
/// Every catalog read comes through here, several times per read, so the
/// answer is remembered for as long as [`substrate_stamp`] — everything the
/// decision reads — is unchanged, including by another process: an edit
/// there makes the next call decide afresh. A decision that wrote anything,
/// raced a writer or printed a warning is not remembered.
pub fn ensure_extensions_substrate() -> Result<ExtensionPack, String> {
    ensure_extensions_substrate_at(std::time::SystemTime::now())
}

/// [`ensure_extensions_substrate`], its stamps taken as at `now`.
pub(crate) fn ensure_extensions_substrate_at(
    now: std::time::SystemTime,
) -> Result<ExtensionPack, String> {
    let stamp = substrate_stamp(now);
    if let Some(stamp) = &stamp {
        let remembered = ENSURE_MEMO
            .lock()
            .as_ref()
            .filter(|(seen, _)| seen == stamp)
            .map(|(_, pack)| pack.clone());
        if let Some(pack) = remembered {
            // What a decision does besides reading: the directories it owns
            // stay owner-only, and on Unix the files it writes stay 0600
            // (a chmod changes neither size nor mtime, so the stamp misses it).
            private_dir(&extensions_root())?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let file = state_path();
                if std::fs::metadata(&file).is_ok_and(|m| m.permissions().mode() & 0o777 != 0o600) {
                    let _ = std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600));
                }
            }
            return Ok(pack);
        }
    }
    let decided = decide_extensions_substrate();
    if let (Ok((pack, true)), Some(before)) = (&decided, stamp) {
        if substrate_stamp(now).as_ref() == Some(&before) {
            *ENSURE_MEMO.lock() = Some((before, pack.clone()));
        }
    }
    decided.map(|(pack, _)| pack)
}

#[cfg(test)]
thread_local! {
    /// Full decisions taken on this thread: what the remembered answer saves.
    pub(crate) static ENSURE_DECISIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The decision itself, and whether its answer may be remembered: not when
/// it printed a warning (the damaged state's fallback, a pack skipped),
/// which every call prints until the operator acts, nor when the active
/// pack is not a directory directly under the root, whose manifest the
/// stamp does not cover.
fn decide_extensions_substrate() -> Result<(ExtensionPack, bool), String> {
    #[cfg(test)]
    ENSURE_DECISIONS.with(|n| n.set(n.get() + 1));
    let compiled_defaults = || ExtensionPack {
        id: DEFAULT_PACK_ID.to_string(),
        root: extensions_root().join(DEFAULT_PACK_ID),
    };
    // Nothing installed: compiled defaults, and nothing written. Core never
    // creates the extensions root on its own (zero-UI plan A9).
    if !extensions_root().is_dir() {
        return Ok((compiled_defaults(), false));
    }
    private_dir(&extensions_root())?;
    // Zero-config must not brick on a damaged state, and must not overwrite
    // it: run on compiled defaults until the operator repairs it.
    let damaged = |e: String| {
        eprintln!("[extensions] {e}; using compiled defaults without rewriting state");
        Ok((compiled_defaults(), false))
    };
    let (state, text) = match read_state_text() {
        Ok(read) => read,
        Err(e) => return damaged(e),
    };
    let mut quiet = true;
    let discovered = discover_packs(&mut quiet);
    // Only a valid discovered pack can be forced active.
    let forced = std::env::var("SUSI_EXTENSION_PACK")
        .ok()
        .map(|forced| forced.trim().to_string())
        .filter(|forced| manifest_for(forced).is_ok());
    let settled = settle_state(state, &discovered, forced.as_deref());
    let state = if text.as_deref() == Some(render_state(&settled)?.as_str()) {
        // Nothing to settle: a reader writes nothing (and needs no lock).
        keep_private_mode(&state_path());
        settled
    } else {
        // Settling changes the file. Derive again from the state as it is
        // under the lock: the snapshot above may predate a concurrent
        // `load`/`unload`/quarantine, and writing a derivation of it back
        // would silently undo that.
        match StateLock::acquire() {
            Ok(_lock) => {
                let fresh = match read_state() {
                    Ok(fresh) => fresh,
                    Err(e) => return damaged(e),
                };
                let settled = settle_state(fresh, &discovered, forced.as_deref());
                write_state(&settled)?;
                settled
            }
            Err(e) => {
                // A reader serves what it derived rather than fail; the next
                // decision settles the file.
                eprintln!("[extensions] {e}; serving the derived state without writing it");
                quiet = false;
                settled
            }
        }
    };
    let mut parts = Path::new(&state.active).components();
    let directly_under_root = matches!(
        (parts.next(), parts.next()),
        (Some(std::path::Component::Normal(_)), None)
    );
    let pack = ExtensionPack {
        id: state.active.clone(),
        root: extensions_root().join(&state.active),
    };
    Ok((pack, quiet && directly_under_root))
}

/// Valid pack directories directly under the extensions root, in listing
/// order. A pack whose manifest is rejected is skipped with a warning, and
/// `quiet` is cleared.
fn discover_packs(quiet: &mut bool) -> Vec<String> {
    let mut discovered = Vec::new();
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
            if let Err(e) = manifest_for(id) {
                eprintln!(
                    "[extensions] skipping pack `{id}`: {e} (optional failure; core continues)"
                );
                *quiet = false;
                continue;
            }
            discovered.push(id.to_string());
        }
    }
    discovered
}

/// The zero-config settlement of `state`: auto-load each `discovered` pack
/// the user did not unload, apply a valid `forced` id, and fall back to
/// `default` when the active pack is gone. Reads pack manifests, never
/// writes.
fn settle_state(
    mut state: ExtensionsState,
    discovered: &[String],
    forced: Option<&str>,
) -> ExtensionsState {
    for id in discovered {
        if state.unloaded.iter().any(|x| x == id) {
            continue;
        }
        if !state.loaded.iter().any(|x| x == id) {
            state.loaded.push(id.clone());
        }
    }
    if state.active.trim().is_empty() {
        state.active = DEFAULT_PACK_ID.to_string();
    }
    if let Some(forced) = forced {
        state.active = forced.to_string();
        if !state.loaded.iter().any(|x| x == forced) {
            state.loaded.push(forced.to_string());
        }
    }
    // If active pack dir is missing (and not default), fall back.
    if state.active != DEFAULT_PACK_ID && manifest_for(&state.active).is_err() {
        state.active = DEFAULT_PACK_ID.to_string();
    }
    state
}

/// Invalidate in-process pack caches after load/unload/seed.
pub fn invalidate_extension_caches() {
    CACHE_GEN.fetch_add(1, Ordering::SeqCst);
    *CLOUD_VENDOR_CACHE.lock() = None;
    *ENSURE_MEMO.lock() = None;
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
        let Ok(manifest) = manifest_for(id) else {
            return;
        };
        packs.push(PackStatus {
            id: id.to_string(),
            name: manifest.name,
            version: manifest.version,
            seeded: root.join("manifest.json").is_file(),
            loaded: state.loaded.iter().any(|x| x == id),
            active: state.active == id && !unloaded_pack(&state, id),
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
/// Only on an explicit request; does not activate — call [`load_pack`] to
/// make it active.
pub fn create_pack(id: &str) -> Result<PackStatus, String> {
    let id = id.trim();
    validate_pack_id(id)?;
    ensure_extensions_substrate()?;
    private_dir(&extensions_root())?;
    let root = extensions_root().join(id);
    // Creation must never follow an existing pack directory redirect.
    if root.exists() {
        validate_pack_root(id)?;
    }
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
    list_packs()
        .into_iter()
        .find(|p| p.id == id)
        .ok_or_else(|| format!("pack `{id}` has an invalid manifest"))
}

/// Load (activate + mark loaded) an installed pack after verifying its
/// integrity. Core never seeds a pack; the host installs it first.
pub fn load_pack(id: &str) -> Result<PackStatus, String> {
    let id = id.trim();
    validate_pack_id(id)?;
    ensure_extensions_substrate()?;
    let root = extensions_root().join(id);
    if !root.join("manifest.json").is_file() {
        return Err(format!(
            "pack `{id}` not found under {} — install the pack directory (with its manifest.json) there first",
            extensions_root().display()
        ));
    }
    let manifest = manifest_for(id).map_err(|e| format!("pack `{id}` rejected: {e}"))?;
    // Verify and pin against the state as it is under the lock, and hold it
    // through the write: a concurrent reader's settlement or another
    // lifecycle command can then neither be undone by this load nor undo it.
    let lock = StateLock::acquire()?;
    lock.hold_while(|| {
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
        write_state(&state)
    })?;
    // Not reentrant: `list_packs` settles the state again.
    drop(lock);
    invalidate_extension_caches();
    list_packs()
        .into_iter()
        .find(|p| p.id == id)
        .ok_or_else(|| format!("pack `{id}` no longer has a valid manifest"))
}

/// Explicitly re-admit a quarantined pack after its files have been inspected.
/// Re-admission pins the current bytes and is the only path that clears a
/// quarantine or rotates a pinned signer.
pub fn readmit_pack(id: &str) -> Result<PackStatus, String> {
    let id = id.trim();
    validate_pack_id(id)?;
    ensure_extensions_substrate()?;
    let root = extensions_root().join(id);
    let manifest = manifest_for(id)?;
    let lock = StateLock::acquire()?;
    lock.hold_while(|| {
        let mut state = read_state()?;
        state.trusted_digests.remove(id);
        state.trusted_signers.remove(id);
        state.quarantined.remove(id);
        let digests = verify_pack_integrity(&root, &manifest, &state)?;
        state.trusted_digests.insert(id.to_string(), digests);
        if let Some(signer) = manifest.signer {
            state.trusted_signers.insert(id.to_string(), signer);
        } else {
            state.trusted_signers.remove(id);
        }
        state.quarantined.remove(id);
        write_state(&state)
    })?;
    // Not reentrant: `load_pack` takes it again.
    drop(lock);
    invalidate_extension_caches();
    load_pack(id)
}

/// Unload a pack (deactivate; files kept). When the unloaded pack was
/// active, readers fall back to core's compiled defaults (the `default` id);
/// no other installed pack is activated in its place. Unloading `default`
/// itself also stops serving a legacy `extensions/default/` directory an
/// earlier core seeded.
pub fn unload_pack(id: &str) -> Result<PackStatus, String> {
    let id = id.trim();
    validate_pack_id(id)?;
    ensure_extensions_substrate()?;
    // With no extensions root there is no state to change, and none is
    // created; otherwise the change is made to the state as it is under the
    // lock.
    let lock = if extensions_root().is_dir() {
        Some(StateLock::acquire()?)
    } else {
        None
    };
    let mut state = read_state()?;
    state.loaded.retain(|x| x != id);
    if !state.unloaded.iter().any(|x| x == id) {
        state.unloaded.push(id.to_string());
    }
    if state.active == id {
        state.active = default_pack_id();
    }
    if lock.is_some() {
        write_state(&state)?;
    }
    drop(lock);
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

/// Resolve the active pack (`default` with nothing installed: compiled
/// defaults).
pub fn active_pack() -> ExtensionPack {
    match ensure_extensions_substrate() {
        Ok(pack) => pack,
        Err(_) => ExtensionPack {
            id: DEFAULT_PACK_ID.to_string(),
            root: extensions_root().join(DEFAULT_PACK_ID),
        },
    }
}

/// Host pack file path when present and readable as a file.
pub fn pack_file(name: &str) -> Option<PathBuf> {
    verified_pack_file(name).map(|(path, _)| path)
}

/// An explicitly unloaded pack that nothing re-admitted since (`load`, or
/// the `SUSI_EXTENSION_PACK` override, puts it back in `loaded`): its files
/// are never served, even while its id is the active one.
fn unloaded_pack(state: &ExtensionsState, id: &str) -> bool {
    state.unloaded.iter().any(|x| x == id) && !state.loaded.iter().any(|x| x == id)
}

fn verified_pack_file(name: &str) -> Option<(PathBuf, String)> {
    let pack = active_pack();
    let state = read_state().ok()?;
    if unloaded_pack(&state, &pack.id) {
        return None;
    }
    let manifest = manifest_for(&pack.id).ok()?;
    let digests = match verify_pack_integrity(&pack.root, &manifest, &state) {
        Ok(digests) => digests,
        Err(reason) => {
            // Already recorded exactly as this read would record it (a
            // standing quarantine, from the second read on): nothing to
            // write, so no lock either — every catalog read lands here.
            let recorded = state
                .quarantined
                .get(&pack.id)
                .and_then(|files| files.get("pack"));
            if recorded == Some(&reason) {
                return None;
            }
            // Record the quarantine in the state as it is under the lock,
            // judged against that state: the snapshot above may predate a
            // concurrent load or re-admission, which a write of it would
            // undo. Without the lock the file is not served and nothing is
            // recorded; the next read tries again.
            let lock = StateLock::acquire().ok()?;
            lock.hold_while(|| {
                let mut state = read_state().ok()?;
                if unloaded_pack(&state, &pack.id) {
                    return None;
                }
                match verify_pack_integrity(&pack.root, &manifest, &state) {
                    Ok(digests) => Some(digests),
                    Err(reason) => {
                        state
                            .quarantined
                            .entry(pack.id.clone())
                            .or_default()
                            .insert("pack".to_string(), reason);
                        let _ = write_state(&state);
                        None
                    }
                }
            })?
        }
    };
    let path = resolve_pack_path(&pack, &manifest, name)?;
    Some((path, digests.get(name)?.clone()))
}

pub(crate) fn resolve_pack_path(
    pack: &ExtensionPack,
    manifest: &ExtensionManifest,
    name: &str,
) -> Option<PathBuf> {
    let rel = manifest.files.get(name)?;
    declared_path(
        &pack.root,
        rel,
        manifest.permissions.iter().any(|p| p == "filesystem.read"),
    )
    .ok()
}

/// Load JSON from the active pack's file when present and verified;
/// otherwise parse `bundled` (core's compiled default — `"[]"` for the
/// launchable vendor catalogs core no longer ships).
#[allow(clippy::panic)]
pub fn load_json_or_bundled<T: DeserializeOwned>(pack_relative: &str, bundled: &str) -> T {
    if let Some((path, expected)) = verified_pack_file(pack_relative) {
        if let Ok(bytes) = std::fs::read(path) {
            if sha256_digest(&bytes) == expected {
                if let Ok(value) = serde_json::from_slice(&bytes) {
                    return value;
                }
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

fn validate_pack_root(id: &str) -> Result<PathBuf, String> {
    validate_pack_id(id)?;
    let base = extensions_root()
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let root = base.join(id);
    if !root.is_dir() || root.canonicalize().map_err(|e| e.to_string())? != root {
        return Err(format!(
            "pack directory for `{id}` is not a direct physical directory"
        ));
    }
    Ok(root)
}

/// Read and validate the manifest actually present under this pack identity.
/// Author-side admission check for a pack directory anywhere on disk: the
/// host's own manifest validation, path containment, signature and checksum
/// rules, without reading or changing host trust state. Returns the manifest
/// and the SHA-256 of every artifact the host would pin on `load`.
pub fn validate_pack_dir(
    root: &Path,
) -> Result<(ExtensionManifest, BTreeMap<String, String>), String> {
    let file = declared_path(root, "manifest.json", false)?;
    let bytes = std::fs::read(&file).map_err(|e| format!("cannot read {}: {e}", file.display()))?;
    let manifest: ExtensionManifest =
        serde_json::from_slice(&bytes).map_err(|e| format!("invalid manifest: {e}"))?;
    validate_manifest(&manifest)?;
    let digests = artifact_digests(root, &manifest)?;
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
    Ok((manifest, digests))
}

/// Crown `pluggable` proof (Mandate 62): a pack built outside this binary
/// is admitted by the host's own manifest, checksum and Ed25519 signature
/// rules without a rebuild, and a tampered one is refused. Builds a signed
/// synthetic pack under `scratch` (never the host's extensions root, whose
/// trust state it neither reads nor changes) and removes it afterwards.
/// Returns what was admitted, or why the loader failed.
pub fn pack_admission_probe(scratch: &Path) -> Result<String, String> {
    let root = scratch.join("crown-pack-probe");
    let result = pack_admission_probe_at(&root);
    let _ = std::fs::remove_dir_all(&root);
    result
}

fn pack_admission_probe_at(root: &Path) -> Result<String, String> {
    use ed25519_dalek::Signer;
    const CATALOG: &str = "cloud-vendors.json";
    let _ = std::fs::remove_dir_all(root);
    std::fs::create_dir_all(root).map_err(|e| format!("cannot create probe pack: {e}"))?;
    let catalog =
        br#"[{"id":"crown-probe","aliases":["crown-probe"],"api_key_env":"SUSI_CROWN_PROBE_KEY"}]"#;
    let write = |name: &str, bytes: &[u8]| {
        std::fs::write(root.join(name), bytes).map_err(|e| format!("cannot write {name}: {e}"))
    };
    write(CATALOG, catalog)?;
    // A fixed probe key: this signs throwaway bytes, it is no identity.
    let key = ed25519_dalek::SigningKey::from_bytes(&[0x5a; 32]);
    let manifest = |checksum: &str| {
        serde_json::json!({
            "id": "crown-pack-probe",
            "name": "Crown pack admission probe",
            "version": "1.0.0",
            "apiVersion": HOST_PACK_API_MAJOR.to_string(),
            "capabilities": ["catalog.cloud_vendors"],
            "permissions": [],
            "files": { CATALOG: CATALOG },
            "checksums": { CATALOG: checksum },
            "signatures": { CATALOG: hex::encode(key.sign(catalog).to_bytes()) },
            "signer": hex::encode(key.verifying_key().to_bytes()),
        })
        .to_string()
    };
    let digest = sha256_digest(catalog);
    write("manifest.json", manifest(&digest).as_bytes())?;
    let (admitted, digests) =
        validate_pack_dir(root).map_err(|e| format!("valid signed pack refused: {e}"))?;
    if digests.get(CATALOG) != Some(&digest) || !digests.contains_key("manifest.json") {
        return Err("admission did not pin the pack's artifact digests".into());
    }
    // Parse the bytes the admission rules just accepted on disk, not the
    // in-memory constant, so a pack the reader cannot use fails the probe.
    let admitted_bytes = std::fs::read(root.join(CATALOG))
        .map_err(|e| format!("admitted catalog unreadable: {e}"))?;
    let vendors: Vec<CloudVendorEntry> = serde_json::from_slice(&admitted_bytes)
        .map_err(|e| format!("admitted catalog unreadable: {e}"))?;
    if vendors.first().map(|v| v.id.as_str()) != Some("crown-probe") {
        return Err("admitted catalog did not parse into its reader type".into());
    }
    // A wrong checksum binding over the signed bytes must be refused.
    write(
        "manifest.json",
        manifest(&sha256_digest(b"other bytes")).as_bytes(),
    )?;
    if validate_pack_dir(root).is_ok() {
        return Err("pack with a mismatched checksum was admitted".into());
    }
    // Bytes changed after signing must be refused by the signature alone:
    // the checksum is re-bound to the tampered bytes.
    let tampered: &[u8] = b"[]";
    write(CATALOG, tampered)?;
    write(
        "manifest.json",
        manifest(&sha256_digest(tampered)).as_bytes(),
    )?;
    if validate_pack_dir(root).is_ok() {
        return Err("pack whose signed file was tampered with was admitted".into());
    }
    Ok(format!(
        "signed pack `{}` v{} passed the runtime admission rules without a rebuild (manifest, checksum, Ed25519 signature; validated, not installed); mismatched checksum and tampered file refused",
        admitted.id, admitted.version
    ))
}

pub fn manifest_for(pack_id: &str) -> Result<ExtensionManifest, String> {
    let root = validate_pack_root(pack_id)?;
    let file = declared_path(&root, "manifest.json", false)?;
    let bytes =
        std::fs::read(file).map_err(|e| format!("cannot read pack `{pack_id}` manifest: {e}"))?;
    let manifest: ExtensionManifest = serde_json::from_slice(&bytes)
        .map_err(|e| format!("invalid manifest for `{pack_id}`: {e}"))?;
    if manifest.id != pack_id {
        return Err(format!(
            "manifest id `{}` does not match pack directory `{pack_id}`",
            manifest.id
        ));
    }
    validate_manifest(&manifest)?;
    Ok(manifest)
}

/// Ids of the agent adapter files the host (or the commercial desktop) wrote
/// into `dir` — `<config_dir>/execution-agents/` or
/// `<config_dir>/agent-engines/`: `<id>.json` holding a JSON object with a
/// string `kind` (the adapter tag). Bookkeeping files such
/// as `ready.json` (an array) and dotfiles are not adapters.
pub fn host_adapter_ids(dir: &std::path::Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut ids: Vec<String> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let id = path
                .file_name()?
                .to_str()?
                .strip_suffix(".json")?
                .to_string();
            if id.is_empty() || id.starts_with('.') || !path.is_file() {
                return None;
            }
            let value: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).ok()?).ok()?;
            value.get("kind")?.as_str()?;
            Some(id)
        })
        .collect();
    ids.sort();
    ids
}

/// Core's compiled cloud-vendor aliases (`config/cloud-vendors.json`), with
/// no pack lookup: for readers that must not touch the host's pack state.
pub fn compiled_cloud_vendors() -> Vec<CloudVendorEntry> {
    serde_json::from_str(BUNDLED_CLOUD_VENDORS).unwrap_or_default()
}

/// Cloud vendors from the active pack (host override) or core's compiled
/// `config/cloud-vendors.json`.
pub fn load_cloud_vendors() -> Vec<CloudVendorEntry> {
    // Third-party files and their pinned identities may change externally.
    // Only the `default` id (compiled defaults) uses the process-wide cache.
    if active_pack().id != DEFAULT_PACK_ID {
        return load_json_or_bundled("cloud-vendors.json", BUNDLED_CLOUD_VENDORS);
    }
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

// ── Lifecycle reports (one document for the CLI and the socket) ─────────
//
// `susi extensions <action>` prints these and the MCP `extensions_*` tools
// return them, so the desktop over the socket and an operator at the CLI
// read the same JSON (plan B4 group 11).

/// `susi extensions status`: the active pack, its root, and every pack.
pub fn status_report() -> serde_json::Value {
    let pack = active_pack();
    serde_json::json!({
        "active": pack.id,
        "root": pack.root,
        "packs": list_packs(),
    })
}

/// `susi extensions load <id>`: load (verify and activate) the pack, then
/// report it and the pack now active.
pub fn load_report(id: &str) -> Result<serde_json::Value, String> {
    let status = load_pack(id)?;
    Ok(serde_json::json!({
        "loaded": status,
        "active": active_pack().id,
    }))
}

/// `susi extensions unload <id>`: deactivate the pack (files kept), then
/// report it and the pack now active.
pub fn unload_report(id: &str) -> Result<serde_json::Value, String> {
    let status = unload_pack(id)?;
    Ok(serde_json::json!({
        "unloaded": status.id,
        "active": active_pack().id,
    }))
}

/// `susi extensions validate <path>`: the author-side admission check
/// ([`validate_pack_dir`]); reads and changes no host state.
pub fn validate_report(root: &Path) -> Result<serde_json::Value, String> {
    let (manifest, artifacts) = validate_pack_dir(root)?;
    Ok(serde_json::json!({
        "valid": true,
        "id": manifest.id,
        "version": manifest.version,
        "apiVersion": manifest.api_version,
        "permissions": manifest.permissions,
        "signed": manifest.signer.is_some(),
        "artifacts_sha256": artifacts,
    }))
}

