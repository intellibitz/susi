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
//! the whole first argument of a call written with the full path rooted at
//! the crate, `susi_core::bounded_cmd::output_within(Command::new(..)<builder
//! calls>, ..)` (or `::susi_core::..`). Anything else, a terminal call such
//! as `.output()` inside the argument, a command that is not the first
//! argument, a shorter path, a path that continues another one
//! (`crate::susi_core::..`, `m::susi_core::..`), or another function of the
//! same name, is a direct launch.
//!
//! The helper and the wrapper are accepted by spelling, so each spelling must
//! reach the real item. No first-party source may bind `susi_core`,
//! `susi_paths`, `bounded_cmd`, `output_within`, `background_command` or
//! `without_window` to anything else: each may be declared only at the top
//! level of its one file, imported only from its real module, and appear
//! otherwise only as a path segment or a call. A `mod`, `struct`, `type`,
//! `let`, alias (`as`), field, method, macro argument or import from another
//! module of one of those names is a violation (an independent review
//! reproduced the bypass with a local `mod susi_core { pub mod bounded_cmd
//! { .. } }`). A `#[path]` or `include!` must name a file the scan reads, and
//! no `Cargo.toml` may give either crate name to another package (a `package`
//! rename, a path elsewhere, a `[lib]` or `[package]` of that name).
//!
//! No held launch or flag exceptions remain. This checkpoint requires the
//! accepted tasklist removal and the dot-gmcp-cleanup taskkill wrappers in
//! its native combined batch; running it alone fails closed.
//!
//! Not judged: a program name held in a constant or built at run time
//! (`Command::new(PROGRAM)`); code a procedural macro generates; and code
//! generated at build time and included with a computed path, whose one
//! existing site is recorded exactly in [`GENERATED_INCLUDES`] (a new one
//! fails the guard). The manifest check reads dependency lines, not a
//! resolved graph.

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
const HELD_BY_OTHER_CLAIMS: [(&str, &str, usize, &str); 0] = [];

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
            Tok::Id(_) | Tok::P(_) => None,
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
    let root = k - callee.len();
    let before = &tokens[root..k];
    let spelled = before.iter().zip(callee).all(|(t, want)| match &t.tok {
        Tok::Id(name) => name == want,
        Tok::P(p) => want.len() == 1 && want.starts_with(*p),
        Tok::Str(_) => false,
    });
    // `susi_core` must be the root of the path: a method call
    // (`x.susi_core::..`) or a continuation (`crate::susi_core::..`,
    // `m::susi_core::..`) names some other item. A leading `::` (the extern
    // crate itself) is the one prefix allowed.
    let continued = root >= 2
        && is_p(tokens.get(root - 1), ':')
        && is_p(tokens.get(root - 2), ':')
        && root >= 3
        && matches!(&tokens[root - 3].tok, Tok::Id(_) | Tok::P('>' | ')' | ']'));
    if !spelled || continued || (root > 0 && is_p(tokens.get(root - 1), '.')) {
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
    Ok(launch_findings(&tokenize(source)?))
}

fn launch_findings(tokens: &[Token]) -> Vec<(usize, String)> {
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
        let Some(close) = close_of(tokens, open) else {
            unsupported(line, "unbalanced brackets after a launch", &mut found);
            continue;
        };
        for program in guarded_in(&tokens[open + 1..close]) {
            // The helper is the sanctioned way, except for `icacls`, which is
            // never launched. A wrapped `Command::new` is no direct launch.
            let sanctioned = if is_new {
                program != NEVER_LAUNCHED && wrapped_by_bounded_cmd(tokens, at, close)
            } else {
                program != NEVER_LAUNCHED
            };
            if !sanctioned {
                found.push((line, program.to_string()));
            }
        }
    }
    found.sort_unstable();
    found
}

/// The names the two sanctioned forms resolve through. The guard accepts
/// `susi_paths::console::background_command(..)` (or the bare name imported
/// from there) and `susi_core::bounded_cmd::output_within(..)` by spelling,
/// so each spelling must reach the real item: no first-party source may bind
/// one of these names to anything else. A local `mod susi_core` with its own
/// `output_within`, `use x as susi_core`, `extern crate x as susi_core`, a
/// local `fn background_command`, `let background_command = ..`, a macro
/// argument or a field of that name would otherwise turn a direct launch
/// into one the guard accepts.
const BOUND: [&str; 6] = [
    "susi_core",
    "susi_paths",
    "bounded_cmd",
    "output_within",
    "background_command",
    "without_window",
];

