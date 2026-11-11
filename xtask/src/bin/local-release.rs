//! Native local release qualification, immutable packages and atomic promotion.
#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use sha2::{Digest, Sha256};
#[cfg(windows)]
extern crate susi_windows_runtime;

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

#[path = "../compiler_env.rs"]
mod compiler_env;
#[path = "../workspace_test_count.rs"]
mod workspace_test_count;
#[allow(dead_code)]
#[path = "../workspace_test_proof.rs"]
mod workspace_test_proof;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const QUALIFICATION_POLICY: &str =
    include!("../../../src/installed_channel/qualification_policy.rs");
const CURRENT: &str = "refs/susi/releases/current";
const PREVIOUS: &str = "refs/susi/releases/previous";
const INTEGRATED: &str = "refs/susi/integration/current";

#[path = "../advisory_evidence.rs"]
mod advisory_evidence;
#[path = "../complete_release_gates.rs"]
mod complete_release_gates;
#[path = "../elf_imports.rs"]
mod elf_imports;
#[path = "../gmcp_evidence.rs"]
mod gmcp_evidence;
#[path = "../installed_delivery.rs"]
mod installed_delivery;
#[path = "../native_openssl.rs"]
mod native_openssl;
#[path = "../prepare_release.rs"]
mod prepare_release;
#[path = "../product_features.rs"]
mod product_features;
#[path = "../release_coordinator.rs"]
mod release_coordinator;
#[path = "../sysroot_license.rs"]
mod sysroot_license;
#[path = "../sysroot_sources.rs"]
mod sysroot_sources;
#[path = "../trusted_sysroot.rs"]
mod trusted_sysroot;
#[path = "../upstream_notices.rs"]
mod upstream_notices;
#[path = "../worktree_admin.rs"]
mod worktree_admin;
use susi_build_os_ucrt::{native_link, pe_imports};
use susi_build_policy as vendor_regressions;

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
    trust_registry: Option<PathBuf>,
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
            trust_registry: env::var_os("SUSI_RELEASE_TRUST_REGISTRY").map(PathBuf::from),
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
    fs::create_dir_all(home.join("AppData/Local"))?;
    let original = env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .ok_or("missing provisioned toolchain home")?;
    let mut command = Command::new(program);
    compiler_env::apply_checked(&mut command)?;
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
                "API_BASE",
                "BASE_URL",
                "API_URL",
                "CUSTOM_HEADERS",
                "HTTP_PROXY",
                "HTTPS_PROXY",
                "ALL_PROXY",
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
        .env("SUSI_NO_REGISTRY_ENV", "1")
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

// Cargo metadata may unify features across unrelated workspace members. A
// native-code exemption must instead be corroborated by the selected product.
fn selected_feature_contexts(tree: &str, name: &str, version: &str) -> Result<Vec<Vec<String>>> {
    let wanted = format!("{name} v{version}");
    let mut contexts = BTreeSet::new();
    for line in tree.lines() {
        let Some((package, features)) = line.split_once('|') else {
            continue;
        };
        if package == wanted || package.starts_with(&format!("{wanted} (")) {
            contexts.insert(
                features
                    .trim_end_matches(" (*)")
                    .split(',')
                    .filter(|feature| !feature.is_empty())
                    .map(str::to_owned)
                    .collect::<BTreeSet<_>>(),
            );
        }
    }
    if contexts.is_empty() {
        return Err("selected product feature proof missing".into());
    }
    Ok(contexts
        .into_iter()
        .map(|context| context.into_iter().collect())
        .collect())
}

