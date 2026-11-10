//! Fast, targeted feedback in an already reserved worktree. This command
//! produces no integration/release evidence and never publishes source.
use super::compiler_env;
#[path = "dev_capacity.rs"]
mod dev_capacity;
use dev_capacity::{private_directory, Pool};
use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Debug, PartialEq, Eq)]
enum Action {
    Capacity(usize),
    Check {
        kind: String,
        package: String,
        jobs: usize,
        wait_ms: u64,
    },
}

fn parse(args: &[String]) -> io::Result<Action> {
    if let [command, count @ ..] = args {
        if command == "capacity" && count.len() <= 1 {
            let agents = count
                .first()
                .map(|s| s.parse::<usize>())
                .transpose()
                .map_err(io::Error::other)?
                .unwrap_or(100);
            if agents > 0 && agents <= 100_000 {
                return Ok(Action::Capacity(agents));
            }
        }
    }
    let [kind, package, options @ ..] = args else {
        return Err(usage());
    };
    if !matches!(kind.as_str(), "check" | "clippy" | "test")
        || package.is_empty()
        || package.starts_with('-')
        || !package
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
    {
        return Err(usage());
    }
    let (mut jobs, mut wait_ms) = (2, 2_000);
    let (mut saw_jobs, mut saw_wait) = (false, false);
    for pair in options.chunks(2) {
        match pair {
            [flag, value] if flag == "--jobs" && !saw_jobs => {
                jobs = value.parse().map_err(io::Error::other)?;
                saw_jobs = true;
            }
            [flag, value] if flag == "--wait-ms" && !saw_wait => {
                wait_ms = value.parse().map_err(io::Error::other)?;
                saw_wait = true;
            }
            _ => return Err(usage()),
        }
    }
    if jobs == 0 || jobs > 16 || wait_ms > 60_000 {
        return Err(usage());
    }
    Ok(Action::Check {
        kind: kind.clone(),
        package: package.clone(),
        jobs,
        wait_ms,
    })
}

fn usage() -> io::Error {
    io::Error::other("usage: susi-dev dev capacity [agents] | dev check|clippy|test <package> [--jobs 1..16] [--wait-ms 0..60000]; development feedback only, not a merge or release gate")
}

fn git(root: &Path, args: &[&str]) -> io::Result<String> {
    let output = Command::new("git")
        .current_dir(root)
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .args(args)
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other("cannot inspect current native worktree"));
    }
    String::from_utf8(output.stdout)
        .map(|s| s.trim().to_owned())
        .map_err(io::Error::other)
}

fn claimed_worktree(root: &Path) -> io::Result<String> {
    // A native reservation is a linked worktree, never the primary checkout.
    if !root.join(".git").is_file() {
        return Err(io::Error::other(
            "development checks require a native reserved worktree, not the primary checkout",
        ));
    }
    let branch = git(root, &["branch", "--show-current"])?;
    let worker = branch
        .strip_prefix("local-agent/")
        .filter(|name| {
            !name.is_empty()
                && name
                    .bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        })
        .ok_or_else(|| io::Error::other("development checks require a local-agent branch"))?;
    let common = PathBuf::from(git(
        root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?);
    let text = fs::read_to_string(
        common
            .join("susi-local-workflow/claims")
            .join(format!("{worker}.claim")),
    )?;
    let mut lines = text.lines();
    if lines.next() != Some("susi-local-claim-v2")
        || lines.next() != Some(worker)
        || lines
            .next()
            .map(PathBuf::from)
            .map(|path| path.canonicalize())
            .transpose()?
            != Some(root.canonicalize()?)
    {
        return Err(io::Error::other(
            "native claim does not belong to this worktree",
        ));
    }
    Ok(worker.to_owned())
}

fn tool_homes() -> io::Result<(PathBuf, PathBuf)> {
    let original = env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::other("cannot resolve toolchain home"))?;
    let cargo = env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| original.join(".cargo"))
        .canonicalize()?;
    let rustup = env::var_os("RUSTUP_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| original.join(".rustup"))
        .canonicalize()?;
    Ok((cargo, rustup))
}

fn cargo_args(kind: &str, package: &str, host: &str, target: &Path, jobs: usize) -> Vec<String> {
    let mut args = vec![
        kind.into(),
        "--offline".into(),
        "--locked".into(),
        "-p".into(),
        package.into(),
        "--target".into(),
        host.into(),
        "--target-dir".into(),
        target.to_string_lossy().into_owned(),
        "--jobs".into(),
        jobs.to_string(),
    ];
    if kind != "test" {
        args.push("--all-targets".into());
    }
    if package == "xtask" {
        args.extend(["--features".into(), "release-tooling".into()]);
    }
    if kind == "clippy" {
        args.extend(["--".into(), "-D".into(), "warnings".into()]);
    }
    args
}

