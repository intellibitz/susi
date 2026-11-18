//! The native engine's local fallback looks for a running local engine on
//! every inference request. It must only detect (PATH lookup, port check):
//! running `<engine> --version` launched Jan's desktop window once per request,
//! because Jan's `jan` command is its GUI launcher.
#![cfg(unix)]
#![allow(missing_docs)] // integration test crate: no public API to document
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test-only assertions

use std::os::unix::fs::PermissionsExt;
use susi_gemi::engines::runtime::{NativeInferenceEngine, SusiGgufEngine};
use susi_paths::test_env::EnvGuard;

#[test]
fn local_fallback_runs_no_installed_engine_binary() {
    let root = susi_paths::tempfile::tempdir().expect("tempdir");
    let bin = root.path().join("bin");
    std::fs::create_dir(&bin).expect("bin dir");
    let marker = root.path().join("launched");
    // One stand-in per engine the scan knows; each records any launch.
    for name in [
        "jan",
        "ollama",
        "llama-server",
        "llama-cli",
        "vllm",
        "sglang",
        "local-ai",
    ] {
        let path = bin.join(name);
        std::fs::write(
            &path,
            format!("#!/bin/sh\necho \"{name} $*\" >> '{}'\n", marker.display()),
        )
        .expect("stand-in");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    // A port nothing listens on, so no engine on the real host answers.
    let closed = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .expect("free port")
        .port();
    let ports = [
        "ollama",
        "llama-cpp",
        "vllm",
        "sglang",
        "localai",
        "jan",
        "gpt4all",
    ]
    .map(|id| format!("{id}={closed}"))
    .join(",");
    // This file holds a single test, so its process environment is its own.
    let home = root.path().join("home");
    let mut env = EnvGuard::isolated();
    env.set("HOME", &home)
        .set("SUSI_HOME", home.join(".susi"))
        .set("XDG_CACHE_HOME", home.join(".cache"))
        .set("XDG_CONFIG_HOME", home.join(".config"))
        .set("XDG_DATA_HOME", home.join(".local/share"))
        .set("PATH", &bin)
        .set("SUSI_ENGINE_PORTS", ports)
        .remove("SUSI_TEST_MOCK_INFERENCE");

    let result = SusiGgufEngine.run_inference("hello");

    assert!(
        result.is_err(),
        "no engine runs, so nothing may answer: {result:?}"
    );
    let launched = std::fs::read_to_string(&marker).unwrap_or_default();
    assert!(
        launched.is_empty(),
        "the local fallback launched installed engine binaries:\n{launched}"
    );
}
