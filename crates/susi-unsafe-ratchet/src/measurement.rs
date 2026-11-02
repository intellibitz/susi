//! Complete, source-bound measurement of the crate-root forbid declaration.
//! This is not a full-grammar Rust parser or an unsafe-function counter.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const SCHEMA: &str = "susi/unsafe-measurement/v1";
pub const UNIT: &str = "crate-forbid";

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub name: String,
    pub version: String,
    pub source: String,
}

impl std::fmt::Display for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}@{} ({})", self.name, self.version, self.source)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileMeasurement {
    pub path: String,
    pub sha256: String,
    pub bytes: u64,
    pub forbids_unsafe: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageMeasurement {
    pub identity: Identity,
    pub first_party: bool,
    pub files: Vec<FileMeasurement>,
    pub forbids_unsafe: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Measurement {
    pub schema: String,
    pub complete: bool,
    pub source_root: String,
    pub source_commit: String,
    pub source_tree: String,
    pub cargo_lock_sha256: String,
    pub target: String,
    pub features: String,
    pub unit: String,
    pub scanner_sha256: String,
    pub scanner_version: String,
    pub metadata_sha256: String,
    pub graph_sha256: String,
    pub cargo_version: String,
    pub selection_args: Vec<String>,
    pub selection_tree: String,
    pub selection_tree_sha256: String,
    pub metadata: Value,
    pub packages: Vec<PackageMeasurement>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExpectedPackage {
    pub identity: Identity,
    pub first_party: bool,
    pub features: Vec<String>,
    pub dependencies: Vec<Identity>,
    pub files: Vec<String>,
    #[serde(skip)]
    pub directory: PathBuf,
}

pub fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn file_sha256(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    Ok(sha256(&bytes))
}

pub fn contained_file(directory: &Path, relative: &str) -> Result<PathBuf, String> {
    let directory = directory.canonicalize().map_err(|e| e.to_string())?;
    let path = directory
        .join(relative)
        .canonicalize()
        .map_err(|e| e.to_string())?;
    if !path.starts_with(&directory) || !path.is_file() {
        return Err(format!(
            "source entry escapes its package or is not a file: {}",
            path.display()
        ));
    }
    Ok(path)
}

pub fn json_sha256<T: Serialize>(value: &T) -> Result<String, String> {
    serde_json::to_vec(value)
        .map(|bytes| sha256(&bytes))
        .map_err(|e| e.to_string())
}

/// Digest of the exact serialization emitted by the native JSON writer.
pub fn artifact_sha256<T: Serialize>(value: &T) -> Result<String, String> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    bytes.push(b'\n');
    Ok(sha256(&bytes))
}

pub fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

fn object<'a>(value: &'a Value, label: &str) -> Result<&'a serde_json::Map<String, Value>, String> {
    value
        .as_object()
        .ok_or_else(|| format!("{label} must be an object"))
}

fn array<'a>(value: &'a Value, label: &str) -> Result<&'a Vec<Value>, String> {
    value
        .as_array()
        .ok_or_else(|| format!("{label} must be an array"))
}

fn field<'a>(value: &'a Value, key: &str) -> Result<&'a Value, String> {
    object(value, key)?
        .get(key)
        .ok_or_else(|| format!("missing metadata {key}"))
}

fn text<'a>(value: &'a Value, label: &str) -> Result<&'a str, String> {
    value
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("{label} must be a nonempty string"))
}

fn strings(value: &Value, label: &str) -> Result<Vec<String>, String> {
    let mut out = BTreeSet::new();
    for item in array(value, label)? {
        let item = text(item, label)?.to_owned();
        if !out.insert(item) {
            return Err(format!("duplicate {label}"));
        }
    }
    Ok(out.into_iter().collect())
}

fn relative(path: &Path, base: &Path) -> Result<String, String> {
    let relative = path
        .strip_prefix(base)
        .map_err(|_| format!("{} is outside {}", path.display(), base.display()))?;
    if relative
        .components()
        .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return Err(format!("non-normal source path {}", path.display()));
    }
    let value = relative
        .to_str()
        .ok_or("non-UTF8 source path")?
        .replace('\\', "/");
    if value.is_empty() {
        return Err("empty relative source path".into());
    }
    Ok(value)
}

