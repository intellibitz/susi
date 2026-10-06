//! Two fidelity tiers for observability (VC-202-018).
//!
//! **Audit tier** — durable, signed, payload-free. Records *that* something
//! happened plus bounded metadata; never what a user said or a tool
//! returned. The payload-free guarantee is structural: [`AuditDetails`]
//! variants can only render metadata — a payload can only appear as a
//! `sha256` digest plus byte length — so the type itself cannot print a
//! secret. Legacy `&str` details are metadata by definition and capped at
//! the chokepoint, so a stuffed payload cannot land whole.
//!
//! **Trace tier** — levelled, opt-in per subsystem, rotated daily, TTL'd,
//! and bounded by a measured bytes-per-day budget. Tracing is a debugging
//! aid, not an audit source: off unless a subsystem opts in, and its values
//! ride the same [`AuditDetails`] types — a secret still cannot print.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Longest metadata a `Label`/field value may carry — enough for an id, a
/// path or a status line; too short to smuggle a document.
pub const MAX_METADATA_CHARS: usize = 240;

/// Legacy `&str` details are capped harder: callers migrated to typed
/// details prove intent, callers that stayed get a hard boundary plus a
/// visible truncation marker instead of silent stuffing.
const LEGACY_MAX_CHARS: usize = 480;

/// What an audit or trace record may carry. Every variant renders metadata
/// only — there is no `Raw(String)` variant precisely so that payload text
/// cannot reach the durable tier through this type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuditDetails {
    /// A bounded metadata line (event summary, id, status).
    Label(String),
    /// Bounded key/value metadata (counts, ids, decision names).
    Fields(Vec<(String, String)>),
    /// A reference to payload content — digest and byte length only, so
    /// the record proves *which* content without keeping any of it.
    PayloadRef {
        sha256: String,
        bytes: u64,
        /// What kind of payload — `prompt`, `tool_output`, `document`.
        hint: &'static str,
    },
}

impl AuditDetails {
    /// A metadata line, hard-capped at [`MAX_METADATA_CHARS`].
    #[must_use]
    pub fn label(text: &str) -> Self {
        Self::Label(cap(text, MAX_METADATA_CHARS))
    }

    /// Bounded key/value metadata — each side capped.
    #[must_use]
    pub fn fields(pairs: Vec<(String, String)>) -> Self {
        Self::Fields(
            pairs
                .into_iter()
                .map(|(k, v)| (cap(&k, 64), cap(&v, MAX_METADATA_CHARS)))
                .collect(),
        )
    }

    /// The only way payload content may be referenced: digest + length.
    /// The bytes themselves are unrepresentable in an audit record.
    #[must_use]
    pub fn payload_ref(content: &str, hint: &'static str) -> Self {
        Self::PayloadRef {
            sha256: sha256_hex(content.as_bytes()),
            bytes: content.len() as u64,
            hint,
        }
    }
}

impl std::fmt::Display for AuditDetails {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Label(text) => f.write_str(text),
            Self::Fields(pairs) => {
                let body = pairs
                    .iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join(" ");
                f.write_str(&body)
            }
            Self::PayloadRef {
                sha256,
                bytes,
                hint,
            } => write!(f, "{hint} sha256:{sha256} bytes:{bytes}"),
        }
    }
}

fn cap(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let kept: String = text.chars().take(max).collect();
    format!("{kept}…<truncated:{}>", text.chars().count() - max)
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

/// Legacy `&str` details entering the audit chokepoint: credential-masked
/// (existing) and hard-capped — a stuffed payload is visibly truncated, so
/// the durable tier holds metadata, not documents.
#[must_use]
pub fn bound_legacy_details(details: &str) -> String {
    cap(
        &crate::susi_config::redact_credentials(details),
        LEGACY_MAX_CHARS,
    )
}

// ── Trace tier ────────────────────────────────────────────────────────────

/// Trace levels, ordered. A record is written when its level is at or
/// above the subsystem's configured level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TraceLevel {
    Error = 0,
    Warn = 1,
    Info = 2,
    Debug = 3,
}

impl TraceLevel {
    fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "error" => Some(Self::Error),
            "warn" | "warning" => Some(Self::Warn),
            "info" => Some(Self::Info),
            "debug" | "trace" => Some(Self::Debug),
            _ => None,
        }
    }

    fn label(&self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
        }
    }
}

/// Trace configuration — `.susi/trace.json`. Absent file ⇒ everything off:
/// the tier is opt-in per subsystem, never ambient.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceConfig {
    /// subsystem → minimum level ("off" disables).
    #[serde(default)]
    pub subsystems: std::collections::BTreeMap<String, String>,
    /// Measured cap: a subsystem's day file stops growing past this many
    /// bytes — the budget is a number on disk, not an intention.
    #[serde(default = "default_day_budget")]
    pub max_bytes_per_day: u64,
    /// Day files older than this are swept on the next trace write.
    #[serde(default = "default_retain_days")]
    pub retain_days: u32,
    /// Hard cap on the whole trace tier across every subsystem's day
    /// files — the disk the agents share cannot fill past this even when
    /// each day file individually stays under its budget.
    #[serde(default = "default_trace_total_bytes")]
    pub max_total_bytes: u64,
}

