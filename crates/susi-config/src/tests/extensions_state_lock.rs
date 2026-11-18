//! Every read-modify-write of `extensions/state.json` holds [`StateLock`]:
//! two writers whose read, derive and write steps interleave cannot drop
//! each other's change.

use super::*;

/// Isolated config root (as `extensions_tests::with_temp_home`) with an
/// extensions root holding the packs `ids`.
fn with_packs<F: FnOnce()>(ids: &[&str], f: F) {
    let _guard = crate::env_test_lock();
    let tmp = std::env::temp_dir().join(format!(
        "susi_ext_lock_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(tmp.join(".susi")).unwrap();
    let mut env = susi_paths::test_env::EnvGuard::isolated();
    env.set("HOME", &tmp);
    env.set("XDG_CONFIG_HOME", tmp.join("config"));
    env.set("APPDATA", tmp.join("appdata"));
    env.set("LOCALAPPDATA", tmp.join("localappdata"));
    env.set("SUSI_XDG", "0");
    env.remove("SUSI_EXTENSION_PACK");
    invalidate_extension_caches();
    for id in ids {
        let root = extensions_root().join(id);
        private_dir(&root).unwrap();
        write_private_file(
            &root.join("manifest.json"),
            &format!(
                r#"{{"id":"{id}","name":"{id}","version":"1.0.0","apiVersion":"1","files":{{}}}}"#
            ),
        )
        .unwrap();
    }
    f();
    drop(env);
    invalidate_extension_caches();
    let _ = std::fs::remove_dir_all(tmp);
}

/// What `load_pack` changes: the pack goes active with pinned digests.
fn apply_load(state: &mut ExtensionsState, id: &str) {
    state.active = id.to_string();
    if !state.loaded.iter().any(|x| x == id) {
        state.loaded.push(id.to_string());
    }
    state.trusted_digests.insert(
        id.to_string(),
        BTreeMap::from([("manifest.json".to_string(), format!("sha256:{id}"))]),
    );
}

fn loaded_and_pinned(state: &ExtensionsState, id: &str) -> bool {
    state.loaded.iter().any(|x| x == id) && state.trusted_digests.contains_key(id)
}

/// The hazard, without the lock: B reads before A writes and writes after,
/// so A's load is silently gone. This is what the lock must prevent.
#[test]
fn unlocked_interleaved_writers_drop_a_change() {
    with_packs(&["acme"], || {
        write_state(&ExtensionsState::default()).unwrap();
        let mut a = read_state().unwrap();
        let mut b = read_state().unwrap();
        apply_load(&mut a, "acme");
        write_state(&a).unwrap();
        b.extra
            .insert("reader".to_string(), serde_json::Value::Bool(true));
        write_state(&b).unwrap();
        let after = read_state().unwrap();
        assert_eq!(after.active, DEFAULT_PACK_ID, "the unlocked write won");
        assert!(!after.trusted_digests.contains_key("acme"));
    });
}

/// Wait until a contending thread has announced `.state.lock.wait`, then hold
/// briefly so any broken lock path has a window to write out of turn.
fn wait_for_contender_then_hold() {
    let wait_marker = extensions_root().join(".state.lock.wait");
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(250);
    while !wait_marker.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    std::thread::sleep(std::time::Duration::from_millis(20));
}

/// Two threads each take the lock, read, derive and write, with B's attempt
/// started while A holds the lock between its read and its write. B waits,
/// re-reads A's result and keeps it: both changes survive.
#[test]
fn locked_interleaved_writers_keep_both_changes() {
    use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
    with_packs(&["acme", "zeta"], || {
        write_state(&ExtensionsState::default()).unwrap();
        let a_has_read = std::sync::Barrier::new(2);
        let b_wrote_early = AtomicBool::new(false);
        let a_done = AtomicBool::new(false);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                let lock = StateLock::acquire().unwrap();
                let mut state = read_state().unwrap();
                a_has_read.wait();
                // B is now trying to take the lock; wait until it contends.
                wait_for_contender_then_hold();
                apply_load(&mut state, "acme");
                write_state(&state).unwrap();
                a_done.store(true, SeqCst);
                drop(lock);
            });
            scope.spawn(|| {
                a_has_read.wait();
                let lock = StateLock::acquire().unwrap();
                if !a_done.load(SeqCst) {
                    b_wrote_early.store(true, SeqCst);
                }
                let mut state = read_state().unwrap();
                if !state.loaded.iter().any(|x| x == "zeta") {
                    state.loaded.push("zeta".to_string());
                }
                write_state(&state).unwrap();
                drop(lock);
            });
        });
        assert!(
            !b_wrote_early.load(SeqCst),
            "B entered while A held the lock"
        );
        let after = read_state().unwrap();
        assert_eq!(after.active, "acme", "{after:?}");
        assert!(loaded_and_pinned(&after, "acme"), "{after:?}");
        assert!(after.loaded.iter().any(|x| x == "zeta"), "{after:?}");
        assert!(!extensions_root().join(".state.lock").exists());
    });
}

