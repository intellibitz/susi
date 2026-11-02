use super::package::{
    atomic_file, digest_bytes, directory, id, link_or_copy, regular, small, Package, Result,
};
use serde::{Deserialize, Serialize};
use std::env;
use std::fs::{self, OpenOptions};
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

const ACTIVE: &str = "admitted-release-v1.json";
const PENDING: &str = "admitted-transition-v1.json";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Selection {
    current: String,
    previous: Option<String>,
    dispatcher: String,
}

impl Selection {
    fn require(&self, expected: Option<&str>) -> Result<()> {
        if expected.is_some_and(|id| id != self.current) {
            return Err("installed release differs from the requested target".into());
        }
        Ok(())
    }
    fn validate(&self) -> Result<()> {
        if !id(&self.current)
            || !id(&self.dispatcher)
            || self
                .previous
                .as_deref()
                .is_some_and(|p| !id(p) || p == self.current)
        {
            return Err("invalid release selection".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Transition {
    requested: Option<String>,
    from_dispatcher: Option<String>,
    before: Option<Selection>,
    after: Selection,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt<T> {
    value: T,
    sha256: String,
}

fn put<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let sha256 = digest_bytes(&serde_json::to_vec(value)?);
    atomic_file(path, &serde_json::to_vec(&Receipt { value, sha256 })?)
}

fn get<T: serde::de::DeserializeOwned + Serialize>(path: &Path) -> Result<Option<T>> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
        Ok(_) => {}
    }
    let receipt: Receipt<T> = serde_json::from_slice(&small(path)?)?;
    if digest_bytes(&serde_json::to_vec(&receipt.value)?) != receipt.sha256 {
        return Err("installed release receipt integrity mismatch".into());
    }
    Ok(Some(receipt.value))
}

fn installed(root: &Path, id: &str) -> Result<Package> {
    Package::open(&root.join("releases").join(id), id)
}

pub(super) fn is_dispatcher(exe: &Path) -> bool {
    let mut parts = exe.components().rev();
    ["susi.exe", "entry", "production", "SusiLocal"]
        .into_iter()
        .all(|expected| {
            parts
                .next()
                .and_then(|part| part.as_os_str().to_str())
                .is_some_and(|s| s.eq_ignore_ascii_case(expected))
        })
}

pub(super) fn dispatch(exe: &Path) -> Result<ExitStatus> {
    let root = exe
        .parent()
        .and_then(Path::parent)
        .ok_or("missing installed channel")?;
    let selection: Selection =
        get(&root.join(ACTIVE))?.ok_or("missing installed release receipt")?;
    selection.validate()?;
    let package = installed(root, &selection.current)?;
    let pinned = package.pinned_executable()?;
    let mut command = Command::new(package.executable());
    if env::var_os("SUSI_HOME").is_none() {
        // An immutable production slot is outside ~/.susi/bin. Select the same
        // native user state explicitly so the child's development-isolation
        // fallback never invents a new model cache for each installed release.
        let home = crate::get_home_dir().join(".susi");
        if !home.is_absolute() {
            return Err("cannot discover the installed user's production home".into());
        }
        command.env("SUSI_HOME", home);
    }
    let mut child = command.args(env::args_os().skip(1)).spawn()?;
    drop(pinned);
    Ok(child.wait()?)
}

fn bootstrap(root: &Path, from_dispatcher: Option<&str>, after: &Selection) -> Result<()> {
    let bin = root.join("entry");
    directory(&bin)?;
    let entry = bin.join("susi.exe");
    let target = installed(root, &after.dispatcher)?;
    let pinned = target.pinned_executable()?;
    if entry.try_exists()? {
        if target.verify_entry(&entry).is_ok() {
            return Ok(());
        }
        let old = from_dispatcher.ok_or("refusing to replace an unrecognized dispatcher")?;
        installed(root, old)?.verify_entry(&entry)?;
    }
    let staged = bin.join("dispatcher-pending.exe");
    if staged.try_exists()? {
        drop(regular(&staged)?);
        fs::remove_file(&staged)?;
    }
    link_or_copy(&target.executable(), &staged)?;
    target.verify_entry(&staged)?;
    drop(pinned);
    fs::rename(staged, entry)?;
    Ok(())
}

fn finish(root: &Path, change: &Transition, register: &impl Fn(&Path) -> Result<()>) -> Result<()> {
    change.after.validate()?;
    if let Some(before) = &change.before {
        before.validate()?;
    }
    let observed: Option<Selection> = get(&root.join(ACTIVE))?;
    if observed != change.before && observed.as_ref() != Some(&change.after) {
        return Err("installed selector changed outside the recorded transaction".into());
    }
    let current = installed(root, &change.after.current)?;
    current.verify_sidecars()?;
    let current_pin = current.pinned_executable()?;
    if let Some(previous) = &change.after.previous {
        let package = installed(root, previous)?;
        package.verify_sidecars()?;
        package.pinned_executable()?;
    }
    check_version(&current)?;
    bootstrap(root, change.from_dispatcher.as_deref(), &change.after)?;
    // For a fresh install the selector must exist before publishing the first
    // shortcut. Existing public entry points continue using their prior selector
    // until the one same-volume atomic write after registration.
    if observed.is_none() {
        put(&root.join(ACTIVE), &change.after)?;
    }
    register(root)?;
    if observed.is_some() {
        put(&root.join(ACTIVE), &change.after)?;
    }
    drop(current_pin);
    fs::remove_file(root.join(PENDING))?;
    Ok(())
}

fn version_output(exe: &Path) -> Result<String> {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let scratch = env::temp_dir().join(format!(
        "susi-installed-health-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir(&scratch)?;
    let mut command = Command::new(exe);
    command.env_clear().current_dir(&scratch).arg("--version");
    let system = env::var_os("SystemRoot").ok_or("Windows system directory is missing")?;
    let drive = Path::new(&system)
        .ancestors()
        .last()
        .ok_or("Windows volume is missing")?
        .to_string_lossy();
    command
        .env("SystemRoot", &system)
        .env("WINDIR", &system)
        .env("SystemDrive", drive.trim_end_matches(['\\', '/']))
        .env("PATH", Path::new(&system).join("System32"))
        .env("HOME", &scratch)
        .env("USERPROFILE", &scratch)
        .env("SUSI_HOME", scratch.join(".susi"));
    for key in [
        "ProgramData",
        "ALLUSERSPROFILE",
        "APPDATA",
        "LOCALAPPDATA",
        "TEMP",
        "TMP",
    ] {
        command.env(key, &scratch);
    }
    let output =
        susi_core::bounded_cmd::output_within(&mut command, std::time::Duration::from_secs(30))?;
    if !output.status.success()
        || !std::str::from_utf8(&output.stdout)?
            .trim()
            .starts_with("susi ")
    {
        return Err("installed release health check failed".into());
    }
    let version = std::str::from_utf8(&output.stdout)?.trim().into();
    fs::remove_dir(scratch)?;
    Ok(version)
}

fn check_version(package: &Package) -> Result<()> {
    version_output(&package.executable()).map(|_| ())
}

fn apply(
    root: &Path,
    package: Option<(&Package, Option<&Package>)>,
    register: &impl Fn(&Path) -> Result<()>,
    expected: Option<&str>,
) -> Result<Selection> {
    directory(root)?;
    let lock_path = root.join("admitted-install.lock");
    if let Ok(meta) = fs::symlink_metadata(&lock_path) {
        if !meta.is_file() || meta.file_attributes() & 0x400 != 0 {
            return Err("unsafe installation lock".into());
        }
    }
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)?;
    lock.try_lock()?;
    let requested = package.map(|(source, _)| source.release.clone());
    if let Some(pending) = get::<Transition>(&root.join(PENDING))? {
        // A retry of an interrupted rollback must complete its recorded target,
        // rather than perform a second rollback and reselect the failed version.
        if requested.is_none() && pending.requested.is_some() {
            if let Some(restore) = &pending.before {
                restore.require(expected)?;
                let current: Option<Selection> = get(&root.join(ACTIVE))?;
                if current != pending.before && current.as_ref() != Some(&pending.after) {
                    return Err("interrupted activation has an unknown selector".into());
                }
                let restored = Transition {
                    requested: None,
                    from_dispatcher: Some(pending.after.dispatcher.clone()),
                    before: current,
                    after: restore.clone(),
                };
                put(&root.join(PENDING), &restored)?;
                finish(root, &restored, register)?;
                return Ok(restored.after);
            }
        }
        if requested == pending.requested {
            pending.after.require(expected)?;
        }
        finish(root, &pending, register)?;
        if requested == pending.requested {
            return Ok(pending.after);
        }
    }
    let mut before: Option<Selection> = get(&root.join(ACTIVE))?;
    if let Some(old) = &before {
        old.validate()?;
    }
    let after = if let Some((source, predecessor)) = package {
        let target = source.stage(root)?;
        // An existing deployment keeps its own last verified version. A user may
        // legitimately skip intermediate releases from the producer's history.
        let imported = predecessor
            .filter(|_| before.is_none())
            .map(|p| p.stage(root))
            .transpose()?;
        if before.is_none() && root.join("channel.json").try_exists()? {
            let legacy: serde_json::Value =
                serde_json::from_slice(&small(&root.join("channel.json"))?)?;
            let prior = imported
                .as_ref()
                .ok_or("existing production channel requires its qualified predecessor")?;
            let sha = legacy["sha256"]
                .as_str()
                .filter(|s| id(s))
                .ok_or("missing legacy production digest")?;
            let expected = root.join("builds").join(sha).join("susi.exe");
            if legacy["current"].as_str().map(Path::new) != Some(&expected) {
                return Err("legacy launch path does not match its admitted slot".into());
            }
            prior.verify_entry(&expected)?;
            let initial = Selection {
                current: prior.release.clone(),
                previous: None,
                dispatcher: target.release.clone(),
            };
            put(&root.join(ACTIVE), &initial)?;
            before = Some(initial);
        }
        let previous = match &before {
            Some(old) if old.current == target.release => old.previous.clone(),
            Some(old) => Some(old.current.clone()),
            None => imported.as_ref().map(|p| p.release.clone()),
        };
        Selection {
            current: target.release.clone(),
            previous,
            dispatcher: target.release,
        }
    } else {
        let old = before.as_ref().ok_or("no installed release to roll back")?;
        if expected == Some(old.current.as_str()) {
            old.clone()
        } else {
            let previous = installed(root, &old.current)
                .and_then(|p| {
                    p.verify_sidecars()?;
                    p.pinned_executable()
                })
                .is_ok()
                .then(|| old.current.clone());
            Selection {
                current: old
                    .previous
                    .clone()
                    .ok_or("no verified installed predecessor")?,
                previous,
                dispatcher: old.dispatcher.clone(),
            }
        }
    };
    after.validate()?;
    after.require(expected)?;
    let target = installed(root, &after.current)?;
    target.verify_sidecars()?;
    let pin = target.pinned_executable()?;
    check_version(&target)?;
    drop(pin);
    let from_dispatcher = before.as_ref().map(|p| p.dispatcher.clone());
    let change = Transition {
        requested,
        from_dispatcher,
        before,
        after: after.clone(),
    };
    put(&root.join(PENDING), &change)?;
    finish(root, &change, register)?;
    Ok(after)
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

pub(super) fn manage(
    release: Option<&str>,
    previous: Option<&str>,
    rollback: bool,
    status: bool,
    expected: Option<&str>,
) -> Result<()> {
    let root = PathBuf::from(env::var_os("LOCALAPPDATA").ok_or("LOCALAPPDATA missing")?)
        .join("SusiLocal")
        .join("production");
    if !root.is_absolute() {
        return Err("installed release root must be absolute".into());
    }
    let register = |root: &Path| -> Result<()> {
        if susi_harness::channel_bindings::register(root)? {
            susi_config::windows_environment::notify_environment_changed()?;
            susi_harness::channel_bindings::notification_complete(root)?;
        }
        Ok(())
    };
    let selection = if status {
        let chosen: Selection =
            get(&root.join(ACTIVE))?.ok_or("no installed production selector")?;
        chosen.validate()?;
        chosen.require(expected)?;
        let current = installed(&root, &chosen.current)?;
        current.verify_sidecars()?;
        let _pinned = current.pinned_executable()?;
        let entry = root.join("entry/susi.exe");
        installed(&root, &chosen.dispatcher)?.verify_entry(&entry)?;
        susi_harness::channel_bindings::verify(&root)?;
        if version_output(&entry)? != version_output(&current.executable())? {
            return Err("ordinary entry point launches a different version".into());
        }
        chosen
    } else if rollback {
        apply(&root, None, &register, expected)?
    } else {
        let id = release.ok_or("trusted release ID is required")?;
        let exe = env::current_exe()?;
        let path = exe.parent().ok_or("package directory missing")?;
        let package = Package::open(path, id)?;
        package.verify_entry(&exe)?;
        let prior = previous
            .map(|id| {
                if !super::package::id(id) {
                    return Err("invalid predecessor release ID".into());
                }
                let parent = path.parent().ok_or("sibling package directory missing")?;
                Package::open(&parent.join(id), id)
            })
            .transpose()?;
        apply(&root, Some((&package, prior.as_ref())), &register, None)?
    };
    println!(
        "Installed verified Susi release {}. Open Susi or run susi.",
        selection.current
    );
    Ok(())
}