type SelectedTree = (
    BTreeSet<String>,
    BTreeMap<String, BTreeSet<String>>,
    BTreeMap<String, Vec<String>>,
);

fn display_matches(package: &Value, display: &str) -> Result<bool, String> {
    let prefix = format!(
        "{} v{}",
        text(field(package, "name")?, "name")?,
        text(field(package, "version")?, "version")?
    );
    let Some(rest) = display.strip_prefix(&prefix) else {
        return Ok(false);
    };
    let rest = rest.strip_prefix(" (proc-macro)").unwrap_or(rest);
    match field(package, "source")? {
        Value::Null => {
            let directory = Path::new(text(field(package, "manifest_path")?, "manifest")?)
                .parent()
                .ok_or("manifest has no directory")?;
            Ok(rest == format!(" ({})", directory.display()))
        }
        Value::String(source) if source.starts_with("registry+") => Ok(rest.is_empty()),
        // Do not guess abbreviated Git revisions or alternative registry
        // labels. Unsupported/ambiguous Cargo display formats fail closed.
        Value::Bool(_)
        | Value::Number(_)
        | Value::String(_)
        | Value::Array(_)
        | Value::Object(_) => Ok(false),
    }
}

fn selected_tree(
    tree: &str,
    root: &str,
    packages: &BTreeMap<String, &Value>,
    nodes: &BTreeMap<String, &Value>,
    allowed_edges: &BTreeMap<String, BTreeSet<String>>,
) -> Result<SelectedTree, String> {
    let mut selected = BTreeSet::new();
    let mut edges = BTreeMap::<String, BTreeSet<String>>::new();
    let mut features = BTreeMap::<String, BTreeSet<String>>::new();
    let mut expanded = BTreeSet::new();
    let mut stack = Vec::<String>::new();
    for (index, line) in tree.lines().enumerate() {
        let (depth, entry) = line
            .split_once("::")
            .ok_or("unknown Cargo tree depth format")?;
        if depth.is_empty() || !depth.bytes().all(|b| b.is_ascii_digit()) {
            return Err("invalid Cargo tree depth".into());
        }
        let depth: usize = depth.parse().map_err(|_| "Cargo tree depth overflow")?;
        let (display, raw_features) = entry
            .split_once("||")
            .ok_or("missing Cargo tree feature delimiter")?;
        if raw_features.contains("||") {
            return Err("ambiguous Cargo tree feature format".into());
        }
        let (raw_features, duplicate) = match raw_features.strip_suffix(" (*)") {
            Some(value) => (value, true),
            None => (raw_features, false),
        };
        let mut matches = Vec::new();
        for (id, package) in packages {
            if display_matches(package, display)? {
                matches.push(id.clone());
            }
        }
        if matches.len() != 1 {
            return Err(format!(
                "Cargo tree package is missing or ambiguous: {display}"
            ));
        }
        let id = matches.pop().ok_or("tree identity missing")?;
        if !allowed_edges.contains_key(&id) {
            return Err(format!(
                "Cargo tree selected a non-normal or unknown identity: {id}"
            ));
        }
        if index == 0 {
            if depth != 0 || id != root || duplicate {
                return Err("Cargo tree root differs from metadata root".into());
            }
        } else if depth == 0 || depth > stack.len() {
            return Err("Cargo tree skips a parent or repeats its root".into());
        }
        stack.truncate(depth);
        if let Some(parent) = stack.last() {
            if !allowed_edges
                .get(parent)
                .is_some_and(|deps| deps.contains(&id))
            {
                return Err("Cargo tree edge is not a metadata normal dependency".into());
            }
            edges.entry(parent.clone()).or_default().insert(id.clone());
        }
        let values: BTreeSet<String> = if raw_features.is_empty() {
            BTreeSet::new()
        } else {
            let fields: Vec<_> = raw_features.split(',').collect();
            let unique: BTreeSet<_> = fields.iter().copied().collect();
            if fields.len() != unique.len() {
                return Err("duplicate selected feature".into());
            }
            unique.into_iter().map(str::to_owned).collect()
        };
        if values
            .iter()
            .any(|f| f.is_empty() || f.chars().any(char::is_whitespace))
        {
            return Err("invalid selected feature name".into());
        }
        let node = nodes.get(&id).ok_or("tree node absent")?;
        let known: BTreeSet<_> = strings(field(node, "features")?, "metadata features")?
            .into_iter()
            .collect();
        if !values.is_subset(&known) {
            return Err("Cargo tree selected unknown metadata features".into());
        }
        let context = (id.clone(), values.iter().cloned().collect::<Vec<_>>());
        if duplicate && !expanded.contains(&context) {
            return Err("Cargo tree deduplication omitted an unmeasured context".into());
        }
        if !duplicate {
            expanded.insert(context);
        }
        selected.insert(id.clone());
        edges.entry(id.clone()).or_default();
        features.entry(id.clone()).or_default().extend(values);
        stack.push(id);
    }
    if selected.is_empty() {
        return Err("empty selected Cargo tree".into());
    }
    Ok((
        selected,
        edges,
        features
            .into_iter()
            .map(|(id, features)| (id, features.into_iter().collect()))
            .collect(),
    ))
}

