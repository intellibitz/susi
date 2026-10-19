//! Process probes every layer needs but only this leaf can host (leaf
//! order `paths → error → config → core`): liveness for stale-lock reclaim
//! and resident memory for service tables. `std`-only — `susi-paths`
//! forbids `unsafe`, so Windows goes through `tasklist` instead of
//! `OpenProcess` FFI.

#[cfg(target_os = "linux")]
use std::path::PathBuf;

/// Whether `pid` refers to a live process. `/proc` on Linux; `kill -0`
/// elsewhere on unix; `tasklist` on Windows; conservatively "alive" where
/// neither works — a false "dead" would make a supervisor respawn a
/// healthy service or let a stale-lock check steal a live holder's lock.
pub fn pid_alive(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        if !PathBuf::from(format!("/proc/{pid}")).is_dir() {
            return false;
        }
        // A zombie still has a /proc dir but cannot serve or hold
        // anything — read the state field (the char after the last ')'
        // in `pid (comm) state ...`; comm itself may contain parens).
        // Unreadable stat keeps the conservative "alive" answer.
        std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|s| s.rsplit(')').next().map(|tail| tail.to_string()))
            .and_then(|tail| tail.trim().chars().next())
            .map(|state| state != 'Z')
            .unwrap_or(true)
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(true)
    }
    #[cfg(windows)]
    {
        tasklist_list(pid)
            .map(|text| text.contains("PID:"))
            .unwrap_or(true)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        true
    }
}

/// Resident set size of `pid` in KiB — `VmRSS` from `/proc/<pid>/status`
/// on Linux, `tasklist /FO LIST`'s `Mem Usage` on Windows. `None` when
/// the pid is dead, unreadable, or the platform has no probe.
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
        let text = tasklist_list(pid)?;
        if !text.contains("PID:") {
            return None;
        }
        text.lines()
            .find_map(|l| l.trim().strip_prefix("Mem Usage:"))
            .and_then(|v| {
                v.trim()
                    .trim_end_matches('K')
                    .trim()
                    .replace(',', "")
                    .parse()
                    .ok()
            })
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = pid;
        None
    }
}

/// `tasklist /FO LIST` output for `pid`; `None` when the tool itself
/// fails. The LIST form labels each field, and a lookup that matched no
/// process prints only an INFO notice, so `PID:` absent means dead.
#[cfg(windows)]
fn tasklist_list(pid: u32) -> Option<String> {
    let out = std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/FO", "LIST"])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}
