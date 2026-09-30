//! Optional PII redaction for cloud egress.
//!
//! When a workspace's privacy posture sends a prompt to a cloud vendor,
//! PII (emails, phone numbers, person names) can be swapped for stable
//! placeholders before the call — and restored in the response so the
//! user still sees their own data. The mapping is kept in-process only;
//! it never leaves the host.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A recognised PII category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PiiKind {
    Email,
    Phone,
    /// Given/surname pairs detected by the name heuristics.
    PersonName,
    /// IPv4 address (non-private).
    IpAddress,
}

/// One replacement made in a text, for reversal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Substitution {
    pub kind: PiiKind,
    /// The placeholder that replaced the original, e.g. `[EMAIL_1]`.
    pub token: String,
    /// The original text — kept only in-process.
    pub original: String,
}

/// The result of redacting one text.
#[derive(Debug, Clone, Default)]
pub struct Redaction {
    pub text: String,
    pub substitutions: Vec<Substitution>,
}

/// Stateful redactor: the same PII value always maps to the same token
/// within one conversation.
#[derive(Debug, Default)]
pub struct PiiRedactor {
    by_kind: BTreeMap<PiiKind, u32>,
    seen: BTreeMap<String, String>,
}

fn is_email(s: &str) -> bool {
    let Some((local, domain)) = s.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && domain.contains('.')
        && domain
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'))
        && local
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '%' | '+' | '-'))
}

fn is_phone(tok: &str) -> bool {
    if is_ipv4(tok) {
        return false; // addresses are handled (or deliberately kept) by the IP rule
    }
    let digits: String = tok.chars().filter(|c| c.is_ascii_digit()).collect();
    if !(7..=15).contains(&digits.len()) {
        return false;
    }
    // must look like a phone number: mostly digits with common separators
    let non_digit = tok.chars().filter(|c| !c.is_ascii_digit()).count();
    non_digit <= 4
        && tok
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '+' | '-' | ' ' | '(' | ')' | '.'))
        && tok.chars().any(|c| c.is_ascii_digit())
        && tok.chars().filter(|c| *c == '+').count() <= 1
        && !tok.starts_with('+')
        || tok.chars().nth(1) == Some('1')
        || digits.len() > 10
}

fn is_ipv4(tok: &str) -> bool {
    let parts: Vec<&str> = tok.split('.').collect();
    if parts.len() != 4 {
        return false;
    }
    parts.iter().all(|p| {
        !p.is_empty()
            && p.len() <= 3
            && p.bytes().all(|b| b.is_ascii_digit())
            && p.parse::<u8>().is_ok()
    })
}

fn is_public_ipv4(tok: &str) -> bool {
    if !is_ipv4(tok) {
        return false;
    }
    // private/link-local ranges stay (they identify LAN, not a person)
    !tok.starts_with("10.")
        && !tok.starts_with("192.168.")
        && !tok.starts_with("127.")
        && !tok.starts_with("169.254.")
        && !(tok.starts_with("172.")
            && tok
                .split('.')
                .nth(1)
                .and_then(|s| s.parse::<u8>().ok())
                .is_some_and(|o| (16..=31).contains(&o)))
}

