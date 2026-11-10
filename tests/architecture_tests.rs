#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]

//! Architecture boundary tests — see `ARCHITECTURE.md`.
//!
//! Parses workspace `Cargo.toml` files (no network) and asserts:
//! - no workspace crate dependency cycles
//! - every SUSI edge points down the AGENTS.md leaf order
//! - forbidden edges (feature-plane isolation / error purity)
//! - ratchets that only decrease (unwired daemon modules, cross-crate
//!   source mounts)

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn parse_workspace_deps(toml: &str) -> HashSet<String> {
    let mut deps = HashSet::new();
    let mut in_deps = false;
    for line in toml.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            // Production edges only — ignore [dev-dependencies] / [target.*.dev-dependencies].
            in_deps = trimmed == "[dependencies]"
                || trimmed.starts_with("[dependencies.")
                || (trimmed.starts_with("[target.")
                    && trimmed.contains("dependencies]")
                    && !trimmed.contains("dev-dependencies]"));
            continue;
        }
        if !in_deps {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("susi-") {
            if let Some(name_end) = rest.find([' ', '=', '{']) {
                let name = format!("susi-{}", &rest[..name_end]);
                deps.insert(name);
            }
        }
        if let Some(idx) = trimmed.find("path = \"crates/") {
            let rest = &trimmed[idx + "path = \"crates/".len()..];
            if let Some(end) = rest.find('"') {
                deps.insert(rest[..end].to_string());
            }
        }
        if let Some(idx) = trimmed.find("path = \"../") {
            let rest = &trimmed[idx + "path = \"../".len()..];
            if let Some(end) = rest.find('"') {
                let name = rest[..end].to_string();
                if name.starts_with("susi-") {
                    deps.insert(name);
                }
            }
        }
    }
    deps
}

fn workspace_graph() -> HashMap<String, HashSet<String>> {
    let root = workspace_root();
    let mut graph = HashMap::new();

    let root_toml = std::fs::read_to_string(root.join("Cargo.toml")).expect("root Cargo.toml");
    graph.insert("susi".to_string(), parse_workspace_deps(&root_toml));

    let crates_dir = root.join("crates");
    for entry in std::fs::read_dir(&crates_dir).expect("crates/") {
        let entry = entry.expect("entry");
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("susi-") {
            continue;
        }
        let toml_path = entry.path().join("Cargo.toml");
        if !toml_path.is_file() {
            continue;
        }
        let text = std::fs::read_to_string(&toml_path).expect("crate Cargo.toml");
        graph.insert(name, parse_workspace_deps(&text));
    }
    graph
}

/// AGENTS.md leaf order (`paths → error → config → core/sandbox → services
/// → daemon`) as ranks: a crate may depend only on strictly lower-ranked
/// SUSI crates, so shared code is reached through a Cargo edge in one
/// direction instead of being copied or source-mounted upward.
const LEAF_RANK: &[(&str, u8)] = &[
    ("susi-abi", 0),
    ("susi-paths", 0),
    ("susi-build-os-ucrt", 0),
    ("susi-build-policy", 0),
    ("susi-cli-defs", 0),
    ("susi-windows-runtime", 0),
    ("susi-error", 1),
    ("susi-config", 2),
    ("susi-native-client", 2),
    // No SUSI dependencies: a leaf, reachable from every rank above it.
    ("susi-http-transport", 0),
    // std + JSON only, no Susi crates; loopback client of the host contract.
    ("susi-wire-client", 0),
    // Black-box client of the host contract: std + JSON only; HTTP framing
    // from susi-wire-client.
    ("susi-conformance", 1),
    ("susi-adapters-llm", 2),
    ("susi-core", 3),
    ("susi-sandbox-client", 3),
    ("susi-vendor-wasmtime", 2),
    ("susi-vendor-syn", 0),
    // Measurement uses the rank-0 parser implementation, not a pure facade.
    // Rank 1 is the lowest valid layer; all non-facade edges still point down.
    ("susi-unsafe-ratchet", 1),
    ("susi-leaf-services", 6),
    ("susi-tools", 5),
    ("susi-gemi-models", 6),
    ("susi-gawd-agents", 6),
    ("susi-gmcp", 5),
    ("susi-server", 5),
    ("susi-gawd-swarm", 7),
    ("susi-gemi", 7),
    ("susi-gawd-a2a", 8),
    ("susi-harness", 9),
    ("susi-gawd", 9),
    // Daemon host runtime with no pillar edge (supervision, NAT, TLS, ...).
    ("susi-daemon-runtime", 4),
    ("susi-daemon", 10),
    ("susi", 11),
];

/// Pure re-export facades (`susi-vendor-*` crates whose only dependency is
/// the third-party crate they wrap). They are leaves by construction —
/// they depend on no SUSI crate — and are exempt from the leaf-order check:
/// a rank-0 crate like susi-paths legitimately depends on a facade.
/// The zero-external-deps ratchet below is what actually constrains them.
const VENDOR_FACADES: &[&str] = &[
    "susi-vendor-anyhow",
    "susi-vendor-axum",
    "susi-vendor-chacha20poly1305",
    "susi-vendor-clap",
    "susi-vendor-ed25519-dalek",
    "susi-vendor-getrandom",
    "susi-vendor-hex",
    "susi-vendor-http-body-util",
    "susi-vendor-hyper",
    "susi-vendor-hyper-util",
    "susi-vendor-libc",
    "susi-vendor-proptest",
    "susi-vendor-rcgen",
    "susi-vendor-serde",
    "susi-vendor-serde-json",
    "susi-vendor-sha2",
    "susi-vendor-signal-hook",
    "susi-vendor-windows-registry",
    "susi-vendor-tempfile",
    "susi-vendor-tokio",
    "susi-vendor-tokio-rustls",
    "susi-vendor-tower",
    "susi-vendor-tower-service",
    "susi-vendor-tracing",
    "susi-vendor-tracing-subscriber",
    "susi-vendor-winapi",
    "susi-vendor-x25519-dalek",
];

#[test]
fn workspace_edges_follow_the_leaf_order() {
    let rank: HashMap<&str, u8> = LEAF_RANK.iter().copied().collect();
    let facades: HashSet<&str> = VENDOR_FACADES.iter().copied().collect();
    let graph = workspace_graph();
    for (package, dependencies) in &graph {
        if facades.contains(package.as_str()) {
            continue; // leaves: they own a third-party crate, depend on no SUSI crate
        }
        let own = *rank
            .get(package.as_str())
            .unwrap_or_else(|| panic!("{package} has no leaf rank in LEAF_RANK"));
        for dependency in dependencies {
            if facades.contains(dependency.as_str()) {
                continue; // facade edge: permitted from every rank
            }
            let theirs = *rank
                .get(dependency.as_str())
                .unwrap_or_else(|| panic!("{dependency} has no leaf rank in LEAF_RANK"));
            assert!(
                theirs < own,
                "{package} (rank {own}) must not depend on {dependency} (rank {theirs}); \
                 edges point down the leaf order only"
            );
        }
    }
}

/// Mandate: susi-* crates declare zero third-party dependencies — every
/// external crate is owned by exactly one `susi-vendor-*` crate (implementation
/// vendors like wasmer/candle, or pure re-export facades in VENDOR_FACADES).
/// This ratchet scans each non-vendor member's `[dependencies]`,
/// `[build-dependencies]` and `[target.*.dependencies]`/`dev-dependencies`
/// sections and requires every declared package to be a workspace member.
#[test]
fn susi_crates_declare_zero_external_dependencies() {
    let root = workspace_root();
    let root_toml = std::fs::read_to_string(root.join("Cargo.toml")).expect("root Cargo.toml");

    // Workspace member names = root package + every members[] dir + xtask.
    let mut members: HashSet<String> = ["susi".to_string(), "xtask".to_string()].into();
    let mut in_members = false;
    for line in root_toml.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            in_members = t == "[workspace]";
        }
        if in_members && t.contains('"') {
            // Members may be several per line: "crates/a", "crates/b", ...
            for entry in t.split('"').skip(1).step_by(2) {
                if let Some(name) = entry.strip_prefix("crates/") {
                    members.insert(name.to_string());
                } else if entry == "xtask" {
                    members.insert("xtask".to_string());
                }
            }
        }
    }

    let mut manifests: Vec<std::path::PathBuf> = vec![root.join("Cargo.toml")];
    manifests.push(root.join("xtask/Cargo.toml"));
    for entry in std::fs::read_dir(root.join("crates")).expect("crates/") {
        let entry = entry.expect("entry");
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with("susi-vendor-") {
            continue; // vendor crates are where external deps belong
        }
        let p = entry.path().join("Cargo.toml");
        if p.is_file() {
            manifests.push(p);
        }
    }
    for path in manifests {
        let text = std::fs::read_to_string(&path).expect("crate Cargo.toml");
        let mut in_deps = false;
        for line in text.lines() {
            let t = line.trim();
            if t.starts_with('[') {
                in_deps = t == "[dependencies]"
                    || t == "[build-dependencies]"
                    || t == "[dev-dependencies]"
                    || (t.starts_with("[target.") && t.contains("dependencies]"));
                continue;
            }
            if !in_deps || t.is_empty() || t.starts_with('#') {
                continue;
            }
            let Some((key, rhs)) = t.split_once('=') else {
                continue;
            };
            let declared = if let Some(i) = rhs.find("package = \"") {
                let rest = &rhs[i + 10..];
                rest.split('"').next().unwrap_or("").to_string()
            } else {
                key.trim().to_string()
            };
            if declared.is_empty() {
                continue;
            }
            assert!(
                members.contains(&declared),
                "{} declares third-party dependency `{declared}` — susi crates \
                 must declare zero external deps; route it through a \
                 susi-vendor-* crate",
                path.display()
            );
        }
    }
}

fn find_cycle(graph: &HashMap<String, HashSet<String>>) -> Option<Vec<String>> {
    let mut visiting = HashSet::new();
    let mut visited = HashSet::new();
    let mut stack = Vec::new();

    fn dfs(
        node: &str,
        graph: &HashMap<String, HashSet<String>>,
        visiting: &mut HashSet<String>,
        visited: &mut HashSet<String>,
        stack: &mut Vec<String>,
    ) -> Option<Vec<String>> {
        if visited.contains(node) {
            return None;
        }
        if visiting.contains(node) {
            let idx = stack.iter().position(|n| n == node).unwrap_or(0);
            let mut cycle = stack[idx..].to_vec();
            cycle.push(node.to_string());
            return Some(cycle);
        }
        visiting.insert(node.to_string());
        stack.push(node.to_string());
        if let Some(deps) = graph.get(node) {
            for dep in deps {
                if graph.contains_key(dep) {
                    if let Some(c) = dfs(dep, graph, visiting, visited, stack) {
                        return Some(c);
                    }
                }
            }
        }
        stack.pop();
        visiting.remove(node);
        visited.insert(node.to_string());
        None
    }

    for node in graph.keys() {
        if let Some(c) = dfs(node, graph, &mut visiting, &mut visited, &mut stack) {
            return Some(c);
        }
    }
    None
}

#[test]
fn no_workspace_crate_cycles() {
    let graph = workspace_graph();
    if let Some(cycle) = find_cycle(&graph) {
        panic!("workspace crate cycle: {}", cycle.join(" -> "));
    }
}

