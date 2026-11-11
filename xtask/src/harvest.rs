//! Native Apache-2.0 harvest tooling (CEO Mandate 74, section 5).
//!
//! `local-harvest` pins an upstream commit in a local quarantine ref
//! (`refs/susi/upstream/<slug>/<sha>`), verifies the license of every
//! requested file at that exact commit, imports approved blobs together with a
//! `susi-harvest-manifest-v1` manifest, regenerates `THIRD_PARTY_NOTICES`, and
//! audits the Apache-2.0 section 4 obligations of every recorded harvest.
//!
//! Upstream content is untrusted: it is read only as Git objects (`ls-tree`,
//! `cat-file`) and is never checked out, built or executed. The tool works in
//! whatever Git checkout is the current directory, so any repository can run
//! the same binary. License detection is heuristic text matching, not legal
//! review; see `docs/harvest.md` for what is and is not covered.

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

type Result<T> = std::result::Result<T, String>;

const MANIFEST_SCHEMA: &str = "susi-harvest-manifest-v1";
const QUARANTINE_PREFIX: &str = "refs/susi/upstream";
const INTAKE_DIR: &str = "susi-harvest";
const MANIFEST_DIR: &str = ".agents/harvest";
const NOTICES_FILE: &str = "THIRD_PARTY_NOTICES";
const APACHE_TEXT_PATH: &str = "LICENSES/Apache-2.0.txt";
const HEADER_LINES: usize = 30;
const LICENSE_NAMES: [&str; 6] = [
    "LICENSE",
    "LICENSE.txt",
    "LICENSE.md",
    "LICENSE-APACHE",
    "LICENSE-MIT",
    "COPYING",
];
const NOTICE_NAMES: [&str; 3] = ["NOTICE", "NOTICE.txt", "NOTICE.md"];
/// Harvested code is taken under Apache-2.0 only (CEO decision 2026-11-12:
/// "only apache 2.0 license only for pure compliance"). A dual-licensed file
/// (`Apache-2.0 OR MIT`) is taken under its Apache-2.0 alternative. MIT, BSD
/// and `Apache-2.0 WITH LLVM-exception` code is not harvested. This is the
/// rule for source copied into Susi; the dependency allowlist (Mandate 68) is
/// a different rule and is unchanged.
const ALLOWED_LICENSES: [&str; 1] = ["Apache-2.0"];
const REJECT_PHRASES: [&str; 7] = [
    "GNU General Public License",
    "GNU Lesser General Public",
    "GNU Affero",
    "Mozilla Public License",
    "Server Side Public License",
    "Business Source License",
    "Commons Clause",
];
const VALUE_FLAGS: [&str; 12] = [
    "url", "rev", "slug", "path", "map", "id", "task", "feature", "mode", "agent", "range",
    "summary",
];
const SWITCHES: [&str; 3] = ["json", "overwrite", "check"];
const USAGE: &str = "usage: local-harvest <command> [--json]
  intake  --url <git-url> --rev <40-hex sha | ref name> --slug <slug>
  scan    --slug <slug> --rev <sha> --path <upstream-path>...
  import  --slug <slug> --rev <sha> --map <upstream-path>=<dest-path>... --id H-<AGENT>-<n>
          --task T-<AGENT>-<n> [--feature <id>] [--mode transplant|port] [--agent <name>]
          [--summary <text>] [--url <git-url>] [--overwrite]
  notices [--check]
  audit   [--range <A..B>]
See docs/harvest.md (CEO Mandate 74, section 5).";

/// Runs one `local-harvest` command in the Git worktree containing `cwd`.
pub fn run(cwd: &Path, args: &[String]) -> Result<String> {
    let cli = parse_cli(args)?;
    if matches!(cli.command.as_str(), "help" | "--help" | "-h") {
        return Ok(USAGE.to_owned());
    }
    let git = Git::open(cwd)?;
    let report = match cli.command.as_str() {
        "intake" => intake(&git, &cli)?,
        "scan" => scan_command(&git, &cli)?,
        "import" => import(&git, &cli)?,
        "notices" => notices_command(&git, &cli)?,
        "audit" => audit(&git, &cli)?,
        other => return Err(format!("unknown command `{other}`\n{USAGE}")),
    };
    let body = if cli.switch("json") {
        serde_json::to_string_pretty(&report.json).map_err(|e| e.to_string())?
    } else {
        report.text
    };
    if report.ok {
        Ok(body)
    } else {
        Err(body)
    }
}

struct Report {
    ok: bool,
    text: String,
    json: Value,
}

// ---------------------------------------------------------------- CLI

struct Cli {
    command: String,
    values: BTreeMap<String, Vec<String>>,
    switches: BTreeSet<String>,
}

impl Cli {
    fn all(&self, key: &str) -> Vec<String> {
        self.values.get(key).cloned().unwrap_or_default()
    }

    fn optional(&self, key: &str) -> Result<Option<String>> {
        match self.values.get(key).map(Vec::as_slice) {
            None | Some([]) => Ok(None),
            Some([one]) => Ok(Some(one.clone())),
            Some(_) => Err(format!("--{key} may be given only once")),
        }
    }

    fn required(&self, key: &str) -> Result<String> {
        self.optional(key)?
            .ok_or_else(|| format!("`{}` needs --{key}\n{USAGE}", self.command))
    }

    fn switch(&self, key: &str) -> bool {
        self.switches.contains(key)
    }

    fn only(&self, allowed: &[&str]) -> Result<()> {
        let used = self.values.keys().chain(self.switches.iter());
        for key in used {
            if key != "json" && !allowed.contains(&key.as_str()) {
                return Err(format!("`{}` does not take --{key}", self.command));
            }
        }
        Ok(())
    }
}

fn parse_cli(args: &[String]) -> Result<Cli> {
    let mut iter = args.iter();
    let command = iter.next().cloned().ok_or_else(|| USAGE.to_owned())?;
    let mut values: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut switches = BTreeSet::new();
    while let Some(arg) = iter.next() {
        let Some(flag) = arg.strip_prefix("--") else {
            return Err(format!("unexpected argument `{arg}`\n{USAGE}"));
        };
        let (name, inline) = match flag.split_once('=') {
            Some((name, value)) => (name, Some(value.to_owned())),
            None => (flag, None),
        };
        if SWITCHES.contains(&name) {
            if inline.is_some() {
                return Err(format!("--{name} takes no value"));
            }
            switches.insert(name.to_owned());
        } else if VALUE_FLAGS.contains(&name) {
            let value = match inline {
                Some(value) => value,
                None => iter
                    .next()
                    .cloned()
                    .ok_or_else(|| format!("--{name} needs a value"))?,
            };
            values.entry(name.to_owned()).or_default().push(value);
        } else {
            return Err(format!("unknown option --{name}\n{USAGE}"));
        }
    }
    Ok(Cli {
        command,
        values,
        switches,
    })
}

// ---------------------------------------------------------------- Git

struct Git {
    root: PathBuf,
}

impl Git {
    fn open(cwd: &Path) -> Result<Self> {
        let probe = Git {
            root: cwd.to_path_buf(),
        };
        let top = probe
            .text(&["rev-parse", "--show-toplevel"])
            .map_err(|e| format!("not inside a Git worktree: {e}"))?;
        Ok(Git {
            root: PathBuf::from(top),
        })
    }

    fn output(&self, args: &[&str], input: Option<&[u8]>) -> Result<Output> {
        let mut command = Command::new("git");
        command
            .current_dir(&self.root)
            .arg("--literal-pathspecs")
            .arg("--no-replace-objects")
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            });
        for key in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_COMMON_DIR",
            "GIT_INDEX_FILE",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "GIT_GRAFT_FILE",
        ] {
            command.env_remove(key);
        }
        #[cfg(test)]
        tests::isolate_git_env(&mut command, &self.root);
        let mut child = command
            .spawn()
            .map_err(|e| format!("cannot start git: {e}"))?;
        if let Some(bytes) = input {
            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| "git stdin unavailable".to_owned())?;
            return std::thread::scope(|scope| {
                let writer = scope.spawn(move || stdin.write_all(bytes));
                let out = child
                    .wait_with_output()
                    .map_err(|e| format!("cannot wait for git: {e}"))?;
                let written = writer
                    .join()
                    .map_err(|_| "git stdin writer panicked".to_owned())?;
                if out.status.success() {
                    written.map_err(|e| format!("cannot write git input: {e}"))?;
                }
                Ok(out)
            });
        }
        child
            .wait_with_output()
            .map_err(|e| format!("cannot wait for git: {e}"))
    }

    fn checked(&self, args: &[&str], input: Option<&[u8]>) -> Result<Vec<u8>> {
        let out = self.output(args, input)?;
        if out.status.success() {
            return Ok(out.stdout);
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        let detail = stderr
            .lines()
            .map(str::trim)
            .rfind(|line| !line.is_empty())
            .unwrap_or("no diagnostic");
        Err(format!(
            "`git {}` failed ({}): {detail}",
            args.join(" "),
            out.status
        ))
    }

    fn text(&self, args: &[&str]) -> Result<String> {
        let bytes = self.checked(args, None)?;
        String::from_utf8(bytes)
            .map(|s| s.trim().to_owned())
            .map_err(|_| "git returned non-UTF-8 output".to_owned())
    }

    fn try_text(&self, args: &[&str]) -> Result<Option<String>> {
        let out = self.output(args, None)?;
        if !out.status.success() {
            return Ok(None);
        }
        Ok(Some(String::from_utf8_lossy(&out.stdout).trim().to_owned()))
    }

    fn common_dir(&self) -> Result<PathBuf> {
        self.text(&["rev-parse", "--path-format=absolute", "--git-common-dir"])
            .map(PathBuf::from)
    }

    fn has_commit(&self, oid: &str) -> Result<bool> {
        let out = self.output(&["cat-file", "-e", &format!("{oid}^{{commit}}")], None)?;
        Ok(out.status.success())
    }
}

#[derive(Clone)]
struct TreeEntry {
    mode: String,
    kind: String,
    oid: String,
}

fn list_tree(git: &Git, commit: &str) -> Result<BTreeMap<String, TreeEntry>> {
    let raw = git.checked(&["ls-tree", "-r", "-z", "--full-tree", commit], None)?;
    let mut tree = BTreeMap::new();
    for record in raw.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let line = std::str::from_utf8(record)
            .map_err(|_| "upstream tree has a non-UTF-8 path; refusing".to_owned())?;
        let (meta, path) = line
            .split_once('\t')
            .ok_or_else(|| format!("unexpected ls-tree record `{line}`"))?;
        let mut parts = meta.split(' ');
        let (Some(mode), Some(kind), Some(oid)) = (parts.next(), parts.next(), parts.next()) else {
            return Err(format!("unexpected ls-tree record `{line}`"));
        };
        tree.insert(
            path.to_owned(),
            TreeEntry {
                mode: mode.to_owned(),
                kind: kind.to_owned(),
                oid: oid.to_owned(),
            },
        );
    }
    Ok(tree)
}

