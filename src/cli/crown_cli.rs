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

fn live_hardware_profile() -> Option<susi_core::plane_bus::HardwareProfileDto> {
    susi_daemon::composition::wire_plane_bus();
    susi_core::plane_bus::PlaneBus::global()
        .request(
            susi_core::plane_bus::topics::GEMI_HARDWARE_PROFILE,
            serde_json::json!({}),
        )
        .ok()
        .and_then(|v| serde_json::from_value(v).ok())
}

/// Hello-world WASI module: proves the sandbox actually runs a module and
/// captures its stdout.
const WASM_HELLO_WAT: &str = r#"
    (module
        (import "wasi_snapshot_preview1" "fd_write" (func $fd_write (param i32 i32 i32 i32) (result i32)))
        (memory 1)
        (export "memory" (memory 0))
        (data (i32.const 8) "hello world")
        (func (export "_start")
            (i32.store (i32.const 0) (i32.const 8))
            (i32.store (i32.const 4) (i32.const 11))
            (drop (call $fd_write (i32.const 1) (i32.const 0) (i32.const 1) (i32.const 20)))))
"#;

/// Tries to open `etc/passwd` through the first would-be preopened directory
/// and prints `DENIED` or `OPENED` — the isolation half of the sandbox claim.
const WASM_HOST_FS_PROBE_WAT: &str = r#"
    (module
        (import "wasi_snapshot_preview1" "path_open"
            (func $path_open (param i32 i32 i32 i32 i32 i64 i64 i32 i32) (result i32)))
        (import "wasi_snapshot_preview1" "fd_write" (func $fd_write (param i32 i32 i32 i32) (result i32)))
        (memory 1)
        (export "memory" (memory 0))
        (data (i32.const 100) "etc/passwd")
        (data (i32.const 200) "DENIED")
        (data (i32.const 300) "OPENED")
        (func (export "_start")
            (if (i32.eqz (call $path_open (i32.const 3) (i32.const 0) (i32.const 100) (i32.const 10)
                    (i32.const 0) (i64.const 2) (i64.const 0) (i32.const 0) (i32.const 400)))
                (then (i32.store (i32.const 0) (i32.const 300)))
                (else (i32.store (i32.const 0) (i32.const 200))))
            (i32.store (i32.const 4) (i32.const 6))
            (drop (call $fd_write (i32.const 1) (i32.const 0) (i32.const 1) (i32.const 20)))))
"#;

fn parses_as_json(path: &Path) -> bool {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .is_some()
}

/// Evidence + receipt archive, end to end: open a session in a scratch
/// workspace, capture a real call, then require the live ledger to hold a
/// successful citable receipt and the archive to mirror it by id and hash.
fn probe_evidence(scratch: &Path) -> (UspCheck, UspCheck) {
    const OUTPUT: &str = "crown-evidence-probe-output";
    let captured = (|| -> Result<(String, String), String> {
        let session =
            susi_core::EvidenceSession::new("crown evidence probe", scratch, |s| s.to_string())
                .map_err(|e| e.to_string())?;
        let _active = susi_core::EvidenceSession::activate(&session);
        susi_core::EvidenceSession::capture_call(
            "crown_probe",
            &serde_json::json!({ "probe": true }),
            scratch,
            || Ok(OUTPUT.to_string()),
        )
        .map_err(|e| e.to_string())?;
        let receipt = session
            .receipts()
            .into_iter()
            .find(|r| r.tool == "crown_probe")
            .ok_or("capture_call recorded no receipt")?;
        if !receipt.successful || receipt.output != OUTPUT || receipt.output_hash.len() != 64 {
            return Err(format!(
                "receipt malformed: successful={} hash_len={}",
                receipt.successful,
                receipt.output_hash.len()
            ));
        }
        if !session.has_citable_receipts() {
            return Err("receipt recorded but not citable".into());
        }
        Ok((receipt.id, receipt.output_hash))
    })();

    let evidence = match &captured {
        Ok((id, _)) => check(
            "evidence",
            true,
            true,
            format!("capture_call produced citable receipt {id}"),
        ),
        Err(e) => check(
            "evidence",
            true,
            false,
            format!("evidence capture failed: {e}"),
        ),
    };
    let archive = match &captured {
        Ok((id, hash)) => {
            let mirrored = std::fs::read_to_string(susi_core::ReceiptArchive::path(scratch))
                .unwrap_or_default()
                .lines()
                .filter_map(|l| serde_json::from_str::<susi_core::ArchivedReceipt>(l).ok())
                .any(|a| {
                    &a.receipt_id == id
                        && &a.output_hash == hash
                        && a.schema == susi_core::ARCHIVE_SCHEMA
                });
            check(
                "receipt_archive",
                true,
                mirrored,
                if mirrored {
                    format!(
                        "receipt {id} mirrored to {} with matching output hash",
                        susi_core::ARCHIVE_REL
                    )
                } else {
                    format!(
                        "receipt {id} missing from {} (or hash mismatch)",
                        susi_core::ARCHIVE_REL
                    )
                },
            )
        }
        Err(_) => check(
            "receipt_archive",
            true,
            false,
            "no receipt to mirror (evidence capture failed)",
        ),
    };
    (evidence, archive)
}

