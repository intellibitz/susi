//! Private, pinned native product-stdlib builder. It never edits rustup sources.
#![deny(unsafe_code)]
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    error::Error,
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
type Result<T> = std::result::Result<T, Box<dyn Error>>;
mod native_target;
const COMMIT: &str = "b940084d7eb6a299eb4bfeb8e34901bc051e7ac4";
// This policy lives in builder.rs, an input to the outer sysroot identity.
// Only compiler output paths are virtual. Evidence retains the physical paths.
const VIRTUAL_BUILD_ROOT: &str = "/susi/windows-stdlib";
const REMAP_SCOPE: &str = "--remap-path-scope=all";
const BASE_FLAGS: &str = "-Zforce-unstable-if-unmarked";

fn compiler_flags(physical: &Path) -> Result<String> {
    let path = physical.to_str().ok_or("non-UTF8 stdlib destination")?;
    if path.contains('\x1f') {
        return Err("stdlib destination contains the encoded rustflags separator".into());
    }
    let mut flags = vec![BASE_FLAGS.to_owned(), REMAP_SCOPE.to_owned()];
    // rustc and Cargo can spell Windows paths differently. Cover the native,
    // extended and slash-separated forms of the same canonical directory.
    let canonical = physical.canonicalize()?;
    let extended = canonical.to_str().ok_or("non-UTF8 canonical destination")?;
    let slash = path.replace('\\', "/");
    let mut prefixes = vec![path, extended, slash.as_str()];
    prefixes.sort_unstable();
    prefixes.dedup();
    for from in prefixes {
        flags.push(format!("--remap-path-prefix={from}={VIRTUAL_BUILD_ROOT}"));
    }
    Ok(flags.join("\x1f"))
}

fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn copy_tree(
    source: &Path,
    destination: &Path,
    base: &Path,
    manifest: &mut Vec<Value>,
) -> Result<()> {
    fs::create_dir_all(destination)?;
    let mut entries: Vec<_> = fs::read_dir(source)?.collect::<std::result::Result<_, _>>()?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let metadata = entry.file_type()?;
        if metadata.is_symlink() {
            return Err("source symlink is not admitted".into());
        }
        let path = entry.path();
        let target = destination.join(entry.file_name());
        if metadata.is_dir() {
            copy_tree(&path, &target, base, manifest)?;
        } else if metadata.is_file() {
            let bytes = fs::read(&path)?;
            manifest.push(json!({"path": path.strip_prefix(base)?.to_string_lossy().replace('\\', "/"), "sha256": hash(&bytes)}));
            fs::write(target, bytes)?;
        } else {
            return Err("nonregular source is not admitted".into());
        }
    }
    Ok(())
}

fn controlled(command: &mut Command) -> &mut Command {
    for (name, _) in std::env::vars_os() {
        let upper = name.to_string_lossy().to_ascii_uppercase();
        if (upper.starts_with("RUST") && upper != "RUSTUP_HOME")
            || upper.starts_with("CARGO_TARGET_")
            || upper.starts_with("CARGO_BUILD_")
            || upper.starts_with("CARGO_ENCODED_")
            || matches!(
                upper.as_str(),
                "LD_LIBRARY_PATH"
                    | "LD_PRELOAD"
                    | "LD_AUDIT"
                    | "DYLD_LIBRARY_PATH"
                    | "DYLD_INSERT_LIBRARIES"
            )
        {
            command.env_remove(name);
        }
    }
    // Removing these variables unmasks Cargo's host config. Empty wrappers
    // explicitly disable it, and encoded flags take precedence over config.
    command
        .env("RUSTC_WRAPPER", "")
        .env("RUSTC_WORKSPACE_WRAPPER", "")
        .env("CARGO_ENCODED_RUSTFLAGS", "")
        .env("CARGO_ENCODED_RUSTDOCFLAGS", "")
        .stdin(Stdio::null())
}

fn native_host() -> &'static str {
    if cfg!(all(windows, target_arch = "aarch64")) {
        "aarch64-pc-windows-msvc"
    } else if cfg!(windows) {
        "x86_64-pc-windows-msvc"
    } else {
        "linux"
    }
}

