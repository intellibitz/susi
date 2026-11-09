//! Exact execution evidence for Cargo's complete workspace test binaries.
//!
//! This is not a reusable serial test proof. Cargo's actual build output seals
//! the inventory before launch; a separate, identical-selection doctest gate
//! remains mandatory. Historical serial receipts do not enter this protocol.
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;

use super::test_runner::{Binary, Outcome};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
pub const SCHEMA: &str = "susi/cargo-test-binaries/v1";
const BUILD_SUFFIX: [&str; 2] = ["--no-run", "--message-format=json"];

/// The experimental inventory protocol must not be promoted as complete
/// source-input evidence until doctest dependency coverage is supported.
/// Pinned stable rustdoc rejects --test with --emit=dep-info, and its no-run
/// discovery flag requires unstable-options. Never use bootstrap to evade it.
pub fn require_supported_source_inputs() -> Result<()> {
    Err("complete workspace binary mode is unavailable: supported doctest input accounting is not yet implemented; use serial Cargo".into())
}

pub fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub fn file_digest(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 128 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(hash.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("binary evidence missing {key}").into())
}

fn strings(value: &Value, key: &str) -> Result<Vec<String>> {
    value
        .get(key)
        .and_then(Value::as_array)
        .ok_or("missing string array")?
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .ok_or_else(|| "invalid string array".into())
        })
        .collect()
}

fn sha(value: &str) -> Result<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err("invalid binary evidence SHA-256".into());
    }
    Ok(())
}

fn inside(path: &Path, root: &Path) -> Result<()> {
    if !path.is_absolute()
        || path.components().any(|c| matches!(c, Component::ParentDir))
        || !path.starts_with(root)
        || path == root
    {
        return Err(format!(
            "binary evidence path {} escaped isolated directory {}",
            path.display(),
            root.display()
        )
        .into());
    }
    Ok(())
}

pub fn build_argv(test_argv: &[String]) -> Result<Vec<String>> {
    if test_argv.first().map(String::as_str) != Some("cargo")
        || test_argv.get(1).map(String::as_str) != Some("test")
        || !test_argv.iter().any(|a| a == "--workspace")
        || test_argv.iter().any(|a| {
            matches!(a.as_str(), "--" | "--doc" | "--no-run" | "--list")
                || a.starts_with("--message-format")
        })
    {
        return Err("invalid whole-workspace binary test command".into());
    }
    let mut seen = BTreeSet::new();
    let mut arguments = test_argv[2..].iter();
    while let Some(flag) = arguments.next() {
        if !seen.insert(flag) {
            return Err("duplicate binary selection argument".into());
        }
        match flag.as_str() {
            "--workspace"
            | "--offline"
            | "--locked"
            | "--all-features"
            | "--no-default-features" => {}
            "--target" | "--target-dir" | "--features" => {
                let value = arguments
                    .next()
                    .ok_or("binary selection flag without value")?;
                if value.is_empty() || value.starts_with('-') {
                    return Err("invalid binary selection value".into());
                }
            }
            _ => {
                return Err(
                    "unsupported or filtering whole-workspace argument; use serial Cargo".into(),
                );
            }
        }
    }
    let mut argv = test_argv.to_vec();
    argv.extend(BUILD_SUFFIX.map(str::to_owned));
    Ok(argv)
}

pub fn test_argv(build: &[String]) -> Result<Vec<String>> {
    if build.len() < 2 || build[build.len() - 2..] != BUILD_SUFFIX {
        return Err("binary evidence lost exact Cargo build suffix".into());
    }
    let test = build[..build.len() - 2].to_vec();
    if build_argv(&test)? != build {
        return Err("substituted binary build command".into());
    }
    Ok(test)
}

pub fn doc_argv(test: &[String]) -> Result<Vec<String>> {
    build_argv(test)?;
    let mut doc = test.to_vec();
    doc.insert(2, "--doc".into());
    Ok(doc)
}

/// Parse every executable artifact before the older runner can deduplicate it.
/// The same parser is used on the retained raw bytes by the receipt verifier.
pub fn identities(raw: &[u8], metadata: &Value, root: &Path, target: &Path) -> Result<Vec<Value>> {
    if !root.is_absolute()
        || !target.is_absolute()
        || target
            .components()
            .any(|c| matches!(c, Component::ParentDir))
    {
        return Err("non-absolute binary evidence directories".into());
    }
    let mut found = BTreeMap::new();
    let mut paths = BTreeSet::new();
    let mut finished = false;
    for line in std::str::from_utf8(raw)?
        .lines()
        .filter(|line| !line.trim().is_empty())
    {
        if finished {
            return Err("Cargo output continued after build-finished".into());
        }
        let message: Value = serde_json::from_str(line)?;
        if message["reason"] == "compiler-artifact" {
            text(&message, "package_id")?;
            text(&message, "manifest_path")?;
            text(&message["target"], "name")?;
            text(&message["target"], "src_path")?;
            if strings(&message["target"], "kind")?.is_empty()
                || message["profile"]["test"].as_bool().is_none()
                || (!message["executable"].is_null() && message["executable"].as_str().is_none())
            {
                return Err("malformed Cargo artifact record".into());
            }
            strings(&message, "features")?;
            if message.get("executable").is_none() {
                return Err("Cargo artifact omitted executable field".into());
            }
        }
        match message["reason"].as_str() {
            Some("build-finished") => {
                if message["success"] != true {
                    return Err("Cargo build did not succeed".into());
                }
                finished = true;
            }
            Some("compiler-artifact")
                if message["profile"]["test"] == true && !message["executable"].is_null() =>
            {
                let executable = text(&message, "executable")?;
                let manifest = text(&message, "manifest_path")?;
                let package_id = text(&message, "package_id")?;
                let packages = metadata["packages"]
                    .as_array()
                    .ok_or("missing retained Cargo packages")?;
                let matches: Vec<_> = packages.iter().filter(|p| p["id"] == package_id).collect();
                if matches.len() != 1 {
                    return Err("missing or duplicate retained Cargo package identity".into());
                }
                let package = text(matches[0], "name")?;
                let version = text(matches[0], "version")?;
                if text(matches[0], "manifest_path")? != manifest {
                    return Err("artifact manifest differs from retained metadata".into());
                }
                let cargo_target = message.get("target").ok_or("missing Cargo target")?;
                text(cargo_target, "name")?;
                let source = text(cargo_target, "src_path")?;
                if strings(cargo_target, "kind")?.is_empty() {
                    return Err("empty Cargo target kinds".into());
                }
                strings(&message, "features")?;
                inside(Path::new(executable), target)?;
                inside(Path::new(manifest), root)?;
                inside(Path::new(source), root)?;
                let identity = json!({"package_id":package_id,"package":package,"version":version,
                    "manifest_path":manifest,"target":cargo_target,"features":message["features"],
                    "profile":message["profile"],"executable":executable});
                let key = identity_key(&identity)?;
                if !paths.insert(executable.to_owned()) || found.insert(key, identity).is_some() {
                    return Err("duplicate Cargo test artifact identity or executable".into());
                }
            }
            Some("compiler-artifact" | "compiler-message" | "build-script-executed") => {}
            _ => return Err("unknown Cargo build output record".into()),
        }
    }
    if !finished || found.is_empty() {
        return Err("incomplete or empty Cargo test inventory".into());
    }
    Ok(found.into_values().collect())
}

