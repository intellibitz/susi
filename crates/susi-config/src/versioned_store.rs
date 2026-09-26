use parking_lot::RwLock;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

type CacheEntry<T> = (SystemTime, PathBuf, Arc<T>);

/// Generic "versioned JSON config store" abstraction that unifies mtime-cache
/// and self-healing-merge logic originally hand-rolled in five places.
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
        let current_modified = std::fs::metadata(path)
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);

        {
            let guard = self.cache.read();
            if let Some((cached_time, cached_path, cached_val)) = guard.as_ref() {
                if cached_path == path
                    && *cached_time == current_modified
                    && current_modified != SystemTime::UNIX_EPOCH
                {
                    return Ok(Arc::clone(cached_val));
                }
            }
        }

        let mut guard = self.cache.write();

        let current_modified = std::fs::metadata(path)
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);

        if let Some((cached_time, cached_path, cached_val)) = guard.as_ref() {
            if cached_path == path
                && *cached_time == current_modified
                && current_modified != SystemTime::UNIX_EPOCH
            {
                return Ok(Arc::clone(cached_val));
            }
        }

        let val = Arc::new(self.read_from_disk(path, on_missing, heal_fn, strict_parse)?);

        let final_modified = std::fs::metadata(path)
            .and_then(|m| m.modified())
            .unwrap_or(current_modified);

        *guard = Some((final_modified, path.to_path_buf(), Arc::clone(&val)));
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
        let modified = std::fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        *guard = Some((modified, path.to_path_buf(), Arc::clone(&value)));
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

        let current_modified = std::fs::metadata(path)
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);

        let mut val = if let Some((cached_time, cached_path, cached_val)) = guard.as_ref() {
            if cached_path == path
                && *cached_time == current_modified
                && current_modified != SystemTime::UNIX_EPOCH
            {
                (**cached_val).clone()
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
        crate::atomic_write_json_pretty(path, &val)?;

        let final_modified = std::fs::metadata(path)
            .and_then(|m| m.modified())
            .unwrap_or(current_modified);

        let shared = Arc::new(val.clone());
        *guard = Some((final_modified, path.to_path_buf(), Arc::clone(&shared)));
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
            crate::atomic_write_json_pretty(path, &val)?;
        }

        Ok(val)
    }
}

impl<T: Clone + serde::de::DeserializeOwned + serde::Serialize> Default for VersionedJsonStore<T> {
    fn default() -> Self {
        Self::new()
    }
}