#[test]
fn layer_matrix_forbidden_edges() {
    // ARCHITECTURE.md "must not import" column, enforced per crate.
    // Each entry: (crate, workspace crates it may not depend on).
    // `susi-core` is foundation (rank 3): crates above it depend on the one
    // compiled copy. It stays banned for the crates at or below its rank.
    let forbidden: &[(&str, &[&str])] = &[
        (
            "susi-gmcp",
            &[
                "susi-gawd",
                "susi-gawd-agents",
                "susi-gawd-swarm",
                "susi-gawd-a2a",
                "susi-gemi",
                "susi-gemi-models",
                "susi-tools",
                "susi-agents",
                "susi-daemon",
                "susi-server",
                "susi-sandbox",
                "susi-vendor-wasmtime",
            ],
        ),
        (
            "susi-tools",
            &[
                "susi-gawd",
                "susi-gawd-agents",
                "susi-gawd-swarm",
                "susi-gawd-a2a",
                "susi-gemi",
                "susi-gemi-models",
                "susi-gmcp",
                "susi-agents",
                "susi-daemon",
                "susi-server",
                "susi-sandbox",
                "susi-vendor-wasmtime",
            ],
        ),
        (
            "susi-gawd-agents",
            &[
                "susi-gawd",
                "susi-gawd-swarm",
                "susi-gawd-a2a",
                "susi-gemi",
                "susi-gemi-models",
                "susi-gmcp",
                "susi-tools",
                "susi-agents",
                "susi-daemon",
                "susi-server",
                "susi-sandbox",
                "susi-vendor-wasmtime",
            ],
        ),
        (
            "susi-gawd-swarm",
            &[
                "susi-gawd",
                "susi-gawd-a2a",
                "susi-gemi",
                "susi-gemi-models",
                "susi-gmcp",
                "susi-tools",
                "susi-agents",
                "susi-daemon",
                "susi-server",
                "susi-sandbox",
                "susi-vendor-wasmtime",
            ],
        ),
        (
            "susi-gawd-a2a",
            &[
                "susi-gawd",
                "susi-gemi",
                "susi-gmcp",
                "susi-tools",
                "susi-agents",
                "susi-daemon",
                "susi-server",
                "susi-sandbox",
                "susi-vendor-wasmtime",
            ],
        ),
        (
            "susi-gawd",
            &[
                "susi-gemi",
                "susi-gemi-models",
                "susi-gmcp",
                "susi-tools",
                "susi-agents",
                "susi-daemon",
                "susi-server",
                "susi-sandbox",
                "susi-vendor-wasmtime",
            ],
        ),
        (
            "susi-adapters-llm",
            &[
                "susi-core",
                "susi-config",
                "susi-error",
                "susi-paths",
                "susi-vendor-wasmtime",
                "susi-native-client",
                "susi-sandbox",
                "susi-sandbox-client",
                // `susi-http-transport` is allowed: adapters-llm posts its
                // wire shapes over the one shared transport (`post_json`).
                "susi-tools",
                "susi-agents",
                "susi-gemi",
                "susi-gemi-models",
                "susi-gmcp",
                "susi-gawd",
                "susi-gawd-agents",
                "susi-gawd-swarm",
                "susi-gawd-a2a",
                "susi-daemon",
                "susi-server",
            ],
        ),
        (
            "susi-gemi-models",
            &[
                "susi-gemi",
                "susi-gmcp",
                "susi-gawd",
                "susi-gawd-agents",
                "susi-gawd-swarm",
                "susi-gawd-a2a",
                "susi-tools",
                "susi-agents",
                "susi-daemon",
                "susi-server",
                "susi-sandbox",
                "susi-vendor-wasmtime",
            ],
        ),
        (
            "susi-gemi",
            &[
                "susi-gmcp",
                "susi-gawd",
                "susi-gawd-agents",
                "susi-gawd-swarm",
                "susi-gawd-a2a",
                "susi-tools",
                "susi-agents",
                "susi-daemon",
                "susi-server",
                "susi-sandbox",
                "susi-vendor-wasmtime",
            ],
        ),
        (
            "susi-server",
            &[
                "susi-gawd",
                "susi-gawd-agents",
                "susi-gawd-swarm",
                "susi-gawd-a2a",
                "susi-gemi",
                "susi-gemi-models",
                "susi-gmcp",
                "susi-tools",
                "susi-agents",
                "susi-daemon",
                "susi-sandbox",
                "susi-vendor-wasmtime",
            ],
        ),
        ("susi-daemon", &["susi-sandbox"]),
        (
            "susi-config",
            &[
                "susi-core",
                "susi-vendor-wasmtime",
                "susi-sandbox",
                "susi-tools",
                "susi-agents",
                "susi-gemi",
                "susi-gemi-models",
                "susi-gmcp",
                "susi-gawd",
                "susi-gawd-agents",
                "susi-gawd-swarm",
                "susi-gawd-a2a",
                "susi-daemon",
                "susi-server",
            ],
        ),
        (
            "susi-vendor-wasmtime",
            &[
                "susi-core",
                "susi-tools",
                "susi-agents",
                "susi-sandbox",
                "susi-gemi",
                "susi-gemi-models",
                "susi-gmcp",
                "susi-gawd",
                "susi-gawd-agents",
                "susi-gawd-swarm",
                "susi-gawd-a2a",
                "susi-daemon",
                "susi-server",
            ],
        ),
    ];
    let graph = workspace_graph();
    // Raising the scanner above its parser must not admit rank-0 HTTP/SDK
    // dependencies or arbitrary pure facades into offline evidence tooling.
    let ratchet_allowed = [
        "susi-vendor-syn",
        "susi-vendor-serde",
        "susi-vendor-serde-json",
        "susi-vendor-sha2",
    ];
    for dependency in graph.get("susi-unsafe-ratchet").expect("unsafe ratchet") {
        assert!(
            ratchet_allowed.contains(&dependency.as_str()),
            "susi-unsafe-ratchet must use only parser/serialization/hash dependencies, found {dependency}"
        );
    }
    for (crate_name, banned) in forbidden {
        let deps = graph
            .get(*crate_name)
            .unwrap_or_else(|| panic!("{crate_name} missing from workspace graph"));
        for dep in *banned {
            assert!(
                !deps.contains(*dep),
                "{crate_name} must not depend on {dep} (ARCHITECTURE.md layer matrix)"
            );
        }
    }
}

#[test]
fn susi_error_must_not_depend_on_candle_or_features() {
    let root = workspace_root();
    let text = std::fs::read_to_string(root.join("crates/susi-error/Cargo.toml"))
        .expect("susi-error Cargo.toml");
    for forbidden in [
        "candle",
        "candle-core",
        "reqwest",
        "hyper",
        "wasmer",
        "wasmtime",
        "bollard",
        "rmcp",
        "susi-gemi",
        "susi-gmcp",
        "susi-gawd",
        "susi-core",
    ] {
        assert!(
            !text.lines().any(|l| {
                let t = l.trim();
                t.starts_with(forbidden)
                    || t.contains(&format!("{forbidden} ="))
                    || t.contains(&format!("\"{forbidden}\""))
            }),
            "susi-error must not depend on `{forbidden}`"
        );
    }
}

#[test]
fn susi_core_must_not_depend_on_infra_or_features() {
    let root = workspace_root();
    let text = std::fs::read_to_string(root.join("crates/susi-core/Cargo.toml"))
        .expect("susi-core Cargo.toml");
    for forbidden in [
        "candle",
        "candle-core",
        "reqwest",
        "hyper",
        "wasmer",
        "wasmtime",
        "bollard",
        "rmcp",
        "susi-gemi",
        "susi-gmcp",
        "susi-gawd",
        "susi-gawd-agents",
        "susi-gawd-swarm",
        "susi-gawd-a2a",
        "susi-daemon",
        "susi-server",
        "susi-agents",
        "susi-tools",
        "susi-sandbox",
        "susi-vendor-wasmtime",
    ] {
        assert!(
            !text.contains(forbidden),
            "susi-core must not depend on `{forbidden}` (see ARCHITECTURE.md)"
        );
    }
}

/// No workspace crate may declare a third-party HTTP client crate except the
/// transport and the vendor/adapter crates: outbound JSON HTTP lives in
/// `susi-http-transport`; vendor SDKs live in `susi-vendor-*` /
/// `susi-adapters-*`. An
/// allow-list, so a new crate is covered without editing this test.
#[test]
fn core_os_crates_must_not_declare_http_clients() {
    let root = workspace_root();
    let mut forbidden_in: Vec<String> = std::fs::read_dir(root.join("crates"))
        .expect("crates dir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| {
            name != "susi-http-transport"
                && !name.starts_with("susi-vendor-")
                && !name.starts_with("susi-adapters-")
        })
        .collect();
    forbidden_in.sort();
    for crate_name in forbidden_in {
        let Ok(text) =
            std::fs::read_to_string(root.join(format!("crates/{crate_name}/Cargo.toml")))
        else {
            continue;
        };
        let mut in_deps = false;
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with('[') {
                in_deps = trimmed == "[dependencies]"
                    || trimmed.starts_with("[dependencies.")
                    || (trimmed.starts_with("[target.")
                        && trimmed.contains("dependencies]")
                        && !trimmed.contains("dev-dependencies]"));
                continue;
            }
            if !in_deps {
                continue;
            }
            for client in ["reqwest", "ureq"] {
                let declared = trimmed.starts_with(&format!("{client} "))
                    || trimmed.starts_with(&format!("{client}="));
                assert!(
                    !declared,
                    "{crate_name} must not declare `{client}` (HTTP clients belong in susi-http-transport / adapters / vendor crates)"
                );
            }
        }
    }
}

/// No crate links an MCP SDK: GMCP serves MCP with its own implementation
/// (`crates/susi-gmcp/src/mcp`) and the MCP client is the std JSON-RPC
/// client in `susi-tools`. Neither `rmcp` nor its macro or schema crates may
/// return as a dependency of any workspace crate.
#[test]
fn no_crate_links_an_mcp_sdk() {
    let root = workspace_root();
    let mut crates: Vec<String> = std::fs::read_dir(root.join("crates"))
        .expect("crates dir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    crates.sort();
    let mut manifests: Vec<(String, std::path::PathBuf)> = crates
        .into_iter()
        .map(|name| {
            let path = root.join(format!("crates/{name}/Cargo.toml"));
            (name, path)
        })
        .collect();
    manifests.push(("susi".to_string(), root.join("Cargo.toml")));
    for (crate_name, path) in manifests {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for line in text.lines() {
            let trimmed = line.trim();
            for sdk in ["rmcp", "rmcp-macros", "schemars"] {
                let declared = trimmed.starts_with(&format!("{sdk} "))
                    || trimmed.starts_with(&format!("{sdk}="))
                    || trimmed.contains(&format!("package = \"{sdk}\""));
                assert!(
                    !declared,
                    "{crate_name} must not declare `{sdk}` (GMCP serves MCP itself: crates/susi-gmcp/src/mcp)"
                );
            }
        }
    }
    let lock = std::fs::read_to_string(root.join("Cargo.lock")).expect("Cargo.lock");
    for sdk in ["rmcp", "rmcp-macros"] {
        assert!(
            !lock.contains(&format!("name = \"{sdk}\"")),
            "Cargo.lock must not resolve `{sdk}`"
        );
    }
}

/// Production sources outside `susi-http-transport` must not name `ureq::`
/// or call a vanished `http_agent()` — every outbound request uses
/// `http_call` / `http_call_with_body`.
#[test]
fn ureq_types_stay_inside_http_transport() {
    let root = workspace_root();
    let mut files = Vec::new();
    rust_files(&root.join("crates"), &mut files);
    rust_files(&root.join("src"), &mut files);
    rust_files(&root.join("xtask/src"), &mut files);
    for path in files {
        let rel = path
            .strip_prefix(&root)
            .unwrap_or(path.as_path())
            .to_string_lossy()
            .replace('\\', "/");
        if rel.contains("susi-http-transport/") {
            continue;
        }
        // Facade crates legitimately name their upstream (`pub use <crate>::*`
        // is the entire point of a susi-vendor-* facade).
        if let Some(crate_dir) = rel.split('/').nth(1) {
            if VENDOR_FACADES.contains(&crate_dir) {
                continue;
            }
        }
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            !text.contains("ureq::"),
            "{rel} must not name ureq:: (use susi-http-transport::http_call)"
        );
        assert!(
            !text.contains("http_agent("),
            "{rel} must not call http_agent (use http_call / http_call_with_body)"
        );
    }
}

/// Model-plane crates (gemi-models) must not declare Hugging Face HTTP or
/// tokenizer vendor crates: downloads go through `susi-http-transport`,
/// tokenizers through `susi-vendor-candle`.
#[test]
fn gemi_models_must_not_declare_vendor_http_or_tokenizers() {
    let root = workspace_root();
    let text = std::fs::read_to_string(root.join("crates/susi-gemi-models/Cargo.toml"))
        .expect("susi-gemi-models Cargo.toml");
    let mut in_deps = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_deps = trimmed == "[dependencies]"
                || trimmed.starts_with("[dependencies.")
                || (trimmed.starts_with("[target.")
                    && trimmed.contains("dependencies]")
                    && !trimmed.contains("dev-dependencies]"));
            continue;
        }
        if !in_deps {
            continue;
        }
        for banned in ["reqwest", "tokenizers"] {
            let declared = trimmed.starts_with(&format!("{banned} "))
                || trimmed.starts_with(&format!("{banned}="));
            assert!(
                !declared,
                "susi-gemi-models must not declare `{banned}` (HF HTTP via susi-http-transport, tokenizers via susi-vendor-candle)"
            );
        }
    }
}