fn identity_key(identity: &Value) -> Result<String> {
    Ok(digest(&serde_json::to_vec(identity)?))
}

fn agrees(identity: &Value, binary: &Binary) -> Result<bool> {
    Ok(text(identity, "package")? == binary.package
        && text(identity, "version")? == binary.version
        && Path::new(text(identity, "manifest_path")?) == binary.manifest_path
        && text(&identity["target"], "name")? == binary.target_name
        && strings(&identity["target"], "kind")?.first() == Some(&binary.kind)
        && Path::new(text(&identity["target"], "src_path")?) == binary.src_path
        && Path::new(text(identity, "executable")?) == binary.executable)
}

pub struct Inventory {
    pub artifacts: Vec<Value>,
    launched: Mutex<BTreeSet<String>>,
    // Windows denies replacement/writes for the complete execution lifetime.
    // This is stronger than hoping a transient sibling rebuild remains visible
    // to an after-the-fact hash. Unsupported hosts retain the serial path.
    _pins: Vec<fs::File>,
}

impl Inventory {
    pub fn seal(
        raw: &[u8],
        metadata: &Value,
        root: &Path,
        target: &Path,
        binaries: &[Binary],
    ) -> Result<Self> {
        let expected = identities(raw, metadata, root, target)?;
        if expected.len() != binaries.len() {
            return Err("parsed binary inventory lost artifacts".into());
        }
        let canonical_root = root.canonicalize()?;
        let canonical_target = target.canonicalize()?;
        let mut observed = BTreeSet::new();
        let mut projections = BTreeSet::new();
        let mut canonical_paths = BTreeSet::new();
        let mut pins = Vec::new();
        let mut artifacts = Vec::new();
        for identity in expected {
            let matches = binaries
                .iter()
                .filter_map(|b| match agrees(&identity, b) {
                    Ok(true) => Some(Ok(b)),
                    Ok(false) => None,
                    Err(e) => Some(Err(e)),
                })
                .collect::<Result<Vec<_>>>()?;
            if matches.len() != 1 {
                return Err("missing, duplicated or substituted parsed binary".into());
            }
            let executable = Path::new(text(&identity, "executable")?);
            let canonical = executable.canonicalize()?;
            inside(&canonical, &canonical_target)?;
            if !canonical_paths.insert(canonical) || !projections.insert(matches[0].id()) {
                return Err("duplicate canonical executable or runner projection".into());
            }
            reject_redirects(executable, target)?;
            inside(
                &Path::new(text(&identity, "manifest_path")?).canonicalize()?,
                &canonical_root,
            )?;
            inside(
                &Path::new(text(&identity["target"], "src_path")?).canonicalize()?,
                &canonical_root,
            )?;
            if !observed.insert(identity_key(&identity)?) {
                return Err("duplicate binary identity".into());
            }
            let mut options = fs::OpenOptions::new();
            options.read(true);
            #[cfg(windows)]
            {
                use std::os::windows::fs::OpenOptionsExt;
                options.share_mode(1);
            }
            pins.push(options.open(executable)?);
            artifacts.push(json!({"identity":identity,"sha256":file_digest(executable)?}));
        }
        Ok(Self {
            artifacts,
            launched: Mutex::new(BTreeSet::new()),
            _pins: pins,
        })
    }

    pub fn launch(&self, binary: &Binary) -> Result<()> {
        let matching = self
            .artifacts
            .iter()
            .filter_map(|a| match agrees(&a["identity"], binary) {
                Ok(true) => Some(Ok(a)),
                Ok(false) => None,
                Err(e) => Some(Err(e)),
            })
            .collect::<Result<Vec<_>>>()?;
        if matching.len() != 1 {
            return Err("unplanned or substituted binary launch".into());
        }
        let artifact = matching[0];
        if file_digest(&binary.executable)? != text(artifact, "sha256")? {
            return Err("test executable changed before launch".into());
        }
        if !self
            .launched
            .lock()
            .map_err(|_| "launch accounting poisoned")?
            .insert(identity_key(&artifact["identity"])?)
        {
            return Err("duplicate binary launch".into());
        }
        Ok(())
    }

    pub fn recheck(&self) -> Result<()> {
        for artifact in &self.artifacts {
            if file_digest(Path::new(text(&artifact["identity"], "executable")?))?
                != text(artifact, "sha256")?
            {
                return Err("test executable changed during execution".into());
            }
        }
        Ok(())
    }

    pub fn complete(&self, outcome: &Outcome) -> Result<Vec<String>> {
        self.recheck()?;
        let expected = self
            .artifacts
            .iter()
            .map(|a| identity_key(&a["identity"]))
            .collect::<Result<BTreeSet<_>>>()?;
        let launched = self
            .launched
            .lock()
            .map_err(|_| "launch accounting poisoned")?;
        if outcome.ran != expected.len() || !outcome.failed.is_empty() || *launched != expected {
            return Err("incomplete or failed exact binary execution".into());
        }
        Ok(launched.iter().cloned().collect())
    }
}

pub struct RetainedRecords<'a> {
    pub plan: &'a Value,
    pub result: &'a Value,
    pub raw: &'a [u8],
    pub metadata_raw: &'a [u8],
}

