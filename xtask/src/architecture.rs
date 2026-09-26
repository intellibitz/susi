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

const VENDORED_TREES: &[(&str, &str)] = &[
    ("crates/susi-core/vendor_template/susi_core", "susi_core"),
    (
        "crates/susi-sandbox/vendor_template/susi_sandbox",
        "susi_sandbox",
    ),
    (
        "crates/susi-native/vendor_template/susi_native",
        "susi_native",
    ),
];

const FLAT_MODULES: &[(&str, &str)] = &[
    ("crates/susi-core/src/susi_error.rs", "susi_error.rs"),
    ("crates/susi-core/src/susi_paths.rs", "susi_paths.rs"),
    ("crates/susi-core/src/susi_config.rs", "susi_config.rs"),
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
    verify_vendored_sources(&metadata.workspace_root, &mut failures)?;

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

fn verify_vendored_sources(root: &Path, failures: &mut Vec<String>) -> Result<(), String> {
    for (template, module) in VENDORED_TREES {
        let template = root.join(template);
        let template_files = rust_files(&template)?;
        if template_files.is_empty() {
            failures.push(format!(
                "vendored template is empty: {}",
                template.display()
            ));
            continue;
        }

        let mut consumers = 0_usize;
        for crate_dir in crate_directories(root)? {
            let consumer = crate_dir.join("src").join(module);
            if !consumer.is_dir() {
                continue;
            }
            consumers += 1;
            compare_trees(&template, &consumer, failures)?;
        }
        if consumers == 0 {
            failures.push(format!(
                "vendored template has no consumers: {}",
                template.display()
            ));
        }
    }

    compare_matching_files(
        &root.join("crates/susi-core/vendor_template/susi_core"),
        &root.join("crates/susi-core/src"),
        failures,
    )?;

    for (canonical, file_name) in FLAT_MODULES {
        let canonical = root.join(canonical);
        for crate_dir in crate_directories(root)? {
            let consumer = crate_dir.join("src").join(file_name);
            if consumer.is_file() && consumer != canonical {
                compare_files(&canonical, &consumer, failures)?;
            }
        }
    }
    Ok(())
}

fn crate_directories(root: &Path) -> Result<Vec<PathBuf>, String> {
    let crates = root.join("crates");
    let entries = fs::read_dir(&crates)
        .map_err(|error| format!("could not read {}: {error}", crates.display()))?;
    let mut directories = Vec::new();
    for entry in entries {
        let path = entry
            .map_err(|error| format!("could not read {} entry: {error}", crates.display()))?
            .path();
        if path.is_dir() {
            directories.push(path);
        }
    }
    directories.sort();
    Ok(directories)
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

fn compare_trees(
    reference: &Path,
    consumer: &Path,
    failures: &mut Vec<String>,
) -> Result<(), String> {
    let reference_files = rust_files(reference)?;
    let consumer_files = rust_files(consumer)?;
    for missing in reference_files.difference(&consumer_files) {
        failures.push(format!(
            "missing vendored source: {}",
            consumer.join(missing).display()
        ));
    }
    for orphan in consumer_files.difference(&reference_files) {
        failures.push(format!(
            "orphan vendored source: {}",
            consumer.join(orphan).display()
        ));
    }
    for relative in reference_files.intersection(&consumer_files) {
        compare_files(
            &reference.join(relative),
            &consumer.join(relative),
            failures,
        )?;
    }
    Ok(())
}

fn compare_matching_files(
    reference: &Path,
    consumer: &Path,
    failures: &mut Vec<String>,
) -> Result<(), String> {
    let reference_files = rust_files(reference)?;
    let consumer_files = rust_files(consumer)?;
    for relative in reference_files.intersection(&consumer_files) {
        compare_files(
            &reference.join(relative),
            &consumer.join(relative),
            failures,
        )?;
    }
    Ok(())
}

fn compare_files(
    reference: &Path,
    consumer: &Path,
    failures: &mut Vec<String>,
) -> Result<(), String> {
    let reference_bytes = fs::read(reference)
        .map_err(|error| format!("could not read {}: {error}", reference.display()))?;
    let consumer_bytes = fs::read(consumer)
        .map_err(|error| format!("could not read {}: {error}", consumer.display()))?;
    if reference_bytes != consumer_bytes {
        failures.push(format!(
            "vendored source drift: {} != {}",
            consumer.display(),
            reference.display()
        ));
    }
    Ok(())
}