fn read_blobs(git: &Git, oids: &BTreeSet<String>) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut blobs = BTreeMap::new();
    if oids.is_empty() {
        return Ok(blobs);
    }
    let input: String = oids.iter().map(|oid| format!("{oid}\n")).collect();
    let out = git.checked(&["cat-file", "--batch"], Some(input.as_bytes()))?;
    let mut pos = 0;
    for oid in oids {
        let rest = out.get(pos..).unwrap_or_default();
        let newline = rest
            .iter()
            .position(|b| *b == b'\n')
            .ok_or_else(|| "truncated cat-file output".to_owned())?;
        let header = String::from_utf8_lossy(&rest[..newline]).into_owned();
        let parts: Vec<&str> = header.split(' ').collect();
        if parts.len() != 3 || parts[0] != oid || parts[1] != "blob" {
            return Err(format!("cannot read blob {oid}: `{header}`"));
        }
        let size: usize = parts[2]
            .parse()
            .map_err(|_| format!("bad blob size in `{header}`"))?;
        let start = pos + newline + 1;
        let body = out
            .get(start..start + size)
            .ok_or_else(|| "truncated cat-file output".to_owned())?;
        blobs.insert(oid.clone(), body.to_vec());
        pos = start + size + 1;
    }
    Ok(blobs)
}

/// Git blob ids of worktree files, with (`filters`) and without clean filters.
fn hash_paths(git: &Git, paths: &[String], filters: bool) -> Result<Vec<String>> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let input: String = paths.iter().map(|p| format!("{p}\n")).collect();
    let mut args = vec!["hash-object", "--stdin-paths"];
    if !filters {
        args.push("--no-filters");
    }
    let out = git.checked(&args, Some(input.as_bytes()))?;
    let ids: Vec<String> = String::from_utf8_lossy(&out)
        .lines()
        .map(str::to_owned)
        .collect();
    if ids.len() != paths.len() {
        return Err("git hash-object returned an unexpected number of ids".to_owned());
    }
    Ok(ids)
}

// ---------------------------------------------------------------- validation

fn is_hex_oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn check_slug(slug: &str) -> Result<()> {
    let ok = !slug.is_empty()
        && slug.len() <= 64
        && slug.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
        && !slug.starts_with(['.', '-'])
        && !slug.ends_with('.')
        && !slug.ends_with(".lock")
        && !slug.contains("..");
    if ok {
        Ok(())
    } else {
        Err(format!(
            "invalid slug `{slug}`: use 1-64 of [a-z0-9._-], not starting with `.` or `-`"
        ))
    }
}

fn check_labelled_id(id: &str, prefix: &str) -> Result<()> {
    let ok = id
        .strip_prefix(prefix)
        .and_then(|rest| rest.rsplit_once('-'))
        .is_some_and(|(agent, n)| {
            !agent.is_empty()
                && agent
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
                && !n.is_empty()
                && n.bytes().all(|b| b.is_ascii_digit())
        });
    if ok {
        Ok(())
    } else {
        Err(format!("invalid id `{id}`: expected {prefix}<AGENT>-<n>"))
    }
}

fn check_feature(feature: &str) -> Result<()> {
    let ok = !feature.is_empty()
        && feature.len() <= 128
        && feature
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    if ok {
        Ok(())
    } else {
        Err(format!("invalid feature id `{feature}`"))
    }
}

fn check_url(url: &str) -> Result<()> {
    let ok = !url.is_empty()
        && url.len() <= 2048
        && !url.starts_with('-')
        && !url.chars().any(char::is_control)
        && !url.to_ascii_lowercase().starts_with("ext::");
    if ok {
        Ok(())
    } else {
        Err(format!("refusing Git URL `{url}`"))
    }
}

fn check_rev(rev: &str) -> Result<()> {
    let ok = !rev.is_empty()
        && !rev.starts_with(['-', '+'])
        && !rev
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || ":*^~?[\\".contains(c));
    if ok {
        Ok(())
    } else {
        Err(format!(
            "invalid --rev `{rev}`: give a full commit id or a plain ref name"
        ))
    }
}

/// Normalizes a repository-relative path. `dest` paths are additionally
/// confined: no drive letters, no `.git` component.
fn normalize_rel(raw: &str, dest: bool) -> Result<String> {
    let slashed = raw.replace('\\', "/");
    let path = slashed.trim_end_matches('/');
    if path.is_empty() || path == "." {
        return if dest {
            Err("destination path is empty".to_owned())
        } else {
            Ok(String::new())
        };
    }
    let bad = path.starts_with('/')
        || path.chars().any(char::is_control)
        || (dest && path.contains(':'))
        || path.split('/').any(|c| {
            c.is_empty() || c == "." || c == ".." || (dest && c.eq_ignore_ascii_case(".git"))
        });
    if bad {
        return Err(format!(
            "path `{raw}` must be a confined relative path without `..`"
        ));
    }
    Ok(path.to_owned())
}

fn join_rel(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_owned()
    } else {
        format!("{dir}/{name}")
    }
}

/// Directories from the file's parent up to the repository root (`""`).
fn ancestor_dirs(path: &str) -> Vec<String> {
    let mut dirs = Vec::new();
    let mut current = path;
    while let Some((parent, _)) = current.rsplit_once('/') {
        dirs.push(parent.to_owned());
        current = parent;
    }
    dirs.push(String::new());
    dirs
}

// ---------------------------------------------------------------- license logic

/// Lowercase alphanumeric words separated by single spaces, so phrase checks
/// survive line wrapping and comment leaders.
fn prose(text: &str) -> String {
    let mapped: String = text
        .chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect();
    mapped.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn rejected_phrases(text: &str) -> Vec<&'static str> {
    let words = format!(" {} ", prose(text));
    REJECT_PHRASES
        .iter()
        .copied()
        .filter(|phrase| words.contains(&format!(" {} ", prose(phrase))))
        .collect()
}

fn classify_license_text(text: &str) -> Option<&'static str> {
    if !rejected_phrases(text).is_empty() {
        return None;
    }
    let words = prose(text);
    if words.contains("apache license") && words.contains("version 2 0 january 2004") {
        Some("Apache-2.0")
    } else if words.contains("permission is hereby granted free of charge") {
        Some("MIT")
    } else if words.contains("redistribution and use in source and binary forms") {
        if words.contains("neither the name") {
            Some("BSD-3-Clause")
        } else {
            Some("BSD-2-Clause")
        }
    } else {
        None
    }
}

fn spdx_header(text: &str) -> Option<String> {
    const TAG: &str = "SPDX-License-Identifier:";
    for line in text.lines().take(HEADER_LINES) {
        let Some(index) = line.find(TAG) else {
            continue;
        };
        let rest = line[index + TAG.len()..].trim_start();
        let expr: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || " .+-()".contains(*c))
            .collect();
        let expr = expr
            .trim_end_matches(|c: char| c == '-' || c.is_whitespace())
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if !expr.is_empty() {
            return Some(expr);
        }
    }
    None
}

fn tokenize(expr: &str) -> Vec<String> {
    expr.replace('(', " ( ")
        .replace(')', " ) ")
        .split_whitespace()
        .map(str::to_owned)
        .collect()
}

fn strip_outer_parens(mut tokens: &[String]) -> &[String] {
    loop {
        let wrapped = tokens.len() >= 2 && tokens[0] == "(" && tokens[tokens.len() - 1] == ")" && {
            let mut depth = 0i32;
            let mut closes_at_end = true;
            for (i, t) in tokens.iter().enumerate() {
                if t == "(" {
                    depth += 1;
                } else if t == ")" {
                    depth -= 1;
                    if depth == 0 && i + 1 != tokens.len() {
                        closes_at_end = false;
                    }
                }
            }
            closes_at_end && depth == 0
        };
        if !wrapped {
            return tokens;
        }
        tokens = &tokens[1..tokens.len() - 1];
    }
}

fn split_top<'a>(tokens: &'a [String], op: &str) -> Vec<&'a [String]> {
    let tokens = strip_outer_parens(tokens);
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    for (i, t) in tokens.iter().enumerate() {
        if t == "(" {
            depth += 1;
        } else if t == ")" {
            depth -= 1;
        } else if depth == 0 && t.eq_ignore_ascii_case(op) {
            parts.push(strip_outer_parens(&tokens[start..i]));
            start = i + 1;
        }
    }
    parts.push(strip_outer_parens(&tokens[start..]));
    parts
}

fn canonical_license(tokens: &[String]) -> Option<&'static str> {
    let term = tokens.join(" ");
    ALLOWED_LICENSES
        .iter()
        .copied()
        .find(|allowed| allowed.eq_ignore_ascii_case(&term))
}

/// Picks the license Susi uses from an SPDX expression: the `OR` alternative
/// that is exactly Apache-2.0. Anything else is refused.
fn choose_license(expr: &str) -> std::result::Result<String, String> {
    let tokens = tokenize(expr);
    let mut allowed = Vec::new();
    for alternative in split_top(&tokens, "OR") {
        let terms: Option<Vec<&str>> = split_top(alternative, "AND")
            .into_iter()
            .map(canonical_license)
            .collect();
        if let Some(terms) = terms.filter(|t| !t.is_empty()) {
            allowed.push(terms.join(" AND "));
        }
    }
    allowed
        .iter()
        .find(|a| *a == "Apache-2.0")
        .cloned()
        .ok_or_else(|| {
            format!("license `{expr}` is not Apache-2.0; only Apache-2.0 code is harvested")
        })
}

fn strip_comment(line: &str) -> &str {
    let mut s = line
        .trim()
        .trim_start_matches(|c: char| "/*#;!%-'\"<>".contains(c) || c.is_whitespace());
    loop {
        let before = s.len();
        s = s.trim_end();
        for suffix in ["*/", "-->", "*)"] {
            s = s.strip_suffix(suffix).unwrap_or(s);
        }
        if s.len() == before {
            return s;
        }
    }
}

fn is_copyright_line(line: &str) -> bool {
    let rest = if let Some(rest) = line.strip_prefix('©') {
        rest
    } else {
        match line.get(..9) {
            Some(word) if word.eq_ignore_ascii_case("copyright") => &line[9..],
            _ => return false,
        }
    };
    if !rest.is_empty() && !rest.starts_with([' ', '\t', ':', '(', '©']) {
        return false;
    }
    let mut body = rest.trim_start_matches([' ', '\t', ':']);
    loop {
        let before = body.len();
        for mark in ["(c)", "(C)", "©"] {
            body = body.strip_prefix(mark).unwrap_or(body);
        }
        body = body.trim_start();
        if body.len() == before {
            break;
        }
    }
    body.chars()
        .next()
        .is_some_and(|c| c.is_ascii_digit() || c.is_uppercase())
}

