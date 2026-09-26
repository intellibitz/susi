use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const COMPOSITION_ROOTS: &[(&str, &[&str])] = &[
    (
        "susi",
        &[
            "susi-agents",
            "susi-config",
            "susi-core",
            "susi-daemon",
            "susi-error",
            "susi-gawd",
            "susi-gemi",
            "susi-gmcp",
            "susi-native",
            "susi-paths",
            "susi-sandbox",
            "susi-server",
            "susi-tools",
        ],
    ),
    (
        "susi-daemon",
        &[
            "susi-agents",
            "susi-core",
            "susi-gawd",
            "susi-gemi",
            "susi-gmcp",
            "susi-server",
            "susi-tools",
        ],
    ),
];

#[derive(Deserialize)]
struct Metadata {
    packages: Vec<Package>,
    workspace_members: BTreeSet<String>,
    workspace_root: PathBuf,
}

#[derive(Deserialize)]
struct Package {
    id: String,
    name: String,
    dependencies: Vec<Dependency>,
}

#[derive(Deserialize)]
struct Dependency {
    name: String,
    path: Option<PathBuf>,
    kind: Option<String>,
    target: Option<String>,
}

pub fn verify() -> Result<(), String> {
    let metadata = cargo_metadata()?;
    let mut failures = Vec::new();
    verify_dependency_edges(&metadata, &mut failures);
    verify_source_mounts(&metadata.workspace_root, &mut failures)?;

    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("\n"))
    }
}

fn cargo_metadata() -> Result<Metadata, String> {
    let output = Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version", "1", "--locked"])
        .output()
        .map_err(|error| format!("could not run cargo metadata: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "cargo metadata exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("could not decode cargo metadata: {error}"))
}

fn verify_dependency_edges(metadata: &Metadata, failures: &mut Vec<String>) {
    let allowed: BTreeMap<&str, BTreeSet<&str>> = COMPOSITION_ROOTS
        .iter()
        .map(|(package, dependencies)| (*package, dependencies.iter().copied().collect()))
        .collect();

    for package in metadata
        .packages
        .iter()
        .filter(|package| metadata.workspace_members.contains(&package.id))
    {
        let actual: BTreeSet<&str> = package
            .dependencies
            .iter()
            .filter(|dependency| dependency.path.is_some() && dependency.name.starts_with("susi-"))
            .map(|dependency| dependency.name.as_str())
            .collect();
        let expected = allowed
            .get(package.name.as_str())
            .cloned()
            .unwrap_or_default();

        if actual != expected {
            failures.push(format!(
                "Cargo edge violation in {}: expected {expected:?}, found {actual:?}",
                package.name
            ));
        }

        for dependency in package
            .dependencies
            .iter()
            .filter(|dependency| dependency.path.is_some() && dependency.name.starts_with("susi-"))
        {
            if dependency
                .kind
                .as_deref()
                .is_some_and(|kind| kind != "normal")
                || dependency.target.is_some()
            {
                failures.push(format!(
                    "conditional/dev/build SUSI edge is forbidden: {} -> {} (kind={}, target={})",
                    package.name,
                    dependency.name,
                    dependency.kind.as_deref().unwrap_or("normal"),
                    dependency.target.as_deref().unwrap_or("all")
                ));
            }
        }
    }
}

fn verify_source_mounts(root: &Path, failures: &mut Vec<String>) -> Result<(), String> {
    let canonical_mounts = [
        root.join("crates/susi-core/src/embedded.rs"),
        root.join("crates/susi-sandbox/vendor_template/susi_sandbox/mod.rs"),
        root.join("crates/susi-native/vendor_template/susi_native/mod.rs"),
        root.join("crates/susi-core/src/susi_error.rs"),
        root.join("crates/susi-core/src/susi_paths.rs"),
        root.join("crates/susi-core/src/susi_config.rs"),
    ];
    for canonical in canonical_mounts {
        if !canonical.is_file() {
            failures.push(format!(
                "canonical shared source is missing: {}",
                canonical.display()
            ));
        }
    }

    let crates = root.join("crates");
    for relative in rust_files(&crates)? {
        let source = crates.join(&relative);
        let text = fs::read_to_string(&source)
            .map_err(|error| format!("could not read {}: {error}", source.display()))?;
        for line in text.lines() {
            let Some(rest) = line.split_once("#[path = \"").map(|(_, rest)| rest) else {
                continue;
            };
            let Some((mount, _)) = rest.split_once("\"") else {
                failures.push(format!(
                    "malformed source mount in {}: {line}",
                    source.display()
                ));
                continue;
            };
            let Some(parent) = source.parent() else {
                failures.push(format!(
                    "source has no parent directory: {}",
                    source.display()
                ));
                continue;
            };
            let target = parent.join(mount);
            if !target.is_file() {
                failures.push(format!(
                    "source mount in {} points to missing file: {}",
                    source.display(),
                    target.display()
                ));
            }
        }
    }

    for crate_dir in fs::read_dir(&crates)
        .map_err(|error| format!("could not read {}: {error}", crates.display()))?
    {
        let crate_dir = crate_dir
            .map_err(|error| format!("could not read {} entry: {error}", crates.display()))?
            .path();
        let src = crate_dir.join("src");
        if crate_dir
            .file_name()
            .is_some_and(|name| name == "susi-core")
        {
            continue;
        }
        for module in ["susi_core", "susi_sandbox", "susi_native"] {
            let duplicate = src.join(module);
            if duplicate.exists() {
                failures.push(format!(
                    "duplicate shared source tree must be a canonical #[path] mount: {}",
                    duplicate.display()
                ));
            }
        }
        for module in ["susi_error.rs", "susi_paths.rs", "susi_config.rs"] {
            let duplicate = src.join(module);
            if duplicate.exists() {
                failures.push(format!(
                    "duplicate shared source file must be a canonical #[path] mount: {}",
                    duplicate.display()
                ));
            }
        }
    }
    Ok(())
}

fn rust_files(directory: &Path) -> Result<BTreeSet<PathBuf>, String> {
    let mut files = BTreeSet::new();
    collect_rust_files(directory, directory, &mut files)?;
    Ok(files)
}

fn collect_rust_files(
    root: &Path,
    directory: &Path,
    files: &mut BTreeSet<PathBuf>,
) -> Result<(), String> {
    let entries = fs::read_dir(directory)
        .map_err(|error| format!("could not read {}: {error}", directory.display()))?;
    for entry in entries {
        let path = entry
            .map_err(|error| format!("could not read {} entry: {error}", directory.display()))?
            .path();
        if path.is_dir() {
            collect_rust_files(root, &path, files)?;
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            let relative = path
                .strip_prefix(root)
                .map_err(|error| format!("could not relativize {}: {error}", path.display()))?;
            files.insert(relative.to_path_buf());
        }
    }
    Ok(())
}
