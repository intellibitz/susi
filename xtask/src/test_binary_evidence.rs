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
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use super::test_runner::{Binary, Outcome};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
pub const SCHEMA: &str = "susi/cargo-test-binaries/v1";
const BUILD_SUFFIX: [&str; 2] = ["--no-run", "--message-format=json"];

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
        return Err("binary evidence path escaped its isolated directory".into());
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
    inside(target, root)?;
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

/// Verify retained records without depending on the lifetime of target caches.
pub fn verify_plan(
    plan: &Value,
    result: &Value,
    raw: &[u8],
    metadata_raw: &[u8],
    argv: &[String],
    cwd: &Path,
) -> Result<Vec<String>> {
    if plan["schema"] != SCHEMA
        || result["schema"] != SCHEMA
        || strings(plan, "argv")? != argv
        || Path::new(text(plan, "cwd")?) != cwd
        || text(plan, "build_stdout_sha256")? != digest(raw)
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
    pub fn capture(settings: &super::test_runner::Settings<'_>) -> Result<Self> {
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
            "callback_source_sha256":digest(include_bytes!("bin/local-workflow.rs")),
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
}
impl Runtime {
    pub fn parse(raw: &[u8]) -> Result<Self> {
        let mut environments = BTreeMap::new();
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
                    || [
                        "PATH",
                        "HOME",
                        "USERPROFILE",
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
            if environments
                .insert(package.to_owned(), environment)
                .is_some()
            {
                return Err("duplicate build-script package runtime record".into());
            }
        }
        Ok(Self { environments })
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
}
