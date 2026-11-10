//! A custom sysroot is selected only after this native tool executes the tracked
//! builder and holds its observed output in memory through compilation. Prototype
//! receipts are build diagnostics, never an import/approval mechanism.
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
#[path = "../../toolchain/windows-stdlib/native_target.rs"]
mod native_target;

fn target(compiler: &str) -> Result<&str> {
    let host = compiler
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .ok_or("missing compiler host")?;
    if !native_target::supported(host) {
        return Err("no native sysroot contract for compiler host".into());
    }
    Ok(host)
}
const INPUTS: &[&str] = &[
    "builder.rs",
    "native_target.rs",
    "platform_unicode.rs",
    "consumer.rs",
    "Cargo.toml",
    "Cargo.lock",
];

fn git(root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git").current_dir(root).args(args).output()?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}
fn hash_file(path: &Path) -> Result<String> {
    Ok(super::sysroot_sources::hash(&fs::read(path)?))
}
fn compiler(root: &Path) -> Result<(String, PathBuf)> {
    let mut command = Command::new("rustc");
    super::compiler_env::apply(&mut command);
    let output = command.current_dir(root).arg("-vV").output()?;
    let text = String::from_utf8(output.stdout)?;
    if !output.status.success()
        || !text.contains("commit-hash: b940084d7eb6a299eb4bfeb8e34901bc051e7ac4\n")
        || target(&text).is_err()
    {
        return Err("custom sysroot requires reviewed native compiler".into());
    }
    let mut locate = Command::new("rustc");
    super::compiler_env::apply(&mut locate);
    let output = locate
        .current_dir(root)
        .args(["--print", "sysroot"])
        .output()?;
    if !output.status.success() {
        return Err("cannot identify compiler sysroot".into());
    }
    Ok((
        text,
        PathBuf::from(String::from_utf8(output.stdout)?.trim()),
    ))
}
fn identity(root: &Path, compiler: &str) -> Result<String> {
    let mut bytes = compiler.as_bytes().to_vec();
    bytes.extend_from_slice(include_bytes!("rust-src-1.99.0-authenticated.json"));
    for input in INPUTS {
        bytes.extend_from_slice(&fs::read(
            root.join("toolchain/windows-stdlib").join(input),
        )?);
    }
    Ok(super::sysroot_sources::hash(&bytes))
}
fn artifacts(destination: &Path, host: &str) -> Result<Value> {
    let mut files = Vec::new();
    for entry in fs::read_dir(
        destination
            .join("sysroot/lib/rustlib")
            .join(host)
            .join("lib"),
    )? {
        let entry = entry?;
        if !entry.file_type()?.is_file() || entry.path().extension().is_none_or(|ext| ext != "rlib")
        {
            return Err("unadmitted sysroot artifact".into());
        }
        files.push(json!({"filename":entry.file_name().to_str().ok_or("nonUTF8 artifact")?, "sha256":hash_file(&entry.path())?}));
    }
    files.sort_by_key(|value| value["filename"].as_str().unwrap_or_default().to_owned());
    if files.len()
        != if host.ends_with("-windows-msvc") {
            20
        } else {
            26
        }
    {
        return Err(
            "native sysroot archive count differs from the reviewed target contract".into(),
        );
    }
    for required in [
        "core",
        "alloc",
        "std",
        "compiler_builtins",
        "panic_unwind",
        "proc_macro",
        "test",
        "unwind",
        "sysroot",
    ] {
        let prefix = format!("lib{required}-");
        if files
            .iter()
            .filter(|f| {
                f["filename"].as_str().is_some_and(|name| {
                    name.starts_with(&prefix) || name == format!("lib{required}.rlib")
                })
            })
            .count()
            != 1
        {
            return Err(format!("missing or duplicate native sysroot archive: {required}").into());
        }
    }
    Ok(Value::Array(files))
}
fn source_check(root: &Path, destination: &Path, installed: &Path) -> Result<()> {
    super::sysroot_sources::replacement(
        &destination.join("source/library"),
        &installed.join("lib/rustlib/src/rust/library"),
        &root.join("toolchain/windows-stdlib/platform_unicode.rs"),
    )?;
    Ok(())
}

/// Held across all compiler subprocesses for one acceptance operation. Windows
/// read handles deny concurrent write/delete; post-build hashes remain required.
pub(crate) struct VerifiedSysroot {
    path: PathBuf,
    expected: Value,
    _handles: Vec<fs::File>,
    pub(crate) evidence: Value,
}
impl VerifiedSysroot {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
    pub(crate) fn apply(&self, command: &mut Command) -> Result<()> {
        super::compiler_env::apply(command);
        let path = self.path.to_str().ok_or("nonUTF8 selected sysroot")?;
        if path.contains('\x1f') {
            return Err("invalid sysroot path".into());
        }
        let host = target(
            self.evidence["compiler"]
                .as_str()
                .ok_or("missing compiler evidence")?,
        )?;
        if host.ends_with("-unknown-linux-gnu") {
            super::compiler_env::configure_system_openssl(command, host)?;
        }
        command.env("SUSI_VERIFIED_SYSROOT", path);
        command.env("CARGO_ENCODED_RUSTFLAGS", format!("--sysroot\x1f{path}"));
        command.env("CARGO_ENCODED_RUSTDOCFLAGS", format!("--sysroot\x1f{path}"));
        Ok(())
    }
    pub(crate) fn recheck(&self) -> Result<()> {
        if self.evidence["artifacts"] != self.expected
            || artifacts(
                self.path().parent().ok_or("sysroot parent missing")?,
                target(
                    self.evidence["compiler"]
                        .as_str()
                        .ok_or("missing compiler evidence")?,
                )?,
            )? != self.expected
        {
            return Err("sysroot artifacts changed during compiler operation".into());
        }
        Ok(())
    }
}
fn reject_redirects(path: &Path) -> Result<()> {
    for ancestor in path.ancestors() {
        let metadata = fs::symlink_metadata(ancestor)?;
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes() & 0x400 != 0 {
                return Err("sysroot ancestor reparse point is forbidden".into());
            }
        }
        if metadata.file_type().is_symlink() {
            return Err("sysroot ancestor symlink is forbidden".into());
        }
    }
    Ok(())
}
#[allow(clippy::too_many_arguments)] // binds all verified sysroot admission inputs in one helper
fn admit_destination(
    root: &Path,
    destination: &Path,
    installed: &Path,
    compiler: &str,
    host: &str,
    key: &str,
    started: std::time::Instant,
    cache_hit: bool,
) -> Result<VerifiedSysroot> {
    source_check(root, destination, installed)?;
    if identity(root, compiler)? != key {
        return Err("sysroot builder source changed during execution".into());
    }
    let expected = artifacts(destination, host)?;
    let mut handles = Vec::new();
    let path = destination.join("sysroot");
    for entry in expected.as_array().ok_or("invalid artifact inventory")? {
        let artifact = path
            .join("lib/rustlib")
            .join(host)
            .join("lib")
            .join(entry["filename"].as_str().ok_or("missing filename")?);
        reject_redirects(&artifact)?;
        let mut options = fs::OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(1); // FILE_SHARE_READ only: prohibit writes/deletes.
        }
        handles.push(options.open(&artifact)?);
    }

    if artifacts(destination, host)? != expected {
        return Err("sysroot changed before archive locks were acquired".into());
    }
    let mut evidence = serde_json::Map::new();
    for name in [
        "selected-dependencies.txt",
        "build-artifacts.jsonl",
        "COPYRIGHT-library.upstream.html",
        "prototype-receipt.json",
    ] {
        evidence.insert(name.into(), json!(hash_file(&destination.join(name))?));
    }
    let record = json!({"schema":"susi/executed-sysroot/v2", "cache_hit":cache_hit, "elapsed_ms":started.elapsed().as_millis(), "identity":key,"compiler":compiler,"destination":destination,"artifacts":expected,"evidence":evidence,"authenticated_source_manifest_sha256":super::sysroot_sources::hash(include_bytes!("rust-src-1.99.0-authenticated.json")),"selected_dependencies":fs::read_to_string(destination.join("selected-dependencies.txt"))?,"upstream_copyright":fs::read_to_string(destination.join("COPYRIGHT-library.upstream.html"))?,"source_delta":"core Unicode tables replaced by tracked native OS backend; exactly3 char const APIs made runtime-only; all other authenticated files unchanged"});
    if !cache_hit {
        fs::write(
            destination.join("executed-record.json"),
            serde_json::to_vec_pretty(&record)?,
        )?;
    }
    let verified = VerifiedSysroot {
        path,
        expected,
        _handles: handles,
        evidence: record,
    };
    verified.recheck()?;
    Ok(verified)
}