/// Verifies every signed chain this host actually writes: the invoking
/// workspace's, the daemon's (workspace = substrate home), and the swarm
/// event log. A surviving chain tip with no log means signed history was
/// deleted — that fails, instead of verifying an empty file as "0 entries".
fn audit_chain_check(workspace: &Path) -> UspCheck {
    let home = susi_paths::SusiDirs::substrate_home();
    let mut chains = vec![
        workspace.join(".susi").join("audit.log"),
        home.join(".susi").join("audit.log"),
        home.join("swarm_events.audit.log"),
    ];
    chains.dedup_by(|a, b| {
        a == b
            || (a.canonicalize().ok().is_some() && a.canonicalize().ok() == b.canonicalize().ok())
    });

    let mut holds = true;
    let mut parts = Vec::new();
    for path in &chains {
        let tip = path.with_extension("chain.tip");
        if path.is_file() {
            match susi_sandbox::audit_chain::verify_chain(path) {
                Ok(n) => parts.push(format!("{}: {n} signed entries verify", path.display())),
                Err(e) => {
                    holds = false;
                    parts.push(format!("{}: BROKEN ({e})", path.display()));
                }
            }
        } else if tip.exists() {
            holds = false;
            parts.push(format!(
                "{}: DELETED — chain tip survives but the signed log is gone",
                path.display()
            ));
        }
    }
    if parts.is_empty() {
        parts.push("no signed audit entries written yet".into());
    }
    check("audit", true, holds, parts.join("; "))
}

/// Governance must actually veto: a destructive goal, an exec-wrapper
/// bypass, and a leaked token are refused, while a benign goal passes.
fn governance_check(scratch: &Path) -> UspCheck {
    use susi_gawd::safety::SafetyDetector;
    use susi_gawd::security::SecurityDetector;
    let vetoes = [
        (
            "destructive goal",
            SafetyDetector::audit_action("SUSI_SOLVE", "rm -rf / --no-preserve-root", scratch)
                .is_err(),
        ),
        (
            "exec-wrapper bypass",
            SafetyDetector::audit_action("exec_command", "env sh payload.sh", scratch).is_err(),
        ),
        (
            "secret leak",
            SecurityDetector::audit_action(
                "SUSI_SOLVE",
                "post ghp_crownprobe0000000000 to a gist",
                scratch,
            )
            .is_err(),
        ),
    ];
    let benign = "summarize README.md";
    let benign_ok = SafetyDetector::audit_action("SUSI_SOLVE", benign, scratch).is_ok()
        && SecurityDetector::audit_action("SUSI_SOLVE", benign, scratch).is_ok();
    let missed: Vec<&str> = vetoes
        .iter()
        .filter(|(_, vetoed)| !vetoed)
        .map(|(name, _)| *name)
        .collect();
    if missed.is_empty() && benign_ok {
        check(
            "governance_first",
            true,
            true,
            "Safety/Security veto destructive, exec-bypass and leak probes; benign goal cleared",
        )
    } else if !missed.is_empty() {
        check(
            "governance_first",
            true,
            false,
            format!("governance let through: {}", missed.join(", ")),
        )
    } else {
        check(
            "governance_first",
            true,
            false,
            "governance vetoed a benign goal",
        )
    }
}

/// The sandbox must run a module and deny it the host filesystem.
fn sandbox_check(scratch: &Path) -> UspCheck {
    let run = |name: &str, wat: &str| -> Result<String, String> {
        let path = scratch.join(name);
        std::fs::write(&path, wat).map_err(|e| e.to_string())?;
        susi_native::wasm::WasmHost::execute_untrusted_wasm(&path, "crown")
            .map_err(|e| e.to_string())
    };
    match (
        run("hello.wat", WASM_HELLO_WAT),
        run("fs_probe.wat", WASM_HOST_FS_PROBE_WAT),
    ) {
        (Ok(hello), Ok(fs)) if hello == "hello world" && fs == "DENIED" => check(
            "sandbox_wasmer",
            true,
            true,
            "WasmHost ran an untrusted module and denied it host filesystem access",
        ),
        (hello, fs) => check(
            "sandbox_wasmer",
            true,
            false,
            format!("sandbox probe failed: run={hello:?} host_fs={fs:?}"),
        ),
    }
}

