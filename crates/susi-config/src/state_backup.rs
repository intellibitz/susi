//! Versioned, authenticated backup / restore of susi state.
//!
//! Secret-bearing sections require an explicit opt-in. Every section is sealed
//! independently so a corrupted or wrongly keyed backup never yields partial
//! plaintext, and file/artifact references are checked before restore returns.

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub const BACKUP_FORMAT_VERSION: u32 = 2;
const NONCE_LEN: usize = 12;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupManifest {
    pub format_version: u32,
    pub sections: Vec<String>,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupBlob {
    pub manifest: BackupManifest,
    /// section -> hex(nonce || authenticated ciphertext)
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

/// Controls optional material included in a backup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackupOptions {
    pub include_secrets: bool,
    pub dry_run: bool,
}

impl BackupOptions {
    pub const fn new(dry_run: bool) -> Self {
        Self {
            include_secrets: false,
            dry_run,
        }
    }

    pub const fn with_secrets(self) -> Self {
        Self {
            include_secrets: true,
            dry_run: self.dry_run,
        }
    }
}

/// The result of collecting requested sections. Missing inputs are returned
/// alongside the sections that were available so callers can report degraded
/// exports without confusing an absent file with an unrequested one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SectionCollection {
    pub sections: BTreeMap<String, Vec<u8>>,
    pub missing: Vec<String>,
}