/// Verify retained records without depending on the lifetime of target caches.
pub fn verify_plan(
    records: RetainedRecords<'_>,
    argv: &[String],
    cwd: &Path,
) -> Result<Vec<String>> {
    let RetainedRecords {
        plan,
        result,
        raw,
        metadata_raw,
    } = records;
    if plan["schema"] != SCHEMA
        || result["schema"] != SCHEMA
        || strings(plan, "argv")? != argv
        || Path::new(text(plan, "cwd")?) != cwd
        || text(plan, "build_stdout_sha256")? != digest(raw)
        || text(result, "plan_sha256")? != digest(&serde_json::to_vec(plan)?)
    {
        return Err("substituted binary execution plan".into());
    }
    if text(plan, "metadata_sha256")? != digest(metadata_raw) {
        return Err("retained Cargo metadata changed".into());
    }
    let metadata: Value = serde_json::from_slice(metadata_raw)?;
    let target = Path::new(text(plan, "target_dir")?);
    let dirs: Vec<_> = argv.windows(2).filter(|p| p[0] == "--target-dir").collect();
    if dirs.len() != 1 || Path::new(&dirs[0][1]) != target {
        return Err("binary plan target differs from command".into());
    }
    let expected = identities(raw, &metadata, cwd, target)?;
    let artifacts = plan["artifacts"]
        .as_array()
        .ok_or("missing planned artifacts")?;
    if expected.len() != artifacts.len() {
        return Err("missing planned binary artifacts".into());
    }
    let mut keys = BTreeSet::new();
    for (identity, artifact) in expected.iter().zip(artifacts) {
        if identity != &artifact["identity"] {
            return Err("substituted binary artifact".into());
        }
        sha(text(artifact, "sha256")?)?;
        if !keys.insert(identity_key(identity)?) {
            return Err("duplicate planned binary".into());
        }
    }
    let executed = strings(result, "executed")?;
    if executed.len() != keys.len()
        || executed.iter().cloned().collect::<BTreeSet<_>>() != keys
        || result["ran"].as_u64() != Some(keys.len() as u64)
        || result["failed"].as_u64() != Some(0)
        || text(result, "input_sha256_after")? != text(plan, "input_sha256")?
    {
        return Err("incomplete binary execution evidence".into());
    }
    sha(text(plan, "input_sha256")?)?;
    let runner = &plan["runner"];
    if runner["assurance"] != "deterministic-runner-inputs-v1"
        || runner["nested_target"] != "per-binary-home/target"
        || runner["runtime_libraries"] != "outer-target-profile-and-deps"
        || Path::new(text(&runner["settings"], "target_dir")?) != target
    {
        return Err("missing binary runtime assurance contract".into());
    }
    for field in [
        "runner_source_sha256",
        "callback_source_sha256",
        "parent_path_sha256",
        "parent_cargo_sha256",
        "resolved_cargo_sha256",
        "runner_executable_sha256",
    ] {
        sha(text(runner, field)?)?;
    }
    for field in ["concurrency", "total_test_threads", "build_jobs"] {
        if runner["settings"][field].as_u64().is_none_or(|n| n == 0) {
            return Err("invalid binary runner budget".into());
        }
    }
    let triple = argv
        .windows(2)
        .find_map(|p| (p[0] == "--target").then_some(p[1].as_str()));
    if runner["settings"]["triple"] != json!(triple)
        || plan["runtime"] != Runtime::parse(raw, target, triple)?.record
    {
        return Err("substituted build-script runtime contract".into());
    }
    let docs = doc_argv(&test_argv(argv)?)?;
    if strings(plan, "doc_argv")? != docs {
        return Err("weakened planned doctest companion".into());
    }
    Ok(docs)
}

fn reject_redirects(path: &Path, root: &Path) -> Result<()> {
    for ancestor in path.ancestors() {
        let metadata = fs::symlink_metadata(ancestor)?;
        if metadata.file_type().is_symlink() {
            return Err("redirected test executable path".into());
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes() & 0x400 != 0 {
                return Err("reparse point in executable path".into());
            }
        }
        if ancestor == root {
            return Ok(());
        }
    }
    Err("test artifact lacks isolated target ancestor".into())
}

pub fn metadata_for_build(root: &Path, build: &Command) -> Result<Vec<u8>> {
    let mut command = Command::new(build.get_program());
    command.current_dir(root);
    for (key, value) in build.get_envs() {
        match value {
            Some(value) => {
                command.env(key, value);
            }
            None => {
                command.env_remove(key);
            }
        }
    }
    command.args([
        "metadata",
        "--format-version",
        "1",
        "--no-deps",
        "--offline",
        "--locked",
    ]);
    let args: Vec<_> = build.get_args().collect();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let arg = *arg;
        if arg == "--features" {
            command
                .arg("--features")
                .arg(args.next().ok_or("features without value")?);
        } else if arg == "--no-default-features" || arg == "--all-features" {
            command.arg(arg);
        } else if arg == "--target" {
            command
                .arg("--filter-platform")
                .arg(args.next().ok_or("target without value")?);
        }
    }
    let output = command.output()?;
    if !output.status.success() {
        return Err("Cargo metadata failed for binary execution".into());
    }
    let _: Value = serde_json::from_slice(&output.stdout)?;
    Ok(output.stdout)
}

/// Runtime settings are input-bound to the compiled runner implementation.
/// This deliberately does not claim to observe its final Command: that runner
/// adds Cargo variables after the launcher callback returns.
pub struct RunnerInputs {
    pub record: Value,
    parent_path: Option<OsString>,
    parent_cargo: Option<OsString>,
    cargo: PathBuf,
}
impl RunnerInputs {
    pub fn capture(
        settings: &super::test_runner::Settings<'_>,
        callback_source: &[u8],
    ) -> Result<Self> {
        let parent_path = std::env::var_os("PATH");
        let parent_cargo = std::env::var_os("CARGO");
        let cargo = parent_cargo
            .as_ref()
            .map(PathBuf::from)
            .or_else(|| {
                parent_path.as_ref().and_then(|path| {
                    std::env::split_paths(path)
                        .map(|dir| dir.join(format!("cargo{}", std::env::consts::EXE_SUFFIX)))
                        .find(|p| p.is_file())
                })
            })
            .ok_or("runner cannot bind parent Cargo resolution")?;
        if !cargo.is_absolute() {
            return Err("runner Cargo resolution is not absolute".into());
        }
        let path_text = parent_path
            .as_deref()
            .map(|p| p.to_str().ok_or("non-UTF8 runner PATH"))
            .transpose()?;
        let cargo_text = parent_cargo
            .as_deref()
            .map(|p| p.to_str().ok_or("non-UTF8 runner CARGO"))
            .transpose()?;
        let record = json!({"assurance":"deterministic-runner-inputs-v1","runner_source_sha256":digest(include_bytes!("test_runner.rs")),
            "callback_source_sha256":digest(callback_source),
            "runner_executable_sha256":file_digest(&std::env::current_exe()?)?,
            "parent_path_sha256":digest(&serde_json::to_vec(&path_text)?),"parent_cargo_sha256":digest(&serde_json::to_vec(&cargo_text)?),
            "resolved_cargo":cargo,"resolved_cargo_sha256":file_digest(&cargo)?,
            "settings":{"concurrency":settings.concurrency,"total_test_threads":settings.total_test_threads,"build_jobs":settings.build_jobs,
                "home_root":settings.home_root,"target_dir":settings.target_dir,"triple":settings.triple,"heavy":settings.heavy},
            "nested_target":"per-binary-home/target","runtime_libraries":"outer-target-profile-and-deps"});
        Ok(Self {
            record,
            parent_path,
            parent_cargo,
            cargo,
        })
    }
    pub fn recheck(&self) -> Result<()> {
        if std::env::var_os("PATH") != self.parent_path
            || std::env::var_os("CARGO") != self.parent_cargo
            || file_digest(&self.cargo)? != text(&self.record, "resolved_cargo_sha256")?
        {
            return Err("runner parent inputs changed".into());
        }
        Ok(())
    }
}