/// The only declarations of a bound name: `(name, keyword, file)`, each at
/// the top level of its file.
const DEFINITIONS: [(&str, &str, &str); 4] = [
    ("output_within", "fn", "crates/susi-core/src/bounded_cmd.rs"),
    (
        "background_command",
        "fn",
        "crates/susi-paths/src/console.rs",
    ),
    ("without_window", "fn", "crates/susi-paths/src/console.rs"),
    ("bounded_cmd", "mod", "crates/susi-core/src/lib.rs"),
];

/// The only imports of a bound name as a leaf (`use <parent>::<name>;`, in a
/// group too): `(name, parent, file prefix)`. An empty parent is the crate
/// itself (`use susi_core;`, `extern crate susi_core;`).
const IMPORTS: [(&str, &str, &str); 10] = [
    ("susi_core", "", ""),
    ("susi_paths", "", ""),
    ("bounded_cmd", "susi_core", ""),
    ("output_within", "susi_core::bounded_cmd", ""),
    ("background_command", "susi_paths::console", ""),
    ("without_window", "susi_paths::console", ""),
    ("bounded_cmd", "crate", "crates/susi-core/src/"),
    (
        "output_within",
        "crate::bounded_cmd",
        "crates/susi-core/src/",
    ),
    (
        "background_command",
        "crate::console",
        "crates/susi-paths/src/",
    ),
    ("without_window", "crate::console", "crates/susi-paths/src/"),
];

/// The one alias of a bound name: susi-core names itself for code shared
/// with crates that reach it as `crate::susi_core`.
const SELF_ALIAS: (&str, &str) = ("crates/susi-core/src/lib.rs", "susi_core");

/// `include!` with a computed path reads code no scan can see. The exact
/// recorded sites: `(file, count, what it includes)`. A new one fails.
const GENERATED_INCLUDES: [(&str, usize, &str); 1] = [(
    "crates/susi-gawd-agents/src/self_core.rs",
    1,
    "OUT_DIR/generated_axioms.rs, written by crates/susi-gawd-agents/build.rs (scanned)",
)];

/// For each token, whether it is inside a `use` or `extern crate` item.
fn import_items(tokens: &[Token]) -> Vec<Option<usize>> {
    let mut inside = vec![None; tokens.len()];
    let mut at = 0;
    while at < tokens.len() {
        let starts = is_id(tokens.get(at), "use")
            || (is_id(tokens.get(at), "extern") && is_id(tokens.get(at + 1), "crate"));
        if starts {
            let end = tokens[at..]
                .iter()
                .position(|t| t.tok == Tok::P(';'))
                .map_or(tokens.len(), |n| at + n);
            for slot in &mut inside[at..end] {
                *slot = Some(at);
            }
            at = end;
        }
        at += 1;
    }
    inside
}

/// The path a `use` tree imports the leaf at `at` from (`a::b` for
/// `use a::b::{c, leaf}`), walking back to the item start `start`.
fn import_parent(tokens: &[Token], start: usize, at: usize) -> String {
    let mut segments = Vec::new();
    let mut k = at;
    loop {
        let sep = k >= 2 && is_p(tokens.get(k - 1), ':') && is_p(tokens.get(k - 2), ':');
        if sep && k >= 3 && k - 3 > start {
            if let Tok::Id(segment) = &tokens[k - 3].tok {
                segments.push(segment.clone());
                k -= 3;
                continue;
            }
        }
        if !sep && k > start + 1 && matches!(tokens[k - 1].tok, Tok::P('{' | ',')) {
            // Back to the `{` opening this group.
            let mut depth = 0usize;
            let mut j = k - 1;
            while j > start {
                match tokens[j].tok {
                    Tok::P('}') => depth += 1,
                    Tok::P('{') if depth == 0 => break,
                    Tok::P('{') => depth -= 1,
                    Tok::P(_) | Tok::Id(_) | Tok::Str(_) => {}
                }
                j -= 1;
            }
            if j == start {
                return "<unbalanced>".into();
            }
            k = j;
            continue;
        }
        break;
    }
    segments.reverse();
    segments.join("::")
}

