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
/// Use the shared scrubber on a non-tool carrier. The selected executables
/// below are already absolute, so rediscovery through a PATH proxy is unnecessary.
fn bind(command: &mut Command, installed: &Path) -> Result<()> {
    let mut policy = Command::new("stdlib-policy");
    super::compiler_env::apply(&mut policy);
    for (key, value) in policy.get_envs() {
        if let Some(value) = value {
            command.env(key, value);
        } else {
            command.env_remove(key);
        }
    }
    let bin = installed.join("bin");
    let path = std::env::var_os("PATH").unwrap_or_default();
    command
        .env(
            "PATH",
            std::env::join_paths(std::iter::once(bin.clone()).chain(std::env::split_paths(&path)))?,
        )
        .env("RUSTC", bin.join("rustc.exe"))
        .env("RUSTDOC", bin.join("rustdoc.exe"));
    Ok(())
}
fn compiler(root: &Path) -> Result<(String, PathBuf)> {
    // The platform boundary is the installed pinned toolchain, never a PATH
    // proxy. Validate the actual driver and the sysroot reported by that tool.
    let home = std::env::var_os("RUSTUP_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(|p| PathBuf::from(p).join(".rustup")))
        .ok_or("cannot locate installed native toolchain")?;
    let expected = home
        .join("toolchains")
        .join(format!("1.99.0-{TARGET}"))
        .canonicalize()?;
    let rustc = expected.join("bin/rustc.exe");
    let checked = |args: &[&str]| -> Result<String> {
        let mut command = Command::new(&rustc);
        bind(&mut command, &expected)?;
        let output = command.current_dir(root).args(args).output()?;
        if !output.status.success() {
            return Err("pinned compiler refused identification".into());
        }
        Ok(String::from_utf8(output.stdout)?)
    };
    let version = checked(&["-vV"])?;
    if !version.lines().any(|s| s == "release: 1.99.0")
        || !version
            .lines()
            .any(|s| s == "commit-hash: b940084d7eb6a299eb4bfeb8e34901bc051e7ac4")
        || !version.lines().any(|s| s == format!("host: {TARGET}"))
    {
        return Err("custom sysroot requires reviewed native compiler".into());
    }
    let installed = PathBuf::from(checked(&["--print", "sysroot"])?.trim());
    if installed.canonicalize()? != expected {
        return Err("compiler selected a different sysroot".into());
    }
    Ok((version, installed))
}
/// Ask the selected MSVC linker for the libraries it actually consumed. The
/// repro contains a copy of its environment, so use a minimal platform-only
/// environment and remove the private probe after hashing.
fn native_inputs(root: &Path, installed: &Path) -> Result<Vec<(String, String)>> {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let dir =
        std::env::temp_dir().join(format!("susi-native-inputs-{}-{nonce}", std::process::id()));
    fs::create_dir(&dir)?;
    let result = (|| -> Result<Vec<(String, String)>> {
        let mut probe = Command::new(installed.join("bin/rustc.exe"));
        probe.env_clear();
        for name in [
            "SystemRoot",
            "SystemDrive",
            "ProgramFiles",
            "ProgramFiles(x86)",
            "ProgramData",
            "PATH",
            "TEMP",
            "TMP",
            "USERPROFILE",
            "LOCALAPPDATA",
            "COMSPEC",
            "LIB",
            "LIBPATH",
            "INCLUDE",
            "VCToolsInstallDir",
            "VCINSTALLDIR",
            "VSINSTALLDIR",
            "WindowsSdkDir",
            "WindowsSDKVersion",
            "UniversalCRTSdkDir",
            "UCRTVersion",
        ] {
            if let Some(value) = std::env::var_os(name) {
                probe.env(name, value);
            }
        }
        let output = probe
            .current_dir(root)
            .args([
                "--edition",
                "2021",
                "--test",
                "toolchain/windows-stdlib/consumer.rs",
                "--print",
                "link-args",
                "-C",
            ])
            .arg(format!("link-arg=/LINKREPRO:{}", dir.display()))
            .arg("-o")
            .arg(dir.join("probe.exe"))
            .output()?;
        if !output.status.success() {
            return Err("native linker input probe failed".into());
        }
        let linker = serde_json::Deserializer::from_slice(&output.stdout)
            .into_iter::<String>()
            .next()
            .ok_or("missing resolved linker")??;
        let bin = Path::new(&linker)
            .parent()
            .ok_or("invalid resolved linker")?;
        let mut result = Vec::new();
        for entry in fs::read_dir(bin)? {
            let path = entry?.path();
            if path
                .extension()
                .is_some_and(|s| s.eq_ignore_ascii_case("exe") || s.eq_ignore_ascii_case("dll"))
            {
                result.push((
                    format!(
                        "msvc/{}",
                        path.file_name()
                            .ok_or("missing tool name")?
                            .to_string_lossy()
                    ),
                    hash_file(&path)?,
                ));
            }
        }
        let mut imports = 0;
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
            if path
                .extension()
                .is_some_and(|s| s.eq_ignore_ascii_case("lib") || s.eq_ignore_ascii_case("rlib"))
            {
                imports += usize::from(
                    path.extension()
                        .is_some_and(|s| s.eq_ignore_ascii_case("lib")),
                );
                result.push((
                    format!(
                        "native/{}",
                        path.file_name()
                            .ok_or("missing library name")?
                            .to_string_lossy()
                    ),
                    hash_file(&path)?,
                ));
            }
        }
        if imports == 0 {
            return Err("no native link inputs observed".into());
        }
        Ok(result)
    })();
    fs::remove_dir_all(dir)?;
    result
}
fn identity(root: &Path, compiler: &str, installed: &Path) -> Result<(String, bool)> {
    let mut inputs = vec![
        ("compiler".to_owned(), compiler.to_owned()),
        (
            "manifest".into(),
            super::sysroot_sources::hash(include_bytes!("rust-src-1.99.0-authenticated.json")),
        ),
    ];
    // Source, compiled policy and selected configuration are separate inputs:
    // an older installed verifier must not approve an edited checkout.
    for (name, compiled) in [
        (
            "xtask/src/trusted_sysroot.rs",
            include_bytes!("trusted_sysroot.rs").as_slice(),
        ),
        (
            "xtask/src/stdlib_cache.rs",
            include_bytes!("stdlib_cache.rs").as_slice(),
        ),
        (
            "xtask/src/compiler_env.rs",
            include_bytes!("compiler_env.rs").as_slice(),
        ),
        (
            "xtask/src/sysroot_sources.rs",
            include_bytes!("sysroot_sources.rs").as_slice(),
        ),
        (
            "crates/susi-vendor-winapi/src/sealed_directory.rs",
            include_bytes!("../../crates/susi-vendor-winapi/src/sealed_directory.rs").as_slice(),
        ),
    ] {
        if fs::read(root.join(name))? != compiled {
            return Err("installed verifier policy differs from checkout".into());
        }
        inputs.push((name.into(), super::sysroot_sources::hash(compiled)));
    }
    for input in INPUTS {
        inputs.push((
            input.to_string(),
            hash_file(&root.join("toolchain/windows-stdlib").join(input))?,
        ));
    }
    for file in [
        "Cargo.lock",
        "xtask/Cargo.toml",
        "crates/susi-vendor-winapi/Cargo.toml",
        "crates/susi-vendor-winapi/src/lib.rs",
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
        "bin/rustdoc.exe",
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
    for (index, path) in configs(root)?.iter().enumerate() {
        let value = match fs::read(path) {
            Ok(bytes) => super::sysroot_sources::hash(&bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e.into()),
        };
        inputs.push((format!("cargo-config-{index}"), value));
    }
    for (name, value) in std::env::vars_os() {
        let name = name.to_string_lossy().to_ascii_uppercase();
        if name.starts_with("CARGO_PROFILE_")
            || name.starts_with("CARGO_TARGET_") && !name.ends_with("_DIR")
            || name.starts_with("CARGO_SOURCE_")
            || name.starts_with("CARGO_REGISTRIES_") && !name.ends_with("_TOKEN")
            || [
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
    let eligible = match native_inputs(root, installed) {
        Ok(native) => {
            inputs.extend(native);
            true
        }
        Err(error) => {
            eprintln!("stdlib reuse unavailable: {error}");
            false
        }
    };
    inputs.sort();
    Ok((
        super::sysroot_sources::hash(&serde_json::to_vec(&inputs)?),
        eligible,
    ))
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
struct InputGuard {
    root: PathBuf,
    installed: PathBuf,
    compiler: String,
    key: String,
    eligible: bool,
    _files: Vec<fs::File>,
}

fn read_guard(path: &Path) -> Result<fs::File> {
    reject_redirects(path)?;
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(1);
    }
    Ok(options.open(path)?)
}

fn guard_source_tree(path: &Path, handles: &mut Vec<fs::File>) -> Result<()> {
    reject_redirects(path)?;
    #[cfg(windows)]
    handles.push(cache::anchor(path)?);
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            guard_source_tree(&entry.path(), handles)?;
        } else if entry.file_type()?.is_file() {
            handles.push(read_guard(&entry.path())?);
        } else {
            return Err("redirected or nonregular stdlib input".into());
        }
    }
    Ok(())
}

impl InputGuard {
    fn new(root: &Path, installed: &Path, compiler: &str) -> Result<Self> {
        let library = installed.join("lib/rustlib/src/rust/library");
        let mut files = Vec::new();
        #[cfg(windows)]
        {
            let mut anchors = std::collections::BTreeSet::new();
            let mut dirs = vec![
                installed.join("bin"),
                library.clone(),
                root.join("toolchain/windows-stdlib"),
                root.join("xtask/src"),
                root.join("crates/susi-vendor-winapi/src"),
                root.join("crates/susi-vendor-sha2/src"),
                root.join("crates/susi-vendor-serde-json/src"),
                installed.join("share/doc/rust"),
            ];
            for path in configs(root)? {
                if let Some(parent) = path.parent() {
                    dirs.push(parent.to_owned());
                }
            }
            for directory in dirs {
                for ancestor in directory.ancestors() {
                    if ancestor.is_dir() && anchors.insert(ancestor.to_owned()) {
                        files.push(cache::anchor(ancestor)?);
                    }
                }
            }
        }
        guard_source_tree(&library, &mut files)?;
        super::sysroot_sources::original(&library)?;
        for name in INPUTS {
            files.push(read_guard(
                &root.join("toolchain/windows-stdlib").join(name),
            )?);
        }
        for name in [
            "bin/rustc.exe",
            "bin/rustdoc.exe",
            "bin/cargo.exe",
            "share/doc/rust/COPYRIGHT-library.html",
        ] {
            files.push(read_guard(&installed.join(name))?);
        }
        for entry in fs::read_dir(installed.join("bin"))? {
            let path = entry?.path();
            if path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("rustc_driver") && n.ends_with(".dll"))
            {
                files.push(read_guard(&path)?);
            }
        }
        for file in [
            "Cargo.lock",
            "xtask/Cargo.toml",
            "crates/susi-vendor-winapi/Cargo.toml",
            "crates/susi-vendor-winapi/src/lib.rs",
            "crates/susi-vendor-sha2/Cargo.toml",
            "crates/susi-vendor-sha2/src/lib.rs",
            "crates/susi-vendor-serde-json/Cargo.toml",
            "crates/susi-vendor-serde-json/src/lib.rs",
            "rust-toolchain.toml",
            "xtask/src/trusted_sysroot.rs",
            "xtask/src/stdlib_cache.rs",
            "crates/susi-vendor-winapi/src/sealed_directory.rs",
            "xtask/src/compiler_env.rs",
            "xtask/src/sysroot_sources.rs",
        ] {
            files.push(read_guard(&root.join(file))?);
        }
        for path in configs(root)? {
            if path.try_exists()? {
                files.push(read_guard(&path)?);
            }
        }
        let (key, eligible) = identity(root, compiler, installed)?;
        Ok(Self {
            root: root.into(),
            installed: installed.into(),
            compiler: compiler.into(),
            key,
            eligible,
            _files: files,
        })
    }
    fn recheck(&self) -> Result<()> {
        super::sysroot_sources::original(&self.installed.join("lib/rustlib/src/rust/library"))?;
        if identity(&self.root, &self.compiler, &self.installed)?
            != (self.key.clone(), self.eligible)
        {
            return Err(
                "compiler, selected configuration or stdlib inputs changed during use".into(),
            );
        }
        Ok(())
    }
}

