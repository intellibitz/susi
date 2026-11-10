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

/// Which file `meta` describes, beyond its size and modification time: on
/// Unix its device and inode, and for anything but a directory its change
/// time (a rewrite in place whose mtime was then restored, as `cp -p` does,
/// still moves it); on Windows its creation time (a file replaced by another
/// — renamed over, or deleted and written again — is a new file). Two stats
/// of one unchanged file give the same string; empty where neither is
/// available. For identities that must not mistake a replaced file of the
/// same size and mtime for the old one. A directory's change time is left
/// out: [`create_private_dir`] re-applies its mode, which moves it, on every
/// call.
#[must_use]
pub fn file_node(meta: &fs::Metadata) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.is_dir() {
            format!("{}:{}", meta.dev(), meta.ino())
        } else {
            format!(
                "{}:{}:{}.{}",
                meta.dev(),
                meta.ino(),
                meta.ctime(),
                meta.ctime_nsec()
            )
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        format!("c{}", meta.creation_time())
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = meta;
        String::new()
    }
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
    replace_bytes(path, bytes, true)
}

/// [`atomic_write_bytes`] without flushing the staged bytes to stable storage
/// before the rename. Readers still never see a torn or empty file, but after
/// a power loss the file may be missing or empty. Only for rebuildable state
/// whose authority ends with the writing process, such as plane-bus
/// rendezvous and capability endpoint files: a crash makes those stale
/// whatever their durability. One synced write costs milliseconds on
/// Windows, and a first goal publishes a few hundred of these files.
pub fn atomic_replace_unsynced(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    replace_bytes(path, bytes, false)
}