#[test]
fn architecture_md_exists() {
    let path = workspace_root().join("ARCHITECTURE.md");
    assert!(
        path.is_file(),
        "ARCHITECTURE.md missing at {}",
        path.display()
    );
    let text = std::fs::read_to_string(&path).expect("read");
    assert!(text.contains("Composition roots"));
    assert!(text.contains("susi-core"));
    assert!(
        text.contains("plane_bus"),
        "ARCHITECTURE.md must document plane_bus as the feature-plane control plane"
    );
}

/// BT-01 as sequenced in zero-UI plan A9: core ships no extension pack, so
/// the manifest schema the host admits is checked on a fixture created inside
/// the test, through the host's own author-side admission check.
#[test]
fn extension_pack_manifest_schema_on_an_in_test_fixture() {
    let dir = std::env::temp_dir().join(format!(
        "susi_arch_pack_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let manifest = |api: &str| {
        serde_json::json!({
            "id": "acme-pack",
            "name": "Acme",
            "version": "1.0.0",
            "apiVersion": api,
            "capabilities": ["catalog.cloud_vendors"],
            "permissions": [],
            "files": {"cloud-vendors.json": "cloud-vendors.json"},
        })
        .to_string()
    };
    std::fs::write(dir.join("manifest.json"), manifest("1")).unwrap();
    std::fs::write(
        dir.join("cloud-vendors.json"),
        r#"[{"id":"acme","aliases":["acme"],"api_key_env":"ACME_API_KEY"}]"#,
    )
    .unwrap();
    let (admitted, digests) = susi_sandbox_client::extensions::validate_pack_dir(&dir)
        .expect("fixture pack is admissible");
    assert_eq!(admitted.id, "acme-pack");
    assert_eq!(admitted.api_version, "1");
    assert!(digests.contains_key("manifest.json"));
    assert!(digests.contains_key("cloud-vendors.json"));
    std::fs::write(dir.join("manifest.json"), manifest("2")).unwrap();
    let err = susi_sandbox_client::extensions::validate_pack_dir(&dir)
        .expect_err("an incompatible apiVersion major is refused");
    assert!(err.contains("incompatible"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn reachability_from_core_stays_downward() {
    // From susi-core, BFS of reverse edges must not reach feature crates
    // that core is forbidden to depend on — sanity on the parsed graph.
    let graph = workspace_graph();
    let mut reverse: HashMap<String, HashSet<String>> = HashMap::new();
    for (from, tos) in &graph {
        for to in tos {
            reverse.entry(to.clone()).or_default().insert(from.clone());
        }
    }
    // Forward: core's transitive workspace deps stay in foundation only.
    let mut seen = HashSet::new();
    let mut q = VecDeque::new();
    q.push_back("susi-core".to_string());
    while let Some(n) = q.pop_front() {
        if !seen.insert(n.clone()) {
            continue;
        }
        if let Some(deps) = graph.get(&n) {
            for d in deps {
                q.push_back(d.clone());
            }
        }
    }
    seen.remove("susi-core");
    for forbidden in [
        "susi-gemi",
        "susi-gmcp",
        "susi-gawd",
        "susi-sandbox",
        "susi-tools",
        "susi-agents",
        "susi-server",
        "susi-daemon",
    ] {
        assert!(
            !seen.contains(forbidden),
            "susi-core transitive workspace deps include `{forbidden}`: {seen:?}"
        );
    }
}

// ── Unwired module ratchet ──────────────────────────────────────────────
//
// A daemon module that no production path reaches is compiled and
// unit-tested but never runs: a feature in name only. Roots are references
// from outside `susi-daemon` (other crates, the root binary) plus
// `lib.rs` re-exports; edges are `crate::`/`super::`/`susi_daemon::` paths
// and brace imports in non-test code. The 2026-09-27 audit found 88 of 119
// modules unreachable; later passes deleted isolated sketches and wired
// modules used by the live composition root. The count may only go down.

fn non_test(text: &str) -> &str {
    text.find("#[cfg(test)]").map_or(text, |i| &text[..i])
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Whether `text` names module `m` through `prefix::m` (word-bounded).
fn names_path(text: &str, prefix: &str, m: &str) -> bool {
    let needle = format!("{prefix}::{m}");
    text.match_indices(&needle).any(|(i, _)| {
        let before_ok = i == 0 || !is_ident_byte(text.as_bytes()[i - 1]);
        let after = text.as_bytes().get(i + needle.len()).copied();
        before_ok && !after.is_some_and(is_ident_byte)
    })
}

/// Whether a `use crate::{…}` / `use super::{…}` group imports `m`.
fn brace_imports(text: &str, m: &str) -> bool {
    ["crate::{", "super::{"].iter().any(|open| {
        text.match_indices(open).any(|(i, _)| {
            let rest = &text[i + open.len()..];
            let group = rest.find('}').map_or(rest, |end| &rest[..end]);
            group
                .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .any(|word| word == m)
        })
    })
}

fn rust_files(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

fn unreachable_daemon_modules() -> Vec<String> {
    let root = workspace_root();
    let src = root.join("crates/susi-daemon/src");
    let mut modules: HashMap<String, String> = HashMap::new();
    for entry in std::fs::read_dir(&src).unwrap().flatten() {
        let path = entry.path();
        let name = path.file_stem().unwrap().to_string_lossy().to_string();
        if path.extension().is_some_and(|e| e == "rs") && name != "lib" && name != "main" {
            let mut text = std::fs::read_to_string(&path).unwrap();
            let mut nested = Vec::new();
            rust_files(&src.join(&name), &mut nested);
            for f in nested {
                text.push_str(&std::fs::read_to_string(f).unwrap());
            }
            modules.insert(name, text);
        }
    }
    let mentions = |text: &str, m: &str| {
        let code = non_test(text);
        ["crate", "super", "susi_daemon"]
            .iter()
            .any(|p| names_path(code, p, m))
            || brace_imports(code, m)
    };
    let mut outside = String::new();
    let mut files = Vec::new();
    rust_files(&root.join("src"), &mut files);
    rust_files(&root.join("crates"), &mut files);
    for f in files {
        if f.starts_with(&src) || !f.components().any(|c| c.as_os_str() == "src") {
            continue;
        }
        outside.push_str(non_test(&std::fs::read_to_string(f).unwrap()));
    }
    let lib = std::fs::read_to_string(src.join("lib.rs")).unwrap();
    let lib_code: String = lib
        .lines()
        .filter(|l| {
            let t = l.trim_start().trim_start_matches("pub ");
            !(t.starts_with("mod ") && t.ends_with(';'))
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mut reached: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<String> = modules
        .keys()
        .filter(|m| {
            names_path(&outside, "susi_daemon", m)
                || names_path(&lib_code, "crate", m)
                || names_path(&lib_code, "self", m)
                || lib_code.contains(&format!("use {m}::"))
                || lib_code.contains(&format!(" {m}::"))
        })
        .cloned()
        .collect();
    while let Some(m) = queue.pop_front() {
        if !reached.insert(m.clone()) {
            continue;
        }
        let text = &modules[&m];
        for other in modules.keys() {
            if other != &m && !reached.contains(other) && mentions(text, other) {
                queue.push_back(other.clone());
            }
        }
    }
    let mut dead: Vec<String> = modules
        .keys()
        .filter(|m| !reached.contains(*m))
        .cloned()
        .collect();
    dead.sort();
    dead
}

#[test]
fn unreachable_daemon_modules_only_decrease() {
    let dead = unreachable_daemon_modules();
    // Ceiling is 0: `len() <= 0` trips clippy::absurd_extreme_comparisons.
    assert!(
        dead.is_empty(),
        "{} susi-daemon modules are unreachable from any production path; \
         wire (a discarded probe does not count) or remove them: {dead:?}",
        dead.len()
    );
}

/// Coverage floor ratchet: every non-facade crate must contain at least one
/// test (`#[test]`, `#[tokio::test]`, or `proptest!`). Pure re-export
/// facades (`susi-vendor-*` crates whose only job is `pub use upstream::*`)
/// are exempt — they carry no logic; their upstreams are tested upstream.
/// Implementation vendor crates (wasmer, candle, agents, models, web,
/// cloud, chrome, mcp-server, …) are NOT exempt: they hold real SUSI code.
#[test]
fn every_non_facade_crate_has_tests() {
    let root = workspace_root();
    let facades: HashSet<&str> = VENDOR_FACADES.iter().copied().collect();
    let mut bare = Vec::new();
    for dir in std::fs::read_dir(root.join("crates")).unwrap().flatten() {
        let crate_dir = dir.path();
        if !crate_dir.join("Cargo.toml").exists() {
            continue;
        }
        let name = crate_dir.file_name().unwrap().to_string_lossy().to_string();
        if facades.contains(name.as_str()) {
            continue;
        }
        let mut text = String::new();
        let mut files = Vec::new();
        rust_files(&crate_dir.join("src"), &mut files);
        rust_files(&crate_dir.join("tests"), &mut files);
        for f in files {
            text.push_str(&std::fs::read_to_string(&f).unwrap());
        }
        if !["#[test]", "#[tokio::test", "proptest!", "#[rstest]"]
            .iter()
            .any(|p| text.contains(p))
        {
            bare.push(name);
        }
    }
    bare.sort();
    assert!(
        bare.is_empty(),
        "every non-facade crate must carry tests; these have none: {bare:?}"
    );
}

// ── Cross-crate source mount ratchet ────────────────────────────────────
//
// A `#[path]` attribute that reaches into another crate's directory compiles
// a second private copy of that code, with its own types and statics. Shared
// code belongs in a crate reached through a Cargo edge. The count may only
// go down.

const CROSS_CRATE_SOURCE_MOUNTS_CEILING: usize = 0;

fn owning_crate(path: &std::path::Path) -> Option<PathBuf> {
    let crates = workspace_root().join("crates");
    let rel = path.strip_prefix(&crates).ok()?;
    rel.components().next().map(|c| crates.join(c.as_os_str()))
}

fn cross_crate_source_mounts() -> Vec<String> {
    let root = workspace_root();
    let mut files = Vec::new();
    rust_files(&root.join("crates"), &mut files);
    let mut mounts = Vec::new();
    for file in files {
        let text = std::fs::read_to_string(&file).unwrap();
        for line in text.lines() {
            let Some(rest) = line.split_once("#[path = \"").map(|(_, rest)| rest) else {
                continue;
            };
            let Some((mount, _)) = rest.split_once('"') else {
                continue;
            };
            let target = file.parent().unwrap().join(mount);
            let target = std::fs::canonicalize(&target).unwrap_or(target);
            let source = std::fs::canonicalize(&file).unwrap_or_else(|_| file.clone());
            if owning_crate(&source) != owning_crate(&target) {
                mounts.push(format!("{} -> {mount}", file.display()));
            }
        }
    }
    mounts.sort();
    mounts
}

#[test]
fn cross_crate_source_mounts_only_decrease() {
    let mounts = cross_crate_source_mounts();
    // Ceiling is 0: `len() <= 0` trips clippy::absurd_extreme_comparisons.
    assert!(
        mounts.is_empty(),
        "{} cross-crate #[path] mounts remain (ceiling {CROSS_CRATE_SOURCE_MOUNTS_CEILING}); \
         depend on the owning crate instead: {mounts:#?}",
        mounts.len()
    );
    eprintln!(
        "cross-crate source mounts: {} (ceiling {CROSS_CRATE_SOURCE_MOUNTS_CEILING})",
        mounts.len()
    );
}

// ── Allow audit trail ───────────────────────────────────────────────────
//
// AGENTS.md: "An #[allow] without a written justification is a compliance
// violation." Every allow of a denied panic-path lint must carry a `//`
// comment on its line, in the comment block right above it, or within the
// next few lines of the item it covers (the body's `Mandate 42: safe`
// note). Test-only files are exempt, as the lints are.

#[test]
fn every_panic_path_allow_is_justified() {
    const LINTS: &[&str] = &[
        "unwrap_used",
        "expect_used",
        "panic",
        "unreachable",
        "todo",
        "unimplemented",
        "wildcard_enum_match_arm",
    ];
    let root = workspace_root();
    let mut files = Vec::new();
    rust_files(&root.join("src"), &mut files);
    rust_files(&root.join("crates"), &mut files);
    let mut unjustified = Vec::new();
    for file in files {
        // Forward slashes on every host: a Windows path spelled with
        // backslashes made every crate's tests/ look like production code.
        let path = file.to_string_lossy().replace('\\', "/");
        if path.contains("/tests/") || path.contains("/benches/") || path.ends_with("_tests.rs") {
            continue;
        }
        let text = std::fs::read_to_string(&file).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            let trimmed = line.trim_start();
            if !(trimmed.starts_with("#[allow(") || trimmed.starts_with("#![allow(")) {
                continue;
            }
            if !LINTS
                .iter()
                .any(|lint| line.contains(&format!("clippy::{lint}")))
            {
                continue;
            }
            let same_line = line.contains("//");
            let above = i > 0 && lines[i - 1].trim_start().starts_with("//");
            let below = lines.iter().skip(i + 1).take(8).any(|l| l.contains("//"));
            if !(same_line || above || below) {
                unjustified.push(format!("{}:{}", path, i + 1));
            }
        }
    }
    assert!(
        unjustified.is_empty(),
        "panic-path #[allow]s without a written justification: {unjustified:?}"
    );
}

/// Mandate 48 regression guard (targeted, not a proof): the two removed
/// dev-install paths must not come back, and dev binaries must select their
/// own instance before anything else runs.
#[test]
fn dev_builds_never_install_the_release_binary() {
    let root = workspace_root();

    let xtask = std::fs::read_to_string(root.join("xtask/src/main.rs")).unwrap();
    let xtask = non_test(&xtask);
    for forbidden in ["fs::copy", "fs::rename", "\".susi\""] {
        assert!(
            !xtask.contains(forbidden),
            "xtask must build only, never install (Mandate 48): found `{forbidden}`"
        );
    }

    let mut files = Vec::new();
    for dir in ["crates", "src", "xtask"] {
        rust_files(&root.join(dir), &mut files);
    }
    let self_deploy: Vec<String> = files
        .iter()
        .filter(|path| {
            let text = std::fs::read_to_string(path).unwrap_or_default();
            let text = non_test(&text);
            text.contains("push_to_hardware") || text.contains("auto_install")
        })
        .map(|path| path.display().to_string())
        .collect();
    assert!(
        self_deploy.is_empty(),
        "dev self-install over ~/.susi/bin is forbidden (Mandate 48): {self_deploy:?}"
    );

    let main = std::fs::read_to_string(root.join("src/main.rs")).unwrap();
    let body = &main[main.find("fn main()").unwrap()..];
    let isolate = body
        .find("dev_instance::isolate_if_dev_build")
        .unwrap_or(usize::MAX);
    let local = body.find("local_delivery_cli::execute").unwrap();
    let install = body.find("install_cli::execute").unwrap();
    let runtime = body.find("match dispatch(cli.command)").unwrap();
    assert!(
        local < isolate && install < isolate && isolate < runtime,
        "native delivery/install dispatch must precede development isolation, which must precede runtime startup"
    );

    assert!(
        root.join("xtask/src/bin/local-release.rs").is_file(),
        "native release qualification and promotion implementation missing"
    );
}

/// Mandate 48 ("SUSI writes SUSI"): every code-writing path into a SUSI
/// tree carries the self-build contract, and installed Susi exposes guarded
/// local delivery. Targeted guard over the known paths, not a
/// proof that no other path exists.
#[test]
fn self_build_contract_reaches_every_code_writing_path() {
    let root = workspace_root();
    let read = |rel: &str| std::fs::read_to_string(root.join(rel)).unwrap();

    for (rel, needle) in [
        (
            "crates/susi-gawd-agents/src/external/mod.rs",
            "self_build::brief_task",
        ),
        (
            "crates/susi-gawd-agents/src/external_peers.rs",
            "self_build::brief_task",
        ),
        (
            "crates/susi-gawd-swarm/src/dag.rs",
            "self_build::brief_task",
        ),
        (
            "crates/susi-gawd/src/patch_cycle.rs",
            "repo_gate::run_repository_gate",
        ),
    ] {
        assert!(
            non_test(&read(rel)).contains(needle),
            "{rel} must route through `{needle}` (Mandate 48)"
        );
    }

    let delivery = read("src/cli/local_delivery_cli.rs");
    assert!(delivery.contains("xtask/bootstrap/susi-dev.rs"));
    assert!(delivery.contains("bootstrap::run(args)"));
    let bootstrap = read("xtask/bootstrap/susi-dev.rs");
    assert!(bootstrap.contains("CARGO_BUILD_BUILD_DIR"));
    assert!(bootstrap.contains("local-workflow"));
    assert!(bootstrap.contains("local-release"));
}

/// Mandate 49 regression guard: the worktree-workflow guard exists, is
/// executable, and every hook that must enforce it actually calls it.
#[cfg(unix)]
#[test]
fn worktree_workflow_guard_is_wired_into_every_hook() {
    use std::os::unix::fs::PermissionsExt;
    let root = workspace_root();
    let guard = root.join(".githooks/workflow-guard");
    let mode = std::fs::metadata(&guard)
        .expect("workflow-guard missing (Mandate 49)")
        .permissions()
        .mode();
    assert!(mode & 0o111 != 0, "workflow-guard must be executable");
    for (hook, stage) in [("pre-commit", "commit"), ("pre-merge-commit", "merge")] {
        let body = std::fs::read_to_string(root.join(".githooks").join(hook))
            .unwrap_or_else(|_| panic!("{hook} missing (Mandate 49)"));
        assert!(
            body.contains(&format!("workflow-guard\" {stage}")),
            "{hook} must call workflow-guard {stage} (Mandate 49)"
        );
    }
    // Main is unqualified development with no mandatory pre-push gate
    // (docs/ceo-agent-mandate.md, section 3), so pre-push no longer runs the
    // workflow guard's task-compliance and sync checks. It keeps the push
    // safety that guard gave: the native transport policy refuses branch
    // deletion, non-fast-forward branch updates and moved or deleted tags.
    let pre_push = std::fs::read_to_string(root.join(".githooks/pre-push"))
        .expect("pre-push missing (Mandate 49)");
    assert!(
        pre_push
            .lines()
            .any(|line| line.trim() == "exec push-policy \"$@\""),
        "pre-push must exec the native push-policy"
    );
    let policy = std::fs::read_to_string(root.join("xtask/src/push_policy.rs"))
        .expect("xtask/src/push_policy.rs missing");
    for rule in [
        "branch deletion prohibited",
        "\"merge-base\", \"--is-ancestor\"",
        "tags are immutable",
    ] {
        assert!(
            policy.contains(rule),
            "push-policy no longer enforces `{rule}`"
        );
    }
    // The guard is a stub for `susi workflow guard`: the rules are Rust's,
    // reached through `.githooks/run-susi`, which needs sh and git only.
    let guard_body = std::fs::read_to_string(&guard).unwrap_or_default();
    assert!(
        guard_body.contains("run-susi\" workflow guard"),
        "workflow-guard must exec `susi workflow guard` (Mandate 49)"
    );
    let runner = std::fs::metadata(root.join(".githooks/run-susi"))
        .expect("run-susi missing (Mandate 49)")
        .permissions()
        .mode();
    assert!(runner & 0o111 != 0, "run-susi must be executable");
    let setup = std::fs::read_to_string(root.join("scripts/setup-dev.sh")).unwrap_or_default();
    assert!(
        setup.contains("core.hooksPath .githooks"),
        "setup-dev.sh must enable the hooks (Mandate 49)"
    );
}

/// `-Clinker-features=-lld` is stable only on x86_64 Linux; on any other
/// target rustc rejects it, which failed v0.16.0's linux-aarch64 release build.
#[test]
fn linker_features_flag_is_x86_64_linux_only() {
    let text = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(".cargo/config.toml"),
    )
    .unwrap();
    let mut section = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            section = line.to_string();
        } else if !line.starts_with('#') && line.contains("linker-features") {
            assert_eq!(
                section, "[target.x86_64-unknown-linux-gnu]",
                "linker-features is unstable outside x86_64 Linux: {line}"
            );
        }
    }
}

/// The development workflow is a constitutional mandate, not a convention:
/// identity.json (compiled into susi and read by every agent) must carry
/// local-workflow mandates, with real enforcement artifacts and honest gaps.
#[test]
fn workflow_mandates_are_in_identity_and_their_enforcement_exists() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let identity: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(root.join(".agents/identity.json")).unwrap())
            .unwrap();
    let mandates = identity["pillars"]["dna"]["mandates"].as_array().unwrap();
    let text_of = |id: u64| -> String {
        let m = mandates
            .iter()
            .find(|m| m["id"].as_u64() == Some(id))
            .unwrap_or_else(|| panic!("identity.json is missing Mandate {id}"));
        assert!(
            !m["title"].as_str().unwrap_or("").is_empty(),
            "Mandate {id} needs a title"
        );
        m["imperative"].as_str().unwrap_or("").to_string()
    };
    // Existing machinery must be named accurately; the constitution must not
    // pretend an implementation gap is a working gate.
    for (id, phrase, artifact) in [
        (
            49,
            "Reserve disjoint file/directory scopes",
            "xtask/src/bin/local-workflow.rs",
        ),
        (49, "CLAUDE.md", "CLAUDE.md"),
        (50, "Task: T-<AGENT>-<n>", "xtask/src/bin/local-workflow.rs"),
        (
            50,
            "One live claim at a time",
            "xtask/src/bin/local-workflow.rs",
        ),
        (
            51,
            "union the entries",
            ".agents/schemas/evidence.schema.json",
        ),
        (52, "SUSI_HOME", "src/dev_instance.rs"),
        (53, "E2E_CHECKS", "tests/release_e2e_checks.rs"),
        (
            54,
            "actual candidate integration tree",
            "xtask/src/bin/local-workflow.rs",
        ),
        (55, "did-you-mean", "src/cli/intent_guard.rs"),
        (
            56,
            "failed or unfunded model can never be the brain",
            "config/cost-tiers.json",
        ),
        (
            57,
            "max_budget_usd",
            "crates/susi-gemi-models/src/cloud_credentials.rs",
        ),
        (58, "GMCP 2026", "crates/susi-gmcp/src/lib.rs"),
        (
            59,
            "forbid(unsafe_code)",
            "crates/susi-unsafe-ratchet/src/bin/unsafe_ratchet.rs",
        ),
        (60, "susi-vendor-", "tests/architecture_tests.rs"),
        (61, "migration building blocks", "xtask/src/local_build.rs"),
        (68, "AND obligations", "deny.toml"),
    ] {
        assert!(
            text_of(id).contains(phrase),
            "Mandate {id} must state `{phrase}`"
        );
        assert!(
            root.join(artifact).exists(),
            "Mandate {id} names enforcement `{artifact}` which does not exist"
        );
    }
    // Every code-writing agent receives the workflow in its task itself.
    for needle in ["cargo local-workflow", "GitHub is optional", "hermetic"] {
        assert!(
            susi_core::self_build::BRIEF.contains(needle),
            "the self-build brief must carry `{needle}`"
        );
    }
}

/// The brain-lanes markdown board was retired: lane ownership is one JSON file
/// per agent (no shared table to conflict on), its step logs live in the
/// evidence ledger, and nothing may point at the deleted file.
#[test]
fn brain_lanes_board_is_retired_into_data() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    assert!(
        !root.join("docs/brain-lanes.md").exists(),
        "docs/brain-lanes.md must stay deleted; use .agents/lanes/<AGENT>.json and `susi-commercial tasks`"
    );
    let mut agents = Vec::new();
    for entry in std::fs::read_dir(root.join(".agents/lanes")).unwrap() {
        let path = entry.unwrap().path();
        let lane: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let agent = lane["agent"].as_str().unwrap();
        assert_eq!(
            path.file_stem().unwrap().to_string_lossy(),
            agent,
            "lane file name must match its agent"
        );
        assert!(agent
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit()));
        assert!(
            !lane["stages"].as_array().unwrap().is_empty(),
            "{agent} lane needs at least one stage"
        );
        assert!(!lane["files"].as_array().unwrap().is_empty());
        agents.push(agent.to_string());
    }
    agents.sort();
    assert_eq!(agents, ["CLAUDE", "DEVIN"]);
    // Each lane's history must exist in the ledger it now lives in.
    let ledger = std::fs::read_to_string(root.join(".agents/evidence.json")).unwrap();
    for agent in &agents {
        assert!(
            ledger.contains(&format!("EV-{agent}-001")),
            "the ledger must hold {agent}'s history (EV-{agent}-001)"
        );
    }
    for rel in ["ARCHITECTURE.md", "AGENTS.md", "README.md"] {
        let text = std::fs::read_to_string(root.join(rel)).unwrap();
        assert!(
            !text.contains("brain-lanes.md"),
            "{rel} still points at the deleted brain-lanes.md"
        );
    }
}

