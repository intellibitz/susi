//! Mandatory per-agent local Git worktree isolation.
//!
//! Every local, parallel, swarm, harness, A2A, ACP, and cloud agent runs in its
//! own dedicated linked Git worktree — never in the repository's primary
//! checkout, never directly on `main`/`master`, and never sharing a writable
//! worktree with another concurrent agent.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

const OWNER_FILE: &str = "susi-agent-owner.json";
static WORKTREE_SEQ: AtomicU64 = AtomicU64::new(1);

fn active_in_process_leases() -> &'static Mutex<HashSet<PathBuf>> {
    static ACTIVE: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    ACTIVE.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Recorded active owner of an isolated agent worktree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeOwner {
    pub agent: String,
    pub role: String,
    pub host: String,
    pub pid: u32,
    pub branch: String,
    pub worktree_path: PathBuf,
    pub acquired_unix: u64,
}

/// Verified isolation identity for an existing Git working tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeIdentity {
    pub worktree_root: PathBuf,
    pub git_dir: PathBuf,
    pub common_dir: PathBuf,
    pub branch: String,
    pub owner: Option<WorktreeOwner>,
}

/// Provisioned isolated Git worktree owned exclusively by one agent task.
///
/// Releasing drops the exclusive owner lock and cleans up throwaway clean
/// scratch worktrees when `cleanup_on_drop` is enabled; committed or dirty
/// worktrees are never deleted implicitly.
#[derive(Debug)]
pub struct ProvisionedWorktree {
    pub path: PathBuf,
    pub git_dir: PathBuf,
    pub common_dir: PathBuf,
    pub branch: String,
    pub runtime_root: PathBuf,
    pub target_dir: PathBuf,
    pub home_dir: PathBuf,
    pub owner: WorktreeOwner,
    lock_path: PathBuf,
    cleanup_on_drop: bool,
    released: bool,
}

impl ProvisionedWorktree {
    /// Retain this worktree on disk after release so committed branches can
    /// be inspected, integrated, or merged later.
    pub fn keep(&mut self) {
        self.cleanup_on_drop = false;
    }

    /// Control whether clean, unmodified worktrees are removed when released.
    pub fn set_cleanup_on_drop(&mut self, cleanup: bool) {
        self.cleanup_on_drop = cleanup;
    }

    /// Working directory root of this isolated Git worktree.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Per-worktree isolated `CARGO_TARGET_DIR`.
    #[must_use]
    pub fn target_dir(&self) -> &Path {
        &self.target_dir
    }

    /// Per-worktree isolated `SUSI_HOME`.
    #[must_use]
    pub fn home_dir(&self) -> &Path {
        &self.home_dir
    }

    /// Environment variables isolating child build/runtime state to this
    /// worktree alone.
    #[must_use]
    pub fn isolated_env(&self) -> Vec<(String, String)> {
        vec![
            (
                "CARGO_TARGET_DIR".to_owned(),
                self.target_dir.to_string_lossy().into_owned(),
            ),
            (
                "SUSI_HOME".to_owned(),
                self.home_dir.to_string_lossy().into_owned(),
            ),
            ("SUSI_AGENT".to_owned(), self.owner.agent.clone()),
            (
                "SUSI_WORKTREE_ROOT".to_owned(),
                self.path.to_string_lossy().into_owned(),
            ),
            ("GIT_TERMINAL_PROMPT".to_owned(), "0".to_owned()),
        ]
    }

    /// Release the exclusive owner lock and, if `cleanup_on_drop` is true and
    /// the worktree has no dirty files, prune the temporary worktree.
    pub fn release(&mut self) -> Result<(), String> {
        if self.released {
            return Ok(());
        }
        self.released = true;
        if let Ok(mut active) = active_in_process_leases().lock() {
            active.remove(&self.path);
        }
        if self.lock_path.is_file() {
            let _ = fs::remove_file(&self.lock_path);
        }
        if self.cleanup_on_drop {
            let clean = is_worktree_clean(&self.path).unwrap_or(false);
            if clean {
                let branch = self.branch.clone();
                let _ = git_out(
                    &self.common_dir,
                    &[
                        "worktree",
                        "remove",
                        "--force",
                        &self.path.to_string_lossy(),
                    ],
                );
                let _ = git_out(&self.common_dir, &["worktree", "prune"]);
                let _ = git_out(&self.common_dir, &["branch", "-D", &branch]);
                let _ = fs::remove_dir_all(&self.runtime_root);
            }
        }
        Ok(())
    }
}