fn copyright_lines(text: &str) -> Vec<String> {
    let set: BTreeSet<String> = text
        .lines()
        .map(strip_comment)
        .filter(|line| is_copyright_line(line))
        .map(str::to_owned)
        .collect();
    set.into_iter().collect()
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

// ---------------------------------------------------------------- pins

struct Pin {
    slug: String,
    commit: String,
    reference: String,
}

fn quarantine_ref(slug: &str, commit: &str) -> String {
    format!("{QUARANTINE_PREFIX}/{slug}/{commit}")
}

fn intake_record_path(git: &Git, slug: &str, commit: &str) -> Result<PathBuf> {
    Ok(git
        .common_dir()?
        .join(INTAKE_DIR)
        .join(slug)
        .join(format!("{commit}.url")))
}

fn resolve_pin(git: &Git, cli: &Cli) -> Result<Pin> {
    let slug = cli.required("slug")?;
    check_slug(&slug)?;
    let rev = cli.required("rev")?.to_ascii_lowercase();
    if !is_hex_oid(&rev) {
        return Err(format!(
            "--rev must be the full pinned commit id printed by intake, not `{rev}`"
        ));
    }
    let reference = quarantine_ref(&slug, &rev);
    let actual = git
        .try_text(&[
            "rev-parse",
            "--verify",
            "-q",
            "--end-of-options",
            &format!("{reference}^{{commit}}"),
        ])?
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            format!("no quarantine ref {reference}; run `local-harvest intake` first")
        })?;
    if actual != rev {
        return Err(format!("{reference} points to {actual}, not {rev}"));
    }
    Ok(Pin {
        slug,
        commit: rev,
        reference,
    })
}

// ---------------------------------------------------------------- intake

fn intake(git: &Git, cli: &Cli) -> Result<Report> {
    cli.only(&["url", "rev", "slug"])?;
    let url = cli.required("url")?;
    check_url(&url)?;
    let rev = cli.required("rev")?;
    check_rev(&rev)?;
    let slug = cli.required("slug")?;
    check_slug(&slug)?;
    git.checked(
        &[
            "-c",
            "protocol.ext.allow=never",
            "-c",
            "fetch.recurseSubmodules=false",
            "fetch",
            "--quiet",
            "--no-tags",
            "--no-recurse-submodules",
            &url,
            &rev,
        ],
        None,
    )?;
    let fetched = git.text(&[
        "rev-parse",
        "--verify",
        "--end-of-options",
        "FETCH_HEAD^{commit}",
    ])?;
    let wanted = rev.to_ascii_lowercase();
    if is_hex_oid(&wanted) && fetched != wanted {
        return Err(format!("fetched {fetched}, but --rev asked for {wanted}"));
    }
    let reference = quarantine_ref(&slug, &fetched);
    let existing = git.try_text(&["rev-parse", "--verify", "-q", &reference])?;
    let created = match existing.filter(|s| !s.is_empty()) {
        Some(current) if current == fetched => false,
        Some(current) => {
            return Err(format!("{reference} already exists at {current}; refusing"));
        }
        None => {
            let zero = "0".repeat(fetched.len());
            git.checked(
                &[
                    "update-ref",
                    "-m",
                    "local-harvest intake",
                    &reference,
                    &fetched,
                    &zero,
                ],
                None,
            )?;
            true
        }
    };
    let record = intake_record_path(git, &slug, &fetched)?;
    if let Some(parent) = record.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("cannot create {parent:?}: {e}"))?;
    }
    fs::write(&record, format!("{url}\n")).map_err(|e| format!("cannot write {record:?}: {e}"))?;
    let state = if created { "pinned" } else { "already pinned" };
    let text = format!(
        "{state} {fetched}\nquarantine ref: {reference}\nsource: {url} ({rev})\nnext: local-harvest scan --slug {slug} --rev {fetched} --path <upstream-path>"
    );
    Ok(Report {
        ok: true,
        text,
        json: json!({
            "ok": true,
            "commit": fetched,
            "quarantine_ref": reference,
            "url": url,
            "rev": rev,
            "created": created,
        }),
    })
}

fn read_intake_url(git: &Git, pin: &Pin) -> Result<Option<String>> {
    let path = intake_record_path(git, &pin.slug, &pin.commit)?;
    Ok(fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty()))
}

// ---------------------------------------------------------------- scan

struct Evidence {
    path: String,
    oid: String,
    kind: &'static str,
    spdx: Option<&'static str>,
}

struct FileScan {
    path: String,
    oid: String,
    license: Option<String>,
    expression: String,
    basis: String,
    license_files: Vec<String>,
    notice_files: Vec<String>,
    copyright: Vec<String>,
    reasons: Vec<String>,
}

struct Scan {
    commit: String,
    reference: String,
    files: Vec<FileScan>,
    evidence: BTreeMap<String, Evidence>,
    blobs: BTreeMap<String, Vec<u8>>,
}

impl Scan {
    fn rejected(&self) -> usize {
        self.files.iter().filter(|f| !f.reasons.is_empty()).count()
    }
}

fn nearest_named(tree: &BTreeMap<String, TreeEntry>, file: &str, names: &[&str]) -> Vec<String> {
    for dir in ancestor_dirs(file) {
        let found = files_in_dir(tree, &dir, names);
        if !found.is_empty() {
            return found;
        }
    }
    Vec::new()
}

fn files_in_dir(tree: &BTreeMap<String, TreeEntry>, dir: &str, names: &[&str]) -> Vec<String> {
    names
        .iter()
        .map(|name| join_rel(dir, name))
        .filter(|p| tree.get(p).is_some_and(|e| e.kind == "blob"))
        .collect()
}

fn select_files(tree: &BTreeMap<String, TreeEntry>, path: &str) -> Result<Vec<String>> {
    if path.is_empty() {
        return Ok(tree.keys().cloned().collect());
    }
    if tree.contains_key(path) {
        return Ok(vec![path.to_owned()]);
    }
    let prefix = format!("{path}/");
    let files: Vec<String> = tree
        .range(prefix.clone()..)
        .take_while(|(k, _)| k.starts_with(&prefix))
        .map(|(k, _)| k.clone())
        .collect();
    if files.is_empty() {
        Err(format!(
            "upstream path `{path}` does not exist at the pinned commit"
        ))
    } else {
        Ok(files)
    }
}

fn license_file_expression(
    files: &[String],
    evidence: &BTreeMap<String, Evidence>,
) -> std::result::Result<String, String> {
    if files.is_empty() {
        return Err("no SPDX header and no license file in any ancestor directory".to_owned());
    }
    let mut ids = BTreeSet::new();
    for file in files {
        match evidence.get(file).and_then(|e| e.spdx) {
            Some(id) => {
                ids.insert(id);
            }
            None => return Err(format!("unrecognized or disallowed license text in {file}")),
        }
    }
    let names: BTreeSet<&str> = files
        .iter()
        .map(|f| f.rsplit('/').next().unwrap_or(f))
        .collect();
    let ids: Vec<&str> = ids.into_iter().collect();
    if ids.len() == 1 {
        Ok(ids[0].to_owned())
    } else if names == BTreeSet::from(["LICENSE-APACHE", "LICENSE-MIT"]) {
        Ok("Apache-2.0 OR MIT".to_owned())
    } else {
        Ok(ids.join(" AND "))
    }
}

fn scan(git: &Git, commit: &str, reference: &str, paths: &[String]) -> Result<Scan> {
    let tree = list_tree(git, commit)?;
    let mut selected = BTreeSet::new();
    for path in paths {
        selected.extend(select_files(&tree, path)?);
    }
    let root_notices = files_in_dir(&tree, "", &NOTICE_NAMES);
    let mut plans = Vec::new();
    let mut evidence: BTreeMap<String, Evidence> = BTreeMap::new();
    for path in &selected {
        let license_files = nearest_named(&tree, path, &LICENSE_NAMES);
        let mut notice_files: BTreeSet<String> = nearest_named(&tree, path, &NOTICE_NAMES)
            .into_iter()
            .collect();
        notice_files.extend(root_notices.iter().cloned());
        for (file, kind) in license_files
            .iter()
            .map(|f| (f, "license"))
            .chain(notice_files.iter().map(|f| (f, "notice")))
        {
            if let Some(entry) = tree.get(file) {
                evidence.entry(file.clone()).or_insert(Evidence {
                    path: file.clone(),
                    oid: entry.oid.clone(),
                    kind,
                    spdx: None,
                });
            }
        }
        plans.push((
            path.clone(),
            license_files,
            notice_files.into_iter().collect(),
        ));
    }
    let mut oids: BTreeSet<String> = evidence.values().map(|e| e.oid.clone()).collect();
    for path in &selected {
        if let Some(entry) = tree.get(path).filter(|e| e.kind == "blob") {
            oids.insert(entry.oid.clone());
        }
    }
    let blobs = read_blobs(git, &oids)?;
    for item in evidence.values_mut() {
        if item.kind == "license" {
            item.spdx = blobs
                .get(&item.oid)
                .and_then(|b| classify_license_text(&lossy(b)));
        }
    }
    let files = plans
        .into_iter()
        .map(|(path, license_files, notice_files)| {
            judge_file(
                &tree,
                &blobs,
                &evidence,
                (path, license_files, notice_files),
            )
        })
        .collect();
    Ok(Scan {
        commit: commit.to_owned(),
        reference: reference.to_owned(),
        files,
        evidence,
        blobs,
    })
}

fn judge_file(
    tree: &BTreeMap<String, TreeEntry>,
    blobs: &BTreeMap<String, Vec<u8>>,
    evidence: &BTreeMap<String, Evidence>,
    (path, license_files, notice_files): (String, Vec<String>, Vec<String>),
) -> FileScan {
    let mut reasons = Vec::new();
    let entry = tree.get(&path).cloned().unwrap_or(TreeEntry {
        mode: String::new(),
        kind: String::new(),
        oid: String::new(),
    });
    if entry.kind != "blob" {
        reasons.push("submodule or non-file entry cannot be harvested".to_owned());
    } else if entry.mode == "120000" {
        reasons.push("symbolic link cannot be harvested".to_owned());
    }
    let text = blobs.get(&entry.oid).map(|b| lossy(b)).unwrap_or_default();
    let (expression, basis) = match spdx_header(&text) {
        Some(expr) => (Ok(expr), "SPDX header".to_owned()),
        None => (
            license_file_expression(&license_files, evidence),
            license_files.join(", "),
        ),
    };
    let (expression, license) = match expression {
        Ok(expr) => match choose_license(&expr) {
            Ok(chosen) => (expr, Some(chosen)),
            Err(reason) => {
                reasons.push(reason);
                (expr, None)
            }
        },
        Err(reason) => {
            reasons.push(reason);
            ("NONE".to_owned(), None)
        }
    };
    for phrase in rejected_phrases(&text) {
        reasons.push(format!("text mentions \"{phrase}\""));
    }
    FileScan {
        copyright: copyright_lines(&text),
        path,
        oid: entry.oid,
        license,
        expression,
        basis: if basis.is_empty() {
            "-".to_owned()
        } else {
            basis
        },
        license_files,
        notice_files,
        reasons,
    }
}

