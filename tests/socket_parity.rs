//! Socket parity map (zero-UI migration plan B3).
//!
//! The CEO directive: every core feature must be reachable from the
//! commercial desktop over the standards socket. This test maps every
//! `susi` CLI command (each `Commands` variant in `src/cli/defs.rs`, and each
//! direct subcommand of a variant that has them) to a socket surface: an MCP
//! tool, an A2A skill, or a 9091 route. Commands the plan exempts (daemon
//! lifecycle, install, local tooling, workflow, tasks, admin
//! release/lint/audit-deps), process entry points and socket clients are
//! listed in `EXEMPT` with their reason. Everything else was a `KNOWN_GAPS`
//! entry, grouped like plan item B4; B4 burned the list down to empty and
//! the ratchet keeps it there.
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
        &[
            Mcp("context_graph_stats"),
            Route("GET", "/context-graph/stats"),
        ],
    ),
    // The graph is the one substrate-home log the CLI and daemon share; the
    // workspace is only a filter key, passed as an absolute path.
    ("context-graph show", &[Mcp("context_graph_show")]),
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
    ("os route-reset", &[Mcp("inference_reset_provider")]),
    ("models list", &[Mcp("coding_models_list")]),
    ("models prefer", &[Mcp("coding_models_prefer")]),
    ("run", &[Route("POST", "/v1/chat/completions")]),
    // Packs live in the shared config dir, so the daemon and the CLI act on
    // the same state; `validate` takes an absolute path over the socket.
    ("extensions list", &[Mcp("extensions_list")]),
    ("extensions status", &[Mcp("extensions_status")]),
    ("extensions validate", &[Mcp("extensions_validate")]),
    ("extensions load", &[Mcp("extensions_load")]),
    ("extensions unload", &[Mcp("extensions_unload")]),
    ("brain pareto", &[Mcp("brain_pareto")]),
    // B4 group 8: the broker, MAC policy and intent bus are host state the
    // CLI and the daemon share; none of these reads the caller's cwd.
    ("broker pending", &[Mcp("ipc_pending")]),
    ("broker check", &[Mcp("ipc_check")]),
    ("broker revoke", &[Mcp("ipc_revoke")]),
    ("broker list", &[Mcp("ipc_list")]),
    ("privacy mode", &[Mcp("privacy_mode")]),
    ("privacy grant", &[Mcp("privacy_grant")]),
    ("privacy revoke", &[Mcp("privacy_revoke")]),
    ("intent list", &[Mcp("intent_list")]),
    // The sample lands in the shared substrate-home graph; the workspace is
    // only its tag, passed as an absolute path.
    ("context-graph sample-os", &[Mcp("context_graph_sample_os")]),
    // B4 group 5: explicit absolute paths, and SUSI-owned residue.
    (
        "admin audit-evidence export",
        &[Mcp("audit_evidence_export")],
    ),
    ("os clean", &[Mcp("clean")]),
    ("os-clean", &[Mcp("clean")]),
    // B4 group 4: setup and the ecosystem scan live in the shared config
    // dir; the knowledge base takes an absolute store dir.
    ("setup plan", &[Mcp("setup_plan")]),
    ("setup apply", &[Mcp("setup_apply")]),
    ("ecosystem scan", &[Mcp("ecosystem_scan")]),
    ("ecosystem status", &[Mcp("ecosystem_scan")]),
    ("ecosystem start", &[Mcp("ecosystem_start")]),
    ("ecosystem kb", &[Mcp("ecosystem_kb")]),
    ("ecosystem explain", &[Mcp("ecosystem_explain")]),
    // B4 group 9: cloud CLIs, the service table, the replicated ledger and
    // the peer roster are host state; services_restart terminates with the
    // platform's own process tool on Windows and Unix.
    ("os provision", &[Mcp("os_provision")]),
    ("services start", &[Mcp("services_start")]),
    ("services stop", &[Mcp("services_stop")]),
    ("services restart", &[Mcp("services_restart")]),
    ("commits audit", &[Mcp("commits_audit")]),
    ("commits replay", &[Mcp("commits_replay")]),
    ("commits show", &[Mcp("commits_show")]),
    ("commits sync", &[Mcp("commits_sync")]),
    ("commits compact", &[Mcp("commits_compact")]),
    ("peers probe", &[Mcp("peers_probe")]),
    ("peers list", &[Mcp("peers_list")]),
    ("peers status", &[Mcp("peers_status")]),
    // B4 group 3: providers, keys and the brain. Provider, key, brain and
    // routing state is host state in the config dir; the tools call
    // `susi_daemon::provider_ops`, as the CLI does. Key values never cross.
    ("openweight list", &[Mcp("provider_list")]),
    ("openweight doctor", &[Mcp("provider_doctor")]),
    ("openweight setup", &[Mcp("provider_setup")]),
    ("openweight pull", &[Mcp("provider_pull")]),
    ("openweight configure", &[Mcp("provider_configure")]),
    ("openweight reset", &[Mcp("provider_reset")]),
    ("openweight prefer", &[Mcp("provider_prefer")]),
    ("openweight probe", &[Mcp("provider_probe")]),
    ("frontier list", &[Mcp("provider_list")]),
    ("frontier doctor", &[Mcp("provider_doctor")]),
    ("frontier setup", &[Mcp("provider_setup")]),
    ("frontier configure", &[Mcp("provider_configure")]),
    ("frontier reset", &[Mcp("provider_reset")]),
    ("frontier prefer", &[Mcp("provider_prefer")]),
    ("frontier probe", &[Mcp("provider_probe")]),
    ("openrouter list", &[Mcp("provider_list")]),
    ("openrouter doctor", &[Mcp("provider_doctor")]),
    ("openrouter setup", &[Mcp("provider_setup")]),
    ("openrouter prefer", &[Mcp("provider_prefer")]),
    ("openrouter probe", &[Mcp("provider_probe")]),
    ("openrouter models", &[Mcp("provider_models")]),
    ("models probe", &[Mcp("provider_probe")]),
    ("keys list", &[Mcp("keys_status")]),
    ("keys prefer", &[Mcp("keys_prefer")]),
    ("keys check", &[Mcp("keys_check")]),
    ("keys models", &[Mcp("keys_models")]),
    ("brain status", &[Mcp("brain_status")]),
    ("brain budget", &[Mcp("brain_budget")]),
    ("brain classify", &[Mcp("brain_classify")]),
    ("brain explain", &[Mcp("brain_explain")]),
    ("brain export", &[Mcp("brain_export")]),
    ("brain import", &[Mcp("brain_import")]),
    ("brain matrix", &[Mcp("brain_matrix")]),
    ("brain cache", &[Mcp("brain_cache")]),
    ("brain reset", &[Mcp("brain_reset")]),
    ("brain bench", &[Mcp("brain_bench")]),
    ("brain probe", &[Mcp("brain_probe")]),
    ("benchmark", &[Mcp("benchmark_run")]),
    // select_model and scout_model read an object now; the daemon downloads
    // on its agent where the exiting CLI downloads in the foreground.
    ("select-model", &[Mcp("select_model")]),
    ("scout-model", &[Mcp("scout_model")]),
    ("verify-download-agent", &[Mcp("verify_download_agent")]),
    // B4 group 7: adapter overrides are host configuration; retry reads
    // the task in the server workspace, or a policy-admitted `workspace`.
    ("agents doctor", &[Mcp("adapter_doctor")]),
    ("agents setup", &[Mcp("adapter_setup")]),
    ("agents configure", &[Mcp("adapter_configure")]),
    ("agents reset", &[Mcp("adapter_reset")]),
    ("agents retry", &[Mcp("agents_retry")]),
    ("frameworks doctor", &[Mcp("adapter_doctor")]),
    ("frameworks setup", &[Mcp("adapter_setup")]),
    ("frameworks configure", &[Mcp("adapter_configure")]),
    ("frameworks reset", &[Mcp("adapter_reset")]),
    ("frameworks retry", &[Mcp("frameworks_retry")]),
    ("models doctor", &[Mcp("adapter_doctor")]),
    ("models setup", &[Mcp("adapter_setup")]),
    ("models configure", &[Mcp("adapter_configure")]),
    ("models reset", &[Mcp("adapter_reset")]),
    ("mcp doctor", &[Mcp("adapter_doctor")]),
    ("mcp setup", &[Mcp("adapter_setup")]),
    ("mcp configure", &[Mcp("adapter_configure")]),
    ("mcp reset", &[Mcp("adapter_reset")]),
    ("mcp-add", &[Mcp("mcp_add")]),
    // B4 group 1: the harness operator commands call
    // `susi_harness::operator`; paths are absolute and `repo` (the CLI's
    // cwd) is admitted by the MAC policy. A run starts on a daemon thread.
    ("harness status", &[Mcp("harness_status")]),
    ("harness budget", &[Mcp("harness_budget")]),
    ("harness models", &[Mcp("harness_models")]),
    ("harness plan", &[Mcp("harness_plan")]),
    ("harness author", &[Mcp("harness_author")]),
    // A socket run always drives the daemon's own binary: the CLI's
    // `--susi` override is a local operator choice the socket refuses.
    ("harness run", &[Mcp("harness_run"), Mcp("harness_runs")]),
    // B4 groups 2, 4 and 9: host views built by `susi_daemon` view modules
    // (`status_report`, `host_views`, `ecosystem_view`, `os_status`,
    // `runtime_view`) and roster changes by `susi_core::peer_admin`, the
    // functions the CLI calls. A `workspace` argument stands in for the
    // CLI's cwd and is admitted by the MAC policy.
    // `operator_status` is `susi status --json`; with `detail` it is the
    // whole human view (`status_report::to_json` of the `UnifiedStatus`
    // that `render` prints).
    ("status", &[Mcp("operator_status")]),
    ("review", &[Mcp("operator_review")]),
    ("os ecosystem", &[Mcp("os_ecosystem")]),
    ("os status", &[Mcp("os_status")]),
    ("os runtime", &[Mcp("os_runtime")]),
    // `os manage <service|model|mcp|agent|framework>` is an alias facade
    // over other commands; each of its actions has its tool.
    (
        "os manage",
        &[
            Mcp("os_services"),
            Mcp("services_start"),
            Mcp("services_stop"),
            Mcp("services_restart"),
            Mcp("os_logs"),
            Mcp("coding_models_list"),
            Mcp("coding_models_prefer"),
            Mcp("scout_model"),
            Mcp("model_uninstall"),
            Mcp("runtime_models_load"),
            Mcp("runtime_models_unload"),
            Mcp("leading_mcp_list"),
            Mcp("leading_mcp_enable"),
            Mcp("leading_mcp_disable"),
            Mcp("adapter_doctor"),
            Mcp("adapter_configure"),
            Mcp("adapter_reset"),
            Mcp("agents_list"),
            Mcp("agents_run"),
            Mcp("agents_tasks"),
            Mcp("agents_cancel"),
            Mcp("frameworks_list"),
            Mcp("frameworks_run"),
            Mcp("frameworks_tasks"),
            Mcp("frameworks_cancel"),
        ],
    ),
    ("peers add", &[Mcp("peers_add")]),
    ("peers remove", &[Mcp("peers_remove")]),
    ("peers unban", &[Mcp("peers_unban")]),
    ("peers rekey", &[Mcp("peers_rekey")]),
    // B4 group 10: `susi stop` stops this instance's daemons; over the
    // socket the daemon replies first, then makes the same call.
    ("stop", &[Mcp("daemon_shutdown")]),
    // Formerly unassigned (bucket 13).
    ("extensions create", &[Mcp("extensions_create")]),
    ("auto status", &[Mcp("auto_status")]),
    ("auto prime", &[Mcp("auto_prime")]),
    ("deep-scan", &[Mcp("deep_scan")]),
    ("admin reload", &[Mcp("config_reload")]),
    ("admin provider-contract", &[Mcp("provider_contract")]),
    ("admin intent-ladder", &[Mcp("intent_ladder")]),
    // Formerly workspace-bound (bucket 15): each tool now takes an
    // optional `workspace` (the CLI's cwd), absolute, an existing directory
    // and admitted by the MAC policy (filesystem.read, or filesystem.write
    // for tools that change it); without it the tool acts on the server
    // workspace as before.
    ("<goal>", &[Mcp("intent_ingest"), Mcp("mission_run")]),
    ("automate", &[Mcp("susi_solve")]),
    ("agents run", &[Mcp("agents_run")]),
    ("agents tasks", &[Mcp("agents_tasks")]),
    ("frameworks run", &[Mcp("frameworks_run")]),
    ("frameworks tasks", &[Mcp("frameworks_tasks")]),
    ("mcp ledger", &[Mcp("ledger_verify")]),
    ("patch", &[Mcp("apply_patch_cycle")]),
    ("ambient pulse", &[Mcp("ambient_pulse")]),
    ("ambient start", &[Mcp("ambient_start")]),
    ("tx begin", &[Mcp("tx_begin")]),
    ("tx list", &[Mcp("tx_list")]),
    ("bloat-audit", &[Mcp("bloat_audit")]),
    ("top", &[Mcp("operator_snapshot")]),
    ("plan", &[Mcp("plan_run")]),
    ("pulse", &[Mcp("intent_ingest")]),
    ("admin pulse", &[Mcp("intent_ingest")]),
    ("blackboard show", &[Mcp("blackboard_show")]),
    ("audit verify", &[Mcp("audit_verify")]),
    ("audit query", &[Mcp("audit_query")]),
    ("audit", &[Mcp("compliance_audit")]),
    ("admin audit", &[Mcp("compliance_audit")]),
    ("substrate-report", &[Mcp("substrate_report")]),
    ("substrate status", &[Mcp("substrate_status")]),
    ("crown verify", &[Mcp("crown_verify")]),
    ("crown status", &[Mcp("crown_verify")]),
    ("os capabilities", &[Mcp("os_capabilities")]),
];