impl Drop for ProvisionedWorktree {
    fn drop(&mut self) {
        let _ = self.release();
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn current_host() -> String {
    if let Ok(host) = std::env::var("SUSI_HOST") {
        let trimmed = host.trim();
        if !trimmed.is_empty() {
            return trimmed.to_owned();
        }
    }
    for key in ["HOSTNAME", "COMPUTERNAME"] {
        if let Ok(val) = std::env::var(key) {
            let trimmed = val.trim();
            if !trimmed.is_empty() {
                return trimmed.to_owned();
            }
        }
    }
    if let Ok(text) = fs::read_to_string("/etc/hostname") {
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            return trimmed.to_owned();
        }
    }
    "local".to_owned()
}

fn sanitize_slug(input: &str, fallback: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut last_dash = false;
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
        if out.len() >= 28 {
            break;
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        fallback.to_owned()
    } else {
        trimmed.to_owned()
    }
}

fn normalize_path(path: &Path) -> PathBuf {
    if let Ok(canon) = fs::canonicalize(path) {
        #[cfg(windows)]
        {
            let s = canon.to_string_lossy();
            if let Some(stripped) = s.strip_prefix(r"\\?\") {
                return PathBuf::from(stripped);
            }
        }
        return canon;
    }
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other @ (Component::Prefix(_) | Component::RootDir | Component::Normal(_)) => {
                out.push(other.as_os_str());
            }
        }
    }
    out
}

fn resolve_git_path(base: &Path, raw: &str) -> PathBuf {
    let candidate = PathBuf::from(raw.trim());
    if candidate.is_absolute() {
        normalize_path(&candidate)
    } else {
        normalize_path(&base.join(candidate))
    }
}

