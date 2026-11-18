use std::fs;
use std::path::Path;

pub struct SusiDaemonState;

impl SusiDaemonState {
    /// SHA-256 of the whole file. A read error is returned, never folded
    /// into a digest of the bytes read so far: the old `while let Ok(n)`
    /// loop ended silently on an error (EINTR included), and the truncated
    /// digest was then cached and written as the `binary.hash` trust anchor.
    pub fn calculate_binary_hash(bin_path: &Path) -> std::io::Result<String> {
        use sha2::{Digest, Sha256};
        let mut file = fs::File::open(bin_path)?;
        let mut hasher = Sha256::new();
        let mut buffer = [0u8; 65536];
        loop {
            match std::io::Read::read(&mut file, &mut buffer) {
                Ok(0) => break,
                Ok(n) => hasher.update(buffer.get(..n).unwrap_or_default()),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(susi_paths::hex::encode(hasher.finalize()))
    }

    pub fn calculate_binary_hash_cached(
        bin_path: &Path,
        global_dir: &Path,
    ) -> std::io::Result<String> {
        let cache_path = global_dir.join("binary.hash.cache");
        let meta = fs::metadata(bin_path)?;
        let size = meta.len();
        let mtime_ns = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos())
            .unwrap_or(0);

        if let Ok(cached) = fs::read_to_string(&cache_path) {
            let mut parts = cached.trim().splitn(3, ':');
            if let (Some(c_mtime), Some(c_size), Some(c_hash)) =
                (parts.next(), parts.next(), parts.next())
            {
                if c_mtime.parse::<u128>().ok() == Some(mtime_ns)
                    && c_size.parse::<u64>().ok() == Some(size)
                    && !c_hash.is_empty()
                {
                    return Ok(c_hash.to_string());
                }
            }
        }

        let hash = Self::calculate_binary_hash(bin_path)?;
        // Best effort: a failed cache write only costs a re-hash next time.
        let _ = crate::susi_config::atomic_write_bytes(
            &cache_path,
            format!("{}:{}:{}", mtime_ns, size, hash).as_bytes(),
        );
        Ok(hash)
    }

    pub fn verify_binary_integrity(bin_path: &Path, global_dir: &Path) -> std::io::Result<bool> {
        let hash_file = global_dir.join("binary.hash");
        let current_sig = Self::calculate_binary_hash_cached(bin_path, global_dir)?;
        if hash_file.exists() {
            if let Ok(saved_sig) = fs::read_to_string(&hash_file) {
                if saved_sig.trim() == current_sig.trim() {
                    return Ok(true);
                }
            }
            crate::susi_config::atomic_write_bytes(&hash_file, current_sig.as_bytes())?;
            return Ok(false);
        }
        // The trust anchor is replaced whole: a torn write would make the
        // next check report a phantom recompile and restart the daemon.
        crate::susi_config::atomic_write_bytes(&hash_file, current_sig.as_bytes())?;
        Ok(true)
    }

    /// Whether a daemon holds the substrate lock in `global_dir`: the record
    /// the daemon itself writes (`substrate.lock`, mirrored to the unlocked
    /// `substrate.pid`) and the check `susi status` relies on
    /// ([`susi_paths::ports::daemon_lock_live`]). One daemon serves every
    /// workspace, so `workspace` does not name a file. Read-only: a stale
    /// record is reported dead, never removed here.
    ///
    /// This used to read `daemon_<hex of workspace path>.lock`, a per-workspace
    /// file nothing writes since the daemon took one global lock, so the answer
    /// was always "not running"; and for a workspace path over about 120 bytes
    /// the name passed the 255-byte file-name limit.
    pub fn check_status(_workspace: &Path, global_dir: &Path) -> bool {
        ["substrate.pid", "substrate.lock"]
            .iter()
            .find_map(|name| {
                fs::read_to_string(global_dir.join(name))
                    .ok()?
                    .lines()
                    .next()?
                    .trim()
                    .parse::<u32>()
                    .ok()
            })
            .is_some_and(|pid| susi_paths::ports::daemon_lock_live(global_dir, pid))
    }
}

#[cfg(test)]
mod tests {
    use super::SusiDaemonState;

