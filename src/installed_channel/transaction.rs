use super::package::{
    atomic_file, digest_bytes, directory, id, is_reparse_or_symlink, link_or_copy, regular, small,
    Package, Result, BIN_NAME,
};
use serde::{Deserialize, Serialize};
use std::env;
use std::fs::{self, OpenOptions};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
#[cfg(windows)]
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
    [BIN_NAME, "entry", "production", "SusiLocal"]
        .into_iter()
        .all(|expected| {
            parts
                .next()
                .and_then(|part| part.as_os_str().to_str())
                .is_some_and(|s| s.eq_ignore_ascii_case(expected))
        })
}

const ENTRY_RECEIPT: &str = "entry-receipt-v1.txt";

/// Identity of the entry file as last proven byte-equal to `release`.
fn entry_stamp(release: &str, exe: &Path) -> Result<String> {
    let meta = fs::metadata(exe)?;
    #[cfg(windows)]
    {
        Ok(format!(
            "release={release}\nbytes={}\ncreated={}\nwritten={}\n",
            meta.file_size(),
            meta.creation_time(),
            meta.last_write_time()
        ))
    }
    #[cfg(unix)]
    {
        Ok(format!(
            "release={release}\nbytes={}\nino={}\nmode={}\nmtime={}:{}\n",
            meta.len(),
            meta.ino(),
            meta.mode(),
            meta.mtime(),
            meta.mtime_nsec()
        ))
    }
    #[cfg(not(any(windows, unix)))]
    {
        let modified = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        Ok(format!(
            "release={release}\nbytes={}\nmodified={modified}\n",
            meta.len()
        ))
    }
}

/// When the running entry is the current release, it already is the selected
/// product: hashing a second 51 MB image and spawning it added ~120 ms to every
/// command. The entry is proven byte-equal to the current release once and
/// that proof is keyed to the entry file's identity; any change to the file or
/// the selection repeats the proof, and a mismatch keeps the verified child
/// launch. Integrity of the code already running cannot be self-checked, so
/// nothing is lost by skipping the repeat.
pub(super) fn run_in_place(exe: &Path) -> Result<bool> {
    let root = exe
        .parent()
        .and_then(Path::parent)
        .ok_or("missing installed channel")?;
    let selection: Selection =
        get(&root.join(ACTIVE))?.ok_or("missing installed release receipt")?;
    selection.validate()?;
    if selection.current != selection.dispatcher {
        return Ok(false);
    }
    let stamp = entry_stamp(&selection.current, exe)?;
    let receipt = root.join(ENTRY_RECEIPT);
    if fs::read_to_string(&receipt).ok().as_deref() != Some(stamp.as_str()) {
        if installed(root, &selection.current)?
            .verify_entry(exe)
            .is_err()
        {
            return Ok(false);
        }
        // A missing receipt only costs the next launch another proof.
        let _ = atomic_file(&receipt, stamp.as_bytes());
    }
    if env::var_os("SUSI_HOME").is_none() {
        let home = crate::get_home_dir().join(".susi");
        if !home.is_absolute() {
            return Err("cannot discover the installed user's production home".into());
        }
        // Still single-threaded: `dispatch` runs first in `main`.
        env::set_var("SUSI_HOME", home);
    }
    Ok(true)
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
    // Shell-launched processes need the verbatim Windows path for deep release slots.
    let mut command = Command::new(package.executable().canonicalize()?);
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
    // The release still gets this caller's console or pipes as its own stdio,
    // but not the dispatcher's originals as extra inheritable handles: a
    // daemon it detaches would otherwise keep `$(susi start)` pipes open.
    let mut child =
        susi_windows_runtime::spawn_without_inheriting_stdio(command.args(env::args_os().skip(1)))?;
    drop(pinned);
    Ok(child.wait()?)
}

