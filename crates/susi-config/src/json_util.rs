// susi Sandbox Manager: 100% DYNAMIC - Zero hardcoded keys
// Pattern used by Astral (ruff) and Claude Code: Registry + HashMap + Value
// Add new model, prompt, message, endpoint without touching Rust

use crate::susi_error::{EaiError, EaiResult};
use serde::Serialize;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

// === CORE DYNAMIC TYPES ===
// Everything is a registry. No struct fields are hardcoded.

pub type DynamicValue = serde_json::Value;
pub type DynamicRegistry = HashMap<String, DynamicValue>;
pub type StringRegistry = HashMap<String, String>;

// ModelTier and ProviderType are now dynamic strings, not hardcoded enums
pub type ModelTier = String;
pub type ProviderType = String;

// === SHARED SELF-HEALING JSON LOAD/SAVE ===
// Every `*.default.json`-backed config type (SusiConfig, SusiPrompts,
// SusiMessages) needs the same two things: an atomic write (so a concurrent
// reader never observes a torn file) and a recursive merge that backfills a
// key/array-element present in the compiled-in default but missing from the
// user's persisted file, without ever touching a value the user already set.
// Factored out once here instead of three separately hand-rolled (and, until
// this was noticed, inconsistently deep) copies.

/// Writes `value` as pretty JSON to `path` via [`atomic_write_bytes`].
pub fn atomic_write_json_pretty<T: Serialize>(path: &Path, value: &T) -> EaiResult<()> {
    let json = serde_json::to_string_pretty(value).map_err(|e| EaiError::config(e.to_string()))?;
    atomic_write_bytes(path, json.as_bytes()).map_err(|error| EaiError::config(error.to_string()))
}

/// Creates `path` (and parents) as an owner-only directory (0700 on Unix)
/// for host state that may hold credentials or private overrides.
pub fn create_private_dir(path: &Path) -> std::io::Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Removes the file at `path`; a file that is already absent is success.
pub fn remove_file_if_present(path: &Path) -> std::io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Writes one per-id JSON override (`<dir>/<id>.json`, dir owner-only).
/// The catalog managers (coding models, frontier models, leading MCPs)
/// share this instead of each hand-rolling the same three steps.
pub fn write_json_override<T: Serialize>(dir: &Path, id: &str, over: &T) -> EaiResult<()> {
    create_private_dir(dir).map_err(|e| EaiError::config(e.to_string()))?;
    atomic_write_json_pretty(&dir.join(format!("{id}.json")), over)
}

/// Removes the per-id JSON override written by [`write_json_override`];
/// an absent override is success.
pub fn clear_json_override(dir: &Path, id: &str) -> EaiResult<()> {
    remove_file_if_present(&dir.join(format!("{id}.json")))
        .map_err(|e| EaiError::config(e.to_string()))
}

/// Loads the 32-byte secret at `path`, creating it on first use.
///
/// Creation is atomic and never replaces an existing secret: the bytes are
/// written in full to a unique `create_new` staging file (0600 on Unix) and
/// then hard-linked into place, which fails if another writer won the race.
/// Concurrent first use (daemon + CLI, parallel tests) therefore converges
/// on one secret instead of each signing with its own. A secret file of the
/// wrong length is an error, never silently regenerated or truncated —
/// replacing it would invalidate everything already signed with it — and an
/// entropy failure is an error, never a predictable key.
pub fn load_or_create_secret(path: &Path) -> std::io::Result<[u8; 32]> {
    match read_secret(path) {
        Ok(key) => return Ok(key),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let mut key = [0u8; 32];
    getrandom::fill(&mut key).map_err(|e| std::io::Error::other(e.to_string()))?;
    if install_private_file(path, &key)? {
        Ok(key)
    } else {
        // Another writer installed its secret first — use theirs.
        read_secret(path)
    }
}

/// Installs `bytes` at `path` only if nothing is there yet: written in full
/// to a unique owner-only (0600 on Unix) staging file, then hard-linked into
/// place, so readers never see a partial or umask-readable file and a
/// concurrent installer never overwrites the winner. Returns `false` when
/// `path` already existed (nothing written). Parents are created on demand.
pub fn install_private_file(path: &Path, bytes: &[u8]) -> std::io::Result<bool> {
    let dir = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        Some(_) | None => Path::new("."),
    };
    fs::create_dir_all(dir)?;
    let (tmp, mut file) = create_staging_file(dir, path)?;
    let staged = {
        use std::io::Write;
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .and_then(|()| fs::hard_link(&tmp, path))
    };
    drop(file);
    let _ = fs::remove_file(&tmp);
    match staged {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(e),
    }
}