fn is_secret_section(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [
        "credential",
        "key",
        "password",
        "private",
        "secret",
        "token",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

fn derive_key(seed: u8) -> [u8; 32] {
    let mut material = b"susi-state-backup-key-v2".to_vec();
    material.push(seed);
    Sha256::digest(material).into()
}

fn associated_data(name: &str, dry_run: bool) -> Vec<u8> {
    format!(
        "susi-state-backup-v{}:{}:{}",
        BACKUP_FORMAT_VERSION, dry_run, name
    )
    .into_bytes()
}

fn seal(data: &[u8], seed: u8, name: &str, dry_run: bool) -> Result<String, String> {
    let mut nonce = [0u8; NONCE_LEN];
    getrandom::fill(&mut nonce).map_err(|error| format!("backup nonce: {error}"))?;
    let key = derive_key(seed);
    let cipher = chacha20poly1305::ChaCha20Poly1305::new((&key).into());
    let aad = associated_data(name, dry_run);
    let encrypted = cipher
        .encrypt(
            chacha20poly1305::Nonce::from_slice(&nonce),
            Payload {
                msg: data,
                aad: &aad,
            },
        )
        .map_err(|_| format!("failed to encrypt backup section {name}"))?;
    let mut framed = Vec::with_capacity(NONCE_LEN + encrypted.len());
    framed.extend_from_slice(&nonce);
    framed.extend_from_slice(&encrypted);
    Ok(hex::encode(framed))
}

fn open(ciphertext: &str, seed: u8, name: &str, dry_run: bool) -> Result<Vec<u8>, String> {
    let framed =
        hex::decode(ciphertext).map_err(|error| format!("invalid backup section: {error}"))?;
    if framed.len() <= NONCE_LEN {
        return Err(format!(
            "backup section {name} has no authenticated payload"
        ));
    }
    let nonce = <&[u8; NONCE_LEN]>::try_from(&framed[..NONCE_LEN])
        .map_err(|_| format!("backup section {name} has an invalid nonce"))?;
    let key = derive_key(seed);
    let cipher = chacha20poly1305::ChaCha20Poly1305::new((&key).into());
    let aad = associated_data(name, dry_run);
    cipher
        .decrypt(
            chacha20poly1305::Nonce::from_slice(nonce),
            Payload {
                msg: &framed[NONCE_LEN..],
                aad: &aad,
            },
        )
        .map_err(|_| format!("backup authentication failed for section {name}"))
}

/// Build an encrypted backup using the default policy, which excludes
/// secret-bearing sections and fails closed if one is requested.
pub fn backup(
    sections: &BTreeMap<String, Vec<u8>>,
    key: u8,
    dry_run: bool,
) -> Result<BackupBlob, String> {
    backup_with_options(sections, key, BackupOptions::new(dry_run))
}

/// Build an encrypted backup with an explicit secret-material opt-in.
pub fn backup_with_options(
    sections: &BTreeMap<String, Vec<u8>>,
    key: u8,
    options: BackupOptions,
) -> Result<BackupBlob, String> {
    let secret_sections: Vec<&str> = sections
        .keys()
        .filter(|name| is_secret_section(name))
        .map(String::as_str)
        .collect();
    if !secret_sections.is_empty() && !options.include_secrets {
        return Err(format!(
            "secret sections require explicit inclusion: {}",
            secret_sections.join(", ")
        ));
    }

    let mut blob = BackupBlob {
        manifest: BackupManifest {
            format_version: BACKUP_FORMAT_VERSION,
            sections: sections.keys().cloned().collect(),
            dry_run: options.dry_run,
        },
        sections: BTreeMap::new(),
    };
    if options.dry_run {
        return Ok(blob);
    }
    for (name, data) in sections {
        blob.sections
            .insert(name.clone(), seal(data, key, name, options.dry_run)?);
    }
    Ok(blob)
}

fn validate_manifest(blob: &BackupBlob) -> Result<BTreeSet<&String>, String> {
    if blob.manifest.format_version != BACKUP_FORMAT_VERSION {
        return Err(format!(
            "unsupported backup format {}",
            blob.manifest.format_version
        ));
    }
    if blob.manifest.dry_run {
        return Err("cannot restore a dry-run backup".into());
    }
    let expected: BTreeSet<&String> = blob.manifest.sections.iter().collect();
    if expected.len() != blob.manifest.sections.len() {
        return Err("backup manifest contains duplicate sections".into());
    }
    let actual: BTreeSet<&String> = blob.sections.keys().collect();
    if expected != actual {
        return Err("backup manifest does not match encrypted sections".into());
    }
    Ok(expected)
}

fn reference_candidates(text: &str) -> Vec<String> {
    let mut references = Vec::new();
    for scheme in ["file://", "artifact://"] {
        let mut offset = 0;
        while let Some(relative) = text[offset..].find(scheme) {
            let start = offset + relative;
            let end = text[start..]
                .find(|character: char| {
                    character == '"'
                        || character == '\''
                        || character.is_whitespace()
                        || matches!(character, ',' | '}' | ']')
                })
                .map(|relative_end| start + relative_end)
                .unwrap_or(text.len());
            let value = &text[start..end];
            if !value.is_empty() {
                references.push(value.to_string());
            }
            offset = end.max(start + scheme.len());
            if offset >= text.len() {
                break;
            }
        }
    }
    references.sort();
    references.dedup();
    references
}

fn verify_references(section: &str, data: &[u8]) -> Result<(), String> {
    let text = String::from_utf8_lossy(data);
    for reference in reference_candidates(&text) {
        if let Some(path) = reference.strip_prefix("file://") {
            let path = if let Some(path) = path.strip_prefix("localhost/") {
                format!("/{path}")
            } else {
                path.to_string()
            };
            if !Path::new(&path).is_file() {
                return Err(format!(
                    "unavailable external resource {reference} in section {section}"
                ));
            }
        } else {
            return Err(format!(
                "unavailable external resource {reference} in section {section}"
            ));
        }
    }
    Ok(())
}

/// Restore sections after authenticating the manifest, every section, and its
/// external file/artifact references.
pub fn restore(blob: &BackupBlob, key: u8) -> Result<BTreeMap<String, Vec<u8>>, String> {
    let expected = validate_manifest(blob)?;
    let mut out = BTreeMap::new();
    for name in expected {
        let ciphertext = blob
            .sections
            .get(name)
            .ok_or_else(|| format!("missing encrypted section {name}"))?;
        let plaintext = open(ciphertext, key, name, blob.manifest.dry_run)?;
        verify_references(name, &plaintext)?;
        out.insert(name.clone(), plaintext);
    }
    Ok(out)
}

/// Read named files under `root`, retaining a report for unavailable inputs.
pub fn collect_sections_report(root: &Path, names: &[&str]) -> Result<SectionCollection, String> {
    if !root.is_dir() {
        return Err(format!("backup root is unavailable: {}", root.display()));
    }
    let mut sections = BTreeMap::new();
    let mut missing = Vec::new();
    for name in names {
        let path = root.join(name);
        if path.is_file() {
            sections.insert(
                (*name).to_string(),
                std::fs::read(&path).map_err(|error| error.to_string())?,
            );
        } else {
            missing.push((*name).to_string());
        }
    }
    Ok(SectionCollection { sections, missing })
}

/// Read named files under `root`, failing with every unavailable input rather
/// than silently dropping requested sections.
pub fn collect_sections(root: &Path, names: &[&str]) -> Result<BTreeMap<String, Vec<u8>>, String> {
    let report = collect_sections_report(root, names)?;
    if !report.missing.is_empty() {
        return Err(format!(
            "unavailable external resources: {}",
            report.missing.join(", ")
        ));
    }
    Ok(report.sections)
}
