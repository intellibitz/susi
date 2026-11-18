//! Test probe for [`susi_paths::test_env::isolate_home`].
//!
//! Spawned by `tests/isolate_home.rs` to verify child directory resolution and
//! `SUSI_PORT_OFFSET` scrubbing.

fn env_port_offset() -> u16 {
    std::env::var("SUSI_PORT_OFFSET")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0)
}

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
    println!("port_offset={}", env_port_offset());
}

#[cfg(test)]
mod tests {
    // Test-only assertions in inline unit tests.
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::sync::Mutex;
    use susi_paths::SusiDirs;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn env_port_offset_and_susi_dirs_resolve_under_isolated_env() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let instance = susi_paths::tempfile::tempdir().unwrap();
        let mut guard = susi_paths::test_env::EnvGuard::isolated();
        guard.set("SUSI_HOME", instance.path());
        assert_eq!(super::env_port_offset(), 0);
        assert_eq!(SusiDirs::substrate_home(), instance.path());
        assert_eq!(SusiDirs::config_dir(), instance.path());
        guard.set("SUSI_PORT_OFFSET", "120");
        assert_eq!(super::env_port_offset(), 120);
    }
}
