//! Cross-process advisory lock on `<dir>/<name>.lock`, shared by every
//! read-modify-write on host state that sibling processes also touch.

use std::fs;
use std::path::{Path, PathBuf};

/// In-process mutexes cover threads; this
/// lockfile covers sibling processes sharing a config dir — without it,
/// two processes could both read state N and each write N+1, forking the
/// term sequence or interleaving torn JSONL lines into the ledger.
/// Acquisition is `O_CREAT|O_EXCL` on `<name>.lock` inside `dir`; the
/// file records the holder pid for stale detection, and a stale lockfile is
/// removed only while it is still the one judged stale (see `reclaim`).
/// Waiters leave a `<name>.lock.wait` marker so the holder hands over on
/// release (see `handoff`).
/// `pub` so sibling kernel modules (service_table) share the primitive —
/// every cross-process read-modify-write on `~/.susi` state needs it.
#[doc(hidden)]
pub struct FileLock {
    path: PathBuf,
}

impl FileLock {
    /// Retries for ~3 s — the guarded sections are millisecond-scale, so a
    /// longer wait means a wedged holder, not contention.
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
        handoff::wait_out(&lock);
        // A deadline, not an attempt count: a sleep can overshoot (a coarse
        // Windows timer stretches 10 ms towards 16 ms), and counted attempts
        // then stretched the timeout with it.
        let start = std::time::Instant::now();
        // Staleness (a dead holder) is checked on first contention and then
        // every `STALE_CHECK`, not on every brisk poll: it opens and reads
        // the lockfile, and waiters hammering the directory slow the holder
        // down.
        let mut stale_check_due = std::time::Duration::ZERO;
        loop {
            let retry_at_once = match fs::OpenOptions::new()
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
                    let waited = start.elapsed();
                    let check = waited >= stale_check_due;
                    if check {
                        stale_check_due = waited + STALE_CHECK;
                    }
                    if check && reclaim_if_stale(&lock) {
                        true
                    } else {
                        handoff::announce(&lock);
                        false
                    }
                }
                Err(e) if is_delete_pending(&e) => {
                    handoff::announce(&lock);
                    false
                }
                Err(_) => return None,
            };
            let waited = start.elapsed();
            if waited >= timeout {
                return None;
            }
            if !retry_at_once {
                std::thread::sleep(poll_after(waited).min(timeout - waited));
            }
        }
    }

    /// Mark the lock freshly held (mtime = now), so the staleness age rule
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
}

/// One look at a lockfile: the holder's pid line and when the lock was last
/// marked held, both through one open handle, so they describe the same file
/// even while holders come and go under that name.
#[derive(Debug, PartialEq, Eq)]
struct Sighting {
    body: String,
    modified: Option<std::time::SystemTime>,
}

impl Sighting {
    fn read(lock: &Path) -> Option<Self> {
        use std::io::Read;
        let mut file = fs::File::open(lock).ok()?;
        let mut body = String::new();
        file.read_to_string(&mut body).ok()?;
        let modified = file.metadata().and_then(|m| m.modified()).ok();
        Some(Self { body, modified })
    }

    /// A lockfile is stale when its recorded holder is no longer running,
    /// or it has sat unclaimed for over a minute — either means the holder
    /// died mid-claim. Holder liveness comes from `process_probe` (Linux
    /// `/proc`, the Windows process object); where the platform cannot tell,
    /// the age rule alone decides. Windows used to be such a platform, so a
    /// lock whose holder had died blocked every waiter for a full minute.
    fn is_stale(&self) -> bool {
        if let Ok(pid) = self.body.trim().parse::<u32>() {
            if crate::process_probe::pid_alive(pid) == Some(false) {
                return true;
            }
        }
        self.modified
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|e| e.as_secs() > 60)
    }
}

/// How long a reclaimer may hold `<name>.lock.break` before it counts as
/// abandoned (its owner died mid-reclaim). Reclaiming is two small reads
/// and an unlink, so this is generous.
const BREAKER_ABANDONED: std::time::Duration = std::time::Duration::from_secs(5);

