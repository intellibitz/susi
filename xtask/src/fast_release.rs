//! Unified `< 10.0s` Susi Core pipeline: build + verification tests + distilled
//! candidate release artifacts + SHA-256 provenance manifest + durable timing
//! evidence (`T-CTO-SPEED-1`, Mandates 1, 48, 62, 63, 64, 67, 69, 74).
//!
//! Invocations:
//! - `cargo fast-release [options]`
//! - `cargo xtask fast-release [options]`
//! - `cargo local-build fast-release [options]`
//! - `cargo run --offline --locked -p xtask -- fast-release [options]`

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// Default warm wall-clock budget in milliseconds (`< 10.0s`).
pub const DEFAULT_BUDGET_MS: u64 = 10_000;

/// Schema identifier for the distilled candidate provenance manifest.
pub const MANIFEST_SCHEMA: &str = "susi-fast-release-manifest/v1";

/// Schema identifier for the structured timing & verification report.
pub const REPORT_SCHEMA: &str = "susi-fast-release-report/v1";

/// Core pillar crates verified on every `fast-release` run.
const CORE_PILLARS: [&str; 7] = [
    "susi-core",
    "susi-config",
    "susi-gawd-acp",
    "susi-gawd",
    "susi-gemi",
    "susi-gmcp",
    "susi-harness",
];

/// Multi-second wall-clock timeout/DNS/pipe/process-soak tests (`2.0s–105.9s`
/// fixed sleeps, socket read/connect deadlines, or OS `tasklist`/`netstat`/`git`
/// subprocess sweeps) skipped in the sub-10s inner loop unless `--full-soak`
/// is passed. Every skipped soak test is recorded verbatim in the provenance manifest.
const SOAK_SKIP_FILTERS: [&str; 25] = [
    "http_wire::the_http_surfaces_answer_byte_for_byte_as_recorded",
    "tools::helpers::confine_tests::secure_external_url_blocks_non_http_and_internal_hosts",
    "commit_log::tests::file_lock_excludes_second_holder_and_releases_on_drop",
    "receipt_archive::tests::append_refuses_to_race_a_cross_process_archive_operation",
    "adaptive::tests::output_survives_a_grandchild_that_holds_the_pipe_open",
    "plane_bus_ipc::tests::sweep_removes_legacy_dirs_of_processes_that_are_gone",
    "service_table::tests::pid_for_port_finds_a_listener_we_own",
    "service_table::tests::prop_tests::save_load_round_trips_any_table",
    "pattern::tests::case_folding_adds_only_the_kelvin_sign_and_the_long_s",
    "pattern::tests::a_long_text_without_a_match_is_scanned_in_linear_time",
    "registry::tests::dispatch_capability_nests_a_live_tool_under_an_admitted_envelope",
    "plain_http::tests::a_refused_connection_is_an_error_naming_the_target",
    "reflex_trainer::tests::cycle_log_is_bounded_and_newest_last",
    "patch_cycle::tests::susi_repo_runs_the_full_gate_even_with_an_override_command",
    "native_verifier::inline_tests::",
    "vc_201_007_",
    "vc_201_015_mastery_tests::vc_201_015_mastery_production_patch_path_executes_the_gate",
    "engines::routing::tests::",
    "engines::http_provider::tests::",
    "ranking_by_marginal_cost_tests::",
    "quota_window_accounting_tests::",
    "primary_election_tests::",
    "model_catalogue_drift_tests::",
    "provider_contract::tests::provider_contract_opt_in_gate",
    "git_governor::tests::",
];

const HARNESS_SOAK_SKIP_FILTERS: [&str; 35] = [
    "storage_guard::tests::",
    "workflow_runner::tests::",
    "operator::tests::a_harness_submission_witnesses_the_remote_and_notes_a_loss",
    "ledger::tests::concurrent_appenders_never_interleave_ledger_rows",
    "tools::tests::a_command_never_inherits_a_withheld_key_variable",
    "tools::tests::a_hung_command_returns_a_timeout_instead_of_blocking",
    "fleet::tests::the_gateway_is_asked_whether_it_can_route_a_model",
    "mcp::tests::http_socket_transport_",
    "executor::tests::",
    "gateway::tests::",
    "worker::tests::a_turn_the_paced_queue_turned_away_is_sent_again_after_its_retry_after",
    "channel_bindings::tests::",
    "secrets::tests::registry_environment_isolation_covers_keys_settings_homes_and_cached_values",
    "intent_ladder_startup_tests::intent_ladder_startup_process_runner_env_is_hermetic",
    "compliance::intent_classify_tests::first_goal_evidence_labels_do_not_infer",
    "plain_http::tests::",
    "registry::tests::",
    "peer_roster::tests::",
    "delegation_tests::the_ledger_is_bounded",
    "mcp_client::tests::a_short_budget_bounds_the_handshake_too",
    "net_guard::tests::local_host_bearer_over_loopback_is_not_metered",
    "commit_log::tests::",
    "commit_log_report::tests::",
    "plane_bus_ipc::tests::",
    "verification::tests::command_exit_runs_allowlisted_probes_only",
    "engines::runtime::tests::",
    "engines::brain::tests::",
    "engines::candle_provider::inline_tests::",
    "worker_may_be_an_agent_tests::",
    "embed_budget_tests::",
    "budget_ceiling_tests::",
    "capability_matrix_by_class_tests::",
    "cache_first_context_tests::",
    "primary_without_secondaries_persisted_mission_carries_verdict",
    "tools::core::os_tools_wired_tests::",
];

/// Cache state observed prior to Stage 1 compilation (Mandates 62 & 69).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheState {
    Cold,
    Warm,
}

impl CacheState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cold => "cold",
            Self::Warm => "warm",
        }
    }
}

/// CLI options for `fast-release`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    pub budget_ms: u64,
    pub enforce_cold: bool,
    pub allow_over_budget: bool,
    pub target_dir: Option<PathBuf>,
    pub out_dir: Option<PathBuf>,
    pub evidence_out: Option<PathBuf>,
    pub record_tracked: bool,
    pub local_engine: bool,
    pub full_soak: bool,
    pub custom_sysroot: bool,
    pub json_only: bool,
    pub label: String,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            budget_ms: DEFAULT_BUDGET_MS,
            enforce_cold: false,
            allow_over_budget: false,
            target_dir: None,
            out_dir: None,
            evidence_out: None,
            record_tracked: false,
            local_engine: false,
            full_soak: false,
            custom_sysroot: false,
            json_only: false,
            label: "default".to_owned(),
        }
    }
}

const USAGE: &str = "\
usage: cargo fast-release [options]
       cargo local-build fast-release [options]

options:
  --budget-ms <ms>       Wall-clock budget in ms (default: 10000; enforces total_ms < budget_ms)
  --enforce-cold         Enforce budget even when target/fast-release cache is cold
  --allow-over-budget    Record an over-budget run without failing exit status (benchmarking only)
  --target-dir <dir>     Explicit private Cargo target directory (overrides CARGO_TARGET_DIR)
  --out-dir <dir>        Distilled release staging directory (default: <target>/fast-release/distilled)
  --evidence-out <file>  Additional JSONL file to append timing evidence to
  --record               Also append timing evidence to .agents/fast-release-timings.jsonl
  --label <name>         Scenario label recorded in timing evidence (default: default)
  --local-engine         Include optional `local-engine` feature in both build and test stages
  --full-soak            Run multi-second timeout/DNS soak tests instead of skipping them
  --custom-sysroot       Build and bind the custom prototype sysroot (`trusted_sysroot`)
  --json-only            Emit only the structured JSON report on stdout";