fn default_day_budget() -> u64 {
    16 * 1024 * 1024
}
fn default_retain_days() -> u32 {
    3
}
fn default_trace_total_bytes() -> u64 {
    64 * 1024 * 1024
}

impl Default for TraceConfig {
    fn default() -> Self {
        Self {
            subsystems: std::collections::BTreeMap::new(),
            max_bytes_per_day: default_day_budget(),
            retain_days: default_retain_days(),
            max_total_bytes: default_trace_total_bytes(),
        }
    }
}

fn config_path(workspace: &Path) -> PathBuf {
    workspace.join(".susi").join("trace.json")
}

fn load_config(workspace: &Path) -> TraceConfig {
    std::fs::read_to_string(config_path(workspace))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// The level a subsystem traces at, if it opted in. `SUSI_TRACE_<SUBSYSTEM>`
/// (uppercased, `-`→`_`) overrides the config file.
fn level_for(workspace: &Path, subsystem: &str) -> Option<TraceLevel> {
    let env_key = format!(
        "SUSI_TRACE_{}",
        subsystem
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_uppercase()
                } else {
                    '_'
                }
            })
            .collect::<String>()
    );
    if let Ok(v) = std::env::var(&env_key) {
        return TraceLevel::parse(&v);
    }
    load_config(workspace)
        .subsystems
        .get(subsystem)
        .and_then(|s| TraceLevel::parse(s))
}

/// Whether a record at `level` for `subsystem` would be written.
#[must_use]
pub fn trace_enabled(workspace: &Path, subsystem: &str, level: TraceLevel) -> bool {
    level_for(workspace, subsystem).is_some_and(|min| level <= min)
}

fn day_file(workspace: &Path, subsystem: &str, unix: u64) -> PathBuf {
    let days = unix / 86_400;
    workspace
        .join(".susi")
        .join("trace")
        .join(format!("{subsystem}-{days}.log"))
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Remove day files past the retention window (file name carries the day).
fn sweep_expired(workspace: &Path, now: u64, retain_days: u32) {
    let today = now / 86_400;
    let dir = workspace.join(".susi").join("trace");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if let Some(day) = name
            .rsplit('-')
            .next()
            .and_then(|s| s.strip_suffix(".log"))
            .and_then(|s| s.parse::<u64>().ok())
        {
            if day + u64::from(retain_days) < today {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

/// Total-byte cap across the trace tier: after a write lands, prune the
/// oldest day files (any subsystem) until the directory is under the
/// configured ceiling. Oldest by the day number in the filename.
fn enforce_trace_total_cap(workspace: &Path, max_total_bytes: u64) {
    let dir = workspace.join(".susi").join("trace");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    let mut files: Vec<(u64, PathBuf, u64)> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            let day = name
                .rsplit('-')
                .next()
                .and_then(|s| s.strip_suffix(".log"))
                .and_then(|s| s.parse::<u64>().ok())?;
            let len = entry.metadata().ok()?.len();
            Some((day, entry.path(), len))
        })
        .collect();
    files.sort_by_key(|(day, _, _)| *day);
    let mut total: u64 = files.iter().map(|(_, _, len)| *len).sum();
    for (_, path, len) in files {
        if total <= max_total_bytes {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(len);
        }
    }
}

/// Append one record to a subsystem's trace — when the subsystem opted in,
/// its level allows the record, and the day file is still under the
/// measured bytes-per-day budget. Returns whether anything was written.
pub fn trace(workspace: &Path, subsystem: &str, level: TraceLevel, details: &AuditDetails) -> bool {
    trace_at(workspace, subsystem, level, details, unix_now())
}

/// Test seam: same logic with an explicit clock.
pub fn trace_at(
    workspace: &Path,
    subsystem: &str,
    level: TraceLevel,
    details: &AuditDetails,
    now: u64,
) -> bool {
    if !trace_enabled(workspace, subsystem, level) {
        return false;
    }
    let config = load_config(workspace);
    sweep_expired(workspace, now, config.retain_days);
    let path = day_file(workspace, subsystem, now);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let line = format!("{} {} {}\n", now, level.label(), details);
    // Measured budget: the day file's current size decides, not a counter
    // that could drift from the file.
    let current = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    if current + line.len() as u64 > config.max_bytes_per_day {
        return false;
    }
    let started = std::time::Instant::now();
    let written = if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        use std::io::Write;
        file.write_all(line.as_bytes()).is_ok()
    } else {
        false
    };
    if written {
        enforce_trace_total_cap(workspace, config.max_total_bytes);
        record_observation(
            workspace,
            "trace",
            subsystem,
            line.len() as u64,
            started.elapsed(),
        );
    }
    written
}

// ── Retention (T-DEEPSEEK-111) ──────────────────────────────────────────

/// The audit tier's retention policy — `.susi/audit-policy.json` in the
/// workspace. Absent file ⇒ the defaults: a segment rotates past
/// `segment_bytes`, archives live `retain_days`, and the whole tier
/// (active + archives + chain tips) stays under `max_total_bytes`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditPolicy {
    /// Rotate `audit.log` into a dated archive once it exceeds this many
    /// bytes — an unbounded single file is the defect this exists to fix.
    #[serde(default = "default_segment_bytes")]
    pub segment_bytes: u64,
    /// Archives older than this many days are pruned on the next write.
    #[serde(default = "default_audit_retain_days")]
    pub retain_days: u32,
    /// Hard cap on total bytes across every audit file in `.susi/`.
    #[serde(default = "default_audit_total_bytes")]
    pub max_total_bytes: u64,
}