fn configs(root: &Path) -> Result<Vec<PathBuf>> {
    let cargo_home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(|home| PathBuf::from(home).join(".cargo")))
        .ok_or("cannot identify Cargo home")?;
    let common = PathBuf::from(git(
        root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?);
    let mut dirs: Vec<_> = root.ancestors().map(|p| p.join(".cargo")).collect();
    // The stdlib Cargo subprocess starts under the common-dir staging area.
    for dir in common
        .join("susi-product-sysroots")
        .ancestors()
        .map(|p| p.join(".cargo"))
    {
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    dirs.push(cargo_home);
    Ok(dirs
        .into_iter()
        .flat_map(|p| [p.join("config"), p.join("config.toml")])
        .collect())
}

pub(crate) struct VerifiedSysroot {
    inputs: InputGuard,
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
        bind(command, &self.inputs.installed)?;
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
        self.inputs.recheck()?;
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
    // This future candidate depends on the reviewed count1102 inner-Cargo
    // binding. Never execute the older builder that can unmask a host wrapper.
    if hash_file(&root.join("toolchain/windows-stdlib/builder.rs"))?
        != "5bddfb882ef443e235f8b12d732db5b19aff03daa60e72450980e8fc19229860"
        || super::sysroot_sources::hash(include_bytes!("compiler_env.rs"))
            != "5381095c33854b1ef1c614f8bfab846429fa2ecda820ebf3e2c357e105850058"
    {
        return Err("stdlib candidate requires the reviewed count1102 compiler bindings".into());
    }
    let (compiler, installed) = compiler(root)?;
    let inputs = InputGuard::new(root, &installed, &compiler)?;
    let key = inputs.key.clone();
    let eligible = inputs.eligible;
    let common = PathBuf::from(git(
        root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?);
    let storage = common.join("susi-product-sysroots");
    fs::create_dir_all(&storage)?;
    reject_redirects(&storage)?;
    let hit = if eligible {
        cache::lookup(&storage, &key)?
    } else {
        None
    };
    if let Some((directory, mut evidence)) = hit {
        let expected = evidence["artifacts"].clone();
        let mut handles = cache::hold_namespace(&directory)?;
        handles.extend(lock_archives(&directory, &expected)?);
        evidence["destination"] = json!(directory);
        evidence["cache_hit"] = json!(true);
        evidence["elapsed_ms"] = json!(started.elapsed().as_millis());
        let verified = VerifiedSysroot {
            inputs,
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
    let mut command = Command::new(installed.join("bin/cargo.exe"));
    bind(&mut command, &installed)?;
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
    if identity(root, &compiler, &installed)? != (key.clone(), eligible) {
        return Err("sysroot builder source changed during execution".into());
    }
    let expected = artifacts(&destination)?;
    let handles = lock_archives(&destination, &expected)?;
    let object = cache::store(&storage, &destination, &expected, nonce)?;
    let canonical = storage.join("objects").join(&object);
    let mut canonical_handles = cache::hold_namespace(&canonical)?;
    canonical_handles.extend(lock_archives(&canonical, &expected)?);
    drop(handles);
    // The tracked builder must finish and the reviewed stable consumer must
    // execute before admission; externally supplied receipts cannot enter.
    let executable = destination.join("consumer.exe");
    let mut consumer = Command::new(installed.join("bin/rustc.exe"));
    bind(&mut consumer, &installed)?;
    if !consumer
        .current_dir(root)
        .arg("toolchain/windows-stdlib/consumer.rs")
        .args(["--test", "--edition", "2021", "--sysroot"])
        .arg(canonical.join("sysroot"))
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
    let record = json!({"schema":"susi/executed-sysroot/v3","object":object,"cache_hit":false, "elapsed_ms":started.elapsed().as_millis(), "identity":key,"compiler":compiler,"destination":canonical,"artifacts":expected,"evidence":evidence,"authenticated_source_manifest_sha256":super::sysroot_sources::hash(include_bytes!("rust-src-1.99.0-authenticated.json")),"selected_dependencies":fs::read_to_string(destination.join("selected-dependencies.txt"))?,"upstream_copyright":fs::read_to_string(destination.join("COPYRIGHT-library.upstream.html"))?,"source_delta":"core Unicode tables replaced by tracked Windows OS backend; exactly3 char const APIs made runtime-only; all other authenticated files unchanged"});
    fs::write(
        destination.join("executed-record.json"),
        serde_json::to_vec_pretty(&record)?,
    )?;
    let verified = VerifiedSysroot {
        inputs,
        path: canonical.join("sysroot"),
        expected,
        _handles: canonical_handles,
        evidence: record,
    };
    verified.recheck()?;
    cache::publish(&storage, &verified.evidence, nonce)?;
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
