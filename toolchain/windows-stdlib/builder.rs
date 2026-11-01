//! Private, pinned Windows product-stdlib builder. It never edits rustup sources.
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
const TARGET: &str = "x86_64-pc-windows-msvc";
const COMMIT: &str = "b940084d7eb6a299eb4bfeb8e34901bc051e7ac4";

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
        if upper.starts_with("RUST")
            || upper.starts_with("CARGO_TARGET_")
            || upper.starts_with("CARGO_BUILD_")
            || upper.starts_with("CARGO_ENCODED_")
        {
            command.env_remove(name);
        }
    }
    command.stdin(Stdio::null())
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
    let output = controlled(Command::new("rustc").arg("--print").arg("sysroot")).output()?;
    if !output.status.success() {
        return Err("cannot locate installed compiler".into());
    }
    let installed = PathBuf::from(String::from_utf8(output.stdout)?.trim());
    let compiler = installed.join("bin/rustc.exe");
    let cargo = installed.join("bin/cargo.exe");
    let version = controlled(Command::new(&compiler).args(["--version", "--verbose"])).output()?;
    let version_text = String::from_utf8(version.stdout.clone())?;
    if !version.status.success()
        || !version_text.contains("release: 1.99.0\n")
        || !version_text.contains(&format!("commit-hash: {COMMIT}"))
        || !version_text.contains(&format!("host: {TARGET}"))
    {
        return Err("unqualified compiler: requires pinned native Rust1.99".into());
    }
    fs::create_dir_all(&destination)?;
    let absolute = destination.canonicalize()?;
    let text = absolute.to_string_lossy();
    let destination = PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text));
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
    let mut graph_command = Command::new(&cargo);
    let graph = controlled(&mut graph_command)
        .current_dir(&library)
        .env("RUSTC", &compiler)
        .env("RUSTC_BOOTSTRAP", "1")
        .args([
            "tree",
            "-p",
            "sysroot",
            "--target",
            TARGET,
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
            return Err("unreviewed incompatible dependency selected for Windows sysroot".into());
        }
    }
    fs::write(destination.join("selected-dependencies.txt"), &graph.stdout)?;
    let build = destination.join("build");
    let mut command = Command::new(&cargo);
    controlled(&mut command)
        .current_dir(&library)
        .env("RUSTC", &compiler)
        .env("RUSTC_BOOTSTRAP", "1")
        .env("RUSTFLAGS", "-Zforce-unstable-if-unmarked")
        .args([
            "build",
            "-p",
            "sysroot",
            "--target",
            TARGET,
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
    let lib = sysroot.join("lib/rustlib").join(TARGET).join("lib");
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
                    && path.starts_with(build.join(TARGET))
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
    let receipt = json!({
        "schema": "susi/product-stdlib-prototype/v1", "status": "requires-qualification",
        "target": TARGET, "compiler_commit": COMMIT, "compiler_vV_sha256": hash(&version.stdout),
        "upstream_notice_sha256": hash(&copyright), "unicode_replacement_sha256": hash(replacement),
        "original_sources_manifest_sha256": hash(&fs::read(destination.join("original-sources.json"))?),
        "selected_dependencies_sha256": hash(&graph.stdout),
        "builder_source_sha256": hash(include_bytes!("builder.rs")),
        "char_methods_sha256": hash(text.as_bytes()), "os_unicode_version": [15,1,0],
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
