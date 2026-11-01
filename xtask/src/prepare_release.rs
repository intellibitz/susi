//! Local-only, transactional source-version preparation after a promoted release.
use super::{
    checked_package, clean, git, isolated_command, measured_health, resolve, stamp, Context, Lock,
    Manifest, Result, CURRENT, HEALTH_BUDGET,
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

fn plan(root: &Path) -> Result<VersionPlan> {
    let old = package_value(&fs::read_to_string(root.join("Cargo.toml"))?, "version")?;
    let next = next_patch(&old)?;
    let mut files = Vec::new();
    for (path, name) in MANIFESTS {
        let bytes = fs::read(root.join(path))?;
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
        let bytes = fs::read(root.join(path))?;
        let mut json: serde_json::Value = serde_json::from_slice(&bytes)?;
        if json["schema"] != schema || json["version"] != old {
            return Err(format!("governance version/schema mismatch: {path}").into());
        }
        json["version"] = next.clone().into();
        let new = (serde_json::to_string_pretty(&json)? + "\n").into_bytes();
        files.push((path.into(), bytes, new));
    }
    let bytes = fs::read(root.join("README.md"))?;
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
    let lock = fs::read(root.join("Cargo.lock"))?;
    files.push(("Cargo.lock".into(), lock.clone(), lock));
    Ok((old, next, files))
}

fn lock_invariants(before: &[u8], after: &[u8], old: &str, next: &str) -> Result<()> {
    let old_text = std::str::from_utf8(before)?;
    let new_text = std::str::from_utf8(after)?;
    let field = |block: &str, key: &str| -> Option<String> {
        block
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{key} = \"")))
            .and_then(|value| value.strip_suffix('"'))
            .map(str::to_owned)
    };
    let mut remaining_before = Vec::new();
    let mut remaining_after = Vec::new();
    for (text, expected, remaining) in [
        (old_text, old, &mut remaining_before),
        (new_text, next, &mut remaining_after),
    ] {
        let blocks: Vec<_> = text.split("[[package]]").skip(1).collect();
        for (_, name) in MANIFESTS {
            let matches: Vec<_> = blocks
                .iter()
                .filter(|block| field(block, "name").as_deref() == Some(name))
                .collect();
            if matches.len() != 1
                || field(matches[0], "version").as_deref() != Some(expected)
                || field(matches[0], "source").is_some()
            {
                return Err(format!("engine lock entry mismatch: {name}").into());
            }
        }
        remaining.extend(
            blocks
                .into_iter()
                .filter(|block| {
                    !MANIFESTS
                        .iter()
                        .any(|(_, name)| field(block, "name").as_deref() == Some(name))
                })
                .map(|block| block.trim().to_owned()),
        );
        remaining.sort();
    }
    if remaining_before != remaining_after {
        return Err("version preparation changed non-engine lock entries".into());
    }
    Ok(())
}

fn apply<F>(root: &Path, journal: &Path, update: F) -> Result<String>
where
    F: FnOnce(&Path) -> Result<()>,
{
    let (old, next, files) = plan(root)?;
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
        let lock = files
            .iter()
            .find(|(path, _, _)| path == "Cargo.lock")
            .ok_or("lock snapshot missing")?;
        lock_invariants(&lock.1, &fs::read(root.join("Cargo.lock"))?, &old, &next)?;
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
        return Err("prepare-next requires a fresh claim at the promoted source head".into());
    }
    let scopes: Vec<_> = lines.collect();
    let (_, _, files) = plan(&ctx.root)?;
    if files.iter().any(|(path, _, _)| {
        !scopes.iter().any(|scope| {
            path == scope
                || path
                    .strip_prefix(scope)
                    .is_some_and(|rest| rest.starts_with('/'))
        })
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
    if manifest.source != head {
        return Err("worktree HEAD is not the promoted release source".into());
    }
    let attempt = ctx.state.join(format!("prepare-next-{}", stamp()?));
    let (old, _, _) = plan(&ctx.root)?;
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
        let status = Command::new("cargo")
            .current_dir(root)
            .args(["update", "--workspace", "--offline"])
            .status()?;
        if !status.success() {
            return Err("offline workspace lock update failed".into());
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
            .map(|(path, _, _)| path.as_str())
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
