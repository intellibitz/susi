use super::{git, sysroot_sources::hash, Result};
use serde_json::{json, Value};
use std::{collections::BTreeMap, fs, path::Path, process::Command};

pub(crate) const PREFIX: &str = "workspace-test-pass-v1=";

fn compiler_files(root: &Path) -> Result<BTreeMap<String, String>> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let info = Command::new("rustc")
        .current_dir(root)
        .args(["--print", "sysroot"])
        .output()?;
    if !info.status.success() {
        return Err("cannot locate compiler executables".into());
    }
    let bin = Path::new(String::from_utf8(info.stdout)?.trim()).join("bin");
    let mut paths = vec![
        bin.join("cargo.exe"),
        bin.join("rustc.exe"),
        bin.join("rustdoc.exe"),
    ];
    for entry in fs::read_dir(&bin)? {
        let entry = entry?;
        if entry
            .file_name()
            .to_str()
            .is_some_and(|n| n.starts_with("rustc_driver") && n.ends_with(".dll"))
        {
            paths.push(entry.path());
        }
    }
    if paths.len() == 3 {
        return Err("missing compiler driver".into());
    }
    let mut identities = BTreeMap::new();
    for path in paths {
        let mut file = fs::File::open(&path)?;
        let mut digest = Sha256::new();
        let mut buf = vec![0; 128 * 1024];
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            digest.update(&buf[..n]);
        }
        identities.insert(
            path.file_name()
                .and_then(|n| n.to_str())
                .ok_or("invalid compiler path")?
                .into(),
            digest
                .finalize()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        );
    }
    Ok(identities)
}

