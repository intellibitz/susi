#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Lean-core ceilings (`docs/lean-core.md`): the four pillars' dependency
//! reach may only shrink. Every cut lowers a constant here in the same change;
//! nothing raises one without a CEO decision recorded in the doc.
//!
//! Per-pillar, category and file ceilings remain removed. The later explicit
//! under-100k socket goal (Vision V-6) has a separate acceptance check, run by
//! `cargo core-size`. It is intentionally not a claim that today's tree meets
//! the goal, nor does a size check establish functional/protocol parity.
//! Mandate 84 makes that goal a standing rule: the complete socket never grows
//! past `COMPLETE_SOCKET_CEILING`, which only goes down until it is 99,999.
//!
//! The reach is measured from `cargo metadata` (normal and build edges, the
//! way the linked product is built; dev edges excluded), so the numbers are
//! what a clean clone compiles for GAWD, GEMI, GMCP and the harness.
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

const CLOSURE_ROOTS: [&str; 4] = ["susi-gawd", "susi-gemi", "susi-gmcp", "susi-harness"];

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
/// 2026-11-11: 138 -> 128 (T-CARBONLEAN-7: `hyper-util`, `tower-layer`,
/// `tower-service`, `futures-util`, `futures-task`, `ipnet`,
/// `percent-encoding`, `core-foundation`, `system-configuration` and
/// `system-configuration-sys` leave Cargo.lock and the pillar closure when
/// `susi-vendor-hyper-util` is replaced by `susi_http_transport::http_conn`).
/// 2026-11-11: 128 -> 124 (T-CARBONLEAN-8: `tempfile` and `fastrand` leave
/// Cargo.lock, and `tempfile`, `fastrand`, `rustix` and `linux-raw-sys` leave
/// the pillar closure when `susi-vendor-tempfile` and `vendor/native-tls`
/// replace external `tempfile` with pure-std exclusive `DirBuilder` guards).
/// 2026-11-11: 124 -> 122 (T-CARBONLEAN-9: deduplicate `susi-vendor-sha2` onto
/// `sha2 0.10.9` and `susi-vendor-getrandom` onto `getrandom 0.2`, removing 9
/// packages from Cargo.lock and dropping `hybrid-array` and `r-efi` from the
/// pillar closure).
/// 2026-11-11: 122 -> 117 (T-CARBONLEAN-10: replace `tokio` `"full"` and
/// `"tracing"` features in `susi-vendor-tokio` and root `Cargo.toml` with the
/// exact required feature set, removing `parking_lot`, `parking_lot_core`,
/// `lock_api`, `scopeguard` and `redox_syscall` from Cargo.lock and the pillar
/// closure).
/// 2026-11-11: 117 -> 115 (T-CARBONLEAN-11: drop `"client"` from `hyper` in
/// `susi-vendor-hyper` (`["http1", "http2", "server"]` only) and replace the
/// single test-only `hyper::client::conn::http2::handshake` in
/// `susi_http_transport::http_conn` with raw RFC 9113 prior-knowledge frames,
/// removing `want` and `try-lock` from Cargo.lock and the pillar closure).
/// 2026-11-11: 115 -> 114 (T-AGYWIN-4: disable `tracing` default `"attributes"`
/// feature in root `Cargo.toml` and `susi-vendor-tracing` (`features = ["std"]`
/// only); zero files use `#[instrument]`, dropping the `tracing-attributes`
/// proc-macro from Cargo.lock and the pillar closure).
/// 2026-11-11: 114 -> 108 (T-CARBONLEAN-13: replace `x509-cert` and
/// `rustls-pki-types` in `susi-vendor-tokio-rustls` and `susi-daemon-runtime`
/// with `#![forbid(unsafe_code)]` RFC 7468 PEM chain decoding and RFC 5280
/// ASN.1 DER `Certificate -> TBSCertificate -> SubjectPublicKeyInfo` validation;
/// removes `x509-cert`, `tls_codec`, `tls_codec_derive`, `der_derive` and
/// `flagset` from Cargo.lock (199 -> 194) and additionally removes
/// `rustls-pki-types` from the four-pillar closure).
/// 2026-11-11: 108 -> 107 (T-CARBONLEAN-14: drop unused `log` dependency from
/// `vendor/native-tls`, removing `log` from Cargo.lock (194 -> 193) and the
/// four-pillar closure).
/// 2026-11-11: 107 -> 99 (T-CARBONLEAN-15: replace external `chacha20poly1305`
/// in `susi-vendor-chacha20poly1305` with `#![forbid(unsafe_code)]` RFC 8439
/// ChaCha20-Poly1305 AEAD using constant-time 5x26-bit Poly1305,
/// `subtle::ConstantTimeEq` and `zeroize::Zeroize`; removes `aead`, `chacha20`,
/// `chacha20poly1305`, `cipher`, `inout`, `opaque-debug`, `poly1305` and
/// `universal-hash` from Cargo.lock (193 -> 185) and brings the four-pillar
/// third-party closure under 100 crates for the first time: 331 -> 99).
/// 2026-11-11: 99 -> 96 (T-CARBONLEAN-16: back `susi-vendor-x25519-dalek`
/// directly with `curve25519-dalek` (`EdwardsPoint::mul_base_clamped`,
/// `MontgomeryPoint::mul_clamped` and constant-time `IsIdentity`) + `zeroize`,
/// removing `x25519-dalek`, `rand_core` and the `zeroize_derive` proc-macro
/// from Cargo.lock (185 -> 182) and the four-pillar closure).
const THIRD_PARTY_CEILING: usize = 96;
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
/// 2026-11-11: 35 -> 34 (susi-vendor-hyper-util deleted, T-CARBONLEAN-7).
/// 2026-11-11: 34 -> 33 (susi-vendor-tracing leaves the pillar closure across
/// susi-core, susi-gemi and susi-sandbox-client, T-AGYWIN-1).
/// 2026-11-12: 33 unchanged on T-CARBONCLOUD-3 (`susi-vendor-tempfile` was a
/// `[dev-dependencies]`-only leaf across 8 crates; `WORKSPACE_MEMBERS_CEILING`
/// ratchets 50 -> 49 and `CARGO_LOCK_PACKAGES_CEILING` ratchets 182 -> 181).
const WORKSPACE_CEILING: usize = 33;
/// Total workspace members in `Cargo.toml` (`cargo metadata` `workspace_members`).
/// 2026-11-12: 50 -> 49 (T-CARBONCLOUD-3: fold `susi-vendor-tempfile` into
/// `susi_paths::tempfile` and delete `crates/susi-vendor-tempfile`;
/// `VENDOR_FACADES` 17 -> 16).
const WORKSPACE_MEMBERS_CEILING: usize = 49;
/// Total resolved packages in `Cargo.lock` (`cargo metadata` `packages`).
/// 2026-11-12: 182 -> 181 (T-CARBONCLOUD-3: `susi-vendor-tempfile` leaves
/// `Cargo.lock` alongside 8 workspace `[dev-dependencies]` facade edges).
const CARGO_LOCK_PACKAGES_CEILING: usize = 181;
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
/// 2026-11-11: 11 -> 10 (susi-gmcp -> susi-vendor-hyper-util replaced by
/// `susi_http_transport::http_conn`, T-CARBONLEAN-7).
/// 2026-11-11: 10 -> 9 (susi-gemi -> susi-vendor-tracing removed, T-AGYWIN-1).
/// 2026-11-12: 9 normal edges unchanged on T-CARBONCLOUD-3 (`susi-vendor-tempfile`
/// removed 8 `[dev-dependencies]` facade edges across `susi-core`, `susi-gawd`,
/// `susi-gemi-models`, `susi-gemi`, `susi-gmcp`, `susi-harness`,
/// `susi-leaf-services` and `susi-tools`).
const DIRECT_FACADE_EDGES_CEILING: usize = 9;
/// The pillar crates whose shipped source size is reported.
const PILLAR_CRATES: [&str; 8] = [
    "susi-gawd",
    "susi-gawd-swarm",
    "susi-gawd-agents",
    "susi-gawd-a2a",
    "susi-gemi",
    "susi-gemi-models",
    "susi-gmcp",
    "susi-harness",
];

