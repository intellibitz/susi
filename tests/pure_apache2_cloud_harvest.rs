#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! End-to-end integration proof for Mandate 75 Cloud Raw-Source Pickup, Smart
//! Core-vs-Commercial Routing, and Pure `Apache-2.0` License Compliance:
//! 1. `local-harvest raw-features` discovers raw-branch `Task:` / `Feature:`
//!    commits and pure-`Apache-2.0` Rust modules from a bare cloud Git repo
//!    inside quarantine (`refs/susi/upstream/...`) without checking out code.
//! 2. `local-harvest pickup` imports pure `Apache-2.0` and dual-licensed
//!    (`MIT OR Apache-2.0` / `LICENSE-APACHE` + `LICENSE-MIT` -> elected
//!    `"Apache-2.0"` exclusively) capabilities with full Apache-2.0 §4(a)–(d)
//!    artifacts (`LICENSES/Apache-2.0.txt`, deterministic `THIRD_PARTY_NOTICES`,
//!    copyright preservation, and `Modified by` notice enforcement).
//! 3. `MIT`-only, `BSD-3-Clause`-only, `Apache-2.0 WITH LLVM-exception`, `ISC`,
//!    `GPL-3.0`, and unlicensed cloud repos are **hard-refused** with zero
//!    files written.
//! 4. UI, enterprise, SSO/RBAC/FinOps, connector, and commercial pack targets
//!    are **routed to `commercial` and refused in `susi` Core**, while accepted
//!    when targeting a `susi-commercial-apps` checkout.
//! 5. A picked-up commit on Raw `main` passes both `local-harvest audit` and
//!    `distill lint --strict` for Release distillation!

use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

const APACHE_LICENSE_TEXT: &str = "\
                                 Apache License
                           Version 2.0, January 2004
                        http://www.apache.org/licenses/

   TERMS AND CONDITIONS FOR USE, REPRODUCTION, AND DISTRIBUTION

   4. Redistribution. You may reproduce and distribute copies of the
      Work or Derivative Works thereof in any medium, with or without
      modifications, provided that You meet the following conditions:
      (a) You must give any other recipients of the Work a copy of this License;
      (b) You must cause any modified files to carry prominent notices;
      (c) You must retain all copyright, patent, trademark, and attribution notices;
      (d) If the Work includes a NOTICE text file, any Derivative Works must include it.
";

const MIT_LICENSE_TEXT: &str = "\
MIT License

Copyright (c) 2025 Dual Crate Contributors

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the \"Software\"), to deal
in the Software without restriction.
";

const BSD3_LICENSE_TEXT: &str = "\
Copyright (c) 2025 BSD Corp.
Redistribution and use in source and binary forms, with or without modification,
are permitted provided that the following conditions are met:
Neither the name of the copyright holder nor the names of its contributors may be used.
";

const ISC_LICENSE_TEXT: &str = "\
Copyright (c) 2025 ISC Author
Permission to use, copy, modify, and/or distribute this software for any purpose
with or without fee is hereby granted.
";

const GPL3_LICENSE_TEXT: &str = "\
                    GNU GENERAL PUBLIC LICENSE
                       Version 3, 29 June 2007
";

static SANDBOX_SEQ: AtomicUsize = AtomicUsize::new(0);
static XTASK_BINS: OnceLock<(PathBuf, PathBuf)> = OnceLock::new();

fn newest_mtime(path: &Path) -> Option<SystemTime> {
    let meta = fs::symlink_metadata(path).ok()?;
    let mut newest = meta.modified().ok();
    if meta.is_dir() {
        let entries = fs::read_dir(path).ok()?;
        for entry in entries.flatten() {
            let name = entry.file_name();
            if matches!(name.to_str(), Some("target" | ".git")) {
                continue;
            }
            if let Some(sub) = newest_mtime(&entry.path()) {
                newest = Some(newest.map_or(sub, |cur| cur.max(sub)));
            }
        }
    }
    newest
}

fn bins_are_fresh(workspace_root: &Path, bins: &[&Path]) -> bool {
    let mut oldest_bin: Option<SystemTime> = None;
    for bin in bins {
        let Ok(meta) = fs::metadata(bin) else {
            return false;
        };
        if !meta.is_file() {
            return false;
        }
        let Ok(mtime) = meta.modified() else {
            return false;
        };
        oldest_bin = Some(oldest_bin.map_or(mtime, |cur| cur.min(mtime)));
    }
    let Some(oldest_bin) = oldest_bin else {
        return false;
    };
    let watched = [
        "Cargo.toml",
        "Cargo.lock",
        "xtask",
        "crates/susi-paths",
        "crates/susi-config",
        "crates/susi-build-policy",
        "crates/susi-build-os-ucrt",
        "crates/susi-windows-runtime",
        "crates/susi-vendor-sha2",
        "crates/susi-vendor-serde-json",
    ];
    for rel in watched {
        let path = workspace_root.join(rel);
        if path.exists() {
            let Some(src_mtime) = newest_mtime(&path) else {
                return false;
            };
            if src_mtime > oldest_bin {
                return false;
            }
        }
    }
    true
}

