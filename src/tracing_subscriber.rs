//! Pure-std `tracing::Subscriber` replacing `tracing-subscriber` (and its
//! `regex-automata`, `regex-syntax`, `sharded-slab`, `nu-ansi-term`,
//! `matchers`, `thread_local`, `tracing-log`, `tracing-serde` transitive
//! closure) for the `susi` binary.
//!
//! Preserves:
//! - `RUST_LOG` directive parsing (`level` defaults and `target=level` longest
//!   prefix rules such as `"info,susi=debug,tantivy=warn"`).
//! - Structured single-line JSON event emission (`timestamp`, `level`,
//!   `fields`, `target`, and active `span`/`spans`) to the daily rolling
//!   `audit_writer` without touching `stdout`.
//! - Human-readable `stderr` diagnostic formatting (quiet `warn` default vs
//!   `RUST_LOG` / `SUSI_VERBOSE` target + thread-id + line-number detail).
#![forbid(unsafe_code)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Level, Metadata, Subscriber};

use crate::audit_writer::AuditWriterFactory;

std::thread_local! {
    static SPAN_STACK: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum FilterRank {
    Off = 0,
    Error = 1,
    Warn = 2,
    Info = 3,
    Debug = 4,
    Trace = 5,
}

impl FilterRank {
    fn from_level(level: Level) -> Self {
        if level == Level::ERROR {
            Self::Error
        } else if level == Level::WARN {
            Self::Warn
        } else if level == Level::INFO {
            Self::Info
        } else if level == Level::DEBUG {
            Self::Debug
        } else {
            Self::Trace
        }
    }

    fn parse(token: &str) -> Result<Self, ()> {
        let trimmed = token.trim();
        if trimmed.eq_ignore_ascii_case("off") || trimmed == "0" {
            Ok(Self::Off)
        } else if trimmed.eq_ignore_ascii_case("error") {
            Ok(Self::Error)
        } else if trimmed.eq_ignore_ascii_case("warn") || trimmed.eq_ignore_ascii_case("warning") {
            Ok(Self::Warn)
        } else if trimmed.eq_ignore_ascii_case("info") {
            Ok(Self::Info)
        } else if trimmed.eq_ignore_ascii_case("debug") {
            Ok(Self::Debug)
        } else if trimmed.eq_ignore_ascii_case("trace") {
            Ok(Self::Trace)
        } else {
            Err(())
        }
    }
}

#[derive(Clone, Debug)]
struct TargetDirective {
    prefix: String,
    rank: FilterRank,
}

#[derive(Clone, Debug)]
pub(crate) struct EnvFilter {
    default_rank: FilterRank,
    directives: Vec<TargetDirective>,
}

impl EnvFilter {
    pub(crate) fn new(spec: &str) -> Self {
        Self::parse(spec).unwrap_or(Self {
            default_rank: FilterRank::Info,
            directives: Vec::new(),
        })
    }

    pub(crate) fn try_from_default_env() -> Result<Self, ()> {
        let raw = std::env::var("RUST_LOG").map_err(|_| ())?;
        if raw.trim().is_empty() {
            return Err(());
        }
        Self::parse(&raw)
    }

    pub(crate) fn parse(spec: &str) -> Result<Self, ()> {
        let mut default_rank = FilterRank::Error;
        let mut has_default = false;
        let mut directives = Vec::new();

        for part in spec.split(',') {
            let item = part.trim();
            if item.is_empty() {
                continue;
            }
            if let Some((lhs, rhs)) = item.split_once('=') {
                let rank = FilterRank::parse(rhs)?;
                // Strip optional `[span{field=val}]` selector so target prefix
                // matching stays predictable on standard `RUST_LOG` specs.
                let prefix = lhs
                    .split_once('[')
                    .map_or(lhs, |(target_part, _)| target_part)
                    .trim();
                if prefix.is_empty() {
                    default_rank = rank;
                    has_default = true;
                } else {
                    directives.push(TargetDirective {
                        prefix: prefix.to_owned(),
                        rank,
                    });
                }
            } else {
                default_rank = FilterRank::parse(item)?;
                has_default = true;
            }
        }

        if !has_default && directives.is_empty() {
            return Err(());
        }
        if !has_default {
            default_rank = FilterRank::Error;
        }
        Ok(Self {
            default_rank,
            directives,
        })
    }

    pub(crate) fn enabled(&self, target: &str, level: Level) -> bool {
        let event_rank = FilterRank::from_level(level);
        let mut matched: Option<(usize, FilterRank)> = None;
        for d in &self.directives {
            if target.starts_with(&d.prefix) {
                let len = d.prefix.len();
                if matched.is_none_or(|(best_len, _)| len >= best_len) {
                    matched = Some((len, d.rank));
                }
            }
        }
        let max_rank = matched.map_or(self.default_rank, |(_, rank)| rank);
        event_rank <= max_rank
    }
}

#[derive(Clone, Debug)]
struct SpanEntry {
    name: &'static str,
    parent: Option<u64>,
    fields: Map<String, Value>,
    refs: usize,
}

struct JsonVisitor<'a> {
    fields: &'a mut Map<String, Value>,
}

