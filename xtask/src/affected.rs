//! Select the workspace packages a change can affect, so the local integration
//! gate builds and tests only those instead of the whole workspace.
//!
//! The selection only narrows when it can show the change is local. Anything it
//! cannot attribute to packages (toolchain, lockfile, root manifest, Cargo or
//! nextest configuration, vendored adapters, release tooling) widens it to the
//! whole workspace. A package is also selected when one of its sources names a
//! changed path through a relative string literal (`include_str!`, build-script
//! `rerun-if-changed`, test fixture joins), because such reads carry no Cargo
//! dependency edge. The root package is always selected: its architecture and
//! documentation tests scan the whole tree. Production qualification
//! (`local-release qualify`) keeps the complete workspace gate.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// What the gate must cover.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    /// Every workspace package; the reason names the first widening input.
    Workspace(String),
    /// These package names, never empty (the root package is always present).
    Packages(BTreeSet<String>),
}

/// Paths whose change can alter every package's build or test run.
/// Entries ending in `/` are directory prefixes; the rest are exact paths.
const GLOBAL: &[&str] = &[
    "Cargo.lock",
    "Cargo.toml",
    "rust-toolchain",
    "rust-toolchain.toml",
    "deny.toml",
    "clippy.toml",
    ".clippy.toml",
    "rustfmt.toml",
    ".rustfmt.toml",
    ".cargo/",
    ".config/",
    "vendor/",
    "xtask/",
];

/// One workspace member, with its directory relative to the workspace root
/// (`""` for the root package) using `/` separators.
#[derive(Debug, Clone)]
pub struct Member {
    pub name: String,
    pub dir: String,
}

/// Workspace facts the selection needs, gathered once per gate run.
#[derive(Debug, Default)]
pub struct Workspace {
    pub members: Vec<Member>,
    /// `dependencies[name]` = workspace members `name` depends on (any kind).
    pub dependencies: BTreeMap<String, BTreeSet<String>>,
    /// `package` reads `path` (a file or directory).
    pub references: Vec<Reference>,
    /// Packages whose sources reference the workspace root itself, with
    /// whether the read is compiled into the package.
    pub root_readers: BTreeMap<String, bool>,
}

/// A relative path read with no Cargo dependency edge.
#[derive(Debug, Clone)]
pub struct Reference {
    pub path: String,
    pub package: String,
    /// The read is compiled into the package (`include_str!`/`include_bytes!`
    /// or a build script), so packages depending on it must be rechecked too.
    /// Otherwise only the reader's own tests can observe it.
    pub compiled: bool,
}

fn global(path: &str) -> bool {
    GLOBAL.iter().any(|entry| match entry.strip_suffix('/') {
        Some(dir) => path == dir || path.starts_with(entry),
        None => path == *entry,
    })
}

fn under(path: &str, dir: &str) -> bool {
    dir.is_empty() || path == dir || path.starts_with(&format!("{dir}/"))
}

/// The member whose directory most specifically contains `path`.
fn owner<'a>(members: &'a [Member], path: &str) -> Option<&'a Member> {
    members
        .iter()
        .filter(|member| under(path, &member.dir))
        .max_by_key(|member| member.dir.len())
}

/// Lexically resolve `relative` against `dir` (both `/`-separated, relative to
/// the workspace root). `None` when the result escapes the workspace.
fn resolve(dir: &str, relative: &str) -> Option<String> {
    let mut parts: Vec<&str> = dir.split('/').filter(|part| !part.is_empty()).collect();
    for part in relative.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            other => parts.push(other),
        }
    }
    Some(parts.join("/"))
}

/// Relative path literals in Rust source that start by leaving their directory:
/// `"../x"`, `"/../x"` (the `concat!(env!("CARGO_MANIFEST_DIR"), "/../x")`
/// form), `"key=../x"` (build-script `rerun-if-changed=`) and a bare `".."`.
/// Each comes with whether its statement embeds it at compile time.
fn parent_literals(source: &str) -> Vec<(&str, bool)> {
    let mut found = Vec::new();
    let mut offset = 0;
    while let Some(start) = source[offset..].find('"') {
        let open = offset + start;
        let body = &source[open + 1..];
        let Some(end) = body.find('"') else { break };
        let literal = &body[..end];
        let path = literal.rsplit_once('=').map_or(literal, |(_, value)| value);
        let path = path.strip_prefix('/').unwrap_or(path);
        if path == ".." || path.starts_with("../") || path.starts_with("..\\") {
            let statement = source[..open]
                .rfind([';', '{', '}'])
                .map_or(&source[..open], |at| &source[at..open]);
            let compiled =
                statement.contains("include_str!") || statement.contains("include_bytes!");
            found.push((path, compiled));
        }
        offset = open + 1 + end + 1;
    }
    found
}