fn git_out(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("failed to invoke git {:?} in {}: {e}", args, dir.display()))?;
    if !out.status.success() {
        return Err(format!(
            "git {} failed in {}: {}",
            args.join(" "),
            dir.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

fn is_worktree_clean(path: &Path) -> Result<bool, String> {
    let status = git_out(path, &["status", "--porcelain"])?;
    Ok(status.is_empty())
}

/// Check cross-platform whether `pid` on `host` is still alive.
#[must_use]
pub fn process_is_alive(pid: u32, host: &str) -> bool {
    if pid == 0 {
        return false;
    }
    if pid == std::process::id() {
        return true;
    }
    let local_host = current_host();
    if !host.is_empty() && !host.eq_ignore_ascii_case(&local_host) {
        // Different host cannot be declared dead from local proc checks.
        return true;
    }
    crate::service_table::pid_alive(pid)
}

/// Return true if `path` is inside the repository's primary checkout (not a
/// linked worktree).
pub fn is_primary_checkout(path: &Path) -> Result<bool, String> {
    if !path.exists() {
        return Err(format!("workspace does not exist: {}", path.display()));
    }
    let bare = git_out(path, &["rev-parse", "--is-bare-repository"])?;
    if bare == "true" {
        return Err(format!(
            "bare repository {} cannot be used as an agent worktree",
            path.display()
        ));
    }
    let raw_git_dir = git_out(path, &["rev-parse", "--git-dir"])?;
    let raw_common = git_out(path, &["rev-parse", "--git-common-dir"])?;
    let git_dir = resolve_git_path(path, &raw_git_dir);
    let common_dir = resolve_git_path(path, &raw_common);
    Ok(git_dir == common_dir)
}

/// Inspect the current branch name (`"HEAD"` if detached).
pub fn current_branch(path: &Path) -> Result<String, String> {
    match git_out(path, &["symbolic-ref", "--quiet", "--short", "HEAD"]) {
        Ok(branch) if !branch.is_empty() => Ok(branch),
        Ok(_) | Err(_) => {
            // Confirm `path` is still a valid git working tree before returning detached HEAD.
            let _ = git_out(path, &["rev-parse", "--git-dir"])?;
            Ok("HEAD".to_owned())
        }
    }
}

/// Verify that `path` is an isolated, non-primary, non-main linked Git worktree
/// and that no other active agent currently owns it.
pub fn require_isolated_worktree(
    path: &Path,
    expected_agent: Option<&str>,
) -> Result<WorktreeIdentity, String> {
    if !path.exists() {
        return Err(format!("worktree path does not exist: {}", path.display()));
    }
    let bare = git_out(path, &["rev-parse", "--is-bare-repository"])?;
    if bare == "true" {
        return Err(format!(
            "bare repository {} cannot run agent work; provision an isolated worktree",
            path.display()
        ));
    }
    let worktree_root = resolve_git_path(path, &git_out(path, &["rev-parse", "--show-toplevel"])?);
    let git_dir = resolve_git_path(path, &git_out(path, &["rev-parse", "--git-dir"])?);
    let common_dir = resolve_git_path(path, &git_out(path, &["rev-parse", "--git-common-dir"])?);

    if git_dir == common_dir {
        return Err(format!(
            "primary checkout `{}` is forbidden for agent execution; agents must run in their own isolated linked Git worktree",
            worktree_root.display()
        ));
    }
    let branch = current_branch(&worktree_root)?;
    if matches!(branch.as_str(), "main" | "master") {
        return Err(format!(
            "branch `{branch}` in `{}` is forbidden for direct agent work; use a dedicated worktree branch",
            worktree_root.display()
        ));
    }

    let owner_path = git_dir.join(OWNER_FILE);
    let mut owner = None;
    if owner_path.is_file() {
        let text = fs::read_to_string(&owner_path).map_err(|e| {
            format!(
                "failed to read worktree owner {}: {e}",
                owner_path.display()
            )
        })?;
        let parsed: WorktreeOwner = serde_json::from_str(&text)
            .map_err(|e| format!("corrupt worktree owner {}: {e}", owner_path.display()))?;
        if process_is_alive(parsed.pid, &parsed.host) {
            if let Some(agent) = expected_agent.map(str::trim).filter(|s| !s.is_empty()) {
                if !parsed.agent.eq_ignore_ascii_case(agent) {
                    return Err(format!(
                        "worktree `{}` is exclusively owned by active agent `{}` (pid {}) on host `{}`; `{}` must use its own isolated worktree",
                        worktree_root.display(),
                        parsed.agent,
                        parsed.pid,
                        parsed.host,
                        agent
                    ));
                }
            }
            owner = Some(parsed);
        } else {
            // Reclaim stale lock from a terminated process.
            let _ = fs::remove_file(&owner_path);
        }
    }

    Ok(WorktreeIdentity {
        worktree_root,
        git_dir,
        common_dir,
        branch,
        owner,
    })
}

/// Provision a brand-new, exclusively owned linked Git worktree for one agent
/// task/worker, along with isolated `target` and `SUSI_HOME` directories.
///
/// Even when called from inside an already isolated parent worktree (for
/// example, a coordinator spawning parallel sub-agents), this always creates a
/// distinct child worktree so two concurrent workers never share one checkout.
pub fn provision_agent_worktree(
    workspace_hint: &Path,
    agent: &str,
    role_or_task: &str,
    base_ref: Option<&str>,
) -> Result<ProvisionedWorktree, String> {
    let agent_trimmed = agent.trim();
    if agent_trimmed.is_empty() || agent_trimmed.chars().any(char::is_whitespace) {
        return Err(
            "agent identity must be a non-empty single word when provisioning a worktree"
                .to_owned(),
        );
    }
    if !workspace_hint.exists() {
        return Err(format!(
            "workspace hint `{}` does not exist; agents require a real Git repository",
            workspace_hint.display()
        ));
    }

    let raw_common = git_out(workspace_hint, &["rev-parse", "--git-common-dir"])?;
    let common_dir = resolve_git_path(workspace_hint, &raw_common);
    let primary_parent = common_dir
        .parent()
        .and_then(|repo_dir| repo_dir.parent().map(|p| (repo_dir, p)))
        .ok_or_else(|| {
            format!(
                "cannot resolve parent workspace directory for `{}`",
                common_dir.display()
            )
        })?;
    let repo_name = primary_parent
        .0
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| sanitize_slug(s, "repo"))
        .unwrap_or_else(|| "repo".to_owned());
    let worktree_parent = primary_parent.1;

    let base_sha = match base_ref {
        Some(r) if !r.trim().is_empty() => {
            git_out(workspace_hint, &["rev-parse", "--verify", r.trim()])?
        }
        _ => git_out(workspace_hint, &["rev-parse", "--verify", "HEAD"]).map_err(|e| {
            format!(
                "cannot resolve base commit HEAD in `{}` (empty repos must have an initial commit): {e}",
                workspace_hint.display()
            )
        })?,
    };

    let agent_slug = sanitize_slug(agent_trimmed, "agent");
    let role_slug = sanitize_slug(role_or_task, "worker");
    let pid = std::process::id();
    let host = current_host();

    let mut last_err = String::new();
    for _ in 0..16 {
        let seq = WORKTREE_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir_name = format!("{repo_name}-wt-{agent_slug}-{role_slug}-{pid}-{seq}");
        let wt_path = worktree_parent.join(&dir_name);
        if wt_path.exists() {
            continue;
        }
        let branch = format!("wt/{agent_slug}/{role_slug}-{pid}-{seq}");
        match git_out(
            workspace_hint,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                &branch,
                &wt_path.to_string_lossy(),
                &base_sha,
            ],
        ) {
            Ok(_) => {
                let wt_canon = normalize_path(&wt_path);
                let git_dir =
                    resolve_git_path(&wt_canon, &git_out(&wt_canon, &["rev-parse", "--git-dir"])?);
                let lock_path = git_dir.join(OWNER_FILE);
                let owner = WorktreeOwner {
                    agent: agent_trimmed.to_owned(),
                    role: role_or_task.trim().to_owned(),
                    host: host.clone(),
                    pid,
                    branch: branch.clone(),
                    worktree_path: wt_canon.clone(),
                    acquired_unix: unix_now(),
                };
                let lock_json = serde_json::to_string_pretty(&owner).map_err(|e| e.to_string())?;
                match OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&lock_path)
                {
                    Ok(mut file) => {
                        file.write_all(lock_json.as_bytes())
                            .map_err(|e| format!("failed to write worktree lock: {e}"))?;
                    }
                    Err(e) => {
                        last_err = format!("lock collision at {}: {e}", lock_path.display());
                        let _ = git_out(
                            workspace_hint,
                            &["worktree", "remove", "--force", &wt_canon.to_string_lossy()],
                        );
                        continue;
                    }
                }
                if let Ok(mut active) = active_in_process_leases().lock() {
                    active.insert(wt_canon.clone());
                }

                let runtime_root = worktree_parent.join(format!(".{dir_name}-runtime"));
                let target_dir = runtime_root.join("target");
                let home_dir = runtime_root.join("home");
                fs::create_dir_all(&target_dir)
                    .map_err(|e| format!("failed to create isolated target dir: {e}"))?;
                fs::create_dir_all(&home_dir)
                    .map_err(|e| format!("failed to create isolated home dir: {e}"))?;

                return Ok(ProvisionedWorktree {
                    path: wt_canon,
                    git_dir,
                    common_dir,
                    branch,
                    runtime_root,
                    target_dir,
                    home_dir,
                    owner,
                    lock_path,
                    cleanup_on_drop: false,
                    released: false,
                });
            }
            Err(err) => {
                last_err = err;
            }
        }
    }

    Err(format!(
        "failed to provision isolated worktree for `{agent_trimmed}` after 16 attempts: {last_err}"
    ))
}

