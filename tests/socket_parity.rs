//! Socket parity map (zero-UI migration plan B3).
//!
//! The CEO directive: every core feature must be reachable from the
//! commercial desktop over the standards socket. This test maps every
//! `susi` CLI command (each `Commands` variant in `src/cli/defs.rs`, and each
//! direct subcommand of a variant that has them) to a socket surface: an MCP
//! tool, an A2A skill, or a 9091 route. Commands the plan exempts (daemon
//! lifecycle, install, local tooling, workflow, tasks, admin
//! release/lint/audit-deps) are listed in `EXEMPT`. Everything else is a
//! `KNOWN_GAPS` entry, grouped like plan item B4, which B4 burns down.
//!
//! The test fails when:
//! - a CLI command is neither mapped, exempt nor allow-listed;
//! - a table names a command that no longer exists, or a surface (MCP tool,
//!   route, A2A skill) that the code does not serve;
//! - an allow-listed gap is also mapped, or every tool its B4 group planned
//!   for it now exists (move it to `MAPPED`);
//! - `KNOWN_GAPS` grows past `MAX_KNOWN_GAPS`, or a group past its ceiling
//!   (the ratchet that keeps the list shrinking to zero).
//!
//! A mapping must deliver the same capability over the socket, against the
//! same scope, on every supported OS; a tool with a similar name is not one.
//!
//! Commands are read from source because the CLI enums are private to the
//! binary. Set `SUSI_SOCKET_PARITY_REPORT=<path>` to write the grouped gap
//! list as Markdown.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test-only assertions

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[derive(Debug)]
enum Surface {
    /// MCP tool served on 9090/9093 `tools/list` (registered in `gmcp_bootstrap.rs`).
    Mcp(&'static str),
    /// A2A agent-card skill id on 9094.
    #[allow(dead_code)] // no mapping needs an A2A skill today; B4 entries may
    A2a(&'static str),
    /// 9091 HTTP route: method and path.
    Route(&'static str, &'static str),
}
use Surface::{A2a, Mcp, Route};

/// CLI command → the socket surface(s) that reach the same capability.
///
/// A mapping must provide the same capability, against the same scope (the
/// caller's workspace when the CLI uses its cwd), and must work over the
/// socket on every supported OS — not merely a tool with a similar name.
/// A surface that covers only part of a command, or only the daemon's
/// substrate home, is a `KNOWN_GAPS` entry instead.
const MAPPED: &[(&str, &[Surface])] = &[
    ("agents list", &[Mcp("agents_list")]),
    ("agents status", &[Mcp("agents_status")]),
    ("agents logs", &[Mcp("agents_logs")]),
    ("agents cancel", &[Mcp("agents_cancel")]),
    ("agents send", &[Mcp("agents_send")]),
    ("frameworks list", &[Mcp("frameworks_list")]),
    ("frameworks status", &[Mcp("frameworks_status")]),
    ("frameworks logs", &[Mcp("frameworks_logs")]),
    ("frameworks cancel", &[Mcp("frameworks_cancel")]),
    ("mcp list", &[Mcp("leading_mcp_list")]),
    ("mcp status", &[Mcp("leading_mcp_list")]),
    ("mcp enable", &[Mcp("leading_mcp_enable")]),
    ("mcp disable", &[Mcp("leading_mcp_disable")]),
    ("context-graph related", &[Mcp("context_graph_query")]),
    (
        "context-graph stats",
        &[Route("GET", "/context-graph/stats")],
    ),
    ("context-graph compact", &[Mcp("context_graph_compact")]),
    ("context-graph ingest", &[Mcp("context_graph_ingest")]),
    ("broker grant", &[Mcp("ipc_grant")]),
    ("broker request", &[Mcp("ipc_request")]),
    ("broker negotiate", &[Mcp("ipc_negotiate")]),
    ("broker send", &[Mcp("ipc_send")]),
    ("broker receive", &[Mcp("ipc_receive")]),
    ("telemetry sample", &[Mcp("host_telemetry")]),
    ("privacy status", &[Mcp("privacy_status")]),
    ("privacy consent", &[Mcp("privacy_consent_egress")]),
    ("intent advertise", &[Mcp("intent_advertise")]),
    ("intent need", &[Mcp("intent_need")]),
    ("tx commit", &[Mcp("tx_commit")]),
    ("tx abort", &[Mcp("tx_abort")]),
    ("services status", &[Mcp("os_services")]),
    ("services logs", &[Mcp("os_logs")]),
    ("commits list", &[Mcp("commit_log")]),
    ("os route", &[Mcp("inference_placement")]),
    (
        "os route-reset",
        &[Route("POST", "/runtime/providers/reset")],
    ),
    ("models list", &[Mcp("coding_models_list")]),
    ("models prefer", &[Mcp("coding_models_prefer")]),
    ("run", &[Route("POST", "/v1/chat/completions")]),
    ("extensions list", &[Mcp("extensions_list")]),
    ("extensions status", &[Mcp("extensions_status")]),
    ("extensions validate", &[Mcp("extensions_validate")]),
    ("extensions load", &[Mcp("extensions_load")]),
    ("extensions unload", &[Mcp("extensions_unload")]),
];

/// The plan's explicit exemptions (B3): no socket equivalent is needed. A
/// bare command exempts every subcommand under it.
const EXEMPT: &[(&str, &str)] = &[
    (
        "start",
        "daemon lifecycle: starting a stopped daemon stays with the CLI the desktop runs",
    ),
    ("stop", "daemon lifecycle"),
    ("restart", "daemon lifecycle"),
    ("daemon-start", "daemon lifecycle (internal entry)"),
    ("service-run", "daemon lifecycle (leaf-service spawn mode)"),
    ("install", "install"),
    ("install-release", "install"),
    ("uninstall", "install"),
    ("local", "local tooling"),
    ("clean", "local tooling"),
    ("workflow", "workflow (developer tooling)"),
    ("tasks", "tasks: exempted with workflow per PSP-22"),
    (
        "keys set",
        "plan B4 group 3: setting keys stays local to the CLI and desktop",
    ),
    (
        "keys remove",
        "plan B4 group 3: setting keys stays local to the CLI and desktop",
    ),
    ("admin release", "admin release tooling"),
    ("admin lint", "admin release tooling"),
    ("admin audit-deps", "admin release tooling"),
];

/// B4 groups (1-12) plus three buckets the plan has not assigned yet, each
/// with its ceiling: the most gaps the group may hold. A ceiling only goes
/// down (each B4 PR lowers it with the gaps it closes); raising one is a
/// visible edit that review must justify.
const GROUPS: &[(u8, &str, usize)] = &[
    (1, "Harness (PSP-21, PSP-12)", 7),
    (2, "Operator view", 3),
    (3, "Providers (PSP-24)", 41),
    (4, "Onboarding (PSP-25)", 8),
    (5, "Assurance (PSP-26)", 12),
    (6, "Missions (PSP-27)", 4),
    (7, "Adapters (PSP-28)", 19),
    (8, "Context and IPC (PSP-29)", 11),
    (9, "OS (PSP-30)", 20),
    (10, "Lifecycle (PSP-19)", 0),
    (11, "Extensions (PSP-20, corrected)", 0),
    (12, "9091 replacements (PSP-04 prerequisite)", 0),
    (13, "Not assigned to a B4 group yet", 10),
    (14, "Proposed exemption: process entry points or socket clients (needs a plan decision)", 8),
    (15, "Workspace-bound: the CLI runs in the caller's cwd, the socket serves only the daemon's substrate home (needs an optional, policy-confined workspace argument)", 12),
];

/// Ratchet on the whole allow-list: it only shrinks. Lower this with every
/// gap closed; never raise it to park a new command.
const MAX_KNOWN_GAPS: usize = 155;

/// Commands with no socket surface yet: (command, B4 group, the MCP tool(s)
/// the plan names for it — empty when the plan names none). When every
/// planned tool exists the entry must move to `MAPPED`.
const KNOWN_GAPS: &[(&str, u8, &[&str])] = &[
    // 1. Harness
    // `harness author` authors from a roadmap vector; `harness_plan` plans
    // queue batches (`harness plan`), so it does not close this gap.
    ("harness author", 1, &[]),
    ("harness plan", 1, &["harness_plan"]),
    ("harness run", 1, &["harness_run"]),
    ("harness scorecard", 1, &["harness_scorecard"]),
    ("harness status", 1, &["harness_status"]),
    ("harness models", 1, &["harness_models"]),
    ("harness budget", 1, &["harness_budget"]),
    // 2. Operator view
    ("top", 2, &["operator_snapshot"]),
    ("review", 2, &[]),
    // MCP `status` reports engine version, hardware and downloads, not
    // status_cli's missions, queue, routing, spend, approvals and pid.
    ("status", 2, &[]),
    // 3. Providers
    ("brain status", 3, &["brain_status"]),
    ("brain explain", 3, &["brain_explain"]),
    ("brain bench", 3, &["benchmark_run"]),
    ("benchmark", 3, &["benchmark_run"]),
    ("brain budget", 3, &[]),
    ("brain classify", 3, &[]),
    ("brain export", 3, &[]),
    ("brain import", 3, &[]),
    ("brain matrix", 3, &[]),
    ("brain cache", 3, &[]),
    ("brain reset", 3, &[]),
    ("brain probe", 3, &["provider_probe"]),
    ("keys list", 3, &["keys_status"]),
    ("keys prefer", 3, &["provider_prefer"]),
    ("keys check", 3, &[]),
    ("keys models", 3, &[]),
    ("models probe", 3, &["provider_probe"]),
    ("openweight list", 3, &["provider_list"]),
    ("openweight doctor", 3, &["provider_doctor"]),
    ("openweight prefer", 3, &["provider_prefer"]),
    ("openweight probe", 3, &["provider_probe"]),
    ("openweight setup", 3, &[]),
    ("openweight pull", 3, &[]),
    ("openweight configure", 3, &[]),
    ("openweight reset", 3, &[]),
    ("frontier list", 3, &["provider_list"]),
    ("frontier doctor", 3, &["provider_doctor"]),
    ("frontier prefer", 3, &["provider_prefer"]),
    ("frontier probe", 3, &["provider_probe"]),
    ("frontier setup", 3, &[]),
    ("frontier configure", 3, &[]),
    ("frontier reset", 3, &[]),
    ("openrouter list", 3, &["provider_list"]),
    ("openrouter doctor", 3, &["provider_doctor"]),
    ("openrouter prefer", 3, &["provider_prefer"]),
    ("openrouter probe", 3, &["provider_probe"]),
    ("openrouter setup", 3, &[]),
    ("openrouter models", 3, &[]),
    // select_model / scout_model read only a bare string; MCP tools/call
    // always passes an object, so over the socket they return usage text.
    ("select-model", 3, &[]),
    ("scout-model", 3, &[]),
    // verify_model_download_agent checks local GGUF files; the CLI runs the
    // provisioning ladder (network probe, steps, background provisioner).
    ("verify-download-agent", 3, &[]),
    // 4. Onboarding
    ("setup plan", 4, &["setup_plan"]),
    ("setup apply", 4, &["setup_apply"]),
    ("ecosystem scan", 4, &["ecosystem_scan"]),
    ("ecosystem status", 4, &["ecosystem_scan"]),
    ("ecosystem start", 4, &["ecosystem_start"]),
    ("ecosystem kb", 4, &["ecosystem_kb"]),
    ("ecosystem explain", 4, &["ecosystem_explain"]),
    ("os ecosystem", 4, &["ecosystem_scan"]),
    // 5. Assurance
    ("audit verify", 5, &["audit_verify"]),
    ("audit query", 5, &["audit_query"]),
    ("admin audit-evidence export", 5, &["audit_evidence_export"]),
    ("admin audit-evidence verify", 5, &[]),
    // Bare `susi audit` and `susi admin audit` run the same compliance audit.
    ("audit", 5, &[]),
    ("admin audit", 5, &[]),
    // sovereign_dashboard counts audit events; the CLI serves
    // generate_substrate_report (version, model, axioms, topology).
    ("substrate-report", 5, &[]),
    ("substrate status", 5, &["substrate_status"]),
    ("crown verify", 5, &["crown_verify"]),
    ("crown status", 5, &["crown_verify"]),
    ("os clean", 5, &["clean"]),
    ("os-clean", 5, &["clean"]),
    // 6. Missions
    ("plan", 6, &["plan_run"]),
    ("pulse", 6, &["intent_ingest"]),
    ("admin pulse", 6, &["intent_ingest"]),
    ("blackboard show", 6, &["blackboard_show"]),
    // 7. Adapters
    ("agents doctor", 7, &[]),
    ("agents setup", 7, &[]),
    ("agents configure", 7, &[]),
    ("agents reset", 7, &[]),
    ("agents retry", 7, &["agents_retry"]),
    ("frameworks doctor", 7, &[]),
    ("frameworks setup", 7, &[]),
    ("frameworks configure", 7, &[]),
    ("frameworks reset", 7, &[]),
    ("frameworks retry", 7, &["frameworks_retry"]),
    ("models doctor", 7, &[]),
    ("models setup", 7, &[]),
    ("models configure", 7, &[]),
    ("models reset", 7, &[]),
    ("mcp doctor", 7, &[]),
    ("mcp setup", 7, &[]),
    ("mcp configure", 7, &[]),
    ("mcp reset", 7, &[]),
    ("mcp-add", 7, &[]),
    // 8. Context and IPC
    ("broker pending", 8, &["ipc_pending"]),
    ("broker check", 8, &["ipc_check"]),
    ("broker revoke", 8, &["ipc_revoke"]),
    ("broker list", 8, &["ipc_list"]),
    ("privacy mode", 8, &["privacy_mode"]),
    ("privacy grant", 8, &["privacy_grant"]),
    ("privacy revoke", 8, &["privacy_revoke"]),
    ("intent list", 8, &["intent_list"]),
    ("ambient start", 8, &["ambient_start"]),
    ("tx list", 8, &["tx_list"]),
    ("context-graph sample-os", 8, &[]),
    // 9. OS
    ("os provision", 9, &["os_provision"]),
    ("os manage", 9, &["os_manage"]),
    ("os capabilities", 9, &["os_capabilities"]),
    ("os status", 9, &[]),
    // GET /runtime/models covers only loaded models, not process ownership,
    // stale binaries or per-pid GPU memory.
    ("os runtime", 9, &[]),
    ("services start", 9, &["services_start"]),
    ("services stop", 9, &["services_stop"]),
    // os_services restart runs `kill -TERM`, which does not work on Windows.
    ("services restart", 9, &[]),
    ("commits audit", 9, &["commits_audit"]),
    ("commits replay", 9, &["commits_replay"]),
    ("commits show", 9, &[]),
    ("commits sync", 9, &[]),
    ("commits compact", 9, &[]),
    ("peers probe", 9, &["peers_probe"]),
    ("peers list", 9, &[]),
    ("peers add", 9, &[]),
    ("peers remove", 9, &[]),
    ("peers unban", 9, &[]),
    ("peers rekey", 9, &[]),
    ("peers status", 9, &[]),
    // 13. Not assigned to a B4 group yet
    ("extensions create", 13, &[]),
    ("auto status", 13, &[]),
    ("auto prime", 13, &[]),
    ("deep-scan", 13, &[]),
    ("admin sync", 13, &[]),
    ("admin verify", 13, &[]),
    ("admin reload", 13, &[]),
    ("admin provider-contract", 13, &[]),
    ("admin intent-ladder", 13, &[]),
    // 14. Proposed exemptions (not in the plan's list)
    ("mcp serve", 14, &[]),
    ("mcp call", 14, &[]),
    ("mcp tools", 14, &[]),
    ("gemi", 14, &[]),
    ("harness api", 14, &[]),
    ("harness gateway", 14, &[]),
    ("agents worker", 14, &[]),
    ("frameworks worker", 14, &[]),
    // 15. Workspace-bound: the tools exist but act only on the substrate home.
    ("<goal>", 15, &[]),
    ("agents run", 15, &[]),
    ("agents tasks", 15, &[]),
    ("frameworks run", 15, &[]),
    ("frameworks tasks", 15, &[]),
    ("mcp ledger", 15, &[]),
    ("context-graph show", 15, &[]),
    ("patch", 15, &[]),
    ("ambient pulse", 15, &[]),
    ("tx begin", 15, &[]),
    ("bloat-audit", 15, &[]),
    ("automate", 15, &[]),
];

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(root().join(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

fn kebab(name: &str) -> String {
    let mut out = String::new();
    for (i, c) in name.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('-');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

struct Variant {
    cli: String,
    /// `(module, enum)` of a `#[command(subcommand)]` field, if any.
    subcommand: Option<(Option<String>, String)>,
    /// The subcommand field is an `Option`: the bare command runs too.
    optional: bool,
}

/// Variants of `enum <name>` in a rustfmt-formatted source file: 4-space
/// indented PascalCase lines, named by a preceding `#[command(name = ..)]`
/// or by clap's kebab-case default.
fn enum_variants(source: &str, name: &str) -> Vec<Variant> {
    let start = source
        .find(&format!("enum {name} {{"))
        .unwrap_or_else(|| panic!("enum {name} not found"));
    let mut variants: Vec<Variant> = Vec::new();
    let mut attrs = String::new();
    let mut subcommand_attr = false;
    for line in source[start..].lines().skip(1) {
        if line.starts_with('}') {
            break;
        }
        if let Some(attr) = line.strip_prefix("    #[") {
            attrs.push_str(attr);
            continue;
        }
        if line.starts_with("    //") {
            continue;
        }
        let ident: String = line
            .strip_prefix("    ")
            .filter(|rest| rest.starts_with(|c: char| c.is_ascii_uppercase()))
            .map(|rest| {
                rest.chars()
                    .take_while(char::is_ascii_alphanumeric)
                    .collect()
            })
            .unwrap_or_default();
        if !ident.is_empty() {
            let cli = attrs
                .split("(name = \"")
                .nth(1)
                .or_else(|| attrs.split(", name = \"").nth(1))
                .and_then(|rest| rest.split('"').next())
                .map(str::to_string)
                .unwrap_or_else(|| kebab(&ident));
            variants.push(Variant {
                cli,
                subcommand: None,
                optional: false,
            });
            attrs.clear();
            subcommand_attr = false;
            continue;
        }
        let Some(current) = variants.last_mut() else {
            continue;
        };
        if line.contains("#[command(subcommand)]") {
            subcommand_attr = true;
            continue;
        }
        if subcommand_attr
            && line
                .trim_start()
                .starts_with(|c: char| c.is_ascii_lowercase())
        {
            // `action: Option<Box<tasks_cli::TaskCommands>>,` → (tasks_cli, TaskCommands)
            let ty = line.split(':').skip(1).collect::<Vec<_>>().join(":");
            let path = ty
                .replace("Option<", "")
                .replace("Box<", "")
                .replace(['>', ',', ' '], "");
            let (module, enum_name) = match path.rsplit_once("::") {
                Some((module, enum_name)) => (
                    Some(module.trim_start_matches(':').to_string()),
                    enum_name.to_string(),
                ),
                None => (None, path.clone()),
            };
            current.subcommand = Some((module, enum_name));
            current.optional = ty.contains("Option<");
            subcommand_attr = false;
        }
    }
    variants
}

/// Bare forms of `Option<subcommand>` commands whose default runs a
/// capability no subcommand covers (bare `susi audit` runs the compliance
/// audit; `audit verify` / `audit query` read the chain).
const BARE_DEFAULTS: &[&str] = &["audit"];

/// Subcommands whose own nested subcommands need different socket tools, so
/// each nested one is its own key. Elsewhere a single planned tool takes the
/// nested action as an argument (`ecosystem kb`, `os manage`,
/// `os provision`, `os capabilities`), and the subcommand is one key.
const NESTED: &[&str] = &["admin audit-evidence"];

/// Key for the CLI's primary invocation, `susi <goal>` or a piped goal
/// (src/main.rs: pulse ingest, then a streaming solve in the caller's cwd).
const GOAL_KEY: &str = "<goal>";

/// Every CLI command key: `"<command>"` for leaf commands,
/// `"<command> <subcommand>"` for each direct subcommand (one level deeper
/// for the subcommands in `NESTED`), the bare forms in `BARE_DEFAULTS`, and
/// `GOAL_KEY` for the free-text goal.
fn cli_commands() -> Vec<String> {
    let defs = read("src/cli/defs.rs");
    let mut keys = Vec::new();
    if defs.contains("intent: Vec<String>,") {
        keys.push(GOAL_KEY.to_string());
    }
    for variant in enum_variants(&defs, "Commands") {
        let Some((module, enum_name)) = &variant.subcommand else {
            keys.push(variant.cli.clone());
            continue;
        };
        if BARE_DEFAULTS.contains(&variant.cli.as_str()) {
            assert!(
                variant.optional,
                "`susi {}` has no bare form; remove it from BARE_DEFAULTS",
                variant.cli
            );
            keys.push(variant.cli.clone());
        }
        let source = match module {
            Some(module) => read(&format!("src/cli/{module}.rs")),
            None => defs.clone(),
        };
        let subs = enum_variants(&source, enum_name);
        assert!(!subs.is_empty(), "{} has no subcommands", variant.cli);
        for sub in subs {
            let key = format!("{} {}", variant.cli, sub.cli);
            let Some((nested_module, nested_enum)) = &sub.subcommand else {
                assert!(
                    !NESTED.contains(&key.as_str()),
                    "`susi {key}` has no nested subcommands"
                );
                keys.push(key);
                continue;
            };
            if !NESTED.contains(&key.as_str()) {
                keys.push(key);
                continue;
            }
            let nested_source = match nested_module {
                Some(module) => read(&format!("src/cli/{module}.rs")),
                None => source.clone(),
            };
            let nested = enum_variants(&nested_source, nested_enum);
            assert!(!nested.is_empty(), "{key} has no subcommands");
            if sub.optional {
                keys.push(key.clone());
            }
            for leaf in nested {
                keys.push(format!("{key} {}", leaf.cli));
            }
        }
    }
    keys
}

/// MCP tools the daemon registers (`ToolRegistry::register_meta_tool(registry, "<name>", ..)`).
fn registered_mcp_tools() -> BTreeSet<String> {
    let source = read("crates/susi-daemon/src/gmcp_bootstrap.rs");
    let body = source.split("#[cfg(test)]").next().unwrap_or(&source);
    body.split("ToolRegistry::register_meta_tool(")
        .skip(1)
        .filter_map(|call| call.split('"').nth(1).map(str::to_string))
        .collect()
}

/// A 9091 route is served when its quoted path sits in a `(&Method::<M>, ..)`
/// match arm (the arm line or up to three lines below it).
fn route_served(server: &str, method: &str, path: &str) -> bool {
    let lines: Vec<&str> = server.lines().collect();
    let arm = format!("(&Method::{method},");
    let quoted = format!("\"{path}\"");
    lines.iter().enumerate().any(|(i, line)| {
        line.contains(&quoted)
            && lines[i.saturating_sub(3)..=i]
                .iter()
                .any(|l| l.contains(&arm))
    })
}

fn exempt(key: &str) -> Option<&'static str> {
    let top = key.split(' ').next().unwrap_or(key);
    EXEMPT
        .iter()
        .find(|(k, _)| *k == key || *k == top)
        .map(|(_, why)| *why)
}

#[test]
fn every_cli_command_is_mapped_exempt_or_a_known_gap() {
    let commands = cli_commands();
    assert!(
        commands.len() > 100,
        "parsed only {} commands",
        commands.len()
    );
    let mapped: BTreeSet<&str> = MAPPED.iter().map(|(k, _)| *k).collect();
    let gaps: BTreeSet<&str> = KNOWN_GAPS.iter().map(|(k, _, _)| *k).collect();
    let mut unclassified = Vec::new();
    for key in &commands {
        let classes = [
            exempt(key).is_some() as u8,
            mapped.contains(key.as_str()) as u8,
            gaps.contains(key.as_str()) as u8,
        ];
        match classes.iter().sum::<u8>() {
            1 => {}
            0 => unclassified.push(key.clone()),
            _ => panic!("`susi {key}` is in more than one of MAPPED / EXEMPT / KNOWN_GAPS"),
        }
    }
    assert!(
        unclassified.is_empty(),
        "CLI commands with no socket surface: map each in MAPPED (MCP tool, A2A skill or 9091 route), \
         or add it to KNOWN_GAPS under its B4 group: {unclassified:?}"
    );
}

#[test]
fn parity_tables_name_real_commands_and_served_surfaces() {
    let commands: BTreeSet<String> = cli_commands().into_iter().collect();
    let tops: BTreeSet<&str> = commands
        .iter()
        .map(|k| k.split(' ').next().unwrap_or(k))
        .collect();
    let mut keys = BTreeSet::new();
    for key in MAPPED
        .iter()
        .map(|(k, _)| *k)
        .chain(KNOWN_GAPS.iter().map(|(k, _, _)| *k))
        .chain(EXEMPT.iter().map(|(k, _)| *k))
    {
        assert!(keys.insert(key), "`{key}` listed twice");
        assert!(
            commands.contains(key) || (EXEMPT.iter().any(|(k, _)| *k == key) && tops.contains(key)),
            "`susi {key}` is not a CLI command any more; remove it from the parity tables"
        );
    }
    let tools = registered_mcp_tools();
    assert!(tools.len() >= 80, "parsed only {} MCP tools", tools.len());
    let server = read("crates/susi-server/src/lib.rs");
    let a2a = read("crates/susi-gawd-a2a/src/executor.rs");
    for (key, surfaces) in MAPPED {
        assert!(!surfaces.is_empty(), "`{key}` maps to nothing");
        for surface in *surfaces {
            let served = match surface {
                Mcp(tool) => tools.contains(*tool),
                Route(method, path) => route_served(&server, method, path),
                A2a(skill) => a2a.contains(&format!("id: \"{skill}\"")),
            };
            assert!(
                served,
                "`susi {key}` maps to {surface:?}, which is not served"
            );
        }
    }
    let groups: BTreeSet<u8> = GROUPS.iter().map(|(g, _, _)| *g).collect();
    for (key, group, _) in KNOWN_GAPS {
        assert!(groups.contains(group), "`{key}` has unknown group {group}");
    }
}

#[test]
fn known_gaps_are_ratcheted() {
    assert!(
        KNOWN_GAPS.len() <= MAX_KNOWN_GAPS,
        "KNOWN_GAPS grew to {} (ratchet {MAX_KNOWN_GAPS}): give the new command a socket surface in MAPPED instead of parking it as a gap",
        KNOWN_GAPS.len()
    );
    let mut over = Vec::new();
    for (group, title, ceiling) in GROUPS {
        let count = KNOWN_GAPS.iter().filter(|(_, g, _)| g == group).count();
        if count > *ceiling {
            over.push(format!("{group}. {title}: {count} > {ceiling}"));
        }
    }
    assert!(
        over.is_empty(),
        "known-gap groups over their ceiling: {over:?}"
    );
    let total: usize = GROUPS.iter().map(|(_, _, ceiling)| ceiling).sum();
    assert_eq!(
        total, MAX_KNOWN_GAPS,
        "group ceilings must add up to MAX_KNOWN_GAPS"
    );
}

#[test]
fn known_gaps_only_shrink() {
    let tools = registered_mcp_tools();
    let closed: Vec<String> = KNOWN_GAPS
        .iter()
        .filter(|(_, _, planned)| !planned.is_empty() && planned.iter().all(|t| tools.contains(*t)))
        .map(|(key, _, planned)| format!("`susi {key}` -> {planned:?}"))
        .collect();
    assert!(
        closed.is_empty(),
        "these gaps are now served; move them from KNOWN_GAPS to MAPPED: {closed:?}"
    );
}

#[test]
fn gap_report_is_written_on_request() {
    let Some(path) = std::env::var_os("SUSI_SOCKET_PARITY_REPORT") else {
        return;
    };
    let commands = cli_commands();
    let mut by_group: BTreeMap<u8, Vec<String>> = BTreeMap::new();
    for (key, group, planned) in KNOWN_GAPS {
        let line = if planned.is_empty() {
            format!("- `susi {key}`")
        } else {
            format!("- `susi {key}` -> planned: {}", planned.join(", "))
        };
        by_group.entry(*group).or_default().push(line);
    }
    let exempt_count = commands.iter().filter(|k| exempt(k).is_some()).count();
    let mut out = format!(
        "# Socket parity gaps (generated by tests/socket_parity.rs)\n\n\
         CLI commands: {} | mapped: {} | exempt: {} | known gaps: {}\n",
        commands.len(),
        MAPPED.len(),
        exempt_count,
        KNOWN_GAPS.len()
    );
    for (group, title, _) in GROUPS {
        let Some(lines) = by_group.get(group) else {
            continue;
        };
        out.push_str(&format!("\n## {group}. {title} ({})\n\n", lines.len()));
        for line in lines {
            out.push_str(line);
            out.push('\n');
        }
    }
    let path = Path::new(&path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, out).unwrap();
}