/// Cargo also supplies a package's build-script rustc-env declarations at test
/// runtime. Honor those explicitly; refuse native search paths that this runner
/// cannot reproduce. Serial Cargo remains available for unsupported contracts.
pub struct Runtime {
    environments: BTreeMap<String, BTreeMap<String, String>>,
    record: Value,
}
impl Runtime {
    pub fn parse(raw: &[u8], target: &Path, triple: Option<&str>) -> Result<Self> {
        let mut environments = BTreeMap::new();
        let mut records: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
        for line in std::str::from_utf8(raw)?
            .lines()
            .filter(|l| !l.trim().is_empty())
        {
            let value: Value = serde_json::from_str(line)?;
            if value["reason"] != "build-script-executed" {
                continue;
            }
            if !strings(&value, "linked_paths")?.is_empty() {
                return Err("binary runtime does not support build-script library search paths; use serial Cargo".into());
            }
            let package = text(&value, "package_id")?;
            // Cargo legitimately reports a dependency's build script once for
            // the host and again for an explicit target. Their cfgs/link flags
            // affect the already-bound compiled artifacts; they are not child
            // environment variables. Preserve BOTH complete records and roles.
            // Only the equal runtime env projection may be shared.
            strings(&value, "cfgs")?;
            strings(&value, "linked_libs")?;
            let out = Path::new(text(&value, "out_dir")?);
            inside(out, target)?;
            if out.file_name().is_none_or(|name| name != "out") {
                return Err("unsupported build-script output directory".into());
            }
            let profile = out
                .parent()
                .and_then(Path::parent)
                .ok_or("invalid build-script profile")?;
            let host_profile = target.join("debug/build");
            let selected_profile = triple.map(|t| target.join(t).join("debug/build"));
            let role = if selected_profile.as_deref() == Some(profile) {
                "target"
            } else if profile == host_profile {
                "host"
            } else {
                return Err("unsupported build-script target/profile".into());
            };
            if records
                .entry(package.into())
                .or_default()
                .insert(role.into(), digest(&serde_json::to_vec(&value)?))
                .is_some()
            {
                return Err("duplicate build-script record in one target/profile".into());
            }
            let mut environment = BTreeMap::new();
            for pair in value["env"]
                .as_array()
                .ok_or("missing build-script runtime environment")?
            {
                let pair = pair
                    .as_array()
                    .filter(|p| p.len() == 2)
                    .ok_or("malformed build-script environment pair")?;
                let key = pair[0]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .ok_or("invalid runtime environment key")?;
                let val = pair[1]
                    .as_str()
                    .ok_or("invalid runtime environment value")?;
                let upper = key.to_ascii_uppercase();
                if upper.starts_with("CARGO")
                    || upper.starts_with("RUST")
                    || upper.starts_with("XDG_")
                    || [
                        "PATH",
                        "HOME",
                        "USERPROFILE",
                        "HOMEDRIVE",
                        "HOMEPATH",
                        "APPDATA",
                        "LOCALAPPDATA",
                        "SUSI_HOME",
                        "SUSI_NO_REGISTRY_ENV",
                        "TEMP",
                        "TMP",
                        "TMPDIR",
                    ]
                    .contains(&upper.as_str())
                    || [
                        "TOKEN",
                        "SECRET",
                        "PASSWORD",
                        "CREDENTIAL",
                        "API_KEY",
                        "ACCESS_KEY",
                    ]
                    .iter()
                    .any(|p| upper.contains(p))
                {
                    return Err("unsupported reserved or sensitive build-script runtime environment; use serial Cargo".into());
                }
                if environment.insert(key.to_owned(), val.to_owned()).is_some() {
                    return Err("duplicate build-script runtime environment".into());
                }
            }
            if let Some(previous) = environments.get(package) {
                if previous != &environment {
                    return Err("conflicting host/target build-script runtime environments".into());
                }
            } else {
                environments.insert(package.to_owned(), environment);
            }
        }
        Ok(Self {
            environments,
            record: json!({"schema":"susi/build-script-runtime/v1",
            "profile":"debug","triple":triple,"records":records}),
        })
    }
    pub fn apply(
        &self,
        inventory: &Inventory,
        binary: &Binary,
        command: &mut Command,
    ) -> Result<()> {
        let artifact = inventory
            .artifacts
            .iter()
            .find(|a| agrees(&a["identity"], binary).unwrap_or(false))
            .ok_or("unknown runtime binary")?;
        let package = text(&artifact["identity"], "package_id")?;
        if let Some(environment) = self.environments.get(package) {
            command.envs(environment);
        }
        Ok(())
    }
}

fn relevant_environment(name: &str) -> bool {
    if [
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
        "PROXY",
    ]
    .iter()
    .any(|part| name.contains(part))
    {
        return false;
    }
    name.starts_with("CARGO_")
        || name.starts_with("RUST")
        || matches!(
            name,
            "PATH"
                | "PATHEXT"
                | "SYSTEMROOT"
                | "WINDIR"
                | "COMSPEC"
                | "HOME"
                | "USERPROFILE"
                | "APPDATA"
                | "LOCALAPPDATA"
                | "XDG_CONFIG_HOME"
                | "XDG_DATA_HOME"
                | "XDG_CACHE_HOME"
                | "TEMP"
                | "TMP"
                | "TMPDIR"
                | "LIB"
                | "LIBPATH"
                | "INCLUDE"
                | "CC"
                | "CXX"
                | "AR"
                | "SUSI_VERIFIED_SYSROOT"
                | "SUSI_HOME"
                | "SUSI_NO_REGISTRY_ENV"
        )
}

/// Only relevant effective compiler inputs participate, and only their digest
/// leaves memory. Credentials and unrelated inherited values are never retained.
pub struct Inputs {
    root: PathBuf,
    argv: Vec<String>,
    overrides: Vec<(OsString, Option<OsString>)>,
    pub sha256: String,
}

impl Inputs {
    pub fn capture(root: &Path, command: &Command) -> Result<Self> {
        let mut input = Self {
            root: root.to_owned(),
            argv: command
                .get_args()
                .map(|a| {
                    a.to_str()
                        .map(str::to_owned)
                        .ok_or("non-UTF8 compiler argument")
                })
                .collect::<std::result::Result<_, _>>()?,
            overrides: command
                .get_envs()
                .filter(|(k, _)| relevant_environment(&k.to_string_lossy().to_ascii_uppercase()))
                .map(|(k, v)| (k.to_owned(), v.map(std::ffi::OsStr::to_owned)))
                .collect(),
            sha256: String::new(),
        };
        input.sha256 = input.measure()?;
        Ok(input)
    }

    fn environment(&self) -> Result<BTreeMap<String, String>> {
        let mut environment = BTreeMap::new();
        for (key, value) in std::env::vars_os()
            .filter(|(k, _)| relevant_environment(&k.to_string_lossy().to_ascii_uppercase()))
        {
            environment.insert(
                key.to_str()
                    .ok_or("non-UTF8 compiler environment key")?
                    .to_ascii_uppercase(),
                value
                    .to_str()
                    .ok_or("non-UTF8 compiler environment value")?
                    .to_owned(),
            );
        }
        for (key, value) in &self.overrides {
            let key = key
                .to_str()
                .ok_or("non-UTF8 compiler environment key")?
                .to_ascii_uppercase();
            match value {
                Some(value) => {
                    environment.insert(
                        key,
                        value
                            .to_str()
                            .ok_or("non-UTF8 compiler environment value")?
                            .to_owned(),
                    );
                }
                None => {
                    environment.remove(&key);
                }
            }
        }
        Ok(environment)
    }

