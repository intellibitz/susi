//! Native local release qualification, immutable packages and atomic promotion.
#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const CURRENT: &str = "refs/susi/releases/current";
const PREVIOUS: &str = "refs/susi/releases/previous";
const INTEGRATED: &str = "refs/susi/integration/current";

fn text(path: &Path) -> Result<&str> {
    path.to_str().ok_or_else(|| "path is not UTF-8".into())
}

fn git(root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git").current_dir(root).args(args).output()?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
    }
    Ok(String::from_utf8(output.stdout)?.trim().into())
}

fn git_input(root: &Path, args: &[&str], bytes: &[u8]) -> Result<String> {
    let mut child = Command::new("git")
        .current_dir(root)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child.stdin.take().ok_or("no git stdin")?.write_all(bytes)?;
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
    }
    Ok(String::from_utf8(output.stdout)?.trim().into())
}

fn resolve(root: &Path, reference: &str) -> Result<Option<String>> {
    let output = Command::new("git")
        .current_dir(root)
        .args(["show-ref", "--verify", "--quiet", reference])
        .output()?;
    match output.status.code() {
        Some(0) => Ok(Some(git(root, &["rev-parse", reference])?)),
        Some(1) => Ok(None),
        _ => Err("cannot inspect release reference".into()),
    }
}

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn file_digest(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn hex(value: &str, lengths: &[usize]) -> bool {
    lengths.contains(&value.len()) && value.bytes().all(|b| b.is_ascii_hexdigit())
}

fn real_directory(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            let redirected = meta.file_type().is_symlink();
            #[cfg(windows)]
            let redirected = {
                use std::os::windows::fs::MetadataExt;
                redirected || meta.file_attributes() & 0x400 != 0
            };
            if redirected || !meta.is_dir() {
                return Err("release directory must not be redirected".into());
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(path)?;
        }
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

struct Context {
    root: PathBuf,
    state: PathBuf,
    packages: PathBuf,
}
impl Context {
    fn open(path: &Path) -> Result<Self> {
        let root = PathBuf::from(git(path, &["rev-parse", "--show-toplevel"])?);
        let common = PathBuf::from(git(
            &root,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )?);
        let primary = common.parent().ok_or("missing primary repository")?;
        let target = primary.join("target");
        real_directory(&target)?;
        let packages = target.join("local-releases");
        real_directory(&packages)?;
        real_directory(&packages.join("objects"))?;
        let state = common.join("susi-local-release");
        real_directory(&state)?;
        Ok(Self {
            root,
            state,
            packages,
        })
    }
}

struct Lock(PathBuf);
impl Lock {
    fn acquire(state: &Path) -> Result<Self> {
        let path = state.join("lock");
        fs::create_dir(&path).map_err(|e| {
            format!(
                "release operation is locked or unavailable at {}: {e}",
                path.display()
            )
        })?;
        let guard = Self(path);
        fs::write(
            guard.0.join("owner"),
            format!("pid={}\n", std::process::id()),
        )?;
        Ok(guard)
    }
}
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.0.join("owner"));
        let _ = fs::remove_dir(&self.0);
    }
}

fn stamp() -> Result<String> {
    Ok(format!(
        "{}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos(),
        std::process::id()
    ))
}

fn binary_name() -> &'static str {
    if cfg!(windows) {
        "susi.exe"
    } else {
        "susi"
    }
}

fn compiler(root: &Path) -> Result<(String, String)> {
    let out = Command::new("rustc")
        .current_dir(root)
        .arg("-vV")
        .output()?;
    if !out.status.success() {
        return Err("native compiler unavailable".into());
    }
    let description = String::from_utf8(out.stdout)?;
    let host = description
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .ok_or("missing compiler host")?
        .to_owned();
    if !host
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        || (cfg!(windows) && !host.ends_with("-pc-windows-msvc"))
    {
        return Err("release requires the native platform compiler (MSVC on Windows)".into());
    }
    Ok((host, description))
}

fn isolated_command(program: &Path, home: &Path) -> Result<Command> {
    fs::create_dir_all(home)?;
    let original = env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .ok_or("missing provisioned toolchain home")?;
    let mut command = Command::new(program);
    for (key, _) in env::vars_os() {
        let name = key.to_string_lossy().to_ascii_uppercase();
        if name.starts_with("SUSI_")
            || [
                "TOKEN",
                "SECRET",
                "PASSWORD",
                "CREDENTIAL",
                "API_KEY",
                "ACCESS_KEY",
            ]
            .iter()
            .any(|part| name.contains(part))
        {
            command.env_remove(key);
        }
    }
    command
        .env(
            "CARGO_HOME",
            env::var_os("CARGO_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| original.join(".cargo")),
        )
        .env(
            "RUSTUP_HOME",
            env::var_os("RUSTUP_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| original.join(".rustup")),
        )
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("SUSI_HOME", home.join("susi"))
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env("APPDATA", home.join("appdata"))
        .env("LOCALAPPDATA", home.join("localappdata"))
        .env("RUSTC_WRAPPER", "")
        .env("RUSTC_WORKSPACE_WRAPPER", "");
    Ok(command)
}

fn integration_receipt(ctx: &Context, head: &str, tree: &str) -> Result<String> {
    if resolve(&ctx.root, INTEGRATED)?.as_deref() != Some(head) {
        return Err("qualify the exact locally integrated head".into());
    }
    for oid in git(
        &ctx.root,
        &[
            "for-each-ref",
            "--format=%(objectname)",
            "refs/susi/integration/receipts/",
        ],
    )?
    .lines()
    {
        let receipt = git(&ctx.root, &["cat-file", "blob", oid])?;
        if receipt.lines().next() == Some("susi-local-integration-v1")
            && receipt.lines().any(|line| line == format!("commit={head}"))
            && receipt.lines().any(|line| line == format!("tree={tree}"))
        {
            return Ok(oid.into());
        }
    }
    Err("no local integration receipt matches the source commit and tree".into())
}

fn clean(root: &Path) -> Result<()> {
    if !git(root, &["status", "--porcelain", "--untracked-files=all"])?.is_empty() {
        return Err("qualification requires a clean source checkout".into());
    }
    Ok(())
}

fn required<'a>(value: &'a serde_json::Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("missing metadata {key}").into())
}