// Recompile from the tracked consumer instead of trusting an old executable.
// The sysroot identity covers source/archives, but not the current OS ICU.
fn qualify_consumer(root: &Path, destination: &Path, host: &str, output: &Path) -> Result<()> {
    let executable = output.join(native_target::executable(host, "consumer"));
    let mut consumer = Command::new("rustc");
    super::compiler_env::apply(&mut consumer);
    let mut execute = Command::new(&executable);
    super::compiler_env::apply(&mut execute);
    if !consumer
        .current_dir(root)
        .arg("toolchain/windows-stdlib/consumer.rs")
        .args(["--test", "--edition", "2021", "--sysroot"])
        .arg(destination.join("sysroot"))
        .arg("-o")
        .arg(&executable)
        .status()?
        .success()
        || !execute.status()?.success()
    {
        return Err("custom sysroot stable consumer qualification failed".into());
    }
    Ok(())
}

/// Private build directory for one builder run. rustc and link.exe write
/// import libraries about 90 characters deep inside it
/// (`build\x86_64-pc-windows-msvc\dist\deps\rustcXXXXXX\api-ms-win-core-synch-l1-2-0.dll_imports.lib`),
/// and link.exe does not accept paths over the 260-character Windows limit.
/// The full 64-hex key plus the nonce put that path at 263 characters under
/// `<checkout>\.git\susi-product-sysroots\`, so the key is shortened here.
/// The key still names the admitted destination, and the nonce keeps
/// concurrent builders apart.
fn staging_name(key: &str, nonce: u128) -> String {
    format!("{}-s{nonce}", key.get(..12).unwrap_or(key))
}

