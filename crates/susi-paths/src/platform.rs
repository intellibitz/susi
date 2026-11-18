//! Platform services core does not implement. Core qualifies Linux only
//! (Mandate 84); a distribution that runs these crates on another OS installs
//! its own reviewed services here once, before its first use of them. Without
//! them, the operations that protect data refuse rather than use a weaker
//! portable substitute, and process probes answer "unknown".
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// One process as the OS reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessSnapshot {
    /// Process id.
    pub pid: u32,
    /// Parent process id as the OS records it (the parent may be gone).
    pub parent_pid: Option<u32>,
    /// Short process name (`comm` on Linux, the image name on Windows).
    pub name: String,
    /// Executable path, when the OS lets this process read it.
    pub exe: Option<PathBuf>,
    /// Command line, when requested and readable (empty otherwise).
    pub cmd: Vec<String>,
    /// Resident memory in KiB (0 when unreadable).
    pub rss_kb: u64,
}

/// The services, each with the contract of the core function that calls it.
#[derive(Clone, Copy)]
pub struct Services {
    /// Create-or-open for appending, owner-only from birth.
    pub open_owner_only_append: fn(&Path) -> io::Result<File>,
    /// Create a new file, owner-only from birth; fails if it exists.
    pub create_new_owner_only: fn(&Path) -> io::Result<File>,
    /// Atomically replace `destination` with the staged file.
    pub replace_staged: fn(staged: &Path, destination: &Path) -> io::Result<()>,
    /// Read one complete snapshot of a file published by atomic replacement.
    pub read_published: fn(&Path) -> io::Result<Vec<u8>>,
    /// Every process; command lines only when asked.
    pub processes: fn(with_cmd: bool) -> Vec<ProcessSnapshot>,
    /// `Some(true)` running, `Some(false)` gone, `None` cannot tell.
    pub pid_alive: fn(u32) -> Option<bool>,
    /// Resident memory in KiB of a running process.
    pub rss_kb: fn(u32) -> Option<u64>,
    /// The executable a process runs.
    pub exe_path: fn(u32) -> Option<PathBuf>,
    /// A process's command line.
    pub argv: fn(u32) -> Option<Vec<String>>,
    /// A process's environment as `KEY=value` entries.
    pub environment: fn(u32) -> Option<Vec<String>>,
}

static SERVICES: OnceLock<Services> = OnceLock::new();

/// Install the services; the first installation wins and later ones are
/// ignored, so every caller may install unconditionally.
pub fn install(services: Services) {
    let _ = SERVICES.set(services);
}

/// The installed services, if any.
pub fn services() -> Option<&'static Services> {
    SERVICES.get()
}

/// The installed services, or the refusal a protected operation returns.
pub fn required(operation: &str) -> io::Result<&'static Services> {
    services().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::Unsupported,
            format!("{operation} on this OS needs the platform services Susi Commercial Apps installs; core implements it for Linux only"),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_protected_operation_without_services_refuses() {
        // Nothing in core installs services, so the refusal is what core sees.
        let error = required("owner-only create").err().map(|e| e.kind());
        assert_eq!(error, Some(io::ErrorKind::Unsupported));
    }
}
