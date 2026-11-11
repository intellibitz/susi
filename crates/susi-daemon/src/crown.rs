//! Tier S crown: verify every USP holds in source/runtime (not marketing).
//! The checks `susi crown verify|status` print and the MCP `crown_verify`
//! tool returns over the `daemon.crown.verify` plane topic (zero-UI plan B4
//! group 5): one implementation for both.
//!
//! Critical checks must pass for exit 0. Host-gated capabilities report
//! `ready`/`optional` without failing the crown when the host lacks Docker/daemon.
use crate::discovery_pipeline::Catalogs;
use serde::Serialize;
use std::path::Path;
use susi_paths::ports;

/// What `crown verify|status` primes: the `pluggable` check reports the agent
/// capabilities the execution-agent and framework scans register, and the
/// `provision` check reads the selected model that coding-model auto-prefer
/// can write. No check reads the enabled MCP set.
pub const PRIMED: Catalogs = Catalogs::AGENTS.union(Catalogs::CODING_MODELS);

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
    crate::composition::wire_plane_bus();
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
///
/// The host executes compiled `.wasm` only (`susi-vendor-wasmtime` refuses the
/// text format so no parser ships in the product), so this probe carries the
/// compiled bytes of the fixture below, produced once with `wat 1.262`:
///
/// ```wat
/// (module
///     (import "wasi_snapshot_preview1" "fd_write" (func $fd_write (param i32 i32 i32 i32) (result i32)))
///     (memory 1)
///     (export "memory" (memory 0))
///     (data (i32.const 8) "hello world")
///     (func (export "_start")
///         (i32.store (i32.const 0) (i32.const 8))
///         (i32.store (i32.const 4) (i32.const 11))
///         (drop (call $fd_write (i32.const 1) (i32.const 0) (i32.const 1) (i32.const 20)))))
/// ```
const WASM_HELLO: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x0c, 0x02, 0x60, //
    0x04, 0x7f, 0x7f, 0x7f, 0x7f, 0x01, 0x7f, 0x60, 0x00, 0x00, 0x02, 0x23, //
    0x01, 0x16, 0x77, 0x61, 0x73, 0x69, 0x5f, 0x73, 0x6e, 0x61, 0x70, 0x73, //
    0x68, 0x6f, 0x74, 0x5f, 0x70, 0x72, 0x65, 0x76, 0x69, 0x65, 0x77, 0x31, //
    0x08, 0x66, 0x64, 0x5f, 0x77, 0x72, 0x69, 0x74, 0x65, 0x00, 0x00, 0x03, //
    0x02, 0x01, 0x01, 0x05, 0x03, 0x01, 0x00, 0x01, 0x07, 0x13, 0x02, 0x06, //
    0x6d, 0x65, 0x6d, 0x6f, 0x72, 0x79, 0x02, 0x00, 0x06, 0x5f, 0x73, 0x74, //
    0x61, 0x72, 0x74, 0x00, 0x01, 0x0a, 0x1d, 0x01, 0x1b, 0x00, 0x41, 0x00, //
    0x41, 0x08, 0x36, 0x02, 0x00, 0x41, 0x04, 0x41, 0x0b, 0x36, 0x02, 0x00, //
    0x41, 0x01, 0x41, 0x00, 0x41, 0x01, 0x41, 0x14, 0x10, 0x00, 0x1a, 0x0b, //
    0x0b, 0x11, 0x01, 0x00, 0x41, 0x08, 0x0b, 0x0b, 0x68, 0x65, 0x6c, 0x6c, //
    0x6f, 0x20, 0x77, 0x6f, 0x72, 0x6c, 0x64, 0x00, 0x12, 0x04, 0x6e, 0x61, //
    0x6d, 0x65, 0x01, 0x0b, 0x01, 0x00, 0x08, 0x66, 0x64, 0x5f, 0x77, 0x72, //
    0x69, 0x74, 0x65, //
];