fn render_scan(scan: &Scan) -> String {
    let lic_w = scan
        .files
        .iter()
        .map(|f| f.license.as_deref().unwrap_or("-").len())
        .max()
        .unwrap_or(1)
        .max(7);
    let basis_w = scan
        .files
        .iter()
        .map(|f| f.basis.len())
        .max()
        .unwrap_or(1)
        .max(5);
    let mut out = format!(
        "license scan of {} ({})\n{:<7} {:<lic_w$} {:<basis_w$} PATH\n",
        scan.commit, scan.reference, "VERDICT", "LICENSE", "BASIS"
    );
    for f in &scan.files {
        let verdict = if f.reasons.is_empty() {
            "accept"
        } else {
            "REJECT"
        };
        out.push_str(&format!(
            "{verdict:<7} {:<lic_w$} {:<basis_w$} {}",
            f.license.as_deref().unwrap_or("-"),
            f.basis,
            f.path
        ));
        if !f.reasons.is_empty() {
            out.push_str(&format!("  [{}]", f.reasons.join("; ")));
        }
        out.push('\n');
    }
    let notices: Vec<&str> = scan
        .evidence
        .values()
        .filter(|e| e.kind == "notice")
        .map(|e| e.path.as_str())
        .collect();
    out.push_str(&format!(
        "NOTICE files: {}\n",
        if notices.is_empty() {
            "none".to_owned()
        } else {
            notices.join(", ")
        }
    ));
    let rejected = scan.rejected();
    out.push_str(&format!(
        "result: {} accepted, {rejected} rejected",
        scan.files.len() - rejected
    ));
    out
}

fn scan_json(scan: &Scan) -> Value {
    let files: Vec<Value> = scan
        .files
        .iter()
        .map(|f| {
            json!({
                "path": f.path,
                "blob_sha1": f.oid,
                "verdict": if f.reasons.is_empty() { "accept" } else { "reject" },
                "license": f.license,
                "expression": f.expression,
                "basis": f.basis,
                "license_files": f.license_files,
                "notice_files": f.notice_files,
                "copyright_lines": f.copyright,
                "reasons": f.reasons,
            })
        })
        .collect();
    let evidence: Vec<Value> = scan
        .evidence
        .values()
        .map(|e| json!({"path": e.path, "kind": e.kind, "spdx": e.spdx, "blob_sha1": e.oid}))
        .collect();
    json!({
        "ok": scan.rejected() == 0,
        "source_commit": scan.commit,
        "quarantine_ref": scan.reference,
        "files": files,
        "evidence": evidence,
        "accepted": scan.files.len() - scan.rejected(),
        "rejected": scan.rejected(),
    })
}

fn scan_command(git: &Git, cli: &Cli) -> Result<Report> {
    cli.only(&["slug", "rev", "path"])?;
    let pin = resolve_pin(git, cli)?;
    let paths = cli
        .all("path")
        .iter()
        .map(|p| normalize_rel(p, false))
        .collect::<Result<Vec<_>>>()?;
    if paths.is_empty() {
        return Err(format!("`scan` needs at least one --path\n{USAGE}"));
    }
    let scan = scan(git, &pin.commit, &pin.reference, &paths)?;
    Ok(Report {
        ok: scan.rejected() == 0,
        text: render_scan(&scan),
        json: scan_json(&scan),
    })
}

// ---------------------------------------------------------------- import

struct Planned {
    source_path: String,
    dest_path: String,
    oid: String,
    spdx: String,
    copyright: Vec<String>,
}

fn parse_maps(raw: &[String]) -> Result<Vec<(String, String)>> {
    if raw.is_empty() {
        return Err(format!("`import` needs at least one --map\n{USAGE}"));
    }
    raw.iter()
        .map(|m| {
            let (up, dest) = m
                .split_once('=')
                .ok_or_else(|| format!("--map `{m}` must be <upstream-path>=<dest-path>"))?;
            Ok((normalize_rel(up, false)?, normalize_rel(dest, true)?))
        })
        .collect()
}

fn plan_files(scan: &Scan, maps: &[(String, String)], port: bool) -> Result<Vec<Planned>> {
    let mut planned = Vec::new();
    let mut dests = BTreeSet::new();
    for (up, dest) in maps {
        for file in &scan.files {
            let relative = if up.is_empty() {
                Some(file.path.as_str())
            } else if file.path == *up {
                None
            } else if let Some(rest) = file.path.strip_prefix(&format!("{up}/")) {
                Some(rest)
            } else {
                continue;
            };
            if port && relative.is_some() {
                return Err(format!(
                    "port mode maps one upstream file to one destination; `{up}` is a directory"
                ));
            }
            let dest_path = match relative {
                Some(rest) => normalize_rel(&format!("{dest}/{rest}"), true)?,
                None => dest.clone(),
            };
            if dest_path == NOTICES_FILE
                || dest_path == APACHE_TEXT_PATH
                || dest_path.starts_with(&format!("{MANIFEST_DIR}/"))
            {
                return Err(format!("destination `{dest_path}` is reserved"));
            }
            if !port && !dests.insert(dest_path.clone()) {
                return Err(format!("two upstream files map to `{dest_path}`"));
            }
            planned.push(Planned {
                source_path: file.path.clone(),
                dest_path,
                oid: file.oid.clone(),
                spdx: file.license.clone().unwrap_or_default(),
                copyright: file.copyright.clone(),
            });
        }
    }
    planned.sort_by(|a, b| (&a.dest_path, &a.source_path).cmp(&(&b.dest_path, &b.source_path)));
    Ok(planned)
}

/// Refuses symlinked or file-shadowed parents and, without `overwrite`,
/// existing destination files.
fn check_dest(root: &Path, dest: &str, overwrite: bool) -> Result<()> {
    let parts: Vec<&str> = dest.split('/').collect();
    let mut current = root.to_path_buf();
    for (i, part) in parts.iter().enumerate() {
        current.push(part);
        let Ok(meta) = fs::symlink_metadata(&current) else {
            return Ok(());
        };
        let last = i + 1 == parts.len();
        if meta.file_type().is_symlink() {
            return Err(format!("`{dest}` passes through a symbolic link; refusing"));
        }
        if !last && !meta.is_dir() {
            return Err(format!("a parent of `{dest}` is not a directory"));
        }
        if last && meta.is_dir() {
            return Err(format!("`{dest}` is a directory"));
        }
        if last && !overwrite {
            return Err(format!(
                "`{dest}` already exists; pass --overwrite to replace it"
            ));
        }
    }
    Ok(())
}

fn write_file(root: &Path, rel: &str, bytes: &[u8]) -> Result<()> {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("cannot create {parent:?}: {e}"))?;
    }
    fs::write(&path, bytes).map_err(|e| format!("cannot write {rel}: {e}"))
}

fn combined_license(planned: &[Planned]) -> String {
    let set: BTreeSet<&str> = planned.iter().map(|p| p.spdx.as_str()).collect();
    set.into_iter().collect::<Vec<_>>().join(" AND ")
}

fn apache_text(scan: &Scan, root: &Path) -> Result<Vec<u8>> {
    let mut candidates: Vec<&Evidence> = scan
        .evidence
        .values()
        .filter(|e| e.kind == "license" && e.spdx == Some("Apache-2.0"))
        .collect();
    candidates.sort_by_key(|e| (e.path.matches('/').count(), e.path.clone()));
    if let Some(bytes) = candidates.first().and_then(|e| scan.blobs.get(&e.oid)) {
        return Ok(bytes.clone());
    }
    let own = fs::read(root.join("LICENSE")).unwrap_or_default();
    if classify_license_text(&lossy(&own)) == Some("Apache-2.0") {
        return Ok(own);
    }
    Err(format!(
        "Apache-2.0 code needs {APACHE_TEXT_PATH}, but no Apache-2.0 license text was found upstream or in ./LICENSE"
    ))
}

fn import(git: &Git, cli: &Cli) -> Result<Report> {
    cli.only(&[
        "slug",
        "rev",
        "map",
        "id",
        "task",
        "feature",
        "mode",
        "agent",
        "summary",
        "url",
        "overwrite",
    ])?;
    let pin = resolve_pin(git, cli)?;
    let id = cli.required("id")?;
    check_labelled_id(&id, "H-")?;
    let task = cli.required("task")?;
    check_labelled_id(&task, "T-")?;
    let feature = cli.optional("feature")?;
    if let Some(feature) = &feature {
        check_feature(feature)?;
    }
    let mode = cli
        .optional("mode")?
        .unwrap_or_else(|| "transplant".to_owned());
    let port = match mode.as_str() {
        "transplant" => false,
        "port" => true,
        other => return Err(format!("--mode must be transplant or port, not `{other}`")),
    };
    let overwrite = cli.switch("overwrite");
    let maps = parse_maps(&cli.all("map"))?;
    let url = match cli.optional("url")? {
        Some(url) => {
            check_url(&url)?;
            url
        }
        None => read_intake_url(git, &pin)?.ok_or_else(|| {
            format!(
                "no intake record for {}; re-run intake or pass --url",
                pin.reference
            )
        })?,
    };
    let root = git.root.clone();
    let manifest_rel = format!("{MANIFEST_DIR}/{id}.json");
    if root.join(&manifest_rel).exists() && !overwrite {
        return Err(format!(
            "{manifest_rel} already exists; pass --overwrite to replace it"
        ));
    }

    let upstream_paths: Vec<String> = maps.iter().map(|(up, _)| up.clone()).collect();
    let scan = scan(git, &pin.commit, &pin.reference, &upstream_paths)?;
    if scan.rejected() > 0 {
        return Ok(Report {
            ok: false,
            text: format!(
                "import refused: license scan rejected {} file(s)\n{}",
                scan.rejected(),
                render_scan(&scan)
            ),
            json: scan_json(&scan),
        });
    }
    let planned = plan_files(&scan, &maps, port)?;
    if !port {
        for p in &planned {
            check_dest(&root, &p.dest_path, overwrite)?;
        }
    }
    let license = combined_license(&planned);
    let needs_apache_text = license.contains("Apache-2.0") && !root.join(APACHE_TEXT_PATH).exists();
    let apache = if needs_apache_text {
        Some(apache_text(&scan, &root)?)
    } else {
        None
    };

    let manifest = build_manifest(
        &scan,
        &planned,
        &ManifestMeta {
            id: id.clone(),
            task: task.clone(),
            feature: feature.clone(),
            url: url.clone(),
            mode: mode.clone(),
            license: license.clone(),
            summary: cli.optional("summary")?.unwrap_or_default(),
            agent: harvested_by(git, cli)?,
        },
    );
    let manifest_text = serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?;

    if !port {
        for p in &planned {
            let bytes = scan
                .blobs
                .get(&p.oid)
                .ok_or_else(|| format!("blob for {} missing", p.source_path))?;
            write_file(&root, &p.dest_path, bytes)?;
        }
    }
    if let Some(bytes) = &apache {
        write_file(&root, APACHE_TEXT_PATH, bytes)?;
    }
    write_file(
        &root,
        &manifest_rel,
        format!("{manifest_text}\n").as_bytes(),
    )?;
    let notices = write_notices(&root)?;

    let mut trailers = vec![format!("Harvest: {id}")];
    trailers.extend(
        maps.iter()
            .map(|(up, _)| format!("Origin: {url}@{}:{up}", pin.commit)),
    );
    trailers.push(format!("Origin-License: {license}"));
    trailers.push(format!("Task: {task}"));
    if let Some(feature) = &feature {
        trailers.push(format!("Feature: {feature}"));
    }
    let mut text = format!(
        "{} {id}: {} file(s) from {url}@{} (license {license}, mode {mode})\n",
        if port { "recorded port" } else { "imported" },
        planned.len(),
        pin.commit
    );
    for p in &planned {
        text.push_str(&format!("  {} -> {}\n", p.source_path, p.dest_path));
    }
    if port {
        text.push_str("port mode: no upstream files written; write the Rust port at the destination paths, keep the copyright lines and add a `Modified by` notice\n");
    }
    text.push_str(&format!("wrote {manifest_rel}\n"));
    if apache.is_some() {
        text.push_str(&format!("wrote {APACHE_TEXT_PATH}\n"));
    }
    text.push_str(&format!("{NOTICES_FILE}: {notices}\n"));
    text.push_str("not committed; commit with these trailers:\n\n");
    text.push_str(&trailers.join("\n"));
    Ok(Report {
        ok: true,
        text,
        json: json!({
            "ok": true,
            "id": id,
            "manifest": manifest_rel,
            "license": license,
            "mode": mode,
            "files": planned.iter().map(|p| json!({"source_path": p.source_path, "dest_path": p.dest_path})).collect::<Vec<_>>(),
            "wrote_apache_text": apache.is_some(),
            "notices": notices,
            "trailers": trailers,
        }),
    })
}

