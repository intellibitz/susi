#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![allow(missing_docs)]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn script_command(script: &Path, root: &Path) -> Command {
    #[cfg(windows)]
    let mut command = {
        let mut command = Command::new("sh");
        command.arg(script);
        command
    };
    #[cfg(not(windows))]
    let mut command = {
        let mut command = Command::new("bash");
        command.arg(script);
        command
    };
    command
        .arg(root)
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn workspace() -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "susi-unsafe-scope-{}-{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("test")
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("src/admin")).unwrap();
    fs::create_dir_all(root.join("tools")).unwrap();
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"scope-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[[bin]]\nname = \"custom-tool\"\npath = \"tools/custom.rs\"\n",
    )
    .unwrap();
    fs::write(root.join("src/lib.rs"), "#![forbid(unsafe_code)]\n").unwrap();
    fs::write(root.join("src/admin/release.rs"), "pub fn release() {}\n").unwrap();
    fs::write(root.join("tools/custom.rs"), "fn main() {}\n").unwrap();
    root
}

fn should_run(script: &Path, root: &Path, paths: &[&str]) -> bool {
    let mut command = script_command(script, root).spawn().unwrap();
    let mut stdin = command.stdin.take().unwrap();
    writeln!(stdin, "{}", paths.join("\n")).unwrap();
    drop(stdin);
    let output = command.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "scope selector failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim() == "true"
}

#[test]
fn unsafe_ratchet_scope_tracks_geiger_entry_points_and_inputs() {
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.github/workflows/unsafe-exposure-scope.sh");
    let root = workspace();

    assert!(!should_run(&script, &root, &["src/admin/release.rs"]));
    assert!(should_run(&script, &root, &["src/lib.rs"]));
    assert!(should_run(&script, &root, &["tools/custom.rs"]));
    assert!(should_run(&script, &root, &["generated/custom.rs"]));
    assert!(should_run(&script, &root, &["crates/example/Cargo.toml"]));
    assert!(should_run(&script, &root, &["Cargo.lock"]));
    assert!(should_run(
        &script,
        &root,
        &[".agents/baseline/unsafe-default.json"],
    ));

    let workflow = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.github/workflows/test.yml"),
    )
    .unwrap();
    assert!(workflow.contains("unsafe-exposure-scope.sh"));
    assert!(workflow.contains("run=true"));
    let _ = fs::remove_dir_all(root);
}
