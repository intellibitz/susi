//! CEO decision 2026-11-04 ("No, ask first"): core never downloads a model
//! by itself.
//!
//! Observed before this decision: on its first start in an empty home, core
//! pulled the ~429 MB Qwen2.5-0.5B starter from huggingface.co with no user
//! request (`spawn_background_hardware_model_provisioner` at daemon boot and
//! in the zero-config bootstrap, `ensure_hardware_optimal_models` on the
//! maintenance ticks, and `provision_and_retry` when routing fell back to a
//! local model with none installed).
//!
//! Now a download starts only from an explicit user request:
//! `susi os manage model install starter` (the audited starter and its
//! tokenizer, `ModelManager::install_starter_model_on_request`),
//! `susi os manage model install <https url>` and `susi scout-model <url>`
//! (`install_model_foreground`), and the explicit install request on the
//! `gemi.models.install` bus topic / the MCP `scout_model` tool
//! (`install_model`). A request that needs a local model while none is
//! installed gets the typed `NoLocalModelInstalled` error, which names that
//! command and the starter's size; cloud routing is unchanged.
//!
//! Source-level invariant, like `core_boot_starts_nothing.rs`: it walks the
//! call graph from the daemon's boot, first-start bootstrap, rediscovery and
//! maintenance-tick entry points, and from the inference router's local
//! fallback (`GemiEngine::reason_internal`), through every function defined
//! in the walked daemon, router and model-lifecycle modules (test modules
//! excluded), and fails if any reached body names a model download or pull
//! function. A second check pins every caller of those functions, across all
//! first-party sources, to the explicit user-request entry points above.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test-only assertions

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;

fn read(rel: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(rel))
        .unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

