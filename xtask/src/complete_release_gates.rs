//! Complete release admission. External coordinator pins authorize gate tools and
//! immutable provenance permits; package-provided success claims are not inputs.
use serde_json::{json, Value};
#[path = "native_json.rs"]
mod native_json;
#[path = "native_test_counts.rs"]
mod native_test_counts;
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Stdio,
    time::Instant,
};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
pub const SCHEMA: &str = "susi/complete-release-gates/v1";
const REQUIRED: &[&str] = &[
    "format",
    "default-licenses",
    "native-advisories",
    "all-features-policy",
    "workspace-clippy",
    "workspace-tests",
    "production-build",
    "unsafe-measurement",
    "unsafe-ratchet",
    "native-provenance",
];
const ADAPTERS: &[&str] = &[
    "idna-adapter",
    "unicode-ident",
    "htmlescape",
    "regex-syntax",
    "regex-syntax/os-unicode",
];

fn string<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    super::required(value, field)
}
fn hash(value: &Value, field: &str) -> Result<String> {
    let text = string(value, field)?;
    if !super::hex(text, &[64]) {
        return Err(format!("invalid complete-gate digest: {field}").into());
    }
    Ok(text.to_owned())
}
fn source(root: &Path) -> Result<Value> {
    Ok(
        json!({"commit": super::git(root, &["rev-parse", "HEAD"])? , "tree": super::git(root, &["rev-parse", "HEAD^{tree}"])? , "lock_sha256": super::file_digest(&root.join("Cargo.lock"))?}),
    )
}
fn external(path: &Path, root: &Path) -> Result<PathBuf> {
    let path = path.canonicalize()?;
    if path.starts_with(root.canonicalize()?) {
        return Err("trusted gate input must live outside candidate source".into());
    }
    Ok(path)
}
/// The reviewed unsafe-exposure baseline for one host. A baseline binds its
/// measurement target, so each release host has its own; the original Linux
/// file keeps its name. A host without a reviewed baseline fails the gate.
fn unsafe_baseline(root: &Path, host: &str) -> PathBuf {
    if host == "x86_64-unknown-linux-gnu" {
        root.join(".agents/baseline/unsafe-default.json")
    } else {
        root.join(format!(".agents/baseline/unsafe-default-{host}.json"))
    }
}
fn read_json(path: &Path) -> Result<Value> {
    native_json::from_slice(&fs::read(path)?)
}
fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && !matches!(name, "." | "..")
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}

#[derive(Clone)]
struct Tool {
    path: PathBuf,
    sha256: String,
}
impl Tool {
    fn read(value: &Value, root: &Path) -> Result<Self> {
        let tool = Self {
            path: external(Path::new(string(value, "path")?), root)?,
            sha256: hash(value, "sha256")?,
        };
        tool.recheck()?;
        Ok(tool)
    }
    fn recheck(&self) -> Result<()> {
        if super::file_digest(&self.path)? != self.sha256 {
            return Err("trusted gate executable changed".into());
        }
        Ok(())
    }
}

pub struct Runner {
    registry: PathBuf,
    trusted: Option<(PathBuf, String, Value)>,
    #[cfg(test)]
    fixture: bool,
}
impl Runner {
    pub fn provisioned(root: &Path, registry: Option<&Path>) -> Result<Self> {
        let registry = external(
            registry.ok_or("SUSI_RELEASE_TRUST_REGISTRY is required")?,
            root,
        )?;
        let path = env::var_os("SUSI_RELEASE_GATE_TRUST").map(PathBuf::from).ok_or("SUSI_RELEASE_GATE_TRUST must name the coordinator's immutable source permit/tool manifest")?;
        let path = external(&path, root)?;
        let pin = env::var("SUSI_RELEASE_GATE_TRUST_SHA256")
            .map_err(|_| "SUSI_RELEASE_GATE_TRUST_SHA256 is required")?;
        if !super::hex(&pin, &[64]) || super::file_digest(&path)? != pin {
            return Err("trusted gate manifest does not match its external pin".into());
        }
        let value = read_json(&path)?;
        if string(&value, "schema")? != "susi/release-gate-trust/v1" {
            return Err("unsupported trusted gate manifest".into());
        }
        registered_bytes(&registry, "gate-trust", &pin, &fs::read(&path)?)?;
        Ok(Self {
            registry,
            trusted: Some((path, pin, value)),
            #[cfg(test)]
            fixture: false,
        })
    }
    #[cfg(test)]
    pub fn fixture(registry: &Path) -> Self {
        Self {
            registry: registry.to_owned(),
            trusted: None,
            fixture: true,
        }
    }