fn replace_bytes(path: &Path, bytes: &[u8], sync: bool) -> std::io::Result<()> {
    use std::io::Write;
    let dir = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        Some(_) | None => Path::new("."),
    };
    let (tmp_path, mut file) = if sync {
        fs::create_dir_all(dir)?;
        create_staging_file(dir, path)?
    } else {
        // Rebuildable endpoint publications usually share an existing parent.
        // Try the same unique staging create first, avoiding a directory probe
        // per file. On failure, preserve the original preparation/error path;
        // a parent removed since the last publication is recreated as before.
        match create_staging_file(dir, path) {
            Ok(staged) => staged,
            Err(_) => {
                fs::create_dir_all(dir)?;
                create_staging_file(dir, path)?
            }
        }
    };
    let result = file
        .write_all(bytes)
        .and_then(|()| if sync { file.sync_all() } else { Ok(()) });
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn scratch(tag: &str) -> PathBuf {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "susi-json-util-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn entries(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn atomic_write_json_pretty_creates_parents_and_replaces_without_staging_files() {
        let dir = scratch("pretty");
        let path = dir.join("nested").join("deeper").join("state.json");
        atomic_write_json_pretty(&path, &json!({"a": 1})).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains('\n'), "pretty output is multi-line");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&text).unwrap(),
            json!({"a": 1})
        );

        atomic_write_json_pretty(&path, &json!({"a": 2, "b": [true]})).unwrap();
        let again: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(again, json!({"a": 2, "b": [true]}));
        assert_eq!(entries(path.parent().unwrap()), ["state.json"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn atomic_write_bytes_replaces_whole_content_and_leaves_no_temporaries() {
        let dir = scratch("bytes");
        let path = dir.join("blob.bin");
        atomic_write_bytes(&path, b"first, longer content").unwrap();
        atomic_write_bytes(&path, b"short").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"short");
        assert_eq!(entries(&dir), ["blob.bin"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn atomic_replace_unsynced_replaces_whole_content_and_leaves_no_temporaries() {
        let dir = scratch("unsynced");
        let path = dir.join("nested").join("endpoint.json");
        atomic_replace_unsynced(&path, b"{\"endpoint\":\"127.0.0.1:1\",\"key\":\"long\"}").unwrap();
        atomic_replace_unsynced(&path, b"{}").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"{}");
        assert_eq!(entries(&dir.join("nested")), ["endpoint.json"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn atomic_replace_unsynced_recreates_a_removed_parent() {
        let dir = scratch("unsynced-recreate");
        let parent = dir.join("nested").join("endpoints");
        let path = parent.join("tool.json");
        atomic_replace_unsynced(&path, b"first").unwrap();
        fs::remove_file(&path).unwrap();
        fs::remove_dir(&parent).unwrap();

        atomic_replace_unsynced(&path, b"recreated").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"recreated");
        assert_eq!(entries(&parent), ["tool.json"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn atomic_replace_unsynced_preserves_invalid_parent_errors() {
        let dir = scratch("unsynced-invalid-parent");
        let parent = dir.join("blocker");
        fs::write(&parent, b"leave this file intact").unwrap();
        let path = parent.join("endpoint.json");

        let expected = atomic_write_bytes(&path, b"never published").unwrap_err();
        let actual = atomic_replace_unsynced(&path, b"never published").unwrap_err();
        assert_eq!(actual.kind(), expected.kind());
        assert_eq!(fs::read(&parent).unwrap(), b"leave this file intact");
        assert_eq!(entries(&dir), ["blocker"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn atomic_replace_unsynced_cleans_staging_after_rename_failure() {
        let dir = scratch("unsynced-rename-failure");
        let path = dir.join("destination-directory");
        fs::create_dir(&path).unwrap();
        fs::write(path.join("kept"), b"unchanged").unwrap();

        assert!(atomic_replace_unsynced(&path, b"never published").is_err());
        assert_eq!(fs::read(path.join("kept")).unwrap(), b"unchanged");
        assert_eq!(entries(&dir), ["destination-directory"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn atomic_replace_unsynced_concurrent_readers_only_observe_complete_files() {
        use std::sync::Barrier;

        let dir = scratch("unsynced-concurrent");
        let path = dir.join("endpoint.json");
        const LENGTH: usize = 16 * 1024;
        atomic_replace_unsynced(&path, &vec![b'0'; LENGTH]).unwrap();
        let active = AtomicUsize::new(4);
        let ready = Barrier::new(5);
        std::thread::scope(|scope| {
            for marker in b'1'..=b'4' {
                let path = &path;
                let active = &active;
                let ready = &ready;
                scope.spawn(move || {
                    let bytes = vec![marker; LENGTH];
                    ready.wait();
                    let result = (|| -> std::io::Result<()> {
                        for _ in 0..16 {
                            atomic_replace_unsynced(path, &bytes)?;
                        }
                        Ok(())
                    })();
                    // Release the reader even if an I/O error fails this writer.
                    active.fetch_sub(1, Ordering::Release);
                    result.unwrap();
                });
            }
            ready.wait();
            loop {
                let bytes = fs::read(&path).unwrap();
                assert_eq!(bytes.len(), LENGTH);
                assert!((b'0'..=b'4').contains(&bytes[0]));
                assert!(bytes.iter().all(|byte| *byte == bytes[0]));
                if active.load(Ordering::Acquire) == 0 {
                    break;
                }
                std::thread::yield_now();
            }
        });
        assert_eq!(entries(&dir), ["endpoint.json"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn atomic_replace_unsynced_keeps_new_and_replaced_files_private() {
        use std::os::unix::fs::PermissionsExt;

        let dir = scratch("unsynced-permissions");
        let path = dir.join("nested").join("endpoint.json");
        atomic_replace_unsynced(&path, b"first").unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        atomic_replace_unsynced(&path, b"replacement").unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(entries(path.parent().unwrap()), ["endpoint.json"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn atomic_replace_unsynced_preserves_readonly_destination_on_failure() {
        let dir = scratch("unsynced-readonly");
        let path = dir.join("endpoint.json");
        atomic_replace_unsynced(&path, b"keep readonly content").unwrap();
        let original = fs::metadata(&path).unwrap().permissions();
        let mut readonly = original.clone();
        readonly.set_readonly(true);
        fs::set_permissions(&path, readonly).unwrap();

        let result = atomic_replace_unsynced(&path, b"never published");
        let still_readonly = fs::metadata(&path).unwrap().permissions().readonly();
        let content = fs::read(&path).unwrap();
        let remaining = entries(&dir);
        fs::set_permissions(&path, original).unwrap();
        assert!(result.is_err());
        assert!(still_readonly);
        assert_eq!(content, b"keep readonly content");
        assert_eq!(remaining, ["endpoint.json"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn remove_file_if_present_is_idempotent_but_reports_real_failures() {
        let dir = scratch("remove");
        let path = dir.join("gone.txt");
        fs::write(&path, "x").unwrap();
        remove_file_if_present(&path).unwrap();
        assert!(!path.exists());
        remove_file_if_present(&path).unwrap();
        // A directory is not a removable file: that is a real error, not "absent".
        assert!(remove_file_if_present(&dir).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn create_private_dir_creates_nested_directories_idempotently() {
        let dir = scratch("private-dir");
        let nested = dir.join("a").join("b");
        create_private_dir(&nested).unwrap();
        create_private_dir(&nested).unwrap();
        assert!(nested.is_dir());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn json_overrides_are_written_per_id_and_cleared_idempotently() {
        let dir = scratch("override");
        let overrides = dir.join("overrides");
        write_json_override(&overrides, "model-a", &json!({"tier": "Deep"})).unwrap();
        write_json_override(&overrides, "model-b", &json!({"tier": "Reflex"})).unwrap();
        assert_eq!(entries(&overrides), ["model-a.json", "model-b.json"]);
        let stored: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(overrides.join("model-a.json")).unwrap())
                .unwrap();
        assert_eq!(stored, json!({"tier": "Deep"}));

        clear_json_override(&overrides, "model-a").unwrap();
        clear_json_override(&overrides, "model-a").unwrap();
        clear_json_override(&overrides, "never-written").unwrap();
        assert_eq!(entries(&overrides), ["model-b.json"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_or_create_secret_creates_32_bytes_once_and_reloads_the_same_key() {
        let dir = scratch("secret");
        let path = dir.join("keys").join("cluster.key");
        let first = load_or_create_secret(&path).unwrap();
        assert_eq!(fs::read(&path).unwrap(), first);
        assert_ne!(first, [0u8; 32], "an entropy-backed key is never all zero");
        assert_eq!(load_or_create_secret(&path).unwrap(), first);
        assert_eq!(entries(path.parent().unwrap()), ["cluster.key"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_or_create_secret_never_replaces_a_wrong_length_secret() {
        let dir = scratch("secret-bad");
        for (name, bytes) in [
            ("short", vec![7u8; 31]),
            ("long", vec![7u8; 33]),
            ("empty", vec![]),
        ] {
            let path = dir.join(name);
            fs::write(&path, &bytes).unwrap();
            let err = load_or_create_secret(&path).unwrap_err();
            assert_eq!(err.kind(), std::io::ErrorKind::InvalidData, "{name}");
            assert!(err.to_string().contains("expected 32"), "{err}");
            assert_eq!(fs::read(&path).unwrap(), bytes, "{name} left untouched");
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn concurrent_first_use_converges_on_a_single_secret() {
        let dir = scratch("secret-race");
        let path = dir.join("race.key");
        let keys: Vec<[u8; 32]> = (0..6)
            .map(|_| {
                let path = path.clone();
                std::thread::spawn(move || load_or_create_secret(&path).unwrap())
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();
        assert!(keys.windows(2).all(|pair| pair[0] == pair[1]));
        assert_eq!(fs::read(&path).unwrap(), keys[0]);
        assert_eq!(entries(&dir), ["race.key"], "no staging file may remain");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn install_private_file_never_overwrites_an_existing_destination() {
        let dir = scratch("install");
        let path = dir.join("sub").join("token");
        assert!(install_private_file(&path, b"winner").unwrap());
        assert!(!install_private_file(&path, b"loser").unwrap());
        assert_eq!(fs::read(&path).unwrap(), b"winner");
        assert_eq!(entries(path.parent().unwrap()), ["token"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn atomic_replace_file_replaces_content_and_creates_parents() {
        let dir = scratch("replace");
        let path = dir.join("src").join("lib.rs");
        atomic_replace_file(&path, b"fn a() {}").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"fn a() {}");
        atomic_replace_file(&path, b"fn b() {}").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"fn b() {}");
        assert_eq!(entries(path.parent().unwrap()), ["lib.rs"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn atomic_replace_file_refuses_to_follow_or_clobber_a_symlink() {
        let dir = scratch("replace-link");
        let target = dir.join("target.txt");
        let link = dir.join("link.txt");
        fs::write(&target, "original").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let err = atomic_replace_file(&link, b"hijack").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        assert_eq!(fs::read_to_string(&target).unwrap(), "original");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_node_is_stable_for_one_file_and_changes_when_it_is_replaced() {
        let dir = scratch("node");
        let path = dir.join("watched.txt");
        fs::write(&path, "same size").unwrap();
        let first = file_node(&fs::metadata(&path).unwrap());
        assert_eq!(first, file_node(&fs::metadata(&path).unwrap()));
        #[cfg(any(unix, windows))]
        assert!(!first.is_empty());

        // Replace with identical-length content: size alone cannot tell them apart.
        std::thread::sleep(std::time::Duration::from_millis(40));
        atomic_replace_file(&path, b"SAME SIZE").unwrap();
        #[cfg(any(unix, windows))]
        assert_ne!(first, file_node(&fs::metadata(&path).unwrap()));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn confined_workspace_join_admits_relative_paths_including_missing_subdirectories() {
        let dir = scratch("confine-ok");
        let canonical = dir.canonicalize().unwrap();

        let top = confined_workspace_join(&dir, "notes.txt").unwrap();
        assert_eq!(top, canonical.join("notes.txt"));

        let nested = confined_workspace_join(&dir, "new/sub/dir/file.rs").unwrap();
        assert!(nested.starts_with(&canonical));
        assert!(nested.ends_with(Path::new("new").join("sub").join("dir").join("file.rs")));
        assert!(
            !dir.join("new").exists(),
            "joining must not create anything"
        );

        let quoted = confined_workspace_join(&dir, "  \"quoted.txt\"  ").unwrap();
        assert_eq!(quoted, canonical.join("quoted.txt"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn confined_workspace_join_rejects_absolute_traversal_and_empty_paths() {
        let dir = scratch("confine-bad");
        let absolute = std::env::temp_dir().to_string_lossy().into_owned();
        for (input, needle) in [
            (absolute.as_str(), "Absolute paths not allowed"),
            ("../outside.txt", "Parent directory traversal"),
            ("a/../../outside.txt", "Parent directory traversal"),
            ("a/b/..", "Parent directory traversal"),
            ("", "Empty path not allowed"),
            ("   ", "Empty path not allowed"),
            (".", "Empty path not allowed"),
            ("./", "Empty path not allowed"),
        ] {
            let err = confined_workspace_join(&dir, input)
                .unwrap_err()
                .to_string();
            assert!(
                err.contains(needle),
                "{input:?}: wanted {needle:?}, got {err}"
            );
        }
        let missing = dir.join("no-such-workspace");
        let err = confined_workspace_join(&missing, "a.txt")
            .unwrap_err()
            .to_string();
        assert!(err.contains("Workspace error"), "{err}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn confined_workspace_join_rejects_a_symlink_that_escapes_the_workspace() {
        let dir = scratch("confine-link");
        let outside = scratch("confine-outside");
        std::os::unix::fs::symlink(&outside, dir.join("escape")).unwrap();
        let err = confined_workspace_join(&dir, "escape/file.txt")
            .unwrap_err()
            .to_string();
        assert!(err.contains("Path escape attempt"), "{err}");
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&outside);
    }

    #[test]
    fn merge_missing_json_defaults_backfills_without_overwriting_user_values() {
        let mut existing = json!({
            "keep": "user",
            "nested": {"user_only": 1, "shared": "mine"},
            "explicit_null": null
        });
        let default = json!({
            "keep": "default",
            "added": 5,
            "nested": {"shared": "default", "fresh": {"deep": true}},
            "explicit_null": {"would": "replace"}
        });
        assert!(merge_missing_json_defaults(&mut existing, &default));
        assert_eq!(
            existing,
            json!({
                "keep": "user",
                "added": 5,
                "nested": {
                    "user_only": 1,
                    "shared": "mine",
                    "fresh": {"deep": true}
                },
                "explicit_null": null
            })
        );
        // Converged: a second pass changes nothing.
        assert!(!merge_missing_json_defaults(&mut existing, &default));
    }

    #[test]
    fn merge_missing_json_defaults_heals_same_length_arrays_only() {
        let mut same = json!([{"a": 1}, {"b": 2}]);
        let default_same = json!([{"a": 9, "x": 0}, {"b": 9, "y": 0}]);
        assert!(merge_missing_json_defaults(&mut same, &default_same));
        assert_eq!(same, json!([{"a": 1, "x": 0}, {"b": 2, "y": 0}]));

        let mut shorter = json!([{"a": 1}]);
        let before = shorter.clone();
        assert!(!merge_missing_json_defaults(&mut shorter, &default_same));
        assert_eq!(shorter, before, "length-mismatched arrays are user data");
    }

    #[test]
    fn merge_missing_json_defaults_ignores_type_mismatches() {
        let mut scalar = json!("user");
        assert!(!merge_missing_json_defaults(&mut scalar, &json!({"k": 1})));
        assert_eq!(scalar, json!("user"));
        let mut object = json!({"k": 1});
        assert!(!merge_missing_json_defaults(&mut object, &json!([1, 2])));
        assert_eq!(object, json!({"k": 1}));
        let mut equal = json!({"k": [1, 2]});
        assert!(!merge_missing_json_defaults(
            &mut equal,
            &json!({"k": [3, 4]})
        ));
    }

    #[test]
    fn merge_missing_registry_defaults_backfills_top_level_and_heals_nested() {
        let mut existing: DynamicRegistry = serde_json::from_value(json!({
            "mine": "kept",
            "section": {"user": 1}
        }))
        .unwrap();
        let default: DynamicRegistry = serde_json::from_value(json!({
            "mine": "default",
            "section": {"user": 0, "added": 2},
            "top_level_new": [1]
        }))
        .unwrap();
        assert!(merge_missing_registry_defaults(&mut existing, &default));
        assert_eq!(existing["mine"], json!("kept"));
        assert_eq!(existing["section"], json!({"user": 1, "added": 2}));
        assert_eq!(existing["top_level_new"], json!([1]));
        assert!(!merge_missing_registry_defaults(&mut existing, &default));
        assert!(!merge_missing_registry_defaults(
            &mut existing,
            &DynamicRegistry::new()
        ));
    }
}
