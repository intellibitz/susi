//! Pure, dependency-free secret-token redaction (Mandate 10: No Secret
//! Leaks). Callers own loading the configured patterns (from `SusiConfig`),
//! so this primitive depends on nothing.

/// Replaces every occurrence of each pattern (plus trailing token-shaped
/// characters `[A-Za-z0-9_-]`) in `text` with `[REDACTED]`. An occurrence
/// counts only where a token can start (see [`secret_match_starts`]).
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

/// Byte offsets where `pattern` begins a token in `text`: the preceding
/// character (if any) is not ASCII alphanumeric. Shared by the secret
/// redactor/veto and the destructive/exfiltration command patterns. A bare substring match
/// flagged ordinary words — `risk-`, `task-`, `disk-` all contain `sk-` —
/// so the governance veto refused goals like "write a risk-assessment" and
/// the redactor rewrote "task-queue" as "ta[REDACTED]".
pub fn secret_match_starts<'a>(
    text: &'a str,
    pattern: &'a str,
) -> impl Iterator<Item = usize> + 'a {
    text.match_indices(pattern).filter_map(move |(i, _)| {
        let starts_token = text[..i]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_ascii_alphanumeric());
        starts_token.then_some(i)
    })
}

/// Whether any configured pattern begins a token in `text`.
#[must_use]
pub fn contains_secret_pattern(patterns: &[String], text: &str) -> Option<String> {
    patterns
        .iter()
        .filter(|p| !p.is_empty())
        .find(|p| secret_match_starts(text, p).next().is_some())
        .cloned()
}

fn redact_one(text: &str, pattern: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    for start in secret_match_starts(text, pattern).collect::<Vec<_>>() {
        if start < copied {
            continue; // inside a token already redacted
        }
        out.push_str(&text[copied..start]);
        let tail = &text[start + pattern.len()..];
        let end = tail
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
            .unwrap_or(tail.len());
        out.push_str("[REDACTED]");
        copied = start + pattern.len() + end;
    }
    out.push_str(&text[copied..]);
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
