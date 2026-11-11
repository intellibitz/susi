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

/// Serializes integration tests that spawn real Susi listeners.
///
/// Cargo runs different integration-test binaries concurrently, while the
/// daemon tests exercise the host network stack and several child daemons at
/// once.  An OS lock gives those tests crash-safe cross-process ownership of
/// that resource without relying on a process-local mutex or leaving a stale
/// pid file behind.
pub struct SocketTestGuard {
    _lock: std::fs::File,
}

impl SocketTestGuard {
    /// Acquire the host-wide socket-test reservation.
    pub fn acquire() -> Self {
        let path = std::env::temp_dir().join("susi-native-socket-tests.lock");
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .expect("create the socket integration lock");
        lock.lock().expect("acquire the socket integration lock");
        Self { _lock: lock }
    }
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
        env.set("HOME", &tmp);
        env.set("USERPROFILE", &tmp);
        env.set("XDG_CONFIG_HOME", tmp.join("xdg"));
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

/// The POSIX shell that runs the repository's `.sh` scripts and git hooks.
///
/// On Windows that is Git for Windows' bash, found from git itself
/// (`git --exec-path` is `<git>/<mingw64|ucrt64>/libexec/git-core`, the shell
/// is `<git>/bin/bash.exe`): `bash.exe` or `sh.exe` on PATH can be WSL's
/// launcher in System32 or a package manager's shim, not the shell git runs
/// its hooks with. Elsewhere it is `bash` on PATH.
pub fn posix_shell() -> PathBuf {
    #[cfg(windows)]
    {
        static SHELL: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
        SHELL
            .get_or_init(|| {
                let out = std::process::Command::new("git")
                    .arg("--exec-path")
                    .output()
                    .expect("git must run to locate Git for Windows bash");
                assert!(
                    out.status.success(),
                    "git --exec-path failed: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
                let exec_path = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
                let bash = exec_path
                    .ancestors()
                    .nth(3)
                    .unwrap_or_else(|| panic!("unexpected git exec path {exec_path:?}"))
                    .join("bin")
                    .join("bash.exe");
                assert!(
                    bash.is_file(),
                    "Git for Windows bash not found at {}",
                    bash.display()
                );
                bash
            })
            .clone()
    }
    #[cfg(not(windows))]
    {
        PathBuf::from("bash")
    }
}

/// A command that runs `script` the way git and the operator do: directly on
/// Unix (shebang and exec bit), under [`posix_shell`] on Windows.
pub fn script_command(script: &std::path::Path) -> std::process::Command {
    #[cfg(windows)]
    {
        let mut command = std::process::Command::new(posix_shell());
        command.arg(script);
        command
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new(script)
    }
}

/// How long one script, hook or git invocation of a fixture may run. Whole
/// binaries of these fixtures finish in seconds; the bound only turns a hang
/// (a prompt, a held lock, a leftover child) into a failure.
pub const SCRIPT_BOUND: std::time::Duration = std::time::Duration::from_secs(120);

/// Runs `command` with `stdin` piped in and returns its output, failing (never
/// hanging) when it outlives `bound`: the child is killed and the test panics
/// with what it printed. Both pipes are drained on their own threads, and read
/// with a deadline, so neither a full pipe nor a grandchild that keeps one open
/// can stall the test.
pub fn run_script_bounded(
    command: &mut std::process::Command,
    stdin: &[u8],
    bound: std::time::Duration,
) -> std::process::Output {
    use std::io::{Read, Write};
    use std::process::Stdio;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    let program = command.get_program().to_string_lossy().into_owned();
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("start {program}: {e}"));
    let input = stdin.to_vec();
    let mut pipe_in = child.stdin.take().expect("stdin pipe");
    std::thread::spawn(move || {
        let _ = pipe_in.write_all(&input);
    });
    fn drain(mut pipe: impl Read + Send + 'static) -> mpsc::Receiver<Vec<u8>> {
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = pipe.read_to_end(&mut bytes);
            let _ = sender.send(bytes);
        });
        receiver
    }
    let out = drain(child.stdout.take().expect("stdout pipe"));
    let err = drain(child.stderr.take().expect("stderr pipe"));
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().expect("poll the child") {
            break Some(status);
        }
        if started.elapsed() > bound {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let read = |pipe: mpsc::Receiver<Vec<u8>>| pipe.recv_timeout(Duration::from_secs(30));
    let (stdout, stderr) = (read(out), read(err));
    let shown = |bytes: &Result<Vec<u8>, mpsc::RecvTimeoutError>| match bytes {
        Ok(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        Err(_) => String::from("<pipe still held open after 30 s>"),
    };
    let Some(status) = status else {
        panic!(
            "{program} did not finish within {bound:?}\nstdout:\n{}\nstderr:\n{}",
            shown(&stdout),
            shown(&stderr)
        );
    };
    match (stdout, stderr) {
        (Ok(stdout), Ok(stderr)) => std::process::Output {
            status,
            stdout,
            stderr,
        },
        (stdout, stderr) => panic!(
            "{program} exited ({status}) but a process it left behind still holds its \
             output pipe\nstdout:\n{}\nstderr:\n{}",
            shown(&stdout),
            shown(&stderr)
        ),
    }
}