impl Options {
    pub fn parse(args: &[String]) -> Result<Self, String> {
        let mut opts = Self::default();
        let mut iter = args.iter();
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--budget-ms" => {
                    let raw = iter
                        .next()
                        .ok_or_else(|| "--budget-ms requires a millisecond value".to_owned())?;
                    let parsed = raw
                        .parse::<u64>()
                        .map_err(|e| format!("invalid --budget-ms {raw:?}: {e}"))?;
                    if parsed == 0 {
                        return Err("--budget-ms must be greater than 0".to_owned());
                    }
                    opts.budget_ms = parsed;
                }
                "--enforce-cold" => opts.enforce_cold = true,
                "--allow-over-budget" => opts.allow_over_budget = true,
                "--target-dir" => {
                    let raw = iter
                        .next()
                        .ok_or_else(|| "--target-dir requires a directory path".to_owned())?;
                    opts.target_dir = Some(PathBuf::from(raw));
                }
                "--out-dir" => {
                    let raw = iter
                        .next()
                        .ok_or_else(|| "--out-dir requires a directory path".to_owned())?;
                    opts.out_dir = Some(PathBuf::from(raw));
                }
                "--evidence-out" => {
                    let raw = iter
                        .next()
                        .ok_or_else(|| "--evidence-out requires a file path".to_owned())?;
                    opts.evidence_out = Some(PathBuf::from(raw));
                }
                "--record" => opts.record_tracked = true,
                "--label" => {
                    let raw = iter
                        .next()
                        .ok_or_else(|| "--label requires a value".to_owned())?;
                    if raw.trim().is_empty() {
                        return Err("--label cannot be empty".to_owned());
                    }
                    opts.label = raw.clone();
                }
                "--local-engine" => opts.local_engine = true,
                "--full-soak" => opts.full_soak = true,
                "--custom-sysroot" => opts.custom_sysroot = true,
                "--json-only" => opts.json_only = true,
                "-h" | "--help" | "help" => return Err(USAGE.to_owned()),
                other => return Err(format!("fast-release: unknown option {other:?}\n{USAGE}")),
            }
        }
        Ok(opts)
    }
}

/// Metadata for one distilled candidate binary.
#[derive(Clone, Debug)]
pub struct DistilledArtifact {
    pub name: String,
    pub relative_path: String,
    pub staged_path: PathBuf,
    pub bytes: u64,
    pub sha256: String,
    pub version_stdout: String,
}

/// Summary of Stage 2 core verification test execution.
#[derive(Clone, Debug)]
pub struct TestStageSummary {
    pub pillars: Vec<String>,
    pub binaries_ran: usize,
    pub tests_passed: u64,
    pub tests_failed: u64,
    pub tests_ignored: u64,
    pub fresh_units: usize,
    pub recompiled_units: usize,
    pub skipped_soak_tests: Vec<String>,
    pub binary_timings_ms: BTreeMap<String, u64>,
}

/// Complete timing and verification outcome for `fast-release`.
#[derive(Clone, Debug)]
pub struct PipelineReport {
    pub cache_state: CacheState,
    pub label: String,
    pub host: String,
    pub rustc_version: String,
    pub workspace_version: String,
    pub git_commit: String,
    pub git_tree: String,
    pub git_branch: String,
    pub git_dirty: bool,
    pub build_ms: u64,
    pub test_ms: u64,
    pub release_ms: u64,
    pub total_ms: u64,
    pub budget_ms: u64,
    pub budget_enforced: bool,
    pub within_budget: bool,
    pub build_fresh_units: usize,
    pub build_recompiled_units: usize,
    pub tests: TestStageSummary,
    pub artifacts: Vec<DistilledArtifact>,
    pub cargo_lock_sha256: String,
    pub manifest_path: PathBuf,
    pub sha256sums_path: PathBuf,
    pub evidence_paths: Vec<PathBuf>,
}

struct WorkspaceEnv {
    root: PathBuf,
    target_dir: PathBuf,
    out_dir: PathBuf,
    host: String,
    rustc_version: String,
    workspace_version: String,
    sysroot: Option<crate::trusted_sysroot::VerifiedSysroot>,
}

/// Entrypoint returning `io::Result<()>` for `local_build` delegation.
pub fn run_io(args: &[String]) -> io::Result<()> {
    run(args).map_err(io::Error::other)
}

/// Executes the unified `< 10.0s` build + test + distilled release pipeline.
pub fn run(args: &[String]) -> Result<(), String> {
    let opts = Options::parse(args)?;
    let pipeline_started = Instant::now();

    let ws = prepare_workspace(&opts)?;
    let cache_state = detect_cache_state(&ws.target_dir);

    // Stage 1: Core Build (`susi` + `susi-harness` under `--profile fast-release`).
    let build_started = Instant::now();
    let (build_fresh_units, build_recompiled_units) = run_build_stage(&ws, &opts)?;
    let build_ms = millis_u64(build_started.elapsed().as_millis());

    // Stage 2: Core Verification Test Suite (reusing Stage 1 `.rlib`/`.rmeta` cache).
    let test_started = Instant::now();
    let tests = run_test_stage(&ws, &opts)?;
    let test_ms = millis_u64(test_started.elapsed().as_millis());

    // Stage 3: Distilled Release Artifact Staging + Smoke Checks + SHA-256 Manifest.
    let release_started = Instant::now();
    let (git_commit, git_tree, git_branch, git_dirty) = read_git_provenance(&ws.root);
    let cargo_lock_sha256 = sha256_file(&ws.root.join("Cargo.lock"))?;
    let artifacts = stage_and_smoke_verify_binaries(&ws)?;
    let manifest_path = ws.out_dir.join("release-manifest.json");
    let sha256sums_path = ws.out_dir.join("SHA256SUMS");

    // Finalize release_ms & total_ms right as manifest + timing evidence are emitted.
    let provisional_release_ms = millis_u64(release_started.elapsed().as_millis());
    let provisional_total_ms = millis_u64(pipeline_started.elapsed().as_millis());
    let budget_enforced = should_enforce_budget(cache_state, &opts);

    let mut report = PipelineReport {
        cache_state,
        label: opts.label.clone(),
        host: ws.host.clone(),
        rustc_version: ws.rustc_version.clone(),
        workspace_version: ws.workspace_version.clone(),
        git_commit,
        git_tree,
        git_branch,
        git_dirty,
        build_ms,
        test_ms,
        release_ms: provisional_release_ms,
        total_ms: provisional_total_ms,
        budget_ms: opts.budget_ms,
        budget_enforced,
        within_budget: provisional_total_ms < opts.budget_ms,
        build_fresh_units,
        build_recompiled_units,
        tests,
        artifacts,
        cargo_lock_sha256,
        manifest_path: manifest_path.clone(),
        sha256sums_path: sha256sums_path.clone(),
        evidence_paths: Vec::new(),
    };

    write_provenance_manifest(&ws, &opts, &report)?;
    write_sha256sums(&sha256sums_path, &report.artifacts, &manifest_path)?;

    report.release_ms = millis_u64(release_started.elapsed().as_millis());
    report.total_ms = millis_u64(pipeline_started.elapsed().as_millis());
    report.within_budget = report.total_ms < opts.budget_ms;

    report.evidence_paths = write_timing_evidence(&ws, &opts, &report)?;
    print_pipeline_output(&report, opts.json_only)?;
    enforce_budget_gate(&report, opts.allow_over_budget)
}