    #[test]
    fn integrity_anchor_tracks_full_file_digest() {
        let dir = std::env::temp_dir().join(format!(
            "susi-daemon-state-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("susi");
        // Larger than one read chunk so the digest must span several reads.
        std::fs::write(&bin, vec![7u8; 200_000]).unwrap();
        let expected = {
            use sha2::{Digest, Sha256};
            susi_paths::hex::encode(Sha256::digest(vec![7u8; 200_000]))
        };
        assert_eq!(
            SusiDaemonState::calculate_binary_hash(&bin).unwrap(),
            expected
        );

        assert!(SusiDaemonState::verify_binary_integrity(&bin, &dir).unwrap());
        assert_eq!(
            std::fs::read_to_string(dir.join("binary.hash")).unwrap(),
            expected
        );
        std::fs::write(&bin, b"rebuilt").unwrap();
        assert!(!SusiDaemonState::verify_binary_integrity(&bin, &dir).unwrap());
        assert!(SusiDaemonState::verify_binary_integrity(&bin, &dir).unwrap());
        assert!(SusiDaemonState::calculate_binary_hash(&dir.join("missing")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_binary_integrity_caches_baseline_and_matches_subsequent_checks() {
        let dir = std::env::temp_dir().join(format!(
            "susi-daemon-baseline-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("susi-bin");
        std::fs::write(&bin, b"baseline-v1").unwrap();

        assert!(!dir.join("binary.hash").exists());
        assert!(SusiDaemonState::verify_binary_integrity(&bin, &dir).unwrap());
        assert!(dir.join("binary.hash").exists());
        assert!(SusiDaemonState::verify_binary_integrity(&bin, &dir).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_binary_integrity_detects_modified_binary_bytes_and_updates_anchor() {
        let dir = std::env::temp_dir().join(format!(
            "susi-daemon-tamper-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("susi-bin");
        std::fs::write(&bin, b"initial-payload").unwrap();
        assert!(SusiDaemonState::verify_binary_integrity(&bin, &dir).unwrap());

        std::fs::write(&bin, b"modified-payload-with-different-length").unwrap();
        assert!(!SusiDaemonState::verify_binary_integrity(&bin, &dir).unwrap());
        assert!(SusiDaemonState::verify_binary_integrity(&bin, &dir).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn calculate_binary_hash_cached_refreshes_cache_when_file_size_changes() {
        let dir = std::env::temp_dir().join(format!(
            "susi-daemon-cache-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("susi-bin");
        std::fs::write(&bin, b"short").unwrap();

        let h1 = SusiDaemonState::calculate_binary_hash_cached(&bin, &dir).unwrap();
        let h1_cached = SusiDaemonState::calculate_binary_hash_cached(&bin, &dir).unwrap();
        assert_eq!(h1, h1_cached);
        assert!(dir.join("binary.hash.cache").exists());

        std::fs::write(&bin, b"longer-replacement-binary").unwrap();
        let h2 = SusiDaemonState::calculate_binary_hash_cached(&bin, &dir).unwrap();
        assert_ne!(h1, h2);
        let cache_text = std::fs::read_to_string(dir.join("binary.hash.cache")).unwrap();
        assert!(cache_text.ends_with(&h2));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn status_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "susi-daemon-status-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn check_status_is_false_without_a_live_substrate_record() {
        let dir = status_dir("absent");
        let ws = dir.join("ws");
        assert!(!SusiDaemonState::check_status(&ws, &dir));

        std::fs::write(dir.join("substrate.pid"), "not-a-pid\n").unwrap();
        assert!(!SusiDaemonState::check_status(&ws, &dir));

        // A recorded pid no process has: the largest pid Linux can hand out
        // is 2^22, so this one names nothing; the record stays in place.
        std::fs::write(dir.join("substrate.pid"), "4294967294\n").unwrap();
        assert!(!SusiDaemonState::check_status(&ws, &dir));
        assert!(dir.join("substrate.pid").exists());

        // The workspace path names no file: a 300-byte one is answered, where
        // `daemon_<hex>.lock` exceeded the 255-byte file-name limit.
        let deep = dir.join("w".repeat(300));
        assert!(!SusiDaemonState::check_status(&deep, &dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The live case reads the daemon's own record: this test process stands
    /// in for the daemon. Linux judges the recorded pid by `/proc`; Windows
    /// needs the daemon's byte-range lock, which only the daemon holds.
    #[test]
    #[cfg(target_os = "linux")]
    fn check_status_sees_the_daemons_substrate_record() {
        let dir = status_dir("live");
        let ws = dir.join("w".repeat(300));
        std::fs::write(
            dir.join("substrate.lock"),
            format!("{}\n", std::process::id()),
        )
        .unwrap();
        assert!(SusiDaemonState::check_status(&ws, &dir));
        std::fs::write(
            dir.join("substrate.pid"),
            format!("{}\n", std::process::id()),
        )
        .unwrap();
        assert!(SusiDaemonState::check_status(&ws, &dir));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
