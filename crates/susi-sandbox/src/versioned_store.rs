use parking_lot::RwLock;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

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
    ) -> susi_error::EaiResult<T>
    where
        F: FnOnce() -> susi_error::EaiResult<T>, // Generates the default to adopt if missing
        H: FnOnce(&mut T) -> bool,               // Modifies in-place, returns true if save needed
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

    /// Reads from disk even if its modification time matches the cached version.
    /// Updates the cache under the same lock used by normal reads and mutations.
    pub fn reload_with_healing<F, H>(
        &self,
        path: &Path,
        on_missing: F,
        heal_fn: H,
        strict_parse: bool,
    ) -> susi_error::EaiResult<T>
    where
        F: FnOnce() -> susi_error::EaiResult<T>,
        H: FnOnce(&mut T) -> bool,
    {
        let mut guard = self.cache.write();
        // A failed reload must not leave a previously valid value cached.
        *guard = None;
        let value = self.read_from_disk(path, on_missing, heal_fn, strict_parse)?;
        let modified = std::fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        *guard = Some((modified, path.to_path_buf(), value.clone()));
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
    ) -> susi_error::EaiResult<T>
    where
        F: FnOnce() -> susi_error::EaiResult<T>,
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
            std::fs::create_dir_all(parent)?;
        }
        crate::manager::atomic_write_json_pretty(path, &val)?;

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
    ) -> susi_error::EaiResult<T>
    where
        F: FnOnce() -> susi_error::EaiResult<T>,
        H: FnOnce(&mut T) -> bool,
    {
        let mut loaded_opt = None;
        if path.is_file() {
            match std::fs::read_to_string(path) {
                Ok(content) => match serde_json::from_str::<T>(&content) {
                    Ok(val) => loaded_opt = Some(val),
                    Err(e) => {
                        if strict_parse {
                            return Err(susi_error::EaiError::config(format!(
                                "Malformed configuration {}: {}",
                                path.display(),
                                e
                            )));
                        }
                    }
                },
                Err(e) => {
                    if strict_parse {
                        return Err(susi_error::EaiError::config(format!(
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
            crate::manager::atomic_write_json_pretty(path, &val)?;
        }

        Ok(val)
    }
}

impl<T: Clone + serde::de::DeserializeOwned + serde::Serialize> Default for VersionedJsonStore<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "susi-store-{}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn config_defaults_are_consistent_and_independent() {
        use crate::manager::SusiConfig;
        let mut direct = SusiConfig::default();
        let generic: SusiConfig = Default::default();
        assert_eq!(direct.settings, generic.settings);
        assert!(!generic.settings.is_empty());
        direct.settings.clear();
        assert_eq!(SusiConfig::default().settings, generic.settings);
        let sparse: SusiConfig = serde_json::from_str(r#"{"custom_setting":true}"#).unwrap();
        assert_eq!(sparse.settings.len(), 1);
        assert_eq!(sparse.gmcp_port(), generic.gmcp_port());
    }

    #[test]
    fn config_reload_bypasses_unchanged_mtime_without_rewriting() {
        use crate::manager::SusiConfig;
        let dir = TestDir::new();
        let nested = dir.0.join("new/config");
        SusiConfig::default().save(&nested).unwrap();
        let path = SusiConfig::get_config_path(&nested);
        let mut cfg = SusiConfig::load(&nested).unwrap();
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        cfg.settings
            .insert("gmcp_port".into(), serde_json::json!(12345));
        cfg.settings.insert(
            "custom_setting".into(),
            serde_json::json!({"enabled": true}),
        );
        // Use compact JSON so an unnecessary pretty-print rewrite is observable.
        let content = serde_json::to_string(&cfg).unwrap();
        std::fs::write(&path, &content).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
        assert_eq!(SusiConfig::reload(&nested).unwrap().gmcp_port(), 12345);
        assert_eq!(SusiConfig::load(&nested).unwrap().gmcp_port(), 12345);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), content);
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            modified
        );
    }

    #[test]
    fn failed_reload_invalidates_previous_cached_value() {
        let dir = TestDir::new();
        let path = dir.0.join("config.json");
        let store = VersionedJsonStore::<u32>::new();
        store
            .load_with_healing(&path, || Ok(1), |_| false, true)
            .unwrap();
        std::fs::write(&path, "malformed").unwrap();
        assert!(store
            .reload_with_healing(&path, || Ok(1), |_| false, true)
            .is_err());
        assert!(store.cache.read().is_none());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "malformed");
    }

    #[test]
    fn concurrent_atomic_writers_publish_complete_json_and_clean_up() {
        let dir = TestDir::new();
        let path = dir.0.join("config.json");
        let barrier = std::sync::Barrier::new(8);
        std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|writer| {
                    let path = &path;
                    let barrier = &barrier;
                    scope.spawn(move || {
                        barrier.wait();
                        crate::manager::atomic_write_json_pretty(path, &vec![writer; 1000])
                            .unwrap();
                        let value: Vec<u32> =
                            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
                        assert_eq!(value.len(), 1000);
                        assert!(value.iter().all(|element| *element == value[0]));
                    })
                })
                .collect();
            for handle in handles {
                handle.join().unwrap();
            }
        });
        assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 1);
    }

    #[test]
    fn failed_modify_does_not_report_success_or_cache_unsaved_value() {
        let dir = TestDir::new();
        let path = dir.0.join("config.json");
        let store = VersionedJsonStore::<u32>::new();
        assert_eq!(
            store
                .load_with_healing(&path, || Ok(1), |_| false, true)
                .unwrap(),
            1
        );
        assert!(store
            .modify(
                &path,
                || Ok(1),
                |_| false,
                true,
                |value| {
                    *value = 2;
                    // Fail the write after the cached value has been read and mutated.
                    std::fs::remove_file(&path).unwrap();
                    std::fs::create_dir(&path).unwrap();
                }
            )
            .is_err());
        assert_eq!(store.cache.read().as_ref().unwrap().2, 1);
    }

    #[test]
    fn failed_initial_save_leaves_cache_empty() {
        let dir = TestDir::new();
        let blocker = dir.0.join("file");
        std::fs::write(&blocker, "not a directory").unwrap();
        let store = VersionedJsonStore::<u32>::new();
        assert!(store
            .load_with_healing(&blocker.join("config.json"), || Ok(1), |_| false, true)
            .is_err());
        assert!(store.cache.read().is_none());
    }

    #[test]
    fn successful_modify_persists_and_malformed_json_is_preserved() {
        let dir = TestDir::new();
        let path = dir.0.join("config.json");
        let store = VersionedJsonStore::<u32>::new();
        assert_eq!(
            store
                .modify(&path, || Ok(1), |_| false, true, |value| *value += 1)
                .unwrap(),
            2
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "2");
        std::fs::write(&path, "malformed").unwrap();
        let fresh = VersionedJsonStore::<u32>::new();
        assert!(fresh
            .load_with_healing(&path, || Ok(1), |_| false, true)
            .is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "malformed");
    }
}