/// Where a repository-relative `target` written in `file` resolves, as a
/// repository-relative path (lexically: `.` and `..` are applied).
fn resolve_beside(file: &str, target: &str) -> String {
    let mut parts: Vec<&str> = file.split('/').collect();
    parts.pop();
    for piece in target.split(['/', '\\']) {
        match piece {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return format!("<outside the repository>/{target}");
                }
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

/// Bindings of [`BOUND`] names other than the canonical ones, `#[path]`
/// attributes and `include!` calls that read a file the scan does not see,
/// in one source: `(line, what)`.
fn binding_findings(file: &str, tokens: &[Token], scanned: &[String]) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    let mut flag = |line: usize, what: String| found.push((line, format!("unsupported: {what}")));
    let imports = import_items(tokens);
    let mut depth = 0usize;
    for (at, token) in tokens.iter().enumerate() {
        match token.tok {
            Tok::P('{') => depth += 1,
            Tok::P('}') => depth = depth.saturating_sub(1),
            Tok::P(_) | Tok::Id(_) | Tok::Str(_) => {}
        }
        let Tok::Id(name) = &token.tok else {
            continue;
        };
        let line = token.line;
        // `#[path = ".."]`, also inside `cfg_attr`: the module's file must be
        // a scanned first-party source.
        if name == "path" && is_p(tokens.get(at + 1), '=') && in_attribute(tokens, at) {
            match tokens.get(at + 2).map(|t| &t.tok) {
                Some(Tok::Str(target)) => {
                    let resolved = resolve_beside(file, target);
                    if !scanned.contains(&resolved) {
                        flag(
                            line,
                            format!("#[path] to {resolved}, which the scan does not read"),
                        );
                    }
                }
                _ => flag(line, "#[path] that is not a string literal".into()),
            }
        }
        if name == "include" && is_p(tokens.get(at + 1), '!') {
            match (
                tokens.get(at + 2).map(|t| &t.tok),
                tokens.get(at + 3).map(|t| &t.tok),
            ) {
                (Some(Tok::P('(')), Some(Tok::Str(target))) if is_p(tokens.get(at + 4), ')') => {
                    let resolved = resolve_beside(file, target);
                    if !scanned.contains(&resolved) {
                        flag(
                            line,
                            format!("include! of {resolved}, which the scan does not read"),
                        );
                    }
                }
                _ => flag(line, "include! of a computed path".into()),
            }
        }
        if !BOUND.contains(&name.as_str()) {
            continue;
        }
        let prev = at.checked_sub(1).map(|p| &tokens[p].tok);
        let next = tokens.get(at + 1).map(|t| &t.tok);
        let path_next = is_p(tokens.get(at + 1), ':') && is_p(tokens.get(at + 2), ':');
        let path_prev = at >= 2 && is_p(tokens.get(at - 1), ':') && is_p(tokens.get(at - 2), ':');
        if let Some(start) = imports[at] {
            if path_next {
                continue; // a segment of the imported path
            }
            if is_id(at.checked_sub(1).and_then(|p| tokens.get(p)), "as") {
                let self_alias = (file, name.as_str()) == SELF_ALIAS
                    && at >= 2
                    && is_id(tokens.get(at - 2), "self");
                if !self_alias {
                    flag(line, format!("`{name}` bound by an alias"));
                }
                continue;
            }
            let parent = import_parent(tokens, start, at);
            let canonical = IMPORTS
                .iter()
                .any(|(n, p, prefix)| n == name && *p == parent && file.starts_with(prefix));
            if !canonical {
                flag(line, format!("`{name}` imported from `{parent}`"));
            }
            continue;
        }
        match prev {
            Some(Tok::Id(keyword)) if keyword == "fn" || keyword == "mod" => {
                let canonical = DEFINITIONS
                    .iter()
                    .any(|(n, k, f)| n == name && k == keyword && *f == file && depth == 0);
                if !canonical {
                    flag(
                        line,
                        format!("`{keyword} {name}` outside its one definition"),
                    );
                }
            }
            // A path segment or root, or a call of the name in scope.
            _ if path_next || path_prev => {}
            Some(Tok::P('.')) => flag(line, format!("`.{name}` method or field")),
            _ if next == Some(&Tok::P('(')) => {}
            _ => flag(
                line,
                format!("`{name}` bound or used other than as a path or call"),
            ),
        }
    }
    found
}

/// Whether the token at `at` is inside a `#[..]` or `#![..]` attribute.
fn in_attribute(tokens: &[Token], at: usize) -> bool {
    let mut depth = 0usize;
    for j in (0..at).rev() {
        match tokens[j].tok {
            Tok::P(']') => depth += 1,
            Tok::P('[') if depth == 0 => {
                return is_p(j.checked_sub(1).and_then(|p| tokens.get(p)), '#')
                    || (is_p(j.checked_sub(1).and_then(|p| tokens.get(p)), '!')
                        && is_p(j.checked_sub(2).and_then(|p| tokens.get(p)), '#'));
            }
            Tok::P('[') => depth -= 1,
            Tok::P(';' | '{' | '}') => return false,
            Tok::P(_) | Tok::Id(_) | Tok::Str(_) => {}
        }
    }
    false
}

/// Every first-party `Cargo.toml` with its repository-relative path.
fn all_manifests() -> Vec<(String, String)> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
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
                    walk(&path, out);
                }
            } else if entry.file_name() == "Cargo.toml" {
                out.push(path);
            }
        }
    }
    let mut files = Vec::new();
    walk(&root(), &mut files);
    files
        .iter()
        .map(|file| {
            let text = fs::read_to_string(file)
                .unwrap_or_else(|e| panic!("cannot read {file:?} for the guard: {e}"));
            (relative(file), text)
        })
        .collect()
}

