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
    ("susi-error", 1),
    ("susi-config", 2),
    ("susi-native-client", 2),
    // No SUSI dependencies: a leaf, reachable from every rank above it.
    ("susi-http-transport", 0),
    ("susi-vendor-candle", 2),
    ("susi-adapters-llm", 2),
    ("susi-core", 3),
    ("susi-sandbox-client", 3),
    ("susi-vendor-wasmer", 2),
    ("susi-vendor-mcp", 0),
    ("susi-vendor-mcp-server", 0),
    ("susi-vendor-tantivy", 2),
    ("susi-vendor-fastembed", 2),
    ("susi-vendor-chrome", 2),
    ("susi-vendor-syn", 0),
    ("susi-vendor-cloud", 0),
    ("susi-vendor-agents", 4),
    ("susi-vendor-models", 4),
    ("susi-vendor-web", 4),
    ("susi-sandbox", 4),
    ("susi-leaf-services", 5),
    ("susi-agents", 5),
    ("susi-tools", 4),
    ("susi-gemi-models", 5),
    ("susi-gawd-agents", 5),
    ("susi-gmcp", 4),
    ("susi-server", 4),
    ("susi-dsh-cell", 4),
    ("susi-universal-cell", 4),
    ("susi-gawd-swarm", 6),
    ("susi-gemi", 6),
    ("susi-gawd-a2a", 7),
    ("susi-gawd", 8),
    ("susi-daemon", 9),
    ("susi", 10),
];