/// The plan's explicit exemptions (B3): no socket equivalent is needed. A
/// bare command exempts every subcommand under it.
const EXEMPT: &[(&str, &str)] = &[
    (
        "start",
        "daemon lifecycle: starting a stopped daemon stays with the CLI the desktop runs",
    ),
    (
        "restart",
        "daemon lifecycle: a daemon cannot start its own successor after it stops; restarting stays with the CLI the desktop runs (stop is mapped to daemon_shutdown)",
    ),
    ("daemon-start", "daemon lifecycle (internal entry)"),
    ("service-run", "daemon lifecycle (leaf-service spawn mode)"),
    ("install", "install"),
    ("install-release", "install"),
    ("uninstall", "install"),
    ("local", "local tooling"),
    ("clean", "local tooling"),
    (
        "workflow",
        "refusal only: the worktree workflow moved to Susi Commercial Apps (`susi-commercial workflow`)",
    ),
    (
        "tasks",
        "local tooling: `eval-corpus` records RSI evidence in the checkout; the task queue moved to Susi Commercial Apps (`susi-commercial tasks`)",
    ),
    (
        "keys set",
        "plan B4 group 3: setting keys stays local to the CLI and desktop",
    ),
    (
        "keys remove",
        "plan B4 group 3: setting keys stays local to the CLI and desktop",
    ),
    ("admin lint", "local tooling: runs cargo clippy in the checkout"),
    ("admin audit-deps", "local tooling: runs cargo audit in the checkout"),
    (
        "admin audit-evidence verify",
        "takes signing secrets (key-id=secret) as arguments; verification stays local so secrets never cross the socket",
    ),
    (
        "harness scorecard",
        "runs scripts/check-rsi-scorecard.py, which the Rust-only migration deleted (5a90467ad): the CLI command itself no longer works, so there is no capability to expose until it is rebuilt natively",
    ),
    (
        "mcp serve",
        "process entry point: runs Susi's MCP stdio server; the socket is that MCP server",
    ),
    (
        "mcp call",
        "socket client: calls a tool on another MCP endpoint; the desktop calls tools/call on the socket itself, where registered servers' tools are served as server:tool proxies",
    ),
    (
        "mcp tools",
        "socket client: lists another MCP endpoint's tools; the desktop calls tools/list on the socket itself",
    ),
    ("gemi", "process entry point: serves the GEMI HTTP listener"),
    (
        "harness api",
        "process entry point: serves the harness JSON/SSE API on its own port",
    ),
    (
        "harness gateway",
        "process entry point: serves the metered multi-provider gateway on its own port",
    ),
    (
        "agents worker",
        "process entry point: the detached worker `agents run` spawns; over the socket tasks run on daemon threads (agents_run, agents_retry)",
    ),
    (
        "frameworks worker",
        "process entry point: the detached worker `frameworks run` spawns; over the socket tasks run on daemon threads (frameworks_run, frameworks_retry)",
    ),
];

