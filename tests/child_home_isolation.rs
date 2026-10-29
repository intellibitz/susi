#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! Every launch of a child under a throwaway home goes through
//! `susi_paths::test_env::isolate_home`.
//!
//! A child handed only `HOME` still resolves the developer's real profile on
//! Windows (its home is the inherited `USERPROFILE`, its config and data dirs
//! the inherited `%APPDATA%` / `%LOCALAPPDATA%`) and any inherited `XDG_*_HOME`
//! on Linux — so about thirty tests here read and wrote the real `~/.susi` and
//! `AppData\Roaming\intellibitz\susi` unless the whole run was isolated, and
//! the release gate's E2E checks linked the real `cloud.env` into their scratch
//! instance. The helper moves all of it with the home; this scan keeps a test,
//! or a production helper a test drives, from naming `HOME` by hand again.
//! `crates/susi-paths/tests/isolate_home.rs` proves a child launched through
//! the helper resolves under its home.

use std::path::{Path, PathBuf};

/// Files that still hand a child a bare `HOME` because their fix belongs to
/// another change. Each must still need the exemption (see
/// `pending_exemptions_are_still_needed`), so this list only shrinks.
const PENDING: &[&str] = &[
    // Owned by another change. `World::susi` needs
    // `isolate_home(&mut cmd, &self.root.join("home"))` in place of its
    // `.env("HOME", …)` and `.env_remove("SUSI_HOME")`; that change drops this
    // entry in the same commit.
    "tests/parallel_workflow.rs",
];

/// Production code that launches `susi` under a throwaway home and that a
/// test drives on every run: its code outside `#[cfg(test)] mod`s is held to
/// the same rule as a test source, and it must actually call the helper.
const SPAWN_HELPERS: &[&str] = &[
    // `SusiAdmin::run_e2e_checks`, run by tests/release_e2e_checks.rs.
    "crates/susi-gawd/src/admin/release.rs",
];

/// This file names the patterns it looks for, so it is not scanned.
const SELF: &str = "tests/child_home_isolation.rs";

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// Every integration-test source (`tests/` and each `crates/*/tests/`), and
/// the [`SPAWN_HELPERS`].
fn scanned_sources() -> Vec<PathBuf> {
    let root = workspace_root();
    let mut files = Vec::new();
    rust_files(&root.join("tests"), &mut files);
    for krate in std::fs::read_dir(root.join("crates")).unwrap().flatten() {
        rust_files(&krate.path().join("tests"), &mut files);
    }
    files.extend(SPAWN_HELPERS.iter().map(|helper| root.join(helper)));
    files.sort();
    files
}

