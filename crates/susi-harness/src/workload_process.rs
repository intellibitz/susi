//! Keep each provider's workers in an OS-owned process tree for its turn.

use std::io;
use std::process::Child;
#[cfg(not(windows))]
use std::process::Command;

#[cfg(windows)]
pub(crate) use winapi::process_job::{spawn, Job as Tree};

#[cfg(unix)]
pub(crate) struct Tree(Option<u32>);

#[cfg(unix)]
pub(crate) fn spawn(command: &mut Command) -> io::Result<(Child, Tree)> {
    use std::os::unix::process::CommandExt;
    let child = command.process_group(0).spawn()?;
    let tree = Tree(Some(child.id()));
    Ok((child, tree))
}

#[cfg(unix)]
impl Drop for Tree {
    fn drop(&mut self) {
        if let Some(pid) = self.0.take() {
            let _ = susi_config::process_probe::kill_child_group(pid);
        }
    }
}

/// Stop remaining descendants before reaping a Unix group leader, so a
/// reused PID can never direct cleanup at an unrelated process group.
pub(crate) fn try_wait(
    tree: &mut Option<Tree>,
    child: &mut Child,
) -> io::Result<Option<std::process::ExitStatus>> {
    #[cfg(unix)]
    if let Some(guard) = tree {
        if !susi_config::process_probe::child_exited_unreaped(child)? {
            return Ok(None);
        }
        if let Some(pid) = guard.0 {
            susi_config::process_probe::kill_child_group(pid)?;
            guard.0 = None;
        }
        return child.wait().map(Some);
    }
    #[cfg(not(unix))]
    let _ = tree;
    child.try_wait()
}

#[cfg(not(any(unix, windows)))]
pub(crate) struct Tree;

#[cfg(not(any(unix, windows)))]
pub(crate) fn spawn(_: &mut Command) -> io::Result<(Child, Tree)> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "no workload process containment on this platform",
    ))
}

#[cfg(all(test, any(unix, windows)))]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;
    use std::process::{Command, Stdio};
    use std::thread::sleep;
    use std::time::{Duration, Instant};

    const FIXTURE: &str = "workload_process::tests::workload_child";

    fn child_command(root: &Path, role: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", FIXTURE])
            .env("SUSI_TREE_TEST_ROOT", root)
            .env("SUSI_TREE_TEST_ROLE", role)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command
    }

    #[test]
    #[allow(
        clippy::zombie_processes,
        reason = "the parent-exit test deliberately leaves its child to verify that the harness reaps the provider tree"
    )]
    fn workload_child() {
        let Some(root) = std::env::var_os("SUSI_TREE_TEST_ROOT") else {
            return;
        };
        let root = Path::new(&root);
        if std::env::var("SUSI_TREE_TEST_ROLE").unwrap() == "worker" {
            fs::write(root.join("ready"), []).unwrap();
            for _ in 0..500 {
                if root.join("probe").exists() {
                    fs::write(root.join("worker_survived"), []).unwrap();
                    return;
                }
                sleep(Duration::from_millis(10));
            }
            return;
        }
        let mut worker = child_command(root, "worker").spawn().unwrap();
        for _ in 0..500 {
            if root.join("exit").exists() {
                return;
            }
            sleep(Duration::from_millis(10));
        }
        let _ = worker.wait();
    }

    #[test]
    fn workload_tree_stops_grandchild_on_cancellation_and_provider_exit() {
        for normal_exit in [false, true] {
            let dir = susi_paths::tempfile::tempdir().unwrap();
            let root = dir.path();
            let (mut child, guard) = spawn(&mut child_command(root, "provider")).unwrap();
            let mut tree = Some(guard);
            let deadline = Instant::now() + Duration::from_secs(10);
            while !root.join("ready").is_file() {
                assert!(Instant::now() < deadline, "worker did not start");
                sleep(Duration::from_millis(15));
            }
            if normal_exit {
                fs::write(root.join("exit"), []).unwrap();
                while try_wait(&mut tree, &mut child).unwrap().is_none() {
                    assert!(Instant::now() < deadline, "provider did not exit");
                    sleep(Duration::from_millis(15));
                }
            }
            drop(tree);
            child.wait().unwrap();
            fs::write(root.join("probe"), []).unwrap();
            sleep(Duration::from_millis(350));
            assert!(
                !root.join("worker_survived").exists(),
                "worker outlived its grant"
            );
        }
    }
}
