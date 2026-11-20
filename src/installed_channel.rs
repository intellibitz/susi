//! Ordinary production launches follow one local selector. Release activation,
//! promotion and rollback delegate to an external runtime host when configured;
//! launching and verifying the admitted entry needs neither Git nor any host binary.
use crate::cli::defs::Commands;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const ACTIVE: &str = "admitted-release-v1.json";
const ENTRY_RECEIPT: &str = "entry-receipt-v1.txt";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Selection {
    current: String,
    previous: Option<String>,
    dispatcher: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt<T> {
    value: T,
    sha256: String,
}

fn valid_id(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
}

fn hex_sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn regular(path: &Path) -> Result<File> {
    for parent in path.ancestors().skip(1) {
        let meta = fs::symlink_metadata(parent)?;
        if !meta.is_dir() || meta.file_type().is_symlink() {
            return Err(format!("redirected release path: {}", parent.display()).into());
        }
    }
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err(format!("not an ordinary release file: {}", path.display()).into());
    }
    Ok(OpenOptions::new().read(true).open(path)?)
}

fn read_selection(root: &Path) -> Result<Selection> {
    let mut raw = Vec::new();
    regular(&root.join(ACTIVE))?
        .take(65537)
        .read_to_end(&mut raw)?;
    if raw.len() > 65536 {
        return Err("release metadata exceeds 64 KiB".into());
    }
    let receipt: Receipt<Selection> = serde_json::from_slice(&raw)?;
    if hex_sha256(&serde_json::to_vec(&receipt.value)?) != receipt.sha256 {
        return Err("installed release receipt integrity mismatch".into());
    }
    let s = receipt.value;
    if !valid_id(&s.current)
        || !valid_id(&s.dispatcher)
        || s.previous
            .as_deref()
            .is_some_and(|p| !valid_id(p) || p == s.current)
    {
        return Err("invalid release selection".into());
    }
    Ok(s)
}

fn verified_slot_binary(root: &Path, release: &str) -> Result<(std::path::PathBuf, File, String)> {
    let slot = root.join("releases").join(release);
    let mut manifest = Vec::new();
    regular(&slot.join("manifest.txt"))?
        .take(65537)
        .read_to_end(&mut manifest)?;
    if hex_sha256(&manifest) != release {
        return Err("release manifest SHA-256 mismatch".into());
    }
    let text = std::str::from_utf8(&manifest)?;
    let artifact = text
        .lines()
        .find_map(|l| l.strip_prefix("artifact-sha256="))
        .filter(|v| valid_id(v))
        .ok_or("missing artifact-sha256 in release manifest")?
        .to_owned();
    let exe = slot.join("susi");
    let mut pinned = regular(&exe)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = pinned.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let actual: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if actual != artifact {
        return Err("installed release executable digest mismatch".into());
    }
    Ok((exe, pinned, artifact))
}

pub(crate) fn is_dispatcher(exe: &Path) -> bool {
    let mut parts = exe.components().rev();
    ["susi", "entry", "production", "SusiLocal"]
        .into_iter()
        .all(|expected| {
            parts
                .next()
                .and_then(|p| p.as_os_str().to_str())
                .is_some_and(|s| s.eq_ignore_ascii_case(expected))
        })
}

pub(crate) fn management(command: Option<&Commands>) -> Option<ExitCode> {
    command
        .filter(|cmd| matches!(cmd, Commands::InstallRelease { .. }))
        .map(manage)
}

