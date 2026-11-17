use super::command;
use crate::cli::defs::{Cli, Commands};
use clap::Parser;
use std::ffi::OsString;
use std::io::Write;
use std::process::{Command, Stdio};

#[test]
fn provider_forward_keeps_public_aliases_and_errors() {
    for family in [
        "brain",
        "models",
        "openweight",
        "ow",
        "frontier",
        "fm",
        "openrouter",
        "or",
        "keys",
        "key",
    ] {
        assert!(Cli::try_parse_from(["susi", family]).is_ok(), "{family}");
    }
    assert!(matches!(
        Cli::try_parse_from(["susi", "key", "list"])
            .unwrap()
            .command,
        Some(Commands::Keys { .. })
    ));
    assert_eq!(
        Cli::try_parse_from(["susi", "brain", "status", "--top", "nope"])
            .err()
            .unwrap()
            .exit_code(),
        2
    );
    assert_eq!(
        Cli::try_parse_from(["susi", "brain", "--help"])
            .err()
            .unwrap()
            .exit_code(),
        0
    );
}

#[test]
fn provider_forward_transports_arguments_streams_directory_and_exit_code() {
    let home = std::env::temp_dir().join(format!(
        "susi-provider-forward-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&home).unwrap();
    let source = home.as_path().join("fixture.rs");
    std::fs::write(&source, r#"
use std::io::Read;
fn main() {
    let words: Vec<_> = std::env::args_os().skip(1).collect();
    assert_eq!(words[0], "core-provider-cli");
    assert_eq!(words[1], "ow");
    assert_eq!(std::env::var_os("SUSI_CORE_BIN").unwrap(), std::env::current_dir().unwrap().join("original-core"));
    assert_eq!(std::fs::read("relative-marker").unwrap(), b"same-cwd");
    assert_eq!(std::env::var_os("PATH"), std::env::var_os("SUSI_EXPECTED_PATH"));
    let mut input = String::new(); std::io::stdin().read_to_string(&mut input).unwrap();
    print!("{}", input);
    for arg in &words[2..] { for byte in arg.as_encoded_bytes() { print!("{byte:02x}"); } println!(); }
    eprintln!("inherited stderr");
    std::process::exit(2);
}
"#).unwrap();
    let fixture = home
        .as_path()
        .join(format!("fixture{}", std::env::consts::EXE_SUFFIX));
    assert!(Command::new("rustc")
        .arg("--edition=2021")
        .arg(&source)
        .arg("-o")
        .arg(&fixture)
        .status()
        .unwrap()
        .success());
    std::fs::write(home.as_path().join("relative-marker"), b"same-cwd").unwrap();
    let mut words = vec![
        OsString::from("ow"),
        OsString::from("path with spaces"),
        OsString::from(""),
    ];
    #[cfg(unix)]
    let extra = {
        use std::os::unix::ffi::OsStringExt;
        OsString::from_vec(vec![b'p', 0xff])
    };
    #[cfg(not(unix))]
    let extra = OsString::from("unicode-λ");
    words.push(extra);
    let expected: String = words[1..]
        .iter()
        .map(|word| {
            let mut line: String = word
                .as_encoded_bytes()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            line.push('\n');
            line
        })
        .collect();
    let mut child = command(
        &fixture,
        home.as_path(),
        &home.as_path().join("original-core"),
        &words,
    )
    .env(
        "SUSI_EXPECTED_PATH",
        std::env::var_os("PATH").unwrap_or_default(),
    )
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"piped input\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        String::from_utf8(output.stdout)
            .unwrap()
            .replace("\r\n", "\n"),
        format!("piped input\n{expected}")
    );
    assert_eq!(
        String::from_utf8(output.stderr).unwrap().trim(),
        "inherited stderr"
    );
    std::fs::remove_dir_all(home).unwrap();
}

#[test]
fn provider_forward_missing_suite_names_the_serving_command() {
    const CHILD: &str = "SUSI_PROVIDER_FORWARD_MISSING_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let error = super::execute(None).unwrap_err().to_string();
        assert!(error.contains("Susi Commercial Apps"));
        assert!(error.contains("susi-commercial core-provider-cli"));
        return;
    }
    let test = format!(
        "{}::provider_forward_missing_suite_names_the_serving_command",
        module_path!().split_once("::").unwrap().1
    );
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", &test])
        .env(CHILD, "1")
        .env(
            "SUSI_COMMERCIAL_BIN",
            std::env::temp_dir().join(format!("missing-suite-{}", std::process::id())),
        )
        .status()
        .unwrap();
    assert!(status.success());
}