/// `lock`'s path with `suffix` appended to its file name.
fn with_suffix(lock: &Path, suffix: &str) -> PathBuf {
    let mut name = lock.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Remove `lock` if it is stale. Whether it was removed.
fn reclaim_if_stale(lock: &Path) -> bool {
    Sighting::read(lock).is_some_and(|judged| judged.is_stale() && reclaim(lock, &judged))
}

/// Remove `lock`, but only while it is still the file `judged` stale.
///
/// A judgement is about the file that was read, and by the time it is made
/// the name can hold another: the holder that was read releases, its process
/// exits (so its pid now reads dead), and a live waiter takes the lock.
/// Removing by name then deleted that live holder's lock, and the next
/// acquirer held the lock alongside it: sibling processes claiming terms
/// lost bumps that way once a dead holder became recognisable on Windows.
/// So the file is read again after the judgement and removed only if it is
/// still the one judged. Its holder is dead (or past the age rule, which
/// counts as dead), so only another reclaimer could change it in between,
/// and `<name>.lock.break` (create-new, like the lock itself) lets one
/// reclaimer at a time do that. A reclaimer that finds the breaker taken
/// leaves the reclaim to its owner and keeps waiting.
fn reclaim(lock: &Path, judged: &Sighting) -> bool {
    let breaker = with_suffix(lock, ".break");
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&breaker)
    {
        Ok(file) => {
            // Closed before the unlink below, or Windows leaves the name
            // delete-pending and the next reclaimer is refused it.
            drop(file);
            let removed =
                Sighting::read(lock).as_ref() == Some(judged) && fs::remove_file(lock).is_ok();
            let _ = fs::remove_file(&breaker);
            removed
        }
        Err(_) => {
            let abandoned = fs::metadata(&breaker)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age > BREAKER_ABANDONED);
            if abandoned {
                let _ = fs::remove_file(&breaker);
            }
            false
        }
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
        handoff::released(&self.path);
    }
}

/// Retry interval while the lock is held elsewhere. Brisk through the
/// default `acquire` horizon: the guarded sections are millisecond-scale,
/// every millisecond between a release and the next waiter's attempt is the
/// lock standing idle, and waiters on one lock must poll at one rate — one
/// that slowed down the longer it waited lost every race to newcomers. Past
/// that horizon only long-timeout callers remain (Tier-0 training waits on
/// a seconds-long holder), and they settle to `POLL` rather than spin.
fn poll_after(waited: std::time::Duration) -> std::time::Duration {
    const BRISK_FOR: std::time::Duration = std::time::Duration::from_secs(3);
    const BRISK: std::time::Duration = std::time::Duration::from_millis(2);
    if waited < BRISK_FOR {
        BRISK
    } else {
        POLL
    }
}

/// The slowest retry interval while the lock is held elsewhere.
const POLL: std::time::Duration = std::time::Duration::from_millis(10);

/// How often a waiter re-checks whether the holder died.
const STALE_CHECK: std::time::Duration = std::time::Duration::from_millis(50);

/// Waiter handoff, so a holder that takes the same lock again in a loop
/// cannot starve the processes waiting on it.
///
/// Waiters poll, but a releaser re-acquires within microseconds; a waiter
/// only won when its attempt happened to land in that gap, so under
/// sustained contention (sibling processes appending to one audit chain)
/// some waited out the whole timeout and their claim was dropped. Now a
/// waiter that finds the lock taken leaves a `<name>.lock.wait` marker; a
/// holder that finds the marker when it releases consumes it, and this
/// process then stays off that lock until a waiter has taken it, or for at
/// most `HANDOFF`. Without waiters there is no marker and no delay.
mod handoff {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    /// Upper bound on the yield, which normally ends within a poll or two
    /// when a waiter takes the lock: long enough that a waiter whose wakeup
    /// is delayed on a loaded host still gets in, and paid in full only when
    /// the waiter that announced itself has since given up.
    const HANDOFF: Duration = Duration::from_millis(100);
    /// How often a yielding releaser checks whether a waiter took the lock.
    const CHECK: Duration = Duration::from_millis(1);

    /// Locks this process released to announced waiters, and when.
    static RELEASED: Mutex<Vec<(PathBuf, Instant)>> = Mutex::new(Vec::new());

    fn marker(lock: &Path) -> PathBuf {
        super::with_suffix(lock, ".wait")
    }

    /// Tell the holder someone is waiting. Errors are ignored: a missed
    /// announcement only costs this waiter the handoff, not correctness.
    pub(super) fn announce(lock: &Path) {
        let _ = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(marker(lock));
    }

    /// After releasing `lock`: if waiters announced themselves, remember to
    /// give them the next turn.
    pub(super) fn released(lock: &Path) {
        if fs::remove_file(marker(lock)).is_ok() {
            let mut released = RELEASED.lock().unwrap_or_else(|e| e.into_inner());
            released.retain(|(path, at)| path != lock && at.elapsed() < HANDOFF);
            released.push((lock.to_path_buf(), Instant::now()));
        }
    }

    /// Before acquiring `lock`: if this process just released it to
    /// waiters, stay off it until one of them has taken it (the lockfile
    /// exists again) or the handoff window has passed.
    pub(super) fn wait_out(lock: &Path) {
        let released_at = {
            let mut released = RELEASED.lock().unwrap_or_else(|e| e.into_inner());
            let found = released.iter().position(|(path, _)| path == lock);
            found.map(|i| released.swap_remove(i).1)
        };
        let Some(released_at) = released_at else {
            return;
        };
        while released_at.elapsed() < HANDOFF && !lock.exists() {
            std::thread::sleep(CHECK);
        }
    }
}