fn private_env(
    command: &mut Command,
    target: &Path,
    home: &Path,
    homes: (&Path, &Path),
    jobs: usize,
) {
    let (cargo, rustup) = homes;
    // Allowlist rather than a secret-name blacklist. Real provider credentials,
    // inherited runtimes, compiler wrappers and alternate output paths do not pass.
    command.env_clear();
    for key in [
        "PATH",
        "SystemRoot",
        "WINDIR",
        "COMSPEC",
        "PATHEXT",
        "INCLUDE",
        "LIB",
        "LIBPATH",
        "SYSTEMDRIVE",
        "NUMBER_OF_PROCESSORS",
        "PROCESSOR_ARCHITECTURE",
    ] {
        if let Some(value) = env::var_os(key) {
            command.env(key, value);
        }
    }
    command
        .env("CARGO_HOME", cargo)
        .env("RUSTUP_HOME", rustup)
        .env("CARGO_TARGET_DIR", target)
        .env("CARGO_BUILD_TARGET_DIR", target)
        .env("CARGO_BUILD_BUILD_DIR", target)
        .env("CARGO_BUILD_JOBS", jobs.to_string())
        .env("RUST_TEST_THREADS", jobs.to_string())
        .env("NEXTEST_TEST_THREADS", jobs.to_string())
        .env("RAYON_NUM_THREADS", jobs.to_string())
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("SUSI_HOME", home.join("susi"))
        .env("SUSI_NO_REGISTRY_ENV", "1")
        .env("APPDATA", home.join("appdata"))
        .env("LOCALAPPDATA", home.join("localappdata"))
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env("XDG_STATE_HOME", home.join("state"))
        .env("TMP", home.join("temp"))
        .env("TEMP", home.join("temp"))
        .env("TMPDIR", home.join("temp"))
        .env("GIT_TERMINAL_PROMPT", "0");
}