#[cfg(test)]
fn selected_features(tree: &str, name: &str, version: &str) -> Result<Vec<String>> {
    let mut contexts = selected_feature_contexts(tree, name, version)?;
    if contexts.len() != 1 {
        return Err("ambiguous selected product feature resolution".into());
    }
    Ok(contexts.remove(0))
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
        || (name == "blake3" && features.iter().any(|f| f == "pure"))
        // signal-hook's build.rs compiles extract.c only under this feature;
        // without it the selected build-script main is empty on every target.
        || (name == "signal-hook"
            && package.get("version").and_then(serde_json::Value::as_str) == Some("0.4.5")
            && !features.iter().any(|f| f == "extended-siginfo-raw"));
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
        .args(["metadata", "--offline", "--locked"])
        .args(product_features::product_feature_args(candidate))
        .args(["--format-version", "1", "--filter-platform", host])
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
    // Metadata includes features unified by workspace and development consumers.
    // The artifact inventory must instead use this exact product's selected
    // normal/build graph, with the same target and features as its build.
    let output = isolated_command(Path::new("cargo"), &attempt.join("metadata-home"))?
        .current_dir(candidate)
        .args(["tree", "--offline", "--locked", "-p", "susi"])
        .args(product_features::product_feature_args(candidate))
        .args([
            "--target",
            host,
            "--edges",
            "normal,build",
            "--prefix",
            "none",
            "--format",
            "{p}|{f}",
        ])
        .output()?;
    if !output.status.success() {
        return Err("selected product graph proof failed".into());
    }
    let tree = String::from_utf8(output.stdout)?;
    let mut selected = BTreeSet::new();
    for line in tree.lines() {
        let (package, _) = line
            .split_once('|')
            .ok_or("invalid selected product graph row")?;
        let mut words = package.split_whitespace();
        let name = words.next().ok_or("selected package name missing")?;
        let version = words
            .next()
            .and_then(|v| v.strip_prefix('v'))
            .ok_or("selected package version missing")?;
        selected.insert((name.to_owned(), version.to_owned()));
    }
    if !selected.contains(&("susi".into(), required(root, "version")?.into())) {
        return Err("selected graph does not contain the product root".into());
    }
    let mut records = Vec::new();
    let mut missing_notices = Vec::new();
    let mut unique_notices = BTreeMap::new();
    let mut audit=String::from("susi-native-source-screen-v1\nCoverage: resolved native target normal/build closure, known native crates and direct build-script compiler markers. This screening is not a proof that arbitrary generated or downloaded code is Rust.\n");
    let source_root = candidate.canonicalize()?;
    for (name, version) in selected {
        let matches: Vec<_> = packages
            .iter()
            .filter(|package| {
                package.get("name").and_then(serde_json::Value::as_str) == Some(&name)
                    && package.get("version").and_then(serde_json::Value::as_str) == Some(&version)
            })
            .collect();
        if matches.len() != 1 {
            return Err(format!("ambiguous selected source identity: {name} {version}").into());
        }
        let package = matches[0];
        let id = required(package, "id")?;
        let feature_contexts = selected_feature_contexts(&tree, &name, &version)?;
        // Host build dependencies and target dependencies can legitimately
        // select different features. Screen each context separately: unioning
        // `pure` from one context must never hide native code in another.
        for context in &feature_contexts {
            if name == "openssl-sys" && native_openssl::supported(host) {
                native_openssl::admit(
                    host,
                    &version,
                    context,
                    Path::new(required(package, "manifest_path")?),
                )?;
                audit.push_str("OpenSSL Rust binding admitted provisionally: dynamic OS library and link-map proof required at artifact qualification.\n");
            } else {
                native_source_check(package, context)?;
            }
        }
        let features: Vec<_> = feature_contexts
            .iter()
            .flatten()
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
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
                .any(|member| member.as_str() == Some(id));
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
        if notices.is_empty() && !workspace {
            notices = upstream_notices::verify(candidate, package, &directory)?;
        }
        if notices.is_empty() {
            missing_notices.push(required(package, "name")?.to_owned());
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
        records.push(serde_json::json!({"name":required(package,"name")?,"version":required(package,"version")?,"license":package.get("license"),"source":package.get("source"),"authors":package.get("authors"),"features":features,"feature_contexts":feature_contexts,"notices_sha256":notice_ids}));
    }
    fs::write(attempt.join("native-source-audit.txt"), &audit)?;
    if !missing_notices.is_empty() {
        return Err(format!(
            "upstream license/notice text missing for {}; cannot distribute without attribution",
            missing_notices.join(", ")
        )
        .into());
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

fn native_link_proof(binary: &Path, map: &Path) -> Result<String> {
    if fs::metadata(binary)?.len() > 256 * 1024 * 1024
        || fs::metadata(map)?.len() > 64 * 1024 * 1024
    {
        return Err("native artifact evidence exceeds budget".into());
    }
    let bytes = fs::read(binary)?;
    let imports = pe_imports::inspect(&bytes)?;
    let map_text = fs::read_to_string(map)?;
    let entry = native_link::validate_map(native_link::entry_address(&bytes)?, &map_text)?;
    Ok(format!(
        "native-link={}\n",
        serde_json::to_string(
            &serde_json::json!({"binary":binary.file_name().and_then(|name|name.to_str()),"artifact_sha256":digest(&bytes),"entry":entry,"imports":imports,"map_sha256":digest(map_text.as_bytes()),"map":map_text})
        )?
    ))
}

#[allow(dead_code)]
fn receipt_covers_workspace_test(receipt: &str, key: &str) -> bool {
    let mut proofs = receipt
        .lines()
        .filter(|line| line.starts_with(workspace_test_proof::PREFIX));
    let successes = receipt
        .lines()
        .filter(|line| {
            line.starts_with("cargo test --workspace ")
                && line.split_once(" => ").is_some_and(|(_, status)| {
                    status
                        .strip_prefix("exit code: 0; elapsed-ms=")
                        .is_some_and(|ms| ms.parse::<u128>().is_ok())
                })
        })
        .count();
    successes == 1
        && proofs.next() == Some(workspace_test_proof::record(key).as_str())
        && proofs.next().is_none()
        && !receipt.lines().any(|line| {
            line.split_once(" => ")
                .is_some_and(|(_, s)| !s.starts_with("exit code: 0;"))
        })
}
#[allow(dead_code)]
fn authenticated_workspace_pass(root: &Path, receipt: &str, key: &str) -> bool {
    let check = || -> Result<bool> {
        if !receipt_covers_workspace_test(receipt, key) {
            return Ok(false);
        }
        let mut signatures = receipt
            .lines()
            .filter_map(|line| line.strip_prefix("workspace-test-attestation-v1="));
        let raw: String =
            serde_json::from_str(signatures.next().ok_or("missing signed workspace pass")?)?;
        if signatures.next().is_some() {
            return Ok(false);
        }
        if !raw.starts_with("-----BEGIN PGP SIGNED MESSAGE-----") {
            return Ok(false);
        }
        let signer = git(root, &["config", "--get", "user.signingkey"])?.to_ascii_uppercase();
        if !matches!(signer.len(), 16 | 40 | 64) || !signer.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Ok(false);
        }
        let keyring = workspace_test_proof::gpg(
            root,
            &["--with-colons", "--fingerprint", "--list-keys", &signer],
            &[],
        )?;
        let listing = String::from_utf8(keyring.stdout)?;
        let mut primary = None;
        let mut want_primary = false;
        for line in listing.lines() {
            if line.starts_with("pub:") {
                if primary.is_some() {
                    return Ok(false);
                }
                want_primary = true;
            } else if want_primary && line.starts_with("fpr:") {
                primary = line.split(':').nth(9);
                want_primary = false;
            }
        }
        let primary = primary.ok_or("missing configured primary signing fingerprint")?;
        let out =
            workspace_test_proof::gpg(root, &["--status-fd", "2", "--decrypt"], raw.as_bytes())?;
        let status = String::from_utf8(out.stderr)?;
        let mut valid = status
            .lines()
            .filter_map(|line| line.strip_prefix("[GNUPG:] VALIDSIG "));
        let Some(signature) = valid.next() else {
            return Ok(false);
        };
        let expected = workspace_test_proof::signed_input(root, key)?;
        let text = String::from_utf8(out.stdout)?.replace("\r\n", "\n");
        Ok(valid.next().is_none()
            && signature
                .split_whitespace()
                .last()
                .is_some_and(|fingerprint| fingerprint == primary)
            && text == expected)
    };
    check().unwrap_or(false)
}

#[cfg(test)]
mod receipt_match_tests {
    use super::{receipt_covers_workspace_test, workspace_test_proof::record};
    #[test]
    fn workspace_test_proof_rejects_missing_changed_duplicate_and_failed_records() {
        let pass = format!(
            "cargo test --workspace --locked => exit code: 0; elapsed-ms=3\n{}",
            record("expected")
        );
        assert!(receipt_covers_workspace_test(&pass, "expected"));
        for invalid in [
            "old receipt".into(),
            record("expected"),
            record("other"),
            format!("{pass}\n{pass}"),
            format!("{pass}\ncargo test => exit code: 1;"),
        ] {
            assert!(!receipt_covers_workspace_test(&invalid, "expected"));
        }
    }
}

struct GateContext<'a> {
    candidate: &'a Path,
    attempt: &'a Path,
    host: &'a str,
    compiler: &'a str,
}

struct GateRun {
    proof: String,
    sysroot: Option<trusted_sysroot::VerifiedSysroot>,
    native: complete_release_gates::Native,
    records: Vec<serde_json::Value>,
}

fn run_gates(
    context: GateContext<'_>,
    advisory_source: &advisory_evidence::Source,
    complete_runner: &complete_release_gates::Runner,
) -> Result<GateRun> {
    let GateContext {
        candidate,
        attempt,
        host,
        compiler,
    } = context;
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
    let mut gates: Vec<Vec<String>> = gates
        .iter()
        .map(|args| args.iter().map(|arg| (*arg).into()).collect())
        .collect();
    // The production build ships the product's features (the local engine).
    for gate in &mut gates {
        product_features::select_product_features(gate, candidate);
    }
    vendor_regressions::include_release_tooling(candidate, &mut gates);
    if candidate.join("crates/susi-core/Cargo.toml").is_file() {
        gates.push(
            [
                "build",
                "-p",
                "xtask",
                "--features",
                "release-tooling",
                "--bins",
                "--release",
                "--offline",
                "--locked",
                "--target",
                host,
                "--target-dir",
                text(&target)?,
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        );
    }
    let adapters = vendor_regressions::commands(candidate, host, &target)?;
    gates.splice(4..4, adapters);
    gates.insert(2, advisory_evidence::arguments(candidate, attempt, host)?);
    gates.insert(
        3,
        complete_release_gates::all_features_args(candidate, attempt)?,
    );
    let mut sysroot = None;
    let mut advisory = None;
    let mut native = None;
    let mut complete_records = Vec::new();
    let mut proof = String::from(QUALIFICATION_POLICY);
    for (index, args) in gates.iter().enumerate() {
        if index == 2 {
            let snapshot =
                advisory_evidence::Snapshot::prepare(candidate, attempt, advisory_source)?;
            proof.push_str(&format!(
                "advisory-evidence={}\n",
                serde_json::to_string(&snapshot.evidence)?
            ));
            advisory = Some(snapshot);
        }
        if index == 4 {
            native = Some(complete_runner.native(candidate, attempt, host)?);
        }
        if sysroot.is_none()
            && matches!(
                args.first().map(String::as_str),
                Some("clippy" | "test" | "build")
            )
            && candidate.join("crates/susi-core/Cargo.toml").is_file()
        {
            sysroot = Some(sysroot_license::qualify(
                candidate, attempt, host, compiler,
            )?);
        }
        let log_path = attempt.join(format!("gate-{index}.log"));
        let log = fs::File::create(&log_path)?;
        let started = std::time::Instant::now();
        let scanner = if matches!(index, 2 | 3) {
            advisory.as_ref()
        } else {
            None
        };
        let program = scanner.map_or_else(
            || Path::new("cargo"),
            |snapshot| snapshot.executable.as_path(),
        );
        let mut command = isolated_command(program, &attempt.join("gate-home"))?;
        if let Some(sysroot) = &sysroot {
            sysroot.apply(&mut command)?;
        }
        if native_openssl::supported(host) {
            elf_imports::configure_system_openssl(&mut command, host)?;
        }
        let gate_target = args
            .windows(2)
            .find_map(|pair| (pair[0] == "--target-dir").then(|| PathBuf::from(&pair[1])))
            .unwrap_or_else(|| target.clone());
        let status = command
            .current_dir(candidate)
            .args(if scanner.is_some() { &args[1..] } else { args })
            .env("CARGO_TARGET_DIR", &gate_target)
            .env("CARGO_BUILD_TARGET_DIR", &gate_target)
            .env("CARGO_BUILD_BUILD_DIR", &gate_target)
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log))
            .status()?;
        // ExitStatus Display differs across OSes ("exit status" on Unix).
        // Durable receipts use one explicit numeric wire representation.
        let status_text = status
            .code()
            .map_or_else(|| status.to_string(), |code| format!("exit code: {code}"));
        proof.push_str(&format!(
            "cargo {} => {status_text}; elapsed-ms={}\n",
            args.join(" "),
            started.elapsed().as_millis()
        ));
        fs::write(attempt.join("gates.txt"), &proof)?;
        if let Some(snapshot) = scanner {
            snapshot.recheck()?;
        }
        if !status.success() {
            return Err(format!(
                "release qualification failed: cargo {}; evidence {}",
                args.join(" "),
                attempt.display()
            )
            .into());
        }
        if args.first().is_some_and(|arg| arg == "test")
            && args.iter().any(|arg| arg == "--workspace")
        {
            let passed = workspace_test_count::verified_pass_count(&log_path)?;
            proof.push_str(&format!("workspace-tests-passed={passed}\n"));
            fs::write(attempt.join("gates.txt"), &proof)?;
        }
        let mut record = complete_release_gates::record(
            args,
            status.code(),
            started.elapsed().as_millis(),
            &attempt.join(format!("gate-{index}.log")),
            candidate,
        )?;
        if let Some(snapshot) = scanner {
            record["tool_sha256"] = snapshot.evidence["cargo_deny_sha256"].clone();
        }
        complete_records.push(record);
    }
    advisory
        .ok_or("production advisory gate did not execute")?
        .recheck()?;
    if let Some(sysroot) = &sysroot {
        sysroot.recheck()?;
        proof.push_str(&format!(
            "sysroot={}\n",
            serde_json::to_string(&sysroot.evidence)?
        ));
    }
    Ok(GateRun {
        proof,
        sysroot,
        native: native.ok_or("required native gates did not execute")?,
        records: complete_records,
    })
}

const ARTIFACT_BUDGET_BYTES: u64 = 256 * 1024 * 1024;
const HEALTH_RSS_BUDGET_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const HEALTH_OUTPUT_BUDGET_BYTES: u64 = 2 * 1024 * 1024;
const HEALTH_BUDGET: std::time::Duration = std::time::Duration::from_secs(30);

