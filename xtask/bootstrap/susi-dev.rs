//! Standalone std-only bootstrap: establish private Cargo output before xtask builds.
#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

#[derive(Debug, PartialEq, Eq)]
enum Action {
    Build(String),
    Workflow(Vec<String>),
    Release(Vec<String>),
}

fn parse(args: &[String]) -> io::Result<Action> {
    match args {
        [mode, profile] if mode == "build" && matches!(profile.as_str(), "debug" | "production") => Ok(Action::Build(profile.clone())),
        [mode, rest @ ..] if mode == "workflow" && !rest.is_empty() => Ok(Action::Workflow(rest.to_vec())),
        [mode, rest @ ..] if mode == "release" && !rest.is_empty() => Ok(Action::Release(rest.to_vec())),
        _ => Err(io::Error::other("usage: susi-dev build debug|production | workflow <command> [args] | release <command> [args]")),
    }
}

fn conventional(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        let value = path.as_os_str().to_string_lossy();
        if let Some(rest) = value.strip_prefix(r"\\?\UNC\") {
            return PathBuf::from(format!(r"\\{rest}"));
        }
        if let Some(rest) = value.strip_prefix(r"\\?\") {
            return PathBuf::from(rest);
        }
    }
    path
}

fn workspace(start: &Path) -> io::Result<PathBuf> {
    let root = start
        .ancestors()
        .find(|p| p.join("Cargo.toml").is_file() && p.join("xtask/Cargo.toml").is_file())
        .ok_or_else(|| io::Error::other("run susi-dev from a Susi checkout"))?;
    root.canonicalize().map(conventional)
}

fn directory(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            let redirected = metadata.file_type().is_symlink();
            #[cfg(windows)]
            let redirected = {
                use std::os::windows::fs::MetadataExt;
                redirected || metadata.file_attributes() & 0x400 != 0
            };
            if redirected || !metadata.is_dir() {
                return Err(io::Error::other(format!(
                    "private Cargo output rejects links, junctions and files: {}",
                    path.display()
                )));
            }
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn private_output(root: &Path, host: &str) -> io::Result<PathBuf> {
    let target = root.join("target");
    directory(&target)?;
    let output = target.join("local-tools");
    directory(&output)?;
    directory(&output.join(host))?;
    for profile in [output.join("debug"), output.join(host).join("debug")] {
        directory(&profile)?;
        for subdir in ["deps", "build", "incremental", ".fingerprint", "examples"] {
            directory(&profile.join(subdir))?;
        }
    }
    fs::create_dir_all(&output)?;
    Ok(output)
}

fn host(root: &Path) -> io::Result<String> {
    let output = Command::new("rustc")
        .current_dir(root)
        .arg("-vV")
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other("native Rust compiler is unavailable"));
    }
    parse_host(&String::from_utf8_lossy(&output.stdout), cfg!(windows))
}

fn parse_host(version: &str, windows: bool) -> io::Result<String> {
    let host = version
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .filter(|host| {
            !host.is_empty()
                && host
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        })
        .ok_or_else(|| io::Error::other("compiler did not report a valid host"))?;
    if windows && !host.ends_with("-pc-windows-msvc") {
        return Err(io::Error::other(
            "native Windows development requires the MSVC Rust toolchain",
        ));
    }
    Ok(host.into())
}

fn configure(command: &mut Command, root: &Path, output: &Path, host: &str, action: &Action) {
    // Cargo 1.99 has independent target-dir and build-dir settings. Both must
    // be pinned before the outer `cargo run` compiles even the first xtask crate.
    command
        .current_dir(root)
        .args([
            "run",
            "--offline",
            "--locked",
            "--package",
            "xtask",
            "--target",
            host,
            "--target-dir",
        ])
        .arg(output)
        .env("CARGO_TARGET_DIR", output)
        .env("CARGO_BUILD_TARGET_DIR", output)
        .env("CARGO_BUILD_BUILD_DIR", output)
        .env("CARGO_BUILD_TARGET", host);
    match action {
        Action::Build(profile) => {
            command.args(["--bin", "xtask", "--", "local-build", profile]);
        }
        Action::Workflow(args) => {
            command.args(["--bin", "local-workflow", "--"]).args(args);
        }
        Action::Release(args) => {
            command
                .args([
                    "--features",
                    "release-tooling",
                    "--bin",
                    "local-release",
                    "--",
                ])
                .args(args);
        }
    }
}

fn run(args: &[String]) -> io::Result<ExitCode> {
    let action = parse(args)?;
    let root = workspace(&env::current_dir()?)?;
    let host = host(&root)?;
    let output = private_output(&root, &host)?;
    let mut command = Command::new("cargo");
    configure(&mut command, &root, &output, &host, &action);
    println!(
        "susi-dev: native {host}; private launcher output {}",
        output.display()
    );
    let status = command.status()?;
    Ok(ExitCode::from(
        status
            .code()
            .and_then(|code| u8::try_from(code).ok())
            .unwrap_or(1),
    ))
}

