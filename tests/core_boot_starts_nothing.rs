//! CEO decision 2026-11-03: core starts nothing on its own at boot.
//!
//! The daemon's boot path must not launch a local inference engine
//! (`ollama serve` and the like), must not run the agent-framework readiness
//! scan, and must not run third-party CLIs (engine `--version`, the cloud
//! operator CLIs). A local engine starts only for a user request that holds
//! the `OnDemandEngineStart` guard (`susi run`, a `susi <goal>` mission, a
//! `/v1` completion: `HttpProvider::generate`/`embed` →
//! `local_ecosystem::ensure_running_on_port`, or the routing retry
//! `GemiEngine::start_local_engine_for_request`) or on
//! `susi ecosystem start <id>`; frameworks are probed only on request
//! (`susi auto prime`, `susi substrate status`, `susi frameworks doctor`);
//! the cloud CLIs only on `susi os provision probe|list|apply`.
//!
//! Boot entry functions, precisely:
//! - `SusiDaemon::run_daemon_loop` (`crates/susi-daemon/src/server.rs`), the
//!   daemon process entry, and everything it reaches in the daemon's
//!   composition, discovery, runtime-administration, estate, cell and
//!   leaf-service modules — in particular
//!   `discovery_pipeline::{register_contract_providers,
//!   bootstrap_zero_config_substrate, spawn_periodic_rediscovery,
//!   local_ecosystem_report}`, `composition::{wire_daemon_engine_hooks,
//!   wire_daemon_os_planes}` (with the 30 s OS-plane tick),
//!   `runtime_admin::SusiRuntimeAdmin::start_administration_cycle` (the
//!   maintenance tick: brain scout, estate loop, ecosystem rescan),
//!   `auto_discovery::spawn_all_cells` and `supervisor::start`.
//! - `composition::wire_cli_substrate_priming`, the CLI composition root, for
//!   the engine-start and cloud-CLI checks. (Its catalog priming is chosen
//!   per command by the user's command line, not at boot.)
//!
//! This is a source-level invariant: it walks the call graph of those entry
//! points through every function defined in `discovery_pipeline.rs`,
//! `composition.rs`, `runtime_admin.rs`, `estate_loop.rs`,
//! `auto_discovery.rs` and `supervisor.rs` (test modules excluded) and fails
//! if any reached body calls the engine-awakening, on-demand start,
//! framework-probe or cloud-CLI functions, or spawns a process other than
//! the allow-listed first-party and OS ones. Calls that leave the daemon
//! crate (the brain scout's `provider.generate`) are covered by the guard
//! checks: no daemon source and no scout source ever holds
//! `OnDemandEngineStart`, and only the user request entry points do.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test-only assertions

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;

fn read(rel: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(rel))
        .unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

/// Byte offset just past the `}` matching the `{` at `open`.
fn block_end(src: &str, open: usize) -> usize {
    let bytes = src.as_bytes();
    assert_eq!(bytes[open], b'{');
    let mut depth = 0usize;
    for (i, b) in bytes.iter().enumerate().skip(open) {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return i + 1;
                }
            }
            _ => {}
        }
    }
    panic!("unbalanced block at {open}");
}