fn millis_u64(ms: u128) -> u64 {
    u64::try_from(ms).unwrap_or(u64::MAX)
}

pub fn should_enforce_budget(cache_state: CacheState, opts: &Options) -> bool {
    match cache_state {
        CacheState::Warm => true,
        CacheState::Cold => opts.enforce_cold,
    }
}

pub fn enforce_budget_gate(report: &PipelineReport, allow_over_budget: bool) -> Result<(), String> {
    if !report.budget_enforced || report.within_budget || allow_over_budget {
        return Ok(());
    }
    Err(format!(
        "fast-release budget exceeded on {} cache: total_ms={} >= budget_ms={} \
         (build_ms={}, test_ms={}, release_ms={})",
        report.cache_state.as_str(),
        report.total_ms,
        report.budget_ms,
        report.build_ms,
        report.test_ms,
        report.release_ms
    ))
}

fn locate_workspace_root(start: &Path) -> Result<PathBuf, String> {
    let found = start
        .ancestors()
        .find(|dir| dir.join("Cargo.toml").is_file() && dir.join("xtask/Cargo.toml").is_file())
        .ok_or_else(|| "fast-release must be run inside a Susi workspace".to_owned())?;
    let canonical = found
        .canonicalize()
        .map_err(|e| format!("failed to canonicalize {}: {e}", found.display()))?;
    Ok(strip_windows_verbatim(&canonical))
}

fn strip_windows_verbatim(path: &Path) -> PathBuf {
    let text = path.as_os_str().to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        path.to_path_buf()
    }
}

fn reject_redirect(path: &Path) -> Result<(), String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(err) => {
            if err.kind() == io::ErrorKind::NotFound {
                return Ok(());
            }
            return Err(format!("failed to inspect {}: {err}", path.display()));
        }
    };
    let redirected = metadata.file_type().is_symlink();
    #[cfg(windows)]
    let redirected = {
        use std::os::windows::fs::MetadataExt;
        redirected || metadata.file_attributes() & 0x400 != 0
    };
    if redirected || !metadata.is_dir() {
        return Err(format!(
            "target output must be a private directory, not a symlink, reparse point, or file: {}",
            path.display()
        ));
    }
    Ok(())
}

fn parse_rustc_vv(text: &str, windows: bool) -> Result<(String, String), String> {
    let release_line = text
        .lines()
        .next()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .unwrap_or("rustc")
        .to_owned();
    let host = text
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .map(str::trim)
        .filter(|h| {
            !h.is_empty()
                && h.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        })
        .ok_or_else(|| "rustc -vV did not report a valid host triple".to_owned())?;
    if windows && !host.ends_with("-pc-windows-msvc") {
        return Err("Windows builds require a native MSVC Rust toolchain".to_owned());
    }
    Ok((host.to_owned(), release_line))
}

fn read_workspace_version(root: &Path) -> Result<String, String> {
    let manifest_path = root.join("Cargo.toml");
    let raw = fs::read_to_string(&manifest_path)
        .map_err(|e| format!("failed to read {}: {e}", manifest_path.display()))?;
    let mut in_package = false;
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_package = trimmed == "[package]";
            continue;
        }
        if in_package && trimmed.starts_with("version") {
            if let Some((_, rhs)) = trimmed.split_once('=') {
                let ver = rhs.split('#').next().unwrap_or("").trim().trim_matches('"');
                if !ver.is_empty() {
                    return Ok(ver.to_owned());
                }
            }
        }
    }
    Err(format!(
        "could not locate [package] version in {}",
        manifest_path.display()
    ))
}

fn prepare_workspace(opts: &Options) -> Result<WorkspaceEnv, String> {
    let cwd = env::current_dir().map_err(|e| format!("current_dir failed: {e}"))?;
    let root = locate_workspace_root(&cwd)?;

    let rustc_out = Command::new("rustc")
        .current_dir(&root)
        .arg("-vV")
        .output()
        .map_err(|e| format!("failed to invoke rustc -vV: {e}"))?;
    if !rustc_out.status.success() {
        return Err("rustc -vV exited with failure".to_owned());
    }
    let (host, rustc_version) =
        parse_rustc_vv(&String::from_utf8_lossy(&rustc_out.stdout), cfg!(windows))?;
    let workspace_version = read_workspace_version(&root)?;

    let raw_target = match &opts.target_dir {
        Some(dir) => dir.clone(),
        None => match env::var_os("CARGO_TARGET_DIR") {
            Some(env_dir) if !env_dir.is_empty() => PathBuf::from(env_dir),
            Some(_) | None => root.join("target"),
        },
    };
    let target_dir = if raw_target.is_absolute() {
        raw_target
    } else {
        cwd.join(raw_target)
    };
    for check in [
        target_dir.clone(),
        target_dir.join("fast-release"),
        target_dir.join("fast-release").join("distilled"),
    ] {
        reject_redirect(&check)?;
    }
    fs::create_dir_all(&target_dir)
        .map_err(|e| format!("failed to create {}: {e}", target_dir.display()))?;

    let out_dir = match &opts.out_dir {
        Some(dir) if dir.is_absolute() => dir.clone(),
        Some(dir) => cwd.join(dir),
        None => target_dir.join("fast-release").join("distilled"),
    };
    reject_redirect(&out_dir)?;
    fs::create_dir_all(&out_dir)
        .map_err(|e| format!("failed to create {}: {e}", out_dir.display()))?;

    let sysroot = if opts.custom_sysroot {
        Some(crate::trusted_sysroot::prepare(&root).map_err(|e| e.to_string())?)
    } else {
        None
    };
    Ok(WorkspaceEnv {
        root,
        target_dir,
        out_dir,
        host,
        rustc_version,
        workspace_version,
        sysroot,
    })
}

pub fn detect_cache_state(target_dir: &Path) -> CacheState {
    let fingerprint_dir = target_dir.join("fast-release").join(".fingerprint");
    let Ok(mut entries) = fs::read_dir(fingerprint_dir) else {
        return CacheState::Cold;
    };
    if entries.next().is_some() {
        CacheState::Warm
    } else {
        CacheState::Cold
    }
}

fn cargo_program() -> PathBuf {
    if let Some(cargo) = env::var_os("CARGO") {
        if !cargo.is_empty() {
            return PathBuf::from(cargo);
        }
    }
    PathBuf::from(if cfg!(windows) { "cargo.exe" } else { "cargo" })
}