/// The real reader path: a catalog read whose snapshot needs settling (a
/// newly installed pack) races a load that holds the lock between its read
/// and its write. The reader writes nothing while the load holds the lock,
/// then settles the load's state rather than its own stale snapshot.
#[test]
fn a_settling_reader_never_undoes_a_concurrent_load() {
    use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
    with_packs(&["acme", "zeta"], || {
        // `zeta` was installed after the state was last settled.
        let mut stale = ExtensionsState::default();
        stale.loaded.push("acme".to_string());
        write_state(&stale).unwrap();
        let before = std::fs::read(state_path()).unwrap();
        let load_has_read = std::sync::Barrier::new(2);
        let load_done = AtomicBool::new(false);
        let reader_saw = std::thread::scope(|scope| {
            scope.spawn(|| {
                let lock = StateLock::acquire().unwrap();
                let mut state = read_state().unwrap();
                load_has_read.wait();
                wait_for_contender_then_hold();
                assert_eq!(
                    std::fs::read(state_path()).unwrap(),
                    before,
                    "the reader wrote while the load held the lock"
                );
                apply_load(&mut state, "acme");
                write_state(&state).unwrap();
                load_done.store(true, SeqCst);
                drop(lock);
            });
            let reader = scope.spawn(|| {
                load_has_read.wait();
                let decided = decide_extensions_substrate().unwrap().0;
                (decided, load_done.load(SeqCst))
            });
            reader.join().unwrap()
        });
        let (decided, after_load) = reader_saw;
        assert!(after_load, "the reader settled before the load finished");
        assert_eq!(decided.id, "acme", "the reader answered the stale state");
        let after = read_state().unwrap();
        assert_eq!(after.active, "acme", "{after:?}");
        assert!(loaded_and_pinned(&after, "acme"), "{after:?}");
        assert!(after.loaded.iter().any(|x| x == "zeta"), "{after:?}");
    });
}

/// A settled state is read without the lock and without a write: a reader
/// whose derivation matches the file leaves it untouched even while another
/// process holds the lock.
#[test]
fn a_settled_reader_neither_writes_nor_waits_for_the_lock() {
    with_packs(&["acme"], || {
        ensure_extensions_substrate().unwrap();
        let before = std::fs::read(state_path()).unwrap();
        let held = StateLock::acquire().unwrap();
        invalidate_extension_caches();
        let started = std::time::Instant::now();
        let pack = ensure_extensions_substrate().unwrap();
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "a settled reader waited for the lock"
        );
        assert_eq!(pack.id, DEFAULT_PACK_ID);
        assert_eq!(std::fs::read(state_path()).unwrap(), before);
        drop(held);
    });
}