/// Ranges of `#[cfg(test)] mod … { … }` blocks.
fn test_module_ranges(src: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for (at, _) in src.match_indices("#[cfg(test)]") {
        let rest = &src[at..];
        let Some(mod_at) = rest.find("mod ") else {
            continue;
        };
        // Only when the attribute directly annotates a module.
        let between = &rest["#[cfg(test)]".len()..mod_at];
        if !between.trim().is_empty() {
            continue;
        }
        let Some(open) = rest[mod_at..].find(['{', ';']) else {
            continue;
        };
        let open = at + mod_at + open;
        if src.as_bytes()[open] == b'{' {
            out.push((at, block_end(src, open)));
        }
    }
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
        // The body opens at the first `{` after the signature; a `;` first
        // means a declaration without a body.
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

/// `body` with `//` comments removed (token checks only, never braces).
fn code_only(body: &str) -> String {
    body.lines()
        .map(|line| {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                ""
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// True when `code` calls a function named exactly `name`.
fn calls(code: &str, name: &str) -> bool {
    let bytes = code.as_bytes();
    code.match_indices(&format!("{name}("))
        .any(|(i, _)| i == 0 || !is_ident(bytes[i - 1]))
}

/// True when `code` names `name` as a whole identifier: a call, or a
/// function passed by name (`phase("…", try_awaken_local_engines)`).
fn mentions(code: &str, name: &str) -> bool {
    let bytes = code.as_bytes();
    code.match_indices(name).any(|(i, _)| {
        let end = i + name.len();
        (i == 0 || !is_ident(bytes[i - 1])) && (end == bytes.len() || !is_ident(bytes[end]))
    })
}

fn body_of<'a>(fns: &'a [(String, String)], name: &str) -> &'a str {
    let found: Vec<_> = fns.iter().filter(|(n, _)| n == name).collect();
    assert_eq!(found.len(), 1, "expected exactly one `fn {name}`");
    &found[0].1
}

/// The daemon modules whose functions the boot walk follows.
const WALKED: [&str; 6] = [
    "crates/susi-daemon/src/discovery_pipeline.rs",
    "crates/susi-daemon/src/composition.rs",
    "crates/susi-daemon/src/runtime_admin.rs",
    "crates/susi-daemon/src/estate_loop.rs",
    "crates/susi-daemon/src/auto_discovery.rs",
    "crates/susi-daemon-runtime/src/supervisor.rs",
];

/// Process spawns the boot path may reach, by `module::fn`: first-party
/// susi processes and OS tools only — never third-party or locally
/// installed software.
const ALLOWED_SPAWNS: [(&str, &str); 3] = [
    // The user's own swarm cells (`~/.susi/cells`): native cell binaries
    // and `susi-universal-cell` (first-party) hosting WASM/JSON cells.
    ("auto_discovery", "spawn_cell"),
    // Leaf services: this `susi` binary re-executed as `service-run <name>`.
    ("supervisor", "spawn_service"),
    // Windows `taskkill` to stop a leaf service the daemon started.
    ("supervisor", "signal"),
];

fn module_of(rel: &str) -> &str {
    rel.rsplit('/')
        .next()
        .and_then(|f| f.strip_suffix(".rs"))
        .unwrap_or(rel)
}

/// The boot bodies: the entry points plus every walked daemon function they
/// reach, transitively, keyed `module::fn` (a name defined in several walked
/// modules reaches each of them: the walk over-approximates).
fn boot_reachable() -> BTreeMap<String, String> {
    let server = read("crates/susi-daemon/src/server.rs");
    let server_fns = functions(&server);
    let mut graph: Vec<(String, String, String)> = Vec::new();
    for rel in WALKED {
        let module = module_of(rel).to_string();
        graph.extend(
            functions(&read(rel))
                .into_iter()
                .map(|(name, body)| (module.clone(), name, body)),
        );
    }
    let entry_body = |module: &str, name: &str| -> String {
        let found: Vec<_> = graph
            .iter()
            .filter(|(m, n, _)| m == module && n == name)
            .collect();
        assert_eq!(found.len(), 1, "expected exactly one `fn {module}::{name}`");
        found[0].2.clone()
    };
    let mut reached = BTreeMap::new();
    let mut queue = VecDeque::new();
    queue.push_back((
        "server::run_daemon_loop".to_string(),
        body_of(&server_fns, "run_daemon_loop").to_string(),
    ));
    for (module, entry) in [
        ("discovery_pipeline", "register_contract_providers"),
        ("discovery_pipeline", "bootstrap_zero_config_substrate"),
        ("discovery_pipeline", "spawn_periodic_rediscovery"),
        ("discovery_pipeline", "local_ecosystem_report"),
        ("composition", "wire_daemon_engine_hooks"),
        ("composition", "wire_daemon_os_planes"),
        ("composition", "wire_cli_substrate_priming"),
        ("runtime_admin", "start_administration_cycle"),
        ("auto_discovery", "spawn_all_cells"),
        ("supervisor", "start"),
    ] {
        queue.push_back((format!("{module}::{entry}"), entry_body(module, entry)));
    }
    // `prime_selected_catalogs` primes exactly the catalogs it is handed:
    // the boot hands it `Catalogs::BOOT` (checked below) and the CLI root
    // the user's command's set, so the walk does not descend into it.
    let names: BTreeSet<&str> = graph
        .iter()
        .map(|(_, n, _)| n.as_str())
        .filter(|n| *n != "prime_selected_catalogs")
        .collect();
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

#[test]
fn boot_path_starts_no_engine_probes_no_framework_and_runs_no_third_party_cli() {
    let reached = boot_reachable();
    for must in [
        "server::run_daemon_loop",
        "discovery_pipeline::bootstrap_zero_config_substrate",
        "discovery_pipeline::boot_prime_catalogs",
        "discovery_pipeline::local_ecosystem_report_at",
        "composition::activate_host_control_planes",
        "composition::tick_host_control_planes",
        "runtime_admin::start_administration_cycle",
        "auto_discovery::spawn_cell",
        "supervisor::spawn_service",
    ] {
        assert!(
            reached.contains_key(must),
            "the boot walk must reach `{must}` (reached: {:?})",
            reached.keys().collect::<Vec<_>>()
        );
    }
    // Named anywhere (called or passed by name).
    let banned_names = [
        // Engine awakening / start.
        "try_awaken_local_engines",
        "ensure_running_on_port",
        "ensure_local_engine",
        // Full prime (includes the frameworks).
        "prime_catalogs",
        "ensure_startable_engine_running",
        "start_local_engine_for_request",
        // The user-request guard: no boot body ever holds it.
        "OnDemandEngineStart",
        // Cloud operator CLIs.
        "probe_all",
        "list_nodes",
    ];
    // Called: the framework manager (`AgentManager::frameworks(..)`).
    let banned_calls = ["frameworks"];
    let banned_tokens = [
        "local_ecosystem::start(",
        "Catalogs::ALL",
        "Catalogs::AGENTS",
        "CatalogKind::Framework",
        "\"serve\"",
        "awaken",
    ];
    let mut offenders = Vec::new();
    for (name, code) in &reached {
        let allowed_spawn = ALLOWED_SPAWNS
            .iter()
            .any(|(m, f)| name == &format!("{m}::{f}"));
        if code.contains("Command::new") && !allowed_spawn {
            offenders.push(format!("{name} spawns a process (Command::new)"));
        }
        for banned in banned_names {
            if mentions(code, banned) {
                offenders.push(format!("{name} names {banned}"));
            }
        }
        for callee in banned_calls {
            if calls(code, callee) {
                offenders.push(format!("{name} calls {callee}()"));
            }
        }
        for token in banned_tokens {
            if code.contains(token) {
                offenders.push(format!("{name} contains `{token}`"));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "core starts nothing on its own at boot:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn allow_listed_spawns_run_only_first_party_or_os_programs() {
    let fns = |rel: &str| functions(&read(rel));
    let auto = fns("crates/susi-daemon/src/auto_discovery.rs");
    let cell = code_only(body_of(&auto, "spawn_cell"));
    assert!(cell.contains("susi-universal-cell"), "{cell}");
    let sup = fns("crates/susi-daemon-runtime/src/supervisor.rs");
    let service = code_only(body_of(&sup, "spawn_service"));
    assert!(service.contains("service-run"), "{service}");
    for (name, body) in sup.iter().filter(|(n, _)| n == "signal") {
        let code = code_only(body);
        if code.contains("Command::new") {
            assert!(
                code.contains("Command::new(\"taskkill\")"),
                "{name}: {code}"
            );
        }
    }
}

#[test]
fn contract_providers_register_before_the_bootstrap_primes_catalogs() {
    // Explicit, so the ordering never rests on a substring accident
    // (`prime_catalogs(` also matches inside `boot_prime_catalogs(`).
    let server = functions(&read("crates/susi-daemon/src/server.rs"));
    let run = code_only(body_of(&server, "run_daemon_loop"));
    let register = run
        .find("register_contract_providers(")
        .expect("run_daemon_loop registers the contract providers");
    let bootstrap = run
        .find("bootstrap_zero_config_substrate(")
        .expect("run_daemon_loop runs the zero-config bootstrap");
    assert!(
        register < bootstrap,
        "register_contract_providers must run before bootstrap_zero_config_substrate"
    );
    let pipeline = functions(&read("crates/susi-daemon/src/discovery_pipeline.rs"));
    let boot = code_only(body_of(&pipeline, "bootstrap_zero_config_substrate"));
    assert!(boot.contains("boot_prime_catalogs(&substrate)"), "{boot}");
}

/// Every first-party `.rs` file under `src/` and `crates/`.
fn first_party_sources() -> Vec<(String, String)> {
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
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let rel = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push((rel, std::fs::read_to_string(&path).unwrap_or_default()));
            }
        }
    }
    out
}

#[test]
fn only_user_request_entry_points_allow_an_on_demand_engine_start() {
    // Where `OnDemandEngineStart::allow()` may be taken: the user's own
    // inference and missions, plus the guard's definition and unit tests.
    let entry_points = [
        "src/cli/run_cli.rs",                            // `susi run`
        "crates/susi-server/src/lib.rs",                 // `/v1` completions
        "crates/susi-gawd-swarm/src/ama/master.rs",      // `susi <goal>` missions
        "crates/susi-gawd/src/plane_handler.rs",         // forwarded goal (`mission_run`)
        "crates/susi-core/src/provider.rs",              // definition + tests
        "crates/susi-gemi/src/engines/http_provider.rs", // unit test only
    ];
    let mut offenders = Vec::new();
    let mut matched_entry_points = std::collections::BTreeSet::new();
    for (rel, text) in first_party_sources() {
        let code = code_only(&text);
        if code.contains("OnDemandEngineStart::allow()") {
            if entry_points.contains(&rel.as_str()) {
                matched_entry_points.insert(rel.clone());
            } else {
                offenders.push(format!("{rel} takes the on-demand engine-start guard"));
            }
        }
        // Unattended work never holds the guard: the daemon crate (boot,
        // maintenance, scheduled missions, pulse worker) and the brain scout
        // that calls `provider.generate` on every registered provider.
        let unattended = rel.starts_with("crates/susi-daemon/")
            || rel.ends_with("/scout_probe.rs")
            || rel.ends_with("/scout_schedule.rs")
            || rel.ends_with("/estate_loop.rs");
        if unattended && mentions(&code, "OnDemandEngineStart") {
            offenders.push(format!("{rel} (unattended) names OnDemandEngineStart"));
        }
    }
    assert!(offenders.is_empty(), "{}", offenders.join("\n"));
    for ep in entry_points {
        assert!(
            matched_entry_points.contains(ep),
            "expected entry point `{ep}` did not match `OnDemandEngineStart::allow()`; update scanner path if moved"
        );
    }

    // In the GAWD plane only the forwarded goal (`gawd.mission.run`, the
    // `mission_run` tool) runs as the user's request; `gawd.mission.solve`,
    // which unattended callers (A2A, `susi_solve`) also reach, does not.
    let handler = code_only(&read("crates/susi-gawd/src/plane_handler.rs"));
    let arm = |topic: &str| {
        handler
            .split(&format!("topics::{topic} =>"))
            .nth(1)
            .and_then(|rest| rest.split("topics::").next())
            .unwrap_or_default()
            .to_string()
    };
    // Exactly one call outside the unit tests (the definition is generic,
    // `as_user_request<T>(`): no other GAWD topic (plans, rollbacks) runs
    // as the user's request.
    let production = handler.split("#[cfg(test)]").next().unwrap_or_default();
    assert_eq!(
        production.matches("as_user_request(").count(),
        1,
        "only gawd.mission.run may run as the user's request"
    );
    let (run, solve) = (arm("GAWD_MISSION_RUN"), arm("GAWD_SOLVE"));
    assert!(
        run.contains("as_user_request("),
        "mission_run must run as the user's request"
    );
    assert!(!solve.is_empty(), "the GAWD_SOLVE arm was not found");
    assert!(
        !solve.contains("as_user_request") && !solve.contains("OnDemandEngineStart"),
        "gawd.mission.solve must not allow an on-demand engine start"
    );

    // The mission guard covers the user's `susi <goal>` entry only: the
    // daemon's pulse worker enters through `solve_stream`, which must not
    // take it.
    let master = functions(&read("crates/susi-gawd-swarm/src/ama/master.rs"));
    assert!(
        code_only(body_of(&master, "solve_stream_report")).contains("OnDemandEngineStart::allow()")
    );
    for unattended in [
        "solve_stream",
        "solve_stream_with_model",
        "solve_stream_report_with_model",
    ] {
        assert!(
            !code_only(body_of(&master, unattended)).contains("OnDemandEngineStart"),
            "{unattended} must not take the guard"
        );
    }

    // Both start paths honour the guard.
    let http = functions(&read("crates/susi-gemi/src/engines/http_provider.rs"));
    for op in ["generate", "embed"] {
        let bodies: Vec<String> = http
            .iter()
            .filter(|(n, _)| n == op)
            .map(|(_, b)| code_only(b))
            .filter(|b| b.contains("ensure_local_engine("))
            .collect();
        assert_eq!(
            bodies.len(),
            1,
            "HttpProvider::{op} starts engines through ensure_local_engine"
        );
        assert!(
            bodies[0].contains("OnDemandEngineStart::allowed()"),
            "{}",
            bodies[0]
        );
        assert!(
            bodies[0].contains("ensure_local_engine(&api_base, may_start)"),
            "{}",
            bodies[0]
        );
    }
    let target = code_only(body_of(&http, "on_demand_start_target"));
    let compact: String = target.split_whitespace().collect();
    assert!(compact.contains("may_start.then("), "{target}");
    let runtime = functions(&read("crates/susi-gemi/src/engines/runtime.rs"));
    let retry = code_only(body_of(&runtime, "start_local_engine_for_request"));
    assert!(retry.contains("OnDemandEngineStart::allowed()"), "{retry}");
}

#[test]
fn boot_catalogs_exclude_frameworks_and_the_boot_scan_runs_no_binary() {
    let pipeline = read("crates/susi-daemon/src/discovery_pipeline.rs");
    let fns = functions(&pipeline);
    let boot = code_only(body_of(&fns, "bootstrap_zero_config_substrate"));
    assert!(
        calls(&boot, "boot_prime_catalogs"),
        "the daemon bootstrap primes through boot_prime_catalogs"
    );
    let boot_prime = code_only(body_of(&fns, "boot_prime_catalogs"));
    assert!(
        boot_prime.contains("prime_selected_catalogs(substrate, Catalogs::BOOT)"),
        "{boot_prime}"
    );
    let at = pipeline
        .find("pub const BOOT: Self = Self {")
        .expect("Catalogs::BOOT is defined");
    let open = at + pipeline[at..].find('{').unwrap();
    let def = &pipeline[open..block_end(&pipeline, open)];
    assert!(
        def.contains("frameworks: false"),
        "Catalogs::BOOT must never select the frameworks: {def}"
    );
    // The unattended host scan observes; it never runs an engine binary.
    let scan = code_only(body_of(&fns, "local_ecosystem_report_at"));
    assert!(
        scan.contains("DetectOnlyProbe::with_recorded_versions"),
        "{scan}"
    );
    assert!(!scan.contains("HostProbe"), "{scan}");
}

#[test]
fn the_engine_awakening_function_is_gone_from_core() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut stack = vec![root.join("src"), root.join("crates")];
    let mut offenders = Vec::new();
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let text = std::fs::read_to_string(&path).unwrap_or_default();
                if text.contains("fn try_awaken_local_engines") {
                    offenders.push(path.display().to_string());
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "no boot-time engine awakening may return: {offenders:?}"
    );
}
