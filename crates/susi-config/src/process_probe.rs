//! Liveness and memory footprint of a process named only by its pid: the
//! holder recorded in a lockfile, a supervised leaf service.
//!
//! Linux answers from `/proc`. Windows answers from the process object
//! itself (`OpenProcess`, then `GetExitCodeProcess` /
//! `K32GetProcessMemoryInfo`); that FFI is the only `unsafe` here, wrapped in
//! an owned handle so callers stay safe code. macOS answers through the
//! vendored `sysinfo` facade. Elsewhere the answer is unknown (`None`) and
//! callers keep their own conservative default.
//!
//! Process enumeration and identity ([`processes`], [`exe_path`], [`argv`],
//! [`environment_var`]) work on every supported OS through `sysinfo`, except
//! that Linux keeps the kernel's own `/proc/<pid>/exe` and
//! `/proc/<pid>/environ` records for identity: the raw exe link carries the
//! ` (deleted)` marker of a replaced binary, which `sysinfo` strips, and trust
//! checks must see it.

/// Whether `pid` names a running process: `Some(true)` running, `Some(false)`
/// gone, `None` when this platform cannot tell.
///
/// Linux: `/proc/<pid>` exists. Windows: the process object opens and has not
/// exited; a pid that names no process is gone, and one that exists but is not
/// ours to query (another session, a protected process) is running.
pub fn pid_alive(pid: u32) -> Option<bool> {
    #[cfg(target_os = "linux")]
    {
        Some(std::path::Path::new(&format!("/proc/{pid}")).exists())
    }
    #[cfg(windows)]
    {
        windows::pid_alive(pid)
    }
    #[cfg(target_os = "macos")]
    {
        Some(with_process(pid, sysinfo::ProcessRefreshKind::nothing(), |_| ()).is_some())
    }
    #[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
    {
        let _ = pid;
        None
    }
}

/// Resident memory of a running `pid` in KiB: `VmRSS` from
/// `/proc/<pid>/status` on Linux, the working set on Windows. `None` when the
/// process is gone, cannot be read, or this platform cannot tell.
pub fn rss_kb(pid: u32) -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let text = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
        text.lines()
            .find_map(|l| l.strip_prefix("VmRSS:"))
            .and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok())
    }
    #[cfg(windows)]
    {
        windows::rss_kb(pid)
    }
    #[cfg(target_os = "macos")]
    {
        with_process(
            pid,
            sysinfo::ProcessRefreshKind::nothing().with_memory(),
            |p| p.memory() / 1024,
        )
        .filter(|kb| *kb > 0)
    }
    #[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
    {
        let _ = pid;
        None
    }
}

/// One process as the OS reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessSnapshot {
    /// Process id.
    pub pid: u32,
    /// Short process name (`comm` on Linux, the image name on Windows).
    pub name: String,
    /// Executable path, when the OS lets this process read it.
    pub exe: Option<std::path::PathBuf>,
    /// Command line, when requested and readable (empty otherwise).
    pub cmd: Vec<String>,
    /// Resident memory in KiB (0 when unreadable).
    pub rss_kb: u64,
}

fn snapshot_of(pid: u32, process: &sysinfo::Process) -> ProcessSnapshot {
    ProcessSnapshot {
        pid,
        name: process.name().to_string_lossy().into_owned(),
        exe: process.exe().map(std::path::Path::to_path_buf),
        cmd: process
            .cmd()
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect(),
        rss_kb: process.memory() / 1024,
    }
}

/// Refresh just `pid` with `kind` and read it through `read`; `None` when
/// no such process exists.
fn with_process<T>(
    pid: u32,
    kind: sysinfo::ProcessRefreshKind,
    read: impl FnOnce(&sysinfo::Process) -> T,
) -> Option<T> {
    let target = sysinfo::Pid::from_u32(pid);
    let mut system = sysinfo::System::new();
    system.refresh_processes_specifics(sysinfo::ProcessesToUpdate::Some(&[target]), true, kind);
    system.process(target).map(read)
}