    fn git(&self, args: &[&str]) -> Result<Vec<u8>> {
        let out = Command::new("git")
            .arg("--literal-pathspecs")
            .env("GIT_NO_REPLACE_OBJECTS", "1")
            .current_dir(&self.root)
            .args(args)
            .output()?;
        if !out.status.success() {
            return Err("cannot snapshot source inputs".into());
        }
        Ok(out.stdout)
    }

    fn measure(&self) -> Result<String> {
        let environment = self.environment()?;
        let mut sources = BTreeMap::new();
        let names = self.git(&[
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ])?;
        for name in names.split(|b| *b == 0).filter(|name| !name.is_empty()) {
            let name = std::str::from_utf8(name)?;
            let path = self.root.join(name);
            inside(&path, &self.root)?;
            let value =
                match fs::symlink_metadata(&path) {
                    Ok(meta) if meta.file_type().is_symlink() => return Err(
                        "redirected source input is unsupported by binary gate; use serial Cargo"
                            .into(),
                    ),
                    Ok(meta) if meta.is_file() => {
                        reject_redirects(&path, &self.root)?;
                        #[cfg(unix)]
                        let mode = {
                            use std::os::unix::fs::PermissionsExt;
                            meta.permissions().mode()
                        };
                        #[cfg(windows)]
                        let mode = {
                            use std::os::windows::fs::MetadataExt;
                            meta.file_attributes()
                        };
                        json!({"sha256":file_digest(&path)?,"mode":mode})
                    }
                    Ok(_) => return Err("non-file source input".into()),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Value::Null,
                    Err(error) => return Err(error.into()),
                };
            sources.insert(name.to_owned(), value);
        }
        let mut configs = BTreeMap::new();
        for directory in self
            .root
            .ancestors()
            .map(|p| p.join(".cargo"))
            .chain(environment.get("CARGO_HOME").map(PathBuf::from))
        {
            for filename in ["config", "config.toml"] {
                let path = directory.join(filename);
                match fs::read(&path) {
                    Ok(bytes) => {
                        configs.insert(path, digest(&bytes));
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
            }
        }
        // Reuse the established pinned compiler selection rather than inventing
        // a replacement compiler or trusting an inherited wrapper.
        let mut compiler = Command::new("rustc");
        compiler.current_dir(&self.root);
        super::compiler_env::apply_checked(&mut compiler)?;
        let rustc = compiler
            .get_envs()
            .find_map(|(k, v)| (k == "RUSTC").then_some(v).flatten())
            .map(PathBuf::from)
            .ok_or("missing qualified compiler")?;
        let bin = rustc.parent().ok_or("compiler without directory")?;
        let toolchain = bin.parent().ok_or("compiler without sysroot")?;
        let mut tools = BTreeMap::new();
        for name in ["cargo", "rustc", "rustdoc"] {
            let path = bin.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
            tools.insert(path.clone(), file_digest(&path)?);
        }
        for directory in [bin.to_owned(), toolchain.join("lib")] {
            for entry in fs::read_dir(directory)? {
                let entry = entry?;
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with("rustc_driver") || name.starts_with("librustc_driver") {
                    tools.insert(entry.path(), file_digest(&entry.path())?);
                }
            }
        }
        let info = Command::new(&rustc).arg("-vV").output()?;
        if !info.status.success() {
            return Err("cannot reidentify compiler".into());
        }
        let info = String::from_utf8(info.stdout)?;
        let host = info
            .lines()
            .find_map(|l| l.strip_prefix("host: "))
            .ok_or("compiler omitted host")?;
        let target = self
            .argv
            .windows(2)
            .find_map(|p| (p[0] == "--target").then_some(p[1].as_str()))
            .unwrap_or(host);
        let sysroot = environment
            .get("SUSI_VERIFIED_SYSROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| toolchain.to_owned());
        let libraries = sysroot.join("lib/rustlib").join(target).join("lib");
        for entry in fs::read_dir(libraries)? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                tools.insert(entry.path(), file_digest(&entry.path())?);
            }
        }
        Ok(digest(&serde_json::to_vec(
            &json!({"argv":self.argv,"source":sources,
            "head":digest(&self.git(&["rev-parse","HEAD","HEAD^{tree}"])?),
            "config":configs,"tools":tools,"compiler":info,
            "effective_environment_sha256":digest(&serde_json::to_vec(&environment)?)}),
        )?))
    }

    pub fn recheck(&self) -> Result<()> {
        if self.measure()? != self.sha256 {
            return Err("binary gate execution inputs changed".into());
        }
        Ok(())
    }
}

/// A successful binary run whose inputs and artifact pins remain live until
/// every companion gate has finished. It is never a serial workspace proof.
pub struct Execution {
    pub plan: Value,
    pub result: Value,
    inventory: Inventory,
    inputs: Inputs,
    runner: RunnerInputs,
    started: std::time::Instant,
}

impl Execution {
    pub fn recheck(&self) -> Result<()> {
        self.inventory.recheck()?;
        self.inputs.recheck()?;
        self.runner.recheck()
    }

    pub fn elapsed_ms(&self) -> u128 {
        self.started.elapsed().as_millis()
    }
}

pub struct ExecutionSpec<'a> {
    pub root: &'a Path,
    pub argv: &'a [String],
    pub settings: &'a super::test_runner::Settings<'a>,
    pub callback_source: &'a [u8],
}

pub type RetainPlan<'a> = dyn Fn(&Value, &[u8], &[u8]) -> Result<()> + 'a;

