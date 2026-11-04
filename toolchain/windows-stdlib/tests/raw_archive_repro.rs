//! Cold Windows qualification. Keep both physical builds and raw receipts.
#![cfg(windows)]
#![forbid(unsafe_code)]

use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    error::Error,
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn build(destination: &Path) -> Result<(Value, BTreeMap<String, PathBuf>)> {
    let status = Command::new(env!("CARGO_BIN_EXE_susi-windows-stdlib-builder"))
        .arg(destination)
        .status()?;
    if !status.success() {
        return Err(format!("cold build failed at {}", destination.display()).into());
    }
    let receipt: Value =
        serde_json::from_slice(&fs::read(destination.join("prototype-receipt.json"))?)?;
    let physical = receipt["compiler_paths"]["physical_destination"]
        .as_str()
        .ok_or("missing physical destination")?;
    assert_eq!(
        Path::new(physical).canonicalize()?,
        destination.canonicalize()?
    );
    let flags = receipt["compiler_paths"]["encoded_rustflags"]
        .as_str()
        .ok_or("missing compiler flags")?;
    assert!(flags
        .split('\x1f')
        .any(|flag| flag == format!("--remap-path-prefix={physical}=/susi/windows-stdlib")));
    assert!(flags
        .split('\x1f')
        .any(|flag| flag == "--remap-path-scope=all"));
    // The builder records the native host it qualified; read the archives there.
    let target = receipt["target"].as_str().ok_or("missing target")?;
    assert!(target.ends_with("-pc-windows-msvc"), "{target}");
    let lib = destination
        .join("sysroot/lib/rustlib")
        .join(target)
        .join("lib");
    let mut inventory = BTreeMap::new();
    for entry in fs::read_dir(lib)? {
        let entry = entry?;
        assert!(entry.file_type()?.is_file());
        assert_eq!(
            entry.path().extension().and_then(|e| e.to_str()),
            Some("rlib")
        );
        inventory.insert(
            entry
                .file_name()
                .into_string()
                .map_err(|_| "non-UTF8 archive")?,
            entry.path(),
        );
    }
    let archives = receipt["artifacts"]
        .as_array()
        .ok_or("missing archive receipt")?;
    assert_eq!(inventory.len(), 20);
    assert_eq!(archives.len(), 20);
    for (name, path) in &inventory {
        let mut matches = archives
            .iter()
            .filter(|a| a["filename"].as_str() == Some(name.as_str()));
        let recorded = matches.next().ok_or("unrecorded archive")?;
        assert!(matches.next().is_none());
        let digest: String = Sha256::digest(fs::read(path)?)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert_eq!(recorded["sha256"], digest);
    }
    Ok((receipt, inventory))
}

#[test]
fn independent_roots_produce_identical_raw_archives() -> Result<()> {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let root = std::env::temp_dir().join(format!("sr-{nonce:x}"));
    fs::create_dir(&root)?;
    eprintln!("Retaining both physical builds at {}", root.display());
    let (first, a) = build(&root.join("first"))?;
    let (second, b) = build(&root.join("different-second"))?;
    for field in [
        "builder_source_sha256",
        "original_sources_manifest_sha256",
        "compiler_vV_sha256",
    ] {
        assert!(first[field].as_str().is_some_and(|s| s.len() == 64));
        assert_eq!(first[field], second[field], "{field}");
    }
    assert_eq!(first["target"], second["target"]);
    assert_eq!(a.keys().collect::<Vec<_>>(), b.keys().collect::<Vec<_>>());
    let mut differences = Vec::new();
    for (name, path) in a {
        if fs::read(path)? != fs::read(&b[&name])? {
            differences.push(name);
        }
    }
    assert!(
        differences.is_empty(),
        "raw archive bytes differ: {differences:?}"
    );
    Ok(())
}