/// Every process this OS lets us see, with name, exe and resident memory.
/// `with_cmd` also reads each command line, which costs one extra read per
/// process (a remote memory read on Windows), so listings that only need
/// names leave it off. Empty on targets `sysinfo` does not support.
/// Processes only: on Linux `sysinfo` would otherwise list every thread
/// (`/proc/<pid>/task/<tid>`) as its own row, so tasks are not walked and any
/// thread row that still appears is dropped.
pub fn processes(with_cmd: bool) -> Vec<ProcessSnapshot> {
    let mut kind = sysinfo::ProcessRefreshKind::nothing()
        .without_tasks()
        .with_memory()
        .with_exe(sysinfo::UpdateKind::Always);
    if with_cmd {
        kind = kind.with_cmd(sysinfo::UpdateKind::Always);
    }
    let mut system = sysinfo::System::new();
    system.refresh_processes_specifics(sysinfo::ProcessesToUpdate::All, true, kind);
    let mut rows: Vec<ProcessSnapshot> = system
        .processes()
        .iter()
        .filter(|(_, process)| process.thread_kind().is_none())
        .map(|(pid, process)| snapshot_of(pid.as_u32(), process))
        .collect();
    rows.sort_by_key(|row| row.pid);
    rows
}

/// The executable `pid` runs, as the OS records it. Linux returns the raw
/// `/proc/<pid>/exe` link, ` (deleted)` marker included; other platforms
/// ask `sysinfo`. `None` when the process is gone or not ours to inspect.
pub fn exe_path(pid: u32) -> Option<std::path::PathBuf> {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_link(format!("/proc/{pid}/exe")).ok()
    }
    #[cfg(not(target_os = "linux"))]
    {
        with_process(
            pid,
            sysinfo::ProcessRefreshKind::nothing().with_exe(sysinfo::UpdateKind::Always),
            |p| p.exe().map(std::path::Path::to_path_buf),
        )
        .flatten()
    }
}

/// The command line of `pid`; `None` when the process is gone or its
/// arguments are unreadable (an empty argv reads as unreadable).
pub fn argv(pid: u32) -> Option<Vec<String>> {
    with_process(
        pid,
        sysinfo::ProcessRefreshKind::nothing().with_cmd(sysinfo::UpdateKind::Always),
        |p| {
            p.cmd()
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        },
    )
    .filter(|args| !args.is_empty())
}

/// The value of environment variable `key` in process `pid`:
/// `Some(Some(value))` set, `Some(None)` readable but unset, `None` when the
/// environment cannot be read (gone, another user's, protected). A real
/// process always carries some environment, so an empty one reads as
/// unreadable rather than as "unset".
///
/// Linux reads `/proc/<pid>/environ`; other platforms ask `sysinfo`, which
/// copies the environment block out of the process's memory. Meant for
/// other processes: a copy of the calling process's own block can come back
/// unreadable or torn while one of its threads runs `std::env::set_var`
/// (and on Linux it shows the environment at exec, not later changes), so
/// read your own environment with `std::env::var_os`.
pub fn environment_var(pid: u32, key: &str) -> Option<Option<std::ffi::OsString>> {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStrExt;
        let environ = std::fs::read(format!("/proc/{pid}/environ")).ok()?;
        if environ.is_empty() {
            return None;
        }
        let mut prefix = key.as_bytes().to_vec();
        prefix.push(b'=');
        Some(environ.split(|b| *b == 0).find_map(|kv| {
            kv.strip_prefix(prefix.as_slice())
                .map(|v| std::ffi::OsStr::from_bytes(v).to_os_string())
        }))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let entries = with_process(
            pid,
            sysinfo::ProcessRefreshKind::nothing().with_environ(sysinfo::UpdateKind::Always),
            |p| p.environ().to_vec(),
        )?;
        if entries.is_empty() {
            return None;
        }
        let prefix = format!("{key}=");
        Some(entries.iter().find_map(|kv| {
            let kv = kv.to_string_lossy();
            // Windows variable names are case-insensitive.
            let head = kv.get(..prefix.len())?;
            let matches = if cfg!(windows) {
                head.eq_ignore_ascii_case(&prefix)
            } else {
                head == prefix
            };
            matches.then(|| std::ffi::OsString::from(kv.get(prefix.len()..).unwrap_or_default()))
        }))
    }
}

