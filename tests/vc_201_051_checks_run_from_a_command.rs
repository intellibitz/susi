#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! VC-201-051: the provider contract checks must run *from a command*.
//! The library existed (`probe_credentialed_opt_in`, `probe_fixture`) but
//! had no caller — an operator could not execute it. `susi admin
//! provider-contract` now runs the fixture-backed checks for all six
//! kinds unconditionally and the credentialed read-only probes only when
//! `SUSI_PROVIDER_PROBE=1`.

use std::path::{Path, PathBuf};
use std::process::Command;

fn scratch(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("susi-vc201051-{tag}-{}", std::process::id()))
}

/// `susi admin provider-contract` in a throwaway instance. `env_clear`
/// keeps inherited provider keys (OPENAI_API_KEY, …) out of the child so
/// `configured_targets` finds nothing and the opt-in leg stays hermetic.
fn run_probe(home: &Path, probe_env: Option<&str>) -> (i32, String) {
    std::fs::create_dir_all(home.join("cwd")).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_susi"));
    cmd.args(["admin", "provider-contract"])
        .current_dir(home.join("cwd"))
        .env_clear()
        .env("HOME", home)
        .env("SUSI_HOME", home.join(".susi-dev"))
        .env("PATH", std::env::var("PATH").unwrap_or_default());
    if let Some(v) = probe_env {
        cmd.env("SUSI_PROVIDER_PROBE", v);
    }
    let out = cmd.output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

/// The report is a pretty-printed JSON object after the boot-time tracing
/// line; slice from the first '{' to the end.
fn report(out: &str) -> serde_json::Value {
    let start = out.find('{').expect("JSON report in stdout");
    serde_json::from_str(&out[start..]).expect("valid JSON report")
}

#[test]
fn vc_201_051_checks_run_from_a_command_fixture_covers_all_six_kinds() {
    let home = scratch("fixture");
    let (code, out) = run_probe(&home, None);
    assert_eq!(code, 0, "provider-contract failed: {out}");
    let report = report(&out);
    let claims = report["fixture"].as_array().expect("fixture claim list");
    let kinds: std::collections::BTreeSet<String> = claims
        .iter()
        .map(|c| c["kind"].as_str().unwrap().to_string())
        .collect();
    for kind in [
        "authentication",
        "streaming",
        "tool_calls",
        "embeddings",
        "usage",
        "errors",
    ] {
        assert!(kinds.contains(kind), "fixture checks must cover {kind}");
    }
    // Every claim carries the contract's required fields.
    for c in claims {
        for field in [
            "provider",
            "model",
            "version",
            "observed_unix",
            "status",
            "detail",
        ] {
            assert!(c.get(field).is_some(), "claim missing {field}: {c}");
        }
    }
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn vc_201_051_checks_run_from_a_command_credentialed_probe_is_opt_in() {
    let home = scratch("optin");
    // Default: the live half is recorded as not opted in, claims empty.
    let (code, out) = run_probe(&home, None);
    assert_eq!(code, 0, "provider-contract failed: {out}");
    let credentialed = &report(&out)["credentialed"];
    assert_eq!(credentialed["opted_in"], serde_json::json!(false));
    assert_eq!(
        credentialed["claims"].as_array().unwrap().len(),
        0,
        "no live claims without the opt-in"
    );

    // Opted in with zero configured endpoints: the gate flips and the
    // report records explicit `unsupported` claims — "checked, nothing
    // configured" — instead of silence or fabricated coverage.
    let (code, out) = run_probe(&home, Some("1"));
    assert_eq!(code, 0, "opted-in run failed: {out}");
    let credentialed = &report(&out)["credentialed"];
    assert_eq!(credentialed["opted_in"], serde_json::json!(true));
    let claims = credentialed["claims"].as_array().unwrap();
    assert_eq!(claims.len(), 6, "one honest claim per probe kind");
    for c in claims {
        assert_eq!(c["status"], serde_json::json!("unsupported"));
        assert!(
            c["detail"]
                .as_str()
                .unwrap()
                .contains("no configured credentialed endpoint"),
            "empty config must be reported, not hidden: {c}"
        );
    }
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn vc_202_051_checks_run_from_a_command() {
    // The original gap: probe_credentialed_opt_in existed with zero
    // callers outside its own file. Pin the fix — the admin command must
    // invoke it (or its live-probe sibling) and the fixture checks.
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let admin = std::fs::read_to_string(root.join("src/cli/admin_cli.rs")).unwrap();
    assert!(
        admin.contains("probe_credentialed_opt_in"),
        "the admin command must call the credentialed probe entry point"
    );
    assert!(
        admin.contains("fixture_checks"),
        "the admin command must run the fixture-backed checks"
    );
    // And the probe must not narrate: it composes measured claims.
    let module =
        std::fs::read_to_string(root.join("crates/susi-gemi/src/provider_contract.rs")).unwrap();
    for forbidden in ["solve_clean", "solve_stream", "SusiMasterAgent"] {
        assert!(
            !module.contains(forbidden),
            "contract probes must not call the swarm: found `{forbidden}`"
        );
    }
}