fn read_secret(path: &Path) -> std::io::Result<[u8; 32]> {
    let bytes = fs::read(path)?;
    <[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "secret {} is {} bytes, expected 32",
                path.display(),
                bytes.len()
            ),
        )
    })
}

/// A fresh owner-only (0600 on Unix) `create_new` staging file.
fn create_staging_file(dir: &Path, path: &Path) -> std::io::Result<(PathBuf, fs::File)> {
    create_staging_file_with(dir, path, true)
}

/// A fresh `create_new` staging file beside `path` (dot-prefixed so
/// directory scanners skip it), unique per process and call. `private`
/// makes it 0600 on Unix; otherwise the process umask applies.
fn create_staging_file_with(
    dir: &Path,
    path: &Path,
    private: bool,
) -> std::io::Result<(PathBuf, fs::File)> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
    loop {
        let tmp_path = dir.join(format!(
            ".{}.tmp.{}.{}",
            path.file_name().and_then(|n| n.to_str()).unwrap_or("state"),
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed),
        ));
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        if private {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        #[cfg(not(unix))]
        let _ = private;
        match options.open(&tmp_path) {
            Ok(file) => return Ok((tmp_path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
}

/// Replaces a *user* file (workspace source, not SUSI state) atomically: a
/// crash or kill mid-write leaves either the old or the new content, never a
/// truncated file. Unlike [`atomic_write_bytes`] it keeps the file's
/// existing permissions (a new file gets the normal umask mode), and it
/// refuses to replace a symlink rather than follow or clobber it. Parent
/// directories are created on demand. Replacing breaks any extra hard links
/// to the old inode, which keep the previous content.
pub fn atomic_replace_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let existing = match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("refusing to replace symlink {}", path.display()),
            ))
        }
        Ok(meta) => Some(meta.permissions()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    let dir = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        Some(_) | None => Path::new("."),
    };
    fs::create_dir_all(dir)?;
    let (tmp_path, mut file) = create_staging_file_with(dir, path, false)?;
    let mut result = file.write_all(bytes).and_then(|()| file.sync_all());
    drop(file);
    if let (Ok(()), Some(perms)) = (&result, existing) {
        result = fs::set_permissions(&tmp_path, perms);
    }
    let result = result.and_then(|()| fs::rename(&tmp_path, path));
    if result.is_err() {
        let _ = fs::remove_file(&tmp_path);
    }
    result
}

/// Replaces `path` with `bytes` via a same-directory temp file + rename, so a
/// concurrent reader — another process's CLI invocation, the daemon's own
/// background cycle — never observes a torn/empty file. Each writer stages
/// into its own `create_new` temp file (process id + counter), so concurrent
/// writers in any process never share or truncate one another's staging
/// file; the last rename wins with a complete file. The parent directory is
/// created on demand. On Unix the file is `0600` so secrets that land in
/// host state are not world-readable under a permissive umask.
pub fn atomic_write_bytes(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let dir = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        Some(_) | None => Path::new("."),
    };
    fs::create_dir_all(dir)?;
    let (tmp_path, mut file) = create_staging_file(dir, path)?;
    let result = file.write_all(bytes).and_then(|()| file.sync_all());
    drop(file);
    let result = result.and_then(|()| fs::rename(&tmp_path, path));
    if result.is_err() {
        let _ = fs::remove_file(&tmp_path);
    }
    #[cfg(unix)]
    if result.is_ok() {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
    result
}