/// Roadmap vectors say what and why; tasks are how it gets done. Every task
/// link must name a real vector, and every P0 vector must have a task (so the
/// most important work is always reachable from the queue).
#[test]
fn roadmap_vectors_and_tasks_stay_linked() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let roadmap: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(root.join(".agents/roadmap.json")).unwrap())
            .unwrap();
    let vectors: Vec<(String, String)> = roadmap["vectors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| {
            (
                v["id"].as_str().unwrap().to_string(),
                v["priority"].as_str().unwrap_or("").to_string(),
            )
        })
        .collect();
    let mut linked = std::collections::HashSet::new();
    for dir in [".agents/tasks", ".agents/tasks/done"] {
        let Ok(entries) = std::fs::read_dir(root.join(dir)) else {
            continue;
        };
        for e in entries.filter_map(Result::ok) {
            if e.path().extension().is_none_or(|x| x != "json") {
                continue;
            }
            let t: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(e.path()).unwrap()).unwrap();
            if let Some(v) = t["roadmap"].as_str() {
                assert!(
                    vectors.iter().any(|(id, _)| id == v),
                    "{} links roadmap vector {v}, which does not exist",
                    e.path().display()
                );
                linked.insert(v.to_string());
            }
        }
    }
    let uncovered: Vec<&str> = vectors
        .iter()
        .filter(|(id, p)| p == "P0" && !linked.contains(id))
        .map(|(id, _)| id.as_str())
        .collect();
    assert!(
        uncovered.is_empty(),
        "P0 roadmap vectors with no task: {uncovered:?} — `susi-commercial tasks add --roadmap <id> …`"
    );
}