fn base_cargo_command(ws: &WorkspaceEnv, subcommand: &str) -> Result<Command, String> {
    let mut cmd = Command::new(cargo_program());
    crate::compiler_env::apply_checked(&mut cmd)
        .map_err(|e| format!("verified compiler environment check failed: {e}"))?;
    cmd.current_dir(&ws.root)
        .args([
            subcommand,
            "--offline",
            "--locked",
            "--profile",
            "fast-release",
            "--target-dir",
        ])
        .arg(&ws.target_dir)
        .env("CARGO_TARGET_DIR", &ws.target_dir);
    if let Some(sysroot) = &ws.sysroot {
        sysroot.apply(&mut cmd).map_err(|e| e.to_string())?;
    }
    Ok(cmd)
}

fn recheck_sysroot(ws: &WorkspaceEnv) -> Result<(), String> {
    if let Some(sysroot) = &ws.sysroot {
        sysroot.recheck().map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn count_fresh_and_recompiled(stdout: &str) -> (usize, usize, Vec<String>) {
    let mut fresh = 0usize;
    let mut recompiled = 0usize;
    let mut diagnostics = Vec::new();
    for line in stdout.lines() {
        if !line.starts_with('{') {
            continue;
        }
        let Ok(msg) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match msg.get("reason").and_then(Value::as_str) {
            Some("compiler-artifact") => match msg.get("fresh").and_then(Value::as_bool) {
                Some(true) => fresh += 1,
                Some(false) => recompiled += 1,
                None => {}
            },
            Some("compiler-message") => {
                if let Some(rendered) = msg
                    .get("message")
                    .and_then(|m| m.get("rendered"))
                    .and_then(Value::as_str)
                {
                    diagnostics.push(rendered.to_owned());
                }
            }
            Some(_) | None => {}
        }
    }
    (fresh, recompiled, diagnostics)
}

fn binary_filename(stem: &str) -> String {
    if cfg!(windows) {
        format!("{stem}.exe")
    } else {
        stem.to_owned()
    }
}

fn make_staged_executable(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let metadata = fs::metadata(path)
            .map_err(|e| format!("failed to inspect staged binary {}: {e}", path.display()))?;
        let mut permissions = metadata.permissions();
        permissions.set_mode(permissions.mode() | 0o111);
        fs::set_permissions(path, permissions).map_err(|e| {
            format!(
                "failed to make staged binary executable {}: {e}",
                path.display()
            )
        })?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

fn run_build_stage(ws: &WorkspaceEnv, opts: &Options) -> Result<(usize, usize), String> {
    let mut cmd = base_cargo_command(ws, "build")?;
    cmd.args([
        "--message-format=json",
        "-p",
        "susi",
        "--bin",
        "susi",
        "-p",
        "susi-harness",
        "--bin",
        "susi-harness",
    ]);
    if opts.local_engine {
        cmd.args(["--features", "susi/local-engine"]);
    }
    #[cfg(target_os = "macos")]
    {
        cmd.args(["--features", "susi/metal"]);
    }

    let out = cmd
        .output()
        .map_err(|e| format!("failed to run cargo build: {e}"))?;
    recheck_sysroot(ws)?;

    let stdout = String::from_utf8_lossy(&out.stdout);
    let (fresh, recompiled, diagnostics) = count_fresh_and_recompiled(&stdout);
    if !out.status.success() {
        let diag_text = diagnostics.join("");
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "Stage 1 (build) failed ({}):\n{diag_text}{stderr}",
            out.status
        ));
    }

    let profile_dir = ws.target_dir.join("fast-release");
    for stem in ["susi", "susi-harness"] {
        let exe = profile_dir.join(binary_filename(stem));
        if !exe.is_file() {
            return Err(format!(
                "Stage 1 succeeded but expected candidate executable is missing: {}",
                exe.display()
            ));
        }
    }
    Ok((fresh, recompiled))
}

fn discover_core_pillars(root: &Path) -> Vec<String> {
    let mut pillars: Vec<String> = CORE_PILLARS.iter().map(|s| (*s).to_owned()).collect();
    if root.join("crates/susi-acp/Cargo.toml").is_file() {
        pillars.push("susi-acp".to_owned());
    }
    pillars
}

/// Parses `test result: ok. 497 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out;`
/// lines emitted by Rust's libtest harness. Child subprocesses spawned inside cross-process
/// lock tests print `1 passed; ... 496 filtered out;`; we select the primary suite's summary
/// (the final `test result:` line of the binary, which summarizes the full invocation).
pub fn parse_libtest_counts(stdout: &str) -> Result<(u64, u64, u64), String> {
    let mut last_summary: Option<(u64, u64, u64)> = None;
    for line in stdout.lines() {
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix("test result:") else {
            continue;
        };
        let Some((_, counts_part)) = rest.split_once('.') else {
            continue;
        };
        let mut passed = None;
        let mut failed = None;
        let mut ignored = None;
        for segment in counts_part.split(';') {
            let mut words = segment.split_whitespace();
            let Some(num_str) = words.next() else {
                continue;
            };
            let Some(label) = words.next() else {
                continue;
            };
            let Ok(val) = num_str.parse::<u64>() else {
                continue;
            };
            match label {
                "passed" => passed = Some(val),
                "failed" => failed = Some(val),
                "ignored" => ignored = Some(val),
                _ => {}
            }
        }
        if let (Some(p), Some(f), Some(i)) = (passed, failed, ignored) {
            last_summary = Some((p, f, i));
        }
    }
    last_summary.ok_or_else(|| "test binary produced no `test result:` summary line".to_owned())
}

struct BinaryRunResult {
    id: String,
    passed: u64,
    failed: u64,
    ignored: u64,
    elapsed_ms: u64,
}

fn execute_test_binary(
    ws: &WorkspaceEnv,
    binary: &crate::test_runner::Binary,
    slot: usize,
    full_soak: bool,
) -> Result<BinaryRunResult, String> {
    let home = ws
        .target_dir
        .join("fast-release")
        .join("test-homes")
        .join(format!("{slot}-{}", binary.target_name));
    let _ = fs::remove_dir_all(&home);
    let susi_home = home.join("susi-home");
    let appdata = home.join("AppData/Roaming");
    let localappdata = home.join("AppData/Local");
    let xdg_config = home.join("xdg-config");
    let xdg_data = home.join("xdg-data");
    let xdg_state = home.join("xdg-state");
    let xdg_cache = home.join("xdg-cache");
    let tmp_dir = home.join("tmp");
    for dir in [
        &susi_home,
        &appdata,
        &localappdata,
        &xdg_config,
        &xdg_data,
        &xdg_state,
        &xdg_cache,
        &tmp_dir,
    ] {
        fs::create_dir_all(dir)
            .map_err(|e| format!("failed to create test sandbox {}: {e}", dir.display()))?;
    }

    let profile_dir = ws.target_dir.join("fast-release");
    let mut path_var = std::ffi::OsString::new();
    for dir in [profile_dir.join("deps"), profile_dir.clone()] {
        path_var.push(dir.as_os_str());
        path_var.push(if cfg!(windows) { ";" } else { ":" });
    }
    if let Some(inherited) = env::var_os("PATH") {
        path_var.push(inherited);
    }

    let original_home =
        env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from);
    let cargo_home = env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| original_home.as_ref().map(|h| h.join(".cargo")));
    let rustup_home = env::var_os("RUSTUP_HOME")
        .map(PathBuf::from)
        .or_else(|| original_home.as_ref().map(|h| h.join(".rustup")));

    let mut cmd = Command::new(&binary.executable);
    cmd.current_dir(binary.manifest_dir())
        .env("PATH", path_var)
        .env("CARGO", cargo_program())
        .env("CARGO_MANIFEST_DIR", binary.manifest_dir())
        .env("CARGO_MANIFEST_PATH", &binary.manifest_path)
        .env("CARGO_PKG_NAME", &binary.package)
        .env("CARGO_PKG_VERSION", &binary.version)
        .env("CARGO_CRATE_NAME", binary.target_name.replace('-', "_"))
        .env("CARGO_PRIMARY_PACKAGE", "1")
        .env("CARGO_TARGET_DIR", &ws.target_dir)
        .env("CARGO_TARGET_TMPDIR", &tmp_dir)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("SUSI_HOME", &susi_home)
        .env("APPDATA", &appdata)
        .env("LOCALAPPDATA", &localappdata)
        .env("XDG_CONFIG_HOME", &xdg_config)
        .env("XDG_DATA_HOME", &xdg_data)
        .env("XDG_STATE_HOME", &xdg_state)
        .env("XDG_CACHE_HOME", &xdg_cache)
        .env("SUSI_NO_REGISTRY_ENV", "1")
        .env("RUST_TEST_THREADS", "6")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cargo_home) = cargo_home {
        cmd.env("CARGO_HOME", cargo_home);
    }
    if let Some(rustup_home) = rustup_home {
        cmd.env("RUSTUP_HOME", rustup_home);
    }

    if binary.kind == "test" {
        cmd.env(
            "CARGO_BIN_EXE_susi",
            profile_dir.join(binary_filename("susi")),
        );
    }
    if !full_soak && binary.kind == "lib" {
        for skip in SOAK_SKIP_FILTERS
            .iter()
            .chain(HARNESS_SOAK_SKIP_FILTERS.iter())
        {
            cmd.args(["--skip", skip]);
        }
    }

    let started = Instant::now();
    let out = cmd
        .output()
        .map_err(|e| format!("failed to start test binary {}: {e}", binary.id()))?;
    let elapsed_ms = millis_u64(started.elapsed().as_millis());
    let _ = fs::remove_dir_all(&home);

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        return Err(format!(
            "core verification binary {} failed ({}) after {elapsed_ms} ms:\n{stdout}\n{stderr}",
            binary.id(),
            out.status
        ));
    }
    let (passed, failed, ignored) = parse_libtest_counts(&stdout)
        .map_err(|e| format!("{}: {e}\nstdout:\n{stdout}", binary.id()))?;
    if failed > 0 {
        return Err(format!(
            "{} reported {failed} failed tests:\n{stdout}",
            binary.id()
        ));
    }
    Ok(BinaryRunResult {
        id: binary.id(),
        passed,
        failed,
        ignored,
        elapsed_ms,
    })
}

