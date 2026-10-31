use parking_lot::RwLock;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

pub(crate) struct CacheEntry<T> {
    pub(crate) modified: SystemTime,
    pub(crate) len: u64,
    pub(crate) path: PathBuf,
    pub(crate) value: Arc<T>,
}

fn file_stamp(path: &Path) -> (SystemTime, u64) {
    match std::fs::metadata(path) {
        Ok(meta) => (
            meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            meta.len(),
        ),
        Err(_) => (SystemTime::UNIX_EPOCH, 0),
    }
}

/// Generic "versioned JSON config store" abstraction that unifies the
/// stamp cache and self-healing-merge logic originally hand-rolled in five
/// places. A hit needs the same path, modification time, and length: a
/// same-tick rewrite that changes the file's size must not serve the
/// previous snapshot.
///
/// The cache holds `Arc<T>` so hot paths can share a snapshot without cloning
/// the full registry on every `load_*` hit.
pub struct VersionedJsonStore<T> {
    pub(crate) cache: RwLock<Option<CacheEntry<T>>>,
}

impl<T: Clone + serde::de::DeserializeOwned + serde::Serialize> VersionedJsonStore<T> {
    pub const fn new() -> Self {
        Self {
            cache: RwLock::new(None),
        }
    }

    pub fn load_with_healing<F, H>(
        &self,
        path: &Path,
        on_missing: F,
        heal_fn: H,
        strict_parse: bool,
    ) -> crate::susi_error::EaiResult<T>
    where
        F: FnOnce() -> crate::susi_error::EaiResult<T>,
        H: FnOnce(&mut T) -> bool,
    {
        Ok((*self.load_arc_with_healing(path, on_missing, heal_fn, strict_parse)?).clone())
    }

    /// Same as [`Self::load_with_healing`] but returns a shared `Arc` so cache
    /// hits avoid cloning the full value tree.
    pub fn load_arc_with_healing<F, H>(
        &self,
        path: &Path,
        on_missing: F,
        heal_fn: H,
        strict_parse: bool,
    ) -> crate::susi_error::EaiResult<Arc<T>>
    where
        F: FnOnce() -> crate::susi_error::EaiResult<T>,
        H: FnOnce(&mut T) -> bool,
    {
        let (current_modified, current_len) = file_stamp(path);

        {
            let guard = self.cache.read();
            if let Some(cached) = guard.as_ref() {
                if cached.path == path
                    && cached.modified == current_modified
                    && cached.len == current_len
                    && current_modified != SystemTime::UNIX_EPOCH
                {
                    return Ok(Arc::clone(&cached.value));
                }
            }
        }

        let mut guard = self.cache.write();

        let (current_modified, current_len) = file_stamp(path);

        if let Some(cached) = guard.as_ref() {
            if cached.path == path
                && cached.modified == current_modified
                && cached.len == current_len
                && current_modified != SystemTime::UNIX_EPOCH
            {
                return Ok(Arc::clone(&cached.value));
            }
        }

        let val = Arc::new(self.read_from_disk(path, on_missing, heal_fn, strict_parse)?);

        let (final_modified, final_len) = file_stamp(path);

        *guard = Some(CacheEntry {
            modified: final_modified,
            len: final_len,
            path: path.to_path_buf(),
            value: Arc::clone(&val),
        });
        Ok(val)
    }

    /// Reads from disk even if its modification time matches the cached version.
    /// Updates the cache under the same lock used by normal reads and mutations.
    pub fn reload_with_healing<F, H>(
        &self,
        path: &Path,
        on_missing: F,
        heal_fn: H,
        strict_parse: bool,
    ) -> crate::susi_error::EaiResult<T>
    where
        F: FnOnce() -> crate::susi_error::EaiResult<T>,
        H: FnOnce(&mut T) -> bool,
    {
        Ok((*self.reload_arc_with_healing(path, on_missing, heal_fn, strict_parse)?).clone())
    }

