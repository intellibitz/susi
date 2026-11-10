//! Cooperative capacity leases for native development commands, not gate receipts.
//! Slot files are permanent identities. Never unlink a locked file: another
//! process could then lock a replacement inode and spend the same token twice.
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub(crate) fn private_directory(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        private_directory(parent)?;
    }
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            let redirected = metadata.file_type().is_symlink();
            #[cfg(windows)]
            let redirected = {
                use std::os::windows::fs::MetadataExt;
                redirected || metadata.file_attributes() & 0x400 != 0
            };
            if redirected || !metadata.is_dir() {
                return Err(io::Error::other(format!(
                    "directory is redirected or not a directory: {}",
                    path.display()
                )));
            }
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => match fs::create_dir(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => private_directory(path),
            Err(error) => Err(error),
        },
        Err(error) => Err(error),
    }
}

pub(crate) fn try_lock(path: &Path) -> io::Result<Option<File>> {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        let redirected = metadata.file_type().is_symlink();
        #[cfg(windows)]
        let redirected = {
            use std::os::windows::fs::MetadataExt;
            redirected || metadata.file_attributes() & 0x400 != 0
        };
        if !metadata.is_file() || redirected {
            return Err(io::Error::other("capacity lock is not a regular file"));
        }
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(fs::TryLockError::WouldBlock) => Ok(None),
        Err(fs::TryLockError::Error(error)) => Err(error),
    }
}

/// A held lock that is released explicitly on drop. Closing the file alone is
/// not enough: a process spawned concurrently by another thread holds a copy
/// of every open descriptor until it execs, and that copy keeps a `flock`
/// held, so a dropped token would stay spent for that window.
#[derive(Debug)]
struct Held(File);

