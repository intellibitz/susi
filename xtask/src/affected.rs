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
    // xtask is a leaf package: nothing depends on it, and the gate runs the
    // already-built launcher, not the candidate's. Only the parts that decide
    // how every other package is compiled widen the gate; the rest of xtask
    // selects xtask (and root) like any other member.
    "xtask/build.rs",
    "xtask/bootstrap/",
    "xtask/src/compiler_env.rs",
    "xtask/src/trusted_sysroot.rs",
    "xtask/src/sysroot_sources.rs",
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
fn parent_literals(source: &str, test_only: bool) -> Vec<(&str, bool)> {
    let tests = test_module_ranges(source);
    let mut found = Vec::new();
    let mut offset = 0;
    while let Some(start) = source[offset..].find('"') {
        let open = offset + start;
        let body = &source[open + 1..];
        let Some(end) = body.find('"') else { break };
        let literal = &body[..end];
        let path = literal.rsplit_once('=').map_or(literal, |(_, value)| value);
        let path = path.strip_prefix('/').unwrap_or(path);
        // A literal on a `//` comment line is prose, not a read.
        let line_start = source[..open].rfind('\n').map_or(0, |at| at + 1);
        let commented = source[line_start..open].contains("//");
        // A bare `".."` is a path only as a path-building argument
        // (`join("..")`, `push("..")`, `Path::new("..")`); elsewhere it is a
        // traversal check such as `name.contains("..")` or `c == ".."`.
        let bare_non_path = path == ".."
            && !["join(", "push(", "new("]
                .iter()
                .any(|call| source[..open].trim_end().ends_with(call));
        if commented || bare_non_path {
            offset = open + 1 + end + 1;
            continue;
        }
        if path == ".." || path.starts_with("../") || path.starts_with("..\\") {
            let statement = source[..open]
                .rfind([';', '{', '}'])
                .map_or(&source[..open], |at| &source[at..open]);
            // An embed inside a `#[cfg(test)] mod` is compiled into that
            // package's test binary only: dependents never see it, so it is
            // an observation, not a compiled input.
            let in_test = tests.iter().any(|(from, to)| (*from..*to).contains(&open));
            let compiled = !test_only
                && !in_test
                && (statement.contains("include_str!") || statement.contains("include_bytes!"));
            found.push((path, compiled));
        }
        offset = open + 1 + end + 1;
    }
    found
}

/// Byte ranges of `#[cfg(test)] mod name { ... }` bodies in `source`.
///
/// A small Rust lexer skips comments, string, raw-string, byte-string and char
/// literals so braces inside them do not count. Only a `mod` item directly
/// carrying `#[cfg(test)]` (other attributes may sit between) qualifies; any
/// other shape stays outside every range, which keeps the selection
/// conservative: an embed that is not provably test-only counts as compiled.
pub(crate) fn test_module_ranges(source: &str) -> Vec<(usize, usize)> {
    scan_test_modules(source).0
}

/// Names of out-of-line `#[cfg(test)] mod name;` declarations in `source`:
/// the module's whole file (and its submodules) is test-only.
pub(crate) fn test_module_decls(source: &str) -> Vec<String> {
    scan_test_modules(source).1
}

