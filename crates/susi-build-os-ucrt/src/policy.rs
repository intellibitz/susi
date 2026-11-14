#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

//! Regression gates for standalone platform adapters excluded from the workspace.
use std::io;
use std::path::Path;

/// Include native release-tooling tests in complete workspace acceptance.
pub fn include_release_tooling(root: &Path, gates: &mut [Vec<String>]) {
    if !root.join("crates/susi-core/Cargo.toml").is_file() {
        return;
    }
    for args in gates {
        if matches!(args.first().map(String::as_str), Some("clippy" | "test"))
            && args.iter().any(|arg| arg == "--workspace")
        {
            let at = args
                .iter()
                .position(|arg| arg == "--")
                .unwrap_or(args.len());
            args.splice(
                at..at,
                ["--features".into(), "xtask/release-tooling".into()],
            );
        }
    }
}

/// Generate offline regression commands for every required local platform adapter.
pub fn commands(root: &Path, host: &str, target: &Path) -> io::Result<Vec<Vec<String>>> {
    // Minimal release-machinery fixtures are deliberately not the Susi product.
    // This is source identification, never a caller-controlled skip flag. A
    // real product missing this core manifest also fails its workspace build.
    if !root.join("crates/susi-core/Cargo.toml").is_file() {
        return Ok(Vec::new());
    }
    for name in [
        "idna-adapter",
        "unicode-ident",
        "htmlescape",
        "regex-syntax",
        "regex-syntax/os-unicode",
    ] {
        if !root.join("vendor").join(name).join("Cargo.toml").is_file() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("mandatory platform adapter manifest missing: {name}"),
            ));
        }
    }
    let path = root.join("vendor/idna-adapter");
    let path = path
        .to_str()
        .ok_or_else(|| io::Error::other("adapter path is not UTF-8"))?
        .replace('\\', "/");
    if path.chars().any(char::is_control) {
        return Err(io::Error::other("adapter path contains control characters"));
    }
    // Each standalone workspace needs this explicit local patch: the root
    // manifest's [patch] does not apply to another workspace. This prevents
    // testing the registry adapter accidentally instead of our OS boundary.
    let patch = format!(
        "patch.crates-io.idna_adapter.path=\"{}\"",
        path.replace('"', "\\\"")
    );
    let mut commands = Vec::new();
    for name in [
        "idna-adapter",
        "unicode-ident",
        "htmlescape",
        "regex-syntax",
        "regex-syntax/os-unicode",
    ] {
        let manifest = format!("vendor/{name}/Cargo.toml");
        // Their generated lockfiles are ignored, private build evidence. All
        // actual dependencies are pinned local source; resolution stays offline.
        commands.push(vec![
            "generate-lockfile".into(),
            "--offline".into(),
            "--manifest-path".into(),
            manifest.clone(),
            "--config".into(),
            patch.clone(),
        ]);
        commands.push(vec![
            "test".into(),
            "--offline".into(),
            "--locked".into(),
            "--manifest-path".into(),
            manifest,
            "--config".into(),
            patch.clone(),
            "--target".into(),
            host.into(),
            "--target-dir".into(),
            target
                .join("platform-adapters")
                .join(name)
                .to_string_lossy()
                .into_owned(),
        ]);
    }
    Ok(commands)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn product_requires_all_adapters_and_tests_exact_local_source_offline() {
        let root = std::env::temp_dir().join(format!(
            "susi-vendor-gates-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("crates/susi-core")).unwrap();
        std::fs::write(root.join("crates/susi-core/Cargo.toml"), "[package]").unwrap();
        let mut gates = vec![
            vec![
                "clippy".into(),
                "--workspace".into(),
                "--".into(),
                "-D".into(),
                "warnings".into(),
            ],
            vec!["test".into(), "--workspace".into()],
            vec!["build".into(), "-p".into(), "susi".into()],
        ];
        include_release_tooling(&root, &mut gates);
        assert_eq!(
            gates[0],
            [
                "clippy",
                "--workspace",
                "--features",
                "xtask/release-tooling",
                "--",
                "-D",
                "warnings"
            ]
        );
        assert_eq!(
            gates[1],
            ["test", "--workspace", "--features", "xtask/release-tooling"]
        );
        assert_eq!(gates[2], ["build", "-p", "susi"]);
        assert!(commands(&root, "x86_64-pc-windows-msvc", &root.join("target")).is_err());
        for name in [
            "idna-adapter",
            "unicode-ident",
            "htmlescape",
            "regex-syntax",
            "regex-syntax/os-unicode",
        ] {
            std::fs::create_dir_all(root.join("vendor").join(name)).unwrap();
            std::fs::write(
                root.join("vendor").join(name).join("Cargo.toml"),
                "[package]",
            )
            .unwrap();
        }
        let commands = commands(&root, "x86_64-pc-windows-msvc", &root.join("target")).unwrap();
        assert_eq!(commands.len(), 10);
        for command in &commands {
            assert!(command.contains(&"--offline".into()));
            assert!(command
                .iter()
                .any(|arg| arg.starts_with("patch.crates-io.idna_adapter.path=")));
        }
        for command in commands.iter().filter(|command| command[0] == "test") {
            assert_eq!(command[0], "test");
            assert!(
                !command.contains(&"--lib".into()),
                "integration contracts must run"
            );
            for arg in ["--locked", "--target", "x86_64-pc-windows-msvc"] {
                assert!(command.contains(&arg.into()));
            }
        }
        assert_ne!(commands[1].last(), commands[3].last());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn non_susi_fixture_skips_release_tooling_and_adapter_commands() {
        let root = std::env::temp_dir().join(format!(
            "susi-vendor-minimal-fixture-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();

        let original = vec![
            vec!["test".into(), "--workspace".into()],
            vec!["clippy".into(), "-p".into(), "xtask".into()],
        ];
        let mut gates = original.clone();
        include_release_tooling(&root, &mut gates);
        assert_eq!(gates, original);
        assert!(
            commands(&root, "x86_64-unknown-linux-gnu", &root.join("target"))
                .unwrap()
                .is_empty()
        );

        std::fs::create_dir_all(root.join("crates/susi-core")).unwrap();
        std::fs::write(root.join("crates/susi-core/Cargo.toml"), "[package]").unwrap();
        include_release_tooling(&root, &mut gates);
        assert_eq!(
            gates[0],
            ["test", "--workspace", "--features", "xtask/release-tooling"]
        );
        assert_eq!(gates[1], ["clippy", "-p", "xtask"]);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn include_release_tooling_splices_before_double_dash_or_at_end_for_workspace_gates() {
        let root = std::env::temp_dir().join(format!(
            "susi-vendor-splice-order-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("crates/susi-core")).unwrap();
        std::fs::write(root.join("crates/susi-core/Cargo.toml"), "[package]").unwrap();

        let mut gates = vec![
            vec![
                "test".into(),
                "--workspace".into(),
                "--".into(),
                "--nocapture".into(),
            ],
            vec![
                "clippy".into(),
                "--workspace".into(),
                "--all-targets".into(),
            ],
            vec!["test".into(), "-p".into(), "susi-core".into(), "--".into()],
            vec!["check".into(), "--workspace".into()],
            Vec::new(),
        ];
        include_release_tooling(&root, &mut gates);

        assert_eq!(
            gates[0],
            [
                "test",
                "--workspace",
                "--features",
                "xtask/release-tooling",
                "--",
                "--nocapture"
            ]
        );
        assert_eq!(
            gates[1],
            [
                "clippy",
                "--workspace",
                "--all-targets",
                "--features",
                "xtask/release-tooling"
            ]
        );
        assert_eq!(gates[2], ["test", "-p", "susi-core", "--"]);
        assert_eq!(gates[3], ["check", "--workspace"]);
        assert!(gates[4].is_empty());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn commands_rejects_missing_each_required_adapter_manifest_and_preserves_manifest_order() {
        let required_adapters = [
            "idna-adapter",
            "unicode-ident",
            "htmlescape",
            "regex-syntax",
            "regex-syntax/os-unicode",
        ];
        let root = std::env::temp_dir().join(format!(
            "susi-vendor-missing-each-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("crates/susi-core")).unwrap();
        std::fs::write(root.join("crates/susi-core/Cargo.toml"), "[package]").unwrap();

        for name in required_adapters {
            std::fs::create_dir_all(root.join("vendor").join(name)).unwrap();
            std::fs::write(
                root.join("vendor").join(name).join("Cargo.toml"),
                "[package]",
            )
            .unwrap();
        }

        for name in required_adapters {
            let manifest_path = root.join("vendor").join(name).join("Cargo.toml");
            std::fs::remove_file(&manifest_path).unwrap();
            let err = commands(&root, "x86_64-pc-windows-msvc", &root.join("target")).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::NotFound);
            assert!(err.to_string().contains(name));
            std::fs::write(&manifest_path, "[package]").unwrap();
        }

        let cmds = commands(&root, "x86_64-pc-windows-msvc", &root.join("target")).unwrap();
        assert_eq!(cmds.len(), required_adapters.len() * 2);
        for (pair, name) in cmds.as_chunks::<2>().0.iter().zip(required_adapters) {
            let expected_manifest = format!("vendor/{name}/Cargo.toml");
            assert_eq!(pair[0][0], "generate-lockfile");
            assert_eq!(pair[0][3], expected_manifest);
            assert_eq!(pair[1][0], "test");
            assert_eq!(pair[1][4], expected_manifest);
        }

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn include_release_tooling_is_noop_on_empty_gates_slice() {
        let mut gates: Vec<Vec<String>> = Vec::new();
        include_release_tooling(Path::new("."), &mut gates);
        assert!(gates.is_empty());
    }
}
