//! IPC-first wrapper over the local daemon-state implementation: each call
//! asks the `susi-sandbox` service first and falls back to [`local`] when it
//! is unreachable.

use std::path::Path;

pub mod local;

pub struct SusiDaemonState;

impl SusiDaemonState {
    pub fn calculate_binary_hash(bin_path: &Path) -> std::io::Result<String> {
        local::SusiDaemonState::calculate_binary_hash(bin_path)
    }

    pub fn calculate_binary_hash_cached(
        bin_path: &Path,
        global_dir: &Path,
    ) -> std::io::Result<String> {
        if let Some(hash) = super::service::hash_cached(bin_path, global_dir) {
            return Ok(hash);
        }
        local::SusiDaemonState::calculate_binary_hash_cached(bin_path, global_dir)
    }

    pub fn verify_binary_integrity(bin_path: &Path, global_dir: &Path) -> std::io::Result<bool> {
        if let Some(ok) = super::service::verify_integrity(bin_path, global_dir) {
            return Ok(ok);
        }
        local::SusiDaemonState::verify_binary_integrity(bin_path, global_dir)
    }

    pub fn check_status(workspace: &Path, global_dir: &Path) -> bool {
        if let Some(status) = super::service::check_status(workspace, global_dir) {
            return status;
        }
        local::SusiDaemonState::check_status(workspace, global_dir)
    }
}

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod inline_tests {
    use super::*;
    use std::fs;

    #[test]
    fn calculate_binary_hash_produces_deterministic_sha256_hex_for_file() {
        let tmp =
            std::env::temp_dir().join(format!("susi_daemon_state_hash_{}.bin", std::process::id()));
        fs::write(&tmp, b"susi-binary-payload").expect("write temp bin");
        let h1 = SusiDaemonState::calculate_binary_hash(&tmp).expect("hash 1");
        let h2 = SusiDaemonState::calculate_binary_hash(&tmp).expect("hash 2");
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64);
        let _ = fs::remove_file(&tmp);
    }

    #[test]
    fn calculate_binary_hash_returns_error_for_missing_file() {
        let missing = std::env::temp_dir().join("nonexistent_susi_binary_xyz_12345.bin");
        let _ = fs::remove_file(&missing);
        assert!(SusiDaemonState::calculate_binary_hash(&missing).is_err());
    }

    #[test]
    fn calculate_binary_hash_cached_computes_and_caches_digest_in_global_dir() {
        let root =
            std::env::temp_dir().join(format!("susi_daemon_state_cached_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create root");
        let bin = root.join("fake.exe");
        fs::write(&bin, b"cached-binary-bytes").expect("write bin");

        let direct = SusiDaemonState::calculate_binary_hash(&bin).expect("direct hash");
        let cached =
            SusiDaemonState::calculate_binary_hash_cached(&bin, &root).expect("cached hash");
        assert_eq!(direct, cached);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn verify_binary_integrity_and_check_status_handle_fresh_directories() {
        let root =
            std::env::temp_dir().join(format!("susi_daemon_state_verify_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create root");
        let bin = root.join("fake.exe");
        fs::write(&bin, b"integrity-bytes").expect("write bin");

        let ok = SusiDaemonState::verify_binary_integrity(&bin, &root).expect("verify integrity");
        assert!(ok);
        let _status = SusiDaemonState::check_status(&root, &root);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn verify_binary_integrity_returns_false_after_binary_modification_then_recovers() {
        let root =
            std::env::temp_dir().join(format!("susi_daemon_state_mutate_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create root");
        let bin = root.join("fake.exe");
        fs::write(&bin, b"v1-binary-bytes").expect("write v1");
        assert!(SusiDaemonState::verify_binary_integrity(&bin, &root).expect("initial verify"));

        fs::write(&bin, b"v2-modified-binary-bytes").expect("write v2");
        assert!(!SusiDaemonState::verify_binary_integrity(&bin, &root).expect("detect change"));
        assert!(SusiDaemonState::verify_binary_integrity(&bin, &root).expect("re-anchored verify"));
        let _ = fs::remove_dir_all(&root);
    }
}