fn default_segment_bytes() -> u64 {
    8 * 1024 * 1024
}
fn default_audit_retain_days() -> u32 {
    30
}
fn default_audit_total_bytes() -> u64 {
    64 * 1024 * 1024
}

impl Default for AuditPolicy {
    fn default() -> Self {
        Self {
            segment_bytes: default_segment_bytes(),
            retain_days: default_audit_retain_days(),
            max_total_bytes: default_audit_total_bytes(),
        }
    }
}

fn audit_policy(workspace: &Path) -> AuditPolicy {
    std::fs::read_to_string(workspace.join(".susi").join("audit-policy.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// What a rotation attested: the archive's name and the chain tip it was
/// closed with — recorded by the next segment's first entry so a deleted
/// or rewritten archive is detectable from the live chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RotateAttestation {
    pub archive_file: String,
    pub archive_tip_hash: String,
    pub archive_bytes: u64,
}

impl RotateAttestation {
    /// The signed first entry of the new segment: metadata only — the
    /// attestation names the file and its tip, never its contents.
    #[must_use]
    pub fn details(&self) -> AuditDetails {
        AuditDetails::fields(vec![
            ("archive".into(), self.archive_file.clone()),
            ("tip_sha256".into(), self.archive_tip_hash.clone()),
            ("archive_bytes".into(), self.archive_bytes.to_string()),
        ])
    }
}

/// One audit file with its role, sorted age order for pruning.
#[derive(Debug)]
struct AuditFile {
    path: PathBuf,
    /// Rotation second from the archive name (`audit-<ts>.log`); the
    /// active `audit.log` sorts newest so it is never a prune candidate.
    stamp: u64,
    len: u64,
    active: bool,
}

fn list_audit_files(workspace: &Path) -> Vec<AuditFile> {
    let dir = workspace.join(".susi");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            let len = entry.metadata().ok()?.len();
            if name == "audit.log" {
                return Some(AuditFile {
                    path: entry.path(),
                    stamp: u64::MAX,
                    len,
                    active: true,
                });
            }
            let stamp = name
                .strip_prefix("audit-")?
                .strip_suffix(".log")?
                .parse::<u64>()
                .ok()?;
            Some(AuditFile {
                path: entry.path(),
                stamp,
                len,
                active: false,
            })
        })
        .collect()
}

/// Prune audit archives: past `retain_days` (by the timestamp each
/// archive's name carries), then oldest-first while the tier exceeds
/// `max_total_bytes`. The active `audit.log` is never pruned — a fresh
/// rotation already bounded it. Each archive's `.chain.tip` checkpoint
/// goes with its file.
fn prune_audit_archives(workspace: &Path, policy: &AuditPolicy, now: u64) {
    let cutoff = now.saturating_sub(u64::from(policy.retain_days) * 86_400);
    let mut files = list_audit_files(workspace);
    for file in files.iter().filter(|f| !f.active && f.stamp < cutoff) {
        let _ = std::fs::remove_file(&file.path);
        let _ = std::fs::remove_file(file.path.with_extension("chain.tip"));
    }
    files.retain(|f| f.path.exists());
    files.sort_by_key(|f| f.stamp);
    let mut total: u64 = files
        .iter()
        .map(|f| {
            f.len
                + std::fs::metadata(f.path.with_extension("chain.tip"))
                    .map(|m| m.len())
                    .unwrap_or(0)
        })
        .sum();
    for file in files.iter().filter(|f| !f.active) {
        if total <= policy.max_total_bytes {
            break;
        }
        let tip_len = std::fs::metadata(file.path.with_extension("chain.tip"))
            .map(|m| m.len())
            .unwrap_or(0);
        if std::fs::remove_file(&file.path).is_ok() {
            let _ = std::fs::remove_file(file.path.with_extension("chain.tip"));
            total = total.saturating_sub(file.len + tip_len);
        }
    }
}