/// Manifest lines that would make `susi_core` or `susi_paths` name another
/// package: `(line, what)`. A dependency under either key must be the
/// workspace crate (a `path` to it or `workspace = true`, no `package`
/// rename), no other package or library may take either name, and a
/// dependency table header for either (`[dependencies.susi-core]`) is not
/// judged and so refused.
fn manifest_findings(file: &str, text: &str) -> Vec<(usize, String)> {
    let crates = [
        ("susi_core", "crates/susi-core"),
        ("susi_paths", "crates/susi-paths"),
    ];
    let mut found = Vec::new();
    let mut section = String::new();
    for (n, raw) in text.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.starts_with('[') {
            section = line.trim_matches(['[', ']']).trim().to_string();
            let header = section.replace('-', "_");
            if crates.iter().any(|(name, _)| {
                header.ends_with(&format!(".{name}")) || header.contains(&format!(".{name}."))
            }) {
                found.push((n + 1, format!("unsupported: table [{section}]")));
            }
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().trim_matches('"').replace('-', "_");
        let value = value.trim();
        // `{ a = "x", b = true }` as `[("a", "\"x\""), ("b", "true")]`.
        let fields: Vec<(String, String)> = value
            .trim_matches(['{', '}'])
            .split(',')
            .filter_map(|field| field.split_once('='))
            .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
            .collect();
        let field = |name: &str| {
            fields
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.as_str())
        };
        let in_dependencies = section.ends_with("dependencies");
        for (name, dir) in crates {
            if in_dependencies && key == name {
                let path = field("path").map(|p| resolve_beside(file, p.trim_matches('"')));
                let canonical = field("package").is_none()
                    && match path {
                        Some(path) => path == dir,
                        None => field("workspace") == Some("true"),
                    };
                if !canonical {
                    found.push((n + 1, format!("unsupported: dependency {line}")));
                }
            }
            // Only a package or library name binds a crate name (a binary
            // named after it does not).
            let names_a_crate = section == "package" || section == "lib";
            if names_a_crate
                && key == "name"
                && value.trim_matches('"').replace('-', "_") == name
                && file != format!("{dir}/Cargo.toml")
            {
                found.push((n + 1, format!("unsupported: [{section}] named {value}")));
            }
        }
    }
    found
}

/// Launches and binding violations in one file, given the scanned set.
fn scan_file(file: &str, source: &str, scanned: &[String]) -> Result<Vec<(usize, String)>, String> {
    let tokens = tokenize(source)?;
    let mut found = launch_findings(&tokens);
    found.extend(binding_findings(file, &tokens, scanned));
    found.sort_unstable();
    Ok(found)
}