struct ManifestMeta {
    id: String,
    task: String,
    feature: Option<String>,
    url: String,
    mode: String,
    license: String,
    summary: String,
    agent: String,
}

fn harvested_by(git: &Git, cli: &Cli) -> Result<String> {
    if let Some(agent) = cli.optional("agent")? {
        return Ok(agent);
    }
    Ok(git
        .try_text(&["config", "user.name"])?
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_owned()))
}

fn build_manifest(scan: &Scan, planned: &[Planned], meta: &ManifestMeta) -> Value {
    let harvested: BTreeSet<&str> = planned.iter().map(|p| p.source_path.as_str()).collect();
    let mut evidence_paths = BTreeSet::new();
    let mut copyright: BTreeSet<String> = BTreeSet::new();
    for file in scan
        .files
        .iter()
        .filter(|f| harvested.contains(f.path.as_str()))
    {
        evidence_paths.extend(file.license_files.iter().cloned());
        evidence_paths.extend(file.notice_files.iter().cloned());
        copyright.extend(file.copyright.iter().cloned());
    }
    let mut evidence = Vec::new();
    let mut notice_parts = Vec::new();
    let mut license_texts = Vec::new();
    for item in evidence_paths.iter().filter_map(|p| scan.evidence.get(p)) {
        let bytes = scan.blobs.get(&item.oid).cloned().unwrap_or_default();
        let text = lossy(&bytes);
        if item.kind == "license" {
            copyright.extend(copyright_lines(&text));
            if let Some(spdx) = item.spdx.filter(|s| *s != "Apache-2.0") {
                license_texts
                    .push(json!({"path": item.path, "spdx": spdx, "text": text.trim_end()}));
            }
        } else {
            notice_parts.push(format!("--- {} ---\n{}", item.path, text.trim_end()));
        }
        evidence.push(json!({
            "path": item.path,
            "kind": item.kind,
            "spdx": item.spdx,
            "blob_sha1": item.oid,
            "sha256": sha256_hex(&bytes),
        }));
    }
    let files: Vec<Value> = planned
        .iter()
        .map(|p| {
            json!({
                "source_path": p.source_path,
                "dest_path": p.dest_path,
                "source_blob_sha1": p.oid,
                "spdx": p.spdx,
                "copyright_lines": p.copyright,
            })
        })
        .collect();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    json!({
        "schema": MANIFEST_SCHEMA,
        "id": meta.id,
        "task": meta.task,
        "feature": meta.feature,
        "source_url": meta.url,
        "source_commit": scan.commit,
        "quarantine_ref": scan.reference,
        "mode": meta.mode,
        "license": meta.license,
        "license_evidence": evidence,
        "license_texts": license_texts,
        "notice_text": notice_parts.join("\n\n"),
        "copyright_lines": copyright.into_iter().collect::<Vec<_>>(),
        "modifications": meta.summary,
        "files": files,
        "harvested_by": meta.agent,
        "harvested_unix": now,
    })
}

// ---------------------------------------------------------------- manifests

struct ManifestFile {
    source_path: String,
    dest_path: String,
    blob: String,
    copyright: Vec<String>,
}

struct Manifest {
    id: String,
    url: String,
    commit: String,
    license: String,
    evidence: Vec<(String, String, String)>,
    license_texts: Vec<(String, String, String)>,
    notice_text: String,
    copyright: Vec<String>,
    files: Vec<ManifestFile>,
}

fn str_field(value: &Value, key: &str, ctx: &str, errors: &mut Vec<String>) -> String {
    match value.get(key).and_then(Value::as_str) {
        Some(s) => s.to_owned(),
        None => {
            errors.push(format!("{ctx}: missing string field `{key}`"));
            String::new()
        }
    }
}

fn str_list(value: &Value, key: &str, ctx: &str, errors: &mut Vec<String>) -> Vec<String> {
    match value.get(key).and_then(Value::as_array) {
        Some(items) => items
            .iter()
            .filter_map(|item| {
                let s = item.as_str().map(str::to_owned);
                if s.is_none() {
                    errors.push(format!("{ctx}: `{key}` must hold strings"));
                }
                s
            })
            .collect(),
        None => {
            errors.push(format!("{ctx}: missing array field `{key}`"));
            Vec::new()
        }
    }
}