fn native_source_check(package: &serde_json::Value, features: &[String]) -> Result<()> {
    let name = required(package, "name")?;
    let known = [
        "ring",
        "aws-lc-sys",
        "openssl-sys",
        "ort-sys",
        "onig_sys",
        "zstd-sys",
        "bzip2-sys",
        "lz4-sys",
        "libsqlite3-sys",
        "curl-sys",
        "libgit2-sys",
        "torch-sys",
        "tch",
        "candle-kernels",
        "intel-mkl-src",
        "accelerate-src",
    ];
    if known.contains(&name) {
        return Err(format!("Rust-only release blocked by native dependency {name}").into());
    }
    if name == "esaxx-rs" && features.iter().any(|f| f == "cpp") {
        return Err("Rust-only release blocks esaxx-rs/cpp".into());
    }
    // These reviewed feature branches omit their foreign-language compiler calls.
    let dormant = (name == "esaxx-rs" && !features.iter().any(|f| f == "cpp"))
        || (name == "blake3" && features.iter().any(|f| f == "pure"));
    for target in package
        .get("targets")
        .and_then(serde_json::Value::as_array)
        .ok_or("missing package targets")?
    {
        let build = target
            .get("kind")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|kinds| {
                kinds
                    .iter()
                    .any(|kind| kind.as_str() == Some("custom-build"))
            });
        if build && !dormant {
            let path = required(target, "src_path")?;
            let source = fs::read_to_string(path)?;
            let compact: String = source.chars().filter(|c| !c.is_whitespace()).collect();
            if [
                "cc::",
                "cmake::",
                "autotools::",
                "Command::new(\"cc\")",
                "Command::new(\"gcc\")",
                "Command::new(\"clang\")",
                "Command::new(\"cl\")",
                "Command::new(\"nvcc\")",
            ]
            .iter()
            .any(|marker| compact.contains(marker))
            {
                return Err(format!("Rust-only release requires review/replacement of native compiler build script in {name}").into());
            }
        }
    }
    Ok(())
}

fn notice_files(directory: &Path) -> Result<Vec<PathBuf>> {
    let mut result = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        if entry.file_type()?.is_file()
            && ["license", "licence", "copying", "notice", "copyright"]
                .iter()
                .any(|prefix| name.starts_with(prefix))
        {
            result.push(entry.path());
        }
        if entry.file_type()?.is_dir() && matches!(name.as_str(), "licenses" | "licences") {
            for file in fs::read_dir(entry.path())? {
                let file = file?;
                if file.file_type()?.is_file() {
                    result.push(file.path());
                }
            }
        }
    }
    result.sort();
    Ok(result)
}

