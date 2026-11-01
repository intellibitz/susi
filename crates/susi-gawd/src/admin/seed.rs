//! A cargo target of a worktree's own, warm from a snapshot of origin/main:
//! `susi workflow seed-target [--background] [DIR]` and `susi workflow
//! seed-target --refresh [--force] [--log FILE]`.
//!
//! `scripts/seed-target.sh` carried this in bash; it is a stub now that execs
//! this (so hooks, docs and branches cut before the port keep working). The
//! Rust runs `git`, `cargo`, the copy tool (`robocopy` on Windows, `cp -a
//! --reflink=auto` elsewhere) and, on Windows, `tasklist` through [`Command`],
//! with no shell, coreutils, awk, `find`, `cygpath` or `ps -W` in between, so
//! an installed `susi.exe` seeds a worktree without Git's MSYS userland on
//! PATH. Linux and macOS behave exactly as the script did;
//! `tests/worktree_target_dir.rs` pins every path below through the binary.
//!
//! Worktrees used to share the primary checkout's `target/` through a link.
//! Cargo names a workspace crate's build by its path relative to the
//! workspace, so a crate another worktree built last read as fresh in this one
//! and its code was linked into this branch's build. A target of the
//! worktree's own is correct by construction; seeded from the snapshot it costs
//! the copy of the snapshot's registry crates plus the workspace crates
//! instead of a cold build.
//!
//! What seeding does to `DIR/target` (never in the primary checkout, which
//! keeps the target it has and is told while worktrees still link it):
//!
//! 1. A link there (a symlink, or a junction on Windows) is removed: the link
//!    only, never what it points to.
//! 2. A missing or empty target is filled from the snapshot, `SUSI_TARGET_SEED`
//!    (default `<primary>/../.susi-target-seed`), timestamps kept. Never hard
//!    links: cargo rewrites fingerprint files in place. `incremental/` is left
//!    out, and nothing is ever removed through a link the copy holds: every
//!    delete checks each directory from the tree it works in down to what it
//!    removes. Only a snapshot `--refresh` published is copied (it carries
//!    `.susi-seed`): never a checkout, never the primary's own target, never
//!    one with a link where cargo keeps builds.
//! 3. Every unit not built from a registry or git source of `DIR`'s
//!    `Cargo.lock` is stripped from the copy (the workspace members, path
//!    dependencies, crates the lockfile does not name): their builds are named
//!    by their path relative to the workspace, so the snapshot's would read as
//!    fresh here. A copy that cannot be stripped all the way is dropped, and
//!    the target starts empty: cold, and correct.
//!
//! The copy is made in `DIR/target.seed-<pid>-<time>.tmp` and renamed into
//! place, so a cargo that starts meanwhile never reads half a snapshot. A
//! worktree removed while its copy runs is never brought back. Nothing is
//! copied, and no copy is waited for, when `CARGO_TARGET_DIR` (or
//! `CARGO_BUILD_TARGET_DIR`) names another directory. A target that holds
//! anything is this worktree's own build and is left alone. `--background`
//! replaces a link now and runs the copy in a detached process (the hooks).
//! A foreground run whose worktree is being copied into waits for that copy
//! (`SUSI_SEED_WAIT_SECS`, 120). `SUSI_NO_SEED=1` skips the copy (a link is
//! still replaced). It never fails its caller.
//!
//! `--refresh` is the snapshot's only writer: it builds origin/main at low
//! priority in a clone of its own (`SUSI_TARGET_SEED_SRC`, default
//! `<seed>-src`), one it made and marked, strips it, and publishes by rename;
//! one refresh at a time (`<seed>.lock`), nothing when the snapshot already
//! holds origin/main or is younger than `SUSI_SEED_REFRESH_SECS` (3600) unless
//! `--force`.
//!
//! Locks are directories holding their holder's owner line: `<pid> <since>
//! <winpid> <runtime>`. A pid means something only in the process table of the
//! runtime that wrote it: this binary's is the host's own (`windows`, or the
//! OS name elsewhere); Git for Windows' bash, which wrote the lines before the
//! port, named its install root and recorded its Windows pid. A holder of this
//! table is alive while its pid is; one of another table is asked about by its
//! Windows pid (`tasklist`); only a holder neither can tell about is presumed
//! alive until its line is `SUSI_SEED_LOCK_STALE` seconds old (a day for
//! `--refresh`, two hours for a worktree's copy). A live holder's lock is
//! never taken over, however long it builds.
//!
//! Test seams, unset in use: `SUSI_SEED_TEST_GATES=<dir>` has a run append
//! each phase it reaches (`lock`, `copy`, `rename`, `done`) to `<dir>/log` and
//! wait at a phase while `<dir>/<phase>.wait` exists until `<dir>/<phase>.go`
//! does; `SUSI_SEED_CARGO` names the cargo `--refresh` builds with (a program
//! and leading arguments, as a PATH-style list), since a stand-in script
//! cannot be run by name on Windows.

use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Run a host tool without creating an interactive console. In particular, a
/// Git wrapper spawned by a detached worker must not allocate a transient
/// console whose shutdown can terminate it with STATUS_CONTROL_C_EXIT.
fn host_command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let command = Command::new(program);
    #[cfg(windows)]
    let command = {
        use std::os::windows::process::CommandExt;
        let mut command = command;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        command
    };
    command
}
/// The snapshot's marker: `<origin/main commit> <epoch>`; only `--refresh`
/// writes it.
const SEED_MARK: &str = ".susi-seed";
/// A seeded target's marker: `<commit or -> <epoch>`.
const SEEDED_MARK: &str = ".susi-seeded";
/// A staging directory's owner line.
const RUN_MARK: &str = ".susi-seed-run";
/// The background copy's log, in the worktree's git dir.
const LOG_NAME: &str = "susi-seed-target.log";
/// The lock of a copy into a worktree, in its git dir.
const LOCK_NAME: &str = "susi-seed-target.lockdir";

// --- small helpers -----------------------------------------------------------

/// Where [`say`] writes when the run was given `--log`: the background copy
/// reports into the worktree's seed log itself, since nothing else may hold
/// its output (see [`spawn_background`]).
static REPORT: OnceLock<PathBuf> = OnceLock::new();

/// Send every report of this run to `path`, appended, instead of stderr.
pub fn report_to(path: &Path) {
    let _ = REPORT.set(path.to_path_buf());
}

fn say(msg: &str) {
    if let Some(path) = REPORT.get() {
        if let Ok(mut f) = fs::OpenOptions::new().append(true).create(true).open(path) {
            let _ = writeln!(f, "target: {msg}");
            return;
        }
    }
    eprintln!("target: {msg}");
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `YYYY-MM-DD HH:MM:SS` (UTC) for the refresh log.
pub(crate) fn timestamp() -> String {
    let secs = now_secs();
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Howard Hinnant's civil-from-days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

fn env_flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| v == "1")
}

