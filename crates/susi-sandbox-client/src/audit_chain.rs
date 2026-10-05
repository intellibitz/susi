//! Immutable accountability chain for audit events (Pillar Audit).
//!
//! Append-only: each entry is hash-linked to the previous tip and authenticated
//! with HMAC-SHA256 under a host-local key (`~/.susi/audit.hmac.key`). Rewriting
//! or deleting any historical line breaks the chain or the MAC — the history is
//! cryptographically immutable under the host key.

use serde::Serialize;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const GENESIS: &str = "SUSI_AUDIT_GENESIS_v1";

static CHAIN_LOCK: Mutex<()> = Mutex::new(());

fn key_path() -> PathBuf {
    susi_paths::SusiDirs::substrate_home().join("audit.hmac.key")
}

fn tip_path(audit_file: &Path) -> PathBuf {
    audit_file.with_extension("chain.tip")
}

/// Load or create the 32-byte host HMAC key (see
/// `susi_config::load_or_create_secret` for the creation guarantees).
pub fn load_or_create_hmac_key() -> std::io::Result<[u8; 32]> {
    crate::susi_config::load_or_create_secret(&key_path())
}

/// HMAC-SHA256 — shared implementation lives in `crate::susi_config::cluster_key`
/// (also used by the cluster-key peer handshake).
fn hmac_sha256(key: &[u8; 32], message: &[u8]) -> [u8; 32] {
    crate::susi_config::cluster_key::hmac_sha256(key, message)
}

fn last_hash(audit_file: &Path, key: &[u8; 32]) -> std::io::Result<String> {
    let log_len = match fs::metadata(audit_file) {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(GENESIS.to_string())
        }
        Err(error) => return Err(error),
    };
    let tip = tip_path(audit_file);
    if let Ok(checkpoint) = fs::read_to_string(&tip) {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&checkpoint) {
            let hash = value.get("entry_hash").and_then(|v| v.as_str());
            let checkpoint_len = value.get("log_len").and_then(serde_json::Value::as_u64);
            if checkpoint_len == Some(log_len) {
                if let Some(hash) = hash.filter(|hash| hash.len() == 64) {
                    return Ok(hash.to_string());
                }
            }
        }
    }

    verify_with_key(audit_file, key).map_err(std::io::Error::other)?;
    let content = fs::read_to_string(audit_file)?;
    for line in content.lines().rev() {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(line) {
            if let Some(hash) = value.get("entry_hash").and_then(|v| v.as_str()) {
                return Ok(hash.to_string());
            }
        }
    }
    Ok(GENESIS.to_string())
}

fn mac_hex(key: &[u8; 32], entry_hash: &str) -> String {
    hex::encode(hmac_sha256(key, entry_hash.as_bytes()))
}

/// Append a cryptographically linked, HMAC-authenticated audit entry.
/// Returns the new `entry_hash`.
pub fn append_signed_entry(
    audit_file: &Path,
    level: &str,
    event_type: &str,
    details: &str,
    pid: u32,
) -> std::io::Result<String> {
    let key = load_or_create_hmac_key()?;
    append_with_key(
        audit_file,
        &key,
        &Entry {
            level,
            event_type,
            details,
            pid,
        },
    )
}

/// The caller-supplied fields of one audit entry.
pub(crate) struct Entry<'a> {
    pub(crate) level: &'a str,
    pub(crate) event_type: &'a str,
    pub(crate) details: &'a str,
    pub(crate) pid: u32,
}

