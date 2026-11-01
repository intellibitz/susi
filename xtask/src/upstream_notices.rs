//! Offline verification of version-bound upstream attribution supplements.
use super::{digest, file_digest, hex, required, Result};
use sha2::Digest;
use std::fs;
use std::path::{Component, Path, PathBuf};

fn confined(root: &Path, relative: &str) -> Result<PathBuf> {
    let path = Path::new(relative);
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err("upstream notice path must be a confined relative path".into());
    }
    let result = root.join(path).canonicalize()?;
    if !result.starts_with(root.canonicalize()?) || !result.is_file() {
        return Err("upstream notice path escapes its source directory".into());
    }
    Ok(result)
}

fn repository(value: &str) -> &str {
    value.trim_end_matches('/').trim_end_matches(".git")
}

fn blob_sha1(bytes: &[u8]) -> String {
    let mut hash = sha1::Sha1::new();
    hash.update(format!("blob {}\0", bytes.len()).as_bytes());
    hash.update(bytes);
    hash.finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn bound_file(bytes: &[u8], record: &serde_json::Value, repo: &str, revision: &str) -> Result<()> {
    if digest(bytes) != required(record, "sha256")?
        || blob_sha1(bytes) != required(record, "git_blob_sha1")?
    {
        return Err("supplemental upstream file digest mismatch".into());
    }
    let source = required(record, "source_path")?;
    if Path::new(source)
        .components()
        .any(|c| !matches!(c, Component::Normal(_)))
        || source.is_empty()
    {
        return Err("invalid upstream source path".into());
    }
    let project = repository(repo)
        .strip_prefix("https://github.com/")
        .ok_or("supplement requires an explicitly verified HTTPS GitHub repository")?;
    let raw = format!("https://raw.githubusercontent.com/{project}/{revision}/{source}");
    let web = format!("https://github.com/{project}/blob/{revision}/{source}");
    if ![raw.as_str(), web.as_str()].contains(&required(record, "url")?) {
        return Err("upstream notice URL is not pinned to its repository/revision/path".into());
    }
    Ok(())
}

fn locked_checksum(root: &Path, package: &serde_json::Value) -> Result<String> {
    let lock = fs::read_to_string(root.join("Cargo.lock"))?;
    let field = |block: &str, key: &str| -> Option<String> {
        block
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{key} = \"")))
            .and_then(|value| value.strip_suffix('"'))
            .map(str::to_owned)
    };
    let name = required(package, "name")?;
    let version = required(package, "version")?;
    let source = required(package, "source")?;
    if !source.starts_with("registry+") {
        return Err("notice supplement requires registry source identity".into());
    }
    let matching: Vec<_> = lock
        .split("[[package]]")
        .filter(|block| {
            field(block, "name").as_deref() == Some(name)
                && field(block, "version").as_deref() == Some(version)
                && field(block, "source").as_deref() == Some(source)
        })
        .collect();
    if matching.len() != 1 {
        return Err("notice package is not uniquely bound to Cargo.lock".into());
    }
    let checksum = field(matching[0], "checksum")
        .ok_or("registry package checksum missing from Cargo.lock")?;
    if !hex(&checksum, &[64]) {
        return Err("invalid registry checksum".into());
    }
    Ok(checksum)
}

