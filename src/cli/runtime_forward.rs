//! Complete command invocations cross the process boundary before composition.
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::{Command, ExitCode, Stdio};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub(crate) fn execute(
    words: &[OsString],
    prepare_instance: impl FnOnce() -> std::io::Result<()>,
) -> ExitCode {
    match run(words, prepare_instance) {
        Ok(status) => status,
        Err(error) => {
            eprintln!("Could not run the Susi command: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(
    words: &[OsString],
    prepare_instance: impl FnOnce() -> std::io::Result<()>,
) -> Result<ExitCode> {
    let program: Option<PathBuf> = None;
    let program = program
        .or_else(susi_config::vault_exec::commercial_executable)
        .ok_or_else(|| {
            let entry = words
                .first()
                .and_then(|word| word.to_str())
                .filter(|command| matches!(*command, "setup" | "top" | "peers" | "commits"))
                .unwrap_or("core-runtime");
            format!("this command is served by Susi Commercial Apps: install it and run `susi-commercial {entry} <arguments>` (or repeat the same `susi` command)")
        })?;
    let launcher = core_executable(susi_paths::SusiDirs::cache_dir)?;
    if program.canonicalize()? == launcher.canonicalize()? {
        return Err("the commercial runtime resolves to the public core launcher".into());
    }
    // Keep the same admitted image pinned while its parser handles help and
    // errors before allocating any instance or consuming the caller's stdin.
    let mut parser = Command::new(&program);
    parser
        .arg("core-parse")
        .args(words)
        .env("SUSI_CORE_BIN", &launcher)
        .env("SUSI_COMMERCIAL_BIN", &program)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let status = susi_paths::stdio::spawn_without_inheriting_stdio(&mut parser)?.wait()?;
    if status.code() != Some(64) {
        return Ok(ExitCode::from(status.code().unwrap_or(1) as u8));
    }
    prepare_instance()?;
    let mut command = Command::new(&program);
    command
        .arg("core-runtime")
        .args(words)
        .env("SUSI_CORE_BIN", launcher)
        .env("SUSI_COMMERCIAL_BIN", &program)
        // Core already selected the original instance at its original startup
        // point. In particular, an installed XDG instance keeps SUSI_HOME unset.
        .env("SUSI_CORE_INSTANCE_READY", "1")
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let status = susi_paths::stdio::spawn_without_inheriting_stdio(&mut command)?.wait()?;
    Ok(ExitCode::from(status.code().unwrap_or(1) as u8))
}

/// A detached worker may start after this invocation has exited. A Linux
/// procfs link belongs to its process; retain the actual running bytes when
/// the original pathname was removed or replaced, rather than passing that
/// link across the process boundary.
fn core_executable(cache: impl FnOnce() -> PathBuf) -> Result<PathBuf> {
    use sha2::{Digest, Sha256};
    use std::fs::{self, File, OpenOptions};
    use std::io::{Read, Seek};
    use std::os::unix::fs::MetadataExt;

    let current = std::env::current_exe()?;
    let mut original = File::open("/proc/self/exe")?;
    let metadata = original.metadata()?;
    if fs::metadata(&current)
        .is_ok_and(|named| named.dev() == metadata.dev() && named.ino() == metadata.ino())
    {
        return Ok(current);
    }
    let mut hash = Sha256::new();
    let mut bytes = [0; 65536];
    loop {
        let count = original.read(&mut bytes)?;
        if count == 0 {
            break;
        }
        hash.update(&bytes[..count]);
    }
    let digest: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
    let directory = std::path::absolute(cache())?
        .join("core-launchers")
        .join(&digest);
    super::install_cli::directory(&directory)?;
    let destination = directory.join("susi");
    let verify = || -> Result<()> {
        let meta = fs::symlink_metadata(&destination)?;
        if !meta.is_file()
            || meta.file_type().is_symlink()
            || super::install_cli::digest(&destination)? != digest
        {
            return Err("retained launcher differs from the running core image".into());
        }
        Ok(())
    };
    if destination.try_exists()? {
        verify()?;
        return Ok(destination);
    }
    let staging = directory.join(format!(".staging-{}", std::process::id()));
    let mut copy = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&staging)?;
    original.rewind()?;
    std::io::copy(&mut original, &mut copy)?;
    copy.set_permissions(metadata.permissions())?;
    copy.sync_all()?;
    drop(copy);
    let linked = fs::hard_link(&staging, &destination);
    fs::remove_file(&staging)?;
    if let Err(error) = linked {
        if error.kind() != std::io::ErrorKind::AlreadyExists {
            return Err(error.into());
        }
    }
    verify()?;
    Ok(destination)
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::path::Path;

    #[cfg(target_os = "linux")]
    #[test]
    fn unlinked_launcher_survives_its_original_process() {
        use std::{
            env, fs, thread,
            time::{Duration, Instant},
        };
        const CHILD: &str = "SUSI_RETAINED_LAUNCHER_TEST";
        if let Some(root) = env::var_os(CHILD).map(PathBuf::from) {
            if env::var_os("SUSI_RETAINED_REENTRY").is_some() {
                fs::write(root.join("reentered"), b"original image").unwrap();
                return;
            }
            fs::write(root.join("ready"), b"ready").unwrap();
            let deadline = Instant::now() + Duration::from_secs(15);
            while !root.join("unlinked").exists() {
                assert!(Instant::now() < deadline);
                thread::sleep(Duration::from_millis(10));
            }
            let retained = core_executable(|| root.join("cache")).unwrap();
            fs::write(
                root.join("retained"),
                retained.as_os_str().as_encoded_bytes(),
            )
            .unwrap();
            assert_eq!(retained, core_executable(|| root.join("cache")).unwrap());
            return;
        }
        let temp = susi_paths::tempfile::tempdir().unwrap();
        let root = temp.path();
        let launcher = root.join("original core launcher");
        fs::copy(env::current_exe().unwrap(), &launcher).unwrap();
        let test = "cli::runtime_forward::tests::unlinked_launcher_survives_its_original_process";
        // A sibling test thread forking while `copy` still held the file open
        // keeps a write descriptor to it until that child execs, so exec can
        // briefly fail with ETXTBSY; wait that inherited descriptor out.
        let spawn_deadline = Instant::now() + Duration::from_secs(10);
        let mut child = loop {
            match Command::new(&launcher)
                .args(["--exact", test, "--nocapture"])
                .env(CHILD, root)
                .spawn()
            {
                Err(error)
                    if error.kind() == std::io::ErrorKind::ExecutableFileBusy
                        && Instant::now() < spawn_deadline =>
                {
                    thread::sleep(Duration::from_millis(20));
                }
                spawned => break spawned.unwrap(),
            }
        };
        let deadline = Instant::now() + Duration::from_secs(15);
        while !root.join("ready").exists() {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(10));
        }
        fs::remove_file(&launcher).unwrap();
        fs::write(root.join("unlinked"), b"unlinked").unwrap();
        assert!(child.wait().unwrap().success());
        assert!(!Path::new(&format!("/proc/{}/exe", child.id())).exists());
        let retained = fs::read_to_string(root.join("retained")).unwrap();
        assert!(Command::new(retained)
            .args(["--exact", test, "--nocapture"])
            .env(CHILD, root)
            .env("SUSI_RETAINED_REENTRY", "1")
            .status()
            .unwrap()
            .success());
        assert_eq!(fs::read(root.join("reentered")).unwrap(), b"original image");
    }
}
