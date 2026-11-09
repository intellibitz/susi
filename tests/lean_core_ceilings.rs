#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Lean-core ceilings (`docs/lean-core.md`): the four pillars' dependency
//! reach and size may only shrink. Every cut lowers a constant here in the
//! same change; nothing raises one without a CEO decision recorded in the doc.
//!
//! The reach is measured from `cargo metadata` (normal and build edges, the
//! way the linked product is built; dev edges excluded), so the numbers are
//! what a clean clone compiles for GAWD, GEMI, GMCP and the harness.
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

const PILLARS: [&str; 4] = ["susi-gawd", "susi-gemi", "susi-gmcp", "susi-harness"];

/// Third-party (non-workspace) crates the four pillars compile. 2026-11-07:
/// 331 before the lean-core program began, 313 after wave 1 (rayon, flume
/// and jsonschema edges cut from the GAWD and GEMI crates).
const THIRD_PARTY_CEILING: usize = 259;
/// Workspace crates the four pillars compile (themselves included).
const WORKSPACE_CEILING: usize = 50;
/// Distinct `susi-vendor-*` facades the pillar crates name directly in
/// their manifests (normal edges): the edges the program removes one by one.
const DIRECT_FACADE_EDGES_CEILING: usize = 22;
/// Rust lines under `crates/<crate>/src` per pillar family, counting what
/// ships: the `tests/` and `mastery_tests/` directories directly under `src`
/// hold standalone test suites (`#[cfg(test)] #[path = ..]` modules) and are
/// left out, so a suite returning to core (2026-11-08: 35 core-subject suites
/// come back from the commercial copies) never reads as production growth
/// and a cut can never hide behind a deleted test. Inline `mod tests` inside
/// production files still count. A pillar may take production lines only from
/// a crate leaving core (a contract moving in, as `set_selected_model_override`
/// did on 2026-11-07, +128 in gemi-models while vendor-models shed far more)
/// or from a plug port that replaces a stub (mandate rule 4); the change that
/// moves them re-pins every crate here at its measured value and says so in
/// the wave log. A new security capability's boundary checks (2026-11-08:
/// the `host_only` privacy mode) are re-pinned the same way.
const LINE_CEILINGS: [(&str, usize); 8] = [
    ("susi-gawd", 13_293),
    ("susi-gawd-swarm", 23_002),
    ("susi-gawd-agents", 11_495),
    ("susi-gawd-a2a", 2_113),
    ("susi-gemi", 21_290),
    ("susi-gemi-models", 16_797),
    ("susi-gmcp", 19_469),
    ("susi-harness", 40_790),
];

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn metadata() -> Value {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
        .current_dir(workspace_root())
        .args(["metadata", "--format-version", "1", "--offline", "--locked"])
        .output()
        .expect("cargo metadata runs");
    assert!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("cargo metadata is JSON")
}

struct Reach {
    workspace: BTreeSet<String>,
    third_party: BTreeSet<String>,
    direct_facades: BTreeSet<String>,
}

/// Normal+build closure of the pillar packages.
fn pillar_reach(metadata: &Value) -> Reach {
    let members: BTreeSet<&str> = metadata["workspace_members"]
        .as_array()
        .expect("members")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let names: BTreeMap<&str, &str> = metadata["packages"]
        .as_array()
        .expect("packages")
        .iter()
        .map(|p| {
            (
                p["id"].as_str().expect("id"),
                p["name"].as_str().expect("name"),
            )
        })
        .collect();
    let nodes: BTreeMap<&str, &Value> = metadata["resolve"]["nodes"]
        .as_array()
        .expect("resolve nodes")
        .iter()
        .map(|n| (n["id"].as_str().expect("node id"), n))
        .collect();
    let built_edge = |dep: &Value| {
        dep["dep_kinds"]
            .as_array()
            .map(|kinds| {
                kinds.iter().any(|k| {
                    matches!(k["kind"].as_str(), None | Some("normal") | Some("build"))
                        && k["kind"] != Value::String("dev".into())
                })
            })
            .unwrap_or(true)
    };
    let roots: Vec<&str> = members
        .iter()
        .copied()
        .filter(|id| PILLARS.contains(&names[id]))
        .collect();
    assert_eq!(
        roots.len(),
        PILLARS.len(),
        "every pillar is a workspace member"
    );
    let mut direct_facades = BTreeSet::new();
    for root in &roots {
        for dep in nodes[root]["deps"].as_array().expect("deps") {
            let id = dep["pkg"].as_str().expect("pkg");
            if built_edge(dep) && names[id].starts_with("susi-vendor-") {
                direct_facades.insert(names[id].to_owned());
            }
        }
    }
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut stack = roots;
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        for dep in nodes[id]["deps"].as_array().expect("deps") {
            if built_edge(dep) {
                stack.push(dep["pkg"].as_str().expect("pkg"));
            }
        }
    }
    let (workspace, third_party): (Vec<&str>, Vec<&str>) =
        seen.iter().copied().partition(|id| members.contains(id));
    Reach {
        workspace: workspace
            .into_iter()
            .map(|id| names[id].to_owned())
            .collect(),
        third_party: third_party
            .into_iter()
            .map(|id| names[id].to_owned())
            .collect(),
        direct_facades,
    }
}

/// Directories directly under a crate's `src` that hold standalone test
/// suites rather than shipped code (see [`LINE_CEILINGS`]).
const SUITE_DIRS: [&str; 2] = ["tests", "mastery_tests"];

fn rust_lines(dir: &Path) -> usize {
    rust_lines_in(dir, true)
}

fn rust_lines_in(dir: &Path, top: bool) -> usize {
    let mut total = 0;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let is_suite_dir = top
                && path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| SUITE_DIRS.contains(&n));
            if !is_suite_dir {
                total += rust_lines_in(&path, false);
            }
        } else if path.extension().is_some_and(|e| e == "rs") {
            total += std::fs::read_to_string(&path).map_or(0, |s| s.lines().count());
        }
    }
    total
}

#[test]
fn pillar_dependency_reach_does_not_grow() {
    let reach = pillar_reach(&metadata());
    eprintln!(
        "lean-core: pillars compile {} workspace crates, {} third-party crates, {} direct facade edges",
        reach.workspace.len(),
        reach.third_party.len(),
        reach.direct_facades.len()
    );
    assert!(
        reach.third_party.len() <= THIRD_PARTY_CEILING,
        "the four pillars now compile {} third-party crates (ceiling {}): {:?}",
        reach.third_party.len(),
        THIRD_PARTY_CEILING,
        reach.third_party
    );
    assert!(
        reach.workspace.len() <= WORKSPACE_CEILING,
        "the four pillars now compile {} workspace crates (ceiling {}): {:?}",
        reach.workspace.len(),
        WORKSPACE_CEILING,
        reach.workspace
    );
    assert!(
        reach.direct_facades.len() <= DIRECT_FACADE_EDGES_CEILING,
        "the pillar crates now name {} vendor facades directly (ceiling {}): {:?}",
        reach.direct_facades.len(),
        DIRECT_FACADE_EDGES_CEILING,
        reach.direct_facades
    );
}

#[test]
fn pillar_source_does_not_grow() {
    let root = workspace_root();
    for (krate, ceiling) in LINE_CEILINGS {
        let lines = rust_lines(&root.join("crates").join(krate).join("src"));
        eprintln!("lean-core: {krate} src = {lines} lines (ceiling {ceiling})");
        assert!(
            lines <= ceiling,
            "{krate} grew to {lines} shipped Rust lines under src (ceiling {ceiling}; suite dirs excluded); lean-core cuts only"
        );
    }
}
