#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use susi_gawd::audit_evidence::AuditEvidence;

fn run(home: &Path, args: &[&str]) -> (i32, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_susi"))
        .args(args)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("xdg"))
        .env("XDG_DATA_HOME", home.join("xdg-data"))
        .env_remove("SUSI_HOME")
        .output()
        .unwrap();
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn root() -> PathBuf {
    static NEXT_ROOT: AtomicUsize = AtomicUsize::new(0);
    let serial = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "susi-vc-201-079-cli-{}-{serial}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("home")).unwrap();
    root
}

fn source_export(path: &Path) {
    let mut evidence = AuditEvidence::new();
    evidence.register_key("k1", "super-secret");
    evidence
        .append("action=login actor=redacted", "k1")
        .unwrap();
    evidence.append("action=read path=redacted", "k1").unwrap();
    std::fs::write(path, serde_json::to_vec(&evidence.export()).unwrap()).unwrap();
}

fn export_file(root: &Path) -> PathBuf {
    let raw = root.join("raw.json");
    let exported = root.join("evidence.json");
    source_export(&raw);
    let raw_arg = raw.to_str().unwrap();
    let output_arg = exported.to_str().unwrap();
    let (code, _, err) = run(
        &root.join("home"),
        &[
            "admin",
            "audit-evidence",
            "export",
            "--input",
            raw_arg,
            "--output",
            output_arg,
        ],
    );
    assert_eq!(code, 0, "{err}");
    exported
}

fn truncate(value: &mut serde_json::Value) {
    value["segments"].as_array_mut().unwrap().pop();
}

fn tamper(value: &mut serde_json::Value) {
    value["segments"][1]["redacted_body"] = "evil".into();
}

fn remove_first(value: &mut serde_json::Value) {
    value["expected_len"] = 1.into();
    value["segments"].as_array_mut().unwrap().remove(0);
}

#[test]
fn vc_201_079_cli_verify_round_trips_a_file_without_leaking_secrets() {
    let root = root();
    let exported = export_file(&root);
    let path = exported.to_str().unwrap();
    let (code, stdout, stderr) = run(
        &root.join("home"),
        &[
            "admin",
            "audit-evidence",
            "verify",
            "--input",
            path,
            "--key",
            "k1=super-secret",
        ],
    );
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("Audit evidence verified: 2 segments"));
    assert!(!stdout.contains("super-secret"));
    assert!(!stderr.contains("super-secret"));
    let file = std::fs::read_to_string(exported).unwrap();
    assert!(!file.contains("super-secret"));
}

#[test]
fn vc_201_079_cli_verify_reports_distinct_failures() {
    let root = root();
    let exported = export_file(&root);
    let original: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&exported).unwrap()).unwrap();
    let path = exported.to_str().unwrap();

    for (name, mutate, expected) in [
        (
            "truncated",
            truncate as fn(&mut serde_json::Value),
            "truncation",
        ),
        (
            "tampered",
            tamper as fn(&mut serde_json::Value),
            "tampering",
        ),
        (
            "missing",
            remove_first as fn(&mut serde_json::Value),
            "missing segment",
        ),
    ] {
        let mut changed = original.clone();
        mutate(&mut changed);
        std::fs::write(&exported, serde_json::to_vec(&changed).unwrap()).unwrap();
        let (code, _, stderr) = run(
            &root.join("home"),
            &[
                "admin",
                "audit-evidence",
                "verify",
                "--input",
                path,
                "--key",
                "k1=super-secret",
            ],
        );
        assert_ne!(code, 0, "{name} unexpectedly passed");
        assert!(stderr.contains(expected), "{name}: {stderr}");
    }

    std::fs::write(&exported, serde_json::to_vec(&original).unwrap()).unwrap();
    let (code, _, stderr) = run(
        &root.join("home"),
        &[
            "admin",
            "audit-evidence",
            "verify",
            "--input",
            path,
            "--key",
            "other=super-secret",
        ],
    );
    assert_ne!(code, 0);
    assert!(stderr.contains("unknown key"), "{stderr}");
}