pub fn selection_args(metadata: &Value, target: &str) -> Result<Vec<String>, String> {
    let root = text(field(field(metadata, "resolve")?, "root")?, "resolve root")?;
    Ok([
        "tree",
        "--offline",
        "--locked",
        "--color",
        "never",
        "--target",
        target,
        "--package",
        root,
        "--edges",
        "normal",
        "--prefix",
        "depth",
        "--format",
        "::{p}||{f}",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect())
}

/// Derive the exact host-filtered root normal-dependency closure from Cargo's
/// resolve graph. Preserve name/version/source identities and workspace IDs.
pub fn expected_packages(
    metadata: &Value,
    source_root: &Path,
    selection_tree: &str,
) -> Result<Vec<ExpectedPackage>, String> {
    if field(metadata, "version")?.as_u64() != Some(1) {
        return Err("unsupported cargo metadata version".into());
    }
    if Path::new(text(field(metadata, "workspace_root")?, "workspace_root")?) != source_root {
        return Err("metadata workspace root does not match source root".into());
    }
    let members: BTreeSet<_> = strings(field(metadata, "workspace_members")?, "workspace member")?
        .into_iter()
        .collect();
    if members.is_empty() {
        return Err("empty workspace members".into());
    }
    let mut packages = BTreeMap::new();
    let mut identities = BTreeMap::new();
    let mut seen_identities = BTreeSet::new();
    for package in array(field(metadata, "packages")?, "metadata packages")? {
        let id = text(field(package, "id")?, "package id")?.to_string();
        let name = text(field(package, "name")?, "package name")?.to_string();
        let version = text(field(package, "version")?, "package version")?.to_string();
        let manifest = Path::new(text(field(package, "manifest_path")?, "manifest path")?);
        let source = match field(package, "source")? {
            Value::Null => {
                let kind = if members.contains(&id) {
                    "workspace"
                } else {
                    "path"
                };
                format!("{kind}:{}", relative(manifest, source_root)?)
            }
            Value::String(source) if !source.is_empty() && !members.contains(&id) => source.clone(),
            Value::Bool(_)
            | Value::Number(_)
            | Value::String(_)
            | Value::Array(_)
            | Value::Object(_) => {
                return Err(format!(
                    "invalid package source or workspace identity: {id}"
                ))
            }
        };
        let identity = Identity {
            name,
            version,
            source,
        };
        if !seen_identities.insert(identity.clone())
            || packages.insert(id.clone(), package).is_some()
        {
            return Err(format!("duplicate package identity: {id}"));
        }
        identities.insert(id, identity);
    }
    if packages.is_empty() || members.iter().any(|id| !packages.contains_key(id)) {
        return Err("empty packages or unknown workspace member".into());
    }
    let resolve = field(metadata, "resolve")?;
    let root = text(field(resolve, "root")?, "resolve root")?.to_string();
    if !members.contains(&root) {
        return Err("resolve root is not a workspace member".into());
    }
    let mut nodes = BTreeMap::new();
    for node in array(field(resolve, "nodes")?, "resolve nodes")? {
        let id = text(field(node, "id")?, "node id")?.to_string();
        if !packages.contains_key(&id) || nodes.insert(id.clone(), node).is_some() {
            return Err(format!("unknown or duplicate resolve node: {id}"));
        }
    }
    let mut pending = vec![root.clone()];
    let mut selected = BTreeSet::new();
    let mut normal_edges = BTreeMap::new();
    while let Some(id) = pending.pop() {
        if !selected.insert(id.clone()) {
            continue;
        }
        let node = nodes
            .get(&id)
            .ok_or_else(|| format!("missing resolve node: {id}"))?;
        let mut edges = BTreeSet::new();
        for dep in array(field(node, "deps")?, "node deps")? {
            let target = text(field(dep, "pkg")?, "dependency id")?.to_string();
            if !nodes.contains_key(&target) {
                return Err(format!("unknown dependency node: {target}"));
            }
            let kinds = array(field(dep, "dep_kinds")?, "dependency kinds")?;
            if kinds.is_empty() {
                return Err(format!("missing dependency kind: {target}"));
            }
            let mut normal = false;
            for kind in kinds {
                // Cargo --filter-platform has already selected target edges.
                match field(kind, "kind")? {
                    Value::Null => normal = true,
                    Value::String(value) if value == "build" || value == "dev" => {}
                    Value::Bool(_)
                    | Value::Number(_)
                    | Value::String(_)
                    | Value::Array(_)
                    | Value::Object(_) => return Err(format!("unknown dependency kind: {target}")),
                }
                match field(kind, "target")? {
                    Value::Null | Value::String(_) => {}
                    Value::Bool(_) | Value::Number(_) | Value::Array(_) | Value::Object(_) => {
                        return Err("malformed target condition".into())
                    }
                }
            }
            if normal {
                edges.insert(target.clone());
                pending.push(target);
            }
        }
        normal_edges.insert(id, edges);
    }
    // Metadata resolves the workspace and can unify optional features selected
    // only by another member/dev target. Cargo's explicit root tree is the
    // selected build graph; retain its raw output and require unambiguous IDs.
    let (selected, normal_edges, selected_features) =
        selected_tree(selection_tree, &root, &packages, &nodes, &normal_edges)?;
    let mut out = Vec::new();
    for id in selected {
        let package = packages.get(&id).ok_or("selected package absent")?;
        let directory = Path::new(text(field(package, "manifest_path")?, "manifest path")?)
            .parent()
            .ok_or("manifest has no directory")?
            .to_path_buf();
        let mut files = BTreeSet::new();
        for target in array(field(package, "targets")?, "package targets")? {
            let kinds = strings(field(target, "kind")?, "target kinds")?;
            if kinds.is_empty() {
                return Err("empty target kinds".into());
            }
            let selected = kinds.iter().any(|kind| {
                matches!(
                    kind.as_str(),
                    "lib" | "rlib" | "dylib" | "cdylib" | "staticlib" | "proc-macro" | "bin"
                )
            });
            if selected {
                let path = Path::new(text(field(target, "src_path")?, "target source")?);
                files.insert(relative(path, &directory)?);
            } else if kinds
                .iter()
                .any(|kind| !matches!(kind.as_str(), "custom-build" | "test" | "example" | "bench"))
            {
                return Err(format!("unknown target kind in {id}"));
            }
        }
        if files.is_empty() {
            return Err(format!(
                "selected package has no measured entry points: {id}"
            ));
        }
        let dependencies = normal_edges
            .get(&id)
            .ok_or("selected edges absent")?
            .iter()
            .map(|id| {
                identities
                    .get(id)
                    .cloned()
                    .ok_or_else(|| "dependency identity absent".to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        out.push(ExpectedPackage {
            identity: identities.get(&id).cloned().ok_or("identity absent")?,
            first_party: members.contains(&id),
            features: selected_features
                .get(&id)
                .cloned()
                .ok_or("selected features absent")?,
            dependencies,
            files: files.into_iter().collect(),
            directory,
        });
    }
    out.sort_by(|a, b| a.identity.cmp(&b.identity));
    Ok(out)
}

pub fn validate(measurement: &Measurement) -> Result<Vec<ExpectedPackage>, String> {
    if measurement.schema != SCHEMA
        || !measurement.complete
        || measurement.unit != UNIT
        || measurement.features != "default"
    {
        return Err("incomplete or incompatible unsafe measurement schema/unit/features".into());
    }
    if measurement.target.is_empty()
        || measurement.scanner_version.is_empty()
        || !Path::new(&measurement.source_root).is_absolute()
    {
        return Err("missing measurement context".into());
    }
    for value in [&measurement.source_commit, &measurement.source_tree] {
        if value.len() != 40
            || !value
                .bytes()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        {
            return Err("invalid source commit/tree".into());
        }
    }
    for value in [
        &measurement.cargo_lock_sha256,
        &measurement.scanner_sha256,
        &measurement.metadata_sha256,
        &measurement.graph_sha256,
        &measurement.selection_tree_sha256,
    ] {
        if !valid_sha256(value) {
            return Err("invalid measurement digest".into());
        }
    }
    if json_sha256(&measurement.metadata)? != measurement.metadata_sha256 {
        return Err("metadata digest mismatch".into());
    }
    if !measurement.cargo_version.starts_with("cargo ")
        || measurement.selection_args != selection_args(&measurement.metadata, &measurement.target)?
        || sha256(measurement.selection_tree.as_bytes()) != measurement.selection_tree_sha256
    {
        return Err("selected Cargo graph context or digest mismatch".into());
    }
    let expected = expected_packages(
        &measurement.metadata,
        Path::new(&measurement.source_root),
        &measurement.selection_tree,
    )?;
    if json_sha256(&expected)? != measurement.graph_sha256 {
        return Err("selected graph digest mismatch".into());
    }
    if expected.len() != measurement.packages.len() {
        return Err("incomplete or excess measured packages".into());
    }
    let mut seen = BTreeSet::new();
    for package in &measurement.packages {
        if !seen.insert(package.identity.clone()) {
            return Err("duplicate measured package".into());
        }
        let expected = expected
            .iter()
            .find(|item| item.identity == package.identity)
            .ok_or("unselected measured package")?;
        if expected.first_party != package.first_party {
            return Err("first-party identity mismatch".into());
        }
        let mut files = BTreeSet::new();
        for file in &package.files {
            if !valid_sha256(&file.sha256) || !files.insert(file.path.clone()) {
                return Err("invalid or duplicate measured file".into());
            }
        }
        if files != expected.files.iter().cloned().collect() || files.is_empty() {
            return Err(format!(
                "incomplete entry-point measurement: {}",
                package.identity
            ));
        }
        if package.forbids_unsafe != package.files.iter().all(|file| file.forbids_unsafe) {
            return Err("inconsistent package declaration aggregate".into());
        }
    }
    Ok(expected)
}

fn command(root: &Path, program: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(program)
        .args(args)
        .current_dir(root)
        .output()
        .map_err(|e| format!("start {program}: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "{program} {args:?}: {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    String::from_utf8(output.stdout).map_err(|e| e.to_string())
}

fn clean_source(root: &Path) -> Result<(String, String), String> {
    if !command(
        root,
        "git",
        &["status", "--porcelain", "--untracked-files=normal"],
    )?
    .trim()
    .is_empty()
    {
        return Err("unsafe measurement requires a clean committed source tree".into());
    }
    Ok((
        command(root, "git", &["rev-parse", "HEAD"])?.trim().into(),
        command(root, "git", &["rev-parse", "HEAD^{tree}"])?
            .trim()
            .into(),
    ))
}

pub fn scan(root: &Path, target: &str, features: &str) -> Result<Measurement, String> {
    if features != "default" {
        return Err("only the explicit default feature selection is supported".into());
    }
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let compiler = command(&root, "rustc", &["-vV"])?;
    let host = compiler
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .ok_or("rustc did not identify its host")?;
    if host != target {
        return Err("unsafe measurement target must equal the native compiler host".into());
    }
    let (source_commit, source_tree) = clean_source(&root)?;
    let lock_hash = file_sha256(&root.join("Cargo.lock"))?;
    let metadata_text = command(
        &root,
        "cargo",
        &[
            "metadata",
            "--format-version",
            "1",
            "--locked",
            "--offline",
            "--filter-platform",
            target,
        ],
    )?;
    let metadata: Value = serde_json::from_str(&metadata_text).map_err(|e| e.to_string())?;
    let selection_args = selection_args(&metadata, target)?;
    let tree_args: Vec<_> = selection_args.iter().map(String::as_str).collect();
    let selection_tree = command(&root, "cargo", &tree_args)?;
    let expected = expected_packages(&metadata, &root, &selection_tree)?;
    let mut packages = Vec::new();
    for package in &expected {
        let directory = package
            .directory
            .canonicalize()
            .map_err(|e| e.to_string())?;
        if (package.first_party || package.identity.source.starts_with("path:"))
            && !directory.starts_with(&root)
        {
            return Err("path package escapes the clean source checkout".into());
        }
        let mut files = Vec::new();
        for relative in &package.files {
            let path = contained_file(&directory, relative)?;
            let bytes = std::fs::read(&path)
                .map_err(|e| format!("unmeasured entry point {}: {e}", path.display()))?;
            let source = std::str::from_utf8(&bytes)
                .map_err(|e| format!("non-UTF8 entry point {}: {e}", path.display()))?;
            let forbids_unsafe = susi_vendor_syn::crate_forbids_unsafe_checked(source)
                .map_err(|e| format!("unmeasured entry point {}: {e}", path.display()))?;
            let digest = sha256(&bytes);
            if file_sha256(&path)? != digest {
                return Err(format!(
                    "entry point changed during measurement: {}",
                    path.display()
                ));
            }
            files.push(FileMeasurement {
                path: relative.clone(),
                sha256: digest,
                bytes: bytes.len() as u64,
                forbids_unsafe,
            });
        }
        packages.push(PackageMeasurement {
            identity: package.identity.clone(),
            first_party: package.first_party,
            forbids_unsafe: files.iter().all(|file| file.forbids_unsafe),
            files,
        });
    }
    if clean_source(&root)? != (source_commit.clone(), source_tree.clone())
        || file_sha256(&root.join("Cargo.lock"))? != lock_hash
    {
        return Err("source changed during unsafe measurement".into());
    }
    for (context, package) in expected.iter().zip(&packages) {
        for file in &package.files {
            let path = contained_file(&context.directory, &file.path)?;
            if file_sha256(&path)? != file.sha256 {
                return Err(format!(
                    "source changed before measurement completion: {}",
                    path.display()
                ));
            }
        }
    }
    let measurement = Measurement {
        schema: SCHEMA.into(),
        complete: true,
        source_root: root.to_str().ok_or("non-UTF8 source root")?.into(),
        source_commit,
        source_tree,
        cargo_lock_sha256: lock_hash,
        target: target.into(),
        features: features.into(),
        unit: UNIT.into(),
        scanner_sha256: file_sha256(&std::env::current_exe().map_err(|e| e.to_string())?)?,
        scanner_version: format!("susi-unsafe-measure/{}", env!("CARGO_PKG_VERSION")),
        metadata_sha256: json_sha256(&metadata)?,
        graph_sha256: json_sha256(&expected)?,
        cargo_version: command(&root, "cargo", &["--version"])?.trim().into(),
        selection_args,
        selection_tree_sha256: sha256(selection_tree.as_bytes()),
        selection_tree,
        metadata,
        packages,
    };
    validate(&measurement)?;
    Ok(measurement)
}

pub fn read_measurement(path: &Path) -> Result<Measurement, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let measurement: Measurement =
        serde_json::from_slice(&bytes).map_err(|e| format!("invalid complete measurement: {e}"))?;
    validate(&measurement)?;
    Ok(measurement)
}

pub fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    use std::io::Write;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let name = path
        .file_name()
        .ok_or("output path has no filename")?
        .to_string_lossy();
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let temporary = parent.join(format!(".{name}.{}.{nonce}.tmp", std::process::id()));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|e| e.to_string())?;
        let bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
        file.write_all(&bytes)
            .and_then(|()| file.write_all(b"\n"))
            .and_then(|()| file.sync_all())
            .map_err(|e| e.to_string())?;
        std::fs::rename(&temporary, path)
            .map_err(|e| format!("atomic output replacement {}: {e}", path.display()))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    result
}
