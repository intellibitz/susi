//! Liveness and memory footprint of a process named only by its pid: the
//! holder recorded in a lockfile, a supervised leaf service.
//!
//! Linux answers from `/proc`. Windows answers from the process object
//! itself (`OpenProcess`, then `GetExitCodeProcess` /
//! `K32GetProcessMemoryInfo`); that FFI is the only `unsafe` here, wrapped in
//! an owned handle so callers stay safe code. Elsewhere the answer is
//! unknown (`None`) and callers keep their own conservative default.

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
    #[cfg(not(any(target_os = "linux", windows)))]
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
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = pid;
        None
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
    #[cfg(any(target_os = "linux", windows))]
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

    // Linux and Windows are the platforms with a liveness source; on every
    // other target `pid_alive`/`rss_kb` answer `None`, which
    // `unknown_platforms_answer_none` below pins.
    #[cfg(any(target_os = "linux", windows))]
    #[test]
    fn a_running_process_is_alive_and_an_exited_one_is_gone() {
        assert_eq!(pid_alive(std::process::id()), Some(true));
        let (_child, pid) = exited_process();
        assert_eq!(pid_alive(pid), Some(false), "exited pid {pid}");
        // Windows pids are multiples of 4 and this one is far above any the
        // kernel hands out; on Linux it is above the pid_max ceiling.
        assert_eq!(pid_alive(4_194_304 * 4 + 4), Some(false));
    }

    #[cfg(any(target_os = "linux", windows))]
    #[test]
    fn rss_reads_a_running_process_and_none_for_an_exited_one() {
        assert!(rss_kb(std::process::id()).is_some_and(|kb| kb > 0));
        let (_child, pid) = exited_process();
        assert_eq!(rss_kb(pid), None, "exited pid {pid}");
    }

    #[cfg(not(any(target_os = "linux", windows)))]
    #[test]
    fn unknown_platforms_answer_none() {
        assert_eq!(pid_alive(std::process::id()), None);
        assert_eq!(rss_kb(std::process::id()), None);
    }
}
