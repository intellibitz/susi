//! Native, no-overwrite installation of the executable the user launched.
use sha2::{Digest, Sha256};
use std::{
    env, fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
fn digest(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut bytes = [0; 65536];
    loop {
        let n = file.read(&mut bytes)?;
        if n == 0 {
            break;
        }
        hash.update(&bytes[..n]);
    }
    Ok(hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}
fn directory(path: &Path) -> Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|p| *p != path && !p.as_os_str().is_empty())
    {
        directory(parent)?;
    }
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            let redirected = meta.file_type().is_symlink();
            #[cfg(windows)]
            let redirected = {
                use std::os::windows::fs::MetadataExt;
                redirected || meta.file_attributes() & 0x400 != 0
            };
            if redirected || !meta.is_dir() {
                return Err(format!(
                    "installation directory is redirected or not a directory: {}",
                    path.display()
                )
                .into());
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => fs::create_dir(path)?,
        Err(error) => return Err(error.into()),
    }
    Ok(())
}
fn install(source: &Path, root: &Path) -> Result<PathBuf> {
    if !root.is_absolute() {
        return Err("installation home must be absolute".into());
    }
    directory(root)?;
    let bin = root.join("bin");
    directory(&bin)?;
    let lock_path = root.join("native-install.lock");
    if let Ok(meta) = fs::symlink_metadata(&lock_path) {
        if meta.file_type().is_symlink() || !meta.is_file() {
            return Err("unsafe installation lock".into());
        }
    }
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)?;
    lock.lock()?;
    let destination = bin.join(if cfg!(windows) { "susi.exe" } else { "susi" });
    let expected = digest(source)?;
    if let Ok(meta) = fs::symlink_metadata(&destination) {
        if !meta.is_file() || meta.file_type().is_symlink() || digest(&destination)? != expected {
            return Err("another installed executable exists; use the verified local release promotion workflow, which preserves the running version".into());
        }
        return Ok(destination);
    }
    let staging = bin.join(format!(".susi-install-{}.exe", std::process::id()));
    let mut staged = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&staging)?;
    let mut original = fs::File::open(source)?;
    std::io::copy(&mut original, &mut staged)?;
    staged.sync_all()?;
    drop(staged);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&staging, fs::Permissions::from_mode(0o755))?;
    }
    if digest(&staging)? != expected {
        return Err("installation copy digest mismatch; candidate retained".into());
    }
    let version = Command::new(&staging).arg("--version").output()?;
    let reported = String::from_utf8(version.stdout)?;
    if !version.status.success() || reported.trim() != format!("susi {}", susi::SUSI_VERSION) {
        return Err(format!(
            "installed candidate version check failed: {}",
            reported.trim()
        )
        .into());
    }
    let receipt = serde_json::json!({"schema":"susi-native-install-v1", "source":source, "sha256":expected, "bytes":fs::metadata(&staging)?.len(), "version":susi::SUSI_VERSION, "class":if cfg!(debug_assertions) { "debug-candidate" } else { "production-candidate" }, "production_qualified":false});
    let receipt_path = root.join("native-install.json");
    if receipt_path.exists() {
        return Err("installation receipt already exists without its executable; preserving state for recovery".into());
    }
    let mut proof = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&receipt_path)?;
    proof.write_all(serde_json::to_string_pretty(&receipt)?.as_bytes())?;
    proof.sync_all()?;
    // Same-volume hard-link creation is atomic and never replaces a target.
    fs::hard_link(&staging, &destination)?;
    fs::remove_file(&staging)?;
    Ok(destination)
}
pub(crate) fn execute() -> ExitCode {
    let result = (|| -> Result<PathBuf> {
        let root = match env::var_os("SUSI_HOME") {
            Some(value) => PathBuf::from(value),
            None => crate::get_home_dir().join(".susi"),
        };
        install(&env::current_exe()?, &root)
    })();
    match result {
        Ok(path) => {
            println!("Susi installed: {}", path.display());
            println!("Open Susi and enter your first goal. A compatible licensed starter model downloads automatically when needed.");
            if cfg!(debug_assertions) {
                println!("This is a debug candidate; production qualification and promotion remain separate.");
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("Native installation failed: {error}");
            ExitCode::FAILURE
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_candidate_is_verified_installed_and_idempotent() {
        let root = env::temp_dir().join(format!(
            "susi-native-install-success-{}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        let source = root.join("candidate.exe");
        let rust_source = root.join("candidate.rs");
        fs::write(
            &rust_source,
            format!("fn main() {{ println!(\"susi {}\"); }}", susi::SUSI_VERSION),
        )
        .unwrap();
        assert!(Command::new("rustc")
            .args(["--edition", "2021"])
            .arg(&rust_source)
            .arg("-o")
            .arg(&source)
            .status()
            .unwrap()
            .success());
        let home = root.join("home");
        let installed = install(&source, &home).unwrap();
        assert_eq!(digest(&installed).unwrap(), digest(&source).unwrap());
        assert_eq!(install(&source, &home).unwrap(), installed);
        let receipt: serde_json::Value =
            serde_json::from_slice(&fs::read(home.join("native-install.json")).unwrap()).unwrap();
        assert_eq!(receipt["production_qualified"], false);
        assert_eq!(receipt["sha256"], digest(&installed).unwrap());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn different_existing_binary_is_never_replaced() {
        let root = env::temp_dir().join(format!(
            "susi-native-install-refusal-{}",
            std::process::id()
        ));
        fs::create_dir_all(root.join("bin")).unwrap();
        let existing = root
            .join("bin")
            .join(if cfg!(windows) { "susi.exe" } else { "susi" });
        fs::write(&existing, b"existing production").unwrap();
        assert!(install(&env::current_exe().unwrap(), &root).is_err());
        assert_eq!(fs::read(&existing).unwrap(), b"existing production");
        fs::remove_dir_all(root).unwrap();
    }
}
