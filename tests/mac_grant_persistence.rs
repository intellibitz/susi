//! Exercise capability grants from separate processes through a private installed user PATH.
#![allow(clippy::unwrap_used)] // Any failed fixture operation must fail the integration test.
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use susi_paths::test_env::isolate_home;

struct Account {
    root: PathBuf,
    home: PathBuf,
    runtime: PathBuf,
    path: std::ffi::OsString,
}

impl Account {
    fn install() -> Self {
        let root = std::env::temp_dir().join(format!(
            "susi-grants-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let home = root.join("home");
        let runtime = home.join(".susi");
        std::fs::create_dir_all(&runtime).unwrap();
        let mut paths = vec![runtime.join("bin")];
        if let Some(windows) = std::env::var_os("SystemRoot") {
            paths.push(PathBuf::from(windows).join("System32"));
        }
        let account = Self {
            root,
            home,
            runtime,
            path: std::env::join_paths(paths).unwrap(),
        };
        let result = account.run(Path::new(env!("CARGO_BIN_EXE_susi")), &["install"]);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        account
    }

    fn run(&self, exe: &Path, args: &[&str]) -> Output {
        let mut command = Command::new(exe);
        command.env_clear();
        for key in ["SystemRoot", "WINDIR", "ComSpec", "SystemDrive"] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        isolate_home(&mut command, &self.home)
            .env("SUSI_HOME", &self.runtime)
            .env("PATH", &self.path)
            .current_dir(&self.root)
            .args(args)
            .output()
            .unwrap()
    }

    fn privacy(&self, args: &[&str]) -> Output {
        let mut command = vec!["privacy"];
        command.extend_from_slice(args);
        // Deliberately use the command a user types. The first search path
        // contains only this account's natively installed executable.
        self.run(Path::new("susi"), &command)
    }

    fn json(&self, args: &[&str]) -> Value {
        let result = self.privacy(args);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        serde_json::from_slice(&result.stdout).unwrap()
    }

    fn contains(&self, resource: &str) -> bool {
        self.json(&["status"])["grants"]
            .as_array()
            .unwrap()
            .iter()
            .any(|grant| grant["action"] == "process.exec" && grant["resource"] == resource)
    }
}

#[test]
fn installed_grants_survive_long_scopes_and_revoke_legacy_records() {
    let account = Account::install();
    account.json(&["mode", "local_only"]);
    let resource = account
        .root
        .join("optional peers 原生")
        .join("r".repeat(160))
        .join("native 工具.exe");
    std::fs::create_dir_all(resource.parent().unwrap()).unwrap();
    let installed = account
        .runtime
        .join("bin")
        .join(format!("susi{}", std::env::consts::EXE_SUFFIX));
    if std::fs::hard_link(&installed, &resource).is_err() {
        std::fs::copy(&installed, &resource).unwrap();
    }
    assert!(resource.is_file());
    let resource = resource.to_str().unwrap();
    let token = account.json(&["grant", "--action", "process.exec", "--resource", resource]);
    assert_eq!(token["resource"], resource);
    assert!(account.contains(resource));
    let dir = account.runtime.join("mac.grants");
    let records = || {
        std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect::<Vec<_>>()
    };
    let persisted = records()
        .into_iter()
        .find(|p| {
            std::fs::read(p).is_ok_and(|b| {
                serde_json::from_slice::<Value>(&b).is_ok_and(|v| v["resource"] == resource)
            })
        })
        .unwrap();
    assert!(persisted.file_name().unwrap().len() < 80);
    assert_eq!(
        account.json(&["revoke", "--action", "process.exec", "--resource", resource])["revoked"],
        true
    );
    assert!(!account.contains(resource));

    let legacy = "legacy-peer";
    account.json(&["grant", "--action", "process.exec", "--resource", legacy]);
    let canonical = records()
        .into_iter()
        .find(|p| {
            std::fs::read(p).is_ok_and(|b| {
                serde_json::from_slice::<Value>(&b).is_ok_and(|v| v["resource"] == legacy)
            })
        })
        .unwrap();
    let old = dir.join("susi%7Cprocess.exec%7Clegacy-peer");
    std::fs::rename(canonical, &old).unwrap();
    assert!(account.contains(legacy));
    assert_eq!(
        account.json(&["revoke", "--action", "process.exec", "--resource", legacy])["revoked"],
        true
    );
    assert!(!account.contains(legacy));
    assert!(!old.exists());
    std::fs::remove_dir_all(account.root).unwrap();
}

#[test]
fn installed_cli_reports_persistence_failures_for_grant_revoke_and_consent() {
    let account = Account::install();
    let dir = account.runtime.join("mac.grants");
    if dir.is_dir() {
        std::fs::remove_dir_all(&dir).unwrap();
    }
    std::fs::write(&dir, "unavailable storage").unwrap();
    for args in [
        &[
            "grant",
            "--action",
            "process.exec",
            "--resource",
            "failed-peer",
        ][..],
        &[
            "revoke",
            "--action",
            "process.exec",
            "--resource",
            "failed-peer",
        ][..],
        &["consent", "--egress"][..],
    ] {
        let result = account.privacy(args);
        assert!(!result.status.success(), "{:?}", args);
        assert!(!result.stderr.is_empty());
        assert!(result.stdout.is_empty());
    }
    assert!(!account.contains("failed-peer"));
    std::fs::remove_dir_all(account.root).unwrap();
}

#[test]
fn an_unavailable_shared_key_cannot_report_a_process_local_grant_as_durable() {
    let account = Account::install();
    std::fs::write(account.runtime.join("mac.hmac.key"), "invalid").unwrap();
    let result = account.privacy(&[
        "grant",
        "--action",
        "network.egress",
        "--resource",
        "https://peer.invalid",
    ]);
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("shared MAC signing key is unavailable")
    );
    std::fs::remove_dir_all(account.root).unwrap();
}

#[test]
fn concurrent_installed_writers_keep_case_distinct_endpoint_scopes() {
    let account = Account::install();
    account.json(&["mode", "local_only"]);
    let endpoints = ["https://peer.invalid/Tools", "https://peer.invalid/tools"];
    std::thread::scope(|threads| {
        for endpoint in endpoints {
            let account = &account;
            threads.spawn(move || {
                account.json(&[
                    "grant",
                    "--action",
                    "network.egress",
                    "--resource",
                    endpoint,
                ]);
            });
        }
    });
    let status = account.json(&["status"]);
    for endpoint in endpoints {
        assert!(status["grants"]
            .as_array()
            .unwrap()
            .iter()
            .any(|g| g["action"] == "network.egress" && g["resource"] == endpoint));
    }
    account.json(&[
        "revoke",
        "--action",
        "network.egress",
        "--resource",
        endpoints[0],
    ]);
    let after = account.json(&["status"]);
    let grants = after["grants"].as_array().unwrap();
    assert!(!grants.iter().any(|g| g["resource"] == endpoints[0]));
    assert!(grants.iter().any(|g| g["resource"] == endpoints[1]));
    std::fs::remove_dir_all(account.root).unwrap();
}

#[test]
fn unit_isolate_home_sets_isolated_environment_variables() {
    let scratch = std::env::temp_dir().join(format!("susi-grants-unit-{}", std::process::id()));
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_susi"));
    isolate_home(&mut cmd, &scratch);
    let has_scratch_env = cmd
        .get_envs()
        .filter_map(|(_, v)| v.map(PathBuf::from))
        .any(|p| p.starts_with(&scratch));
    assert!(has_scratch_env);
}
