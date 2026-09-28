//! Keeps external agent runs from rewriting the workspace's git identity.
//!
//! Aider 0.86 (`aider/main.py::setup_git`) runs `git config --get
//! user.name/user.email` and, when either is missing, writes the placeholders
//! `Your Name` / `you@example.com` into the repo-local `.git/config`. Launched
//! under an isolated HOME it silently re-authored every later commit in the
//! operator's repo. `GIT_AUTHOR_*` env alone cannot prevent that — `git config
//! --get` ignores it — so the guard (1) refuses to launch into a git work tree
//! whose identity the agent cannot see, (2) pins author/committer env to the
//! identity the agent does see, and (3) restores the repo-local identity if the
//! agent changed it anyway.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;
use susi_error::{eai_bail as bail, EaiResult as Result, ResultExt as Context};

const KEYS: [&str; 2] = ["user.name", "user.email"];
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

pub(super) struct GitIdentityGuard {
    git: PathBuf,
    workspace: PathBuf,
    envs: Vec<(OsString, Option<OsString>)>,
    /// Repo-local `user.name` / `user.email` before launch (`None` = unset).
    local: [Option<String>; 2],
}

impl GitIdentityGuard {
    /// Arms the guard for `cmd` (the agent launch). Probes run with the env
    /// overrides already set on `cmd`, so they see exactly what the agent
    /// will. `Ok(None)` when git is absent or `workspace` is not a work tree.
    pub(super) fn arm(workspace: &Path, cmd: &mut Command) -> Result<Option<Self>> {
        let Some(git) = super::catalog::resolve_program("git") else {
            return Ok(None);
        };
        let mut guard = Self {
            git,
            workspace: workspace.to_path_buf(),
            envs: cmd
                .get_envs()
                .map(|(k, v)| (k.to_os_string(), v.map(|v| v.to_os_string())))
                .collect(),
            local: [None, None],
        };
        let inside = guard.git(&["rev-parse", "--is-inside-work-tree"])?;
        if inside.as_deref() != Some("true") {
            return Ok(None);
        }
        let name = guard.git(&["config", "--get", KEYS[0]])?;
        let email = guard.git(&["config", "--get", KEYS[1]])?;
        let (Some(name), Some(email)) = (name, email) else {
            bail!(
                "no git user.name/user.email visible to the agent in {} (isolated HOME?); \
                 refusing to launch: agents such as aider write placeholder identity into \
                 the repo-local config. Configure git identity or use a scratch workspace",
                workspace.display()
            );
        };
        guard.local = [guard.local_value(KEYS[0])?, guard.local_value(KEYS[1])?];
        cmd.env("GIT_AUTHOR_NAME", &name)
            .env("GIT_COMMITTER_NAME", &name)
            .env("GIT_AUTHOR_EMAIL", &email)
            .env("GIT_COMMITTER_EMAIL", &email);
        Ok(Some(guard))
    }

    /// Restores any repo-local identity key the agent changed. Returns a note
    /// naming the repaired keys, or `None` when the identity is untouched.
    pub(super) fn restore(&self) -> Result<Option<String>> {
        let mut repaired = Vec::new();
        for (key, before) in KEYS.iter().zip(&self.local) {
            let after = self.local_value(key)?;
            if &after == before {
                continue;
            }
            let args: Vec<&str> = match before {
                Some(value) => vec!["config", "--local", key, value.as_str()],
                None => vec!["config", "--local", "--unset-all", key],
            };
            if !self.run(&args)?.status.success() {
                bail!("failed to restore repo-local {key}");
            }
            repaired.push(format!(
                "{key}: {} -> {}",
                after.as_deref().unwrap_or("<unset>"),
                before.as_deref().unwrap_or("<unset>")
            ));
        }
        Ok((!repaired.is_empty()).then(|| {
            format!(
                "agent altered repo-local git identity; restored ({})",
                repaired.join(", ")
            )
        }))
    }

    fn local_value(&self, key: &str) -> Result<Option<String>> {
        self.git(&["config", "--local", "--get", key])
    }

    /// Trimmed stdout on success, `None` on a non-zero exit (unset key / not a repo).
    fn git(&self, args: &[&str]) -> Result<Option<String>> {
        let out = self.run(args)?;
        if !out.status.success() {
            return Ok(None);
        }
        let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
        Ok((!text.is_empty()).then_some(text))
    }

