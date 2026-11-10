#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Orphan-source ratchet: every Rust file under `crates/*/src`, `tests`,
//! `benches` and `examples` must be reached by a build or test target.
//!
//! A file nothing compiles is bloat the workspace tests cannot see. On
//! 2026-11-07 the "migrate unreferenced module" iterations left one such
//! file in core (`susi-gawd-a2a/src/mastery_tests/mod.rs`, after its suite
//! moved out) and 228 of them in `susi-commercial-apps`. Core's ceiling is
//! zero: a migration that leaves a file behind, or a copy nothing declares,
//! fails this test at once.
//!
//! Reachability follows the compiler's rules closely enough for this tree:
//! crate roots (`src/lib.rs`, `src/main.rs`, `src/bin/*`, `tests/*`,
//! `benches/*`, `examples/*`, manifest `path =` targets), `mod x;` resolved
//! against the declaring file's module directory (including inline
//! `mod y { mod x; }` blocks), `#[path = ".."]` and
//! `#[cfg_attr(.., path = "..")]`, and `include!`-style macros. Files under
//! a `fixtures/` directory are inputs tests compile or read themselves and
//! are not counted.
use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Unreached files across the workspace: none. Never raise it.
const ORPHAN_CEILING: usize = 0;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

fn rust_files(dir: &Path, out: &mut BTreeSet<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if entry.file_type().is_ok_and(|ft| ft.is_dir()) {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.insert(path);
        }
    }
}

fn is_fixture(path: &Path) -> bool {
    path.components().any(|c| c.as_os_str() == "fixtures")
}

/// Strip `//` comments so commented-out declarations do not count.
fn without_line_comments(source: &str) -> Cow<'_, str> {
    if !source.contains("//") {
        return Cow::Borrowed(source);
    }
    let mut out = String::with_capacity(source.len());
    for (idx, line) in source.lines().enumerate() {
        if idx > 0 {
            out.push('\n');
        }
        let kept = match line.find("//") {
            Some(at) if !line[..at].contains('"') => &line[..at],
            Some(_) | None => line,
        };
        out.push_str(kept);
    }
    Cow::Owned(out)
}

fn ident_at(text: &str, from: usize) -> Option<(&str, usize)> {
    let rest = &text[from..];
    let trimmed = rest.trim_start();
    let skipped = rest.len() - trimmed.len();
    let end = trimmed
        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .unwrap_or(trimmed.len());
    if end == 0 {
        return None;
    }
    Some((&trimmed[..end], from + skipped + end))
}

/// `(module name, explicit path if any, inline block or not)` for every
/// `mod` item in `source`, with the inline module nesting each one sits in.
#[derive(Clone)]
struct ModDecl {
    name: String,
    explicit_path: Option<String>,
    /// The explicit path came from `#[cfg_attr(.., path = ..)]`: the default
    /// resolution is live under the other configuration, so both count.
    cfg_attr: bool,
    inline: bool,
    nesting: Vec<String>,
}

