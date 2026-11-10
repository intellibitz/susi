//! Native exact-tree release qualification: the record a release tag carries.
//!
//! `release-qualify vX.Y.Z` runs the release gates on the clean committed
//! release tree P, writes [`record_path`] and commits only that file as Q, whose
//! single parent is P. The release tag points at Q. `push-policy` accepts a new
//! `vX.Y.Z` tag only when [`verify_tag`] holds: Q changes exactly the record;
//! the record names P and P's tree; every gate in [`REQUIRED_GATES`] ran and
//! exited 0; tests ran and none failed; every built asset answered `--version`
//! with the release version; and the version, lockfile digest and toolchain it
//! names are P's own. The record is evidence bound to one tree, not a flag: a
//! change to the tree after qualification changes P and invalidates it.
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::Path;
use std::process::Command;

pub const SCHEMA: &str = "susi-release-qualification/v1";
pub const REQUIRED_GATES: [&str; 7] = [
    "format", "check", "clippy", "licenses", "tests", "build", "smoke",
];

/// The committed record of `tag`, relative to the repository root.
pub fn record_path(tag: &str) -> String {
    format!("releases/qualification/{tag}.json")
}

/// `X.Y.Z` for a release tag `vX.Y.Z` (digits only), `None` for anything else.
pub fn release_version(tag: &str) -> Option<&str> {
    let version = tag.strip_prefix('v')?;
    let parts: Vec<&str> = version.split('.').collect();
    let numeric = |part: &&str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
    (parts.len() == 3 && parts.iter().all(numeric)).then_some(version)
}

/// The `version` of the root `[package]` in a Cargo manifest.
pub fn package_version(manifest: &str) -> Option<String> {
    let mut in_package = false;
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }
        if in_package {
            if let Some(value) = line.strip_prefix("version") {
                let value = value.trim_start().strip_prefix('=')?.trim();
                return Some(value.trim_matches('"').to_owned());
            }
        }
    }
    None
}

/// The `channel` pinned by a `rust-toolchain.toml`.
pub fn toolchain_channel(pin: &str) -> Option<String> {
    pin.lines().find_map(|line| {
        let value = line.trim().strip_prefix("channel")?;
        let value = value.trim_start().strip_prefix('=')?.trim();
        Some(value.trim_matches('"').to_owned())
    })
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn git_bytes(root: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let out = Command::new("git")
        .current_dir(root)
        .arg("--no-replace-objects")
        .args(args)
        .output()
        .map_err(|e| format!("cannot run Git: {e}"))?;
    if !out.status.success() {
        return Err(format!("Git failed: {}", args.join(" ")));
    }
    Ok(out.stdout)
}

fn git_text(root: &Path, args: &[&str]) -> Result<String, String> {
    String::from_utf8(git_bytes(root, args)?)
        .map(|s| s.trim().to_owned())
        .map_err(|e| e.to_string())
}

fn text<'a>(record: &'a Value, field: &str) -> Result<&'a str, String> {
    record[field]
        .as_str()
        .ok_or_else(|| format!("record field `{field}` missing"))
}

fn count(value: &Value, field: &str) -> Result<u64, String> {
    value[field]
        .as_u64()
        .ok_or_else(|| format!("test count `{field}` missing"))
}

/// Check that `new` (a tag object or commit) is a qualified release `tag`.
pub fn verify_tag(root: &Path, tag: &str, new: &str) -> Result<(), String> {
    let version = release_version(tag).ok_or("not a vX.Y.Z release tag")?;
    let commit = git_text(
        root,
        &["rev-parse", "--verify", &format!("{new}^{{commit}}")],
    )?;
    let lineage = git_text(root, &["rev-list", "--parents", "-n", "1", &commit])?;
    let parents: Vec<&str> = lineage.split_whitespace().skip(1).collect();
    let [parent] = parents.as_slice() else {
        return Err("the tagged commit must be the record commit, with one parent".into());
    };
    let path = record_path(tag);
    let changed = git_text(
        root,
        &[
            "diff-tree",
            "--no-commit-id",
            "--no-renames",
            "--name-only",
            "-r",
            parent,
            &commit,
        ],
    )?;
    if changed != path {
        return Err(format!(
            "the tagged commit must change only {path}, not: {changed}"
        ));
    }
    let bytes = git_bytes(root, &["cat-file", "blob", &format!("{commit}:{path}")])?;
    let record: Value =
        serde_json::from_slice(&bytes).map_err(|e| format!("{path} is not JSON: {e}"))?;
    let expect = |field: &str, wanted: &str| -> Result<(), String> {
        let found = text(&record, field)?;
        if found == wanted {
            Ok(())
        } else {
            Err(format!("record `{field}` is {found}, expected {wanted}"))
        }
    };
    expect("schema", SCHEMA)?;
    expect("product", "susi")?;
    expect("tag", tag)?;
    expect("version", version)?;
    expect("qualified_commit", parent)?;
    let tree = git_text(root, &["rev-parse", &format!("{parent}^{{tree}}")])?;
    expect("qualified_tree", &tree)?;
    let manifest = git_text(root, &["cat-file", "blob", &format!("{parent}:Cargo.toml")])?;
    if package_version(&manifest).as_deref() != Some(version) {
        return Err(format!(
            "Cargo.toml of the qualified tree is not version {version}"
        ));
    }
    let lock = git_bytes(root, &["cat-file", "blob", &format!("{parent}:Cargo.lock")])?;
    expect("cargo_lock_sha256", &sha256_hex(&lock))?;
    let pin = git_text(
        root,
        &["cat-file", "blob", &format!("{parent}:rust-toolchain.toml")],
    )?;
    let channel = toolchain_channel(&pin).ok_or("rust-toolchain.toml names no channel")?;
    if record["toolchain"]["channel"].as_str() != Some(channel.as_str()) {
        return Err(format!("record toolchain is not the pinned {channel}"));
    }
    if text(&record, "host")?.is_empty() {
        return Err("record names no host".into());
    }
    let gates = record["gates"].as_array().ok_or("record has no gates")?;
    for name in REQUIRED_GATES {
        let gate = gates
            .iter()
            .find(|gate| gate["name"].as_str() == Some(name))
            .ok_or_else(|| format!("gate `{name}` did not run"))?;
        let argv = gate["argv"].as_array().filter(|argv| !argv.is_empty());
        if argv.is_none() {
            return Err(format!("gate `{name}` records no command"));
        }
        if gate["exit_code"].as_i64() != Some(0) {
            return Err(format!("gate `{name}` did not exit 0"));
        }
        if name == "tests" {
            let tests = &gate["tests"];
            if count(tests, "binaries")? == 0 || count(tests, "passed")? == 0 {
                return Err("the tests gate ran no tests".into());
            }
            if count(tests, "failed")? != 0 {
                return Err("the tests gate recorded failures".into());
            }
        }
    }
    let assets = record["assets"]
        .as_array()
        .filter(|assets| !assets.is_empty())
        .ok_or("record has no built assets")?;
    for asset in assets {
        let file = asset["file"].as_str().unwrap_or("?");
        let digest = asset["sha256"].as_str().unwrap_or("");
        if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(format!("asset {file} has no sha256"));
        }
        if !asset["version_output"]
            .as_str()
            .is_some_and(|out| out.contains(version))
        {
            return Err(format!("asset {file} did not report version {version}"));
        }
    }
    Ok(())
}