/// Byte offset just past the `}` matching the `{` at `open`, or `None`
/// when braces inside literals leave it unbalanced.
fn try_block_end(src: &str, open: usize) -> Option<usize> {
    let bytes = src.as_bytes();
    if bytes.get(open) != Some(&b'{') {
        return None;
    }
    // Braces inside string literals, char literals and comments do not count
    // (e.g. a test that writes "{corrupt json" must not unbalance its module).
    let mut depth = 0usize;
    let mut i = open;
    while i < bytes.len() {
        match bytes[i] {
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i += 1;
            }
            b'r' if matches!(bytes.get(i + 1), Some(b'"') | Some(b'#'))
                && (i == 0 || !is_ident(bytes[i - 1])) =>
            {
                let mut hashes = 0;
                let mut j = i + 1;
                while bytes.get(j) == Some(&b'#') {
                    hashes += 1;
                    j += 1;
                }
                if bytes.get(j) == Some(&b'"') {
                    let close: Vec<u8> = std::iter::once(b'"')
                        .chain(std::iter::repeat_n(b'#', hashes))
                        .collect();
                    j += 1;
                    while j < bytes.len() && !bytes[j..].starts_with(&close) {
                        j += 1;
                    }
                    i = j + close.len() - 1;
                }
            }
            b'"' => {
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    if bytes[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            b'\'' => {
                // A char literal like '{' or '\''; a lifetime has no closing quote nearby.
                if bytes.get(i + 2) == Some(&b'\'') {
                    i += 2;
                } else if bytes.get(i + 1) == Some(&b'\\') {
                    if let Some(off) = bytes[i + 2..].iter().take(10).position(|&c| c == b'\'') {
                        i += 2 + off;
                    }
                }
            }
            b'{' => depth += 1,
            b'}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

fn block_end(src: &str, open: usize) -> usize {
    try_block_end(src, open).unwrap_or_else(|| panic!("unbalanced block at {open}"))
}

/// Ranges of `#[cfg(test)] mod … { … }` blocks.
fn test_module_ranges(src: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for (at, _) in src.match_indices("#[cfg(test)]") {
        let rest = &src[at..];
        let Some(mod_at) = rest.find("mod ") else {
            continue;
        };
        // Only further attributes (e.g. `#[allow(clippy::unwrap_used)]`) or comments may sit
        // between `#[cfg(test)]` and `mod`; anything else means the cfg is not
        // on this module.
        let between = &rest["#[cfg(test)]".len()..mod_at];
        let only_attributes = between
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .all(|line| (line.starts_with("#[") && line.ends_with(']')) || line.starts_with("//"));
        if !only_attributes {
            continue;
        }
        let Some(open) = rest[mod_at..].find(['{', ';']) else {
            continue;
        };
        let open = at + mod_at + open;
        // Unbalanced (braces in literals): keep the module, over-approximating.
        if let Some(end) = try_block_end(src, open) {
            out.push((at, end));
        }
    }
    out
}

/// `src` without its `#[cfg(test)] mod … { … }` blocks.
fn without_test_modules(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut from = 0;
    for (start, end) in test_module_ranges(src) {
        if start >= from {
            out.push_str(&src[from..start]);
            from = end;
        }
    }
    out.push_str(&src[from..]);
    out
}

fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Every non-test `fn name(...) { body }` in `src`, by name.
fn functions(src: &str) -> Vec<(String, String)> {
    let tests = test_module_ranges(src);
    let bytes = src.as_bytes();
    let mut out = Vec::new();
    for (at, _) in src.match_indices("fn ") {
        if at > 0 && is_ident(bytes[at - 1]) {
            continue;
        }
        if tests.iter().any(|(s, e)| (*s..*e).contains(&at)) {
            continue;
        }
        let name: String = src[at + 3..]
            .bytes()
            .take_while(|b| is_ident(*b))
            .map(char::from)
            .collect();
        if name.is_empty() {
            continue;
        }
        let Some(rel) = src[at..].find(['{', ';']) else {
            continue;
        };
        let open = at + rel;
        if bytes[open] != b'{' {
            continue;
        }
        out.push((name, src[open..block_end(src, open)].to_string()));
    }
    out
}

/// `body` with `//` comment lines removed (token checks only).
fn code_only(body: &str) -> String {
    body.lines()
        .map(|line| {
            if line.trim_start().starts_with("//") {
                ""
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// True when `code` names `name` as a whole identifier.
fn mentions(code: &str, name: &str) -> bool {
    let bytes = code.as_bytes();
    code.match_indices(name).any(|(i, _)| {
        let end = i + name.len();
        (i == 0 || !is_ident(bytes[i - 1])) && (end == bytes.len() || !is_ident(bytes[end]))
    })
}

/// True when `code` calls a function named exactly `name`.
fn calls(code: &str, name: &str) -> bool {
    let bytes = code.as_bytes();
    code.match_indices(&format!("{name}("))
        .any(|(i, _)| i == 0 || !is_ident(bytes[i - 1]))
}

fn body_of<'a>(fns: &'a [(String, String)], name: &str) -> &'a str {
    let found: Vec<_> = fns.iter().filter(|(n, _)| n == name).collect();
    assert_eq!(found.len(), 1, "expected exactly one `fn {name}`");
    &found[0].1
}

/// The model download and pull functions, named anywhere (called or passed
/// by name). Each one fetches weights or a tokenizer from the network, or
/// queues a background fetch.
const DOWNLOAD_FNS: [&str; 9] = [
    // The explicit starter install (`susi os manage model install starter`).
    "install_starter_model_on_request",
    // The explicit embedding-model install
    // (`susi os manage model install embeddings`).
    "install_on_request",
    // Foreground HTTPS install (`os manage model install <url>`, `scout-model`).
    "install_model_foreground",
    // Background install enqueue (explicit install requests only).
    "install_model",
    // The background download controller and its transfer loop.
    "start_download",
    "execute_download_stream",
    // The starter's paired tokenizer fetch.
    "ensure_ladder_tokenizer",
    // Removed unrequested paths: they must never return.
    "spawn_background_hardware_model_provisioner",
    "provision_and_retry",
];

/// Download primitives named by path.
const DOWNLOAD_TOKENS: [&str; 5] = [
    "download::transfer(",
    "resumable_downloads::download(",
    "zc_first_model::fetch(",
    // Ollama registry pulls (`susi openweight pull`).
    "manager.pull(",
    // The embedding model's first-use fetch.
    "fastembed::embed",
];

fn offenders(reached: &BTreeMap<String, String>) -> Vec<String> {
    let mut out = Vec::new();
    for (name, code) in reached {
        for banned in DOWNLOAD_FNS {
            if mentions(code, banned) {
                out.push(format!("{name} names {banned}"));
            }
        }
        for token in DOWNLOAD_TOKENS {
            if code.contains(token) {
                out.push(format!("{name} contains `{token}`"));
            }
        }
    }
    out
}

fn module_of(rel: &str) -> &str {
    rel.rsplit('/')
        .next()
        .and_then(|f| f.strip_suffix(".rs"))
        .unwrap_or(rel)
}

/// The daemon modules the boot, first-start and tick walk follows (the same
/// set `core_boot_starts_nothing.rs` walks).
const DAEMON: [&str; 6] = [
    "crates/susi-daemon/src/discovery_pipeline.rs",
    "crates/susi-daemon/src/composition.rs",
    "crates/susi-daemon/src/runtime_admin.rs",
    "crates/susi-daemon/src/estate_loop.rs",
    "crates/susi-daemon/src/auto_discovery.rs",
    "crates/susi-daemon-runtime/src/supervisor.rs",
];

/// The model-lifecycle modules every walk descends into: where the download
/// functions live, so a call that crosses into the model crate is followed.
const MODELS: [&str; 8] = [
    "crates/susi-gemi-models/src/lifecycle/provision.rs",
    "crates/susi-gemi-models/src/lifecycle/model_manager.rs",
    "crates/susi-gemi-models/src/lifecycle/selection.rs",
    "crates/susi-gemi-models/src/lifecycle/scan.rs",
    "crates/susi-gemi-models/src/lifecycle/download_controller.rs",
    "crates/susi-gemi-models/src/lifecycle/types.rs",
    "crates/susi-gemi-models/src/zc_first_model.rs",
    "crates/susi-gemi-models/src/hardware.rs",
];

/// The inference router whose local fallback used to fetch a model.
const ROUTER: &str = "crates/susi-gemi/src/engines/runtime.rs";

/// Walk the call graph of `entries` (`module::fn`, plus extra
/// `(key, body)` roots) through every function in `modules`. A name defined
/// in several walked modules reaches each of them: the walk
/// over-approximates, so it can only report too much, never too little.
fn reachable(
    modules: &[&str],
    entries: &[(&str, &str)],
    roots: Vec<(String, String)>,
) -> BTreeMap<String, String> {
    let mut graph: Vec<(String, String, String)> = Vec::new();
    for rel in modules {
        let module = module_of(rel).to_string();
        graph.extend(
            functions(&read(rel))
                .into_iter()
                .map(|(name, body)| (module.clone(), name, body)),
        );
    }
    let mut queue: VecDeque<(String, String)> = roots.into_iter().collect();
    for (module, entry) in entries {
        let found: Vec<_> = graph
            .iter()
            .filter(|(m, n, _)| m == module && n == entry)
            .collect();
        assert_eq!(
            found.len(),
            1,
            "expected exactly one `fn {module}::{entry}`"
        );
        queue.push_back((format!("{module}::{entry}"), found[0].2.clone()));
    }
    let names: BTreeSet<&str> = graph.iter().map(|(_, n, _)| n.as_str()).collect();
    let mut reached = BTreeMap::new();
    while let Some((key, body)) = queue.pop_front() {
        if reached.contains_key(&key) {
            continue;
        }
        let code = code_only(&body);
        for callee in &names {
            if mentions(&code, callee) {
                for (m, n, b) in graph.iter().filter(|(_, n, _)| n == callee) {
                    let callee_key = format!("{m}::{n}");
                    if callee_key != key && !reached.contains_key(&callee_key) {
                        queue.push_back((callee_key, b.clone()));
                    }
                }
            }
        }
        reached.insert(key, code);
    }
    reached
}

/// The daemon's boot module. Its helpers are walked too, so a download
/// reintroduced in a `server.rs` function that `run_daemon_loop` calls fails.
const SERVER: &str = "crates/susi-daemon/src/server.rs";

#[test]
fn boot_first_start_rediscovery_and_ticks_never_download_a_model() {
    let modules: Vec<&str> = std::iter::once(SERVER)
        .chain(DAEMON.iter().copied())
        .chain(MODELS.iter().copied())
        .collect();
    let reached = reachable(
        &modules,
        &[
            // Daemon boot.
            ("server", "run_daemon_loop"),
            ("discovery_pipeline", "register_contract_providers"),
            // First start / zero-config bootstrap.
            ("discovery_pipeline", "bootstrap_zero_config_substrate"),
            // Rediscovery tick.
            ("discovery_pipeline", "spawn_periodic_rediscovery"),
            ("discovery_pipeline", "local_ecosystem_report"),
            // Auto substrate priming (boot and CLI).
            ("discovery_pipeline", "prime_selected_catalogs"),
            ("composition", "wire_daemon_engine_hooks"),
            ("composition", "wire_daemon_os_planes"),
            ("composition", "wire_cli_substrate_priming"),
            // Maintenance ticks: host readiness and the substrate audit.
            ("runtime_admin", "start_administration_cycle"),
            ("runtime_admin", "perform_host_readiness"),
            ("runtime_admin", "perform_substrate_audit"),
            ("auto_discovery", "spawn_all_cells"),
            ("supervisor", "start"),
        ],
        Vec::new(),
    );
    for must in [
        "server::run_daemon_loop",
        "discovery_pipeline::bootstrap_zero_config_substrate",
        "runtime_admin::perform_host_readiness",
        "runtime_admin::perform_substrate_audit",
        // The walk crosses into the model crate: the ticks' readiness check.
        "provision::ensure_hardware_optimal_models",
        "selection::identify_best_suited_local_model",
    ] {
        assert!(
            reached.contains_key(must),
            "the boot/tick walk must reach `{must}` (reached: {:?})",
            reached.keys().collect::<Vec<_>>()
        );
    }
    let found = offenders(&reached);
    assert!(
        found.is_empty(),
        "core never downloads a model on its own at boot, first start or on a tick:\n{}",
        found.join("\n")
    );
}

#[test]
fn routing_never_falls_back_to_downloading_a_local_model() {
    let modules: Vec<&str> = std::iter::once(ROUTER)
        .chain(MODELS.iter().copied())
        .collect();
    let reached = reachable(
        &modules,
        &[
            ("runtime", "reason_internal"),
            ("runtime", "has_usable_local_model"),
        ],
        Vec::new(),
    );
    for must in [
        "runtime::reason_internal",
        "runtime::has_usable_local_model",
        "selection::identify_best_suited_local_model",
    ] {
        assert!(
            reached.contains_key(must),
            "the routing walk must reach `{must}` (reached: {:?})",
            reached.keys().collect::<Vec<_>>()
        );
    }
    let found = offenders(&reached);
    assert!(
        found.is_empty(),
        "a routing fallback never downloads a model:\n{}",
        found.join("\n")
    );
    // With no local model the router answers with the typed error.
    let router = functions(&read(ROUTER));
    let route = code_only(body_of(&router, "reason_internal"));
    assert!(
        route.contains("NoLocalModelInstalled::starter()"),
        "reason_internal reports NoLocalModelInstalled when no local model is installed"
    );
}

#[test]
fn unattended_readiness_checks_only_observe() {
    // `ensure_hardware_optimal_models` serves the daemon ticks and the
    // `gemi.models.ensure` bus topic: it observes and reports the command.
    let provision = functions(&read(MODELS[0]));
    let ensure = code_only(body_of(&provision, "ensure_hardware_optimal_models"));
    assert!(
        ensure.contains("NoLocalModelInstalled::starter()"),
        "{ensure}"
    );
    for banned in DOWNLOAD_FNS {
        assert!(!mentions(&ensure, banned), "{ensure}");
    }
    // The verify-download-agent report starts no download either.
    let verify = code_only(body_of(&provision, "verify_model_ladder"));
    for banned in DOWNLOAD_FNS {
        assert!(
            !mentions(&verify, banned),
            "verify_model_ladder names {banned}"
        );
    }

    // The bus arm for `gemi.models.ensure` calls only the observation.
    let handler = read("crates/susi-gemi/src/plane_handler.rs");
    let at = handler
        .find("topics::GEMI_MODELS_ENSURE =>")
        .expect("plane handler serves gemi.models.ensure");
    let open = at + handler[at..].find('{').unwrap();
    let arm = code_only(&handler[open..block_end(&handler, open)]);
    assert!(arm.contains("ensure_hardware_optimal_models("), "{arm}");
    for banned in DOWNLOAD_FNS {
        assert!(!mentions(&arm, banned), "{arm}");
    }

    // The runtime agent a pulse mission runs reports readiness; it never
    // installs a model.
    let native = read("crates/susi-gawd-agents/src/agents/native.rs");
    let at = native
        .find("impl GawdAgent for SusiRuntimeAgent")
        .expect("SusiRuntimeAgent is defined");
    let open = at + native[at..].find('{').unwrap();
    let agent = code_only(&native[open..block_end(&native, open)]);
    for banned in DOWNLOAD_FNS {
        assert!(!mentions(&agent, banned), "SusiRuntimeAgent names {banned}");
    }
    for token in DOWNLOAD_TOKENS {
        assert!(!agent.contains(token), "SusiRuntimeAgent contains {token}");
    }
}

/// Every first-party `.rs` file under `src/` and `crates/`, with its
/// `#[cfg(test)]` modules and comment lines removed.
fn first_party_code() -> Vec<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut stack = vec![root.join("src"), root.join("crates")];
    let mut out = Vec::new();
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                // Integration tests and fixtures may exercise the explicit
                // commands; only shipped code is pinned here.
                if path.file_name().is_some_and(|n| n == "tests") {
                    continue;
                }
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let rel = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                let text = std::fs::read_to_string(&path).unwrap_or_default();
                out.push((rel, code_only(&without_test_modules(&text))));
            }
        }
    }
    out
}

#[test]
fn only_explicit_user_requests_call_the_download_functions() {
    // (download function, the files whose shipped code may call it). Every
    // other caller is an unrequested download and fails this test.
    let allowed: [(&str, &[&str]); 7] = [
        (
            "install_starter_model_on_request",
            // `susi os manage model install starter`.
            &["src/cli/os_cli.rs"],
        ),
        (
            "install_on_request",
            // `susi os manage model install embeddings`.
            &["src/cli/os_cli.rs"],
        ),
        (
            "install_model_foreground",
            &[
                // `susi os manage model install <https url>`.
                "src/cli/os_cli.rs",
                // `susi scout-model <url>`.
                "src/cli/mission_cli.rs",
                // Inside `install_starter_model_on_request` (checked below).
                "crates/susi-gemi-models/src/lifecycle/provision.rs",
            ],
        ),
        (
            "install_model",
            &[
                // The `gemi.models.install` bus request and its facade.
                "crates/susi-gemi/src/plane_handler.rs",
                "crates/susi-core/src/plane_bus.rs",
                // The MCP `scout_model` tool a client calls explicitly.
                "crates/susi-gmcp/src/tools/core.rs",
            ],
        ),
        (
            "start_download",
            // Only `install_model`.
            &["crates/susi-gemi-models/src/lifecycle/model_manager.rs"],
        ),
        (
            "execute_download_stream",
            // `install_model_foreground` and the controller `start_download`.
            &[
                "crates/susi-gemi-models/src/lifecycle/model_manager.rs",
                "crates/susi-gemi-models/src/lifecycle/download_controller.rs",
            ],
        ),
        (
            "ensure_ladder_tokenizer",
            // Inside `install_starter_model_on_request` (checked below).
            &["crates/susi-gemi-models/src/lifecycle/provision.rs"],
        ),
    ];
    let mut offenders = Vec::new();
    let mut matched_allowed = std::collections::BTreeSet::new();
    for (rel, code) in first_party_code() {
        for (function, files) in &allowed {
            let defines = code.contains(&format!("fn {function}("));
            // A definition is not a call; count calls outside it.
            let mut rest = code.clone();
            if defines {
                if let Some(at) = rest.find(&format!("fn {function}(")) {
                    let open = at + rest[at..].find('{').unwrap_or(0);
                    if let Some(end) = try_block_end(&rest, open) {
                        rest.replace_range(at..end, "");
                    }
                }
            }
            if calls(&rest, function) {
                if files.contains(&rel.as_str()) {
                    matched_allowed.insert((*function, rel.clone()));
                } else {
                    offenders.push(format!("{rel} calls {function}()"));
                }
            } else if defines && files.contains(&rel.as_str()) {
                matched_allowed.insert((*function, rel.clone()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "only explicit user requests may download a model:\n{}",
        offenders.join("\n")
    );
    for (function, files) in &allowed {
        for file in *files {
            assert!(
                matched_allowed.contains(&(*function, (*file).to_string())),
                "allowed download caller `{file}` did not call `{function}()`; update scanner path if moved"
            );
        }
    }

    // Inside the model crate the starter's downloads run only in the
    // explicit install, and the background controller only for an explicit
    // `install_model` request.
    let provision = functions(&read(MODELS[0]));
    for (name, body) in &provision {
        let code = code_only(body);
        for function in ["install_model_foreground", "ensure_ladder_tokenizer"] {
            if calls(&code, function) {
                assert_eq!(
                    name, "install_starter_model_on_request",
                    "provision::{name} calls {function}()"
                );
            }
        }
    }
    // Every link of the transfer chain has exactly the callers the explicit
    // requests use: a new function that calls one, even if a boot helper
    // the walk misses calls it, fails here.
    let pinned: [(&str, &str, &[&str]); 7] = [
        (MODELS[1], "start_download", &["install_model"]),
        (
            MODELS[1],
            "execute_download_stream",
            &["install_model_foreground"],
        ),
        (
            MODELS[1],
            "execute_download_stream_in",
            &["execute_download_stream"],
        ),
        (
            MODELS[1],
            "stage_and_record",
            &["execute_download_stream_in"],
        ),
        (MODELS[1], "download::transfer", &["stage_and_record"]),
        (MODELS[4], "execute_download_stream", &["start_download"]),
        (
            MODELS[0],
            "download::transfer",
            &["ensure_ladder_tokenizer"],
        ),
    ];
    for (file, function, callers) in pinned {
        let module = module_of(file);
        let mut seen = 0;
        for (name, body) in functions(&read(file)) {
            if name == function.rsplit("::").next().unwrap_or(function) {
                continue;
            }
            let code = code_only(&body);
            let called = if function.contains("::") {
                code.contains(&format!("{function}("))
            } else {
                calls(&code, function)
            };
            if called {
                assert!(
                    callers.contains(&name.as_str()),
                    "{module}::{name} calls {function}() (allowed: {callers:?})"
                );
                seen += 1;
            }
        }
        assert!(seen > 0, "{module}: no caller of {function}() found");
    }
}

/// The pinned semantic-search embedding model downloads only on the
/// explicit `susi os manage model install embeddings`: its lazy load reads
/// the cache and never fetches, and its callers are the two semantic paths.
#[test]
fn the_embedding_model_downloads_only_on_its_explicit_install() {
    const EMBEDDER: &str = "crates/susi-gmcp/src/embed_provider.rs";
    let embedder = functions(&read(EMBEDDER));
    for name in ["embed", "embed_one", "load_failed", "install_on_request"] {
        for (n, body) in embedder.iter().filter(|(n, _)| n == name) {
            let code = code_only(body);
            assert!(
                !calls(&code, "http_call"),
                "embed_provider::{n} can fetch the model"
            );
        }
    }
    let lib = read(EMBEDDER);
    assert!(lib.contains(
        "pub const INSTALL_COMMAND: &str = \"susi os manage model install embeddings\";"
    ));
    assert!(lib.contains("pub const INSTALL_SOURCE: &str = \"embeddings\";"));
    let os = read("src/cli/os_cli.rs");
    assert!(os.contains("susi_gmcp::embed_provider::INSTALL_SOURCE"));
    assert!(os.contains("susi_gmcp::embed_provider::install_on_request()"));
}

/// A model reply, a mission plan or a DAG step can name any registered
/// tool. None of them can start a download: `scout_model` (the only tool
/// that reaches `install_model`) is refused on every autonomous dispatch
/// path, and only an explicit client (`tools/call` over MCP) runs it.
#[test]
fn model_emitted_and_mission_tool_dispatch_cannot_reach_a_model_download() {
    const BUS: &str = "crates/susi-core/src/plane_bus.rs";
    let bus = read(BUS);
    assert!(bus.contains("pub const USER_REQUEST_ONLY_TOOLS: &[&str] = &[\"scout_model\"];"));
    let bus_fns = functions(&bus);
    let guard = code_only(body_of(&bus_fns, "execute_autonomous_tool"));
    let check = guard
        .find("is_user_request_only(name)")
        .expect("execute_autonomous_tool checks the tool name");
    let run = guard
        .find("execute_tool(name, args, workspace)")
        .expect("execute_autonomous_tool delegates to execute_tool");
    assert!(check < run, "the refusal comes before the bus: {guard}");

    // `scout_model` is the one tool whose body reaches `install_model`.
    let core = functions(&read("crates/susi-gmcp/src/tools/core.rs"));
    for (name, body) in &core {
        if calls(&code_only(body), "install_model") {
            assert_eq!(name, "scout_model", "core::{name} installs a model");
        }
    }

    // The model-emitted tool calls refuse it before anything runs.
    let router = functions(&read(ROUTER));
    let fulfill = code_only(body_of(&router, "fulfill_emitted_tool_calls"));
    assert!(
        fulfill.contains("is_user_request_only(&call.name)"),
        "{fulfill}"
    );
    assert!(
        !fulfill.contains("tools::execute_tool("),
        "model-emitted calls bypass the guard: {fulfill}"
    );

    // Every other shipped dispatch outside the MCP server and the tool
    // registry names a fixed, non-download tool, or goes through the guard.
    let mut offenders = Vec::new();
    for (rel, code) in first_party_code() {
        if rel.starts_with("crates/susi-gmcp/")
            || rel.starts_with("crates/susi-tools/")
            || rel == BUS
        {
            continue;
        }
        if code.contains("ToolRegistry::execute_tool(") {
            offenders.push(format!("{rel} calls ToolRegistry::execute_tool directly"));
        }
        for (at, _) in code.match_indices("tools::execute_tool(") {
            let arg = code[at + "tools::execute_tool(".len()..].trim_start();
            let literal = arg
                .strip_prefix('"')
                .and_then(|rest| rest.split('"').next());
            match literal {
                Some(tool) if !tool.eq_ignore_ascii_case("scout_model") => {}
                _ => offenders.push(format!(
                    "{rel} dispatches `{}` without execute_autonomous_tool",
                    arg.chars().take(40).collect::<String>()
                )),
            }
        }
    }
    assert!(offenders.is_empty(), "{offenders:#?}");
}

#[test]
fn the_unrequested_download_paths_are_gone() {
    let mut found = Vec::new();
    for (rel, code) in first_party_code() {
        for gone in [
            "fn spawn_background_hardware_model_provisioner",
            "fn provision_and_retry",
            "fn prepare_hardware_optimal_models",
            "fn retry_after_provisioning",
        ] {
            if code.contains(gone) {
                found.push(format!("{rel}: {gone}"));
            }
        }
    }
    assert!(
        found.is_empty(),
        "removed download paths returned: {found:?}"
    );
}

#[test]
fn the_no_model_answer_names_the_download_command_and_size() {
    // The command the typed error names is the one the CLI serves.
    let first = read("crates/susi-gemi-models/src/zc_first_model.rs");
    assert!(first.contains(
        "pub const STARTER_INSTALL_COMMAND: &str = \"susi os manage model install starter\";"
    ));
    assert!(first.contains("pub const STARTER_SOURCE: &str = \"starter\";"));
    let os = read("src/cli/os_cli.rs");
    assert!(os.contains("zc_first_model::STARTER_SOURCE"));
    assert!(os.contains("install_starter_model_on_request("));
    let types = code_only(&read("crates/susi-gemi-models/src/lifecycle/types.rs"));
    for part in [
        "pub struct NoLocalModelInstalled",
        "STARTER_INSTALL_COMMAND",
        "size_bytes: starter.size_bytes",
        "Cloud models work without a download.",
        "impl From<NoLocalModelInstalled> for crate::susi_error::EaiError",
    ] {
        assert!(types.contains(part), "NoLocalModelInstalled lacks `{part}`");
    }
}

/// A fresh, isolated home: `susi status` and `susi auto status` say no local
/// model is installed and name the download command, and neither creates or
/// fills a models directory. No daemon is started (status answers with it
/// down; `auto` needs none).
#[test]
fn status_on_a_fresh_home_reports_no_local_model_and_the_download_command() {
    let home = std::env::temp_dir().join(format!(
        "susi-no-auto-model-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&home).unwrap();
    let run = |args: &[&str]| {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_susi"));
        susi_paths::test_env::isolate_home(&mut command, &home)
            .args(args)
            // Never the default ports another instance may hold.
            .env("SUSI_PORT_OFFSET", "4711")
            .env("SUSI_HOME", home.join(".susi"))
            .env_remove("SUSI_MODEL_DIR");
        let out = command.output().unwrap();
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    };
    for args in [&["status"][..], &["auto", "status"]] {
        let out = run(args);
        assert!(
            out.contains("no local model installed"),
            "susi {args:?}:\n{out}"
        );
        assert!(
            out.contains("`susi os manage model install starter`"),
            "susi {args:?}:\n{out}"
        );
        assert!(out.contains("about 429 MB"), "susi {args:?}:\n{out}");
    }
    // Nothing was downloaded anywhere under the home.
    let mut stack = vec![home.clone()];
    let mut weights = Vec::new();
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.to_string_lossy().contains(".gguf") {
                weights.push(path);
            }
        }
    }
    assert!(weights.is_empty(), "status downloaded weights: {weights:?}");
    let _ = std::fs::remove_dir_all(&home);
}