/// Inventory only the target-filtered normal/build closure of the shipped Susi
/// binary. Development-only packages are not represented as shipped components.
fn inventory(candidate: &Path, attempt: &Path, host: &str) -> Result<(String, String, String)> {
    let out = isolated_command(Path::new("cargo"), &attempt.join("metadata-home"))?
        .current_dir(candidate)
        .args([
            "metadata",
            "--offline",
            "--locked",
            "--no-default-features",
            "--format-version",
            "1",
            "--filter-platform",
            host,
        ])
        .output()?;
    if !out.status.success() {
        return Err(format!(
            "cannot inventory release dependencies: {}",
            String::from_utf8_lossy(&out.stderr)
        )
        .into());
    }
    let metadata: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    let packages = metadata
        .get("packages")
        .and_then(serde_json::Value::as_array)
        .ok_or("missing package metadata")?;
    let nodes = metadata
        .pointer("/resolve/nodes")
        .and_then(serde_json::Value::as_array)
        .ok_or("missing resolved graph")?;
    let root_manifest = candidate.join("Cargo.toml").canonicalize()?;
    let root = packages
        .iter()
        .find(|p| {
            p.get("manifest_path")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|path| {
                    Path::new(path)
                        .canonicalize()
                        .is_ok_and(|p| p == root_manifest)
                })
        })
        .ok_or("missing Susi root package")?;
    if required(root, "name")? != "susi" {
        return Err("release source must contain the Susi package".into());
    }
    let mut reached = BTreeSet::new();
    let mut pending = vec![required(root, "id")?.to_owned()];
    while let Some(id) = pending.pop() {
        if !reached.insert(id.clone()) {
            continue;
        }
        let node = nodes
            .iter()
            .find(|node| node.get("id").and_then(serde_json::Value::as_str) == Some(&id))
            .ok_or("incomplete dependency graph")?;
        for dependency in node
            .get("deps")
            .and_then(serde_json::Value::as_array)
            .ok_or("missing resolved dependencies")?
        {
            let kinds = dependency
                .get("dep_kinds")
                .and_then(serde_json::Value::as_array)
                .ok_or("missing dependency kinds")?;
            if kinds.iter().any(|kind| {
                kind.get("kind")
                    .is_some_and(|kind| kind.is_null() || kind.as_str() == Some("build"))
            }) {
                pending.push(required(dependency, "pkg")?.into());
            }
        }
    }
    let mut records = Vec::new();
    let mut unique_notices = BTreeMap::new();
    let mut audit=String::from("susi-native-source-screen-v1\nCoverage: resolved native target normal/build closure, known native crates and direct build-script compiler markers. This screening is not a proof that arbitrary generated or downloaded code is Rust.\n");
    let source_root = candidate.canonicalize()?;
    for id in reached {
        let package = packages
            .iter()
            .find(|p| p.get("id").and_then(serde_json::Value::as_str) == Some(&id))
            .ok_or("missing reached package")?;
        let node = nodes
            .iter()
            .find(|n| n.get("id").and_then(serde_json::Value::as_str) == Some(&id))
            .ok_or("missing reached node")?;
        let features = node
            .get("features")
            .and_then(serde_json::Value::as_array)
            .ok_or("missing feature resolution")?
            .iter()
            .map(|f| f.as_str().map(str::to_owned).ok_or("invalid feature"))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        native_source_check(package, &features)?;
        let directory = Path::new(required(package, "manifest_path")?)
            .parent()
            .ok_or("missing package directory")?
            .canonicalize()?;
        let workspace = directory.starts_with(&source_root)
            && metadata
                .get("workspace_members")
                .and_then(serde_json::Value::as_array)
                .ok_or("missing workspace membership")?
                .iter()
                .any(|member| member.as_str() == Some(&id));
        let mut notices = notice_files(&directory)?;
        if let Some(file) = package
            .get("license_file")
            .and_then(serde_json::Value::as_str)
        {
            let path = directory.join(file).canonicalize()?;
            if !(path.starts_with(&directory) || (workspace && path.starts_with(&source_root))) {
                return Err("license file escapes package source".into());
            }
            notices.push(path);
        }
        if notices.is_empty() && workspace {
            let mut ancestor = directory.parent();
            while let Some(path) = ancestor {
                if !path.starts_with(&source_root) {
                    break;
                }
                notices = notice_files(path)?;
                if !notices.is_empty() {
                    break;
                }
                ancestor = path.parent();
            }
        }
        notices.sort();
        notices.dedup();
        if notices.is_empty() {
            return Err(format!("upstream license/notice text missing for {}; cannot distribute without attribution",required(package,"name")?).into());
        }
        let mut notice_ids = Vec::new();
        for notice in notices {
            let content = fs::read_to_string(&notice)?;
            let hash = digest(content.as_bytes());
            unique_notices.insert(hash.clone(), content);
            notice_ids.push(hash);
        }
        audit.push_str(&format!(
            "{} {}: passed known-native and build-script screening; features={}\n",
            required(package, "name")?,
            required(package, "version")?,
            features.join(",")
        ));
        records.push(serde_json::json!({"name":required(package,"name")?,"version":required(package,"version")?,"license":package.get("license"),"source":package.get("source"),"authors":package.get("authors"),"features":features,"notices_sha256":notice_ids}));
    }
    let sbom = serde_json::to_string_pretty(
        &serde_json::json!({"schema":"susi/sbom/v1","target":host,"scope":"normal-and-build dependency closure; development-only dependencies excluded","packages":records}),
    )? + "\n";
    let mut notices=String::from("Susi third-party and first-party license notices. See sbom.json for package-to-notice mapping. Original notice texts follow unchanged.\n");
    for (hash, content) in unique_notices {
        notices.push_str(&format!("\n===== Notice SHA256 {hash} =====\n{content}\n"));
    }
    fs::write(attempt.join("native-source-audit.txt"), &audit)?;
    Ok((sbom, notices, audit))
}

