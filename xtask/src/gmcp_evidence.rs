//! Native current GMCP evidence snapshots after actual named tests pass.
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    process::Command,
};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const PREFIX: &str = "cargo test --locked --test gmcp_requirements ";
const LOCAL: &str = ".susi/records/gmcp/evidence";
const LEGACY: &str = ".agents/gmcp/evidence";
const MAX_EVIDENCE_BYTES: u64 = 16 * 1024 * 1024;
const RUNTIME_REPOSITORY: &str = "https://github.com/intellibitz/susi-commercial-apps.git";
const RUNTIME_ARTIFACTS: [&str; 11] = [
    "Cargo.toml",
    "Cargo.lock",
    "build.rs",
    "rust-toolchain.toml",
    ".cargo",
    "src",
    "crates",
    "config",
    "providers",
    "tests",
    "vendor",
];

#[derive(Debug, Default, PartialEq)]
struct Options {
    runtime: Option<PathBuf>,
    mastery: bool,
    vectors: Vec<String>,
}

fn options(args: &[String]) -> Result<Options> {
    let mut result = Options::default();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--mastery" if !result.mastery => result.mastery = true,
            "--runtime-checkout" if result.runtime.is_none() => {
                result.runtime = Some(PathBuf::from(
                    args.next().ok_or("--runtime-checkout requires a path")?,
                ));
            }
            word if word.starts_with('-') => return Err("unknown or duplicate GMCP option".into()),
            _ => result.vectors.push(arg.clone()),
        }
    }
    Ok(result)
}

