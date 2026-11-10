//! Execute the Unix installer against fake git, cargo and rustup: it builds
//! the release tag with its recorded settings and downloads nothing.
//! No provider, network, installed Susi, or user profile is touched.
#![allow(missing_docs)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

mod common;

const INSTALLER: &str = include_str!("../install.sh");

fn run_bash(script: &str) -> std::process::Output {
    let mut command = Command::new(common::posix_shell());
    command.env_clear();
    for name in ["PATH", "SystemRoot", "TMPDIR", "TEMP", "TMP"] {
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
fn installer_fixture_preserves_the_private_temp_directory() {
    let output = run_bash("printf '%s' \"${TMPDIR-}\"");
    assert!(output.status.success());
    assert_eq!(
        output.stdout,
        std::env::var_os("TMPDIR")
            .unwrap_or_default()
            .as_encoded_bytes()
    );
}

/// Fake `git`, `cargo`, `rustup`, `curl` and `wget` executables in
/// `$scratch/bin`, ahead of everything else on PATH. `git` serves one tag,
/// `v0.21.52`, whose record is the committed `releases/v0.21.52.json`; `cargo`
/// writes a fixture binary that reports `$FIXTURE_VERSION`. Every call is
/// logged to `$CALLS`; `curl` and `wget` fail, so any download shows up.
const FAKE_TOOLS: &str = r#"mkdir -p "$scratch/bin" "$scratch/fixture"
export CALLS="$scratch/calls" RECORD_FIXTURE="$scratch/fixture/record.json"
: >"$CALLS"
export FIXTURE_VERSION="${FIXTURE_VERSION:-0.21.52}" TAG_COMMIT="${TAG_COMMIT:-92f7ee498ac9103ee2e4bd2a86f0a646a20443ad}"
cat >"$RECORD_FIXTURE" <<'RECORD_EOF'
__RECORD__
RECORD_EOF
cat >"$scratch/bin/git" <<'GIT_EOF'
#!/usr/bin/env bash
printf 'git %s\n' "$*" >>"$CALLS"
if [ "$1" = -C ]; then dir="$2"; shift 2; fi
if [ "$1" = -c ]; then shift 2; fi
case "$1" in
    ls-remote) printf '%s\trefs/tags/v0.21.51\n%s\trefs/tags/v0.21.52\n' 1111111111111111111111111111111111111111 "$TAG_COMMIT" ;;
    clone) mkdir -p "${!#}/.git" ;;
    show) cat "$RECORD_FIXTURE" ;;
    rev-parse) echo "$TAG_COMMIT" ;;
    checkout) printf '[toolchain]\nchannel = "1.99.0"\n' >"$dir/rust-toolchain.toml" ;;
    remote | fetch | clean) ;;
    *) exit 64 ;;
esac
GIT_EOF
cat >"$scratch/bin/cargo" <<'CARGO_EOF'
#!/usr/bin/env bash
printf 'cargo %s LTO=%s CGU=%s AUTO=%s\n' "$*" "$CARGO_PROFILE_DIST_LTO" "$CARGO_PROFILE_DIST_CODEGEN_UNITS" "$RUSTUP_AUTO_INSTALL" >>"$CALLS"
mkdir -p "$CARGO_TARGET_DIR/dist"
printf '#!/usr/bin/env bash\nprintf "%%s\\n" "$*" >>"$CALLS"\ncase "$1" in --version) echo "susi %s";; *) exit 71;; esac\n' "$FIXTURE_VERSION" >"$CARGO_TARGET_DIR/dist/susi"
chmod +x "$CARGO_TARGET_DIR/dist/susi"
CARGO_EOF
cat >"$scratch/bin/rustup" <<'RUSTUP_EOF'
#!/usr/bin/env bash
printf 'rustup %s\n' "$*" >>"$CALLS"
[ "$1 $2" = "toolchain list" ] && printf '%s-x86_64-unknown-linux-gnu\n' "${RUSTUP_HAS:-1.99.0}"
RUSTUP_EOF
for tool in curl wget; do printf '#!/usr/bin/env bash\necho %s >>"$CALLS"\nexit 1\n' "$tool" >"$scratch/bin/$tool"; done
chmod +x "$scratch/bin/"*
export PATH="$scratch/bin:$PATH"
"#;

/// Exposes only fake tools and the real utilities needed to reach the
/// requirements check, assert its result and clean up. A host may install
/// Cargo beside tr/grep/rm; removing that entire PATH entry loses those too.
/// Shell lines that make `$utility` runnable from directory `$into` as a
/// one-line wrapper executing the host's own copy. Not a symlink: without
/// symlink privilege Git Bash's `ln -s` copies the file, and a copied MSYS
/// executable cannot load its runtime DLL from the scratch directory, so the
/// Windows fixture failed before the installer under test ever ran.
const LINK_UTILITY: &str = r#"target=$(command -v "$utility")
    printf '#!%s\nexec "%s" "$@"\n' "$BASH" "$target" >"$into/$utility"
    chmod +x "$into/$utility""#;