/// Whether a refused create of the lockfile is a just-released lock that
/// Windows has not finished deleting. A Windows delete marks the file on an
/// open handle and the name only goes when that handle closes; until then
/// the file is "delete pending" and creating the name fails with
/// `ERROR_ACCESS_DENIED`, not `AlreadyExists`. Every release passes through
/// that window, and under contention a waiter's create lands in it often —
/// treating it as a hard failure made sibling processes drop claims (a lost
/// term bump, a refused audit append). So on Windows it is waited out like a
/// held lock; a directory that is really unwritable then fails at the
/// timeout rather than at once, with the same `None`. Unix unlinks the name
/// atomically, so there `PermissionDenied` is a real error.
fn is_delete_pending(e: &std::io::Error) -> bool {
    cfg!(windows) && e.kind() == std::io::ErrorKind::PermissionDenied
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
            Sighting::read(&lock).unwrap().is_stale(),
            "a 2-minute-old lock reads as wedged"
        );
        held.refresh();
        assert!(
            !Sighting::read(&lock).unwrap().is_stale(),
            "refresh marks it freshly held"
        );
    }

    /// A stale judgement is about the file that was read. When the name
    /// holds another file by the time of the reclaim (the judged holder
    /// released and a live acquirer took the lock), that live lock stays:
    /// removing by name let the next acquirer in beside its holder, and
    /// sibling processes claiming terms lost bumps. The file that was judged
    /// is still reclaimed.
    #[test]
    fn a_reclaim_removes_only_the_lockfile_judged_stale() {
        let dir = Scratch::new("judged");
        let lock = dir.path().join("judged.lock");
        let judge = || {
            let seen = Sighting::read(&lock).unwrap();
            assert!(seen.is_stale(), "{seen:?}");
            seen
        };
        // A holder past the age rule, judged stale...
        fs::write(&lock, "1\n").unwrap();
        age(&lock, 120);
        let judged = judge();
        // ...releases, and a live acquirer takes the name.
        fs::remove_file(&lock).unwrap();
        let live = FileLock::acquire(dir.path(), "judged").unwrap();
        assert!(!reclaim(&lock, &judged), "reclaimed a lock nobody judged");
        assert!(lock.exists(), "the live holder's lock was removed");
        assert!(
            FileLock::acquire_within(dir.path(), "judged", std::time::Duration::from_millis(50))
                .is_none(),
            "a second holder got in beside the live one"
        );
        drop(live);
        // The file that was judged is reclaimed, breaker and all.
        fs::write(&lock, "1\n").unwrap();
        age(&lock, 120);
        let judged = judge();
        assert!(reclaim(&lock, &judged));
        assert!(!lock.exists());
        assert!(!with_suffix(&lock, ".break").exists());
    }

    /// One reclaimer at a time: while another holds the breaker, a reclaim
    /// leaves the stale lock to it; a breaker abandoned by a reclaimer that
    /// died mid-reclaim is cleared, so it cannot block reclaims for good.
    #[test]
    fn a_held_breaker_defers_the_reclaim_and_an_abandoned_one_is_cleared() {
        let dir = Scratch::new("breaker");
        let lock = dir.path().join("breaker.lock");
        let breaker = with_suffix(&lock, ".break");
        fs::write(&lock, "1\n").unwrap();
        age(&lock, 120);
        let judged = Sighting::read(&lock).unwrap();
        fs::write(&breaker, "").unwrap();
        assert!(
            !reclaim(&lock, &judged),
            "reclaimed under another's breaker"
        );
        assert!(lock.exists() && breaker.exists());
        age(&breaker, 60);
        assert!(!reclaim(&lock, &judged), "this attempt only clears it");
        assert!(!breaker.exists(), "an abandoned breaker is cleared");
        assert!(reclaim(&lock, &judged));
        assert!(!lock.exists());
    }

    /// A holder that died without releasing is recognised by its pid at
    /// once, not after the 60 s age rule. Linux and Windows have a liveness
    /// source; elsewhere only the age rule applies, which
    /// `refresh_keeps_a_long_live_holder_from_going_stale` covers.
    #[cfg(any(target_os = "linux", windows))]
    #[test]
    fn a_lock_whose_holder_exited_is_reclaimed_at_once() {
        let dir = Scratch::new("dead");
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--list", "--format=terse"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let dead = child.id();
        assert!(child.wait().unwrap().success());
        let lock = dir.path().join("dead.lock");
        fs::write(&lock, format!("{dead}\n")).unwrap();
        assert!(
            FileLock::acquire_within(dir.path(), "dead", std::time::Duration::from_millis(500))
                .is_some(),
            "a lock naming exited pid {dead} must be reclaimed without waiting out its age"
        );
        // A live holder's lock is not: this process is running.
        fs::write(&lock, format!("{}\n", std::process::id())).unwrap();
        assert!(FileLock::acquire_within(
            dir.path(),
            "dead",
            std::time::Duration::from_millis(100)
        )
        .is_none());
    }

    /// Contending acquirers all get the lock, one at a time: none gives up
    /// while the lock is only changing hands. On Windows a release leaves the
    /// lockfile delete-pending for a moment, and a create landing then is
    /// refused with access denied; `acquire` used to return `None` on that at
    /// once, so a sibling process dropped its claim (a lost term bump, a
    /// refused audit append).
    #[test]
    fn contending_acquirers_all_get_the_lock_one_at_a_time() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
        const THREADS: usize = 4;
        const ROUNDS: usize = 100;
        let dir = Scratch::new("contend");
        let inside = AtomicBool::new(false);
        let gave_up = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..THREADS {
                scope.spawn(|| {
                    for _ in 0..ROUNDS {
                        let Some(held) = FileLock::acquire(dir.path(), "contend") else {
                            gave_up.fetch_add(1, SeqCst);
                            continue;
                        };
                        assert!(!inside.swap(true, SeqCst), "two holders at once");
                        inside.store(false, SeqCst);
                        drop(held);
                    }
                });
            }
        });
        assert_eq!(
            gave_up.load(SeqCst),
            0,
            "acquires gave up on a lock that was only changing hands"
        );
    }

    /// A holder that takes the lock again the moment it lets go hands it to
    /// a waiter first. The waiter polls every few milliseconds and the holder
    /// is back within microseconds, so without the handoff the waiter only got
    /// in when a poll happened to land in that gap — sibling processes
    /// appending to one audit chain came within a whisker of the 3 s timeout
    /// that way, and past it on a loaded host.
    #[test]
    fn a_holder_retaking_the_lock_hands_it_to_a_waiting_acquirer_first() {
        use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
        use std::time::Duration;
        let dir = Scratch::new("handoff");
        let waiter_in = AtomicBool::new(false);
        let mut held = FileLock::acquire(dir.path(), "handoff");
        assert!(held.is_some());
        let waiter_got_a_turn = std::thread::scope(|scope| {
            scope.spawn(|| {
                let got = FileLock::acquire_within(dir.path(), "handoff", Duration::from_secs(5));
                assert!(got.is_some());
                waiter_in.store(true, SeqCst);
            });
            // Hold long enough for the waiter to find the lock taken, then
            // release and retake it at once, as a looping holder does. Three
            // rounds, so one overslept poll on a loaded host is not a failure;
            // without a handoff each round is a ~1% lottery for the waiter.
            for _ in 0..3 {
                std::thread::sleep(Duration::from_millis(40));
                drop(held.take());
                held = FileLock::acquire(dir.path(), "handoff");
                if waiter_in.load(SeqCst) {
                    break;
                }
            }
            let got_a_turn = waiter_in.load(SeqCst);
            drop(held.take());
            got_a_turn
        });
        assert!(
            waiter_got_a_turn,
            "the holder kept retaking the lock past a waiting acquirer"
        );
    }

    /// The handoff itself, deterministically: a release that finds a
    /// waiter's marker keeps this process off the lock until someone takes
    /// it, or for the whole window when nobody does; a release nobody waited
    /// for keeps nobody off, and a yield is spent once.
    #[test]
    fn a_release_seen_by_a_waiter_yields_until_the_lock_is_taken() {
        use std::time::{Duration, Instant};
        let dir = Scratch::new("yield");
        let lock = dir.path().join("yield.lock");
        let timed = |lock: &Path| {
            let t = Instant::now();
            handoff::wait_out(lock);
            t.elapsed()
        };
        // Nobody announced: no yield.
        handoff::released(&lock);
        assert!(timed(&lock) < Duration::from_millis(90));
        // A waiter announced but never takes the lock: the whole window,
        // once.
        handoff::announce(&lock);
        handoff::released(&lock);
        let waited = timed(&lock);
        assert!(waited >= Duration::from_millis(90), "{waited:?}");
        assert!(timed(&lock) < Duration::from_millis(90), "a spent yield");
        // A waiter announced and takes the lock: the yield ends there.
        handoff::announce(&lock);
        handoff::released(&lock);
        fs::write(&lock, "1\n").unwrap();
        let waited = timed(&lock);
        assert!(waited < Duration::from_millis(90), "{waited:?}");
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
