//! Source guard: no unattended console launch opens a window.
//!
//! The detached daemon has no console, so a console program it launches
//! (`icacls`, `tasklist`, `netstat`, `taskkill`, `nvidia-smi`) gets a new,
//! visible console window from Windows (CEO, 2026-11-09: error 0x800700e8
//! launching `icacls` on `node.key`). Owner-only ACLs are set and read through
//! `susi_vendor_winapi::acl`, which starts no process; every other helper
//! starts through `susi_paths::console::background_command`, which owns
//! `CREATE_NO_WINDOW`.
//!
//! The guard tokenizes every first-party Rust source in the repository (crate
//! sources with their inline test modules and `tests.rs` files, crate and root
//! integration tests, `xtask`, `fuzz`, the root `build.rs`, and this file):
//! comments are dropped and string literals (plain, raw, byte) are single
//! tokens, so spacing, line breaks, comments between the pieces, raw strings,
//! path prefixes and `.exe` do not hide a launch, and a fixture quoted inside
//! a string is not a launch. Test code is not exempt. Syntax it cannot judge
//! (an unterminated literal or comment, unbalanced brackets, `Command::new`
//! used other than as a direct call, an alias of `Command`) is a violation,
//! and an unreadable directory or file fails the test: nothing is skipped
//! silently.
//!
//! One wrapper is accepted. `susi_core::bounded_cmd::output_within` applies
//! `without_window` to the command before it spawns (proved below from the
//! helper's own source), so a guarded `Command::new` is no launch when it is
//! the whole first argument of a call written with the full path
//! `susi_core::bounded_cmd::output_within(Command::new(..)<builder calls>, ..)`.
//! Anything else, a terminal call such as `.output()` inside the argument, a
//! command that is not the first argument, a shorter path, or another
//! function of the same name, is a direct launch.
//!
//! Two launches sit in files reserved by other native workers; this task may
//! not edit them. They are named in [`HELD_BY_OTHER_CLAIMS`] with their exact
//! count: the guard fails if either grows or any new one appears, and it also
//! fails once a holder fixes theirs, so the entry must then be deleted. The
//! list may only shrink. Both fixes are written and independently verified on
//! other branches (the `bounded_cmd` wrapper for `taskkill`, a pid probe for
//! `tasklist`); this branch cannot carry them, so the entries stay until the
//! combined tree and must be empty there.
//!
//! Not judged: a program name held in a constant or built at run time
//! (`Command::new(PROGRAM)`).

// Test code: a missing source file must fail the test loudly.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::path::{Path, PathBuf};

/// Programs that must never be launched directly from first-party source.
const GUARDED: [&str; 5] = ["icacls", "tasklist", "netstat", "taskkill", "nvidia-smi"];

/// `icacls` is never launched at all (ACLs are native); the rest may only be
/// launched through the helper.
const NEVER_LAUNCHED: &str = "icacls";

/// The `Command` methods that only configure it; a wrapped command may use
/// these and nothing that starts it.
const BUILDERS: [&str; 10] = [
    "arg",
    "args",
    "env",
    "envs",
    "env_clear",
    "env_remove",
    "current_dir",
    "stdin",
    "stdout",
    "stderr",
];