/// Record what `package` (directory `crate_dir`) reads through the literals of
/// one source file at `file` (both relative to the workspace root).
pub fn scan_source(
    workspace: &mut Workspace,
    package: &str,
    crate_dir: &str,
    file: &str,
    source: &str,
) {
    let file_dir = file.rsplit_once('/').map_or("", |(dir, _)| dir);
    let build_script = file.rsplit('/').next() == Some("build.rs");
    for (literal, embedded) in parent_literals(source) {
        let compiled = embedded || build_script;
        if literal == ".." {
            // Chained `join("..")` calls can reach anywhere in the tree.
            let entry = workspace
                .root_readers
                .entry(package.to_owned())
                .or_insert(false);
            *entry |= compiled;
            continue;
        }
        // `include_str!` resolves against the file; runtime and build-script
        // reads resolve against the manifest directory. Record both.
        for base in [file_dir, crate_dir] {
            match resolve(base, literal) {
                Some(target) if target.is_empty() => {
                    let entry = workspace
                        .root_readers
                        .entry(package.to_owned())
                        .or_insert(false);
                    *entry |= compiled;
                }
                Some(target) => workspace.references.push(Reference {
                    path: target,
                    package: package.to_owned(),
                    compiled,
                }),
                None => {}
            }
        }
    }
}

/// Decide the selection for `changed` paths (relative, `/`-separated).
pub fn select(workspace: &Workspace, changed: &[String]) -> Selection {
    let Some(root) = workspace
        .members
        .iter()
        .find(|member| member.dir.is_empty())
    else {
        return Selection::Workspace("workspace has no root package".into());
    };
    // `code`: packages whose compiled output may change, so their dependents
    // are rechecked too. `observers`: packages whose tests merely read a path.
    let mut code: BTreeSet<String> = BTreeSet::new();
    let mut observers: BTreeSet<String> = BTreeSet::new();
    observers.insert(root.name.clone());
    if !changed.is_empty() {
        for (package, compiled) in &workspace.root_readers {
            if *compiled { &mut code } else { &mut observers }.insert(package.clone());
        }
    }
    for path in changed {
        if global(path) {
            return Selection::Workspace(format!("{path} affects every package"));
        }
        match owner(&workspace.members, path) {
            Some(member) => {
                code.insert(member.name.clone());
            }
            None => return Selection::Workspace(format!("{path} belongs to no package")),
        }
        for reference in &workspace.references {
            if under(path, &reference.path) {
                if reference.compiled {
                    &mut code
                } else {
                    &mut observers
                }
                .insert(reference.package.clone());
            }
        }
    }
    let mut selected = code;
    // Everything that depends on a selected package, transitively.
    let mut dependents: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (package, deps) in &workspace.dependencies {
        for dep in deps {
            dependents
                .entry(dep.as_str())
                .or_default()
                .push(package.as_str());
        }
    }
    let mut queue: Vec<String> = selected.iter().cloned().collect();
    while let Some(package) = queue.pop() {
        for dependent in dependents.get(package.as_str()).into_iter().flatten() {
            if selected.insert((*dependent).to_owned()) {
                queue.push((*dependent).to_owned());
            }
        }
    }
    selected.extend(observers);
    if selected.len() >= workspace.members.len() {
        return Selection::Workspace("every package is affected".into());
    }
    Selection::Packages(selected)
}