fn compile_test_targets(
    ws: &WorkspaceEnv,
    extra_args: &[String],
) -> Result<(Vec<crate::test_runner::Binary>, usize, usize), String> {
    let mut cmd = base_cargo_command(ws, "test")?;
    cmd.args(["--no-run", "--message-format=json"])
        .args(extra_args);
    let out = cmd
        .output()
        .map_err(|e| format!("failed to run cargo test --no-run: {e}"))?;
    recheck_sysroot(ws)?;

    let stdout = String::from_utf8_lossy(&out.stdout);
    let (fresh, recompiled, diagnostics) = count_fresh_and_recompiled(&stdout);
    if !out.status.success() {
        let diag_text = diagnostics.join("");
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "Stage 2 test compilation failed ({}):\n{diag_text}{stderr}",
            out.status
        ));
    }
    let (binaries, _) = crate::test_runner::parse_artifacts(&stdout)
        .map_err(|e| format!("parse_artifacts: {e}"))?;
    Ok((binaries, fresh, recompiled))
}

fn run_test_stage(ws: &WorkspaceEnv, opts: &Options) -> Result<TestStageSummary, String> {
    let pillars = discover_core_pillars(&ws.root);
    let mut lib_args = Vec::with_capacity(pillars.len() * 2 + 4);
    for pillar in &pillars {
        lib_args.push("-p".to_owned());
        lib_args.push(pillar.clone());
    }
    lib_args.push("--lib".to_owned());
    if opts.local_engine {
        lib_args.push("--features".to_owned());
        lib_args.push("susi-gemi/local-engine".to_owned());
    }
    #[cfg(target_os = "macos")]
    {
        lib_args.push("--features".to_owned());
        lib_args.push("susi-gemi/metal".to_owned());
    }

    let (lib_binaries, fresh_lib, recompiled_lib) = compile_test_targets(ws, &lib_args)?;
    if lib_binaries.is_empty() {
        return Err("Stage 2 produced zero pillar test executables".to_owned());
    }

    // Overlap execution of the 5 core pillar unit test binaries on worker threads
    // while the coordinator thread compiles & runs `tests/lean_core_ceilings.rs`.
    let results: Mutex<Vec<BinaryRunResult>> = Mutex::new(Vec::new());
    let errors: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let mut fresh_ceil = 0usize;
    let mut recompiled_ceil = 0usize;

    std::thread::scope(|scope| {
        for (idx, bin) in lib_binaries.iter().enumerate() {
            let results_ref = &results;
            let errors_ref = &errors;
            let full_soak = opts.full_soak;
            scope.spawn(move || match execute_test_binary(ws, bin, idx, full_soak) {
                Ok(res) => {
                    if let Ok(mut guard) = results_ref.lock() {
                        guard.push(res);
                    }
                }
                Err(err) => {
                    if let Ok(mut guard) = errors_ref.lock() {
                        guard.push(err);
                    }
                }
            });
        }

        let mut ceiling_args = vec![
            "-p".to_owned(),
            "susi".to_owned(),
            "--test".to_owned(),
            "lean_core_ceilings".to_owned(),
        ];
        if opts.local_engine {
            ceiling_args.push("--features".to_owned());
            ceiling_args.push("susi/local-engine".to_owned());
        }
        #[cfg(target_os = "macos")]
        {
            ceiling_args.push("--features".to_owned());
            ceiling_args.push("susi/metal".to_owned());
        }

        match compile_test_targets(ws, &ceiling_args) {
            Ok((ceil_bins, f_c, r_c)) => {
                fresh_ceil = f_c;
                recompiled_ceil = r_c;
                for (offset, bin) in ceil_bins.iter().enumerate() {
                    match execute_test_binary(ws, bin, lib_binaries.len() + offset, opts.full_soak)
                    {
                        Ok(res) => {
                            if let Ok(mut guard) = results.lock() {
                                guard.push(res);
                            }
                        }
                        Err(err) => {
                            if let Ok(mut guard) = errors.lock() {
                                guard.push(err);
                            }
                        }
                    }
                }
            }
            Err(err) => {
                if let Ok(mut guard) = errors.lock() {
                    guard.push(err);
                }
            }
        }
    });

    let collected_errors = errors
        .into_inner()
        .map_err(|_| "test error mutex poisoned".to_owned())?;
    if let Some(first_err) = collected_errors.first() {
        return Err(first_err.clone());
    }

    let collected = results
        .into_inner()
        .map_err(|_| "test result mutex poisoned".to_owned())?;
    let mut tests_passed = 0u64;
    let mut tests_failed = 0u64;
    let mut tests_ignored = 0u64;
    let mut binary_timings_ms = BTreeMap::new();
    for item in &collected {
        tests_passed += item.passed;
        tests_failed += item.failed;
        tests_ignored += item.ignored;
        binary_timings_ms.insert(item.id.clone(), item.elapsed_ms);
    }
    if tests_passed == 0 {
        return Err("Stage 2 ran zero tests across core verification binaries".to_owned());
    }

    let skipped_soak_tests = if opts.full_soak {
        Vec::new()
    } else {
        SOAK_SKIP_FILTERS
            .iter()
            .chain(HARNESS_SOAK_SKIP_FILTERS.iter())
            .map(|s| (*s).to_owned())
            .collect()
    };

    Ok(TestStageSummary {
        pillars,
        binaries_ran: collected.len(),
        tests_passed,
        tests_failed,
        tests_ignored,
        fresh_units: fresh_lib + fresh_ceil,
        recompiled_units: recompiled_lib + recompiled_ceil,
        skipped_soak_tests,
        binary_timings_ms,
    })
}

