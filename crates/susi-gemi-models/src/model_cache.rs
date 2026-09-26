//! Serializes loads of each canonical file without blocking unrelated models.

use crate::susi_error::{EaiError, EaiResult};
use candle_core::Device;
use parking_lot::{Mutex, RwLock};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

#[derive(Debug, PartialEq, Eq)]
struct FileVersion {
    len: u64,
    modified: SystemTime,
}

impl FileVersion {
    fn read(path: &Path) -> EaiResult<Self> {
        let metadata = path.metadata()?;
        if !metadata.is_file() {
            return Err(EaiError::inference(format!(
                "Not a model file: {}",
                path.display()
            )));
        }
        Ok(Self {
            len: metadata.len(),
            modified: metadata.modified()?,
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
struct ModelVersion {
    weights: FileVersion,
    provenance: Option<FileVersion>,
}

impl ModelVersion {
    fn read(path: &Path) -> EaiResult<Self> {
        let provenance_path = path.with_extension("provenance.json");
        let provenance = match provenance_path.try_exists()? {
            true => Some(FileVersion::read(&provenance_path)?),
            false => None,
        };
        Ok(Self {
            weights: FileVersion::read(path)?,
            provenance,
        })
    }
}

struct Entry<T> {
    version: ModelVersion,
    device: Device,
    kv_capacity: usize,
    loaded_at: SystemTime,
    value: Arc<RwLock<T>>,
}

type Slot<T> = Arc<Mutex<Option<Entry<T>>>>;

/// Weights removed by [`ModelCache::evict`] and the device they live on.
pub type Evicted<T> = (Arc<RwLock<T>>, Device);

pub struct ModelCache<T> {
    slots: Mutex<HashMap<PathBuf, Slot<T>>>,
}

impl<T> Default for ModelCache<T> {
    fn default() -> Self {
        Self {
            slots: Mutex::new(HashMap::new()),
        }
    }
}

impl<T> ModelCache<T> {
    /// Point-in-time view of every slot. Uses `try_lock`/`try_read` only:
    /// a slot mid-load reports `loading`, a model mid-generation (write
    /// lock held) reports `busy` without its backend detail — observing
    /// the cache must never stall inference.
    pub fn snapshot(&self, describe: impl Fn(&T) -> serde_json::Value) -> Vec<serde_json::Value> {
        let slots: Vec<(PathBuf, Slot<T>)> = self
            .slots
            .lock()
            .iter()
            .map(|(path, slot)| (path.clone(), Arc::clone(slot)))
            .collect();
        let mut out: Vec<serde_json::Value> = slots
            .into_iter()
            .filter_map(|(path, slot)| {
                let Some(guard) = slot.try_lock() else {
                    return Some(serde_json::json!({ "path": path, "state": "loading" }));
                };
                let entry = guard.as_ref()?;
                let detail = entry.value.try_read().map(|value| describe(&value));
                Some(serde_json::json!({
                    "path": path,
                    "state": if detail.is_some() { "loaded" } else { "busy" },
                    "device": format!("{:?}", entry.device.location()),
                    "kv_capacity_tokens": entry.kv_capacity,
                    "bytes": entry.version.weights.len,
                    "loaded_at": entry
                        .loaded_at
                        .duration_since(SystemTime::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0),
                    // The cache holds one reference; the rest are requests
                    // currently using these weights.
                    "in_flight_users": Arc::strong_count(&entry.value).saturating_sub(1),
                    "backend": detail,
                }))
            })
            .collect();
        out.sort_by(|a, b| a["path"].to_string().cmp(&b["path"].to_string()));
        out
    }

    /// Remove the cached weights for `path` and hand them to the caller, who
    /// decides where the final drop happens: GPU memory must be freed on a
    /// thread with the device's context bound (cudarc's `CudaSlice` drop
    /// does not bind it and silently records `CUDA_ERROR_INVALID_CONTEXT`,
    /// leaking the allocation). Other holders of the returned `Arc` are
    /// in-flight requests. `Ok(None)` = not loaded. A slot mid-load is refused rather
    /// than waited on. The slot itself stays in the map so a concurrent
    /// `get_or_load` never loads into an orphaned slot.
    pub fn evict(&self, path: &Path) -> EaiResult<Option<Evicted<T>>> {
        let path = path.canonicalize()?;
        let Some(slot) = self.slots.lock().get(&path).cloned() else {
            return Ok(None);
        };
        let Some(mut entry) = slot.try_lock() else {
            return Err(EaiError::inference(
                "model is still loading; retry after the load finishes",
            ));
        };
        Ok(entry.take().map(|evicted| (evicted.value, evicted.device)))
    }

    pub fn get_or_load(
        &self,
        path: &Path,
        device: &Device,
        kv_capacity: usize,
        load: impl FnOnce(&Path) -> EaiResult<T>,
    ) -> EaiResult<Arc<RwLock<T>>> {
        let path = path.canonicalize()?;
        let slot = self.slots.lock().entry(path.clone()).or_default().clone();
        let mut entry = slot.lock();
        let version = ModelVersion::read(&path)?;
        if let Some(cached) = entry.as_ref() {
            if cached.version == version
                && cached.device.same_device(device)
                && cached.kv_capacity == kv_capacity
            {
                return Ok(Arc::clone(&cached.value));
            }
        }
        // Release obsolete weights before allocating their replacement. Active
        // inference requests retain their own Arc and may finish using them.
        *entry = None;
        let value = load(&path)?;
        if ModelVersion::read(&path)? != version {
            return Err(EaiError::inference(
                "Model files changed during loading; retry the request",
            ));
        }
        let value = Arc::new(RwLock::new(value));
        *entry = Some(Entry {
            version,
            device: device.clone(),
            kv_capacity,
            loaded_at: SystemTime::now(),
            value: Arc::clone(&value),
        });
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let dir = std::env::temp_dir().join(format!(
                "susi-model-cache-{}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&dir).unwrap();
            let path = dir.join("weights.gguf");
            std::fs::write(&path, "weights").unwrap();
            Self(path)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(self.0.parent().unwrap());
        }
    }

    #[test]
    fn concurrent_requests_load_once_and_share_weights() {
        let file = Fixture::new();
        let cache = ModelCache::default();
        let loads = AtomicUsize::new(0);
        let barrier = std::sync::Barrier::new(8);
        std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        barrier.wait();
                        cache
                            .get_or_load(&file.0, &Device::Cpu, 32, |_| {
                                loads.fetch_add(1, Ordering::Relaxed);
                                Ok(42)
                            })
                            .unwrap()
                    })
                })
                .collect();
            let values: Vec<_> = handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect();
            assert!(values.iter().all(|value| Arc::ptr_eq(value, &values[0])));
        });
        assert_eq!(loads.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn changes_to_weights_provenance_and_capacity_reload() {
        let file = Fixture::new();
        let cache = ModelCache::default();
        let first = cache
            .get_or_load(&file.0, &Device::Cpu, 32, |_| Ok(1))
            .unwrap();
        let alias = file.0.parent().unwrap().join(".").join("weights.gguf");
        let reused = cache
            .get_or_load(&alias, &Device::Cpu, 32, |_| panic!("cache miss"))
            .unwrap();
        assert!(Arc::ptr_eq(&first, &reused));
        std::fs::write(&file.0, "changed weights").unwrap();
        let second = cache
            .get_or_load(&file.0, &Device::Cpu, 32, |_| Ok(2))
            .unwrap();
        assert_eq!(*first.read(), 1);
        assert_eq!(*second.read(), 2);
        std::fs::write(file.0.with_extension("provenance.json"), "{}").unwrap();
        assert_eq!(
            *cache
                .get_or_load(&file.0, &Device::Cpu, 32, |_| Ok(3))
                .unwrap()
                .read(),
            3
        );
        assert_eq!(
            *cache
                .get_or_load(&file.0, &Device::Cpu, 64, |_| Ok(4))
                .unwrap()
                .read(),
            4
        );
    }

    #[test]
    fn snapshot_reports_loaded_busy_and_failed_slots() {
        let file = Fixture::new();
        let cache = ModelCache::<u32>::default();
        assert!(cache.snapshot(|_| serde_json::json!({})).is_empty());
        let value = cache
            .get_or_load(&file.0, &Device::Cpu, 32, |_| Ok(7))
            .unwrap();
        let snap = cache.snapshot(|v| serde_json::json!({ "value": v }));
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0]["state"], "loaded");
        assert_eq!(snap[0]["backend"]["value"], 7);
        assert_eq!(snap[0]["kv_capacity_tokens"], 32);
        assert_eq!(snap[0]["in_flight_users"], 1);
        {
            let _generating = value.write();
            let busy = cache.snapshot(|_| serde_json::json!({}));
            assert_eq!(busy[0]["state"], "busy");
            assert!(busy[0]["backend"].is_null());
        }
        drop(value);
        let other = Fixture::new();
        assert!(cache
            .get_or_load(&other.0, &Device::Cpu, 32, |_| Err(EaiError::inference(
                "no"
            )))
            .is_err());
        // A failed load leaves an empty slot, which is not reported.
        assert_eq!(cache.snapshot(|_| serde_json::json!({})).len(), 1);
    }

    #[test]
    fn evict_hands_back_weights_and_frees_the_slot() {
        let file = Fixture::new();
        let cache = ModelCache::<u32>::default();
        assert!(cache.evict(&file.0).unwrap().is_none());
        let held = cache
            .get_or_load(&file.0, &Device::Cpu, 32, |_| Ok(1))
            .unwrap();
        let (weights, device) = cache.evict(&file.0).unwrap().unwrap();
        // The evicted handle plus the in-flight holder.
        assert_eq!(Arc::strong_count(&weights), 2);
        assert!(device.same_device(&Device::Cpu));
        drop(weights);
        assert!(cache.snapshot(|_| serde_json::json!({})).is_empty());
        // The in-flight holder keeps working on its own handle.
        assert_eq!(*held.read(), 1);
        assert!(cache.evict(&file.0).unwrap().is_none());
        let reloaded = cache
            .get_or_load(&file.0, &Device::Cpu, 32, |_| Ok(2))
            .unwrap();
        assert_eq!(*reloaded.read(), 2);
    }

    #[test]
    fn failures_and_files_changed_during_loading_are_not_cached() {
        let file = Fixture::new();
        let cache = ModelCache::<u32>::default();
        assert!(cache
            .get_or_load(&file.0, &Device::Cpu, 32, |_| Err(EaiError::inference(
                "test load failure"
            )))
            .is_err());
        assert!(cache
            .get_or_load(&file.0, &Device::Cpu, 32, |path| {
                std::fs::write(path, "replacement weights").unwrap();
                Ok(1)
            })
            .is_err());
        assert_eq!(
            *cache
                .get_or_load(&file.0, &Device::Cpu, 32, |_| Ok(2))
                .unwrap()
                .read(),
            2
        );
    }
}
