//! Cross-process advisory lock on `<dir>/<name>.lock`, shared by every
//! read-modify-write on host state that sibling processes also touch.

use std::fs;
use std::path::{Path, PathBuf};

/// In-process mutexes cover threads; this
/// lockfile covers sibling processes sharing a config dir — without it,
/// two processes could both read state N and each write N+1, forking the
/// term sequence or interleaving torn JSONL lines into the ledger.
/// Acquisition is `O_CREAT|O_EXCL` on `<name>.lock` inside `dir`; the
/// file records the holder pid for stale detection.
/// `pub` so sibling kernel modules (service_table) share the primitive —
/// every cross-process read-modify-write on `~/.susi` state needs it.
#[doc(hidden)]
pub struct FileLock {
    path: PathBuf,
}

impl FileLock {
    /// ~3s of 10ms retries — the guarded sections are millisecond-scale,
    /// so a longer wait means a wedged holder, not contention.
    pub fn acquire(dir: &Path, name: &str) -> Option<Self> {
        // A bare filename's parent is the empty path — normalize it to
        // the current directory so lock placement is well-defined.
        let dir = if dir.as_os_str().is_empty() {
            Path::new(".")
        } else {
            dir
        };
        // The guarded file may not exist yet — the lockfile lives in the
        // same directory, so it must be created before O_EXCL can succeed.
        fs::create_dir_all(dir).ok()?;
        let lock = dir.join(format!("{name}.lock"));
        for _ in 0..300 {
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock)
            {
                Ok(mut f) => {
                    use std::io::Write;
                    let _ = writeln!(f, "{}", std::process::id());
                    return Some(Self { path: lock });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if Self::is_stale(&lock) {
                        let _ = fs::remove_file(&lock);
                        continue;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(_) => return None,
            }
        }
        None
    }

    /// A lockfile is stale when its recorded pid no longer exists
    /// (Linux `/proc`), or it has sat unclaimed for over a minute —
    /// either means the holder died mid-claim.
    fn is_stale(lock: &Path) -> bool {
        if let Ok(body) = fs::read_to_string(lock) {
            if let Ok(pid) = body.trim().parse::<u32>() {
                #[cfg(target_os = "linux")]
                {
                    if !std::path::Path::new(&format!("/proc/{pid}")).exists() {
                        return true;
                    }
                }
            }
        }
        fs::metadata(lock)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .map(|e| e.as_secs() > 60)
            .unwrap_or(false)
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}