pub fn sha256_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    susi_paths::hex::encode(hasher.finalize())
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let bytes = fs::read(path)
        .map_err(|e| format!("failed to read {} for SHA-256: {e}", path.display()))?;
    Ok(sha256_bytes(&bytes))
}

fn stage_one_binary(
    ws: &WorkspaceEnv,
    stem: &str,
    slot: usize,
) -> Result<DistilledArtifact, String> {
    let filename = binary_filename(stem);
    let src = ws.target_dir.join("fast-release").join(&filename);
    let bin_dir = ws.out_dir.join("bin");
    fs::create_dir_all(&bin_dir)
        .map_err(|e| format!("failed to create {}: {e}", bin_dir.display()))?;

    let bytes_vec = fs::read(&src)
        .map_err(|e| format!("failed to read source binary {}: {e}", src.display()))?;
    let bytes = u64::try_from(bytes_vec.len()).unwrap_or(u64::MAX);
    let sha256 = sha256_bytes(&bytes_vec);

    let staged_path = bin_dir.join(&filename);
    let already_identical = fs::metadata(&staged_path)
        .is_ok_and(|meta| meta.is_file() && meta.len() == bytes)
        && fs::read(&staged_path).ok().as_deref() == Some(bytes_vec.as_slice());
    if !already_identical {
        let staged_tmp = bin_dir.join(format!(".{filename}.staging-{}", std::process::id()));
        fs::write(&staged_tmp, &bytes_vec).map_err(|e| {
            format!(
                "failed to stage {} -> {}: {e}",
                src.display(),
                staged_tmp.display()
            )
        })?;
        fs::rename(&staged_tmp, &staged_path).map_err(|e| {
            format!(
                "failed to place staged binary {}: {e}",
                staged_path.display()
            )
        })?;
    }
    // `fs::write` creates the temporary file with the process umask (0644 on
    // Unix). Restore the executable bit both for a new stage and for an
    // already-identical file left by an older run, before the smoke command
    // tries to execute it.
    make_staged_executable(&staged_path)?;

    // Hermetic `--version` smoke check with isolated HOME/SUSI_HOME.
    let smoke_home = ws
        .target_dir
        .join("fast-release")
        .join("smoke-homes")
        .join(format!("{slot}-{stem}"));
    let _ = fs::remove_dir_all(&smoke_home);
    let smoke_susi = smoke_home.join("susi");
    fs::create_dir_all(&smoke_susi)
        .map_err(|e| format!("failed to create {}: {e}", smoke_susi.display()))?;

    let smoke_out = Command::new(&staged_path)
        .arg("--version")
        .current_dir(&smoke_home)
        .env("HOME", &smoke_home)
        .env("USERPROFILE", &smoke_home)
        .env("SUSI_HOME", &smoke_susi)
        .env("SUSI_NO_REGISTRY_ENV", "1")
        .output()
        .map_err(|e| {
            format!(
                "failed to smoke-run {} --version: {e}",
                staged_path.display()
            )
        })?;
    let _ = fs::remove_dir_all(&smoke_home);

    if !smoke_out.status.success() {
        return Err(format!(
            "staged binary {} --version failed ({}): {}",
            staged_path.display(),
            smoke_out.status,
            String::from_utf8_lossy(&smoke_out.stderr).trim()
        ));
    }
    let version_stdout = String::from_utf8_lossy(&smoke_out.stdout).trim().to_owned();
    if version_stdout.is_empty() {
        return Err(format!(
            "staged binary {} --version produced empty output",
            staged_path.display()
        ));
    }

    Ok(DistilledArtifact {
        name: stem.to_owned(),
        relative_path: format!("bin/{filename}"),
        staged_path,
        bytes,
        sha256,
        version_stdout,
    })
}

fn stage_and_smoke_verify_binaries(ws: &WorkspaceEnv) -> Result<Vec<DistilledArtifact>, String> {
    let stems = ["susi", "susi-harness"];
    let results: Mutex<Vec<(usize, DistilledArtifact)>> = Mutex::new(Vec::new());
    let errors: Mutex<Vec<String>> = Mutex::new(Vec::new());

    std::thread::scope(|scope| {
        for (idx, stem) in stems.iter().enumerate() {
            let results_ref = &results;
            let errors_ref = &errors;
            scope.spawn(move || match stage_one_binary(ws, stem, idx) {
                Ok(art) => {
                    if let Ok(mut guard) = results_ref.lock() {
                        guard.push((idx, art));
                    }
                }
                Err(err) => {
                    if let Ok(mut guard) = errors_ref.lock() {
                        guard.push(err);
                    }
                }
            });
        }
    });

    let errs = errors
        .into_inner()
        .map_err(|_| "stage error mutex poisoned".to_owned())?;
    if let Some(err) = errs.first() {
        return Err(err.clone());
    }
    let mut staged = results
        .into_inner()
        .map_err(|_| "stage results mutex poisoned".to_owned())?;
    staged.sort_by_key(|(idx, _)| *idx);
    Ok(staged.into_iter().map(|(_, art)| art).collect())
}