impl Visit for JsonVisitor<'_> {
    fn record_f64(&mut self, field: &Field, value: f64) {
        let v = serde_json::Number::from_f64(value)
            .map_or_else(|| Value::String(value.to_string()), Value::Number);
        self.fields.insert(field.name().to_owned(), v);
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.fields
            .insert(field.name().to_owned(), Value::Number(value.into()));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.fields
            .insert(field.name().to_owned(), Value::Number(value.into()));
    }

    fn record_i128(&mut self, field: &Field, value: i128) {
        let v = i64::try_from(value)
            .map(|n| Value::Number(n.into()))
            .unwrap_or_else(|_| Value::String(value.to_string()));
        self.fields.insert(field.name().to_owned(), v);
    }

    fn record_u128(&mut self, field: &Field, value: u128) {
        let v = u64::try_from(value)
            .map(|n| Value::Number(n.into()))
            .unwrap_or_else(|_| Value::String(value.to_string()));
        self.fields.insert(field.name().to_owned(), v);
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.fields
            .insert(field.name().to_owned(), Value::Bool(value));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.fields
            .insert(field.name().to_owned(), Value::String(value.to_owned()));
    }

    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        self.fields
            .insert(field.name().to_owned(), Value::String(value.to_string()));
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.fields
            .insert(field.name().to_owned(), Value::String(format!("{value:?}")));
    }
}

/// Convert Unix epoch duration to ISO-8601 UTC (`YYYY-MM-DDTHH:MM:SS.uuuuuuZ`)
/// using Howard Hinnant's civil-from-days algorithm.
pub(crate) fn format_utc_rfc3339_micros(now: SystemTime) -> String {
    let dur = now.duration_since(UNIX_EPOCH).unwrap_or_default();
    let total_secs = dur.as_secs();
    let micros = dur.subsec_micros();

    let days = (total_secs / 86_400) as i64;
    let rem_secs = total_secs % 86_400;
    let hour = rem_secs / 3_600;
    let minute = (rem_secs % 3_600) / 60;
    let second = rem_secs % 60;

    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    if month <= 2 {
        year += 1;
    }

    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{micros:06}Z")
}

pub(crate) struct SusiSubscriber {
    audit_filter: EnvFilter,
    console_filter: EnvFilter,
    verbose_console: bool,
    audit_writer: AuditWriterFactory,
    next_span_id: AtomicU64,
    spans: Mutex<HashMap<u64, SpanEntry>>,
}

