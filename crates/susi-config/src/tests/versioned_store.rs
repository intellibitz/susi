//! Tests for `versioned_store`.

use crate::versioned_store::*;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

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
    use crate::SusiConfig;
    // `gmcp_port()` resolves through the public port contract, which reads
    // SUSI_PORT_OFFSET from the process env; hold the env lock like every
    // other reader so a concurrent mutator cannot land between two calls.
    let _env = crate::env_test_lock();
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
    use crate::SusiConfig;
    // Readers of env-derived values hold env_test_lock(); another test in this
    // binary sets SUSI_PORT_OFFSET=42 inside that lock, and without it here the
    // assertion below saw 9090 + 42 (9132 != 9090) under load.
    let _env = crate::env_test_lock();
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
    // Public ports ignore polluted values; custom keys still round-trip.
    assert_eq!(
        SusiConfig::reload(&nested).unwrap().gmcp_port(),
        susi_paths::ports::GMCP
    );
    assert_eq!(
        SusiConfig::load(&nested).unwrap().gmcp_port(),
        susi_paths::ports::GMCP
    );
    assert_eq!(
        SusiConfig::load(&nested)
            .unwrap()
            .get::<serde_json::Value>("custom_setting")
            .unwrap()["enabled"],
        true
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), content);
    assert_eq!(
        std::fs::metadata(&path).unwrap().modified().unwrap(),
        modified
    );
}

#[test]
fn save_stays_visible_when_the_rewrite_keeps_its_mtime() {
    use crate::SusiConfig;
    let _env = crate::env_test_lock();
    let dir = TestDir::new();
    SusiConfig::default().save(&dir.0).unwrap();
    let path = SusiConfig::get_config_path(&dir.0);
    let before = SusiConfig::load(&dir.0).unwrap();
    let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
    assert_ne!(before.trust_level(), "paranoid");
    let mut cfg = before;
    cfg.settings
        .insert("trust_level".into(), serde_json::json!("paranoid"));
    cfg.save(&dir.0).unwrap();
    // A coarse clock can publish this rewrite with the mtime the cache
    // already stored. Put that stamp back so the assertion does not depend
    // on the host moving the timestamp by itself.
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(modified))
        .unwrap();
    assert_eq!(SusiConfig::load(&dir.0).unwrap().trust_level(), "paranoid");
}

#[test]
fn load_sees_a_same_tick_rewrite_when_its_length_changes() {
    let dir = TestDir::new();
    let path = dir.0.join("config.json");
    let store = VersionedJsonStore::<u32>::new();
    std::fs::write(&path, "1").unwrap();
    let first = store
        .load_with_healing(&path, || Ok(0), |_| false, true)
        .unwrap();
    assert_eq!(first, 1);
    let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
    std::fs::write(&path, "100").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(modified))
        .unwrap();
    let second = store
        .load_with_healing(&path, || Ok(0), |_| false, true)
        .unwrap();
    assert_eq!(second, 100);
}

#[test]
fn load_arc_shares_cache_hit_without_cloning_value() {
    let dir = TestDir::new();
    let path = dir.0.join("config.json");
    let store = VersionedJsonStore::<u32>::new();
    let first = store
        .load_arc_with_healing(&path, || Ok(7), |_| false, true)
        .unwrap();
    let second = store
        .load_arc_with_healing(&path, || Ok(7), |_| false, true)
        .unwrap();
    assert_eq!(*first, 7);
    assert!(Arc::ptr_eq(&first, &second));
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
                    crate::atomic_write_json_pretty(path, &vec![writer; 1000]).unwrap();
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
    assert_eq!(*store.cache.read().as_ref().unwrap().value, 1);
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