/// Build, strictly seal, and actually execute the complete Cargo inventory.
/// `retain` runs before any child and lets the native launcher persist immutable
/// evidence. Public replay rebuilds fresh evidence, not unavailable old blobs.
pub fn execute(
    spec: ExecutionSpec<'_>,
    build: &mut Command,
    launch: &(dyn Fn(&Binary, &Path) -> Result<Command> + Sync),
    retain: &RetainPlan<'_>,
) -> Result<Execution> {
    let ExecutionSpec {
        root,
        argv,
        settings,
        callback_source,
    } = spec;
    if !cfg!(windows) {
        return Err("complete binary runtime currently requires Windows; use serial Cargo".into());
    }
    let started = std::time::Instant::now();
    let logical = test_argv(argv)?;
    let actual = build
        .get_args()
        .map(|a| a.to_str().ok_or("non-UTF8 build argument"))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if actual != argv[1..] || build.get_current_dir() != Some(root) {
        return Err("binary build command differs from its plan".into());
    }
    let inputs = Inputs::capture(root, build)?;
    let metadata_raw = metadata_for_build(root, build)?;
    let metadata: Value = serde_json::from_slice(&metadata_raw)?;
    let mut log = fs::OpenOptions::new().append(true).open(settings.log)?;
    let output = build
        .stdout(Stdio::piped())
        .stderr(Stdio::from(log.try_clone()?))
        .output()?;
    if !output.status.success() {
        log.write_all(&output.stdout)?;
        return Err("complete binary build failed".into());
    }
    // The strict parser must run before the legacy parser drops duplicate rows.
    identities(&output.stdout, &metadata, root, settings.target_dir)?;
    let (binaries, diagnostics) =
        super::test_runner::parse_artifacts(std::str::from_utf8(&output.stdout)?)?;
    for diagnostic in diagnostics {
        log.write_all(diagnostic.as_bytes())?;
    }
    let runtime = Runtime::parse(&output.stdout, settings.target_dir, settings.triple)?;
    let inventory = Inventory::seal(
        &output.stdout,
        &metadata,
        root,
        settings.target_dir,
        &binaries,
    )?;
    let runner = RunnerInputs::capture(settings, callback_source)?;
    inputs.recheck()?;
    let plan = json!({"schema":SCHEMA,"argv":argv,"cwd":root,"target_dir":settings.target_dir,
        "doc_argv":doc_argv(&logical)?,"build_stdout_sha256":digest(&output.stdout),
        "metadata_sha256":digest(&metadata_raw),"artifacts":inventory.artifacts,
        "input_sha256":inputs.sha256,"runner":runner.record,"runtime":runtime.record});
    retain(&plan, &output.stdout, &metadata_raw)?;
    let checked_launch = |binary: &Binary, home: &Path| {
        let mut command = launch(binary, home)?;
        runtime.apply(&inventory, binary, &mut command)?;
        inventory.launch(binary)?;
        Ok(command)
    };
    let outcome = super::test_runner::run(&binaries, settings, &checked_launch)?;
    let executed = inventory.complete(&outcome)?;
    inputs.recheck()?;
    runner.recheck()?;
    let result = json!({"schema":SCHEMA,"executed":executed,"ran":outcome.ran,"failed":0,
        "input_sha256_after":inputs.sha256,"plan_sha256":digest(&serde_json::to_vec(&plan)?)});
    Ok(Execution {
        plan,
        result,
        inventory,
        inputs,
        runner,
        started,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Fixture {
        root: PathBuf,
        target: PathBuf,
        binary: Binary,
        artifact: Value,
    }
    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "susi-exact-binaries-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(root.join("src")).unwrap();
            let target = root.join("target");
            fs::create_dir_all(&target).unwrap();
            let manifest = root.join("Cargo.toml");
            let source = root.join("src/lib.rs");
            let executable = target.join("fixture.exe");
            fs::write(
                &manifest,
                "[package]\nname=\"fixture\"\nversion=\"0.1.0\"\n",
            )
            .unwrap();
            fs::write(&source, "pub fn value()->u32 { 3 }\n").unwrap();
            fs::write(&executable, b"unit-test artifact, not executed").unwrap();
            let binary = Binary {
                package: "fixture".into(),
                version: "0.1.0".into(),
                manifest_path: manifest.clone(),
                target_name: "fixture".into(),
                kind: "lib".into(),
                src_path: source.clone(),
                executable: executable.clone(),
            };
            let artifact = json!({"reason":"compiler-artifact","package_id":"path+file:///fixture#fixture@0.1.0","manifest_path":manifest,
                "target":{"name":"fixture","kind":["lib"],"src_path":source},"features":[],"profile":{"test":true},"executable":executable});
            Self {
                root,
                target,
                binary,
                artifact,
            }
        }
        fn metadata(&self) -> Value {
            json!({"packages":[{"id":self.artifact["package_id"],"name":"fixture","version":"0.1.0","manifest_path":self.binary.manifest_path}]})
        }
        fn raw(&self) -> Vec<u8> {
            format!(
                "{}\n{}\n",
                self.artifact,
                json!({"reason":"build-finished","success":true})
            )
            .into_bytes()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn inventory_requires_complete_strict_cargo_output() {
        let f = Fixture::new();
        assert_eq!(
            identities(&f.raw(), &f.metadata(), &f.root, &f.target)
                .unwrap()
                .len(),
            1
        );
        for raw in [
            vec![255],
            b"not JSON\n".to_vec(),
            format!("{}\n", f.artifact).into_bytes(),
            format!(
                "{}\n{{\"reason\":\"build-finished\",\"success\":false}}\n",
                f.artifact
            )
            .into_bytes(),
            format!(
                "{}\n{}\n{{\"reason\":\"build-finished\",\"success\":true}}\n",
                f.artifact, f.artifact
            )
            .into_bytes(),
        ] {
            assert!(identities(&raw, &f.metadata(), &f.root, &f.target).is_err());
        }
        let mut trailing = f.raw();
        trailing.extend_from_slice(b"{\"reason\":\"build-finished\",\"success\":true}\n");
        assert!(identities(&trailing, &f.metadata(), &f.root, &f.target).is_err());
    }

    #[test]
    fn complete_inventory_and_full_identity_are_required() {
        let f = Fixture::new();
        let raw = f.raw();
        assert!(Inventory::seal(&raw, &f.metadata(), &f.root, &f.target, &[]).is_err());
        assert!(Inventory::seal(
            &raw,
            &f.metadata(),
            &f.root,
            &f.target,
            &[f.binary.clone(), f.binary.clone()]
        )
        .is_err());
        for field in 0..7 {
            let mut changed = f.binary.clone();
            match field {
                0 => changed.version = "2".into(),
                1 => changed.package = "other".into(),
                2 => changed.kind = "bin".into(),
                3 => changed.target_name = "other".into(),
                4 => changed.src_path = f.root.join("other.rs"),
                5 => changed.manifest_path = f.root.join("other.toml"),
                _ => changed.executable = f.target.join("other.exe"),
            };
            assert!(Inventory::seal(&raw, &f.metadata(), &f.root, &f.target, &[changed]).is_err());
        }
    }

    #[test]
    fn exact_launches_completion_and_unchanged_artifacts_are_required() {
        let f = Fixture::new();
        let inventory = Inventory::seal(
            &f.raw(),
            &f.metadata(),
            &f.root,
            &f.target,
            std::slice::from_ref(&f.binary),
        )
        .unwrap();
        let success = Outcome {
            ran: 1,
            failed: Vec::new(),
            elapsed_ms: 0,
        };
        assert!(inventory.complete(&success).is_err());
        inventory.launch(&f.binary).unwrap();
        assert!(inventory.launch(&f.binary).is_err());
        assert_eq!(inventory.complete(&success).unwrap().len(), 1);
        assert!(inventory
            .complete(&Outcome {
                ran: 0,
                failed: Vec::new(),
                elapsed_ms: 0
            })
            .is_err());
        assert!(inventory
            .complete(&Outcome {
                ran: 1,
                failed: vec![(f.binary.id(), None)],
                elapsed_ms: 0
            })
            .is_err());
        let changed = fs::write(&f.binary.executable, b"substituted");
        if cfg!(windows) {
            assert!(changed.is_err(), "artifact pin must deny writes");
            inventory.recheck().unwrap();
        } else {
            changed.unwrap();
            assert!(inventory.recheck().is_err());
            assert!(inventory.launch(&f.binary).is_err());
        }
    }

    #[test]
    fn companion_preserves_exact_selection_and_build_suffix() {
        let argv = [
            "cargo",
            "test",
            "--workspace",
            "--offline",
            "--locked",
            "--target",
            "native",
            "--target-dir",
            "target",
            "--features",
            "xtask/release-tooling",
        ]
        .map(str::to_owned)
        .to_vec();
        let build = build_argv(&argv).unwrap();
        assert_eq!(test_argv(&build).unwrap(), argv);
        let doc = doc_argv(&argv).unwrap();
        assert_eq!(doc[2], "--doc");
        assert_eq!(&doc[3..], &argv[2..]);
        assert!(test_argv(&argv).is_err());
        let mut weakened = argv;
        weakened.push("--".into());
        weakened.push("filter".into());
        assert!(build_argv(&weakened).is_err());
    }

    #[test]
    fn repeated_build_scripts_require_distinct_roles_and_equal_runtime_maps() {
        let f = Fixture::new();
        let host = json!({"reason":"build-script-executed","package_id":"same-package",
            "linked_paths":[],"linked_libs":[],"cfgs":["host_config"],"env":[["VALUE","same"]],
            "out_dir":f.target.join("debug/build/pkg-host/out")});
        let mut target = host.clone();
        target["out_dir"] = json!(f.target.join("native/debug/build/pkg-target/out"));
        target["cfgs"] = json!(["target_config"]);
        let raw = |other: &Value| format!("{host}\n{other}\n").into_bytes();
        let parsed = Runtime::parse(&raw(&target), &f.target, Some("native")).unwrap();
        assert_eq!(parsed.environments.len(), 1);
        assert_eq!(
            parsed.record["records"]["same-package"]
                .as_object()
                .unwrap()
                .len(),
            2
        );
        assert!(Runtime::parse(&raw(&host), &f.target, Some("native")).is_err());
        for case in 0..4 {
            let mut changed = target.clone();
            match case {
                0 => changed["env"] = json!([["VALUE", "different"]]),
                1 => {
                    changed["out_dir"] = json!(f.target.join("native/release/build/pkg-target/out"))
                }
                2 => changed["linked_paths"] = json!(["native=unsupported"]),
                _ => changed["env"] = json!([["CARGO_TARGET_DIR", "unsupported"]]),
            }
            assert!(Runtime::parse(&raw(&changed), &f.target, Some("native")).is_err());
        }
        for key in [
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "XDG_CACHE_HOME",
            "XDG_STATE_HOME",
            "HOMEDRIVE",
            "HOMEPATH",
        ] {
            let mut redirected = target.clone();
            redirected["env"] = json!([[key, "outside-private-home"]]);
            assert!(Runtime::parse(&raw(&redirected), &f.target, Some("native")).is_err());
        }
    }

    #[test]
    #[cfg(windows)]
    fn real_ignored_include_mutation_cannot_enable_complete_input_claim() {
        let f = Fixture::new();
        fs::write(f.root.join("Cargo.toml"),"[package]\nname=\"ignored_fixture\"\nversion=\"0.1.0\"\nedition=\"2021\"\n[workspace]\n").unwrap();
        fs::write(f.root.join(".gitignore"), "target/\nignored-data.txt\n").unwrap();
        fs::write(f.root.join("ignored-data.txt"), b"first").unwrap();
        fs::write(f.root.join("src/lib.rs"),"/// ```\n/// assert_eq!(ignored_fixture::TEXT, \"first\");\n/// assert_eq!(ignored_fixture::BYTES, b\"first\");\n/// ```\npub const TEXT: &str=include_str!(\"../ignored-data.txt\");\npub const BYTES: &[u8]=include_bytes!(\"../ignored-data.txt\");\n").unwrap();
        let mut cargo = Command::new("cargo");
        super::super::compiler_env::apply_checked(&mut cargo).unwrap();
        cargo.current_dir(&f.root);
        for key in [
            "CARGO_TARGET_DIR",
            "CARGO_BUILD_TARGET_DIR",
            "CARGO_BUILD_BUILD_DIR",
        ] {
            cargo.env(key, &f.target);
        }
        assert!(cargo
            .args(["generate-lockfile", "--offline"])
            .output()
            .unwrap()
            .status
            .success());
        for args in [
            vec!["init", "-q"],
            vec!["add", "."],
            vec![
                "-c",
                "user.name=fixture",
                "-c",
                "user.email=fixture@localhost",
                "-c",
                "core.hooksPath=",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-qm",
                "ignored fixture",
            ],
        ] {
            assert!(Command::new("git")
                .current_dir(&f.root)
                .args(args)
                .output()
                .unwrap()
                .status
                .success());
        }
        let mut docs = Command::new("cargo");
        super::super::compiler_env::apply_checked(&mut docs).unwrap();
        docs.current_dir(&f.root)
            .args(["test", "--doc", "--workspace", "--offline", "--locked"])
            .env("CARGO_BUILD_JOBS", "4");
        for key in [
            "CARGO_TARGET_DIR",
            "CARGO_BUILD_TARGET_DIR",
            "CARGO_BUILD_BUILD_DIR",
        ] {
            docs.env(key, &f.target);
        }
        assert!(docs.output().unwrap().status.success());
        assert!(require_supported_source_inputs().is_err());
        fs::write(f.root.join("ignored-data.txt"), b"second").unwrap();
        assert!(Command::new("git")
            .current_dir(&f.root)
            .args(["diff", "--quiet"])
            .status()
            .unwrap()
            .success());
        assert!(!docs.output().unwrap().status.success());
        // Neither unchanged Git state nor passing binaries authorizes a claim
        // covering ignored doctest inputs. No ignored contents are retained.
        assert!(require_supported_source_inputs().is_err());
    }

    #[test]
    #[cfg(windows)]
    fn real_cargo_inventory_runtime_docs_and_relocated_history() {
        let f = Fixture::new();
        fs::write(f.root.join("Cargo.toml"), "[package]\nname=\"fixture\"\nversion=\"0.1.0\"\nedition=\"2021\"\n[workspace]\n[features]\na=[]\nb=[]\n").unwrap();
        fs::write(
            f.root.join(".gitignore"),
            "target/\nhomes/\n*.log\ndurations.json\n",
        )
        .unwrap();
        fs::write(
            f.root.join("build.rs"),
            "fn main(){println!(\"cargo:rustc-env=FIXTURE_RUNTIME=from-build\");}",
        )
        .unwrap();
        fs::write(f.root.join("src/lib.rs"), r#"
/// ```
/// assert_eq!(fixture::value(), 3);
/// ```
pub fn value() -> u32 { 3 }
#[test] fn unit() {
    assert!(cfg!(feature="a") && cfg!(feature="b"));
    assert_eq!(std::env::var("FIXTURE_RUNTIME").unwrap(), "from-build");
    assert_eq!(std::env::var("CARGO_PKG_NAME").unwrap(), "fixture");
    let home = std::path::PathBuf::from(std::env::var_os("HOME").unwrap());
    assert_eq!(std::path::PathBuf::from(std::env::var_os("CARGO_TARGET_DIR").unwrap()), home.join("target"));
    assert_eq!(std::path::PathBuf::from(std::env::var_os("CARGO").unwrap()), std::path::PathBuf::from(std::env::var_os("FIXTURE_EXPECTED_CARGO").unwrap()));
    assert_eq!(std::env::split_paths(&std::env::var_os("PATH").unwrap()).next().unwrap(), std::path::PathBuf::from(std::env::var_os("FIXTURE_OUTER_DEPS").unwrap()));
    assert!(std::env::var("RUST_TEST_THREADS").unwrap().parse::<usize>().unwrap() <= 4);
}
"#).unwrap();
        fs::create_dir_all(f.root.join("tests")).unwrap();
        fs::write(
            f.root.join("tests/integration.rs"),
            "#[test] fn integration(){assert_eq!(fixture::value(),3);}",
        )
        .unwrap();
        let mut generate = Command::new("cargo");
        super::super::compiler_env::apply_checked(&mut generate).unwrap();
        assert!(generate
            .current_dir(&f.root)
            .args(["generate-lockfile", "--offline"])
            .output()
            .unwrap()
            .status
            .success());
        for args in [
            vec!["init", "-q"],
            vec!["add", "."],
            vec![
                "-c",
                "user.name=fixture",
                "-c",
                "user.email=fixture@localhost",
                "-c",
                "core.hooksPath=",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-qm",
                "fixture",
            ],
        ] {
            assert!(Command::new("git")
                .current_dir(&f.root)
                .args(args)
                .output()
                .unwrap()
                .status
                .success());
        }
        let logical = vec![
            "cargo".into(),
            "test".into(),
            "--workspace".into(),
            "--offline".into(),
            "--locked".into(),
            "--target-dir".into(),
            f.target.to_str().unwrap().into(),
            "--features".into(),
            "a,b".into(),
        ];
        let argv = build_argv(&logical).unwrap();
        let mut build = Command::new("cargo");
        super::super::compiler_env::apply_checked(&mut build).unwrap();
        build
            .current_dir(&f.root)
            .args(&argv[1..])
            .env("CARGO_BUILD_JOBS", "4");
        for key in [
            "CARGO_TARGET_DIR",
            "CARGO_BUILD_TARGET_DIR",
            "CARGO_BUILD_BUILD_DIR",
        ] {
            build.env(key, &f.target);
        }
        let homes = f.root.join("homes");
        let log = f.root.join("run.log");
        let durations = f.root.join("durations.json");
        fs::write(&log, "").unwrap();
        let settings = super::super::test_runner::Settings {
            concurrency: 2,
            total_test_threads: 4,
            build_jobs: 2,
            home_root: &homes,
            target_dir: &f.target,
            triple: None,
            durations: &durations,
            heavy: &[],
            log: &log,
        };
        let runner =
            RunnerInputs::capture(&settings, include_bytes!("test_binary_evidence.rs")).unwrap();
        let launch = |binary: &Binary, home: &Path| -> Result<Command> {
            let mut command = Command::new(&binary.executable);
            command
                .env("HOME", home)
                .env("USERPROFILE", home)
                .env("CARGO_TARGET_DIR", home.join("target"))
                .env(
                    "FIXTURE_EXPECTED_CARGO",
                    text(&runner.record, "resolved_cargo")?,
                )
                .env("FIXTURE_OUTER_DEPS", f.target.join("debug/deps"));
            Ok(command)
        };
        let retained = Mutex::new(None);
        let retain = |plan: &Value, raw: &[u8], metadata: &[u8]| -> Result<()> {
            *retained.lock().unwrap() = Some((plan.clone(), raw.to_vec(), metadata.to_vec()));
            Ok(())
        };
        let execution = execute(
            ExecutionSpec {
                root: &f.root,
                argv: &argv,
                settings: &settings,
                callback_source: include_bytes!("test_binary_evidence.rs"),
            },
            &mut build,
            &launch,
            &retain,
        )
        .unwrap_or_else(|error| panic!("{error}: {}", fs::read_to_string(&log).unwrap()));
        assert_eq!(execution.result["ran"], 2);
        let mut docs = Command::new("cargo");
        super::super::compiler_env::apply_checked(&mut docs).unwrap();
        for key in [
            "CARGO_TARGET_DIR",
            "CARGO_BUILD_TARGET_DIR",
            "CARGO_BUILD_BUILD_DIR",
        ] {
            docs.env(key, &f.target);
        }
        let doc_output = docs
            .current_dir(&f.root)
            .args(&doc_argv(&logical).unwrap()[1..])
            .env("CARGO_BUILD_JOBS", "4")
            .output()
            .unwrap();
        assert!(
            doc_output.status.success(),
            "{}",
            String::from_utf8_lossy(&doc_output.stderr)
        );
        execution.recheck().unwrap();
        let (plan, raw, metadata) = retained.into_inner().unwrap().unwrap();
        let result = execution.result.clone();
        assert_eq!(
            verify_plan(
                RetainedRecords {
                    plan: &plan,
                    result: &result,
                    raw: &raw,
                    metadata_raw: &metadata
                },
                &argv,
                &f.root,
            )
            .unwrap(),
            doc_argv(&logical).unwrap()
        );
        let mut weakened = plan.clone();
        weakened["doc_argv"][9] = json!("a");
        let mut forged_result = result.clone();
        forged_result["plan_sha256"] = json!(digest(&serde_json::to_vec(&weakened).unwrap()));
        assert!(verify_plan(
            RetainedRecords {
                plan: &weakened,
                result: &forged_result,
                raw: &raw,
                metadata_raw: &metadata
            },
            &argv,
            &f.root,
        )
        .is_err());
        for index in 0..4 {
            let mut changed = result.clone();
            match index {
                0 => {
                    changed["executed"].as_array_mut().unwrap().pop();
                }
                1 => {
                    changed["executed"][1] = changed["executed"][0].clone();
                }
                2 => {
                    changed["executed"][0] = json!("unplanned");
                }
                _ => {
                    changed["failed"] = json!(1);
                }
            }
            assert!(verify_plan(
                RetainedRecords {
                    plan: &plan,
                    result: &changed,
                    raw: &raw,
                    metadata_raw: &metadata
                },
                &argv,
                &f.root,
            )
            .is_err());
        }
        fs::write(
            f.root.join("src/lib.rs"),
            "/// ```\n/// assert!(false);\n/// ```\npub fn value()->u32 {3}\n",
        )
        .unwrap();
        assert!(execution.recheck().is_err());
        let failed_docs = docs.output().unwrap();
        assert!(!failed_docs.status.success());
        drop(execution);
        drop(build);
        drop(docs);
        drop(generate);
        let relocated = f.root.with_extension("relocated");
        fs::rename(&f.root, &relocated).unwrap();
        // No old source/target path exists. Historical verification uses only
        // retained bytes and intentionally never calls the legacy live parser.
        assert!(verify_plan(
            RetainedRecords {
                plan: &plan,
                result: &result,
                raw: &raw,
                metadata_raw: &metadata
            },
            &argv,
            &f.root,
        )
        .is_ok());
        fs::remove_dir_all(relocated).unwrap();
    }
}
