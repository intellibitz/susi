//! Local-only, transactional source-version preparation after a promoted release.
use super::{
    checked_package, clean, git, integration_receipt, isolated_command, measured_health, resolve,
    stamp, Context, Lock, Manifest, Result, CURRENT, HEALTH_BUDGET,
};
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::Command;

const MANIFESTS: [(&str, &str); 7] = [
    ("Cargo.toml", "susi"),
    ("crates/susi-gawd/Cargo.toml", "susi-gawd"),
    ("crates/susi-gemi/Cargo.toml", "susi-gemi"),
    ("crates/susi-gmcp/Cargo.toml", "susi-gmcp"),
    ("crates/susi-daemon/Cargo.toml", "susi-daemon"),
    ("crates/susi-server/Cargo.toml", "susi-server"),
    ("crates/susi-harness/Cargo.toml", "susi-harness"),
];
const JSON: [(&str, &str); 3] = [
    (".agents/identity.json", "susi/identity/v1"),
    (".agents/roadmap.json", "susi/roadmap/v1"),
    (".agents/evidence.json", "susi/evidence/v1"),
];

fn package_value(text: &str, key: &str) -> Result<String> {
    let mut package = false;
    let mut values = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            package = line == "[package]";
        }
        if package {
            if let Some(value) = line.strip_prefix(&format!("{key} = \"")) {
                values.push(
                    value
                        .strip_suffix('"')
                        .ok_or("invalid package field")?
                        .to_owned(),
                );
            }
        }
    }
    if values.len() != 1 {
        return Err(format!("expected one [package] {key}").into());
    }
    Ok(values.remove(0))
}

fn next_patch(version: &str) -> Result<String> {
    let parts = version
        .split('.')
        .map(|part| {
            if part.is_empty()
                || (part.len() > 1 && part.starts_with('0'))
                || !part.bytes().all(|b| b.is_ascii_digit())
            {
                return Err("version must be canonical major.minor.patch".into());
            }
            Ok(part.parse::<u64>()?)
        })
        .collect::<Result<Vec<_>>>()?;
    if parts.len() != 3 {
        return Err("version must have three components".into());
    }
    let patch = parts[2].checked_add(1).ok_or("patch version overflow")?;
    Ok(format!("{}.{}.{patch}", parts[0], parts[1]))
}

type FileChange = (String, Vec<u8>, Vec<u8>);
type VersionPlan = (String, String, Vec<FileChange>);

fn version_input(root: &Path, relative: &str) -> Result<Vec<u8>> {
    let mut path = root.to_path_buf();
    for part in Path::new(relative).components() {
        path.push(part);
        let metadata = fs::symlink_metadata(&path)?;
        let redirected = metadata.file_type().is_symlink();
        #[cfg(windows)]
        let redirected = {
            use std::os::windows::fs::MetadataExt;
            redirected || metadata.file_attributes() & 0x400 != 0
        };
        if redirected {
            return Err(format!("redirected version input: {relative}").into());
        }
    }
    Ok(fs::read(path)?)
}