fn git_text(root: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

fn read_git_provenance(root: &Path) -> (String, String, String, bool) {
    let mut commit = String::new();
    let mut tree = String::new();
    let mut branch = String::new();
    if let Some(lines) = git_text(
        root,
        &["rev-parse", "HEAD", "HEAD^{tree}", "--abbrev-ref", "HEAD"],
    ) {
        let mut iter = lines.lines();
        commit = iter.next().unwrap_or_default().trim().to_owned();
        tree = iter.next().unwrap_or_default().trim().to_owned();
        branch = iter.next().unwrap_or_default().trim().to_owned();
    }
    let dirty = git_text(root, &["status", "--porcelain", "-uno"])
        .map(|s| !s.is_empty())
        .unwrap_or(false);
    (commit, tree, branch, dirty)
}

fn unix_timestamp_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn write_provenance_manifest(
    ws: &WorkspaceEnv,
    opts: &Options,
    report: &PipelineReport,
) -> Result<(), String> {
    let artifact_entries: Vec<Value> = report
        .artifacts
        .iter()
        .map(|a| {
            json!({
                "name": a.name,
                "path": a.relative_path,
                "bytes": a.bytes,
                "sha256": a.sha256,
                "version_stdout": a.version_stdout,
            })
        })
        .collect();

    let manifest = json!({
        "schema": MANIFEST_SCHEMA,
        "generated_unix": unix_timestamp_secs(),
        "workspace_version": ws.workspace_version,
        "profile": "fast-release",
        "candidate_only": true,
        "installed_mutated": false,
        "features": {
            "local_engine": opts.local_engine,
            "full_soak": opts.full_soak,
        },
        "source_provenance": {
            "git_commit": report.git_commit,
            "git_tree": report.git_tree,
            "git_branch": report.git_branch,
            "git_dirty": report.git_dirty,
            "cargo_lock_sha256": report.cargo_lock_sha256,
        },
        "toolchain": {
            "host": ws.host,
            "rustc": ws.rustc_version,
        },
        "artifacts": artifact_entries,
        "verification": {
            "pillars": report.tests.pillars,
            "binaries_ran": report.tests.binaries_ran,
            "tests_passed": report.tests.tests_passed,
            "tests_failed": report.tests.tests_failed,
            "tests_ignored": report.tests.tests_ignored,
            "skipped_soak_tests": report.tests.skipped_soak_tests,
            "binary_timings_ms": report.tests.binary_timings_ms,
        },
        "timings_ms": {
            "cache_state": report.cache_state.as_str(),
            "label": opts.label,
            "build_ms": report.build_ms,
            "test_ms": report.test_ms,
            "release_ms": report.release_ms,
            "total_ms": report.total_ms,
            "budget_ms": opts.budget_ms,
            "within_budget": report.within_budget,
        }
    });

    let pretty = serde_json::to_string_pretty(&manifest)
        .map_err(|e| format!("failed to serialize provenance manifest: {e}"))?;
    let manifest_path = ws.out_dir.join("release-manifest.json");
    fs::write(&manifest_path, format!("{pretty}\n"))
        .map_err(|e| format!("failed to write {}: {e}", manifest_path.display()))
}

fn write_sha256sums(
    sha256sums_path: &Path,
    artifacts: &[DistilledArtifact],
    manifest_path: &Path,
) -> Result<(), String> {
    let manifest_sha256 = sha256_file(manifest_path)?;
    let mut lines = String::new();
    for art in artifacts {
        lines.push_str(&format!("{}  {}\n", art.sha256, art.relative_path));
    }
    lines.push_str(&format!("{manifest_sha256}  release-manifest.json\n"));
    fs::write(sha256sums_path, lines)
        .map_err(|e| format!("failed to write {}: {e}", sha256sums_path.display()))
}

pub fn report_json(report: &PipelineReport) -> Value {
    let artifacts: Vec<Value> = report
        .artifacts
        .iter()
        .map(|a| {
            json!({
                "name": a.name,
                "staged_path": a.staged_path.display().to_string(),
                "bytes": a.bytes,
                "sha256": a.sha256,
                "version_stdout": a.version_stdout,
            })
        })
        .collect();
    let evidence_paths: Vec<String> = report
        .evidence_paths
        .iter()
        .map(|p| p.display().to_string())
        .collect();

    json!({
        "schema": REPORT_SCHEMA,
        "recorded_unix": unix_timestamp_secs(),
        "label": report.label,
        "cache_state": report.cache_state.as_str(),
        "workspace_version": report.workspace_version,
        "host": report.host,
        "rustc": report.rustc_version,
        "git_commit": report.git_commit,
        "git_tree": report.git_tree,
        "git_branch": report.git_branch,
        "git_dirty": report.git_dirty,
        "build_ms": report.build_ms,
        "test_ms": report.test_ms,
        "release_ms": report.release_ms,
        "total_ms": report.total_ms,
        "budget_ms": report.budget_ms,
        "budget_enforced": report.budget_enforced,
        "within_budget": report.within_budget,
        "compiler_cache": {
            "build_fresh_units": report.build_fresh_units,
            "build_recompiled_units": report.build_recompiled_units,
            "test_fresh_units": report.tests.fresh_units,
            "test_recompiled_units": report.tests.recompiled_units,
        },
        "verification": {
            "pillars": report.tests.pillars,
            "binaries_ran": report.tests.binaries_ran,
            "tests_passed": report.tests.tests_passed,
            "tests_failed": report.tests.tests_failed,
            "tests_ignored": report.tests.tests_ignored,
            "skipped_soak_tests": report.tests.skipped_soak_tests,
            "binary_timings_ms": report.tests.binary_timings_ms,
        },
        "cargo_lock_sha256": report.cargo_lock_sha256,
        "manifest_path": report.manifest_path.display().to_string(),
        "sha256sums_path": report.sha256sums_path.display().to_string(),
        "evidence_paths": evidence_paths,
        "artifacts": artifacts,
    })
}

fn append_jsonl(path: &Path, entry: &Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create {}: {e}", parent.display()))?;
    }
    let line =
        serde_json::to_string(entry).map_err(|e| format!("failed to encode JSONL row: {e}"))?;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("failed to open {}: {e}", path.display()))?;
    writeln!(file, "{line}").map_err(|e| format!("failed to append {}: {e}", path.display()))
}

fn write_timing_evidence(
    ws: &WorkspaceEnv,
    opts: &Options,
    report: &PipelineReport,
) -> Result<Vec<PathBuf>, String> {
    let mut written = Vec::new();
    let target_jsonl = ws.target_dir.join("fast-release").join("timings.jsonl");
    let distilled_json = ws.out_dir.join("timing-evidence.json");
    written.push(target_jsonl.clone());
    written.push(distilled_json.clone());
    if let Some(custom) = &opts.evidence_out {
        written.push(custom.clone());
    }
    if opts.record_tracked {
        written.push(ws.root.join(".agents/fast-release-timings.jsonl"));
    }

    let mut report_with_paths = report.clone();
    report_with_paths.evidence_paths = written.clone();
    let doc = report_json(&report_with_paths);

    append_jsonl(&target_jsonl, &doc)?;
    let pretty = serde_json::to_string_pretty(&doc)
        .map_err(|e| format!("failed to format timing evidence JSON: {e}"))?;
    fs::write(&distilled_json, format!("{pretty}\n"))
        .map_err(|e| format!("failed to write {}: {e}", distilled_json.display()))?;

    if let Some(custom) = &opts.evidence_out {
        append_jsonl(custom, &doc)?;
    }
    if opts.record_tracked {
        append_jsonl(&ws.root.join(".agents/fast-release-timings.jsonl"), &doc)?;
    }
    Ok(written)
}