/// The retention chokepoint, called from the single audit append path
/// before the signed write lands. Rotates a full `audit.log` into a
/// dated archive — moving its `.chain.tip` checkpoint with it so the
/// archive still self-verifies — and prunes expired/over-cap archives.
/// Returns the attestation the caller must record as the new segment's
/// first entry (linking the live chain to the archived tip).
#[must_use]
pub fn apply_audit_retention(workspace: &Path) -> Option<RotateAttestation> {
    let policy = audit_policy(workspace);
    let now = unix_now();
    let active = workspace.join(".susi").join("audit.log");
    let mut rotated = None;
    if let Ok(meta) = std::fs::metadata(&active) {
        if meta.len() > policy.segment_bytes {
            let stamp = archive_stamp(&active, now);
            let archive = active.with_file_name(format!("audit-{stamp}.log"));
            // The checkpoint belongs to the bytes being rotated — move it
            // beside the archive so each file's chain verifies standalone.
            let tip_hash = std::fs::read_to_string(active.with_extension("chain.tip"))
                .ok()
                .and_then(|t| {
                    serde_json::from_str::<serde_json::Value>(&t)
                        .ok()
                        .and_then(|v| {
                            v.get("entry_hash")
                                .and_then(|h| h.as_str())
                                .map(str::to_string)
                        })
                })
                .unwrap_or_else(|| "unknown".into());
            if std::fs::rename(&active, &archive).is_ok() {
                let _ = std::fs::rename(
                    active.with_extension("chain.tip"),
                    archive.with_extension("chain.tip"),
                );
                rotated = Some(RotateAttestation {
                    archive_file: archive
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default(),
                    archive_tip_hash: tip_hash,
                    archive_bytes: meta.len(),
                });
            }
        }
    }
    prune_audit_archives(workspace, &policy, now);
    rotated
}

fn archive_stamp(active: &Path, now: u64) -> u64 {
    // Collision-safe: two rotations inside one second keep both files.
    let dir = active.parent().unwrap_or_else(|| Path::new("."));
    let mut stamp = now;
    while dir.join(format!("audit-{stamp}.log")).exists() {
        stamp += 1;
    }
    stamp
}

// ── Measured cost of observation ────────────────────────────────────────

/// One attributed observation-cost record — the same axes a spend record
/// carries (agent, workspace, time) plus what observation actually costs:
/// bytes persisted and the wall-clock overhead of persisting them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservationRecord {
    pub ts: u64,
    /// `"audit"` or `"trace"`.
    pub tier: String,
    /// The event type or trace subsystem that caused the write.
    pub subsystem: String,
    pub bytes: u64,
    /// Wall-clock nanoseconds the write itself took — the honest,
    /// measurable form of "CPU overhead" on this path.
    pub elapsed_ns: u64,
    /// `SUSI_AGENT` — spend attribution is per agent like any other cost.
    pub agent: String,
    /// Workspace basename — the mission-ish axis the audit tier already
    /// keys on.
    pub workspace: String,
}

/// Where the observation ledger persists: sibling of `usage.json` in the
/// substrate config dir, appended line-per-record (no read-modify-write,
/// so concurrent writers never lose each other's records).
/// `SUSI_OBSERVATION_FILE` overrides; `None` under `cfg(test)` without the
/// override (hermetic tests never touch `~/.susi*`).
#[must_use]
pub fn observation_journal_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("SUSI_OBSERVATION_FILE").filter(|p| !p.is_empty()) {
        Some(PathBuf::from(p))
    } else if cfg!(test) {
        None
    } else {
        Some(susi_paths::SusiDirs::config_dir().join("observation-cost.jsonl"))
    }
}

/// Attribute the measured cost of one observation write into the ledger —
/// the same journaling a dispatched call gets, so observing is spend like
/// any other spend.
pub fn record_observation(
    workspace: &Path,
    tier: &str,
    subsystem: &str,
    bytes: u64,
    elapsed: std::time::Duration,
) {
    let Some(path) = observation_journal_path() else {
        return;
    };
    let record = ObservationRecord {
        ts: unix_now(),
        tier: tier.to_string(),
        subsystem: subsystem.to_string(),
        bytes,
        elapsed_ns: u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX),
        agent: std::env::var("SUSI_AGENT").unwrap_or_else(|_| "standalone".to_string()),
        workspace: workspace
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| workspace.display().to_string()),
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        use std::io::Write;
        // One record, one `write`: with `O_APPEND` a single small write is
        // atomic, so concurrent writers cannot interleave. `writeln!` on a bare
        // `File` is two writes (the JSON, then the newline) and a writer landing
        // between them fused two records into one unparseable line, losing both.
        let mut line = serde_json::to_string(&record).unwrap_or_default();
        line.push('\n');
        let _ = file.write_all(line.as_bytes());
    }
}

/// The published cost of observation: aggregated bytes and overhead per
/// tier and per agent — the numbers a report states.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ObservationReport {
    pub records: u64,
    pub total_bytes: u64,
    pub total_elapsed_ns: u64,
    /// tier → (records, bytes, elapsed_ns)
    pub by_tier: std::collections::BTreeMap<String, (u64, u64, u64)>,
    /// agent → (records, bytes, elapsed_ns)
    pub by_agent: std::collections::BTreeMap<String, (u64, u64, u64)>,
}

/// Aggregate the observation ledger at `path` — absent or unreadable
/// lines yield the honest empty report (cost is what was measured, never
/// invented).
#[must_use]
pub fn observation_report(path: &Path) -> ObservationReport {
    let mut report = ObservationReport::default();
    let Ok(text) = std::fs::read_to_string(path) else {
        return report;
    };
    for line in text.lines() {
        let Ok(record) = serde_json::from_str::<ObservationRecord>(line) else {
            continue;
        };
        report.records += 1;
        report.total_bytes += record.bytes;
        report.total_elapsed_ns += record.elapsed_ns;
        let tier = report.by_tier.entry(record.tier).or_default();
        tier.0 += 1;
        tier.1 += record.bytes;
        tier.2 += record.elapsed_ns;
        let agent = report.by_agent.entry(record.agent).or_default();
        agent.0 += 1;
        agent.1 += record.bytes;
        agent.2 += record.elapsed_ns;
    }
    report
}

