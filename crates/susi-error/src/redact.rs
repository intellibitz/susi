//! Pure, dependency-free secret-token redaction (Mandate 10: No Secret
//! Leaks). Callers own loading the configured patterns (from `SusiConfig`),
//! so this primitive depends on nothing — every crate `#[path]`-mounts this
//! file through `crates/susi-core/src/susi_error.rs`.

/// Replaces every occurrence of each pattern (plus trailing token-shaped
/// characters `[A-Za-z0-9_-]`) in `text` with `[REDACTED]`.
#[must_use]
pub fn redact_patterns(patterns: &[String], text: &str) -> String {
    let mut redacted = text.to_string();
    for pattern in patterns {
        if !pattern.is_empty() {
            redacted = redact_one(&redacted, pattern);
        }
    }
    redacted
}

fn redact_one(text: &str, pattern: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find(pattern) {
        out.push_str(&rest[..i]);
        let tail = &rest[i + pattern.len()..];
        let end = tail
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
            .unwrap_or(tail.len());
        out.push_str("[REDACTED]");
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}
