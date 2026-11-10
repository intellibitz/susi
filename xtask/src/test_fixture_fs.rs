//! Fixture helpers shared by the test modules of the xtask sources and by
//! `xtask/tests/*`: one copy, mounted with `#[path]` because xtask has no
//! library target.

// Every mount uses a subset, and a fixture that cannot be laid down fails its test.
#![allow(dead_code, clippy::unwrap_used)]

use std::fs;
use std::path::Path;

/// Recursively copies `src` into `dst`, keeping file permissions.
pub fn copy_dir_all(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let ty = entry.file_type().unwrap();
        let dst_path = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_all(&entry.path(), &dst_path);
        } else {
            fs::copy(entry.path(), &dst_path).unwrap();
            if let Ok(meta) = entry.metadata() {
                let _ = fs::set_permissions(&dst_path, meta.permissions());
            }
        }
    }
}

/// Points `origin` of the clone at `repo` to `new_origin` by rewriting `.git/config`.
pub fn set_git_origin_url(repo: &Path, new_origin: &Path) {
    let cfg_path = repo.join(".git/config");
    let raw = fs::read_to_string(&cfg_path).unwrap();
    let url_val = new_origin.to_str().unwrap().replace('\\', "/");
    let mut in_origin = false;
    let mut out = String::with_capacity(raw.len() + 64);
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_origin = trimmed == "[remote \"origin\"]";
            out.push_str(line);
            out.push('\n');
        } else if in_origin && trimmed.starts_with("url =") {
            out.push_str(&format!("\turl = {url_val}\n"));
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    fs::write(&cfg_path, out).unwrap();
}
