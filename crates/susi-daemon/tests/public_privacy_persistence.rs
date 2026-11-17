#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
//! persist_privacy_mode must not rewrite a caller-owned routing preference
//! (board 2835). Isolated from the operator's real profile.
use std::sync::Mutex;
use susi_core::mac_policy::PrivacyMode;
use susi_paths::SusiDirs;
use susi_paths::test_env::EnvGuard;

static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn persisting_privacy_mode_preserves_the_routing_preference() {
    let _lock = match ENV_LOCK.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let home = std::env::temp_dir().join(format!("susi-privacy-persist-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    assert!(std::fs::create_dir_all(&home).is_ok());
    let mut env = EnvGuard::isolated();
    env.set("SUSI_HOME", &home)
        .set("HOME", &home)
        .set("USERPROFILE", &home)
        .set("APPDATA", home.join("AppData").join("Roaming"))
        .set("LOCALAPPDATA", home.join("AppData").join("Local"))
        .set("XDG_CONFIG_HOME", home.join(".config"))
        .set("XDG_DATA_HOME", home.join(".local").join("share"))
        .set("XDG_CACHE_HOME", home.join(".cache"));
    let pref = SusiDirs::config_dir().join("routing_preference.json");
    match pref.parent() {
        Some(parent) => assert!(std::fs::create_dir_all(parent).is_ok()),
        None => panic!("preference path has no parent"),
    }
    let initial = br#"{"test_preference":"keep"}"#;
    assert!(std::fs::write(&pref, initial).is_ok());
    assert!(susi_daemon::privacy::persist_privacy_mode(PrivacyMode::LocalOnly).is_ok());
    let mode_path = SusiDirs::substrate_home().join("privacy_mode");
    let mode = match std::fs::read_to_string(&mode_path) {
        Ok(text) => text,
        Err(err) => panic!("sticky privacy_mode: {err}"),
    };
    assert_eq!(mode.trim(), "local_only");
    let after = match std::fs::read_to_string(&pref) {
        Ok(text) => text,
        Err(err) => panic!("routing preference after persist: {err}"),
    };
    assert_eq!(after.as_bytes(), initial);
    drop(env);
    let _ = std::fs::remove_dir_all(&home);
}