fn mod_decls(source: &str) -> Vec<ModDecl> {
    if !source.contains("mod ") {
        return Vec::new();
    }
    let text = without_line_comments(source);
    let mut decls = Vec::new();
    let mut pending_path: Option<String> = None;
    let mut pending_cfg_attr = false;
    let mut nesting: Vec<String> = Vec::new();
    // Brace depth at which each inline module opened, to pop it on close.
    let mut module_depths: Vec<usize> = Vec::new();
    let mut depth = 0usize;
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if matches!(b, b'/' | b'"' | b'\'' | b'r' | b'b') && text.is_char_boundary(i) {
            if let Some(next) = skip_literal_or_comment(&text, i) {
                i = next;
                continue;
            }
        }
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth = depth.saturating_sub(1);
                if module_depths.last().is_some_and(|d| *d == depth) {
                    module_depths.pop();
                    nesting.pop();
                }
            }
            b'#' => {
                let rest = &text[i..];
                // A `path = "..."` key: whole word (`"service-paths"` inside a
                // cfg list is not one) followed by `=`.
                let attr_end = rest.find(']').unwrap_or(rest.len());
                let key = rest[..attr_end].match_indices("path").find(|(at, _)| {
                    let before = rest[..*at].chars().next_back();
                    let after = rest[at + 4..attr_end].trim_start();
                    before.is_none_or(|c| matches!(c, '(' | ',' | '[') || c.is_whitespace())
                        && after.starts_with('=')
                });
                if let Some((at, _)) = key {
                    let after = &rest[at + 4..attr_end];
                    if let Some(eq) = after.find('=') {
                        let value = after[eq + 1..].trim();
                        if let Some(stripped) = value.strip_prefix('"') {
                            if let Some(end) = stripped.find('"') {
                                pending_path = Some(stripped[..end].to_owned());
                                pending_cfg_attr = rest[..at].contains("cfg_attr");
                            }
                        }
                    }
                }
            }
            b'm' if bytes.get(i..i + 4) == Some(b"mod ")
                && (i == 0 || !bytes[i - 1].is_ascii_alphanumeric() && bytes[i - 1] != b'_') =>
            {
                if let Some((name, after)) = ident_at(&text, i + 4) {
                    let tail = text[after..].trim_start();
                    if let Some(first) = tail.chars().next() {
                        if first == ';' {
                            decls.push(ModDecl {
                                name: name.to_owned(),
                                explicit_path: pending_path.take(),
                                cfg_attr: std::mem::take(&mut pending_cfg_attr),
                                inline: false,
                                nesting: nesting.clone(),
                            });
                        } else if first == '{' {
                            decls.push(ModDecl {
                                name: name.to_owned(),
                                explicit_path: None,
                                cfg_attr: false,
                                inline: true,
                                nesting: nesting.clone(),
                            });
                            pending_path = None;
                            pending_cfg_attr = false;
                            nesting.push(name.to_owned());
                            module_depths.push(depth);
                        }
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }
    decls
}

/// If a string literal, raw string, char literal or block comment starts at
/// `at`, the index just past it; `None` otherwise. Lifetimes (`'a`) are not
/// char literals and are left alone.
fn skip_literal_or_comment(text: &str, at: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let rest = &text[at..];
    if rest.starts_with("/*") {
        let mut depth = 0usize;
        let mut j = at;
        while j < bytes.len() {
            if !text.is_char_boundary(j) {
                j += 1;
                continue;
            }
            if text[j..].starts_with("/*") {
                depth += 1;
                j += 2;
            } else if text[j..].starts_with("*/") {
                depth = depth.saturating_sub(1);
                j += 2;
                if depth == 0 {
                    return Some(j);
                }
            } else {
                j += 1;
            }
        }
        return Some(bytes.len());
    }
    // raw strings: r"...", r#"..."#, br"...", br#"..."#
    let raw_start = if rest.starts_with("br") {
        Some(2)
    } else if rest.starts_with('r') {
        Some(1)
    } else {
        None
    };
    if let Some(skip) = raw_start {
        let after = &rest[skip..];
        let hashes = after.bytes().take_while(|b| *b == b'#').count();
        if after[hashes..].starts_with('"') {
            let close = format!("\"{}", "#".repeat(hashes));
            let body = at + skip + hashes + 1;
            return Some(
                text[body..]
                    .find(&close)
                    .map_or(bytes.len(), |n| body + n + close.len()),
            );
        }
    }
    match bytes[at] {
        b'"' => {
            let mut j = at + 1;
            while j < bytes.len() && bytes[j] != b'"' {
                if bytes[j] == b'\\' {
                    j += 1;
                }
                j += 1;
            }
            Some((j + 1).min(bytes.len()))
        }
        b'\'' => {
            // 'x', '\n', '\u{1F600}', '"' ; not a lifetime like 'a or 'static
            let after = &rest[1..];
            if let Some(escaped) = after.strip_prefix('\\') {
                let end = escaped.find('\'')?;
                return Some(at + 1 + 1 + end + 1);
            }
            let mut chars = after.char_indices();
            let (_, first) = chars.next()?;
            let (second_at, second) = chars.next()?;
            if second == '\'' && first != '\'' {
                return Some(at + 1 + second_at + 1);
            }
            None
        }
        _ => None,
    }
}

fn includes(source: &str) -> Vec<String> {
    if !source.contains("include") {
        return Vec::new();
    }
    let mut out = Vec::new();
    for marker in ["include!(", "include_str!(", "include_bytes!("] {
        let mut rest = source;
        while let Some(at) = rest.find(marker) {
            let after = &rest[at + marker.len()..];
            if let Some(start) = after.find('"') {
                if let Some(end) = after[start + 1..].find('"') {
                    out.push(after[start + 1..start + 1 + end].to_owned());
                }
            }
            rest = after;
        }
    }
    out
}

/// The directory a plain `mod x;` in `file` resolves against.
fn module_dir(file: &Path) -> PathBuf {
    let dir = file.parent().unwrap().to_path_buf();
    let stem = file.file_stem().unwrap().to_string_lossy().to_string();
    let parent_name = dir.file_name().map(|n| n.to_string_lossy().to_string());
    if stem == "mod" || stem == "lib" || stem == "main" {
        return dir;
    }
    if matches!(
        parent_name.as_deref(),
        Some("bin" | "tests" | "benches" | "examples")
    ) {
        return dir;
    }
    dir.join(stem)
}

type ParsedFileCache = Mutex<HashMap<PathBuf, (Vec<ModDecl>, Vec<String>)>>;

fn parsed_file(file: &Path) -> (Vec<ModDecl>, Vec<String>) {
    static CACHE: OnceLock<ParsedFileCache> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(hit) = cache.lock().unwrap().get(file).cloned() {
        return hit;
    }
    let source = read(file);
    let parsed = (mod_decls(&source), includes(&source));
    cache
        .lock()
        .unwrap()
        .insert(file.to_path_buf(), parsed.clone());
    parsed
}

fn reach(root: &Path, seen: &mut BTreeSet<PathBuf>) {
    let mut stack = vec![root.to_path_buf()];
    let mut raw_seen = HashSet::new();
    while let Some(raw_file) = stack.pop() {
        if !raw_seen.insert(raw_file.clone()) {
            continue;
        }
        let Ok(file) = raw_file.canonicalize() else {
            continue;
        };
        if !seen.insert(file.clone()) {
            continue;
        }
        let (decls, file_includes) = parsed_file(&file);
        let dir = file.parent().unwrap().to_path_buf();
        let base = module_dir(&file);
        for decl in decls {
            if decl.inline {
                continue;
            }
            if let Some(explicit) = decl.explicit_path {
                stack.push(dir.join(explicit));
                if !decl.cfg_attr {
                    continue;
                }
            }
            let mut nested = base.clone();
            for level in &decl.nesting {
                nested = nested.join(level);
            }
            let candidates = [
                nested.join(format!("{}.rs", decl.name)),
                nested.join(&decl.name).join("mod.rs"),
                base.join(format!("{}.rs", decl.name)),
                base.join(&decl.name).join("mod.rs"),
                dir.join(format!("{}.rs", decl.name)),
                dir.join(&decl.name).join("mod.rs"),
            ];
            if let Some(found) = candidates.iter().find(|c| c.is_file()) {
                stack.push(found.clone());
            }
        }
        for inc in file_includes {
            stack.push(dir.join(inc));
        }
    }
}

fn manifest_target_paths(crate_dir: &Path) -> Vec<PathBuf> {
    read(&crate_dir.join("Cargo.toml"))
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let value = line
                .strip_prefix("path")?
                .trim_start()
                .strip_prefix('=')?
                .trim();
            let value = value.strip_prefix('"')?;
            let end = value.find('"')?;
            Some(crate_dir.join(&value[..end]))
        })
        .filter(|p| p.extension().is_some_and(|e| e == "rs"))
        .collect()
}

