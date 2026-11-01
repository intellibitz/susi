//! Native, append-only GMCP evidence recording after actual named tests pass.
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Component, Path},
    process::Command,
};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const PREFIX: &str = "cargo test --locked --test gmcp_requirements ";
fn git(root: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("git").current_dir(root).args(args).output()?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
    }
    Ok(output.stdout)
}
fn newest(data: &str) -> Result<Vec<Value>> {
    let mut chains: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    let names = |record: &Value, target: &Value| {
        ["recorded_unix", "identity", "artifact_sha256"]
            .iter()
            .all(|key| record["supersedes"][*key] == target[*key])
    };
    for line in data.lines().filter(|line| !line.trim().is_empty()) {
        let record: Value = serde_json::from_str(line)?;
        let key = record["requirement"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or("missing requirement")?
            .to_owned();
        let chain = chains.entry(key).or_default();
        if !chain.is_empty() && !chain.iter().any(|old| names(&record, old)) {
            return Err("unlinked evidence supersession".into());
        }
        chain.push(record);
    }
    Ok(chains
        .values()
        .filter_map(|chain| {
            chain
                .iter()
                .filter(|record| !chain.iter().any(|later| names(later, record)))
                .max_by_key(|record| record["recorded_unix"].as_u64().unwrap_or(0))
                .cloned()
        })
        .collect())
}
fn confined(path: &str) -> bool {
    let path = Path::new(path);
    !path.as_os_str().is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}
fn artifact_paths(root: &Path, ledger: &Value, vector: &str) -> Result<Vec<String>> {
    let mut paths: BTreeSet<String> = ledger["requirements"]
        .as_array()
        .ok_or("missing requirements")?
        .iter()
        .filter(|row| row["trace"]["vector"] == vector)
        .filter_map(|row| row["trace"]["production_path"].as_str())
        .filter(|path| confined(path) && root.join(path).exists())
        .map(str::to_owned)
        .collect();
    for shared in [
        "tests/gmcp_requirements.rs",
        ".agents/gmcp/requirements-ledger.json",
        ".agents/gmcp/sources.json",
        ".agents/gmcp/spec",
        ".agents/gmcp/fixtures",
        "docs/gmcp",
        "docs/gmcp-roadmap.md",
        "scripts/check-gmcp-ledger.sh",
        "crates/susi-gmcp/src",
        "crates/susi-gmcp/Cargo.toml",
    ] {
        if root.join(shared).exists() {
            paths.insert(shared.into());
        }
    }
    Ok(paths.into_iter().collect())
}
fn tree_digest(tree: &[u8], manifest: &str) -> String {
    let mut digest = Sha256::new();
    for row in tree.split_inclusive(|byte| *byte == b'\n') {
        if !row.ends_with(b"\tcrates/susi-gmcp/Cargo.toml\n") {
            digest.update(row);
        }
    }
    digest.update(b"susi-gmcp manifest without package.version\n");
    let mut package = false;
    for line in manifest.split_inclusive('\n') {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            package = trimmed == "[package]";
        }
        if package
            && line
                .split_once('=')
                .is_some_and(|(key, _)| key.trim() == "version")
        {
            continue;
        }
        digest.update(line.as_bytes());
    }
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn test_name<'a>(record: &'a Value, source: &str) -> Result<&'a str> {
    let name = record["check"]
        .as_str()
        .and_then(|s| s.strip_prefix(PREFIX))
        .filter(|name| {
            !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        })
        .ok_or("unsupported GMCP check command")?;
    let lines: Vec<_> = source.lines().collect();
    let at = lines
        .iter()
        .position(|line| *line == format!("fn {name}() {{"))
        .ok_or("named GMCP test missing")?;
    for line in lines[..at].iter().rev().map(|line| line.trim()) {
        if line == "#[test]" {
            return Ok(name);
        }
        if line.starts_with("#[") || line.starts_with("//") {
            continue;
        }
        break;
    }
    Err("named GMCP check is not a test".into())
}
fn clean(root: &Path) -> Result<()> {
    if !git(root, &["status", "--porcelain"])?.is_empty() {
        return Err("commit first: evidence records must name the clean tested source".into());
    }
    Ok(())
}
pub fn run(root: &Path, args: &[String]) -> Result<()> {
    clean(root)?;
    let gitdir = String::from_utf8(git(
        root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?)?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(Path::new(gitdir.trim()).join("susi-gmcp-record.lock"))?;
    lock.lock()?;
    clean(root)?;
    let head = String::from_utf8(git(root, &["rev-parse", "HEAD"])?)?
        .trim()
        .to_owned();
    let mastery = args.first().is_some_and(|arg| arg == "--mastery");
    let mut vectors: Vec<String> = args[usize::from(mastery)..].to_vec();
    let directory = root.join(".agents/gmcp/evidence");
    if vectors.is_empty() {
        for entry in fs::read_dir(&directory)? {
            let path = entry?.path();
            if path.extension().is_some_and(|e| e == "jsonl") {
                if let Some(stem) = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .filter(|s| s.starts_with("VC-203-"))
                {
                    vectors.push(stem.into());
                }
            }
        }
    }
    vectors.sort();
    vectors.dedup();
    if vectors.is_empty() {
        return Err("no GMCP evidence vectors".into());
    }
    let ledger_bytes = fs::read(root.join(".agents/gmcp/requirements-ledger.json"))?;
    let ledger: Value = serde_json::from_slice(&ledger_bytes)?;
    let ledger_sha = Sha256::digest(&ledger_bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let source = fs::read_to_string(root.join("tests/gmcp_requirements.rs"))?;
    let identity = std::env::var("SUSI_AGENT")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or(
            String::from_utf8(git(root, &["config", "user.name"])?)?
                .trim()
                .to_owned(),
        );
    if identity.is_empty() {
        return Err("recording agent identity missing".into());
    }
    let mut planned = Vec::new();
    let mut checks = BTreeSet::new();
    for vector in vectors {
        if !vector.starts_with("VC-203-")
            || vector[7..].len() != 3
            || !vector[7..].bytes().all(|b| b.is_ascii_digit())
        {
            return Err("invalid GMCP vector".into());
        }
        let path = directory.join(format!("{vector}.jsonl"));
        if fs::symlink_metadata(&path)?.file_type().is_symlink() {
            return Err("evidence file must not be redirected".into());
        }
        let original = fs::read_to_string(&path)?;
        let mut records = Vec::new();
        for record in newest(&original)? {
            let name = test_name(&record, &source)?;
            if name.starts_with("gmcp_vc_") == mastery {
                checks.insert(name.to_owned());
                records.push(record);
            }
        }
        if records.is_empty() {
            continue;
        }
        let paths = artifact_paths(root, &ledger, &vector)?;
        let mut args = vec!["ls-tree", "-r", "--full-tree", head.as_str(), "--"];
        args.extend(paths.iter().map(String::as_str));
        let tree = git(root, &args)?;
        if tree.is_empty() {
            return Err("empty GMCP source tree".into());
        }
        let manifest = String::from_utf8(git(
            root,
            &["show", &format!("{head}:crates/susi-gmcp/Cargo.toml")],
        )?)?;
        planned.push((path, original, records, tree_digest(&tree, &manifest)));
    }
    if checks.is_empty() {
        return Err("no checks selected; no evidence recorded".into());
    }
    let version = Command::new("rustc").arg("-vV").output()?;
    if !version.status.success() {
        return Err("native compiler identity unavailable".into());
    }
    let version = String::from_utf8(version.stdout)?;
    let host = version
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .ok_or("native compiler host unavailable")?;
    if cfg!(windows) && !host.ends_with("-pc-windows-msvc") {
        return Err("GMCP recording requires native Windows MSVC".into());
    }
    let target = root.join("target/local-native");
    let output = Command::new("cargo")
        .current_dir(root)
        .args([
            "test",
            "--offline",
            "--locked",
            "--test",
            "gmcp_requirements",
            "--target",
            host,
            "--",
            "--exact",
        ])
        .args(&checks)
        .env("CARGO_TARGET_DIR", &target)
        .env("CARGO_BUILD_BUILD_DIR", &target)
        .output()?;
    std::io::stdout().write_all(&output.stdout)?;
    std::io::stderr().write_all(&output.stderr)?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !output.status.success()
        || !stdout.contains(&format!(
            "test result: ok. {} passed; 0 failed; 0 ignored;",
            checks.len()
        ))
    {
        return Err("GMCP checks did not all execute successfully; nothing recorded".into());
    }
    clean(root)?;
    if String::from_utf8(git(root, &["rev-parse", "HEAD"])?)?.trim() != head {
        return Err("HEAD changed during GMCP tests; nothing recorded".into());
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    // Prepare every append before writing. History is retained byte-for-byte.
    let mut updates = Vec::new();
    for (path, original, records, digest) in planned {
        if fs::read_to_string(&path)? != original {
            return Err("evidence changed during checks".into());
        }
        let mut bytes = original.as_bytes().to_vec();
        if !bytes.is_empty() && !bytes.ends_with(b"\n") {
            bytes.push(b'\n');
        }
        for record in records {
            let value = json!({"requirement":record["requirement"],"check":record["check"],"exit":0,"detail":record["detail"],"recorded_unix":now,"artifact_sha256":ledger_sha,"identity":identity,"tested_commit":head,"tested_artifacts":digest,"supersedes":{"recorded_unix":record["recorded_unix"],"identity":record["identity"],"artifact_sha256":record["artifact_sha256"]}});
            serde_json::to_writer(&mut bytes, &value)?;
            bytes.push(b'\n');
        }
        updates.push((path, original, bytes));
    }
    for (path, _, bytes) in &updates {
        if let Err(error) = fs::write(path, bytes) {
            for (path, original, _) in &updates {
                fs::write(path, original)?;
            }
            return Err(error.into());
        }
    }
    println!("Recorded {} passing checks against {head}; commit these append-only records before the next phase.",checks.len());
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chain_and_test_selection_fail_closed() {
        let first = json!({"requirement":"r","recorded_unix":1,"identity":"a","artifact_sha256":"x","check":format!("{PREFIX}gmcp_req_x")});
        let mut next = first.clone();
        next["recorded_unix"] = json!(2);
        assert!(newest(&format!("{first}\n{next}")).is_err());
        next["supersedes"] = json!({"recorded_unix":1,"identity":"a","artifact_sha256":"x"});
        assert_eq!(newest(&format!("{first}\n{next}")).unwrap(), vec![next]);
        assert!(test_name(&first, "// #[test]\nfn gmcp_req_x() {\n}").is_err());
        assert_eq!(
            test_name(&first, "#[test]\nfn gmcp_req_x() {\n}").unwrap(),
            "gmcp_req_x"
        );
    }
    #[test]
    fn digest_omits_only_package_version() {
        let a = "[package]\nname = \"x\"\nversion = \"1\"\n[dependencies]\nx = \"1\"\n";
        assert_eq!(
            tree_digest(b"tree\n", a),
            tree_digest(b"tree\n", &a.replace("version = \"1\"", "version = \"2\""))
        );
        assert_ne!(
            tree_digest(b"tree\n", a),
            tree_digest(b"tree\n", &a.replace("x = \"1\"", "x = \"2\""))
        );
    }
}