/// B4 groups (1-12) plus three buckets the plan has not assigned yet, each
/// with its ceiling: the most gaps the group may hold. A ceiling only goes
/// down (each B4 PR lowers it with the gaps it closes); raising one is a
/// visible edit that review must justify.
const GROUPS: &[(u8, &str, usize)] = &[
    (1, "Harness (PSP-21, PSP-12)", 0),
    (2, "Operator view", 0),
    (3, "Providers (PSP-24)", 0),
    (4, "Onboarding (PSP-25)", 0),
    (5, "Assurance (PSP-26)", 0),
    (6, "Missions (PSP-27)", 0),
    (7, "Adapters (PSP-28)", 0),
    (8, "Context and IPC (PSP-29)", 0),
    (9, "OS (PSP-30)", 0),
    (10, "Lifecycle (PSP-19)", 0),
    (11, "Extensions (PSP-20, corrected)", 0),
    (12, "9091 replacements (PSP-04 prerequisite)", 0),
    (13, "Not assigned to a B4 group yet", 0),
    (14, "Proposed exemption: process entry points or socket clients (needs a plan decision)", 0),
    (15, "Workspace-bound: the CLI runs in the caller's cwd, the socket serves only the daemon's substrate home (needs an optional, policy-confined workspace argument)", 0),
];

