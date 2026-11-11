//! Forget one worktree's admin record without a repository-wide prune.
//!
//! `git worktree prune` deletes every `.git/worktrees/<id>` record whose
//! worktree this host cannot see. The core checkout lives on a drive shared
//! by a Windows and a Linux boot of the same laptop, so a record written by
//! Windows Git names its worktree as `F:/src/...`; Linux Git reads that as a
//! relative path that does not exist and prunes the record, detaching a live
//! worktree it never meant to touch. On 2026-11-11 every core worktree record
//! was lost at once (board item 147). A tool that only wants its own
//! throwaway checkout gone must remove exactly that record and nothing else.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

/// Remove the admin record of the worktree that was checked out at
/// `checkout`, once that directory no longer exists. Records of other
/// worktrees, locked records and records whose `gitdir` cannot be read are
/// left alone. Returns how many records were removed (0 or 1 in practice).
pub fn forget(root: &Path, checkout: &Path) -> io::Result<usize> {
    if checkout.exists() {
        return Ok(0);
    }
    let records = common_dir(root)?.join("worktrees");
    let wanted = lexical(&absolute(checkout)?.join(".git"));
    let entries = match fs::read_dir(&records) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    let mut removed = 0;
    for entry in entries {
        let admin = entry?.path();
        if !admin.is_dir() || admin.join("locked").exists() {
            continue;
        }
        let Ok(gitdir) = fs::read_to_string(admin.join("gitdir")) else {
            continue;
        };
        let named = Path::new(gitdir.trim());
        let named = if named.is_absolute() {
            named.to_path_buf()
        } else {
            admin.join(named)
        };
        if lexical(&named) == wanted {
            fs::remove_dir_all(&admin)?;
            removed += 1;
        }
    }
    Ok(removed)
}

fn common_dir(root: &Path) -> io::Result<PathBuf> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "git rev-parse --git-common-dir failed in {}",
            root.display()
        )));
    }
    let text = String::from_utf8(output.stdout).map_err(io::Error::other)?;
    Ok(PathBuf::from(text.trim()))
}

fn absolute(path: &Path) -> io::Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

/// Resolve `.` and `..` without touching the filesystem (the checkout is
/// already gone, so it cannot be canonicalized).
fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other @ (Component::Prefix(_) | Component::RootDir | Component::Normal(_)) => {
                out.push(other.as_os_str())
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@localhost"])
            .args(["-c", "core.hooksPath=", "-c", "commit.gpgsign=false"])
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    /// A repository with two worktrees, `keep` and `gone`, plus the record a
    /// Windows boot leaves behind for a worktree it registered.
    fn fixture(tag: &str) -> (Scratch, PathBuf, PathBuf, PathBuf, PathBuf) {
        let base =
            std::env::temp_dir().join(format!("susi-worktree-admin-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let scratch = Scratch(base.clone());
        let repo = base.join("repo");
        fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "root"]);
        let keep = base.join("keep");
        let gone = base.join("gone");
        for wt in [&keep, &gone] {
            git(
                &repo,
                &["worktree", "add", "-q", "--detach", wt.to_str().unwrap()],
            );
        }
        let windows = repo.join(".git/worktrees/from-windows");
        fs::create_dir_all(&windows).unwrap();
        fs::write(
            windows.join("gitdir"),
            "F:/src/github.com/x/susi-agent-w/.git\n",
        )
        .unwrap();
        fs::write(windows.join("commondir"), "../..\n").unwrap();
        fs::write(windows.join("HEAD"), "ref: refs/heads/master\n").unwrap();
        fs::remove_dir_all(&gone).unwrap();
        (scratch, repo, keep, gone, windows)
    }

    #[test]
    fn forgets_only_the_record_of_the_removed_checkout() {
        let (_scratch, repo, keep, gone, windows) = fixture("forget");
        assert_eq!(forget(&repo, &gone).unwrap(), 1);
        assert!(!repo.join(".git/worktrees/gone").exists());
        assert!(repo.join(".git/worktrees/keep").is_dir());
        assert!(windows.is_dir(), "a Windows-registered record must survive");
        assert!(keep.join(".git").is_file());
        // Idempotent, and a checkout that still exists is never forgotten.
        assert_eq!(forget(&repo, &gone).unwrap(), 0);
        assert_eq!(forget(&repo, &keep).unwrap(), 0);
        assert!(repo.join(".git/worktrees/keep").is_dir());
    }

    #[test]
    fn a_locked_record_is_kept() {
        let (_scratch, repo, _keep, gone, _windows) = fixture("locked");
        fs::write(repo.join(".git/worktrees/gone/locked"), "held\n").unwrap();
        assert_eq!(forget(&repo, &gone).unwrap(), 0);
        assert!(repo.join(".git/worktrees/gone").is_dir());
    }

    /// The hazard this module exists for: a bare prune on this host also
    /// deletes the Windows-registered record of a worktree that is alive.
    #[test]
    fn a_bare_prune_would_also_delete_the_windows_record() {
        let (_scratch, repo, _keep, _gone, windows) = fixture("prune");
        git(&repo, &["worktree", "prune"]);
        assert!(!windows.exists());
        assert!(repo.join(".git/worktrees/keep").is_dir());
    }
}