/// Agents read the top of AGENTS.md: it must open with the START HERE block
/// whose first act is the CEO contract and local isolation, before engineering rules.
#[test]
fn agents_md_opens_with_start_here_and_the_workflow_check() {
    let text =
        std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("AGENTS.md"))
            .unwrap();
    let first = text.lines().next().unwrap();
    assert!(
        first.starts_with("**START HERE") && first.contains("CEO mandates"),
        "AGENTS.md line 1 must be the START HERE instruction, got: {first}"
    );
    let start = text.find("START HERE").unwrap();
    let mandates = text.find("# Agent Engineering Mandates").unwrap();
    assert!(start < mandates, "START HERE must precede the mandates");
    for step in [
        "cargo local-workflow",
        "cargo local-build debug",
        "Task: T-<AGENT>-<n>",
        // Main is unqualified development (Mandate 73); exact-tree gates
        // belong to release branches and snapshots.
        "exact-tree gates apply to release branches",
        "CEO never looks at code",
    ] {
        assert!(
            text[..mandates].contains(step),
            "the START HERE block must mention `{step}`"
        );
    }
}

/// The README is the front door: it must cover the workflow, the task queue,
/// the ecosystem and brain commands, and its mandate range must match
/// identity.json — so it cannot silently fall behind again.
#[test]
fn readme_covers_the_workflow_ecosystem_and_brain() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let readme = std::fs::read_to_string(root.join("README.md")).unwrap();
    for needle in [
        "cargo local-workflow",
        "cargo local-build debug",
        "Task: T-<AGENT>-<n>",
        "actual combined tree",
        "CEO never inspects code",
        "susi ecosystem scan",
        "susi keys check",
        "susi brain",
        "SUSI_BUDGET",
        ".agents/tasks/",
        ".agents/lanes/",
    ] {
        assert!(readme.contains(needle), "README.md must cover `{needle}`");
    }
    // "Mandates 49–N": N must be the highest mandate in identity.json.
    let identity: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(root.join(".agents/identity.json")).unwrap())
            .unwrap();
    let max = identity["pillars"]["dna"]["mandates"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["id"].as_u64())
        .max()
        .unwrap();
    let after = readme
        .split("Mandates 49–")
        .nth(1)
        .expect("README names the workflow mandates as `Mandates 49–N`");
    let stated: u64 = after
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .unwrap();
    assert_eq!(
        stated, max,
        "README says Mandates 49–{stated}; identity.json goes to {max}"
    );
}

/// README quickstart may ask a new user for at most two manual commands;
/// the budget may only go down (ratchet).
#[test]
fn zc_quickstart_budget() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let readme = std::fs::read_to_string(root.join("README.md")).unwrap();
    // New-user install only: ## Install until the developing/Mandate-48 section.
    let install = readme
        .split("## Install")
        .nth(1)
        .expect("README has ## Install");
    let end = install
        .find("### Developing susi")
        .or_else(|| install.find("\n## "))
        .unwrap_or(install.len());
    let section = &install[..end];
    let mut cmds = Vec::new();
    let mut in_fence = false;
    for line in section.lines() {
        let t = line.trim();
        if t.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence && !t.is_empty() && !t.starts_with('#') {
            // Count base install one-liners; env-prefix variants of the same
            // pipe are the same manual step for budget purposes.
            let canonical = if t.contains("install.sh") {
                "install.sh | bash".to_string()
            } else if t.contains("install.ps1") {
                "install.ps1".to_string()
            } else {
                t.to_string()
            };
            cmds.push(canonical);
        }
    }
    cmds.sort();
    cmds.dedup();
    const MAX_MANUAL_COMMANDS: usize = 2;
    assert!(
        cmds.len() <= MAX_MANUAL_COMMANDS,
        "README Install asks for {} manual commands (budget {MAX_MANUAL_COMMANDS}): {cmds:?}",
        cmds.len()
    );
}

/// Patched sources ship with the checkout: development must not fetch GitHub.
#[test]
fn vendor_patches_are_local_and_preserve_upstream_licenses() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
    let patch = manifest
        .split("[patch.crates-io]")
        .nth(1)
        .unwrap()
        .split("\n[")
        .next()
        .unwrap();
    for line in patch
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
    {
        assert!(line.contains("path = \"vendor/"), "nonlocal patch: {line}");
        assert!(!line.contains("git ="), "GitHub-dependent patch: {line}");
    }
    let lock = std::fs::read_to_string(root.join("Cargo.lock")).unwrap();
    assert!(
        !lock.contains("source = \"git+"),
        "development sources must resolve without a Git host"
    );
    assert!(
        !lock.contains("[[patch.unused]]"),
        "Cargo.lock must not carry unused [patch.crates-io] entries"
    );
    for retired in ["reqwest", "fluent-uri"] {
        assert!(
            !root.join("vendor").join(retired).exists(),
            "retired unused vendor patch directory returned: vendor/{retired}"
        );
    }
    for (name, license) in [
        ("esaxx-rs", "LICENSE"),
        ("native-tls", "LICENSE-APACHE"),
        ("native-tls", "LICENSE-MIT"),
    ] {
        assert!(root.join("vendor").join(name).join("Cargo.toml").is_file());
        assert!(root.join("vendor").join(name).join(license).is_file());
    }
}
/// The lockfile carries no `zip` with GitHub's five Dependabot advisories
/// (GHSA-m665-v7pm-7grj and four more, all fixed in 9.0.5), and the candle
/// crates that pull it in come from crates.io, not a git revision: candle
/// 0.11.1 wants zip ^9.1.3 and tokenizers ^0.23, which is why the git patch
/// that kept the workspace on one tokenizers could go. A registry crate is
/// also a compiler-cache hit where a git one never is.
#[test]
fn the_lockfile_carries_fixed_zip_and_the_approved_candle_sources() {
    let lock = std::fs::read_to_string(workspace_root().join("Cargo.lock")).unwrap();
    let packages: Vec<&str> = lock.split("[[package]]").collect();
    let field = |package: &str, name: &str| -> Option<String> {
        package
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{name} = ")))
            .map(|value| value.trim_matches('"').to_string())
    };
    let named = |name: &str| -> Vec<&str> {
        packages
            .iter()
            .copied()
            .filter(|package| field(package, "name").as_deref() == Some(name))
            .collect()
    };
    for zip in named("zip") {
        let version = field(zip, "version").unwrap();
        let mut parts = version
            .split('.')
            .map(|part| part.parse::<u64>().unwrap_or(0));
        let triple = (
            parts.next().unwrap_or(0),
            parts.next().unwrap_or(0),
            parts.next().unwrap_or(0),
        );
        assert!(
            triple >= (9, 0, 5),
            "zip {version} carries the Dependabot advisories fixed in 9.0.5"
        );
    }
    for crate_name in [
        "candle-core",
        "candle-nn",
        "candle-transformers",
        "tokenizers",
    ] {
        let found = named(crate_name);
        assert!(
            found.is_empty(),
            "{crate_name} must not remain in the slim core lockfile after susi-vendor-candle migration"
        );
    }
    let manifest = std::fs::read_to_string(workspace_root().join("Cargo.toml")).unwrap();
    assert!(!manifest.contains("candle-core = { path = \"vendor/candle-core\" }"));
    assert!(workspace_root()
        .join("vendor/candle-core/LICENSE")
        .is_file());
}

/// The release installer builds the newest tag from source on this host (CEO
/// decision 2026-11-09 00:22: release binaries are built from git tags; no
/// artifact store) and asks nothing of a git host beyond git itself. The tag
/// comes from `git ls-remote --tags`; its record `releases/<tag>.json` comes
/// from the remote's default branch and must name the commit the tag points
/// at; the build uses the record's profile, features and environment under
/// the tag's toolchain pin; the version is the gate, and the reference
/// checksum is reported, never enforced. The idle wait, the atomic swap, the
/// rollback and the Windows ownership guard stay as they were.
#[test]
fn release_sync_builds_the_release_tag_with_its_recorded_settings() {
    let script =
        std::fs::read_to_string(workspace_root().join("scripts/susi-release-sync.sh")).unwrap();
    for needle in [
        "git ls-remote --tags --refs \"$REMOTE\"",
        "show \"FETCH_HEAD:releases/${tag}.json\"",
        "refusing to build",
        "rust-toolchain.toml",
        "RUSTUP_AUTO_INSTALL=0",
        "--profile \"$profile\"",
        "report_reference \"$built\"",
        "wait_for_idle",
        "swap_in \"$built\"",
        "rollback || true",
        "susi-release-sync-windows.ps1",
    ] {
        assert!(
            script.contains(needle),
            "susi-release-sync.sh lacks {needle:?}"
        );
    }
    for retired in [
        "gh ",
        "github.com",
        "download",
        "SUSI_RELEASE_BUILD",
        "susi-macos-aarch64",
        "susi-linux-aarch64",
        "susi-windows-aarch64",
    ] {
        assert!(
            !script.contains(retired),
            "susi-release-sync.sh still contains {retired:?}"
        );
    }
    let flow = script
        .split("sync_release() {")
        .nth(1)
        .expect("sync_release exists");
    let order = [
        "checkout_tag \"$tag\"",
        "read_record \"$tag\"",
        "build_tag \"$tag\" \"$features\"",
        "expected '${want}'",
        "report_reference \"$built\"",
        "wait_for_idle",
        "swap_in \"$built\"",
    ];
    let mut at = 0;
    for step in order {
        let found = flow[at..]
            .find(step)
            .unwrap_or_else(|| panic!("sync_release runs {step:?} after the steps before it"));
        at += found + step.len();
    }
}

/// Susi is licensed under Apache-2.0 and admits only Apache-2.0, MIT, and
/// BSD-family permissive licenses into its dependency graph. Strong copyleft
/// (GPL-2.0, GPL-3.0, AGPL), weak copyleft (LGPL, MPL-2.0), and license
/// exceptions (such as `auto_generate_cdp` under `GPL-3.0-or-later` or
/// `option-ext` under `MPL-2.0`) are strictly excluded, and every workspace
/// manifest explicitly declares `license = "Apache-2.0"`.
#[test]
fn workspace_enforces_apache_mit_bsd_license_policy() {
    let root = workspace_root();

    // 1. Every workspace crate manifest explicitly declares `license = "Apache-2.0"`.
    let mut manifests = vec![
        root.join("Cargo.toml"),
        root.join("xtask/Cargo.toml"),
        root.join("fuzz/Cargo.toml"),
    ];
    for entry in std::fs::read_dir(root.join("crates")).expect("crates/") {
        let entry = entry.expect("entry");
        let p = entry.path().join("Cargo.toml");
        if p.is_file() {
            manifests.push(p);
        }
    }
    for path in manifests {
        let text = std::fs::read_to_string(&path).expect("read Cargo.toml");
        assert!(
            text.lines()
                .any(|line| line.trim() == "license = \"Apache-2.0\""),
            "{} must explicitly declare `license = \"Apache-2.0\"`",
            path.display()
        );
    }

    // 2. deny.toml has no license exceptions and admits no copyleft or
    //    non-Apache/MIT/BSD licenses.
    let deny = std::fs::read_to_string(root.join("deny.toml")).expect("read deny.toml");
    assert!(
        !deny.contains("[[licenses.exceptions]]"),
        "deny.toml must not grant [[licenses.exceptions]]"
    );
    for forbidden in [
        "GPL",
        "AGPL",
        "LGPL",
        "MPL-2.0",
        "BSL-1.0",
        "CC0-1.0",
        "Unlicense",
        "0BSD",
        "auto_generate_cdp",
    ] {
        assert!(
            !deny.contains(forbidden),
            "deny.toml must not admit {forbidden}"
        );
    }
    for required in [
        "\"Apache-2.0\"",
        "\"MIT\"",
        "\"BSD-2-Clause\"",
        "\"BSD-3-Clause\"",
    ] {
        assert!(
            deny.contains(required),
            "deny.toml must allow permissive license {required}"
        );
    }

    // 3. Cargo.lock contains no reachable GPL/MPL dependencies (`auto_generate_cdp`,
    //    `headless_chrome`, `hf-hub`, `dirs`).
    let lock = std::fs::read_to_string(root.join("Cargo.lock")).expect("read Cargo.lock");
    for excluded_pkg in ["auto_generate_cdp", "headless_chrome", "hf-hub", "dirs"] {
        let needle = format!("name = \"{excluded_pkg}\"\n");
        assert!(
            !lock.contains(&needle),
            "Cargo.lock must not contain excluded package {excluded_pkg}"
        );
    }
}

