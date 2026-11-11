#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Committed release records (`releases/<tag>.json`, `docs/releases.md`).
//!
//! A release is a tag plus its record in git. The record names the commit the
//! tag points at, the command and settings that built the reference binaries,
//! each reference binary's sha256, the targets that have no reference build,
//! and the checks run. No host's release page is the record.
//!
//! `scripts/susi-release-sync.sh`, `install.sh` and the commercial
//! `scripts/refresh-certified-core.sh` read these files with plain line tools
//! (no JSON parser is assumed on a fresh host), so this suite pins the layout
//! they rely on as well as the content: the scalar fields they read are on
//! lines of their own and appear once, `env` is a one-line array, and every
//! asset is a one-line object. The values they pass to `cargo` are limited to
//! characters that cannot reach a shell as anything but a word.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

const SCHEMA: &str = "susi-release-record/v1";

/// The only targets core releases (CEO mandate 2026-11-05).
const TARGETS: [&str; 2] = ["x86_64-pc-windows-msvc", "x86_64-unknown-linux-gnu"];

/// Scalar fields a script reads with `sed`; each must sit alone on its line.
const LINE_FIELDS: [&str; 6] = [
    "version",
    "tag",
    "commit",
    "toolchain",
    "profile",
    "features",
];

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn records() -> Vec<(PathBuf, String, Value)> {
    let dir = root().join("releases");
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("releases/ holds the committed release records") {
        let path = entry.unwrap().path();
        // These directories hold host-neutral release metadata, not the
        // top-level release records checked by this suite:
        //
        // * releases/qualification/<tag>.json are the exact-tree
        //   qualification records push-policy reads (schema
        //   susi-release-qualification/v1); each belongs to a release record
        //   here, checked below.
        // * releases/distill/<name>.json are the immutable distillation
        //   envelopes that describe a release-branch candidate.
        if path.is_dir()
            && matches!(
                path.file_name().and_then(|n| n.to_str()),
                Some("qualification" | "distill")
            )
        {
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            panic!(
                "releases/ holds only <tag>.json records, found {}",
                path.display()
            );
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let value: Value = serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("{} is not JSON: {e}", path.display()));
        out.push((path, text, value));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    assert!(!out.is_empty(), "releases/ has no record");
    out
}

fn str_at<'a>(value: &'a Value, path: &[&str], file: &Path) -> &'a str {
    let mut at = value;
    for key in path {
        at = &at[*key];
    }
    at.as_str()
        .unwrap_or_else(|| panic!("{}: {} must be a string", file.display(), path.join(".")))
}

fn semver(text: &str) -> Option<(u64, u64, u64)> {
    let mut parts = text.split('.');
    let parse = |p: Option<&str>| {
        p.filter(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))?
            .parse::<u64>()
            .ok()
    };
    let version = (
        parse(parts.next())?,
        parse(parts.next())?,
        parse(parts.next())?,
    );
    parts.next().is_none().then_some(version)
}

/// A word `cargo` receives from a script unquoted after word splitting.
fn cargo_word(text: &str) -> bool {
    text.bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'/' | b'.' | b','))
}

fn workspace_version() -> String {
    let manifest = std::fs::read_to_string(root().join("Cargo.toml")).unwrap();
    let package = manifest.split("[package]").nth(1).unwrap();
    package
        .lines()
        .find_map(|l| l.trim().strip_prefix("version = \""))
        .and_then(|v| v.strip_suffix('"'))
        .unwrap()
        .to_owned()
}