/// Lifecycle commands take the lock too: `unload` and `load` racing on two
/// threads both land, in some order, and neither is half-applied.
#[test]
fn concurrent_lifecycle_commands_both_land() {
    with_packs(&["acme", "zeta"], || {
        load_pack("zeta").unwrap();
        std::thread::scope(|scope| {
            scope.spawn(|| load_pack("acme").unwrap());
            scope.spawn(|| unload_pack("zeta").unwrap());
        });
        let after = read_state().unwrap();
        assert_eq!(after.active, "acme", "{after:?}");
        assert!(loaded_and_pinned(&after, "acme"), "{after:?}");
        assert!(after.unloaded.iter().any(|x| x == "zeta"), "{after:?}");
        assert!(!after.loaded.iter().any(|x| x == "zeta"), "{after:?}");
        assert!(after.trusted_digests.contains_key("zeta"), "{after:?}");
    });
}

/// The lock lives outside the pack namespace: a pack id cannot take its
/// name, and only its three exact file names are left out of the stamp.
#[test]
fn the_state_lock_names_are_exact_and_no_pack_id() {
    for name in [".state.lock", ".state.lock.wait", ".state.lock.break"] {
        assert!(is_state_lock_file(name, false), "{name}");
        assert!(!is_state_lock_file(name, true), "{name} as a directory");
        assert!(validate_pack_id(name).is_err(), "{name} is a pack id");
    }
    for name in [
        "state.json",
        "statements",
        "state.lock",
        "state.lockbox",
        ".state.lockx",
        ".state.lock.waiting",
    ] {
        assert!(!is_state_lock_file(name, false), "{name}");
    }
}

/// Lock files coming and going do not change the substrate stamp; a pack
/// directory does, whatever its name looks like.
#[test]
fn the_substrate_stamp_ignores_only_the_state_lock_files() {
    with_packs(&["acme"], || {
        let later = || std::time::SystemTime::now() + std::time::Duration::from_secs(60);
        let before = substrate_stamp(later()).unwrap();
        let held = StateLock::acquire().unwrap();
        assert!(extensions_root().join(".state.lock").is_file());
        std::fs::write(extensions_root().join(".state.lock.wait"), "").unwrap();
        assert_eq!(substrate_stamp(later()).unwrap(), before);
        let _ = std::fs::remove_file(extensions_root().join(".state.lock.wait"));
        drop(held);
        for id in ["state.lockbox", "state.lock"] {
            validate_pack_id(id).unwrap();
            let stamp = substrate_stamp(later()).unwrap();
            private_dir(&extensions_root().join(id)).unwrap();
            assert_ne!(
                substrate_stamp(later()).unwrap(),
                stamp,
                "pack dir {id} left out of the stamp"
            );
        }
    });
}

/// A standing quarantine is the hot path of every catalog read: it is
/// already recorded, so the read neither writes nor waits for the lock.
#[test]
fn a_standing_quarantine_read_neither_locks_nor_writes() {
    with_packs(&[], || {
        let root = extensions_root().join("acme");
        private_dir(&root).unwrap();
        write_private_file(
            &root.join("manifest.json"),
            r#"{"id":"acme","name":"Acme","version":"1","files":{"leading-mcp.json":"leading-mcp.json"}}"#,
        )
        .unwrap();
        write_private_file(&root.join("leading-mcp.json"), "[2]").unwrap();
        load_pack("acme").unwrap();
        std::fs::write(root.join("leading-mcp.json"), "[]").unwrap();
        // The first read quarantines; the second records the standing reason.
        assert!(pack_file("leading-mcp.json").is_none());
        assert!(pack_file("leading-mcp.json").is_none());
        let before = std::fs::read(state_path()).unwrap();
        assert!(read_state().unwrap().quarantined.contains_key("acme"));
        let held = StateLock::acquire().unwrap();
        let started = std::time::Instant::now();
        assert!(pack_file("leading-mcp.json").is_none());
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "a standing quarantine read waited for the lock"
        );
        assert_eq!(std::fs::read(state_path()).unwrap(), before);
        drop(held);
    });
}