fn plan(root: &Path) -> Result<VersionPlan> {
    let old = package_value(
        std::str::from_utf8(&version_input(root, "Cargo.toml")?)?,
        "version",
    )?;
    let next = next_patch(&old)?;
    let mut files = Vec::new();
    for (path, name) in MANIFESTS {
        let bytes = version_input(root, path)?;
        let text = std::str::from_utf8(&bytes)?;
        if package_value(text, "name")? != name || package_value(text, "version")? != old {
            return Err(format!("engine manifest is inconsistent: {path}").into());
        }
        let mut package = false;
        let mut new = String::new();
        for line in text.split_inclusive('\n') {
            let trimmed = line.trim();
            if trimmed.starts_with('[') {
                package = trimmed == "[package]";
            }
            if package && trimmed.starts_with("version = \"") {
                new.push_str(&line.replacen(&format!("\"{old}\""), &format!("\"{next}\""), 1));
            } else {
                new.push_str(line);
            }
        }
        files.push((path.into(), bytes, new.into_bytes()));
    }
    for (path, schema) in JSON {
        let bytes = version_input(root, path)?;
        let new = governance_version(&bytes, schema, &old, &next)?;
        files.push((path.into(), bytes, new));
    }
    let bytes = version_input(root, "README.md")?;
    let text = std::str::from_utf8(&bytes)?;
    let badge = format!("https://img.shields.io/badge/version-v{old}-blue.svg");
    if text.matches(&badge).count() != 1 {
        return Err("expected one current version badge".into());
    }
    let new = text
        .replacen(
            &badge,
            &format!("https://img.shields.io/badge/version-v{next}-blue.svg"),
            1,
        )
        .into_bytes();
    files.push(("README.md".into(), bytes, new));
    let lock = version_input(root, "Cargo.lock")?;
    let updated = next_lock(&lock, &old, &next)?;
    files.push(("Cargo.lock".into(), lock, updated));
    Ok((old, next, files))
}

/// Use native JSON token offsets so the edit preserves all other bytes, including
/// whitespace, escaping, key order, and historical versions with the same value.
fn governance_version(bytes: &[u8], schema: &str, old: &str, next: &str) -> Result<Vec<u8>> {
    let doc: serde_json::Value = serde_json::from_slice(bytes)?;
    if doc["schema"] != schema || doc["version"] != old {
        return Err("governance version/schema mismatch".into());
    }
    let mut rest = bytes
        .trim_ascii_start()
        .strip_prefix(b"{")
        .ok_or("expected governance object")?;
    let mut span = None;
    loop {
        rest = rest.trim_ascii_start();
        if rest.starts_with(b"}") {
            break;
        }
        let mut keys = serde_json::Deserializer::from_slice(rest).into_iter::<String>();
        let key = keys.next().transpose()?.ok_or("missing governance key")?;
        rest = rest[keys.byte_offset()..]
            .trim_ascii_start()
            .strip_prefix(b":")
            .ok_or("missing field separator")?
            .trim_ascii_start();
        let mut values =
            serde_json::Deserializer::from_slice(rest).into_iter::<serde_json::Value>();
        let value = values
            .next()
            .transpose()?
            .ok_or("missing governance value")?;
        let length = values.byte_offset();
        if key == "version" {
            if value != old || span.is_some() {
                return Err("ambiguous governance version".into());
            }
            let start = bytes.len() - rest.len();
            span = Some(start..start + length);
        }
        rest = rest[length..].trim_ascii_start();
        if let Some(fields) = rest.strip_prefix(b",") {
            rest = fields;
        } else {
            break;
        }
    }
    let span = span.ok_or("missing top-level governance version")?;
    let mut result = bytes.to_vec();
    result.splice(span, serde_json::to_vec(next)?);
    Ok(result)
}

/// Change exactly the version field of each of the seven engine packages.
fn next_lock(before: &[u8], old: &str, next: &str) -> Result<Vec<u8>> {
    let text = std::str::from_utf8(before)?;
    let mut blocks = text.split("[[package]]");
    let mut updated = blocks.next().unwrap_or_default().to_owned();
    let mut seen = std::collections::BTreeSet::new();
    for block in blocks {
        updated.push_str("[[package]]");
        let name = block.lines().find_map(|line| {
            line.strip_prefix("name = \"")
                .and_then(|n| n.strip_suffix('"'))
        });
        if let Some(engine) = name.filter(|name| MANIFESTS.iter().any(|(_, engine)| engine == name))
        {
            let version = format!("version = \"{old}\"");
            if !seen.insert(engine)
                || block.lines().filter(|line| *line == version).count() != 1
                || block.lines().any(|line| line.starts_with("source = "))
            {
                return Err(format!("engine lock entry mismatch: {engine}").into());
            }
            for line in block.split_inclusive('\n') {
                if line.trim_end_matches(['\r', '\n']) == version {
                    updated.push_str(&line.replacen(&version, &format!("version = \"{next}\""), 1));
                } else {
                    updated.push_str(line);
                }
            }
        } else {
            updated.push_str(block);
        }
    }
    if seen.len() != MANIFESTS.len() {
        return Err("engine lock entries missing".into());
    }
    Ok(updated.into_bytes())
}

