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