const WITHOUT: &str = r#"mkdir -p "$scratch/support-bin"
into="$scratch/support-bin"
for utility in bash tr grep rm; do
    __LINK_UTILITY__
done
rm "$scratch/bin/TOOL"
export PATH="$scratch/bin:$scratch/support-bin"
hash -r
if command -v TOOL >/dev/null 2>&1; then exit 91; fi"#;

/// Shell lines that remove the fake `tool` and leave only the utilities the
/// installer needs before it checks for its build tools.
fn without(tool: &str) -> String {
    WITHOUT
        .replace("__LINK_UTILITY__", LINK_UTILITY)
        .replace("TOOL", tool)
}

fn fake_tools() -> String {
    FAKE_TOOLS.replace(
        "__RECORD__",
        include_str!("../releases/v0.21.52.json").trim_end(),
    )
}

#[test]
fn installer_unix_builds_the_release_tag_with_its_recorded_settings_and_never_downloads() {
    // Exercise both explicit noninteractive mode and a pipe with no terminal.
    for noninteractive in ["1", ""] {
        let script = format!(
            r#"set -e
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
export HOME="$scratch/home" SUSI_HOME="$scratch/isolated home" SUSI_SOURCE_REPO="$scratch/origin.git"
export SUSI_ALWAYS_ON=1 NONINTERACTIVE='{noninteractive}'
mkdir -p "$HOME"
{tools}
uname() {{ if [ "$1" = '-s' ]; then echo Linux; else echo x86_64; fi; }}
pkill() {{ return 0; }}
(
{INSTALLER}
)
test -x "$SUSI_HOME/bin/susi"
test ! -e "$HOME/.susi"
test ! -e "$HOME/.profile"
test ! -e "$HOME/.config"
! grep -Eq '^(curl|wget)$' "$CALLS"
grep -q "^git ls-remote --tags --refs $SUSI_SOURCE_REPO v\*$" "$CALLS"
grep -q 'fetch --quiet --force origin refs/tags/v0.21.52:refs/tags/v0.21.52$' "$CALLS"
grep -q 'show FETCH_HEAD:releases/v0.21.52.json$' "$CALLS"
grep -q 'checkout --quiet --force --detach refs/tags/v0.21.52$' "$CALLS"
grep -qx 'cargo build --locked --profile dist -p susi --bin susi --features susi/local-engine LTO=false CGU=16 AUTO=0' "$CALLS"
test "$(grep -cx -- '--version' "$CALLS")" -eq 1
test ! -e "$SUSI_HOME/release-v0.21.52.json"
echo INSTALLER_CONTRACT_PASSED
"#,
            tools = fake_tools()
        );
        let output = run_bash(&script);
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "stdout={stdout} stderr={}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(stdout.contains("INSTALLER_CONTRACT_PASSED"));
        assert!(stdout.contains("the release records no reference build for susi-linux-x86_64"));
        assert!(stdout.contains("Open Susi and give it a goal:"));
    }
}

/// The version check is the gate, and the tag must point at the record's
/// commit; a missing tool, toolchain or source repository stops the install
/// with an accurate message, and nothing is installed in any of these cases.
#[test]
fn installer_unix_refuses_what_it_cannot_build_or_verify() {
    let cases = [
        (
            "export FIXTURE_VERSION=0.21.51".to_owned(),
            "reports version '0.21.51', expected '0.21.52'",
        ),
        (
            "export TAG_COMMIT=2222222222222222222222222222222222222222".to_owned(),
            "Refusing to build",
        ),
        (
            "export RUSTUP_HAS=1.98.0".to_owned(),
            "rustup toolchain install 1.99.0",
        ),
        (without("git"), "git is required"),
        (without("rustup"), "rustup was not found"),
        (without("cargo"), "cargo was not found"),
        ("unset SUSI_SOURCE_REPO".to_owned(), "Set SUSI_SOURCE_REPO"),
    ];
    for (setup, needle) in cases {
        let script = format!(
            r#"scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
export HOME="$scratch/home" SUSI_HOME="$scratch/install" SUSI_SOURCE_REPO="$scratch/origin.git" NONINTERACTIVE=1
mkdir -p "$HOME"
{tools}
{setup}
uname() {{ if [ "$1" = '-s' ]; then echo Linux; else echo x86_64; fi; }}
pkill() {{ return 0; }}
(
{INSTALLER}
)
status=$?
test "$status" -ne 0 && test ! -e "$SUSI_HOME/bin/susi" && ! grep -Eq '^(curl|wget)$' "$CALLS"
"#,
            tools = fake_tools()
        );
        let output = run_bash(&script);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{setup}: {stderr}");
        assert!(stderr.contains(needle), "{setup}: {stderr}");
    }
}