    pub fn native(&self, root: &Path, attempt: &Path, host: &str) -> Result<Native> {
        #[cfg(test)]
        if self.fixture {
            return fixture_native(root, attempt, host, &self.registry);
        }
        let (path, pin, trust) = self
            .trusted
            .as_ref()
            .ok_or("missing trusted native gate runner")?;
        if super::file_digest(path)? != *pin {
            return Err("trusted gate manifest changed".into());
        }
        let identity = source(root)?;
        if trust["source"]["commit"] != identity["commit"]
            || trust["source"]["tree"] != identity["tree"]
        {
            return Err("trusted gate manifest names another candidate".into());
        }
        let measure = Tool::read(&trust["unsafe_measure"], root)?;
        let ratchet = Tool::read(&trust["unsafe_ratchet"], root)?;
        let verifier = Tool::read(&trust["provenance"], root)?;
        let policy = hash(&trust["provenance"], "policy_sha256")?;
        let permit_pin = hash(&trust["provenance"], "permit_sha256")?;
        let permit_path = external(
            Path::new(string(&trust["provenance"], "permit_path")?),
            root,
        )?;
        let permit = fs::read(&permit_path)?;
        if super::digest(&permit) != permit_pin {
            return Err("provenance permit pin mismatch".into());
        }
        authorized_permit(
            &self.registry,
            &permit_pin,
            &permit,
            &identity,
            (&verifier.sha256, &policy),
        )?;
        let mut result = Native::new(identity, root, host);
        result.write(attempt, "complete-provenance-permit.json", &permit)?;
        result.write(attempt, "complete-trust.json", &fs::read(path)?)?;
        let measurement = attempt.join("complete-unsafe-measurement.json");
        let receipt = attempt.join("complete-unsafe-receipt.json");
        let baseline = unsafe_baseline(root, host);
        result.write(
            attempt,
            "complete-unsafe-baseline.json",
            &fs::read(&baseline)?,
        )?;
        let args = vec![
            "--root".into(),
            super::text(root)?.into(),
            "--target".into(),
            host.into(),
            "--features".into(),
            "default".into(),
            "--output".into(),
            super::text(&measurement)?.into(),
        ];
        result.execute(attempt, "unsafe-measurement", &measure, &args)?;
        result.retain(attempt, "complete-unsafe-measurement.json")?;
        let args = vec![
            "--measurement".into(),
            super::text(&measurement)?.into(),
            "--baseline".into(),
            super::text(&baseline)?.into(),
            "--receipt".into(),
            super::text(&receipt)?.into(),
        ];
        result.execute(attempt, "unsafe-ratchet", &ratchet, &args)?;
        result.retain(attempt, "complete-unsafe-receipt.json")?;
        let report = attempt.join("complete-provenance.json");
        let args = vec![
            "verify-provenance".into(),
            "--root".into(),
            super::text(root)?.into(),
            "--permit".into(),
            super::text(&permit_path)?.into(),
            "--permit-sha256".into(),
            permit_pin.clone(),
            "--verifier-sha256".into(),
            verifier.sha256.clone(),
            "--policy-sha256".into(),
            policy.clone(),
            "--head".into(),
            string(&result.source, "commit")?.into(),
        ];
        result.execute(attempt, "native-provenance", &verifier, &args)?;
        result.retain(attempt, "complete-provenance.json")?;
        result.trust = json!({"permit_sha256":permit_pin,"permit_path":permit_path,"verifier_sha256":verifier.sha256,"policy_sha256":policy,"manifest_sha256":pin});
        result.unsafe_receipt = read_json(&receipt)?;
        result.provenance = read_json(&report)?;
        result.validate_outputs(attempt)?;
        for tool in [&measure, &ratchet, &verifier] {
            tool.recheck()?;
        }
        if super::file_digest(path)? != *pin
            || super::file_digest(&baseline)?
                != super::file_digest(&attempt.join("complete-unsafe-baseline.json"))?
        {
            return Err("gate inputs changed while measuring candidate".into());
        }
        Ok(result)
    }
}

