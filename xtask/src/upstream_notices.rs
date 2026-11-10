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
    object_sha1("blob", bytes)
}

fn object_sha1(kind: &str, bytes: &[u8]) -> String {
    let mut hash = sha1::Sha1::new();
    hash.update(format!("{kind} {}\0", bytes.len()).as_bytes());
    hash.update(bytes);
    hash.finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn decode_object(value: &str) -> Result<Vec<u8>> {
    // Proofs are small Git metadata, not archives. Bound allocation before decoding.
    if value.is_empty() || value.len() > 8 * 1024 * 1024 || !value.len().is_multiple_of(2) {
        return Err("invalid or oversized Git proof object".into());
    }
    value
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let text = std::str::from_utf8(pair)?;
            Ok(u8::from_str_radix(text, 16)?)
        })
        .collect()
}

fn commit_tree(record: &serde_json::Value, revision: &str) -> Result<Option<String>> {
    match record["schema"].as_str() {
        Some("susi/upstream-notice/v1") => Ok(None),
        Some("susi/upstream-notice/v2") => {
            let bytes = decode_object(required(record, "git_commit_hex")?)?;
            if object_sha1("commit", &bytes) != revision {
                return Err("upstream proof commit does not match pinned revision".into());
            }
            let first = bytes
                .split(|byte| *byte == b'\n')
                .next()
                .ok_or("empty commit")?;
            let tree = std::str::from_utf8(first)?
                .strip_prefix("tree ")
                .filter(|oid| hex(oid, &[40]))
                .ok_or("upstream proof commit has no tree")?;
            Ok(Some(tree.to_owned()))
        }
        _ => Err("unsupported upstream notice schema".into()),
    }
}

fn tree_entry(bytes: &[u8], name: &str) -> Result<(String, String)> {
    let mut remaining = bytes;
    let mut found = None;
    while !remaining.is_empty() {
        let end = remaining
            .iter()
            .position(|byte| *byte == 0)
            .ok_or("invalid Git tree")?;
        let header = &remaining[..end];
        let separator = header
            .iter()
            .position(|byte| *byte == b' ')
            .ok_or("invalid Git tree entry")?;
        let mode = std::str::from_utf8(&header[..separator])?;
        // Git filenames are bytes. An unrelated non-UTF-8 sibling cannot
        // invalidate a proof for the explicitly UTF-8 source path.
        let entry = &header[separator + 1..];
        let tail = &remaining[end + 1..];
        let oid = tail.get(..20).ok_or("truncated Git tree object ID")?;
        if entry == name.as_bytes() {
            if found.is_some() {
                return Err("ambiguous Git tree path".into());
            }
            found = Some((
                mode.to_owned(),
                oid.iter().map(|byte| format!("{byte:02x}")).collect(),
            ));
        }
        remaining = &tail[20..];
    }
    found.ok_or_else(|| "upstream source path missing from pinned tree".into())
}

fn verify_tree_path(record: &serde_json::Value, root: &str, source: &str) -> Result<()> {
    let components: Vec<_> = source.split('/').collect();
    if components.len() > 128
        || components
            .iter()
            .any(|part| part.is_empty() || *part == "." || *part == "..")
        || source.contains(['\\', ':', '\0'])
    {
        return Err("invalid portable upstream proof path".into());
    }
    let chain = record["git_tree_chain_hex"]
        .as_array()
        .ok_or("missing Git tree proof")?;
    if chain.len() != components.len() {
        return Err("Git tree proof does not cover exactly the source path".into());
    }
    let mut proof_bytes = 0_usize;
    for object in chain {
        proof_bytes =
            proof_bytes.saturating_add(object.as_str().ok_or("invalid Git tree proof")?.len());
        if proof_bytes > 8 * 1024 * 1024 {
            return Err("Git tree path proof exceeds the size bound".into());
        }
    }
    let mut expected = root.to_owned();
    for (index, (component, object)) in components.iter().zip(chain).enumerate() {
        let bytes = decode_object(object.as_str().ok_or("invalid Git tree proof")?)?;
        if object_sha1("tree", &bytes) != expected {
            return Err("upstream Git tree proof digest mismatch".into());
        }
        let (mode, oid) = tree_entry(&bytes, component)?;
        let last = index + 1 == components.len();
        if (last && !matches!(mode.as_str(), "100644" | "100755"))
            || (!last && !matches!(mode.as_str(), "40000" | "040000"))
        {
            return Err("upstream proof path must contain directories and a regular blob".into());
        }
        expected = oid;
    }
    if expected != required(record, "git_blob_sha1")? {
        return Err("upstream file is not the blob named by the pinned Git tree".into());
    }
    Ok(())
}

