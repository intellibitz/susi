//! Regression gates for standalone platform adapters excluded from the workspace.
use std::io;
use std::path::Path;

pub(crate) fn commands(root: &Path, host: &str, target: &Path) -> io::Result<Vec<Vec<String>>> {
    // Minimal release-machinery fixtures are deliberately not the Susi product.
    // This is source identification, never a caller-controlled skip flag. A
    // real product missing this core manifest also fails its workspace build.
    if !root.join("crates/susi-core/Cargo.toml").is_file() {
        return Ok(Vec::new());
    }
    for name in ["idna-adapter", "unicode-ident", "htmlescape"] {
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
    for name in ["idna-adapter", "unicode-ident", "htmlescape"] {
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
        assert!(commands(&root, "x86_64-pc-windows-msvc", &root.join("target")).is_err());
        for name in ["idna-adapter", "unicode-ident", "htmlescape"] {
            std::fs::create_dir_all(root.join("vendor").join(name)).unwrap();
            std::fs::write(
                root.join("vendor").join(name).join("Cargo.toml"),
                "[package]",
            )
            .unwrap();
        }
        let commands = commands(&root, "x86_64-pc-windows-msvc", &root.join("target")).unwrap();
        assert_eq!(commands.len(), 6);
        for command in &commands {
            assert!(command.contains(&"--offline".into()));
            assert!(command
                .iter()
                .any(|arg| arg.starts_with("patch.crates-io.idna_adapter.path=")));
        }
        for command in [&commands[1], &commands[3], &commands[5]] {
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
    }
}
