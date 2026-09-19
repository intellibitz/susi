use std::fs;
use std::io::Read;
use std::path::Path;

pub struct SusiDaemonState;

impl SusiDaemonState {
    pub fn calculate_binary_hash(bin_path: &Path) -> std::io::Result<String> {
        use sha2::{Digest, Sha256};
        let mut file = fs::File::open(bin_path)?;
        let mut hasher = Sha256::new();
        let mut buffer = [0u8; 65536];
        while let Ok(n) = file.read(&mut buffer) {
            if n == 0 {
                break;
            }
            hasher.update(&buffer[..n]);
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
        let _ = fs::write(&cache_path, format!("{}:{}:{}", mtime_ns, size, hash));
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
            let _ = fs::write(&hash_file, &current_sig);
            return Ok(false);
        }
        let _ = fs::write(&hash_file, &current_sig);
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