fn bootstrap(root: &Path, from_dispatcher: Option<&str>, after: &Selection) -> Result<()> {
    let bin = root.join("entry");
    directory(&bin)?;
    let entry = bin.join(BIN_NAME);
    let target = installed(root, &after.dispatcher)?;
    let pinned = target.pinned_executable()?;
    if entry.try_exists()? {
        if target.verify_entry(&entry).is_ok() {
            return Ok(());
        }
        let old = from_dispatcher.ok_or("refusing to replace an unrecognized dispatcher")?;
        installed(root, old)?.verify_entry(&entry)?;
    }
    let staged = bin.join(format!("dispatcher-pending{}", env::consts::EXE_SUFFIX));
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
    let mut command = Command::new(exe.canonicalize()?);
    command.env_clear().current_dir(&scratch).arg("--version");
    #[cfg(windows)]
    {
        let system = susi_harness::channel_bindings::windows_root()?;
        let drive = Path::new(&system)
            .ancestors()
            .last()
            .ok_or("Windows volume is missing")?
            .to_string_lossy();
        command
            .env("SystemRoot", &system)
            .env("WINDIR", &system)
            .env("SystemDrive", drive.trim_end_matches(['\\', '/']))
            .env("PATH", Path::new(&system).join("System32"));
    }
    #[cfg(not(windows))]
    {
        command.env("PATH", "/usr/bin:/bin");
    }
    command
        .env("HOME", &scratch)
        .env("USERPROFILE", &scratch)
        .env("SUSI_HOME", scratch.join(".susi"))
        .env("SUSI_NO_REGISTRY_ENV", "1");
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
        if !meta.is_file() || is_reparse_or_symlink(&meta) {
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
            let expected = root.join("builds").join(sha).join(BIN_NAME);
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

// A packaged caller can read its own newly installed files while ordinary
// Windows processes see an entirely different release at the same path.
#[cfg(windows)]
fn public_host_view(local: &Path) -> Result<bool> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_FLAG_DELETE_ON_CLOSE: u32 = 0x0400_0000;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let name = format!(".susi-host-view-{}-{nonce}", std::process::id());
    let requested = local.join(&name);
    // Probe a new write: existing public files can be visible in both views.
    let _probe = OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(FILE_FLAG_DELETE_ON_CLOSE)
        .open(&requested)?;
    let physical = requested.canonicalize()?;
    let private_cache = physical.ancestors().any(|part| {
        part.ends_with("LocalCache/Local")
            && part
                .parent()
                .and_then(Path::parent)
                .and_then(Path::parent)
                .is_some_and(|parent| parent.ends_with("Packages"))
    });
    Ok(!private_cache && physical == local.canonicalize()?.join(name))
}

#[derive(Default)]
pub(super) struct Request<'a> {
    pub release: Option<&'a str>,
    pub previous: Option<&'a str>,
    pub rollback: bool,
    pub status: bool,
    pub expected: Option<&'a str>,
}

pub(super) fn manage(request: Request<'_>, host_worker: bool) -> Result<()> {
    let result = manage_in_view(request, host_worker);
    #[cfg(windows)]
    if host_worker {
        super::host_task::reply(&result)?;
    }
    println!(
        "Installed verified Susi release {}. Open Susi or run susi.",
        result?
    );
    Ok(())
}