fn xtask_bins() -> &'static (PathBuf, PathBuf) {
    XTASK_BINS.get_or_init(|| {
        let exe = std::env::current_exe().expect("current test executable path");
        let profile_dir = exe
            .parent()
            .and_then(Path::parent)
            .expect("target/<profile> directory");
        let suffix = std::env::consts::EXE_SUFFIX;
        let harvest_bin = profile_dir.join(format!("local-harvest{suffix}"));
        let distill_bin = profile_dir.join(format!("distill{suffix}"));
        let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        if !bins_are_fresh(
            &workspace_root,
            &[harvest_bin.as_path(), distill_bin.as_path()],
        ) {
            let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
            let target_dir = profile_dir.parent().expect("target root directory");
            let mut build_cmd = Command::new(cargo);
            build_cmd
                .current_dir(&workspace_root)
                .env("CARGO_TARGET_DIR", target_dir)
                .args([
                    "build",
                    "--offline",
                    "--locked",
                    "-p",
                    "xtask",
                    "--bin",
                    "local-harvest",
                    "--bin",
                    "distill",
                ]);
            if let Some(profile_name) = profile_dir.file_name().and_then(|n| n.to_str()) {
                if profile_name == "release" {
                    build_cmd.arg("--release");
                } else if profile_name != "debug" {
                    build_cmd.args(["--profile", profile_name]);
                }
            }
            let out = build_cmd
                .output()
                .expect("build local-harvest and distill binaries");
            assert!(
                out.status.success(),
                "building xtask binaries failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        assert!(harvest_bin.is_file(), "missing {}", harvest_bin.display());
        assert!(distill_bin.is_file(), "missing {}", distill_bin.display());
        (harvest_bin, distill_bin)
    })
}

fn null_config() -> &'static str {
    if cfg!(windows) {
        "NUL"
    } else {
        "/dev/null"
    }
}

struct CloudSandbox {
    dir: PathBuf,
}

impl CloudSandbox {
    fn new(label: &str) -> Self {
        let seq = SANDBOX_SEQ.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "susi-pure-apache2-{label}-{}-{seq}-{nanos}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create sandbox dir");
        Self { dir }
    }

    /// Creates a local bare Git "cloud" repository populated directly via
    /// `git fast-import`, returning `(bare_repo_path, head_sha)`.
    fn create_bare_cloud_repo(&self, name: &str, commits: &[CommitSpec<'_>]) -> (PathBuf, String) {
        let bare = self.dir.join(format!("{name}.git"));
        git(
            &self.dir,
            &[
                "init",
                "--quiet",
                "--bare",
                "--initial-branch=main",
                bare.to_str().unwrap(),
            ],
        );
        let mut stream = Vec::with_capacity(4096);
        for (idx, (msg, files)) in commits.iter().enumerate() {
            let mark = idx + 1;
            let ts = 1_700_000_000usize + idx;
            writeln!(stream, "commit refs/heads/main").unwrap();
            writeln!(stream, "mark :{mark}").unwrap();
            writeln!(
                stream,
                "author Susi Harvest Test <harvest-test@susi.local> {ts} +0000"
            )
            .unwrap();
            writeln!(
                stream,
                "committer Susi Harvest Test <harvest-test@susi.local> {ts} +0000"
            )
            .unwrap();
            writeln!(stream, "data {}", msg.len()).unwrap();
            stream.extend_from_slice(msg.as_bytes());
            stream.push(b'\n');
            if idx > 0 {
                writeln!(stream, "from :{idx}").unwrap();
            }
            for (rel, content) in *files {
                writeln!(stream, "M 100644 inline {rel}").unwrap();
                writeln!(stream, "data {}", content.len()).unwrap();
                stream.extend_from_slice(content.as_bytes());
                stream.push(b'\n');
            }
            stream.push(b'\n');
        }
        git_stdin(&bare, &["fast-import", "--quiet"], &stream);
        let head = fs::read_to_string(bare.join("refs/heads/main"))
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| s.len() == 40)
            .unwrap_or_else(|| git(&bare, &["rev-parse", "refs/heads/main"]));
        (bare, head)
    }

    fn create_core_worktree(&self, name: &str) -> PathBuf {
        let repo = self.dir.join(name);
        fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "--quiet", "--initial-branch=main"]);
        write_rel(
            &repo,
            "Cargo.toml",
            "[package]\nname = \"susi\"\nversion = \"0.21.63\"\nedition = \"2021\"\n",
        );
        write_rel(&repo, "LICENSE", APACHE_LICENSE_TEXT);
        git(&repo, &["add", "-A"]);
        git(
            &repo,
            &[
                "commit",
                "-q",
                "-m",
                "chore: init core repo\n\nTask: T-INIT-1",
            ],
        );
        repo
    }

    fn create_commercial_worktree(&self, name: &str) -> PathBuf {
        let repo = self.dir.join(name);
        fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "--quiet", "--initial-branch=main"]);
        write_rel(
            &repo,
            "Cargo.toml",
            "[package]\nname = \"susi-commercial\"\nversion = \"3.1.5\"\nedition = \"2021\"\n",
        );
        write_rel(
            &repo,
            "LICENSE-COMMERCIAL.md",
            "Closed, licensed, and paid corporate software — IntelliBitz.\n",
        );
        git(&repo, &["add", "-A"]);
        git(
            &repo,
            &[
                "commit",
                "-q",
                "-m",
                "chore: init commercial repo\n\nTask: T-COMMINIT-1",
            ],
        );
        repo
    }
}