/// Tries to open `etc/passwd` through the first would-be preopened directory
/// and prints `DENIED` or `OPENED` — the isolation half of the sandbox claim.
///
/// Compiled from:
///
/// ```wat
/// (module
///     (import "wasi_snapshot_preview1" "path_open"
///         (func $path_open (param i32 i32 i32 i32 i32 i64 i64 i32 i32) (result i32)))
///     (import "wasi_snapshot_preview1" "fd_write" (func $fd_write (param i32 i32 i32 i32) (result i32)))
///     (memory 1)
///     (export "memory" (memory 0))
///     (data (i32.const 100) "etc/passwd")
///     (data (i32.const 200) "DENIED")
///     (data (i32.const 300) "OPENED")
///     (func (export "_start")
///         (if (i32.eqz (call $path_open (i32.const 3) (i32.const 0) (i32.const 100) (i32.const 10)
///                 (i32.const 0) (i64.const 2) (i64.const 0) (i32.const 0) (i32.const 400)))
///             (then (i32.store (i32.const 0) (i32.const 300)))
///             (else (i32.store (i32.const 0) (i32.const 200))))
///         (i32.store (i32.const 4) (i32.const 6))
///         (drop (call $fd_write (i32.const 1) (i32.const 0) (i32.const 1) (i32.const 20)))))
/// ```
const WASM_HOST_FS_PROBE: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x19, 0x03, 0x60, //
    0x09, 0x7f, 0x7f, 0x7f, 0x7f, 0x7f, 0x7e, 0x7e, 0x7f, 0x7f, 0x01, 0x7f, //
    0x60, 0x04, 0x7f, 0x7f, 0x7f, 0x7f, 0x01, 0x7f, 0x60, 0x00, 0x00, 0x02, //
    0x46, 0x02, 0x16, 0x77, 0x61, 0x73, 0x69, 0x5f, 0x73, 0x6e, 0x61, 0x70, //
    0x73, 0x68, 0x6f, 0x74, 0x5f, 0x70, 0x72, 0x65, 0x76, 0x69, 0x65, 0x77, //
    0x31, 0x09, 0x70, 0x61, 0x74, 0x68, 0x5f, 0x6f, 0x70, 0x65, 0x6e, 0x00, //
    0x00, 0x16, 0x77, 0x61, 0x73, 0x69, 0x5f, 0x73, 0x6e, 0x61, 0x70, 0x73, //
    0x68, 0x6f, 0x74, 0x5f, 0x70, 0x72, 0x65, 0x76, 0x69, 0x65, 0x77, 0x31, //
    0x08, 0x66, 0x64, 0x5f, 0x77, 0x72, 0x69, 0x74, 0x65, 0x00, 0x01, 0x03, //
    0x02, 0x01, 0x02, 0x05, 0x03, 0x01, 0x00, 0x01, 0x07, 0x13, 0x02, 0x06, //
    0x6d, 0x65, 0x6d, 0x6f, 0x72, 0x79, 0x02, 0x00, 0x06, 0x5f, 0x73, 0x74, //
    0x61, 0x72, 0x74, 0x00, 0x02, 0x0a, 0x41, 0x01, 0x3f, 0x00, 0x41, 0x03, //
    0x41, 0x00, 0x41, 0xe4, 0x00, 0x41, 0x0a, 0x41, 0x00, 0x42, 0x02, 0x42, //
    0x00, 0x41, 0x00, 0x41, 0x90, 0x03, 0x10, 0x00, 0x45, 0x04, 0x40, 0x41, //
    0x00, 0x41, 0xac, 0x02, 0x36, 0x02, 0x00, 0x05, 0x41, 0x00, 0x41, 0xc8, //
    0x01, 0x36, 0x02, 0x00, 0x0b, 0x41, 0x04, 0x41, 0x06, 0x36, 0x02, 0x00, //
    0x41, 0x01, 0x41, 0x00, 0x41, 0x01, 0x41, 0x14, 0x10, 0x01, 0x1a, 0x0b, //
    0x0b, 0x29, 0x03, 0x00, 0x41, 0xe4, 0x00, 0x0b, 0x0a, 0x65, 0x74, 0x63, //
    0x2f, 0x70, 0x61, 0x73, 0x73, 0x77, 0x64, 0x00, 0x41, 0xc8, 0x01, 0x0b, //
    0x06, 0x44, 0x45, 0x4e, 0x49, 0x45, 0x44, 0x00, 0x41, 0xac, 0x02, 0x0b, //
    0x06, 0x4f, 0x50, 0x45, 0x4e, 0x45, 0x44, 0x00, 0x1d, 0x04, 0x6e, 0x61, //
    0x6d, 0x65, 0x01, 0x16, 0x02, 0x00, 0x09, 0x70, 0x61, 0x74, 0x68, 0x5f, //
    0x6f, 0x70, 0x65, 0x6e, 0x01, 0x08, 0x66, 0x64, 0x5f, 0x77, 0x72, 0x69, //
    0x74, 0x65, //
];

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
            match crate::susi_sandbox::audit_chain::verify_chain(path) {
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
                &["post gh", "p_crownprobe0000000000 to a gist"].concat(),
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
    let run = |name: &str, module: &[u8]| -> Result<String, String> {
        let path = scratch.join(name);
        std::fs::write(&path, module).map_err(|e| e.to_string())?;
        susi_vendor_wasmtime::wasm::WasmHost::execute_untrusted_wasm(&path, "crown")
            .map_err(|e| e.to_string())
    };
    match (
        run("hello.wasm", WASM_HELLO),
        run("fs_probe.wasm", WASM_HOST_FS_PROBE),
    ) {
        (Ok(hello), Ok(fs)) if hello == "hello world" && fs == "DENIED" => check(
            "sandbox_wasmtime",
            true,
            true,
            "WasmHost ran an untrusted module and denied it host filesystem access",
        ),
        (hello, fs) => check(
            "sandbox_wasmtime",
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
    // The detail used to assert every plane's handlers were registered
    // while only GEMI's hardware answer was checked; check each plane.
    let swarm_cpus = profile.as_ref().map(|p| p.cpus).unwrap_or(0);
    let bus = susi_core::plane_bus::PlaneBus::global();
    let planes = [
        ("GAWD", susi_core::plane_bus::topics::GAWD_SOLVE),
        ("GEMI", susi_core::plane_bus::topics::GEMI_HARDWARE_PROFILE),
        ("tools", susi_core::plane_bus::topics::TOOLS_EXECUTE),
        ("agents", susi_core::plane_bus::topics::AGENTS_META_LIST),
    ];
    let unwired: Vec<&str> = planes
        .iter()
        .filter(|(_, topic)| !bus.is_wired(topic))
        .map(|(plane, _)| *plane)
        .collect();
    out.push(check(
        "swarm",
        true,
        swarm_cpus > 0 && unwired.is_empty(),
        if unwired.is_empty() {
            format!(
                "plane bus gemi.hardware.profile answered ({swarm_cpus} cpus); GAWD/GEMI/tools/agents handlers wired"
            )
        } else {
            format!("plane handlers not wired: {}", unwired.join(", "))
        },
    ));

    // --- Blackboard (round-trip in the scratch workspace, not the user's) ---
    let bb_path = {
        let store = susi_core::HighDensityContextStore::new(4);
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
    let pack = crate::susi_sandbox::extensions::ensure_extensions_substrate();
    let auto_ok = pack.is_ok();
    let _ = crate::boot_trace::phase("crown: prime catalogs", || {
        crate::discovery_pipeline::prime_selected_catalogs(
            &susi_paths::SusiDirs::substrate_home(),
            PRIMED,
        )
    });
    out.push(check(
        "zero_config_auto",
        true,
        auto_ok,
        match &pack {
            Ok(p) => format!("active pack `{}` ({})", p.id, p.root.display()),
            Err(e) => e.clone(),
        },
    ));

    // --- Governance-first ---
    out.push(governance_check(&scratch));

    // --- Pluggable ---
    // Core ships no pack (zero-UI plan A9), so the proof is the generic
    // mechanism (Mandate 62): the extension substrate resolves (compiled
    // defaults with nothing installed) and a pack built outside this binary
    // is admitted through the manifest/checksum/signature path without a
    // rebuild, while a tampered one is refused. Mounted capabilities depend
    // on which packs the host installed, so they are reported, not gated.
    let caps = susi_core::registry::CapabilityRegistry::global()
        .list_all_capabilities()
        .len();
    let admission = crate::susi_sandbox::extensions::pack_admission_probe(&scratch);
    out.push(check(
        "pluggable",
        true,
        pack.is_ok() && admission.is_ok(),
        match (&pack, &admission) {
            (Ok(p), Ok(proof)) => format!(
                "extension substrate resolves (active `{}`); {proof}; {caps} capabilities mounted this process",
                p.id
            ),
            (Err(e), _) => format!("extension substrate unresolved: {e}"),
            (_, Err(e)) => format!("pack admission broken: {e}"),
        },
    ));

    // --- Sandbox (Wasmer critical; Docker host-gated) ---
    out.push(sandbox_check(&scratch));
    let docker = susi_gawd::external::docker_present();
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
        crate::SusiDaemon::host_contract_ready(),
        if crate::SusiDaemon::host_contract_ready() {
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
    let wasi_target = susi_paths::console::background_command("rustc")
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
            crate::susi_sandbox::manager::SusiConfig::load_global()
                .unwrap_or_default()
                .reflex_training_threshold()
        ),
    ));

    // --- GPU acceleration (non-critical host capability) ---
    let gpu_present = susi_core::bounded_cmd::output_within(
        susi_paths::console::background_command("nvidia-smi").arg("-L"),
        std::time::Duration::from_secs(5),
    )
    .is_ok_and(|o| o.status.success());
    let accel = profile
        .as_ref()
        .map(|p| p.native_acceleration.as_str())
        .unwrap_or("");
    out.push(check(
        "gpu_accel",
        false,
        !gpu_present || accel.contains("Metal"),
        if accel.contains("Metal") {
            format!("Metal device is the inference device ({accel})")
        } else if gpu_present {
            "NVIDIA hardware detected; in-process CUDA/MKL support is retired because its dependencies do not pass product license/native-code gates. CPU inference and external providers remain available.".to_string()
        } else {
            "CPU inference selected; no NVIDIA hardware detected".to_string()
        },
    ));

    // --- Concurrency: the pools the swarm and daemon actually run on ---
    let swarm_threads = susi_gawd::swarm_pool_threads();
    let tokio_ok = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .is_ok();
    out.push(check(
        "concurrency_first",
        true,
        swarm_threads > 1 && tokio_ok,
        format!(
            "Swarm worker pool (std scoped threads) {swarm_threads} threads; Tokio multi-thread runtime builds={tokio_ok}"
        ),
    ));

    // --- USP ROI & SOVA Algorithmic Arbitrage ---
    let sova = susi_harness::roi::verify_sova_invariants();
    let tier0_roi = susi_gemi::pulse::SusiPulse::roi_summary(workspace);
    out.push(check(
        "usp_roi_arbitrage",
        true,
        sova.is_ok(),
        match sova {
            Ok(proof) => format!(
                "{proof}; Tier-0 offload {} bps (saved {} micro-USD across {} local reflexes)",
                tier0_roi.offload_rate_bps,
                tier0_roi.avoided_cloud_micro_usd,
                tier0_roi
                    .sub_100us_cache_hits
                    .saturating_add(tier0_roi.neural_reflex_hits)
                    .saturating_add(tier0_roi.tier1_generative_hits),
            ),
            Err(error) => format!("SOVA invariant failed: {error}"),
        },
    ));

    // --- Provision: the model inference would use exists on disk ---
    let selected = susi_core::plane_bus::gemi::ModelManager::get_selected_model(Some(workspace));
    let weights = selected
        .as_deref()
        .and_then(susi_core::plane_bus::gemi::ModelManager::get_model_path);
    let provisioned = weights.as_ref().is_some_and(|p| p.is_file());
    // A selected model whose weights are missing is a broken install and
    // fails the gate; a fresh host that has not picked a model yet is host
    // state (like Docker or a listening daemon), reported but not critical.
    out.push(check(
        "provision",
        selected.is_some(),
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

/// `susi crown verify|status` / MCP `crown_verify`: run every check, persist
/// the document to `last_crown.json` (config dir and workspace) and return it.
pub fn report(workspace: &Path) -> serde_json::Value {
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

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod inline_tests {
    use super::*;

    #[test]
    fn primed_catalogs_combine_agents_and_coding_models_only() {
        let primed = std::hint::black_box(&PRIMED);
        assert!(primed.coding_models);
        assert!(primed.execution_agents);
        assert!(primed.frameworks);
        assert_eq!(PRIMED, Catalogs::ALL);
        assert_ne!(PRIMED, Catalogs::NONE);
    }

    #[test]
    fn check_constructor_populates_tier_s_and_fields() {
        let item = check("probe_id", true, false, "probe detail");
        assert_eq!(item.id, "probe_id");
        assert_eq!(item.tier, "S");
        assert!(item.critical);
        assert!(!item.holds);
        assert_eq!(item.detail, "probe detail");
    }

    #[test]
    fn parses_as_json_validates_files_and_rejects_malformed_or_missing() {
        let dir = std::env::temp_dir().join(format!("susi-crown-inline-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let good = dir.join("good.json");
        let bad = dir.join("bad.json");
        let missing = dir.join("missing.json");
        std::fs::write(&good, r#"{"ok":true}"#).unwrap();
        std::fs::write(&bad, "{not json").unwrap();

        assert!(parses_as_json(&good));
        assert!(!parses_as_json(&bad));
        assert!(!parses_as_json(&missing));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The embedded probe modules must still be exactly what the host runs:
    /// binary `.wasm` that executes, prints, and is denied the host
    /// filesystem. A corrupted byte array fails here, not only in a live
    /// `crown verify`.
    #[test]
    fn sandbox_check_runs_the_embedded_binary_probes() {
        let dir = std::env::temp_dir().join(format!("susi-crown-sandbox-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let sandbox = sandbox_check(&dir);
        assert_eq!(sandbox.id, "sandbox_wasmtime");
        assert!(sandbox.critical);
        assert!(sandbox.holds, "detail: {}", sandbox.detail);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn governance_check_vetoes_destructive_and_clears_benign_probes() {
        let dir = std::env::temp_dir().join(format!("susi-crown-gov-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let gov = governance_check(&dir);
        assert_eq!(gov.id, "governance_first");
        assert!(gov.critical);
        assert!(gov.holds, "detail: {}", gov.detail);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn probe_evidence_produces_citable_receipt_and_mirrored_archive_in_scratch() {
        let dir = std::env::temp_dir().join(format!("susi-crown-ev-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let (evidence, archive) = probe_evidence(&dir);
        assert_eq!(evidence.id, "evidence");
        assert!(evidence.holds, "detail: {}", evidence.detail);
        assert_eq!(archive.id, "receipt_archive");
        assert!(archive.holds, "detail: {}", archive.detail);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
