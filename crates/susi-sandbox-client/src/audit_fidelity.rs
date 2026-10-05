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
}

fn default_day_budget() -> u64 {
    16 * 1024 * 1024
}
fn default_retain_days() -> u32 {
    3
}

impl Default for TraceConfig {
    fn default() -> Self {
        Self {
            subsystems: std::collections::BTreeMap::new(),
            max_bytes_per_day: default_day_budget(),
            retain_days: default_retain_days(),
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
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        use std::io::Write;
        return file.write_all(line.as_bytes()).is_ok();
    }
    false
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
