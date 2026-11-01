//! The linked standard library is shipped software, outside Cargo's graph.
//! Until the audited custom-sysroot builder has a verified receipt contract,
//! no stock or self-asserted custom sysroot is eligible for production.
use std::{fs, path::Path, process::Command};

pub(crate) fn qualify(
    candidate: &Path,
    attempt: &Path,
    target: &str,
    compiler: &str,
) -> super::Result<()> {
    let mut command = Command::new("rustc");
    command.current_dir(candidate).args(["--print", "sysroot"]);
    super::compiler_env::apply(&mut command);
    let output = command.output()?;
    if !output.status.success() {
        return Err("cannot identify selected standard library for license qualification".into());
    }
    let root = String::from_utf8(output.stdout)?;
    let root = Path::new(root.trim());
    let notices = root.join("share/doc/rust/COPYRIGHT-library.html");
    let notice = fs::read(&notices)?;
    let library = root.join("lib/rustlib").join(target).join("lib");
    let mut artifacts = Vec::new();
    for entry in fs::read_dir(&library)? {
        let path = entry?.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "rlib")
        {
            artifacts.push(serde_json::json!({"file": path.file_name().and_then(|name| name.to_str()), "sha256": super::file_digest(&path)?}));
        }
    }
    artifacts.sort_by_key(|entry| entry["file"].as_str().unwrap_or_default().to_owned());
    if artifacts.is_empty() {
        return Err("selected target standard-library archives are absent".into());
    }
    let evidence = serde_json::json!({
        "schema": "susi/sysroot-license-diagnostic/v1",
        "qualified": false,
        "compiler": compiler,
        "compiler_sha256": super::digest(compiler.as_bytes()),
        "target": target,
        "sysroot": root,
        "copyright_path": notices,
        "copyright_sha256": super::digest(&notice),
        "copyright_text": String::from_utf8_lossy(&notice),
        "target_archives": artifacts,
        "reason": rejection_reason(),
    });
    fs::write(
        attempt.join("sysroot-license-diagnostic.json"),
        serde_json::to_vec_pretty(&evidence)?,
    )?;
    Err(rejection_reason().into())
}

fn rejection_reason() -> &'static str {
    "standard-library license qualification is unavailable: stock Rust includes Unicode-3.0 material; a verified custom-sysroot source/build/artifact provenance contract is required. Compiler identity, Cargo license results and editable license receipts do not authorize production"
}

#[cfg(test)]
mod tests {
    #[test]
    fn installed_sysroot_is_recorded_but_never_self_authorizes() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let attempt =
            std::env::temp_dir().join(format!("susi-sysroot-gate-{}", std::process::id()));
        std::fs::create_dir_all(&attempt).unwrap();
        let (target, compiler) = super::super::compiler(root).unwrap();
        let error = super::qualify(root, &attempt, &target, &compiler).unwrap_err();
        assert!(error.to_string().contains("verified custom-sysroot"));
        let path = attempt.join("sysroot-license-diagnostic.json");
        let evidence: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(evidence["qualified"], false);
        assert_eq!(evidence["compiler"], compiler);
        assert!(!evidence["target_archives"].as_array().unwrap().is_empty());
        assert_eq!(evidence["copyright_sha256"].as_str().unwrap().len(), 64);
        // Even a caller's assertion cannot turn the diagnostic into admission.
        std::fs::write(
            attempt.join("sysroot-license-receipt.json"),
            br#"{"license_expression":"MIT","qualified":true}"#,
        )
        .unwrap();
        assert!(super::qualify(root, &attempt, &target, &compiler).is_err());
        std::fs::remove_file(path).unwrap();
        std::fs::remove_file(attempt.join("sysroot-license-receipt.json")).unwrap();
        std::fs::remove_dir(attempt).unwrap();
    }
}