impl Drop for Held {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// Leave a logical CPU free; cap at the established Linux job limit of 16.
/// This bounds cooperating Cargo jobs, not memory or arbitrary test subprocesses.
pub(crate) fn default_capacity(cpus: usize) -> usize {
    cpus.saturating_sub(1).clamp(1, 16)
}

#[derive(Debug)]
pub(crate) struct Pool {
    root: PathBuf,
    capacity: usize,
}

#[derive(Debug)]
pub(crate) struct Lease {
    _slots: Vec<Held>,
    markers: Vec<PathBuf>,
}

impl Pool {
    /// A persisted ceiling prevents participants with different affinity masks
    /// from silently changing the token count. Corrupt/partial state fails closed.
    pub(crate) fn open(root: &Path, ceiling: usize) -> io::Result<Self> {
        if ceiling == 0 || ceiling > 16 {
            return Err(io::Error::other("capacity must be between 1 and 16"));
        }
        private_directory(root)?;
        let started = Instant::now();
        let _guard = loop {
            if let Some(lock) = try_lock(&root.join("admission.lock"))? {
                break Held(lock);
            }
            if started.elapsed() >= Duration::from_secs(2) {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "capacity admission metadata is busy; retry shortly",
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let record = root.join("capacity-v1");
        let capacity = match OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&record)
        {
            Ok(mut file) => {
                writeln!(file, "susi-dev-capacity-v1\n{ceiling}")?;
                file.sync_all()?;
                ceiling
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let text = fs::read_to_string(&record)?;
                let mut lines = text.lines();
                let version = lines.next();
                let number = lines.next().and_then(|line| line.parse::<usize>().ok());
                match (version, number, lines.next()) {
                    (Some("susi-dev-capacity-v1"), Some(number), None) if number > 0 && number <= ceiling => number,
                    _ => return Err(io::Error::other("invalid capacity record or ceiling exceeds current CPU budget; do not replace live coordination state")),
                }
            }
            Err(error) => return Err(error),
        };
        Ok(Self {
            root: root.to_owned(),
            capacity,
        })
    }

    pub(crate) fn capacity(&self) -> usize {
        self.capacity
    }

    /// All-or-nothing, bounded, nonblocking admission. The OS returns tokens
    /// after exit or crash. Busy callers retain their source and can do other work.
    pub(crate) fn try_acquire(&self, jobs: usize) -> io::Result<Option<Lease>> {
        if jobs == 0 || jobs > self.capacity {
            return Err(io::Error::other(
                "requested jobs exceed the coordinator capacity",
            ));
        }
        let Some(_guard) = try_lock(&self.root.join("admission.lock"))?.map(Held) else {
            return Ok(None);
        };
        let mut slots = Vec::with_capacity(jobs);
        let mut markers = Vec::with_capacity(jobs);
        for index in 0..self.capacity {
            if let Some(slot) = try_lock(&self.root.join(format!("slot-{index}.lock")))?.map(Held) {
                if self
                    .root
                    .join(format!("slot-{index}.running"))
                    .try_exists()?
                {
                    continue;
                }
                slots.push(slot);
                markers.push(self.root.join(format!("slot-{index}.running")));
                if slots.len() == jobs {
                    return Ok(Some(Lease {
                        _slots: slots,
                        markers,
                    }));
                }
            }
        }
        Ok(None)
    }
}

impl Lease {
    /// Persist before spawning any build. If the supervisor is killed while
    /// Cargo survives, unlocked tokens must NOT be reused. Only a normal wait
    /// completion clears these markers; recovery needs independent process proof.
    pub(crate) fn mark_running(&self, owner: &str) -> io::Result<()> {
        let mut created = Vec::new();
        let result = (|| -> io::Result<()> {
            for marker in &self.markers {
                let mut file = OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(marker)?;
                created.push(marker);
                writeln!(file, "susi-dev-running-v1\n{owner}")?;
                file.sync_all()?;
            }
            Ok(())
        })();
        if result.is_err() {
            // No child has started. Remove only records this attempt created;
            // unknown/pre-existing records remain fail-closed for recovery.
            for marker in created {
                let _ = fs::remove_file(marker);
            }
        }
        result
    }
    pub(crate) fn complete(&self) -> io::Result<()> {
        for marker in &self.markers {
            fs::remove_file(marker)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "susi-dev-capacity-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn admission_is_atomic_and_returns_all_tokens_on_drop() {
        let scratch = Scratch::new();
        let pool = Pool::open(&scratch.0, 3).unwrap();
        let lease = pool.try_acquire(2).unwrap().unwrap();
        assert!(pool.try_acquire(2).unwrap().is_none());
        let single = pool.try_acquire(1).unwrap().unwrap();
        assert!(pool.try_acquire(1).unwrap().is_none());
        drop((lease, single));
        assert!(pool.try_acquire(3).unwrap().is_some());
        assert!(pool.try_acquire(0).is_err());
        assert!(pool.try_acquire(4).is_err());
    }

    #[test]
    fn one_hundred_simultaneous_agents_never_oversubscribe() {
        let scratch = Scratch::new();
        let pool = Arc::new(Pool::open(&scratch.0, 8).unwrap());
        let barrier = Arc::new(Barrier::new(100));
        let admitted = Arc::new(AtomicUsize::new(0));
        std::thread::scope(|scope| {
            for _ in 0..100 {
                let (pool, barrier, admitted) = (pool.clone(), barrier.clone(), admitted.clone());
                scope.spawn(move || {
                    barrier.wait();
                    let lease = pool.try_acquire(2).unwrap();
                    if lease.is_some() {
                        admitted.fetch_add(1, Ordering::SeqCst);
                    }
                    barrier.wait();
                    drop(lease);
                });
            }
        });
        // Contending on the admission lock can defer a caller even with free
        // tokens. Capacity is a ceiling, never a promise of a scheduling order.
        assert!((1..=4).contains(&admitted.load(Ordering::SeqCst)));
        assert!(pool.try_acquire(8).unwrap().is_some());
    }

    #[test]
    fn a_hundred_requests_complete_in_bounded_capacity_waves() {
        let scratch = Scratch::new();
        let pool = Pool::open(&scratch.0, 7).unwrap();
        let mut complete = 0;
        let mut waves = 0;
        while complete < 100 {
            let mut leases = Vec::new();
            while complete + leases.len() < 100 {
                let Some(lease) = pool.try_acquire(2).unwrap() else {
                    break;
                };
                leases.push(lease);
            }
            assert!(!leases.is_empty());
            assert!(leases.len() * 2 <= pool.capacity());
            complete += leases.len();
            waves += 1;
        }
        assert_eq!(waves, 34);
    }

    /// A process spawned by another thread holds a copy of every descriptor
    /// until it execs; a `try_clone` shares the same open file description,
    /// so it stands in for that copy without depending on timing.
    #[test]
    fn dropped_tokens_return_while_a_descriptor_copy_is_alive() {
        let scratch = Scratch::new();
        let pool = Pool::open(&scratch.0, 2).unwrap();
        let lease = pool.try_acquire(2).unwrap().unwrap();
        let copies: Vec<File> = lease
            ._slots
            .iter()
            .map(|slot| slot.0.try_clone().unwrap())
            .collect();
        drop(lease);
        assert!(pool.try_acquire(2).unwrap().is_some());
        drop(copies);
    }

    #[test]
    fn crashed_running_supervisor_cannot_release_live_cargo_capacity() {
        let scratch = Scratch::new();
        let pool = Pool::open(&scratch.0, 2).unwrap();
        let lease = pool.try_acquire(2).unwrap().unwrap();
        lease.mark_running("test only; no child launched").unwrap();
        drop(lease);
        assert!(pool.try_acquire(1).unwrap().is_none());
        assert!(scratch.0.join("slot-0.running").is_file());
    }

    #[test]
    fn failed_prelaunch_recording_rolls_back_only_owned_markers() {
        let scratch = Scratch::new();
        let pool = Pool::open(&scratch.0, 2).unwrap();
        let lease = pool.try_acquire(2).unwrap().unwrap();
        fs::write(scratch.0.join("slot-1.running"), "unknown record").unwrap();
        assert!(lease.mark_running("not launched").is_err());
        assert!(!scratch.0.join("slot-0.running").exists());
        assert_eq!(
            fs::read_to_string(scratch.0.join("slot-1.running")).unwrap(),
            "unknown record"
        );
        drop(lease);
        assert!(pool.try_acquire(1).unwrap().is_some());
    }
    #[test]
    fn completed_running_command_returns_its_tokens() {
        let scratch = Scratch::new();
        let pool = Pool::open(&scratch.0, 2).unwrap();
        let lease = pool.try_acquire(2).unwrap().unwrap();
        lease
            .mark_running("test only; simulated completed child")
            .unwrap();
        lease.complete().unwrap();
        drop(lease);
        assert!(pool.try_acquire(2).unwrap().is_some());
    }

    #[test]
    fn ceiling_is_persisted_and_corrupt_state_fails_closed() {
        let scratch = Scratch::new();
        assert_eq!(Pool::open(&scratch.0, 4).unwrap().capacity(), 4);
        assert_eq!(Pool::open(&scratch.0, 8).unwrap().capacity(), 4);
        assert!(Pool::open(&scratch.0, 2).is_err());
        fs::write(
            scratch.0.join("capacity-v1"),
            "susi-dev-capacity-v1\n16\nextra\n",
        )
        .unwrap();
        assert!(Pool::open(&scratch.0, 16).is_err());
        assert_eq!(default_capacity(0), 1);
        assert_eq!(default_capacity(8), 7);
        assert_eq!(default_capacity(128), 16);
    }

    #[test]
    fn capacity_child() {
        let Some(root) = std::env::var_os("SUSI_DEV_CAPACITY_TEST_CHILD") else {
            return;
        };
        let root = PathBuf::from(root);
        let pool = Pool::open(&root, 2).unwrap();
        let _lease = pool.try_acquire(2).unwrap().unwrap();
        fs::write(root.join("child-ready"), "ready").unwrap();
        loop {
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    #[test]
    fn killed_process_releases_capacity_without_stale_lock_deletion() {
        let scratch = Scratch::new();
        let pool = Pool::open(&scratch.0, 2).unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["capacity_child", "--nocapture"])
            .env("SUSI_DEV_CAPACITY_TEST_CHILD", &scratch.0)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let started = Instant::now();
        while !scratch.0.join("child-ready").exists() && started.elapsed() < Duration::from_secs(10)
        {
            std::thread::sleep(Duration::from_millis(10));
        }
        let ready = scratch.0.join("child-ready").exists();
        let blocked = pool.try_acquire(1).unwrap().is_none();
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(ready, "child did not hold a lease before termination");
        assert!(blocked);
        assert!(pool.try_acquire(2).unwrap().is_some());
        assert!(scratch.0.join("slot-0.lock").is_file());
    }
}
