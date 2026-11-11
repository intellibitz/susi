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
/// and jsonschema edges cut from the GAWD and GEMI crates). 2026-11-09:
/// 259 -> 231 when susi-gawd stopped depending on the A2A tier (ra2a, its
/// reqwest/rustls client side and tower-http); susi-daemon serves A2A and
/// still compiles them, so the host binary's unit count did not move.
/// 2026-11-09: 231 -> 198 when GMCP began serving MCP itself (rmcp,
/// rmcp-macros, schemars, schemars_derive, sse-stream, pastey, darling and
/// the rest of the SDK's tail left the build).
/// 2026-11-10: 198 -> 196 after the unused swarm flume edge (and spin) left.
/// 2026-11-11: 196 -> 191 (measured; wasmparser 0.259 and wasm-encoder 0.259
/// left with T-CLAUDEWASMSTD-1, the rest had already gone unpinned).
/// 2026-11-11: 191 -> 184 (measured after T-CLAUDENOA2ASDK-1 and T-AGYCLOUD-3
/// removed susi-vendor-flume and its fastrand/getrandom wasm_js + spin
/// lock_api workspace feature unification).
/// 2026-11-11: 184 -> 180 (T-AGYCLOUD-4: delete susi-vendor-futures and
/// susi-vendor-bytes; futures, futures-executor, futures-io and proc-macro
/// futures-macro leave Cargo.lock and the pillar closure).
/// 2026-11-11: 180 -> 161 (measured; T-NETTRANSPORT-1: susi-http-transport's
/// outbound client is HTTP/1.1 on std sockets over the platform TLS already
/// in the closure, so ureq, ureq-proto, flate2 and its zlib backends,
/// cookie_store, time and their tail left the pillars).
/// 2026-11-11: 161 -> 154 (T-SMALLFACADES-1: regex, regex-automata,
/// regex-syntax, aho-corasick, dashmap, crossbeam-utils and susi-os-unicode
/// left with the regex and dashmap facades; the truth gate's claim patterns
/// run on `susi_core::pattern`, shared maps on `susi_paths::sync`).
/// 2026-11-11: 154 -> 145 (measured; T-HARNESSSTD-1: susi-harness serves
/// its HTTP surfaces on its own std server, so axum, axum-core, matchit,
/// tower, serde_path_to_error, serde_urlencoded, sync_wrapper, mime and ryu
/// left the pillars; susi-gawd-a2a and susi-leaf-services still use axum).
/// 2026-11-11: 145 -> 141 (T-SMALLFACADES-2: url, idna, utf8_iter and
/// form_urlencoded left with the url facade; `susi_paths::weburl` parses).
/// 2026-11-11: 141 -> 140 (T-CARBONLEAN-4: `valuable` leaves Cargo.lock and
/// `tracing`'s feature unification when `susi-vendor-tracing-subscriber` goes).
/// 2026-11-11: 140 -> 139 (T-CARBONLEAN-5: `http-body-util` leaves Cargo.lock
/// and the pillar closure when `susi-vendor-http-body-util` is replaced by
/// `susi_http_transport::http_body`).
/// 2026-11-11: 139 -> 138 (T-CARBONLEAN-6: `hex` leaves Cargo.lock and the
/// pillar closure when `susi-vendor-hex` is replaced by `susi_paths::hex`).
const THIRD_PARTY_CEILING: usize = 138;
/// Workspace crates the four pillars compile (themselves included).
/// 2026-11-09: 50 -> 47 (susi-gawd-a2a, susi-vendor-ra2a, susi-vendor-tower;
/// susi-vendor-ra2a was deleted on 2026-11-11, T-CLAUDENOA2ASDK-1),
/// then 47 -> 46 (susi-vendor-mcp-server deleted).
/// 2026-11-11: 46 -> 45 (T-AGYCLOUD-4: susi-vendor-futures deleted).
/// 2026-11-11: 45 -> 44 (susi-vendor-ureq deleted, T-NETTRANSPORT-1).
/// 2026-11-11: 44 -> 41 (susi-vendor-regex, susi-vendor-dashmap and
/// susi-vendor-parking-lot deleted; T-SMALLFACADES-1).
/// 2026-11-11: 41 -> 40 (susi-vendor-axum left the pillar closure;
/// T-HARNESSSTD-1).
/// 2026-11-11: 40 -> 39 (susi-vendor-url deleted, T-SMALLFACADES-2).
/// 2026-11-11: 39 -> 38 (susi-vendor-shlex deleted, T-CARBONLEAN-2).
/// 2026-11-11: 38 -> 37 (susi-vendor-tower-service deleted, T-AGYCLOUD-8).
/// 2026-11-11: 37 -> 36 (susi-vendor-http-body-util deleted, T-CARBONLEAN-5).
/// 2026-11-11: 36 -> 35 (susi-vendor-hex deleted, T-CARBONLEAN-6).
const WORKSPACE_CEILING: usize = 35;
/// Distinct `susi-vendor-*` facades the pillar crates name directly in
/// their manifests (normal edges): the edges the program removes one by one.
/// 2026-11-09: 22 -> 21 (susi-gmcp -> susi-vendor-mcp-server).
/// 2026-11-10: 21 -> 20; GMCP uses Hyper's existing Bytes/Buf exports.
/// 2026-11-11: 20 -> 19 (T-AGYCLOUD-4: susi-gemi -> susi-vendor-parking-lot
/// replaced with std::sync::RwLock; susi-harness -> susi-vendor-futures
/// replaced with hyper::body::Body already in the pillar facade set).
/// 2026-11-11: 19 -> 17 (susi-gmcp -> dashmap and parking-lot;
/// T-SMALLFACADES-1).
/// 2026-11-11: 17 -> 16 (susi-harness -> susi-vendor-axum; T-HARNESSSTD-1).
/// 2026-11-11: 16 -> 15 (susi-gmcp -> url, T-SMALLFACADES-2).
/// 2026-11-11: 15 -> 14 (susi-gmcp -> shlex, T-CARBONLEAN-2).
/// 2026-11-11: 14 -> 13 (susi-gmcp -> tower-service replaced by direct
/// `StreamableHttpService::handle`, T-AGYCLOUD-8).
/// 2026-11-11: 13 -> 12 (susi-gmcp -> http-body-util replaced by
/// `susi_http_transport::http_body`, T-CARBONLEAN-5).
/// 2026-11-11: 12 -> 11 (susi-{gawd,gemi,gmcp,harness} -> susi-vendor-hex
/// replaced by `susi_paths::hex`, T-CARBONLEAN-6).
const DIRECT_FACADE_EDGES_CEILING: usize = 11;
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
/// the `host_only` privacy mode) are re-pinned the same way. 2026-11-09:
/// susi-gmcp 19,469 -> 25,900 when the MCP SDK facade (susi-vendor-mcp-server
/// over rmcp) left core and GMCP took its protocol in (`src/mcp`, 6,433
/// lines in place of the rmcp crate's 55,000 source lines across its
/// features); 28 fewer units in the host build (T-NOMCPSDK-1, wave log).
/// 2026-11-09: susi-gemi-models 16,797 -> 16,743 (hardware.rs reads Windows
/// memory, swap and uptime through host_metrics; T-NOWINDOW-1).
/// 2026-11-09: 16,743 -> 16,737 after shared scan/cache predicates and
/// fail-closed model publication consolidation (T-DOTMODELPUBLISH-1).
/// 2026-11-10: gemi-models 16,737 -> 15,938: move the unchanged model-manager
/// unit suite to src/tests while adding explicit retry-clock assertions
/// (T-DOTLATENCY-1). Test organization, not removed runtime capability.
/// 2026-11-11: susi-gawd-a2a 2,113 -> 3,702 when the A2A SDK facade
/// (susi-vendor-ra2a over ra2a 0.10.1) left core and the tier took the
/// protocol subset it serves in (`src/protocol`, 1,744 lines in place of
/// ra2a's 10,221 source lines and its reqwest/hyper-tls/encoding_rs/chrono/
/// tower-http/uuid tail; T-CLAUDENOA2ASDK-1, wave log). The same "crate
/// leaving core, contract moving in" re-pin as T-NOMCPSDK-1. The A2A tier is
/// outside the pillar closure since T-A2ASTD-1, so the reach ceilings above
/// do not move; the host build loses the units.
/// 2026-11-11: ratify accumulated source reductions across four pillar crates
/// (susi-gawd-swarm 20,641 -> 20,635, susi-gawd-agents 11,263 -> 10,447,
/// susi-gemi 21,290 -> 21,263, susi-harness 40,790 -> 40,594; -1,045 lines
/// total) alongside removing the unreferenced susi-vendor-flume workspace
/// facade (T-AGYCLOUD-3).
/// 2026-11-11: T-AGYCLOUD-4: susi-gawd-agents 10,447 -> 10,443 (drop regex
/// facade edge in accountability.rs), susi-gemi 21,263 -> 21,171 (std RwLock
/// in local_plug.rs + move unchanged usage_accounting unit tests to src/tests),
/// susi-harness 40,594 -> 40,575 (poll_body over hyper::body::Body + move
/// unchanged MicroUsd inline tests from lib.rs to src/tests/money.rs).
/// 2026-11-11: susi-harness 40,575 -> 39,446 (T-HARNESSSTD-1, measured).
/// Production lines grew by 1,859: src/http.rs (1,843 lines, the std
/// HTTP/1.1 server and axum's JSON rejection paths) replaces axum and the
/// eight crates it brought into the pillars (about 45,000 source lines),
/// and poll_body went. The ceiling still falls because the gateway, ingress
/// and cockpit inline test modules (2,988 lines) moved unchanged to
/// src/tests, the crate's convention. Stated here so the drop is not read
/// as a cut.
/// 2026-11-11: susi-gemi-models 15,950 -> 15,945 and susi-gmcp 25,900 ->
/// 25,891 (URL parsing moved to `susi_paths::weburl`; T-SMALLFACADES-2).
/// 2026-11-11: susi-gawd-swarm 20,635 -> 20,116 (T-CARBONLEAN-1, measured):
/// move unchanged amas.rs inline unit suite (519 lines) to src/tests/amas.rs
/// alongside deleting susi-vendor-console-subscriber, susi-vendor-criterion
/// and susi-vendor-sha1 (63 packages removed from Cargo.lock).
/// 2026-11-11: susi-gmcp 25,891 -> 25,889 (T-CARBONLEAN-2, measured): POSIX
/// shell splitting moves to `susi_paths::shlex` alongside deleting
/// `susi-vendor-shlex` and `susi-vendor-tracing-appender` (9 packages removed
/// from Cargo.lock; host packages 237 -> 229).
/// 2026-11-11: susi-gmcp 25,889 -> 25,860 (T-AGYCLOUD-8, measured): delete
/// `tower_service::Service` wrapper on `StreamableHttpService` and call
/// `StreamableHttpService::handle` directly; delete `susi-vendor-tower-service`
/// (host packages 229 -> 228).
const LINE_CEILINGS: [(&str, usize); 8] = [
    ("susi-gawd", 12_653),
    ("susi-gawd-swarm", 20_116),
    ("susi-gawd-agents", 10_443),
    ("susi-gawd-a2a", 3_702),
    ("susi-gemi", 21_171),
    ("susi-gemi-models", 15_945),
    ("susi-gmcp", 25_860),
    ("susi-harness", 39_446),
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