/// `path` relative to the workspace root, `/`-separated on every OS.
fn relative(path: &Path) -> String {
    path.strip_prefix(workspace_root())
        .unwrap_or(path)
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn read(name: &str) -> String {
    std::fs::read_to_string(workspace_root().join(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The literal a `HOME` string keeps in [`lex`]'s output.
const HOME_LITERAL: &str = "\"HOME\"";

/// `source` reduced to what the scan reads: comments dropped, char literals
/// blanked to `' '`, and every string literal emptied to `""` — except one
/// whose value is exactly `HOME`, which stays `"HOME"` however it was spelled
/// (`"HOME"`, `r"HOME"`, `r#"HOME"#`).
///
/// A small Rust lexer, so that a `//` or a quote inside a string, a `'"'`
/// char literal or a lifetime can neither hide code from the scan nor invent
/// some, and no brace inside a literal counts.
fn lex(source: &str) -> String {
    let chars: Vec<char> = source.chars().collect();
    let at = |k: usize| chars.get(k).copied();
    let mut code = String::with_capacity(source.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        // A raw string: `r`, hashes, a quote, and not inside an ident
        // (`br"…"` is one too: its `b` is the only ident char allowed before).
        let raw_hashes = if c == 'r' {
            let mut before = code.chars().rev();
            let starts_token = match (before.next(), before.next()) {
                (Some('b'), prev) => !prev.is_some_and(is_ident),
                (last, _) => !last.is_some_and(is_ident),
            };
            let hashes = chars[i + 1..].iter().take_while(|&&h| h == '#').count();
            (starts_token && at(i + 1 + hashes) == Some('"')).then_some(hashes)
        } else {
            None
        };
        if c == '/' && at(i + 1) == Some('/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && at(i + 1) == Some('*') {
            let mut depth = 0;
            while i < chars.len() {
                if chars[i] == '/' && at(i + 1) == Some('*') {
                    depth += 1;
                    i += 2;
                } else if chars[i] == '*' && at(i + 1) == Some('/') {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            code.push(' ');
        } else if let Some(hashes) = raw_hashes {
            let body = i + 2 + hashes;
            let closes =
                |k: usize| at(k) == Some('"') && (1..=hashes).all(|h| at(k + h) == Some('#'));
            let mut end = body;
            while end < chars.len() && !closes(end) {
                end += 1;
            }
            let value: String = chars[body..end.min(chars.len())].iter().collect();
            code.push_str(if value == "HOME" {
                HOME_LITERAL
            } else {
                "\"\""
            });
            i = (end + 1 + hashes).min(chars.len());
        } else if c == '"' {
            let start = i;
            i += 1;
            while i < chars.len() && chars[i] != '"' {
                i += if chars[i] == '\\' { 2 } else { 1 };
            }
            i = (i + 1).min(chars.len());
            let literal: String = chars[start..i].iter().collect();
            code.push_str(if literal == HOME_LITERAL {
                HOME_LITERAL
            } else {
                "\"\""
            });
        } else if c == '\'' && at(i + 1) == Some('\\') {
            // An escaped char literal: `'\''`, `'\n'`, `'\u{…}'`.
            i += 3;
            while i < chars.len() && chars[i] != '\'' {
                i += 1;
            }
            i = (i + 1).min(chars.len());
            code.push_str("' '");
        } else if c == '\'' && at(i + 2) == Some('\'') {
            // A plain char literal, `'"'` and `'{'` included.
            code.push_str("' '");
            i += 3;
        } else {
            // Anything else, a lifetime's `'` included.
            code.push(c);
            i += 1;
        }
    }
    code
}

/// Lexed `code` without its `#[cfg(test)] mod … { … }` items: a spawn
/// helper's own unit tests name the variables they check its launch for.
fn without_test_modules(code: &str) -> String {
    const ATTR: &str = "#[cfg(test)]";
    let mut kept = String::new();
    let mut rest = code;
    while let Some(attr) = rest.find(ATTR) {
        let after = &rest[attr + ATTR.len()..];
        let module = after
            .trim_start()
            .strip_prefix("mod ")
            .and_then(|module| module.find('{').map(|open| &module[open..]));
        let Some(module) = module else {
            kept.push_str(&rest[..attr + ATTR.len()]);
            rest = after;
            continue;
        };
        kept.push_str(&rest[..attr]);
        let mut depth = 0usize;
        let end = module.char_indices().find_map(|(k, c)| {
            match c {
                '{' => depth += 1,
                '}' => depth -= 1,
                _ => {}
            }
            (depth == 0).then_some(k + 1)
        });
        rest = &module[end.unwrap_or(module.len())..];
    }
    kept.push_str(rest);
    kept
}

/// How many places in lexed `code` name `HOME` other than to read it or to
/// swap this process's own home through an `EnvGuard`.
///
/// Any other `"HOME"` literal is how a child gets handed a bare `HOME`: a
/// `.env("HOME", …)` call, a `("HOME", …)` pair fed to `.envs` or a loop, a
/// `"HOME".into()` key, an `OsStr::new("HOME")`, a constant holding it,
/// `Command::env(&mut cmd, "HOME", …)` or an `env_remove`. Only
/// `env.set("HOME", …)` (an `EnvGuard`, which moves the whole home) and
/// `var("HOME")` / `var_os("HOME")` (reads) are fine.
fn bare_home_sites_in(code: &str) -> usize {
    const ALLOWED: [&str; 3] = ["set", "var", "var_os"];
    code.match_indices(HOME_LITERAL)
        .filter(|&(at, _)| {
            let Some(callee) = code[..at].trim_end().strip_suffix('(') else {
                return true;
            };
            let callee = callee.trim_end();
            let name_at = callee
                .char_indices()
                .rev()
                .take_while(|&(_, c)| is_ident(c))
                .last()
                .map_or(callee.len(), |(k, _)| k);
            !ALLOWED.contains(&&callee[name_at..])
        })
        .count()
}

fn bare_home_sites(source: &str) -> usize {
    bare_home_sites_in(&lex(source))
}

#[test]
fn every_child_home_goes_through_isolate_home() {
    let offenders: Vec<String> = scanned_sources()
        .into_iter()
        .filter_map(|path| {
            let name = relative(&path);
            if name == SELF || PENDING.contains(&name.as_str()) {
                return None;
            }
            let mut code = lex(&read(&name));
            if SPAWN_HELPERS.contains(&name.as_str()) {
                code = without_test_modules(&code);
            }
            let sites = bare_home_sites_in(&code);
            (sites > 0).then(|| format!("{name} ({sites})"))
        })
        .collect();
    assert!(
        offenders.is_empty(),
        "these hand a child HOME by hand, so on Windows it still resolves the \
         real profile (USERPROFILE, APPDATA, LOCALAPPDATA) and on Linux any \
         inherited XDG_*_HOME; launch it through \
         susi_paths::test_env::isolate_home(&mut cmd, home) instead: {offenders:?}"
    );
}

/// A spawn helper that stopped naming `HOME` without calling the helper would
/// hand its child the launching shell's home outright.
#[test]
fn spawn_helpers_call_isolate_home() {
    for name in SPAWN_HELPERS {
        assert!(
            without_test_modules(&lex(&read(name))).contains("isolate_home("),
            "{name} launches susi under a throwaway home without isolate_home"
        );
    }
}

#[test]
fn pending_exemptions_are_still_needed() {
    for name in PENDING {
        assert!(
            bare_home_sites(&read(name)) > 0,
            "{name} no longer sets HOME by hand: drop it from PENDING"
        );
    }
}

/// The scan sees each way a source spells a bare `HOME`, and none of the ways
/// that are fine.
#[test]
fn the_scan_tells_a_bare_home_from_an_isolated_one() {
    for (source, sites) in [
        // Handing a child `HOME`, however it is spelled.
        ("cmd.env(\"HOME\", home)", 1),
        (
            "Command::new(bin)\n    .env(\n        \"HOME\",\n        home,\n    )",
            1,
        ),
        (
            "run(dir, bin, &[(\"HOME\", h), (\"SUSI_AGENT\", \"t\")])",
            1,
        ),
        ("let envs = vec![\n    (\"HOME\", home),\n];", 1),
        ("cmd.env(\"HOME\", a);\ncmd.envs([(\"HOME\", b)]);", 2),
        ("cmd.envs([(\"HOME\".into(), v)]);", 1),
        ("let e = vec![(\"HOME\".to_string(), v)];", 1),
        ("cmd.envs([(OsStr::new(\"HOME\"), v)]);", 1),
        ("const HOME_VAR: &str = \"HOME\";\ncmd.env(HOME_VAR, h);", 1),
        ("cmd.env(r\"HOME\", h)", 1),
        ("cmd.env(r#\"HOME\"#, h)", 1),
        ("Command::env(&mut cmd, \"HOME\", h)", 1),
        ("cmd.env_remove(\"HOME\")", 1),
        ("std::env::set_var(\"HOME\", h)", 1),
        // What surrounds it cannot hide it.
        ("let u = \"http://x\"; cmd.env(\"HOME\", h)", 1),
        ("let q = '\"'; cmd.env(\"HOME\", h)", 1),
        ("let q = '\\''; cmd.env(\"HOME\", h)", 1),
        ("let s = r#\"a \"quoted\" // b\"#; cmd.env(\"HOME\", h)", 1),
        ("fn f<'a>(h: &'a str) { cmd.env(\"HOME\", h) }", 1),
        // Fine: the helper, an EnvGuard swap, reads, comments, other words.
        (
            "isolate_home(&mut cmd, home).env(\"XDG_CONFIG_HOME\", x)",
            0,
        ),
        ("env.set(\"HOME\", &home);", 0),
        ("env.set(\n    \"HOME\",\n    &home,\n);", 0),
        ("let real = std::env::var(\"HOME\");", 0),
        ("let real = std::env::var_os(\"HOME\");", 0),
        ("// .env(\"HOME\", home) used to be enough", 0),
        ("/// cmd.env(\"HOME\", home)", 0),
        ("let x = 1; // cmd.env(\"HOME\", h)", 0),
        ("/* cmd.env(\"HOME\", h) /* nested */ */ let x = 1;", 0),
        ("let s = \"set \\\"HOME\\\" too\";", 0),
        ("let s = [\"$HOME\", \"HOME_DIR\", \"XDG_DATA_HOME\"];", 0),
    ] {
        assert_eq!(bare_home_sites(source), sites, "{source}");
    }
}

/// A spawn helper's unit tests are cut out of its scan, braces in their
/// literals and all, and nothing after them is.
#[test]
fn a_spawn_helpers_unit_tests_are_cut_and_nothing_else() {
    let source = "fn launch() { isolate_home(&mut cmd, h); }\n\
                  #[cfg(test)]\nmod tests {\n    fn t() { let v = [\"HOME\"]; let s = \"}\"; let c = '}'; }\n}\n\
                  #[cfg(test)]\nfn helper() {}\n\
                  fn later() { cmd.env(\"HOME\", h); }\n";
    let code = without_test_modules(&lex(source));
    assert_eq!(bare_home_sites_in(&code), 1, "{code}");
    assert!(code.contains("isolate_home(") && code.contains("fn helper()"));
    assert!(!code.contains("fn t()"), "{code}");
}