fn apply<F>(root: &Path, journal: &Path, update: F) -> Result<String>
where
    F: FnOnce(&Path) -> Result<()>,
{
    let (_, next, files) = plan(root)?;
    fs::create_dir(journal)?;
    for (index, (path, bytes, _)) in files.iter().enumerate() {
        for (name, content) in [
            (format!("{index}.original"), bytes.as_slice()),
            (format!("{index}.path"), path.as_bytes()),
        ] {
            let mut file = fs::File::create(journal.join(name))?;
            file.write_all(content)?;
            file.sync_all()?;
        }
    }
    fs::write(journal.join("state"), "preparing\n")?;
    let result = (|| -> Result<()> {
        for (path, _, bytes) in &files {
            fs::write(root.join(path), bytes)?;
        }
        update(root)?;
        for (path, _, expected) in &files {
            if fs::read(root.join(path))? != *expected {
                return Err(
                    format!("version preparation changed additional contents of {path}").into(),
                );
            }
        }
        fs::write(
            journal.join("state"),
            "prepared; source changes need commit, integration and qualification\n",
        )?;
        Ok(())
    })();
    if let Err(error) = result {
        let mut failed = Vec::new();
        for (path, bytes, _) in &files {
            if fs::write(root.join(path), bytes).is_err() {
                failed.push(path.clone());
            }
        }
        let _ = fs::write(
            journal.join("state"),
            "failed; original snapshots retained\n",
        );
        if !failed.is_empty() {
            return Err(format!(
                "{error}; restore failed for {failed:?}; journal {}",
                journal.display()
            )
            .into());
        }
        return Err(error);
    }
    Ok(next)
}