pub(crate) fn append_with_key(
    audit_file: &Path,
    key: &[u8; 32],
    entry: &Entry,
) -> std::io::Result<String> {
    let Entry {
        level,
        event_type,
        details,
        pid,
    } = *entry;
    let _guard = CHAIN_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // CHAIN_LOCK covers threads; the daemon and CLI processes append to the
    // same logs, and two of them reading one tip would fork the chain. An
    // unobtainable lock fails the append rather than risk that.
    let dir = audit_file.parent().unwrap_or_else(|| Path::new("."));
    let lock_name = format!(
        "{}.chain",
        audit_file
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("audit")
    );
    let _file_lock = crate::susi_config::file_lock::FileLock::acquire(dir, &lock_name)
        .ok_or_else(|| std::io::Error::other("audit chain lock unavailable"))?;
    let prev = last_hash(audit_file, key)?;
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let mut hasher = Sha256::new();
    hasher.update(prev.as_bytes());
    hasher.update(b"|");
    hasher.update(ts.to_string().as_bytes());
    hasher.update(b"|");
    hasher.update(level.as_bytes());
    hasher.update(b"|");
    hasher.update(event_type.as_bytes());
    hasher.update(b"|");
    hasher.update(details.as_bytes());
    hasher.update(b"|");
    hasher.update(pid.to_string().as_bytes());
    let entry_hash = hex::encode(hasher.finalize());
    let signature = mac_hex(key, &entry_hash);

    let log_entry = serde_json::json!({
        "ts": ts,
        "level": level,
        "type": event_type,
        "details": details,
        "pid": pid,
        "prev_hash": prev,
        "entry_hash": entry_hash,
        "hmac": signature,
    });

    if let Some(parent) = audit_file.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(audit_file)?;
    f.write_all(format!("{}\n", log_entry).as_bytes())?;
    f.sync_data()?;
    let log_len = f.metadata()?.len();
    let checkpoint = serde_json::json!({
        "entry_hash": entry_hash,
        "log_len": log_len,
    });
    crate::susi_config::atomic_write_bytes(
        &tip_path(audit_file),
        checkpoint.to_string().as_bytes(),
    )?;
    Ok(entry_hash)
}

/// Verify the HMAC + hash-link chain in an audit log.
pub fn verify_chain(audit_file: &Path) -> Result<usize, String> {
    verify_chain_since(audit_file, None)
}

/// Verify the complete chain while counting entries at or after `since`.
///
/// Verification always starts at the beginning of the file: a caller may
/// filter the reported count by time, but cannot use that filter to skip a
/// predecessor whose link or signature has been damaged.
pub fn verify_chain_since(audit_file: &Path, since: Option<u64>) -> Result<usize, String> {
    let key = load_or_create_hmac_key().map_err(|e| e.to_string())?;
    verify_with_key_since(audit_file, &key, since)
}

/// Filters for the structured action history query surface.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuditQuery {
    pub actor: Option<String>,
    pub mission: Option<String>,
    pub kind: Option<String>,
    pub since: Option<u64>,
    pub until: Option<u64>,
}

/// Redacted, queryable metadata from one signed audit entry.
///
/// Raw details are intentionally excluded. Structured action records expose
/// only their bounded metadata; legacy records retain their event type and
/// timestamp without pretending that unstructured text has an actor, mission,
/// or cost.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AuditRecord {
    pub ts: u64,
    pub level: String,
    #[serde(rename = "type")]
    pub event_type: String,
    pub actor: Option<String>,
    pub mission: Option<String>,
    pub kind: String,
    pub outcome: Option<String>,
    pub duration_ms: Option<u64>,
    pub cost_micros: Option<u64>,
    pub correlation_id: Option<String>,
    pub pid: u64,
    pub entry_hash: String,
}

