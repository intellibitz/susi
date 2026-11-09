//! Export qualified packages and deploy the already-promoted release to the user's
//! ordinary Windows entry points. The portable target never needs this Git checkout.
use super::{
    checked_package, digest, file_digest, git, resolve, stamp, Context, Lock, Manifest, Result,
    CURRENT, PREVIOUS,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn qualified(ctx: &Context, id: &str) -> Result<(String, PathBuf)> {
    if id.len() != 64
        || !id
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err("expected a lowercase release SHA-256".into());
    }
    let blob = resolve(&ctx.root, &format!("refs/susi/releases/qualified/{id}"))?
        .ok_or("release has not qualified")?;
    let (verified, exe) = checked_package(ctx, &blob)?;
    if verified != id {
        return Err("qualification reference and requested ID differ".into());
    }
    Ok((blob, exe))
}

fn copy_verified(ctx: &Context, blob: &str, stage: &Path) -> Result<String> {
    let (id, exe) = checked_package(ctx, blob)?;
    let raw = git(&ctx.root, &["cat-file", "blob", blob])? + "\n";
    let manifest = Manifest::decode(&raw)?;
    let dest = stage.join(&id);
    fs::create_dir(&dest)?;
    let source = exe.parent().ok_or("missing qualification package")?;
    for (name, hash) in [
        ("susi.exe", manifest.artifact.as_str()),
        ("manifest.txt", id.as_str()),
        ("gates.txt", manifest.gates.as_str()),
        ("sbom.json", manifest.sbom.as_str()),
        ("native-source-audit.txt", manifest.native_audit.as_str()),
        ("THIRD-PARTY-NOTICES.txt", manifest.notices.as_str()),
    ] {
        let path = dest.join(name);
        fs::copy(source.join(name), &path)?;
        if file_digest(&path)? != hash {
            return Err(format!("exported {name} differs from qualification").into());
        }
    }
    if digest(raw.as_bytes()) != id {
        return Err("exported release identity mismatch".into());
    }
    Ok(id)
}

pub(super) fn export(ctx: &Context, id: &str, destination: &Path) -> Result<()> {
    if !cfg!(windows) {
        return Err("portable Windows export requires its native qualification host".into());
    }
    if !destination.is_absolute() || fs::symlink_metadata(destination).is_ok() {
        return Err("export destination must be a new absolute directory".into());
    }
    let _lock = Lock::acquire(&ctx.state)?;
    let (blob, _) = qualified(ctx, id)?;
    let current = resolve(&ctx.root, CURRENT)?;
    let prior = if current.as_deref() == Some(&blob) {
        resolve(&ctx.root, PREVIOUS)?
    } else {
        current
    };
    let parent = destination
        .parent()
        .ok_or("export directory needs an existing parent")?;
    let stage = parent.join(format!(".susi-qualified-export-{}", stamp()?));
    fs::create_dir(&stage)?;
    let exported = copy_verified(ctx, &blob, &stage)?;
    let previous = prior
        .as_deref()
        .filter(|old| *old != blob)
        .map(|old| copy_verified(ctx, old, &stage))
        .transpose()?;
    let receipt = serde_json::json!({"schema":"susi-portable-release-handoff-v1","release":exported,"previous":previous});
    fs::write(
        stage.join("handoff.json"),
        serde_json::to_vec_pretty(&receipt)?,
    )?;
    fs::rename(&stage, destination)?;
    println!("Portable verified Windows release {exported} exported to {}. Deliver this ID through the trusted agent connection.",destination.display());
    Ok(())
}

fn checked_command(
    ctx: &Context,
    exe: &Path,
    args: &[&str],
    label: &str,
    user: bool,
) -> Result<String> {
    let scratch = ctx.state.join(format!("ordinary-{label}-{}", stamp()?));
    fs::create_dir(&scratch)?;
    let mut command = if user {
        let mut command = Command::new(exe);
        command.env_clear();
        susi_paths::test_env::inherit_registry_isolation(&mut command);
        for key in [
            "SystemRoot",
            "WINDIR",
            "USERPROFILE",
            "APPDATA",
            "LOCALAPPDATA",
            "TEMP",
            "TMP",
            "ProgramFiles",
            "ProgramFiles(x86)",
            "ProgramW6432",
            "CommonProgramFiles",
            "CommonProgramFiles(x86)",
            "SystemDrive",
            "ProgramData",
            "ALLUSERSPROFILE",
        ] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        let system = std::env::var_os("SystemRoot").ok_or("Windows system directory is missing")?;
        let volume = Path::new(&system)
            .ancestors()
            .last()
            .ok_or("missing Windows system volume")?;
        let program_data = std::env::var_os("ProgramData")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| volume.join("ProgramData"));
        command
            .env(
                "SystemDrive",
                volume.to_string_lossy().trim_end_matches(['\\', '/']),
            )
            .env("ProgramData", &program_data)
            .env("ALLUSERSPROFILE", &program_data)
            .env("PATH", Path::new(&system).join("System32"));
        command
    } else {
        super::isolated_command(exe, &scratch.join("home"))?
    };
    command.current_dir(&scratch).args(args);
    super::measured_health(
        command,
        &scratch,
        label,
        std::time::Duration::from_secs(180),
        b"",
    )
    .map_err(|e| format!("{label} failed: {e}; evidence {}", scratch.display()))?;
    Ok(fs::read_to_string(scratch.join(format!("{label}.stdout")))?)
}

fn supports_installer(ctx: &Context, exe: &Path) -> Result<bool> {
    Ok(checked_command(ctx, exe, &["--help"], "help", false)?
        .lines()
        .any(|line| line.split_whitespace().next() == Some("install-release")))
}

fn status(ctx: &Context, exe: &Path, id: &str) -> Result<()> {
    checked_command(
        ctx,
        exe,
        &["install-release", "--status", "--expect", id],
        "status",
        true,
    )?;
    Ok(())
}

pub(super) fn install_current(ctx: &Context) -> Result<()> {
    if !cfg!(windows) {
        return Err("ordinary Windows installation requires Windows".into());
    }
    let _lock = Lock::acquire(&ctx.state)?;
    let blob = resolve(&ctx.root, CURRENT)?.ok_or("no promoted local release")?;
    let (id, exe) = checked_package(ctx, &blob)?;
    let previous = resolve(&ctx.root, PREVIOUS)?
        .map(|old| checked_package(ctx, &old))
        .transpose()?;
    let manager = if supports_installer(ctx, &exe)? {
        let mut args = vec!["install-release", "--id", &id];
        if let Some((previous, _)) = &previous {
            args.extend(["--previous", previous]);
        }
        checked_command(ctx, &exe, &args, "install", true)?;
        exe
    } else {
        // A previously promoted release can predate native ordinary-channel
        // management. Its verified successor performs the exact-target rollback.
        let (_, manager) = previous
            .ok_or("this release predates the native installer and has no qualified controller")?;
        if !supports_installer(ctx, &manager)? {
            return Err("no qualified ordinary-channel controller is available".into());
        }
        if status(ctx, &manager, &id).is_err() {
            checked_command(
                ctx,
                &manager,
                &["install-release", "--rollback", "--expect", &id],
                "rollback",
                true,
            )?;
        }
        manager
    };
    status(ctx, &manager, &id)?;
    println!("Ordinary Windows production verified at release {id}");
    Ok(())
}