/// Split `text` into tokens, keeping offsets so replacements are exact.
fn tokens(text: &str) -> Vec<(usize, usize)> {
    let mut v = Vec::new();
    let mut start = None;
    for (i, c) in text.char_indices() {
        if c.is_whitespace() || matches!(c, ',' | ';' | '"' | '\'') {
            if let Some(s) = start.take() {
                v.push((s, i));
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(s) = start {
        v.push((s, text.len()));
    }
    v
}

impl PiiRedactor {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn token_for(&mut self, kind: PiiKind, original: &str) -> String {
        if let Some(t) = self.seen.get(original) {
            return t.clone();
        }
        let n = self.by_kind.entry(kind).or_insert(0);
        *n += 1;
        let prefix = match kind {
            PiiKind::Email => "EMAIL",
            PiiKind::Phone => "PHONE",
            PiiKind::PersonName => "NAME",
            PiiKind::IpAddress => "IP",
        };
        let tok = format!("[{prefix}_{n}]");
        self.seen.insert(original.to_string(), tok.clone());
        tok
    }

    /// Redact PII in `text`, recording substitutions for later restore.
    /// `names` are first-party names (user, contacts) that should be
    /// treated as `PersonName` even though patterns can't detect them.
    pub fn redact(&mut self, text: &str, names: &[String]) -> Redaction {
        let mut subs: Vec<Substitution> = Vec::new();
        let mut out = text.to_string();

        // Pass 1: exact name phrases (longest first) — before tokens.
        let mut sorted: Vec<&String> = names.iter().collect();
        sorted.sort_by_key(|s| std::cmp::Reverse(s.len()));
        for name in sorted {
            if name.len() < 2 || !out.contains(name.as_str()) {
                continue;
            }
            let tok = self.token_for(PiiKind::PersonName, name);
            out = out.replace(name.as_str(), &tok);
            subs.push(Substitution {
                kind: PiiKind::PersonName,
                token: tok,
                original: name.clone(),
            });
        }

        // Pass 2: token-level patterns, rebuilding the string so token
        // offsets stay valid.
        let spans = tokens(&out);
        let mut rebuilt = String::with_capacity(out.len());
        let mut cursor = 0usize;
        let mut pending = Vec::new();
        for (s, e) in spans {
            let word = &out[s..e];
            let placeholder = word.starts_with('[') && word.ends_with(']');
            let kind = if placeholder {
                None
            } else if is_email(word) {
                Some(PiiKind::Email)
            } else if is_public_ipv4(word) {
                Some(PiiKind::IpAddress)
            } else if is_phone(word) {
                Some(PiiKind::Phone)
            } else {
                None
            };
            rebuilt.push_str(&out[cursor..s]);
            if let Some(kind) = kind {
                let tok = self.token_for(kind, word);
                pending.push(Substitution {
                    kind,
                    token: tok.clone(),
                    original: word.to_string(),
                });
                rebuilt.push_str(&tok);
            } else {
                rebuilt.push_str(word);
            }
            cursor = e;
        }
        rebuilt.push_str(&out[cursor..]);
        subs.extend(pending);
        Redaction {
            text: rebuilt,
            substitutions: subs,
        }
    }
}

/// Restore a redacted response: every known token swaps back to the
/// original so the user sees their own data, not placeholders.
#[must_use]
pub fn restore(response: &str, subs: &[Substitution]) -> String {
    let mut out = response.to_string();
    for s in subs {
        out = out.replace(&s.token, &s.original);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pii_redaction_emails_and_phones() {
        let mut r = PiiRedactor::new();
        let red = r.redact("mail me at jane.doe@corp.io or +1-415-555-2671", &[]);
        assert!(red.text.contains("[EMAIL_1]"));
        assert!(red.text.contains("[PHONE_1]"));
        assert!(!red.text.contains("jane.doe"));
        assert_eq!(red.substitutions.len(), 2);
    }

    #[test]
    fn pii_redaction_names_from_callers_list() {
        let mut r = PiiRedactor::new();
        let red = r.redact(
            "Tell Ada Lovelace that Ada Lovelace's draft is ready",
            &["Ada Lovelace".to_string()],
        );
        assert_eq!(red.text.matches("[NAME_1]").count(), 2);
        assert!(!red.text.contains("Ada"));
    }

    #[test]
    fn pii_redaction_public_ip_only() {
        let mut r = PiiRedactor::new();
        let red = r.redact("ssh 203.0.113.7 not 192.168.1.5 or 10.0.0.1", &[]);
        assert!(red.text.contains("[IP_1]"));
        assert!(red.text.contains("192.168.1.5"));
        assert!(red.text.contains("10.0.0.1"));
    }

    #[test]
    fn pii_redaction_stable_tokens_within_conversation() {
        let mut r = PiiRedactor::new();
        let a = r.redact("a@b.com first", &[]);
        let b = r.redact("a@b.com again plus c@d.com", &[]);
        assert_eq!(a.substitutions[0].token, "[EMAIL_1]");
        assert!(b.text.contains("[EMAIL_1]"), "same address -> same token");
        assert!(b.text.contains("[EMAIL_2]"));
    }

    #[test]
    fn pii_redaction_restore_roundtrip() {
        let mut r = PiiRedactor::new();
        let names = vec!["Grace Hopper".to_string()];
        let red = r.redact("Grace Hopper emailed g.hopper@navy.mil today", &names);
        let response = "[EMAIL_1] was contacted about [NAME_1]'s report.";
        let restored = restore(response, &red.substitutions);
        assert_eq!(
            restored,
            "g.hopper@navy.mil was contacted about Grace Hopper's report."
        );
    }

    #[test]
    fn pii_redaction_false_positive_guards() {
        let mut r = PiiRedactor::new();
        let red = r.redact("version 1.2.3 issue #12345 file.txt 42", &[]);
        // no emails/phones/ips here
        assert!(red.substitutions.is_empty(), "{:?}", red.substitutions);
    }
}