fn runtime_digest(root: &Path, revision: &str) -> Result<String> {
    let mut args = vec!["ls-tree", "-r", "--full-tree", revision, "--"];
    args.extend(RUNTIME_ARTIFACTS);
    let tree = git(root, &args)?;
    if tree.is_empty() {
        return Err("empty commercial runtime source tree".into());
    }
    Ok(Sha256::digest(tree)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

// Only absence permits a legacy fallback. Corrupt or redirected local state
// must not silently select older proof from source.
fn read_record(path: &Path) -> Result<Option<String>> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
        Ok(meta) if !meta.is_file() => return Err("evidence must be a regular file".into()),
        Ok(_) => {}
    }
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(MAX_EVIDENCE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_EVIDENCE_BYTES {
        return Err("evidence exceeds the 16 MiB recording bound".into());
    }
    Ok(Some(String::from_utf8(bytes)?))
}

fn local_directory(root: &Path, create: bool) -> Result<PathBuf> {
    let mut path = root.to_path_buf();
    for part in LOCAL.split('/') {
        path.push(part);
        match fs::symlink_metadata(&path) {
            Ok(meta) if !meta.is_dir() => {
                return Err("local evidence directory must not be redirected".into())
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if create {
                    fs::create_dir(&path)?;
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(path)
}

fn requirements_digest(records: &[Value], mastery_id: &str) -> Result<String> {
    let requirements: BTreeMap<&str, &Value> = records
        .iter()
        .filter(|record| record["requirement"] != mastery_id)
        .map(|record| {
            Ok((
                record["requirement"]
                    .as_str()
                    .ok_or("missing requirement")?,
                record,
            ))
        })
        .collect::<Result<_>>()?;
    if requirements.is_empty() {
        return Err("mastery requires requirement evidence".into());
    }
    Ok(Sha256::digest(serde_json::to_vec(&requirements)?)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn write_snapshot(path: &Path, bytes: &[u8]) -> Result<()> {
    let temp = path.with_extension(format!("jsonl.{}.tmp", std::process::id()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    let result = (|| -> Result<()> {
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}
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
        "tests/gmcp_requirements",
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
    let options = options(args)?;
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
    let mastery = options.mastery;
    let mut vectors = options.vectors;
    let runtime = options.runtime.map(fs::canonicalize).transpose()?;
    let runtime_binding = runtime
        .as_deref()
        .map(|path| -> Result<(String, String)> {
            clean(path)?;
            if !path
                .join("crates/susi-cli-runtime/tests/gmcp_requirements.rs")
                .is_file()
            {
                return Err("runtime checkout does not own the GMCP test target".into());
            }
            let commit = String::from_utf8(git(path, &["rev-parse", "HEAD"])?)?
                .trim()
                .to_owned();
            let digest = runtime_digest(path, &commit)?;
            Ok((commit, digest))
        })
        .transpose()?;
    let test_root = runtime.as_ref().map_or_else(
        || root.to_path_buf(),
        |path| path.join("crates/susi-cli-runtime"),
    );
    let directory = local_directory(root, false)?;
    if !git(root, &["ls-files", "--", LOCAL])?.is_empty() {
        return Err("local GMCP evidence must not be tracked source".into());
    }
    let ignored = Command::new("git")
        .current_dir(root)
        .args([
            "check-ignore",
            "--quiet",
            "--no-index",
            ".susi/records/gmcp/evidence/probe",
        ])
        .status()?;
    if !ignored.success() {
        return Err("local GMCP evidence directory must be ignored".into());
    }
    if vectors.is_empty() {
        for dir in [root.join(LEGACY), directory.clone()] {
            let entries = match fs::read_dir(dir) {
                Ok(entries) => entries,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            for entry in entries {
                let path = entry?.path();
                if path.extension().is_some_and(|e| e == "jsonl") {
                    if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                        vectors.push(stem.into());
                    }
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
    let mut source = fs::read_to_string(test_root.join("tests/gmcp_requirements.rs"))?;
    let split_dir = test_root.join("tests/gmcp_requirements");
    if split_dir.is_dir() {
        let mut extra = Vec::new();
        for entry in fs::read_dir(&split_dir)? {
            let path = entry?.path();
            if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
                extra.push(path);
            }
        }
        extra.sort();
        for path in extra {
            if !source.is_empty() && !source.ends_with('\n') {
                source.push('\n');
            }
            source.push_str(&fs::read_to_string(path)?);
        }
    }
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
        let before = read_record(&path)?;
        let original = match &before {
            Some(value) => value.clone(),
            None => read_record(&root.join(LEGACY).join(format!("{vector}.jsonl")))?
                .ok_or("no current or legacy evidence; no proof can be inferred")?,
        };
        let current = newest(&original)?;
        let inputs = requirements_digest(&current, &format!("GMCP-{}-M", &vector[3..]))?;
        let mut records = Vec::new();
        for record in &current {
            let name = test_name(record, &source)?;
            if name.starts_with("gmcp_vc_") == mastery {
                checks.insert(name.to_owned());
                records.push(record.clone());
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
        planned.push((
            path,
            before,
            current,
            records,
            tree_digest(&tree, &manifest),
            inputs,
        ));
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
    let mut command =
        super::isolated_command(Path::new("cargo"), &target.join("gmcp-evidence-home"))?;
    super::compiler_env::apply_checked(&mut command)?;
    if runtime.is_some() {
        command.args(["test", "-p", "susi-cli-runtime"]);
        command.env("SUSI_CORE_CHECKOUT", fs::canonicalize(root)?);
    } else {
        command.arg("test");
    }
    let output = command
        .current_dir(runtime.as_deref().unwrap_or(root))
        .args([
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
        .env("CARGO_BUILD_TARGET_DIR", &target)
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
    if let (Some(runtime), Some((commit, digest))) = (&runtime, &runtime_binding) {
        clean(runtime)?;
        if String::from_utf8(git(runtime, &["rev-parse", "HEAD"])?)?.trim() != commit
            || runtime_digest(runtime, commit)? != *digest
        {
            return Err("commercial source changed during GMCP tests; nothing recorded".into());
        }
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    // Keep only the latest proof for each requirement. Optional history stays
    // in legacy source unchanged; new snapshots never append to that source.
    local_directory(root, true)?;
    let mut updates = Vec::new();
    for (path, before, current, records, digest, inputs) in planned {
        if read_record(&path)? != before {
            return Err("evidence changed during checks".into());
        }
        let mut snapshot: BTreeMap<String, Value> = current
            .into_iter()
            .map(|record| {
                (
                    record["requirement"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                    record,
                )
            })
            .collect();
        for record in records {
            let mut value = json!({"requirement":record["requirement"],"check":record["check"],"exit":0,"detail":record["detail"],"recorded_unix":now,"artifact_sha256":ledger_sha,"identity":identity,"tested_commit":head,"tested_artifacts":digest});
            if let Some((commit, digest)) = &runtime_binding {
                value["runtime_repository"] = json!(RUNTIME_REPOSITORY);
                value["runtime_commit"] = json!(commit);
                value["runtime_artifacts_sha256"] = json!(digest);
                value["target"] = json!(host);
            }
            if mastery {
                value["requirements_sha256"] = json!(inputs);
            }
            snapshot.insert(
                record["requirement"]
                    .as_str()
                    .ok_or("missing requirement")?
                    .to_owned(),
                value,
            );
        }
        let mut bytes = Vec::new();
        for record in snapshot.values() {
            serde_json::to_writer(&mut bytes, record)?;
            bytes.push(b'\n');
        }
        if bytes.len() as u64 > MAX_EVIDENCE_BYTES {
            return Err("evidence snapshot exceeds recording bound".into());
        }
        updates.push((path, before, bytes));
    }
    for (path, _, bytes) in &updates {
        if let Err(error) = write_snapshot(path, bytes) {
            for (path, before, _) in &updates {
                match before {
                    Some(value) => write_snapshot(path, value.as_bytes())?,
                    None if path.exists() => fs::remove_file(path)?,
                    None => {}
                }
            }
            return Err(error);
        }
    }
    println!("Recorded {} passing checks against {head} in ignored {LOCAL}; run the remaining phase on the same source commit.",checks.len());
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paired_source_options_are_explicit_and_reject_ambiguous_inputs() {
        let words = |args: &[&str]| {
            args.iter()
                .map(|word| (*word).to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            options(&words(&[
                "--runtime-checkout",
                "commercial checkout",
                "VC-203-001",
                "--mastery"
            ]))
            .unwrap(),
            Options {
                runtime: Some(PathBuf::from("commercial checkout")),
                mastery: true,
                vectors: vec!["VC-203-001".into()]
            }
        );
        for args in [
            vec!["--runtime-checkout"],
            vec!["--mastery", "--mastery"],
            vec!["--runtime-checkout", "one", "--runtime-checkout", "two"],
            vec!["--unknown"],
        ] {
            assert!(options(&words(&args)).is_err());
        }
    }
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
    fn fixture(label: &str) -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "gmcp-record-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        path
    }

    #[test]
    fn local_snapshot_inputs_are_exact_order_independent_and_nonempty() {
        let records = vec![
            json!({"requirement":"R-2","exit":0,"tested_commit":"a"}),
            json!({"requirement":"R-1","exit":0,"tested_commit":"a"}),
            json!({"requirement":"M","exit":0}),
        ];
        let before = requirements_digest(&records, "M").unwrap();
        let mut reordered = records.clone();
        reordered.reverse();
        assert_eq!(before, requirements_digest(&reordered, "M").unwrap());
        reordered[0]["exit"] = json!(1);
        assert_eq!(before, requirements_digest(&reordered, "M").unwrap());
        reordered[1]["tested_commit"] = json!("b");
        assert_ne!(before, requirements_digest(&reordered, "M").unwrap());
        assert!(requirements_digest(&[json!({"requirement":"M"})], "M").is_err());
    }

    #[test]
    fn local_snapshots_are_bounded_atomic_and_do_not_follow_redirection() {
        let root = fixture("atomic");
        let dir = local_directory(&root, true).unwrap();
        let path = dir.join("VC-203-001.jsonl");
        assert_eq!(read_record(&path).unwrap(), None);
        write_snapshot(&path, b"first\n").unwrap();
        let temp = path.with_extension(format!("jsonl.{}.tmp", std::process::id()));
        fs::write(&temp, b"owned elsewhere").unwrap();
        assert!(write_snapshot(&path, b"replacement\n").is_err());
        assert_eq!(fs::read(&temp).unwrap(), b"owned elsewhere");
        assert_eq!(read_record(&path).unwrap().unwrap(), "first\n");
        fs::remove_file(&temp).unwrap();
        write_snapshot(&path, b"replacement\n").unwrap();
        assert_eq!(read_record(&path).unwrap().unwrap(), "replacement\n");
        let large = fs::File::create(&path).unwrap();
        large.set_len(MAX_EVIDENCE_BYTES + 1).unwrap();
        drop(large);
        assert!(read_record(&path).is_err());
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(read_record(&path).is_err());
        #[cfg(unix)]
        {
            let redirected = fixture("redirect");
            std::os::unix::fs::symlink(&root, redirected.join(".susi")).unwrap();
            assert!(local_directory(&redirected, true).is_err());
            fs::remove_dir_all(redirected).unwrap();
        }
        fs::remove_dir_all(root).unwrap();
    }

    fn recorder_fixture() -> PathBuf {
        let root = fixture("native");
        for path in ["src", "tests", "crates/susi-gmcp", ".agents/gmcp/evidence"] {
            fs::create_dir_all(root.join(path)).unwrap();
        }
        fs::write(root.join(".gitignore"), ".susi/\ntarget/\n").unwrap();
        fs::write(root.join("Cargo.toml"),"[package]\nname = \"gmcp-record-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[workspace]\n").unwrap();
        fs::write(root.join("src/lib.rs"), "").unwrap();
        fs::write(root.join("fixture.txt"), "ready").unwrap();
        fs::write(
            root.join("crates/susi-gmcp/Cargo.toml"),
            "[package]\nname = \"susi-gmcp\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        fs::write(root.join(".agents/gmcp/requirements-ledger.json"),json!({"requirements":[{"trace":{"vector":"VC-203-001","production_path":"fixture.txt"}}]}).to_string()).unwrap();
        fs::write(root.join("tests/gmcp_requirements.rs"),r#"#[test]
fn gmcp_req_203_001_01_contract() {
    assert_eq!(std::fs::read_to_string("fixture.txt").unwrap(), "ready");
}
#[test]
fn gmcp_vc_203_001_mastery() {
    let proof = std::fs::read_to_string(".susi/records/gmcp/evidence/VC-203-001.jsonl").unwrap();
    assert!(proof.lines().any(|line| line.contains("\"requirement\":\"GMCP-203-001-01\"") && line.contains("\"tested_commit\":")));
}
"#).unwrap();
        let req = json!({"requirement":"GMCP-203-001-01","check":format!("{PREFIX}gmcp_req_203_001_01_contract"),"recorded_unix":1,"identity":"fixture","artifact_sha256":"seed"});
        let mastery = json!({"requirement":"GMCP-203-001-M","check":format!("{PREFIX}gmcp_vc_203_001_mastery"),"recorded_unix":1,"identity":"fixture","artifact_sha256":"seed"});
        fs::write(
            root.join(LEGACY).join("VC-203-001.jsonl"),
            format!("{req}\n{mastery}\n"),
        )
        .unwrap();
        let mut cargo = Command::new("cargo");
        super::super::compiler_env::apply_checked(&mut cargo).unwrap();
        assert!(cargo
            .current_dir(&root)
            .args(["generate-lockfile", "--offline"])
            .status()
            .unwrap()
            .success());
        git(&root, &["init", "-b", "main"]).unwrap();
        git(&root, &["config", "user.name", "fixture"]).unwrap();
        git(&root, &["config", "user.email", "fixture@localhost"]).unwrap();
        git(&root, &["add", "."]).unwrap();
        git(&root, &["commit", "-m", "fixture"]).unwrap();
        root
    }

    #[test]
    fn native_recording_keeps_source_clean_current_and_fails_without_executed_checks() {
        let root = recorder_fixture();
        let legacy = root.join(LEGACY).join("VC-203-001.jsonl");
        let original = fs::read(&legacy).unwrap();
        let local = root.join(LOCAL).join("VC-203-001.jsonl");
        let head = String::from_utf8(git(&root, &["rev-parse", "HEAD"]).unwrap()).unwrap();
        run(&root, &["VC-203-001".into()]).unwrap();
        assert_eq!(fs::read(&legacy).unwrap(), original);
        assert!(git(&root, &["status", "--porcelain"]).unwrap().is_empty());
        run(&root, &["--mastery".into(), "VC-203-001".into()]).unwrap();
        let records = newest(&fs::read_to_string(&local).unwrap()).unwrap();
        assert_eq!(records.len(), 2);
        for record in &records {
            assert_eq!(record["tested_commit"], head.trim());
            assert_eq!(record["exit"], 0);
        }
        let mastery = records
            .iter()
            .find(|r| r["requirement"] == "GMCP-203-001-M")
            .unwrap();
        assert_eq!(
            mastery["requirements_sha256"],
            requirements_digest(&records, "GMCP-203-001-M").unwrap()
        );
        run(&root, &["VC-203-001".into()]).unwrap();
        assert_eq!(fs::read_to_string(&local).unwrap().lines().count(), 2);
        let before = fs::read(&local).unwrap();
        fs::write(root.join("fixture.txt"), "fails").unwrap();
        git(&root, &["add", "fixture.txt"]).unwrap();
        git(&root, &["commit", "-m", "failing fixture"]).unwrap();
        assert!(run(&root, &["VC-203-001".into()]).is_err());
        assert_eq!(fs::read(&local).unwrap(), before);
        fs::write(&local, "corrupt local record").unwrap();
        assert!(run(&root, &["VC-203-001".into()]).is_err());
        assert_eq!(fs::read(&legacy).unwrap(), original);
        fs::remove_file(&local).unwrap();
        fs::remove_file(&legacy).unwrap();
        git(&root, &["add", "-u"]).unwrap();
        git(&root, &["commit", "-m", "missing history"]).unwrap();
        assert!(run(&root, &["VC-203-001".into()]).is_err());
        assert!(!local.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn paired_recording_executes_the_commercial_owner_and_binds_both_real_histories() {
        let core = recorder_fixture();
        let runtime = recorder_fixture();
        let package = runtime.join("crates/susi-cli-runtime");
        fs::create_dir_all(package.join("tests")).unwrap();
        fs::create_dir_all(package.join("src")).unwrap();
        fs::write(package.join("src/lib.rs"), "// runtime implementation\n").unwrap();
        fs::write(
            package.join("Cargo.toml"),
            "[package]\nname = \"susi-cli-runtime\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        fs::write(
            runtime.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/susi-cli-runtime\"]\nresolver = \"2\"\n",
        )
        .unwrap();
        fs::write(package.join("tests/gmcp_requirements.rs"), r#"#[test]
fn gmcp_req_203_001_01_contract() {
    let core = std::path::PathBuf::from(std::env::var_os("SUSI_CORE_CHECKOUT").unwrap());
    assert_eq!(std::fs::read_to_string(core.join("fixture.txt")).unwrap(), "ready");
}
#[test]
fn gmcp_vc_203_001_mastery() {
    let core = std::path::PathBuf::from(std::env::var_os("SUSI_CORE_CHECKOUT").unwrap());
    let proof = std::fs::read_to_string(core.join(".susi/records/gmcp/evidence/VC-203-001.jsonl")).unwrap();
    assert!(proof.contains("\"runtime_commit\":"));
}
"#).unwrap();
        let mut cargo = Command::new("cargo");
        super::super::compiler_env::apply_checked(&mut cargo).unwrap();
        assert!(cargo
            .current_dir(&runtime)
            .args(["generate-lockfile", "--offline"])
            .status()
            .unwrap()
            .success());
        git(&runtime, &["add", "."]).unwrap();
        git(&runtime, &["commit", "-m", "commercial fixture owner"]).unwrap();
        let args = vec![
            "--runtime-checkout".into(),
            runtime.to_str().unwrap().into(),
            "VC-203-001".into(),
        ];
        run(&core, &args).unwrap();
        let path = core.join(LOCAL).join("VC-203-001.jsonl");
        let records = newest(&fs::read_to_string(&path).unwrap()).unwrap();
        let proof = records
            .iter()
            .find(|record| record["requirement"] == "GMCP-203-001-01")
            .unwrap();
        let commit = String::from_utf8(git(&runtime, &["rev-parse", "HEAD"]).unwrap()).unwrap();
        assert_eq!(proof["runtime_commit"], commit.trim());
        assert_eq!(proof["runtime_repository"], RUNTIME_REPOSITORY);
        assert_eq!(
            proof["runtime_artifacts_sha256"],
            runtime_digest(&runtime, commit.trim()).unwrap()
        );
        let mut mastery = args.clone();
        mastery.push("--mastery".into());
        run(&core, &mastery).unwrap();
        let before = fs::read(&path).unwrap();
        fs::write(
            package.join("src/lib.rs"),
            "// uncommitted runtime change\n",
        )
        .unwrap();
        assert!(run(&core, &args).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
        git(&runtime, &["add", "."]).unwrap();
        git(&runtime, &["commit", "-m", "changed runtime fixture"]).unwrap();
        assert_ne!(
            runtime_digest(&runtime, "HEAD").unwrap(),
            proof["runtime_artifacts_sha256"]
        );
        fs::write(core.join("fixture.txt"), "fails").unwrap();
        git(&core, &["add", "."]).unwrap();
        git(&core, &["commit", "-m", "failing normative fixture"]).unwrap();
        assert!(run(&core, &args).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
        fs::remove_dir_all(core).unwrap();
        fs::remove_dir_all(runtime).unwrap();
    }
}