fn run_gates(candidate: &Path, attempt: &Path, host: &str) -> Result<String> {
    let target = candidate.join("target");
    let gates: &[&[&str]] = &[
        &["fmt", "--all", "--check"],
        &["deny", "--offline", "--locked", "check", "licenses"],
        &[
            "clippy",
            "--workspace",
            "--all-targets",
            "--offline",
            "--locked",
            "--target",
            host,
            "--target-dir",
            text(&target)?,
            "--",
            "-D",
            "warnings",
        ],
        &[
            "test",
            "--workspace",
            "--offline",
            "--locked",
            "--target",
            host,
            "--target-dir",
            text(&target)?,
        ],
        &[
            "build",
            "-p",
            "susi",
            "--bin",
            "susi",
            "--release",
            "--offline",
            "--locked",
            "--no-default-features",
            "--target",
            host,
            "--target-dir",
            text(&target)?,
        ],
    ];
    let mut proof = String::new();
    for (index, args) in gates.iter().enumerate() {
        let log = fs::File::create(attempt.join(format!("gate-{index}.log")))?;
        let status = isolated_command(Path::new("cargo"), &attempt.join("gate-home"))?
            .current_dir(candidate)
            .args(*args)
            .env("CARGO_TARGET_DIR", &target)
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log))
            .status()?;
        proof.push_str(&format!("cargo {} => {status}\n", args.join(" ")));
        fs::write(attempt.join("gates.txt"), &proof)?;
        if !status.success() {
            return Err(format!(
                "release qualification failed: cargo {}; evidence {}",
                args.join(" "),
                attempt.display()
            )
            .into());
        }
    }
    Ok(proof)
}

fn smoke(binary: &Path, scratch: &Path) -> Result<()> {
    for (index, arg) in ["--version", "--help"].iter().enumerate() {
        let out = isolated_command(binary, &scratch.join("home"))?
            .arg(arg)
            .output()?;
        fs::write(scratch.join(format!("smoke-{index}.stdout")), &out.stdout)?;
        fs::write(scratch.join(format!("smoke-{index}.stderr")), &out.stderr)?;
        if !out.status.success() || out.stdout.is_empty() {
            return Err(format!("artifact smoke failed: {arg} ({})", out.status).into());
        }
    }
    Ok(())
}

#[derive(Debug)]
struct Manifest {
    source: String,
    tree: String,
    artifact: String,
    bytes: u64,
    host: String,
    integration: String,
    lock: String,
    compiler: String,
    gates: String,
    sbom: String,
    notices: String,
    native_audit: String,
}
impl Manifest {
    fn encode(&self) -> String {
        format!("susi-local-release-v1\nsource={}\ntree={}\nartifact-sha256={}\nartifact-bytes={}\nhost={}\nintegration-receipt={}\nlock-sha256={}\ncompiler-sha256={}\ngates-sha256={}\nsbom-sha256={}\nnotices-sha256={}\nnative-audit-sha256={}\n", self.source,self.tree,self.artifact,self.bytes,self.host,self.integration,self.lock,self.compiler,self.gates,self.sbom,self.notices,self.native_audit)
    }
    fn decode(raw: &str) -> Result<Self> {
        let mut lines = raw.lines();
        if lines.next() != Some("susi-local-release-v1") {
            return Err("invalid release manifest".into());
        }
        let mut values = BTreeMap::new();
        for line in lines {
            let (key, value) = line.split_once('=').ok_or("invalid manifest field")?;
            if values.insert(key, value).is_some() {
                return Err("duplicate manifest field".into());
            }
        }
        if values.len() != 12 {
            return Err("unexpected manifest fields".into());
        }
        let get = |key| values.get(key).copied().ok_or("missing manifest field");
        let result = Self {
            source: get("source")?.into(),
            tree: get("tree")?.into(),
            artifact: get("artifact-sha256")?.into(),
            bytes: get("artifact-bytes")?.parse()?,
            host: get("host")?.into(),
            integration: get("integration-receipt")?.into(),
            lock: get("lock-sha256")?.into(),
            compiler: get("compiler-sha256")?.into(),
            gates: get("gates-sha256")?.into(),
            sbom: get("sbom-sha256")?.into(),
            notices: get("notices-sha256")?.into(),
            native_audit: get("native-audit-sha256")?.into(),
        };
        if ![&result.source, &result.tree, &result.integration]
            .iter()
            .all(|v| hex(v, &[40, 64]))
            || ![
                &result.artifact,
                &result.lock,
                &result.compiler,
                &result.gates,
                &result.sbom,
                &result.notices,
                &result.native_audit,
            ]
            .iter()
            .all(|v| hex(v, &[64]))
            || result.bytes == 0
            || !result
                .host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err("invalid release identity".into());
        }
        Ok(result)
    }
}

