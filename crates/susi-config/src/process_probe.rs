//! Linux `/proc` process liveness and executable-path probes.

use std::path::{Path, PathBuf};

/// Whether `pid` names a running Linux process (`/proc/<pid>` exists).
#[must_use]
pub fn pid_alive(pid: u32) -> Option<bool> {
    Some(Path::new(&format!("/proc/{pid}")).exists())
}

/// Raw `/proc/<pid>/exe` target for `pid`, or `None` when gone or unreadable.
#[must_use]
pub fn exe_path(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/exe")).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn running_and_exited_processes_report_liveness_and_exe_path() {
        let own = std::process::id();
        assert_eq!(pid_alive(own), Some(true));
        assert_eq!(
            std::fs::canonicalize(exe_path(own).unwrap()).unwrap(),
            std::fs::canonicalize(std::env::current_exe().unwrap()).unwrap()
        );
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--list", "--format=terse"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        assert!(child.wait().unwrap().success());
        assert_eq!(pid_alive(pid), Some(false));
        assert_eq!(exe_path(pid), None);
    }
}
