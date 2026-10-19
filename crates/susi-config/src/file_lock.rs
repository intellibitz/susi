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
    /// The exclusively-held handle on Windows — sharing mode zero makes the
    /// *open* the lock, so the kernel itself releases it on process death.
    #[cfg(windows)]
    _hold: fs::File,
}

impl FileLock {
    /// ~3s of 10ms retries — the guarded sections are millisecond-scale,
    /// so a longer wait means a wedged holder, not contention.
    pub fn acquire(dir: &Path, name: &str) -> Option<Self> {
        Self::acquire_within(dir, name, std::time::Duration::from_secs(3))
    }

    /// `acquire`, retrying for up to `timeout` — for a lock whose holders
    /// legitimately work for seconds (Tier-0 training), where a waiter should
    /// take its turn rather than fail after the default ~3 s.
    pub fn acquire_within(dir: &Path, name: &str, timeout: std::time::Duration) -> Option<Self> {
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
        // Wall-time deadline, not attempt count: on unix the stale check
        // can cost hundreds of ms per probe, and looping by attempts once
        // ran 60s — long enough for the held lockfile's own mtime to trip
        // the 60s staleness rule and be stolen.
        let deadline = std::time::Instant::now() + timeout;

        // Windows: an exclusive-share open IS the lock — the kernel denies
        // the next open while a holder lives and releases it the moment the
        // holder's handle closes, even on a crash, so there is no lockfile
        // create/delete churn and no pid bookkeeping to go stale. Contenders
        // see PermissionDenied (a held share, or a transient scanner) and
        // simply retry within the same wall-time budget.
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            let mut attempt = 0u64;
            loop {
                match fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .truncate(false)
                    .share_mode(0)
                    .open(&lock)
                {
                    Ok(mut f) => {
                        use std::io::Write;
                        let _ = writeln!(f, "{}", std::process::id());
                        return Some(Self {
                            path: lock,
                            _hold: f,
                        });
                    }
                    // ERROR_SHARING_VIOLATION (a live holder) surfaces as
                    // Uncategorized, not PermissionDenied — and a transient
                    // scanner is PermissionDenied. Both are contention.
                    Err(e)
                        if e.kind() == std::io::ErrorKind::PermissionDenied
                            || e.raw_os_error() == Some(32) =>
                    {
                        std::thread::sleep(Self::contention_pause(&mut attempt));
                    }
                    Err(_) => return None,
                }
                if std::time::Instant::now() >= deadline {
                    return None;
                }
            }
        }

        // Unix: advisory create_new on the lockfile; the file records the
        // holder pid so a crashed locker's file is reclaimed (is_stale) —
        // the OS has no auto-release for an existence-based lock.
        // The pid probe is throttled to once per half-second of sustained
        // contention: a live holder's millisecond-scale section yields long
        // before the deadline and probing is not free everywhere.
        #[cfg(not(windows))]
        {
            let mut last_probe: Option<std::time::Instant> = None;
            let mut attempt = 0u64;
            loop {
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
                        let probe = match last_probe {
                            Some(t) if t.elapsed() < std::time::Duration::from_millis(500) => false,
                            _ => {
                                last_probe = Some(std::time::Instant::now());
                                Self::is_stale(&lock)
                            }
                        };
                        if probe {
                            let _ = fs::remove_file(&lock);
                        } else {
                            std::thread::sleep(Self::contention_pause(&mut attempt));
                        }
                    }
                    Err(_) => return None,
                }
                if std::time::Instant::now() >= deadline {
                    return None;
                }
            }
        }
    }

    /// Back off on contention with per-process jitter: `thread::sleep`
    /// quantizes to the ~15.6ms timer tick on Windows, so flat polls wake
    /// every contender on the same boundary and the same thread wins the
    /// handoff again and again until a peer's deadline lapses.
    fn contention_pause(attempt: &mut u64) -> std::time::Duration {
        *attempt = attempt.wrapping_add(1);
        let jitter = (std::process::id() as u64).wrapping_add(*attempt) % 11;
        std::time::Duration::from_millis(8 + jitter)
    }

    /// Mark the lock freshly held (mtime = now), so `is_stale`'s age rule
    /// does not treat a live, legitimately long holder as wedged.
    pub fn refresh(&self) {
        // The exclusive-share handle is the only accessor on Windows — the
        // file denies even its holder a second open while it is held.
        #[cfg(windows)]
        {
            let _ = self._hold.set_modified(std::time::SystemTime::now());
        }
        #[cfg(not(windows))]
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
    /// (`susi_paths::process::pid_alive` — `/proc`, `kill -0`, or
    /// `tasklist` per OS), or it has sat unclaimed for over a minute —
    /// either means the holder died mid-claim. Unix acquire relies on this;
    /// Windows never consults it (the kernel enforces the share-mode lock)
    /// but tests exercise it on every platform.
    #[cfg(any(test, not(windows)))]
    fn is_stale(lock: &Path) -> bool {
        if let Ok(body) = fs::read_to_string(lock) {
            if let Ok(pid) = body.trim().parse::<u32>() {
                if !susi_paths::process::pid_alive(pid) {
                    return true;
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
        // Unix removes the name so the next O_EXCL create wins. On Windows
        // the still-open exclusive-share handle denies the delete's own open,
        // so the file persists — which is correct there: the persistent file
        // is the lock object and the kernel releases the share on close.
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

    /// Backdate the lockfile's mtime — through the holder's own handle on
    /// Windows, where the exclusive-share open denies even the holder a
    /// second path open.
    fn age(held: &FileLock, secs: u64) {
        let when = std::time::SystemTime::now() - std::time::Duration::from_secs(secs);
        #[cfg(windows)]
        held._hold.set_modified(when).unwrap();
        #[cfg(not(windows))]
        fs::OpenOptions::new()
            .write(true)
            .open(&held.path)
            .unwrap()
            .set_modified(when)
            .unwrap();
    }

    #[test]
    fn refresh_keeps_a_long_live_holder_from_going_stale() {
        let dir = Scratch::new("long");
        let held = FileLock::acquire(dir.path(), "long").unwrap();
        let lock = dir.path().join("long.lock");
        age(&held, 120);
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
