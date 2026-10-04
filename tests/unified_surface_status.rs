#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! `susi status` is the single operator surface: missions in flight, the
//! scheduled queue, brain state and the model it would serve, recorded
//! spend, and pending approvals — every line measured from the persisted
//! stores, never narrated by an LLM (the old path sent a bare "status"
//! prompt through the swarm). These tests run the real binary against a
//! seeded throwaway HOME, so a regression that swaps any axis back to a
//! generated answer, drops an axis, or needs the daemon up fails here.

use std::path::{Path, PathBuf};
use std::process::Command;

fn scratch(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("susi-unified-status-{tag}-{}", std::process::id()))
}

/// Run `susi status` against `home` from a *separate* cwd — running with
/// cwd == HOME makes the binary's workspace-scoped state create `~/.susi`,
/// which flips `SusiDirs` off the XDG layout this test seeds.
/// Returns `(exit code, stdout)`: the report itself. stderr carries
/// timestamped tracing lines and is not part of the surface.
fn run_status(home: &Path, extra_env: &[(&str, PathBuf)]) -> (i32, String) {
    let cwd = home.join("cwd");
    std::fs::create_dir_all(&cwd).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_susi"));
    cmd.args(["status"])
        .current_dir(&cwd)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("xdg"))
        .env("XDG_DATA_HOME", home.join("xdg-data"))
        .env("XDG_CACHE_HOME", home.join("xdg-cache"))
        .env_remove("SUSI_HOME");
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

/// `SusiDirs::config_dir()` under the env `run_status` installs.
fn config_dir(home: &Path) -> PathBuf {
    home.join("xdg").join("susi")
}

#[test]
fn unified_surface_status_reports_all_axes() {
    let home = scratch("axes");
    let cfg = config_dir(&home);
    std::fs::create_dir_all(cfg.join("missions")).unwrap();

    // One mission dispatched to the ingress dir (runtime_admin's layout).
    std::fs::write(
        cfg.join("missions").join("m-7-1700000000.json"),
        serde_json::json!({
            "id": "m-7",
            "prompt": "scan the fleet",
            "dispatched_unix": 1_700_000_000u64,
        })
        .to_string(),
    )
    .unwrap();

    // One scheduled mission that has never run (due immediately).
    std::fs::write(
        cfg.join("scheduled-missions.json"),
        serde_json::json!({
            "missions": [{
                "id": "nightly-scan",
                "prompt": "scan",
                "schedule": {"every_secs": 3600},
                "paused": false,
            }]
        })
        .to_string(),
    )
    .unwrap();

    // Usage ledger: two recorded calls, one success each for two vendors.
    std::fs::write(
        cfg.join("usage.json"),
        serde_json::json!({
            "records": [
                {"provider": "groq", "task_class": "chat",
                 "usage": {"prompt_tokens": 100, "completion_tokens": 50},
                 "success": true},
                {"provider": "groq", "task_class": "code",
                 "usage": {"prompt_tokens": 200, "completion_tokens": 100},
                 "success": false},
                {"provider": "openai", "task_class": "chat",
                 "usage": {"prompt_tokens": 10, "completion_tokens": 5},
                 "success": true},
            ]
        })
        .to_string(),
    )
    .unwrap();

    // Brain evidence: one provider leads the `chat` class.
    let evidence = scratch("axes-evidence").join("brain_evidence.json");
    std::fs::create_dir_all(evidence.parent().unwrap()).unwrap();
    std::fs::write(
        &evidence,
        serde_json::json!({
            "records": {"groq|chat": {"ok": 9, "fail": 1, "ema_ms": 120.0}},
            "health": {},
            "applied": [],
        })
        .to_string(),
    )
    .unwrap();

    // A pending approval request on the shared broker rendezvous — a
    // sibling pid dir the CLI process's broker scans on read.
    let req_dir = home
        .join("xdg-cache")
        .join("susi")
        .join("bus")
        .join("99999999")
        .join("broker")
        .join("requests");
    std::fs::create_dir_all(&req_dir).unwrap();
    std::fs::write(
        req_dir.join("req-9.json"),
        serde_json::json!({
            "id": "req-9",
            "requester": "cell-a",
            "grantor": "susi",
            "scope": {"resource": "fs:/etc", "action": "write"},
            "ttl_secs": null,
            "created_at": 1_700_000_000u64,
            "status": "Pending",
            "resolved_at": null,
        })
        .to_string(),
    )
    .unwrap();

    let (code, out) = run_status(&home, &[("SUSI_BRAIN_EVIDENCE_FILE", evidence)]);

    assert_eq!(code, 0, "susi status failed: {out}");
    for axis in [
        "susi status",
        "daemon:",
        "missions in flight: 1",
        "m-7",
        "scheduled queue: 1",
        "brain: 1 provider(s) with evidence",
        "chat: groq",
        "budget:",
        "spend: 3 calls (2 ok), 310 prompt + 155 completion tokens",
        "groq: 2 calls, 1 ok",
        "pending approvals: 1",
        "cell-a asks fs:/etc:write (req-9)",
    ] {
        assert!(out.contains(axis), "missing `{axis}` in:\n{out}");
    }
    // The daemon was never needed: it stayed down and status still ran.
    assert!(
        out.contains("daemon: not running"),
        "status must answer with the daemon down:\n{out}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn unified_surface_status_empty_home_reports_measured_zeros() {
    let home = scratch("empty");
    std::fs::create_dir_all(&home).unwrap();
    let (code, out) = run_status(&home, &[]);

    assert_eq!(code, 0, "susi status failed on empty HOME: {out}");
    for line in [
        "daemon: not running",
        "missions in flight: 0",
        "scheduled queue: 0",
        "brain: 0 provider(s) with evidence",
        "spend: no usage recorded",
        "pending approvals: 0",
    ] {
        assert!(out.contains(line), "missing `{line}` in:\n{out}");
    }
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn unified_surface_status_is_deterministic_across_runs() {
    let home = scratch("stable");
    std::fs::create_dir_all(&home).unwrap();
    let (c1, o1) = run_status(&home, &[]);
    let (c2, o2) = run_status(&home, &[]);
    assert_eq!((c1, o1), (c2, o2), "status output must be deterministic");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn unified_surface_status_source_never_touches_the_swarm() {
    // Pin the mandate: the Status arm renders the collected snapshot; the
    // surface module itself never calls the swarm (`ama`, `solve_*`) or an
    // engine — a reintroduction of LLM narration fails this scan.
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let arm = std::fs::read_to_string(root.join("src/cli/mission_cli.rs")).unwrap();
    let status_arm = arm
        .split("Commands::Status => {")
        .nth(1)
        .and_then(|s| s.split("Commands::").next())
        .expect("Commands::Status arm must exist");
    assert!(
        status_arm.contains("status_cli::run"),
        "Status arm must dispatch to the unified surface"
    );
    let surface = std::fs::read_to_string(root.join("src/cli/status_cli.rs")).unwrap();
    for forbidden in [
        "solve_clean",
        "solve_stream",
        "SusiMasterAgent",
        ".ama",
        "generate(",
    ] {
        assert!(
            !surface.contains(forbidden),
            "status surface must not call the swarm/engine: found `{forbidden}`"
        );
    }
}