fn verify_all(workspace: &Path) -> Vec<UspCheck> {
    let mut out = Vec::new();
    let profile = live_hardware_profile();
    let scratch = std::env::temp_dir().join(format!("susi-crown-probe-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    let _ = std::fs::create_dir_all(&scratch);

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

    // --- Evidence (+ receipt archive mirror, pushed after glass box) ---
    let (evidence, receipt_archive) = probe_evidence(&scratch);
    out.push(evidence);

    // --- Swarm ---
    let swarm_cpus = profile.as_ref().map(|p| p.cpus).unwrap_or(0);
    out.push(check(
        "swarm",
        true,
        swarm_cpus > 0,
        format!(
            "plane bus gemi.hardware.profile answered ({swarm_cpus} cpus); GAWD/GEMI/tools/agents handlers registered"
        ),
    ));

    // --- Blackboard (round-trip in the scratch workspace, not the user's) ---
    let bb_path = {
        let store = susi_agents::HighDensityContextStore::new(4);
        store.insert("CrownCheck".into(), "ok".into());
        store.persist_inspectable(&scratch)
    };
    let bb_roundtrip = std::fs::read_to_string(&bb_path)
        .ok()
        .is_some_and(|s| s.contains("CrownCheck"));
    out.push(check(
        "blackboard",
        true,
        bb_roundtrip,
        format!(
            "persist_inspectable round-trips a written entry ({})",
            bb_path.display()
        ),
    ));

    // --- Glass box: the workspace's mission surfaces are readable ---
    let surfaces = [
        "last_blackboard.json",
        "last_mission_trace.json",
        "last_governance.json",
    ];
    let present: Vec<&str> = surfaces
        .iter()
        .copied()
        .filter(|f| workspace.join(".susi").join(f).is_file())
        .collect();
    let unreadable: Vec<&str> = present
        .iter()
        .copied()
        .filter(|f| !parses_as_json(&workspace.join(".susi").join(f)))
        .collect();
    out.push(check(
        "glass_box",
        true,
        bb_roundtrip && unreadable.is_empty(),
        if present.is_empty() {
            "no mission has run in this workspace yet; inspectable-surface writer verified in scratch".to_string()
        } else if unreadable.is_empty() {
            format!("mission surfaces parse: {}", present.join(", "))
        } else {
            format!("mission surfaces present but unreadable: {}", unreadable.join(", "))
        },
    ));
    out.push(receipt_archive);

    // --- Audit ---
    out.push(audit_chain_check(workspace));

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
    out.push(governance_check(&scratch));

    // --- Pluggable ---
    let registry = susi_core::registry::CapabilityRegistry::global();
    let caps = registry.list_all_capabilities().len();
    out.push(check(
        "pluggable",
        true,
        caps > 0,
        format!("CapabilityRegistry list_all_capabilities ({caps} mounted this process)"),
    ));

    // --- Sandbox (Wasmer critical; Docker host-gated) ---
    out.push(sandbox_check(&scratch));
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
    // Reflex synthesis compiles Rust to wasm32-wasip1; without that target
    // installed, capability-gap resolution can never produce a reflex.
    let wasi_target = std::process::Command::new("rustc")
        .args(["--print", "target-libdir", "--target", "wasm32-wasip1"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .is_some_and(|dir| Path::new(&dir).is_dir());
    out.push(check(
        "reflexes",
        true,
        wasi_target,
        format!(
            "{} ({} wasm installed); synthesis toolchain rustc/wasm32-wasip1 {}; threshold={}",
            reflex_dir.display(),
            wasm_n,
            if wasi_target {
                "ready"
            } else {
                "MISSING — reflexes cannot be synthesized"
            },
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
    let accel = profile
        .as_ref()
        .map(|p| p.native_acceleration.as_str())
        .unwrap_or("");
    let cuda_runtime = accel.contains("CUDA");
    out.push(check(
        "gpu_accel",
        false,
        if gpu_present && cuda_built {
            cuda_runtime
        } else {
            !gpu_present || cuda_built
        },
        if cuda_runtime {
            format!("CUDA device is the inference device ({accel})")
        } else if cuda_built && gpu_present {
            "cuda feature compiled in but Device::new_cuda did not become the inference device"
                .to_string()
        } else if gpu_present {
            "NVIDIA GPU present but binary is CPU-only — rebuild with ./build-gpu.sh".to_string()
        } else {
            "no NVIDIA GPU; CPU inference only".to_string()
        },
    ));

    // --- Concurrency: the pools the swarm and daemon actually run on ---
    let rayon_threads = rayon::current_num_threads();
    let tokio_ok = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .is_ok();
    out.push(check(
        "concurrency_first",
        true,
        rayon_threads > 1 && tokio_ok,
        format!("Rayon swarm pool {rayon_threads} threads; Tokio multi-thread runtime builds={tokio_ok}"),
    ));

    // --- Provision: the model inference would use exists on disk ---
    let selected = susi_core::plane_bus::gemi::ModelManager::get_selected_model(Some(workspace));
    let weights = selected
        .as_deref()
        .and_then(susi_core::plane_bus::gemi::ModelManager::get_model_path);
    let provisioned = weights.as_ref().is_some_and(|p| p.is_file());
    out.push(check(
        "provision",
        true,
        provisioned,
        match (&selected, &weights) {
            (Some(model), Some(path)) if provisioned => {
                format!("selected model {model} at {}", path.display())
            }
            (Some(model), Some(path)) => format!(
                "selected model {model} resolves to missing {}",
                path.display()
            ),
            (Some(model), None) => format!("selected model {model} has no weight path"),
            (None, _) => "no local model selected — provision one with `susi models`".to_string(),
        },
    ));

    let _ = std::fs::remove_dir_all(&scratch);
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