fn parse_manifest(name: &str, v: &Value) -> std::result::Result<Manifest, Vec<String>> {
    let mut errors = Vec::new();
    let e = &mut errors;
    let ctx = format!("{MANIFEST_DIR}/{name}");
    if v.get("schema").and_then(Value::as_str) != Some(MANIFEST_SCHEMA) {
        e.push(format!("{ctx}: schema must be {MANIFEST_SCHEMA}"));
    }
    let id = str_field(v, "id", &ctx, e);
    if let Err(err) = check_labelled_id(&id, "H-") {
        e.push(format!("{ctx}: {err}"));
    }
    if format!("{id}.json") != name {
        e.push(format!("{ctx}: id `{id}` does not match the file name"));
    }
    let task = str_field(v, "task", &ctx, e);
    if let Err(err) = check_labelled_id(&task, "T-") {
        e.push(format!("{ctx}: {err}"));
    }
    if !v
        .get("feature")
        .is_some_and(|f| f.is_null() || f.is_string())
    {
        e.push(format!("{ctx}: `feature` must be a string or null"));
    }
    let url = str_field(v, "source_url", &ctx, e);
    if url.is_empty() {
        e.push(format!("{ctx}: empty source_url"));
    }
    let commit = str_field(v, "source_commit", &ctx, e);
    if !is_hex_oid(&commit) {
        e.push(format!("{ctx}: source_commit is not a full commit id"));
    }
    let mode = str_field(v, "mode", &ctx, e);
    if mode != "transplant" && mode != "port" {
        e.push(format!("{ctx}: mode must be transplant or port"));
    }
    let license = str_field(v, "license", &ctx, e);
    if choose_license(&license).is_err() {
        e.push(format!("{ctx}: license `{license}` is not allowed"));
    }
    let notice_text = str_field(v, "notice_text", &ctx, e);
    let copyright = str_list(v, "copyright_lines", &ctx, e);
    str_field(v, "harvested_by", &ctx, e);
    if v.get("harvested_unix").and_then(Value::as_u64).is_none() {
        e.push(format!("{ctx}: missing integer `harvested_unix`"));
    }
    let evidence = parse_evidence(v, &ctx, e);
    let license_texts = v
        .get("license_texts")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|t| {
                    (
                        str_field(t, "path", &ctx, e),
                        str_field(t, "spdx", &ctx, e),
                        str_field(t, "text", &ctx, e),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let files = parse_manifest_files(v, &ctx, e);
    if errors.is_empty() {
        Ok(Manifest {
            id,
            url,
            commit,
            license,
            evidence,
            license_texts,
            notice_text,
            copyright,
            files,
        })
    } else {
        Err(errors)
    }
}

fn parse_evidence(v: &Value, ctx: &str, e: &mut Vec<String>) -> Vec<(String, String, String)> {
    let Some(items) = v.get("license_evidence").and_then(Value::as_array) else {
        e.push(format!("{ctx}: missing array `license_evidence`"));
        return Vec::new();
    };
    items
        .iter()
        .map(|item| {
            let path = str_field(item, "path", ctx, e);
            let blob = str_field(item, "blob_sha1", ctx, e);
            let sha = str_field(item, "sha256", ctx, e);
            if !is_hex_oid(&blob) || sha.len() != 64 || !is_hex_oid(&sha) {
                e.push(format!("{ctx}: evidence `{path}` has malformed hashes"));
            }
            (path, blob, sha)
        })
        .collect()
}

fn parse_manifest_files(v: &Value, ctx: &str, e: &mut Vec<String>) -> Vec<ManifestFile> {
    let items = v.get("files").and_then(Value::as_array);
    if items.is_none_or(|items| items.is_empty()) {
        e.push(format!("{ctx}: `files` must be a non-empty array"));
    }
    items
        .into_iter()
        .flatten()
        .map(|f| {
            let source_path = str_field(f, "source_path", ctx, e);
            let dest_path = str_field(f, "dest_path", ctx, e);
            if normalize_rel(&dest_path, true).as_deref() != Ok(dest_path.as_str()) {
                e.push(format!(
                    "{ctx}: dest_path `{dest_path}` is not a confined relative path"
                ));
            }
            let blob = str_field(f, "source_blob_sha1", ctx, e);
            if !is_hex_oid(&blob) {
                e.push(format!(
                    "{ctx}: `{source_path}` has a malformed source_blob_sha1"
                ));
            }
            let spdx = str_field(f, "spdx", ctx, e);
            if choose_license(&spdx).is_err() {
                e.push(format!(
                    "{ctx}: `{source_path}` license `{spdx}` is not allowed"
                ));
            }
            ManifestFile {
                source_path,
                dest_path,
                blob,
                copyright: str_list(f, "copyright_lines", ctx, e),
            }
        })
        .collect()
}

/// Every `.agents/harvest/*.json` file name with its parsed JSON, sorted by name.
fn load_manifest_values(root: &Path) -> Result<Vec<(String, Result<Value>)>> {
    let dir = root.join(MANIFEST_DIR);
    let Ok(entries) = fs::read_dir(&dir) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("cannot list {MANIFEST_DIR}: {e}"))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".json") {
            continue;
        }
        let parsed = fs::read(entry.path())
            .map_err(|e| format!("{MANIFEST_DIR}/{name}: {e}"))
            .and_then(|bytes| {
                serde_json::from_slice(&bytes).map_err(|e| format!("{MANIFEST_DIR}/{name}: {e}"))
            });
        out.push((name, parsed));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

fn load_manifests(root: &Path) -> (Vec<Manifest>, Vec<String>) {
    let mut manifests = Vec::new();
    let mut errors = Vec::new();
    match load_manifest_values(root) {
        Ok(values) => {
            for (name, parsed) in values {
                match parsed {
                    Ok(value) => match parse_manifest(&name, &value) {
                        Ok(m) => manifests.push(m),
                        Err(errs) => errors.extend(errs),
                    },
                    Err(err) => errors.push(err),
                }
            }
        }
        Err(err) => errors.push(err),
    }
    manifests.sort_by(|a, b| a.id.cmp(&b.id));
    (manifests, errors)
}

// ---------------------------------------------------------------- notices

fn render_notices(manifests: &[Manifest]) -> String {
    let mut out = String::from(
        "THIRD-PARTY NOTICES\n\n\
         Generated by `local-harvest notices` from .agents/harvest/*.json\n\
         (susi-harvest-manifest-v1). Do not edit by hand; regenerate after every harvest.\n\
         Harvested code keeps its upstream license. The Apache License 2.0 text is in\n\
         LICENSES/Apache-2.0.txt.\n",
    );
    if manifests.is_empty() {
        out.push_str("\nNo harvested third-party code.\n");
    }
    for m in manifests {
        out.push('\n');
        out.push_str(&"=".repeat(78));
        out.push_str(&format!(
            "\n{}\nSource: {}@{}\nLicense: {}\nFiles:\n",
            m.id, m.url, m.commit, m.license
        ));
        let mut files: Vec<&ManifestFile> = m.files.iter().collect();
        files.sort_by(|a, b| (&a.dest_path, &a.source_path).cmp(&(&b.dest_path, &b.source_path)));
        for f in files {
            out.push_str(&format!("  {} -> {}\n", f.source_path, f.dest_path));
        }
        if !m.copyright.is_empty() {
            out.push_str("Copyright notices:\n");
            for line in &m.copyright {
                out.push_str(&format!("  {line}\n"));
            }
        }
        if !m.notice_text.trim().is_empty() {
            out.push_str(&format!("NOTICE:\n{}\n", m.notice_text.trim_end()));
        }
        for (path, spdx, text) in &m.license_texts {
            out.push_str(&format!(
                "License text ({path}, {spdx}):\n{}\n",
                text.trim_end()
            ));
        }
    }
    out
}

fn normalized_disk_text(path: &Path) -> Option<String> {
    fs::read(path)
        .ok()
        .map(|bytes| lossy(&bytes).replace("\r\n", "\n"))
}

#[derive(PartialEq, Eq, Debug)]
enum NoticesState {
    Current,
    Missing,
    Stale,
}

fn notices_state(root: &Path, manifests: &[Manifest]) -> NoticesState {
    let expected = render_notices(manifests);
    match normalized_disk_text(&root.join(NOTICES_FILE)) {
        Some(text) if text == expected => NoticesState::Current,
        Some(_) => NoticesState::Stale,
        None if manifests.is_empty() => NoticesState::Current,
        None => NoticesState::Missing,
    }
}

/// Regenerates `THIRD_PARTY_NOTICES`; returns `written` or `unchanged`.
fn write_notices(root: &Path) -> Result<&'static str> {
    let (manifests, errors) = load_manifests(root);
    if !errors.is_empty() {
        return Err(format!(
            "cannot generate {NOTICES_FILE}; invalid manifests:\n  {}",
            errors.join("\n  ")
        ));
    }
    let expected = render_notices(&manifests);
    if normalized_disk_text(&root.join(NOTICES_FILE)).as_deref() == Some(expected.as_str()) {
        return Ok("unchanged");
    }
    write_file(root, NOTICES_FILE, expected.as_bytes())?;
    Ok("written")
}

fn notices_command(git: &Git, cli: &Cli) -> Result<Report> {
    cli.only(&["check"])?;
    let root = &git.root;
    if !cli.switch("check") {
        let state = write_notices(root)?;
        return Ok(Report {
            ok: true,
            text: format!("{NOTICES_FILE} {state}"),
            json: json!({"ok": true, "state": state}),
        });
    }
    let (manifests, errors) = load_manifests(root);
    if !errors.is_empty() {
        return Ok(Report {
            ok: false,
            text: format!("invalid manifests:\n  {}", errors.join("\n  ")),
            json: json!({"ok": false, "errors": errors}),
        });
    }
    let state = notices_state(root, &manifests);
    let (ok, word) = match state {
        NoticesState::Current => (true, "current"),
        NoticesState::Missing => (false, "missing"),
        NoticesState::Stale => (false, "stale"),
    };
    let text = if ok {
        format!("{NOTICES_FILE} is current ({} manifests)", manifests.len())
    } else {
        format!("{NOTICES_FILE} is {word}; run `local-harvest notices`")
    };
    Ok(Report {
        ok,
        text,
        json: json!({"ok": ok, "state": word, "manifests": manifests.len()}),
    })
}

// ---------------------------------------------------------------- audit

fn has_modified_notice(text: &str) -> bool {
    text.lines()
        .any(|line| line.to_lowercase().contains("modified by"))
}

/// Checks one manifest; returns the number of destination files examined.
fn audit_manifest(git: &Git, m: &Manifest, violations: &mut Vec<String>) -> Result<usize> {
    let upstream = if git.has_commit(&m.commit)? {
        Some(list_tree(git, &m.commit)?)
    } else {
        None
    };
    let mut upstream_copyright: BTreeMap<String, Vec<String>> = BTreeMap::new();
    if let Some(tree) = &upstream {
        let recorded: BTreeSet<&str> = m.copyright.iter().map(String::as_str).collect();
        let mut oids = BTreeSet::new();
        for (path, blob, _) in &m.evidence {
            if tree.get(path).map(|e| e.oid.as_str()) != Some(blob.as_str()) {
                violations.push(format!(
                    "{}: evidence {path} does not match upstream {}",
                    m.id, m.commit
                ));
            } else {
                oids.insert(blob.clone());
            }
        }
        for f in &m.files {
            if tree.get(&f.source_path).map(|e| e.oid.as_str()) != Some(f.blob.as_str()) {
                violations.push(format!(
                    "{}: {} is not blob {} at upstream {}",
                    m.id, f.source_path, f.blob, m.commit
                ));
            } else {
                oids.insert(f.blob.clone());
            }
        }
        let blobs = read_blobs(git, &oids)?;
        for (path, blob, sha) in &m.evidence {
            if blobs.get(blob).is_some_and(|b| sha256_hex(b) != *sha) {
                violations.push(format!(
                    "{}: sha256 of evidence {path} does not match",
                    m.id
                ));
            }
        }
        for f in &m.files {
            if let Some(bytes) = blobs.get(&f.blob) {
                let lines = copyright_lines(&lossy(bytes))
                    .into_iter()
                    .filter(|l| recorded.contains(l.as_str()))
                    .collect();
                upstream_copyright.insert(f.source_path.clone(), lines);
            }
        }
    }

    let root = &git.root;
    let present: Vec<&ManifestFile> = m
        .files
        .iter()
        .filter(|f| {
            let exists = root.join(&f.dest_path).is_file();
            if !exists {
                violations.push(format!("{}: destination {} is missing", m.id, f.dest_path));
            }
            exists
        })
        .collect();
    let dests: Vec<String> = present.iter().map(|f| f.dest_path.clone()).collect();
    let filtered = hash_paths(git, &dests, true)?;
    let raw = hash_paths(git, &dests, false)?;
    for (i, f) in present.iter().enumerate() {
        let bytes = fs::read(root.join(&f.dest_path))
            .map_err(|e| format!("cannot read {}: {e}", f.dest_path))?;
        let text = lossy(&bytes);
        let unmodified = filtered[i] == f.blob || raw[i] == f.blob;
        if !unmodified && !has_modified_notice(&text) {
            violations.push(format!(
                "{}: {} differs from upstream {} but has no \"Modified by\" notice (Apache-2.0 4(b))",
                m.id, f.dest_path, f.source_path
            ));
        }
        let mut expected: BTreeSet<&str> = f.copyright.iter().map(String::as_str).collect();
        if let Some(lines) = upstream_copyright.get(&f.source_path) {
            expected.extend(lines.iter().map(String::as_str));
        }
        for line in expected {
            if !text.contains(line) {
                violations.push(format!(
                    "{}: {} lost upstream copyright line \"{line}\" (Apache-2.0 4(c))",
                    m.id, f.dest_path
                ));
            }
        }
    }
    Ok(present.len())
}

fn harvest_trailers(git: &Git, range: &str) -> Result<Vec<(String, Vec<String>)>> {
    if range.is_empty() || range.starts_with('-') || range.chars().any(char::is_whitespace) {
        return Err(format!("invalid --range `{range}`"));
    }
    let raw = git.checked(
        &[
            "log",
            "-z",
            "--format=%H%n%(trailers:key=Harvest,valueonly)",
            range,
            "--",
        ],
        None,
    )?;
    let mut commits = Vec::new();
    for record in String::from_utf8_lossy(&raw).split('\0') {
        let mut lines = record.lines();
        let Some(commit) = lines.next().map(str::trim).filter(|c| !c.is_empty()) else {
            continue;
        };
        let ids: Vec<String> = lines
            .flat_map(|l| l.split([',', ' ', '\t']))
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
        if !ids.is_empty() {
            commits.push((commit.to_owned(), ids));
        }
    }
    Ok(commits)
}

