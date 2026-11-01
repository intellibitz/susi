//! Fail-closed recognition of an exact SUSI release-version-only Git diff.
use std::collections::BTreeSet;
use std::io::Write;
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
    let mut child = Command::new("jq")
        .args(["-er", ".version | strings"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    child.stdin.take()?.write_all(content.as_bytes()).ok()?;
    let out = child.wait_with_output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8(out.stdout).ok())
        .flatten()
        .map(|s| s.trim().to_owned())
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
