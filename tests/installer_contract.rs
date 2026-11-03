//! Execute the Unix installer with a checksum-verified fixture download.
//! No provider, network, installed Susi, or user profile is touched.
// Optional POSIX/hosted compatibility fixtures execute shell scripts.
// Native Windows delivery is covered by the Rust coordinator, bootstrap,
// installer and release suites; it must not require a POSIX userland.
#![allow(missing_docs)]
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const INSTALLER: &str = include_str!("../install.sh");

fn run_bash(script: &str) -> std::process::Output {
    let bash = if cfg!(windows) {
        "C:/Program Files/Git/bin/bash.exe"
    } else {
        "bash"
    };
    let mut command = Command::new(bash);
    command.env_clear();
    for name in ["PATH", "SystemRoot", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    let mut child = command
        .arg("--noprofile")
        .arg("--norc")
        .env_remove("BASH_ENV")
        .env_remove("SUSI_FORCE_SOURCE")
        .env_remove("SUSI_NO_DAEMON")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("bash is required to verify its installer");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(script.as_bytes())
        .unwrap();
    let started = Instant::now();
    while child.try_wait().unwrap().is_none() {
        if started.elapsed() > Duration::from_secs(20) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("a headless installer must not wait for interactive input");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    child.wait_with_output().unwrap()
}

#[test]
fn installer_unix_step1_isolates_verified_install_and_never_launches_headless() {
    // Exercise both explicit noninteractive mode and a pipe with no terminal.
    for (noninteractive, gpu_status) in [("1", 0), ("", 0), ("1", 1), ("", 1)] {
        let script = format!(
            r#"set -e
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
export HOME="$scratch/home" SUSI_HOME="$scratch/isolated home"
export CALLS="$scratch/calls" FIXTURE="$scratch/fixture" URLS="$scratch/urls"
export SUSI_ALWAYS_ON=1 NONINTERACTIVE='{noninteractive}'
mkdir -p "$HOME"
cat >"$FIXTURE" <<'FIXTURE_EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$CALLS"
case "$1" in --version) echo 'susi fixture';; *) exit 71;; esac
FIXTURE_EOF
uname() {{ if [ "$1" = '-s' ]; then echo Linux; else echo x86_64; fi; }}
nvidia-smi() {{ return {gpu_status}; }}
pkill() {{ return 0; }}
curl() {{
    local url='' dest=''
    while [ "$#" -gt 0 ]; do
        case "$1" in https:*) url="$1";; -o) shift; dest="$1";; esac
        shift
    done
    printf '%s\n' "$url" >>"$URLS"
    case "$url" in
        *.sha256)
            if command -v sha256sum >/dev/null 2>&1; then
                sha256sum "$FIXTURE" >"$dest"
            else
                shasum -a 256 "$FIXTURE" >"$dest"
            fi;;
        *) cp "$FIXTURE" "$dest";;
    esac
}}
(
{INSTALLER}
)
test -x "$SUSI_HOME/bin/susi"
test "$(cat "$CALLS")" = '--version'
test ! -e "$HOME/.susi"
test ! -e "$HOME/.profile"
test ! -e "$HOME/.config"
test "$(wc -l <"$URLS")" -eq 2
test "$(grep -c '/susi-linux-x86_64$' "$URLS")" -eq 1
test "$(grep -c '/susi-linux-x86_64.sha256$' "$URLS")" -eq 1
! grep -Eq 'cuda|mkl|tar.gz' "$URLS"
echo INSTALLER_CONTRACT_PASSED
"#
        );
        let output = run_bash(&script);
        assert!(
            output.status.success(),
            "stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("INSTALLER_CONTRACT_PASSED"));
        assert!(String::from_utf8_lossy(&output.stdout).contains("Open Susi and give it a goal:"));
    }
}

#[test]
fn installer_unix_step1_rejects_relative_home_before_writes() {
    let script = format!(
        r#"scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
cd "$scratch" || exit 1
export HOME="$scratch" SUSI_HOME=relative-install-home
uname() {{ if [ "$1" = '-s' ]; then echo Linux; else echo x86_64; fi; }}
(
{INSTALLER}
)
status=$?
test "$status" -ne 0 && test ! -e relative-install-home
"#
    );
    let output = run_bash(&script);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("SUSI_HOME must be an absolute"));
}