/// Join `user_path` under `workspace`, rejecting absolutes, `..`, and escapes.
pub fn confined_workspace_join(workspace: &Path, user_path: &str) -> EaiResult<PathBuf> {
    use std::path::Component;
    let user_path = user_path.trim().trim_matches('"').trim_matches('\'');
    let path = PathBuf::from(user_path);
    if path.is_absolute() {
        return Err(EaiError::config("Absolute paths not allowed"));
    }
    for component in path.components() {
        if matches!(component, Component::ParentDir) {
            return Err(EaiError::config("Parent directory traversal not allowed"));
        }
    }
    if !path.components().any(|c| matches!(c, Component::Normal(_))) {
        return Err(EaiError::config("Empty path not allowed"));
    }
    let full = workspace.join(&path);
    let canonical_workspace = workspace
        .canonicalize()
        .map_err(|e| EaiError::config(format!("Workspace error: {e}")))?;
    // Resolve through the deepest existing ancestor (the full path when it
    // exists, so a symlinked leaf is judged by where it really points), then
    // re-append the not-yet-created components unchanged. Missing parent
    // dirs must not collapse onto the workspace root.
    let mut existing = full.as_path();
    let mut pending = Vec::new();
    while std::fs::symlink_metadata(existing).is_err() {
        let (Some(parent), Some(name)) = (existing.parent(), existing.file_name()) else {
            break;
        };
        pending.push(name.to_os_string());
        existing = parent;
    }
    let mut resolved = existing
        .canonicalize()
        .map_err(|e| EaiError::config(format!("Path resolution error for {user_path}: {e}")))?;
    if !resolved.starts_with(&canonical_workspace) {
        return Err(EaiError::config(format!(
            "Path escape attempt: {user_path}"
        )));
    }
    for name in pending.iter().rev() {
        resolved.push(name);
    }
    if resolved.file_name().is_none() {
        return Err(EaiError::config(format!(
            "Path has no file name: {user_path}"
        )));
    }
    Ok(resolved)
}

/// Recursively backfills any key (object) or element (same-length array)
/// present in `default` but absent from `existing`. Returns whether
/// `existing` was modified. A value `existing` already has is never
/// overwritten, at any nesting depth.
pub fn merge_missing_json_defaults(existing: &mut DynamicValue, default: &DynamicValue) -> bool {
    match (existing, default) {
        (DynamicValue::Object(existing_map), DynamicValue::Object(default_map)) => {
            let mut changed = false;
            for (k, def_v) in default_map {
                match existing_map.get_mut(k) {
                    Some(existing_v) => {
                        if merge_missing_json_defaults(existing_v, def_v) {
                            changed = true;
                        }
                    }
                    None => {
                        existing_map.insert(k.clone(), def_v.clone());
                        changed = true;
                    }
                }
            }
            changed
        }
        (DynamicValue::Array(existing_arr), DynamicValue::Array(default_arr))
            if existing_arr.len() == default_arr.len() =>
        {
            let mut changed = false;
            for (e, d) in existing_arr.iter_mut().zip(default_arr.iter()) {
                if merge_missing_json_defaults(e, d) {
                    changed = true;
                }
            }
            changed
        }
        _ => false,
    }
}

/// Backfills into `existing` any top-level key present in `default` but
/// missing, and recursively self-heals nested values (via
/// `merge_missing_json_defaults`) for keys both sides already have. Returns
/// whether `existing` changed.
pub fn merge_missing_registry_defaults(
    existing: &mut DynamicRegistry,
    default: &DynamicRegistry,
) -> bool {
    let mut changed = false;
    for (key, default_val) in default {
        match existing.get_mut(key) {
            Some(existing_val) => {
                if merge_missing_json_defaults(existing_val, default_val) {
                    changed = true;
                }
            }
            None => {
                existing.insert(key.clone(), default_val.clone());
                changed = true;
            }
        }
    }
    changed
}