/// Verify an existing isolated worktree when `require_fresh == false`, or
/// provision a dedicated linked worktree when `workspace` is the primary
/// checkout / `main` / already active in this process / when `require_fresh` is
/// true.
pub fn acquire_isolated_worktree(
    workspace: &Path,
    agent: &str,
    role_or_task: &str,
    require_fresh: bool,
) -> Result<ProvisionedWorktree, String> {
    if !require_fresh {
        if let Ok(id) = require_isolated_worktree(workspace, Some(agent)) {
            let already_active = active_in_process_leases()
                .lock()
                .is_ok_and(|set| set.contains(&id.worktree_root));
            if !already_active {
                let lock_path = id.git_dir.join(OWNER_FILE);
                let host = current_host();
                let pid = std::process::id();
                let owner = WorktreeOwner {
                    agent: agent.trim().to_owned(),
                    role: role_or_task.trim().to_owned(),
                    host,
                    pid,
                    branch: id.branch.clone(),
                    worktree_path: id.worktree_root.clone(),
                    acquired_unix: unix_now(),
                };
                if let Ok(mut file) = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&lock_path)
                {
                    let lock_json =
                        serde_json::to_string_pretty(&owner).map_err(|e| e.to_string())?;
                    file.write_all(lock_json.as_bytes())
                        .map_err(|e| format!("failed to write worktree lock: {e}"))?;
                    if let Ok(mut active) = active_in_process_leases().lock() {
                        active.insert(id.worktree_root.clone());
                    }
                    let dir_name = id
                        .worktree_root
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("susi-wt");
                    let parent = id.worktree_root.parent().unwrap_or_else(|| Path::new("."));
                    let runtime_root = parent.join(format!(".{dir_name}-runtime"));
                    let target_dir = runtime_root.join("target");
                    let home_dir = runtime_root.join("home");
                    fs::create_dir_all(&target_dir)
                        .map_err(|e| format!("failed to create isolated target dir: {e}"))?;
                    fs::create_dir_all(&home_dir)
                        .map_err(|e| format!("failed to create isolated home dir: {e}"))?;
                    return Ok(ProvisionedWorktree {
                        path: id.worktree_root,
                        git_dir: id.git_dir,
                        common_dir: id.common_dir,
                        branch: id.branch,
                        runtime_root,
                        target_dir,
                        home_dir,
                        owner,
                        lock_path,
                        cleanup_on_drop: false,
                        released: false,
                    });
                }
            }
        }
    }
    provision_agent_worktree(workspace, agent, role_or_task, None)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // Test module: unwrap/expect assert isolated temp-repo fixture setup.
    use super::*;
    use std::sync::atomic::AtomicUsize;

    static TEST_SEQ: AtomicUsize = AtomicUsize::new(0);

    struct RepoFixture {
        root: PathBuf,
        primary: PathBuf,
    }

    impl RepoFixture {
        fn new(tag: &str) -> Self {
            let id = TEST_SEQ.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "susi-core-wt-test-{tag}-{}-{id}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&root);
            let primary = root.join("repo");
            fs::create_dir_all(&primary).unwrap();
            git(&primary, &["init", "-q", "-b", "main"]);
            git(&primary, &["config", "user.name", "Test"]);
            git(&primary, &["config", "user.email", "test@susi.local"]);
            git(&primary, &["config", "core.hooksPath", "no-hooks"]);
            git(&primary, &["config", "commit.gpgsign", "false"]);
            fs::write(primary.join("README.md"), "seed\n").unwrap();
            git(&primary, &["add", "README.md"]);
            git(&primary, &["commit", "-q", "-m", "initial commit"]);
            Self { root, primary }
        }
    }

    impl Drop for RepoFixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn git(dir: &Path, args: &[&str]) {
        git_out(dir, args).unwrap();
    }

    #[test]
    fn primary_checkout_is_detected_and_forbidden() {
        let fx = RepoFixture::new("primary");
        assert!(is_primary_checkout(&fx.primary).unwrap());
        let err = require_isolated_worktree(&fx.primary, Some("antigravity"))
            .expect_err("primary checkout must be refused");
        assert!(err.contains("primary checkout"), "{err}");
    }

    #[test]
    fn parallel_provisioned_worktrees_are_distinct_isolated_and_cleanly_released() {
        let fx = RepoFixture::new("parallel");
        let mut wt1 = provision_agent_worktree(&fx.primary, "antigravity", "worker-a", None)
            .expect("provision wt1");
        let mut wt2 = provision_agent_worktree(&fx.primary, "antigravity", "worker-b", None)
            .expect("provision wt2");

        assert_ne!(wt1.path, wt2.path);
        assert_ne!(wt1.branch, wt2.branch);
        assert_ne!(wt1.target_dir, wt2.target_dir);
        assert_ne!(wt1.home_dir, wt2.home_dir);
        assert!(!is_primary_checkout(&wt1.path).unwrap());
        assert!(!is_primary_checkout(&wt2.path).unwrap());

        let id1 = require_isolated_worktree(&wt1.path, Some("antigravity")).unwrap();
        assert_eq!(
            id1.owner.as_ref().map(|o| o.agent.as_str()),
            Some("antigravity")
        );

        let err_other = require_isolated_worktree(&wt1.path, Some("other-agent"))
            .expect_err("foreign agent must not share owned worktree");
        assert!(err_other.contains("exclusively owned"), "{err_other}");

        wt1.set_cleanup_on_drop(true);
        wt2.set_cleanup_on_drop(true);
        let p1 = wt1.path.clone();
        let p2 = wt2.path.clone();
        wt1.release().unwrap();
        wt2.release().unwrap();
        assert!(!p1.exists());
        assert!(!p2.exists());
    }

    #[test]
    fn nested_acquire_never_reuses_parent_in_process_worktree() {
        let fx = RepoFixture::new("nested");
        let parent = acquire_isolated_worktree(&fx.primary, "codex", "coordinator", false).unwrap();
        let child = acquire_isolated_worktree(&parent.path, "codex", "subtask", false).unwrap();
        assert_ne!(parent.path, child.path);
        assert_ne!(parent.branch, child.branch);
        assert!(!is_primary_checkout(&child.path).unwrap());
    }
}
