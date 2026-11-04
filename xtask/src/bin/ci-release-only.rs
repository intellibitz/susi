//! Fail-closed recognition of an exact SUSI release-version-only Git diff.
#![forbid(unsafe_code)]

#[cfg(windows)]
extern crate susi_windows_runtime;

use std::collections::BTreeSet;
use std::process::{Command, Stdio};

const MANIFESTS: [(&str, &str); 7] = [
    ("Cargo.toml", "susi"),
    ("crates/susi-daemon/Cargo.toml", "susi-daemon"),
    ("crates/susi-gawd/Cargo.toml", "susi-gawd"),
    ("crates/susi-gemi/Cargo.toml", "susi-gemi"),
    ("crates/susi-gmcp/Cargo.toml", "susi-gmcp"),
    ("crates/susi-harness/Cargo.toml", "susi-harness"),
    ("crates/susi-server/Cargo.toml", "susi-server"),
];
const META: [&str; 3] = [
    ".agents/evidence.json",
    ".agents/identity.json",
    ".agents/roadmap.json",
];

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .args(args)
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8(out.stdout).ok())
        .flatten()
}
fn blob(rev: &str, path: &str) -> Option<String> {
    git(&["show", &format!("{rev}:{path}")])
}
fn field<'a>(section: &'a str, key: &str) -> Option<&'a str> {
    let mut values = section.lines().filter_map(|line| {
        let (name, value) = line.trim().split_once('=')?;
        (name.trim() == key).then(|| value.trim().strip_prefix('"')?.strip_suffix('"'))?
    });
    let first = values.next()?;
    values.next().is_none().then_some(first)
}
fn package(content: &str) -> Option<&str> {
    content.split("[package]").nth(1)?.split("\n[").next()
}
fn version(value: &str) -> Option<[u64; 3]> {
    let parts: Vec<_> = value.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let mut out = [0; 3];
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty()
            || (part.len() > 1 && part.starts_with('0'))
            || !part.bytes().all(|b| b.is_ascii_digit())
        {
            return None;
        }
        out[i] = part.parse().ok()?;
    }
    Some(out)
}
fn json_version(content: &str) -> Option<String> {
    let doc: serde_json::Value = serde_json::from_str(content).ok()?;
    let ver = doc.as_object()?.get("version")?.as_str()?.trim();
    (!ver.is_empty()).then(|| ver.to_owned())
}
fn lock_has_packages(lock: &str, expected_version: &str) -> bool {
    MANIFESTS.iter().all(|(_, name)| {
        lock.split("[[package]]")
            .skip(1)
            .filter(|section| {
                field(section, "name") == Some(*name)
                    && field(section, "version") == Some(expected_version)
                    && !section
                        .lines()
                        .any(|l| l.trim_start().starts_with("source ="))
            })
            .count()
            == 1
    })
}
fn check() -> Option<()> {
    let base_ref = std::env::var("BASE_REF").unwrap_or_else(|_| "origin/main".into());
    let base = git(&["merge-base", &base_ref, "HEAD"])?;
    let base = base.trim();
    let mut paths: BTreeSet<&str> = MANIFESTS.iter().map(|(p, _)| *p).collect();
    paths.extend(META);
    paths.extend(["Cargo.lock", "README.md"]);
    let diff = git(&["diff", "--no-renames", "--name-only", "-z", base, "HEAD"])?;
    if diff
        .split('\0')
        .filter(|p| !p.is_empty())
        .collect::<BTreeSet<_>>()
        != paths
    {
        return None;
    }
    let before_root = blob(base, "Cargo.toml")?;
    let after_root = blob("HEAD", "Cargo.toml")?;
    let old = field(package(&before_root)?, "version")?;
    let new = field(package(&after_root)?, "version")?;
    if version(new)? <= version(old)? {
        return None;
    }
    for path in &paths {
        for rev in [base, "HEAD"] {
            let tree = git(&["ls-tree", rev, "--", path])?;
            if tree.split_whitespace().next()? != "100644" {
                return None;
            }
        }
        let before = blob(base, path)?;
        let after = blob("HEAD", path)?;
        let count = if *path == "Cargo.lock" {
            MANIFESTS.len()
        } else {
            1
        };
        if before.matches(old).count() != count || before.replace(old, new) != after {
            return None;
        }
        if let Some((_, name)) = MANIFESTS.iter().find(|(p, _)| p == path) {
            for (content, ver) in [(&before, old), (&after, new)] {
                let pkg = package(content)?;
                if field(pkg, "name") != Some(*name) || field(pkg, "version") != Some(ver) {
                    return None;
                }
            }
        }
        if META.contains(path)
            && (json_version(&before).as_deref() != Some(old)
                || json_version(&after).as_deref() != Some(new))
        {
            return None;
        }
        if *path == "Cargo.lock"
            && (!lock_has_packages(&before, old) || !lock_has_packages(&after, new))
        {
            return None;
        }
    }
    Some(())
}
fn main() {
    if check().is_none() {
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::{field, json_version, lock_has_packages, package, version, MANIFESTS};

    #[test]
    fn version_parses_strict_semver_triples_and_rejects_leading_zeros_or_prereleases() {
        assert_eq!(version("0.21.47"), Some([0, 21, 47]));
        assert_eq!(version("1.0.0"), Some([1, 0, 0]));
        for bad in [
            "",
            "01.2.3",
            "1.02.3",
            "1.2.03",
            "1.2",
            "1.2.3.4",
            "1.2.3-rc1",
            "v1.2.3",
            "1.2.x",
        ] {
            assert_eq!(version(bad), None, "expected {bad:?} to be rejected");
        }
    }

    #[test]
    fn field_and_package_extract_unique_toml_keys_and_reject_duplicates() {
        let toml = "[workspace]\nmembers = []\n\n[package]\nname = \"susi\"\nversion = \"0.21.47\"\n\n[dependencies]\nversion = \"9.9.9\"\n";
        let pkg = package(toml);
        assert!(pkg.is_some());
        let pkg_section = pkg.unwrap_or("");
        assert_eq!(field(pkg_section, "name"), Some("susi"));
        assert_eq!(field(pkg_section, "version"), Some("0.21.47"));
        assert_eq!(field(pkg_section, "missing"), None);

        let dup = "name = \"susi\"\nversion = \"0.21.47\"\nversion = \"0.21.48\"\n";
        assert_eq!(field(dup, "version"), None);
    }

    #[test]
    fn json_version_extracts_string_version_without_external_jq_and_rejects_non_string_or_invalid_json(
    ) {
        assert_eq!(
            json_version(r#"{"version":"1.2.3","meaning":"preserve"}"#),
            Some("1.2.3".to_string())
        );
        for bad in [
            "",
            "not json",
            r#"{"version":123}"#,
            r#"{"version":null}"#,
            r#"{"version":""}"#,
            r#"{"version":["1.2.3"]}"#,
            r#"["1.2.3"]"#,
            r#"{}"#,
        ] {
            assert_eq!(json_version(bad), None, "expected {bad:?} to be rejected");
        }
    }

    #[test]
    fn lock_has_packages_requires_exact_seven_workspace_crates_without_external_source() {
        let mut valid_lock = String::new();
        for (_, name) in MANIFESTS {
            valid_lock.push_str(&format!(
                "[[package]]\nname = \"{name}\"\nversion = \"0.21.47\"\n\n"
            ));
        }
        assert!(lock_has_packages(&valid_lock, "0.21.47"));
        assert!(!lock_has_packages(&valid_lock, "0.21.46"));

        let with_source = format!(
            "{valid_lock}[[package]]\nname = \"susi\"\nversion = \"0.21.47\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n"
        );
        let replaced_source = valid_lock.replacen(
            "name = \"susi\"\nversion = \"0.21.47\"",
            "name = \"susi\"\nversion = \"0.21.47\"\nsource = \"registry+https://example.com\"",
            1,
        );
        assert!(!lock_has_packages(&replaced_source, "0.21.47"));
        assert!(lock_has_packages(&with_source, "0.21.47"));

        let duplicate_local =
            format!("{valid_lock}[[package]]\nname = \"susi\"\nversion = \"0.21.47\"\n");
        assert!(!lock_has_packages(&duplicate_local, "0.21.47"));
    }
}