#[cfg(windows)]
mod windows {
    use winapi::shared::minwindef::{DWORD, FALSE};
    use winapi::shared::winerror::{ERROR_ACCESS_DENIED, ERROR_INVALID_PARAMETER};
    use winapi::um::errhandlingapi::GetLastError;
    use winapi::um::handleapi::CloseHandle;
    use winapi::um::minwinbase::STILL_ACTIVE;
    use winapi::um::processthreadsapi::{GetExitCodeProcess, OpenProcess};
    use winapi::um::psapi::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
    use winapi::um::winnt::{HANDLE, PROCESS_QUERY_LIMITED_INFORMATION};

    /// A process handle opened for limited query access, closed on drop.
    struct Process(HANDLE);

    impl Process {
        /// Open `pid`, or the Win32 error that refused it.
        #[allow(unsafe_code)]
        fn open(pid: u32) -> Result<Self, DWORD> {
            // SAFETY: OpenProcess takes only integer arguments and returns a
            // new handle that this value then owns, or NULL on failure.
            let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid) };
            if handle.is_null() {
                // SAFETY: GetLastError reads this thread's last-error value
                // and takes no arguments.
                Err(unsafe { GetLastError() })
            } else {
                Ok(Self(handle))
            }
        }

        /// Whether the process is still running. A process that exited with
        /// code 259 (`STILL_ACTIVE`) reads as running: the conservative side,
        /// since a false "gone" would let a lock be stolen from a live holder.
        #[allow(unsafe_code)]
        fn running(&self) -> Option<bool> {
            let mut code: DWORD = 0;
            // SAFETY: `self.0` is a live handle with
            // PROCESS_QUERY_LIMITED_INFORMATION access, owned by `self`; `code`
            // is a writable DWORD that outlives the call.
            let ok = unsafe { GetExitCodeProcess(self.0, &mut code) } != 0;
            ok.then_some(code == STILL_ACTIVE)
        }

        /// Working-set size in bytes.
        #[allow(unsafe_code)]
        fn working_set_bytes(&self) -> Option<usize> {
            let mut counters = PROCESS_MEMORY_COUNTERS {
                cb: 0,
                PageFaultCount: 0,
                PeakWorkingSetSize: 0,
                WorkingSetSize: 0,
                QuotaPeakPagedPoolUsage: 0,
                QuotaPagedPoolUsage: 0,
                QuotaPeakNonPagedPoolUsage: 0,
                QuotaNonPagedPoolUsage: 0,
                PagefileUsage: 0,
                PeakPagefileUsage: 0,
            };
            let size = DWORD::try_from(std::mem::size_of::<PROCESS_MEMORY_COUNTERS>()).ok()?;
            counters.cb = size;
            // SAFETY: `self.0` is a live handle with
            // PROCESS_QUERY_LIMITED_INFORMATION access, owned by `self`;
            // `counters` is a writable PROCESS_MEMORY_COUNTERS of exactly
            // `size` bytes that outlives the call.
            let ok = unsafe { K32GetProcessMemoryInfo(self.0, &mut counters, size) } != 0;
            ok.then_some(counters.WorkingSetSize)
        }
    }

    impl Drop for Process {
        #[allow(unsafe_code)]
        fn drop(&mut self) {
            // SAFETY: `self.0` came non-NULL from OpenProcess and is closed
            // exactly once, here.
            unsafe { CloseHandle(self.0) };
        }
    }

    pub(super) fn pid_alive(pid: u32) -> Option<bool> {
        match Process::open(pid) {
            Ok(process) => process.running(),
            // No process object has this id.
            Err(ERROR_INVALID_PARAMETER) => Some(false),
            // The process exists; it is just not ours to query.
            Err(ERROR_ACCESS_DENIED) => Some(true),
            Err(_) => None,
        }
    }

    pub(super) fn rss_kb(pid: u32) -> Option<u64> {
        let process = Process::open(pid).ok()?;
        if !process.running()? {
            return None;
        }
        let bytes = u64::try_from(process.working_set_bytes()?).ok()?;
        Some(bytes / 1024)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A process that has run and exited, with its pid. The `Child` stays
    /// alive with it: on Windows that keeps the exited process object (and so
    /// its pid) from being recycled while the test asks about it.
    #[cfg(any(target_os = "linux", windows, target_os = "macos"))]
    fn exited_process() -> (std::process::Child, u32) {
        let exe = std::env::current_exe().unwrap();
        let mut child = std::process::Command::new(exe)
            .args(["--list", "--format=terse"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        assert!(child.wait().unwrap().success());
        (child, pid)
    }

    // Linux, Windows and macOS are the platforms with a liveness source; on
    // every other target `pid_alive`/`rss_kb` answer `None`, which
    // `unknown_platforms_answer_none` below pins.
    #[cfg(any(target_os = "linux", windows, target_os = "macos"))]
    #[test]
    fn a_running_process_is_alive_and_an_exited_one_is_gone() {
        assert_eq!(pid_alive(std::process::id()), Some(true));
        let (_child, pid) = exited_process();
        assert_eq!(pid_alive(pid), Some(false), "exited pid {pid}");
        // Windows pids are multiples of 4 and this one is far above any the
        // kernel hands out; on Linux it is above the pid_max ceiling.
        assert_eq!(pid_alive(4_194_304 * 4 + 4), Some(false));
    }

    #[cfg(any(target_os = "linux", windows, target_os = "macos"))]
    #[test]
    fn rss_reads_a_running_process_and_none_for_an_exited_one() {
        assert!(rss_kb(std::process::id()).is_some_and(|kb| kb > 0));
        let (_child, pid) = exited_process();
        assert_eq!(rss_kb(pid), None, "exited pid {pid}");
    }

    #[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
    #[test]
    fn unknown_platforms_answer_none() {
        assert_eq!(pid_alive(std::process::id()), None);
        assert_eq!(rss_kb(std::process::id()), None);
    }

    #[cfg(any(target_os = "linux", windows, target_os = "macos"))]
    #[test]
    fn this_process_is_listed_with_its_identity() {
        let own = std::process::id();
        let me = processes(true).into_iter().find(|p| p.pid == own).unwrap();
        assert!(!me.name.is_empty());
        assert!(me.rss_kb > 0);
        assert!(!me.cmd.is_empty());
        let exe = exe_path(own).unwrap();
        let current = std::env::current_exe().unwrap();
        assert_eq!(
            std::fs::canonicalize(&exe).unwrap(),
            std::fs::canonicalize(&current).unwrap()
        );
        assert!(argv(own).is_some_and(|args| !args.is_empty()));
        let (_child, gone) = exited_process();
        assert_eq!(exe_path(gone), None);
        assert_eq!(environment_var(gone, "PATH"), None);
    }

    /// Variable the environment probe child is started with, and its value.
    #[cfg(any(target_os = "linux", windows, target_os = "macos"))]
    const PROBE_CHILD_MARKER: (&str, &str) = ("SUSI_PROBE_CHILD_MARKER", "present-9f3a");
    /// Variable the environment probe child is started without.
    #[cfg(any(target_os = "linux", windows, target_os = "macos"))]
    const PROBE_CHILD_UNSET: &str = "SUSI_DEFINITELY_UNSET_VARIABLE_9F3A";
    /// Line the probe child prints once it runs, before it parks.
    #[cfg(any(target_os = "linux", windows, target_os = "macos"))]
    const PROBE_CHILD_READY: &str = "susi-environ-probe-ready";

    /// Body of the environment probe child: when started by
    /// [`another_process_environment_is_read`] (marker variable set), announce
    /// readiness and park until the parent closes stdin. Run any other way it
    /// returns at once, so `--ignored` runs never block.
    #[cfg(any(target_os = "linux", windows, target_os = "macos"))]
    #[test]
    #[ignore = "helper process for another_process_environment_is_read"]
    fn environ_probe_child() {
        use std::io::{Read, Write};
        if std::env::var_os(PROBE_CHILD_MARKER.0).is_none() {
            return;
        }
        let mut stdout = std::io::stdout();
        writeln!(stdout, "{PROBE_CHILD_READY}").unwrap();
        stdout.flush().unwrap();
        let _ = std::io::stdin().read_to_end(&mut Vec::new());
    }

    /// The environment of another, live process is read as it was started:
    /// a set variable has its value, a removed one is readable-unset.
    ///
    /// The probe reads a child rather than this process on purpose. On
    /// Windows and macOS it copies the target's environment block out of
    /// process memory, and sibling tests here call `set_var`/`remove_var`,
    /// which replace this process's block while the copy is in flight — an
    /// unreadable or torn snapshot of our own environment is a property of
    /// that race, not of the probe. Production callers only ask about other
    /// processes, whose blocks no thread of ours rewrites.
    #[cfg(any(target_os = "linux", windows, target_os = "macos"))]
    #[test]
    fn another_process_environment_is_read() {
        use std::io::BufRead;
        let (marker, value) = PROBE_CHILD_MARKER;
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "--nocapture",
                "--test-threads=1",
                "process_probe::tests::environ_probe_child",
            ])
            .env(marker, value)
            .env_remove(PROBE_CHILD_UNSET)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        // Wait for the child to run before reading it, so the environment
        // inspected is the started process's own, never a half-made one.
        let mut lines = std::io::BufReader::new(child.stdout.take().unwrap()).lines();
        assert!(
            lines.any(|line| line.is_ok_and(|l| l.contains(PROBE_CHILD_READY))),
            "probe child {pid} never reported ready"
        );

        let read = |key: &str| environment_var(pid, key).expect("child environment readable");
        assert_eq!(read(marker), Some(std::ffi::OsString::from(value)));
        assert_eq!(read(PROBE_CHILD_UNSET), None);
        assert!(read("PATH").is_some(), "PATH inherited by the child");
        // Windows variable names are case-insensitive; elsewhere they are not.
        let folded = read(&marker.to_ascii_lowercase());
        if cfg!(windows) {
            assert_eq!(folded, Some(std::ffi::OsString::from(value)));
        } else {
            assert_eq!(folded, None);
        }

        drop(child.stdin.take());
        assert!(child.wait().unwrap().success());
        assert_eq!(environment_var(pid, marker), None, "exited child {pid}");
    }

    /// Linux lists threads under `/proc/self/task`; a parked thread's tid must
    /// not show up as a process row while this process's own pid still does.
    #[cfg(target_os = "linux")]
    #[test]
    fn threads_are_not_listed_as_processes() {
        let own = std::process::id();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
        let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
        let parked = std::thread::spawn(move || {
            ready_tx.send(()).unwrap();
            let _ = stop_rx.recv();
        });
        ready_rx.recv().unwrap();
        let tids: Vec<u32> = std::fs::read_dir("/proc/self/task")
            .unwrap()
            .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse().ok())
            .filter(|tid| *tid != own)
            .collect();
        assert!(!tids.is_empty(), "the parked thread has a tid");
        let rows = processes(false);
        assert!(rows.iter().any(|row| row.pid == own));
        for tid in &tids {
            assert!(
                !rows.iter().any(|row| row.pid == *tid),
                "thread {tid} listed as a process"
            );
        }
        stop_tx.send(()).unwrap();
        parked.join().unwrap();
    }
}
