//! Shared fixtures for the integration tests.

// Each integration test compiles this module separately and uses a subset.
#![allow(dead_code)]

use std::path::PathBuf;

/// Swaps `HOME` / `USERPROFILE` / `XDG_CONFIG_HOME` to a fresh temp dir
/// (with `.susi` pre-created, so `SusiDirs` picks the deterministic legacy
/// path) and restores them — and removes the dir — on drop.
pub struct HomeGuard {
    tmp: PathBuf,
    // Scrubs `SUSI_HOME`/`SUSI_PORT_OFFSET` (they outrank HOME/XDG) and
    // restores every variable it touched; dropped before the dir is removed.
    env: Option<susi_paths::test_env::EnvGuard>,
}

impl HomeGuard {
    /// `tag` keeps concurrent test binaries' temp dirs apart.
    pub fn new(tag: &str) -> Self {
        let tmp = std::env::temp_dir().join(format!(
            "susi_{tag}_it_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join(".susi")).unwrap();
        let mut env = susi_paths::test_env::EnvGuard::isolated();
        env.set_home(&tmp);
        Self {
            tmp,
            env: Some(env),
        }
    }

    /// The legacy `.susi` config dir under the swapped HOME.
    pub fn config(&self) -> PathBuf {
        self.tmp.join(".susi")
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        self.env = None;
        let _ = std::fs::remove_dir_all(&self.tmp);
    }
}

/// A `Command` that runs a POSIX shell script on any host — Windows cannot
/// exec a shebang (os error 193), so scripts go through the shell
/// `susi_core::bounded_cmd` resolves on every platform. The env also pins
/// gpgsign off so fixture repos stay hermetic to the host's signing setup:
/// a global commit.gpgsign with an unreachable keyboxd must not fail the
/// commits fixture scripts make internally.
pub fn script_command(script: &std::path::Path, args: &[&str]) -> std::process::Command {
    let mut cmd = susi_core::bounded_cmd::script_command(script, args);
    cmd.env("GIT_CONFIG_COUNT", "2")
        .env("GIT_CONFIG_KEY_0", "commit.gpgsign")
        .env("GIT_CONFIG_VALUE_0", "false")
        .env("GIT_CONFIG_KEY_1", "tag.gpgsign")
        .env("GIT_CONFIG_VALUE_1", "false");
    cmd
}

/// The real git binary, found by scanning PATH past `exclude_dir` (which a
/// test fills with its own `git` stub). Forward slashes so the value is
/// usable inside POSIX shell stubs on Windows, where /usr/bin/git is absent.
pub fn real_git(exclude_dir: &std::path::Path) -> String {
    for dir in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        if dir == exclude_dir {
            continue;
        }
        for name in ["git", "git.exe"] {
            let c = dir.join(name);
            if c.is_file() {
                return c.to_string_lossy().replace('\\', "/");
            }
        }
    }
    "/usr/bin/git".to_string()
}

/// Render `p` the way MSYS/Git Bash needs it inside generated script text:
/// `C:\a\b` → `/c/a/b` (a bare `C:\…` loses `\t`-style escapes under the
/// shell's quoting rules). On unix the path is already in that form.
pub fn posix_path(p: &std::path::Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    let bytes = s.as_bytes();
    if bytes.len() >= 3 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        format!("/{}/{}", (bytes[0] as char).to_ascii_lowercase(), &s[3..])
    } else {
        s
    }
}

/// `dir` prepended to the inherited PATH with the OS list separator (`;`
/// on Windows, `:` on unix). MSYS children translate the native form to
/// POSIX themselves; joining with `:` here would corrupt Windows paths.
pub fn path_with_first(dir: &std::path::Path) -> std::ffi::OsString {
    std::env::join_paths(
        std::iter::once(dir.to_path_buf()).chain(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        )),
    )
    .unwrap_or_else(|_| dir.as_os_str().to_os_string())
}

/// `Command` for a generic `run(dir, program, args)` helper: a script path
/// (`.sh`, `.py`, a `.githooks` hook, or an extensionless path) goes through
/// the script resolver — Windows cannot exec a shebang; anything else is a
/// real binary name.
pub fn program_command(program: &str, args: &[&str]) -> std::process::Command {
    let p = std::path::Path::new(program);
    let script_like = program.ends_with(".sh")
        || program.ends_with(".py")
        || program.contains(".githooks")
        || (p.extension().is_none() && p.components().count() > 1 && p.is_file());
    if script_like {
        script_command(p, args)
    } else {
        let mut c = std::process::Command::new(program);
        c.args(args);
        c
    }
}