pub struct Native {
    source: Value,
    root: PathBuf,
    fixture_only: bool,
    host: String,
    records: Vec<Value>,
    files: Vec<Value>,
    trust: Value,
    unsafe_receipt: Value,
    provenance: Value,
}
impl Native {
    fn new(source: Value, root: &Path, host: &str) -> Self {
        Self {
            source,
            root: root.to_owned(),
            fixture_only: false,
            host: host.into(),
            records: Vec::new(),
            files: Vec::new(),
            trust: Value::Null,
            unsafe_receipt: Value::Null,
            provenance: Value::Null,
        }
    }
    fn retain(&mut self, directory: &Path, name: &str) -> Result<()> {
        if !safe_name(name) || self.files.iter().any(|entry| entry["path"] == name) {
            return Err("duplicate or invalid complete-evidence path".into());
        }
        let path = directory.join(name);
        if !fs::symlink_metadata(&path)?.file_type().is_file() {
            return Err("complete gate output is missing or redirected".into());
        }
        self.files
            .push(json!({"path":name,"sha256":super::file_digest(&path)?}));
        Ok(())
    }
    fn write(&mut self, directory: &Path, name: &str, bytes: &[u8]) -> Result<()> {
        fs::write(directory.join(name), bytes)?;
        self.retain(directory, name)
    }
    fn execute(&mut self, attempt: &Path, id: &str, tool: &Tool, args: &[String]) -> Result<()> {
        let name = format!("complete-{id}.log");
        let log = fs::File::create(attempt.join(&name))?;
        let stdout = if id == "native-provenance" {
            fs::File::create(attempt.join("complete-provenance.json"))?
        } else {
            log.try_clone()?
        };
        let root = &self.root;
        let started = Instant::now();
        let status = super::isolated_command(&tool.path, &attempt.join("complete-gate-home"))?
            .current_dir(root)
            .args(args)
            .env("CARGO_TARGET_DIR", root.join("target"))
            .env("CARGO_BUILD_TARGET_DIR", root.join("target"))
            .env("CARGO_BUILD_BUILD_DIR", root.join("target"))
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(log))
            .status()?;
        self.retain(attempt, &name)?;
        self.records.push(json!({"id":id,"argv":args,"tool_sha256":tool.sha256,"exit":status.code(),"elapsed_ms":started.elapsed().as_millis(),"log":name}));
        fs::write(
            attempt.join("complete-native-attempt.json"),
            serde_json::to_vec_pretty(&self.records)?,
        )?;
        if !status.success() {
            return Err(format!(
                "complete release gate {id} failed; evidence {}",
                attempt.display()
            )
            .into());
        }
        Ok(())
    }
    fn validate_outputs(&self, directory: &Path) -> Result<()> {
        let unsafe_report = &self.unsafe_receipt;
        if string(unsafe_report, "schema")? != "susi/unsafe-ratchet/v2"
            || unsafe_report["complete"] != true
            || unsafe_report["verdict"] != "clean"
            || unsafe_report["source_commit"] != self.source["commit"]
            || unsafe_report["source_tree"] != self.source["tree"]
            || unsafe_report["cargo_lock_sha256"] != self.source["lock_sha256"]
            || unsafe_report["target"] != self.host
            || unsafe_report["features"] != "default"
            || unsafe_report["unit"] != "crate-forbid"
            || unsafe_report["unreviewed_exposures"]
                .as_array()
                .is_none_or(|v| !v.is_empty())
            || unsafe_report["measured_packages"].as_u64().unwrap_or(0) == 0
            || unsafe_report["measured_files"].as_u64().unwrap_or(0) == 0
        {
            return Err("unsafe gate receipt is incomplete, stale or for another context".into());
        }
        for (field, name) in [
            ("measurement_sha256", "complete-unsafe-measurement.json"),
            ("baseline_sha256", "complete-unsafe-baseline.json"),
        ] {
            if hash(unsafe_report, field)? != super::file_digest(&directory.join(name))? {
                return Err("unsafe gate receipt substitutes its measured inputs".into());
            }
        }
        for field in [
            "graph_sha256",
            "review_sha256",
            "scanner_sha256",
            "comparator_sha256",
        ] {
            hash(unsafe_report, field)?;
        }
        for (id, field) in [
            ("unsafe-measurement", "scanner_sha256"),
            ("unsafe-ratchet", "comparator_sha256"),
        ] {
            if self
                .records
                .iter()
                .find(|r| r["id"] == id)
                .map(|r| &r["tool_sha256"])
                != Some(&unsafe_report[field])
            {
                return Err("unsafe receipt does not bind the executed tools".into());
            }
        }
        if self.provenance["schema"] != "susi-native-provenance-report-v1"
            || self.provenance["verified"] != true
            || self.provenance["acceptance"] != "forward-revalidated"
            || self.provenance["historical_ownership"] != "incomplete"
            || self.provenance["receipts"]
                .as_array()
                .is_none_or(|items| items.is_empty())
            || self.provenance["source"]["commit"] != self.source["commit"]
            || self.provenance["source"]["tree"] != self.source["tree"]
        {
            return Err("provenance receipt is incomplete or names another candidate".into());
        }
        for key in ["permit_sha256", "verifier_sha256", "policy_sha256"] {
            if self.provenance[key] != self.trust[key] {
                return Err("provenance receipt does not bind trusted authorization".into());
            }
        }
        if self
            .records
            .iter()
            .find(|r| r["id"] == "native-provenance")
            .map(|r| &r["tool_sha256"])
            != Some(&self.trust["verifier_sha256"])
        {
            return Err("provenance receipt substitutes the executed verifier".into());
        }
        Ok(())
    }
}

pub fn gate_id(args: &[String]) -> String {
    if args.first().is_some_and(|a| a == "fmt") {
        "format".into()
    } else if args.first().is_some_and(|a| a == "deny") {
        if args.iter().any(|a| a == "--all-features") {
            "all-features-policy".into()
        } else if args.last().is_some_and(|a| a == "advisories") {
            "native-advisories".into()
        } else {
            "default-licenses".into()
        }
    } else if args.first().is_some_and(|a| a == "clippy") && args.iter().any(|a| a == "--workspace")
    {
        "workspace-clippy".into()
    } else if args.first().is_some_and(|a| a == "test") && args.iter().any(|a| a == "--workspace") {
        "workspace-tests".into()
    } else if args.first().is_some_and(|a| a == "build")
        && args.windows(2).any(|a| a == ["-p", "susi"])
    {
        "production-build".into()
    } else if args.first().is_some_and(|a| a == "build")
        && args.windows(2).any(|a| a == ["-p", "xtask"])
    {
        "tooling-build".into()
    } else if let Some(path) = args
        .windows(2)
        .find(|pair| pair[0] == "--manifest-path")
        .map(|pair| pair[1].as_str())
    {
        if let Some(name) = ADAPTERS
            .iter()
            .find(|name| path == format!("vendor/{name}/Cargo.toml"))
        {
            let kind = if args.first().is_some_and(|a| a == "generate-lockfile") {
                "adapter-lock"
            } else {
                "adapter-tests"
            };
            format!("{kind}/{name}")
        } else {
            "unknown".into()
        }
    } else {
        "unknown".into()
    }
}