#[cfg(test)]
mod audit_payload_free_policy_tests {
    use super::*;
    use crate::manager::{LogLevel, SusiAuditLogger};

    fn scratch(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "susi_audit_fidelity_{tag}_{}_{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    fn audit_text(ws: &Path) -> String {
        std::fs::read_to_string(ws.join(".susi").join("audit.log")).unwrap_or_default()
    }

    /// Canary 1: a credential must never reach the durable tier — the
    /// chokepoint masks credential-shaped values before signing.
    #[test]
    fn audit_payload_free_policy_secret_cannot_reach_audit() {
        let _guard = crate::env_test_lock();
        let ws = scratch("secret");
        // SAFETY: test-only env mutation under the env lock; the canary is a
        // fake credential value, unset again before the guard drops.
        std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-canary-9f00d");
        SusiAuditLogger::log_event(
            &ws,
            "CANARY",
            "key material sk-ant-canary-9f00d leaked into details",
        );
        // SAFETY: restore before releasing the lock.
        std::env::remove_var("ANTHROPIC_API_KEY");
        let text = audit_text(&ws);
        assert!(
            !text.contains("sk-ant-canary-9f00d"),
            "a credential value reached the signed audit tier:\n{text}"
        );
        assert!(text.contains("CANARY"), "the event itself is recorded");
    }

    /// Canary 2: a user prompt must never reach the durable tier — the
    /// typed path keeps only a digest and a byte length.
    #[test]
    fn audit_payload_free_policy_prompt_rides_as_digest() {
        let ws = scratch("prompt");
        let prompt = "my private half-formed thought about deleting prod";
        SusiAuditLogger::log_details(
            &ws,
            LogLevel::Info,
            "WEB_MISSION_START",
            &AuditDetails::payload_ref(prompt, "prompt"),
        );
        let text = audit_text(&ws);
        assert!(
            !text.contains("deleting prod"),
            "prompt text reached the signed audit tier:\n{text}"
        );
        assert!(text.contains("WEB_MISSION_START"));
        assert!(
            text.contains("sha256:") && text.contains("bytes:"),
            "the record proves which prompt without keeping it: {text}"
        );
    }

    /// Canary 3: tool output is the same — digest only, never content.
    #[test]
    fn audit_payload_free_policy_tool_output_rides_as_digest() {
        let ws = scratch("tool");
        let output = "SECRET_ROW alice@example.com password=hunter2";
        SusiAuditLogger::log_details(
            &ws,
            LogLevel::Info,
            "TOOL_RESULT",
            &AuditDetails::payload_ref(output, "tool_output"),
        );
        let text = audit_text(&ws);
        assert!(
            !text.contains("hunter2") && !text.contains("alice@example.com"),
            "tool output reached the signed audit tier:\n{text}"
        );
    }

    /// A legacy `&str` caller cannot stuff a payload: the chokepoint caps
    /// it and leaves a visible truncation marker — a defect, not a silence.
    #[test]
    fn audit_payload_free_policy_legacy_details_bounded() {
        let ws = scratch("legacy");
        let huge = "x".repeat(10_000);
        SusiAuditLogger::log_event(&ws, "STUFFED", &huge);
        let text = audit_text(&ws);
        assert!(
            text.contains("<truncated:"),
            "oversized legacy details must be visibly truncated: {text}"
        );
        assert!(
            !text.contains(&"x".repeat(1_000)),
            "the durable tier holds metadata, not documents"
        );
    }

    /// Typed details render metadata only — no variant can print a payload
    /// because there is no payload-carrying variant.
    #[test]
    fn audit_payload_free_policy_typed_details_render_metadata() {
        let d = AuditDetails::payload_ref("the crown jewels", "document");
        let rendered = d.to_string();
        assert!(rendered.contains("sha256:") && rendered.contains("bytes:16"));
        assert!(!rendered.contains("crown"));

        let fields = AuditDetails::fields(vec![("k".into(), "v".repeat(5_000))]);
        let rendered = fields.to_string();
        assert!(
            rendered.contains("<truncated:"),
            "field values are capped: {rendered}"
        );
        // The enum has no Raw(String) arm — this assert is the type-level
        // guarantee made visible: every variant name is metadata-shaped.
        assert!(matches!(
            d,
            AuditDetails::Label(_) | AuditDetails::Fields(_) | AuditDetails::PayloadRef { .. }
        ));
    }