pub(crate) fn prepare(root: &Path) -> Result<VerifiedSysroot> {
    let started = std::time::Instant::now();
    let (compiler, installed) = compiler(root)?;
    let host = target(&compiler)?;
    super::sysroot_sources::original(&installed.join("lib/rustlib/src/rust/library"))?;
    let key = identity(root, &compiler)?;
    let common = PathBuf::from(git(
        root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?);
    let storage = common.join("susi-product-sysroots");
    fs::create_dir_all(&storage)?;
    let destination = storage.join(&key);
    if destination.join("executed-record.json").is_file() {
        if let Ok(mut verified) = admit_destination(
            root,
            &destination,
            &installed,
            &compiler,
            host,
            &key,
            started,
            true,
        ) {
            if host.ends_with("-unknown-linux-gnu") {
                // OS ICU may change without changing the compiler or source key.
                // Private output avoids concurrent cache readers replacing each
                // other's consumer. A failed recheck is an error, not a cache miss.
                let nonce = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)?
                    .as_nanos();
                let output = storage.join(format!("consumer-check-{}-{nonce}", std::process::id()));
                fs::create_dir(&output)?;
                reject_redirects(&output)?;
                qualify_consumer(root, &destination, host, &output)?;
                if identity(root, &compiler)? != key {
                    return Err("sysroot consumer source changed during recheck".into());
                }
                verified.recheck()?;
                verified.evidence["consumer_rechecked"] = json!(true);
                verified.evidence["elapsed_ms"] = json!(started.elapsed().as_millis());
                fs::remove_dir_all(&output)?;
            }
            return Ok(verified);
        }
    }
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let staging = storage.join(staging_name(&key, nonce));
    let target = storage.join("builder-target");
    let mut command = Command::new("cargo");
    super::compiler_env::apply(&mut command);
    let status = command
        .current_dir(root)
        .args([
            "run",
            "--offline",
            "--locked",
            "--manifest-path",
            "toolchain/windows-stdlib/Cargo.toml",
            "--target-dir",
        ])
        .arg(&target)
        .env("CARGO_TARGET_DIR", &target)
        .env("CARGO_BUILD_TARGET_DIR", &target)
        .env("CARGO_BUILD_BUILD_DIR", &target)
        .arg("--")
        .arg(&staging)
        .status()?;
    if !status.success() {
        return Err("tracked sysroot builder failed; private evidence retained".into());
    }
    // Stable consumer execution, not an imported receipt, is required.
    qualify_consumer(root, &staging, host, &staging)?;
    let _ = admit_destination(
        root, &staging, &installed, &compiler, host, &key, started, false,
    )?;
    if !destination.exists() && fs::rename(&staging, &destination).is_err() && !destination.exists()
    {
        return admit_destination(
            root, &staging, &installed, &compiler, host, &key, started, false,
        );
    }
    if staging.exists() {
        let _ = fs::remove_dir_all(&staging);
    }
    admit_destination(
        root,
        &destination,
        &installed,
        &compiler,
        host,
        &key,
        started,
        false,
    )
}

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    use super::*;
    #[test]
    fn staging_directory_name_is_short_and_unique_per_nonce() {
        let key = "19223169d1b9a9ae6d37ce16977f96efb8f6b6f8e3cb27a8bc266af2aa3e68a5";
        let nonce = 1_794_392_993_025_426_200_u128;
        let name = super::staging_name(key, nonce);
        assert_eq!(name, "19223169d1b9-s1794392993025426200");
        // The old 92-character name gave a 263-character link path on the
        // CEO laptop; this one is 33 characters (nanosecond nonces stay at
        // 19 digits until 2286), so the same path is 204 characters.
        assert!(name.len() <= 40, "{name}");
        assert_ne!(name, super::staging_name(key, nonce + 1));
        assert_eq!(super::staging_name("abc", 7), "abc-s7");
    }
    #[test]
    fn native_compiler_selection_rejects_foreign_or_unknown_targets() {
        assert_eq!(
            super::target("host: x86_64-unknown-linux-gnu\n").unwrap(),
            "x86_64-unknown-linux-gnu"
        );
        assert!(super::target("host: aarch64-unknown-linux-gnu\n").is_ok());
        assert!(super::target("host: x86_64-pc-windows-msvc\n").is_ok());
        assert!(super::target("host: aarch64-pc-windows-msvc\n").is_ok());
        assert!(super::target("host: i686-pc-windows-msvc\n").is_err());
        assert!(super::target("host: wasm32-wasip1\n").is_err());
        assert!(super::target("host: x86_64-unknown-linux-musl\n").is_err());
    }
    #[cfg(windows)]
    #[test]
    fn archive_guard_denies_mutation_until_compilation_finishes() {
        use std::os::windows::fs::OpenOptionsExt;
        let directory =
            std::env::temp_dir().join(format!("susi-archive-lock-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("core.rlib");
        fs::write(&path, b"observed compiler output").unwrap();
        reject_redirects(&path).unwrap();
        let guard = fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&path)
            .unwrap();
        assert!(fs::write(&path, b"replacement").is_err());
        assert!(fs::remove_file(&path).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"observed compiler output");
        drop(guard);
        fs::write(&path, b"after operation").unwrap();
        fs::remove_file(path).unwrap();
        fs::remove_dir(directory).unwrap();
    }
}
