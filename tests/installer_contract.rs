//! Execute the Unix installer with a checksum-verified fixture download.
//! No provider, network, installed Susi, or user profile is touched.
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
    for noninteractive in ["1", ""] {
        let script = format!(
            r#"set -e
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
export HOME="$scratch/home" SUSI_HOME="$scratch/isolated home"
export CALLS="$scratch/calls" FIXTURE="$scratch/fixture"
export SUSI_ALWAYS_ON=1 NONINTERACTIVE='{noninteractive}'
mkdir -p "$HOME"
cat >"$FIXTURE" <<'FIXTURE_EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$CALLS"
case "$1" in --version) echo 'susi fixture';; *) exit 71;; esac
FIXTURE_EOF
uname() {{ if [ "$1" = '-s' ]; then echo Linux; else echo x86_64; fi; }}
nvidia-smi() {{ return 1; }}
pkill() {{ return 0; }}
curl() {{
    local url='' dest=''
    while [ "$#" -gt 0 ]; do
        case "$1" in https:*) url="$1";; -o) shift; dest="$1";; esac
        shift
    done
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