/// Verify the complete chain and return its redacted, filterable history.
///
/// The entire chain is verified before any records are returned, including
/// entries outside the requested time window. A missing log is an empty
/// history; an unreadable or damaged existing log is an error.
pub fn query_chain(audit_file: &Path, query: &AuditQuery) -> Result<Vec<AuditRecord>, String> {
    let content = match fs::read_to_string(audit_file) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("cannot read {}: {error}", audit_file.display())),
    };
    let key = load_or_create_hmac_key().map_err(|e| e.to_string())?;
    verify_content(&content, &key, None)?;

    let mut records = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            // The complete-chain verification above already rejected this
            // line. Keep the parser defensive if that implementation changes.
            continue;
        };
        let Some(entry_hash) = value.get("entry_hash").and_then(|v| v.as_str()) else {
            // Unsigned legacy records are accepted only before the signed
            // chain begins, and have no integrity-backed query metadata.
            continue;
        };
        let ts = value.get("ts").and_then(|v| v.as_u64()).unwrap_or(0);
        if !query.since.is_none_or(|minimum| ts >= minimum)
            || !query.until.is_none_or(|maximum| ts <= maximum)
        {
            continue;
        }

        let level = value
            .get("level")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let event_type = value
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let details = value.get("details").and_then(|v| v.as_str());
        let structured = details
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
            .filter(serde_json::Value::is_object);
        let actor = structured
            .as_ref()
            .and_then(|details| first_string(details, &["actor"]));
        let correlation_id = structured
            .as_ref()
            .and_then(|details| first_string(details, &["correlation_id"]));
        let mission = structured.as_ref().and_then(|details| {
            first_string(details, &["mission", "mission_id"])
                .or_else(|| correlation_id.clone())
                .or_else(|| {
                    details
                        .get("target")
                        .and_then(|target| first_string(target, &["mission", "mission_id"]))
                })
        });
        let action_kind = structured
            .as_ref()
            .and_then(|details| first_string(details, &["action_kind", "kind"]));
        let kind = action_kind.clone().unwrap_or_else(|| event_type.clone());
        let outcome = structured
            .as_ref()
            .and_then(|details| first_string(details, &["outcome"]));
        let duration_ms = structured
            .as_ref()
            .and_then(|details| details.get("duration_ms"))
            .and_then(serde_json::Value::as_u64);
        let cost_micros = structured
            .as_ref()
            .and_then(|details| details.get("cost_micros"))
            .and_then(serde_json::Value::as_u64);

        if !query
            .actor
            .as_deref()
            .is_none_or(|expected| actor.as_deref() == Some(expected))
            || !query
                .mission
                .as_deref()
                .is_none_or(|expected| mission.as_deref() == Some(expected))
            || !query
                .kind
                .as_deref()
                .is_none_or(|expected| kind == expected)
        {
            continue;
        }

        records.push(AuditRecord {
            ts,
            level,
            event_type,
            actor,
            mission,
            kind,
            outcome,
            duration_ms,
            cost_micros,
            correlation_id,
            pid: value.get("pid").and_then(|v| v.as_u64()).unwrap_or(0),
            entry_hash: entry_hash.to_string(),
        });
    }
    Ok(records)
}

fn first_string(value: &serde_json::Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        value
            .get(*key)
            .and_then(|candidate| candidate.as_str())
            .map(str::to_string)
    })
}

pub(crate) fn verify_with_key(audit_file: &Path, key: &[u8; 32]) -> Result<usize, String> {
    verify_with_key_since(audit_file, key, None)
}

pub(crate) fn verify_with_key_since(
    audit_file: &Path,
    key: &[u8; 32],
    since: Option<u64>,
) -> Result<usize, String> {
    let content = fs::read_to_string(audit_file)
        .map_err(|e| format!("cannot read {}: {e}", audit_file.display()))?;
    verify_content(&content, key, since)
}