    pub fn reload_arc_with_healing<F, H>(
        &self,
        path: &Path,
        on_missing: F,
        heal_fn: H,
        strict_parse: bool,
    ) -> crate::susi_error::EaiResult<Arc<T>>
    where
        F: FnOnce() -> crate::susi_error::EaiResult<T>,
        H: FnOnce(&mut T) -> bool,
    {
        let mut guard = self.cache.write();
        // A failed reload must not leave a previously valid value cached.
        *guard = None;
        let value = Arc::new(self.read_from_disk(path, on_missing, heal_fn, strict_parse)?);
        let (modified, len) = file_stamp(path);
        *guard = Some(CacheEntry {
            modified,
            len,
            path: path.to_path_buf(),
            value: Arc::clone(&value),
        });
        Ok(value)
    }

    /// Modifies the JSON configuration in-place, synchronizing the change to disk.
    /// Uses an exclusive lock to prevent process-local race conditions.
    #[allow(clippy::too_many_arguments)]
    pub fn modify<F, H, M>(
        &self,
        path: &Path,
        on_missing: F,
        heal_fn: H,
        strict_parse: bool,
        mut_fn: M,
    ) -> crate::susi_error::EaiResult<T>
    where
        F: FnOnce() -> crate::susi_error::EaiResult<T>,
        H: FnOnce(&mut T) -> bool,
        M: FnOnce(&mut T),
    {
        let mut guard = self.cache.write();

        let (current_modified, current_len) = file_stamp(path);

        let mut val = if let Some(cached) = guard.as_ref() {
            if cached.path == path
                && cached.modified == current_modified
                && cached.len == current_len
                && current_modified != SystemTime::UNIX_EPOCH
            {
                (*cached.value).clone()
            } else {
                self.read_from_disk(path, on_missing, heal_fn, strict_parse)?
            }
        } else {
            self.read_from_disk(path, on_missing, heal_fn, strict_parse)?
        };

        mut_fn(&mut val);

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        super::atomic_write_json_pretty(path, &val)?;

        let (final_modified, final_len) = file_stamp(path);

        let shared = Arc::new(val.clone());
        *guard = Some(CacheEntry {
            modified: final_modified,
            len: final_len,
            path: path.to_path_buf(),
            value: Arc::clone(&shared),
        });
        Ok(val)
    }

    fn read_from_disk<F, H>(
        &self,
        path: &Path,
        on_missing: F,
        heal_fn: H,
        strict_parse: bool,
    ) -> crate::susi_error::EaiResult<T>
    where
        F: FnOnce() -> crate::susi_error::EaiResult<T>,
        H: FnOnce(&mut T) -> bool,
    {
        let mut loaded_opt = None;
        if path.is_file() {
            match std::fs::read_to_string(path) {
                Ok(content) => match serde_json::from_str::<T>(&content) {
                    Ok(val) => loaded_opt = Some(val),
                    Err(e) => {
                        if strict_parse {
                            return Err(crate::susi_error::EaiError::config(format!(
                                "Malformed configuration {}: {}",
                                path.display(),
                                e
                            )));
                        }
                    }
                },
                Err(e) => {
                    if strict_parse {
                        return Err(crate::susi_error::EaiError::config(format!(
                            "Failed to read {}: {}",
                            path.display(),
                            e
                        )));
                    }
                }
            }
        }

        let (mut val, mut needs_save) = match loaded_opt {
            Some(v) => (v, false),
            None => {
                let default_val = on_missing()?;
                (default_val, true)
            }
        };

        if heal_fn(&mut val) {
            needs_save = true;
        }

        if needs_save {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            super::atomic_write_json_pretty(path, &val)?;
        }

        Ok(val)
    }
}

impl<T: Clone + serde::de::DeserializeOwned + serde::Serialize> Default for VersionedJsonStore<T> {
    fn default() -> Self {
        Self::new()
    }
}