/// Pure re-export facades (`susi-vendor-*` crates whose only dependency is
/// the third-party crate they wrap). They are leaves by construction —
/// they depend on no SUSI crate — and are exempt from the leaf-order check:
/// a rank-0 crate like susi-paths legitimately depends on a facade.
/// The zero-external-deps ratchet below is what actually constrains them.
const VENDOR_FACADES: &[&str] = &[
    "susi-vendor-anyhow",
    "susi-vendor-axum",
    "susi-vendor-base64",
    "susi-vendor-bollard",
    "susi-vendor-bytes",
    "susi-vendor-chacha20poly1305",
    "susi-vendor-clap",
    "susi-vendor-console-subscriber",
    "susi-vendor-criterion",
    "susi-vendor-dashmap",
    "susi-vendor-ed25519-dalek",
    "susi-vendor-flume",
    "susi-vendor-futures",
    "susi-vendor-getrandom",
    "susi-vendor-hex",
    "susi-vendor-http-body-util",
    "susi-vendor-hyper",
    "susi-vendor-hyper-util",
    "susi-vendor-indicatif",
    "susi-vendor-jsonschema",
    "susi-vendor-libc",
    "susi-vendor-md-5",
    "susi-vendor-parking-lot",
    "susi-vendor-proptest",
    "susi-vendor-ra2a",
    "susi-vendor-rayon",
    "susi-vendor-rcgen",
    "susi-vendor-regex",
    "susi-vendor-serde",
    "susi-vendor-serde-json",
    "susi-vendor-sha1",
    "susi-vendor-sha2",
    "susi-vendor-shlex",
    "susi-vendor-signal-hook",
    "susi-vendor-sysinfo",
    "susi-vendor-tempfile",
    "susi-vendor-tokio",
    "susi-vendor-tokio-rustls",
    "susi-vendor-tokio-stream",
    "susi-vendor-tower",
    "susi-vendor-tower-service",
    "susi-vendor-tracing",
    "susi-vendor-tracing-appender",
    "susi-vendor-tracing-subscriber",
    "susi-vendor-ureq",
    "susi-vendor-url",
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
                "susi-vendor-wasmer",
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
                "susi-vendor-wasmer",
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
                "susi-vendor-wasmer",
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
                "susi-vendor-wasmer",
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
                "susi-vendor-wasmer",
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
                "susi-vendor-wasmer",
            ],
        ),
        (
            "susi-agents",
            &[
                "susi-gemi",
                "susi-gemi-models",
                "susi-gmcp",
                "susi-gawd",
                "susi-gawd-agents",
                "susi-gawd-swarm",
                "susi-gawd-a2a",
                "susi-tools",
                "susi-daemon",
                "susi-server",
                "susi-sandbox",
                "susi-vendor-wasmer",
            ],
        ),
        (
            "susi-vendor-candle",
            &[
                "susi-core",
                "susi-config",
                "susi-error",
                "susi-paths",
                "susi-vendor-wasmer",
                "susi-native-client",
                "susi-sandbox",
                "susi-sandbox-client",
                "susi-http-transport",
                "susi-adapters-llm",
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
            "susi-adapters-llm",
            &[
                "susi-core",
                "susi-config",
                "susi-error",
                "susi-paths",
                "susi-vendor-wasmer",
                "susi-native-client",
                "susi-sandbox",
                "susi-sandbox-client",
                // `susi-http-transport` is allowed: adapters-llm posts its
                // wire shapes over the one shared transport (`post_json`).
                "susi-vendor-candle",
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
                "susi-vendor-wasmer",
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
                "susi-vendor-wasmer",
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
                "susi-vendor-wasmer",
            ],
        ),
        ("susi-daemon", &["susi-sandbox"]),
        (
            "susi-sandbox",
            &[
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
                "susi-core",
                "susi-vendor-wasmer",
            ],
        ),
        (
            "susi-config",
            &[
                "susi-core",
                "susi-vendor-wasmer",
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
            "susi-vendor-wasmer",
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
        "susi-vendor-wasmer",
    ] {
        assert!(
            !text.contains(forbidden),
            "susi-core must not depend on `{forbidden}` (see ARCHITECTURE.md)"
        );
    }
}

/// No workspace crate may declare a third-party HTTP client crate except the
/// transport and the vendor/adapter crates: outbound JSON HTTP lives in
/// `susi-http-transport`; vendor SDKs (e.g. rmcp's reqwest transport in
/// `susi-vendor-mcp`) live in `susi-vendor-*` / `susi-adapters-*`. An
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

/// Only `susi-vendor-mcp` (client) and `susi-vendor-mcp-server` (server)
/// may declare the `rmcp` SDK. Feature planes depend on those crates.
#[test]
fn mcp_sdk_stays_in_vendor_crates() {
    let root = workspace_root();
    let mut forbidden_in: Vec<String> = std::fs::read_dir(root.join("crates"))
        .expect("crates dir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name != "susi-vendor-mcp" && name != "susi-vendor-mcp-server")
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
            let declared = trimmed.starts_with("rmcp ") || trimmed.starts_with("rmcp=");
            assert!(
                !declared,
                "{crate_name} must not declare `rmcp` (MCP SDK belongs in susi-vendor-mcp / susi-vendor-mcp-server)"
            );
        }
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
        // Facade crates legitimately name their upstream (`pub use ureq::*`
        // inside susi-vendor-ureq is the entire point).
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

#[test]
fn default_extension_manifest_declares_api_version() {
    let path = workspace_root().join("config/extensions/default/manifest.json");
    let text = std::fs::read_to_string(&path).expect("manifest");
    let v: serde_json::Value = serde_json::from_str(&text).expect("json");
    assert_eq!(v["id"], "default");
    assert!(
        v.get("apiVersion").is_some() || v.get("api_version").is_some(),
        "default pack must declare apiVersion"
    );
    assert!(v.get("permissions").is_some());
    assert!(v.get("capabilities").is_some());
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
        let path = file.to_string_lossy().to_string();
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
    let parse = body.find("Cli::parse()").unwrap();
    assert!(
        isolate < parse,
        "dev instance isolation must run first in main, before any other work (Mandate 48)"
    );

    assert!(
        root.join("scripts/susi-release-sync.sh").is_file(),
        "release promotion script missing (Mandate 48)"
    );
}

/// Mandate 48 ("SUSI writes SUSI"): every code-writing path into a SUSI
/// tree carries the self-build contract, and the CI builder is the released
/// susi working on a branch. Targeted guard over the known paths, not a
/// proof that no other path exists.
#[test]
fn self_build_contract_reaches_every_code_writing_path() {
    let root = workspace_root();
    let read = |rel: &str| std::fs::read_to_string(root.join(rel)).unwrap();

    for (rel, needle) in [
        (
            "crates/susi-vendor-agents/src/external/mod.rs",
            "self_build::brief_task",
        ),
        (
            "crates/susi-gawd-swarm/src/dag.rs",
            "self_build::brief_task",
        ),
        (
            "crates/susi-gawd/src/patch_cycle.rs",
            "self_build::VERIFY_COMMAND",
        ),
    ] {
        assert!(
            non_test(&read(rel)).contains(needle),
            "{rel} must route through `{needle}` (Mandate 48)"
        );
    }

    let builder = read(".github/workflows/susi-builder.yml");
    assert!(
        builder.contains("bash ./install.sh"),
        "susi-builder must run the released susi, not a build of the branch it changes"
    );
    assert!(
        !builder.contains("build-gpu.sh"),
        "susi-builder must not bootstrap its builder from the dev tree"
    );
    assert!(
        builder.contains("gh pr create"),
        "susi-builder work must land as a PR"
    );
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
    for (hook, stage) in [
        ("pre-commit", "commit"),
        ("pre-merge-commit", "merge"),
        ("pre-push", "push"),
    ] {
        let body = std::fs::read_to_string(root.join(".githooks").join(hook))
            .unwrap_or_else(|_| panic!("{hook} missing (Mandate 49)"));
        assert!(
            body.contains(&format!("workflow-guard\" {stage}")),
            "{hook} must call workflow-guard {stage} (Mandate 49)"
        );
    }
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

/// Every PR is opened by auto-merge.yml as github-actions[bot]; a
/// `pull_request` run for it is held for approval (`action_required`) and dies
/// jobs-less when the PR merges seconds later. The branch-push run is the gate.
#[test]
fn ci_has_no_bot_pr_triggered_noise_runs() {
    let on_block = |file: &str| -> String {
        let text =
            std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(file))
                .unwrap();
        let start = text.find("\non:").expect("workflow has an on: block");
        let rest = &text[start + 1..];
        let end = rest[3..].find("\n\n").map_or(rest.len(), |i| i + 3);
        rest[..end].to_string()
    };
    assert!(
        !on_block(".github/workflows/test.yml").contains("pull_request"),
        "test.yml must not trigger on pull_request"
    );
    let builder = on_block(".github/workflows/susi-builder.yml");
    let types = builder
        .lines()
        .skip_while(|l| l.trim() != "pull_request:")
        .skip(1)
        .find(|l| l.trim_start().starts_with("types:"))
        .expect("susi-builder pull_request declares types");
    assert_eq!(
        types.trim(),
        "types: [labeled]",
        "susi-builder pull_request must only react to `labeled`"
    );
}