    fn run(&self, args: &[&str]) -> Result<Output> {
        let mut cmd = Command::new(&self.git);
        cmd.arg("-C").arg(&self.workspace).args(args);
        for (key, value) in &self.envs {
            match value {
                Some(value) => cmd.env(key, value),
                None => cmd.env_remove(key),
            };
        }
        crate::susi_core::bounded_cmd::output_within(&mut cmd, PROBE_TIMEOUT)
            .with_context(|| format!("git {}", args.join(" ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Scratch repo whose git sees no global/system config — the isolated-HOME
    /// shape under which aider wrote placeholder identity into the real repo.
    struct Repo(PathBuf);
    impl Repo {
        fn new() -> Self {
            let mut bytes = [0u8; 8];
            getrandom::fill(&mut bytes).unwrap();
            let id: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
            let dir = std::env::temp_dir().join(format!("susi-git-identity-{id}"));
            std::fs::create_dir_all(&dir).unwrap();
            let repo = Self(dir.canonicalize().unwrap());
            repo.git(&["init", "-q"]);
            repo
        }
        fn isolate(&self, cmd: &mut Command) {
            cmd.env("HOME", &self.0)
                .env("XDG_CONFIG_HOME", self.0.join("xdg"))
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1");
        }
        fn git(&self, args: &[&str]) -> Output {
            let mut cmd = Command::new("git");
            cmd.arg("-C").arg(&self.0).args(args);
            self.isolate(&mut cmd);
            cmd.output().unwrap()
        }
        fn local(&self, key: &str) -> Option<String> {
            let out = self.git(&["config", "--local", "--get", key]);
            out.status
                .success()
                .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
        }
    }
    impl Drop for Repo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn isolated_home_without_identity_refuses_launch_and_writes_nothing() {
        let repo = Repo::new();
        let mut cmd = Command::new("true");
        repo.isolate(&mut cmd);
        let err = GitIdentityGuard::arm(&repo.0, &mut cmd)
            .err()
            .expect("launch must be refused");
        assert!(err.to_string().contains("refusing to launch"), "{err}");
        assert_eq!(repo.local("user.name"), None);
        assert_eq!(repo.local("user.email"), None);
    }

    #[test]
    fn non_repo_workspace_is_not_guarded() {
        let repo = Repo::new();
        let plain = repo.0.join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        std::fs::remove_dir_all(repo.0.join(".git")).unwrap();
        let mut cmd = Command::new("true");
        repo.isolate(&mut cmd);
        cmd.env("GIT_CEILING_DIRECTORIES", &repo.0);
        assert!(GitIdentityGuard::arm(&plain, &mut cmd).unwrap().is_none());
    }

    #[test]
    fn aider_style_placeholder_write_is_restored_and_author_env_pinned() {
        let repo = Repo::new();
        repo.git(&["config", "--local", "user.name", "Operator"]);
        // email deliberately unset locally: restore must unset, not keep, it.
        let mut cmd = Command::new("true");
        repo.isolate(&mut cmd);
        cmd.env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "user.email")
            .env("GIT_CONFIG_VALUE_0", "op@example.test");
        let guard = GitIdentityGuard::arm(&repo.0, &mut cmd).unwrap().unwrap();
        let pinned: std::collections::HashMap<_, _> = cmd
            .get_envs()
            .filter_map(|(k, v)| Some((k.to_str()?.to_string(), v?.to_str()?.to_string())))
            .collect();
        assert_eq!(pinned["GIT_AUTHOR_NAME"], "Operator");
        assert_eq!(pinned["GIT_COMMITTER_EMAIL"], "op@example.test");
        assert_eq!(guard.restore().unwrap(), None);

        // What aider's setup_git does when it believes identity is missing.
        repo.git(&["config", "user.name", "Your Name"]);
        repo.git(&["config", "user.email", "you@example.com"]);
        let note = guard.restore().unwrap().expect("tampering must be flagged");
        assert!(note.contains("Your Name"), "{note}");
        assert_eq!(repo.local("user.name").as_deref(), Some("Operator"));
        assert_eq!(repo.local("user.email"), None);
    }
}