/// Unsafe-exposure baselines under `.agents/baseline/` may not multiply. A file
/// count, not a line count.
const UNSAFE_BASELINE_FILES_CEILING: usize = 2;

/// Mandate 84 (`docs/ceo-mandate-84.md`): Susi core stays strictly below
/// 100,000 lines. The complete socket (first-party runtime Rust plus the
/// in-repository vendor facades, counted by `repository_size`) may never exceed
/// this pinned ceiling. A change that shrinks core lowers it to the new measure,
/// or to `COMPLETE_SOCKET_LIMIT` once core is below that. Only a CEO decision
/// recorded in `docs/ceo-mandate-84.md` raises it.
const COMPLETE_SOCKET_CEILING: usize = 300_266;
/// The CEO's limit: strictly below 100,000 physical lines.
const COMPLETE_SOCKET_LIMIT: usize = 99_999;

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
        .filter(|id| CLOSURE_ROOTS.contains(&names[id]))
        .collect();
    assert_eq!(
        roots.len(),
        CLOSURE_ROOTS.len(),
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
/// suites (`#[cfg(test)] #[path = ..]` modules) rather than shipped code. They
/// are left out of a pillar's reported size; inline `mod tests` still count.
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
    let meta = metadata();
    let workspace_members = meta["workspace_members"]
        .as_array()
        .expect("workspace_members")
        .len();
    let lock_packages = meta["packages"].as_array().expect("packages").len();
    let reach = pillar_reach(&meta);
    eprintln!(
        "lean-core: pillars compile {} workspace crates, {} third-party crates, {} direct facade edges (workspace members: {}, Cargo.lock packages: {})",
        reach.workspace.len(),
        reach.third_party.len(),
        reach.direct_facades.len(),
        workspace_members,
        lock_packages
    );
    assert!(
        workspace_members <= WORKSPACE_MEMBERS_CEILING,
        "workspace now declares {workspace_members} members (ceiling {WORKSPACE_MEMBERS_CEILING})"
    );
    assert!(
        lock_packages <= CARGO_LOCK_PACKAGES_CEILING,
        "Cargo.lock now resolves {lock_packages} packages (ceiling {CARGO_LOCK_PACKAGES_CEILING})"
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

/// Shipped Rust lines under each pillar's `src`, printed for the record. Not a
/// ceiling: growth never fails this.
#[test]
fn pillar_source_size_is_reported() {
    let root = workspace_root();
    for krate in PILLAR_CRATES {
        let src = root.join("crates").join(krate).join("src");
        assert!(src.is_dir(), "{krate} has no src directory");
        eprintln!("lean-core: {krate} src = {} lines", rust_lines(&src));
    }
}

fn physical_lines(path: &Path) -> usize {
    let bytes = std::fs::read(path).expect("tracked file should be readable");
    bytes.iter().filter(|&&b| b == b'\n').count()
        + usize::from(!bytes.is_empty() && !bytes.ends_with(b"\n"))
}

fn is_test_or_bench_rust(rel: &str) -> bool {
    // Despite its name, this is an unconditional public runtime module used
    // by release tooling, not a cfg(test)-only suite.
    if rel == "crates/susi-paths/src/test_env.rs" {
        return false;
    }
    let parts: Vec<&str> = rel.split('/').collect();
    let Some((&file, dirs)) = parts.split_last() else {
        return false;
    };
    dirs.iter().any(|seg| {
        matches!(*seg, "tests" | "benches" | "mastery_tests" | "fuzz") || seg.ends_with("_tests")
    }) || file.starts_with("test_")
        || file.ends_with("_test.rs")
        || file.ends_with("_tests.rs")
}

/// Physical lines by repository category (`git ls-files -z`), printed
/// for the record: runtime Rust, vendor facades, tests, tooling, vendored sources
/// and audit trails, with the largest file of each watched kind. Includes
/// untracked, non-ignored files so a new unstaged source file is not invisible.
/// It also holds Mandate 84: core never grows past its pinned ceiling.
#[test]
fn repository_size_is_reported_and_unsafe_baselines_do_not_multiply() {
    complete_socket_never_grows(repository_size());
}

/// Mandate 84: while core is above 100,000 lines it never grows, and once it is
/// below it never reaches 100,000 again.
fn complete_socket_never_grows(lines: usize) {
    assert!(
        lines <= COMPLETE_SOCKET_CEILING,
        "Mandate 84: Susi core's complete socket is {lines} physical Rust lines, above its \
         pinned ceiling {COMPLETE_SOCKET_CEILING}. Core never grows: remove as many runtime \
         lines from core in the same change, or ship an add-on as a plug in \
         susi-commercial-apps (docs/ceo-mandate-84.md)"
    );
    let won = lines.max(COMPLETE_SOCKET_LIMIT);
    if won < COMPLETE_SOCKET_CEILING {
        eprintln!(
            "Mandate 84: core is {lines} lines; lower COMPLETE_SOCKET_CEILING in \
             tests/lean_core_ceilings.rs from {COMPLETE_SOCKET_CEILING} to {won} so the cut stays won"
        );
    }
}

// The ceiling stops at the limit: below 100,000, core may use the room the CEO gave it.
const _: () = assert!(COMPLETE_SOCKET_CEILING >= COMPLETE_SOCKET_LIMIT);

/// Goal acceptance is explicit while the reduction remains in progress. All
/// first-party runtime and facade code counts, not just the pillar crates.
#[test]
#[ignore = "unfinished reduction goal; run cargo core-size to measure acceptance"]
fn complete_socket_is_under_100k_lines() {
    let lines = repository_size();
    assert!(
        lines < 100_000,
        "complete socket has {lines} physical Rust lines; must be strictly below 100000; functionality and protocol parity must also be verified"
    );
}

fn repository_size() -> usize {
    let root = workspace_root();
    let output = Command::new("git")
        .current_dir(&root)
        .args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ])
        .output()
        .expect("git ls-files runs");
    assert!(
        output.status.success(),
        "git ls-files failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let mut first_party_runtime_rust = 0usize;
    let mut vendor_facade_rust = 0usize;
    let mut test_rust = 0usize;
    let mut tooling_rust = 0usize;
    let mut vendor_lines = 0usize;
    let mut agents_lines = 0usize;
    let mut unsafe_baseline_files = 0usize;
    let mut evidence_jsonl_lines = 0usize;
    let mut runtime_by_package: BTreeMap<String, usize> = BTreeMap::new();
    let mut largest_xtask_file = (0usize, String::new());
    let mut largest_gmcp_tools_file = (0usize, String::new());
    let mut largest_gmcp_core_file = (0usize, String::new());
    let mut largest_integration_test_file = (0usize, String::new());
    let mut largest_gmcp_requirements_file = (0usize, String::new());

    for raw in output.stdout.split(|&b| b == 0).collect::<BTreeSet<_>>() {
        if raw.is_empty() {
            continue;
        }
        let rel = String::from_utf8_lossy(raw);
        let path = root.join(rel.as_ref());
        if !path.is_file() {
            continue;
        }
        let lines = physical_lines(&path);
        if rel.starts_with("vendor/") {
            vendor_lines += lines;
        } else if rel.starts_with(".agents/") {
            agents_lines += lines;
            if rel.starts_with(".agents/baseline/unsafe-default") && rel.ends_with(".json") {
                unsafe_baseline_files += 1;
            }
            if rel.starts_with(".agents/gmcp/evidence/") && rel.ends_with(".jsonl") {
                evidence_jsonl_lines += lines;
            }
        } else if rel.ends_with(".rs") {
            if rel.starts_with("crates/susi-gmcp/src/tools/") && lines > largest_gmcp_tools_file.0 {
                largest_gmcp_tools_file = (lines, rel.as_ref().to_owned());
            }
            if (rel == "crates/susi-gmcp/src/tools/core.rs"
                || rel.starts_with("crates/susi-gmcp/src/tools/core/"))
                && lines > largest_gmcp_core_file.0
            {
                largest_gmcp_core_file = (lines, rel.as_ref().to_owned());
            }
            if rel.starts_with("tests/") && lines > largest_integration_test_file.0 {
                largest_integration_test_file = (lines, rel.as_ref().to_owned());
            }
            if (rel == "tests/gmcp_requirements.rs" || rel.starts_with("tests/gmcp_requirements/"))
                && lines > largest_gmcp_requirements_file.0
            {
                largest_gmcp_requirements_file = (lines, rel.as_ref().to_owned());
            }

            if rel.starts_with("xtask/") || rel.starts_with("toolchain/") {
                tooling_rust += lines;
                if rel.starts_with("xtask/") && lines > largest_xtask_file.0 {
                    largest_xtask_file = (lines, rel.into_owned());
                }
            } else if is_test_or_bench_rust(&rel) {
                test_rust += lines;
            } else if rel.starts_with("crates/susi-vendor-") {
                vendor_facade_rust += lines;
                let package = rel.split('/').nth(1).expect("crate path");
                *runtime_by_package.entry(package.to_owned()).or_default() += lines;
            } else {
                first_party_runtime_rust += lines;
                let package = if rel.starts_with("crates/") {
                    rel.split('/').nth(1).expect("crate path")
                } else {
                    "susi (root)"
                };
                *runtime_by_package.entry(package.to_owned()).or_default() += lines;
            }
        }
    }

    eprintln!(
        "lean-core categories: runtime_rust={first_party_runtime_rust}, vendor_facade_rust={vendor_facade_rust}, test_rust={test_rust}, tooling_rust={tooling_rust}, vendor={vendor_lines}, .agents={agents_lines} (append-only evidence={evidence_jsonl_lines}, unsafe_baselines={unsafe_baseline_files} / ceiling {UNSAFE_BASELINE_FILES_CEILING})"
    );
    // A missing/empty inventory cannot satisfy the goal by counting zero.
    // Include ACP explicitly: the historical pillar report predates its crate.
    for package in PILLAR_CRATES.iter().copied().chain([
        "susi (root)",
        "susi-gawd-acp",
        "susi-core",
        "susi-config",
        "susi-daemon",
    ]) {
        assert!(
            runtime_by_package
                .get(package)
                .is_some_and(|&lines| lines > 0),
            "runtime inventory is incomplete: no source counted for {package}"
        );
    }
    let complete_socket = first_party_runtime_rust + vendor_facade_rust;
    eprintln!(
        "lean-core goal: complete_socket_rust={complete_socket}, strict_limit=100000, remaining_to_remove={}, size_goal_met={}",
        complete_socket.saturating_sub(99_999),
        complete_socket < 100_000
    );
    let mut packages: Vec<_> = runtime_by_package.into_iter().collect();
    packages.sort_by(|(a_name, a_size), (b_name, b_size)| {
        b_size.cmp(a_size).then_with(|| a_name.cmp(b_name))
    });
    for (package, lines) in packages {
        eprintln!("lean-core runtime: {package} = {lines} lines");
    }
    for (kind, (lines, file)) in [
        ("tests/", &largest_integration_test_file),
        ("gmcp_requirements", &largest_gmcp_requirements_file),
        ("xtask", &largest_xtask_file),
        ("gmcp tools", &largest_gmcp_tools_file),
        ("gmcp core tools", &largest_gmcp_core_file),
    ] {
        eprintln!("lean-core largest {kind} file: {file} = {lines} lines");
    }

    assert!(
        unsafe_baseline_files <= UNSAFE_BASELINE_FILES_CEILING,
        "unsafe baseline file count reached {unsafe_baseline_files} (ceiling {UNSAFE_BASELINE_FILES_CEILING})"
    );
    complete_socket
}
