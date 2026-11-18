//! Whole-command process boundary and original instance identity.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

fn fixture(root: &Path) -> std::path::PathBuf {
    let source = root.join("runtime.rs");
    let binary = root.join(format!(
        "commercial runtime{}",
        std::env::consts::EXE_SUFFIX
    ));
    fs::write(&source, r#"
use std::{env, io::{self, Read}};
fn main() {
    let args: Vec<_> = env::args_os().skip(1).collect();
    if args.first().is_some_and(|arg| arg == "core-parse") { std::process::exit(64); }
    assert_eq!(args, ["core-runtime", "--", "a goal with spaces", "tab\targument", "literal;$()"].map(std::ffi::OsString::from));
    let mut input = String::new(); io::stdin().read_to_string(&mut input).unwrap();
    assert_eq!(input, "caller stdin\n");
    println!("cwd={}", env::current_dir().unwrap().canonicalize().unwrap().display());
    println!("home={}", env::var("SUSI_HOME").unwrap_or_else(|_| "<unset>".into()));
    println!("ready={}", env::var("SUSI_CORE_INSTANCE_READY").unwrap());
    println!("caller={}", env::var("SUSI_FORWARD_TEST_CALLER").unwrap());
    println!("core={}", env::var_os("SUSI_CORE_BIN").map(std::path::PathBuf::from).unwrap().display());
    eprintln!("inherited stderr");
    std::process::exit(37);
}
"#).unwrap();
    assert!(Command::new("rustc")
        .args(["--edition", "2021"])
        .arg(source)
        .arg("-o")
        .arg(&binary)
        .status()
        .unwrap()
        .success());
    binary
}

fn launch(
    core: &Path,
    runtime: &Path,
    home: &Path,
    cwd: &Path,
    instance: Option<&Path>,
) -> std::process::Output {
    let mut command = Command::new(core);
    susi_paths::test_env::isolate_home(&mut command, home);
    command
        .args(["--", "a goal with spaces", "tab\targument", "literal;$()"])
        .current_dir(cwd)
        .env("SUSI_COMMERCIAL_BIN", runtime)
        .env("SUSI_FORWARD_TEST_CALLER", "verbatim value")
        .env("SUSI_XDG", "1")
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env_remove("SUSI_CORE_INSTANCE_READY")
        .env_remove("SUSI_HOME")
        .env_remove("SUSI_INSTALLED_RELEASE")
        .env_remove("SUSI_WORKER_GUARD")
        .env_remove("SUSI_HERMETIC_FORBIDDEN")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(instance) = instance {
        command.env("SUSI_HOME", instance);
    }
    let mut child = command.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"caller stdin\n")
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn complete_invocation_preserves_native_process_contract_and_instance() {
    let temp = susi_paths::tempfile::tempdir().unwrap();
    let root = temp.path();
    let runtime = fixture(root);
    let cwd = root.join("caller workspace");
    fs::create_dir(&cwd).unwrap();
    let home = root.join("home");
    fs::create_dir(&home).unwrap();
    let core = Path::new(env!("CARGO_BIN_EXE_susi"));
    let private = root.join("selected instance");
    let result = launch(core, &runtime, &home, &cwd, Some(&private));
    assert_eq!(result.status.code(), Some(37));
    let output = String::from_utf8(result.stdout).unwrap();
    assert!(output.contains(&format!("cwd={}", cwd.canonicalize().unwrap().display())));
    assert!(output.contains(&format!("home={}", private.display())));
    assert!(output.contains("ready=1\n") && output.contains("caller=verbatim value\n"));
    assert_eq!(
        String::from_utf8(result.stderr).unwrap().trim(),
        "inherited stderr"
    );

    let result = launch(core, &runtime, &home, &cwd, None);
    assert_eq!(result.status.code(), Some(37));
    let output = String::from_utf8(result.stdout).unwrap();
    assert!(output.contains(&format!(
        "home={}",
        home.join(".susi-dev").join("instances").display()
    )));
    let owners = fs::read_dir(home.join(".susi-dev").join("instances"))
        .unwrap()
        .count();
    assert_eq!(
        owners, 1,
        "the runtime must inherit the original development instance"
    );

    let installed = home
        .join(".susi/bin")
        .join(if cfg!(windows) { "susi.exe" } else { "susi" });
    fs::create_dir_all(installed.parent().unwrap()).unwrap();
    fs::copy(core, &installed).unwrap();
    let result = launch(&installed, &runtime, &home, &cwd, None);
    assert_eq!(result.status.code(), Some(37));
    assert!(String::from_utf8(result.stdout)
        .unwrap()
        .contains("home=<unset>\n"));
}

#[test]
fn bootstrap_parser_errors_and_help_create_no_runtime_instance() {
    let temp = susi_paths::tempfile::tempdir().unwrap();
    let root = temp.path();
    for args in [
        vec!["install", "--invalid"],
        vec!["install", "--help"],
        vec!["--help"],
    ] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_susi"));
        susi_paths::test_env::isolate_home(&mut command, root);
        let output = command
            .args(&args)
            .env_remove("SUSI_HOME")
            .env_remove("SUSI_INSTALLED_RELEASE")
            .env_remove("SUSI_WORKER_GUARD")
            .env("SUSI_COMMERCIAL_BIN", root.join("missing-runtime"))
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(if args.contains(&"--invalid") { 2 } else { 0 })
        );
        assert!(!root.join(".susi-dev").exists());
        assert!(!root.join(".susi").exists());
    }
}

#[test]
fn non_unicode_arguments_keep_the_public_refusal_before_runtime_launch() {
    #[cfg(unix)]
    let argument = {
        use std::os::unix::ffi::OsStringExt;
        std::ffi::OsString::from_vec(vec![0xff])
    };
    #[cfg(windows)]
    let argument = {
        use std::os::windows::ffi::OsStringExt;
        std::ffi::OsString::from_wide(&[0xdcff])
    };
    let temp = susi_paths::tempfile::tempdir().unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_susi"));
    susi_paths::test_env::isolate_home(&mut command, temp.path());
    let result = command
        .arg(argument)
        .env_remove("SUSI_INSTALLED_RELEASE")
        .env("SUSI_COMMERCIAL_BIN", temp.path().join("missing-runtime"))
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(2));
    assert!(result.stdout.is_empty());
    assert_eq!(
        String::from_utf8(result.stderr).unwrap().trim(),
        "command arguments must be valid UTF-8"
    );
}