/// One pass of the lexer behind [`test_module_ranges`] and
/// [`test_module_decls`].
fn scan_test_modules(source: &str) -> (Vec<(usize, usize)>, Vec<String>) {
    let bytes = source.as_bytes();
    let mut ranges = Vec::new();
    let mut decls = Vec::new();
    // Each open brace: `Some(start)` when it opens a test module body.
    let mut stack: Vec<Option<usize>> = Vec::new();
    let mut pending_test_brace: Option<usize> = None;
    let mut i = 0;
    while i < bytes.len() {
        if !source.is_char_boundary(i) {
            i += 1;
            continue;
        }
        let rest = &source[i..];
        match bytes[i] {
            b'/' if rest.starts_with("//") => {
                i = rest.find('\n').map_or(bytes.len(), |at| i + at);
                continue;
            }
            b'/' if rest.starts_with("/*") => {
                let mut depth = 0usize;
                let mut j = i;
                while j < bytes.len() {
                    if bytes[j..].starts_with(b"/*") {
                        depth += 1;
                        j += 2;
                    } else if bytes[j..].starts_with(b"*/") {
                        depth = depth.saturating_sub(1);
                        j += 2;
                        if depth == 0 {
                            break;
                        }
                    } else {
                        j += 1;
                    }
                }
                i = j;
                continue;
            }
            b'r' | b'b' if raw_string_open(rest).is_some() => {
                let (hashes, quote_at) = raw_string_open(rest).unwrap_or((0, 0));
                let close = format!("\"{}", "#".repeat(hashes));
                let body = i + quote_at + 1;
                i = source[body..]
                    .find(&close)
                    .map_or(bytes.len(), |at| body + at + close.len());
                continue;
            }
            b'"' => {
                i = skip_quoted(bytes, i, b'"');
                continue;
            }
            b'\'' => {
                // A char literal ('x', '\n', '{') rather than a lifetime.
                if bytes.get(i + 1) == Some(&b'\\') {
                    i = skip_quoted(bytes, i, b'\'');
                    continue;
                }
                if let Some(c) = source[i + 1..].chars().next() {
                    let after = i + 1 + c.len_utf8();
                    if bytes.get(after) == Some(&b'\'') {
                        i = after + 1;
                        continue;
                    }
                }
                i += 1;
                continue;
            }
            b'#' if rest.starts_with("#[cfg(test)]") => {
                pending_test_brace = None;
                match test_mod_item(source, i + "#[cfg(test)]".len()) {
                    Some(TestMod::Inline(brace)) => pending_test_brace = Some(brace),
                    Some(TestMod::OutOfLine(name)) => decls.push(name),
                    None => {}
                }
                i += "#[cfg(test)]".len();
                continue;
            }
            b'{' => {
                if pending_test_brace == Some(i) {
                    pending_test_brace = None;
                    stack.push(Some(i));
                } else {
                    stack.push(None);
                }
            }
            b'}' => {
                if let Some(Some(start)) = stack.pop() {
                    ranges.push((start, i + 1));
                }
            }
            _ => {}
        }
        i += 1;
    }
    (ranges, decls)
}

/// What a `#[cfg(test)]` attribute is attached to, when it is a module.
enum TestMod {
    /// `mod name { ... }`: the offset of its opening brace.
    Inline(usize),
    /// `mod name;`: the module's name.
    OutOfLine(String),
}

/// `r"`, `r#"`, `br##"`, ...: the hash count and the quote's offset.
fn raw_string_open(rest: &str) -> Option<(usize, usize)> {
    let after_prefix = if rest.starts_with("br") {
        2
    } else if rest.starts_with('r') {
        1
    } else {
        return None;
    };
    let hashes = rest[after_prefix..]
        .bytes()
        .take_while(|byte| *byte == b'#')
        .count();
    (rest.as_bytes().get(after_prefix + hashes) == Some(&b'"'))
        .then_some((hashes, after_prefix + hashes))
}

/// Skip a `"..."` or `'...'` literal starting at `open`, honouring escapes.
fn skip_quoted(bytes: &[u8], open: usize, quote: u8) -> usize {
    let mut j = open + 1;
    while j < bytes.len() {
        match bytes[j] {
            b'\\' => j += 2,
            byte if byte == quote => return j + 1,
            _ => j += 1,
        }
    }
    bytes.len()
}