#[test]
fn missing_tool_fixture_preserves_utilities_beside_build_tools() {
    for (missing, needle) in [
        ("git", "git is required"),
        ("cargo", "cargo was not found"),
        ("rustup", "rustup was not found"),
    ] {
        let script = format!(
            r#"set -e
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
export HOME="$scratch/home" SUSI_HOME="$scratch/install" NONINTERACTIVE=1
{tools}
# Simulate a host whose only support-tool directory also contains the
# missing build tool. Keep its executable independent of the fake removed
# below, so filtering whole PATH directories would lose the utilities.
mkdir -p "$scratch/co-located"
into="$scratch/co-located"
for utility in bash tr grep rm mkdir ln chmod; do
    {link_utility}
done
printf '#!/usr/bin/env bash\nexit 97\n' >"$scratch/co-located/{missing}"
chmod +x "$scratch/co-located/{missing}"
export PATH="$scratch/bin:$scratch/co-located"
{without}
! command -v {missing} >/dev/null 2>&1
for retained in git cargo rustup; do
    [ "$retained" = '{missing}' ] || command -v "$retained" >/dev/null
done
printf 'UTILITY_OK\n' | tr '[:upper:]' '[:lower:]' | grep -qx utility_ok
: >"$scratch/remove-me"
rm "$scratch/remove-me"
test ! -e "$scratch/remove-me"
uname() {{ if [ "$1" = '-s' ]; then echo Linux; else echo x86_64; fi; }}
set +e
(
{INSTALLER}
)
status=$?
set -e
test "$status" -ne 0
test ! -e "$SUSI_HOME"
test ! -s "$CALLS"
"#,
            tools = fake_tools(),
            without = without(missing),
            link_utility = LINK_UTILITY,
        );
        let output = run_bash(&script);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{missing}: {stderr}");
        assert!(stderr.contains(needle), "{missing}: {stderr}");
        assert!(!stderr.contains("command not found"), "{missing}: {stderr}");
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

/// CEO Mandate 48: the installed susi tracks the latest release qualified for
/// the host it runs on. The updater skips a release whose record has no
/// reference build for this host, however new its tag.
#[test]
fn release_sync_picks_the_newest_release_qualified_for_this_host() {
    let source = include_str!("../scripts/susi-release-sync.sh");
    let selection = source
        .split_once("release_tags() {")
        .unwrap()
        .1
        .split_once("\ninstalled_version()")
        .unwrap()
        .0;
    let host_asset = source
        .split_once("host_asset() {")
        .unwrap()
        .1
        .split_once("\n}\n")
        .unwrap()
        .0;
    // v0.3.0 is the newest tag but was qualified on Windows only; v0.2.0 was
    // qualified on Linux; v0.1.0 has no record at all.
    let script = |system: &str, tags: &str| {
        format!(
            r#"set -euo pipefail
REMOTE=fake SRC=/nonexistent
log() {{ echo "$*" >&2; }}
uname() {{ case "$1" in -s) echo {system} ;; *) echo x86_64 ;; esac; }}
git() {{
    case "$*" in
        *ls-remote*) for t in {tags}; do echo "0000 refs/tags/$t"; done ;;
        *releases/v0.3.0.json) echo '    {{ "file": "susi-windows-x86_64.exe", "target": "x86_64-pc-windows-msvc" }}' ;;
        *releases/v0.2.0.json) echo '    {{ "file": "susi-linux-x86_64", "target": "x86_64-unknown-linux-gnu" }}' ;;
        *) return 1 ;;
    esac
}}
host_asset() {{{host_asset}
}}
release_tags() {{{selection}
latest_tag
latest_qualified_tag
"#
        )
    };
    let all = "v0.1.0 v0.2.0 v0.3.0 not-a-release";
    let linux = run_bash(&script("Linux", all));
    assert!(linux.status.success(), "{linux:?}");
    assert_eq!(String::from_utf8_lossy(&linux.stdout), "v0.3.0\nv0.2.0\n");
    let skipped = String::from_utf8_lossy(&linux.stderr);
    assert!(
        skipped.contains("skipping v0.3.0: not qualified for this host"),
        "{skipped}"
    );
    // On Windows the same remote yields the newest tag.
    let windows = run_bash(&script("MINGW64_NT", all));
    assert_eq!(String::from_utf8_lossy(&windows.stdout), "v0.3.0\nv0.3.0\n");
    // Nothing qualified here: no tag is chosen, and that is not an error.
    let none = run_bash(&script("Linux", "v0.1.0 v0.3.0"));
    assert!(none.status.success(), "{none:?}");
    assert_eq!(String::from_utf8_lossy(&none.stdout), "v0.3.0\n");
    // The explicit path refuses too, unless knowingly overridden.
    assert!(source.contains("SUSI_RELEASE_ALLOW_UNQUALIFIED_HOST"));
    assert!(source.contains("was not qualified for this host"));
}