pub fn record(
    args: &[String],
    exit: Option<i32>,
    elapsed_ms: u128,
    log: &Path,
    cwd: &Path,
) -> Result<Value> {
    let id = gate_id(args);
    let log_name = log
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("invalid gate log filename")?;
    let tests = if args.first().is_some_and(|a| a == "test") {
        if args
            .iter()
            .any(|a| matches!(a.as_str(), "--list" | "--no-run"))
        {
            return Err("test discovery cannot satisfy release tests".into());
        }
        let mut argv = vec!["cargo".to_owned()];
        argv.extend_from_slice(args);
        let (passed, failed, ignored) =
            native_test_counts::test_counts(&fs::read(log)?, &argv, cwd)?;
        if passed == 0 || failed != 0 {
            return Err("required test gate has no passing tests or has failures".into());
        }
        Some(json!({"passed":passed,"failed":failed,"ignored":ignored}))
    } else {
        None
    };
    Ok(
        json!({"id":id,"argv":args,"exit":exit,"elapsed_ms":elapsed_ms,"tests":tests,"log":format!("complete-{log_name}")}),
    )
}

pub fn all_features_args(candidate: &Path, attempt: &Path) -> Result<Vec<String>> {
    Ok([
        "deny",
        "--config",
        super::text(&attempt.join("advisory-deny.toml"))?,
        "--manifest-path",
        super::text(&candidate.join("Cargo.toml"))?,
        "--offline",
        "--locked",
        "--all-features",
        "check",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect())
}

pub fn finish(
    mut native: Native,
    records: Vec<Value>,
    attempt: &Path,
    manifest: &super::Manifest,
) -> Result<Value> {
    for record in &records {
        let name = string(record, "log")?;
        let index = name
            .strip_prefix("complete-gate-")
            .and_then(|n| n.strip_suffix(".log"))
            .ok_or("invalid gate log name")?;
        native.write(
            attempt,
            name,
            &fs::read(attempt.join(format!("gate-{index}.log")))?,
        )?;
    }
    native.records.extend(records);
    native.retain(attempt, "advisory-receipt.json")?;
    let receipt = json!({"schema":SCHEMA,"policy":super::QUALIFICATION_POLICY.trim(),"source":native.source,"candidate_root":native.root,"attempt_dir":attempt,"fixture_only":native.fixture_only,"host":native.host,"artifact_sha256":manifest.artifact,"artifact_bytes":manifest.bytes,"integration_receipt":manifest.integration,"compiler_sha256":manifest.compiler,"gates":native.records,"files":native.files,"trust":native.trust,"unsafe_receipt":native.unsafe_receipt,"provenance":native.provenance});
    validate_shape(&receipt, manifest)?;
    Ok(receipt)
}

fn authorized_permit(
    registry: &Path,
    pin: &str,
    bytes: &[u8],
    source: &Value,
    authorization: (&str, &str),
) -> Result<()> {
    if !super::hex(pin, &[64]) || super::digest(bytes) != pin {
        return Err("invalid immutable provenance permit".into());
    }
    registered_bytes(registry, "permits", pin, bytes)?;
    let permit: Value = native_json::from_slice(bytes)?;
    if permit["schema"] != "susi-native-provenance-permit-v1"
        || permit["source"]["commit"] != source["commit"]
        || permit["source"]["tree"] != source["tree"]
        || permit["verifier_sha256"] != authorization.0
        || permit["policy_sha256"] != authorization.1
    {
        return Err("registered provenance permit has wrong source or verifier policy".into());
    }
    Ok(())
}

fn registered_bytes(registry: &Path, kind: &str, pin: &str, bytes: &[u8]) -> Result<()> {
    if !super::hex(pin, &[64]) || super::digest(bytes) != pin {
        return Err("invalid trusted registry object digest".into());
    }
    let path = registry.join(kind).join(format!("{pin}.json"));
    if !fs::symlink_metadata(&path)?.file_type().is_file() || fs::read(path)? != bytes {
        return Err("immutable authorization is absent from trusted coordinator registry".into());
    }
    Ok(())
}

fn expected_ids(fixture: bool) -> Result<std::collections::BTreeSet<String>> {
    if fixture && !cfg!(test) {
        return Err("test-only gate evidence cannot qualify a production release".into());
    }
    let mut ids: std::collections::BTreeSet<String> =
        REQUIRED.iter().map(|id| (*id).into()).collect();
    if !fixture {
        ids.insert("tooling-build".into());
        for name in ADAPTERS {
            ids.insert(format!("adapter-lock/{name}"));
            ids.insert(format!("adapter-tests/{name}"));
        }
    }
    Ok(ids)
}

fn expected_argv(id: &str, receipt: &Value) -> Result<Vec<String>> {
    let root = Path::new(string(receipt, "candidate_root")?);
    let attempt = Path::new(string(receipt, "attempt_dir")?);
    if !root.is_absolute() || !attempt.is_absolute() {
        return Err("gate source/attempt paths must be absolute".into());
    }
    let host = string(receipt, "host")?;
    let target = root.join("target");
    let product = receipt["fixture_only"] != true;
    let words = |values: &[&str]| {
        values
            .iter()
            .map(|v| (*v).to_owned())
            .collect::<Vec<String>>()
    };
    let mut args = match id {
        "format" => words(&["fmt", "--all", "--check"]),
        "default-licenses" => words(&["deny", "--offline", "--locked", "check", "licenses"]),
        "native-advisories" => {
            if product {
                super::advisory_evidence::qualified_arguments(root, attempt, host)?
            } else {
                super::advisory_evidence::arguments(root, attempt, host)?
            }
        }
        "all-features-policy" => all_features_args(root, attempt)?,
        "workspace-clippy" => words(&[
            "clippy",
            "--workspace",
            "--all-targets",
            "--offline",
            "--locked",
            "--target",
            host,
            "--target-dir",
            super::text(&target)?,
        ]),
        "workspace-tests" => words(&[
            "test",
            "--workspace",
            "--offline",
            "--locked",
            "--target",
            host,
            "--target-dir",
            super::text(&target)?,
        ]),
        "production-build" => words(&[
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
            super::text(&target)?,
        ]),
        "tooling-build" => words(&[
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
            super::text(&target)?,
        ]),
        "unsafe-measurement" => words(&[
            "--root",
            super::text(root)?,
            "--target",
            host,
            "--features",
            "default",
            "--output",
            super::text(&attempt.join("complete-unsafe-measurement.json"))?,
        ]),
        "unsafe-ratchet" => words(&[
            "--measurement",
            super::text(&attempt.join("complete-unsafe-measurement.json"))?,
            "--baseline",
            super::text(&unsafe_baseline(root, host))?,
            "--receipt",
            super::text(&attempt.join("complete-unsafe-receipt.json"))?,
        ]),
        "native-provenance" => words(&[
            "verify-provenance",
            "--root",
            super::text(root)?,
            "--permit",
            string(&receipt["trust"], "permit_path")?,
            "--permit-sha256",
            string(&receipt["trust"], "permit_sha256")?,
            "--verifier-sha256",
            string(&receipt["trust"], "verifier_sha256")?,
            "--policy-sha256",
            string(&receipt["trust"], "policy_sha256")?,
            "--head",
            string(&receipt["source"], "commit")?,
        ]),
        _ => {
            let (kind, name) = id.split_once('/').ok_or("unknown gate policy ID")?;
            if !ADAPTERS.contains(&name) || !matches!(kind, "adapter-lock" | "adapter-tests") {
                return Err("unknown adapter gate policy".into());
            }
            let manifest = format!("vendor/{name}/Cargo.toml");
            let patch_path = super::text(&root.join("vendor/idna-adapter"))?
                .replace('\\', "/")
                .replace('"', "\\\"");
            let patch = format!("patch.crates-io.idna_adapter.path=\"{patch_path}\"");
            if kind == "adapter-lock" {
                words(&[
                    "generate-lockfile",
                    "--offline",
                    "--manifest-path",
                    &manifest,
                    "--config",
                    &patch,
                ])
            } else {
                words(&[
                    "test",
                    "--offline",
                    "--locked",
                    "--manifest-path",
                    &manifest,
                    "--config",
                    &patch,
                    "--target",
                    host,
                    "--target-dir",
                    super::text(&target.join("platform-adapters").join(name))?,
                ])
            }
        }
    };
    if id == "production-build" {
        if product {
            super::product_features::select_qualified_product_features(&mut args);
        } else {
            super::product_features::select_product_features(&mut args, root);
        }
    }
    if product && matches!(id, "workspace-clippy" | "workspace-tests") {
        args.extend(words(&["--features", "xtask/release-tooling"]));
    }
    if id == "workspace-clippy" {
        args.extend(words(&["--", "-D", "warnings"]));
    }
    Ok(args)
}

fn validate_shape(receipt: &Value, manifest: &super::Manifest) -> Result<()> {
    if receipt["schema"] != SCHEMA
        || receipt["policy"] != super::QUALIFICATION_POLICY.trim()
        || receipt["source"]["commit"] != manifest.source
        || receipt["source"]["tree"] != manifest.tree
        || receipt["source"]["lock_sha256"] != manifest.lock
        || receipt["host"] != manifest.host
        || receipt["artifact_sha256"] != manifest.artifact
        || receipt["artifact_bytes"] != manifest.bytes
        || receipt["integration_receipt"] != manifest.integration
        || receipt["compiler_sha256"] != manifest.compiler
    {
        return Err("complete gate receipt is stale or for another release context".into());
    }
    let gates = receipt["gates"]
        .as_array()
        .ok_or("missing complete gate records")?;
    let expected = expected_ids(receipt["fixture_only"] == true)?;
    let mut ids = std::collections::BTreeSet::new();
    for gate in gates {
        let id = string(gate, "id")?;
        if !ids.insert(id)
            || !expected.contains(id)
            || gate["exit"] != 0
            || gate["elapsed_ms"].as_u64().is_none()
            || gate["argv"].as_array().is_none_or(|args| args.is_empty())
        {
            return Err("missing, duplicate, unknown or failed complete gate".into());
        }
        if gate["argv"] != json!(expected_argv(id, receipt)?) {
            return Err(format!("substituted command for required gate {id}").into());
        }
        if gate["argv"][0] == "test"
            && (gate["tests"]["passed"].as_u64().unwrap_or(0) == 0
                || gate["tests"]["failed"] != 0
                || gate["tests"]["ignored"].as_u64().is_none())
        {
            return Err("required tests have no executed passes or have failures".into());
        }
        if gate["argv"]
            .as_array()
            .is_some_and(|args| args.iter().any(|a| a == "--list" || a == "--no-run"))
        {
            return Err("test discovery cannot satisfy a complete release gate".into());
        }
    }
    if ids.len() != expected.len() || expected.iter().any(|id| !ids.contains(id.as_str())) {
        return Err("release lacks required complete gates; requalification is required".into());
    }
    let files = receipt["files"]
        .as_array()
        .ok_or("missing complete evidence files")?;
    let mut names = std::collections::BTreeSet::new();
    for file in files {
        let name = string(file, "path")?;
        if !safe_name(name) || !names.insert(name) {
            return Err("duplicate or unsafe complete evidence file".into());
        }
        hash(file, "sha256")?;
    }
    for gate in gates {
        if !names.contains(string(gate, "log")?) {
            return Err("gate lacks its raw execution log".into());
        }
    }
    for name in [
        "complete-provenance-permit.json",
        "complete-provenance.json",
        "complete-unsafe-measurement.json",
        "complete-unsafe-receipt.json",
        "complete-unsafe-baseline.json",
        "advisory-receipt.json",
        "complete-trust.json",
    ] {
        if !names.contains(name) {
            return Err("required raw gate evidence is missing".into());
        }
    }
    Ok(())
}

pub fn copy_evidence(receipt: &Value, attempt: &Path, package: &Path) -> Result<()> {
    for file in receipt["files"]
        .as_array()
        .ok_or("missing complete evidence files")?
    {
        let name = string(file, "path")?;
        if !safe_name(name) || super::file_digest(&attempt.join(name))? != hash(file, "sha256")? {
            return Err("complete gate evidence changed before packaging".into());
        }
        fs::copy(attempt.join(name), package.join(name))?;
        let mut permissions = fs::metadata(package.join(name))?.permissions();
        permissions.set_readonly(true);
        fs::set_permissions(package.join(name), permissions)?;
    }
    Ok(())
}

pub fn validate_package(
    package: &Path,
    manifest: &super::Manifest,
    registry: Option<&Path>,
) -> Result<()> {
    let pin = manifest
        .complete_gates
        .as_deref()
        .ok_or("release lacks complete gate admission; requalification is required")?;
    let path = package.join("complete-gates.json");
    if !fs::symlink_metadata(&path)?.file_type().is_file() || super::file_digest(&path)? != pin {
        return Err("complete release receipt was modified".into());
    }
    let receipt = read_json(&path)?;
    validate_shape(&receipt, manifest)?;
    for file in receipt["files"]
        .as_array()
        .ok_or("missing complete evidence files")?
    {
        let name = string(file, "path")?;
        let path = package.join(name);
        if !fs::symlink_metadata(&path)?.file_type().is_file()
            || super::file_digest(&path)? != hash(file, "sha256")?
        {
            return Err("complete gate log or input/output evidence was modified".into());
        }
    }
    for gate in receipt["gates"].as_array().ok_or("missing gate records")? {
        if gate["argv"][0] == "test" {
            let mut argv = vec!["cargo".to_owned()];
            for arg in gate["argv"].as_array().ok_or("invalid test argv")? {
                argv.push(arg.as_str().ok_or("invalid test argument")?.to_owned());
            }
            let (passed, failed, ignored) = native_test_counts::test_counts(
                &fs::read(package.join(string(gate, "log")?))?,
                &argv,
                Path::new(string(&receipt, "candidate_root")?),
            )?;
            if gate["tests"] != json!({"passed":passed,"failed":failed,"ignored":ignored}) {
                return Err(
                    "test receipt counts do not match the retained outer harness results".into(),
                );
            }
        }
    }
    let native = Native {
        source: receipt["source"].clone(),
        root: PathBuf::from(string(&receipt, "candidate_root")?),
        fixture_only: receipt["fixture_only"] == true,
        host: manifest.host.clone(),
        records: receipt["gates"]
            .as_array()
            .ok_or("missing gate records")?
            .clone(),
        files: Vec::new(),
        trust: receipt["trust"].clone(),
        unsafe_receipt: receipt["unsafe_receipt"].clone(),
        provenance: receipt["provenance"].clone(),
    };
    native.validate_outputs(package)?;
    let advisory = read_json(&package.join("advisory-receipt.json"))?;
    if advisory["schema"] != "susi/production-advisories/v1" {
        return Err("unsupported production advisory evidence".into());
    }
    let scanner = hash(&advisory, "cargo_deny_sha256")?;
    for id in ["native-advisories", "all-features-policy"] {
        if native
            .records
            .iter()
            .find(|record| record["id"] == id)
            .map(|record| &record["tool_sha256"])
            != Some(&json!(scanner))
        {
            return Err("dependency gate did not use the pinned advisory scanner".into());
        }
    }
    if read_json(&package.join("complete-unsafe-receipt.json"))? != native.unsafe_receipt
        || read_json(&package.join("complete-provenance.json"))? != native.provenance
    {
        return Err("complete receipt substitutes native gate output".into());
    }
    let registry = registry.ok_or("trusted coordinator registry is required for admission")?;
    let trust_bytes = fs::read(package.join("complete-trust.json"))?;
    registered_bytes(
        registry,
        "gate-trust",
        &hash(&receipt["trust"], "manifest_sha256")?,
        &trust_bytes,
    )?;
    let trust = native_json::from_slice(&trust_bytes)?;
    if trust["schema"] != "susi/release-gate-trust/v1"
        || trust["source"]["commit"] != manifest.source
        || trust["source"]["tree"] != manifest.tree
        || trust["unsafe_measure"]["sha256"] != native.unsafe_receipt["scanner_sha256"]
        || trust["unsafe_ratchet"]["sha256"] != native.unsafe_receipt["comparator_sha256"]
        || trust["provenance"]["sha256"] != receipt["trust"]["verifier_sha256"]
        || trust["provenance"]["policy_sha256"] != receipt["trust"]["policy_sha256"]
        || trust["provenance"]["permit_sha256"] != receipt["trust"]["permit_sha256"]
    {
        return Err("complete receipt substitutes trusted gate tool authorization".into());
    }
    authorized_permit(
        registry,
        &hash(&receipt["trust"], "permit_sha256")?,
        &fs::read(package.join("complete-provenance-permit.json"))?,
        &receipt["source"],
        (
            &hash(&receipt["trust"], "verifier_sha256")?,
            &hash(&receipt["trust"], "policy_sha256")?,
        ),
    )
}

#[cfg(test)]
pub fn test_read_receipt(package: &Path) -> Result<Value> {
    read_json(&package.join("complete-gates.json"))
}

// This fixture substitutes only the new external native-tool boundary. Existing
// release fixtures still execute real Cargo gates, package hashing and admission.
// It is compiled only into tests and cannot be selected by a production option.
#[cfg(test)]
fn fixture_native(root: &Path, attempt: &Path, host: &str, registry: &Path) -> Result<Native> {
    let identity = source(root)?;
    let mut native = Native::new(identity.clone(), root, host);
    native.fixture_only = true;
    let tool = "1".repeat(64);
    let policy = "2".repeat(64);
    let permit = json!({"schema":"susi-native-provenance-permit-v1","source":identity,"verifier_sha256":tool,"policy_sha256":policy});
    let bytes = serde_json::to_vec(&permit)?;
    let pin = super::digest(&bytes);
    fs::create_dir_all(registry.join("permits"))?;
    fs::write(registry.join("permits").join(format!("{pin}.json")), &bytes)?;
    let permit_path = registry.join("permits").join(format!("{pin}.json"));
    let trust = json!({"schema":"susi/release-gate-trust/v1","source":identity,"unsafe_measure":{"path":"test-only-scanner","sha256":tool},"unsafe_ratchet":{"path":"test-only-comparator","sha256":tool},"provenance":{"path":"test-only-verifier","sha256":tool,"policy_sha256":policy,"permit_path":permit_path,"permit_sha256":pin}});
    let trust_bytes = serde_json::to_vec(&trust)?;
    let trust_pin = super::digest(&trust_bytes);
    fs::create_dir_all(registry.join("gate-trust"))?;
    fs::write(
        registry
            .join("gate-trust")
            .join(format!("{trust_pin}.json")),
        &trust_bytes,
    )?;
    native.write(attempt, "complete-trust.json", &trust_bytes)?;
    native.write(attempt, "complete-provenance-permit.json", &bytes)?;
    native.write(
        attempt,
        "complete-unsafe-baseline.json",
        b"test-only reviewed baseline",
    )?;
    native.write(
        attempt,
        "complete-unsafe-measurement.json",
        b"test-only complete measurement",
    )?;
    native.trust = json!({"permit_sha256":pin,"permit_path":permit_path,"verifier_sha256":tool,"policy_sha256":policy,"manifest_sha256":trust_pin});
    native.unsafe_receipt = json!({"schema":"susi/unsafe-ratchet/v2","complete":true,"verdict":"clean","source_commit":identity["commit"],"source_tree":identity["tree"],"cargo_lock_sha256":identity["lock_sha256"],"target":host,"features":"default","unit":"crate-forbid","unreviewed_exposures":[],"measured_packages":1,"measured_files":1,"graph_sha256":"4".repeat(64),"review_sha256":"5".repeat(64),"scanner_sha256":tool,"comparator_sha256":tool,"measurement_sha256":super::file_digest(&attempt.join("complete-unsafe-measurement.json"))?,"baseline_sha256":super::file_digest(&attempt.join("complete-unsafe-baseline.json"))?});
    native.provenance = json!({"schema":"susi-native-provenance-report-v1","verified":true,"acceptance":"forward-revalidated","historical_ownership":"incomplete","receipts":[{"fixture_only":true}],"source":identity,"permit_sha256":pin,"verifier_sha256":tool,"policy_sha256":policy});
    native.write(
        attempt,
        "complete-unsafe-receipt.json",
        &serde_json::to_vec(&native.unsafe_receipt)?,
    )?;
    native.write(
        attempt,
        "complete-provenance.json",
        &serde_json::to_vec(&native.provenance)?,
    )?;
    for id in ["unsafe-measurement", "unsafe-ratchet", "native-provenance"] {
        let name = format!("complete-{id}.log");
        native.write(
            attempt,
            &name,
            b"test-only external gate boundary; not product qualification",
        )?;
        let context = json!({"candidate_root":root,"attempt_dir":attempt,"host":host,"source":identity,"trust":native.trust,"fixture_only":true});
        native.records.push(json!({"id":id,"argv":expected_argv(id,&context)?,"tool_sha256":tool,"exit":0,"elapsed_ms":0,"log":name}));
    }
    native.validate_outputs(attempt)?;
    Ok(native)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn manifest() -> super::super::Manifest {
        super::super::Manifest {
            source: "a".repeat(40),
            tree: "b".repeat(40),
            artifact: "c".repeat(64),
            bytes: 10,
            host: "x86_64-unknown-linux-gnu".into(),
            integration: "d".repeat(40),
            lock: "e".repeat(64),
            compiler: "f".repeat(64),
            gates: "1".repeat(64),
            sbom: "2".repeat(64),
            notices: "3".repeat(64),
            native_audit: "4".repeat(64),
            complete_gates: Some("5".repeat(64)),
        }
    }
    fn shape(manifest: &super::super::Manifest) -> Value {
        let root = env::temp_dir().join("susi-complete-shape");
        let mut receipt = json!({"schema":SCHEMA,"policy":super::super::QUALIFICATION_POLICY.trim(),"source":{"commit":manifest.source,"tree":manifest.tree,"lock_sha256":manifest.lock},"candidate_root":root,"attempt_dir":env::temp_dir().join("susi-complete-attempt"),"fixture_only":true,"trust":{"permit_path":env::temp_dir().join("permit.json"),"permit_sha256":"1".repeat(64),"verifier_sha256":"2".repeat(64),"policy_sha256":"3".repeat(64)},"host":manifest.host,"artifact_sha256":manifest.artifact,"artifact_bytes":manifest.bytes,"integration_receipt":manifest.integration,"compiler_sha256":manifest.compiler});
        let gates:Vec<Value>=REQUIRED.iter().enumerate().map(|(i,id)|json!({"id":id,"argv":expected_argv(id,&receipt).unwrap(),"exit":0,"elapsed_ms":1,"tests":{"passed":2,"failed":0,"ignored":0},"log":format!("log-{i}")})).collect();
        let files: Vec<Value> = gates
            .iter()
            .map(|g| json!({"path":g["log"],"sha256":"1".repeat(64)}))
            .chain(
                [
                    "complete-provenance-permit.json",
                    "complete-provenance.json",
                    "complete-unsafe-measurement.json",
                    "complete-unsafe-receipt.json",
                    "complete-unsafe-baseline.json",
                    "advisory-receipt.json",
                    "complete-trust.json",
                ]
                .into_iter()
                .map(|name| json!({"path":name,"sha256":"2".repeat(64)})),
            )
            .collect();
        receipt["gates"] = json!(gates);
        receipt["files"] = json!(files);
        receipt
    }
    #[test]
    fn admission_rejects_missing_duplicate_failed_stale_and_empty_test_receipts() {
        let manifest = manifest();
        let valid = shape(&manifest);
        validate_shape(&valid, &manifest).unwrap();
        let mut missing = valid.clone();
        missing["gates"].as_array_mut().unwrap().pop();
        assert!(validate_shape(&missing, &manifest).is_err());
        let mut duplicate = valid.clone();
        duplicate["gates"]
            .as_array_mut()
            .unwrap()
            .push(valid["gates"][0].clone());
        assert!(validate_shape(&duplicate, &manifest).is_err());
        let mut failed = valid.clone();
        failed["gates"][0]["exit"] = json!(1);
        assert!(validate_shape(&failed, &manifest).is_err());
        let mut signaled = valid.clone();
        signaled["gates"][0]["exit"] = Value::Null;
        assert!(validate_shape(&signaled, &manifest).is_err());
        let mut stale = valid.clone();
        stale["source"]["commit"] = json!("0".repeat(40));
        assert!(validate_shape(&stale, &manifest).is_err());
        let mut empty = valid.clone();
        empty["gates"][5]["tests"]["passed"] = json!(0);
        assert!(validate_shape(&empty, &manifest).is_err());
        let mut listing = valid.clone();
        listing["gates"][5]["argv"] = json!(["test", "--list"]);
        assert!(validate_shape(&listing, &manifest).is_err());
        let mut echo = valid.clone();
        echo["gates"][0]["argv"] = json!(["echo", "success"]);
        assert!(validate_shape(&echo, &manifest).is_err());
        let mut extra = valid.clone();
        extra["gates"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id":"unknown","argv":["true"],"exit":0,"elapsed_ms":1}));
        assert!(validate_shape(&extra, &manifest).is_err());
        let mut traversal = valid.clone();
        traversal["files"][0]["path"] = json!("../log");
        assert!(validate_shape(&traversal, &manifest).is_err());
    }
    #[test]
    fn each_release_host_has_its_own_unsafe_baseline() {
        let root = Path::new("candidate");
        let linux = unsafe_baseline(root, "x86_64-unknown-linux-gnu");
        let windows = unsafe_baseline(root, "x86_64-pc-windows-msvc");
        assert_eq!(linux, root.join(".agents/baseline/unsafe-default.json"));
        assert_eq!(
            windows,
            root.join(".agents/baseline/unsafe-default-x86_64-pc-windows-msvc.json")
        );
        let manifest = manifest();
        let mut receipt = shape(&manifest);
        receipt["host"] = json!("x86_64-pc-windows-msvc");
        let argv = expected_argv("unsafe-ratchet", &receipt).unwrap();
        let baseline = argv[argv.iter().position(|a| a == "--baseline").unwrap() + 1].clone();
        assert!(
            baseline.ends_with("unsafe-default-x86_64-pc-windows-msvc.json"),
            "{baseline}"
        );
    }
    #[test]
    fn legacy_v6_without_complete_receipt_requires_requalification() {
        let mut old = manifest();
        old.complete_gates = None;
        let raw = old.encode();
        assert!(raw.starts_with("susi-local-release-v1"));
        let old = super::super::Manifest::decode(&raw).unwrap();
        let error = validate_package(Path::new("unused"), &old, None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("requalification"), "{error}");
    }
}
