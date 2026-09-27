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
        Ok(hex::encode(hasher.finalize()))
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

    pub fn check_status(workspace: &Path, global_dir: &Path) -> bool {
        let lock = global_dir.join(format!(
            "daemon_{}.lock",
            hex::encode(workspace.to_string_lossy().as_bytes())
        ));
        if !lock.exists() {
            return false;
        }
        if let Ok(pid_str) = fs::read_to_string(&lock) {
            if let Some(pid_line) = pid_str.lines().next() {
                if let Ok(pid) = pid_line.parse::<u32>() {
                    return Path::new(&format!("/proc/{}", pid)).exists();
                }
            }
        }
        false
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
            hex::encode(Sha256::digest(vec![7u8; 200_000]))
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
}