struct CrateReport {
    name: String,
    orphans: Vec<PathBuf>,
}

fn scan() -> &'static [CrateReport] {
    static REPORTS: OnceLock<Vec<CrateReport>> = OnceLock::new();
    REPORTS.get_or_init(|| {
        let root = workspace_root();
        let mut reports = Vec::new();
        let mut crate_dirs: Vec<PathBuf> = fs::read_dir(root.join("crates"))
            .expect("crates dir")
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.join("Cargo.toml").is_file())
            .collect();
        crate_dirs.push(root.clone());
        crate_dirs.sort();
        for crate_dir in crate_dirs {
            let mut roots = Vec::new();
            for r in ["src/lib.rs", "src/main.rs"] {
                let p = crate_dir.join(r);
                if p.is_file() {
                    roots.push(p);
                }
            }
            for sub in ["src/bin", "tests", "benches", "examples"] {
                let d = crate_dir.join(sub);
                let Ok(entries) = fs::read_dir(&d) else {
                    continue;
                };
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.extension().is_some_and(|e| e == "rs") {
                        roots.push(p);
                    } else if p.join("main.rs").is_file() {
                        roots.push(p.join("main.rs"));
                    }
                }
            }
            roots.extend(manifest_target_paths(&crate_dir));
            let mut seen = BTreeSet::new();
            for r in &roots {
                reach(r, &mut seen);
            }
            let mut all = BTreeSet::new();
            for sub in ["src", "tests", "benches", "examples"] {
                rust_files(&crate_dir.join(sub), &mut all);
            }
            let orphans: Vec<PathBuf> = all
                .into_iter()
                .filter(|f| !is_fixture(f))
                .filter(|f| f.canonicalize().map_or(true, |c| !seen.contains(&c)))
                .collect();
            let name = if crate_dir == root {
                "(root)".to_owned()
            } else {
                crate_dir.file_name().unwrap().to_string_lossy().to_string()
            };
            reports.push(CrateReport { name, orphans });
        }
        reports
    })
}

