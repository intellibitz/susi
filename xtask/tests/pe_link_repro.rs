//! Tiny link-policy fixture. Full product and helper qualification is separate.
#![cfg(all(windows, target_env = "msvc", target_arch = "x86_64"))]
#![forbid(unsafe_code)]
use susi_build_os_ucrt as os_ucrt;

use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    error::Error,
    fs,
    path::Path,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};
type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

// Read the on-disk PE32+ debug directory of our own MSVC fixture.
fn debug_records(pe: &[u8]) -> Result<(u32, Vec<(u32, u32)>)> {
    fn word(data: &[u8], offset: usize) -> Result<u16> {
        Ok(u16::from_le_bytes(
            data.get(offset..offset + 2)
                .ok_or("truncated word")?
                .try_into()?,
        ))
    }
    fn dword(data: &[u8], offset: usize) -> Result<u32> {
        Ok(u32::from_le_bytes(
            data.get(offset..offset + 4)
                .ok_or("truncated dword")?
                .try_into()?,
        ))
    }
    let header = dword(pe, 0x3c)? as usize;
    assert_eq!(pe.get(header..header + 4), Some(b"PE\0\0".as_slice()));
    let optional = header + 24;
    assert_eq!(word(pe, optional)?, 0x20b);
    let debug = optional + 112 + 6 * 8;
    let rva = dword(pe, debug)?;
    let size = dword(pe, debug + 4)?;
    assert!(size > 0 && size % 28 == 0);
    let sections = optional + usize::from(word(pe, header + 20)?);
    for i in 0..usize::from(word(pe, header + 6)?) {
        let s = sections + i * 40;
        let Some(delta) = rva.checked_sub(dword(pe, s + 12)?) else {
            continue;
        };
        if u64::from(delta) + u64::from(size) > u64::from(dword(pe, s + 16)?) {
            continue;
        }
        let start = usize::try_from(u64::from(dword(pe, s + 20)?) + u64::from(delta))?;
        let mut entries = Vec::new();
        for i in 0..size as usize / 28 {
            entries.push((
                dword(pe, start + i * 28 + 12)?,
                dword(pe, start + i * 28 + 4)?,
            ));
        }
        return Ok((dword(pe, header + 8)?, entries));
    }
    Err("debug directory outside PE sections".into())
}

fn link(root: &Path, compiler: &Path) -> Result<Vec<u8>> {
    fs::create_dir(root)?;
    fs::write(
        root.join("fixture.rs"),
        b"fn main() { assert_eq!(std::hint::black_box(7), 7); }\n",
    )?;
    let args = vec![
        "fixture.rs".to_owned(),
        "--crate-name=fixture".into(),
        "--edition=2021".into(),
        "--remap-path-scope=all".into(),
        format!("--remap-path-prefix={}=/susi/pe-fixture", root.display()),
        "-Cdebuginfo=2".into(),
        format!("-Clink-arg={}", os_ucrt::REPRO_LINK_ARG),
        "-Clink-arg=/INCREMENTAL:NO".into(),
        "-Clink-arg=/MAP:fixture.map".into(),
        "-Clink-arg=/PDBALTPATH:%_PDB%".into(),
        "--print=link-args".into(),
        "-o".into(),
        "fixture.exe".into(),
    ];
    let start = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    let output = Command::new(compiler)
        .args(&args)
        .current_dir(root)
        .output()?;
    let end = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    fs::write(root.join("link.argv"), &output.stdout)?;
    fs::write(root.join("link.stderr"), &output.stderr)?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let bytes = fs::read(root.join("fixture.exe"))?;
    let (timestamp, debug) = debug_records(&bytes)?;
    let map = fs::read(root.join("fixture.map"))?;
    assert!(!map.is_empty());
    let receipt = json!({"compiler":compiler,"compiler_sha256":digest(&fs::read(compiler)?),
        "args":args,"root":root,"started_unix_ms":start,"finished_unix_ms":end,
        "raw_pe_sha256":digest(&bytes),"raw_map_sha256":digest(&map),
        "raw_pdb_sha256":digest(&fs::read(root.join("fixture.pdb"))?),
        "coff_timestamp":timestamp,"debug_type_and_timestamp":debug});
    fs::write(
        root.join("link-receipt.json"),
        serde_json::to_vec_pretty(&receipt)?,
    )?;
    assert!(
        debug.iter().any(|(kind, _)| *kind == 16),
        "missing PE REPRO entry"
    );
    assert!(Command::new(root.join("fixture.exe"))
        .current_dir(root)
        .status()?
        .success());
    Ok(bytes)
}

#[test]
fn independent_msvc_links_match_including_debug_records() -> Result<()> {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let root = std::env::temp_dir().join(format!("pe-{nonce:x}"));
    fs::create_dir(&root)?;
    eprintln!("Retaining raw evidence at {}", root.display());
    let selected = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let out = Command::new(selected)
        .args(["--print", "sysroot"])
        .output()?;
    assert!(out.status.success());
    let compiler = Path::new(std::str::from_utf8(&out.stdout)?.trim()).join("bin/rustc.exe");
    let first = link(&root.join("first"), &compiler)?;
    let second = link(&root.join("independent-second"), &compiler)?;
    assert!(
        first == second,
        "raw PE differs: {} vs {}",
        digest(&first),
        digest(&second)
    );
    Ok(())
}