impl SusiSubscriber {
    pub(crate) fn new(
        audit_filter: EnvFilter,
        console_filter: EnvFilter,
        verbose_console: bool,
        audit_writer: AuditWriterFactory,
    ) -> Self {
        Self {
            audit_filter,
            console_filter,
            verbose_console,
            audit_writer,
            next_span_id: AtomicU64::new(1),
            spans: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn install(self) {
        let _ = tracing::subscriber::set_global_default(self);
    }

    fn current_span_chain(&self) -> Vec<Value> {
        let top = SPAN_STACK.with(|stack| stack.borrow().last().copied());
        let Some(mut cur) = top else {
            return Vec::new();
        };
        let guard = match self.spans.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        let mut chain = Vec::new();
        let mut depth = 0usize;
        while let Some(entry) = guard.get(&cur) {
            let mut obj = entry.fields.clone();
            obj.insert(String::from("name"), Value::String(entry.name.to_owned()));
            chain.push(Value::Object(obj));
            depth += 1;
            if depth >= 32 {
                break;
            }
            match entry.parent {
                Some(p) if p != cur => cur = p,
                _ => break,
            }
        }
        chain.reverse();
        chain
    }
}

impl Subscriber for SusiSubscriber {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        self.audit_filter
            .enabled(metadata.target(), *metadata.level())
            || self
                .console_filter
                .enabled(metadata.target(), *metadata.level())
    }

    fn new_span(&self, span: &Attributes<'_>) -> Id {
        let raw_id = self.next_span_id.fetch_add(1, Ordering::Relaxed);
        let parent = if span.is_root() {
            None
        } else if let Some(explicit) = span.parent() {
            Some(explicit.into_u64())
        } else {
            SPAN_STACK.with(|stack| stack.borrow().last().copied())
        };
        let mut fields = Map::new();
        span.values().record(&mut JsonVisitor {
            fields: &mut fields,
        });
        let mut guard = match self.spans.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        // Bound active map growth defensively under pathological span leaks.
        if guard.len() < 16_384 {
            guard.insert(
                raw_id,
                SpanEntry {
                    name: span.metadata().name(),
                    parent,
                    fields,
                    refs: 1,
                },
            );
        }
        Id::from_u64(raw_id)
    }

    fn record(&self, span: &Id, values: &Record<'_>) {
        let mut guard = match self.spans.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(entry) = guard.get_mut(&span.into_u64()) {
            values.record(&mut JsonVisitor {
                fields: &mut entry.fields,
            });
        }
    }

    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let meta = event.metadata();
        let write_audit = self.audit_filter.enabled(meta.target(), *meta.level());
        let write_console = self.console_filter.enabled(meta.target(), *meta.level());
        if !write_audit && !write_console {
            return;
        }

        let timestamp = format_utc_rfc3339_micros(SystemTime::now());
        let mut fields = Map::new();
        event.record(&mut JsonVisitor {
            fields: &mut fields,
        });

        if write_audit {
            let mut root = Map::new();
            root.insert(String::from("timestamp"), Value::String(timestamp.clone()));
            root.insert(
                String::from("level"),
                Value::String(meta.level().as_str().to_owned()),
            );
            root.insert(String::from("fields"), Value::Object(fields.clone()));
            root.insert(
                String::from("target"),
                Value::String(meta.target().to_owned()),
            );
            let spans = self.current_span_chain();
            if let Some(current) = spans.last().cloned() {
                root.insert(String::from("span"), current);
                root.insert(String::from("spans"), Value::Array(spans));
            }
            if let Ok(mut encoded) = serde_json::to_vec(&Value::Object(root)) {
                encoded.push(b'\n');
                let _ = self.audit_writer.writer().write_all(&encoded);
            }
        }

        if write_console {
            let mut line = String::with_capacity(160);
            line.push_str(&timestamp);
            line.push(' ');
            line.push_str(meta.level().as_str());
            if self.verbose_console {
                line.push(' ');
                line.push_str(&format!("{:?}", std::thread::current().id()));
                line.push(' ');
                line.push_str(meta.target());
                if let Some(line_no) = meta.line() {
                    line.push(':');
                    line.push_str(&line_no.to_string());
                }
            }
            line.push_str(": ");
            if let Some(msg) = fields.get("message") {
                if let Some(s) = msg.as_str() {
                    line.push_str(s);
                } else {
                    line.push_str(&msg.to_string());
                }
            }
            for (k, v) in &fields {
                if k == "message" {
                    continue;
                }
                line.push(' ');
                line.push_str(k);
                line.push('=');
                if let Some(s) = v.as_str() {
                    line.push_str(s);
                } else {
                    line.push_str(&v.to_string());
                }
            }
            line.push('\n');
            let _ = std::io::stderr().write_all(line.as_bytes());
        }
    }