fn audit(git: &Git, cli: &Cli) -> Result<Report> {
    cli.only(&["range"])?;
    let root = git.root.clone();
    let (manifests, mut violations) = load_manifests(&root);
    let mut files = 0;
    for m in &manifests {
        files += audit_manifest(git, m, &mut violations)?;
    }
    if violations.is_empty() {
        match notices_state(&root, &manifests) {
            NoticesState::Current => {}
            NoticesState::Missing => violations.push(format!(
                "{NOTICES_FILE} is missing; run `local-harvest notices` (Apache-2.0 4(d))"
            )),
            NoticesState::Stale => violations.push(format!(
                "{NOTICES_FILE} is stale; run `local-harvest notices` (Apache-2.0 4(d))"
            )),
        }
    }
    if manifests.iter().any(|m| m.license.contains("Apache-2.0")) {
        let text = normalized_disk_text(&root.join(APACHE_TEXT_PATH)).unwrap_or_default();
        if classify_license_text(&text) != Some("Apache-2.0") {
            violations.push(format!(
                "{APACHE_TEXT_PATH} is missing or not the Apache License 2.0 text (Apache-2.0 4(a))"
            ));
        }
    }
    let mut trailer_commits = 0;
    if let Some(range) = cli.optional("range")? {
        let known: BTreeSet<String> = load_manifest_values(&root)?
            .into_iter()
            .map(|(name, _)| name.trim_end_matches(".json").to_owned())
            .collect();
        for (commit, ids) in harvest_trailers(git, &range)? {
            trailer_commits += 1;
            for id in ids.iter().filter(|id| !known.contains(*id)) {
                violations.push(format!(
                    "commit {} has `Harvest: {id}` but {MANIFEST_DIR}/{id}.json does not exist",
                    &commit[..commit.len().min(12)]
                ));
            }
        }
    }
    let ok = violations.is_empty();
    let text = if ok {
        format!(
            "harvest audit passed: {} manifest(s), {files} file(s), {trailer_commits} Harvest-trailer commit(s)",
            manifests.len()
        )
    } else {
        format!(
            "harvest audit failed with {} violation(s):\n  {}",
            violations.len(),
            violations.join("\n  ")
        )
    };
    Ok(Report {
        ok,
        text,
        json: json!({
            "ok": ok,
            "manifests": manifests.len(),
            "files": files,
            "harvest_trailer_commits": trailer_commits,
            "violations": violations,
        }),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    pub(super) fn isolate_git_env(command: &mut Command, root: &Path) {
        command.env_clear();
        for key in [
            "PATH",
            "SystemRoot",
            "WINDIR",
            "COMSPEC",
            "PATHEXT",
            "TEMP",
            "TMP",
        ] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        command
            .env("HOME", root)
            .env("USERPROFILE", root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", root.join(".absent-fixture-config"))
            .env("GIT_AUTHOR_NAME", "Harvest Fixture")
            .env("GIT_AUTHOR_EMAIL", "fixture@localhost")
            .env("GIT_COMMITTER_NAME", "Harvest Fixture")
            .env("GIT_COMMITTER_EMAIL", "fixture@localhost")
            .env("GIT_TERMINAL_PROMPT", "0");
    }

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    const APACHE: &str = "\n                                 Apache License\n                           Version 2.0, January 2004\n                        http://www.apache.org/licenses/\n\n   TERMS AND CONDITIONS FOR USE, REPRODUCTION, AND DISTRIBUTION\n\n   (c) You must retain all copyright, patent, trademark notices.\n\n   Copyright [yyyy] [name of copyright owner]\n";
    const WIDGET: &str = "// Copyright 2024 The Widget Authors\n//\n// Licensed under the Apache License, Version 2.0.\n\npub fn widget() -> u32 {\n    7\n}\n";

    struct Fixture {
        dir: PathBuf,
        upstream: PathBuf,
        work: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let id = SEQ.fetch_add(1, Ordering::Relaxed);
            let dir =
                std::env::temp_dir().join(format!("susi-harvest-test-{}-{id}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            let upstream = dir.join("upstream");
            let work = dir.join("work");
            fs::create_dir_all(&upstream).unwrap();
            fs::create_dir_all(&work).unwrap();
            let fx = Self {
                dir,
                upstream,
                work,
            };
            fx.git_at(&fx.upstream, &["init", "--initial-branch=main"]);
            for (rel, text) in [
                ("LICENSE", APACHE),
                ("NOTICE", "Upstream Widgets\nCopyright 2024 The Widget Authors\n"),
                ("src/widget.rs", WIDGET),
                (
                    "src/mit_util.rs",
                    "// SPDX-License-Identifier: MIT OR Apache-2.0\n// Copyright (c) 2023 Jane Example\npub fn util() {}\n",
                ),
                (
                    "other/mit_only.rs",
                    "// SPDX-License-Identifier: MIT\n// Copyright (c) 2022 Sam Example\npub fn only() {}\n",
                ),
                (
                    "third_party/gpl/COPYING",
                    "                    GNU GENERAL PUBLIC LICENSE\n                       Version 3, 29 June 2007\n",
                ),
                (
                    "third_party/gpl/thing.c",
                    "/* This file is part of Thing, licensed under the GNU General\n * Public License version 3. */\nint thing(void) { return 1; }\n",
                ),
            ] {
                write(&fx.upstream, rel, text);
            }
            fx.git_at(&fx.upstream, &["add", "-A"]);
            fx.git_at(&fx.upstream, &["commit", "-q", "-m", "upstream"]);
            fx.git_at(&fx.work, &["init", "--initial-branch=main"]);
            write(&fx.work, "README.md", "work\n");
            fx.git_at(&fx.work, &["add", "-A"]);
            fx.git_at(&fx.work, &["commit", "-q", "-m", "base"]);
            fx
        }

        fn git_at(&self, root: &Path, args: &[&str]) -> String {
            Git {
                root: root.to_path_buf(),
            }
            .text(args)
            .unwrap()
        }

        fn upstream_head(&self) -> String {
            self.git_at(&self.upstream, &["rev-parse", "HEAD"])
        }

        fn url(&self) -> String {
            self.upstream.to_string_lossy().into_owned()
        }

        fn run(&self, args: &[&str]) -> Result<String> {
            let args: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
            run(&self.work, &args)
        }

        fn intake(&self) -> String {
            let url = self.url();
            self.run(&[
                "intake", "--url", &url, "--rev", "main", "--slug", "widgets",
            ])
            .unwrap();
            self.upstream_head()
        }

        fn import_widgets(&self, sha: &str) -> Result<String> {
            self.run(&[
                "import",
                "--slug",
                "widgets",
                "--rev",
                sha,
                "--map",
                "src=vendor/widgets",
                "--id",
                "H-TEST-1",
                "--task",
                "T-TEST-1",
                "--agent",
                "tester",
            ])
        }

        fn read(&self, rel: &str) -> String {
            fs::read_to_string(self.work.join(rel)).unwrap()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    fn write(root: &Path, rel: &str, text: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    #[test]
    fn intake_by_ref_pins_the_fetched_commit() {
        let fx = Fixture::new();
        let url = fx.url();
        let out = fx
            .run(&[
                "intake", "--url", &url, "--rev", "main", "--slug", "widgets",
            ])
            .unwrap();
        let sha = fx.upstream_head();
        let reference = format!("refs/susi/upstream/widgets/{sha}");
        assert!(out.contains(&sha) && out.contains(&reference), "{out}");
        assert_eq!(fx.git_at(&fx.work, &["rev-parse", &reference]), sha);
        let again = fx
            .run(&[
                "intake", "--url", &url, "--rev", &sha, "--slug", "widgets", "--json",
            ])
            .unwrap();
        let v: Value = serde_json::from_str(&again).unwrap();
        assert_eq!(v["created"], false);
        assert_eq!(v["commit"], sha.as_str());
        assert!(fx
            .run(&["intake", "--url", &url, "--rev", "main", "--slug", "Bad/Slug"])
            .is_err());
        assert!(fx
            .run(&[
                "intake",
                "--url",
                &url,
                "--rev",
                "main:refs/heads/x",
                "--slug",
                "w"
            ])
            .is_err());
    }

    #[test]
    fn scan_accepts_permissive_files_and_rejects_the_gpl_directory() {
        let fx = Fixture::new();
        let sha = fx.intake();
        let ok = fx
            .run(&["scan", "--slug", "widgets", "--rev", &sha, "--path", "src"])
            .unwrap();
        assert!(ok.contains("2 accepted, 0 rejected"), "{ok}");
        let err = fx
            .run(&[
                "scan",
                "--slug",
                "widgets",
                "--rev",
                &sha,
                "--path",
                "src",
                "--path",
                "third_party",
                "--json",
            ])
            .unwrap_err();
        let v: Value = serde_json::from_str(&err).unwrap();
        assert_eq!(v["rejected"], 2);
        let verdict = |path: &str| {
            v["files"]
                .as_array()
                .unwrap()
                .iter()
                .find(|f| f["path"] == path)
                .unwrap()
                .clone()
        };
        assert_eq!(verdict("src/widget.rs")["license"], "Apache-2.0");
        assert_eq!(verdict("src/widget.rs")["basis"], "LICENSE");
        // A dual-licensed file is taken under its Apache-2.0 alternative.
        assert_eq!(verdict("src/mit_util.rs")["license"], "Apache-2.0");
        assert_eq!(verdict("src/mit_util.rs")["basis"], "SPDX header");
        let thing = verdict("third_party/gpl/thing.c");
        assert_eq!(thing["verdict"], "reject");
        let reasons = thing["reasons"].to_string();
        assert!(reasons.contains("third_party/gpl/COPYING"), "{reasons}");
        assert!(reasons.contains("GNU General Public License"), "{reasons}");
        assert!(fx
            .run(&["scan", "--slug", "widgets", "--rev", &sha, "--path", "nope"])
            .is_err());
        // Only Apache-2.0 code is harvested: an MIT-only file is refused, by
        // scan and by import, and nothing is written.
        let mit = fx
            .run(&[
                "scan", "--slug", "widgets", "--rev", &sha, "--path", "other",
            ])
            .unwrap_err();
        assert!(mit.contains("not Apache-2.0"), "{mit}");
        let import = fx.run(&[
            "import",
            "--slug",
            "widgets",
            "--rev",
            &sha,
            "--map",
            "other=vendor/other",
            "--id",
            "H-TEST-9",
            "--task",
            "T-TEST-1",
        ]);
        assert!(import.unwrap_err().contains("import refused"));
        assert!(!fx.work.join("vendor/other").exists());
    }

    #[test]
    fn import_writes_files_manifest_license_and_notices() {
        let fx = Fixture::new();
        let sha = fx.intake();
        let out = fx.import_widgets(&sha).unwrap();
        assert!(out.contains("Harvest: H-TEST-1"), "{out}");
        assert!(
            out.contains(&format!("Origin: {}@{sha}:src", fx.url())),
            "{out}"
        );
        assert!(out.contains("Origin-License: Apache-2.0\n"), "{out}");
        assert_eq!(fx.read("vendor/widgets/widget.rs"), WIDGET);
        assert!(fx
            .read("vendor/widgets/mit_util.rs")
            .contains("Jane Example"));
        assert_eq!(fx.read(APACHE_TEXT_PATH), APACHE);
        let m: Value = serde_json::from_str(&fx.read(".agents/harvest/H-TEST-1.json")).unwrap();
        assert_eq!(m["schema"], MANIFEST_SCHEMA);
        assert_eq!(m["source_url"], fx.url().as_str());
        assert_eq!(m["source_commit"], sha.as_str());
        assert_eq!(m["harvested_by"], "tester");
        assert_eq!(m["files"].as_array().unwrap().len(), 2);
        assert!(m["notice_text"]
            .as_str()
            .unwrap()
            .contains("Upstream Widgets"));
        let lines = m["copyright_lines"].to_string();
        assert!(
            lines.contains("Copyright 2024 The Widget Authors"),
            "{lines}"
        );
        assert!(lines.contains("Copyright (c) 2023 Jane Example"), "{lines}");
        assert!(!lines.contains("yyyy"), "{lines}");
        let evidence = m["license_evidence"].to_string();
        assert!(evidence.contains("\"LICENSE\"") && evidence.contains("\"NOTICE\""));
        assert!(fx.read(NOTICES_FILE).contains("H-TEST-1"));
        // refuses to overwrite, to escape the worktree and to import GPL code
        assert!(fx
            .import_widgets(&sha)
            .unwrap_err()
            .contains("already exists"));
        let escape = fx.run(&[
            "import",
            "--slug",
            "widgets",
            "--rev",
            &sha,
            "--map",
            "src/widget.rs=../x.rs",
            "--id",
            "H-TEST-2",
            "--task",
            "T-TEST-1",
        ]);
        assert!(escape.unwrap_err().contains("confined"));
        let gpl = fx.run(&[
            "import",
            "--slug",
            "widgets",
            "--rev",
            &sha,
            "--map",
            "third_party=tp",
            "--id",
            "H-TEST-3",
            "--task",
            "T-TEST-1",
        ]);
        assert!(gpl.unwrap_err().contains("import refused"));
        assert!(!fx.work.join("tp").exists());
    }

    #[test]
    fn notices_are_deterministic_and_check_detects_staleness() {
        let fx = Fixture::new();
        let sha = fx.intake();
        fx.import_widgets(&sha).unwrap();
        let first = fx.read(NOTICES_FILE);
        fs::remove_file(fx.work.join(NOTICES_FILE)).unwrap();
        assert!(fx
            .run(&["notices", "--check"])
            .unwrap_err()
            .contains("missing"));
        assert!(fx.run(&["notices"]).unwrap().contains("written"));
        assert_eq!(fx.read(NOTICES_FILE), first);
        assert!(fx.run(&["notices"]).unwrap().contains("unchanged"));
        assert!(fx.run(&["notices", "--check"]).is_ok());
        write(&fx.work, NOTICES_FILE, &format!("{first}tampered\n"));
        assert!(fx
            .run(&["notices", "--check"])
            .unwrap_err()
            .contains("stale"));
        // a second harvest sorts deterministically by id
        fx.run(&[
            "import",
            "--slug",
            "widgets",
            "--rev",
            &sha,
            "--map",
            "src/widget.rs=other/w.rs",
            "--id",
            "H-TEST-0",
            "--task",
            "T-TEST-1",
            "--agent",
            "tester",
        ])
        .unwrap();
        let both = fx.read(NOTICES_FILE);
        assert!(both.find("H-TEST-0").unwrap() < both.find("H-TEST-1").unwrap());
        assert!(fx.run(&["notices", "--check"]).is_ok());
    }

    #[test]
    fn audit_enforces_modification_notices_and_copyright_retention() {
        let fx = Fixture::new();
        let sha = fx.intake();
        fx.import_widgets(&sha).unwrap();
        let passed = fx.run(&["audit"]).unwrap();
        assert!(passed.contains("1 manifest(s), 2 file(s)"), "{passed}");
        // offline fallback: drop the quarantine ref and prune the upstream objects
        fx.git_at(
            &fx.work,
            &[
                "update-ref",
                "-d",
                &format!("refs/susi/upstream/widgets/{sha}"),
            ],
        );
        fx.git_at(&fx.work, &["reflog", "expire", "--expire=now", "--all"]);
        fx.git_at(&fx.work, &["gc", "-q", "--prune=now"]);
        assert!(!Git {
            root: fx.work.clone()
        }
        .has_commit(&sha)
        .unwrap());
        assert!(fx.run(&["audit"]).is_ok());
        let path = "vendor/widgets/widget.rs";
        write(&fx.work, path, &format!("{WIDGET}pub fn extra() {{}}\n"));
        let err = fx.run(&["audit"]).unwrap_err();
        assert!(err.contains("Modified by") && err.contains(path), "{err}");
        write(
            &fx.work,
            path,
            &format!("{WIDGET}// Modified by Susi contributors, 2026-11-11: add extra\npub fn extra() {{}}\n"),
        );
        assert!(fx.run(&["audit"]).is_ok());
        let stripped = fx
            .read(path)
            .replace("// Copyright 2024 The Widget Authors\n", "");
        write(&fx.work, path, &stripped);
        let err = fx.run(&["audit", "--json"]).unwrap_err();
        let v: Value = serde_json::from_str(&err).unwrap();
        assert!(v["violations"]
            .to_string()
            .contains("Copyright 2024 The Widget Authors"));
        fs::remove_file(fx.work.join(APACHE_TEXT_PATH)).unwrap();
        assert!(fx.run(&["audit"]).unwrap_err().contains("4(a)"));
    }

    #[test]
    fn audit_range_rejects_unknown_harvest_trailers() {
        let fx = Fixture::new();
        let sha = fx.intake();
        let base = fx.git_at(&fx.work, &["rev-parse", "HEAD"]);
        fx.import_widgets(&sha).unwrap();
        fx.git_at(&fx.work, &["add", "-A"]);
        fx.git_at(
            &fx.work,
            &[
                "commit",
                "-q",
                "-m",
                "harvest widgets\n\nHarvest: H-TEST-1\nTask: T-TEST-1",
            ],
        );
        let range = format!("{base}..HEAD");
        let ok = fx.run(&["audit", "--range", &range]).unwrap();
        assert!(ok.contains("1 Harvest-trailer commit(s)"), "{ok}");
        write(&fx.work, "README.md", "changed\n");
        fx.git_at(&fx.work, &["add", "-A"]);
        fx.git_at(
            &fx.work,
            &[
                "commit",
                "-q",
                "-m",
                "bogus\n\nHarvest: H-TEST-99\nTask: T-TEST-1",
            ],
        );
        let err = fx.run(&["audit", "--range", &range]).unwrap_err();
        assert!(err.contains("H-TEST-99"), "{err}");
        assert!(!err.contains("Harvest: H-TEST-1`"), "{err}");
    }

    #[test]
    fn license_expressions_choose_apache_and_reject_the_rest() {
        assert_eq!(choose_license("Apache-2.0").unwrap(), "Apache-2.0");
        assert_eq!(choose_license("MIT OR Apache-2.0").unwrap(), "Apache-2.0");
        assert_eq!(choose_license("(MIT OR Apache-2.0)").unwrap(), "Apache-2.0");
        assert_eq!(choose_license("apache-2.0").unwrap(), "Apache-2.0");
        // The Apache-2.0 alternative is taken whatever else is offered.
        assert_eq!(
            choose_license("GPL-2.0-only OR Apache-2.0").unwrap(),
            "Apache-2.0"
        );
        // CEO decision 2026-11-12: nothing but Apache-2.0 is harvested.
        for not_apache in [
            "MIT",
            "bsd-3-clause",
            "BSD-2-Clause",
            "GPL-2.0-only OR MIT",
            "Apache-2.0 AND MIT",
            "Apache-2.0 WITH LLVM-exception",
        ] {
            let refused = choose_license(not_apache).unwrap_err();
            assert!(
                refused.contains("not Apache-2.0"),
                "{not_apache}: {refused}"
            );
        }
        for bad in [
            "GPL-3.0-or-later",
            "MPL-2.0",
            "Apache-2.0 AND GPL-2.0-only",
            "",
            "NONE",
            "Apache-2.0 WITH Classpath-exception-2.0",
        ] {
            assert!(choose_license(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn headers_license_texts_and_copyright_lines_are_recognized() {
        assert_eq!(
            spdx_header("/* SPDX-License-Identifier: MIT */\nint x;"),
            Some("MIT".to_owned())
        );
        assert_eq!(
            spdx_header("<!-- SPDX-License-Identifier: Apache-2.0 -->"),
            Some("Apache-2.0".to_owned())
        );
        let deep = format!("{}// SPDX-License-Identifier: MIT\n", "x\n".repeat(40));
        assert_eq!(spdx_header(&deep), None);
        assert_eq!(classify_license_text(APACHE), Some("Apache-2.0"));
        assert_eq!(
            classify_license_text("Permission is hereby granted, free of\n charge, to any person"),
            Some("MIT")
        );
        let bsd = "Redistribution and use in source and binary forms, with or without";
        assert_eq!(classify_license_text(bsd), Some("BSD-2-Clause"));
        assert_eq!(
            classify_license_text(&format!("{bsd}\nNeither the name of the copyright holder")),
            Some("BSD-3-Clause")
        );
        assert_eq!(
            classify_license_text("GNU Affero General Public License"),
            None
        );
        assert_eq!(classify_license_text("All rights reserved."), None);
        assert_eq!(
            copyright_lines(" * Copyright (C) 2020 Foo Corp. */\n// copyright notice\n# Copyright [yyyy] x\nCopyrighted 2020 X\n© 2021 Bar\n"),
            vec!["Copyright (C) 2020 Foo Corp.".to_owned(), "© 2021 Bar".to_owned()]
        );
    }

    #[test]
    fn paths_and_ids_are_validated() {
        assert_eq!(normalize_rel("a\\b/", true).unwrap(), "a/b");
        assert_eq!(normalize_rel(".", false).unwrap(), "");
        for bad in ["../x", "a/../b", "/abs", "C:/x", "a/.git/config", ""] {
            assert!(normalize_rel(bad, true).is_err(), "{bad}");
        }
        assert!(check_slug("abseil-cpp_1.2").is_ok());
        for bad in ["", "-x", ".x", "A", "a/b", "a..b", "x.lock"] {
            assert!(check_slug(bad).is_err(), "{bad}");
        }
        assert!(check_labelled_id("H-CLAUDE-12", "H-").is_ok());
        for bad in ["H-claude-1", "H--1", "H-X-", "T-X-1", "H-X-1a"] {
            assert!(check_labelled_id(bad, "H-").is_err(), "{bad}");
        }
        assert!(check_url("ext::sh -c x").is_err());
        assert!(check_url("--upload-pack=x").is_err());
        assert!(check_rev("+main").is_err());
        assert!(parse_cli(&["scan".to_owned(), "--bogus".to_owned()]).is_err());
    }
}
