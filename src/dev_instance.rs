//! Automatic private state and port allocation for development executables.
//!
//! Only the installed release uses production state. Every development binary
//! directory gets its own persistent instance; agent worktrees never share
//! writable runtime state or automatically inherit production credentials.

use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

const DEV_HOME_DIR: &str = ".susi-dev";

fn namespace(identity: &str) -> String {
    // Stable namespace only, not an integrity/security digest. The full identity
    // is stored and compared so a hash collision can never alias another owner.
    let value = identity
        .bytes()
        .fold(0xcbf29ce484222325_u64, |state, byte| {
            (state ^ u64::from(byte)).wrapping_mul(0x100000001b3)
        });
    format!("{value:016x}")
}

fn allocate_instance(home: &Path, identity: &str) -> io::Result<(PathBuf, u16)> {
    let base = home.join(DEV_HOME_DIR);
    fs::create_dir_all(&base)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(base.join("allocation.lock"))?;
    lock.lock()?; // Native OS lock; automatically released even after a crash.
    let slots = base.join("port-owners");
    fs::create_dir_all(&slots)?;
    let mut free = None;
    let mut existing = None;
    for slot in 0..400_u16 {
        let offset = 1000 + slot * 100;
        let owner = slots.join(offset.to_string());
        match fs::read_to_string(&owner) {
            Ok(value) if value == identity => {
                existing = Some(offset);
                break;
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if free.is_none() {
                    free = Some(offset);
                }
            }
            Err(error) => return Err(error),
        }
    }
    let offset = existing
        .or(free)
        .ok_or_else(|| io::Error::other("no private development port range available"))?;
    let root = base.join("instances").join(namespace(identity));
    fs::create_dir_all(&root)?;
    let identity_file = root.join("owner");
    match fs::read_to_string(&identity_file) {
        Ok(value) if value != identity => {
            return Err(io::Error::other(
                "development namespace collision; refusing to share state",
            ))
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut owner = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&identity_file)?;
            owner.write_all(identity.as_bytes())?;
            owner.sync_all()?;
        }
        Err(error) => return Err(error),
    }
    if existing.is_none() {
        let mut owner = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(slots.join(offset.to_string()))?;
        owner.write_all(identity.as_bytes())?;
        owner.sync_all()?;
    }
    Ok((root, offset))
}
fn installed_binary(home: &Path) -> PathBuf {
    home.join(".susi")
        .join("bin")
        .join(if cfg!(windows) { "susi.exe" } else { "susi" })
}

/// True when `exe` is the installed release binary. A daemon still running
/// after its binary was swapped reports `… (deleted)`; that is still the
/// release instance.
fn is_installed_binary(exe: &Path, home: &Path) -> bool {
    let exe = exe.to_string_lossy();
    let exe = Path::new(exe.strip_suffix(" (deleted)").unwrap_or(&exe));
    let installed = installed_binary(home);
    exe == installed
        || std::fs::canonicalize(exe)
            .ok()
            .zip(std::fs::canonicalize(&installed).ok())
            .is_some_and(|(a, b)| a == b)
}

/// Selects the dev instance for any binary other than the installed one.
/// Must run first in `main`, before any thread exists: environment mutation
/// is only sound while the process is single-threaded.
pub(crate) fn isolate_if_dev_build(home: &Path) -> io::Result<()> {
    if env::var_os("SUSI_HOME").is_some() {
        return Ok(());
    }
    let exe = env::current_exe()?;
    if is_installed_binary(&exe, home) {
        return Ok(());
    }
    let directory = exe
        .parent()
        .ok_or_else(|| io::Error::other("executable has no directory"))?
        .canonicalize()?;
    let identity = directory
        .to_str()
        .ok_or_else(|| io::Error::other("executable directory is not Unicode"))?;
    let (root, offset) = allocate_instance(home, identity)?;
    env::set_var("SUSI_HOME", root);
    if env::var_os("SUSI_PORT_OFFSET").is_none() {
        env::set_var("SUSI_PORT_OFFSET", offset.to_string());
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_workers_receive_disjoint_ports() {
        let home =
            std::env::temp_dir().join(format!("susi-concurrent-instances-{}", std::process::id()));
        let workers: Vec<_> = (0..12)
            .map(|worker| {
                let home = home.clone();
                std::thread::spawn(move || {
                    allocate_instance(&home, &format!("worker-{worker}")).unwrap()
                })
            })
            .collect();
        let mut ports = std::collections::HashSet::new();
        let mut homes = std::collections::HashSet::new();
        for worker in workers {
            let (root, offset) = worker.join().unwrap();
            assert!(homes.insert(root));
            for base in [
                9090, 9091, 9092, 9093, 9094, 9095, 18080, 18081, 18082, 18083, 18084,
            ] {
                assert!(ports.insert(base + offset));
            }
        }
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn private_instances_reuse_their_own_state_and_never_share_port_ranges() {
        let home = std::env::temp_dir().join(format!("susi-instance-test-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        let a = allocate_instance(&home, "worktree-a/debug").unwrap();
        let b = allocate_instance(&home, "worktree-b/debug").unwrap();
        assert_ne!(a.0, b.0);
        assert_ne!(a.1, b.1);
        assert_eq!(a, allocate_instance(&home, "worktree-a/debug").unwrap());
        std::fs::write(a.0.join("private"), "a").unwrap();
        assert!(!b.0.join("private").exists());
        assert!(a.1 + 18084 < u16::MAX);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn namespace_collision_fails_instead_of_sharing_runtime() {
        let home =
            std::env::temp_dir().join(format!("susi-instance-collision-{}", std::process::id()));
        let root = home
            .join(DEV_HOME_DIR)
            .join("instances")
            .join(namespace("worker"));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("owner"), "another worker").unwrap();
        assert!(allocate_instance(&home, "worker").is_err());
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn only_the_installed_binary_is_the_release_instance() {
        let home = Path::new("/home/u");
        let bin = if cfg!(windows) { "susi.exe" } else { "susi" };
        assert!(is_installed_binary(&home.join(".susi/bin").join(bin), home));
        assert!(!is_installed_binary(
            Path::new("/home/u/src/susi/target/release/susi"),
            home
        ));
        assert!(!is_installed_binary(
            Path::new("/home/u/.susi/build-cache/release-sync/release/susi"),
            home
        ));
    }

    #[cfg(unix)]
    #[test]
    fn a_swapped_out_release_daemon_is_still_the_release_instance() {
        let home = Path::new("/home/u");
        assert!(is_installed_binary(
            Path::new("/home/u/.susi/bin/susi (deleted)"),
            home
        ));
    }
}
