//! `susi-conformance`: check a running Susi host against the host contract.
//!
//! ```text
//! susi-conformance [--host 127.0.0.1] [--port-offset N] [--token-file PATH] [--max-ms N]
//!                  [--susi-bin PATH] [--inference] [--signed-udp-ping PAYLOAD]
//!                  [--signed-udp-pong-prefix PREFIX] [--json]
//! ```
//!
//! The token defaults to `$SUSI_HOME/ports.json` (`api_token_path`) or
//! `$SUSI_HOME/api_token`, else `~/.susi/api_token` if `~/.susi` exists, else
//! the OS config directory (`%APPDATA%/susi/api_token`,
//! `~/Library/Application Support/susi/api_token`, or
//! `$XDG_CONFIG_HOME/susi/api_token` / `~/.config/susi/api_token`).
//! Exit status: 0 conformant (warnings allowed), 1 a check failed, 2 bad usage.
#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;
use susi_conformance::{conformant, enforce_latency_budget, run_with, Optional, Target};

fn token_from_susi_dir(dir: &Path) -> PathBuf {
    let ports_path = dir.join("ports.json");
    if let Ok(raw) = std::fs::read_to_string(&ports_path) {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw) {
            if let Some(p) = val["api_token_path"].as_str() {
                let trimmed = p.trim();
                if !trimmed.is_empty() {
                    return PathBuf::from(trimmed);
                }
            }
        }
    }
    dir.join("api_token")
}

/// Pure, cross-OS token file resolution (`OSN-26` / `B8`).
///
/// Priority order:
/// 1. `SUSI_HOME` (`ports.json` `api_token_path` if present, else `api_token`).
/// 2. Existing `~/.susi` (`ports.json` `api_token_path` if present, else `api_token`).
/// 3. Platform config directory fallback:
///    - Windows: `%APPDATA%/susi/api_token`
///    - macOS: `~/Library/Application Support/susi/api_token`
///    - Linux/other: `$XDG_CONFIG_HOME/susi/api_token`, else `~/.config/susi/api_token`
fn resolve_default_token_file_with(
    susi_home: Option<&Path>,
    user_home: Option<&Path>,
    appdata: Option<&Path>,
    xdg_config: Option<&Path>,
    os: &str,
) -> Option<PathBuf> {
    if let Some(h) = susi_home.filter(|p| !p.as_os_str().is_empty()) {
        return Some(token_from_susi_dir(h));
    }
    if let Some(u) = user_home.filter(|p| !p.as_os_str().is_empty()) {
        let dot_susi = u.join(".susi");
        if dot_susi.exists() {
            return Some(token_from_susi_dir(&dot_susi));
        }
    }
    match os {
        "windows" => appdata
            .filter(|p| !p.as_os_str().is_empty())
            .map(|a| token_from_susi_dir(&a.join("susi")))
            .or_else(|| {
                user_home
                    .filter(|p| !p.as_os_str().is_empty())
                    .map(|u| token_from_susi_dir(&u.join(".susi")))
            }),
        "macos" => user_home.filter(|p| !p.as_os_str().is_empty()).map(|u| {
            token_from_susi_dir(&u.join("Library").join("Application Support").join("susi"))
        }),
        _ => xdg_config
            .filter(|p| !p.as_os_str().is_empty())
            .map(|x| token_from_susi_dir(&x.join("susi")))
            .or_else(|| {
                user_home
                    .filter(|p| !p.as_os_str().is_empty())
                    .map(|u| token_from_susi_dir(&u.join(".config").join("susi")))
            }),
    }
}

fn default_token_file() -> Option<PathBuf> {
    let susi_home = std::env::var_os("SUSI_HOME").map(PathBuf::from);
    let user_home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .or_else(|| std::env::var_os("HOME"))
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from);
    let appdata = std::env::var_os("APPDATA").map(PathBuf::from);
    let xdg_config = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from);
    resolve_default_token_file_with(
        susi_home.as_deref(),
        user_home.as_deref(),
        appdata.as_deref(),
        xdg_config.as_deref(),
        std::env::consts::OS,
    )
}

