//! Shared low-level helpers consolidated across `xtask` modules and binaries.

use sha2::{Digest, Sha256};
use std::error::Error;
use std::path::Path;
use std::process::Command;

/// Shared boxed error result alias used across `xtask` modules.
pub type BoxResult<T> = std::result::Result<T, Box<dyn Error>>;

/// Computes the lowercase hexadecimal SHA-256 digest of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Returns `true` when `value` is a full 40- or 64-character lowercase hex Git object ID.
pub fn is_valid_oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Returns `true` when `value` is a 40- or 64-character ASCII hex Git object ID (any case).
pub fn is_hex_oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Validates that `oid` is a full 40- or 64-character lowercase Git object ID.
pub fn require_valid_oid(oid: &str) -> std::result::Result<(), String> {
    if is_valid_oid(oid) {
        Ok(())
    } else {
        Err("expected a full lowercase Git object ID".to_owned())
    }
}

/// Strips ECMA-48 ANSI CSI (`\x1b[...@..=~`) escape sequences from `text`.
pub fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for next in chars.by_ref() {
                if ('@'..='~').contains(&next) {
                    break;
                }
            }
        } else {
            out.push(ch);
        }
    }
    out
}

/// Returns `true` when the first dot-separated component of `value` matches a
/// reserved Windows DOS device name (`CON`, `PRN`, `AUX`, `NUL`, `COM1`-`COM9`, `LPT1`-`LPT9`).
pub fn is_reserved_windows_device(value: &str) -> bool {
    let first = value.split('.').next().unwrap_or("").to_ascii_lowercase();
    matches!(first.as_str(), "con" | "prn" | "aux" | "nul")
        || (first.len() == 4
            && (first.starts_with("com") || first.starts_with("lpt"))
            && matches!(first.as_bytes()[3], b'1'..=b'9'))
}

/// Validates a 1-48 character kebab-case identifier (optionally permitting
/// interior single dots) while rejecting Windows reserved device names.
pub fn validate_kebab_name(
    name: &str,
    allow_dots: bool,
    label: &str,
) -> std::result::Result<(), String> {
    let bad_chars = !name.bytes().all(|byte| {
        byte.is_ascii_lowercase()
            || byte.is_ascii_digit()
            || byte == b'-'
            || (allow_dots && byte == b'.')
    });
    if name.is_empty()
        || name.len() > 48
        || is_reserved_windows_device(name)
        || bad_chars
        || name.starts_with(['-', '.'])
        || name.ends_with(['-', '.'])
        || (allow_dots && name.contains(".."))
    {
        let chars_desc = if allow_dots {
            "lowercase letters/digits/interior hyphens or dots"
        } else {
            "lowercase letters/digits/interior hyphens"
        };
        return Err(format!(
            "{label} name needs 1-48 {chars_desc}, not a Windows device name"
        ));
    }
    Ok(())
}

/// Runs `git` inside `dir` with literal pathspecs and replacement objects disabled,
/// returning ANSI-stripped, whitespace-trimmed UTF-8 stdout.
pub fn git_trimmed(dir: &Path, args: &[&str]) -> BoxResult<String> {
    let out = Command::new("git")
        .arg("--literal-pathspecs")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .current_dir(dir)
        .args(args)
        .output()
        .map_err(|error| format!("git {} in {}: {error}", args.join(" "), dir.display()))?;
    if !out.status.success() {
        return Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )
        .into());
    }
    Ok(strip_ansi(&String::from_utf8(out.stdout)?)
        .trim()
        .to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_helpers_enforce_contracts() {
        let zeros = "0".repeat(40);
        let upper = "A".repeat(40);
        assert!(is_valid_oid(&zeros));
        assert!(!is_valid_oid(&upper));
        assert!(is_hex_oid(&upper));
        assert!(require_valid_oid(&zeros).is_ok());
        assert!(require_valid_oid(&upper).is_err());

        assert_eq!(strip_ansi("\u{1b}[32mok\u{1b}[0m!"), "ok!");
        assert!(is_reserved_windows_device("COM1.txt"));
        assert!(!is_reserved_windows_device("common"));
        assert!(validate_kebab_name("2026-11-10-rc1", false, "snapshot").is_ok());
        assert!(validate_kebab_name("v1.2.3", true, "distillation").is_ok());
        assert!(validate_kebab_name("v1..2", true, "distillation").is_err());
        assert!(validate_kebab_name("aux", false, "snapshot").is_err());
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
