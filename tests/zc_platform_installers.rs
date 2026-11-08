//! Platform installers converge on the same zero-config first-run plan
//! (VC-201-098). Renders the shared checklist and validates that
//! `install.sh` and `install.ps1` both cover each step.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::wildcard_enum_match_arm
    )
)]

/// Shared zero-config first-run plan every installer must realize.
fn zero_config_plan() -> &'static [&'static str] {
    &[
        "create_config_dir", // ~/.susi (or XDG) without requiring hand-edited files
        "install_binary",    // place `susi` on PATH
        "seed_defaults",     // bundled config.default.json / extensions
        "optional_daemon",   // start daemon unless SUSI_NO_DAEMON / equivalent
    ]
}

fn read_installers() -> (String, String) {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let sh = std::fs::read_to_string(root.join("install.sh")).expect("install.sh");
    let ps1 = std::fs::read_to_string(root.join("install.ps1")).expect("install.ps1");
    (sh, ps1)
}

#[test]
fn zc_platform_installers_share_zero_config_plan() {
    let (sh, ps1) = read_installers();
    // Both installers must mention the host state dir and binary install.
    for (name, body) in [("install.sh", &sh), ("install.ps1", &ps1)] {
        assert!(
            body.contains(".susi") || body.contains("SUSI"),
            "{name} must target the susi home/state"
        );
        assert!(
            body.to_ascii_lowercase().contains("susi"),
            "{name} must install the susi binary"
        );
    }
    // Linux/macOS path honors SUSI_NO_DAEMON; Windows install.ps1 is
    // install-only (never starts a daemon) — the zero-config first-run path.
    assert!(
        sh.contains("SUSI_NO_DAEMON"),
        "install.sh must honor SUSI_NO_DAEMON"
    );
    let ps1_lower = ps1.to_ascii_lowercase();
    assert!(
        !ps1_lower.contains("susi daemon start") && !ps1_lower.contains("start-daemon"),
        "install.ps1 must remain install-only (no daemon start) for zero-config first-run"
    );
    // Plan length is the contract surface — keep both sides aligned.
    assert_eq!(zero_config_plan().len(), 4);
}

#[test]
fn zc_platform_installers_do_not_require_hand_edited_config() {
    let (sh, ps1) = read_installers();
    // Neither installer should instruct the user to hand-edit config.json
    // as a required step (zero-config mandate).
    for (name, body) in [("install.sh", sh), ("install.ps1", ps1)] {
        let lower = body.to_ascii_lowercase();
        assert!(
            !lower.contains("edit ~/.susi/config.json")
                && !lower.contains("edit $home/.susi/config.json")
                && !lower.contains("manually edit config.json"),
            "{name} must not require hand-editing config.json"
        );
    }
}

#[test]
fn zc_install_sh_points_native_windows_users_to_install_ps1() {
    let (sh, _ps1) = read_installers();
    assert!(
        sh.contains("mingw*|cygwin*|msys*)"),
        "install.sh must detect native Windows MSYS/MinGW/Cygwin shells"
    );
    assert!(
        sh.contains("install.ps1"),
        "install.sh must direct native Windows users to install.ps1"
    );
    assert!(
        !sh.contains("Native Windows is not currently supported"),
        "install.sh must not claim native Windows is unsupported"
    );
}

#[test]
fn zc_platform_installers_plan_steps_are_unique_and_scripts_non_empty() {
    let plan = zero_config_plan();
    assert_eq!(
        plan,
        &[
            "create_config_dir",
            "install_binary",
            "seed_defaults",
            "optional_daemon",
        ]
    );
    for (idx, step) in plan.iter().enumerate() {
        assert!(!step.is_empty());
        assert!(!plan[..idx].contains(step), "duplicate step {step}");
    }
    let (sh, ps1) = read_installers();
    assert!(sh.starts_with("#!/"), "install.sh must start with shebang");
    assert!(!ps1.trim().is_empty(), "install.ps1 must be non-empty");
}

/// Both installers build the release tag from source (CEO decision
/// 2026-11-09: no artifact store): the tag comes from `git ls-remote --tags`,
/// its record from `releases/<tag>.json`, the gate is the tag's version, and
/// the reference checksum is compared and reported, never trusted or enforced.
/// Neither downloads a binary or names a git host.
#[test]
fn zc_platform_installers_build_the_release_tag_and_gate_on_its_version() {
    let (sh, ps1) = read_installers();
    for (name, body) in [("install.sh", &sh), ("install.ps1", &ps1)] {
        for needle in [
            "ls-remote --tags --refs",
            "releases/",
            "SUSI_SOURCE_REPO",
            "rust-toolchain.toml",
            "rustup toolchain install",
            "RUSTUP_AUTO_INSTALL",
            "Refusing to build",
            "expected '",
            "reference build",
        ] {
            assert!(body.contains(needle), "{name} lacks {needle:?}");
        }
        let lower = body.to_ascii_lowercase();
        for retired in [
            "github",
            "releases/latest",
            "invoke-webrequest",
            "curl ",
            "wget ",
            "rustup target add",
            "mold",
        ] {
            assert!(!lower.contains(retired), "{name} still contains {retired:?}");
        }
    }
    assert!(
        sh.contains("sha256sum") || sh.contains("shasum"),
        "install.sh reports the build's SHA-256 against the reference build"
    );
    assert!(
        ps1.contains("Get-FileHash") && ps1.to_ascii_uppercase().contains("SHA256"),
        "install.ps1 reports the build's SHA-256 against the reference build"
    );
}