fn scope_covers_version_path(path: &str, scope: &str) -> bool {
    // The coordinator stores ASCII lowercase scopes with forward slashes on
    // every platform. Normalize the planned path identically; retain the slash
    // boundary so a directory claim never owns a similarly named sibling.
    let path = path.replace('\\', "/").to_ascii_lowercase();
    path == scope
        || path
            .strip_prefix(scope)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn verify_version_base(root: &Path, promoted: &str, head: &str, old: &str) -> Result<()> {
    if git(root, &["merge-base", promoted, head])? != promoted {
        return Err("version preparation requires a descendant of the promoted source".into());
    }
    let read = |path: &str| git(root, &["show", &format!("{promoted}:{path}")]);
    for (path, name) in MANIFESTS {
        let manifest = read(path)?;
        if package_value(&manifest, "name")? != name || package_value(&manifest, "version")? != old
        {
            return Err(format!(
                "accepted engine version differs from its promoted predecessor: {path}"
            )
            .into());
        }
    }
    let next = next_patch(old)?;
    for (path, schema) in JSON {
        governance_version(read(path)?.as_bytes(), schema, old, &next)?;
    }
    if read("README.md")?
        .matches(&format!(
            "https://img.shields.io/badge/version-v{old}-blue.svg"
        ))
        .count()
        != 1
    {
        return Err("promoted version badge does not match the accepted source".into());
    }
    next_lock(read("Cargo.lock")?.as_bytes(), old, &next)?;
    Ok(())
}

fn version_metadata(root: &Path, home: &Path, host: &str) -> Result<Command> {
    // Use the release policy's authenticated compiler selection to address
    // Cargo by its physical executable as well, independently of PATH or cwd.
    let selected = isolated_command(Path::new("cargo"), home)?;
    let rustc = selected
        .get_envs()
        .find_map(|(key, value)| (key == "RUSTC").then_some(value).flatten())
        .ok_or("version preparation requires a verified physical compiler")?;
    let cargo = Path::new(rustc).with_file_name(format!("cargo{}", std::env::consts::EXE_SUFFIX));
    if !cargo.is_absolute() {
        return Err("version preparation requires an absolute Cargo executable".into());
    }
    let mut command = isolated_command(&cargo, home)?;
    let target = home.join("target");
    command
        .current_dir(root)
        .env("CARGO_TARGET_DIR", &target)
        .env("CARGO_BUILD_TARGET_DIR", &target)
        .env("CARGO_BUILD_BUILD_DIR", &target)
        .args([
            "--config",
            "build.rustc-wrapper=''",
            "--config",
            "build.rustc-workspace-wrapper=''",
            "metadata",
            "--offline",
            "--locked",
            "--format-version",
            "1",
            "--filter-platform",
            host,
        ]);
    Ok(command)
}

pub(super) fn run(ctx: &Context) -> Result<()> {
    let _release = Lock::acquire(&ctx.state)?;
    clean(&ctx.root)?;
    let common = ctx.state.parent().ok_or("missing common Git directory")?;
    let workflow = common.join("susi-local-workflow");
    let _workflow = Lock::acquire(&workflow)?;
    let branch = git(&ctx.root, &["branch", "--show-current"])?;
    let claimant = branch
        .strip_prefix("local-agent/")
        .filter(|name| {
            !name.is_empty()
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        })
        .ok_or("prepare-next requires a claimed isolated local-agent branch")?;
    let claim_path = workflow.join("claims").join(format!("{claimant}.claim"));
    if fs::symlink_metadata(&claim_path)?.file_type().is_symlink() {
        return Err("version preparation claim must not be redirected".into());
    }
    let claim = fs::read_to_string(claim_path)?;
    let mut lines = claim.lines();
    if lines.next() != Some("susi-local-claim-v2")
        || lines.next() != Some(claimant)
        || Path::new(lines.next().ok_or("claim worktree missing")?).canonicalize()?
            != ctx.root.canonicalize()?
    {
        return Err("version preparation claim does not match this worktree".into());
    }
    let base = lines.next().ok_or("claim base missing")?;
    let head = git(&ctx.root, &["rev-parse", "HEAD"])?;
    if base != head {
        return Err("prepare-next requires an unchanged claim head".into());
    }
    let scopes: Vec<_> = lines.collect();
    let (old, _, files) = plan(&ctx.root)?;
    if files.iter().any(|(path, _, _)| {
        !scopes
            .iter()
            .any(|scope| scope_covers_version_path(path, scope))
    }) {
        return Err(
            "version claim must own all engine manifests, governance versions, README and lockfile"
                .into(),
        );
    }
    let current = resolve(&ctx.root, CURRENT)?
        .ok_or("prepare-next requires a genuinely qualified and promoted release A")?;
    let (_, binary) = checked_package(ctx, &current)?;
    let manifest = Manifest::decode(&(git(&ctx.root, &["cat-file", "blob", &current])? + "\n"))?;
    let tree = git(&ctx.root, &["rev-parse", "HEAD^{tree}"])?;
    integration_receipt(ctx, &head, &tree)?;
    verify_version_base(&ctx.root, &manifest.source, &head, &old)?;
    let attempt = ctx.state.join(format!("prepare-next-{}", stamp()?));
    let health = ctx.state.join(format!("prepare-health-{}", stamp()?));
    fs::create_dir(&health)?;
    let mut command = isolated_command(&binary, &health.join("home"))?;
    command.arg("--version");
    measured_health(command, &health, "version", HEALTH_BUDGET, b"")?;
    if !fs::read_to_string(health.join("version.stdout"))?
        .split_whitespace()
        .any(|word| word == old)
    {
        return Err("promoted artifact does not report its source version".into());
    }
    let version = apply(&ctx.root, &attempt, |root| {
        let status = version_metadata(root, &attempt.join("metadata-home"), &manifest.host)?
            .stdout(std::process::Stdio::null())
            .status()?;
        if !status.success() {
            return Err("offline verification of the exact versioned workspace lock failed".into());
        }
        Ok(())
    })?;
    println!("Prepared {version} locally; journal {}. Commit and integrate before qualification; production A is unchanged.",attempt.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture() -> PathBuf {
        let root = std::env::temp_dir().join(format!("susi-version-{}", stamp().unwrap()));
        fs::create_dir(&root).unwrap();
        for (relative, name) in MANIFESTS
            .into_iter()
            .chain([("crates/independent/Cargo.toml", "independent")])
        {
            let directory = root.join(relative).parent().unwrap().to_path_buf();
            fs::create_dir_all(directory.join("src")).unwrap();
            fs::write(
                directory.join("src/lib.rs"),
                "// version transaction fixture\n",
            )
            .unwrap();
            let version = if name == "independent" {
                "0.1.0"
            } else {
                "0.21.38"
            };
            fs::write(
                root.join(relative),
                format!(
                    "[package]\nname = \"{name}\"\nversion = \"{version}\"\nedition = \"2021\"\n"
                ),
            )
            .unwrap();
        }
        let mut manifest = fs::read_to_string(root.join("Cargo.toml")).unwrap();
        manifest.push_str("\n[workspace]\nmembers = [\"crates/*\"]\nresolver = \"2\"\n");
        fs::write(root.join("Cargo.toml"), manifest).unwrap();
        fs::create_dir(root.join(".agents")).unwrap();
        for (path, schema) in JSON {
            fs::write(root.join(path),serde_json::json!({"schema":schema,"version":"0.21.38","target_version":"0.15.0","history":[{"version":"0.20.0"}]}).to_string()).unwrap();
        }
        fs::write(root.join("README.md"),"![Version](https://img.shields.io/badge/version-v0.21.38-blue.svg)\nHistorical v0.21.38\n").unwrap();
        fs::write(root.join(".gitignore"), "/target/\n").unwrap();
        assert!(Command::new("cargo")
            .current_dir(&root)
            .args(["generate-lockfile", "--offline"])
            .status()
            .unwrap()
            .success());
        root
    }

    #[test]
    fn offline_patch_updates_all_engine_surfaces_preserving_history_and_independent_crates() {
        let root = fixture();
        let independent = fs::read(root.join("crates/independent/Cargo.toml")).unwrap();
        let journal = root
            .parent()
            .unwrap()
            .join(format!("version-journal-{}", stamp().unwrap()));
        let next = apply(&root, &journal, |root| {
            if !Command::new("cargo")
                .current_dir(root)
                .args(["update", "--workspace", "--offline"])
                .status()?
                .success()
            {
                return Err("fixture cargo update failed".into());
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(next, "0.21.39");
        for (path, _) in MANIFESTS {
            assert_eq!(
                package_value(&fs::read_to_string(root.join(path)).unwrap(), "version").unwrap(),
                next
            );
        }
        for (path, _) in JSON {
            let doc: serde_json::Value =
                serde_json::from_slice(&fs::read(root.join(path)).unwrap()).unwrap();
            assert_eq!(doc["version"], next);
            assert_eq!(doc["history"][0]["version"], "0.20.0");
            assert_eq!(doc["target_version"], "0.15.0");
        }
        assert_eq!(
            fs::read(root.join("crates/independent/Cargo.toml")).unwrap(),
            independent
        );
        assert!(fs::read_to_string(root.join("README.md"))
            .unwrap()
            .contains("Historical v0.21.38"));
    }

    #[test]
    fn lock_failure_restores_every_original_byte_and_keeps_recovery_snapshots() {
        let root = fixture();
        let (_, _, before) = plan(&root).unwrap();
        let journal = root
            .parent()
            .unwrap()
            .join(format!("version-journal-{}", stamp().unwrap()));
        assert!(apply(&root, &journal, |root| {
            fs::write(root.join("Cargo.lock"), "partial failed cargo write")?;
            Err("injected lock update failure".into())
        })
        .is_err());
        for (index, (path, bytes, _)) in before.iter().enumerate() {
            assert_eq!(&fs::read(root.join(path)).unwrap(), bytes);
            assert_eq!(
                &fs::read(journal.join(format!("{index}.original"))).unwrap(),
                bytes
            );
        }
        assert!(next_patch("0.1.18446744073709551615").is_err());
        assert!(next_patch("00.1.2").is_err());
        fs::write(
            root.join("crates/susi-gawd/Cargo.toml"),
            "[package]\nname = \"susi-gawd\"\nversion = \"0.21.38\"\nversion = \"0.21.38\"\n",
        )
        .unwrap();
        assert!(plan(&root).is_err());
    }

    #[test]
    fn coordinator_normalized_scopes_preserve_version_path_boundaries() {
        assert!(scope_covers_version_path("Cargo.toml", "cargo.toml"));
        assert!(scope_covers_version_path("README.md", "readme.md"));
        assert!(scope_covers_version_path(
            "crates\\susi-gawd\\Cargo.toml",
            "crates/susi-gawd"
        ));
        assert!(!scope_covers_version_path(
            "crates/susi-gawd-extra/Cargo.toml",
            "crates/susi-gawd"
        ));
        assert!(!scope_covers_version_path("Cargo.toml", "cargo"));
        assert!(!scope_covers_version_path("Cargo.toml", "../cargo.toml"));
        assert!(!scope_covers_version_path("Cargo.toml", ""));
    }

    #[test]
    fn version_metadata_uses_verified_physical_cargo_and_compiler_under_hostile_config() {
        let root = fixture();
        let home = root.join("isolated-metadata");
        let mut rustc = isolated_command(Path::new("rustc"), &home).unwrap();
        let version = rustc.arg("-vV").output().unwrap();
        assert!(version.status.success());
        let text = String::from_utf8(version.stdout).unwrap();
        let host = text
            .lines()
            .find_map(|line| line.strip_prefix("host: "))
            .unwrap();
        fs::create_dir(root.join(".cargo")).unwrap();
        fs::write(root.join(".cargo/config.toml"),"[build]\nrustc = 'missing-version-compiler'\nrustc-wrapper = 'missing-version-wrapper'\nrustc-workspace-wrapper = 'missing-version-wrapper'\n").unwrap();
        let mut command = version_metadata(&root, &home, host).unwrap();
        let physical = Path::new(command.get_program());
        let compiler = command
            .get_envs()
            .find_map(|(key, value)| (key == "RUSTC").then_some(value).flatten())
            .unwrap();
        assert!(physical.is_absolute());
        assert_eq!(physical.parent(), Path::new(compiler).parent());
        let control = Command::new(physical)
            .current_dir(&root)
            .env_remove("RUSTC")
            .env_remove("RUSTC_WRAPPER")
            .env_remove("RUSTC_WORKSPACE_WRAPPER")
            .env_remove("CARGO_BUILD_RUSTC")
            .args([
                "metadata",
                "--offline",
                "--locked",
                "--format-version",
                "1",
                "--filter-platform",
                host,
            ])
            .output()
            .unwrap();
        assert!(!control.status.success());
        assert!(String::from_utf8_lossy(&control.stderr).contains("missing-version-"));
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let graph: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(graph["workspace_members"].as_array().unwrap().len(), 8);
    }

    #[test]
    fn governance_edit_preserves_formatting_unknown_fields_and_historical_versions() {
        let before=br#"{ "history" : [{"version":"0.21.38"}],  "schema":"susi/test", "ver\u0073ion" : "0\u002e21.38", "note" : "0.21.38" }
"#;
        let updated = governance_version(before, "susi/test", "0.21.38", "0.21.39").unwrap();
        let expected = std::str::from_utf8(before)
            .unwrap()
            .replace(r#""0\u002e21.38""#, r#""0.21.39""#);
        assert_eq!(updated, expected.as_bytes());
        assert!(governance_version(
            br#"{"schema":"susi/test","version":"0.21.38","version":"0.21.38"}"#,
            "susi/test",
            "0.21.38",
            "0.21.39"
        )
        .is_err());
    }

    #[test]
    fn version_preparation_preserves_accepted_changes_and_refuses_version_drift() {
        let root = fixture();
        git(&root, &["init", "-q"]).unwrap();
        git(&root, &["config", "user.name", "Fixture"]).unwrap();
        git(&root, &["config", "user.email", "fixture@example.invalid"]).unwrap();
        let commit = || {
            git(&root, &["add", "."]).unwrap();
            git(
                &root,
                &[
                    "-c",
                    "core.hooksPath=",
                    "commit",
                    "--no-gpg-sign",
                    "-qm",
                    "fixture",
                ],
            )
            .unwrap();
            git(&root, &["rev-parse", "HEAD"]).unwrap()
        };
        let (old, _, _) = plan(&root).unwrap();
        let promoted = commit();
        assert!(verify_version_base(&root, &promoted, &promoted, &old).is_ok());
        fs::write(root.join("src/lib.rs"), "// unrelated accepted change\n").unwrap();
        let head = commit();
        assert!(verify_version_base(&root, &promoted, &head, &old).is_ok());
        assert!(verify_version_base(&root, &head, &promoted, &old).is_err());
        let lock = root.join("Cargo.lock");
        let accepted = format!(
            "# accepted dependency provenance\n{}",
            fs::read_to_string(&lock).unwrap()
        );
        fs::write(&lock, &accepted).unwrap();
        let changed = commit();
        assert!(verify_version_base(&root, &promoted, &changed, &old).is_ok());
        assert!(verify_version_base(&root, &promoted, &changed, "0.21.99").is_err());
        let journal = root
            .parent()
            .unwrap()
            .join(format!("version-lock-{}", stamp().unwrap()));
        assert!(apply(&root, &journal, |_| {
            let text = fs::read_to_string(&lock)?;
            fs::write(
                &lock,
                text.replacen(
                    "name = \"susi\"",
                    "name = \"susi\"\ndependencies = [\"injected\"]",
                    1,
                ),
            )?;
            Ok(())
        })
        .is_err());
        assert_eq!(fs::read_to_string(lock).unwrap(), accepted);
    }

    #[test]
    fn unqualified_current_missing_scope_and_dirty_worktree_cannot_prepare_versions() {
        let root = fixture();
        git(&root, &["init", "-q", "-b", "local-agent/version-fixture"]).unwrap();
        git(&root, &["add", "."]).unwrap();
        git(
            &root,
            &[
                "-c",
                "core.hooksPath=",
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-qm",
                "fixture",
            ],
        )
        .unwrap();
        let ctx = Context::open(&root).unwrap();
        let workflow = ctx.state.parent().unwrap().join("susi-local-workflow");
        fs::create_dir_all(workflow.join("claims")).unwrap();
        let head = git(&root, &["rev-parse", "HEAD"]).unwrap();
        let (_, _, files) = plan(&root).unwrap();
        let scopes = files
            .iter()
            .map(|(path, _, _)| path.replace('\\', "/").to_ascii_lowercase())
            .collect::<Vec<_>>()
            .join("\n");
        let claim = workflow.join("claims/version-fixture.claim");
        let prefix = format!(
            "susi-local-claim-v2\nversion-fixture\n{}\n{head}\n",
            root.display()
        );
        fs::write(&claim, format!("{prefix}{scopes}\n")).unwrap();
        assert!(run(&ctx)
            .unwrap_err()
            .to_string()
            .contains("qualified and promoted"));
        fs::write(&claim, format!("{prefix}xtask\n")).unwrap();
        assert!(run(&ctx)
            .unwrap_err()
            .to_string()
            .contains("claim must own"));
        fs::write(root.join("README.md"), "owned uncommitted work").unwrap();
        assert!(run(&ctx).is_err());
        assert_eq!(
            fs::read_to_string(root.join("README.md")).unwrap(),
            "owned uncommitted work"
        );
    }
}