#[test]
fn unreached_rust_sources_only_shrink_and_stay_in_migrated_folders() {
    let root = workspace_root();
    let reports = scan();
    let mut total = 0;
    let mut outside = Vec::new();
    for report in reports {
        if report.orphans.is_empty() {
            continue;
        }
        total += report.orphans.len();
        eprintln!(
            "orphan sources: {} has {} unreached files",
            report.name,
            report.orphans.len()
        );
        for orphan in &report.orphans {
            let rel = orphan.strip_prefix(&root).unwrap_or(orphan);
            eprintln!("orphan sources:   {}", rel.display());
            let in_migrated = rel.components().any(|c| c.as_os_str() == "migrated");
            if !in_migrated {
                outside.push(rel.display().to_string());
            }
        }
    }
    eprintln!("orphan sources: {total} unreached files in total (ceiling {ORPHAN_CEILING})");
    assert!(
        outside.is_empty(),
        "Rust files no build or test target reaches, outside the migrated folders:\n  {}",
        outside.join("\n  ")
    );
    let over = total.saturating_sub(ORPHAN_CEILING);
    assert!(
        over == 0,
        "{total} unreached Rust files (ceiling {ORPHAN_CEILING}): wire them into their crate or delete them"
    );
}

#[test]
fn reachability_follows_mod_path_inline_and_cfg_attr_declarations() {
    let dir = std::env::temp_dir().join(format!("susi-orphans-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("src/nested")).unwrap();
    fs::create_dir_all(dir.join("src/inner")).unwrap();
    fs::write(
        dir.join("src/lib.rs"),
        "pub mod plain;\n#[path = \"custom_name.rs\"]\nmod renamed;\n#[cfg_attr(not(feature = \"x\"), path = \"alt.rs\")]\nmod gated;\nmod inner {\n    pub mod leaf;\n}\n// mod commented;\ninclude!(\"generated.rs\");\n",
    )
    .unwrap();
    for f in [
        "src/plain.rs",
        "src/custom_name.rs",
        "src/alt.rs",
        "src/inner/leaf.rs",
        "src/generated.rs",
        "src/commented.rs",
        "src/nested/orphan.rs",
    ] {
        fs::write(dir.join(f), "").unwrap();
    }
    let mut seen = BTreeSet::new();
    reach(&dir.join("src/lib.rs"), &mut seen);
    let reached: BTreeSet<String> = seen
        .iter()
        .map(|p| {
            p.strip_prefix(dir.canonicalize().unwrap())
                .unwrap()
                .display()
                .to_string()
                .replace('\\', "/")
        })
        .collect();
    for expected in [
        "src/lib.rs",
        "src/plain.rs",
        "src/custom_name.rs",
        "src/alt.rs",
        "src/inner/leaf.rs",
        "src/generated.rs",
    ] {
        assert!(
            reached.contains(expected),
            "{expected} not reached: {reached:?}"
        );
    }
    assert!(!reached.contains("src/commented.rs"));
    assert!(!reached.contains("src/nested/orphan.rs"));
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn declarations_after_char_literals_raw_strings_and_block_comments_are_seen() {
    let source = "const Q: char = '\"';\nconst R: &str = r#\"a \" b\"#;\n/* \" */\nconst L: &'static str = \"x\";\nmod after;\n";
    let names: Vec<String> = mod_decls(source).into_iter().map(|d| d.name).collect();
    assert_eq!(names, vec!["after".to_owned()], "{names:?}");
}
