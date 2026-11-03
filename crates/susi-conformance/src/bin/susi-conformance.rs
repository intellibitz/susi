//! `susi-conformance`: check a running Susi host against the host contract.
//!
//! ```text
//! susi-conformance [--host 127.0.0.1] [--port-offset N] [--token-file PATH] [--json]
//! ```
//!
//! The token defaults to `$SUSI_HOME/api_token`, else `~/.susi/api_token`.
//! Exit status: 0 conformant (warnings allowed), 1 a check failed, 2 bad usage.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;
use susi_conformance::{conformant, run, Target};

fn default_token_file() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("SUSI_HOME") {
        return Some(PathBuf::from(home).join("api_token"));
    }
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(|home| PathBuf::from(home).join(".susi").join("api_token"))
}

fn usage(error: &str) -> ExitCode {
    eprintln!("susi-conformance: {error}");
    eprintln!(
        "usage: susi-conformance [--host HOST] [--port-offset N] [--token-file PATH] [--timeout-ms N] [--json]"
    );
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let mut host = "127.0.0.1".to_string();
    let mut port_offset = std::env::var("SUSI_PORT_OFFSET")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0u16);
    let mut token_file = default_token_file();
    let mut explicit_token = false;
    let mut timeout_ms = 5_000u64;
    let mut json = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value"));
        let parsed = match arg.as_str() {
            "--host" => value("--host").map(|v| host = v),
            "--port-offset" => value("--port-offset").and_then(|v| {
                v.parse()
                    .map(|n| port_offset = n)
                    .map_err(|_| format!("invalid port offset {v:?}"))
            }),
            "--token-file" => value("--token-file").map(|v| {
                token_file = Some(PathBuf::from(v));
                explicit_token = true;
            }),
            "--timeout-ms" => value("--timeout-ms").and_then(|v| {
                v.parse()
                    .map(|n| timeout_ms = n)
                    .map_err(|_| format!("invalid timeout {v:?}"))
            }),
            "--json" => {
                json = true;
                Ok(())
            }
            "-h" | "--help" => return usage("help"),
            other => Err(format!("unknown argument {other:?}")),
        };
        if let Err(error) = parsed {
            return usage(&error);
        }
    }
    let token = token_file
        .as_ref()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty());
    // A token the caller named must exist: otherwise every authenticated
    // check would be skipped and an unusable setup would read as conformant.
    if explicit_token && token.is_none() {
        let path = token_file
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        return usage(&format!("no bearer token in {path}"));
    }
    let target = Target {
        host,
        port_offset,
        token,
        timeout: Duration::from_millis(timeout_ms),
    };
    let checks = run(&target);
    let ok = conformant(&checks);
    if json {
        let report = serde_json::json!({
            "conformant": ok,
            "host": target.host,
            "port_offset": target.port_offset,
            "checks": checks.iter().map(susi_conformance::Check::to_json).collect::<Vec<_>>(),
        });
        println!("{report}");
    } else {
        for check in &checks {
            println!(
                "{:4}  {:>5} ms  {:28}  {}",
                check.outcome.label(),
                check.millis,
                check.id,
                check.detail
            );
        }
        let failed = checks
            .iter()
            .filter(|c| c.outcome == susi_conformance::Outcome::Fail)
            .count();
        println!(
            "{}: {} checks, {failed} failed",
            if ok { "CONFORMANT" } else { "NOT CONFORMANT" },
            checks.len()
        );
    }
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