fn run(root: &Path, program: &str, args: &[&str]) -> Result<Vec<u8>> {
    let out = Command::new(program)
        .current_dir(root)
        .args(args)
        .output()
        .map_err(|error| format!("{program} {}: {error}", args.join(" ")))?;
    if !out.status.success() {
        return Err(format!(
            "{program} {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )
        .into());
    }
    Ok(out.stdout)
}

fn relative(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    Some(
        rel.components()
            .map(|part| part.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/"),
    )
}

/// Read workspace membership, dependency edges and relative path reads from
/// the checkout at `root` (offline, locked; no build is performed).
pub fn load(root: &Path) -> Result<Workspace> {
    let metadata: serde_json::Value = serde_json::from_slice(&run(
        root,
        "cargo",
        &["metadata", "--offline", "--locked", "--format-version", "1"],
    )?)?;
    let workspace_root = Path::new(
        metadata["workspace_root"]
            .as_str()
            .ok_or("cargo metadata has no workspace_root")?,
    )
    .to_path_buf();
    let member_ids: BTreeSet<&str> = metadata["workspace_members"]
        .as_array()
        .ok_or("cargo metadata has no workspace_members")?
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect();
    let mut names: BTreeMap<&str, String> = BTreeMap::new();
    let mut workspace = Workspace::default();
    for package in metadata["packages"]
        .as_array()
        .ok_or("cargo metadata has no packages")?
    {
        let id = package["id"].as_str().ok_or("package without id")?;
        if !member_ids.contains(id) {
            continue;
        }
        let name = package["name"]
            .as_str()
            .ok_or("package without name")?
            .to_owned();
        let manifest = Path::new(
            package["manifest_path"]
                .as_str()
                .ok_or("package without manifest")?,
        );
        let dir = manifest
            .parent()
            .and_then(|dir| relative(&workspace_root, dir))
            .ok_or("member outside the workspace root")?;
        names.insert(id, name.clone());
        workspace.members.push(Member { name, dir });
    }
    for node in metadata["resolve"]["nodes"]
        .as_array()
        .ok_or("cargo metadata has no resolve graph")?
    {
        let Some(name) = node["id"].as_str().and_then(|id| names.get(id)) else {
            continue;
        };
        let deps = workspace.dependencies.entry(name.clone()).or_default();
        for dep in node["deps"].as_array().into_iter().flatten() {
            if let Some(dep) = dep["pkg"].as_str().and_then(|id| names.get(id)) {
                deps.insert(dep.clone());
            }
        }
    }
    let listing = run(
        root,
        "git",
        &["-c", "core.quotepath=false", "ls-files", "-z", "--", "*.rs"],
    )?;
    let members = workspace.members.clone();
    for file in listing
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        let file = String::from_utf8_lossy(file).into_owned();
        let Some(member) = owner(&members, &file) else {
            continue;
        };
        let Ok(source) = std::fs::read_to_string(root.join(&file)) else {
            continue;
        };
        scan_source(&mut workspace, &member.name, &member.dir, &file, &source);
    }
    Ok(workspace)
}

/// Paths changed between `base` and `head` in the checkout at `root`.
/// Renames are reported as a deletion plus an addition so both sides count.
pub fn changed(root: &Path, base: &str, head: &str) -> Result<Vec<String>> {
    let out = run(
        root,
        "git",
        &[
            "-c",
            "core.quotepath=false",
            "diff",
            "--name-only",
            "--no-renames",
            "-z",
            base,
            head,
        ],
    )?;
    Ok(out
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .map(|entry| String::from_utf8_lossy(entry).into_owned())
        .collect())
}

/// Rewrite a `--workspace` cargo invocation to select only `packages`.
pub fn narrow(args: &mut Vec<String>, packages: &BTreeSet<String>) {
    let Some(at) = args.iter().position(|arg| arg == "--workspace") else {
        return;
    };
    let selection = packages
        .iter()
        .flat_map(|name| ["-p".to_owned(), name.clone()]);
    args.splice(at..=at, selection);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> Workspace {
        let member = |name: &str, dir: &str| Member {
            name: name.into(),
            dir: dir.into(),
        };
        let mut ws = Workspace {
            members: vec![
                member("root", ""),
                member("paths", "crates/paths"),
                member("config", "crates/config"),
                member("daemon", "crates/daemon"),
                member("leaf", "crates/leaf"),
            ],
            ..Workspace::default()
        };
        ws.dependencies
            .insert("config".into(), ["paths".into()].into());
        ws.dependencies
            .insert("daemon".into(), ["config".into()].into());
        ws.dependencies
            .insert("root".into(), ["daemon".into()].into());
        ws
    }

    fn names(selection: Selection) -> Vec<String> {
        match selection {
            Selection::Packages(set) => set.into_iter().collect(),
            Selection::Workspace(reason) => vec![format!("WORKSPACE: {reason}")],
        }
    }

    #[test]
    fn a_leaf_change_selects_its_dependents_and_root_only() {
        let ws = workspace();
        let got = names(select(&ws, &["crates/config/src/lib.rs".into()]));
        assert_eq!(got, ["config", "daemon", "root"]);
    }

    #[test]
    fn an_unrelated_crate_is_not_selected() {
        let ws = workspace();
        let got = names(select(&ws, &["crates/leaf/src/lib.rs".into()]));
        assert_eq!(got, ["leaf", "root"]);
    }

    #[test]
    fn a_docs_change_still_runs_root_tests() {
        let ws = workspace();
        assert_eq!(names(select(&ws, &["docs/guide.md".into()])), ["root"]);
        assert_eq!(names(select(&ws, &[])), ["root"]);
    }

    #[test]
    fn toolchain_lockfile_and_config_widen_to_the_workspace() {
        let ws = workspace();
        for path in [
            "Cargo.lock",
            "Cargo.toml",
            "rust-toolchain.toml",
            ".cargo/config.toml",
            ".config/nextest.toml",
            "vendor/idna-adapter/src/lib.rs",
            "xtask/src/bin/local-workflow.rs",
            "deny.toml",
        ] {
            assert!(
                matches!(select(&ws, &[path.into()]), Selection::Workspace(_)),
                "{path} must widen"
            );
        }
        // A member manifest is not the root manifest.
        assert_eq!(
            names(select(&ws, &["crates/leaf/Cargo.toml".into()])),
            ["leaf", "root"]
        );
    }

    #[test]
    fn relative_reads_select_the_reader_without_a_dependency_edge() {
        let mut ws = workspace();
        scan_source(
            &mut ws,
            "leaf",
            "crates/leaf",
            "crates/leaf/src/lib.rs",
            r#"const A: &str = include_str!("../../../config/defaults.json");"#,
        );
        scan_source(
            &mut ws,
            "paths",
            "crates/paths",
            "crates/paths/build.rs",
            r#"println!("cargo:rerun-if-changed=../../.agents/identity.json");
               let p = concat!(env!("CARGO_MANIFEST_DIR"), "/../daemon/src/wire.rs");"#,
        );
        assert_eq!(
            names(select(&ws, &["config/defaults.json".into()])),
            ["leaf", "root"]
        );
        // Build-script reads are compiled in: the reader's dependents follow.
        assert_eq!(
            names(select(&ws, &[".agents/identity.json".into()])),
            ["config", "daemon", "paths", "root"]
        );
        assert_eq!(
            names(select(&ws, &["crates/daemon/src/wire.rs".into()])),
            ["config", "daemon", "paths", "root"]
        );
    }

    #[test]
    fn a_test_only_read_selects_the_reader_but_not_its_dependents() {
        let mut ws = workspace();
        scan_source(
            &mut ws,
            "paths",
            "crates/paths",
            "crates/paths/tests/docs.rs",
            r#"let spec = std::fs::read_to_string(dir.join("../../docs/spec.md"));"#,
        );
        assert_eq!(
            names(select(&ws, &["docs/spec.md".into()])),
            ["paths", "root"]
        );
    }

    #[test]
    fn a_root_reader_is_always_selected() {
        let mut ws = workspace();
        scan_source(
            &mut ws,
            "config",
            "crates/config",
            "crates/config/tests/t.rs",
            r#"dir.join("..").join("..")"#,
        );
        // A test-only root reader is selected, its dependent `daemon` is not.
        assert_eq!(
            names(select(&ws, &["docs/x.md".into()])),
            ["config", "root"]
        );
        scan_source(
            &mut ws,
            "leaf",
            "crates/leaf",
            "crates/leaf/src/lib.rs",
            r#"include_str!("../../../x/y")"#,
        );
        assert_eq!(
            names(select(&ws, &["x/y".into()])),
            ["config", "leaf", "root"]
        );
    }

    #[test]
    fn selecting_every_member_uses_the_workspace() {
        let ws = workspace();
        let all = [
            "crates/paths/src/lib.rs".into(),
            "crates/leaf/src/lib.rs".into(),
        ];
        assert!(matches!(select(&ws, &all), Selection::Workspace(_)));
    }

    #[test]
    fn paths_outside_the_workspace_are_ignored() {
        assert_eq!(resolve("crates/a/src", "../../../../outside"), None);
        assert_eq!(
            resolve("crates/a/src", "../../../config/x"),
            Some("config/x".into())
        );
        assert_eq!(resolve("crates/a", "../.."), Some(String::new()));
    }

    #[test]
    fn narrow_replaces_only_the_workspace_flag() {
        let mut args: Vec<String> = ["test", "--workspace", "--offline", "--", "-D"]
            .iter()
            .map(|s| (*s).into())
            .collect();
        narrow(&mut args, &["b".into(), "a".into()].into());
        assert_eq!(
            args,
            ["test", "-p", "a", "-p", "b", "--offline", "--", "-D"]
        );
    }
}