#[derive(Debug, Clone)]
struct CliOptions {
    host: String,
    port_offset: u16,
    token_file: Option<PathBuf>,
    explicit_token: bool,
    timeout_ms: u64,
    json: bool,
    max_ms: Option<u128>,
    optional: Optional,
}

fn parse_cli_args(
    args: &[String],
    default_offset: u16,
    default_token: Option<PathBuf>,
) -> Result<CliOptions, String> {
    let mut opts = CliOptions {
        host: "127.0.0.1".to_string(),
        port_offset: default_offset,
        token_file: default_token,
        explicit_token: false,
        timeout_ms: 5_000u64,
        json: false,
        max_ms: None,
        optional: Optional::default(),
    };
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let mut value = |name: &str| {
            iter.next()
                .cloned()
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match arg.as_str() {
            "--host" => opts.host = value("--host")?,
            "--port-offset" => {
                let v = value("--port-offset")?;
                opts.port_offset = v
                    .parse()
                    .map_err(|_| format!("invalid port offset {v:?}"))?;
            }
            "--token-file" => {
                let v = value("--token-file")?;
                opts.token_file = Some(PathBuf::from(v));
                opts.explicit_token = true;
            }
            "--timeout-ms" => {
                let v = value("--timeout-ms")?;
                opts.timeout_ms = v.parse().map_err(|_| format!("invalid timeout {v:?}"))?;
            }
            "--max-ms" => {
                let v = value("--max-ms")?;
                opts.max_ms = Some(
                    v.parse()
                        .map_err(|_| format!("invalid latency budget {v:?}"))?,
                );
            }
            "--susi-bin" => {
                let v = value("--susi-bin")?;
                opts.optional.susi_bin = Some(PathBuf::from(v));
            }
            "--signed-udp-ping" => {
                let v = value("--signed-udp-ping")?;
                opts.optional.signed_udp_ping = Some(v);
            }
            "--signed-udp-pong-prefix" => {
                let v = value("--signed-udp-pong-prefix")?;
                opts.optional.signed_udp_pong_prefix = Some(v);
            }
            "--inference" => opts.optional.inference = true,
            "--json" => opts.json = true,
            "-h" | "--help" => return Err("help".to_string()),
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    Ok(opts)
}

fn usage(error: &str) -> ExitCode {
    eprintln!("susi-conformance: {error}");
    eprintln!(
        "usage: susi-conformance [--host HOST] [--port-offset N] [--token-file PATH] [--timeout-ms N] [--max-ms N] [--susi-bin PATH] [--inference] [--signed-udp-ping PAYLOAD] [--signed-udp-pong-prefix PREFIX] [--json]"
    );
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let default_offset = std::env::var("SUSI_PORT_OFFSET")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0u16);
    let raw_args: Vec<String> = std::env::args().skip(1).collect();
    let opts = match parse_cli_args(&raw_args, default_offset, default_token_file()) {
        Ok(opts) => opts,
        Err(error) => return usage(&error),
    };
    let token = opts
        .token_file
        .as_ref()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty());
    // A token the caller named must exist: otherwise every authenticated
    // check would be skipped and an unusable setup would read as conformant.
    if opts.explicit_token && token.is_none() {
        let path = opts
            .token_file
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        return usage(&format!("no bearer token in {path}"));
    }
    let target = Target {
        host: opts.host,
        port_offset: opts.port_offset,
        token,
        timeout: Duration::from_millis(opts.timeout_ms),
    };
    let mut checks = run_with(&target, &opts.optional);
    if let Some(budget) = opts.max_ms {
        enforce_latency_budget(&mut checks, budget);
    }
    let ok = conformant(&checks);
    if opts.json {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_token_resolution_honors_susi_home_ports_json_dot_susi_and_osn26_fallbacks() {
        let temp =
            std::env::temp_dir().join(format!("susi-conformance-cli-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp);
        std::fs::create_dir_all(&temp).unwrap();

        // 1. SUSI_HOME with ports.json api_token_path
        let susi_home = temp.join("susi_home");
        std::fs::create_dir_all(&susi_home).unwrap();
        let custom_token = temp.join("custom_api_token");
        std::fs::write(
            susi_home.join("ports.json"),
            serde_json::json!({ "api_token_path": custom_token.to_string_lossy() }).to_string(),
        )
        .unwrap();
        assert_eq!(
            resolve_default_token_file_with(Some(&susi_home), None, None, None, "linux"),
            Some(custom_token)
        );

        // 2. SUSI_HOME without ports.json
        let plain_susi_home = temp.join("plain_susi_home");
        std::fs::create_dir_all(&plain_susi_home).unwrap();
        assert_eq!(
            resolve_default_token_file_with(Some(&plain_susi_home), None, None, None, "linux"),
            Some(plain_susi_home.join("api_token"))
        );

        // 3. Existing ~/.susi directory takes priority over platform fallback
        let user_with_dot = temp.join("user_with_dot");
        let dot_susi = user_with_dot.join(".susi");
        std::fs::create_dir_all(&dot_susi).unwrap();
        let appdata = temp.join("appdata");
        assert_eq!(
            resolve_default_token_file_with(
                None,
                Some(&user_with_dot),
                Some(&appdata),
                None,
                "windows"
            ),
            Some(dot_susi.join("api_token"))
        );

        // 4. Platform fallbacks when ~/.susi does not exist (OSN-26)
        let user_no_dot = temp.join("user_no_dot");
        std::fs::create_dir_all(&user_no_dot).unwrap();
        let xdg = temp.join("xdg_config");

        // Windows -> %APPDATA%/susi/api_token
        assert_eq!(
            resolve_default_token_file_with(
                None,
                Some(&user_no_dot),
                Some(&appdata),
                None,
                "windows"
            ),
            Some(appdata.join("susi").join("api_token"))
        );

        // macOS -> ~/Library/Application Support/susi/api_token
        assert_eq!(
            resolve_default_token_file_with(None, Some(&user_no_dot), None, None, "macos"),
            Some(
                user_no_dot
                    .join("Library")
                    .join("Application Support")
                    .join("susi")
                    .join("api_token")
            )
        );

        // Linux with XDG_CONFIG_HOME -> $XDG_CONFIG_HOME/susi/api_token
        assert_eq!(
            resolve_default_token_file_with(None, Some(&user_no_dot), None, Some(&xdg), "linux"),
            Some(xdg.join("susi").join("api_token"))
        );

        // Linux without XDG_CONFIG_HOME -> ~/.config/susi/api_token
        assert_eq!(
            resolve_default_token_file_with(None, Some(&user_no_dot), None, None, "linux"),
            Some(user_no_dot.join(".config").join("susi").join("api_token"))
        );

        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn parse_cli_args_accepts_all_flags_including_signed_udp_ping_and_rejects_bad_inputs() {
        let args = [
            "--host",
            "10.0.0.5",
            "--port-offset",
            "100",
            "--token-file",
            "/tmp/tok",
            "--timeout-ms",
            "2500",
            "--max-ms",
            "1500",
            "--susi-bin",
            "/usr/local/bin/susi",
            "--inference",
            "--signed-udp-ping",
            "SUSI_PING_SIG2:probe",
            "--signed-udp-pong-prefix",
            "SUSI_PONG_SIG2:",
            "--json",
        ]
        .map(String::from);

        let parsed = parse_cli_args(&args, 0, None).unwrap();
        assert_eq!(parsed.host, "10.0.0.5");
        assert_eq!(parsed.port_offset, 100);
        assert_eq!(parsed.token_file, Some(PathBuf::from("/tmp/tok")));
        assert!(parsed.explicit_token);
        assert_eq!(parsed.timeout_ms, 2500);
        assert_eq!(parsed.max_ms, Some(1500));
        assert_eq!(
            parsed.optional.susi_bin,
            Some(PathBuf::from("/usr/local/bin/susi"))
        );
        assert!(parsed.optional.inference);
        assert_eq!(
            parsed.optional.signed_udp_ping.as_deref(),
            Some("SUSI_PING_SIG2:probe")
        );
        assert_eq!(
            parsed.optional.signed_udp_pong_prefix.as_deref(),
            Some("SUSI_PONG_SIG2:")
        );
        assert!(parsed.json);

        // Error cases
        assert!(parse_cli_args(&["--port-offset".into(), "not-a-num".into()], 0, None).is_err());
        assert!(parse_cli_args(&["--timeout-ms".into()], 0, None).is_err());
        assert!(parse_cli_args(&["--unknown-flag".into()], 0, None).is_err());
        assert_eq!(
            parse_cli_args(&["--help".into()], 0, None).unwrap_err(),
            "help"
        );
    }

    #[test]
    fn token_from_susi_dir_falls_back_on_malformed_ports_json_and_empty_args_keep_defaults() {
        let temp = std::env::temp_dir().join(format!(
            "susi-conformance-fallback-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&temp);
        std::fs::create_dir_all(&temp).unwrap();

        std::fs::write(temp.join("ports.json"), "{not-valid-json").unwrap();
        assert_eq!(token_from_susi_dir(&temp), temp.join("api_token"));

        std::fs::write(temp.join("ports.json"), r#"{"api_token_path":"   "}"#).unwrap();
        assert_eq!(token_from_susi_dir(&temp), temp.join("api_token"));

        let _ = std::fs::remove_dir_all(&temp);

        let defaults = parse_cli_args(&[], 42, Some(PathBuf::from("/default/tok"))).unwrap();
        assert_eq!(defaults.host, "127.0.0.1");
        assert_eq!(defaults.port_offset, 42);
        assert_eq!(defaults.token_file, Some(PathBuf::from("/default/tok")));
        assert!(!defaults.explicit_token);
        assert_eq!(defaults.timeout_ms, 5_000);
        assert!(!defaults.json);
        assert_eq!(defaults.max_ms, None);
    }

    #[test]
    fn resolve_default_token_file_with_reads_ports_json_in_dot_susi_and_handles_all_none_inputs() {
        let temp = std::env::temp_dir().join(format!(
            "susi-conformance-dotsusi-ports-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&temp);
        std::fs::create_dir_all(&temp).unwrap();

        let user_home = temp.join("user_home");
        let dot_susi = user_home.join(".susi");
        std::fs::create_dir_all(&dot_susi).unwrap();
        let custom_dot_tok = temp.join("dot_susi_custom_tok");
        std::fs::write(
            dot_susi.join("ports.json"),
            serde_json::json!({ "api_token_path": custom_dot_tok.to_string_lossy() }).to_string(),
        )
        .unwrap();

        assert_eq!(
            resolve_default_token_file_with(None, Some(&user_home), None, None, "linux"),
            Some(custom_dot_tok)
        );

        let win_user_no_dot = temp.join("win_user_no_dot");
        std::fs::create_dir_all(&win_user_no_dot).unwrap();
        assert_eq!(
            resolve_default_token_file_with(None, Some(&win_user_no_dot), None, None, "windows"),
            Some(win_user_no_dot.join(".susi").join("api_token"))
        );

        let empty = Path::new("");
        for os in ["windows", "macos", "linux"] {
            assert_eq!(
                resolve_default_token_file_with(None, None, None, None, os),
                None
            );
            assert_eq!(
                resolve_default_token_file_with(
                    Some(empty),
                    Some(empty),
                    Some(empty),
                    Some(empty),
                    os
                ),
                None
            );
        }

        let _ = std::fs::remove_dir_all(&temp);
    }
}