/// After a `#[cfg(test)]` ending at `from`, the directly following
/// `[pub[(..)]] mod name { | ;` item, skipping whitespace and other outer
/// attributes. `None` for any other item, and for a module carrying a
/// `#[path]` attribute (its file is not where the name says).
fn test_mod_item(source: &str, from: usize) -> Option<TestMod> {
    let mut rest = &source[from..];
    let mut at = from;
    let skip = |rest: &mut &str, at: &mut usize, n: usize| {
        *rest = &rest[n..];
        *at += n;
    };
    loop {
        let trimmed = rest.trim_start();
        let n = rest.len() - trimmed.len();
        skip(&mut rest, &mut at, n);
        if rest.starts_with("#[") {
            if rest.starts_with("#[path") {
                return None;
            }
            let end = rest.find(']')? + 1;
            skip(&mut rest, &mut at, end);
            continue;
        }
        break;
    }
    if rest.starts_with("pub") {
        let after = &rest[3..];
        let n = if after.starts_with('(') {
            3 + after.find(')')? + 1
        } else {
            3
        };
        skip(&mut rest, &mut at, n);
        let trimmed = rest.trim_start();
        let n = rest.len() - trimmed.len();
        skip(&mut rest, &mut at, n);
    }
    let after_mod = rest.strip_prefix("mod")?;
    if !after_mod.starts_with(char::is_whitespace) {
        return None;
    }
    skip(&mut rest, &mut at, 3);
    let trimmed = rest.trim_start();
    let n = rest.len() - trimmed.len();
    skip(&mut rest, &mut at, n);
    let ident = rest
        .bytes()
        .take_while(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        .count();
    if ident == 0 {
        return None;
    }
    let name = rest[..ident].to_owned();
    skip(&mut rest, &mut at, ident);
    let trimmed = rest.trim_start();
    let n = rest.len() - trimmed.len();
    skip(&mut rest, &mut at, n);
    if rest.starts_with('{') {
        Some(TestMod::Inline(at))
    } else if rest.starts_with(';') {
        Some(TestMod::OutOfLine(name))
    } else {
        None
    }
}

/// Files that only a package's own tests compile: integration tests,
/// benchmarks and examples under the crate directory, and the files of
/// out-of-line `#[cfg(test)] mod name;` modules (with their submodules).
pub fn test_only_files<'a>(
    members: &[Member],
    sources: impl IntoIterator<Item = (&'a str, &'a str)> + Clone,
) -> BTreeSet<String> {
    let mut prefixes: Vec<String> = Vec::new();
    for (file, source) in sources.clone() {
        let decls = test_module_decls(source);
        if decls.is_empty() {
            continue;
        }
        let (dir, stem) = file.rsplit_once('/').unwrap_or(("", file));
        let module_dir = match stem {
            "lib.rs" | "main.rs" | "mod.rs" => dir.to_owned(),
            other => {
                let stem = other.strip_suffix(".rs").unwrap_or(other);
                if dir.is_empty() {
                    stem.to_owned()
                } else {
                    format!("{dir}/{stem}")
                }
            }
        };
        for name in decls {
            let base = if module_dir.is_empty() {
                name
            } else {
                format!("{module_dir}/{name}")
            };
            prefixes.push(format!("{base}.rs"));
            prefixes.push(format!("{base}/"));
        }
    }
    let mut test_only = BTreeSet::new();
    for (file, _) in sources {
        let in_test_tree = owner(members, file).is_some_and(|member| {
            ["tests", "benches", "examples"].iter().any(|kind| {
                let dir = if member.dir.is_empty() {
                    (*kind).to_owned()
                } else {
                    format!("{}/{kind}", member.dir)
                };
                file.starts_with(&format!("{dir}/"))
            })
        });
        let in_test_module = prefixes.iter().any(|prefix| {
            file == prefix || (prefix.ends_with('/') && file.starts_with(prefix.as_str()))
        });
        if in_test_tree || in_test_module {
            test_only.insert(file.to_owned());
        }
    }
    test_only
}

/// Record what `package` (directory `crate_dir`) reads through the literals of
/// one source file at `file` (both relative to the workspace root).
#[cfg(test)]
pub fn scan_source(
    workspace: &mut Workspace,
    package: &str,
    crate_dir: &str,
    file: &str,
    source: &str,
) {
    scan_source_in(workspace, package, crate_dir, file, source, false);
}