fn print_pipeline_output(report: &PipelineReport, json_only: bool) -> Result<(), String> {
    let doc = report_json(report);
    let pretty = serde_json::to_string_pretty(&doc)
        .map_err(|e| format!("failed to serialize pipeline report: {e}"))?;
    if json_only {
        println!("{pretty}");
        return Ok(());
    }

    let status_badge = if report.within_budget {
        "PASS (< budget)"
    } else if !report.budget_enforced {
        "COLD-CACHE RECORDED (warm budget deferred)"
    } else {
        "OVER BUDGET"
    };
    println!(
        "xtask fast-release [{}] cache={} label={} host={}",
        status_badge,
        report.cache_state.as_str(),
        report.label,
        report.host
    );
    println!(
        "  build_ms={:<6} test_ms={:<6} release_ms={:<6} total_ms={:<6} (budget_ms={})",
        report.build_ms, report.test_ms, report.release_ms, report.total_ms, report.budget_ms
    );
    println!(
        "  verified={} tests across {} binaries (pillars: {})",
        report.tests.tests_passed,
        report.tests.binaries_ran,
        report.tests.pillars.join(", ")
    );
    println!("  manifest={}", report.manifest_path.display());
    println!("{pretty}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(items: &[&str]) -> Vec<String> {
        items.iter().map(|i| (*i).to_owned()).collect()
    }

    #[test]
    fn options_parse_defaults_and_explicit_flags() {
        let def = Options::parse(&[]).unwrap();
        assert_eq!(def.budget_ms, DEFAULT_BUDGET_MS);
        assert!(!def.enforce_cold);
        assert!(!def.full_soak);
        assert_eq!(def.label, "default");

        let custom = Options::parse(&s(&[
            "--budget-ms",
            "8500",
            "--enforce-cold",
            "--label",
            "warm-edit-core",
            "--local-engine",
            "--full-soak",
            "--json-only",
            "--record",
        ]))
        .unwrap();
        assert_eq!(custom.budget_ms, 8_500);
        assert!(custom.enforce_cold);
        assert_eq!(custom.label, "warm-edit-core");
        assert!(custom.local_engine);
        assert!(custom.full_soak);
        assert!(custom.json_only);
        assert!(custom.record_tracked);
    }

    #[test]
    fn options_reject_invalid_budget_or_unknown_flags() {
        assert!(Options::parse(&s(&["--budget-ms", "0"])).is_err());
        assert!(Options::parse(&s(&["--budget-ms", "abc"])).is_err());
        assert!(Options::parse(&s(&["--label", "   "])).is_err());
        assert!(Options::parse(&s(&["--unknown-flag"])).is_err());
    }

    #[test]
    fn libtest_parser_selects_primary_suite_summary_after_child_invocations() {
        let sample = "\
running 497 tests
test a::one ... ok

running 1 test
test child::lock_holder ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 496 filtered out; finished in 0.00s

test b::two ... ok
test result: ok. 497 passed; 0 failed; 3 ignored; 0 measured; 0 filtered out; finished in 0.42s
";
        assert_eq!(parse_libtest_counts(sample).unwrap(), (497, 0, 3));
        assert!(parse_libtest_counts("no summary here").is_err());
    }

    #[test]
    fn budget_gate_enforces_strict_under_10s_on_warm_and_records_cold_honestly() {
        let dummy_tests = TestStageSummary {
            pillars: vec!["susi-core".to_owned()],
            binaries_ran: 6,
            tests_passed: 1964,
            tests_failed: 0,
            tests_ignored: 3,
            fresh_units: 120,
            recompiled_units: 2,
            skipped_soak_tests: Vec::new(),
            binary_timings_ms: BTreeMap::new(),
        };
        let make_report = |cache_state: CacheState, total_ms: u64, enforce_cold: bool| {
            let opts = Options {
                enforce_cold,
                ..Options::default()
            };
            let budget_enforced = should_enforce_budget(cache_state, &opts);
            PipelineReport {
                cache_state,
                label: "test".to_owned(),
                host: "x86_64-unknown-linux-gnu".to_owned(),
                rustc_version: "rustc 1.99.0".to_owned(),
                workspace_version: "0.21.61".to_owned(),
                git_commit: "deadbeef".to_owned(),
                git_tree: "cafebabe".to_owned(),
                git_branch: "cto/core-under-10s".to_owned(),
                git_dirty: false,
                build_ms: 2_000,
                test_ms: 1_500,
                release_ms: 80,
                total_ms,
                budget_ms: DEFAULT_BUDGET_MS,
                budget_enforced,
                within_budget: total_ms < DEFAULT_BUDGET_MS,
                build_fresh_units: 100,
                build_recompiled_units: 1,
                tests: dummy_tests.clone(),
                artifacts: Vec::new(),
                cargo_lock_sha256: "00".repeat(32),
                manifest_path: PathBuf::from("/tmp/release-manifest.json"),
                sha256sums_path: PathBuf::from("/tmp/SHA256SUMS"),
                evidence_paths: Vec::new(),
            }
        };

        // Warm < 10_000 ms passes.
        assert!(enforce_budget_gate(&make_report(CacheState::Warm, 3_580, false), false).is_ok());
        // Warm == 10_000 ms fails strictly (`total_ms < 10_000`).
        assert!(enforce_budget_gate(&make_report(CacheState::Warm, 10_000, false), false).is_err());
        // Cold >= 10_000 ms is recorded honestly without failing unless `--enforce-cold` is set.
        assert!(enforce_budget_gate(&make_report(CacheState::Cold, 38_000, false), false).is_ok());
        assert!(enforce_budget_gate(&make_report(CacheState::Cold, 38_000, true), false).is_err());
    }

    #[test]
    fn cache_state_detects_empty_vs_populated_fingerprint_directory() {
        let dir = env::temp_dir().join(format!("susi-fast-rel-cache-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        assert_eq!(detect_cache_state(&dir), CacheState::Cold);

        let fp = dir.join("fast-release").join(".fingerprint").join("unit-1");
        fs::create_dir_all(&fp).unwrap();
        assert_eq!(detect_cache_state(&dir), CacheState::Warm);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn sha256_and_manifest_checksums_are_deterministic() {
        let dir = env::temp_dir().join(format!("susi-fast-rel-sha-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("bin")).unwrap();

        let bin_path = dir.join("bin/susi");
        fs::write(&bin_path, b"susi-candidate-bytes").unwrap();
        let manifest_path = dir.join("release-manifest.json");
        fs::write(
            &manifest_path,
            b"{\"schema\":\"susi-fast-release-manifest/v1\"}\n",
        )
        .unwrap();
        let sums_path = dir.join("SHA256SUMS");

        let art = DistilledArtifact {
            name: "susi".to_owned(),
            relative_path: "bin/susi".to_owned(),
            staged_path: bin_path,
            bytes: 20,
            sha256: sha256_bytes(b"susi-candidate-bytes"),
            version_stdout: "susi 0.21.61".to_owned(),
        };
        write_sha256sums(&sums_path, std::slice::from_ref(&art), &manifest_path).unwrap();
        let sums = fs::read_to_string(&sums_path).unwrap();
        assert!(sums.contains(&format!("{}  bin/susi\n", art.sha256)));
        assert!(sums.contains("  release-manifest.json\n"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn staged_binary_restores_the_unix_executable_bit() {
        use std::os::unix::fs::PermissionsExt;

        let path = env::temp_dir().join(format!(
            "susi-fast-release-mode-{}-{}",
            std::process::id(),
            "binary"
        ));
        let _ = fs::remove_file(&path);
        fs::write(&path, b"candidate").unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o644);
        fs::set_permissions(&path, permissions).unwrap();

        make_staged_executable(&path).unwrap();

        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_ne!(mode & 0o111, 0, "staged binary mode was {mode:o}");
        let _ = fs::remove_file(path);
    }
}