fn installed_sysroot() -> Result<PathBuf> {
    let selected = std::env::var_os("RUSTC").map(PathBuf::from);
    if selected.as_ref().is_some_and(|path| !path.is_absolute()) {
        return Err("stdlib build requires an absolute selected compiler".into());
    }
    let program = selected.as_deref().unwrap_or(Path::new("rustc"));
    let output = controlled(Command::new(program).args(["--print", "sysroot"])).output()?;
    if !output.status.success() {
        return Err("cannot locate installed compiler".into());
    }
    let installed = PathBuf::from(String::from_utf8(output.stdout)?.trim());
    if !installed.is_absolute() {
        return Err("compiler reported a relative sysroot".into());
    }
    let expected = installed
        .join("bin")
        .join(native_target::executable(native_host(), "rustc"));
    if let Some(selected) = selected {
        if selected.canonicalize()? != expected.canonicalize()? {
            return Err("selected compiler does not belong to its reported sysroot".into());
        }
    }
    Ok(installed)
}

pub(crate) fn sysroot_cargo(installed: &Path) -> Command {
    let bin = installed.join("bin");
    let host = native_host();
    let mut command = Command::new(bin.join(native_target::executable(host, "cargo")));
    controlled(&mut command)
        .env("RUSTC", bin.join(native_target::executable(host, "rustc")))
        .env(
            "RUSTDOC",
            bin.join(native_target::executable(host, "rustdoc")),
        )
        .env("RUSTC_BOOTSTRAP", "1")
        .env("CARGO_ENCODED_RUSTFLAGS", BASE_FLAGS);
    command
}