/// Known direct launches in files another native worker holds:
/// `(file, program, count, holder)`.
const HELD_BY_OTHER_CLAIMS: [(&str, &str, usize, &str); 2] = [
    // Five `taskkill` calls in test teardown, reserved by the dot-gmcp-cleanup claim
    // (its independent fix wraps each in `bounded_cmd::output_within`).
    ("tests/gmcp_requirements.rs", "taskkill", 5, "dot-gmcp-cleanup"),
    // The `tasklist` liveness probe, reserved by the dot-reuse claim (the
    // independent fix replaces it with the pid probe).
    (
        "xtask/src/bin/local-workflow.rs",
        "tasklist",
        1,
        "dot-reuse",
    ),
];

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn relative(path: &Path) -> String {
    path.strip_prefix(root())
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Every first-party `.rs` file under `dir`, build output and vendored
/// third-party source excluded. An unreadable directory fails the test.
fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(dir).unwrap_or_else(|e| panic!("cannot list {dir:?}: {e}"));
    for entry in entries {
        let entry = entry.unwrap_or_else(|e| panic!("cannot read an entry of {dir:?}: {e}"));
        let path = entry.path();
        let kind = entry
            .file_type()
            .unwrap_or_else(|e| panic!("cannot stat {path:?}: {e}"));
        if kind.is_dir() {
            if !matches!(
                entry.file_name().to_str(),
                Some("target" | "vendor" | ".git")
            ) {
                rust_files(&path, out);
            }
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Every source with its repository-relative path. An unreadable file fails
/// the test.
fn all_sources() -> Vec<(String, String)> {
    let mut files = Vec::new();
    rust_files(&root(), &mut files);
    files
        .iter()
        .map(|file| {
            let text = fs::read_to_string(file)
                .unwrap_or_else(|e| panic!("cannot read {file:?} for the guard: {e}"));
            (relative(file), text)
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Tok {
    /// An identifier, keyword, lifetime name or number (`0x0800_0000`).
    Id(String),
    /// The text of a string literal (escapes are not decoded).
    Str(String),
    /// Any other single character.
    P(char),
}

#[derive(Clone, Debug)]
struct Token {
    tok: Tok,
    line: usize,
}

/// Source text as tokens, or why it cannot be tokenized.
fn tokenize(source: &str) -> Result<Vec<Token>, String> {
    let c: Vec<char> = source.chars().collect();
    let (mut i, mut line, mut out) = (0usize, 1usize, Vec::new());
    while i < c.len() {
        let ch = c[i];
        let at = line;
        if ch == '\n' {
            line += 1;
            i += 1;
        } else if ch.is_whitespace() {
            i += 1;
        } else if ch == '/' && c.get(i + 1) == Some(&'/') {
            while i < c.len() && c[i] != '\n' {
                i += 1;
            }
        } else if ch == '/' && c.get(i + 1) == Some(&'*') {
            let mut depth = 0;
            loop {
                match (c.get(i), c.get(i + 1)) {
                    (Some('/'), Some('*')) => {
                        depth += 1;
                        i += 2;
                    }
                    (Some('*'), Some('/')) => {
                        depth -= 1;
                        i += 2;
                        if depth == 0 {
                            break;
                        }
                    }
                    (Some(&other), _) => {
                        line += usize::from(other == '\n');
                        i += 1;
                    }
                    (None, _) => return Err(format!("unterminated block comment at line {at}")),
                }
            }
        } else if let Some((hashes, start)) = string_start(&c, i) {
            // `start` is the index of the opening quote; `hashes` is None for
            // an escaped string and the number of `#` for a raw one.
            let mut text = String::new();
            let mut j = start + 1;
            loop {
                match (c.get(j), hashes) {
                    (None, _) => return Err(format!("unterminated string at line {at}")),
                    (Some('"'), None) => break,
                    (Some('"'), Some(n)) if (1..=n).all(|k| c.get(j + k) == Some(&'#')) => {
                        j += n;
                        break;
                    }
                    (Some('\\'), None) => {
                        text.push('\\');
                        if let Some(&next) = c.get(j + 1) {
                            text.push(next);
                            line += usize::from(next == '\n');
                        }
                        j += 2;
                        continue;
                    }
                    (Some(&other), _) => {
                        line += usize::from(other == '\n');
                        text.push(other);
                    }
                }
                j += 1;
            }
            out.push(Token {
                tok: Tok::Str(text),
                line: at,
            });
            i = j + 1;
        } else if ch == '\'' {
            // A character literal, or a lifetime/label (its name is an Id).
            if c.get(i + 1) == Some(&'\\') {
                i += 3;
                while c.get(i).is_some_and(|&x| x != '\'') {
                    i += 1;
                }
                if c.get(i).is_none() {
                    return Err(format!("unterminated character literal at line {at}"));
                }
                i += 1;
            } else if c.get(i + 2) == Some(&'\'') {
                i += 3;
            } else {
                i += 1;
            }
        } else if ch.is_alphanumeric() || ch == '_' {
            let start = i;
            while c.get(i).is_some_and(|&x| x.is_alphanumeric() || x == '_') {
                i += 1;
            }
            out.push(Token {
                tok: Tok::Id(c[start..i].iter().collect()),
                line: at,
            });
        } else {
            out.push(Token {
                tok: Tok::P(ch),
                line: at,
            });
            i += 1;
        }
    }
    Ok(out)
}

/// If a string literal starts at `i`: `(None, quote)` for an escaped string
/// (`"`, `b"`) or `(Some(hashes), quote)` for a raw one (`r"`, `r#"`, `br"`).
fn string_start(c: &[char], i: usize) -> Option<(Option<usize>, usize)> {
    let mut j = i;
    if c[j] == 'b' {
        j += 1;
    }
    let raw = c.get(j) == Some(&'r');
    if raw {
        j += 1;
    }
    let hashes = c[j..].iter().take_while(|&&x| x == '#').count();
    j += hashes;
    let valid = match c.get(j) {
        Some('"') => raw || hashes == 0,
        _ => false,
    } && (raw || j == i || j == i + 1);
    // A prefix is only a prefix at the start of an identifier.
    let boundary = i == 0 || !(c[i - 1].is_alphanumeric() || c[i - 1] == '_');
    (valid && boundary).then_some((raw.then_some(hashes), j))
}

fn is_id(token: Option<&Token>, name: &str) -> bool {
    token.is_some_and(|t| t.tok == Tok::Id(name.to_string()))
}

fn is_p(token: Option<&Token>, punct: char) -> bool {
    token.is_some_and(|t| t.tok == Tok::P(punct))
}

/// The index of the bracket closing the one at `open`, if they balance.
fn close_of(tokens: &[Token], open: usize) -> Option<usize> {
    let mut stack = Vec::new();
    for (at, token) in tokens.iter().enumerate().skip(open) {
        if let Tok::P(p) = token.tok {
            match p {
                '(' | '[' | '{' => stack.push(p),
                ')' | ']' | '}' => {
                    let want = match p {
                        ')' => '(',
                        ']' => '[',
                        _ => '{',
                    };
                    if stack.pop() != Some(want) {
                        return None;
                    }
                    if stack.is_empty() {
                        return Some(at);
                    }
                }
                _ => {}
            }
        }
    }
    None
}

/// `program` as a launcher sees it: its last path component, lower case,
/// without a `.exe` suffix.
fn normalized(program: &str) -> String {
    let name = program.rsplit(['/', '\\']).next().unwrap_or(program);
    let lower = name.to_ascii_lowercase();
    lower.strip_suffix(".exe").unwrap_or(&lower).to_string()
}

/// The guarded programs named by string literals in `tokens`.
fn guarded_in(tokens: &[Token]) -> Vec<&'static str> {
    tokens
        .iter()
        .filter_map(|t| match &t.tok {
            Tok::Str(text) => GUARDED.iter().copied().find(|g| *g == normalized(text)),
            _ => None,
        })
        .collect()
}

/// Whether the `Command` token at `at`, whose `new(..)` call closes at
/// `close`, is the whole first argument of
/// `susi_core::bounded_cmd::output_within(.., ..)`: the full path, then the
/// command (optionally behind a `std::process::`-style qualifier), then only
/// builder calls, then the comma before the timeout.
fn wrapped_by_bounded_cmd(tokens: &[Token], at: usize, close: usize) -> bool {
    let mut k = at;
    while k >= 3
        && is_p(tokens.get(k - 1), ':')
        && is_p(tokens.get(k - 2), ':')
        && matches!(tokens[k - 3].tok, Tok::Id(_))
    {
        k -= 3;
    }
    let callee = [
        "susi_core",
        ":",
        ":",
        "bounded_cmd",
        ":",
        ":",
        "output_within",
        "(",
    ];
    if k < callee.len() {
        return false;
    }
    let before = &tokens[k - callee.len()..k];
    let spelled = before.iter().zip(callee).all(|(t, want)| match &t.tok {
        Tok::Id(name) => name == want,
        Tok::P(p) => want.len() == 1 && want.starts_with(*p),
        Tok::Str(_) => false,
    });
    if !spelled || (k > callee.len() && is_p(tokens.get(k - callee.len() - 1), '.')) {
        return false;
    }
    let mut next = close + 1;
    while is_p(tokens.get(next), '.') {
        let Some(Tok::Id(method)) = tokens.get(next + 1).map(|t| &t.tok) else {
            return false;
        };
        let Some(end) = close_of(tokens, next + 2)
            .filter(|_| BUILDERS.contains(&method.as_str()) && is_p(tokens.get(next + 2), '('))
        else {
            return false;
        };
        next = end + 1;
    }
    is_p(tokens.get(next), ',')
        && close_of(tokens, k - 1).is_some_and(|wrapper_close| wrapper_close > next)
}

/// Guarded launches and unjudgeable syntax in one source: `(line, what)`,
/// where `what` is the program name or `unsupported: <reason>`.
fn scan_source(source: &str) -> Result<Vec<(usize, String)>, String> {
    let tokens = tokenize(source)?;
    let mut found = Vec::new();
    let unsupported = |line: usize, reason: &str, found: &mut Vec<(usize, String)>| {
        found.push((line, format!("unsupported: {reason}")));
    };
    for (at, token) in tokens.iter().enumerate() {
        let line = token.line;
        if is_id(Some(token), "type") {
            let end = tokens[at..]
                .iter()
                .position(|t| t.tok == Tok::P(';'))
                .map_or(tokens.len(), |n| at + n);
            if tokens[at..end]
                .iter()
                .any(|t| t.tok == Tok::Id("Command".into()))
            {
                unsupported(line, "`Command` behind a type alias", &mut found);
            }
        }
        if is_id(Some(token), "Command") && is_id(tokens.get(at + 1), "as") {
            unsupported(line, "`Command` renamed with `as`", &mut found);
        }
        let is_new = is_id(Some(token), "Command")
            && is_p(tokens.get(at + 1), ':')
            && is_p(tokens.get(at + 2), ':')
            && is_id(tokens.get(at + 3), "new");
        let is_helper = is_id(Some(token), "background_command");
        if !is_new && !is_helper {
            continue;
        }
        let mut open = at + if is_new { 4 } else { 1 };
        // `Command::new::<T>(..)`: skip the generic arguments.
        if is_new && is_p(tokens.get(open), ':') && is_p(tokens.get(open + 2), '<') {
            let mut depth = 0;
            for (n, t) in tokens.iter().enumerate().skip(open + 2) {
                depth += i32::from(t.tok == Tok::P('<')) - i32::from(t.tok == Tok::P('>'));
                if depth == 0 {
                    open = n + 1;
                    break;
                }
            }
        }
        if !is_p(tokens.get(open), '(') {
            if is_new {
                unsupported(line, "`Command::new` not called directly", &mut found);
            }
            continue;
        }
        let Some(close) = close_of(&tokens, open) else {
            unsupported(line, "unbalanced brackets after a launch", &mut found);
            continue;
        };
        for program in guarded_in(&tokens[open + 1..close]) {
            // The helper is the sanctioned way, except for `icacls`, which is
            // never launched. A wrapped `Command::new` is no direct launch.
            let sanctioned = if is_new {
                program != NEVER_LAUNCHED && wrapped_by_bounded_cmd(&tokens, at, close)
            } else {
                program != NEVER_LAUNCHED
            };
            if !sanctioned {
                found.push((line, program.to_string()));
            }
        }
    }
    found.sort_unstable();
    Ok(found)
}

/// Every violation in the repository: `(file, line, what)`.
fn all_findings() -> Vec<(String, usize, String)> {
    let mut found = Vec::new();
    for (file, source) in all_sources() {
        let hits = scan_source(&source).unwrap_or_else(|e| panic!("{file}: {e}"));
        found.extend(
            hits.into_iter()
                .map(|(line, what)| (file.clone(), line, what)),
        );
    }
    found
}

#[test]
fn no_source_launches_a_console_helper_directly() {
    let found = all_findings();
    let unexpected: Vec<_> = found
        .iter()
        .filter(|(file, _, what)| {
            !HELD_BY_OTHER_CLAIMS
                .iter()
                .any(|(f, p, _, _)| f == file && p == what)
        })
        .collect();
    assert!(
        unexpected.is_empty(),
        "unattended console launches must use susi_paths::console::background_command, or \
         susi_core::bounded_cmd::output_within(Command::new(..), ..) with the full path \
         (and `icacls` must not be launched at all; use susi_vendor_winapi::acl): {unexpected:#?}"
    );
}

#[test]
fn the_launches_held_by_other_claims_are_exactly_the_recorded_ones() {
    let found = all_findings();
    for (file, program, count, holder) in HELD_BY_OTHER_CLAIMS {
        let actual = found
            .iter()
            .filter(|(f, _, what)| f == file && what == program)
            .count();
        assert_eq!(
            actual, count,
            "{file} (held by the {holder} claim) launches `{program}` directly {actual} time(s), \
             the guard records {count}: a holder that fixed theirs deletes the entry, and no \
             new launch is ever added here"
        );
    }
}

/// Files other claims hold that write the flag's value instead of calling
/// the helper: `(file, count, holder)`. Exact, like [`HELD_BY_OTHER_CLAIMS`].
const HELD_FLAG_LITERALS: [(&str, usize, &str); 1] =
    [("tests/workflow_publication.rs", 1, "wintests")];

/// Whether a token is the numeric literal for `CREATE_NO_WINDOW`.
fn is_create_no_window(token: &Token) -> bool {
    let Tok::Id(text) = &token.tok else {
        return false;
    };
    let digits: String = text.chars().filter(|&x| x != '_').collect();
    let digits = digits.trim_end_matches("u32");
    let value = match digits
        .strip_prefix("0x")
        .or_else(|| digits.strip_prefix("0X"))
    {
        Some(hex) => u32::from_str_radix(hex, 16).ok(),
        None => digits.parse().ok(),
    };
    value == Some(1 << 27)
}

#[test]
fn the_helper_owns_create_no_window() {
    let console = fs::read_to_string(root().join("crates/susi-paths/src/console.rs"))
        .expect("console helper source");
    assert!(console.contains("pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;"));
    assert!(console.contains("creation_flags(CREATE_NO_WINDOW)"));
    // No other first-party source (tests included) writes the flag's value,
    // except the recorded ones held by other claims.
    let mut strays = Vec::new();
    for (file, source) in all_sources() {
        let count = tokenize(&source)
            .unwrap_or_else(|e| panic!("{file}: {e}"))
            .iter()
            .filter(|t| is_create_no_window(t))
            .count();
        let held = HELD_FLAG_LITERALS
            .iter()
            .find(|(f, _, _)| *f == file)
            .map_or(0, |(_, n, _)| *n);
        if file != "crates/susi-paths/src/console.rs" && count != held {
            strays.push((file, count, held));
        }
    }
    assert!(
        strays.is_empty(),
        "CREATE_NO_WINDOW belongs to susi_paths::console only (file, found, recorded as held by          another claim; a holder that fixed theirs deletes the entry): {strays:?}"
    );
}

/// The token range of `pub fn output_within`'s body in the helper's source.
fn bounded_cmd_body(tokens: &[Token]) -> &[Token] {
    let at = tokens
        .windows(3)
        .position(|w| {
            is_id(w.first(), "pub") && is_id(w.get(1), "fn") && is_id(w.get(2), "output_within")
        })
        .expect("bounded_cmd defines `pub fn output_within`");
    let open = tokens[at..]
        .iter()
        .position(|t| t.tok == Tok::P('{'))
        .map(|n| at + n)
        .expect("output_within has a body");
    let close = close_of(tokens, open).expect("output_within's body balances");
    &tokens[open + 1..close]
}

#[test]
fn the_accepted_wrapper_really_applies_no_window_before_it_spawns() {
    let source = fs::read_to_string(root().join("crates/susi-core/src/bounded_cmd.rs"))
        .expect("bounded_cmd source");
    let tokens = tokenize(&source).unwrap();
    // Exactly one function of that name, taking `&mut Command` as `cmd`.
    let defs = tokens
        .windows(2)
        .filter(|w| is_id(w.first(), "fn") && is_id(w.get(1), "output_within"))
        .count();
    assert_eq!(defs, 1, "one definition of output_within");
    let signature: Vec<_> = tokens
        .windows(7)
        .filter(|w| is_id(w.first(), "output_within") && is_p(w.get(1), '('))
        .map(|w| {
            is_id(w.get(2), "cmd")
                && is_p(w.get(3), ':')
                && is_p(w.get(4), '&')
                && is_id(w.get(5), "mut")
                && is_id(w.get(6), "Command")
        })
        .collect();
    assert!(
        signature.contains(&true),
        "output_within(cmd: &mut Command, ..)"
    );
    // Its body calls `susi_paths::console::without_window(cmd)` before it
    // ever spawns or runs the command.
    let body = bounded_cmd_body(&tokens);
    let call = [
        "susi_paths",
        ":",
        ":",
        "console",
        ":",
        ":",
        "without_window",
        "(",
        "cmd",
        ")",
    ];
    let applied = body
        .windows(call.len())
        .position(|w| {
            w.iter().zip(call).all(|(t, want)| match &t.tok {
                Tok::Id(name) => name == want,
                Tok::P(p) => want.len() == 1 && want.starts_with(*p),
                Tok::Str(_) => false,
            })
        })
        .expect("output_within applies without_window(cmd)");
    assert!(
        !body[..applied].iter().any(|t| {
            ["spawn", "output", "status", "creation_flags"]
                .iter()
                .any(|m| t.tok == Tok::Id((*m).into()))
        }),
        "without_window must come before any spawn"
    );
    assert!(
        body[applied..]
            .iter()
            .any(|t| t.tok == Tok::Id("spawn".into())),
        "the command is spawned after it"
    );
}

#[test]
fn the_scan_reaches_test_build_and_tool_sources() {
    let files: Vec<String> = all_sources().into_iter().map(|(file, _)| file).collect();
    for expected in [
        "build.rs",
        "tests/installed_windows_entry.rs",
        "tests/no_console_window.rs",
        "xtask/src/main.rs",
        "crates/susi-paths/src/console.rs",
        "crates/susi-daemon/src/runtime_admin.rs",
    ] {
        assert!(
            files.iter().any(|f| f == expected),
            "{expected} must be scanned"
        );
    }
    assert!(
        files.iter().any(|f| f.contains("/tests/")),
        "crate integration tests must be scanned"
    );
    assert!(
        !files
            .iter()
            .any(|f| f.starts_with("target/") || f.starts_with("vendor/")),
        "build output and vendored source are not first-party"
    );
}

fn launches(source: &str) -> Vec<String> {
    scan_source(source)
        .unwrap()
        .into_iter()
        .map(|(_, what)| what)
        .collect()
}

#[test]
fn the_guard_sees_what_it_must_and_only_that() {
    // A plain direct launch, with its line.
    let bad = "fn a() {\n    let _ = std::process::Command::new(\"taskkill\").status();\n}\n";
    assert_eq!(scan_source(bad).unwrap(), vec![(2, "taskkill".to_string())]);
    // Each of these used to hide a launch (or can): none may.
    for hidden in [
        "Command::new(\n    &\"NetStat.EXE\"\n);",
        "tokio::process::Command::new  ( \"nvidia-smi\" )",
        "Command :: new(\"taskkill\")",
        "Command::\nnew(\"taskkill\")",
        "Command::new(\n    // a comment\n    \"taskkill\")",
        "Command::new(/* c */ \"taskkill\")",
        "Command::new(/* /* nested */ */ \"taskkill\")",
        "Command::new(r\"taskkill\")",
        "Command::new(r#\"taskkill.exe\"#)",
        "Command::new(br\"taskkill\")",
        "Command::new(\"C:\\\\Windows\\\\System32\\\\taskkill.exe\")",
        "Command::new(\"/usr/bin/netstat\")",
        "Command::new::<&str>(\"taskkill\")",
        "Command::new(dir.join(\"icacls.exe\"))",
        "Command::new(if cfg { \"tasklist\" } else { \"netstat\" })",
        "Command::new(&format!(\"{}\", \"taskkill\"))",
        "Command::new('x'.to_string() + \"taskkill\")",
    ] {
        assert!(!launches(hidden).is_empty(), "must be flagged: {hidden}");
    }
    // `icacls` is never launched, helper or not.
    assert_eq!(
        launches("susi_paths::console::background_command(\"icacls\");"),
        ["icacls"]
    );
    assert_eq!(launches("Command::new(\"icacls\")"), ["icacls"]);
    // Syntax it cannot judge is a violation, never a pass.
    for unjudged in [
        "use std::process::Command as Cmd;",
        "use std::process::{Child, Command as Process};",
        "type Cmd = std::process::Command;",
        "let make = Command::new;",
        "let _ = Command::new(\"git\"",
        "let _ = Command::new(\"git\"];",
    ] {
        let found = launches(unjudged);
        assert!(
            found.iter().any(|w| w.starts_with("unsupported")),
            "must be unsupported: {unjudged} -> {found:?}"
        );
    }
    for broken in ["let s = \"taskkill;", "/* never closed", "let r = r#\"x\";"] {
        assert!(scan_source(broken).is_err(), "must not tokenize: {broken}");
    }
    // The helper, comments, strings quoting a launch, unguarded and unknown
    // programs are fine, in test modules too (no test-module exemption).
    let good = "let _ = susi_paths::console::background_command(\"taskkill\");\n\
                // Command::new(\"netstat\")\n\
                /* Command::new(\"tasklist\") */\n\
                let q = \"Command::new(\\\"taskkill\\\")\";\n\
                let r = r#\"Command::new(\"taskkill\")\"#;\n\
                let _ = Command::new(\"git\");\n\
                let _ = Command::new(program);\n\
                let _ = Command::new(\"cmd\");\n\
                let _ = 'a';\n\
                fn lifetime<'a>(x: &'a str) {}\n";
    assert!(launches(good).is_empty(), "{:?}", launches(good));
    let in_test_module = "#[cfg(test)]\nmod t { fn x() { Command::new(\"nvidia-smi\"); } }\n";
    assert_eq!(launches(in_test_module), ["nvidia-smi"]);
}

#[test]
fn only_the_bounded_cmd_wrapper_around_the_whole_command_is_accepted() {
    let wrap = |inner: &str| format!("let o = {inner};");
    for sanctioned in [
        "susi_core::bounded_cmd::output_within(Command::new(\"taskkill\").args([\"/PID\", &pid, \"/T\", \"/F\"]), Duration::from_secs(10))",
        "susi_core::bounded_cmd::output_within(std::process::Command::new(\"taskkill\"), d)",
        "susi_core::bounded_cmd::output_within(\n    Command::new(\"netstat\")\n        .arg(\"-ano\") // all\n        .stderr(Stdio::null()),\n    d,\n)",
        "crate::susi_core::bounded_cmd::output_within(Command::new(\"nvidia-smi\").args([\"-L\"]), d)",
        "::susi_core::bounded_cmd::output_within(Command::new(\"tasklist\").env_clear().env(\"A\", \"b\"), d)",
    ] {
        let found = launches(&wrap(sanctioned));
        assert!(found.is_empty(), "must be accepted: {sanctioned} -> {found:?}");
    }
    for refused in [
        // A terminal call: the command starts before (or without) the wrapper.
        "susi_core::bounded_cmd::output_within(Command::new(\"taskkill\").status(), d)",
        "susi_core::bounded_cmd::output_within(Command::new(\"taskkill\").output(), d)",
        "susi_core::bounded_cmd::output_within(Command::new(\"taskkill\").spawn(), d)",
        "susi_core::bounded_cmd::output_within(Command::new(\"taskkill\").arg(\"x\").status(), d)",
        "susi_core::bounded_cmd::output_within(Command::new(\"taskkill\").creation_flags(0), d)",
        // The command is not the first argument.
        "susi_core::bounded_cmd::output_within(other, Command::new(\"taskkill\"))",
        "susi_core::bounded_cmd::output_within(other, { Command::new(\"taskkill\").status(); d })",
        "susi_core::bounded_cmd::output_within(&mut other, { Command::new(\"taskkill\").status(); d })",
        // Not the helper: a shorter path, another crate, a method, a name
        // reached through `use`, or another function.
        "output_within(Command::new(\"taskkill\"), d)",
        "bounded_cmd::output_within(Command::new(\"taskkill\"), d)",
        "other_core::bounded_cmd::output_within(Command::new(\"taskkill\"), d)",
        "susi_core::other::output_within(Command::new(\"taskkill\"), d)",
        "susi_core::bounded_cmd::status_within(Command::new(\"taskkill\"), d)",
        "susi_core::bounded_cmd::output_within_x(Command::new(\"taskkill\"), d)",
        "thing.susi_core::bounded_cmd::output_within(Command::new(\"taskkill\"), d)",
        // Not a plain builder chain, or never closed.
        "susi_core::bounded_cmd::output_within(Command::new(\"taskkill\").unwrap(), d)",
        "susi_core::bounded_cmd::output_within(Command::new(\"taskkill\") + 1, d)",
        "susi_core::bounded_cmd::output_within(Command::new(\"taskkill\")",
        // `icacls` is never accepted, wrapped or not.
        "susi_core::bounded_cmd::output_within(Command::new(\"icacls\"), d)",
        // A wrapped command does not shelter a second launch beside it.
        "susi_core::bounded_cmd::output_within(Command::new(\"taskkill\").arg({ Command::new(\"tasklist\").status(); \"x\" }), d)",
    ] {
        assert!(
            !launches(&wrap(refused)).is_empty(),
            "must be flagged: {refused}"
        );
    }
    // A shelter is per command: the nested launch is the one reported.
    assert_eq!(
        launches(&wrap("susi_core::bounded_cmd::output_within(Command::new(\"taskkill\").arg({ Command::new(\"tasklist\").status(); \"x\" }), d)")),
        ["tasklist"]
    );
}