fn env_secs(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

fn no_seed() -> bool {
    env_flag("SUSI_NO_SEED")
}

fn slashes(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// A symlink, or a junction on Windows (both are reparse points with a name
/// surrogate, which `is_symlink` reports).
fn is_link(p: &Path) -> bool {
    p.symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
}

/// A real directory: not a link to one.
fn is_real_dir(p: &Path) -> bool {
    p.symlink_metadata()
        .is_ok_and(|m| m.file_type().is_dir() && !m.file_type().is_symlink())
}

/// `[ -e p ] || [ -L p ]`: something is there, a dangling link included.
fn is_there(p: &Path) -> bool {
    p.symlink_metadata().is_ok()
}

fn dir_entries(dir: &Path) -> Vec<PathBuf> {
    fs::read_dir(dir)
        .map(|rd| rd.filter_map(Result::ok).map(|e| e.path()).collect())
        .unwrap_or_default()
}

fn age_secs(p: &Path) -> Option<u64> {
    p.symlink_metadata()
        .ok()?
        .modified()
        .ok()?
        .elapsed()
        .ok()
        .map(|d| d.as_secs())
}

fn first_line(p: &Path) -> String {
    fs::read_to_string(p)
        .ok()
        .and_then(|t| t.lines().next().map(str::to_string))
        .unwrap_or_default()
}

/// Remove the link at `link` and nothing it points to. On Windows a junction
/// or directory symlink goes with `remove_dir`, a file symlink with
/// `remove_file`; elsewhere `remove_file` unlinks any symlink.
fn remove_link(link: &Path) -> bool {
    if !is_link(link) {
        return false;
    }
    #[cfg(windows)]
    {
        if fs::remove_dir(link).is_err() {
            let _ = fs::remove_file(link);
        }
    }
    #[cfg(not(windows))]
    {
        let _ = fs::remove_file(link);
    }
    !is_there(link)
}

/// Remove a directory this run made (a refused copy, a staging directory).
/// Never through a link: a link is removed as the link, and what it points to
/// stays.
pub fn discard(dir: &Path) {
    if is_link(dir) {
        remove_link(dir);
    } else if dir.is_dir() {
        let _ = fs::remove_dir_all(dir);
    }
}

/// `rm -rf p`: a link as the link, a directory with its contents, a file.
fn remove_any(p: &Path) -> bool {
    if is_link(p) {
        return remove_link(p);
    }
    match p.symlink_metadata() {
        Err(_) => true,
        Ok(m) if m.is_dir() => fs::remove_dir_all(p).is_ok(),
        Ok(_) => fs::remove_file(p).is_ok(),
    }
}

/// Empty as far as a build goes: nothing, or only what cargo writes before
/// it builds anything.
fn holds_nothing(dir: &Path) -> bool {
    dir_entries(dir).iter().all(|e| {
        matches!(
            e.file_name().and_then(|n| n.to_str()),
            Some("CACHEDIR.TAG" | ".rustc_info.json")
        )
    })
}

/// The directory `p` as it resolves, links followed: on Windows in its drive
/// form (`c:/...`), lowercased, which no mount or junction can disguise.
fn canon_dir(p: &Path) -> Option<String> {
    let real = fs::canonicalize(p).ok()?;
    if !real.is_dir() {
        return None;
    }
    let s = real.to_string_lossy().into_owned();
    if cfg!(windows) {
        let s = s
            .strip_prefix(r"\\?\UNC\")
            .map(|rest| format!(r"\\{rest}"))
            .or_else(|| s.strip_prefix(r"\\?\").map(str::to_string))
            .unwrap_or(s);
        Some(s.replace('\\', "/").to_ascii_lowercase())
    } else {
        Some(s)
    }
}

fn same_dir(a: &Path, b: &Path) -> bool {
    matches!((canon_dir(a), canon_dir(b)), (Some(x), Some(y)) if x == y)
}

/// Whether the path `at`, under the directory `root`, is reached through a
/// link: `at` itself, or any directory between `root` and it, is one. A path
/// not under `root` counts as reached through one: it is refused, never
/// deleted.
fn through_link(root: &Path, at: &Path) -> bool {
    if at == root || at.strip_prefix(root).is_err() {
        return true;
    }
    let mut p = at;
    while p != root {
        if is_link(p) {
            return true;
        }
        match p.parent() {
            Some(up) => p = up,
            None => return true,
        }
    }
    false
}

/// Whether the build tree holds a link where cargo keeps builds: an entry of
/// its own (a profile, a target triple's directory), or an entry of one of
/// its directories that is no profile (a target triple's profile). Cargo
/// makes none there. `tmp/` is the workspace's tests' scratch.
fn holds_foreign_link(tree: &Path) -> bool {
    for a in dir_entries(tree) {
        if is_link(&a) {
            return true;
        }
        let name = a.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if !a.is_dir() || name == "tmp" || a.join(".fingerprint").is_dir() {
            continue;
        }
        if dir_entries(&a).iter().any(|b| is_link(b)) {
            return true;
        }
    }
    false
}

/// `<tree>/*/.fingerprint` and `<tree>/*/*/.fingerprint`, whatever they are.
fn fingerprint_dirs(tree: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for a in dir_entries(tree) {
        let fp = a.join(".fingerprint");
        if is_there(&fp) {
            found.push(fp);
        }
        if a.is_dir() {
            for b in dir_entries(&a) {
                let fp = b.join(".fingerprint");
                if is_there(&fp) {
                    found.push(fp);
                }
            }
        }
    }
    found
}

/// The snapshot for the clone whose primary checkout is `primary`. Dot-named,
/// so no worktree name (`<primary>-<name>`) puts a checkout there.
fn seed_dir(primary: &Path) -> PathBuf {
    match std::env::var_os("SUSI_TARGET_SEED").filter(|v| !v.is_empty()) {
        Some(v) => PathBuf::from(v.to_string_lossy().replace('\\', "/")),
        None => primary
            .parent()
            .unwrap_or(primary)
            .join(".susi-target-seed"),
    }
}

/// A snapshot to copy: one `--refresh` published (it carries `.susi-seed`),
/// with a profile's fingerprints in it. Never a checkout, never the primary
/// checkout's own target, never one with a link where cargo keeps builds.
fn usable_seed(seed: &Path, primary: &Path) -> bool {
    if !seed.is_dir() || !seed.join(SEED_MARK).is_file() || is_there(&seed.join(".git")) {
        return false;
    }
    let Some(real) = canon_dir(seed) else {
        return false;
    };
    if canon_dir(&primary.join("target")).is_some_and(|shared| shared == real) {
        return false;
    }
    if holds_foreign_link(seed) {
        return false;
    }
    let mut found = false;
    for fp in fingerprint_dirs(seed) {
        if through_link(seed, &fp) {
            return false;
        }
        if fp.is_dir() {
            found = true;
        }
    }
    found
}

/// The directory the environment has cargo build into, when that is not the
/// target `t`: `CARGO_TARGET_DIR`, else `CARGO_BUILD_TARGET_DIR`. A relative
/// one is taken from the worktree (`t`'s parent), where the cargo this is
/// for runs: the hooks, the gate and `tasks close` run there, and `workflow
/// start` runs from the primary checkout for a cargo that runs after its `cd`,
/// so this process's own directory would read `target` as the primary's.
/// `None` when neither is set, or it names `t` itself, however spelled.
fn builds_elsewhere(t: &Path) -> Option<(String, &'static str)> {
    let (var, named) = ["CARGO_TARGET_DIR", "CARGO_BUILD_TARGET_DIR"]
        .into_iter()
        .find_map(|var| {
            std::env::var(var)
                .ok()
                .filter(|v| !v.is_empty())
                .map(|v| (var, v))
        })?;
    let mut named = named.replace('\\', "/");
    let absolute = named.starts_with('/')
        || (named.len() >= 3 && named.as_bytes()[0].is_ascii_alphabetic() && &named[1..3] == ":/");
    if !absolute {
        let worktree = t.parent()?;
        named = format!("{}/{named}", slashes(worktree));
    }
    while named.len() > 1 && named.ends_with('/') {
        named.pop();
    }
    let named_path = PathBuf::from(&named);
    let same = if named_path.is_dir() && t.is_dir() {
        same_dir(&named_path, t)
    } else {
        // One is not made yet: the same parent, and the same name.
        match (
            named_path.parent().and_then(canon_dir),
            t.parent().and_then(canon_dir),
        ) {
            (Some(a), Some(b)) => {
                let base = |p: &Path| {
                    p.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default()
                };
                let (mut a, mut b) = (
                    format!("{a}/{}", base(&named_path)),
                    format!("{b}/{}", base(t)),
                );
                if cfg!(windows) {
                    a = a.to_ascii_lowercase();
                    b = b.to_ascii_lowercase();
                }
                a == b
            }
            _ => false,
        }
    };
    (!same).then_some((named, var))
}

// --- owners and locks that hold across runtimes -------------------------------

/// This binary's process table. Git for Windows' bash, which wrote owner
/// lines before the port, named its install root instead.
#[must_use]
pub fn runtime_name() -> String {
    if cfg!(windows) {
        "windows".to_string()
    } else {
        std::env::consts::OS.to_string()
    }
}

/// An owner line: `<pid> <since> <winpid> <runtime>`; the runtime may hold
/// spaces (a Git install root).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Owner {
    pub pid: u64,
    pub since: u64,
    pub wpid: Option<u64>,
    pub runtime: String,
}

impl Owner {
    /// Parse a line as `read -r pid since wpid rt` did; `None` when the pid is
    /// not a number (a line nobody wrote).
    #[must_use]
    pub fn parse(line: &str) -> Option<Self> {
        let mut fields = line.split_whitespace();
        let pid = fields.next()?.parse().ok()?;
        let since = fields.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        let wpid = fields.next().and_then(|s| s.parse().ok());
        let runtime = fields.collect::<Vec<_>>().join(" ");
        Some(Self {
            pid,
            since,
            wpid,
            runtime,
        })
    }

    /// This process's line, with the time it first asked.
    #[must_use]
    pub fn me() -> Self {
        static SINCE: OnceLock<u64> = OnceLock::new();
        let pid = u64::from(std::process::id());
        Self {
            pid,
            since: *SINCE.get_or_init(now_secs),
            wpid: cfg!(windows).then_some(pid),
            runtime: runtime_name(),
        }
    }

    #[must_use]
    pub fn line(&self) -> String {
        let wpid = self.wpid.map_or_else(|| "-".to_string(), |w| w.to_string());
        format!("{} {} {wpid} {}", self.pid, self.since, self.runtime)
    }

    fn is_me(&self) -> bool {
        let me = Self::me();
        self.pid == me.pid
            && same_table(&self.runtime)
            && (self.wpid.is_none() || me.wpid.is_none() || self.wpid == me.wpid)
    }
}

/// Whether a line's runtime is this process table: this binary's own name, or
/// (one table per host outside Windows) the `$OSTYPE`-like name the bash
/// script wrote, which is no path.
fn same_table(runtime: &str) -> bool {
    if runtime.eq_ignore_ascii_case(&runtime_name()) {
        return true;
    }
    !cfg!(windows) && !runtime.contains(['/', '\\', ':'])
}

/// Whether the process `pid` of this table runs. An exited process nothing
/// has reaped yet (a zombie: an orphan where PID 1 never reaps) counts as
/// ended, as it does for the copy watcher in the tests.
fn pid_alive(pid: u64) -> bool {
    #[cfg(target_os = "linux")]
    {
        let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
            return false;
        };
        let state = stat.rsplit(") ").next().unwrap_or("").trim_start();
        !matches!(state.chars().next(), Some('Z' | 'X') | None)
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        let out = host_command("ps")
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .stdin(Stdio::null())
            .output();
        match out {
            Ok(out) => {
                let state = String::from_utf8_lossy(&out.stdout).trim().to_string();
                !state.is_empty() && !state.starts_with('Z')
            }
            Err(_) => true, // cannot tell: presumed alive
        }
    }
    #[cfg(windows)]
    {
        matches!(win_image(pid), Ok(Some(image)) if is_our_image(&image))
    }
}

/// The image name of the Windows process `pid` (`tasklist`): `Ok(None)` when
/// no such process runs, `Err` when tasklist cannot be asked.
#[cfg(windows)]
fn win_image(pid: u64) -> Result<Option<String>, ()> {
    use std::os::windows::process::CommandExt;
    let out = host_command("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
        .stdin(Stdio::null())
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .output()
        .map_err(|_| ())?;
    if !out.status.success() {
        return Err(());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix('"') {
            if let Some(end) = rest.find('"') {
                return Ok(Some(rest[..end].to_string()));
            }
        }
    }
    Ok(None)
}

/// Whether a Windows image is a susi (this binary's name) or one of Git's
/// shells (a holder from before the port). A pid handed to another program is
/// someone else.
#[cfg(windows)]
fn is_our_image(image: &str) -> bool {
    let image = image.to_ascii_lowercase();
    let ours = std::env::current_exe().ok().and_then(|p| {
        p.file_name()
            .map(|n| n.to_string_lossy().to_ascii_lowercase())
    });
    ours.is_some_and(|o| o == image) || matches!(image.as_str(), "susi.exe" | "bash.exe" | "sh.exe")
}

/// Whether the holder an owner line names may still run. Age never ends a
/// holder whose liveness can be told: a refresh at low priority on a busy host
/// can build for hours, and taking its lock over would start a second build
/// in the same checkout and target.
fn holder_alive(owner: &Owner, stale: u64) -> bool {
    if same_table(&owner.runtime) {
        return pid_alive(owner.pid);
    }
    #[cfg(windows)]
    {
        if let Some(wpid) = owner.wpid {
            match win_image(wpid) {
                Ok(Some(image)) => return is_our_image(&image),
                Ok(None) => return false,
                Err(()) => {}
            }
        }
    }
    now_secs().saturating_sub(owner.since) < stale
}

/// Write this process's owner line into `file`, by rename: a reader sees the
/// old line or the new one, never half of one.
fn sign(file: &Path) -> bool {
    let tmp = file.with_extension(format!("{}", std::process::id()));
    fs::write(&tmp, format!("{}\n", Owner::me().line())).is_ok() && fs::rename(&tmp, file).is_ok()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Lock {
    /// Taken (or already this process's).
    Taken,
    /// A live holder has it.
    Held,
    /// No lock can be made there.
    Impossible,
}

/// Take the lock directory `lock` for this process without waiting; a holder
/// is presumed gone once its line is `stale` seconds old.
///
/// A gone holder's lock is never removed (a taker that judged it gone would
/// remove whatever lock stood there when its delete ran, the next holder's
/// included): each taker makes a token named after the line it judged gone,
/// `create_dir` being atomic, and the one that makes it reads the line again
/// and signs the lock in place. A token whose taker died before signing (a
/// minute old) is passed by making the next one.
///
/// Only the lock itself is made, never a missing parent: a worktree's lock is
/// in its git dir, and a parent missing there is a worktree removed meanwhile,
/// whose `.git/worktrees/<name>` must not come back.
pub(crate) fn take_lock(lock: &Path, stale: u64) -> Lock {
    let owner = lock.join("owner");
    if fs::create_dir(lock).is_ok() {
        if sign(&owner) {
            return Lock::Taken;
        }
        let _ = fs::remove_dir_all(lock);
        return Lock::Impossible;
    }
    if !lock.is_dir() || is_link(lock) {
        return Lock::Impossible;
    }
    let line = first_line(&owner);
    let key = if line.is_empty() {
        // Made and not yet signed; its maker died in between once a minute passes.
        if age_secs(lock).is_none_or(|a| a < 60) {
            return Lock::Held;
        }
        "unsigned".to_string()
    } else {
        match Owner::parse(&line) {
            Some(o) if o.is_me() => return Lock::Taken,
            Some(o) if holder_alive(&o, stale) => return Lock::Held,
            Some(o) => format!("{}.{}", o.pid, o.since),
            None => line
                .chars()
                .map(|c| {
                    if c.is_ascii_digit() || c == '.' {
                        c
                    } else {
                        '_'
                    }
                })
                .collect(),
        }
    };
    for k in 1..=5 {
        let tok = lock.join(format!("takeover.{key}.{k}"));
        if fs::create_dir(&tok).is_ok() {
            if first_line(&owner) != line {
                return Lock::Held;
            }
            return if sign(&owner) {
                Lock::Taken
            } else {
                Lock::Held
            };
        }
        if !tok.is_dir() || age_secs(&tok).is_none_or(|a| a < 60) {
            return Lock::Held;
        }
    }
    Lock::Held
}

/// The lock this process holds, let go of on drop: only while its line is
/// this process's own. A run that dies holding it leaves a line whose pid is
/// gone, which the next taker reads as such.
pub(crate) struct HeldLock(pub(crate) PathBuf);

impl Drop for HeldLock {
    fn drop(&mut self) {
        let lock = &self.0;
        if !lock.is_dir() {
            return;
        }
        if Owner::parse(&first_line(&lock.join("owner"))).is_some_and(|o| o.is_me()) {
            let _ = fs::remove_dir_all(lock);
        }
    }
}

/// Mark the staging directory as this run's: a sweep by another run leaves it
/// alone while this one lives.
fn claim_staging(dir: &Path) -> bool {
    fs::write(dir.join(RUN_MARK), format!("{}\n", Owner::me().line())).is_ok()
}

fn still_mine(dir: &Path) -> bool {
    first_line(&dir.join(RUN_MARK)) == Owner::me().line()
}

/// Remove the staging directories among `dirs` that no live run owns: each
/// carries its run's owner line, counted gone after `stale` seconds; one with
/// no line is from a run that died before it wrote one, once it is an hour
/// old.
fn sweep_staging(stale: u64, dirs: &[PathBuf]) {
    for d in dirs {
        if !is_there(d) {
            continue;
        }
        if is_link(d) {
            remove_link(d);
            continue;
        }
        if !d.is_dir() {
            continue;
        }
        let line = first_line(&d.join(RUN_MARK));
        if line.is_empty() {
            if age_secs(d).is_none_or(|a| a < 3600) {
                continue;
            }
        } else if let Some(o) = Owner::parse(&line) {
            if o.is_me() || holder_alive(&o, stale) {
                continue;
            }
        }
        discard(d);
    }
}

/// The entries of `dir` whose names start with `prefix`.
fn named_like(dir: &Path, prefix: &str) -> Vec<PathBuf> {
    dir_entries(dir)
        .into_iter()
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with(prefix))
        })
        .collect()
}

// --- test seams ----------------------------------------------------------------

/// `SUSI_SEED_TEST_GATES`: record the phase, and hold at it while asked to.
fn test_gate(phase: &str) {
    let Some(dir) = std::env::var_os("SUSI_SEED_TEST_GATES").filter(|v| !v.is_empty()) else {
        return;
    };
    let dir = PathBuf::from(dir);
    if let Ok(mut log) = fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(dir.join("log"))
    {
        let _ = writeln!(log, "{phase}");
    }
    if dir.join(format!("{phase}.wait")).exists() {
        let go = dir.join(format!("{phase}.go"));
        let deadline = Instant::now() + Duration::from_secs(120);
        while !go.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// The cargo to run: `SUSI_SEED_CARGO` (a program and its leading arguments
/// as a PATH-style list, a test's seam), else `cargo` on PATH.
fn cargo_command() -> Command {
    if let Some(list) = std::env::var_os("SUSI_SEED_CARGO").filter(|v| !v.is_empty()) {
        let mut parts = std::env::split_paths(&list);
        if let Some(program) = parts.next() {
            let mut cmd = host_command(program);
            cmd.args(parts);
            return cmd;
        }
    }
    host_command("cargo")
}

// --- copying and stripping a build tree -----------------------------------------

/// Copy the build tree `from` into the directory `to`, which the caller made,
/// timestamps kept, with no `incremental/` caches, guard state or snapshot
/// markers. robocopy's exit codes 0-7 all mean success; 8 and up is a failure.
/// robocopy skips junctions and symlinks (`/XJ`); `cp -a` copies a link as a
/// link, and [`drop_extras`] never reaches through one.
fn copy_tree(from: &Path, to: &Path) -> bool {
    if !to.is_dir() || is_link(to) {
        return false;
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // Native spellings, as `cygpath -aw` gave the script: git names a
        // checkout with forward slashes.
        let native = |p: &Path| p.to_string_lossy().replace('/', "\\");
        let status = host_command("robocopy")
            .arg(native(from))
            .arg(native(to))
            .args([
                "/E",
                "/COPY:DAT",
                "/DCOPY:DAT",
                "/MT:32",
                "/XJ",
                "/R:1",
                "/W:1",
                "/XD",
                "incremental",
                ".susi-guard",
                ".susi-strip",
                "/XF",
                SEED_MARK,
                SEEDED_MARK,
                RUN_MARK,
                "/NP",
                "/NFL",
                "/NDL",
                "/NJH",
                "/NJS",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
            .status();
        status.is_ok_and(|s| s.code().is_some_and(|c| c < 8))
    }
    #[cfg(not(windows))]
    {
        let mut src = from.as_os_str().to_owned();
        src.push("/.");
        let mut dst = to.as_os_str().to_owned();
        dst.push("/");
        let gnu = host_command("cp")
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        let mut cp = host_command("cp");
        if gnu {
            cp.args(["-a", "--reflink=auto", "--"]);
        } else {
            // BSD cp (macOS): -p keeps the times, -P copies links as links.
            cp.args(["-pPR", "--"]);
        }
        let ok = cp
            .arg(&src)
            .arg(&dst)
            .stdin(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            return false;
        }
        drop_extras(to);
        true
    }
}

/// Remove from the fresh copy `to` what robocopy's `/XD` and `/XF` leave out:
/// every profile's `incremental/` caches (two or three levels down), the
/// guard's and the strip's state, the snapshot's markers. Never through a
/// link: the copy keeps a link as a link, and a profile that is one leads to
/// another tree's caches.
pub fn drop_extras(to: &Path) {
    for a in dir_entries(to) {
        if !is_real_dir(&a) {
            continue;
        }
        for b in dir_entries(&a) {
            if !is_real_dir(&b) {
                continue;
            }
            if b.file_name().is_some_and(|n| n == "incremental") {
                let _ = fs::remove_dir_all(&b);
                continue;
            }
            for c in dir_entries(&b) {
                if is_real_dir(&c) && c.file_name().is_some_and(|n| n == "incremental") {
                    let _ = fs::remove_dir_all(&c);
                }
            }
        }
    }
    for d in [".susi-guard", ".susi-strip"] {
        remove_any(&to.join(d));
    }
    for f in [SEED_MARK, SEEDED_MARK] {
        let _ = fs::remove_file(to.join(f));
    }
}

/// What the checkout's `Cargo.lock` builds from a registry or a git source
/// (`keep`) and from a path (`deny`, with the workspace members `cargo
/// metadata` names, which a stale lockfile may lack). `None` when there is no
/// lockfile or it names no package.
fn unit_lists(checkout: &Path) -> Option<(HashSet<String>, HashSet<String>)> {
    let lock = fs::read_to_string(checkout.join("Cargo.lock")).ok()?;
    let mut keep = HashSet::new();
    let mut deny = HashSet::new();
    let mut in_pkg = false;
    let mut name: Option<String> = None;
    let mut sourced = false;
    let mut n = 0usize;
    let mut flush = |name: &mut Option<String>, sourced: &mut bool, in_pkg: bool| {
        if let (true, Some(n_)) = (in_pkg, name.take()) {
            n += 1;
            if *sourced {
                keep.insert(n_);
            } else {
                deny.insert(n_);
            }
        }
        *sourced = false;
    };
    for line in lock.lines() {
        let line = line.trim_end_matches('\r');
        if line.starts_with("[[package]]") {
            flush(&mut name, &mut sourced, in_pkg);
            in_pkg = true;
        } else if line.starts_with('[') {
            flush(&mut name, &mut sourced, in_pkg);
            in_pkg = false;
        } else if in_pkg {
            if let Some(rest) = line.strip_prefix("name = \"") {
                name = Some(rest.split('"').next().unwrap_or("").to_string());
            } else if line.starts_with("source = ") {
                sourced = true;
            }
        }
    }
    flush(&mut name, &mut sourced, in_pkg);
    if n == 0 {
        return None;
    }
    if checkout.join("Cargo.toml").is_file() {
        if let Ok(out) = cargo_command()
            .args([
                "metadata",
                "--format-version",
                "1",
                "--no-deps",
                "--offline",
            ])
            .current_dir(checkout)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
        {
            if out.status.success() {
                if let Ok(json) = serde_json::from_slice::<serde_json::Value>(&out.stdout) {
                    for pkg in json["packages"].as_array().into_iter().flatten() {
                        if let Some(n_) = pkg["name"].as_str() {
                            deny.insert(n_.to_string());
                        }
                    }
                }
            }
        }
    }
    Some((keep, deny))
}

/// A fingerprint directory's name, `<package>-<16 hex>`, split.
fn unit_name(entry: &str) -> Option<(&str, &str)> {
    let n = entry.len();
    if n <= 17 || entry.as_bytes()[n - 17] != b'-' {
        return None;
    }
    let hash = &entry[n - 16..];
    hash.bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        .then(|| (&entry[..n - 17], hash))
}

/// Whether an entry of `deps/` or `build/` belongs to a unit whose hash is in
/// `kept`: each unit's files there carry its hash (`libserde-<hash>.rlib`,
/// `serde-<hash>.d`, `serde-<hash>/`). What carries no hash at all is the
/// workspace's (on Windows cargo writes a binary's executable, pdb and
/// dep-info into `deps/` without one), and a stale one would pass for this
/// branch's.
fn is_kept_entry(entry: &str, kept: &HashSet<String>) -> bool {
    let last = entry.rsplit('-').next().unwrap_or("");
    let seg = last.split('.').next().unwrap_or("");
    seg.len() == 16 && kept.contains(seg)
}

fn entry_names(dir: &Path) -> Option<Vec<String>> {
    let rd = fs::read_dir(dir).ok()?;
    Some(
        rd.filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect(),
    )
}

/// A build tree [`strip_units`] could not strip all the way, which must not
/// become a target: it could not tell what to keep, could not list or empty a
/// profile, holds a link where cargo keeps builds, or something stripped is
/// still there after.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unstripped;

impl std::fmt::Display for Unstripped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the build tree could not be stripped to registry and git units")
    }
}

impl std::error::Error for Unstripped {}

/// Strip the build tree `target` down to the units built from a registry or
/// git source of the checkout's `Cargo.lock`: every other unit's fingerprint;
/// everything in `deps/` and `build/` but the files of the units that stay
/// (found by their hash); and what cargo copies up into a profile for the
/// workspace (binaries, libraries, examples), plus `incremental/` and `tmp/`.
/// Returns how many units went. [`Unstripped`], and the tree must not be
/// used, when it cannot tell what to keep, cannot list or empty a profile, or
/// anything stripped is still there after.
///
/// Nothing is deleted through a link: every directory it deletes in is
/// checked, each directory from the tree down to it ([`through_link`]), and a
/// tree with a link where cargo keeps builds ([`holds_foreign_link`]) is
/// refused before anything goes.
pub fn strip_units(target: &Path, checkout: &Path) -> Result<usize, Unstripped> {
    if holds_foreign_link(target) {
        return Err(Unstripped);
    }
    let (keep, deny) = unit_lists(checkout).ok_or(Unstripped)?;
    let doomed_of = |names: &[String]| -> Vec<String> {
        names
            .iter()
            .filter(|n| {
                unit_name(n).is_some_and(|(pkg, _)| !keep.contains(pkg) || deny.contains(pkg))
            })
            .cloned()
            .collect()
    };
    let kept_of = |names: &[String]| -> HashSet<String> {
        names
            .iter()
            .filter_map(|n| unit_name(n))
            .filter(|(pkg, _)| keep.contains(*pkg) && !deny.contains(*pkg))
            .map(|(_, h)| h.to_string())
            .collect()
    };
    let mut n = 0usize;
    for fp in fingerprint_dirs(target) {
        let profile = fp.parent().ok_or(Unstripped)?.to_path_buf();
        if through_link(target, &fp) || !fp.is_dir() {
            return Err(Unstripped);
        }
        let listed = entry_names(&fp).ok_or(Unstripped)?;
        let doomed = doomed_of(&listed);
        let kept = kept_of(&listed);
        n += doomed.len();
        let mut remove: Vec<PathBuf> = doomed.iter().map(|d| fp.join(d)).collect();
        for sub in ["deps", "build"] {
            let dir = profile.join(sub);
            if !dir.is_dir() {
                continue;
            }
            if through_link(target, &dir) {
                return Err(Unstripped);
            }
            let names = entry_names(&dir).ok_or(Unstripped)?;
            remove.extend(
                names
                    .iter()
                    .filter(|e| !is_kept_entry(e, &kept))
                    .map(|e| dir.join(e)),
            );
        }
        // Only workspace targets are copied up into the profile.
        remove.push(profile.join("examples"));
        remove.push(profile.join("incremental"));
        for f in dir_entries(&profile) {
            if f.is_file() && !is_link(&f) {
                remove.push(f);
            }
        }
        let mut failed = false;
        for p in &remove {
            if !remove_any(p) {
                failed = true;
            }
        }
        if failed {
            return Err(Unstripped);
        }
        // What is left must be registry and git units only.
        let listed = entry_names(&fp).ok_or(Unstripped)?;
        if !doomed_of(&listed).is_empty() {
            return Err(Unstripped);
        }
        for sub in ["deps", "build"] {
            let dir = profile.join(sub);
            if !dir.is_dir() {
                continue;
            }
            let names = entry_names(&dir).ok_or(Unstripped)?;
            if names.iter().any(|e| !is_kept_entry(e, &kept)) {
                return Err(Unstripped);
            }
        }
    }
    let tmp = target.join("tmp");
    if tmp.is_dir() && !is_link(&tmp) && fs::remove_dir_all(&tmp).is_err() {
        return Err(Unstripped);
    }
    Ok(n)
}

// --- a worktree's own target ----------------------------------------------------

/// `git` in `dir`: trimmed stdout on success.
fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let result = host_command("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    let out = match result {
        Ok(out) => out,
        Err(error) => {
            if REPORT.get().is_some() {
                say(&format!(
                    "could not start Git in {}: {error}",
                    dir.display()
                ));
            }
            return None;
        }
    };
    if !out.status.success() && REPORT.get().is_some() {
        say(&format!("Git failed in {}: {}", dir.display(), out.status));
    }
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Whether the worktree `dir` (git dir `gitdir`) is still there. `git
/// worktree remove` may run while a copy into it does: once it has, nothing
/// is made there.
fn worktree_alive(dir: &Path, gitdir: &Path) -> bool {
    is_there(&dir.join(".git")) && gitdir.is_dir()
}

/// Take back the empty directories a run into the removed worktree `dir` may
/// have left (its `target/` `t`, and `dir` itself: robocopy makes its
/// destination's parents). Only empty ones, only once `dir/.git` is gone,
/// never a link.
fn leave_no_trace(dir: &Path, t: &Path) {
    if is_there(&dir.join(".git")) {
        return;
    }
    if !is_link(t) {
        let _ = fs::remove_dir(t);
    }
    if !is_link(dir) {
        let _ = fs::remove_dir(dir);
    }
}

/// An empty `target/` for the worktree: what every path that copies nothing
/// leaves (cold, and correct). Made only while the worktree is there, and
/// never with its parents.
fn empty_target(t: &Path, dir: &Path, gitdir: &Path) {
    if worktree_alive(dir, gitdir) {
        if !is_there(t) {
            let _ = fs::create_dir(t);
        }
        if !worktree_alive(dir, gitdir) {
            leave_no_trace(dir, t);
        }
    } else {
        leave_no_trace(dir, t);
    }
}

/// In the primary checkout, which is never seeded: say so when worktrees
/// still link its `target/` (made before the seeding; each gets its own at
/// its next merge of origin/main, sync, finish gate or push). They build into
/// it, and cargo's lock keeps those builds and this checkout's intact, not
/// apart: a crate one of them built last can read as fresh to a cargo run
/// here, and run in place of this checkout's. Nothing is changed.
fn note_shared_primary(primary: &Path) {
    let target = primary.join("target");
    if !target.is_dir() || is_link(&target) {
        return;
    }
    let Some(shared) = canon_dir(&target) else {
        return;
    };
    let Some(list) = git(primary, &["worktree", "list", "--porcelain"]) else {
        return;
    };
    let mut n = 0usize;
    let mut first = String::new();
    for line in list.lines() {
        let Some(wt) = line.strip_prefix("worktree ") else {
            continue;
        };
        let link = Path::new(wt).join("target");
        if is_link(&link) && canon_dir(&link).as_deref() == Some(shared.as_str()) {
            n += 1;
            if first.is_empty() {
                first = wt.to_string();
            }
        }
    }
    if n == 0 {
        return;
    }
    if n >= 2 {
        first.push_str(", ...");
    }
    say(&format!(
        "{}/target is still the shared target of {n} worktree(s) made before the seeding ({first}) \
         until each next merges origin/main: a cargo here can run a crate one of them built in \
         place of this checkout's (cargo's lock keeps the builds intact, not apart). Here, use an \
         installed susi, or skip `workflow check`: scripts/susi-worktree.sh runs no cargo, and \
         cargo is correct in the worktree it makes",
        slashes(primary)
    ));
}

/// The susi that runs the background copy: this executable, which has the
/// verb (it is running it), else `SUSI_WORKFLOW_BIN`. Not the other way
/// round: the stub passes over a `SUSI_WORKFLOW_BIN` naming a release older
/// than the verb and execs a checkout's own build instead, and that
/// environment reaches here unchanged; spawning the old release would
/// succeed, report the copy under way, and leave the target cold.
fn self_program() -> Option<PathBuf> {
    std::env::current_exe().ok().or_else(|| {
        std::env::var_os("SUSI_WORKFLOW_BIN")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    })
}

/// Start the copy into `top` in a process of its own, detached from this one
/// and holding none of its descriptors, with its output added to the log in
/// the worktree's git dir. It runs from the temp directory, not from the
/// worktree: on Windows a process's current directory cannot be removed, and
/// `git worktree remove` must be able to remove the worktree while its copy
/// runs. `CARGO_TARGET_DIR`, if set, names `top/target` ([`builds_elsewhere`]
/// said so): the copy needs neither it nor `CARGO_BUILD_TARGET_DIR`, and a
/// relative one would read as another directory from elsewhere.
fn spawn_background(top: &Path, log_path: &Path) -> bool {
    let Some(program) = self_program() else {
        return false;
    };
    #[cfg(windows)]
    {
        let environment: Vec<_> = std::env::vars_os()
            .filter(|(key, _)| {
                ![
                    "CARGO_TARGET_DIR",
                    "CARGO_BUILD_TARGET_DIR",
                    "CARGO_BUILD_BUILD_DIR",
                ]
                .iter()
                .any(|removed| key.eq_ignore_ascii_case(removed))
            })
            .collect();
        let args = [
            "workflow".into(),
            "seed-target".into(),
            "--log".into(),
            log_path.as_os_str().to_owned(),
            top.as_os_str().to_owned(),
        ];
        winapi::detached::spawn(&program, &args, &std::env::temp_dir(), &environment).is_ok()
    }
    #[cfg(not(windows))]
    {
        let Ok(log) = fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(log_path)
        else {
            return false;
        };
        let Ok(err) = log.try_clone() else {
            return false;
        };
        let mut cmd = host_command(&program);
        cmd.args(["workflow", "seed-target"])
            .arg(top)
            .current_dir(std::env::temp_dir())
            .env_remove("CARGO_TARGET_DIR")
            .env_remove("CARGO_BUILD_TARGET_DIR")
            .env_remove("CARGO_BUILD_BUILD_DIR")
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(err));
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        cmd.spawn().is_ok()
    }
}
/// Seed the worktree at `dir` (any directory inside it; `None` for the
/// current directory): see the module docs. `background` replaces a link now
/// and leaves the copy to a process of its own. Never fails: every outcome is
/// reported on stderr and the caller's cargo goes ahead as it is.
pub fn seed_worktree(dir: Option<&Path>, background: bool) {
    if REPORT.get().is_some() {
        say("native seed worker started");
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let dir = dir.map_or(cwd, Path::to_path_buf);
    if !dir.is_dir() {
        return;
    }
    let Some(top) = git(&dir, &["rev-parse", "--show-toplevel"]) else {
        return;
    };
    let top = PathBuf::from(top);
    let Some(gitdir) = git(&top, &["rev-parse", "--path-format=absolute", "--git-dir"]) else {
        return;
    };
    let Some(common) = git(
        &top,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    ) else {
        return;
    };
    // The primary checkout keeps the target it has: it is nobody's worktree,
    // and older worktrees may still link to it (said, when they do).
    if gitdir == common || same_dir(Path::new(&gitdir), Path::new(&common)) {
        note_shared_primary(&top);
        return;
    }
    let gitdir = PathBuf::from(gitdir);
    let primary = Path::new(&common)
        .parent()
        .map_or_else(|| top.clone(), Path::to_path_buf);
    let t = top.join("target");

    if is_link(&t) {
        if remove_link(&t) {
            say(&format!(
                "replaced the link to a shared target with this worktree's own ({})",
                slashes(&t)
            ));
        } else {
            say(&format!(
                "could not remove the link at {}; it still points at a shared target",
                slashes(&t)
            ));
            return;
        }
    }
    if is_there(&t) && !t.is_dir() {
        return; // a file is in the way: not ours to move
    }
    if t.is_dir() && !holds_nothing(&t) {
        return; // this worktree's own build
    }
    if no_seed() {
        empty_target(&t, &top, &gitdir);
        return;
    }
    // Cargo here builds elsewhere: a copy would be read by no cargo this
    // caller runs, and none is waited for. The link above is still replaced.
    if let Some((elsewhere, var)) = builds_elsewhere(&t) {
        empty_target(&t, &top, &gitdir);
        say(&format!(
            "cargo here builds into {elsewhere} ({var}), not {}: nothing is copied into {}",
            slashes(&t),
            slashes(&t)
        ));
        return;
    }
    let seed = seed_dir(&primary);
    if !usable_seed(&seed, &primary) {
        empty_target(&t, &top, &gitdir);
        if is_there(&seed) {
            say(&format!(
                "{} is no snapshot to copy (not one --refresh published, a checkout, or one whose \
                 profile is a link): this worktree's first build compiles everything",
                slashes(&seed)
            ));
        } else {
            say(&format!(
                "no snapshot at {}: this worktree's first build compiles everything \
                 (susi workflow seed-target --refresh makes one)",
                slashes(&seed)
            ));
        }
        return;
    }
    if !top.join("Cargo.lock").is_file() {
        empty_target(&t, &top, &gitdir);
        say(&format!(
            "no Cargo.lock in {}: nothing in the snapshot can be told safe to reuse",
            slashes(&top)
        ));
        return;
    }
    if background {
        // A real, empty target/ now: what the copy is installed over, and what
        // keeps a tool that links a missing target/ (v0.21.23's `workflow
        // start`, which runs right after the post-checkout hook that calls
        // this) from linking it.
        empty_target(&t, &top, &gitdir);
        let log = gitdir.join(LOG_NAME);
        if spawn_background(&top, &log) {
            say(&format!(
                "copying {} into {} in the background (log: {})",
                slashes(&seed),
                slashes(&t),
                slashes(&log)
            ));
        } else {
            say(&format!(
                "could not start the background copy of {} into {}: this worktree's first build \
                 compiles everything",
                slashes(&seed),
                slashes(&t)
            ));
        }
        return;
    }
    seed_foreground(&top, &gitdir, &t, &seed);
    test_gate("done");
}

/// The copy itself, under the worktree's lock. One copy into a worktree at a
/// time: a foreground caller is about to run cargo, and a cold build costs
/// far more than the copy, so it waits for one that is running.
fn seed_foreground(top: &Path, gitdir: &Path, t: &Path, seed: &Path) {
    let stale = env_secs("SUSI_SEED_LOCK_STALE", 7200);
    let lock = gitdir.join(LOCK_NAME);
    test_gate("lock");
    let mut waited = 0u64;
    let state = loop {
        match take_lock(&lock, stale) {
            Lock::Held => {}
            Lock::Taken => break Lock::Taken,
            Lock::Impossible => break Lock::Impossible,
        }
        if waited == 0 {
            say(&format!(
                "waiting for the copy into {} that is already running",
                slashes(t)
            ));
        }
        // Two minutes, not thirty: a copy still running after that is stuck
        // or huge, and a cold build is cheaper than an agent held idle.
        if waited >= env_secs("SUSI_SEED_WAIT_SECS", 120) {
            say(&format!(
                "gave up waiting after {waited} s; this build goes ahead as it is"
            ));
            return;
        }
        std::thread::sleep(Duration::from_secs(2));
        waited += 2;
    };
    if state != Lock::Taken {
        if worktree_alive(top, gitdir) {
            say(&format!(
                "cannot make the lock {}: no copy, so this worktree's first build compiles everything",
                slashes(&lock)
            ));
        } else {
            leave_no_trace(top, t);
            say(&format!("{} was removed: nothing to seed", slashes(top)));
        }
        return;
    }
    let _held = HeldLock(lock);
    // Seeded, or built, while this waited.
    if is_link(t) || (is_there(t) && (!t.is_dir() || !holds_nothing(t))) {
        return;
    }
    sweep_staging(stale, &named_like(top, "target.seed-"));
    let me = Owner::me();
    let staging = top.join(format!("target.seed-{}-{}.tmp", me.pid, me.since));
    if fs::create_dir(&staging).is_err() || !claim_staging(&staging) {
        discard(&staging);
        say(&format!(
            "cannot make {}: no copy, so this worktree's first build compiles everything",
            slashes(&staging)
        ));
        return;
    }
    let marker = |seed: &Path| -> String {
        first_line(&seed.join(SEED_MARK))
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_string()
    };
    let before = marker(seed);
    let removed = |staging: &Path| {
        discard(staging);
        leave_no_trace(top, t);
        say(&format!(
            "{} was removed while the snapshot was copied into it: nothing is left of the copy",
            slashes(top)
        ));
    };
    let instead = |staging: &Path, why: &str| {
        discard(staging);
        empty_target(t, top, gitdir);
        say(why);
    };
    say(&format!(
        "copying {}{} into {} ...",
        slashes(seed),
        if before.is_empty() {
            String::new()
        } else {
            format!(" (origin/main {})", &before[..before.len().min(12)])
        },
        slashes(t)
    ));
    test_gate("copy");
    let copied = copy_tree(seed, &staging) && claim_staging(&staging);
    // Removed while it was copied (robocopy makes its destination's parents
    // again): nothing of it, or of the copy, is left.
    if !worktree_alive(top, gitdir) {
        removed(&staging);
        return;
    }
    if !copied {
        instead(
            &staging,
            "the copy failed: an empty target instead, and the first build compiles everything",
        );
        return;
    }
    if marker(seed) != before || !seed.is_dir() {
        instead(
            &staging,
            "the snapshot was replaced while it was copied: an empty target instead",
        );
        return;
    }
    let Ok(dropped) = strip_units(&staging, top) else {
        instead(
            &staging,
            "could not strip the units this worktree builds from its own sources out of the copy: \
             an empty target instead (cold, and correct)",
        );
        return;
    };
    let stamp = format!(
        "{} {}\n",
        if before.is_empty() { "-" } else { &before },
        now_secs()
    );
    if fs::write(staging.join(SEEDED_MARK), stamp).is_err() || !still_mine(&staging) {
        instead(
            &staging,
            &format!(
                "the copy at {} changed hands: an empty target instead",
                slashes(&staging)
            ),
        );
        return;
    }
    let _ = fs::remove_file(staging.join(RUN_MARK));
    if !worktree_alive(top, gitdir) {
        removed(&staging);
        return;
    }
    if is_real_dir(t) && holds_nothing(t) {
        let _ = fs::remove_file(t.join("CACHEDIR.TAG"));
        let _ = fs::remove_file(t.join(".rustc_info.json"));
        let _ = fs::remove_dir(t);
    }
    test_gate("rename");
    // Rename onto a path that must not exist: a rename onto a directory that
    // appeared meanwhile would replace an empty one, or fail.
    if is_there(t) || fs::rename(&staging, t).is_err() {
        discard(&staging);
        say(&format!(
            "a build started in {} while the snapshot was copied; it keeps its own",
            slashes(t)
        ));
        return;
    }
    // `git worktree remove` that ran between the check above and the rename:
    // its delete may have listed the worktree's entries before the rename
    // added target/, and then leaves the directory behind, holding a full
    // target. What this run just renamed into place is taken back.
    if !worktree_alive(top, gitdir) {
        removed(t);
        return;
    }
    say(&format!(
        "seeded {}; stripped {dropped} unit(s) this worktree builds from its own sources",
        slashes(t)
    ));
}

// --- the snapshot's one writer ----------------------------------------------------

/// Where `--refresh` reports: stdout, or the `--log` file once a refresh is
/// under way (so the watcher's tick every few minutes, which mostly finds the
/// snapshot current, never empties the record of the last build).
struct Out {
    file: Option<fs::File>,
}

impl Out {
    fn say(&mut self, msg: &str) {
        match &mut self.file {
            Some(f) => {
                let _ = writeln!(f, "seed: {msg}");
            }
            None => println!("seed: {msg}"),
        }
    }

    fn log_to(&mut self, log: &Path) {
        if let Ok(f) = fs::File::create(log) {
            self.file = Some(f);
        }
    }

    /// Where a child's output goes: the log, or this process's own.
    fn stdio(&self) -> (Stdio, Stdio) {
        match &self.file {
            Some(f) => match (f.try_clone(), f.try_clone()) {
                (Ok(a), Ok(b)) => (Stdio::from(a), Stdio::from(b)),
                _ => (Stdio::inherit(), Stdio::inherit()),
            },
            None => (Stdio::inherit(), Stdio::inherit()),
        }
    }
}

/// The git common dir of the checkout at, or the nearest existing directory
/// above, `at`.
fn common_of(at: &Path) -> Option<String> {
    let mut at = at;
    while !at.is_dir() {
        at = at.parent()?;
    }
    git(
        at,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
}

/// Whether `--refresh` may check out, clean and build in `src`: a clone it
/// made (it carries `.git/susi-seed-src`), or nothing yet; never the primary
/// checkout or anything inside a checkout of this clone, whose branch and
/// files belong to whoever works there.
fn src_allowed(src: &Path, common: &Path, primary: &Path) -> Result<(), String> {
    let src_s = slashes(src);
    if same_dir(src, primary) {
        return Err(format!(
            "refusing to build in {src_s}: it is the primary checkout (SUSI_TARGET_SEED_SRC must \
             name a checkout of its own)"
        ));
    }
    if let Some(sc) = common_of(src) {
        if same_dir(Path::new(&sc), common) {
            return Err(format!(
                "refusing to build in {src_s}: it is inside a checkout of this clone \
                 (SUSI_TARGET_SEED_SRC must name a checkout of its own)"
            ));
        }
    }
    if is_there(src) && !src.join(".git").join("susi-seed-src").is_file() {
        return Err(format!(
            "refusing to build in {src_s}: susi did not make it (no .git/susi-seed-src); remove \
             it, or point SUSI_TARGET_SEED_SRC elsewhere"
        ));
    }
    Ok(())
}

/// What every unit's hash in the build checkout depends on: the lockfile, the
/// toolchain file and the compiler.
fn src_inputs(src: &Path) -> String {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    for f in ["Cargo.lock", "rust-toolchain", "rust-toolchain.toml"] {
        if let Ok(bytes) = fs::read(src.join(f)) {
            hasher.update(&bytes);
        }
    }
    if let Ok(out) = host_command("rustc")
        .arg("-vV")
        .current_dir(src)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    {
        hasher.update(&out.stdout);
    }
    hex::encode(hasher.finalize())
}

/// The size of the tree in KiB, links not followed.
fn tree_kib(dir: &Path) -> u64 {
    let mut bytes = 0u64;
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        for e in dir_entries(&d) {
            let Ok(m) = e.symlink_metadata() else {
                continue;
            };
            if m.file_type().is_symlink() {
                continue;
            }
            if m.is_dir() {
                todo.push(e);
            } else {
                bytes += m.len();
            }
        }
    }
    bytes.div_ceil(1024)
}

/// `cargo <args>` in `src` at low priority: `nice -n 19` where there is one,
/// `BELOW_NORMAL_PRIORITY_CLASS` on Windows.
fn low_priority_cargo(src: &Path, args: &[&str], out: &Out) -> bool {
    let mut cmd = cargo_command();
    #[cfg(not(windows))]
    {
        let nice = std::env::var_os("PATH")
            .is_some_and(|path| std::env::split_paths(&path).any(|d| d.join("nice").is_file()));
        if nice {
            let inner = cmd;
            let mut wrapped = host_command("nice");
            wrapped.args(["-n", "19"]).arg(inner.get_program());
            wrapped.args(inner.get_args());
            cmd = wrapped;
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000 | 0x0000_4000); // CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS
    }
    let (stdout, stderr) = out.stdio();
    cmd.args(args)
        .current_dir(src)
        // A refresh owns both final artifacts and Cargo's intermediate files.
        // Inherited gate settings (or user Cargo configuration) must not redirect
        // fingerprints into another worktree while we publish this target tree.
        .env("CARGO_TARGET_DIR", src.join("target"))
        .env("CARGO_BUILD_TARGET_DIR", src.join("target"))
        .env("CARGO_BUILD_BUILD_DIR", src.join("target"))
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        .status()
        .is_ok_and(|s| s.success())
}

/// Rebuild and republish the snapshot: see the module docs. Reports on stdout
/// until a refresh is under way, then in `log` when one is given. Never fails.
pub fn refresh(force: bool, log: Option<&Path>) {
    let mut out = Out { file: None };
    if no_seed() {
        return;
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let Some(common) = git(
        &cwd,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    ) else {
        return;
    };
    let common = PathBuf::from(common);
    let primary = common
        .parent()
        .map_or_else(|| cwd.clone(), Path::to_path_buf);
    let seed = seed_dir(&primary);
    let seed_s = slashes(&seed);
    let src = std::env::var_os("SUSI_TARGET_SEED_SRC")
        .filter(|v| !v.is_empty())
        .map_or_else(
            || PathBuf::from(format!("{seed_s}-src")),
            |v| PathBuf::from(v.to_string_lossy().replace('\\', "/")),
        );
    let src_s = slashes(&src);
    // Only a holder whose liveness cannot be told ages out (holder_alive), so
    // this is far above any build: a day.
    let stale = env_secs("SUSI_SEED_LOCK_STALE", 86_400);
    // The lock's parent: beside the primary checkout by default, but
    // SUSI_TARGET_SEED may name a directory not made yet.
    if let Some(parent) = seed.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let lock = PathBuf::from(format!("{seed_s}.lock"));
    let _held = match take_lock(&lock, stale) {
        Lock::Taken => HeldLock(lock),
        Lock::Held => {
            out.say(&format!(
                "a refresh is already running ({})",
                slashes(&lock)
            ));
            return;
        }
        Lock::Impossible => {
            out.say(&format!(
                "cannot make the lock {}; not refreshing",
                slashes(&lock)
            ));
            return;
        }
    };

    let Some(want) = host_command("git")
        .arg(format!("--git-dir={}", common.display()))
        .args([
            "rev-parse",
            "-q",
            "--verify",
            "refs/remotes/origin/main^{commit}",
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
    else {
        out.say("no origin/main to build");
        return;
    };
    let mark = first_line(&seed.join(SEED_MARK));
    let mut mark_fields = mark.split_whitespace();
    let have = mark_fields.next().unwrap_or("").to_string();
    let since: u64 = mark_fields.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    if have == want {
        out.say(&format!("already origin/main ({want})"));
        return;
    }
    let now = now_secs();
    let refresh_secs = env_secs("SUSI_SEED_REFRESH_SECS", 3600);
    if !force && !have.is_empty() && now.saturating_sub(since) < refresh_secs {
        out.say(&format!(
            "{} s old; refreshed after {refresh_secs} s (--force refreshes now)",
            now.saturating_sub(since)
        ));
        return;
    }
    // From here on a refresh runs, or says why it cannot: only then is the log
    // written.
    if let Some(log) = log {
        out.log_to(log);
    }
    let have_or_nothing = if have.is_empty() {
        "nothing".to_string()
    } else {
        have.clone()
    };
    out.say(&format!(
        "{} refreshing {seed_s} ({have_or_nothing} -> origin/main {want})",
        timestamp()
    ));
    // Only a snapshot a refresh made is ever replaced.
    if is_link(&seed) || (is_there(&seed) && !seed.join(SEED_MARK).is_file()) {
        out.say(&format!(
            "{seed_s} is not a snapshot a refresh made; leaving it (point SUSI_TARGET_SEED elsewhere)"
        ));
        return;
    }
    if let Err(why) = src_allowed(&src, &common, &primary) {
        out.say(&why);
        return;
    }
    let me = Owner::me();
    let siblings = |prefix: &str| -> Vec<PathBuf> {
        let Some(parent) = seed.parent() else {
            return Vec::new();
        };
        let base = seed
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        named_like(parent, &format!("{base}{prefix}"))
    };
    let mut stale_dirs = siblings(".new.");
    stale_dirs.extend(siblings(".old."));
    sweep_staging(stale, &stale_dirs);

    // A clean checkout of origin/main of its own, marked as susi's.
    if !is_there(&src) {
        let tmp = PathBuf::from(format!("{src_s}.clone-{}-{}.tmp", me.pid, me.since));
        let cloned = host_command("git")
            .args(["clone", "-q", "--no-checkout"])
            .arg(&common)
            .arg(&tmp)
            .stdin(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
            && fs::write(
                tmp.join(".git").join("susi-seed-src"),
                format!(
                    "made by susi workflow seed-target --refresh, {}: its checkout and target are \
                     rebuilt at will\n",
                    timestamp()
                ),
            )
            .is_ok()
            && fs::rename(&tmp, &src).is_ok();
        if !cloned {
            discard(&tmp);
            out.say(&format!("cannot clone {} into {src_s}", slashes(&common)));
            return;
        }
    }
    if !src.join(".git").join("susi-seed-src").is_file() {
        return;
    }
    let common_s = common.to_string_lossy();
    if git(
        &src,
        &[
            "fetch",
            "-q",
            "--no-tags",
            common_s.as_ref(),
            "+refs/remotes/origin/main:refs/remotes/seed/main",
        ],
    )
    .is_none()
    {
        out.say(&format!("cannot fetch origin/main into {src_s}"));
        return;
    }
    let Some(got) = git(
        &src,
        &[
            "rev-parse",
            "-q",
            "--verify",
            "refs/remotes/seed/main^{commit}",
        ],
    )
    .filter(|s| !s.is_empty()) else {
        return;
    };
    // Everything but the build.
    if git(
        &src,
        &[
            "-c",
            "advice.detachedHead=false",
            "checkout",
            "-q",
            "--force",
            "--detach",
            got.as_str(),
        ],
    )
    .is_none()
        || git(&src, &["clean", "-q", "-fdx", "-e", "target"]).is_none()
    {
        out.say(&format!("cannot check out {got} in {src_s}"));
        return;
    }

    // The build checkout's target only grows: cargo never removes a unit whose
    // hash went out of use. It starts over when what every hash depends on
    // changes, or once it is twice its size after its last fresh build.
    let state = src.join(".git").join("susi-seed-src.state");
    let recorded = first_line(&state);
    let mut rec = recorded.split_whitespace();
    let rec_inputs = rec.next().unwrap_or("").to_string();
    let mut rec_fresh: u64 = rec.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let rec_last: u64 = rec.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let inputs = src_inputs(&src);
    let src_target = src.join("target");
    let fresh = if !src_target.is_dir() || is_link(&src_target) {
        true
    } else if inputs != rec_inputs {
        out.say(&format!(
            "Cargo.lock, the toolchain or rustc changed; starting {src_s}/target over"
        ));
        true
    } else if rec_fresh > 0 && rec_last > 2 * rec_fresh {
        out.say(&format!(
            "{src_s}/target grew to {rec_last} KiB, over twice its fresh {rec_fresh} KiB; starting it over"
        ));
        true
    } else {
        false
    };
    if fresh {
        discard(&src_target);
    }

    out.say(&format!("building {got} in {src_s}"));
    if !low_priority_cargo(
        &src,
        &["build", "--workspace", "--all-targets", "--locked"],
        &out,
    ) {
        out.say(&format!(
            "the build of {got} failed; the snapshot stays at {have_or_nothing}"
        ));
        return;
    }
    if !low_priority_cargo(
        &src,
        &["clippy", "--workspace", "--all-targets", "--locked"],
        &out,
    ) {
        out.say(&format!(
            "clippy did not pass on {got}; publishing the build anyway"
        ));
    }
    let size = tree_kib(&src_target);
    if fresh || rec_fresh == 0 {
        rec_fresh = size;
    }
    let inputs_or_dash = if inputs.is_empty() { "-" } else { &inputs };
    let _ = fs::write(&state, format!("{inputs_or_dash} {rec_fresh} {size}\n"));

    // A staging directory of this run's own; another run's is never touched.
    let staging = PathBuf::from(format!("{seed_s}.new.{}-{}", me.pid, me.since));
    if fs::create_dir(&staging).is_err() || !claim_staging(&staging) {
        discard(&staging);
        out.say(&format!("cannot make {}", slashes(&staging)));
        return;
    }
    out.say(&format!("copying {src_s}/target to {}", slashes(&staging)));
    if !copy_tree(&src_target, &staging) || !claim_staging(&staging) {
        discard(&staging);
        out.say("the copy failed");
        return;
    }
    let Ok(stripped) = strip_units(&staging, &src) else {
        discard(&staging);
        out.say(&format!(
            "could not strip the workspace's own units from {}; the snapshot stays at {have_or_nothing}",
            slashes(&staging)
        ));
        return;
    };
    out.say(&format!(
        "stripped {stripped} unit(s) built from a path; the snapshot keeps registry and git crates only"
    ));
    let stamp = format!("{got} {}", now_secs());
    let marked = fs::write(staging.join(SEED_MARK), format!("{stamp}\n")).is_ok()
        && first_line(&staging.join(SEED_MARK)) == stamp
        && still_mine(&staging);
    if !marked {
        discard(&staging);
        out.say(&format!(
            "{} did not keep its marker, or is no longer this run's; nothing published",
            slashes(&staging)
        ));
        return;
    }

    // Publish: the old snapshot out of the way, the new one in. A worktree
    // copying the old one holds it open on Windows; then this run's copy is
    // dropped and the next refresh tries again.
    let mut old: Option<PathBuf> = None;
    if seed.is_dir() {
        let aside = PathBuf::from(format!("{seed_s}.old.{}-{}", me.pid, me.since));
        if fs::rename(&seed, &aside).is_err() {
            discard(&staging);
            out.say(&format!("{seed_s} is in use; the next refresh publishes"));
            return;
        }
        claim_staging(&aside);
        old = Some(aside);
    }
    if fs::rename(&staging, &seed).is_err() {
        if let Some(aside) = &old {
            let _ = fs::rename(aside, &seed);
        }
        discard(&staging);
        out.say(&format!("could not move {} into place", slashes(&staging)));
        return;
    }
    if let Some(aside) = &old {
        discard(aside);
    }
    out.say(&format!("published {got} at {seed_s}"));
}

/// Export this checkout's `target/` as a snapshot: a copy at `dest`, stripped
/// to the registry and git units exactly as the refresh strips the primary's
/// (`strip_units`), without `incremental/`, and marked as a snapshot
/// (`.susi-seed`: the checkout's head and the time). The cloud target seed
/// workflow (`.github/workflows/target-seed.yml`) builds `origin/main` the way
/// a cloud session does and publishes this; `scripts/cloud-target-seed.sh`
/// unpacks it into a container's empty `target/`. `dest` must not exist yet.
/// Returns the number of units stripped and the head recorded.
pub fn export(checkout: &Path, dest: &Path) -> Result<(usize, String), String> {
    let target = checkout.join("target");
    if !is_real_dir(&target) || holds_nothing(&target) {
        return Err(format!("{} holds no build to export", slashes(&target)));
    }
    if is_there(dest) {
        return Err(format!("{} exists already", slashes(dest)));
    }
    if fs::create_dir_all(dest).is_err() {
        return Err(format!("cannot make {}", slashes(dest)));
    }
    if !copy_tree(&target, dest) {
        discard(dest);
        return Err(format!("the copy to {} failed", slashes(dest)));
    }
    drop_extras(dest);
    let stripped = match strip_units(dest, checkout) {
        Ok(n) => n,
        Err(Unstripped) => {
            discard(dest);
            return Err(format!(
                "could not strip the workspace's own units from {}",
                slashes(dest)
            ));
        }
    };
    let head = git(checkout, &["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".to_string());
    let stamp = format!("{head} {}", now_secs());
    if fs::write(dest.join(SEED_MARK), format!("{stamp}\n")).is_err() {
        discard(dest);
        return Err(format!("cannot mark {} as a snapshot", slashes(dest)));
    }
    Ok((stripped, head))
}

#[cfg(test)]
mod seed_tests {
    use super::*;

    /// The export is the refresh's snapshot made from this checkout's own
    /// target: the workspace's units are gone, the registry's stay, and the
    /// marker names the head.
    #[test]
    fn an_export_keeps_registry_units_only_and_marks_the_snapshot() {
        let root = std::env::temp_dir().join(format!(
            "susi-seed-export-{}-{}",
            std::process::id(),
            now_secs()
        ));
        let _ = fs::remove_dir_all(&root);
        let checkout = root.join("co");
        fs::create_dir_all(checkout.join("target/debug/.fingerprint")).unwrap();
        fs::create_dir_all(checkout.join("target/debug/deps")).unwrap();
        fs::create_dir_all(checkout.join("target/debug/incremental/app-xyz")).unwrap();
        fs::write(
            checkout.join("Cargo.lock"),
            "version = 4\n\n[[package]]\nname = \"app\"\nversion = \"0.1.0\"\n\n[[package]]\nname = \"serde\"\nversion = \"1.0.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n",
        )
        .unwrap();
        for unit in ["serde-0123456789abcdef", "app-fedcba9876543210"] {
            fs::create_dir_all(checkout.join("target/debug/.fingerprint").join(unit)).unwrap();
            fs::write(
                checkout
                    .join("target/debug/deps")
                    .join(format!("lib{unit}.rlib")),
                b"x",
            )
            .unwrap();
        }
        fs::write(checkout.join("target/debug/app"), b"bin").unwrap();
        assert!(Command::new("git")
            .args(["init", "-q"])
            .current_dir(&checkout)
            .status()
            .is_ok_and(|s| s.success()));
        let dest = root.join("snapshot");
        let (stripped, _head) = export(&checkout, &dest).unwrap();
        assert_eq!(stripped, 1, "the path unit `app` is stripped");
        assert!(dest
            .join("debug/.fingerprint/serde-0123456789abcdef")
            .is_dir());
        assert!(!dest
            .join("debug/.fingerprint/app-fedcba9876543210")
            .exists());
        assert!(dest
            .join("debug/deps/libserde-0123456789abcdef.rlib")
            .is_file());
        assert!(!dest
            .join("debug/deps/libapp-fedcba9876543210.rlib")
            .exists());
        assert!(
            !dest.join("debug/app").exists(),
            "workspace binaries are not shipped"
        );
        assert!(!dest.join("debug/incremental").exists());
        assert!(dest.join(SEED_MARK).is_file());
        // The source target is untouched.
        assert!(checkout.join("target/debug/app").is_file());
        assert!(
            export(&checkout, &dest).is_err(),
            "an existing dest is refused"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn an_owner_line_is_split_like_read_r_with_the_runtime_keeping_its_spaces() {
        let o = Owner::parse("4242 1700000000 - C:/Program Files/Another Git").unwrap();
        assert_eq!(o.pid, 4242);
        assert_eq!(o.since, 1_700_000_000);
        assert_eq!(o.wpid, None);
        assert_eq!(o.runtime, "C:/Program Files/Another Git");
        let o = Owner::parse("7 8 9 linux-gnu").unwrap();
        assert_eq!(o.wpid, Some(9));
        assert!(Owner::parse("").is_none());
        assert!(Owner::parse("abc 1 - x").is_none());
        let me = Owner::me();
        assert_eq!(Owner::parse(&me.line()), Some(me.clone()));
        assert!(me.is_me());
    }

    #[test]
    fn another_git_installs_line_is_another_table_and_ages_out() {
        let other = Owner {
            pid: 4242,
            since: now_secs(),
            wpid: None,
            runtime: "C:/Program Files/Another Git".into(),
        };
        assert!(!same_table(&other.runtime));
        assert!(holder_alive(&other, 3600), "young: presumed alive");
        let old = Owner {
            since: now_secs() - 30_000,
            ..other
        };
        assert!(!holder_alive(&old, 3600), "stale: gone");
        // This process is alive by its pid, however old its line.
        let mine = Owner {
            since: 1,
            ..Owner::me()
        };
        assert!(holder_alive(&mine, 60));
    }

    #[test]
    fn unit_names_and_hashed_entries_are_told_apart() {
        assert_eq!(
            unit_name("cell-build-2000000000000003"),
            Some(("cell-build", "2000000000000003"))
        );
        assert_eq!(unit_name("serde-200000000000000g"), None);
        assert_eq!(unit_name("short-1"), None);
        let kept: HashSet<String> = ["2000000000000001".to_string()].into_iter().collect();
        assert!(is_kept_entry("libserde-2000000000000001.rlib", &kept));
        assert!(is_kept_entry("serde-2000000000000001.d", &kept));
        assert!(is_kept_entry("serde-2000000000000001", &kept));
        assert!(!is_kept_entry("libcell-1000000000000001.rlib", &kept));
        assert!(!is_kept_entry("fixture-root.exe", &kept));
        assert!(!is_kept_entry("fixture-root.pdb", &kept));
    }

    #[test]
    fn a_lockfile_names_what_is_kept_and_what_is_denied() {
        let dir = std::env::temp_dir().join(format!("susi-seed-units-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("Cargo.lock"),
            "version = 4\n\n[[package]]\nname = \"cell\"\nversion = \"0.0.0\"\n\n\
             [[package]]\nname = \"serde\"\nversion = \"1.0.0\"\n\
             source = \"registry+https://github.com/rust-lang/crates.io-index\"\n\n\
             [metadata]\nx = 1\n",
        )
        .unwrap();
        let (keep, deny) = unit_lists(&dir).unwrap();
        assert!(keep.contains("serde"));
        assert!(deny.contains("cell"));
        assert!(!keep.contains("cell"));
        fs::write(dir.join("Cargo.lock"), "# truncated\n").unwrap();
        assert!(unit_lists(&dir).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_timestamp_is_civil_utc() {
        let t = timestamp();
        assert_eq!(t.len(), 19, "{t}");
        assert_eq!(&t[4..5], "-");
        assert_eq!(&t[10..11], " ");
    }

    #[test]
    fn through_link_refuses_what_is_not_under_the_root() {
        let root = Path::new("/a/b");
        assert!(through_link(root, Path::new("/a/c/x")));
        assert!(through_link(root, root));
        assert!(!through_link(root, Path::new("/a/b/nonexistent/deeper")));
    }
}