fn qualify(ctx: &Context) -> Result<String> {
    let _lock = Lock::acquire(&ctx.state)?;
    // The caller may be a dirty primary checkout. Only the immutable integrated
    // snapshot is built; caller files and branches never participate or move.
    let head = resolve(&ctx.root, INTEGRATED)?.ok_or("no locally integrated source")?;
    let tree = git(&ctx.root, &["rev-parse", &format!("{head}^{{tree}}")])?;
    let integration = integration_receipt(ctx, &head, &tree)?;
    let stamp = stamp()?;
    let attempt = ctx.state.join(format!("qualify-{stamp}"));
    fs::create_dir(&attempt)?;
    let candidate = ctx
        .root
        .parent()
        .ok_or("missing repository parent")?
        .join(format!("susi-release-{stamp}"));
    git(
        &ctx.root,
        &[
            "-c",
            "core.hooksPath=",
            "worktree",
            "add",
            "--detach",
            text(&candidate)?,
            &head,
        ],
    )?;
    let (host, compiler) = compiler(&candidate)?;
    let (sbom, notices, native_audit) = inventory(&candidate, &attempt, &host)?;
    let proof = run_gates(&candidate, &attempt, &host)?;
    let binary = candidate
        .join("target")
        .join(&host)
        .join("release")
        .join(binary_name());
    let before = file_digest(&binary)?;
    smoke(&binary, &attempt)?;
    if file_digest(&binary)? != before {
        return Err("artifact changed during smoke check".into());
    }
    clean(&candidate)?;
    if git(&candidate, &["rev-parse", "HEAD"])? != head {
        return Err("source advanced during release qualification".into());
    }
    integration_receipt(ctx, &head, &tree)?;
    let manifest = Manifest {
        source: head,
        tree,
        artifact: before,
        bytes: fs::metadata(&binary)?.len(),
        host,
        integration,
        lock: file_digest(&candidate.join("Cargo.lock"))?,
        compiler: digest(compiler.as_bytes()),
        gates: digest(proof.as_bytes()),
        sbom: digest(sbom.as_bytes()),
        notices: digest(notices.as_bytes()),
        native_audit: digest(native_audit.as_bytes()),
    };
    let raw = manifest.encode();
    let id = digest(raw.as_bytes());
    let staging = ctx.packages.join(format!("staging-{stamp}"));
    fs::create_dir(&staging)?;
    fs::copy(&binary, staging.join(binary_name()))?;
    fs::write(staging.join("manifest.txt"), &raw)?;
    fs::write(staging.join("gates.txt"), &proof)?;
    fs::write(staging.join("sbom.json"), &sbom)?;
    fs::write(staging.join("THIRD-PARTY-NOTICES.txt"), &notices)?;
    fs::write(staging.join("native-source-audit.txt"), &native_audit)?;
    if file_digest(&staging.join(binary_name()))? != manifest.artifact {
        return Err("staged artifact digest mismatch".into());
    }
    for file in [
        binary_name(),
        "manifest.txt",
        "gates.txt",
        "sbom.json",
        "THIRD-PARTY-NOTICES.txt",
        "native-source-audit.txt",
    ] {
        let path = staging.join(file);
        let mut permissions = fs::metadata(&path)?.permissions();
        permissions.set_readonly(true);
        fs::set_permissions(path, permissions)?;
    }
    let destination = ctx.packages.join("objects").join(&id);
    if destination.exists() {
        return Err(
            "immutable release package already exists; inspect or promote it instead".into(),
        );
    }
    fs::rename(&staging, &destination)?;
    let blob = git_input(&ctx.root, &["hash-object", "-w", "--stdin"], raw.as_bytes())?;
    let reference = format!("refs/susi/releases/qualified/{id}");
    // Verification and qualification publication are one atomic transaction:
    // an integration advance cannot race the final receipt check.
    let transaction = format!(
        "start\nverify {INTEGRATED} {}\ncreate {reference} {blob}\nprepare\ncommit\n",
        manifest.source
    );
    git_input(
        &ctx.root,
        &["update-ref", "--stdin"],
        transaction.as_bytes(),
    )?;
    // Completed candidates have no unique source: the immutable commit and
    // copied artifact remain. Git refuses cleanup if anything became dirty.
    if let Err(error) = git(&ctx.root, &["worktree", "remove", text(&candidate)?]) {
        eprintln!(
            "qualified candidate retained at {}: {error}",
            candidate.display()
        );
    }
    println!(
        "Qualified release {id}; source {}; artifact SHA256 {}. Not promoted.",
        manifest.source, manifest.artifact
    );
    Ok(id)
}

fn checked_package(ctx: &Context, blob: &str) -> Result<(String, PathBuf)> {
    let raw = git(&ctx.root, &["cat-file", "blob", blob])? + "\n";
    let manifest = Manifest::decode(&raw)?;
    let id = digest(raw.as_bytes());
    if resolve(&ctx.root, &format!("refs/susi/releases/qualified/{id}"))?.as_deref() != Some(blob) {
        return Err("release has no qualification reference".into());
    }
    let package = ctx.packages.join("objects").join(&id);
    real_directory(&package)?;
    if fs::read_to_string(package.join("manifest.txt"))? != raw
        || file_digest(&package.join("gates.txt"))? != manifest.gates
        || file_digest(&package.join("sbom.json"))? != manifest.sbom
        || file_digest(&package.join("THIRD-PARTY-NOTICES.txt"))? != manifest.notices
        || file_digest(&package.join("native-source-audit.txt"))? != manifest.native_audit
    {
        return Err("release evidence was modified".into());
    }
    let binary = package.join(binary_name());
    if fs::symlink_metadata(&binary)?.file_type().is_symlink()
        || fs::metadata(&binary)?.len() != manifest.bytes
        || file_digest(&binary)? != manifest.artifact
    {
        return Err("release executable integrity check failed".into());
    }
    let platform_matches = if cfg!(windows) {
        manifest.host.ends_with("-pc-windows-msvc")
    } else if cfg!(target_os = "macos") {
        manifest.host.ends_with("-apple-darwin")
    } else if cfg!(target_os = "linux") {
        manifest.host.contains("-linux-")
    } else {
        false
    };
    if manifest.host.split('-').next() != Some(env::consts::ARCH) || !platform_matches {
        return Err("release targets a different platform".into());
    }
    Ok((id, binary))
}