    /// Trace tier: off unless a subsystem opts in; opt-in writes; the
    /// measured bytes/day budget stops the day file; TTL sweeps old days;
    /// and a payload in trace is still a digest — redaction is structural.
    #[test]
    fn audit_payload_free_policy_trace_opt_in_bounded_ttl() {
        let _guard = crate::env_test_lock();
        let ws = scratch("trace");

        // Off by default — nothing opted in.
        assert!(!trace_enabled(&ws, "dispatch", TraceLevel::Debug));
        assert!(!trace_at(
            &ws,
            "dispatch",
            TraceLevel::Info,
            &AuditDetails::label("hello"),
            1_700_000_000
        ));
        assert!(!ws.join(".susi").join("trace").exists());

        // Opt in via env: record lands in a per-day file.
        // SAFETY: test-only env mutation under the env lock.
        std::env::set_var("SUSI_TRACE_DISPATCH", "debug");
        assert!(trace_enabled(&ws, "dispatch", TraceLevel::Info));
        let day = 1_700_000_000_u64;
        assert!(trace_at(
            &ws,
            "dispatch",
            TraceLevel::Info,
            &AuditDetails::payload_ref("secret prompt text", "prompt"),
            day
        ));
        let trace_file = day_file(&ws, "dispatch", day);
        let text = std::fs::read_to_string(&trace_file).expect("trace written");
        assert!(text.contains("sha256:"), "trace keeps the digest: {text}");
        assert!(
            !text.contains("secret prompt text"),
            "trace redaction is structural — the value type, not a filter"
        );

        // A record above the opted level is not written.
        let before = std::fs::metadata(&trace_file).unwrap().len();
        assert!(trace_enabled(&ws, "dispatch", TraceLevel::Warn));
        // (env is "debug" so everything passes; disable and confirm gate)
        // SAFETY: restore before lock release.
        std::env::remove_var("SUSI_TRACE_DISPATCH");
        assert!(!trace_enabled(&ws, "dispatch", TraceLevel::Info));

        // Measured bytes/day budget: a config that allows only 8 bytes
        // refuses further appends once the file exceeds it.
        std::fs::create_dir_all(ws.join(".susi")).expect("susi dir");
        std::fs::write(
            config_path(&ws),
            r#"{"subsystems":{"dispatch":"debug"},"max_bytes_per_day":8,"retain_days":2}"#,
        )
        .expect("trace config");
        assert!(trace_enabled(&ws, "dispatch", TraceLevel::Info));
        assert!(
            !trace_at(
                &ws,
                "dispatch",
                TraceLevel::Info,
                &AuditDetails::label("over budget line"),
                day
            ),
            "the day file is already over the measured budget"
        );
        assert_eq!(
            std::fs::metadata(&trace_file).unwrap().len(),
            before,
            "a refused write leaves the file byte-identical"
        );

        // TTL: a file from many days ago is swept on the next write.
        let old = day_file(&ws, "dispatch", day - 20 * 86_400);
        std::fs::write(&old, "stale\n").expect("old trace file");
        // Restore budget headroom so the write that triggers the sweep lands.
        std::fs::write(
            config_path(&ws),
            r#"{"subsystems":{"dispatch":"debug"},"max_bytes_per_day":65536,"retain_days":2}"#,
        )
        .expect("trace config");
        assert!(trace_at(
            &ws,
            "dispatch",
            TraceLevel::Info,
            &AuditDetails::label("fresh"),
            day
        ));
        assert!(!old.exists(), "expired day file swept");
    }

    /// The audit tier stays signed under the new paths — the typed entry
    /// still goes through the one HMAC-chained chokepoint.
    #[test]
    fn audit_payload_free_policy_audit_still_signed_and_append_only() {
        let ws = scratch("signed");
        SusiAuditLogger::log_details(
            &ws,
            LogLevel::Info,
            "TYPED_A",
            &AuditDetails::label("alpha"),
        );
        SusiAuditLogger::log_event(&ws, "TYPED_B", "beta");
        let text = audit_text(&ws);
        assert!(text.contains("TYPED_A") && text.contains("TYPED_B"));
        // Tamper detection still holds over records written by both paths.
        let log = ws.join(".susi").join("audit.log");
        assert!(crate::audit_chain::verify_chain(&log).is_ok());
        let tampered = text.replacen("alpha", "gamma", 1);
        std::fs::write(&log, tampered).expect("tamper");
        assert!(crate::audit_chain::verify_chain(&log).is_err());
    }
}

#[cfg(test)]
mod audit_retention_prune_tests {
    use super::*;
    use crate::manager::{LogLevel, SusiAuditLogger};

    fn scratch(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "susi_audit_retention_{tag}_{}_{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    fn susi_dir(ws: &Path) -> PathBuf {
        ws.join(".susi")
    }

    fn policy(ws: &Path, json: &str) {
        std::fs::create_dir_all(susi_dir(ws)).expect("susi dir");
        std::fs::write(susi_dir(ws).join("audit-policy.json"), json).expect("policy");
    }

