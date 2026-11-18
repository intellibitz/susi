//! Dynamic JSON field accessors and Linux atomic file publication for plane-bus state.

use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub type DynamicValue = serde_json::Value;

/// Raw typed field reads for JSON values and object maps, without coercion,
/// trimming, defaults or errors.
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

fn create_staging_file(dir: &Path, path: &Path) -> std::io::Result<(PathBuf, fs::File)> {
    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
    loop {
        let tmp_path = dir.join(format!(
            ".{}.tmp.{}.{}",
            path.file_name().and_then(|n| n.to_str()).unwrap_or("state"),
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed),
        ));
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true).mode(0o600);
        match options.open(&tmp_path) {
            Ok(file) => return Ok((tmp_path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
}

/// Atomically replace `path` with `bytes` via an owner-only (`0600`) same-directory
/// temp file + rename without `fsync`, for rebuildable process-scoped endpoint state.
pub fn atomic_replace_unsynced(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        Some(_) | None => Path::new("."),
    };
    let (tmp_path, mut file) = match create_staging_file(dir, path) {
        Ok(staged) => staged,
        Err(_) => {
            fs::create_dir_all(dir)?;
            create_staging_file(dir, path)?
        }
    };
    let result = file.write_all(bytes);
    drop(file);
    let result = result.and_then(|()| fs::rename(&tmp_path, path));
    if result.is_err() {
        let _ = fs::remove_file(&tmp_path);
    } else {
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
    result
}

/// Read one complete snapshot of a file published by atomic replacement.
pub fn read_atomic_bytes(path: &Path) -> std::io::Result<Vec<u8>> {
    fs::read(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn raw_fields_preserve_types_text_bounds_and_absence() {
        let object = json!({"s": " \tλ\n", "n": 42_u64, "b": true, "neg": -1});
        for fields in [&object as &dyn JsonFields, object.as_object().unwrap()] {
            assert_eq!(fields.field_str("s"), Some(" \tλ\n"));
            assert_eq!(fields.field_u64("n"), Some(42));
            assert_eq!(fields.field_bool("b"), Some(true));
            assert_eq!(fields.field_u64("neg"), None);
            assert!(fields.field_value("missing").is_none());
        }
        assert_eq!(json!(null).field_str("s"), None);
    }

    #[test]
    fn atomic_replace_unsynced_replaces_private_content_and_cleans_staging() {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "susi-json-util-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        let parent = dir.join("nested");
        let path = parent.join("endpoint.json");
        atomic_replace_unsynced(&path, b"first, longer").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        atomic_replace_unsynced(&path, b"{}").unwrap();
        assert_eq!(read_atomic_bytes(&path).unwrap(), b"{}");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        fs::remove_file(&path).unwrap();
        fs::remove_dir(&parent).unwrap();
        atomic_replace_unsynced(&path, b"recreated").unwrap();
        assert_eq!(read_atomic_bytes(&path).unwrap(), b"recreated");

        let blocked = dir.join("dest-dir");
        fs::create_dir(&blocked).unwrap();
        assert!(atomic_replace_unsynced(&blocked, b"nope").is_err());
        let _ = fs::remove_dir_all(&dir);
    }
}
