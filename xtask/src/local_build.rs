//! Reproducible local builds without shell scripts, remote gates or installation.

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Profile {
    Debug,
    Production,
}

impl Profile {
    fn parse(args: &[String]) -> io::Result<Self> {
        match args {
            [profile] if profile == "debug" => Ok(Self::Debug),
            [profile] if profile == "production" => Ok(Self::Production),
            _ => Err(io::Error::other(
                "usage: cargo run -p xtask --bin xtask -- local-build debug|production|fast-release",
            )),
        }
    }

    fn directory(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Production => "release",
        }
    }
}

fn workspace(start: &Path) -> io::Result<PathBuf> {
    let root = start
        .ancestors()
        .find(|root| root.join("Cargo.toml").is_file() && root.join("xtask/Cargo.toml").is_file())
        .ok_or_else(|| io::Error::other("run local-build from a Susi workspace"))?
        .canonicalize()?;
    // MSVC's include-path handling does not consistently accept Rust's
    // canonical verbatim prefix (\\?\). Keep a conventional absolute path.
    #[cfg(windows)]
    let root = conventional_windows_path(&root);
    Ok(root)
}

#[cfg_attr(not(windows), allow(dead_code))]
fn conventional_windows_path(path: &Path) -> PathBuf {
    let text = path.as_os_str().to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        path.to_path_buf()
    }
}

fn host_from_version(version: &str, windows: bool) -> io::Result<String> {
    let host = version
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .filter(|host| {
            !host.is_empty()
                && host
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        })
        .ok_or_else(|| io::Error::other("rustc did not report a valid host target"))?;
    if windows && !host.ends_with("-pc-windows-msvc") {
        return Err(io::Error::other(
            "Windows local builds require a native Windows MSVC Rust toolchain",
        ));
    }
    Ok(host.to_owned())
}

fn reject_redirect(path: &Path) -> io::Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let redirected = metadata.file_type().is_symlink();
    #[cfg(windows)]
    let redirected = {
        use std::os::windows::fs::MetadataExt;
        // Reject junctions as well as symlinks: both can share another worker's output.
        redirected || metadata.file_attributes() & 0x400 != 0
    };
    if redirected || !metadata.is_dir() {
        return Err(io::Error::other(format!(
            "build output must be a private directory, not a link, reparse point or file: {}",
            path.display()
        )));
    }
    Ok(())
}

fn private_target(root: &Path, host: &str, profile: Profile) -> io::Result<PathBuf> {
    let target = root.join("target").join("local-native");
    // Check every output ancestor, including pre-existing profile redirects.
    for path in [
        root.join("target"),
        target.clone(),
        target.join(host),
        target.join(host).join(profile.directory()),
    ] {
        reject_redirect(&path)?;
    }
    fs::create_dir_all(&target)?;
    Ok(target)
}

fn build_command(root: &Path, target: &Path, host: &str, profile: Profile) -> Command {
    let mut command = Command::new("cargo");
    command
        .current_dir(root)
        .args([
            "build",
            "--locked",
            "--offline",
            "-p",
            "susi",
            "--bin",
            "susi",
        ])
        .args(["--target", host, "--target-dir"])
        .arg(target)
        .args(crate::product_features::product_feature_args(root))
        // Explicit CLI arguments take precedence over user Cargo target settings.
        .env("CARGO_TARGET_DIR", target)
        .env("CARGO_BUILD_TARGET_DIR", target)
        .env("CARGO_BUILD_BUILD_DIR", target)
        .env("CARGO_BUILD_TARGET", host);
    if profile == Profile::Production {
        command.arg("--release");
    }
    command
}