pub(super) fn verify(
    root: &Path,
    package: &serde_json::Value,
    directory: &Path,
) -> Result<Vec<PathBuf>> {
    let name = required(package, "name")?;
    let version = required(package, "version")?;
    let supplement = root
        .join("vendor/upstream-notices")
        .join(format!("{name}-{version}"));
    if !supplement.exists() {
        return Ok(Vec::new());
    }
    if !supplement.canonicalize()?.starts_with(root.canonicalize()?) {
        return Err("supplement directory escapes the checked source tree".into());
    }
    let provenance = confined(&supplement, "provenance.json")?;
    let record: serde_json::Value = serde_json::from_str(&fs::read_to_string(provenance)?)?;
    if record["schema"] != "susi/upstream-notice/v1"
        || record["name"] != name
        || record["version"] != version
    {
        return Err("upstream notice package/schema mismatch".into());
    }
    let checksum = locked_checksum(root, package)?;
    if required(&record, "crate_checksum")? != checksum {
        return Err("upstream notice crate checksum mismatch".into());
    }
    let legacy = directory.join(".cargo-checksum.json");
    if legacy.exists() {
        let legacy: serde_json::Value = serde_json::from_str(&fs::read_to_string(legacy)?)?;
        if legacy["package"] != checksum {
            return Err("registry cache checksum contradicts lockfile".into());
        }
    }
    if let Some(index) = directory.parent() {
        if let (Some(registry), Some(index_name)) =
            (index.parent().and_then(Path::parent), index.file_name())
        {
            let archive = registry
                .join("cache")
                .join(index_name)
                .join(format!("{name}-{version}.crate"));
            if archive.is_file() && file_digest(&archive)? != checksum {
                return Err("registry archive checksum mismatch".into());
            }
        }
    }
    let repo = required(&record, "repository")?;
    let actual_repo = package
        .get("repository")
        .and_then(serde_json::Value::as_str);
    if actual_repo.is_some_and(|actual| repository(actual) != repository(repo)) {
        return Err("upstream notice repository contradicts package metadata".into());
    }
    let revision = required(&record, "revision")?;
    if !hex(revision, &[40]) {
        return Err("upstream notice revision must be a pinned Git commit".into());
    }
    let vcs_path = directory.join(".cargo_vcs_info.json");
    let has_vcs = vcs_path.is_file();
    if has_vcs {
        let vcs: serde_json::Value = serde_json::from_str(&fs::read_to_string(vcs_path)?)?;
        if vcs.pointer("/git/sha1").and_then(serde_json::Value::as_str) != Some(revision)
            || vcs
                .get("path_in_vcs")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                != required(&record, "path_in_vcs")?
        {
            return Err("upstream notice revision/path contradicts registry VCS metadata".into());
        }
    }
    if !has_vcs || actual_repo.is_none() {
        let matches = record
            .get("source_match")
            .and_then(serde_json::Value::as_array)
            .filter(|rows| !rows.is_empty())
            .ok_or("missing VCS/repository metadata requires pinned source-file proof")?;
        let mut rust_source = false;
        for matched in matches {
            let path = confined(directory, required(matched, "package_path")?)?;
            rust_source |= path.extension().is_some_and(|extension| extension == "rs");
            bound_file(&fs::read(path)?, matched, repo, revision)?;
        }
        if !rust_source {
            return Err("source-match proof must include actual Rust source".into());
        }
    }
    let files = record
        .get("files")
        .and_then(serde_json::Value::as_array)
        .filter(|files| !files.is_empty())
        .ok_or("upstream notice file list missing")?;
    let mut notices = Vec::new();
    let mut license = false;
    for file in files {
        let path = confined(&supplement, required(file, "path")?)?;
        let bytes = fs::read(&path)?;
        bound_file(&bytes, file, repo, revision)?;
        let text = std::str::from_utf8(&bytes)?;
        if text.trim().is_empty() {
            return Err("empty upstream attribution text".into());
        }
        let filename = path
            .file_name()
            .ok_or("missing notice filename")?
            .to_string_lossy()
            .to_ascii_uppercase();
        license |= filename.starts_with("LICENSE")
            || filename.starts_with("LICENCE")
            || filename.starts_with("COPYING");
        notices.push(path);
    }
    if !license {
        return Err("supplement lacks a license/copying file".into());
    }
    Ok(notices)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (PathBuf, PathBuf, serde_json::Value, serde_json::Value) {
        let root = std::env::temp_dir().join(format!("susi-notices-{}", crate::stamp().unwrap()));
        let source = root.join("registry-source");
        let supplement = root.join("vendor/upstream-notices/example-1.0.0");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&supplement).unwrap();
        let checksum = "c".repeat(64);
        let revision = "a".repeat(40);
        fs::write(root.join("Cargo.lock"),format!("version = 4\n[[package]]\nname = \"example\"\nversion = \"1.0.0\"\nsource = \"registry+https://example.invalid/index\"\nchecksum = \"{checksum}\"\n")).unwrap();
        fs::write(
            source.join(".cargo_vcs_info.json"),
            serde_json::json!({"git":{"sha1":revision},"path_in_vcs":"crates/example"}).to_string(),
        )
        .unwrap();
        let content =
            "MIT License\nCopyright example upstream authors\nAttribution fixture only.\n";
        fs::write(supplement.join("LICENSE"), content).unwrap();
        let file = serde_json::json!({"path":"LICENSE","source_path":"LICENSE","sha256":digest(content.as_bytes()),"git_blob_sha1":blob_sha1(content.as_bytes()),"url":format!("https://raw.githubusercontent.com/example/project/{revision}/LICENSE")});
        let record = serde_json::json!({"schema":"susi/upstream-notice/v1","name":"example","version":"1.0.0","repository":"https://github.com/example/project","revision":revision,"path_in_vcs":"crates/example","crate_checksum":checksum,"files":[file]});
        let package = serde_json::json!({"name":"example","version":"1.0.0","repository":"https://github.com/example/project.git","source":"registry+https://example.invalid/index"});
        (root, source, package, record)
    }

    fn write(root: &Path, record: &serde_json::Value) {
        fs::write(
            root.join("vendor/upstream-notices/example-1.0.0/provenance.json"),
            record.to_string(),
        )
        .unwrap();
    }

    #[test]
    fn notices_bind_package_lock_vcs_repository_paths_and_both_digests() {
        let (root, source, package, record) = fixture();
        write(&root, &record);
        assert_eq!(verify(&root, &package, &source).unwrap().len(), 1);
        for key in [
            "name",
            "version",
            "crate_checksum",
            "revision",
            "path_in_vcs",
            "repository",
        ] {
            let mut bad = record.clone();
            bad[key] = "wrong".into();
            write(&root, &bad);
            assert!(verify(&root, &package, &source).is_err(), "{key}");
        }
        for key in ["sha256", "git_blob_sha1", "url", "path"] {
            let mut bad = record.clone();
            bad["files"][0][key] = "../outside".into();
            write(&root, &bad);
            assert!(verify(&root, &package, &source).is_err(), "{key}");
        }
        write(&root, &record);
        fs::write(
            root.join("vendor/upstream-notices/example-1.0.0/LICENSE"),
            "changed",
        )
        .unwrap();
        assert!(verify(&root, &package, &source).is_err());
    }

    #[test]
    fn missing_registry_vcs_or_repository_requires_matching_pinned_rust_source() {
        let (root, source, mut package, mut record) = fixture();
        fs::remove_file(source.join(".cargo_vcs_info.json")).unwrap();
        package["repository"] = serde_json::Value::Null;
        write(&root, &record);
        assert!(verify(&root, &package, &source).is_err());
        let code = "pub fn original() {}\n";
        fs::write(source.join("lib.rs"), code).unwrap();
        record["source_match"] = serde_json::json!([{"package_path":"lib.rs","source_path":"crates/example/lib.rs","sha256":digest(code.as_bytes()),"git_blob_sha1":blob_sha1(code.as_bytes()),"url":format!("https://raw.githubusercontent.com/example/project/{}/crates/example/lib.rs","a".repeat(40))}]);
        write(&root, &record);
        assert!(verify(&root, &package, &source).is_ok());
        fs::write(source.join("lib.rs"), "different source").unwrap();
        assert!(verify(&root, &package, &source).is_err());
    }
}
