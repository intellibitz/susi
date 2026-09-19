//! Serializes loads of each canonical file without blocking unrelated models.

use candle_core::Device;
use parking_lot::{Mutex, RwLock};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;
use susi_error::{EaiError, EaiResult};

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
    value: Arc<RwLock<T>>,
}

type Slot<T> = Arc<Mutex<Option<Entry<T>>>>;

pub(crate) struct ModelCache<T> {
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
    pub(crate) fn get_or_load(
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