fn manage_in_view(request: Request<'_>, host_worker: bool) -> Result<String> {
    let Request {
        release,
        previous,
        rollback,
        status,
        expected,
    } = request;
    #[cfg(windows)]
    let local = PathBuf::from(env::var_os("LOCALAPPDATA").ok_or("LOCALAPPDATA missing")?);
    #[cfg(windows)]
    let root = local.join("SusiLocal").join("production");
    #[cfg(not(windows))]
    let root = match env::var_os("LOCALAPPDATA") {
        Some(dir) => PathBuf::from(dir).join("SusiLocal").join("production"),
        None => crate::get_home_dir()
            .join(".local")
            .join("share")
            .join("SusiLocal")
            .join("production"),
    };
    if !root.is_absolute() {
        return Err("installed release root must be absolute".into());
    }
    #[cfg(windows)]
    if !public_host_view(&local)? {
        if host_worker {
            return Err("Windows host worker still sees a private filesystem view".into());
        }
        return super::host_task::run(release, previous, rollback, status, expected);
    }
    #[cfg(not(windows))]
    if host_worker {
        return Err("the installation host worker requires native Windows".into());
    }
    let register = |root: &Path| -> Result<()> {
        #[cfg(windows)]
        if susi_harness::channel_bindings::register(root)? {
            winapi::environment::notify_environment_changed()?;
            susi_harness::channel_bindings::notification_complete(root)?;
        }
        #[cfg(not(windows))]
        {
            let work = root.join("workspace");
            if !work.try_exists()? {
                fs::create_dir(&work)?;
            }
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
        let entry = root.join("entry").join(BIN_NAME);
        installed(&root, &chosen.dispatcher)?.verify_entry(&entry)?;
        #[cfg(windows)]
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
    Ok(selection.current)
}

#[cfg(all(test, windows))]
#[path = "tests.rs"]
mod tests;

#[cfg(test)]
mod cross_os_tests {
    use super::super::package::{expected_host_target_triple, QUALIFICATION_POLICY, SIDECARS};
    use super::*;

    #[test]
    fn dispatcher_path_and_entry_stamp_work_across_platforms() {
        let root = env::temp_dir().join(format!(
            "susi-cross-os-channel-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let entry_dir = root.join("SusiLocal").join("production").join("entry");
        fs::create_dir_all(&entry_dir).unwrap();
        let entry_exe = entry_dir.join(BIN_NAME);
        fs::write(&entry_exe, b"dispatcher-bytes").unwrap();
        assert!(is_dispatcher(&entry_exe));
        assert!(!is_dispatcher(&root.join("other").join(BIN_NAME)));
        let stamp = entry_stamp(&"a".repeat(64), &entry_exe).unwrap();
        assert!(stamp.contains("bytes=16"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn cross_os_package_stage_and_rollback_succeed() {
        let base = env::temp_dir().join(format!(
            "susi-cross-os-stage-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&base).unwrap();
        let build_pkg = |tag: &str| -> Package {
            let dir = base.join(tag);
            fs::create_dir_all(&dir).unwrap();
            let fixture = dir.join("fixture.rs");
            fs::write(
                &fixture,
                format!("fn main() {{ println!(\"susi {tag}\"); }}"),
            )
            .unwrap();
            let exe = dir.join(BIN_NAME);
            assert!(Command::new("rustc")
                .args(["--edition", "2021"])
                .arg(&fixture)
                .arg("-o")
                .arg(&exe)
                .status()
                .unwrap()
                .success());
            let data = fs::read(&exe).unwrap();
            let side = b"cross-os sidecar";
            let hash = digest_bytes(side);
            let gates = [QUALIFICATION_POLICY, side.as_slice()].concat();
            let gate_hash = digest_bytes(&gates);
            fs::write(dir.join("gates.txt"), gates).unwrap();
            for (name, _) in SIDECARS {
                fs::write(dir.join(name), side).unwrap();
            }
            let oid = "b".repeat(40);
            let manifest = format!(
                "susi-local-release-v1\nsource={oid}\ntree={oid}\nartifact-sha256={}\nartifact-bytes={}\nhost={}\nintegration-receipt={oid}\nlock-sha256={hash}\ncompiler-sha256={hash}\ngates-sha256={gate_hash}\nsbom-sha256={hash}\nnotices-sha256={hash}\nnative-audit-sha256={hash}\n",
                digest_bytes(&data),
                data.len(),
                expected_host_target_triple()
            );
            fs::write(dir.join("manifest.txt"), &manifest).unwrap();
            Package::open(&dir, &digest_bytes(manifest.as_bytes())).unwrap()
        };
        let pkg_a = build_pkg("1.0.0-a");
        let pkg_b = build_pkg("1.0.0-b");
        let root = base.join("SusiLocal").join("production");
        let sel_a = apply(&root, Some((&pkg_a, None)), &|_| Ok(()), None).unwrap();
        assert_eq!(sel_a.current, pkg_a.release);
        let sel_b = apply(&root, Some((&pkg_b, None)), &|_| Ok(()), None).unwrap();
        assert_eq!(sel_b.current, pkg_b.release);
        assert_eq!(sel_b.previous.as_deref(), Some(pkg_a.release.as_str()));
        let rolled = apply(&root, None, &|_| Ok(()), Some(&pkg_a.release)).unwrap();
        assert_eq!(rolled.current, pkg_a.release);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn selection_validation_and_receipt_integrity_reject_corrupt_state() {
        let root = env::temp_dir().join(format!(
            "susi-cross-os-receipt-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let active_path = root.join(ACTIVE);

        let valid = Selection {
            current: "c".repeat(64),
            previous: Some("d".repeat(64)),
            dispatcher: "c".repeat(64),
        };
        assert!(valid.validate().is_ok());
        put(&active_path, &valid).unwrap();
        assert_eq!(get::<Selection>(&active_path).unwrap(), Some(valid.clone()));

        let self_prev = Selection {
            previous: Some("c".repeat(64)),
            ..valid
        };
        assert!(self_prev.validate().is_err());

        let tampered =
            fs::read_to_string(&active_path)
                .unwrap()
                .replacen(&"c".repeat(64), &"e".repeat(64), 1);
        fs::write(&active_path, tampered).unwrap();
        assert!(get::<Selection>(&active_path).is_err());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn apply_rollback_without_installed_release_or_predecessor_fails_cleanly() {
        let root = env::temp_dir().join(format!(
            "susi-cross-os-no-rollback-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();

        assert!(apply(&root, None, &|_| Ok(()), None).is_err());

        let no_prev = Selection {
            current: "f".repeat(64),
            previous: None,
            dispatcher: "f".repeat(64),
        };
        put(&root.join(ACTIVE), &no_prev).unwrap();
        assert!(apply(&root, None, &|_| Ok(()), None).is_err());

        let _ = fs::remove_dir_all(&root);
    }
}