#[test]
fn every_release_record_is_complete_and_names_its_tag() {
    let current = semver(&workspace_version()).unwrap();
    for (path, _, record) in records() {
        let file = path.as_path();
        assert_eq!(
            str_at(&record, &["schema"], file),
            SCHEMA,
            "{}",
            file.display()
        );
        assert_eq!(str_at(&record, &["product"], file), "susi");
        let version = str_at(&record, &["version"], file);
        let tag = str_at(&record, &["tag"], file);
        assert!(
            semver(version).is_some(),
            "{}: version {version}",
            file.display()
        );
        assert_eq!(tag, format!("v{version}"), "{}", file.display());
        assert_eq!(
            path.file_name().unwrap().to_str().unwrap(),
            format!("{tag}.json"),
            "a record is named after its tag"
        );
        assert!(
            semver(version).unwrap() <= current,
            "{}: a record cannot be newer than the workspace version",
            file.display()
        );
        let commit = str_at(&record, &["commit"], file);
        assert!(
            commit.len() == 40
                && commit
                    .bytes()
                    .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')),
            "{}: commit must be a full lower-case object id",
            file.display()
        );

        let toolchain = str_at(&record, &["build", "toolchain"], file);
        assert!(
            semver(toolchain).is_some(),
            "{}: toolchain {toolchain}",
            file.display()
        );
        assert!(TARGETS.contains(&str_at(&record, &["build", "host"], file)));
        let profile = str_at(&record, &["build", "profile"], file);
        assert!(
            !profile.is_empty() && cargo_word(profile),
            "{}: profile",
            file.display()
        );
        let features = str_at(&record, &["build", "features"], file);
        assert!(
            cargo_word(features),
            "{}: features {features}",
            file.display()
        );
        assert!(!str_at(&record, &["build", "command"], file).is_empty());
        for entry in record["build"]["env"]
            .as_array()
            .expect("build.env is an array")
        {
            let entry = entry.as_str().expect("build.env holds NAME=value strings");
            let (name, value) = entry.split_once('=').expect("NAME=value");
            assert!(
                !name.is_empty()
                    && name
                        .bytes()
                        .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
                    && cargo_word(value),
                "{}: env entry {entry}",
                file.display()
            );
        }
        assert!(
            record["reproducible"].is_boolean(),
            "{}: reproducible must say whether a rebuild is byte-identical",
            file.display()
        );
        assert!(record["checks"]["locally_qualified"].is_boolean());

        let assets = record["assets"].as_array().expect("assets is an array");
        let missing = record["missing_targets"]
            .as_array()
            .expect("missing_targets is an array (empty when every target has a build)");
        let mut seen = Vec::new();
        for asset in assets {
            let name = str_at(asset, &["file"], file);
            let target = str_at(asset, &["target"], file);
            let bin = str_at(asset, &["bin"], file);
            assert!(!str_at(asset, &["package"], file).is_empty());
            assert!(
                TARGETS.contains(&target),
                "{}: {name} target",
                file.display()
            );
            let expected = match target {
                "x86_64-pc-windows-msvc" => format!("{bin}-windows-x86_64.exe"),
                _ => format!("{bin}-linux-x86_64"),
            };
            assert_eq!(name, expected, "{}: asset file name", file.display());
            let sha = str_at(asset, &["sha256"], file);
            assert!(
                sha.len() == 64 && sha.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')),
                "{}: {name} sha256",
                file.display()
            );
            assert!(
                !seen.contains(&name),
                "{}: {name} listed twice",
                file.display()
            );
            seen.push(name);
        }
        for target in TARGETS {
            let built = assets
                .iter()
                .any(|a| a["target"] == target && a["bin"] == "susi");
            let owed = missing.iter().any(|m| {
                m["target"] == target && m["reason"].as_str().is_some_and(|r| !r.is_empty())
            });
            assert!(
                built != owed,
                "{}: {target} must have either a susi reference build or a missing_targets entry with its reason",
                file.display()
            );
        }
    }
}

#[test]
fn release_records_keep_the_line_layout_the_scripts_read() {
    for (path, text, record) in records() {
        let lines: Vec<&str> = text.lines().collect();
        for field in LINE_FIELDS {
            let prefix = format!("\"{field}\": \"");
            let hits: Vec<&&str> = lines
                .iter()
                .filter(|l| l.trim_start().starts_with(&prefix))
                .collect();
            assert_eq!(
                hits.len(),
                1,
                "{}: \"{field}\" must appear on exactly one line of its own",
                path.display()
            );
            let line = hits[0].trim().trim_end_matches(',');
            assert!(
                line.ends_with('"')
                    && line[prefix.len()..line.len() - 1]
                        .bytes()
                        .all(|b| b != b'"' && b != b'\\'),
                "{}: \"{field}\" holds a plain string value on its line",
                path.display()
            );
        }
        let env: Vec<&&str> = lines
            .iter()
            .filter(|l| l.trim_start().starts_with("\"env\": ["))
            .collect();
        assert_eq!(env.len(), 1, "{}: one env line", path.display());
        assert!(env[0].trim().trim_end_matches(',').ends_with(']'));
        for asset in record["assets"].as_array().unwrap() {
            let name = asset["file"].as_str().unwrap();
            let needle = format!("\"file\": \"{name}\"");
            let line = lines
                .iter()
                .find(|l| l.contains(&needle))
                .unwrap_or_else(|| panic!("{}: {name} line", path.display()));
            assert!(
                line.contains(&format!(
                    "\"sha256\": \"{}\"",
                    asset["sha256"].as_str().unwrap()
                )),
                "{}: asset {name} is a one-line object carrying its sha256",
                path.display()
            );
        }
        assert!(
            text.ends_with('\n') && !text.contains('\r'),
            "{}: LF line endings",
            path.display()
        );
    }
}