fn activate(ctx: &Context, target: &str) -> Result<()> {
    let old = resolve(&ctx.root, CURRENT)?;
    let previous = resolve(&ctx.root, PREVIOUS)?;
    let (id, binary) = checked_package(ctx, target)?;
    let scratch = ctx.state.join(format!("activate-{}", stamp()?));
    fs::create_dir(&scratch)?;
    smoke(&binary, &scratch)?;
    checked_package(ctx, target)?;
    if old.as_deref() == Some(target) {
        println!("Release {id} is already current");
        return Ok(());
    }
    let zero = "0".repeat(target.len());
    let mut transaction = format!(
        "start\nupdate {CURRENT} {target} {}\n",
        old.as_deref().unwrap_or(&zero)
    );
    if let Some(old) = old {
        transaction.push_str(&format!(
            "update {PREVIOUS} {old} {}\n",
            previous.as_deref().unwrap_or(&zero)
        ));
    } else if previous.is_some() {
        return Err("previous release exists without current release".into());
    }
    transaction.push_str("prepare\ncommit\n");
    git_input(
        &ctx.root,
        &["update-ref", "--stdin"],
        transaction.as_bytes(),
    )?;
    println!("Current verified local release: {id}. Running executables were not overwritten.");
    Ok(())
}

fn promote(ctx: &Context, id: &str) -> Result<()> {
    if !hex(id, &[64]) {
        return Err("release id must be a SHA256 digest".into());
    }
    let _lock = Lock::acquire(&ctx.state)?;
    let blob = resolve(&ctx.root, &format!("refs/susi/releases/qualified/{id}"))?
        .ok_or("release is not qualified")?;
    activate(ctx, &blob)
}

fn rollback(ctx: &Context) -> Result<()> {
    let _lock = Lock::acquire(&ctx.state)?;
    let blob = resolve(&ctx.root, PREVIOUS)?.ok_or("no previous verified release")?;
    activate(ctx, &blob)
}

fn run(args: &[String]) -> Result<()> {
    let ctx = Context::open(&env::current_dir()?)?;
    match args.first().map(String::as_str) {
        Some("qualify") if args.len()==1 => { qualify(&ctx)?; }
        Some("promote") if args.len()==2 => promote(&ctx,&args[1])?,
        Some("rollback") if args.len()==1 => rollback(&ctx)?,
        Some("status") if args.len()==1 => {
            for reference in [CURRENT,PREVIOUS] {
                if let Some(blob)=resolve(&ctx.root,reference)? { let(id,path)=checked_package(&ctx,&blob)?;println!("{reference}: {id} {}",path.display()); }
                else {println!("{reference}: none");}
            }
        }
        Some("run") => {
            let blob = resolve(&ctx.root,CURRENT)?.ok_or("no promoted local release")?;
            let(_,binary)=checked_package(&ctx,&blob)?;
            let runtime=ctx.packages.join("runtime");real_directory(&runtime)?;
            let status=Command::new(binary).args(&args[1..]).env("SUSI_HOME",runtime).status()?;
            if !status.success(){return Err(format!("release process exited {status}").into());}
        }
        _ => return Err("usage: local-release qualify | promote <release-id> | rollback | status | run [susi arguments]".into()),
    }
    Ok(())
}