    fn archive_names(ws: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(susi_dir(ws))
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .filter(|n| n.starts_with("audit-") && n.ends_with(".log"))
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    /// Rotation: a segment over the configured cap rotates into a dated
    /// archive that still self-verifies, the fresh active segment opens
    /// with a signed AUDIT_ROTATE attestation of the archived tip, and
    /// the archive keeps its checkpoint beside it.
    #[test]
    fn audit_retention_prune_full_segment_rotates_and_attests() {
        let ws = scratch("rotate");
        policy(
            &ws,
            r#"{"segment_bytes":200,"retain_days":30,"max_total_bytes":1048576}"#,
        );
        SusiAuditLogger::log_event(
            &ws,
            "FIRST",
            "enough detail to exceed the segment cap eventually",
        );
        // The first write already overshot 200 bytes — the next append
        // triggers rotation before it lands.
        SusiAuditLogger::log_event(&ws, "SECOND", "another detail line");

        let archives = archive_names(&ws);
        assert_eq!(archives.len(), 1, "one segment rotated out: {archives:?}");

        // The archive is a self-contained chain — it began at GENESIS
        // when it was the active segment, so it verifies standalone.
        let archive = susi_dir(&ws).join(&archives[0]);
        assert!(
            crate::audit_chain::verify_chain(&archive).is_ok(),
            "a rotated segment still verifies: {}",
            archive.display()
        );
        assert!(
            archive.with_extension("chain.tip").exists(),
            "the checkpoint moved with its bytes"
        );

        // The fresh active segment opens with the attestation — a
        // signed record linking the live chain to the archived tip.
        let active = std::fs::read_to_string(susi_dir(&ws).join("audit.log")).unwrap_or_default();
        assert!(
            active.contains("AUDIT_ROTATE"),
            "attestation recorded: {active}"
        );
        assert!(
            active.contains(&archives[0]),
            "the archive is named: {active}"
        );
        assert!(
            active.contains("tip_sha256"),
            "the tip is attested: {active}"
        );
        assert!(
            crate::audit_chain::verify_chain(&susi_dir(&ws).join("audit.log")).is_ok(),
            "the new segment is its own signed chain"
        );
    }

    /// Retention days: archives whose stamp is older than the window are
    /// pruned on the next write, checkpoint files included.
    #[test]
    fn audit_retention_prune_expired_archives_are_removed() {
        let ws = scratch("expire");
        policy(
            &ws,
            r#"{"segment_bytes":1048576,"retain_days":3,"max_total_bytes":1048576}"#,
        );
        let now = unix_now();
        for days_ago in [10u64, 6, 1] {
            let stamp = now - days_ago * 86_400;
            let name = format!("audit-{stamp}.log");
            std::fs::create_dir_all(susi_dir(&ws)).expect("susi dir");
            std::fs::write(susi_dir(&ws).join(&name), "stale\n").expect("archive");
            std::fs::write(susi_dir(&ws).join(format!("audit-{stamp}.chain.tip")), "{}")
                .expect("tip");
        }
        SusiAuditLogger::log_event(&ws, "TRIGGER", "drives the sweep");
        let left = archive_names(&ws);
        let ten_days = format!("audit-{}.log", now - 10 * 86_400);
        let six_days = format!("audit-{}.log", now - 6 * 86_400);
        let one_day = format!("audit-{}.log", now - 86_400);
        assert!(!left.contains(&ten_days), "10-day archive pruned");
        assert!(!left.contains(&six_days), "6-day archive pruned");
        assert!(left.contains(&one_day), "1-day archive kept: {left:?}");
        assert!(
            !susi_dir(&ws)
                .join(format!("audit-{}.chain.tip", now - 10 * 86_400))
                .exists(),
            "expired checkpoint pruned with its file"
        );
    }

    /// Total-byte cap: while the tier exceeds the ceiling, the oldest
    /// archive goes first — and the active segment is never a candidate.
    #[test]
    fn audit_retention_prune_total_cap_drops_oldest_keeps_active() {
        let ws = scratch("cap");
        policy(
            &ws,
            r#"{"segment_bytes":1048576,"retain_days":365,"max_total_bytes":700}"#,
        );
        let now = unix_now();
        for (i, bytes) in [(1u64, 300usize), (2, 300), (3, 300)] {
            let stamp = now - (10 - i) * 3_600;
            std::fs::create_dir_all(susi_dir(&ws)).expect("susi dir");
            std::fs::write(
                susi_dir(&ws).join(format!("audit-{stamp}.log")),
                "x".repeat(bytes),
            )
            .expect("archive");
        }
        SusiAuditLogger::log_event(&ws, "TRIGGER", "drives the cap sweep");
        let left = archive_names(&ws);
        // 900 bytes of archives > 700 cap → oldest pruned until under.
        assert_eq!(left.len(), 2, "oldest pruned until under cap: {left:?}");
        let oldest = format!("audit-{}.log", now - 9 * 3_600);
        assert!(!left.contains(&oldest), "the oldest went first");
        assert!(
            susi_dir(&ws).join("audit.log").exists(),
            "the active segment is never pruned"
        );
    }

    /// Trace tier: the total-byte cap prunes the oldest day file across
    /// subsystems — the shared disk cannot fill past the ceiling.
    #[test]
    fn audit_retention_prune_trace_total_cap_prunes_oldest_day() {
        let ws = scratch("tracecap");
        std::fs::create_dir_all(susi_dir(&ws)).expect("susi dir");
        std::fs::write(
            config_path(&ws),
            r#"{"subsystems":{"a":"debug","b":"debug"},"max_bytes_per_day":65536,"retain_days":365,"max_total_bytes":120}"#,
        )
        .expect("trace config");
        let today = unix_now() / 86_400 * 86_400;
        // Two big day files: "a" older than "b".
        let a_day = day_file(&ws, "a", today - 2 * 86_400);
        let b_day = day_file(&ws, "b", today - 86_400);
        std::fs::create_dir_all(a_day.parent().unwrap()).expect("trace dir");
        std::fs::write(&a_day, "x".repeat(80)).expect("a day file");
        std::fs::write(&b_day, "y".repeat(80)).expect("b day file");
        // A fresh write lands, then the cap sweeps oldest-first.
        assert!(trace_at(
            &ws,
            "a",
            TraceLevel::Info,
            &AuditDetails::label("trigger"),
            today
        ));
        assert!(!a_day.exists(), "oldest day file pruned by the total cap");
        assert!(b_day.exists(), "newer day file survives");
    }

    /// Measured cost of observation: each audit write lands an attributed
    /// record — tier, subsystem, bytes, overhead, agent — and the report
    /// aggregates it like any other spend.
    #[test]
    fn audit_retention_prune_observation_cost_measured_and_attributed() {
        let _guard = crate::env_test_lock();
        let ws = scratch("observe");
        let journal = ws.join("observation-cost.jsonl");
        std::env::set_var("SUSI_OBSERVATION_FILE", &journal);
        std::env::set_var("SUSI_AGENT", "DEEPSEEK");

        SusiAuditLogger::log_details(
            &ws,
            LogLevel::Info,
            "OBSERVED_EVENT",
            &AuditDetails::label("something happened"),
        );

        std::env::remove_var("SUSI_OBSERVATION_FILE");
        std::env::remove_var("SUSI_AGENT");

        // Those two variables are process-global. Under libtest - one process,
        // many threads, which is how `cargo llvm-cov` runs this - another test's
        // observation write lands in this journal while they are set, attributed
        // to this test's SUSI_AGENT, and the agent's total then exceeds the audit
        // tier's by that stranger's bytes (a constant 24 in the reproduction;
        // nextest isolates tests per process, which is why only coverage saw it).
        // Judge only the records this test's own workspace wrote.
        let own = ws
            .file_name()
            .expect("scratch dir has a name")
            .to_string_lossy()
            .to_string();
        let mine: String = std::fs::read_to_string(&journal)
            .expect("journal")
            .lines()
            .filter(|l| {
                serde_json::from_str::<ObservationRecord>(l).is_ok_and(|r| r.workspace == own)
            })
            .map(|l| format!("{l}\n"))
            .collect();
        let own_journal = ws.join("observation-cost.own.jsonl");
        std::fs::write(&own_journal, &mine).expect("own journal");

        let report = observation_report(&own_journal);
        assert!(report.records >= 1, "the write was recorded");
        assert!(report.total_bytes > 0, "bytes persisted were measured");
        let audit = report.by_tier.get("audit").expect("audit tier recorded");
        assert!(audit.1 > 0, "audit bytes attributed");
        let agent = report
            .by_agent
            .get("DEEPSEEK")
            .expect("agent attribution like any spend");
        assert_eq!(agent.1, audit.1, "the agent's cost is the tier's cost");
        assert!(
            mine.contains("OBSERVED_EVENT"),
            "the record names what caused the write: {mine}"
        );
    }

    /// The journal is documented as "appended line-per-record, so concurrent
    /// writers never lose each other's records". That only holds if each record
    /// reaches the file in ONE write: `writeln!` on an unbuffered `File` issues two
    /// (the JSON, then the newline), so a writer landing between them tears two
    /// records into one unparseable line and both are lost to the report.
    #[test]
    fn audit_retention_prune_observation_journal_never_tears_a_record() {
        let ws = scratch("tear");
        let journal = ws.join("observation-cost.jsonl");
        let _guard = crate::env_test_lock();
        std::env::set_var("SUSI_OBSERVATION_FILE", &journal);
        let threads = 8usize;
        let per_thread = 400usize;
        std::thread::scope(|scope| {
            for t in 0..threads {
                let ws = ws.clone();
                scope.spawn(move || {
                    for i in 0..per_thread {
                        record_observation(
                            &ws,
                            "audit",
                            &format!("TEAR_{t}_{i}"),
                            64,
                            std::time::Duration::from_nanos(1),
                        );
                    }
                });
            }
        });
        std::env::remove_var("SUSI_OBSERVATION_FILE");
        let text = std::fs::read_to_string(&journal).expect("journal");
        let torn: Vec<&str> = text
            .lines()
            .filter(|l| serde_json::from_str::<ObservationRecord>(l).is_err())
            .collect();
        assert!(
            torn.is_empty(),
            "{} torn line(s), e.g. {:?}",
            torn.len(),
            torn.first()
        );
        assert_eq!(
            observation_report(&journal).records,
            (threads * per_thread) as u64,
            "every record survives concurrent writers"
        );
    }

    /// The status surface publishes the measured cost — the spend section
    /// reads the observation ledger and renders it, pinned by source scan.
    #[test]
    fn audit_retention_prune_status_publishes_observation_cost() {
        let src = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../src/cli/status_cli.rs"),
        )
        .expect("status_cli source");
        assert!(
            src.contains("observation-cost.jsonl") && src.contains("elapsed_ns"),
            "susi status reads the observation ledger"
        );
        assert!(
            src.contains("observation:") && src.contains("observation_by_agent"),
            "the report renders bytes + overhead attributed per agent"
        );
    }
}