/// Every violation in the repository: `(file, line, what)`.
fn all_findings() -> Vec<(String, usize, String)> {
    let sources = all_sources();
    let scanned: Vec<String> = sources.iter().map(|(file, _)| file.clone()).collect();
    let mut found = Vec::new();
    for (file, source) in &sources {
        let hits = scan_file(file, source, &scanned).unwrap_or_else(|e| panic!("{file}: {e}"));
        found.extend(
            hits.into_iter()
                .map(|(line, what)| (file.clone(), line, what)),
        );
    }
    for (file, text) in all_manifests() {
        found.extend(
            manifest_findings(&file, &text)
                .into_iter()
                .map(|(line, what)| (file.clone(), line, what)),
        );
    }
    // The recorded computed includes, exactly.
    for (file, count, _) in GENERATED_INCLUDES {
        let computed = found
            .iter()
            .filter(|(f, _, what)| f == file && what == "unsupported: include! of a computed path")
            .count();
        if computed == count {
            found.retain(|(f, _, what)| {
                !(f == file && what == "unsupported: include! of a computed path")
            });
        } else {
            found.push((
                file.to_string(),
                0,
                format!("recorded {count} computed include!(s), found {computed}"),
            ));
        }
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
const HELD_FLAG_LITERALS: [(&str, usize, &str); 0] = [];

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
        "::susi_core::bounded_cmd::output_within(Command::new(\"tasklist\").env_clear().env(\"A\", \"b\"), d)",
        "(::susi_core::bounded_cmd::output_within(Command::new(\"tasklist\"), d))",
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
        // `susi_core` not at the root of the path names another item.
        "crate::susi_core::bounded_cmd::output_within(Command::new(\"nvidia-smi\").args([\"-L\"]), d)",
        "self::susi_core::bounded_cmd::output_within(Command::new(\"taskkill\"), d)",
        "super::susi_core::bounded_cmd::output_within(Command::new(\"taskkill\"), d)",
        "evil::susi_core::bounded_cmd::output_within(Command::new(\"taskkill\"), d)",
        "<T>::susi_core::bounded_cmd::output_within(Command::new(\"taskkill\"), d)",
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

/// Findings for `source` as if it were the repository file `file`, with
/// `extra` (and `file` itself) as the scanned set.
fn findings_in(file: &str, source: &str, extra: &[&str]) -> Vec<String> {
    let mut scanned: Vec<String> = extra.iter().map(|s| (*s).to_string()).collect();
    scanned.push(file.to_string());
    scan_file(file, source, &scanned)
        .unwrap()
        .into_iter()
        .map(|(_, what)| what)
        .collect()
}

/// The independent review's bypass: a local `susi_core::bounded_cmd` whose
/// `output_within` launches directly. The wrapped call is spelled exactly as
/// the sanctioned form, so the launch scan alone accepts it; the binding
/// check must not.
#[test]
fn a_local_module_shadowing_the_wrapper_path_is_refused() {
    let repro = "use std::process::{Command, Output};\n\
                 use std::time::Duration;\n\
                 mod susi_core {\n\
                     pub mod bounded_cmd {\n\
                         use std::process::{Command, Output};\n\
                         pub fn output_within(cmd: &mut Command, _d: std::time::Duration) -> std::io::Result<Output> {\n\
                             let _ = cmd.status();\n\
                             cmd.output()\n\
                         }\n\
                     }\n\
                 }\n\
                 fn kill(pid: &str) -> std::io::Result<Output> {\n\
                     susi_core::bounded_cmd::output_within(Command::new(\"taskkill\").args([\"/PID\", pid]), Duration::from_secs(1))\n\
                 }\n";
    // The launch scan alone is fooled: that is why bindings are checked.
    assert!(launches(repro).is_empty());
    let found = findings_in("tests/evil.rs", repro, &[]);
    for expected in [
        "unsupported: `mod susi_core` outside its one definition",
        "unsupported: `mod bounded_cmd` outside its one definition",
        "unsupported: `fn output_within` outside its one definition",
    ] {
        assert!(
            found.iter().any(|w| w == expected),
            "missing {expected:?} in {found:?}"
        );
    }
}

#[test]
fn every_other_way_to_rebind_a_sanctioned_name_is_refused() {
    let wrapped =
        "fn k() { let _ = susi_core::bounded_cmd::output_within(Command::new(\"taskkill\"), d); }\n";
    let helper = "fn k() { let _ = background_command(\"taskkill\").status(); }\n";
    // Alone, both are clean, so each case below is refused for what it adds.
    assert_eq!(
        findings_in("crates/x/src/lib.rs", wrapped, &[]),
        Vec::<String>::new()
    );
    assert_eq!(
        findings_in("crates/x/src/lib.rs", helper, &[]),
        Vec::<String>::new()
    );
    for (case, source) in [
        ("use alias", format!("use evil as susi_core;\n{wrapped}")),
        (
            "use alias in a group",
            format!("use x::{{a, evil as susi_core}};\n{wrapped}"),
        ),
        (
            "extern crate alias",
            format!("extern crate evil as susi_core;\n{wrapped}"),
        ),
        (
            "imported from elsewhere",
            format!("use evil::susi_core;\n{wrapped}"),
        ),
        (
            "path module",
            format!("#[path = \"evil.rs\"]\nmod susi_core;\n{wrapped}"),
        ),
        (
            "raw identifier module",
            format!("mod r#susi_core {{}}\n{wrapped}"),
        ),
        ("module made by a macro", format!("m!(susi_core);\n{wrapped}")),
        ("struct of the name", format!("struct susi_core;\n{wrapped}")),
        ("type of the name", format!("type susi_core = u8;\n{wrapped}")),
        (
            "submodule renamed",
            format!("use susi_core::other as bounded_cmd;\n{wrapped}"),
        ),
        (
            "local helper",
            format!("fn background_command(p: &str) -> Command {{ Command::new(p) }}\n{helper}"),
        ),
        (
            "helper in a closure",
            "fn k() { let background_command = |p: &str| Command::new(p); background_command(\"taskkill\").status(); }\n".to_string(),
        ),
        (
            "helper imported from elsewhere",
            format!("use evil::background_command;\n{helper}"),
        ),
        (
            "helper alias",
            format!("use evil::spawn as background_command;\n{helper}"),
        ),
        (
            "helper in a group from elsewhere",
            format!("use evil::{{background_command, x}};\n{helper}"),
        ),
        (
            "helper as a method",
            "fn k() { let _ = x.background_command(\"taskkill\").status(); }\n".to_string(),
        ),
        (
            "helper as a field",
            "struct S { background_command: fn(&str) -> Command }\n".to_string(),
        ),
        (
            "helper by a macro",
            format!("macro_rules! background_command {{ () => {{}} }}\n{helper}"),
        ),
        (
            "without_window redefined",
            "pub fn without_window(c: &mut Command) -> &mut Command { c }\n".to_string(),
        ),
        (
            "include of an unscanned file",
            format!("include!(\"../target/evil.rs\");\n{wrapped}"),
        ),
        (
            "include of a computed path",
            format!("include!(concat!(env!(\"OUT_DIR\"), \"/x.rs\"));\n{wrapped}"),
        ),
        (
            "path attribute behind cfg_attr",
            format!("#[cfg_attr(windows, path = \"../target/x.rs\")]\nmod m;\n{wrapped}"),
        ),
    ] {
        let found = findings_in("crates/x/src/lib.rs", &source, &[]);
        assert!(
            found.iter().any(|w| w.starts_with("unsupported:")),
            "{case} must be refused: {source} -> {found:?}"
        );
    }
}

#[test]
fn the_canonical_bindings_stay_accepted() {
    // The dot-gmcp-cleanup fix, unchanged, in a root integration test.
    let gmcp = "fn stop(child: &std::process::Child) {\n\
                    let pid = child.id().to_string();\n\
                    let _ = susi_core::bounded_cmd::output_within(\n\
                        Command::new(\"taskkill\").args([\"/PID\", &pid, \"/T\", \"/F\"]),\n\
                        std::time::Duration::from_secs(10),\n\
                    );\n\
                }\n";
    assert_eq!(
        findings_in("tests/gmcp_requirements.rs", gmcp, &[]),
        Vec::<String>::new()
    );
    for (file, source, extra) in [
        (
            "crates/a/src/os.rs",
            "use susi_paths::console::background_command;\nfn f() { background_command(\"taskkill\").status(); }\n",
            &[][..],
        ),
        (
            "crates/a/src/os.rs",
            "use susi_paths::console::{self, background_command};\n",
            &[][..],
        ),
        (
            "crates/a/src/lib.rs",
            "pub use susi_core;\nfn f() { crate::susi_core::bounded_cmd::output_within(&mut c, d); }\n",
            &[][..],
        ),
        ("crates/a/src/lib.rs", "extern crate susi_core;\n", &[][..]),
        (
            "crates/susi-core/src/lib.rs",
            "pub use self as susi_core;\npub mod bounded_cmd;\n",
            &[][..],
        ),
        (
            "crates/susi-core/src/verification.rs",
            "use crate::bounded_cmd::output_within;\nfn f() { output_within(&mut c, d); }\n",
            &[][..],
        ),
        (
            "crates/susi-paths/src/console.rs",
            "pub fn background_command(p: &str) -> Command { let mut c = Command::new(p); without_window(&mut c); c }\npub fn without_window(c: &mut Command) -> &mut Command { c }\n",
            &[][..],
        ),
        (
            "build.rs",
            "#[path = \"crates/susi-paths/src/console.rs\"]\nmod console;\nfn f() { console::background_command(\"nvidia-smi\"); }\n",
            &["crates/susi-paths/src/console.rs"][..],
        ),
        (
            "src/a/package.rs",
            "const P: &str = include!(\"policy.rs\");\n",
            &["src/a/policy.rs"][..],
        ),
    ] {
        assert_eq!(
            findings_in(file, source, extra),
            Vec::<String>::new(),
            "{file}: {source}"
        );
    }
    // The self alias is susi-core's alone.
    assert!(!findings_in("crates/a/src/lib.rs", "pub use self as susi_core;\n", &[]).is_empty());
    // A definition is canonical only at the top level of its file.
    assert!(!findings_in(
        "crates/susi-paths/src/console.rs",
        "mod inner { pub fn background_command() {} }\n",
        &[]
    )
    .is_empty());
}

#[test]
fn a_manifest_cannot_rebind_the_crate_names() {
    let refused = [
        (
            "crates/x/Cargo.toml",
            "[dependencies]\nsusi_core = { package = \"evil\", path = \"../evil\" }\n",
        ),
        (
            "crates/x/Cargo.toml",
            "[dependencies]\nsusi-core = { path = \"../evil\" }\n",
        ),
        (
            "crates/x/Cargo.toml",
            "[dev-dependencies]\nsusi-paths = { package = \"evil\", workspace = true }\n",
        ),
        (
            "crates/x/Cargo.toml",
            "[dependencies.susi-core]\npath = \"../susi-core\"\n",
        ),
        ("crates/x/Cargo.toml", "[lib]\nname = \"susi_core\"\n"),
        ("crates/x/Cargo.toml", "[package]\nname = \"susi-paths\"\n"),
        (
            "Cargo.toml",
            "[workspace.dependencies]\nsusi-core = { path = \"crates/evil\" }\n",
        ),
    ];
    for (file, text) in refused {
        assert!(!manifest_findings(file, text).is_empty(), "{file}: {text}");
    }
    let accepted = [
        (
            "crates/x/Cargo.toml",
            "[dependencies]\nsusi-core = { path = \"../susi-core\" }\nsusi-paths = { workspace = true }\n",
        ),
        (
            "Cargo.toml",
            "[workspace.dependencies]\nsusi-paths = { path = \"crates/susi-paths\" } # comment\n",
        ),
        ("crates/susi-core/Cargo.toml", "[package]\nname = \"susi-core\"\n"),
        (
            "crates/x/Cargo.toml",
            "[[bin]]\nname = \"susi-paths\"\npath = \"src/main.rs\"\n",
        ),
        (
            "crates/x/Cargo.toml",
            "[dependencies]\nother = { package = \"susi-core\", path = \"../susi-core\" }\n",
        ),
    ];
    for (file, text) in accepted {
        assert_eq!(manifest_findings(file, text), Vec::new(), "{file}: {text}");
    }
}

#[test]
fn combined_scan_preserves_lexical_errors_and_line_numbered_findings() {
    let file = "tests/guard-fixture.rs";
    let scanned = [file.to_string()];
    for (source, expected) in [
        ("\n/* unfinished", "unterminated block comment at line 2"),
        ("\n\"unfinished", "unterminated string at line 2"),
    ] {
        assert_eq!(scan_source(source).unwrap_err(), expected);
        assert_eq!(scan_file(file, source, &scanned).unwrap_err(), expected);
    }
    let source = "Command::new(\"taskkill\");\nmod susi_core {}\n";
    assert_eq!(scan_source(source).unwrap(), vec![(1, "taskkill".into())]);
    assert_eq!(
        scan_file(file, source, &scanned).unwrap(),
        vec![
            (1, "taskkill".into()),
            (
                2,
                "unsupported: `mod susi_core` outside its one definition".into()
            ),
        ]
    );
}