/// Regression guard for AI-OS bloat pruning: the 35 dead/uncalled stub and
/// duplicate modules removed across `susi-daemon`, `susi-config`, `susi-core`,
/// and `susi-gawd`, plus unused `[workspace.dependencies]` entries, must not
/// return.
#[test]
fn pruned_non_ai_os_bloat_modules_and_deps_stay_removed() {
    let root = workspace_root();
    const PRUNED_DAEMON_MODULES: &[&str] = &[
        "admin",
        "audit_log",
        "budget",
        "cas",
        "cell_snapshot",
        "checkpoint",
        "code_signing",
        "composition_recommender",
        "dashboard",
        "docs_generator",
        "event_log",
        "fork",
        "hot_reload",
        "identity",
        "inspector",
        "leaderboard",
        "load_balancer",
        "logger",
        "metrics",
        "p2p_router",
        "plugins",
        "signal",
        "suspend",
        "task_queue",
        "tool_catalog",
        "tool_proxy",
        "topology",
        "tracing",
        "ttl",
        "webhook_dispatcher",
        "what_if",
        "workflows",
    ];
    for module in PRUNED_DAEMON_MODULES {
        let path = root
            .join("crates/susi-daemon/src")
            .join(format!("{module}.rs"));
        assert!(
            !path.exists(),
            "pruned susi-daemon bloat module must not return: {}",
            path.display()
        );
    }

    for rel in [
        "crates/susi-config/src/zc_installer.rs",
        "crates/susi-config/src/zc_uninstall.rs",
        "crates/susi-config/src/zc_config_overrides.rs",
        "crates/susi-config/src/zc_env_detectable.rs",
        "crates/susi-core/src/abi_bridge.rs",
        "crates/susi-core/src/jit_credentials.rs",
        "crates/susi-error/src/zc_errors_fixable.rs",
        "crates/susi-gawd/src/brain_status.rs",
        "crates/susi-gawd/src/ops_slo.rs",
        "crates/susi-gawd/src/task_edit_guard.rs",
        "crates/susi-gawd/src/update_health.rs",
        "crates/susi-gawd-a2a/src/eco_a2a_security.rs",
        "crates/susi-gawd-swarm/src/capability_market.rs",
        "crates/susi-gawd-swarm/src/zc_lan_cluster.rs",
        "crates/susi-gemi/src/batch_api.rs",
        "crates/susi-gemi/src/eco_capability_routing.rs",
        "crates/susi-gemi/src/status_pages.rs",
        "crates/susi-gmcp/src/eco_mcp_auth.rs",
        "crates/susi-gmcp/src/zc_mcp_autoinstall.rs",
        "crates/susi-harness/src/memory.rs",
        "crates/susi-sandbox-client/src/mission_sandbox_limits.rs",
        "crates/susi-server/src/gateway_embeddings.rs",
        "crates/susi-server/src/web_dashboard.rs",
        "crates/susi-vendor-agents/src/a2a_streaming_client.rs",
        "crates/susi-vendor-agents/src/acp_discovery.rs",
        "crates/susi-vendor-agents/src/a2a_card_cache.rs",
        "crates/susi-vendor-agents/src/zc_a2a_registry.rs",
        "crates/susi-vendor-agents/src/zc_agent_approvals.rs",
        "crates/susi-vendor-agents/src/zc_delegation_dir.rs",
        "crates/susi-vendor-agents/src/zc_workspace_trust.rs",
        "crates/susi-vendor-agents/src/external/aider.rs",
        "crates/susi-vendor-agents/src/external/browser_use.rs",
        "crates/susi-vendor-agents/src/external/byok.rs",
        "crates/susi-vendor-agents/src/external/deerflow.rs",
        "crates/susi-vendor-agents/src/external/gemini_cli.rs",
        "crates/susi-vendor-agents/src/external/openhands.rs",
        "crates/susi-vendor-agents/src/external/openhands_server.rs",
        "crates/susi-vendor-agents/src/external/openviking.rs",
        "crates/susi-vendor-agents/src/external/swe_agent.rs",
        "crates/susi-vendor-models/src/azure_openai_provider.rs",
        "crates/susi-vendor-models/src/bedrock_provider.rs",
        "crates/susi-vendor-models/src/cloud_cli_auth.rs",
        "crates/susi-vendor-models/src/deepseek_default.rs",
        "crates/susi-vendor-models/src/disk_budget.rs",
        "crates/susi-vendor-models/src/eco_gemini.rs",
        "crates/susi-vendor-models/src/eco_openai_chat.rs",
        "crates/susi-vendor-models/src/engine_autopull.rs",
        "crates/susi-vendor-models/src/engine_format_match.rs",
        "crates/susi-vendor-models/src/hf_gated_models.rs",
        "crates/susi-vendor-models/src/hf_token_gate.rs",
        "crates/susi-vendor-models/src/key_detect.rs",
        "crates/susi-vendor-models/src/more_openai_compat.rs",
        "crates/susi-vendor-models/src/multi_gpu_plan.rs",
        "crates/susi-vendor-models/src/ollama_models.rs",
        "crates/susi-vendor-models/src/vertex_provider.rs",
        "crates/susi-vendor-models/src/vllm_launcher.rs",
        "crates/susi-vendor-models/src/zc_disk_default.rs",
        "crates/susi-vendor-models/src/zc_key_autodetect.rs",
    ] {
        let path = root.join(rel);
        assert!(
            !path.exists(),
            "pruned dead module must not return: {}",
            path.display()
        );
    }

    let daemon_manifest = std::fs::read_to_string(root.join("crates/susi-daemon/Cargo.toml"))
        .expect("read susi-daemon Cargo.toml");
    assert!(
        !daemon_manifest.contains("ed25519-dalek"),
        "susi-daemon must not depend on ed25519-dalek after pruning uncalled signing/identity stubs"
    );
    assert!(
        !daemon_manifest.contains("parking_lot") && !daemon_manifest.contains("parking-lot"),
        "susi-daemon must not depend on unused parking_lot"
    );
    let harness_manifest = std::fs::read_to_string(root.join("crates/susi-harness/Cargo.toml"))
        .expect("read susi-harness Cargo.toml");
    assert!(
        !harness_manifest.contains("susi-vendor-fastembed"),
        "susi-harness must not depend on susi-vendor-fastembed after pruning unpersisted memory stub"
    );
    let root_manifest =
        std::fs::read_to_string(root.join("Cargo.toml")).expect("read root Cargo.toml");
    assert!(
        !root_manifest.contains("\"*\".*=.*opt-level = 3")
            && !root_manifest.contains("\"*\" = { opt-level = 3 }"),
        "root Cargo.toml must not force opt-level = 3 across all dev/test dependencies"
    );
    for manifest_rel in [
        "crates/susi-core/Cargo.toml",
        "crates/susi-gawd-a2a/Cargo.toml",
        "crates/susi-gmcp/Cargo.toml",
    ] {
        let manifest =
            std::fs::read_to_string(root.join(manifest_rel)).expect("read crate Cargo.toml");
        assert!(
            !manifest.contains("susi-vendor-models"),
            "{manifest_rel} must not depend on susi-vendor-models after pruning unreferenced eco_* test stubs"
        );
    }

    for py_bridge in [
        "crates/susi-vendor-agents/src/external/python_bridge.rs",
        "crates/susi-vendor-agents/src/external/python_engine.rs",
        "crates/susi-vendor-agents/src/external/autogen.rs",
        "crates/susi-vendor-agents/src/external/crewai.rs",
        "crates/susi-vendor-agents/src/external/e2b.rs",
        "crates/susi-vendor-agents/src/external/haystack.rs",
        "crates/susi-vendor-agents/src/external/langgraph.rs",
        "crates/susi-vendor-agents/src/external/llamaindex.rs",
        "crates/susi-vendor-agents/src/external/openai_agents.rs",
        "crates/susi-vendor-agents/src/external/smolagents.rs",
        "crates/susi-vendor-agents/src/external/temporal.rs",
    ] {
        let path = root.join(py_bridge);
        assert!(
            !path.exists(),
            "Python bridge/engine module must not return in 100% Rust Susi: {}",
            path.display()
        );
    }

    for entry in std::fs::read_dir(root.join("scripts")).expect("read scripts/") {
        let entry = entry.expect("scripts entry");
        let name = entry.file_name().to_string_lossy().to_string();
        assert!(
            !name.ends_with(".py"),
            "scripts/ must contain zero Python scripts in 100% Rust Susi: found scripts/{name}"
        );
    }
}

/// New policy must preserve citable historical identities, cover all CEO goals,
/// and keep the requirement/implementation distinction explicit.
#[test]
fn ceo_mandates_are_complete_and_keep_evidence_honest() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let identity: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(root.join(".agents/identity.json")).unwrap())
            .unwrap();
    let mandates = identity["pillars"]["dna"]["mandates"].as_array().unwrap();
    let ids: Vec<_> = mandates.iter().map(|m| m["id"].as_u64().unwrap()).collect();
    assert_eq!(
        ids,
        (1..=73).collect::<Vec<_>>(),
        "mandate IDs must not be lost, duplicated or renumbered"
    );
    for (id, phrase) in [
        (62, "CEO never looks at code"),
        (63, "full shipped dependency closure"),
        (64, "native MSVC"),
        (65, "Measure cold/warm build time"),
        (66, "without GitHub"),
        (67, "roll back on failure"),
        (68, "never relabel third-party code"),
        (69, "measured justification"),
        (70, "without user-written config"),
        (71, "earns its next upgrade"),
        (72, "vanilla Windows x86_64 and vanilla Linux x86_64"),
        (73, "Three isolated laptops"),
    ] {
        assert!(
            mandates[(id - 1) as usize]["imperative"]
                .as_str()
                .unwrap()
                .contains(phrase),
            "Mandate {id} lost its requirement: {phrase}"
        );
    }
    let agents = std::fs::read_to_string(root.join("AGENTS.md")).unwrap();
    assert!(agents.contains("not claims of completed implementation"));
    for forbidden in [
        "Do not push to `main`, tag, or cut a release",
        "scripts/susi-release-sync.sh` is the only path in",
        "No offline freshness or unavailable claim snapshot",
    ] {
        assert!(
            !agents.contains(forbidden),
            "legacy remote-only prerequisite returned: {forbidden}"
        );
    }
}

