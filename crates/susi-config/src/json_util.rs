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

/// Writes `value` as pretty JSON to `path` via a same-directory temp file +
/// rename, so a concurrent reader — another process's CLI invocation, the
/// daemon's own background cycle — never observes a torn/empty file.
/// On Unix the temp file is created `0600` so secrets that land in config
/// (or adjacent host JSON) are not world-readable under a permissive umask.
pub fn atomic_write_json_pretty<T: Serialize>(path: &Path, value: &T) -> EaiResult<()> {
    let json = serde_json::to_string_pretty(value).map_err(|e| EaiError::config(e.to_string()))?;
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
    // Each writer owns a distinct file, including simultaneous writes in one process.
    let (tmp_path, mut file) = loop {
        let tmp_path = dir.join(format!(
            ".{}.tmp.{}.{}",
            path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("config"),
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed),
        ));
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&tmp_path) {
            Ok(file) => break (tmp_path, file),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(EaiError::config(error.to_string())),
        }
    };
    let result = file
        .write_all(json.as_bytes())
        .and_then(|()| file.sync_all());
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
    result.map_err(|error| EaiError::config(error.to_string()))
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

/// Shared ureq Agent with connect/read/write timeouts. `ureq::get`/`ureq::post`
/// free functions use a default agent with NO timeouts at all — a stalled
/// remote (or one that completes the handshake but then goes silent
/// mid-response, e.g. during SSE body streaming) blocks the calling thread
/// forever. Agent-level timeout_read/timeout_write bound every socket read
/// and write, including streaming body reads after the initial response
/// headers arrive, which a per-request `.timeout()` alone would not cover.
static HTTP_AGENT: std::sync::OnceLock<ureq::Agent> = std::sync::OnceLock::new();

pub fn http_agent() -> ureq::Agent {
    HTTP_AGENT
        .get_or_init(|| {
            let config = ureq::Agent::config_builder()
                .timeout_connect(Some(std::time::Duration::from_secs(10)))
                .timeout_recv_body(Some(std::time::Duration::from_secs(20)))
                .timeout_send_body(Some(std::time::Duration::from_secs(20)))
                .build();
            ureq::Agent::new_with_config(config)
        })
        .clone()
}