impl Drop for CloudSandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn write_rel(root: &Path, rel: &str, content: &str) {
    let full = root.join(rel);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(full, content).unwrap();
}

fn git_cmd(cwd: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new("git");
    cmd.current_dir(cwd)
        .args(["-c", "gc.auto=0"])
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", null_config())
        .env("GIT_AUTHOR_NAME", "Susi Harvest Test")
        .env("GIT_AUTHOR_EMAIL", "harvest-test@susi.local")
        .env("GIT_COMMITTER_NAME", "Susi Harvest Test")
        .env("GIT_COMMITTER_EMAIL", "harvest-test@susi.local")
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(args);
    cmd
}

fn git(cwd: &Path, args: &[&str]) -> String {
    let out = git_cmd(cwd, args).output().expect("git must execute");
    assert!(
        out.status.success(),
        "git {args:?} failed in {}:\n{}",
        cwd.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn git_stdin(cwd: &Path, args: &[&str], stdin_bytes: &[u8]) -> String {
    let mut child = git_cmd(cwd, args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("git must spawn with stdin");
    child
        .stdin
        .take()
        .expect("stdin pipe")
        .write_all(stdin_bytes)
        .expect("write git stdin");
    let out = child.wait_with_output().expect("wait git");
    assert!(
        out.status.success(),
        "git {args:?} failed in {}:\n{}",
        cwd.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn run_harvest(cwd: &Path, args: &[&str]) -> (bool, String, String) {
    let (harvest_bin, _) = xtask_bins();
    let out = Command::new(harvest_bin)
        .current_dir(cwd)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", null_config())
        .env("GIT_AUTHOR_NAME", "Susi Harvest Test")
        .env("GIT_AUTHOR_EMAIL", "harvest-test@susi.local")
        .env("GIT_COMMITTER_NAME", "Susi Harvest Test")
        .env("GIT_COMMITTER_EMAIL", "harvest-test@susi.local")
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(args)
        .output()
        .expect("local-harvest must execute");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn run_distill(cwd: &Path, args: &[&str]) -> (bool, String, String) {
    let (_, distill_bin) = xtask_bins();
    let out = Command::new(distill_bin)
        .current_dir(cwd)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", null_config())
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(args)
        .output()
        .expect("distill must execute");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn raw_features_discovers_raw_branch_commits_and_pure_apache2_modules_from_cloud_repo() {
    let sb = CloudSandbox::new("raw-features");
    let (cloud_bare, head_sha) = sb.create_bare_cloud_repo(
        "cloud-acp-substrate",
        &[
            (
                "feat(acp): add zero-copy JSON-RPC frame splitter\n\nTask: T-UPSTREAM-1\nFeature: F-ACP-SPLITTER-1",
                &[
                    ("LICENSE", APACHE_LICENSE_TEXT),
                    ("NOTICE", "Cloud ACP Substrate\nCopyright 2026 Open Protocol Authors\n"),
                    (
                        "crates/susi-core/src/acp_splitter.rs",
                        "// Copyright 2026 Open Protocol Authors\n// SPDX-License-Identifier: Apache-2.0\npub fn split_frame(buf: &[u8]) -> Option<usize> { buf.iter().position(|b| *b == b'\\n') }\n",
                    ),
                ],
            ),
            (
                "feat(ui): add enterprise FinOps dashboard panel\n\nTask: T-UPSTREAM-2\nFeature: F-FINOPS-UI-1",
                &[(
                    "crates/susi-ui/src/finops_dashboard.rs",
                    "// Copyright 2026 Open Protocol Authors\n// SPDX-License-Identifier: Apache-2.0\npub fn render_finops_dashboard() -> &'static str { \"ok\" }\n",
                )],
            ),
            (
                "chore(contrib): add MIT-only helper\n\nTask: T-UPSTREAM-3\nFeature: F-MIT-HELPER-1",
                &[(
                    "contrib/mit_helper.rs",
                    "// SPDX-License-Identifier: MIT\npub fn mit_only() {}\n",
                )],
            ),
        ],
    );

    let core = sb.create_core_worktree("core-work");
    let url = cloud_bare.to_string_lossy().into_owned();
    let (ok, stdout, stderr) = run_harvest(
        &core,
        &["raw-features", "--url", &url, "--rev", "main", "--json"],
    );
    assert!(ok, "raw-features failed: {stderr}\n{stdout}");

    let report: Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert_eq!(report["ok"], true);
    assert_eq!(report["source_commit"], head_sha);
    assert_eq!(
        report["quarantine_ref"],
        format!("refs/susi/upstream/cloud-acp-substrate/{head_sha}")
    );

    let features = report["features"].as_array().unwrap();
    assert_eq!(
        features.len(),
        3,
        "expected 3 raw feature commits: {report}"
    );
    assert!(features.iter().any(|f| {
        f["features"]
            .as_array()
            .unwrap()
            .contains(&json!("F-ACP-SPLITTER-1"))
            && f["route"] == "core"
            && f["pure_apache2_ready"] == true
    }));
    assert!(features.iter().any(|f| {
        f["features"]
            .as_array()
            .unwrap()
            .contains(&json!("F-FINOPS-UI-1"))
            && f["route"] == "commercial"
            && f["pure_apache2_ready"] == true
    }));
    assert!(features.iter().any(|f| {
        f["features"]
            .as_array()
            .unwrap()
            .contains(&json!("F-MIT-HELPER-1"))
            && f["pure_apache2_ready"] == false
    }));

    // Modules list ONLY Pure-Apache-2.0-accepted files (excluding MIT-only contrib/mit_helper.rs).
    let modules = report["modules"].as_array().unwrap();
    assert!(modules
        .iter()
        .any(|m| m["path"] == "crates/susi-core/src/acp_splitter.rs" && m["route"] == "core"));
    assert!(modules.iter().any(|m| {
        m["path"] == "crates/susi-ui/src/finops_dashboard.rs" && m["route"] == "commercial"
    }));
    assert!(
        !modules.iter().any(|m| m["path"] == "contrib/mit_helper.rs"),
        "MIT-only module must never appear as pure Apache-2.0 ready: {modules:?}"
    );
}

#[test]
fn pickup_imports_pure_and_dual_elected_apache2_with_full_section4_obligations() {
    let sb = CloudSandbox::new("pickup-apache2");
    let (pure_bare, pure_sha) = sb.create_bare_cloud_repo(
        "pure-apache-gmcp",
        &[(
            "feat(gmcp): add MCP cursorcodec\n\nTask: T-UP-10\nFeature: F-GMCP-CURSOR-1",
            &[
                ("LICENSE", APACHE_LICENSE_TEXT),
                ("NOTICE", "GMCP Cursor Codec\nCopyright 2026 Upstream GMCP Team\n"),
                (
                    "src/cursor.rs",
                    "// Copyright 2026 Upstream GMCP Team\n// Licensed under the Apache License, Version 2.0\npub fn encode_cursor(offset: u64) -> String { format!(\"cur:{offset}\") }\n",
                ),
            ],
        )],
    );

    let (dual_bare, dual_sha) = sb.create_bare_cloud_repo(
        "dual-apache-mit-acp",
        &[(
            "feat(acp): dual-licensed frame codec",
            &[
                ("LICENSE-APACHE", APACHE_LICENSE_TEXT),
                ("LICENSE-MIT", MIT_LICENSE_TEXT),
                (
                    "src/dual_acp.rs",
                    "// SPDX-License-Identifier: MIT OR Apache-2.0\n// Copyright (c) 2025 Dual Crate Contributors\npub fn acp_ping_id(seq: u64) -> u64 { seq + 1 }\n",
                ),
            ],
        )],
    );

    let core = sb.create_core_worktree("core-pickup");

    // (a) Pure Apache-2.0 pickup into GMCP pillar.
    let pure_url = pure_bare.to_string_lossy().into_owned();
    let (ok, stdout, stderr) = run_harvest(
        &core,
        &[
            "pickup",
            "--url",
            &pure_url,
            "--rev",
            "main",
            "--slug",
            "pure-gmcp",
            "--map",
            "src/cursor.rs=crates/susi-gmcp/src/cursor_harvested.rs",
            "--id",
            "H-CLOUD-1",
            "--task",
            "T-CLOUD-1",
            "--feature",
            "F-GMCP-CURSOR-1",
            "--agent",
            "core-harvest",
            "--json",
        ],
    );
    assert!(ok, "pure Apache-2.0 pickup failed: {stderr}\n{stdout}");
    let p1: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(p1["license"], "Apache-2.0");
    assert_eq!(p1["source_commit"], pure_sha);
    assert_eq!(p1["route"]["effective_target"], "core");
    assert!(core.join("LICENSES/Apache-2.0.txt").is_file());
    assert!(core.join("THIRD_PARTY_NOTICES").is_file());

    // (b) Dual `MIT OR Apache-2.0` (`LICENSE-APACHE` + `LICENSE-MIT`) elects `"Apache-2.0"` exclusively.
    let dual_url = dual_bare.to_string_lossy().into_owned();
    let (ok2, stdout2, stderr2) = run_harvest(
        &core,
        &[
            "pickup",
            "--url",
            &dual_url,
            "--rev",
            "main",
            "--slug",
            "dual-acp",
            "--map",
            "src/dual_acp.rs=crates/susi-core/src/dual_acp.rs",
            "--id",
            "H-CLOUD-2",
            "--task",
            "T-CLOUD-2",
            "--feature",
            "F-ACP-PING-1",
            "--agent",
            "core-harvest",
            "--json",
        ],
    );
    assert!(
        ok2,
        "dual Apache-2.0 election pickup failed: {stderr2}\n{stdout2}"
    );
    let p2: Value = serde_json::from_str(&stdout2).unwrap();
    assert_eq!(p2["license"], "Apache-2.0");
    assert_eq!(p2["source_commit"], dual_sha);

    let m2: Value = serde_json::from_str(
        &fs::read_to_string(core.join(".agents/harvest/H-CLOUD-2.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(m2["license"], "Apache-2.0");
    assert_eq!(
        m2["license_texts"].as_array().unwrap().len(),
        0,
        "zero MIT/BSD fallback allowed under Pure Apache-2.0 election: {m2}"
    );
    assert!(
        m2["license_evidence"]
            .as_array()
            .unwrap()
            .iter()
            .all(|ev| ev["kind"] != "license" || ev["spdx"] == "Apache-2.0"),
        "evidence must bind elected Apache-2.0 only: {m2}"
    );

    let notices = fs::read_to_string(core.join("THIRD_PARTY_NOTICES")).unwrap();
    assert!(notices.contains("H-CLOUD-1") && notices.contains("H-CLOUD-2"));
    assert!(notices.contains("GMCP Cursor Codec"));
    assert!(notices.contains("Copyright (c) 2025 Dual Crate Contributors"));
    assert!(!notices.contains("Permission is hereby granted, free of charge"));

    // Verify Apache-2.0 §4(b) `Modified by` audit enforcement on modified destination file.
    let dest_dual = core.join("crates/susi-core/src/dual_acp.rs");
    let orig_dual = fs::read_to_string(&dest_dual).unwrap();
    fs::write(
        &dest_dual,
        format!("{orig_dual}pub fn local_tweak() -> bool {{ true }}\n"),
    )
    .unwrap();
    let (audit_ok, audit_out, audit_err) = run_harvest(&core, &["audit"]);
    assert!(
        !audit_ok && (audit_out.contains("Modified by") || audit_err.contains("Modified by")),
        "modified file without notice must fail audit: {audit_out} {audit_err}"
    );

    fs::write(
        &dest_dual,
        format!(
            "{orig_dual}// Modified by Susi Core contributors, 2026-11-12: add local_tweak\npub fn local_tweak() -> bool {{ true }}\n"
        ),
    )
    .unwrap();
    let (audit_ok2, audit_out2, audit_err2) = run_harvest(&core, &["audit"]);
    assert!(
        audit_ok2,
        "audit must pass with Modified by notice: {audit_out2} {audit_err2}"
    );
}

type FileSpec<'a> = (&'a str, &'a str);
type CommitSpec<'a> = (&'a str, &'a [FileSpec<'a>]);
type RefusalCase<'a> = (&'a str, &'a [FileSpec<'a>], &'a str);

#[test]
fn non_apache2_cloud_repos_are_hard_refused_with_zero_files_written() {
    let sb = CloudSandbox::new("refuse-non-apache2");
    let core = sb.create_core_worktree("core-refuse");

    let cases: &[RefusalCase<'_>] = &[
        (
            "mit-only",
            &[
                ("LICENSE", MIT_LICENSE_TEXT),
                (
                    "src/lib.rs",
                    "// Copyright (c) 2025 MIT Author\npub fn x() -> u8 { 1 }\n",
                ),
            ],
            "not Apache-2.0",
        ),
        (
            "bsd3-only",
            &[
                ("LICENSE", BSD3_LICENSE_TEXT),
                (
                    "src/lib.rs",
                    "// Copyright (c) 2025 BSD Corp.\npub fn x() -> u8 { 2 }\n",
                ),
            ],
            "not Apache-2.0",
        ),
        (
            "llvm-exception",
            &[
                ("LICENSE", APACHE_LICENSE_TEXT),
                (
                    "src/lib.rs",
                    "// SPDX-License-Identifier: Apache-2.0 WITH LLVM-exception\npub fn x() -> u8 { 3 }\n",
                ),
            ],
            "not Apache-2.0",
        ),
        (
            "isc-only",
            &[
                ("LICENSE", ISC_LICENSE_TEXT),
                ("src/lib.rs", "pub fn x() -> u8 { 4 }\n"),
            ],
            "not Apache-2.0",
        ),
        (
            "gpl3-only",
            &[
                ("COPYING", GPL3_LICENSE_TEXT),
                (
                    "src/lib.rs",
                    "// Licensed under the GNU General Public License v3\npub fn x() -> u8 { 5 }\n",
                ),
            ],
            "GNU General Public License",
        ),
        (
            "unlicensed",
            &[("src/lib.rs", "pub fn mystery() -> u8 { 6 }\n")],
            "no SPDX header and no license file",
        ),
    ];

    for (idx, (slug, files, expected_msg)) in cases.iter().enumerate() {
        let (bare, _) = sb.create_bare_cloud_repo(slug, &[("initial", files)]);
        let url = bare.to_string_lossy().into_owned();
        let dest = format!("crates/susi-core/src/refused_{idx}.rs");
        let hid = format!("H-REFUSE-{idx}");
        let tid = format!("T-REFUSE-{idx}");
        let (ok, stdout, stderr) = run_harvest(
            &core,
            &[
                "pickup",
                "--url",
                &url,
                "--rev",
                "main",
                "--slug",
                slug,
                "--map",
                &format!("src/lib.rs={dest}"),
                "--id",
                &hid,
                "--task",
                &tid,
            ],
        );
        let combined = format!("{stdout}\n{stderr}");
        assert!(
            !ok,
            "case `{slug}` MUST be refused under Pure Apache-2.0 Compliance, got ok: {combined}"
        );
        assert!(
            combined.contains(expected_msg),
            "case `{slug}` expected `{expected_msg}` in refusal: {combined}"
        );
        assert!(
            !core.join(&dest).exists(),
            "case `{slug}` must write zero destination files"
        );
        assert!(
            !core.join(format!(".agents/harvest/{hid}.json")).exists(),
            "case `{slug}` must write zero manifests"
        );
    }
}

#[test]
fn ui_and_commercial_features_route_to_commercial_and_are_refused_in_core() {
    let sb = CloudSandbox::new("smart-routing");
    let (cloud_bare, _) = sb.create_bare_cloud_repo(
        "cloud-enterprise-pack",
        &[(
            "feat(enterprise): add SSO RBAC FinOps connector and UI view\n\nTask: T-ENT-1\nFeature: F-SSO-FINOPS-1",
            &[
                ("LICENSE", APACHE_LICENSE_TEXT),
                (
                    "src/sso_rbac_guard.rs",
                    "// Copyright 2026 Enterprise OSS Contributors\n// SPDX-License-Identifier: Apache-2.0\npub fn check_sso_rbac(role: &str) -> bool { role == \"admin\" }\n",
                ),
                (
                    "ui/FinOpsPanel.tsx",
                    "// Copyright 2026 Enterprise OSS Contributors\n// SPDX-License-Identifier: Apache-2.0\nexport const FinOpsPanel = () => \"finops\";\n",
                ),
            ],
        )],
    );

    let core = sb.create_core_worktree("core-router-check");
    let comm = sb.create_commercial_worktree("comm-router-check");

    // All 5 Susi Core pillars route to `core`.
    for (pillar_path, expected_pillar) in [
        ("crates/susi-gawd/src/swarm_bus.rs", "gawd"),
        ("crates/susi-gemi/src/router_ table.rs", "gemi"),
        ("crates/susi-gmcp/src/mcp_frame.rs", "gmcp"),
        ("crates/susi-core/src/acp_codec.rs", "acp"),
        ("crates/susi-harness/src/lane_sched.rs", "harness"),
    ] {
        let clean = pillar_path.replace(' ', "_");
        let (ok, out, err) = run_harvest(
            &core,
            &["route", "--path", &clean, "--target", "core", "--json"],
        );
        assert!(
            ok,
            "pillar `{clean}` must pass `--target core`: {err}\n{out}"
        );
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["target"], "core");
        assert!(v["pillars"]
            .as_array()
            .unwrap()
            .contains(&json!(expected_pillar)));
    }

    // Commercial / UI / SSO / FinOps / connectors route to `commercial` and fail `--target core`.
    for comm_path in [
        "ui/FinOpsPanel.tsx",
        "crates/susi-ui/src/panel.rs",
        "crates/susi-department-packs/src/hr_pack.rs",
        "crates/susi-ecosystem-hub/src/sso_connector.rs",
    ] {
        let (ok, out, _) = run_harvest(
            &core,
            &["route", "--path", comm_path, "--target", "core", "--json"],
        );
        assert!(
            !ok,
            "`{comm_path}` must be refused for `--target core`: {out}"
        );
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["target"], "commercial");
        assert_eq!(v["allowed_in_core"], false);
    }

    let url = cloud_bare.to_string_lossy().into_owned();
    // Attempt to pickup SSO/RBAC into Core -> REFUSED (both `--target auto` and `--target commercial`).
    for target_flag in ["auto", "commercial"] {
        let (ok, out, err) = run_harvest(
            &core,
            &[
                "pickup",
                "--url",
                &url,
                "--rev",
                "main",
                "--slug",
                "ent-pack",
                "--map",
                "src/sso_rbac_guard.rs=crates/susi-core/src/sso_rbac_guard.rs",
                "--id",
                "H-COREBLOCK-1",
                "--task",
                "T-COREBLOCK-1",
                "--target",
                target_flag,
            ],
        );
        assert!(
            !ok,
            "Core checkout MUST refuse commercial SSO/RBAC pickup (`--target {target_flag}`): {out}\n{err}"
        );
        assert!(!core.join("crates/susi-core/src/sso_rbac_guard.rs").exists());
    }

    // Same Pure Apache-2.0 enterprise capability picked up into `susi-commercial-apps` -> PASSES!
    let (ok_comm, out_comm, err_comm) = run_harvest(
        &comm,
        &[
            "pickup",
            "--url",
            &url,
            "--rev",
            "main",
            "--slug",
            "ent-pack",
            "--map",
            "src/sso_rbac_guard.rs=crates/susi-ecosystem-hub/src/sso_rbac_guard.rs",
            "--id",
            "H-COMM-1",
            "--task",
            "T-COMM-1",
            "--feature",
            "F-SSO-FINOPS-1",
            "--target",
            "commercial",
            "--json",
        ],
    );
    assert!(
        ok_comm,
        "susi-commercial-apps checkout must accept Pure Apache-2.0 commercial innovation: {err_comm}\n{out_comm}"
    );
    let vc: Value = serde_json::from_str(&out_comm).unwrap();
    assert_eq!(vc["license"], "Apache-2.0");
    assert_eq!(vc["route"]["effective_target"], "commercial");
    let (audit_ok, _, audit_err) = run_harvest(&comm, &["audit"]);
    assert!(audit_ok, "commercial harvest audit must pass: {audit_err}");
}

#[test]
fn picked_up_commit_on_raw_main_passes_harvest_audit_and_distill_lint_strict() {
    let sb = CloudSandbox::new("distill-lint-strict");
    let (cloud_bare, upstream_sha) = sb.create_bare_cloud_repo(
        "cloud-harness-lease",
        &[(
            "feat(harness): add deterministic lease backoff calculator\n\nTask: T-UP-55\nFeature: F-HARNESS-LEASE-1",
            &[
                ("LICENSE", APACHE_LICENSE_TEXT),
                ("NOTICE", "Harness Lease Calc\nCopyright 2026 Lease Authors\n"),
                (
                    "src/lease_backoff.rs",
                    "// Copyright 2026 Lease Authors\n// SPDX-License-Identifier: Apache-2.0\npub fn backoff_ms(attempt: u32) -> u64 { 50u64.saturating_mul(1u64 << attempt.min(6)) }\n",
                ),
            ],
        )],
    );

    let core = sb.create_core_worktree("core-raw-main");
    let base_sha = git(&core, &["rev-parse", "HEAD"]);
    let https_origin = "https://example.org/open-protocol/cloud-harness-lease.git";

    // Intake from local bare cloud repo, then record canonical HTTPS provenance URL via pickup/import
    // or pickup directly from the local bare URL (both `local-harvest audit` and `distill lint --strict`
    // validate exact task scopes, closed task record, and Harvest:/Origin:/Origin-License: trailers).
    let bare_url = cloud_bare.to_string_lossy().into_owned();
    let (ok, stdout, stderr) = run_harvest(
        &core,
        &[
            "pickup",
            "--url",
            &bare_url,
            "--rev",
            "main",
            "--slug",
            "harness-lease",
            "--map",
            "src/lease_backoff.rs=crates/susi-harness/src/lease_backoff.rs",
            "--id",
            "H-RAWMAIN-1",
            "--task",
            "T-RAWMAIN-1",
            "--feature",
            "F-HARNESS-LEASE-1",
            "--agent",
            "core-harvest",
            "--json",
        ],
    );
    assert!(ok, "pickup failed: {stderr}\n{stdout}");
    let pv: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(pv["source_commit"], upstream_sha);
    let trailers: Vec<String> = pv["trailers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect();
    assert!(trailers.contains(&"Harvest: H-RAWMAIN-1".to_owned()));
    assert!(trailers.contains(&"Origin-License: Apache-2.0".to_owned()));
    assert!(trailers.contains(&"Task: T-RAWMAIN-1".to_owned()));
    assert!(trailers.contains(&"Feature: F-HARNESS-LEASE-1".to_owned()));
    let _ = https_origin;

    // Write closed task record covering the exact touched files for Release distillation.
    let task_record = json!({
        "id": "T-RAWMAIN-1",
        "feature_id": "F-HARNESS-LEASE-1",
        "title": "Pick up pure Apache-2.0 harness lease backoff calculator onto Raw main",
        "status": "done",
        "state": "done",
        "owner": "core-harvest",
        "created_by": "core-harvest",
        "created_unix": 1794423000u64,
        "deps": [],
        "scopes": [
            ".agents/harvest/H-RAWMAIN-1.json",
            ".agents/tasks/T-RAWMAIN-1.json",
            "LICENSES/Apache-2.0.txt",
            "THIRD_PARTY_NOTICES",
            "crates/susi-harness/src/lease_backoff.rs"
        ],
        "accept": {
            "cmd": ["cargo", "test", "--offline", "--locked"]
        }
    });
    write_rel(
        &core,
        ".agents/tasks/T-RAWMAIN-1.json",
        &format!("{}\n", serde_json::to_string_pretty(&task_record).unwrap()),
    );

    git(&core, &["add", "-A"]);
    let commit_msg = format!(
        "feat(harness): pick up pure Apache-2.0 lease backoff onto raw main\n\n{}",
        trailers.join("\n")
    );
    git(&core, &["commit", "-q", "-m", &commit_msg]);

    let range = format!("{base_sha}..HEAD");
    let (audit_ok, audit_out, audit_err) =
        run_harvest(&core, &["audit", "--range", &range, "--json"]);
    assert!(
        audit_ok,
        "local-harvest audit --range must pass on Raw main: {audit_err}\n{audit_out}"
    );
    let audit_val: Value = serde_json::from_str(&audit_out).unwrap();
    assert_eq!(audit_val["ok"], true);
    assert_eq!(audit_val["manifests"], 1);
    assert_eq!(audit_val["harvest_trailer_commits"], 1);

    let (lint_ok, lint_out, lint_err) =
        run_distill(&core, &["lint", "--range", &range, "--strict", "--json"]);
    assert!(
        lint_ok,
        "distill lint --strict must pass on picked-up Raw main commit: {lint_err}\n{lint_out}"
    );
    let lint_val: Value = serde_json::from_str(&lint_out).unwrap();
    assert_eq!(
        lint_val["summary"]["findings"], 0,
        "distill lint report not clean: {lint_val}"
    );
}
