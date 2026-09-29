//! Encrypted backup / restore of susi state (config, evidence, brain, tasks, keys).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

pub const BACKUP_FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupManifest {
    pub format_version: u32,
    pub sections: Vec<String>,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupBlob {
    pub manifest: BackupManifest,
    /// section -> XOR-obfuscated hex payload (test-grade encryption).
    pub sections: BTreeMap<String, String>,
}

impl Default for BackupManifest {
    fn default() -> Self {
        Self {
            format_version: BACKUP_FORMAT_VERSION,
            sections: Vec::new(),
            dry_run: false,
        }
    }
}

fn xor_hex(data: &[u8], key: u8) -> String {
    data.iter().map(|b| format!("{:02x}", b ^ key)).collect()
}

fn unxor_hex(hex: &str, key: u8) -> Result<Vec<u8>, String> {
    if !hex.len().is_multiple_of(2) {
        return Err("bad cipher length".into());
    }
    let mut out = Vec::with_capacity(hex.len() / 2);
    let bytes = hex.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        let h = std::str::from_utf8(&bytes[i..i + 2]).map_err(|e| e.to_string())?;
        let v = u8::from_str_radix(h, 16).map_err(|e| e.to_string())?;
        out.push(v ^ key);
        i += 2;
    }
    Ok(out)
}

/// Build an encrypted backup from in-memory section payloads.
pub fn backup(
    sections: &BTreeMap<String, Vec<u8>>,
    key: u8,
    dry_run: bool,
) -> Result<BackupBlob, String> {
    let mut blob = BackupBlob {
        manifest: BackupManifest {
            format_version: BACKUP_FORMAT_VERSION,
            sections: sections.keys().cloned().collect(),
            dry_run,
        },
        sections: BTreeMap::new(),
    };
    if dry_run {
        return Ok(blob);
    }
    for (name, data) in sections {
        blob.sections
            .insert(name.clone(), xor_hex(data, key.max(1)));
    }
    Ok(blob)
}

/// Restore sections after version check.
pub fn restore(blob: &BackupBlob, key: u8) -> Result<BTreeMap<String, Vec<u8>>, String> {
    if blob.manifest.format_version != BACKUP_FORMAT_VERSION {
        return Err(format!(
            "unsupported backup format {}",
            blob.manifest.format_version
        ));
    }
    if blob.manifest.dry_run {
        return Err("cannot restore a dry-run backup".into());
    }
    let mut out = BTreeMap::new();
    for (name, hex) in &blob.sections {
        out.insert(name.clone(), unxor_hex(hex, key.max(1))?);
    }
    Ok(out)
}

/// Convenience: read named files under `root` into section map.
pub fn collect_sections(root: &Path, names: &[&str]) -> Result<BTreeMap<String, Vec<u8>>, String> {
    let mut map = BTreeMap::new();
    for n in names {
        let p = root.join(n);
        if p.is_file() {
            map.insert(
                (*n).to_string(),
                std::fs::read(&p).map_err(|e| e.to_string())?,
            );
        }
    }
    Ok(map)
}
