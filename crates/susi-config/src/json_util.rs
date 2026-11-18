// susi Sandbox Manager: 100% DYNAMIC - Zero hardcoded keys
// Pattern used by Astral (ruff) and Claude Code: Registry + HashMap + Value
// Add new model, prompt, message, endpoint without touching Rust

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

// === CORE DYNAMIC TYPES ===
// Everything is a registry. No struct fields are hardcoded.

pub type DynamicValue = serde_json::Value;
pub type DynamicRegistry = HashMap<String, DynamicValue>;

/// Raw typed field reads for JSON values and object maps, without coercion,
/// trimming, defaults or errors. Missing fields and mismatched types return
/// `None`; callers retain their own validation and fallback policy.
pub trait JsonFields {
    /// Borrow a named object field; non-object values have no named fields.
    fn field_value(&self, key: &str) -> Option<&DynamicValue>;

    /// Borrow a string exactly as stored, including empty strings/whitespace.
    fn field_str(&self, key: &str) -> Option<&str> {
        self.field_value(key)?.as_str()
    }

    /// Read an integer representable by `u64`, without numeric coercion.
    fn field_u64(&self, key: &str) -> Option<u64> {
        self.field_value(key)?.as_u64()
    }

    /// Read a JSON boolean, without interpreting strings or numbers as flags.
    fn field_bool(&self, key: &str) -> Option<bool> {
        self.field_value(key)?.as_bool()
    }
}

impl JsonFields for DynamicValue {
    fn field_value(&self, key: &str) -> Option<&DynamicValue> {
        self.get(key)
    }
}

impl JsonFields for serde_json::Map<String, DynamicValue> {
    fn field_value(&self, key: &str) -> Option<&DynamicValue> {
        self.get(key)
    }
}

// ModelTier and ProviderType are now dynamic strings, not hardcoded enums

// === SHARED SELF-HEALING JSON LOAD/SAVE ===
// Every `*.default.json`-backed config type (SusiConfig, SusiPrompts,
// SusiMessages) needs the same two things: an atomic write (so a concurrent
// reader never observes a torn file) and a recursive merge that backfills a
// key/array-element present in the compiled-in default but missing from the
// user's persisted file, without ever touching a value the user already set.
// Factored out once here instead of three separately hand-rolled (and, until
// this was noticed, inconsistently deep) copies.

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

/// Read one complete snapshot of a file published by atomic replacement.
pub fn read_atomic_bytes(path: &Path) -> std::io::Result<Vec<u8>> {
    fs::read(path)
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn raw_fields_preserve_types_text_bounds_and_absence() {
        for (value, expected) in [
            (json!(" \tλ\n"), (Some(" \tλ\n"), None, None)),
            (json!(""), (Some(""), None, None)),
            (json!(0), (None, Some(0), None)),
            (json!(u64::MAX), (None, Some(u64::MAX), None)),
            (json!(true), (None, None, Some(true))),
            (json!(false), (None, None, Some(false))),
            (json!(null), (None, None, None)),
            (json!(-1), (None, None, None)),
            (json!(1.0), (None, None, None)),
            (json!(1.5), (None, None, None)),
            (json!(18446744073709551616.0), (None, None, None)),
            (json!([]), (None, None, None)),
            (json!({}), (None, None, None)),
        ] {
            let object = json!({"key": value});
            for fields in [&object as &dyn JsonFields, object.as_object().unwrap()] {
                assert_eq!(
                    (
                        fields.field_str("key"),
                        fields.field_u64("key"),
                        fields.field_bool("key")
                    ),
                    expected,
                );
                assert!(fields.field_value("missing").is_none());
            }
        }
        for non_object in [json!(null), json!([]), json!("key"), json!(1), json!(false)] {
            assert_eq!(
                (
                    non_object.field_str("key"),
                    non_object.field_u64("key"),
                    non_object.field_bool("key")
                ),
                (None, None, None),
            );
        }
    }

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
                let bytes = read_atomic_bytes(&path).unwrap();
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
}
