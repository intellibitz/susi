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

/// Canonicalize `path` in the spelling `cargo metadata` reports.
///
/// On Windows `canonicalize` returns a verbatim `\\?\C:\...` path while Cargo
/// reports `C:\...`, so comparing the two spellings rejected every real
/// Windows source root ("metadata workspace root does not match source root").
/// The verbatim local-disk prefix is dropped only when the plain spelling
/// canonicalizes back to exactly the same path; anything else (UNC, long or
/// otherwise unrepresentable paths) keeps the verbatim form.
fn canonical(path: &Path) -> Result<PathBuf, String> {
    let path = path.canonicalize().map_err(|e| e.to_string())?;
    #[cfg(windows)]
    {
        use std::path::{Component, Prefix};
        if let Some(Component::Prefix(prefix)) = path.components().next() {
            if let Prefix::VerbatimDisk(_) = prefix.kind() {
                if let Some(plain) = path.to_str().and_then(|text| text.strip_prefix(r"\\?\")) {
                    let plain = PathBuf::from(plain);
                    if plain.canonicalize().ok().as_ref() == Some(&path) {
                        return Ok(plain);
                    }
                }
            }
        }
    }
    Ok(path)
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
    let root = canonical(root)?;
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
    let metadata: Value = crate::strict_json::parse(metadata_text.as_bytes())?;
    let selection_args = selection_args(&metadata, target)?;
    let tree_args: Vec<_> = selection_args.iter().map(String::as_str).collect();
    let selection_tree = command(&root, "cargo", &tree_args)?;
    let expected = expected_packages(&metadata, &root, &selection_tree)?;
    let mut packages = Vec::new();
    for package in &expected {
        let directory = canonical(&package.directory)?;
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
    let measurement: Measurement = crate::strict_json::parse(&bytes)
        .map_err(|e| format!("invalid complete measurement: {e}"))?;
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

/// Deterministic, disk-free Cargo-graph fixture shared by the measurement and
/// inventory suites: a first-party `app` depending on a registry crate `dep`.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
pub(crate) mod fixture {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    pub(crate) const REGISTRY: &str = "registry+https://github.com/rust-lang/crates.io-index";
    pub(crate) const TARGET: &str = "x86_64-pc-windows-msvc";

    pub(crate) fn scratch(tag: &str) -> PathBuf {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "susi-ratchet-{tag}-{}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    pub(crate) fn dep_identity() -> Identity {
        Identity {
            name: "dep".into(),
            version: "1.0.0".into(),
            source: REGISTRY.into(),
        }
    }

    pub(crate) fn metadata(root: &Path) -> Value {
        let app = root.join("app");
        let dep = root.join("reg").join("dep");
        json!({
            "version": 1,
            "workspace_root": root.to_str().unwrap(),
            "workspace_members": ["app"],
            "packages": [
                {
                    "id": "app", "name": "app", "version": "0.1.0", "source": null,
                    "manifest_path": app.join("Cargo.toml").to_str().unwrap(),
                    "targets": [{
                        "kind": ["bin"],
                        "src_path": app.join("src").join("main.rs").to_str().unwrap()
                    }]
                },
                {
                    "id": "dep", "name": "dep", "version": "1.0.0", "source": REGISTRY,
                    "manifest_path": dep.join("Cargo.toml").to_str().unwrap(),
                    "targets": [
                        {"kind": ["lib"], "src_path": dep.join("src").join("lib.rs").to_str().unwrap()},
                        {"kind": ["custom-build"], "src_path": dep.join("build.rs").to_str().unwrap()}
                    ]
                }
            ],
            "resolve": {
                "root": "app",
                "nodes": [
                    {"id": "app", "features": [], "deps": [
                        {"pkg": "dep", "dep_kinds": [{"kind": null, "target": null}]}
                    ]},
                    {"id": "dep", "features": ["std"], "deps": []}
                ]
            }
        })
    }

    pub(crate) fn app_line(root: &Path) -> String {
        format!("0::app v0.1.0 ({})||", root.join("app").display())
    }

    pub(crate) fn tree(root: &Path) -> String {
        format!("{}\n1::dep v1.0.0||std\n", app_line(root))
    }

    fn hex(c: char) -> String {
        c.to_string().repeat(64)
    }

    /// A valid measurement. `commit` and `dep_hash` must be lowercase hex
    /// digits; `dep_unsafe` makes `dep` lack a crate-root forbid declaration.
    pub(crate) fn measurement(
        root: &Path,
        commit: char,
        dep_unsafe: bool,
        dep_hash: char,
    ) -> Measurement {
        let metadata = metadata(root);
        let selection_tree = tree(root);
        let expected = expected_packages(&metadata, root, &selection_tree).unwrap();
        Measurement {
            schema: SCHEMA.into(),
            complete: true,
            source_root: root.to_str().unwrap().into(),
            source_commit: commit.to_string().repeat(40),
            source_tree: "7".repeat(40),
            cargo_lock_sha256: hex('1'),
            target: TARGET.into(),
            features: "default".into(),
            unit: UNIT.into(),
            scanner_sha256: hex('2'),
            scanner_version: "susi-unsafe-measure/test".into(),
            metadata_sha256: json_sha256(&metadata).unwrap(),
            graph_sha256: json_sha256(&expected).unwrap(),
            cargo_version: "cargo 1.0.0".into(),
            selection_args: selection_args(&metadata, TARGET).unwrap(),
            selection_tree_sha256: sha256(selection_tree.as_bytes()),
            selection_tree,
            metadata,
            packages: vec![
                PackageMeasurement {
                    identity: Identity {
                        name: "app".into(),
                        version: "0.1.0".into(),
                        source: "workspace:app/Cargo.toml".into(),
                    },
                    first_party: true,
                    files: vec![FileMeasurement {
                        path: "src/main.rs".into(),
                        sha256: hex('3'),
                        bytes: 10,
                        forbids_unsafe: true,
                    }],
                    forbids_unsafe: true,
                },
                PackageMeasurement {
                    identity: dep_identity(),
                    first_party: false,
                    files: vec![FileMeasurement {
                        path: "src/lib.rs".into(),
                        sha256: hex(dep_hash),
                        bytes: 20,
                        forbids_unsafe: !dep_unsafe,
                    }],
                    forbids_unsafe: !dep_unsafe,
                },
            ],
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::fixture::{self, dep_identity, REGISTRY, TARGET};
    use super::*;
    use serde_json::json;

    #[test]
    fn sha256_matches_published_vectors() {
        assert_eq!(
            sha256(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn valid_sha256_requires_64_lowercase_hex_digits() {
        assert!(valid_sha256(&"a".repeat(64)));
        assert!(valid_sha256(&"0123456789abcdef".repeat(4)));
        assert!(!valid_sha256(&"A".repeat(64)));
        assert!(!valid_sha256(&"a".repeat(63)));
        assert!(!valid_sha256(&"a".repeat(65)));
        assert!(!valid_sha256(&format!("{}g", "a".repeat(63))));
        assert!(!valid_sha256(""));
    }

    #[test]
    fn identity_display_names_package_version_and_source() {
        assert_eq!(
            dep_identity().to_string(),
            format!("dep@1.0.0 ({REGISTRY})")
        );
    }

    #[test]
    fn artifact_digest_binds_the_writers_trailing_newline() {
        let value = json!({"k": [1, 2]});
        let mut expected = serde_json::to_vec_pretty(&value).unwrap();
        expected.push(b'\n');
        assert_eq!(artifact_sha256(&value).unwrap(), sha256(&expected));
        assert_ne!(
            artifact_sha256(&value).unwrap(),
            json_sha256(&value).unwrap()
        );
    }

    #[test]
    fn contained_file_accepts_package_files_and_rejects_escapes() {
        let dir = fixture::scratch("contained");
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src").join("lib.rs"), "// ok").unwrap();
        let outside_name = format!("{}-outside.txt", dir.file_name().unwrap().to_string_lossy());
        let outside = dir.parent().unwrap().join(&outside_name);
        std::fs::write(&outside, "secret").unwrap();

        assert!(contained_file(&dir, "src/lib.rs").is_ok());
        assert!(contained_file(&dir, "src").is_err(), "directories rejected");
        assert!(contained_file(&dir, "src/missing.rs").is_err());
        let escape = contained_file(&dir, &format!("../{outside_name}")).unwrap_err();
        assert!(escape.contains("escapes its package"), "{escape}");

        let _ = std::fs::remove_file(&outside);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn relative_normalizes_separators_and_rejects_unsafe_paths() {
        assert_eq!(
            relative(Path::new("/a/b/c.rs"), Path::new("/a")).unwrap(),
            "b/c.rs"
        );
        let outside = relative(Path::new("/x/y"), Path::new("/a")).unwrap_err();
        assert!(outside.contains("is outside"), "{outside}");
        let empty = relative(Path::new("/a"), Path::new("/a")).unwrap_err();
        assert!(empty.contains("empty relative source path"), "{empty}");
        let parent = relative(Path::new("/a/../b"), Path::new("/a")).unwrap_err();
        assert!(parent.contains("non-normal source path"), "{parent}");
    }

    #[test]
    fn display_matches_distinguishes_registry_path_and_git_sources() {
        let registry = json!({"name": "x", "version": "1.0.0", "source": REGISTRY});
        assert!(display_matches(&registry, "x v1.0.0").unwrap());
        assert!(display_matches(&registry, "x v1.0.0 (proc-macro)").unwrap());
        assert!(!display_matches(&registry, "x v2.0.0").unwrap());
        assert!(!display_matches(&registry, "y v1.0.0").unwrap());

        let path = json!({
            "name": "x", "version": "1.0.0", "source": null,
            "manifest_path": "/w/x/Cargo.toml"
        });
        let dir = Path::new("/w/x/Cargo.toml").parent().unwrap().display();
        assert!(display_matches(&path, &format!("x v1.0.0 ({dir})")).unwrap());
        assert!(!display_matches(&path, "x v1.0.0").unwrap());

        let git =
            json!({"name": "x", "version": "1.0.0", "source": "git+https://example.invalid/x"});
        assert!(
            !display_matches(&git, "x v1.0.0 (https://example.invalid/x)").unwrap(),
            "unsupported source formats fail closed"
        );
    }

    #[test]
    fn selection_args_pin_the_root_target_and_normal_edges() {
        let root = fixture::scratch("args");
        let metadata = fixture::metadata(&root);
        let args = selection_args(&metadata, TARGET).unwrap();
        assert_eq!(args[0], "tree");
        for flag in ["--offline", "--locked"] {
            assert!(args.iter().any(|a| a == flag), "missing {flag}");
        }
        let at = |flag: &str| args.iter().position(|a| a == flag).unwrap();
        assert_eq!(args[at("--target") + 1], TARGET);
        assert_eq!(args[at("--package") + 1], "app");
        assert_eq!(args[at("--edges") + 1], "normal");
        assert!(selection_args(&json!({}), TARGET).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn expected_packages_derives_closure_features_edges_and_entry_points() {
        let root = fixture::scratch("expected");
        let metadata = fixture::metadata(&root);
        let packages = expected_packages(&metadata, &root, &fixture::tree(&root)).unwrap();
        assert_eq!(packages.len(), 2);

        let app = &packages[0];
        assert_eq!(app.identity.name, "app");
        assert_eq!(app.identity.source, "workspace:app/Cargo.toml");
        assert!(app.first_party);
        assert_eq!(app.files, ["src/main.rs"]);
        assert_eq!(app.dependencies, [dep_identity()]);

        let dep = &packages[1];
        assert_eq!(dep.identity, dep_identity());
        assert!(!dep.first_party);
        assert_eq!(dep.features, ["std"]);
        assert_eq!(
            dep.files,
            ["src/lib.rs"],
            "build scripts are not entry points"
        );
        assert!(dep.dependencies.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn expected_packages_fails_closed_on_inconsistent_graph_inputs() {
        let root = fixture::scratch("graph-errors");
        let app = fixture::app_line(&root);

        let mut metadata = fixture::metadata(&root);
        metadata["version"] = json!(2);
        let err = expected_packages(&metadata, &root, &fixture::tree(&root)).unwrap_err();
        assert!(err.contains("unsupported cargo metadata version"), "{err}");

        let other = fixture::scratch("graph-errors-other");
        let err = expected_packages(&fixture::metadata(&root), &other, &fixture::tree(&root))
            .unwrap_err();
        assert!(err.contains("workspace root does not match"), "{err}");

        let metadata = fixture::metadata(&root);
        for (tree, needle) in [
            (
                format!("{app}\n1::dep v1.0.0||std (*)\n"),
                "deduplication omitted",
            ),
            (format!("{app}\n2::dep v1.0.0||std\n"), "skips a parent"),
            (
                format!("{app}\n1::ghost v9.9.9||\n"),
                "missing or ambiguous",
            ),
            (
                format!(
                    "{app}\n1::dep v1.0.0||std\n2::app v0.1.0 ({})||\n",
                    root.join("app").display()
                ),
                "not a metadata normal dependency",
            ),
            (
                format!("{app}\n1::dep v1.0.0||bogus\n"),
                "unknown metadata features",
            ),
            (
                format!("{app}\n1::dep v1.0.0||std,std\n"),
                "duplicate selected feature",
            ),
            (
                format!("{app}\n1::dep v1.0.0\n"),
                "missing Cargo tree feature delimiter",
            ),
            ("garbage".to_string(), "unknown Cargo tree depth format"),
            (String::new(), "empty selected Cargo tree"),
        ] {
            let err = expected_packages(&metadata, &root, &tree).unwrap_err();
            assert!(err.contains(needle), "wanted {needle:?}, got {err}");
        }
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&other);
    }

    #[test]
    fn validate_accepts_the_fixture_and_selects_both_packages() {
        let root = fixture::scratch("validate-ok");
        let measured = fixture::measurement(&root, 'a', true, 'a');
        assert_eq!(validate(&measured).unwrap().len(), 2);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn validate_rejects_each_class_of_tampering() {
        let root = fixture::scratch("validate-bad");
        let good = fixture::measurement(&root, 'a', true, 'a');

        fn rejects(good: &Measurement, needle: &str, mutate: impl FnOnce(&mut Measurement)) {
            let mut tampered = good.clone();
            mutate(&mut tampered);
            let err = validate(&tampered).unwrap_err();
            assert!(err.contains(needle), "wanted {needle:?}, got {err}");
        }
        rejects(&good, "incomplete or incompatible", |m| {
            m.schema = "other".into()
        });
        rejects(&good, "incomplete or incompatible", |m| m.complete = false);
        rejects(&good, "incomplete or incompatible", |m| {
            m.features = "cuda".into()
        });
        rejects(&good, "missing measurement context", |m| {
            m.source_root = "relative".into()
        });
        rejects(&good, "missing measurement context", |m| m.target.clear());
        rejects(&good, "invalid source commit/tree", |m| {
            m.source_commit = "A".repeat(40)
        });
        rejects(&good, "invalid measurement digest", |m| {
            m.cargo_lock_sha256 = "zz".into()
        });
        rejects(&good, "metadata digest mismatch", |m| {
            m.metadata_sha256 = "9".repeat(64)
        });
        rejects(&good, "selected Cargo graph context", |m| {
            m.cargo_version = "rustc".into()
        });
        rejects(&good, "selected Cargo graph context", |m| {
            m.selection_args.push("--extra".into())
        });
        rejects(&good, "selected graph digest mismatch", |m| {
            m.graph_sha256 = "9".repeat(64)
        });
        rejects(&good, "incomplete or excess", |m| {
            m.packages.pop();
        });
        rejects(&good, "duplicate measured package", |m| {
            m.packages[1] = m.packages[0].clone();
        });
        rejects(&good, "first-party identity mismatch", |m| {
            m.packages[1].first_party = true
        });
        rejects(&good, "invalid or duplicate measured file", |m| {
            m.packages[1].files[0].sha256 = "A".repeat(64);
        });
        rejects(&good, "incomplete entry-point measurement", |m| {
            m.packages[1].files[0].path = "src/other.rs".into();
        });
        rejects(&good, "inconsistent package declaration aggregate", |m| {
            m.packages[1].forbids_unsafe = true;
        });
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn write_json_atomic_replaces_content_without_leaving_temporaries() {
        let dir = fixture::scratch("atomic");
        let path = dir.join("nested").join("out.json");
        write_json_atomic(&path, &json!({"k": 1})).unwrap();
        let first = std::fs::read(&path).unwrap();
        assert_eq!(first.last(), Some(&b'\n'));
        write_json_atomic(&path, &json!({"k": 2})).unwrap();
        let second = String::from_utf8(std::fs::read(&path).unwrap()).unwrap();
        assert!(second.contains('2'));
        let entries: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(entries, ["out.json"], "no temp file may remain");
        assert!(write_json_atomic(Path::new(""), &json!({})).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_measurement_round_trips_and_rejects_duplicate_keys() {
        let root = fixture::scratch("read");
        let measured = fixture::measurement(&root, 'a', true, 'a');
        let path = root.join("m.json");
        write_json_atomic(&path, &measured).unwrap();
        let read = read_measurement(&path).unwrap();
        assert_eq!(read.source_commit, measured.source_commit);
        assert_eq!(
            artifact_sha256(&read).unwrap(),
            file_sha256(&path).unwrap(),
            "the artifact digest is the digest of the written file"
        );

        let text = std::fs::read_to_string(&path).unwrap();
        let duplicated = text.replacen('{', "{\"schema\": \"x\",", 1);
        std::fs::write(&path, duplicated).unwrap();
        let err = read_measurement(&path).unwrap_err();
        assert!(err.contains("invalid complete measurement"), "{err}");
        assert!(read_measurement(&root.join("absent.json")).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
}