/// These limits are versioned acceptance policy, not user configuration or a
/// claim that the program consumes zero resources. RSS is sampled foreground
/// process memory: it does not measure descendants or an OS peak working set.
fn measured_health(
    mut command: Command,
    scratch: &Path,
    label: &str,
    timeout: std::time::Duration,
    input: &[u8],
) -> Result<String> {
    let stdout = fs::File::create(scratch.join(format!("{label}.stdout")))?;
    let stderr = fs::File::create(scratch.join(format!("{label}.stderr")))?;
    let input_path = scratch.join(format!("{label}.stdin"));
    fs::write(&input_path, input)?;
    command
        .stdin(fs::File::open(input_path)?)
        .stdout(stdout.try_clone()?)
        .stderr(stderr.try_clone()?);
    let started = std::time::Instant::now();
    let mut child = command.spawn()?;
    let pid = child.id();
    let mut sampled_rss = 0;
    let outcome = (|| -> Result<_> {
        loop {
            if let Some(kb) = susi_config::process_probe::rss_kb(pid) {
                sampled_rss = sampled_rss.max(kb * 1024);
            }
            let output_bytes = stdout.metadata()?.len() + stderr.metadata()?.len();
            if sampled_rss > HEALTH_RSS_BUDGET_BYTES
                || output_bytes > HEALTH_OUTPUT_BUDGET_BYTES
                || started.elapsed() > timeout
            {
                return Err(format!("health resource budget exceeded: {label}").into());
            }
            if let Some(status) = child.try_wait()? {
                if !status.success() || stdout.metadata()?.len() == 0 {
                    return Err(format!("artifact health failed: {label} ({status})").into());
                }
                return Ok(output_bytes);
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    })();
    if outcome.is_err() {
        // Reap the exact owned child, never kill an unrelated system process.
        let _ = child.kill();
        let _ = child.wait();
    }
    let report = serde_json::json!({
        "command": label,
        "elapsed_ms": started.elapsed().as_millis(),
        "sampled_foreground_rss_bytes": sampled_rss,
        "rss_budget_bytes": HEALTH_RSS_BUDGET_BYTES,
        "deadline_ms": timeout.as_millis(),
        "output_budget_bytes": HEALTH_OUTPUT_BUDGET_BYTES,
        "output_bytes": stdout.metadata()?.len() + stderr.metadata()?.len(),
        "passed": outcome.is_ok(),
    })
    .to_string();
    fs::write(scratch.join(format!("{label}.resources.json")), &report)?;
    outcome?;
    Ok(report)
}

fn smoke(binary: &Path, scratch: &Path) -> Result<String> {
    let bytes = fs::metadata(binary)?.len();
    if bytes == 0 || bytes > ARTIFACT_BUDGET_BYTES {
        return Err("artifact exceeds the 256 MiB release budget or is empty".into());
    }
    let mut proof = format!("artifact-bytes={bytes};budget={ARTIFACT_BUDGET_BYTES}\n");
    // Status exercises real startup and substrate reporting beyond Clap's
    // version/help early exits. This does not certify inference or a daemon.
    for (index, arg) in ["--version", "--help", "status"].iter().enumerate() {
        let mut command = isolated_command(binary, &scratch.join("home"))?;
        command.current_dir(scratch).arg(arg);
        let report = measured_health(
            command,
            scratch,
            &format!("smoke-{index}"),
            HEALTH_BUDGET,
            b"",
        )?;
        proof.push_str(&format!("health {arg}: {report}\n"));
    }
    Ok(proof)
}

const INFERENCE_PROMPTS: [&str; 2] = ["What is 2+2?", "Write one short sentence about a red kite."];
const STARTER_FILES: [&str; 4] = [
    "qwen2.5-0.5b-instruct-q4_0.gguf",
    "qwen2.5-0.5b-instruct-q4_0.license.json",
    "qwen2.5-0.5b-instruct-q4_0.tokenizer.json",
    "qwen2.5-0.5b-instruct-q4_0.tokenizer.license.json",
];

fn bounded_evidence(path: &Path) -> Result<String> {
    let file = fs::File::open(path)?;
    let mut body = String::new();
    file.take(8 * 1024 * 1024 + 1).read_to_string(&mut body)?;
    if body.len() > 8 * 1024 * 1024 {
        return Err("inference evidence exceeded its 8 MiB read budget".into());
    }
    Ok(body)
}

fn verify_runtime_stopped(runtime: &Path) -> Result<()> {
    let runtime = runtime.canonicalize()?;
    let started = std::time::Instant::now();
    loop {
        let owned = susi_config::process_probe::processes(false)
            .into_iter()
            .any(|process| {
                process
                    .exe
                    .and_then(|path| path.canonicalize().ok())
                    .is_some_and(|path| path.starts_with(&runtime))
            });
        if !owned {
            return Ok(());
        }
        if started.elapsed() > std::time::Duration::from_secs(5) {
            return Err(
                "candidate left a process executing from its private installed runtime".into(),
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

fn verify_inference_evidence(traces: &str, last: &str, audit: &str, stdout: &str) -> Result<()> {
    let traces = traces
        .lines()
        .map(serde_json::from_str::<serde_json::Value>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for prompt in INFERENCE_PROMPTS {
        let matching: Vec<_> = traces.iter().filter(|v| v["goal"] == prompt).collect();
        if matching.len() != 1
            || matching[0]["outcome"] != "SUCCESS"
            || !matching[0]["tools"]
                .as_array()
                .is_some_and(|v| v.is_empty())
        {
            return Err(
                format!("inference goal lacks one successful tool-free trace: {prompt}").into(),
            );
        }
    }
    let last: serde_json::Value = serde_json::from_str(last)?;
    let answer = last["final_answer"]
        .as_str()
        .unwrap_or_default()
        .to_lowercase();
    if last["goal"] != INFERENCE_PROMPTS[1]
        || last["status"] != "SUCCESS"
        || !answer.contains("red")
        || !answer.contains("kite")
        || answer.contains("recovery")
        || !stdout.lines().any(|line| {
            let normalized = line.to_lowercase();
            (normalized.contains("2+2") || normalized.contains("2 + 2"))
                && normalized
                    .split(|c: char| !c.is_alphanumeric())
                    .any(|word| word == "4")
        })
    {
        return Err("inference answers failed arithmetic/prose semantic acceptance".into());
    }
    let generations = audit
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|row| {
            row["fields"]["message"] == "In-process Rust Candle generation complete"
                && row["fields"]["backend"] == "candle"
                && row["fields"]["generated_tokens"]
                    .as_u64()
                    .is_some_and(|n| n > 0)
        })
        .count();
    if generations < 2 {
        return Err("inference did not record two actual nonempty Rust Candle generations".into());
    }
    Ok(())
}

fn inference_acceptance(binary: &Path, attempt: &Path) -> Result<String> {
    let artifact_before = file_digest(binary)?;
    let native_home = env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .ok_or("native home unavailable")?;
    let default_cache = env::var_os("SUSI_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| native_home.join(".susi"))
        .join("models");
    // Agent orchestration may supply an already admitted read-only cache;
    // ordinary qualification discovers the installed starter automatically.
    let cache = env::var_os("SUSI_QUALIFICATION_MODEL_CACHE")
        .map(PathBuf::from)
        .unwrap_or(default_cache);
    let cached = STARTER_FILES.iter().all(|name| cache.join(name).is_file());
    let scratch = attempt.join("inference");
    let home = scratch.join("home");
    let runtime = home.join(".susi");
    let workspace = scratch.join("workspace");
    for dir in [&workspace, &runtime.join("models"), &home.join("temp")] {
        fs::create_dir_all(dir)?;
    }
    let mut reservation =
        EphemeralPorts::new(&native_home, format!("qualification:{}", scratch.display()))?;
    let ports = reservation.offset;
    let mut models = Vec::new();
    if cached {
        for name in STARTER_FILES {
            let path = runtime.join("models").join(name);
            fs::copy(cache.join(name), &path)?;
            models.push(serde_json::json!({"file":name,"sha256":file_digest(&path)?,"bytes":fs::metadata(path)?.len()}));
        }
        // Copying a cache does not admit it: the actual candidate must validate
        // model/tokenizer licenses and digests before producing audited tokens.
        fs::write(runtime.join("privacy_mode"), "local_only")?;
    }
    let mut command = Command::new(binary);
    command.env_clear();
    for key in ["SystemRoot", "WINDIR", "ComSpec"] {
        if let Some(value) = env::var_os(key) {
            command.env(key, value);
        }
    }
    if let Some(root) = env::var_os("SystemRoot") {
        let root = PathBuf::from(root);
        command.env("PATH", env::join_paths([root.join("System32"), root])?);
    } else {
        command.env("PATH", "/usr/bin:/bin");
    }
    for key in ["HOME", "USERPROFILE"] {
        command.env(key, &home);
    }
    for (key, path) in [
        ("SUSI_HOME", runtime.clone()),
        ("APPDATA", home.join("appdata")),
        ("LOCALAPPDATA", home.join("localappdata")),
        ("XDG_CONFIG_HOME", home.join("config")),
        ("XDG_DATA_HOME", home.join("data")),
        ("XDG_CACHE_HOME", home.join("cache")),
        ("TEMP", home.join("temp")),
        ("TMP", home.join("temp")),
    ] {
        fs::create_dir_all(&path)?;
        command.env(key, path);
    }
    command
        .current_dir(&workspace)
        .env("SUSI_PORT_OFFSET", ports.to_string())
        .env("SUSI_NO_REGISTRY_ENV", "1")
        .env("RAYON_NUM_THREADS", "4")
        .env("CANDLE_NUM_THREADS", "4");
    if cached {
        command.env("SUSI_OFFLINE", "1").env("HF_HUB_OFFLINE", "1");
    }
    let mut cleanup = Command::new(binary);
    cleanup.env_clear().current_dir(&workspace).arg("stop");
    for (key, value) in command.get_envs() {
        if let Some(value) = value {
            cleanup.env(key, value);
        }
    }
    let mut provisioning = Vec::new();
    reservation.stopped = false;
    if !cached {
        // Network use is an explicit provisioning phase, performed by the
        // exact candidate's license/digest-enforcing native installer/runtime.
        // No credentials, custom endpoints or proxy settings are inherited.
        // Core never downloads a model by itself (CEO decision 2026-11-04),
        // so the starter download is its own explicit, user-visible command.
        let phases: [(&str, &[&str], std::time::Duration); 3] = [
            ("install", &["install"], HEALTH_BUDGET),
            ("start", &["start"], HEALTH_BUDGET),
            (
                "model-install",
                &["os", "manage", "model", "install", "starter"],
                std::time::Duration::from_secs(600),
            ),
        ];
        for (phase, args, budget) in phases {
            let mut setup = Command::new(binary);
            setup.env_clear().current_dir(&workspace).args(args);
            for (key, value) in command.get_envs() {
                if let Some(value) = value {
                    setup.env(key, value);
                }
            }
            match measured_health(setup, &scratch, phase, budget, b"") {
                Ok(report) => provisioning.push(report),
                Err(error) => {
                    if measured_health(cleanup, &scratch, "cleanup", HEALTH_BUDGET, b"").is_ok() {
                        reservation.confirm_stopped(&runtime)?;
                    }
                    return Err(error);
                }
            }
        }
    }
    // Core has no interactive prompt: each prompt is a one-shot goal piped to
    // the exact candidate under the same isolated runtime state, in order.
    // Both prompts share the one session budget; each run gets what is left.
    let budget = std::time::Duration::from_secs(if cached { 60 } else { 600 });
    let session = std::time::Instant::now();
    let resources = INFERENCE_PROMPTS
        .iter()
        .enumerate()
        .map(|(index, prompt)| {
            let mut goal = Command::new(binary);
            goal.env_clear().current_dir(&workspace);
            for (key, value) in command.get_envs() {
                if let Some(value) = value {
                    goal.env(key, value);
                }
            }
            measured_health(
                goal,
                &scratch,
                &format!("inference-{index}"),
                budget.saturating_sub(session.elapsed()),
                format!("{prompt}\n").as_bytes(),
            )
        })
        .collect::<Result<Vec<_>>>();
    // Even rejected candidates may have started their own services. Use only
    // their isolated runtime and reserved ports; never the installed profile.
    let stopped = measured_health(cleanup, &scratch, "cleanup", HEALTH_BUDGET, b"");
    let cleanup_resources = stopped?;
    reservation.confirm_stopped(&runtime)?;
    let resources = resources?
        .iter()
        .map(|report| serde_json::from_str::<serde_json::Value>(report))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let traces = bounded_evidence(&workspace.join(".susi/mission_traces.jsonl"))?;
    let last = bounded_evidence(&workspace.join(".susi/last_mission_trace.json"))?;
    let mut stdout = String::new();
    for index in 0..INFERENCE_PROMPTS.len() {
        stdout.push_str(&bounded_evidence(
            &scratch.join(format!("inference-{index}.stdout")),
        )?);
    }
    let mut audit = String::new();
    for entry in fs::read_dir(&runtime)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("audit") && name.ends_with(".log") {
            audit.push_str(&bounded_evidence(&entry.path())?);
            if audit.len() > 8 * 1024 * 1024 {
                return Err("aggregate audit budget exceeded".into());
            }
        }
    }
    verify_inference_evidence(&traces, &last, &audit, &stdout)?;
    if !cached {
        for name in STARTER_FILES {
            let path = runtime.join("models").join(name);
            models.push(serde_json::json!({"file":name,"sha256":file_digest(&path)?,"bytes":fs::metadata(path)?.len()}));
        }
    }
    for model in &models {
        let name = required(model, "file")?;
        if file_digest(&runtime.join("models").join(name))? != required(model, "sha256")? {
            return Err("starter model evidence changed during inference acceptance".into());
        }
    }
    if file_digest(binary)? != artifact_before {
        return Err("candidate changed during inference acceptance".into());
    }
    let evidence = serde_json::json!({
        "schema":"susi-inference-acceptance-v2", "artifact_sha256":artifact_before,
        "provisioning":if cached {"copied-cache-validated-by-candidate"} else {"uncached-native-install-and-public-model-download"},
        "provisioning_resources":provisioning,"models":models,
        "cleanup_resources":serde_json::from_str::<serde_json::Value>(&cleanup_resources)?,
        "private_installed_processes_remaining":0,
        "prompts":INFERENCE_PROMPTS,"resources":resources,
        "mission_traces_sha256":digest(traces.as_bytes()), "last_trace_sha256":digest(last.as_bytes()),
        "audit_sha256":digest(audit.as_bytes()), "stdout_sha256":digest(stdout.as_bytes()),
        // Keep the actual evidence inside the manifest-bound gates package,
        // not merely hashes pointing at mutable diagnostic files elsewhere.
        "mission_traces":traces,"last_trace":last,"audit":audit,"stdout":stdout,
        "passed":true,
    }).to_string();
    fs::write(scratch.join("acceptance.json"), &evidence)?;
    Ok(format!("inference-acceptance={evidence}\n"))
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
    complete_gates: Option<String>,
}
impl Manifest {
    fn encode(&self) -> String {
        let version = if self.complete_gates.is_some() {
            "susi-local-release-v2"
        } else {
            "susi-local-release-v1"
        };
        let mut raw = format!("{version}\nsource={}\ntree={}\nartifact-sha256={}\nartifact-bytes={}\nhost={}\nintegration-receipt={}\nlock-sha256={}\ncompiler-sha256={}\ngates-sha256={}\nsbom-sha256={}\nnotices-sha256={}\nnative-audit-sha256={}\n", self.source,self.tree,self.artifact,self.bytes,self.host,self.integration,self.lock,self.compiler,self.gates,self.sbom,self.notices,self.native_audit);
        if let Some(digest) = &self.complete_gates {
            raw.push_str(&format!("complete-gates-sha256={digest}\n"));
        }
        raw
    }
    fn decode(raw: &str) -> Result<Self> {
        let mut lines = raw.lines();
        let complete = match lines.next() {
            Some("susi-local-release-v1") => false,
            Some("susi-local-release-v2") => true,
            _ => return Err("invalid release manifest".into()),
        };
        let mut values = BTreeMap::new();
        for line in lines {
            let (key, value) = line.split_once('=').ok_or("invalid manifest field")?;
            if values.insert(key, value).is_some() {
                return Err("duplicate manifest field".into());
            }
        }
        if values.len() != if complete { 13 } else { 12 } {
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
            complete_gates: if complete {
                Some(get("complete-gates-sha256")?.into())
            } else {
                None
            },
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
            || result
                .complete_gates
                .as_ref()
                .is_some_and(|value| !hex(value, &[64]))
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

fn elf_boundary_proof(
    executable: &Path,
    map: &Path,
    host: &str,
    require_tls: bool,
    retain_map: bool,
) -> Result<String> {
    if fs::metadata(executable)?.len() > 256 * 1024 * 1024
        || fs::metadata(map)?.len()
            > if retain_map {
                256 * 1024 * 1024
            } else {
                512 * 1024 * 1024
            }
    {
        return Err("ELF artifact or link map exceeds inspection budget".into());
    }
    let imports = elf_imports::inspect(&fs::read(executable)?)?;
    elf_imports::validate_os_imports(&imports, host, require_tls)?;
    let map_text = fs::read_to_string(map)?;
    let inputs = elf_imports::validate_link_map(&map_text)?;
    let record = serde_json::json!({"binary":executable.file_name().and_then(|s| s.to_str()),"target":host,"artifact_sha256":file_digest(executable)?,"map_sha256":file_digest(map)?,"interpreter":imports.interpreter,"needed":imports.needed,"link_inputs":inputs,"link_map":if retain_map { Some(map_text) } else { None }});
    Ok(format!("ELF native boundary {record}\n"))
}

fn qualify(ctx: &Context) -> Result<String> {
    qualify_with(ctx, inference_acceptance)
}

fn qualify_with(
    ctx: &Context,
    prove_inference: fn(&Path, &Path) -> Result<String>,
) -> Result<String> {
    qualify_with_advisories(
        ctx,
        prove_inference,
        &advisory_evidence::Source::provisioned()?,
        &complete_release_gates::Runner::provisioned(&ctx.root, ctx.trust_registry.as_deref())?,
    )
}

fn qualify_with_advisories(
    ctx: &Context,
    prove_inference: fn(&Path, &Path) -> Result<String>,
    advisory_source: &advisory_evidence::Source,
    complete_runner: &complete_release_gates::Runner,
) -> Result<String> {
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
    let (sbom, mut notices, native_audit) = inventory(&candidate, &attempt, &host)?;
    let GateRun {
        mut proof,
        sysroot,
        native: native_gates,
        records: complete_records,
    } = run_gates(
        GateContext {
            candidate: &candidate,
            attempt: &attempt,
            host: &host,
            compiler: &compiler,
        },
        advisory_source,
        complete_runner,
    )?;
    if let Some(sysroot) = &sysroot {
        notices.push_str("\n===== Original Rust standard-library upstream notices (including omitted upstream components) =====\n");
        notices.push_str(
            sysroot.evidence["upstream_copyright"]
                .as_str()
                .ok_or("missing original stdlib notices")?,
        );
        notices.push_str("\nSelected target dependency graph and exact source delta are retained in manifest-bound gates.txt; upstream notices are preserved without relabeling.\n");
    }
    // Preserve the exact compiler report inside manifest-hashed gate evidence.
    proof.push_str(&format!(
        "compiler-report={}\n",
        serde_json::to_string(&compiler)?
    ));
    let binary = candidate
        .join("target")
        .join(&host)
        .join("release")
        .join(binary_name());
    let before = file_digest(&binary)?;
    const RELEASE_PROOF_BINARIES: [&str; 6] = [
        "susi",
        "xtask",
        "local-workflow",
        "local-release",
        "ci-release-only",
        "susi-dev",
    ];
    if host.ends_with("-pc-windows-msvc") && candidate.join("crates/susi-core/Cargo.toml").is_file()
    {
        if fs::metadata(&binary)?.len() > 256 * 1024 * 1024 {
            return Err("PE artifact exceeds inspection budget".into());
        }
        let profile = candidate.join("target").join(&host).join("release");
        for name in RELEASE_PROOF_BINARIES {
            let executable = profile.join(format!("{name}.exe"));
            let map = native_link::find_map(&profile, name)?;
            proof.push_str(&native_link_proof(&executable, &map)?);
        }
    }
    if native_openssl::supported(&host) && candidate.join("crates/susi-core/Cargo.toml").is_file() {
        let profile = candidate.join("target").join(&host).join("release");
        for name in RELEASE_PROOF_BINARIES {
            let executable = profile.join(name);
            let map = profile.join(format!("{name}.map"));
            proof.push_str(&elf_boundary_proof(
                &executable,
                &map,
                &host,
                name == "susi",
                true,
            )?);
        }
    }
    proof.push_str(&smoke(&binary, &attempt)?);
    proof.push_str(&prove_inference(&binary, &attempt)?);
    if file_digest(&binary)? != before {
        return Err("artifact changed during smoke check".into());
    }
    clean(&candidate)?;
    if git(&candidate, &["rev-parse", "HEAD"])? != head {
        return Err("source advanced during release qualification".into());
    }
    integration_receipt(ctx, &head, &tree)?;
    let mut manifest = Manifest {
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
        complete_gates: None,
    };
    let complete =
        complete_release_gates::finish(native_gates, complete_records, &attempt, &manifest)?;
    let complete_bytes = serde_json::to_vec_pretty(&complete)?;
    manifest.complete_gates = Some(digest(&complete_bytes));
    fs::write(attempt.join("complete-gates.json"), &complete_bytes)?;
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
    fs::write(staging.join("complete-gates.json"), &complete_bytes)?;
    complete_release_gates::copy_evidence(&complete, &attempt, &staging)?;
    complete_release_gates::validate_package(&staging, &manifest, ctx.trust_registry.as_deref())?;
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
        "complete-gates.json",
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
    if let Err(error) = git(
        &ctx.root,
        &[
            "-c",
            "core.longpaths=true",
            "worktree",
            "remove",
            text(&candidate)?,
        ],
    ) {
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
    if !fs::read_to_string(package.join("gates.txt"))?.starts_with(QUALIFICATION_POLICY) {
        return Err(
            "release requires requalification under the current native-runtime evidence policy"
                .into(),
        );
    }
    complete_release_gates::validate_package(&package, &manifest, ctx.trust_registry.as_deref())?;
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

fn activate(ctx: &Context, target: &str, recovering: bool) -> Result<()> {
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
    // Never advertise a damaged current package as the next rollback target.
    let old_valid = old
        .as_ref()
        .map(|value| checked_package(ctx, value))
        .transpose();
    if !recovering {
        old_valid.as_ref().map_err(|error| error.to_string())?;
    }
    let zero = "0".repeat(target.len());
    let mut transaction = format!(
        "start\nupdate {CURRENT} {target} {}\n",
        old.as_deref().unwrap_or(&zero)
    );
    if recovering && old_valid.is_err() {
        if let Some(previous) = previous {
            transaction.push_str(&format!("delete {PREVIOUS} {previous}\n"));
        }
    } else if let Some(old) = old {
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
    activate(ctx, &blob, false)
}

fn rollback(ctx: &Context) -> Result<()> {
    let _lock = Lock::acquire(&ctx.state)?;
    let blob = resolve(&ctx.root, PREVIOUS)?.ok_or("no previous verified release")?;
    activate(ctx, &blob, true)
}

// Shares the application's global allocation protocol so repositories and dev
// executables cannot accidentally claim the same port range. Ownership persists
// across promotion/rollback, with a native OS lock released on process exit.
struct EphemeralPorts {
    home: PathBuf,
    identity: String,
    offset: u16,
    stopped: bool,
}
impl EphemeralPorts {
    fn new(home: &Path, identity: String) -> Result<Self> {
        let offset = release_offset(home, &identity)?;
        Ok(Self {
            home: home.to_owned(),
            identity,
            offset,
            stopped: true,
        })
    }
    fn confirm_stopped(&mut self, runtime: &Path) -> Result<()> {
        verify_runtime_stopped(runtime)?;
        self.stopped = true;
        self.release()
    }
    fn release(&self) -> Result<()> {
        if !self.stopped {
            return Err("cannot release ports before scoped processes stop".into());
        }
        let base = self.home.join(".susi-dev");
        let lock_path = base.join("allocation.lock");
        if fs::symlink_metadata(&lock_path)?.file_type().is_symlink() {
            return Err("unsafe port allocation lock".into());
        }
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(lock_path)?;
        lock.lock()?;
        let path = base.join("port-owners").join(self.offset.to_string());
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err("unsafe port owner".into())
            }
            Ok(_) => {}
        }
        if fs::read_to_string(&path)? != self.identity {
            return Err("port reservation ownership changed; preserving foreign owner".into());
        }
        fs::remove_file(path)?;
        Ok(())
    }
}
impl Drop for EphemeralPorts {
    fn drop(&mut self) {
        if self.stopped {
            if let Err(error) = self.release() {
                eprintln!("qualification port cleanup: {error}");
            }
        }
    }
}

fn release_offset(home: &Path, identity: &str) -> Result<u16> {
    let base = home.join(".susi-dev");
    real_directory(&base)?;
    let lock_path = base.join("allocation.lock");
    if let Ok(metadata) = fs::symlink_metadata(&lock_path) {
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err("unsafe port allocation lock".into());
        }
    }
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)?;
    lock.lock()?;
    let slots = base.join("port-owners");
    real_directory(&slots)?;
    let mut free = None;
    for slot in 0..400_u16 {
        let offset = 1000 + slot * 100;
        let owner = slots.join(offset.to_string());
        match fs::read_to_string(&owner) {
            Ok(value) if value == identity => return Ok(offset),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if free.is_none() {
                    free = Some(offset);
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
    let offset = free.ok_or("no private local-release port range available")?;
    let mut owner = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(slots.join(offset.to_string()))?;
    owner.write_all(identity.as_bytes())?;
    owner.sync_all()?;
    Ok(offset)
}

fn production_command_at(
    ctx: &Context,
    binary: &Path,
    args: &[String],
    home: &Path,
) -> Result<Command> {
    let runtime = ctx.packages.join("runtime");
    real_directory(&runtime)?;
    let identity = format!("local-release:{}", text(&ctx.packages.canonicalize()?)?);
    let offset = release_offset(home, &identity)?;
    let mut command = Command::new(binary);
    // Parent development endpoints must not redirect this release into another
    // instance. Provider credentials outside Susi's namespace remain available.
    for (key, _) in env::vars_os() {
        if key
            .to_string_lossy()
            .to_ascii_uppercase()
            .starts_with("SUSI_")
        {
            command.env_remove(key);
        }
    }
    susi_paths::test_env::inherit_registry_isolation(&mut command);
    command
        .args(args)
        .env("SUSI_HOME", runtime)
        .env("SUSI_PORT_OFFSET", offset.to_string());
    Ok(command)
}

fn production_command(ctx: &Context, binary: &Path, args: &[String]) -> Result<Command> {
    let home = env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .ok_or("native user home is unavailable")?;
    production_command_at(ctx, binary, args, Path::new(&home))
}

fn run(args: &[String]) -> Result<()> {
    let ctx = Context::open(&env::current_dir()?)?;
    match args.first().map(String::as_str) {
        Some("check-native-link") if args.len()==3 => {
            let binary = Path::new(&args[1]);
            if fs::metadata(binary)?.len() > 256 * 1024 * 1024 { return Err("PE artifact exceeds inspection budget".into()); }
            let bytes = fs::read(binary)?;
            let imports = pe_imports::inspect(&bytes)?;
            let (entry, map_sha256) = native_link::validate_reader(native_link::entry_address(&bytes)?, std::io::BufReader::new(fs::File::open(&args[2])?), 256 * 1024 * 1024)?;
            println!("{}", serde_json::json!({"diagnostic_only":true,"artifact_sha256":digest(&bytes),"entry":entry,"imports":imports,"map_sha256":map_sha256}));
        },
        Some("prepare-sysroot") if args.len() == 1 => {
            println!("{}", trusted_sysroot::prepare(&ctx.root)?.path().display());
        },
        Some("check-sysroot-sources") if args.len() == 3 => {
            let installed = Command::new("rustc").args(["--print", "sysroot"]).output()?;
            if !installed.status.success() { return Err("cannot locate pinned compiler sources".into()); }
            let installed = String::from_utf8(installed.stdout)?;
            sysroot_sources::replacement(Path::new(&args[1]), &Path::new(installed.trim()).join("lib/rustlib/src/rust/library"), Path::new(&args[2]))?;
            println!("Authenticated source and reviewed replacement delta verified; artifacts remain unqualified.");
        },
        Some("record-gmcp") => gmcp_evidence::run(&ctx.root, &args[1..])?,
        Some("check-elf") if args.len()==3 => {
            let (host, _) = compiler(&ctx.root)?;
            let proof = elf_boundary_proof(Path::new(&args[1]), Path::new(&args[2]), &host, true, false)?;
            let evidence = ctx.state.join(format!("elf-check-{}.txt",stamp()?));
            fs::write(&evidence, proof)?;
            println!("Native ELF boundary passed; evidence {}. Diagnostic only: no release qualified or promoted.", evidence.display());
        },
        Some("check-pe") if args.len()==2 => {
            let binary = Path::new(&args[1]);
            if fs::metadata(binary)?.len() > 256 * 1024 * 1024 { return Err("PE artifact exceeds inspection budget".into()); }
            let imports = pe_imports::inspect(&fs::read(binary)?)?;
            println!("{}",serde_json::to_string(&imports)?);
        },
        Some("qualify") if args.len()==1 => { qualify(&ctx)?; }
        Some("coordinate") => release_coordinator::run(&ctx,&args[1..])?,
        Some("prepare-next") if args.len()==2 && args[1]=="patch" => prepare_release::run(&ctx)?,
        Some("install-current") if args.len()==1 => installed_delivery::install_current(&ctx)?,
        Some("export-qualified") if args.len()==3 => installed_delivery::export(&ctx,&args[1],Path::new(&args[2]))?,
        Some("check-inventory") if args.len()==1 => {
            let attempt = ctx.state.join(format!("inventory-check-{}",stamp()?));
            fs::create_dir(&attempt)?;
            let (host, _) = compiler(&ctx.root)?;
            let (sbom, notices, native) = inventory(&ctx.root,&attempt,&host)?;
            fs::write(attempt.join("sbom.json"),sbom)?;
            fs::write(attempt.join("THIRD-PARTY-NOTICES.txt"),notices)?;
            fs::write(attempt.join("native-source-audit.txt"),native)?;
            println!("Selected artifact inventory passed; evidence {}. Diagnostic only: no release qualified or promoted.",attempt.display());
        },
        Some("check-inference") if args.len()==2 => {
            let binary = PathBuf::from(&args[1]).canonicalize()?;
            let attempt = ctx.state.join(format!("inference-check-{}",stamp()?));
            fs::create_dir(&attempt)?;
            let proof = inference_acceptance(&binary,&attempt)?;
            fs::write(attempt.join("inference-proof.txt"),proof)?;
            println!("Candidate inference check passed; evidence {}. Diagnostic only: no release qualified or promoted.",attempt.display());
        },
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
            let status=production_command(&ctx,&binary,&args[1..])?.status()?;
            if !status.success(){return Err(format!("release process exited {status}").into());}
        }
        _ => return Err("usage: local-release coordinate [--head <rev>] [--tools-source <rev>] [--verifier <path>] [--home <dir>] | qualify | prepare-next patch | check-inventory | check-elf <candidate> <link-map> | check-inference <candidate> | promote <release-id> | rollback | status | export-qualified <release-id> <destination> | install-current | run [susi arguments]".into()),
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

    // Machinery fixtures deliberately do not masquerade as a working AI OS.
    // The production entry point always calls the real inference acceptance.
    fn qualify(ctx: &Context) -> Result<String> {
        qualify_with(ctx, |_, _| Ok("fixture-only inference boundary\n".into()))
    }

    fn qualify_with(
        ctx: &Context,
        prove_inference: fn(&Path, &Path) -> Result<String>,
    ) -> Result<String> {
        let source = advisory_evidence::fixture_source(
            &ctx.state.join(format!("fixture-advisories-{}", stamp()?)),
        );
        qualify_with_advisories(
            ctx,
            prove_inference,
            &source,
            &complete_release_gates::Runner::fixture(
                ctx.trust_registry
                    .as_deref()
                    .ok_or("missing fixture registry")?,
            ),
        )
    }

    #[test]
    fn inference_acceptance_requires_both_real_generations_and_successful_goal_evidence() {
        let traces = INFERENCE_PROMPTS
            .iter()
            .map(|goal| serde_json::json!({"goal":goal,"outcome":"SUCCESS","tools":[]}).to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let last = serde_json::json!({"goal":INFERENCE_PROMPTS[1],"status":"SUCCESS","final_answer":"A red kite flies."}).to_string();
        let event = serde_json::json!({"fields":{"message":"In-process Rust Candle generation complete","backend":"candle","generated_tokens":3}}).to_string();
        let audit = format!("{event}\n{event}");
        let stdout = "2+2 equals 4.\nA red kite flies.";
        assert!(verify_inference_evidence(&traces, &last, &audit, stdout).is_ok());
        assert!(verify_inference_evidence(&traces, &last, &event, stdout).is_err());
        assert!(verify_inference_evidence(
            &traces.replace("SUCCESS", "FAILED"),
            &last,
            &audit,
            stdout
        )
        .is_err());
        assert!(verify_inference_evidence(
            &traces.replace("\"tools\":[]", "\"tools\":[\"exec\"]"),
            &last,
            &audit,
            stdout
        )
        .is_err());
        assert!(verify_inference_evidence(
            &traces,
            &last,
            &audit.replace("candle", "cloud"),
            stdout
        )
        .is_err());
        assert!(verify_inference_evidence(
            &traces,
            &last.replace("A red kite flies.", "Recovery pending"),
            &audit,
            stdout
        )
        .is_err());
        assert!(verify_inference_evidence(&traces, &last, &audit, "2+2 equals 5.").is_err());
    }

    #[test]
    fn failed_inference_cannot_publish_a_qualified_release() {
        let fixture = Fixture::new();
        let ctx = fixture.context();
        let result = qualify_with(&ctx, |_, _| Err("inference acceptance rejected".into()));
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("inference acceptance rejected"));
        assert!(git(
            &fixture.root(),
            &[
                "for-each-ref",
                "--format=%(refname)",
                "refs/susi/releases/qualified/"
            ]
        )
        .unwrap()
        .is_empty());
        assert!(resolve(&fixture.root(), CURRENT).unwrap().is_none());
    }

    #[test]
    #[ignore = "subprocess fixture; invoked only by the health budget regression"]
    fn health_budget_child_fixture() {
        match env::var("SUSI_HEALTH_TEST_MODE").as_deref() {
            Ok("hang") => std::thread::sleep(std::time::Duration::from_secs(60)),
            Ok("output") => {
                let block = vec![b'x'; HEALTH_OUTPUT_BUDGET_BYTES as usize + 1024];
                std::io::stdout().write_all(&block).unwrap();
            }
            Ok("pass") => println!("healthy fixture"),
            _ => panic!("fixture must be explicitly configured by its parent test"),
        }
    }

    #[test]
    fn health_budgets_measure_success_and_reap_timeout_and_output_failures() {
        let scratch = env::temp_dir().join(format!("susi-health-budget-{}", stamp().unwrap()));
        fs::create_dir(&scratch).unwrap();
        for mode in ["pass", "hang", "output"] {
            let mut command = Command::new(env::current_exe().unwrap());
            command
                .args([
                    "--ignored",
                    "--exact",
                    "tests::health_budget_child_fixture",
                    "--nocapture",
                ])
                .env("SUSI_HEALTH_TEST_MODE", mode);
            let timeout = if mode == "hang" {
                std::time::Duration::from_millis(200)
            } else {
                std::time::Duration::from_secs(10)
            };
            let started = std::time::Instant::now();
            let result = measured_health(command, &scratch, mode, timeout, b"");
            assert_eq!(result.is_ok(), mode == "pass", "{mode}: {result:?}");
            assert!(started.elapsed() < std::time::Duration::from_secs(15));
            let evidence: serde_json::Value = serde_json::from_str(
                &fs::read_to_string(scratch.join(format!("{mode}.resources.json"))).unwrap(),
            )
            .unwrap();
            assert_eq!(evidence["passed"], mode == "pass");
            assert!(evidence["elapsed_ms"].as_u64().is_some());
            if mode == "output" {
                assert!(evidence["output_bytes"].as_u64().unwrap() > HEALTH_OUTPUT_BUDGET_BYTES);
            }
        }
    }

    #[cfg(windows)]
    #[test]
    fn workspace_test_proof_changes_with_execution_inputs() {
        let fixture = Fixture::new();
        let root = fixture.root();
        let home = fixture.0.join("home");
        let target = root.join("target");
        let mut command = isolated_command(Path::new("cargo"), &home).unwrap();
        assert!(command
            .get_envs()
            .any(|(key, value)| key == "SUSI_NO_REGISTRY_ENV"
                && value == Some(std::ffi::OsStr::new("1"))));
        let args: Vec<String> = [
            "test",
            "--workspace",
            "--locked",
            "--offline",
            "--target",
            "test-host",
            "--target-dir",
            target.to_str().unwrap(),
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        let archives:Vec<_>=(0..20).map(|i|serde_json::json!({"filename":format!("archive-{i}.rlib"),"sha256":format!("{i:064x}")})).collect();
        let sysroot = serde_json::json!({"schema":"susi/executed-sysroot/v2","identity":"fixture","compiler":"rustc","authenticated_source_manifest_sha256":"fixture","destination":home,"artifacts":archives});
        let other_archives = {
            let mut changed = sysroot.clone();
            changed["artifacts"][0]["sha256"] = serde_json::json!("different");
            changed
        };
        let key = |args: &[String],
                   compiler: &str,
                   host: &str,
                   sysroot: &serde_json::Value,
                   command: &Command| {
            workspace_test_proof::fingerprint(
                &root,
                args,
                workspace_test_proof::Toolchain {
                    compiler,
                    host,
                    sysroot,
                },
                command,
                &home,
            )
            .unwrap()
        };
        let original = key(&args, "rustc", "test-host", &sysroot, &command);
        assert_eq!(
            original,
            key(&args, "rustc", "test-host", &sysroot, &command)
        );
        assert_ne!(
            original,
            key(
                &args,
                "another rustc",
                "test-host",
                &{
                    let mut v = sysroot.clone();
                    v["compiler"] = serde_json::json!("another rustc");
                    v
                },
                &command
            )
        );
        assert_ne!(
            original,
            key(&args, "rustc", "other-host", &sysroot, &command)
        );
        assert_ne!(
            original,
            key(&args, "rustc", "test-host", &other_archives, &command)
        );
        let mut relocated = sysroot.clone();
        relocated["destination"] = serde_json::json!(fixture.0.join("other-verified-sysroot"));
        relocated["cache_hit"] = serde_json::json!(true);
        relocated["elapsed_ms"] = serde_json::json!(700);
        assert_eq!(
            original,
            key(&args, "rustc", "test-host", &relocated, &command)
        );
        assert!(workspace_test_proof::fingerprint(
            &root,
            &args,
            workspace_test_proof::Toolchain {
                compiler: "rustc",
                host: "test-host",
                sysroot: &serde_json::json!({})
            },
            &command,
            &home
        )
        .is_err());
        for extra in [
            ["--features", "extra"],
            ["--", "--ignored"],
            ["-p", "xtask"],
        ] {
            let mut changed = args.clone();
            changed.extend(extra.into_iter().map(str::to_owned));
            assert_ne!(
                original,
                key(&changed, "rustc", "test-host", &sysroot, &command)
            );
        }
        let lock = root.join("Cargo.lock");
        let previous = fs::read(&lock).unwrap();
        fs::write(&lock, b"different lockfile").unwrap();
        assert_ne!(
            original,
            key(&args, "rustc", "test-host", &sysroot, &command)
        );
        fs::write(lock, previous).unwrap();
        command.env("CARGO_PROFILE_TEST_OPT_LEVEL", "3");
        assert_ne!(
            original,
            key(&args, "rustc", "test-host", &sysroot, &command)
        );
        let command = isolated_command(Path::new("cargo"), &home).unwrap();
        fixture.revision("two", true);
        assert_ne!(
            original,
            key(&args, "rustc", "test-host", &sysroot, &command)
        );
    }

    #[test]
    fn workspace_test_proof_refuses_unsigned_and_malformed_attestations() {
        let fixture = Fixture::new();
        let pass = format!(
            "cargo test --workspace --locked => exit code: 0; elapsed-ms=3\n{}",
            workspace_test_proof::record("expected")
        );
        for suffix in [
            "",
            "\nworkspace-test-attestation-v1=not-json",
            "\nworkspace-test-attestation-v1=\"not a commit\"",
        ] {
            assert!(!authenticated_workspace_pass(
                &fixture.root(),
                &format!("{pass}{suffix}"),
                "expected"
            ));
        }
    }

    #[test]
    #[ignore = "exercises an explicitly provisioned disposable GPG keyring"]
    fn workspace_test_proof_disposable_signer() {
        let fixture = Fixture::new();
        let root = fixture.root();
        let signer = env::var("SUSI_TEST_DISPOSABLE_FINGERPRINT").unwrap();
        git(
            &root,
            &[
                "config",
                "gpg.program",
                &env::var("SUSI_TEST_DISPOSABLE_GPG").unwrap(),
            ],
        )
        .unwrap();
        git(&root, &["config", "user.signingkey", &signer]).unwrap();
        let key = "expected";
        let record = workspace_test_proof::record(key);
        let input = workspace_test_proof::signed_input(&root, key).unwrap();
        let raw = String::from_utf8(
            workspace_test_proof::gpg(
                &root,
                &["--local-user", &signer, "--armor", "--clearsign"],
                input.as_bytes(),
            )
            .unwrap()
            .stdout,
        )
        .unwrap();
        let pass=format!("cargo test --workspace --locked => exit code: 0; elapsed-ms=3\n{record}\nworkspace-test-attestation-v1={}",serde_json::to_string(&raw).unwrap());
        assert!(authenticated_workspace_pass(&root, &pass, key));
        assert!(!authenticated_workspace_pass(
            &root,
            &pass.replace(key, "forged"),
            "forged"
        ));
        assert!(!authenticated_workspace_pass(
            &root,
            &pass.replace("exit code: 0", "exit code: 1"),
            key
        ));
        git(&root, &["config", "user.signingkey", "0000000000000000"]).unwrap();
        assert!(!authenticated_workspace_pass(&root, &pass, key));
        git(&root, &["config", "user.signingkey", &signer]).unwrap();
        fixture.revision("two", true);
        assert!(!authenticated_workspace_pass(&root, &pass, key));
    }

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
        fn context(&self) -> Context {
            let mut ctx = Context::open(&self.root()).unwrap();
            let registry = self.0.join("coordinator");
            fs::create_dir_all(registry.join("permits")).unwrap();
            ctx.trust_registry = Some(registry);
            ctx
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
    fn formatting_failure_precedes_any_private_toolchain_build() {
        let fixture = Fixture::new();
        let root = fixture.root();
        fs::create_dir_all(root.join("crates/susi-core")).unwrap();
        fs::write(root.join("crates/susi-core/Cargo.toml"), "[package]\n").unwrap();
        for adapter in [
            "idna-adapter",
            "unicode-ident",
            "htmlescape",
            "regex-syntax",
            "regex-syntax/os-unicode",
        ] {
            let path = root.join("vendor").join(adapter);
            fs::create_dir_all(&path).unwrap();
            fs::write(path.join("Cargo.toml"), "[package]\n").unwrap();
        }
        fs::write(
            root.join("src/main.rs"),
            "fn main(){println!(\"format me\");}",
        )
        .unwrap();
        let attempt = fixture.0.join("format-first");
        fs::create_dir(&attempt).unwrap();
        let (host, compiler) = compiler(&root).unwrap();
        let source = advisory_evidence::fixture_source(&fixture.0.join("fixture-db"));
        let error = match run_gates(
            GateContext {
                candidate: &root,
                attempt: &attempt,
                host: &host,
                compiler: &compiler,
            },
            &source,
            &complete_release_gates::Runner::fixture(&fixture.0.join("coordinator")),
        ) {
            Ok(_) => panic!("malformed formatting must fail"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("cargo fmt --all --check"), "{error}");
        assert!(attempt.join("gate-0.log").is_file());
        assert!(!attempt.join("gate-1.log").exists());
        assert!(!root.join(".git/susi-product-sysroots").exists());
    }

    #[test]
    fn advisory_rejection_precedes_compilation_and_retains_scanner_receipts() {
        let fixture = Fixture::new();
        let root = fixture.root();
        let manifest = root.join("Cargo.toml");
        let mut cargo = fs::read_to_string(&manifest).unwrap();
        cargo.push_str("\n[dependencies]\nautocfg = \"=1.5.1\"\n");
        fs::write(&manifest, cargo).unwrap();
        let lock = isolated_command(Path::new("cargo"), &fixture.0.join("lock-home"))
            .unwrap()
            .current_dir(&root)
            .args(["generate-lockfile", "--offline"])
            .output()
            .unwrap();
        assert!(
            lock.status.success(),
            "{}",
            String::from_utf8_lossy(&lock.stderr)
        );
        // Deliberately fabricated test-only advisory: no statement about autocfg's
        // actual security status. It proves scanner rejection blocks compilation.
        let source =
            advisory_evidence::fixture_source_for(&fixture.0.join("fixture-db"), "autocfg");
        let attempt = fixture.0.join("advisory-rejection");
        fs::create_dir(&attempt).unwrap();
        let (host, compiler) = compiler(&root).unwrap();
        let error = match run_gates(
            GateContext {
                candidate: &root,
                attempt: &attempt,
                host: &host,
                compiler: &compiler,
            },
            &source,
            &complete_release_gates::Runner::fixture(&fixture.0.join("coordinator")),
        ) {
            Ok(_) => panic!("fixture vulnerability must block qualification"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("check advisories"), "{error}");
        let log = fs::read_to_string(attempt.join("gate-2.log")).unwrap();
        assert!(log.contains("RUSTSEC-2026-0001"), "{log}");
        assert!(!attempt.join("gate-3.log").exists());
        assert!(!root.join("target").exists());
        let receipt: serde_json::Value =
            serde_json::from_slice(&fs::read(attempt.join("advisory-receipt.json")).unwrap())
                .unwrap();
        assert!(receipt["cargo_deny_sha256"]
            .as_str()
            .is_some_and(|value| hex(value, &[64])));
        let gates = fs::read_to_string(attempt.join("gates.txt")).unwrap();
        assert!(gates.contains("advisory-evidence="));
        assert!(gates.contains("exit code: 1"));
    }
    #[test]
    fn workspace_test_count_rejects_empty_cargo_suite_before_production_build() {
        let fixture = Fixture::new();
        let root = fixture.root();
        fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        let attempt = fixture.0.join("empty-suite");
        fs::create_dir(&attempt).unwrap();
        let (host, compiler) = compiler(&root).unwrap();
        let source = advisory_evidence::fixture_source(&fixture.0.join("fixture-db"));
        let error = match run_gates(
            GateContext {
                candidate: &root,
                attempt: &attempt,
                host: &host,
                compiler: &compiler,
            },
            &source,
            &complete_release_gates::Runner::fixture(&fixture.0.join("coordinator")),
        ) {
            Ok(_) => panic!("empty Cargo suite must fail release qualification"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("executed no passing Rust tests"), "{error}");
        assert!(!attempt.join("gate-6.log").exists());
    }
    #[test]
    fn selected_product_feature_proof_rejects_missing_or_ambiguous_features() {
        assert!(selected_features("other v1.0|pure", "blake3", "1.8").is_err());
        assert!(selected_features("blake3 v1.8|pure\nblake3 v1.8|std", "blake3", "1.8").is_err());
        assert_eq!(
            selected_features(
                "blake3 v1.8|pure,std\nblake3 v1.8|pure,std (*)",
                "blake3",
                "1.8"
            )
            .unwrap(),
            vec!["pure", "std"]
        );
        assert!(!selected_features("blake3 v1.8|std", "blake3", "1.8")
            .unwrap()
            .contains(&"pure".into()));
    }

    #[test]
    fn product_inventory_excludes_dev_activated_native_features_but_not_product_ones() {
        let fixture = Fixture::new();
        let root = fixture.root();
        for (name, extra) in [
            ("ring", String::new()),
            ("shared", "[features]\nnative = [\"dep:ring\"]\n[dependencies]\nring = {path = \"../ring\", optional = true}\n".into()),
            ("dev-helper", "[dependencies]\nshared = {path = \"../shared\", features = [\"native\"]}\n".into()),
        ] {
            let dir = root.join(name);
            fs::create_dir(&dir).unwrap();
            fs::write(dir.join("Cargo.toml"),format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\nlicense = \"MIT\"\n[lib]\npath = \"lib.rs\"\n{extra}")).unwrap();
            fs::write(dir.join("lib.rs"), "// source-only feature-resolution fixture\n").unwrap();
            fs::copy(root.join("LICENSE"),dir.join("LICENSE")).unwrap();
        }
        let manifest = root.join("Cargo.toml");
        let original = fs::read_to_string(&manifest).unwrap();
        fs::write(&manifest,format!("{original}\n[dependencies]\nshared = {{path = \"shared\"}}\n[dev-dependencies]\ndev-helper = {{path = \"dev-helper\"}}\n")).unwrap();
        assert!(Command::new("cargo")
            .current_dir(&root)
            .args(["generate-lockfile", "--offline"])
            .status()
            .unwrap()
            .success());
        let attempt = fixture.0.join("selected-inventory");
        fs::create_dir(&attempt).unwrap();
        let (host, _) = compiler(&root).unwrap();
        let (sbom, _, _) = inventory(&root, &attempt, &host).unwrap();
        let sbom: serde_json::Value = serde_json::from_str(&sbom).unwrap();
        assert_eq!(sbom["packages"].as_array().unwrap().len(), 2);
        fs::write(&manifest,format!("{original}\n[dependencies]\nshared = {{path = \"shared\", features = [\"native\"]}}\n[dev-dependencies]\ndev-helper = {{path = \"dev-helper\"}}\n")).unwrap();
        assert!(inventory(&root, &attempt, &host)
            .unwrap_err()
            .to_string()
            .contains("native dependency ring"));
    }

    #[test]
    fn ephemeral_ports_require_stop_and_preserve_foreign_owners() {
        let fixture = Fixture::new();
        let mut reservation = EphemeralPorts::new(&fixture.0, "qualification:test".into()).unwrap();
        let offset = reservation.offset;
        reservation.stopped = false;
        assert!(reservation.release().is_err());
        assert_eq!(release_offset(&fixture.0, "other").unwrap(), offset + 100);
        reservation.stopped = true;
        reservation.release().unwrap();
        assert_eq!(release_offset(&fixture.0, "replacement").unwrap(), offset);
        assert!(reservation.release().is_err());
        drop(reservation);
        assert_eq!(release_offset(&fixture.0, "replacement").unwrap(), offset);
        for index in 0..410 {
            let transient =
                EphemeralPorts::new(&fixture.0, format!("qualification:{index}")).unwrap();
            assert_eq!(transient.offset, offset + 200);
        }
    }

    #[test]
    fn releases_share_global_port_ownership_without_using_installed_ports() {
        let fixture = Fixture::new();
        let home = &fixture.0;
        let first = release_offset(home, "release:first").unwrap();
        let second = release_offset(home, "release:second").unwrap();
        assert_eq!(first, 1000);
        assert_eq!(second, 1100);
        assert_eq!(release_offset(home, "release:first").unwrap(), first);
        assert_eq!(
            release_offset(home, "development-executable").unwrap(),
            1200
        );
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
            complete_gates: None,
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
        let signal = serde_json::json!({"name":"signal-hook","version":"0.4.5","targets":[{"kind":["custom-build"],"src_path":build}]});
        assert!(native_source_check(&signal, &["iterator".into()]).is_ok());
        assert!(native_source_check(&signal, &["extended-siginfo-raw".into()]).is_err());
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
        let ctx = fixture.context();
        let first = qualify(&ctx).unwrap();
        // Model a correctly hashed historical receipt whose gates predate the
        // mandatory native runtime policy. Integrity alone must not admit it.
        let current_package = ctx.packages.join("objects").join(&first);
        let mut historical =
            Manifest::decode(&fs::read_to_string(current_package.join("manifest.txt")).unwrap())
                .unwrap();
        let gates = fs::read_to_string(current_package.join("gates.txt")).unwrap();
        let legacy_gates = gates.strip_prefix(QUALIFICATION_POLICY).unwrap();
        historical.gates = digest(legacy_gates.as_bytes());
        let raw = historical.encode();
        let historical_id = digest(raw.as_bytes());
        let historical_package = ctx.packages.join("objects").join(&historical_id);
        fs::create_dir(&historical_package).unwrap();
        for name in [
            binary_name(),
            "sbom.json",
            "THIRD-PARTY-NOTICES.txt",
            "native-source-audit.txt",
        ] {
            fs::write(
                historical_package.join(name),
                fs::read(current_package.join(name)).unwrap(),
            )
            .unwrap();
        }
        fs::write(historical_package.join("gates.txt"), legacy_gates).unwrap();
        fs::write(historical_package.join("manifest.txt"), &raw).unwrap();
        let blob = git_input(&ctx.root, &["hash-object", "-w", "--stdin"], raw.as_bytes()).unwrap();
        git(
            &ctx.root,
            &[
                "update-ref",
                &format!("refs/susi/releases/qualified/{historical_id}"),
                &blob,
            ],
        )
        .unwrap();
        assert!(promote(&ctx, &historical_id)
            .unwrap_err()
            .to_string()
            .contains("requalification"));
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
        assert_eq!(
            resolve(&ctx.root, PREVIOUS).unwrap(),
            Some(second_blob.clone())
        );
        fixture.revision("broken", false);
        assert!(qualify(&ctx).is_err());
        assert_eq!(
            resolve(&ctx.root, CURRENT).unwrap(),
            Some(first_blob.clone())
        );
        assert!(promote(&ctx, &"a".repeat(64)).is_err());
        assert!(promote(&ctx, "../escape").is_err());
        // An interrupted prepared transaction cannot publish half the pair.
        let interrupted=format!("start\nupdate {CURRENT} {second_blob} {first_blob}\nupdate {PREVIOUS} {first_blob} {second_blob}\nprepare\n");
        let _ = git_input(
            &ctx.root,
            &["update-ref", "--stdin"],
            interrupted.as_bytes(),
        );
        assert_eq!(
            resolve(&ctx.root, CURRENT).unwrap(),
            Some(first_blob.clone())
        );
        assert_eq!(
            resolve(&ctx.root, PREVIOUS).unwrap(),
            Some(second_blob.clone())
        );
        let corrupt = git_input(
            &ctx.root,
            &["hash-object", "-w", "--stdin"],
            b"broken manifest",
        )
        .unwrap();
        git(&ctx.root, &["update-ref", CURRENT, &corrupt]).unwrap();
        git(&ctx.root, &["update-ref", PREVIOUS, &first_blob]).unwrap();
        assert!(checked_package(&ctx, &corrupt).is_err());
        rollback(&ctx).unwrap();
        assert_eq!(
            resolve(&ctx.root, CURRENT).unwrap(),
            Some(first_blob.clone())
        );
        assert_eq!(resolve(&ctx.root, PREVIOUS).unwrap(), None);
        assert!(rollback(&ctx).is_err());
        git(&ctx.root, &["update-ref", PREVIOUS, &second_blob]).unwrap();
        let (_, binary) = checked_package(&ctx, &first_blob).unwrap();
        let command =
            production_command_at(&ctx, &binary, &["--version".into()], &ctx.root).unwrap();
        assert!(command.get_envs().any(|(key, value)| key == "SUSI_HOME"
            && value == Some(ctx.packages.join("runtime").as_os_str())));
        assert!(command
            .get_envs()
            .any(|(key, value)| key == "SUSI_PORT_OFFSET"
                && value == Some(std::ffi::OsStr::new("1000"))));
        // Corrupting CURRENT's bytes must also prevent promotion from
        // advertising that damaged package as PREVIOUS.
        let retained = binary.with_extension("retained");
        fs::rename(&binary, &retained).unwrap();
        assert!(promote(&ctx, &second).is_err());
        assert_eq!(
            resolve(&ctx.root, CURRENT).unwrap(),
            Some(first_blob.clone())
        );
        assert_eq!(
            resolve(&ctx.root, PREVIOUS).unwrap(),
            Some(second_blob.clone())
        );
        fs::rename(retained, &binary).unwrap();
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
    fn partial_v6_and_tampered_complete_packages_cannot_change_release_selection() {
        let fixture = Fixture::new();
        let ctx = fixture.context();
        let id = qualify(&ctx).unwrap();
        let package = ctx.packages.join("objects").join(&id);
        let manifest =
            Manifest::decode(&fs::read_to_string(package.join("manifest.txt")).unwrap()).unwrap();
        let mut partial = Manifest::decode(&manifest.encode()).unwrap();
        partial.complete_gates = None;
        let raw = partial.encode();
        let partial_id = digest(raw.as_bytes());
        let historical = ctx.packages.join("objects").join(&partial_id);
        fs::create_dir(&historical).unwrap();
        for name in [
            binary_name(),
            "gates.txt",
            "sbom.json",
            "THIRD-PARTY-NOTICES.txt",
            "native-source-audit.txt",
        ] {
            fs::copy(package.join(name), historical.join(name)).unwrap();
        }
        fs::write(historical.join("manifest.txt"), &raw).unwrap();
        let blob = git_input(&ctx.root, &["hash-object", "-w", "--stdin"], raw.as_bytes()).unwrap();
        git(
            &ctx.root,
            &[
                "update-ref",
                &format!("refs/susi/releases/qualified/{partial_id}"),
                &blob,
            ],
        )
        .unwrap();
        assert!(promote(&ctx, &partial_id)
            .unwrap_err()
            .to_string()
            .contains("requalification"));
        assert!(resolve(&ctx.root, CURRENT).unwrap().is_none());

        let complete = complete_release_gates::test_read_receipt(&package).unwrap();
        let log = package.join(complete["gates"][0]["log"].as_str().unwrap());
        let original = fs::read(&log).unwrap();
        // Windows-only: the retained gate log is written read-only and must
        // lose that attribute before the fixture can replace it. The lint's
        // world-writable concern is a Unix mode-bit effect that cannot arise
        // inside this cfg(windows) block.
        #[cfg(windows)]
        #[allow(
            clippy::permissions_set_readonly_false,
            reason = "Windows read-only attribute, no Unix mode bits involved"
        )]
        {
            let mut permissions = fs::metadata(&log).unwrap().permissions();
            permissions.set_readonly(false);
            fs::set_permissions(&log, permissions).unwrap();
        }
        fs::remove_file(&log).unwrap();
        fs::write(&log, b"substituted successful output").unwrap();
        assert!(promote(&ctx, &id).is_err());
        assert!(resolve(&ctx.root, CURRENT).unwrap().is_none());
        fs::write(&log, original).unwrap();
        let pin = complete["trust"]["permit_sha256"].as_str().unwrap();
        let permit = ctx
            .trust_registry
            .as_ref()
            .unwrap()
            .join("permits")
            .join(format!("{pin}.json"));
        let authorized = fs::read(&permit).unwrap();
        fs::remove_file(&permit).unwrap();
        assert!(promote(&ctx, &id).is_err());
        assert!(resolve(&ctx.root, CURRENT).unwrap().is_none());
        fs::write(&permit, authorized).unwrap();
        promote(&ctx, &id).unwrap();
        assert_eq!(
            resolve(&ctx.root, CURRENT).unwrap().as_deref(),
            Some(
                git(
                    &ctx.root,
                    &["rev-parse", &format!("refs/susi/releases/qualified/{id}")]
                )
                .unwrap()
                .as_str()
            )
        );
    }

    #[test]
    fn qualification_uses_integrated_snapshot_and_preserves_dirty_primary() {
        let fixture = Fixture::new();
        let ctx = fixture.context();
        fs::write(ctx.root.join("untracked"), "preserve").unwrap();
        let id = qualify_with(&ctx, |binary, attempt| {
            let mut output = binary.parent().unwrap().to_path_buf();
            for _ in 0..6 {
                output.push("nested-windows-artifact-directory-with-a-long-name");
            }
            fs::create_dir_all(&output)?;
            fs::write(output.join("probe"), "build output")?;
            fs::write(
                attempt.join("test-output-path"),
                output.to_string_lossy().as_bytes(),
            )?;
            Ok("fixture-only inference boundary\n".into())
        })
        .unwrap();
        let proof = fs::read_dir(&ctx.state)
            .unwrap()
            .map(|entry| entry.unwrap().path().join("test-output-path"))
            .find(|path| path.is_file())
            .unwrap();
        assert!(!Path::new(&fs::read_to_string(proof).unwrap()).exists());
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
