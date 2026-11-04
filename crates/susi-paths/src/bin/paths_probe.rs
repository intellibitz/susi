//! Test probe for [`susi_paths::test_env::isolate_home`].
//!
//! Prints the directories [`susi_paths::SusiDirs`] resolves in this process,
//! one `key=path` line each, and the port offset it would run at.
//! `tests/isolate_home.rs` launches it through the helper and asserts every
//! dir sits under the home it was given: the child resolves its dirs exactly
//! as the `susi` binary does, from the env it was handed, so this is what a
//! test's `susi` child would read and write.
fn main() {
    use susi_paths::SusiDirs;
    for (key, dir) in [
        ("home_dir", SusiDirs::home_dir()),
        ("config_dir", SusiDirs::config_dir()),
        ("data_dir", SusiDirs::data_dir()),
        ("cache_dir", SusiDirs::cache_dir()),
        ("substrate_home", SusiDirs::substrate_home()),
    ] {
        println!("{key}={}", dir.display());
    }
    println!("port_offset={}", susi_paths::ports::env_port_offset());
}

#[cfg(test)]
mod tests {
    // Test-only assertions in inline unit tests.
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::sync::Mutex;
    use susi_paths::SusiDirs;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn susi_dirs_resolved_paths_are_non_empty() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        assert!(!SusiDirs::home_dir().as_os_str().is_empty());
        assert!(!SusiDirs::config_dir().as_os_str().is_empty());
        assert!(!SusiDirs::data_dir().as_os_str().is_empty());
        assert!(!SusiDirs::cache_dir().as_os_str().is_empty());
        assert!(!SusiDirs::substrate_home().as_os_str().is_empty());
    }

    #[test]
    fn env_port_offset_defaults_to_zero_under_isolated_env() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = susi_paths::test_env::EnvGuard::isolated();
        assert_eq!(susi_paths::ports::env_port_offset(), 0);
    }

    #[test]
    fn env_port_offset_parses_configured_offset_under_guard() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut guard = susi_paths::test_env::EnvGuard::isolated();
        guard.set("SUSI_PORT_OFFSET", "120");
        assert_eq!(susi_paths::ports::env_port_offset(), 120);
    }

    #[test]
    fn isolated_susi_home_redirects_config_data_and_substrate_dirs() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp =
            std::env::temp_dir().join(format!("susi_paths_probe_unit_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp);
        let mut guard = susi_paths::test_env::EnvGuard::isolated();
        guard.set("SUSI_HOME", &tmp);
        assert_eq!(SusiDirs::substrate_home(), tmp);
        assert_eq!(SusiDirs::config_dir(), tmp);
        assert_eq!(SusiDirs::data_dir(), tmp);
        drop(guard);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