fn bound_file(
    bytes: &[u8],
    record: &serde_json::Value,
    repo: &str,
    revision: &str,
    tree: Option<&str>,
) -> Result<()> {
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
    if let Some(tree) = tree {
        return verify_tree_path(record, tree, source);
    }
    let locator = repository(repo)
        .strip_prefix("https://github.com/")
        .ok_or("supplement requires an explicitly verified HTTPS GitHub repository")?;
    // Cargo repository metadata can point at a crate within a monorepo.
    // Preserve the full locator for metadata equality above, but pinned file
    // URLs address the repository itself, not its /tree/<ref>/<path> view.
    let segments: Vec<_> = locator.split('/').collect();
    if segments.len() < 2
        || segments[..2].iter().any(|part| part.is_empty())
        || (segments.len() > 2
            && (segments.len() < 5
                || segments[2] != "tree"
                || segments[3..]
                    .iter()
                    .any(|part| part.is_empty() || *part == "." || *part == "..")))
    {
        return Err("invalid GitHub repository locator".into());
    }
    let project = format!("{}/{}", segments[0], segments[1]);
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
    if !matches!(
        record["schema"].as_str(),
        Some("susi/upstream-notice/v1" | "susi/upstream-notice/v2")
    ) || record["name"] != name
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
    if repo.is_empty() || repo.trim() != repo || repo.chars().any(char::is_control) {
        return Err("upstream notice requires an explicit repository identity".into());
    }
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
    let tree = commit_tree(&record, revision)?;
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
            bound_file(&fs::read(path)?, matched, repo, revision, tree.as_deref())?;
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
        bound_file(&bytes, file, repo, revision, tree.as_deref())?;
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

    fn encoded(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    fn proof_fixture() -> (PathBuf, PathBuf, serde_json::Value, serde_json::Value) {
        let (root, source, mut package, mut record) = fixture();
        let blob = decode_object(record["files"][0]["git_blob_sha1"].as_str().unwrap()).unwrap();
        let mut leaf = b"100644 LICENSE\0".to_vec();
        leaf.extend(blob);
        let mut tree = b"40000 legal\0".to_vec();
        tree.extend(decode_object(&object_sha1("tree", &leaf)).unwrap());
        let commit = format!(
            "tree {}\nauthor Fixture <fixture@example.invalid> 1 +0000\ncommitter Fixture <fixture@example.invalid> 1 +0000\n\nFixture\n",
            object_sha1("tree", &tree)
        );
        let revision = object_sha1("commit", commit.as_bytes());
        package["repository"] = "https://forge.example.invalid/team/project.git".into();
        record["repository"] = "https://forge.example.invalid/team/project".into();
        record["schema"] = "susi/upstream-notice/v2".into();
        record["revision"] = revision.clone().into();
        record["git_commit_hex"] = encoded(commit.as_bytes()).into();
        record["files"][0]["source_path"] = "legal/LICENSE".into();
        record["files"][0]["git_tree_chain_hex"] =
            serde_json::json!([encoded(&tree), encoded(&leaf)]);
        record["files"][0].as_object_mut().unwrap().remove("url");
        fs::write(
            source.join(".cargo_vcs_info.json"),
            serde_json::json!({"git":{"sha1":revision},"path_in_vcs":"crates/example"}).to_string(),
        )
        .unwrap();
        (root, source, package, record)
    }

    #[test]
    fn portable_git_proof_verifies_nested_notice_without_any_host_url() {
        let (root, source, package, record) = proof_fixture();
        write(&root, &record);
        assert_eq!(verify(&root, &package, &source).unwrap().len(), 1);
        assert_eq!(
            object_sha1("tree", b""),
            "4b825dc642cb6eb9a060e54bf8d69288fbee4904"
        );
    }

    #[test]
    fn portable_proof_rejects_commit_tree_path_blob_and_repository_substitution() {
        let (root, source, package, record) = proof_fixture();
        for key in [
            "git_commit_hex",
            "repository",
            "revision",
            "schema",
            "crate_checksum",
        ] {
            let mut bad = record.clone();
            bad[key] = "wrong".into();
            write(&root, &bad);
            assert!(verify(&root, &package, &source).is_err(), "{key}");
        }
        for key in ["source_path", "git_blob_sha1", "sha256"] {
            let mut bad = record.clone();
            bad["files"][0][key] = "other".into();
            write(&root, &bad);
            assert!(verify(&root, &package, &source).is_err(), "{key}");
        }
        for chain in [
            serde_json::json!([]),
            serde_json::json!(["00", "01"]),
            serde_json::json!(["not hex"]),
        ] {
            let mut bad = record.clone();
            bad["files"][0]["git_tree_chain_hex"] = chain;
            write(&root, &bad);
            assert!(verify(&root, &package, &source).is_err());
        }
    }

    #[test]
    fn portable_proof_never_falls_back_to_a_plausible_url() {
        let (root, source, package, mut record) = proof_fixture();
        record["files"][0]["url"] = "https://forge.example.invalid/project/LICENSE".into();
        record["files"][0]
            .as_object_mut()
            .unwrap()
            .remove("git_tree_chain_hex");
        write(&root, &record);
        assert!(verify(&root, &package, &source).is_err());
    }

    #[test]
    fn tree_parser_rejects_truncation_duplicate_paths_and_nonregular_files() {
        let mut tree = b"100644 LICENSE\0".to_vec();
        tree.extend([0; 20]);
        assert!(tree_entry(&tree[..tree.len() - 1], "LICENSE").is_err());
        tree.extend(tree.clone());
        assert!(tree_entry(&tree, "LICENSE").is_err());
        for mode in ["120000", "160000", "40000"] {
            let mut tree = format!("{mode} LICENSE\0").into_bytes();
            tree.extend([0; 20]);
            let record = serde_json::json!({"git_blob_sha1":"0".repeat(40),"git_tree_chain_hex":[encoded(&tree)]});
            assert!(verify_tree_path(&record, &object_sha1("tree", &tree), "LICENSE").is_err());
        }
    }

    #[test]
    fn tree_parser_preserves_non_utf8_sibling_names() {
        let mut tree = b"100644 \xff\0".to_vec();
        tree.extend([1; 20]);
        tree.extend(b"100644 LICENSE\0");
        tree.extend([0; 20]);
        assert_eq!(
            tree_entry(&tree, "LICENSE").unwrap(),
            ("100644".to_owned(), "0".repeat(40))
        );
    }

    #[test]
    fn portable_proof_rejects_ambiguous_paths_and_oversized_objects() {
        let record = serde_json::json!({"git_tree_chain_hex":[]});
        for path in [
            "",
            "../LICENSE",
            "/LICENSE",
            "legal//LICENSE",
            "legal\\LICENSE",
            "C:LICENSE",
        ] {
            assert!(
                verify_tree_path(&record, &"0".repeat(40), path).is_err(),
                "{path}"
            );
        }
        assert!(decode_object("").is_err());
        assert!(decode_object("abc").is_err());
        assert!(decode_object("zz").is_err());
        assert!(decode_object(&"00".repeat(4 * 1024 * 1024 + 1)).is_err());
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

    #[test]
    fn monorepo_locator_keeps_metadata_identity_but_pins_files_at_repository_root() {
        let (root, source, mut package, mut record) = fixture();
        let locator = "https://github.com/example/project/tree/main/crates/example";
        package["repository"] = locator.into();
        record["repository"] = locator.into();
        write(&root, &record);
        assert!(verify(&root, &package, &source).is_ok());
        record["repository"] = "https://github.com/example/project/tree/main/crates/other".into();
        write(&root, &record);
        assert!(verify(&root, &package, &source).is_err());
        record["repository"] = locator.into();
        record["path_in_vcs"] = "crates/other".into();
        write(&root, &record);
        assert!(verify(&root, &package, &source).is_err());
    }
}