fn verify_content(content: &str, key: &[u8; 32], since: Option<u64>) -> Result<usize, String> {
    if content.trim().is_empty() {
        return Ok(0);
    }
    let mut expected_prev = GENESIS.to_string();
    let mut signed_count = 0usize;
    let mut matching_count = 0usize;
    for (idx, line) in content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            return Err(format!("line {}: invalid JSON", idx + 1));
        };
        let Some(entry_hash) = v.get("entry_hash").and_then(|x| x.as_str()) else {
            if signed_count > 0 {
                return Err(format!(
                    "line {}: unsigned entry after signed chain began",
                    idx + 1
                ));
            }
            continue;
        };
        let prev = v.get("prev_hash").and_then(|x| x.as_str()).unwrap_or("");
        let hmac = v.get("hmac").and_then(|x| x.as_str()).unwrap_or("");
        let level = v.get("level").and_then(|x| x.as_str()).unwrap_or("");
        let event_type = v.get("type").and_then(|x| x.as_str()).unwrap_or("");
        let details = v.get("details").and_then(|x| x.as_str()).unwrap_or("");
        let pid = v.get("pid").and_then(|x| x.as_u64()).unwrap_or(0);
        let ts = v.get("ts").and_then(|x| x.as_u64()).unwrap_or(0);

        if signed_count == 0 && prev == GENESIS {
            expected_prev = GENESIS.to_string();
        }
        if prev != expected_prev {
            return Err(format!(
                "line {}: hash-link break (expected prev {}, got {})",
                idx + 1,
                expected_prev,
                prev
            ));
        }

        let mut hasher = Sha256::new();
        hasher.update(prev.as_bytes());
        hasher.update(b"|");
        hasher.update(ts.to_string().as_bytes());
        hasher.update(b"|");
        hasher.update(level.as_bytes());
        hasher.update(b"|");
        hasher.update(event_type.as_bytes());
        hasher.update(b"|");
        hasher.update(details.as_bytes());
        hasher.update(b"|");
        hasher.update(pid.to_string().as_bytes());
        let recomputed = hex::encode(hasher.finalize());
        if recomputed != entry_hash {
            return Err(format!("line {}: entry_hash mismatch", idx + 1));
        }
        if mac_hex(key, entry_hash) != hmac {
            return Err(format!("line {}: HMAC signature invalid", idx + 1));
        }
        expected_prev = entry_hash.to_string();
        signed_count += 1;
        if since.is_none_or(|minimum| ts >= minimum) {
            matching_count += 1;
        }
    }
    Ok(matching_count)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "susi_audit_chain_{name}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn forged_mac_fails_verification() {
        let log = scratch("forge").join("audit.log");
        // Explicit key: sibling tests may repoint the substrate home (and so
        // the host key path) concurrently.
        let key = [0x5a; 32];
        let entry = Entry {
            level: "Info",
            event_type: "TEST",
            details: "genuine",
            pid: 1,
        };
        append_with_key(&log, &key, &entry).unwrap();
        assert_eq!(verify_with_key(&log, &key), Ok(1));
        let content = fs::read_to_string(&log).unwrap();
        let mut entry: serde_json::Value = serde_json::from_str(content.trim()).unwrap();
        entry["hmac"] = serde_json::json!("00".repeat(32));
        fs::write(&log, format!("{entry}\n")).unwrap();
        let err = verify_with_key(&log, &key).unwrap_err();
        assert!(err.contains("HMAC signature invalid"), "{err}");
    }

    #[test]
    fn unreadable_audit_input_is_not_reported_as_an_empty_valid_chain() {
        let dir = scratch("read_error");
        let err = verify_with_key(&dir, &[0x5a; 32]).unwrap_err();
        assert!(err.contains("cannot read"), "{err}");
    }

    #[test]
    fn non_utf8_audit_input_is_not_reported_as_an_empty_valid_chain() {
        let log = scratch("non_utf8").join("audit.log");
        fs::write(&log, [0xff, 0xfe, 0xfd]).unwrap();
        let err = verify_with_key(&log, &[0x5a; 32]).unwrap_err();
        assert!(err.contains("cannot read"), "{err}");
    }

    #[test]
    fn stale_tip_checkpoint_cannot_fork_the_chain() {
        let log = scratch("stale_tip").join("audit.log");
        let key = [0x5a; 32];
        let first = append_with_key(
            &log,
            &key,
            &Entry {
                level: "Info",
                event_type: "FIRST",
                details: "one",
                pid: 1,
            },
        )
        .unwrap();
        append_with_key(
            &log,
            &key,
            &Entry {
                level: "Info",
                event_type: "SECOND",
                details: "two",
                pid: 1,
            },
        )
        .unwrap();

        // Simulate a process that persisted the first checkpoint but crashed
        // after the second complete log append and before updating the tip.
        let stale = serde_json::json!({
            "entry_hash": first,
            "log_len": fs::metadata(&log).unwrap().len() - 1,
        });
        fs::write(tip_path(&log), stale.to_string()).unwrap();
        append_with_key(
            &log,
            &key,
            &Entry {
                level: "Info",
                event_type: "THIRD",
                details: "three",
                pid: 1,
            },
        )
        .unwrap();

        assert_eq!(verify_with_key(&log, &key), Ok(3));
    }
}