fn run() -> Result<()> {
    let destination = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .ok_or("expected fresh output directory")?,
    );
    if destination.exists() {
        return Err("destination already exists; preserve it and choose a new directory".into());
    }
    let installed = installed_sysroot()?;
    let compiler = installed
        .join("bin")
        .join(native_target::executable(native_host(), "rustc"));
    let version = controlled(Command::new(&compiler).args(["--version", "--verbose"])).output()?;
    let version_text = String::from_utf8(version.stdout.clone())?;
    let target = version_text
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .ok_or("missing native target")?;
    if !version.status.success()
        || !version_text.contains("release: 1.99.0\n")
        || !version_text.contains(&format!("commit-hash: {COMMIT}"))
        || !native_target::supported(target)
    {
        return Err("unqualified compiler: requires pinned native Rust1.99".into());
    }
    fs::create_dir_all(&destination)?;
    let absolute = destination.canonicalize()?;
    let text = absolute.to_str().ok_or("non-UTF8 stdlib destination")?;
    let destination = if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{unc}"))
    } else {
        PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(text))
    };
    let library = destination.join("source/library");
    let original = installed.join("lib/rustlib/src/rust/library");
    let mut sources = Vec::new();
    copy_tree(&original, &library, &original, &mut sources)?;
    let unicode_dir = library.join("core/src/unicode");
    let mut names: Vec<_> = fs::read_dir(&unicode_dir)?
        .map(|e| e.map(|e| e.file_name()))
        .collect::<std::result::Result<_, _>>()?;
    names.sort();
    if names != ["mod.rs", "unicode_data.rs"].map(std::ffi::OsString::from) {
        return Err("unreviewed Unicode implementation files in pinned source".into());
    }
    let replacement = include_bytes!("platform_unicode.rs");
    fs::write(unicode_dir.join("unicode_data.rs"), replacement)?;
    let methods = library.join("core/src/char/methods.rs");
    let mut text = fs::read_to_string(&methods)?.replace("\r\n", "\n");
    for name in ["is_lowercase", "is_uppercase", "is_whitespace"] {
        let signature = format!("    pub const fn {name}(");
        let pos = text
            .find(&signature)
            .ok_or("Unicode method signature changed")?;
        let attr = text[..pos]
            .rfind("    #[rustc_const_stable(")
            .ok_or("Unicode const attribute missing")?;
        let end = attr + text[attr..].find('\n').ok_or("malformed const attribute")? + 1;
        text.replace_range(attr..end, "");
        text = text.replacen(&signature, &format!("    pub fn {name}("), 1);
    }
    fs::write(&methods, text.as_bytes())?;
    let copyright = fs::read(installed.join("share/doc/rust/COPYRIGHT-library.html"))?;
    fs::write(
        destination.join("COPYRIGHT-library.upstream.html"),
        &copyright,
    )?;
    fs::write(
        destination.join("original-sources.json"),
        serde_json::to_vec_pretty(&sources)?,
    )?;
    let graph = sysroot_cargo(&installed)
        .current_dir(&library)
        .args([
            "tree",
            "-p",
            "sysroot",
            "--target",
            target,
            "--offline",
            "--locked",
            "--features",
            "backtrace",
            "-e",
            "normal,build",
            "--prefix",
            "none",
            "--format",
            "{p} {l}",
        ])
        .output()?;
    if !graph.status.success() {
        return Err("could not resolve selected sysroot graph".into());
    }
    let graph_text = String::from_utf8(graph.stdout.clone())?;
    for line in graph_text.lines() {
        if matches!(
            line.split_whitespace().next(),
            Some("foldhash" | "unicode-ident" | "fortanix-sgx-abi")
        ) {
            return Err("unreviewed incompatible dependency selected for native sysroot".into());
        }
    }
    fs::write(destination.join("selected-dependencies.txt"), &graph.stdout)?;
    let build = destination.join("build");
    let flags = compiler_flags(&destination)?;
    let mut command = sysroot_cargo(&installed);
    command
        .env("CARGO_ENCODED_RUSTFLAGS", &flags)
        .current_dir(&library)
        .args([
            "build",
            "-p",
            "sysroot",
            "--target",
            target,
            "--offline",
            "--locked",
            "--features",
            "backtrace",
            "--profile",
            "dist",
            "--message-format=json",
            "--target-dir",
        ])
        .arg(&build)
        .stdout(fs::File::create(destination.join("build-artifacts.jsonl"))?)
        .stderr(fs::File::create(destination.join("build.stderr"))?);
    if !command.status()?.success() {
        return Err("private stdlib build failed; retained build.stderr".into());
    }
    let sysroot = destination.join("sysroot");
    let lib = sysroot.join("lib/rustlib").join(target).join("lib");
    fs::create_dir_all(&lib)?;
    let mut artifacts = Vec::new();
    for line in fs::read_to_string(destination.join("build-artifacts.jsonl"))?.lines() {
        let record: Value = serde_json::from_str(line)?;
        if record["reason"] != "compiler-artifact" {
            continue;
        }
        if let Some(files) = record["filenames"].as_array() {
            for file in files {
                let path = PathBuf::from(file.as_str().ok_or("invalid compiler artifact path")?);
                if path.extension().is_some_and(|s| s == "rlib")
                    && path.starts_with(build.join(target))
                {
                    let name = path.file_name().ok_or("artifact has no filename")?;
                    let bytes = fs::read(&path)?;
                    fs::write(lib.join(name), &bytes)?;
                    artifacts.push(json!({"filename": name.to_string_lossy(), "sha256": hash(&bytes), "package_id": record["package_id"]}));
                }
            }
        }
    }
    if artifacts.len() < 10 {
        return Err("incomplete compiler artifact set".into());
    }
    let cargo = installed
        .join("bin")
        .join(native_target::executable(native_host(), "cargo"));
    let receipt = json!({
        "schema": "susi/product-stdlib-prototype/v1", "status": "requires-qualification",
        "target": target, "compiler_commit": COMMIT, "compiler_vV_sha256": hash(&version.stdout),
        "upstream_notice_sha256": hash(&copyright), "unicode_replacement_sha256": hash(replacement),
        "original_sources_manifest_sha256": hash(&fs::read(destination.join("original-sources.json"))?),
        "selected_dependencies_sha256": hash(&graph.stdout),
        "builder_source_sha256": hash(include_bytes!("builder.rs")),
        "compiler_paths": {
            "physical_destination": destination, "physical_build": build,
            "physical_original": original, "physical_source": library,
            "physical_rustc": compiler, "physical_cargo": cargo,
            "rustc_sha256": hash(&fs::read(&compiler)?),
            "cargo_sha256": hash(&fs::read(&cargo)?),
            "virtual_destination": VIRTUAL_BUILD_ROOT, "encoded_rustflags": flags,
        },
        "char_methods_sha256": hash(text.as_bytes()), "os_unicode_versions": if target.contains("linux") { json!([{"icu": 76, "unicode": [16,0,0]}, {"icu": 74, "unicode": [15,1,0]}, {"icu": 72, "unicode": [15,0,0]}]) } else { json!([{"icu": "windows", "unicode": [15,1,0]}]) },
        "removed_const_apis": ["char::is_lowercase", "char::is_uppercase", "char::is_whitespace"],
        "artifacts": artifacts,
        "limitations": ["not a general standard-library compatibility promise", "production source/link/behavior/license admission remains mandatory"]
    });
    fs::write(
        destination.join("prototype-receipt.json"),
        serde_json::to_vec_pretty(&receipt)?,
    )?;
    println!("Private product sysroot: {}", sysroot.display());
    println!("Prototype only; receipt does not authorize release qualification.");
    Ok(())
}
fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("stdlib builder: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}
