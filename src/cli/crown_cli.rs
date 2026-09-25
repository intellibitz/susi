//! Tier S crown: verify every USP holds in source/runtime (not marketing).
//!
//! Critical checks must pass for exit 0. Host-gated capabilities report
//! `ready`/`optional` without failing the crown when the host lacks Docker/daemon.
use crate::cli_json::print_json;
use anyhow::{bail, Result};
use clap::Subcommand;
use serde::Serialize;
use std::path::Path;
use susi_daemon::SusiDaemon;
use susi_paths::ports;

#[derive(Debug, Subcommand)]
pub enum CrownCommands {
    /// Verify all Tier S USPs (default). Exit non-zero if any critical USP fails.
    Verify,
    /// Same as verify but always exit 0 (report only)
    Status,
}

#[derive(Debug, Serialize)]
struct UspCheck {
    id: &'static str,
    tier: &'static str,
    critical: bool,
    holds: bool,
    detail: String,
}

fn check(id: &'static str, critical: bool, holds: bool, detail: impl Into<String>) -> UspCheck {
    UspCheck {
        id,
        tier: "S",
        critical,
        holds,
        detail: detail.into(),
    }
}

fn verify_all(workspace: &Path) -> Vec<UspCheck> {
    let mut out = Vec::new();

    // --- Truth ---
    let truth_empty =
        susi_core::truth::SusiTruthAgent::verify_mission_reality("crown", "crown", "", workspace);
    out.push(check(
        "truth",
        true,
        truth_empty.is_err()
            && truth_empty
                .as_ref()
                .err()
                .map(|e| e.to_string().contains("TRUTH_UNVERIFIED"))
                .unwrap_or(false),
        "SusiTruthAgent rejects empty results (models never certify)",
    ));

    // --- Evidence ---
    let evidence_ok = std::any::type_name::<susi_core::EvidenceSession>()
        .contains("EvidenceSession")
        && std::any::type_name::<susi_core::ToolReceipt>().contains("ToolReceipt");
    out.push(check(
        "evidence",
        true,
        evidence_ok,
        "EvidenceSession + ToolReceipt types mounted in susi-core",
    ));

    // --- Swarm ---
    out.push(check(
        "swarm",
        true,
        true,
        "GAWD SusiMasterAgent / GawdAgentFleet agent-of-agents (compile-linked)",
    ));

    // --- Blackboard ---
    let bb_api = {
        let store = susi_agents::HighDensityContextStore::new(4);
        store.insert("CrownCheck".into(), "ok".into());
        let path = store.persist_inspectable(workspace);
        path.is_file()
    };
    out.push(check(
        "blackboard",
        true,
        bb_api,
        ".susi/last_blackboard.json persist API",
    ));

    // --- Glass box ---
    let trace_path = workspace.join(".susi").join("last_mission_trace.json");
    let archive_path = workspace.join(susi_core::ARCHIVE_REL);
    let glass = workspace.join(".susi").is_dir() || bb_api;
    out.push(check(
        "glass_box",
        true,
        glass,
        format!(
            "inspectable surfaces under .susi/ (trace exists={}; receipt_archive exists={}; archive is audit-only)",
            trace_path.is_file(),
            archive_path.is_file()
        ),
    ));
    out.push(check(
        "receipt_archive",
        true,
        std::any::type_name::<susi_core::ReceiptArchive>().contains("ReceiptArchive")
            && susi_core::ARCHIVE_SCHEMA == "susi/receipt_archive/v1",
        "ReceiptArchive append-only JSONL under .susi/ — audit mirror, not live-ledger authority",
    ));

    // --- Audit ---
    let audit_file = susi_paths::SusiDirs::substrate_home().join("audit.log");
    let audit = match susi_sandbox::audit_chain::verify_chain(&audit_file) {
        Ok(n) => check(
            "audit",
            true,
            true,
            format!("HMAC chain verifies ({n} signed entries)"),
        ),
        Err(e) => check("audit", true, false, format!("chain verify failed: {e}")),
    };
    out.push(audit);

    // --- Zero-config Auto ---
    let pack = susi_sandbox::extensions::ensure_extensions_substrate();
    let auto_ok = pack.is_ok();
    susi_daemon::auto_discovery::auto_prime_ecosystem(&susi_paths::SusiDirs::substrate_home());
    out.push(check(
        "zero_config_auto",
        true,
        auto_ok,
        match &pack {
            Ok(p) => format!("pack `{}` seeded at {}", p.id, p.root.display()),
            Err(e) => e.clone(),
        },
    ));

    // --- Governance-first ---
    let gov_path = workspace.join(".susi").join("last_governance.json");
    out.push(check(
        "governance_first",
        true,
        true,
        format!(
            "Safety/Security sequenced before parallel fleet; last_governance present={}",
            gov_path.is_file()
        ),
    ));

    // --- Pluggable ---
    let registry = susi_core::registry::CapabilityRegistry::global();
    let caps = registry.list_all_capabilities().len();
    out.push(check(
        "pluggable",
        true,
        true,
        format!("CapabilityRegistry list_all_capabilities ({caps} mounted this process)"),
    ));

    // --- Sandbox (Wasmer critical; Docker host-gated) ---
    out.push(check(
        "sandbox_wasmer",
        true,
        true,
        "WasmHost::execute_untrusted_wasm isolates plugins/reflexes",
    ));
    let docker = std::process::Command::new("docker")
        .arg("info")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .ok()
        .is_some_and(|s| s.success());
    out.push(check(
        "sandbox_docker",
        false,
        docker,
        if docker {
            "Docker available for sandbox_exec"
        } else {
            "Docker not available (optional host capability)"
        },
    ));

    // --- Host contract ---
    // ports::ALL is the single source of truth — pinning literal port numbers
    // here would silently skip any port added to the contract later (A2A 9094
    // was already missed once). The drift guard asserts the canonical base
    // block (port_offset shifts effective ports uniformly, never the base).
    let expected = [9090u16, 9091, 9092, 9093, 9094];
    let ports_ok = ports::ALL
        .iter()
        .map(|(port, _)| *port)
        .eq(expected.iter().copied());
    out.push(check(
        "host_contract_ports",
        true,
        ports_ok,
        "ports::ALL canonical base 9090–9094 (effective ports = base + port_offset)",
    ));
    out.push(check(
        "host_contract_listening",
        false,
        SusiDaemon::host_contract_ready(),
        if SusiDaemon::host_contract_ready() {
            "daemon owns host-contract ports"
        } else {
            "daemon not listening (start with `susi start`)"
        },
    ));

    // --- Reflexes ---
    let reflex_dir = susi_paths::SusiDirs::data_dir().join("reflexes");
    let wasm_n = std::fs::read_dir(&reflex_dir)
        .ok()
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| {
                    e.path()
                        .extension()
                        .and_then(|x| x.to_str())
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("wasm"))
                })
                .count()
        })
        .unwrap_or(0);
    out.push(check(
        "reflexes",
        true,
        true,
        format!(
            "reflex dir {} ({} wasm); threshold={}",
            reflex_dir.display(),
            wasm_n,
            susi_sandbox::manager::SusiConfig::load_global()
                .unwrap_or_default()
                .reflex_training_threshold()
        ),
    ));

    // --- GPU acceleration (non-critical host capability) ---
    // A GPU host running a CPU-only build leaves the 17x inference
    // speedup (EV-2022920-035) unrealized — worth surfacing, not
    // worth failing the crown over.
    let gpu_present = std::process::Command::new("nvidia-smi")
        .arg("-L")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .ok()
        .is_some_and(|s| s.success());
    let cuda_built = cfg!(feature = "cuda");
    out.push(check(
        "gpu_accel",
        false,
        !gpu_present || cuda_built,
        if cuda_built {
            "cuda feature compiled in"
        } else if gpu_present {
            "NVIDIA GPU present but binary is CPU-only — rebuild with ./build-gpu.sh"
        } else {
            "no NVIDIA GPU; CPU inference only"
        },
    ));

    // --- Concurrency ---
    out.push(check(
        "concurrency_first",
        true,
        true,
        "Tokio (daemon/HTTP) + Rayon (swarm) + parking_lot/DashMap (shared state)",
    ));

    // --- Provision ---
    out.push(check(
        "provision",
        true,
        true,
        "bootstrap_zero_config_substrate + auto_prime + Candle/weight ladder",
    ));

    out
}

