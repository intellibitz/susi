use std::path::{Path, PathBuf};
use std::time::SystemTime;
use parking_lot::RwLock;

/// Generic "versioned JSON config store" abstraction that unifies mtime-cache
/// and self-healing-merge logic originally hand-rolled in five places.
pub struct VersionedJsonStore<T> {
    cache: RwLock<Option<(SystemTime, PathBuf, T)>>,
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
    ) -> crate::error::EaiResult<T>
    where
        F: FnOnce() -> crate::error::EaiResult<T>, // Generates the default to adopt if missing
        H: FnOnce(&mut T) -> bool, // Modifies in-place, returns true if save needed
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
                    return Ok(cached_val.clone());
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
                return Ok(cached_val.clone());
            }
        }

        let val = self.read_from_disk(path, on_missing, heal_fn, strict_parse)?;

        let final_modified = std::fs::metadata(path)
            .and_then(|m| m.modified())
            .unwrap_or(current_modified);

        *guard = Some((final_modified, path.to_path_buf(), val.clone()));
        Ok(val)
    }

    /// Modifies the JSON configuration in-place, synchronizing the change to disk.
    /// Uses an exclusive lock to prevent process-local race conditions.
    pub fn modify<F, H, M>(
        &self,
        path: &Path,
        on_missing: F,
        heal_fn: H,
        strict_parse: bool,
        mut_fn: M,
    ) -> crate::error::EaiResult<T>
    where
        F: FnOnce() -> crate::error::EaiResult<T>,
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
                cached_val.clone()
            } else {
                self.read_from_disk(path, on_missing, heal_fn, strict_parse)?
            }
        } else {
            self.read_from_disk(path, on_missing, heal_fn, strict_parse)?
        };

        mut_fn(&mut val);

        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = crate::sandbox::manager::atomic_write_json_pretty(path, &val);

        let final_modified = std::fs::metadata(path)
            .and_then(|m| m.modified())
            .unwrap_or(current_modified);

        *guard = Some((final_modified, path.to_path_buf(), val.clone()));
        Ok(val)
    }

    fn read_from_disk<F, H>(
        &self,
        path: &Path,
        on_missing: F,
        heal_fn: H,
        strict_parse: bool,
    ) -> crate::error::EaiResult<T>
    where
        F: FnOnce() -> crate::error::EaiResult<T>,
        H: FnOnce(&mut T) -> bool,
    {
        let mut loaded_opt = None;
        if path.is_file() {
            match std::fs::read_to_string(path) {
                Ok(content) => match serde_json::from_str::<T>(&content) {
                    Ok(val) => loaded_opt = Some(val),
                    Err(e) => {
                        if strict_parse {
                            return Err(crate::error::EaiError::config(format!(
                                "Malformed configuration {}: {}",
                                path.display(),
                                e
                            )));
                        }
                    }
                },
                Err(e) => {
                    if strict_parse {
                        return Err(crate::error::EaiError::config(format!(
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
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = crate::sandbox::manager::atomic_write_json_pretty(path, &val);
        }

        Ok(val)
    }
}