/// When the checkout has the tag (a full clone does; a shallow gate checkout
/// may not), the record's commit is the commit the tag names.
#[test]
fn release_records_name_the_commit_their_tag_points_at() {
    for (path, _, record) in records() {
        let tag = record["tag"].as_str().unwrap();
        let out = Command::new("git")
            .args([
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/tags/{tag}^{{commit}}"),
            ])
            .current_dir(root())
            .output()
            .expect("git runs");
        if !out.status.success() {
            continue;
        }
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            record["commit"].as_str().unwrap(),
            "{}: tag {tag} points elsewhere",
            path.display()
        );
    }
}

#[test]
fn the_release_format_is_documented() {
    let doc = std::fs::read_to_string(root().join("docs/releases.md")).unwrap();
    for needle in [
        SCHEMA,
        "releases/<tag>.json",
        "git ls-remote --tags",
        "reproducib",
        "missing_targets",
        "SUSI_SOURCE_REPO",
        "rust-toolchain.toml",
        "## Promoting a distilled candidate",
        "--from-distill",
        "--verify-only",
        "accept_verified_candidate",
        "adopted_host_commits",
        "`source: distill`",
        "`source: snapshot`",
    ] {
        assert!(
            doc.contains(needle),
            "docs/releases.md must explain {needle:?}"
        );
    }
}

#[test]
fn every_qualification_record_belongs_to_a_release_record() {
    let dir = root().join("releases").join("qualification");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    let tags: Vec<String> = records()
        .iter()
        .map(|(_, _, record)| record["tag"].as_str().unwrap_or_default().to_owned())
        .collect();
    for entry in entries {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_str().unwrap().to_owned();
        let tag = name
            .strip_suffix(".json")
            .unwrap_or_else(|| panic!("releases/qualification holds only <tag>.json: {name}"));
        let value: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap())
            .unwrap_or_else(|e| panic!("{name} is not JSON: {e}"));
        assert_eq!(value["schema"], "susi-release-qualification/v1", "{name}");
        assert_eq!(value["tag"], tag, "{name} names another tag");
        // Where the qualified tree came from: no `source` (records written
        // before the field existed) or `snapshot`, without a `distillation`
        // block; or `distill`, with a complete one. push-policy re-proves the
        // block from the tagged tree (`xtask/src/release_qualification.rs`).
        let distillation = value.get("distillation").filter(|v| !v.is_null());
        match value.get("source") {
            None => assert!(
                distillation.is_none(),
                "{name}: distillation without source"
            ),
            Some(source) if source == "snapshot" => {
                assert!(distillation.is_none(), "{name}: snapshot with distillation")
            }
            Some(source) if source == "distill" => {
                let block = distillation
                    .unwrap_or_else(|| panic!("{name}: source distill without distillation"));
                for field in [
                    "candidate",
                    "receipt_path",
                    "receipt_sha256",
                    "envelope_commit",
                    "base_commit",
                    "final_commit",
                    "final_tree",
                ] {
                    assert!(
                        block[field].as_str().is_some_and(|v| !v.is_empty()),
                        "{name}: distillation.{field} missing"
                    );
                }
                let candidate = block["candidate"].as_str().unwrap();
                assert_eq!(
                    block["receipt_path"],
                    format!("releases/distill/{candidate}.json"),
                    "{name}"
                );
                assert!(
                    block["included_tasks"].is_array() && block["adopted_host_commits"].is_array(),
                    "{name}: distillation names its tasks and adopted host commits"
                );
            }
            Some(other) => panic!("{name}: source {other} is neither snapshot nor distill"),
        }
        assert!(
            tags.iter().any(|t| t == tag),
            "{name}: no releases/{tag}.json for this qualified release"
        );
    }
}