// A stable bootstrap lives outside the Cargo outputs it can rebuild. Never
// rename/remove the user's executable, particularly on a busy retry.
fn launcher_path(executable: &Path, target: &Path) -> io::Result<PathBuf> {
    if executable.starts_with(target) && !executable.starts_with(target.join("local-tools")) {
        return Err(io::Error::other("use a verified susi-dev launcher in target/local-tools or outside the worktree target; development checks never relocate a running launcher"));
    }
    Ok(executable.to_owned())
}
fn worktree_marker_with(
    path: &Path,
    owner: &str,
    persist: impl FnOnce(&mut fs::File, &[u8]) -> io::Result<()>,
) -> io::Result<()> {
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)?;
    let result = persist(&mut file, owner.as_bytes());
    drop(file);
    if result.is_err() {
        // This attempt created the file and has not started a child.
        let _ = fs::remove_file(path);
    }
    result
}
pub(crate) fn run(args: &[String]) -> io::Result<ExitCode> {
    let total = Instant::now();
    let action = parse(args)?;
    let (cargo_home, rustup_home) = tool_homes()?;
    // Resolve from canonical toolchain storage, not the test HOME/LOCALAPPDATA.
    // Different Cargo homes are different coordination domains, explicitly shown.
    let coordinator = cargo_home.join("susi-dev-capacity-v1");
    let cpus = std::thread::available_parallelism().map_or(1, usize::from);
    let pool = Pool::open(&coordinator, dev_capacity::default_capacity(cpus))?;
    eprintln!("development coordinator: {}; {} job tokens; legacy ship/check gates do not share these tokens", coordinator.display(), pool.capacity());
    let Action::Check {
        kind,
        package,
        jobs,
        wait_ms,
    } = action
    else {
        if let Action::Capacity(agents) = action {
            let jobs = pool.capacity().min(2);
            let concurrent = pool.capacity() / jobs;
            println!("capacity model: agents={agents}, logical_cpus={cpus}, job_tokens={}, jobs_per_check={jobs}, concurrent_checks={concurrent}, waves={}; capacity model only, not measured build time or 100-agent readiness", pool.capacity(), agents.div_ceil(concurrent));
        }
        return Ok(ExitCode::SUCCESS);
    };
    let root = super::workspace(&env::current_dir()?)?;
    let worker = claimed_worktree(&root)?;
    let jobs = jobs.min(pool.capacity());
    let target = root.join("target");
    private_directory(&target)?;
    let check_lock = target.join("check-gate");
    private_directory(&check_lock)?;
    launcher_path(&super::conventional(env::current_exe()?), &target)?;
    let started = Instant::now();
    let (_worktree, lease) = loop {
        if let Some(worktree) = dev_capacity::try_lock(&check_lock.join("gate.lock"))? {
            if check_lock.join("dev-running").try_exists()? {
                return Err(io::Error::other("previous development supervisor ended without confirmed child completion; inspect its process tree before recovery"));
            }
            if let Some(lease) = pool.try_acquire(jobs)? {
                break (worktree, lease);
            }
        }
        if started.elapsed() >= Duration::from_millis(wait_ms) {
            eprintln!("development capacity busy after {} ms; exit 75 is retryable. Continue source/review work and retry; no tests or gates passed", started.elapsed().as_millis());
            return Ok(ExitCode::from(75));
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let home = target
        .join("local-dev-runs")
        .join(format!("{}-{stamp}", std::process::id()));
    for part in [
        "",
        "susi",
        "appdata",
        "localappdata",
        "config",
        "data",
        "cache",
        "state",
        "temp",
    ] {
        private_directory(&home.join(part))?;
    }
    let host = super::host(&root)?;
    let mut command = Command::new("cargo");
    command
        .current_dir(&root)
        .args(cargo_args(&kind, &package, &host, &target, jobs));
    private_env(
        &mut command,
        &target,
        &home,
        (&cargo_home, &rustup_home),
        jobs,
    );
    compiler_env::apply_checked(&mut command)?;
    let owner = format!(
        "pid={} worker={worker} package={package} worktree={}",
        std::process::id(),
        root.display()
    );
    lease.mark_running(&owner)?;
    let worktree_marker = check_lock.join("dev-running");
    if let Err(error) = worktree_marker_with(&worktree_marker, &owner, |file, bytes| {
        file.write_all(bytes)?;
        file.sync_all()
    }) {
        lease.complete()?;
        return Err(error);
    }
    println!("stock-toolchain development diagnostic {kind}: worker={worker}, package={package}, jobs={jobs}, waited_ms={}, warm_target={}; no merge/release receipt", started.elapsed().as_millis(), target.display());
    let execution = Instant::now();
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            lease.complete()?;
            fs::remove_file(&worktree_marker)?;
            return Err(error);
        }
    };
    // On a wait error or supervisor crash keep durable markers: an orphan
    // Cargo process may still be using resources and this target directory.
    let status = child.wait()?;
    lease.complete()?;
    fs::remove_file(&worktree_marker)?;
    println!("stock-toolchain diagnostic result: success={}, command_ms={}, total_ms={}; no qualified sysroot or landing/release evidence; development main remains unqualified, full native qualification is required before release", status.success(), execution.elapsed().as_millis(), total.elapsed().as_millis());
    Ok(ExitCode::from(
        status
            .code()
            .and_then(|c| u8::try_from(c).ok())
            .unwrap_or(1),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| (*s).into()).collect()
    }
    #[test]
    fn prelaunch_worktree_marker_failure_removes_only_its_own_partial_record() {
        let dir = env::temp_dir().join(format!(
            "susi-dev-marker-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&dir).unwrap();
        let path = dir.join("dev-running");
        let result = worktree_marker_with(&path, "owner", |file, _| {
            file.write_all(b"partial")?;
            Err(io::Error::other("injected persistence error"))
        });
        assert!(result.is_err());
        assert!(!path.exists());
        fs::write(&path, "pre-existing owner").unwrap();
        assert!(
            worktree_marker_with(&path, "replacement", |file, bytes| file.write_all(bytes))
                .is_err()
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "pre-existing owner");
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn stable_launcher_is_not_moved_or_removed_on_admission() {
        let target = Path::new("repo/target");
        assert!(launcher_path(Path::new("repo/target/debug/susi-dev"), target).is_err());
        for path in [
            "repo/target/local-tools/debug/susi-dev",
            "private-launcher/susi-dev",
        ] {
            assert_eq!(
                launcher_path(Path::new(path), target).unwrap(),
                PathBuf::from(path)
            );
            assert_eq!(
                launcher_path(Path::new(path), target).unwrap(),
                PathBuf::from(path)
            );
        }
    }
    #[test]
    fn parser_allows_targeted_feedback_and_bounds_waits() {
        assert_eq!(
            parse(&strings(&["capacity"])).unwrap(),
            Action::Capacity(100)
        );
        assert_eq!(
            parse(&strings(&[
                "test",
                "susi-core",
                "--jobs",
                "1",
                "--wait-ms",
                "0"
            ]))
            .unwrap(),
            Action::Check {
                kind: "test".into(),
                package: "susi-core".into(),
                jobs: 1,
                wait_ms: 0
            }
        );
        for args in [
            vec!["ship", "worker"],
            vec!["test", "--workspace"],
            vec!["test", "xtask", "--jobs", "0"],
            vec!["check", "susi-core", "--wait-ms", "60001"],
            vec!["test", "susi", "--target-dir", "shared"],
            vec!["test", "susi", "--jobs", "2", "--jobs", "4"],
        ] {
            assert!(parse(&strings(&args)).is_err());
        }
    }
    #[test]
    fn commands_keep_locked_offline_native_checks_and_doctests() {
        for kind in ["check", "clippy", "test"] {
            let args = cargo_args(
                kind,
                "xtask",
                "x86_64-pc-windows-msvc",
                Path::new("private"),
                2,
            );
            assert!(args.iter().any(|arg| arg == "--offline"));
            assert!(args.iter().any(|arg| arg == "--locked"));
            assert!(args
                .windows(2)
                .any(|a| a == ["--features", "release-tooling"]));
            assert!(!args
                .iter()
                .any(|arg| matches!(arg.as_str(), "--no-run" | "--lib" | "--workspace")));
            assert!(args.windows(2).any(|a| a == ["--jobs", "2"]));
        }
    }
    #[test]
    fn real_linked_worktree_requires_its_matching_native_claim() {
        let root = env::temp_dir().join(format!(
            "susi-dev-claim-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let repo = root.join("repo");
        let work = root.join("worker");
        fs::create_dir_all(&repo).unwrap();
        let invoke = |at: &Path, args: &[&str]| {
            let status = Command::new("git")
                .current_dir(at)
                .args(args)
                .env("GIT_AUTHOR_NAME", "native test")
                .env("GIT_AUTHOR_EMAIL", "native-test@localhost")
                .env("GIT_COMMITTER_NAME", "native test")
                .env("GIT_COMMITTER_EMAIL", "native-test@localhost")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap();
            assert!(status.success());
        };
        invoke(&repo, &["init", "--initial-branch=main"]);
        fs::write(repo.join("source.txt"), "fixture\n").unwrap();
        invoke(&repo, &["add", "source.txt"]);
        invoke(
            &repo,
            &["-c", "commit.gpgsign=false", "commit", "-m", "fixture"],
        );
        assert!(claimed_worktree(&repo).is_err());
        invoke(
            &repo,
            &[
                "worktree",
                "add",
                "-b",
                "local-agent/fixture",
                work.to_str().unwrap(),
            ],
        );
        assert!(claimed_worktree(&work).is_err());
        let claims = repo.join(".git/susi-local-workflow/claims");
        fs::create_dir_all(&claims).unwrap();
        fs::write(
            claims.join("fixture.claim"),
            format!(
                "susi-local-claim-v2\nfixture\n{}\nbase\nsource.txt\n",
                work.display()
            ),
        )
        .unwrap();
        assert_eq!(claimed_worktree(&work).unwrap(), "fixture");
        fs::write(
            claims.join("fixture.claim"),
            format!("susi-local-claim-v2\nfixture\n{}\nbase\n", repo.display()),
        )
        .unwrap();
        assert!(claimed_worktree(&work).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn private_environment_binds_all_outputs_homes_and_budgets() {
        let mut command = Command::new("cargo");
        command
            .env("PROVIDER_API_KEY", "not-a-real-key")
            .env("CARGO_BUILD_JOBS", "100");
        private_env(
            &mut command,
            Path::new("target"),
            Path::new("home"),
            (Path::new("cargo"), Path::new("rustup")),
            2,
        );
        let vars: std::collections::BTreeMap<_, _> = command
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.map(|s| s.to_string_lossy().into_owned()),
                )
            })
            .collect();
        assert!(!vars.contains_key("PROVIDER_API_KEY"));
        for key in [
            "CARGO_TARGET_DIR",
            "CARGO_BUILD_TARGET_DIR",
            "CARGO_BUILD_BUILD_DIR",
        ] {
            assert_eq!(vars[key].as_deref(), Some("target"));
        }
        assert_eq!(vars["CARGO_BUILD_JOBS"].as_deref(), Some("2"));
        assert_eq!(vars["RUST_TEST_THREADS"].as_deref(), Some("2"));
        assert_eq!(vars["HOME"].as_deref(), Some("home"));
        assert_eq!(vars["USERPROFILE"].as_deref(), Some("home"));
        assert_eq!(vars["CARGO_HOME"].as_deref(), Some("cargo"));
    }
}