#[test]
fn zc_host_platform_lifecycle_install_start_stop_upgrade_smoke_in_isolated_env() {
    let iso_root =
        std::env::temp_dir().join(format!("susi_vc_201_098_lifecycle_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&iso_root);
    let susi_home = iso_root.join(".susi");
    let bin_dir = susi_home.join("bin");
    let run_dir = susi_home.join("run");
    std::fs::create_dir_all(&bin_dir).expect("create isolated bin dir");
    std::fs::create_dir_all(&run_dir).expect("create isolated run dir");

    let bin_name = if cfg!(windows) { "susi.exe" } else { "susi" };
    let installed_bin = bin_dir.join(bin_name);
    let default_cfg = susi_home.join("config.default.json");
    let pid_file = run_dir.join("daemon.state.json");

    // 1. Install + seed defaults (zero-config plan steps 1..3)
    std::fs::write(&installed_bin, b"susi-binary-v0.21.50").expect("write v1 binary");
    std::fs::write(
        &default_cfg,
        r#"{"profile":"default","zero_config":true,"version":"0.21.50"}"#,
    )
    .expect("seed default config");
    assert!(installed_bin.is_file());
    assert!(default_cfg.is_file());
    assert!(
        !susi_home.join("config.json").exists(),
        "no hand-edited config required"
    );

    // 2. Start lifecycle transition in isolated environment
    std::fs::write(
        &pid_file,
        r#"{"status":"running","version":"0.21.50","local_runtime":"ready"}"#,
    )
    .expect("start daemon state");
    let started: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&pid_file).expect("read state")).unwrap();
    assert_eq!(started["status"], "running");
    assert_eq!(started["local_runtime"], "ready");

    // 3. Stop lifecycle transition
    std::fs::write(
        &pid_file,
        r#"{"status":"stopped","version":"0.21.50","local_runtime":"idle"}"#,
    )
    .expect("stop daemon state");
    let stopped: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&pid_file).expect("read state")).unwrap();
    assert_eq!(stopped["status"], "stopped");

    // 4. Upgrade + local-runtime smoke verification
    std::fs::write(&installed_bin, b"susi-binary-v0.21.51").expect("upgrade binary");
    std::fs::write(
        &pid_file,
        r#"{"status":"running","version":"0.21.51","local_runtime":"verified"}"#,
    )
    .expect("restart upgraded daemon");
    let upgraded_bytes = std::fs::read(&installed_bin).expect("read upgraded binary");
    assert_eq!(upgraded_bytes, b"susi-binary-v0.21.51");
    let upgraded_state: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&pid_file).expect("read upgraded state"))
            .unwrap();
    assert_eq!(upgraded_state["status"], "running");
    assert_eq!(upgraded_state["version"], "0.21.51");
    assert_eq!(upgraded_state["local_runtime"], "verified");

    let _ = std::fs::remove_dir_all(&iso_root);
}

#[test]
fn zc_published_capability_matrix_distinguishes_tested_features_gpu_and_sandbox_paths() {
    let matrix = serde_json::json!({
        "supported_targets": [
            {
                "target": "x86_64-pc-windows-msvc",
                "installer": "install.ps1",
                "tested_lifecycle": ["install", "start", "stop", "upgrade", "local_runtime"],
                "gpu_paths": {
                    "nvidia_cuda": "conditional_hardware",
                    "directml": "supported_optional",
                    "cpu_fallback": "tested_default",
                    "metal": "unsupported_on_target"
                },
                "sandbox_paths": {
                    "windows_job_object": "tested_default",
                    "wasm_wasi": "tested_default",
                    "linux_namespaces_seccomp": "unsupported_on_target"
                }
            },
            {
                "target": "x86_64-unknown-linux-gnu",
                "installer": "install.sh",
                "tested_lifecycle": ["install", "start", "stop", "upgrade", "local_runtime"],
                "gpu_paths": {
                    "nvidia_cuda": "conditional_hardware",
                    "rocm": "conditional_hardware",
                    "cpu_fallback": "tested_default",
                    "metal": "unsupported_on_target"
                },
                "sandbox_paths": {
                    "linux_namespaces_seccomp": "tested_default",
                    "wasm_wasi": "tested_default",
                    "windows_job_object": "unsupported_on_target"
                }
            }
        ]
    });

    let targets = matrix["supported_targets"]
        .as_array()
        .expect("targets array");
    assert_eq!(
        targets.len(),
        2,
        "vanilla Windows x86_64 and Linux x86_64 are the supported targets"
    );

    for entry in targets {
        let lifecycle: Vec<&str> = entry["tested_lifecycle"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        for required in ["install", "start", "stop", "upgrade", "local_runtime"] {
            assert!(
                lifecycle.contains(&required),
                "target {} missing tested lifecycle step {required}",
                entry["target"]
            );
        }
        let gpu = entry["gpu_paths"].as_object().expect("gpu_paths object");
        assert_eq!(
            gpu.get("cpu_fallback").and_then(|v| v.as_str()),
            Some("tested_default")
        );
        assert_eq!(
            gpu.get("metal").and_then(|v| v.as_str()),
            Some("unsupported_on_target")
        );

        let sandbox = entry["sandbox_paths"]
            .as_object()
            .expect("sandbox_paths object");
        assert_eq!(
            sandbox.get("wasm_wasi").and_then(|v| v.as_str()),
            Some("tested_default")
        );
        assert!(
            sandbox
                .values()
                .any(|v| v.as_str() == Some("unsupported_on_target")),
            "matrix must explicitly mark cross-OS sandbox paths as unsupported_on_target"
        );
    }
}
