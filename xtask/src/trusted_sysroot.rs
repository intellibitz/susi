//! A sysroot must originate in a locally executed and qualified build. Its immutable
//! admission is held separately from cache files. Read handles guard selected archives
//! through compilation and the complete compiler-visible inventory is rechecked.
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
#[path = "stdlib_cache.rs"]
mod cache;
const TARGET: &str = "x86_64-pc-windows-msvc";
const INPUTS: &[&str] = &[
    "builder.rs",
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
        || !text.contains(&format!("host: {TARGET}\n"))
    {
        return Err("custom sysroot requires reviewed native compiler".into());
    }
    let output = Command::new("rustc")
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
fn identity(root: &Path, compiler: &str, installed: &Path) -> Result<String> {
    let mut inputs = vec![
        ("compiler".to_owned(), compiler.to_owned()),
        (
            "manifest".into(),
            super::sysroot_sources::hash(include_bytes!("rust-src-1.99.0-authenticated.json")),
        ),
    ];
    for input in INPUTS {
        inputs.push((
            input.to_string(),
            hash_file(&root.join("toolchain/windows-stdlib").join(input))?,
        ));
    }
    for file in [
        "crates/susi-vendor-sha2/Cargo.toml",
        "crates/susi-vendor-sha2/src/lib.rs",
        "crates/susi-vendor-serde-json/Cargo.toml",
        "crates/susi-vendor-serde-json/src/lib.rs",
        ".cargo/config.toml",
        "rust-toolchain.toml",
        "xtask/src/compiler_env.rs",
        "xtask/src/sysroot_sources.rs",
    ] {
        inputs.push((file.into(), hash_file(&root.join(file))?));
    }
    for file in [
        "bin/rustc.exe",
        "bin/cargo.exe",
        "share/doc/rust/COPYRIGHT-library.html",
    ] {
        inputs.push((file.into(), hash_file(&installed.join(file))?));
    }
    for entry in fs::read_dir(installed.join("bin"))? {
        let path = entry?.path();
        if path
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.starts_with("rustc_driver") && s.ends_with(".dll"))
        {
            inputs.push(("rustc driver".into(), hash_file(&path)?));
        }
    }
    // Cargo reads configuration from the invocation directory, its parents
    // and CARGO_HOME. Bind effective config bytes, including an absent file.
    let cargo_home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(|home| PathBuf::from(home).join(".cargo")))
        .ok_or("cannot identify Cargo home")?;
    let mut dirs: Vec<_> = root.ancestors().map(|p| p.join(".cargo")).collect();
    dirs.push(cargo_home);
    for (index, dir) in dirs.iter().enumerate() {
        for file in ["config", "config.toml"] {
            let value = match fs::read(dir.join(file)) {
                Ok(bytes) => super::sysroot_sources::hash(&bytes),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
                Err(err) => return Err(err.into()),
            };
            inputs.push((format!("cargo-config-{index}/{file}"), value));
        }
    }
    for (name, value) in std::env::vars_os() {
        let name = name.to_string_lossy().to_ascii_uppercase();
        if name.starts_with("CARGO_PROFILE_")
            || name.starts_with("CARGO_TARGET_") && !name.ends_with("_DIR")
            || name.starts_with("CARGO_SOURCE_")
            || name.starts_with("CARGO_REGISTRIES_") && !name.ends_with("_TOKEN")
            || [
                "PATH",
                "LIB",
                "LIBPATH",
                "LINK",
                "_LINK_",
                "INCLUDE",
                "VCTOOLSINSTALLDIR",
                "WINDOWSSDKDIR",
                "WINDOWSSDKVERSION",
                "VCTOOLSVERSION",
                "RUSTUP_TOOLCHAIN",
            ]
            .contains(&name.as_str())
        {
            inputs.push((name, super::sysroot_sources::hash(value.as_encoded_bytes())));
        }
    }
    inputs.sort();
    Ok(super::sysroot_sources::hash(&serde_json::to_vec(&inputs)?))
}
fn artifacts(destination: &Path) -> Result<Value> {
    let mut files = Vec::new();
    for entry in fs::read_dir(
        destination
            .join("sysroot/lib/rustlib")
            .join(TARGET)
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
    if files.len() != 20 {
        return Err("reviewed Windows sysroot requires exactly20 target archives".into());
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
        command.env("SUSI_VERIFIED_SYSROOT", path);
        command.env("CARGO_ENCODED_RUSTFLAGS", format!("--sysroot\x1f{path}"));
        command.env("CARGO_ENCODED_RUSTDOCFLAGS", format!("--sysroot\x1f{path}"));
        Ok(())
    }
    pub(crate) fn recheck(&self) -> Result<()> {
        cache::validate_layout(self.path().parent().ok_or("sysroot parent missing")?)?;
        if self.evidence["artifacts"] != self.expected
            || artifacts(self.path().parent().ok_or("sysroot parent missing")?)? != self.expected
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
fn lock_archives(destination: &Path, expected: &Value) -> Result<Vec<fs::File>> {
    let mut handles = Vec::new();
    let path = destination.join("sysroot");
    for entry in expected.as_array().ok_or("invalid artifact inventory")? {
        let artifact = path
            .join("lib/rustlib")
            .join(TARGET)
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

    if artifacts(destination)? != *expected {
        return Err("sysroot changed before archive locks were acquired".into());
    }
    Ok(handles)
}

pub(crate) fn prepare(root: &Path) -> Result<VerifiedSysroot> {
    let started = std::time::Instant::now();
    let (compiler, installed) = compiler(root)?;
    super::sysroot_sources::original(&installed.join("lib/rustlib/src/rust/library"))?;
    let key = identity(root, &compiler, &installed)?;
    let common = PathBuf::from(git(
        root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?);
    let storage = common.join("susi-product-sysroots");
    fs::create_dir_all(&storage)?;
    reject_redirects(&storage)?;
    if let Some((directory, mut evidence)) = cache::lookup(root, &storage, &key)? {
        let expected = evidence["artifacts"].clone();
        let handles = lock_archives(&directory, &expected)?;
        evidence["destination"] = json!(directory);
        evidence["cache_hit"] = json!(true);
        evidence["elapsed_ms"] = json!(started.elapsed().as_millis());
        let verified = VerifiedSysroot {
            path: directory.join("sysroot"),
            expected,
            _handles: handles,
            evidence,
        };
        verified.recheck()?;
        return Ok(verified);
    }
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let destination = storage.join(format!("b-{nonce}"));
    let target = storage.join(format!("builder-{nonce}"));
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
        .arg(&destination)
        .status()?;
    if !status.success() {
        return Err("tracked sysroot builder failed; private evidence retained".into());
    }
    source_check(root, &destination, &installed)?;
    if identity(root, &compiler, &installed)? != key {
        return Err("sysroot builder source changed during execution".into());
    }
    let expected = artifacts(&destination)?;
    let handles = lock_archives(&destination, &expected)?;
    // The tracked builder must finish and the reviewed stable consumer must
    // execute before admission; externally supplied receipts cannot enter.
    let executable = destination.join("consumer.exe");
    let mut consumer = Command::new("rustc");
    super::compiler_env::apply(&mut consumer);
    if !consumer
        .current_dir(root)
        .arg("toolchain/windows-stdlib/consumer.rs")
        .args(["--test", "--edition", "2021", "--sysroot"])
        .arg(destination.join("sysroot"))
        .arg("-o")
        .arg(&executable)
        .status()?
        .success()
        || !Command::new(&executable).status()?.success()
    {
        return Err("custom sysroot stable consumer qualification failed".into());
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
    let object = cache::store(&storage, &destination, &expected, nonce)?;
    let canonical = storage.join("objects").join(&object);
    let canonical_handles = lock_archives(&canonical, &expected)?;
    drop(handles);
    let record = json!({"schema":"susi/executed-sysroot/v3","object":object,"cache_hit":false, "elapsed_ms":started.elapsed().as_millis(), "identity":key,"compiler":compiler,"destination":canonical,"artifacts":expected,"evidence":evidence,"authenticated_source_manifest_sha256":super::sysroot_sources::hash(include_bytes!("rust-src-1.99.0-authenticated.json")),"selected_dependencies":fs::read_to_string(destination.join("selected-dependencies.txt"))?,"upstream_copyright":fs::read_to_string(destination.join("COPYRIGHT-library.upstream.html"))?,"source_delta":"core Unicode tables replaced by tracked Windows OS backend; exactly3 char const APIs made runtime-only; all other authenticated files unchanged"});
    fs::write(
        destination.join("executed-record.json"),
        serde_json::to_vec_pretty(&record)?,
    )?;
    let verified = VerifiedSysroot {
        path: canonical.join("sysroot"),
        expected,
        _handles: canonical_handles,
        evidence: record,
    };
    verified.recheck()?;
    cache::publish(root, &key, &verified.evidence)?;
    Ok(verified)
}

#[cfg(test)]
mod tests {
    use super::*;
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