pub(crate) fn run(args: &[String]) -> io::Result<()> {
    if args.first().map(String::as_str) == Some("fast-release") {
        return crate::fast_release::run_io(&args[1..]);
    }
    let profile = Profile::parse(args)?;
    let root = workspace(&env::current_dir()?)?;
    let version = Command::new("rustc")
        .current_dir(&root)
        .arg("-vV")
        .output()?;
    if !version.status.success() {
        return Err(io::Error::other(
            "could not identify the native Rust compiler",
        ));
    }
    let host = host_from_version(&String::from_utf8_lossy(&version.stdout), cfg!(windows))?;
    if profile == Profile::Production {
        let status = Command::new("cargo")
            .current_dir(&root)
            .args(["deny", "--offline", "--locked", "check", "licenses"])
            .status()?;
        if !status.success() {
            return Err(io::Error::other(
                "production candidate blocked: strict license audit must pass before building",
            ));
        }
    }
    let target = private_target(&root, &host, profile)?;
    println!(
        "xtask: local {} build for {host}; locked dependencies, offline Cargo, private output {}",
        profile.directory(),
        target.display()
    );
    let started = std::time::Instant::now();
    let mut command = build_command(&root, &target, &host, profile);
    let sysroot = super::trusted_sysroot::prepare(&root)
        .map_err(|error| io::Error::other(error.to_string()))?;
    sysroot
        .apply(&mut command)
        .map_err(|error| io::Error::other(error.to_string()))?;
    let status = command.status()?;
    sysroot
        .recheck()
        .map_err(|error| io::Error::other(error.to_string()))?;
    if !status.success() {
        return Err(io::Error::other(format!(
            "local build failed ({status}); see the compiler diagnostics above. If a dependency is missing, provision it before retrying offline"
        )));
    }
    let executable = target
        .join(host)
        .join(profile.directory())
        .join(if cfg!(windows) { "susi.exe" } else { "susi" });
    if !executable.is_file() {
        return Err(io::Error::other(
            "Cargo succeeded but the Susi executable is missing",
        ));
    }
    println!(
        "xtask: built {}; build elapsed-ms={}; artifact-bytes={}; candidate only, not qualified or installed",
        executable.display(), started.elapsed().as_millis(), fs::metadata(&executable)?.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn msvc_paths_preserve_drive_and_unc_roots_without_verbatim_prefix() {
        assert_eq!(
            conventional_windows_path(Path::new(r"\\?\F:\src\susi")),
            PathBuf::from(r"F:\src\susi")
        );
        assert_eq!(
            conventional_windows_path(Path::new(r"\\?\UNC\server\share\susi")),
            PathBuf::from(r"\\server\share\susi")
        );
        assert_eq!(
            conventional_windows_path(Path::new("/tmp/susi")),
            PathBuf::from("/tmp/susi")
        );
    }

    #[test]
    fn windows_rejects_gnu_and_linux_toolchains() {
        for host in ["x86_64-pc-windows-gnu", "x86_64-unknown-linux-gnu"] {
            assert!(host_from_version(&format!("host: {host}\n"), true).is_err());
        }
        assert_eq!(
            host_from_version("host: aarch64-pc-windows-msvc\n", true).unwrap(),
            "aarch64-pc-windows-msvc"
        );
        assert!(host_from_version("host: ../../shared", false).is_err());
    }

    #[test]
    fn profiles_are_explicit_and_unknown_options_fail() {
        assert_eq!(Profile::parse(&["debug".into()]).unwrap(), Profile::Debug);
        assert_eq!(
            Profile::parse(&["production".into()]).unwrap(),
            Profile::Production
        );
        assert!(Profile::parse(&[]).is_err());
        assert!(Profile::parse(&["debug".into(), "--features=cuda".into()]).is_err());
    }

    #[test]
    fn build_is_native_offline_scoped_and_uses_explicit_private_output() {
        let command = build_command(
            Path::new("workspace"),
            Path::new("private-target"),
            "x86_64-pc-windows-msvc",
            Profile::Production,
        );
        assert_eq!(command.get_program(), "cargo");
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "build",
                "--locked",
                "--offline",
                "-p",
                "susi",
                "--bin",
                "susi",
                "--target",
                "x86_64-pc-windows-msvc",
                "--target-dir",
                "private-target",
                "--no-default-features",
                "--release"
            ]
        );
        assert!(command
            .get_envs()
            .any(|(name, value)| name == "CARGO_TARGET_DIR"
                && value == Some(std::ffi::OsStr::new("private-target"))));
    }

    #[test]
    fn output_file_is_rejected_without_modifying_it() {
        let path = env::temp_dir().join(format!("susi-local-build-file-{}", std::process::id()));
        fs::write(&path, "preserve").unwrap();
        assert!(reject_redirect(&path).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "preserve");
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn private_target_creates_local_native_and_rejects_nested_file_collision() {
        let dir = env::temp_dir().join(format!("susi-local-private-target-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let host = "x86_64-pc-windows-msvc";
        let target = private_target(&dir, host, Profile::Debug).unwrap();
        assert_eq!(target, dir.join("target").join("local-native"));
        assert!(target.is_dir());

        let host_dir = target.join(host);
        fs::create_dir_all(&host_dir).unwrap();
        fs::write(host_dir.join("debug"), "not-a-directory").unwrap();
        assert!(private_target(&dir, host, Profile::Debug).is_err());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn workspace_locates_ancestor_with_cargo_and_xtask_manifests_and_rejects_unrelated_dir() {
        let base = env::temp_dir().join(format!("susi-local-ws-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let ws_root = base.join("repo");
        let nested = ws_root.join("crates/sub/src");
        let unrelated = base.join("outside");
        fs::create_dir_all(ws_root.join("xtask")).unwrap();
        fs::create_dir_all(&nested).unwrap();
        fs::create_dir_all(&unrelated).unwrap();
        fs::write(ws_root.join("Cargo.toml"), "[workspace]\n").unwrap();
        fs::write(
            ws_root.join("xtask/Cargo.toml"),
            "[package]\nname = \"xtask\"\n",
        )
        .unwrap();

        let resolved = workspace(&nested).unwrap();
        assert!(resolved.join("xtask/Cargo.toml").is_file());
        assert!(workspace(&unrelated).is_err());

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn debug_build_command_omits_release_flag_and_uses_debug_directory() {
        assert_eq!(Profile::Debug.directory(), "debug");
        assert_eq!(Profile::Production.directory(), "release");
        let command = build_command(
            Path::new("workspace"),
            Path::new("private-target"),
            "x86_64-unknown-linux-gnu",
            Profile::Debug,
        );
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(!args.iter().any(|arg| arg == "--release"));
        assert!(args.iter().any(|arg| arg == "--no-default-features"));
    }
}
