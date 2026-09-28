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

    /// Mark the lock freshly held (mtime = now), so `is_stale`'s age rule
    /// does not treat a live, legitimately long holder as wedged.
    pub fn refresh(&self) {
        if let Ok(file) = fs::OpenOptions::new().write(true).open(&self.path) {
            let _ = file.set_modified(std::time::SystemTime::now());
        }
    }

    /// Run `work` while a heartbeat thread refreshes this lock every
    /// `HEARTBEAT` — for the rare holder whose critical section is seconds
    /// to minutes long (Tier-0 training), which the 60s age rule would
    /// otherwise break mid-section while the holder is still working.
    pub fn hold_while<T>(&self, work: impl FnOnce() -> T) -> T {
        const HEARTBEAT: std::time::Duration = std::time::Duration::from_secs(20);
        const TICK: std::time::Duration = std::time::Duration::from_millis(100);
        let done = std::sync::atomic::AtomicBool::new(false);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                let mut since = std::time::Instant::now();
                while !done.load(std::sync::atomic::Ordering::Acquire) {
                    std::thread::sleep(TICK);
                    if since.elapsed() >= HEARTBEAT {
                        self.refresh();
                        since = std::time::Instant::now();
                    }
                }
            });
            let result = work();
            done.store(true, std::sync::atomic::Ordering::Release);
            result
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Unique scratch dir (removed on drop) without a tempfile dependency.
    struct Scratch(PathBuf);
    impl Scratch {
        fn new(tag: &str) -> Self {
            static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir = std::env::temp_dir()
                .join(format!("susi-file-lock-{tag}-{}-{n}", std::process::id()));
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn age(lock: &Path, secs: u64) {
        let file = fs::OpenOptions::new().write(true).open(lock).unwrap();
        file.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(secs))
            .unwrap();
    }

    #[test]
    fn refresh_keeps_a_long_live_holder_from_going_stale() {
        let dir = Scratch::new("long");
        let held = FileLock::acquire(dir.path(), "long").unwrap();
        let lock = dir.path().join("long.lock");
        age(&lock, 120);
        assert!(
            FileLock::is_stale(&lock),
            "a 2-minute-old lock reads as wedged"
        );
        held.refresh();
        assert!(!FileLock::is_stale(&lock), "refresh marks it freshly held");
    }

    #[test]
    fn hold_while_returns_the_work_result_and_keeps_the_lock() {
        let dir = Scratch::new("work");
        let held = FileLock::acquire(dir.path(), "work").unwrap();
        let out = held.hold_while(|| {
            assert!(
                FileLock::acquire(dir.path(), "work").is_none(),
                "still exclusive"
            );
            7
        });
        assert_eq!(out, 7);
        drop(held);
        assert!(FileLock::acquire(dir.path(), "work").is_some());
    }
}
