use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::RwLock;
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
/// How a load treats what it finds (or does not find) on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadPolicy {
    /// A malformed or unreadable file is an error (true) or falls back to
    /// `on_missing` (false).
    pub strict_parse: bool,
    /// A missing file is written with the healed `on_missing` value (true),
    /// or that value is returned in memory and nothing is written (false).
    pub seed_missing: bool,
}

impl ReadPolicy {
    /// Seed a missing file: the caller named the directory and owns it.
    #[must_use]
    pub const fn seeding(strict_parse: bool) -> Self {
        Self {
            strict_parse,
            seed_missing: true,
        }
    }

    /// Never write a missing file: for paths resolved through the process
    /// environment, which every thread of the process shares.
    #[must_use]
    pub const fn in_memory(strict_parse: bool) -> Self {
        Self {
            strict_parse,
            seed_missing: false,
        }
    }
}

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
        self.load_arc_with_healing_policy(
            path,
            on_missing,
            heal_fn,
            ReadPolicy::seeding(strict_parse),
        )
    }

    /// [`Self::load_arc_with_healing`] with an explicit [`ReadPolicy`]: with
    /// `seed_missing` false the `on_missing` value is returned (and healed) in
    /// memory and nothing is written. A read that resolves its path through
    /// the process environment must use that: in one process several threads
    /// share that environment, and a reader that seeds a file into a directory
    /// another thread owns overwrites what that thread just saved (the
    /// `susi-gemi-models` ladder test lost its config this way).
    pub fn load_arc_with_healing_policy<F, H>(
        &self,
        path: &Path,
        on_missing: F,
        heal_fn: H,
        policy: ReadPolicy,
    ) -> crate::susi_error::EaiResult<Arc<T>>
    where
        F: FnOnce() -> crate::susi_error::EaiResult<T>,
        H: FnOnce(&mut T) -> bool,
    {
        let (current_modified, current_len) = file_stamp(path);

        {
            let guard = self.cache.read().unwrap_or_else(|e| e.into_inner());
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

        let mut guard = self.cache.write().unwrap_or_else(|e| e.into_inner());

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

        let val = Arc::new(self.read_from_disk(path, on_missing, heal_fn, policy)?);

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
        let mut guard = self.cache.write().unwrap_or_else(|e| e.into_inner());
        // A failed reload must not leave a previously valid value cached.
        *guard = None;
        let value = Arc::new(self.read_from_disk(
            path,
            on_missing,
            heal_fn,
            ReadPolicy::seeding(strict_parse),
        )?);
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
        let mut guard = self.cache.write().unwrap_or_else(|e| e.into_inner());

        let (current_modified, current_len) = file_stamp(path);

        let mut val = if let Some(cached) = guard.as_ref() {
            if cached.path == path
                && cached.modified == current_modified
                && cached.len == current_len
                && current_modified != SystemTime::UNIX_EPOCH
            {
                (*cached.value).clone()
            } else {
                self.read_from_disk(path, on_missing, heal_fn, ReadPolicy::seeding(strict_parse))?
            }
        } else {
            self.read_from_disk(path, on_missing, heal_fn, ReadPolicy::seeding(strict_parse))?
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
        policy: ReadPolicy,
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
                        if policy.strict_parse {
                            return Err(crate::susi_error::EaiError::config(format!(
                                "Malformed configuration {}: {}",
                                path.display(),
                                e
                            )));
                        }
                    }
                },
                Err(e) => {
                    if policy.strict_parse {
                        return Err(crate::susi_error::EaiError::config(format!(
                            "Failed to read {}: {}",
                            path.display(),
                            e
                        )));
                    }
                }
            }
        }

        let (mut val, mut needs_save, missing) = match loaded_opt {
            Some(v) => (v, false, false),
            None => {
                let default_val = on_missing()?;
                (default_val, policy.seed_missing, true)
            }
        };

        // Healing a file that exists writes the backfilled result back; a
        // missing file is written only when the caller asked to seed it.
        if heal_fn(&mut val) && (!missing || policy.seed_missing) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn unique_temp_dir(label: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "susi-versioned-store-{label}-{}-{n}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn load_arc_with_healing_seeds_missing_file_shares_arc_on_cache_hit_and_invalidates_on_size_change(
    ) {
        let dir = unique_temp_dir("arc-hit");
        let path = dir.join("store.json");
        let store: VersionedJsonStore<BTreeMap<String, String>> = VersionedJsonStore::default();

        let first = store
            .load_arc_with_healing(
                &path,
                || {
                    let mut m = BTreeMap::new();
                    m.insert("seed".to_string(), "1".to_string());
                    Ok(m)
                },
                |m| {
                    m.insert("healed".to_string(), "yes".to_string());
                    true
                },
                true,
            )
            .expect("initial load_arc_with_healing");
        assert!(path.is_file());
        assert_eq!(first.get("seed").map(String::as_str), Some("1"));
        assert_eq!(first.get("healed").map(String::as_str), Some("yes"));

        let second = store
            .load_arc_with_healing(
                &path,
                || unreachable!("cache hit should not call on_missing"),
                |_| unreachable!("cache hit should not call heal_fn"),
                true,
            )
            .expect("cached load_arc_with_healing");
        assert!(Arc::ptr_eq(&first, &second));

        std::fs::write(
            &path,
            r#"{"seed":"updated-longer-value","healed":"yes","extra":"field"}"#,
        )
        .expect("rewrite store.json with different size");

        let third = store
            .load_arc_with_healing(&path, || Ok(BTreeMap::new()), |_| false, true)
            .expect("re-read after size change");
        assert!(!Arc::ptr_eq(&first, &third));
        assert_eq!(
            third.get("seed").map(String::as_str),
            Some("updated-longer-value")
        );
        assert_eq!(third.get("extra").map(String::as_str), Some("field"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn modify_and_reload_enforce_strict_parse_and_evict_cache_on_corrupt_json() {
        let dir = unique_temp_dir("modify-reload");
        let path = dir.join("config.json");
        let store: VersionedJsonStore<BTreeMap<String, u32>> = VersionedJsonStore::new();

        let updated = store
            .modify(
                &path,
                || {
                    let mut m = BTreeMap::new();
                    m.insert("count".to_string(), 10);
                    Ok(m)
                },
                |_| false,
                true,
                |m| {
                    m.insert("count".to_string(), 42);
                },
            )
            .expect("modify store");
        assert_eq!(updated.get("count"), Some(&42));
        assert!(store
            .cache
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .is_some());

        std::fs::write(&path, "{ invalid json ").expect("write corrupt json");

        let err = store
            .reload_with_healing(&path, || Ok(BTreeMap::new()), |_| false, true)
            .expect_err("strict reload must fail on corrupt JSON");
        assert!(err.to_string().contains("Malformed configuration"));
        assert!(
            store
                .cache
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .is_none(),
            "failed reload must evict previously cached snapshot"
        );

        let healed = store
            .reload_with_healing(
                &path,
                || {
                    let mut m = BTreeMap::new();
                    m.insert("recovered".to_string(), 1);
                    Ok(m)
                },
                |m| {
                    m.insert("healed_after_corrupt".to_string(), 2);
                    true
                },
                false,
            )
            .expect("non-strict reload heals from on_missing");
        assert_eq!(healed.get("recovered"), Some(&1));
        assert_eq!(healed.get("healed_after_corrupt"), Some(&2));

        let persisted: BTreeMap<String, u32> =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read healed file"))
                .expect("healed file on disk must be valid JSON");
        assert_eq!(persisted, healed);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_with_healing_skips_rewrite_when_unhealed_and_propagates_on_missing_error() {
        let dir = unique_temp_dir("no-rewrite-and-err");
        let path = dir.join("existing.json");
        let raw_compact = r#"{"alpha":1}"#;
        std::fs::write(&path, raw_compact).expect("write compact json");

        let store: VersionedJsonStore<BTreeMap<String, u32>> = VersionedJsonStore::new();
        let loaded = store
            .load_with_healing(
                &path,
                || unreachable!("existing file should not invoke on_missing"),
                |_| false,
                true,
            )
            .expect("load existing file without healing");
        assert_eq!(loaded.get("alpha"), Some(&1));
        assert_eq!(
            std::fs::read_to_string(&path).expect("read existing file"),
            raw_compact,
            "file must not be rewritten when heal_fn returns false"
        );

        let missing_path = dir.join("missing.json");
        let err = store
            .load_arc_with_healing(
                &missing_path,
                || Err(crate::susi_error::EaiError::config("on_missing failure")),
                |_| false,
                true,
            )
            .expect_err("on_missing error must propagate");
        assert!(err.to_string().contains("on_missing failure"));
        assert!(!missing_path.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_non_seeding_load_returns_the_healed_default_without_writing_the_missing_file() {
        let dir = unique_temp_dir("no-seed");
        let path = dir.join("store.json");
        let store: VersionedJsonStore<BTreeMap<String, String>> = VersionedJsonStore::default();
        let value = store
            .load_arc_with_healing_policy(
                &path,
                || {
                    let mut m = BTreeMap::new();
                    m.insert("seed".to_string(), "1".to_string());
                    Ok(m)
                },
                |m| {
                    m.insert("healed".to_string(), "yes".to_string());
                    true
                },
                ReadPolicy::in_memory(true),
            )
            .expect("non-seeding load");
        assert_eq!(value.get("seed").map(String::as_str), Some("1"));
        assert_eq!(value.get("healed").map(String::as_str), Some("yes"));
        assert!(
            !path.exists(),
            "a non-seeding read must not create the file"
        );
        // Once the file exists, healing drift still writes back.
        std::fs::write(&path, r#"{"seed":"2"}"#).expect("write store.json");
        let healed = store
            .load_arc_with_healing_policy(
                &path,
                || unreachable!("file exists"),
                |m| {
                    m.insert("healed".to_string(), "yes".to_string());
                    true
                },
                ReadPolicy::in_memory(true),
            )
            .expect("healing load");
        assert_eq!(healed.get("seed").map(String::as_str), Some("2"));
        assert!(std::fs::read_to_string(&path)
            .expect("read back")
            .contains("healed"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn modify_reuses_valid_cache_on_same_stamp_and_creates_missing_parent_directories() {
        let dir = unique_temp_dir("modify-cache-hit-and-parents");
        let path = dir.join("cached.json");
        let store: VersionedJsonStore<BTreeMap<String, u32>> = VersionedJsonStore::new();

        let seeded = store
            .load_arc_with_healing(
                &path,
                || {
                    let mut m = BTreeMap::new();
                    m.insert("initial".into(), 7);
                    Ok(m)
                },
                |_| false,
                true,
            )
            .expect("seed cache");
        assert_eq!(seeded.get("initial"), Some(&7));

        let mutated = store
            .modify(
                &path,
                || unreachable!("should use cached value"),
                |_| false,
                true,
                |m| {
                    m.insert("mutated".into(), 99);
                },
            )
            .expect("modify from valid cache");
        assert_eq!(mutated.get("initial"), Some(&7));
        assert_eq!(mutated.get("mutated"), Some(&99));

        let cached_after = store
            .load_arc_with_healing(
                &path,
                || unreachable!("should hit updated cache"),
                |_| false,
                true,
            )
            .expect("load updated cache");
        assert_eq!(*cached_after, mutated);

        let nested_path = dir.join("nested").join("deep").join("cfg.json");
        let nested_store: VersionedJsonStore<BTreeMap<String, u32>> = VersionedJsonStore::new();
        let created = nested_store
            .modify(
                &nested_path,
                || Ok(BTreeMap::new()),
                |_| false,
                true,
                |m| {
                    m.insert("deep_key".into(), 123);
                },
            )
            .expect("modify creates missing parent directories");
        assert_eq!(created.get("deep_key"), Some(&123));
        assert!(nested_path.is_file());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn default_versioned_json_store_materializes_fallback_when_file_is_absent() {
        let dir = unique_temp_dir("default-store-materialize");
        let absent = dir.join("absent.json");
        let store: VersionedJsonStore<BTreeMap<String, u32>> = VersionedJsonStore::default();
        let loaded = store
            .load_arc_with_healing(
                &absent,
                || {
                    let mut m = BTreeMap::new();
                    m.insert("k".into(), 5);
                    Ok(m)
                },
                |_| false,
                false,
            )
            .expect("load fallback");
        assert_eq!(loaded.get("k"), Some(&5));
        assert!(absent.is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