// Paths created solely for this attempt have different names in qualification.
// Normalize only the exact target argument and freshly provisioned home paths.
pub(crate) struct Toolchain<'a> {
    pub(crate) compiler: &'a str,
    pub(crate) host: &'a str,
    pub(crate) sysroot: &'a Value,
}
pub(crate) fn fingerprint(
    root: &Path,
    args: &[String],
    toolchain: Toolchain<'_>,
    cmd: &Command,
    home: &Path,
) -> Result<String> {
    let Toolchain {
        compiler,
        host,
        sysroot,
    } = toolchain;
    // Both callers hold the verifier's archive handles. Normalize only its private
    // location and diagnostics. Compiler identity, all archive bytes and all
    // other authenticated evidence still participate in the signed key.
    let mut verified = sysroot.clone();
    let verified_fields = verified
        .as_object_mut()
        .ok_or("missing authenticated sysroot")?;
    let destination = verified_fields
        .remove("destination")
        .and_then(|v| v.as_str().map(std::path::PathBuf::from))
        .ok_or("missing authenticated destination")?
        .join("sysroot");
    if !matches!(
        sysroot["schema"].as_str(),
        Some("susi/executed-sysroot/v2" | "susi/executed-sysroot/v3")
    ) || sysroot["compiler"] != compiler
        || !sysroot["identity"].is_string()
        || !sysroot["authenticated_source_manifest_sha256"].is_string()
        || sysroot["artifacts"]
            .as_array()
            .is_none_or(|archives| archives.len() != 20)
    {
        return Err("incomplete authenticated sysroot".into());
    }
    verified_fields.remove("elapsed_ms");
    verified_fields.remove("cache_hit");
    let target = root.join("target");
    let mut argv = args.to_vec();
    let mut dirs = argv
        .windows(2)
        .enumerate()
        .filter(|(_, a)| a[0] == "--target-dir");
    let (index, pair) = dirs.next().ok_or("no target directory")?;
    if dirs.next().is_some()
        || Path::new(&pair[1]) != target
        || argv.first().map(String::as_str) != Some("test")
        || !argv.iter().any(|a| a == "--workspace")
    {
        return Err("unexpected workspace test invocation".into());
    }
    argv[index + 1] = "<target>".into();
    let mut env: BTreeMap<_, _> = std::env::vars_os().collect();
    for (k, v) in cmd.get_envs() {
        if let Some(v) = v {
            env.insert(k.into(), v.into());
        } else {
            env.remove(k);
        }
    }
    let mut environment = BTreeMap::new();
    for (k, v) in env {
        let k = k.to_str().ok_or("non-UTF8 environment key")?.to_uppercase();
        let v = v.to_str().ok_or("non-UTF8 environment value")?;
        let path = Path::new(v);
        let value = match k.as_str() {
            "HOME" | "USERPROFILE" | "SUSI_HOME" | "XDG_CONFIG_HOME" | "XDG_DATA_HOME"
            | "XDG_CACHE_HOME" | "APPDATA" | "LOCALAPPDATA" => {
                format!("<home>/{}", path.strip_prefix(home)?.display())
            }
            "CARGO_TARGET_DIR" | "CARGO_BUILD_TARGET_DIR" | "CARGO_BUILD_BUILD_DIR"
                if path == target =>
            {
                "<target>".into()
            }
            "SUSI_VERIFIED_SYSROOT" if path == destination => "<verified-sysroot>".into(),
            "CARGO_ENCODED_RUSTFLAGS" | "CARGO_ENCODED_RUSTDOCFLAGS"
                if v.strip_prefix("--sysroot\x1f")
                    .is_some_and(|p| Path::new(p) == destination) =>
            {
                "--sysroot\x1f<verified-sysroot>".into()
            }
            _ => v.into(),
        };
        environment.insert(k, value);
    }
    let mut config = Vec::new();
    for dir in root
        .ancestors()
        .map(|p| p.join(".cargo"))
        .chain(environment.get("CARGO_HOME").map(std::path::PathBuf::from))
    {
        for name in ["config", "config.toml"] {
            let p = dir.join(name);
            match fs::read(&p) {
                Ok(bytes) => config.push((name, hash(&bytes))),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                Err(e) => return Err(e.into()),
            }
        }
    }
    let cargo = Command::new("cargo")
        .current_dir(root)
        .arg("-Vv")
        .output()?;
    if !cargo.status.success() {
        return Err("cannot identify cargo".into());
    }
    let inputs = json!({"policy":"exact-workspace-tests-v1","verifier-sources":[hash(include_bytes!("workspace_test_proof.rs")),hash(include_bytes!("test_runner.rs")),hash(include_bytes!("bin/local-workflow.rs")),hash(include_bytes!("bin/local-release.rs")),hash(include_bytes!("compiler_env.rs")),hash(include_bytes!("trusted_sysroot.rs")),hash(include_bytes!("../../crates/susi-build-policy/src/lib.rs"))],"commit":git(root,&["rev-parse","HEAD"])?,"tree":git(root,&["rev-parse","HEAD^{tree}"])?,"argv":argv,"lock":hash(&fs::read(root.join("Cargo.lock"))?),"host":host,"compiler":compiler,"toolchain-files":compiler_files(root)?,"cargo":hash(&cargo.stdout),"sysroot":verified,"environment":environment,"cargo-config":config});
    Ok(hash(&serde_json::to_vec(&inputs)?))
}

pub(crate) fn record(key: &str) -> String {
    format!("{PREFIX}{key}")
}

// Use the Git-configured OpenPGP program and key. Batch pinentry makes an
// unavailable signing key a cache miss, even on an unattended workstation.
pub(crate) fn gpg(root: &Path, args: &[&str], input: &[u8]) -> Result<std::process::Output> {
    use std::{io::Write, process::Stdio};
    if git(root, &["config", "--get", "gpg.format"]).is_ok_and(|v| v != "openpgp") {
        return Err("workspace pass needs an OpenPGP signer".into());
    }
    let program = git(root, &["config", "--get", "gpg.openpgp.program"])
        .or_else(|_| git(root, &["config", "--get", "gpg.program"]))
        .unwrap_or_else(|_| "gpg".into());
    let mut child = Command::new(program)
        .current_dir(root)
        .args([
            "--batch",
            "--no-tty",
            "--no-auto-key-retrieve",
            "--pinentry-mode",
            "error",
        ])
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().ok_or("no signing input")?;
    let write = stdin.write_all(input);
    drop(stdin);
    let output = child.wait_with_output()?;
    write?;
    if !output.status.success() {
        return Err("OpenPGP operation failed".into());
    }
    Ok(output)
}
pub(crate) fn signed_input(root: &Path, key: &str) -> Result<String> {
    Ok(format!(
        "{}\ncommit={}\ntree={}\n",
        record(key),
        git(root, &["rev-parse", "HEAD"])?,
        git(root, &["rev-parse", "HEAD^{tree}"])?
    ))
}
