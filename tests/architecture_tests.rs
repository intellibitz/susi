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
//! - forbidden edges (domain / error purity)

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
                "susi-native",
                "susi-core",
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
                "susi-native",
                "susi-core",
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
                "susi-native",
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
                "susi-native",
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
                "susi-native",
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
                "susi-native",
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
                "susi-native",
                "susi-core",
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
                "susi-native",
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
                "susi-native",
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
                "susi-native",
                "susi-core",
            ],
        ),
        ("susi-daemon", &["susi-sandbox", "susi-native"]),
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
                "susi-config",
                "susi-paths",
                "susi-error",
                "susi-native",
            ],
        ),
        (
            "susi-config",
            &[
                "susi-core",
                "susi-native",
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
            "susi-native",
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
        "susi-native",
    ] {
        assert!(
            !text.contains(forbidden),
            "susi-core must not depend on `{forbidden}` (see ARCHITECTURE.md)"
        );
    }
    // susi-core has no remaining workspace deps: susi-config/susi-paths/
    // susi-error are all vendored as local modules (`susi_config`/
    // `susi_paths`/`susi_error` under src/) that reach the standalone
    // services over HTTP — they no longer appear as workspace deps.
    let deps = parse_workspace_deps(&text);
    assert!(
        deps.is_empty(),
        "susi-core workspace deps drifted: {deps:?}"
    );
}

#[test]
fn susi_sandbox_must_not_depend_on_workspace_crates() {
    let root = workspace_root();
    let text = std::fs::read_to_string(root.join("crates/susi-sandbox/Cargo.toml"))
        .expect("susi-sandbox Cargo.toml");
    let deps = parse_workspace_deps(&text);
    assert!(
        deps.is_empty(),
        "susi-sandbox is a leaf REST service; workspace deps drifted: {deps:?}"
    );
}

#[test]
fn susi_server_must_not_depend_on_workspace_crates() {
    // susi-server vendors its susi_core subset (`src/susi_core/` from
    // crates/susi-core/vendor_template/) plus the leaf modules — the first
    // consumer converted under the microkernel path.
    let root = workspace_root();
    let text = std::fs::read_to_string(root.join("crates/susi-server/Cargo.toml"))
        .expect("susi-server Cargo.toml");
    let deps = parse_workspace_deps(&text);
    assert!(
        deps.is_empty(),
        "susi-server vendors susi_core + leaf modules; workspace deps drifted: {deps:?}"
    );
}

#[test]
fn susi_tools_must_not_depend_on_workspace_crates() {
    // susi-tools vendors the susi_core subset (`src/susi_core/` from
    // crates/susi-core/vendor_template/) plus the leaf modules — the second
    // consumer converted under the microkernel path.
    let root = workspace_root();
    let text = std::fs::read_to_string(root.join("crates/susi-tools/Cargo.toml"))
        .expect("susi-tools Cargo.toml");
    let deps = parse_workspace_deps(&text);
    assert!(
        deps.is_empty(),
        "susi-tools vendors susi_core + leaf modules; workspace deps drifted: {deps:?}"
    );
}

#[test]
fn susi_gmcp_must_not_depend_on_workspace_crates() {
    // susi-gmcp vendors the susi_core subset (`src/susi_core/` from
    // crates/susi-core/vendor_template/) plus the leaf modules — fourth
    // consumer converted under the microkernel path. Its dev-dep on
    // susi-tools is test-only (MCP client round-trip), not a production edge.
    let root = workspace_root();
    let text = std::fs::read_to_string(root.join("crates/susi-gmcp/Cargo.toml"))
        .expect("susi-gmcp Cargo.toml");
    let deps = parse_workspace_deps(&text);
    assert!(
        deps.is_empty(),
        "susi-gmcp vendors susi_core + leaf modules; workspace deps drifted: {deps:?}"
    );
}

#[test]
fn susi_agents_must_not_depend_on_workspace_crates() {
    // susi-agents vendors the susi_core subset (`src/susi_core/` from
    // crates/susi-core/vendor_template/) plus the leaf modules — third
    // consumer converted under the microkernel path.
    let root = workspace_root();
    let text = std::fs::read_to_string(root.join("crates/susi-agents/Cargo.toml"))
        .expect("susi-agents Cargo.toml");
    let deps = parse_workspace_deps(&text);
    assert!(
        deps.is_empty(),
        "susi-agents vendors susi_core + leaf modules; workspace deps drifted: {deps:?}"
    );
}

#[test]
fn susi_native_must_not_depend_on_workspace_crates() {
    let root = workspace_root();
    let text = std::fs::read_to_string(root.join("crates/susi-native/Cargo.toml"))
        .expect("susi-native Cargo.toml");
    let deps = parse_workspace_deps(&text);
    assert!(
        deps.is_empty(),
        "susi-native is a leaf REST service; workspace deps drifted: {deps:?}"
    );
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