fn main() -> ExitCode {
    match run(&env::args().skip(1).collect::<Vec<_>>()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("local-release: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let directory = env::temp_dir().join(format!("susi-release-test-{}", stamp().unwrap()));
            fs::create_dir(&directory).unwrap();
            let repo = directory.join("repo");
            fs::create_dir(&repo).unwrap();
            git(&repo, &["init", "-q"]).unwrap();
            git(&repo, &["config", "user.name", "Test"]).unwrap();
            git(&repo, &["config", "user.email", "test@example.invalid"]).unwrap();
            git(&repo, &["config", "commit.gpgsign", "false"]).unwrap();
            fs::write(repo.join(".gitignore"), "/target/\n").unwrap();
            fs::write(repo.join("Cargo.toml"),"[package]\nname = \"susi\"\nversion = \"0.1.0\"\nedition = \"2021\"\nlicense = \"MIT\"\n").unwrap();
            fs::write(
                repo.join("Cargo.lock"),
                "version = 4\n\n[[package]]\nname = \"susi\"\nversion = \"0.1.0\"\n",
            )
            .unwrap();
            fs::write(repo.join("deny.toml"), "[licenses]\nallow = [\"MIT\"]\n").unwrap();
            fs::write(repo.join("LICENSE"), "MIT License\n\nCopyright (c) 2026 Susi test fixture\n\nPermission is hereby granted, free of charge, to any person obtaining a copy of this software and associated documentation files (the Software), to deal in the Software without restriction, including without limitation the rights to use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of the Software, and to permit persons to whom the Software is furnished to do so, subject to the following conditions:\n\nThe above copyright notice and this permission notice shall be included in all copies or substantial portions of the Software.\n\nTHE SOFTWARE IS PROVIDED AS IS, WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.\n").unwrap();
            fs::create_dir(repo.join("src")).unwrap();
            let fixture = Self(directory);
            fixture.revision("one", true);
            fixture
        }
        fn root(&self) -> PathBuf {
            self.0.join("repo")
        }
        fn revision(&self, version: &str, passes: bool) {
            let root = self.root();
            let expected = if passes { 4 } else { 5 };
            fs::write(root.join("src/main.rs"),format!("fn main() {{\n    println!(\"fixture {version}\");\n}}\n\n#[test]\nfn fixture_test() {{\n    let actual = std::hint::black_box(4);\n    assert_eq!(actual, {expected});\n}}\n")).unwrap();
            git(&root, &["add", "."]).unwrap();
            git(&root, &["-c", "core.hooksPath=", "commit", "-qm", version]).unwrap();
            let head = git(&root, &["rev-parse", "HEAD"]).unwrap();
            let tree = git(&root, &["rev-parse", "HEAD^{tree}"]).unwrap();
            // Integration is a fixture boundary; release still runs all real
            // native qualification commands against this exact committed tree.
            let receipt=format!("susi-local-integration-v1\ncommit={head}\ntree={tree}\nfixture-only integration proof\n");
            let blob =
                git_input(&root, &["hash-object", "-w", "--stdin"], receipt.as_bytes()).unwrap();
            git(&root, &["update-ref", INTEGRATED, &head]).unwrap();
            git(
                &root,
                &[
                    "update-ref",
                    &format!("refs/susi/integration/receipts/fixture/{head}"),
                    &blob,
                ],
            )
            .unwrap();
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            // Retain failed fixtures and their logs for diagnosis. Successful
            // test releases contain readonly immutable artifacts; leave their
            // bounded temp tree to the host's normal temporary-file cleanup.
        }
    }

    #[test]
    fn sha256_and_manifest_reject_invalid_identity() {
        assert_eq!(
            digest(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert!(Manifest::decode("susi-local-release-v1\nsource=../escape\n").is_err());
        let manifest = Manifest {
            source: "a".repeat(40),
            tree: "b".repeat(40),
            artifact: "c".repeat(64),
            bytes: 1,
            host: "x86_64-pc-windows-msvc".into(),
            integration: "d".repeat(40),
            lock: "e".repeat(64),
            compiler: "f".repeat(64),
            gates: "0".repeat(64),
            sbom: "0".repeat(64),
            notices: "0".repeat(64),
            native_audit: "0".repeat(64),
        };
        assert!(Manifest::decode(&manifest.encode()).is_ok());
        assert!(Manifest::decode(&(manifest.encode() + "source=duplicate\n")).is_err());
    }

    #[test]
    fn native_source_screen_rejects_known_native_paths_and_active_compilers() {
        let fixture = Fixture::new();
        let build = fixture.root().join("build.rs");
        fs::write(
            &build,
            "fn main() { cc::Build::new().file(\"native.c\").compile(\"native\"); }",
        )
        .unwrap();
        let package = serde_json::json!({"name":"example-native","targets":[{"kind":["custom-build"],"src_path":build}]});
        assert!(native_source_check(&package, &[]).is_err());
        assert!(native_source_check(&serde_json::json!({"name":"ring"}), &[]).is_err());
        let dormant = serde_json::json!({"name":"esaxx-rs","targets":[{"kind":["custom-build"],"src_path":build}]});
        assert!(native_source_check(&dormant, &[]).is_ok());
        assert!(native_source_check(&dormant, &["cpp".into()]).is_err());
    }

    #[test]
    fn shipped_inventory_excludes_dev_only_edges_and_requires_notice_text() {
        let fixture = Fixture::new();
        let root = fixture.root();
        let dev = root.join("dev-only");
        fs::create_dir(&dev).unwrap();
        fs::write(dev.join("Cargo.toml"),"[package]\nname = \"ring\"\nversion = \"0.1.0\"\nedition = \"2021\"\nlicense = \"MIT\"\n[lib]\npath = \"lib.rs\"\n").unwrap();
        fs::write(dev.join("lib.rs"), "// Test-only package naming fixture.\n").unwrap();
        let mut manifest = fs::OpenOptions::new()
            .append(true)
            .open(root.join("Cargo.toml"))
            .unwrap();
        writeln!(
            manifest,
            "[dev-dependencies]\nring = {{ path = \"dev-only\" }}"
        )
        .unwrap();
        assert!(Command::new("cargo")
            .current_dir(&root)
            .args(["generate-lockfile", "--offline"])
            .status()
            .unwrap()
            .success());
        let attempt = fixture.0.join("inventory");
        fs::create_dir(&attempt).unwrap();
        let (host, _) = compiler(&root).unwrap();
        let (sbom, notices, _) = inventory(&root, &attempt, &host).unwrap();
        let sbom: serde_json::Value = serde_json::from_str(&sbom).unwrap();
        assert_eq!(sbom["packages"].as_array().unwrap().len(), 1);
        assert!(notices.contains("Copyright (c) 2026 Susi test fixture"));
        fs::remove_file(root.join("LICENSE")).unwrap();
        assert!(inventory(&root, &attempt, &host).is_err());
    }

    #[test]
    fn workspace_notice_fallback_does_not_relicense_vendor_packages() {
        let fixture = Fixture::new();
        let root = fixture.root();
        let member = root.join("crates/member");
        let external = root.join("vendor/external");
        for (path, name) in [(&member, "member"), (&external, "external")] {
            fs::create_dir_all(path).unwrap();
            fs::write(path.join("Cargo.toml"),format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\nlicense = \"MIT\"\n[lib]\npath = \"lib.rs\"\n")).unwrap();
            fs::write(path.join("lib.rs"), "// Test package.\n").unwrap();
        }
        let mut manifest = fs::read_to_string(root.join("Cargo.toml")).unwrap();
        manifest.push_str("[dependencies]\nmember = { path = \"crates/member\" }\n[workspace]\nmembers = [\"crates/member\"]\nexclude = [\"vendor/external\"]\n");
        fs::write(root.join("Cargo.toml"), &manifest).unwrap();
        assert!(Command::new("cargo")
            .current_dir(&root)
            .args(["generate-lockfile", "--offline"])
            .status()
            .unwrap()
            .success());
        let attempt = fixture.0.join("inventory");
        fs::create_dir(&attempt).unwrap();
        let (host, _) = compiler(&root).unwrap();
        let (sbom, _, _) = inventory(&root, &attempt, &host).unwrap();
        let sbom: serde_json::Value = serde_json::from_str(&sbom).unwrap();
        assert_eq!(sbom["packages"].as_array().unwrap().len(), 2);
        manifest = manifest.replace(
            "[dependencies]\n",
            "[dependencies]\nexternal = { path = \"vendor/external\" }\n",
        );
        fs::write(root.join("Cargo.toml"), manifest).unwrap();
        assert!(Command::new("cargo")
            .current_dir(&root)
            .args(["generate-lockfile", "--offline"])
            .status()
            .unwrap()
            .success());
        let error = inventory(&root, &attempt, &host).unwrap_err().to_string();
        assert!(
            error.contains("notice text missing for external"),
            "{error}"
        );
    }

    #[test]
    fn real_qualification_promotion_rollback_and_failure_preserve_verified_release() {
        let fixture = Fixture::new();
        let ctx = Context::open(&fixture.root()).unwrap();
        let first = qualify(&ctx).unwrap();
        assert_eq!(resolve(&ctx.root, CURRENT).unwrap(), None);
        promote(&ctx, &first).unwrap();
        let first_blob = resolve(&ctx.root, CURRENT).unwrap().unwrap();
        assert_eq!(checked_package(&ctx, &first_blob).unwrap().0, first);
        assert!(rollback(&ctx).is_err());
        fixture.revision("two", true);
        let second = qualify(&ctx).unwrap();
        assert_ne!(first, second);
        promote(&ctx, &second).unwrap();
        let second_blob = resolve(&ctx.root, CURRENT).unwrap().unwrap();
        assert_eq!(
            resolve(&ctx.root, PREVIOUS).unwrap(),
            Some(first_blob.clone())
        );
        rollback(&ctx).unwrap();
        assert_eq!(
            resolve(&ctx.root, CURRENT).unwrap(),
            Some(first_blob.clone())
        );
        assert_eq!(resolve(&ctx.root, PREVIOUS).unwrap(), Some(second_blob));
        fixture.revision("broken", false);
        assert!(qualify(&ctx).is_err());
        assert_eq!(resolve(&ctx.root, CURRENT).unwrap(), Some(first_blob));
        assert!(promote(&ctx, &"a".repeat(64)).is_err());
        assert!(promote(&ctx, "../escape").is_err());
        // Missing or replaced artifact bytes cannot be activated from a valid
        // reference. Removing only this fixture file models corruption.
        let corrupted = ctx
            .packages
            .join("objects")
            .join(second)
            .join(binary_name());
        fs::rename(&corrupted, corrupted.with_extension("retained")).unwrap();
        assert!(rollback(&ctx).is_err());
        assert_eq!(
            checked_package(&ctx, &resolve(&ctx.root, CURRENT).unwrap().unwrap())
                .unwrap()
                .0,
            first
        );
    }

    #[test]
    fn qualification_uses_integrated_snapshot_and_preserves_dirty_primary() {
        let fixture = Fixture::new();
        let ctx = Context::open(&fixture.root()).unwrap();
        fs::write(ctx.root.join("untracked"), "preserve").unwrap();
        let id = qualify(&ctx).unwrap();
        assert!(hex(&id, &[64]));
        assert_eq!(
            fs::read_to_string(ctx.root.join("untracked")).unwrap(),
            "preserve"
        );
        fs::remove_file(ctx.root.join("untracked")).unwrap();
        git(&ctx.root, &["update-ref", "-d", INTEGRATED]).unwrap();
        assert!(qualify(&ctx).is_err());
        assert_eq!(resolve(&ctx.root, CURRENT).unwrap(), None);
    }
}
