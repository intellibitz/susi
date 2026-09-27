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

/// Masks the value of every environment variable named `*_API_KEY`,
/// `*_TOKEN`, or `*_SECRET` (8+ chars) — the credentials this process
/// actually holds, which is what an error message most often echoes back.
#[must_use]
pub fn mask_env_credentials(text: &str) -> String {
    mask_credentials_from(text, std::env::vars())
}

/// `mask_env_credentials` over an explicit variable list (testable without
/// mutating the process environment).
#[must_use]
pub fn mask_credentials_from(
    text: &str,
    vars: impl IntoIterator<Item = (String, String)>,
) -> String {
    let mut result = text.to_owned();
    for (key, value) in vars {
        if value.len() >= 8
            && (key.ends_with("_API_KEY") || key.ends_with("_TOKEN") || key.ends_with("_SECRET"))
        {
            result = result.replace(&value, "[REDACTED]");
        }
    }
    result
}
