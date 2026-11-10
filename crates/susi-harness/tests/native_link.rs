//! Audit the actual standalone harness, not just its Cargo dependency graph.
#![cfg(all(windows, target_env = "msvc", target_arch = "x86_64"))]
#![forbid(unsafe_code)]
// Test assertions: a failed inspection is the test's failure.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::{fs, path::Path, process::Command};
use susi_build_os_ucrt::{native_link, pe_imports};

#[test]
fn standalone_harness_uses_original_rust_entry_and_only_os_imports() {
    let binary = Path::new(env!("CARGO_BIN_EXE_susi-harness"));
    let bytes = fs::read(binary).unwrap();
    let imports = pe_imports::inspect(&bytes).unwrap();
    assert!(imports.iter().any(|name| name == "import:ucrtbase.dll"));
    // Cargo may separate final artifacts from intermediate linker output.
    // The running test and the linked binary's map share the build profile.
    let test_binary = std::env::current_exe().unwrap();
    let profile = test_binary.parent().unwrap().parent().unwrap();
    let map = native_link::find_map(profile, "susi-harness").unwrap();
    let map = fs::read_to_string(map).unwrap();
    native_link::validate_map(native_link::entry_address(&bytes).unwrap(), &map).unwrap();
}

#[test]
fn standalone_harness_startup_answers_before_loading_credentials() {
    let home = tempfile::tempdir().unwrap();
    for argument in ["--version", "--help"] {
        let output = Command::new(env!("CARGO_BIN_EXE_susi-harness"))
            .env_clear()
            .env("SUSI_NO_REGISTRY_ENV", "1")
            .env("SUSI_HOME", home.path())
            .current_dir(home.path())
            .arg(argument)
            .output()
            .unwrap();
        assert!(output.status.success(), "{argument}: {:?}", output.status);
        assert!(String::from_utf8_lossy(&output.stdout).contains("susi-harness"));
        assert!(output.stderr.is_empty());
    }
    assert_eq!(fs::read_dir(home.path()).unwrap().count(), 0);
}