/// Ratchet on the whole allow-list: it only shrinks. Lower this with every
/// gap closed; never raise it to park a new command.
const MAX_KNOWN_GAPS: usize = 0;

/// Commands with no socket surface yet: (command, B4 group, the MCP tool(s)
/// the plan names for it — empty when the plan names none). When every
/// planned tool exists the entry must move to `MAPPED`.
const KNOWN_GAPS: &[(&str, u8, &[&str])] = &[
    // Empty: B4 closed every gap (groups 1-14 mapped or exempted, bucket 15
    // mapped through the policy-confined `workspace` argument). A new CLI
    // command gets a socket surface in MAPPED, or a reasoned EXEMPT entry.
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

/// Source text that defines the clap enums of a `src/cli/<module>.rs`
/// family. D-SP-4 Stage 4 moves the derive enums family by family into
/// `crates/susi-cli-defs/src/<family>.rs` (`<module>` minus `_cli`), and the
/// root file only re-exports them, so a moved family is read from the
/// definitions crate. `enum_variants` panics when the named enum is missing
/// from the file read, so neither location can pass vacuously.
fn cli_module_source(module: &str, moved: &mut usize) -> String {
    let family = module.strip_suffix("_cli").unwrap_or(module);
    let defs_rel = format!("crates/susi-cli-defs/src/{family}.rs");
    if root().join(&defs_rel).is_file() {
        *moved += 1;
        read(&defs_rel)
    } else {
        read(&format!("src/cli/{module}.rs"))
    }
}

/// Every CLI command key: `"<command>"` for leaf commands,
/// `"<command> <subcommand>"` for each direct subcommand (one level deeper
/// for the subcommands in `NESTED`), the bare forms in `BARE_DEFAULTS`, and
/// `GOAL_KEY` for the free-text goal.
fn cli_commands() -> Vec<String> {
    let defs = read("src/cli/defs.rs");
    assert!(
        defs.contains("intent: Vec<String>,"),
        "src/cli/defs.rs must define the trailing `intent: Vec<String>,` field"
    );
    let mut keys = vec![GOAL_KEY.to_string()];
    let mut moved_sources = 0usize;
    let top_variants = enum_variants(&defs, "Commands");
    assert!(
        !top_variants.is_empty(),
        "src/cli/defs.rs `Commands` enum yielded zero variants"
    );
    for variant in top_variants {
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
            Some(module) => cli_module_source(module, &mut moved_sources),
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
                Some(module) => cli_module_source(module, &mut moved_sources),
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
    assert!(
        keys.len() >= 100,
        "cli_commands scanner must discover the full CLI surface, got {}",
        keys.len()
    );
    assert!(
        moved_sources > 0,
        "cli_commands read zero families from crates/susi-cli-defs/src; update scanner path if moved"
    );
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
    let excess = KNOWN_GAPS.len().saturating_sub(MAX_KNOWN_GAPS);
    assert!(
        excess == 0,
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