fn main() -> ExitCode {
    match run(&env::args().skip(1).collect::<Vec<_>>()) {
        Ok(status) => status,
        Err(error) => {
            eprintln!("susi-dev: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let path = env::temp_dir().join(format!(
                "susi-bootstrap-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            if !std::thread::panicking() {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
    }

    #[test]
    fn routing_preserves_arguments_and_refuses_ambiguous_builds() {
        assert_eq!(
            parse(&["build".into(), "debug".into()]).unwrap(),
            Action::Build("debug".into())
        );
        assert!(parse(&["build".into(), "debug".into(), "--target-dir=shared".into()]).is_err());
        assert!(parse(&["release".into()]).is_err());
        for (action, bin) in [
            (Action::Build("production".into()), "xtask"),
            (
                Action::Workflow(vec![
                    "commit".into(),
                    "agent".into(),
                    "literal $ and spaces".into(),
                ]),
                "local-workflow",
            ),
            (Action::Release(vec!["status".into()]), "local-release"),
        ] {
            let mut command = Command::new("cargo");
            configure(
                &mut command,
                Path::new("repo"),
                Path::new("private"),
                "x86_64-pc-windows-msvc",
                &action,
            );
            let args: Vec<_> = command
                .get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            assert!(args.windows(2).any(|a| a == ["--bin", bin]));
            if bin == "local-workflow" {
                assert_eq!(args.last().unwrap(), "literal $ and spaces");
            }
            if bin == "local-release" {
                assert!(args
                    .windows(2)
                    .any(|a| a == ["--features", "release-tooling"]));
            }
        }
        assert!(parse_host("host: x86_64-unknown-linux-gnu", true).is_err());
        assert!(parse_host("host: ../../escape", false).is_err());
    }

    #[test]
    fn inherited_shared_output_never_reaches_the_child_process() {
        let scratch = Scratch::new();
        let root = scratch.0.join("repo");
        fs::create_dir(&root).unwrap();
        let shared = scratch.0.join("shared");
        fs::create_dir(&shared).unwrap();
        let source = scratch.0.join("probe.rs");
        fs::write(&source,r#"fn main(){
            let target=std::env::var_os("CARGO_TARGET_DIR").unwrap();
            assert_eq!(std::env::var_os("CARGO_BUILD_TARGET_DIR").unwrap(),target);
            assert_eq!(std::env::var_os("CARGO_BUILD_BUILD_DIR").unwrap(),target);
            assert_eq!(std::env::var("CARGO_BUILD_TARGET").unwrap(),"x86_64-pc-windows-msvc");
            let args:Vec<_>=std::env::args().collect();let index=args.iter().position(|a|a=="--target-dir").unwrap();
            assert_eq!(std::path::PathBuf::from(&args[index+1]),std::path::PathBuf::from(&target));
            std::fs::write(std::path::PathBuf::from(target).join("actual-child-output"),"private").unwrap();
        }"#).unwrap();
        let probe = scratch
            .0
            .join(if cfg!(windows) { "probe.exe" } else { "probe" });
        assert!(Command::new("rustc")
            .args(["--edition=2021"])
            .arg(&source)
            .arg("-o")
            .arg(&probe)
            .status()
            .unwrap()
            .success());
        let private = private_output(&root, "x86_64-pc-windows-msvc").unwrap();
        let mut command = Command::new(&probe);
        command
            .env("CARGO_TARGET_DIR", &shared)
            .env("CARGO_BUILD_TARGET_DIR", &shared)
            .env("CARGO_BUILD_BUILD_DIR", &shared)
            .env("CARGO_BUILD_TARGET", "hostile-target");
        configure(
            &mut command,
            &root,
            &private,
            "x86_64-pc-windows-msvc",
            &Action::Workflow(vec!["list".into()]),
        );
        assert!(command.status().unwrap().success());
        assert!(private.join("actual-child-output").is_file());
        assert_eq!(fs::read_dir(shared).unwrap().count(), 0);
    }

    #[test]
    fn files_and_redirected_targets_fail_before_cargo() {
        let scratch = Scratch::new();
        let root = scratch.0.join("repo");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("target"), "keep").unwrap();
        assert!(private_output(&root, "native").is_err());
        assert_eq!(fs::read_to_string(root.join("target")).unwrap(), "keep");
        fs::remove_file(root.join("target")).unwrap();
        let shared = scratch.0.join("shared");
        fs::create_dir(&shared).unwrap();
        fs::write(shared.join("sentinel"), "keep").unwrap();
        #[cfg(windows)]
        assert!(Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(root.join("target"))
            .arg(&shared)
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap()
            .success());
        #[cfg(unix)]
        std::os::unix::fs::symlink(&shared, root.join("target")).unwrap();
        assert!(private_output(&root, "native").is_err());
        #[cfg(windows)]
        fs::remove_dir(root.join("target")).unwrap();
        #[cfg(unix)]
        fs::remove_file(root.join("target")).unwrap();
        assert_eq!(fs::read_to_string(shared.join("sentinel")).unwrap(), "keep");
    }
}