#[test]
fn ceo_independent_agents_mandate_is_discoverable_and_preserves_boundaries() {
    let root = workspace_root();
    for path in [
        "AGENTS.md",
        ".agents/identity.json",
        "CLAUDE.md",
        "GEMINI.md",
        "crates/susi-core/src/self_build.rs",
    ] {
        let text = std::fs::read_to_string(root.join(path)).unwrap();
        assert!(
            text.contains("docs/ceo-agent-mandate.md"),
            "{path}: consolidated mandate must be discoverable"
        );
    }
    let doc = std::fs::read_to_string(root.join("docs/ceo-agent-mandate.md")).unwrap();
    for phrase in [
        "dot, opus, carbon and grok",
        "available, authorized local driver",
        "dot, duet",
        "every Susi container component",
        "a driver can be an engine, an engine can be a driver",
        "cycle detection, termination/cancellation",
        "cannot enlarge\nits authority",
        "independently in full autonomous mode",
        "arbitrary agent-count limit or fixed push-rate limit",
        "no mandatory gates for main branch pushes",
        "unqualified development integration stream",
        "Release branches and release snapshots require the full exact-tree gates",
        "GitHub, when used, is\nstorage only",
        "task manifest",
        "Feature\nmanifests",
        "immutable snapshot",
        "Branches are **versioned",
        "Branches do not create confidentiality boundaries",
        "GAWD, GEMI, GMCP\nand the harness",
        "Apache-2.0 and free forever",
        "Existing privacy, security, credentials, spending, destructive",
        "Requirements are not evidence of completed implementation",
        "supersedes the fixed remote push cadence",
    ] {
        assert!(doc.contains(phrase), "mandate lost requirement: {phrase}");
    }
    let entry = std::fs::read_to_string(root.join("AGENTS.md")).unwrap();
    assert!(!entry.contains("One push per gated cycle (about 20 minutes"));
    assert!(!entry.contains("the gate identity is the only pusher on `main`"));
}

#[test]
fn ceo_git_authority_preserves_evidence_and_scope() {
    let root = workspace_root();
    for path in [
        "AGENTS.md",
        ".agents/identity.json",
        "CLAUDE.md",
        "GEMINI.md",
        "crates/susi-core/src/self_build.rs",
    ] {
        let text = std::fs::read_to_string(root.join(path)).unwrap();
        assert!(
            text.contains("commit, push and merge"),
            "{path}: Git authority missing"
        );
        assert!(
            text.contains("routine CEO approval"),
            "{path}: CEO approval burden returned"
        );
        assert!(
            text.contains("serialized integration") || text.contains("serializes merges"),
            "{path}: parallel merge safety missing"
        );
        assert!(
            text.contains("production") && text.contains("gates"),
            "{path}: evidence gates missing"
        );
        assert!(
            text.contains("destructive") && text.contains("external publication"),
            "{path}: authorization boundary missing"
        );
    }
}

/// Open-core boundary (AGENTS.md): organisation-scale capabilities are sold in
/// susi-commercial-apps and must not return to the Apache-2.0 substrate.
#[test]
fn enterprise_capabilities_live_in_commercial_apps() {
    let root = workspace_root();
    for rel in [
        "crates/susi-gawd/src/tenant_isolation.rs",
        "helm",
        "deploy/helm",
        "scripts/check-helm-chart.sh",
        "crates/susi-config/src/fleet_rollout.rs",
        "crates/susi-config/src/tests/vc_201_068.rs",
        "crates/susi-config/src/tests/vc_201_068_mastery.rs",
        "tests/vc_201_068_mastery.rs",
        "crates/susi-server/src/gateway_keys.rs",
        "crates/susi-vendor-cloud/src/autoscale.rs",
        "crates/susi-vendor-cloud/src/failover_policy.rs",
        "crates/susi-vendor-cloud/src/orphan_gc.rs",
        "crates/susi-vendor-models/src/deployment_spec.rs",
        "crates/susi-vendor-models/src/workload_drain.rs",
        "crates/susi-config/src/managed_drift.rs",
        "crates/susi-config/src/plan_apply.rs",
        "crates/susi-config/src/state_backup.rs",
        "crates/susi-config/src/keyring_storage.rs",
        "crates/susi-core/src/capability_contract.rs",
        "crates/susi-core/src/memory_replicate.rs",
        "crates/susi-core/src/memory_tombstone.rs",
        "crates/susi-core/src/otel_export.rs",
        "crates/susi-core/src/prompt_secret_scan.rs",
        "crates/susi-core/src/retrieval_eval.rs",
        "crates/susi-core/src/skill_artifacts.rs",
        "crates/susi-core/src/vertical_workflow.rs",
        "crates/susi-core/src/wasm_grants.rs",
        "crates/susi-gawd-swarm/src/brain_coordination.rs",
        "crates/susi-gawd-swarm/src/coordinator_fence.rs",
        "crates/susi-gawd-swarm/src/federation_compat.rs",
        "crates/susi-gawd-swarm/src/locality_placement.rs",
        "crates/susi-gawd-swarm/src/membership_model.rs",
        "crates/susi-gawd-swarm/src/snapshot_bootstrap.rs",
        "crates/susi-gawd-swarm/src/wan_peers.rs",
        "crates/susi-gawd-swarm/src/workspace_txn.rs",
        "crates/susi-gawd/src/brain_gap_publish.rs",
        "crates/susi-gawd/src/brain_gap_triggers.rs",
        "crates/susi-gawd/src/model_rate_arbitration.rs",
        "crates/susi-gemi/src/structured_output.rs",
        "crates/susi-gemi/src/eco_compat_gate.rs",
        "crates/susi-vendor-models/src/eco_conflicts.rs",
        "crates/susi-vendor-models/src/eco_coverage.rs",
        "crates/susi-vendor-models/src/eco_matrix.rs",
        "crates/susi-vendor-models/src/eco_provenance.rs",
        "crates/susi-vendor-models/src/eco_signed_kb.rs",
        "crates/susi-vendor-models/src/signed_catalog.rs",
        "crates/susi-vendor-models/src/zc_key_aliases.rs",
        "crates/susi-vendor-models/src/zc_key_sources.rs",
        "crates/susi-vendor-models/src/infer_batch.rs",
        "crates/susi-vendor-models/src/eco_engine_capability_map.rs",
        "crates/susi-vendor-models/src/eco_live_probe.rs",
        "crates/susi-vendor-models/src/eco_gpu_clouds.rs",
        "crates/susi-vendor-agents/src/agent_scoreboard.rs",
        "crates/susi-gawd-agents/src/brain_ranking.rs",
        "crates/susi-gawd-agents/src/cloud_brain_policy.rs",
        "crates/susi-gawd-swarm/src/brain_supervisor.rs",
        "crates/susi-gawd-swarm/src/agent_routing.rs",
        "crates/susi-gawd-swarm/src/chaos_deployment.rs",
        "crates/susi-gawd-swarm/src/fault_matrix.rs",
        "crates/susi-gawd-swarm/src/swarm_chaos.rs",
        "crates/susi-gawd/src/brain_gap_dedup.rs",
        "crates/susi-gawd/src/brain_gap_tasks.rs",
        "crates/susi-gawd/src/brain_gap_audit.rs",
        "crates/susi-gawd/src/disaster_recovery.rs",
        "crates/susi-gawd/src/action_history.rs",
        "crates/susi-gawd/src/autonomous_builder.rs",
        "crates/susi-gawd/src/capacity_limits.rs",
        "crates/susi-gawd/src/cloud_rsi_delivery.rs",
        "crates/susi-gawd/src/cloud_rsi_outcomes.rs",
        "crates/susi-gawd/src/cloud_rsi_lifecycle.rs",
        "crates/susi-gawd/src/formal_invariants.rs",
        "crates/susi-gawd/src/orchestrator_tier.rs",
        "crates/susi-gemi/src/rate_limit_scheduler.rs",
        "crates/susi-gemi/src/thermal_routing.rs",
        "crates/susi-config/src/env_profiles.rs",
        "crates/susi-config/src/rekey_schedule.rs",
        "crates/susi-config/src/selfheal.rs",
        "crates/susi-config/src/zc_bare_susi.rs",
        "crates/susi-config/src/zc_qualification.rs",
        "crates/susi-config/src/zc_susi_fix.rs",
        "crates/susi-config/src/zc_debt_ratchet.rs",
        "crates/susi-config/src/windows_environment.rs",
        "crates/susi-core/src/secret.rs",
        "crates/susi-vendor-models/src/cloud_reconciler.rs",
        "crates/susi-vendor-models/src/offline_bundle.rs",
        "crates/susi-config/src/offline_bundle.rs",
        "crates/susi-vendor-cloud/src/preemptible.rs",
        "crates/susi-error/src/redact_tests.rs",
    ] {
        assert!(
            !root.join(rel).exists(),
            "enterprise capability belongs in susi-commercial-apps: {rel}"
        );
    }
    let handler = std::fs::read_to_string(root.join("crates/susi-gawd/src/plane_handler.rs"))
        .expect("read GAWD plane handler");
    assert!(
        handler.contains("MULTI_TENANT_COMMERCIAL") && !handler.contains("tenant_isolation"),
        "tenant-scoped plane requests must fail closed in the open substrate"
    );
}

// ---------------------------------------------------------------------------
// Zero-UI boundary (CEO directive): core is a pure Rust CLI plus a standards
// protocol socket. It ships no GUI/TUI, no web assets, serves no HTML and
// carries no plugin content; every UI and plugin lives in
// susi-commercial-apps. Plan: susi-commercial-apps docs/zero-ui-migration-plan.md
// (items A1-A7).
// ---------------------------------------------------------------------------

/// Directories a zero-UI scan never descends into: build output, patched
/// upstream sources (`vendor/`), and tool/agent metadata (hidden dirs other
/// than the project's own hook and cargo configuration).
fn zero_ui_skips_dir(name: &str) -> bool {
    matches!(name, "target" | "out" | "vendor" | "node_modules")
        || (name.starts_with('.') && !matches!(name, ".githooks" | ".cargo"))
}

fn zero_ui_walk(dir: &std::path::Path, skip_vendor_facades: bool, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if path.is_dir() {
            if zero_ui_skips_dir(&name) || (skip_vendor_facades && name.starts_with("susi-vendor-"))
            {
                continue;
            }
            zero_ui_walk(&path, skip_vendor_facades, out);
        } else {
            out.push(path);
        }
    }
}