/// [`scan_source`] for a file that may be compiled only into its package's
/// own tests (`test_only`): its embeds are then observations, never compiled
/// inputs of dependents.
pub fn scan_source_in(
    workspace: &mut Workspace,
    package: &str,
    crate_dir: &str,
    file: &str,
    source: &str,
    test_only: bool,
) {
    let file_dir = file.rsplit_once('/').map_or("", |(dir, _)| dir);
    let build_script = file.rsplit('/').next() == Some("build.rs");
    for (literal, embedded) in parent_literals(source, test_only) {
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
    select_explained(workspace, changed).0
}

/// [`select`], plus one human-readable reason per selected package (the first
/// that selected it), so `local-workflow affected` can say why a package is in.
pub fn select_explained(
    workspace: &Workspace,
    changed: &[String],
) -> (Selection, BTreeMap<String, String>) {
    let mut why: BTreeMap<String, String> = BTreeMap::new();
    let mut note = |package: &str, reason: String| {
        why.entry(package.to_owned()).or_insert(reason);
    };
    let Some(root) = workspace
        .members
        .iter()
        .find(|member| member.dir.is_empty())
    else {
        return (
            Selection::Workspace("workspace has no root package".into()),
            BTreeMap::new(),
        );
    };
    // `code`: packages whose compiled output may change, so their dependents
    // are rechecked too. `observers`: packages whose tests merely read a path.
    let mut code: BTreeSet<String> = BTreeSet::new();
    let mut observers: BTreeSet<String> = BTreeSet::new();
    observers.insert(root.name.clone());
    note(
        &root.name,
        "root package (tree-wide architecture and docs tests)".into(),
    );
    if !changed.is_empty() {
        for (package, compiled) in &workspace.root_readers {
            if *compiled { &mut code } else { &mut observers }.insert(package.clone());
            note(
                package,
                format!(
                    "reads the workspace root ({})",
                    if *compiled {
                        "compiled in"
                    } else {
                        "tests only"
                    }
                ),
            );
        }
    }
    for path in changed {
        if global(path) {
            return (
                Selection::Workspace(format!("{path} affects every package")),
                BTreeMap::new(),
            );
        }
        match owner(&workspace.members, path) {
            Some(member) => {
                code.insert(member.name.clone());
                note(&member.name, format!("owns {path}"));
            }
            None => {
                return (
                    Selection::Workspace(format!("{path} belongs to no package")),
                    BTreeMap::new(),
                )
            }
        }
        for reference in &workspace.references {
            if under(path, &reference.path) {
                if reference.compiled {
                    &mut code
                } else {
                    &mut observers
                }
                .insert(reference.package.clone());
                note(
                    &reference.package,
                    format!(
                        "{} {} (changed {path})",
                        if reference.compiled {
                            "compiles in"
                        } else {
                            "tests read"
                        },
                        reference.path
                    ),
                );
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
                note(dependent, format!("depends on {package}"));
                queue.push((*dependent).to_owned());
            }
        }
    }
    selected.extend(observers);
    if selected.len() >= workspace.members.len() {
        return (
            Selection::Workspace("every package is affected".into()),
            why,
        );
    }
    (Selection::Packages(selected), why)
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
    let mut sources: Vec<(String, String)> = Vec::new();
    for file in listing
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        let file = String::from_utf8_lossy(file).into_owned();
        if owner(&members, &file).is_none() {
            continue;
        }
        let Ok(source) = std::fs::read_to_string(root.join(&file)) else {
            continue;
        };
        sources.push((file, source));
    }
    let test_only = test_only_files(
        &members,
        sources
            .iter()
            .map(|(file, source)| (file.as_str(), source.as_str())),
    );
    for (file, source) in &sources {
        let Some(member) = owner(&members, file) else {
            continue;
        };
        let only_tests = test_only.contains(file);
        scan_source_in(
            &mut workspace,
            &member.name,
            &member.dir,
            file,
            source,
            only_tests,
        );
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
            "xtask/src/compiler_env.rs",
            "xtask/src/trusted_sysroot.rs",
            "xtask/bootstrap/susi-dev.rs",
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

    #[test]
    fn workflow_tooling_in_xtask_selects_only_xtask_and_root() {
        let mut ws = workspace();
        ws.members.push(Member {
            name: "xtask".into(),
            dir: "xtask".into(),
        });
        for path in [
            "xtask/src/bin/local-workflow.rs",
            "xtask/src/affected.rs",
            "xtask/tests/ci_workflow_perf.rs",
        ] {
            assert_eq!(
                names(select(&ws, &[path.into()])),
                ["root", "xtask"],
                "{path}"
            );
        }
    }

    #[test]
    fn an_embed_inside_a_cfg_test_module_is_an_observation_not_a_compiled_input() {
        let mut ws = workspace();
        let source = r##"
//! Docs with a brace { in a comment.
pub const BRIEF: &str = include_str!("../brief.md");
fn f() -> char { '{' }
fn g<'a>(s: &'a str) -> &'a str { s }
const RAW: &str = r#"not a } brace "../nope.md" "#;
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    #[test]
    fn reads_governance() {
        let governance = include_str!("../../../AGENTS.md");
        let nested = { 1 };
        assert!(!governance.is_empty() && nested == 1);
    }
}
pub const AFTER: &str = include_str!("../after.md");
"##;
        scan_source(
            &mut ws,
            "config",
            "crates/config",
            "crates/config/src/lib.rs",
            source,
        );
        // Test-only embed: only the reader's own tests observe it.
        assert_eq!(
            names(select(&ws, &["AGENTS.md".into()])),
            ["config", "root"]
        );
        // Production embeds before and after the test module stay compiled.
        assert_eq!(
            names(select(&ws, &["crates/config/brief.md".into()])),
            ["config", "daemon", "root"]
        );
        assert_eq!(
            names(select(&ws, &["crates/config/after.md".into()])),
            ["config", "daemon", "root"]
        );
    }

    #[test]
    fn only_a_mod_carrying_cfg_test_is_a_test_range() {
        let source = "#[cfg(test)]
fn helper() { include_str!(\"../x\"); }
#[cfg(test)]
pub(crate) mod t { fn a() {} }
";
        let ranges = test_module_ranges(source);
        assert_eq!(ranges.len(), 1, "{ranges:?}");
        let (from, to) = ranges[0];
        assert!(source[from..to].starts_with('{') && source[from..to].ends_with('}'));
        assert!(source[from..to].contains("fn a()"));
        assert!(!source[from..to].contains("helper"));
    }

    #[test]
    fn out_of_line_test_modules_and_test_trees_are_test_only() {
        let ws = workspace();
        let lib = "pub mod api;\n#[cfg(test)]\nmod tests;\n#[cfg(test)]\n#[path = \"elsewhere.rs\"]\nmod moved;\n";
        let api = "#[cfg(test)]\nmod api_tests;\n";
        let files = [
            ("crates/config/src/lib.rs", lib),
            ("crates/config/src/api.rs", api),
            ("crates/config/src/tests.rs", ""),
            ("crates/config/src/tests/deep.rs", ""),
            ("crates/config/src/api/api_tests.rs", ""),
            ("crates/config/src/elsewhere.rs", ""),
            ("crates/config/src/moved.rs", ""),
            ("crates/config/tests/integration.rs", ""),
            ("crates/config/benches/b.rs", ""),
            ("crates/config/build.rs", ""),
        ];
        let got = test_only_files(&ws.members, files.iter().map(|(f, s)| (*f, *s)));
        let got: Vec<&str> = got.iter().map(String::as_str).collect();
        assert_eq!(
            got,
            [
                "crates/config/benches/b.rs",
                "crates/config/src/api/api_tests.rs",
                "crates/config/src/tests.rs",
                "crates/config/src/tests/deep.rs",
                "crates/config/tests/integration.rs",
            ]
        );
    }

    #[test]
    fn a_test_only_file_embed_does_not_select_dependents() {
        let mut ws = workspace();
        let source = "const SPEC: &str = include_str!(\"../../../docs/spec.md\");";
        scan_source_in(
            &mut ws,
            "config",
            "crates/config",
            "crates/config/src/tests.rs",
            source,
            true,
        );
        assert_eq!(
            names(select(&ws, &["docs/spec.md".into()])),
            ["config", "root"]
        );
    }

    #[test]
    fn traversal_checks_and_comments_are_not_root_reads() {
        let mut ws = workspace();
        let source = concat!(
            "fn safe(name: &str) -> bool { !name.contains(\"..\") && name != \"..\" }\n",
            "// see \"../../docs/x.md\" for details\n",
            "fn ok(c: &str) -> bool { matches!(c, \".\" | \"..\") }\n",
        );
        scan_source(
            &mut ws,
            "config",
            "crates/config",
            "crates/config/src/lib.rs",
            source,
        );
        assert!(ws.root_readers.is_empty(), "{:?}", ws.root_readers);
        assert!(ws.references.is_empty(), "{:?}", ws.references);
        let reader = "fn up(dir: &Path) -> PathBuf { dir.join(\"..\") }";
        scan_source(
            &mut ws,
            "leaf",
            "crates/leaf",
            "crates/leaf/src/lib.rs",
            reader,
        );
        assert_eq!(ws.root_readers.get("leaf"), Some(&false));
    }
}