pub(crate) fn manage(command: &Commands) -> ExitCode {
    let Commands::InstallRelease {
        id,
        previous,
        import_legacy,
        controller_sha256,
        rollback,
        promote,
        status,
        expect,
        host_worker,
    } = command
    else {
        return ExitCode::FAILURE;
    };
    let Some(program) = susi_config::vault_exec::runtime_host_executable() else {
        eprintln!("no runtime host configured for install-release (set SUSI_RUNTIME_HOST or install a compatible plug host such as `susi-commercial install-release`)");
        return ExitCode::FAILURE;
    };
    let Ok(core) = env::current_exe() else {
        return ExitCode::FAILURE;
    };
    let mut cmd = Command::new(&program);
    cmd.arg("install-release")
        .env("SUSI_CORE_BIN", &core)
        .env(susi_config::vault_exec::RUNTIME_HOST_ENV, &program);
    if let Some(v) = id {
        cmd.args(["--id", v]).arg("--core").arg(&core);
    }
    if let Some(v) = previous {
        cmd.args(["--previous", v]);
    }
    if *import_legacy {
        cmd.arg("--import-legacy");
    }
    if let Some(v) = controller_sha256 {
        cmd.args(["--controller-sha256", v]);
    }
    if *rollback {
        cmd.arg("--rollback");
    }
    if let Some(v) = promote {
        cmd.args(["--promote", v]);
    }
    if *status {
        cmd.arg("--status");
    }
    if let Some(v) = expect {
        cmd.args(["--expect", v]);
    }
    if *host_worker {
        cmd.arg("--host-worker");
    }
    cmd.stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    match susi_paths::stdio::spawn_without_inheriting_stdio(&mut cmd).and_then(|mut c| c.wait()) {
        Ok(st) => ExitCode::from(st.code().unwrap_or(1) as u8),
        Err(error) => {
            eprintln!("Installed release activation failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn ensure_installed_home() -> Result<()> {
    if env::var_os("SUSI_HOME").is_none() {
        let home = crate::get_home_dir().join(".susi");
        if !home.is_absolute() {
            return Err("cannot discover the installed user's production home".into());
        }
        env::set_var("SUSI_HOME", home);
    }
    Ok(())
}

fn run_in_place(exe: &Path, root: &Path, selection: &Selection) -> Result<bool> {
    if selection.current != selection.dispatcher {
        return Ok(false);
    }
    let meta = fs::metadata(exe)?;
    let stamp = format!(
        "release={}\nbytes={}\nino={}\nmode={}\nmtime={}:{}\n",
        selection.current,
        meta.len(),
        meta.ino(),
        meta.mode(),
        meta.mtime(),
        meta.mtime_nsec()
    );
    let receipt = root.join(ENTRY_RECEIPT);
    if fs::read_to_string(&receipt).ok().as_deref() != Some(stamp.as_str()) {
        let (_, _, artifact) = verified_slot_binary(root, &selection.current)?;
        if super::digest(exe)? != artifact {
            return Ok(false);
        }
        if let Ok(mut out) = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&receipt)
        {
            let _ = out.write_all(stamp.as_bytes());
        }
    }
    ensure_installed_home()?;
    Ok(true)
}

pub(crate) fn dispatch() -> Option<ExitCode> {
    let exe = env::current_exe().ok()?;
    if !is_dispatcher(&exe) {
        return None;
    }
    let mut args = env::args_os().skip(1);
    let command = args.next();
    if command.as_deref() == Some(std::ffi::OsStr::new("install-release")) {
        return None;
    }
    let is_install_only =
        command.as_deref() == Some(std::ffi::OsStr::new("install")) && args.next().is_none();
    let verify_and_launch = || -> Result<Option<ExitCode>> {
        let root = exe
            .parent()
            .and_then(Path::parent)
            .ok_or("missing installed channel")?;
        let selection = read_selection(root)?;
        if is_install_only {
            let (slot_exe, _pinned, _) = verified_slot_binary(root, &selection.current)?;
            println!("Verified installed Susi: {}", slot_exe.display());
            return Ok(Some(ExitCode::SUCCESS));
        }
        if run_in_place(&exe, root, &selection)? {
            return Ok(None);
        }
        let (slot_exe, pinned, _) = verified_slot_binary(root, &selection.current)?;
        let mut child_cmd = Command::new(slot_exe.canonicalize()?);
        if env::var_os("SUSI_HOME").is_none() {
            let home = crate::get_home_dir().join(".susi");
            if !home.is_absolute() {
                return Err("cannot discover the installed user's production home".into());
            }
            child_cmd.env("SUSI_HOME", home);
        }
        let mut child = susi_paths::stdio::spawn_without_inheriting_stdio(
            child_cmd.args(env::args_os().skip(1)),
        )?;
        drop(pinned);
        let status = child.wait()?;
        std::process::exit(status.code().unwrap_or(1));
    };
    match verify_and_launch() {
        Ok(outcome) => outcome,
        Err(error) => {
            eprintln!("Installed Susi could not launch: {error}");
            Some(ExitCode::FAILURE)
        }
    }
}