    fn enter(&self, span: &Id) {
        let id = span.into_u64();
        SPAN_STACK.with(|stack| stack.borrow_mut().push(id));
    }

    fn exit(&self, span: &Id) {
        let id = span.into_u64();
        SPAN_STACK.with(|stack| {
            let mut s = stack.borrow_mut();
            if let Some(pos) = s.iter().rposition(|&x| x == id) {
                s.remove(pos);
            }
        });
    }

    fn clone_span(&self, id: &Id) -> Id {
        let raw = id.into_u64();
        let mut guard = match self.spans.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(entry) = guard.get_mut(&raw) {
            entry.refs = entry.refs.saturating_add(1);
        }
        Id::from_u64(raw)
    }

    fn try_close(&self, id: Id) -> bool {
        let raw = id.into_u64();
        let mut guard = match self.spans.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        let remove = match guard.get_mut(&raw) {
            Some(entry) => {
                entry.refs = entry.refs.saturating_sub(1);
                entry.refs == 0
            }
            None => false,
        };
        if remove {
            guard.remove(&raw);
        }
        remove
    }
}

#[cfg(test)]
mod tests {
    use super::{format_utc_rfc3339_micros, EnvFilter, SusiSubscriber};
    use crate::audit_writer::spawn_daily_audit_writer;
    use std::time::{Duration, UNIX_EPOCH};
    use tracing::Level;

    #[test]
    fn env_filter_honors_default_and_longest_target_prefix() {
        let filter = EnvFilter::new("info,susi=debug,susi_gmcp::noisy=warn,tantivy=warn");
        assert!(filter.enabled("other_crate::mod", Level::INFO));
        assert!(!filter.enabled("other_crate::mod", Level::DEBUG));
        assert!(filter.enabled("susi_core::truth", Level::DEBUG));
        assert!(!filter.enabled("susi_core::truth", Level::TRACE));
        assert!(!filter.enabled("susi_gmcp::noisy::inner", Level::INFO));
        assert!(filter.enabled("susi_gmcp::noisy::inner", Level::WARN));
        assert!(!filter.enabled("tantivy::indexer", Level::INFO));
        assert!(filter.enabled("tantivy::indexer", Level::ERROR));
    }

    #[test]
    fn rfc3339_timestamp_formats_epoch_and_microseconds() {
        let ts =
            format_utc_rfc3339_micros(UNIX_EPOCH + Duration::from_micros(1_700_000_000_123_456));
        assert_eq!(ts, "2023-11-14T22:13:20.123456Z");
    }

    #[test]
    fn subscriber_writes_structured_json_with_spans_to_audit_writer() {
        let dir =
            std::env::temp_dir().join(format!("susi-std-tracing-sub-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (handle, guard) = spawn_daily_audit_writer(&dir, "audit", "log", 7);
        let sub = SusiSubscriber::new(
            EnvFilter::new("info,susi=debug,tantivy=warn"),
            EnvFilter::new("off"),
            false,
            handle,
        );

        tracing::subscriber::with_default(sub, || {
            let span = tracing::info_span!("request_gate", req_id = 77u64);
            let _entered = span.enter();
            tracing::info!(status = 200u64, "handled request");
            tracing::info!(target: "tantivy::indexer", "segment merged");
        });
        drop(guard);

        let mut log_body = String::new();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                if let Ok(text) = std::fs::read_to_string(entry.path()) {
                    log_body.push_str(&text);
                }
            }
        }
        let _ = std::fs::remove_dir_all(&dir);

        let lines: Vec<&str> = log_body.lines().collect();
        assert_eq!(lines.len(), 1);
        let parsed: serde_json::Value =
            serde_json::from_str(lines[0]).unwrap_or(serde_json::Value::Null);
        assert_eq!(parsed["level"], "INFO");
        assert_eq!(parsed["fields"]["message"], "handled request");
        assert_eq!(parsed["fields"]["status"], 200);
        assert_eq!(parsed["span"]["name"], "request_gate");
        assert_eq!(parsed["span"]["req_id"], 77);
    }
}