/// Release scope (CEO decision 2026-11-03): the free core release is x86_64
/// Windows and x86_64 Linux only. macOS and non-x86_64 Linux are refused with
/// the commercial-suite message before any write, download or build.
#[test]
fn installer_refuses_platforms_outside_the_public_release_scope_before_writes() {
    for (system, machine, needle) in [
        ("Darwin", "arm64", "this is macOS"),
        ("Darwin", "x86_64", "this is macOS"),
        ("Linux", "aarch64", "this is aarch64 Linux"),
        ("Linux", "armv7l", "this is armv7l Linux"),
    ] {
        let script = format!(
            r#"scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
export HOME="$scratch/home" SUSI_HOME="$scratch/install" NONINTERACTIVE=1
uname() {{ if [ "$1" = '-s' ]; then echo {system}; else echo {machine}; fi; }}
curl() {{ echo curl-called >>"$scratch/calls"; return 1; }}
cargo() {{ echo cargo-called >>"$scratch/calls"; return 1; }}
(
{INSTALLER}
)
status=$?
test "$status" -ne 0 && test ! -e "$SUSI_HOME" && test ! -e "$scratch/calls"
"#
        );
        let output = run_bash(&script);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{system}/{machine}: {stderr}");
        assert!(stderr.contains(needle), "{system}/{machine}: {stderr}");
        assert!(
            stderr.contains("x86_64 Windows and x86_64 Linux only")
                && stderr.contains("Susi commercial suite"),
            "{system}/{machine}: {stderr}"
        );
    }
}

#[test]
fn installer_rejects_retired_build_override_before_download_or_install() {
    for feature in ["cuda", "mkl", "metal,cuda", "susi-gemi/mkl"] {
        let script = format!(
            r#"scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
export HOME="$scratch/home" SUSI_HOME="$scratch/install" SUSI_BUILD_FEATURES='{feature}'
(
{INSTALLER}
)
status=$?
test "$status" -ne 0 && test ! -e "$SUSI_HOME"
"#
        );
        let output = run_bash(&script);
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("retired CUDA/MKL"));
    }
}

#[test]
fn release_sync_rejects_retired_overrides_and_does_not_auto_select_nvidia() {
    let source = include_str!("../scripts/susi-release-sync.sh");
    let function = source
        .split_once("detect_features() {")
        .unwrap()
        .1
        .split_once("\ncheckout_tag()")
        .unwrap()
        .0;
    for (features, rejected) in [
        ("cuda", true),
        ("mkl", true),
        ("metal,cuda", true),
        ("susi-gemi/cuda", true),
        ("metal", false),
        ("", false),
    ] {
        let script = format!(
            r#"die() {{ echo "$*" >&2; return 1; }}
uname() {{ echo Linux; }}
nvidia-smi() {{ return 0; }}
detect_features() {{{function}
export SUSI_RELEASE_FEATURES='{features}'
detect_features
"#
        );
        let output = run_bash(&script);
        assert_eq!(
            !output.status.success(),
            rejected,
            "{features}: {:?}",
            output
        );
        if rejected {
            assert!(String::from_utf8_lossy(&output.stderr).contains("retired"));
        } else {
            assert_eq!(String::from_utf8_lossy(&output.stdout), features);
        }
    }
    let script = format!(
        r#"uname() {{ echo Linux; }}
nvidia-smi() {{ return 0; }}
detect_features() {{{function}
unset SUSI_RELEASE_FEATURES
test -z "$(detect_features)"
"#
    );
    assert!(run_bash(&script).status.success());
    let script = format!(
        r#"die() {{ echo "$*" >&2; return 1; }}
detect_features() {{{function}
export SUSI_BUILD_FEATURES=cuda
unset SUSI_RELEASE_FEATURES
detect_features
"#
    );
    let output = run_bash(&script);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("retired"));
}