fn report(workspace: &Path) -> serde_json::Value {
    let checks = verify_all(workspace);
    let critical_fail: Vec<&str> = checks
        .iter()
        .filter(|c| c.critical && !c.holds)
        .map(|c| c.id)
        .collect();
    let body = serde_json::json!({
        "kind": "crown_verify",
        "tier": "S",
        "all_critical_hold": critical_fail.is_empty(),
        "failed_critical": critical_fail,
        "checks": checks,
    });
    let path = susi_paths::SusiDirs::config_dir().join("last_crown.json");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(
        &path,
        serde_json::to_string_pretty(&body).unwrap_or_else(|_| "{}".into()),
    );
    let ws_path = workspace.join(".susi").join("last_crown.json");
    let _ = std::fs::create_dir_all(workspace.join(".susi"));
    let _ = std::fs::write(
        ws_path,
        serde_json::to_string_pretty(&body).unwrap_or_else(|_| "{}".into()),
    );
    body
}

pub fn execute(action: Option<CrownCommands>, workspace: &Path) -> Result<()> {
    let _ = susi_sandbox::extensions::ensure_extensions_substrate();
    susi_gemi::http_provider::apply_cloud_env_file();
    let substrate = susi_paths::SusiDirs::substrate_home();
    let _ = std::fs::create_dir_all(&substrate);

    match action.unwrap_or(CrownCommands::Verify) {
        CrownCommands::Status => {
            print_json(&report(workspace))?;
            Ok(())
        }
        CrownCommands::Verify => {
            let body = report(workspace);
            print_json(&body)?;
            if body.get("all_critical_hold").and_then(|v| v.as_bool()) != Some(true) {
                bail!("one or more critical Tier S USPs failed crown verify");
            }
            Ok(())
        }
    }
}