fn zero_ui_rel(path: &std::path::Path) -> String {
    path.strip_prefix(workspace_root())
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// BT-05: no GUI or TUI toolkit (nor a terminal spinner) is declared by any
/// first-party manifest, the `susi-vendor-*` facades included (they are where
/// a third-party dependency would enter). `vendor/` holds patched upstream
/// sources and is not scanned.
#[test]
fn core_declares_no_gui_or_tui_crates() {
    const BANNED: &[&str] = &[
        "ratatui",
        "crossterm",
        "termion",
        "egui",
        "eframe",
        "iced",
        "tauri",
        "gtk",
        "winit",
        "slint",
        "dioxus",
        "indicatif",
    ];
    let mut files = Vec::new();
    zero_ui_walk(&workspace_root(), false, &mut files);
    let manifests: Vec<PathBuf> = files
        .into_iter()
        .filter(|path| path.file_name().is_some_and(|name| name == "Cargo.toml"))
        .collect();
    assert!(
        manifests.len() > 10,
        "manifest walk found too few Cargo.toml files"
    );
    let mut offenders = Vec::new();
    for manifest in &manifests {
        let text = std::fs::read_to_string(manifest).expect("read Cargo.toml");
        let mut in_deps = false;
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with('[') {
                let header = trimmed.trim_matches(|c| c == '[' || c == ']');
                in_deps = header.ends_with("dependencies");
                // `[dependencies.<name>]` table form.
                if let Some((_, name)) = header.split_once("dependencies.") {
                    if BANNED.contains(&name.trim()) {
                        offenders.push(format!("{}: {trimmed}", zero_ui_rel(manifest)));
                    }
                }
                continue;
            }
            if !in_deps || trimmed.starts_with('#') {
                continue;
            }
            let key = trimmed
                .split(['=', ' ', '.'])
                .next()
                .unwrap_or_default()
                .trim_matches('"');
            let package = trimmed
                .split("package")
                .nth(1)
                .and_then(|rest| rest.split('"').nth(1))
                .unwrap_or_default();
            if BANNED.contains(&key) || BANNED.contains(&package) {
                offenders.push(format!("{}: {trimmed}", zero_ui_rel(manifest)));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "core has zero UI: GUI/TUI/spinner crates belong in susi-commercial-apps:\n{}",
        offenders.join("\n")
    );
}

/// BT-03: no web or front-end asset ships in core. `target/`, `out/`,
/// `vendor/`, tool/agent metadata (`.git/`, `.claude/`, `.agents/`, ...) and
/// the `susi-vendor-*` facade crates are not scanned. No test-fixture
/// exception is granted: no `tests/fixtures` UI asset exists, so none is
/// tolerated.
#[test]
fn core_ships_zero_ui_assets() {
    const UI_EXTENSIONS: &[&str] = &[
        "html", "htm", "css", "js", "mjs", "cjs", "jsx", "ts", "tsx", "vue", "svelte",
    ];
    let mut files = Vec::new();
    zero_ui_walk(&workspace_root(), true, &mut files);
    assert!(files.len() > 100, "asset walk found too few files");
    let offenders: Vec<String> = files
        .iter()
        .filter(|path| {
            path.extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| UI_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
        })
        .map(|path| zero_ui_rel(path))
        .collect();
    assert!(
        offenders.is_empty(),
        "core ships zero UI assets; move these to susi-commercial-apps:\n{}",
        offenders.join("\n")
    );
}

/// BT-04: no first-party, non-test Rust source serves HTML: no `text/html`
/// content type, no axum `Html` response and no HTML document literal.
/// Test modules (`#[cfg(test)]` onward), `tests/` trees and the
/// `susi-vendor-*` facades are not scanned. The launchd plist in
/// `susi-gawd/src/cross_platform_service.rs` carries an XML `<!DOCTYPE plist`,
/// which is not an HTML document and does not match the ban, so no allow-list
/// entry is needed.
#[test]
fn core_serves_no_html() {
    let root = workspace_root();
    let mut files = Vec::new();
    for dir in ["src", "crates", "xtask"] {
        zero_ui_walk(&root.join(dir), true, &mut files);
    }
    let mut offenders = Vec::new();
    for path in files
        .iter()
        .filter(|path| path.extension().is_some_and(|ext| ext == "rs"))
    {
        let rel = zero_ui_rel(path);
        if rel.split('/').any(|part| part == "tests") || rel.ends_with("/tests.rs") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let code = non_test(&text);
        let lower = code.to_ascii_lowercase();
        let html_type = code.match_indices("Html").any(|(i, _)| {
            let before_ok = i == 0 || !is_ident_byte(code.as_bytes()[i - 1]);
            let after = code.as_bytes().get(i + "Html".len()).copied();
            before_ok && matches!(after, Some(b'(' | b'<' | b';' | b','))
        });
        if lower.contains("text/html") || lower.contains("<!doctype html") || html_type {
            offenders.push(rel);
        }
    }
    assert!(
        offenders.is_empty(),
        "core serves no HTML (JSON/SSE/MCP/A2A only); offending sources:\n{}",
        offenders.join("\n")
    );
}

/// BT-02: the UI and plugin content removed from core must not return; it
/// lives in susi-commercial-apps (or, for the DeepSeek Harness plugin, is
/// retired in favour of a native commercial desktop view).
#[test]
fn ui_and_plugins_live_in_commercial_apps() {
    let root = workspace_root();
    for rel in [
        // A2: the HTML cockpit panel and its standalone server binary.
        "crates/susi-harness/src/panel.html",
        "crates/susi-harness/src/bin/harness-cockpit.rs",
        // A3: the DeepSeek Harness plugin and its asset test.
        "deploy/dsh-cockpit",
        "tests/cockpit_plugin.rs",
        // A4: the terminal spinner facade.
        "crates/susi-vendor-indicatif",
        // A7: dead or vendor-specific plugin code.
        "deploy/harness/plugins.example.json",
        "crates/susi-harness/src/plugins.rs",
        "crates/susi-dsh-cell",
        // A9: the bundled default extension pack. (`config/cloud-vendors.json`
        // is not listed: it is now core's compiled vendor-alias default, which
        // the key resolver depends on — plan NP-04.)
        "config/extensions",
    ] {
        assert!(
            !root.join(rel).exists(),
            "UI and plugin content belongs in susi-commercial-apps, not core: {rel}"
        );
    }
    let root_toml = std::fs::read_to_string(root.join("Cargo.toml")).expect("root Cargo.toml");
    for member in ["crates/susi-dsh-cell", "crates/susi-vendor-indicatif"] {
        assert!(
            !root_toml.contains(&format!("\"{member}\"")),
            "{member} must not be a workspace member"
        );
    }
    let harness_lib = std::fs::read_to_string(root.join("crates/susi-harness/src/lib.rs"))
        .expect("read susi-harness lib.rs");
    assert!(
        !harness_lib.contains("pub mod plugins;"),
        "the harness plugin manifest module must not return"
    );
}

/// Every first-party, non-test Rust source in core (`src/` and `crates/`;
/// third-party `vendor/` is not ours).
fn first_party_non_test_sources() -> Vec<(String, String)> {
    let root = workspace_root();
    let mut paths = Vec::new();
    rust_files(&root.join("src"), &mut paths);
    rust_files(&root.join("crates"), &mut paths);
    let mut out = Vec::new();
    for path in paths {
        let rel = zero_ui_rel(&path);
        if rel
            .split('/')
            .any(|part| part == "tests" || part == "benches")
            || rel.ends_with("/tests.rs")
        {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        out.push((rel, non_test(&text).to_string()));
    }
    out
}

/// A8: core never launches third-party code on its own. No boot-time
/// leading-MCP auto-enable, no essential-MCP auto-link (nor its plane-bus
/// topic), no registry-driven provisioning inside capability-gap
/// resolution, and no remote MCP registry refresh. Admission is explicit
/// only: `susi mcp-add`, `susi mcp add/enable`, `susi extensions load` — the
/// user or the commercial desktop asks, core admits. Scope is A8's: MCP
/// servers and packages. The daemon's boot-time wake of an installed local
/// inference engine was removed; `tests/core_boot_starts_nothing.rs` guards it.
#[test]
fn core_never_autolaunches_third_party_servers() {
    let banned = [
        "auto_enable_ready",
        "auto_link_essential",
        "auto_configure_server",
        "provision_tool_package",
        "tools.registry.auto_link",
        "mcp_registry_url",
    ];
    let mut offenders = Vec::new();
    for (rel, code) in first_party_non_test_sources() {
        for name in banned {
            // `susi-gmcp`'s `auto_configure_server` is an inert stub behind
            // the explicit `mcp_configure` tool: it launches and writes
            // nothing, and its source is bound to the recorded VC-203 GMCP
            // evidence digest, so it is left as is.
            if name == "auto_configure_server" && rel.starts_with("crates/susi-gmcp/src/") {
                continue;
            }
            // The retired key is listed only so upgraded configs heal it away.
            if name == "mcp_registry_url" && rel == "crates/susi-config/src/validate.rs" {
                continue;
            }
            if code.contains(name) {
                offenders.push(format!("{rel}: {name}"));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "core must not launch or provision third-party servers on its own:\n{}",
        offenders.join("\n")
    );
    let defaults: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(workspace_root().join("config/config.default.json"))
            .expect("read config.default.json"),
    )
    .expect("config.default.json is JSON");
    assert!(
        defaults.get("mcp_registry_url").is_none(),
        "no remote MCP registry URL: core never refreshes a registry on its own"
    );
}

/// A9: core bundles no launchable vendor catalog. The leading-MCP list, the
/// MCP scout registry, and the execution-agent / agent-engine catalogs ship
/// in extension packs (susi-commercial-apps), never as compiled defaults,
/// and core's `config.default.json` lists no vendor peers or bootstrap MCP
/// servers. Model catalogs, prices and cloud-vendor aliases stay (NP-04,
/// NP-18, NP-19).
#[test]
fn core_bundles_no_launchable_vendor_catalogs() {
    let root = workspace_root();
    let banned = [
        "leading-mcp",
        "mcp.registry",
        "execution-agents",
        "agent-engines",
    ];
    let mut paths = Vec::new();
    rust_files(&root.join("src"), &mut paths);
    rust_files(&root.join("crates"), &mut paths);
    rust_files(&root.join("tests"), &mut paths);
    let mut offenders = Vec::new();
    for path in paths {
        let rel = zero_ui_rel(&path);
        if rel == "tests/architecture_tests.rs" {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for (i, _) in text.match_indices("include_str!") {
            let arg = text[i..].split(')').next().unwrap_or_default();
            if let Some(name) = banned.iter().find(|name| arg.contains(*name)) {
                offenders.push(format!("{rel}: include_str! of {name}"));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "launchable vendor catalogs belong in extension packs, not core:\n{}",
        offenders.join("\n")
    );
    for rel in [
        "config/leading-mcp.json",
        "config/mcp.registry.default.json",
        "config/execution-agents.json",
        "config/agent-engines.json",
    ] {
        assert!(
            !root.join(rel).exists(),
            "{rel} is a launchable vendor catalog; it ships in an extension pack"
        );
    }
    let defaults: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("config/config.default.json"))
            .expect("read config.default.json"),
    )
    .expect("config.default.json is JSON");
    for key in ["external_peer_agents", "bootstrap_mcp_servers"] {
        assert_eq!(
            defaults[key],
            serde_json::json!([]),
            "config.default.json `{key}` must list no vendor entries"
        );
    }
}

/// BT-07: the CLI has no UI subcommands: nothing in `src/` (or the clap
/// definitions in `crates/susi-cli-defs/src/`) clears and redraws the terminal, `top` has no watch loop, no visible command help names a
/// dashboard or a cockpit (hidden aliases such as `sovereign-dashboard` and
/// `harness cockpit` live in `#[command(alias = ...)]`, not in doc comments),
/// and there is no interactive shell: no `Shell` command variant in defs.rs and
/// no `susi> ` prompt anywhere in `src/`. Bare `susi` prints usage; the
/// conversation lives in the Susi desktop app (susi-commercial-apps).
#[test]
fn core_cli_has_no_ui_subcommands() {
    let root = workspace_root();
    let mut sources = Vec::new();
    rust_files(&root.join("src"), &mut sources);
    assert!(!sources.is_empty(), "no Rust sources under src/");
    // D-SP-4 Stage 4 moves the clap help text into susi-cli-defs; scan it too.
    let root_sources = sources.len();
    rust_files(&root.join("crates/susi-cli-defs/src"), &mut sources);
    assert!(
        sources.len() > root_sources,
        "no Rust sources under crates/susi-cli-defs/src; update scanner path if moved"
    );
    for path in &sources {
        let text = std::fs::read_to_string(path).expect("read src file");
        assert!(
            !text.contains("susi> "),
            "{}: interactive `susi> ` prompt; core takes one-shot goals, the conversation lives in the commercial desktop",
            zero_ui_rel(path)
        );
        for escape in [
            "\\x1B[2J",
            "\\x1b[2J",
            "\\u{1b}[2J",
            "\\u{1B}[2J",
            "\\033[2J",
        ] {
            assert!(
                !text.contains(escape),
                "{}: clear-screen escape {escape}; core prints, it never redraws",
                zero_ui_rel(path)
            );
        }
    }
    assert!(
        !root.join("src/cli/shell_cli.rs").exists(),
        "the interactive shell module must not return to core"
    );
    let defs = std::fs::read_to_string(root.join("src/cli/defs.rs")).expect("read defs.rs");
    assert!(
        defs.contains("enum Commands"),
        "src/cli/defs.rs must define `enum Commands`; update scanner path if moved"
    );
    for (number, line) in defs.lines().enumerate() {
        let trimmed = line.trim();
        assert!(
            !(trimmed == "Shell,"
                || trimmed.starts_with("Shell {")
                || trimmed.starts_with("Shell(")),
            "src/cli/defs.rs:{}: a `Shell` command variant; core has no interactive shell",
            number + 1
        );
    }
    // `TopArgs` lives in susi-cli-defs since D-SP-4 Stage 4d;
    // `src/cli/top_cli.rs` keeps only the handler.
    let top = std::fs::read_to_string(root.join("crates/susi-cli-defs/src/top.rs"))
        .expect("read susi-cli-defs top.rs");
    assert!(
        top.contains("struct TopArgs"),
        "crates/susi-cli-defs/src/top.rs must define `struct TopArgs`; update scanner path if moved"
    );
    assert!(
        !top.contains("pub watch") && !top.contains("pub interval"),
        "`susi top` has no watch loop; a live view lives in the commercial desktop"
    );
    // `HarnessCommands` (and its help text) lives in susi-cli-defs since
    // D-SP-4 Stage 4c; `src/cli/harness_cli.rs` keeps only the handlers.
    for rel in ["src/cli/defs.rs", "crates/susi-cli-defs/src/harness.rs"] {
        let text = std::fs::read_to_string(root.join(rel)).expect("read CLI defs");
        let mut visible_help_matches = 0usize;
        for (number, line) in text.lines().enumerate() {
            let trimmed = line.trim();
            let lower = trimmed.to_ascii_lowercase();
            let visible_help = trimmed.starts_with("///")
                || lower.contains("visible_alias")
                || lower.contains("about =");
            if visible_help {
                visible_help_matches += 1;
            }
            assert!(
                !(visible_help && (lower.contains("dashboard") || lower.contains("cockpit"))),
                "{rel}:{}: visible command help names a dashboard/cockpit: {trimmed}",
                number + 1
            );
        }
        assert!(
            visible_help_matches > 0,
            "{rel} yielded zero visible help lines; update scanner path if moved"
        );
    }
}
