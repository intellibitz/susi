//! Gate receipts: the machine-checkable, third-party re-runnable record of a
//! gated landing, kept in the repository.
//!
//! `cargo local-workflow ship` verifies a merge commit `H` (the integration
//! head) and, before anything is published, writes
//! `.agents/receipts/<H>.json` into a receipt-only commit `R` whose parent is
//! `H`. `R` is what reaches `origin/main`; `H` stays the commit the receipt
//! describes, so the receipt can name the exact tree the gate ran on without
//! the file having to contain its own hash. The receipt carries the exact
//! `cargo` commands the gate ran (with the slot's `--target-dir` removed so no
//! operator path is recorded), their exit codes and durations, the test
//! binaries with their pass/fail/ignored counts, the toolchain (`rustc -vV`
//! text and its SHA-256, the host triple), the `origin/main` the landing
//! merged onto, the SHA-256 of `Cargo.lock` at `H`, and a SHA-256 digest of
//! the body. `cargo xtask verify-receipt <commit>` re-runs the recorded
//! commands against `H` in a scratch worktree and reports which results match
//! ([`verify`]).
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Schema identifier of every receipt this module writes.
pub const SCHEMA: &str = "susi-gate-receipt-v1";
/// Repository directory the receipts live in. Spelled in two pieces because
/// `affected.rs` reads a `.agents/...` literal as a test-time read of the
/// tree; the verifier reads receipts only when a user runs it against a
/// clone, and no xtask test reads this directory.
pub const DIR: &str = concat!(".agents", "/receipts");
/// Prefix of the local ref that names the receipt commit of a verified head.
pub const REF_PREFIX: &str = "refs/susi/receipts/";

/// Repository path of the receipt describing `commit`.
pub fn path_for(commit: &str) -> String {
    format!("{DIR}/{commit}.json")
}

/// Local ref holding the receipt commit (`R`) of verified head `commit`.
pub fn ref_for(commit: &str) -> String {
    format!("{REF_PREFIX}{commit}")
}

/// Lowercase hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn valid_oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn git_output(root: &Path, args: &[&str], stdin: Option<&[u8]>) -> Result<Vec<u8>> {
    let mut command = Command::new("git");
    command
        .arg("--literal-pathspecs")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .current_dir(root)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = if let Some(bytes) = stdin {
        command.stdin(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|error| format!("git {} in {}: {error}", args.join(" "), root.display()))?;
        child
            .stdin
            .take()
            .ok_or("git stdin unavailable")?
            .write_all(bytes)?;
        child.wait_with_output()?
    } else {
        command
            .output()
            .map_err(|error| format!("git {} in {}: {error}", args.join(" "), root.display()))?
    };
    if !output.status.success() {
        return Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok(output.stdout)
}

fn git(root: &Path, args: &[&str]) -> Result<String> {
    Ok(String::from_utf8(git_output(root, args, None)?)?
        .trim()
        .to_owned())
}

fn git_in(root: &Path, env: &[(&str, &Path)], args: &[&str]) -> Result<String> {
    let mut command = Command::new("git");
    command
        .arg("--literal-pathspecs")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .current_dir(root)
        .args(args);
    for (key, value) in env {
        command.env(key, value);
    }
    let output = command
        .output()
        .map_err(|error| format!("git {} in {}: {error}", args.join(" "), root.display()))?;
    if !output.status.success() {
        return Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

/// Blob at `path` in `commit`'s tree, `None` when the tree has no such path.
fn tree_blob(root: &Path, commit: &str, path: &str) -> Result<Option<Vec<u8>>> {
    let spec = format!("{commit}:{path}");
    let exists = Command::new("git")
        .arg("--literal-pathspecs")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .current_dir(root)
        .args(["cat-file", "-e", &spec])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?
        .success();
    if !exists {
        return Ok(None);
    }
    Ok(Some(git_output(root, &["cat-file", "blob", &spec], None)?))
}

/// The digest a receipt body must carry: SHA-256 of the compact JSON of the
/// receipt without its `digest` member. `serde_json` orders object members
/// by key, so the encoding is canonical.
pub fn digest_of(receipt: &Value) -> Result<String> {
    let mut body = receipt
        .as_object()
        .cloned()
        .ok_or("receipt is not a JSON object")?;
    body.remove("digest");
    Ok(sha256_hex(&serde_json::to_vec(&Value::Object(body))?))
}

/// Error unless `receipt` carries the schema and the digest of its own body.
pub fn check_digest(receipt: &Value) -> Result<()> {
    if receipt.get("schema").and_then(Value::as_str) != Some(SCHEMA) {
        return Err(format!("receipt schema is not {SCHEMA}").into());
    }
    let recorded = receipt
        .get("digest")
        .and_then(Value::as_str)
        .ok_or("receipt has no digest")?;
    let computed = digest_of(receipt)?;
    if recorded != computed {
        return Err(
            format!("receipt digest {recorded} does not match its body ({computed})").into(),
        );
    }
    Ok(())
}

/// `argv` without any `--target-dir <path>` pair (the slot's private target
/// directory is an operator path), and whether one was removed.
pub fn sanitize_argv(argv: &[String]) -> (Vec<String>, bool) {
    let mut out = Vec::with_capacity(argv.len());
    let mut stripped = false;
    let mut skip = false;
    for arg in argv {
        if skip {
            skip = false;
            stripped = true;
            continue;
        }
        if arg == "--target-dir" {
            skip = true;
            continue;
        }
        if let Some(rest) = arg.strip_prefix("--target-dir=") {
            if !rest.is_empty() {
                stripped = true;
                continue;
            }
        }
        out.push(arg.clone());
    }
    (out, stripped)
}

fn option_value<'a>(argv: &'a [String], name: &str) -> Option<&'a str> {
    argv.windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].as_str())
        .or_else(|| {
            argv.iter()
                .find_map(|arg| arg.strip_prefix(&format!("{name}=")))
        })
}

/// Pass, fail and ignored counts of one harness.
#[derive(Clone, Copy, Debug, Eq, PartialEq, PartialOrd, Ord)]
pub struct Counts {
    pub passed: u64,
    pub failed: u64,
    pub ignored: u64,
}

impl Counts {
    fn to_json(self) -> Value {
        json!({"passed": self.passed, "failed": self.failed, "ignored": self.ignored})
    }

    fn from_json(value: &Value) -> Option<Self> {
        Some(Self {
            passed: value.get("passed")?.as_u64()?,
            failed: value.get("failed")?.as_u64()?,
            ignored: value.get("ignored")?.as_u64()?,
        })
    }
}

/// One test harness the gate ran: a test executable (`name` is the file
/// stem without Cargo's metadata hash, `source` the `Running` header's
/// source, such as `unittests src/lib.rs` or `tests/cli.rs`) or a crate's
/// doctests (`source` is `doc-tests`). `counts` is `None` when the harness
/// printed no final `test result:` line.
#[derive(Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
pub struct BinaryResult {
    pub name: String,
    pub source: String,
    pub counts: Option<Counts>,
}

impl BinaryResult {
    fn to_json(&self) -> Value {
        json!({"name": self.name, "source": self.source,
            "counts": self.counts.map_or(Value::Null, Counts::to_json)})
    }

    fn from_json(value: &Value) -> Option<Self> {
        Some(Self {
            name: value.get("name")?.as_str()?.to_owned(),
            source: value.get("source")?.as_str()?.to_owned(),
            counts: value.get("counts").and_then(Counts::from_json),
        })
    }
}

/// Where the gate's own test executables live, so a fixture's nested
/// `cargo test` output inside a test binary is never mistaken for an outer
/// harness: `<target_dir>/[<triple>/]`. Cargo prints an executable's path
/// relative to its working directory when the target directory lies inside
/// it (a serial `cargo test` gate in the candidate), and absolute otherwise
/// (the concurrent runner, a verifier's sibling target), so both spellings
/// count when `cwd` is known.
pub struct OuterBinaries<'a> {
    pub target_dir: &'a str,
    pub triple: Option<&'a str>,
    /// The command's working directory.
    pub cwd: Option<&'a str>,
}

impl OuterBinaries<'_> {
    fn with_triple(&self, dir: &str) -> String {
        let mut prefix = dir.replace('\\', "/");
        if !prefix.ends_with('/') {
            prefix.push('/');
        }
        if let Some(triple) = self.triple {
            prefix.push_str(triple);
            prefix.push('/');
        }
        prefix
    }

    /// The absolute prefix and, when the target directory lies inside `cwd`,
    /// the cwd-relative one.
    fn prefixes(&self) -> Vec<String> {
        let mut prefixes = vec![self.with_triple(self.target_dir)];
        if let Some(cwd) = self.cwd {
            let target = Path::new(self.target_dir);
            if let Ok(relative) = target.strip_prefix(Path::new(cwd)) {
                let relative = relative.to_string_lossy();
                if !relative.is_empty() {
                    prefixes.push(self.with_triple(&relative));
                }
            }
        }
        prefixes
    }
}

fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for next in chars.by_ref() {
                if ('@'..='~').contains(&next) {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// `name-<16 hex>[.exe]` -> `name`.
fn binary_name(executable: &str) -> String {
    let file = executable.rsplit(['/', '\\']).next().unwrap_or(executable);
    let stem = file.strip_suffix(".exe").unwrap_or(file);
    match stem.rsplit_once('-') {
        Some((name, hash)) if hash.len() == 16 && hash.bytes().all(|b| b.is_ascii_hexdigit()) => {
            name.to_owned()
        }
        Some(_) | None => stem.to_owned(),
    }
}

fn result_counts(line: &str) -> Option<Counts> {
    let rest = line.strip_prefix("test result: ")?;
    let (_, counts) = rest.split_once(". ")?;
    let mut values = [None; 3];
    for field in counts.split(';') {
        let mut parts = field.trim().splitn(2, ' ');
        // `finished in 0.01s` and the like carry no count.
        let Some(Ok(count)) = parts.next().map(str::parse::<u64>) else {
            continue;
        };
        match parts.next().unwrap_or("") {
            "passed" => values[0] = Some(count),
            "failed" => values[1] = Some(count),
            "ignored" => values[2] = Some(count),
            _ => {}
        }
    }
    Some(Counts {
        passed: values[0]?,
        failed: values[1]?,
        ignored: values[2]?,
    })
}

/// The outer test harnesses of a `cargo test` log (Cargo's own output or the
/// gate's concurrent runner, whose blocks carry the same `Running` headers).
/// Executables outside `outer` are nested fixture runs and are skipped. In a
/// `--doc` gate (`doc_gate`) every `Doc-tests` block is an outer harness;
/// otherwise Cargo's doctest phase follows the last executable, and a
/// `Doc-tests` banner printed by a fixture inside that executable's output
/// is told apart by shape: from the first outer banner on, every block has
/// exactly one `running` line and one `test result:` line, while a nested
/// banner's block also contains the enclosing executable's result.
pub fn parse_binaries(log: &str, outer: &OuterBinaries<'_>, doc_gate: bool) -> Vec<BinaryResult> {
    let text = strip_ansi(log);
    let prefixes = outer.prefixes();
    let lines: Vec<&str> = text.lines().collect();
    // (index, name, source) of every outer header.
    let mut headers: Vec<(usize, String, String)> = Vec::new();
    let mut banners: Vec<(usize, String)> = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        if let Some(running) = trimmed.strip_prefix("Running ") {
            let Some((source, executable)) = running.rsplit_once(" (") else {
                continue;
            };
            let Some(executable) = executable.strip_suffix(')') else {
                continue;
            };
            let normalized = executable.replace('\\', "/");
            if !prefixes
                .iter()
                .any(|prefix| normalized.starts_with(prefix.as_str()))
            {
                continue;
            }
            headers.push((index, binary_name(&normalized), source.to_owned()));
        } else if let Some(krate) = trimmed.strip_prefix("Doc-tests ") {
            banners.push((index, krate.trim().to_owned()));
        }
    }
    let last_header = headers.last().map_or(0, |(index, _, _)| *index);
    let trailing: Vec<&(usize, String)> = banners
        .iter()
        .filter(|(index, _)| doc_gate || *index > last_header)
        .collect();
    let first_outer_banner = if doc_gate {
        0
    } else {
        // Earliest candidate from which every doctest block is well formed.
        (0..trailing.len())
            .find(|candidate| {
                trailing[*candidate..]
                    .iter()
                    .enumerate()
                    .all(|(offset, (start, _))| {
                        let end = trailing
                            .get(*candidate + offset + 1)
                            .map_or(lines.len(), |(next, _)| *next);
                        let block = &lines[*start + 1..end];
                        block.iter().filter(|l| l.starts_with("running ")).count() == 1
                            && block
                                .iter()
                                .filter(|l| l.starts_with("test result: "))
                                .count()
                                == 1
                    })
            })
            .unwrap_or(trailing.len())
    };
    for (index, krate) in &trailing[first_outer_banner..] {
        headers.push((*index, krate.clone(), "doc-tests".to_owned()));
    }
    let mut results = Vec::with_capacity(headers.len());
    for (position, (start, name, source)) in headers.iter().enumerate() {
        let end = headers
            .get(position + 1)
            .map_or(lines.len(), |(next, _, _)| *next);
        let counts = lines[*start + 1..end]
            .iter()
            .rev()
            .find_map(|line| result_counts(line));
        results.push(BinaryResult {
            name: name.clone(),
            source: source.clone(),
            counts,
        });
    }
    results
}

/// Sum of every harness's counts; `None` when any harness has none.
pub fn totals(binaries: &[BinaryResult]) -> Option<Counts> {
    let mut sum = Counts {
        passed: 0,
        failed: 0,
        ignored: 0,
    };
    for binary in binaries {
        let counts = binary.counts?;
        sum.passed = sum.passed.checked_add(counts.passed)?;
        sum.failed = sum.failed.checked_add(counts.failed)?;
        sum.ignored = sum.ignored.checked_add(counts.ignored)?;
    }
    Some(sum)
}

/// One gate result as the receipt records it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GateRecord {
    pub id: String,
    /// `cargo ...` without any `--target-dir`.
    pub argv: Vec<String>,
    /// The gate ran with its own `--target-dir`; a re-run supplies one.
    pub isolated_target_dir: bool,
    pub exit_code: i64,
    pub elapsed_ms: u64,
    /// Test totals when the gate ran tests and its log was retained.
    pub tests: Option<Counts>,
    pub log_sha256: Option<String>,
    pub binaries: Vec<BinaryResult>,
}

impl GateRecord {
    fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "argv": self.argv,
            "isolated_target_dir": self.isolated_target_dir,
            "exit_code": self.exit_code,
            "elapsed_ms": self.elapsed_ms,
            "tests": self.tests.map_or(Value::Null, Counts::to_json),
            "log_sha256": self.log_sha256,
            "binaries": self.binaries.iter().map(BinaryResult::to_json).collect::<Vec<_>>(),
        })
    }

    fn from_json(value: &Value) -> Result<Self> {
        let argv = value
            .get("argv")
            .and_then(Value::as_array)
            .ok_or("gate without argv")?
            .iter()
            .map(|arg| {
                arg.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| "non-string gate argument".into())
            })
            .collect::<Result<Vec<_>>>()?;
        let binaries = value
            .get("binaries")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .map(|item| BinaryResult::from_json(item).ok_or("malformed binary result"))
                    .collect::<std::result::Result<Vec<_>, _>>()
            })
            .transpose()?
            .unwrap_or_default();
        Ok(Self {
            id: value
                .get("id")
                .and_then(Value::as_str)
                .ok_or("gate without id")?
                .to_owned(),
            argv,
            isolated_target_dir: value
                .get("isolated_target_dir")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            exit_code: value
                .get("exit_code")
                .and_then(Value::as_i64)
                .ok_or("gate without exit code")?,
            elapsed_ms: value
                .get("elapsed_ms")
                .and_then(Value::as_u64)
                .ok_or("gate without elapsed_ms")?,
            tests: value.get("tests").and_then(Counts::from_json),
            log_sha256: value
                .get("log_sha256")
                .and_then(Value::as_str)
                .map(str::to_owned),
            binaries,
        })
    }

    fn runs_tests(&self) -> bool {
        matches!(
            self.argv.get(1).map(String::as_str),
            Some("test" | "nextest")
        )
    }
}

/// `cargo <args> => exit code: <n>; elapsed-ms=<ms>[; ...]`, the evidence
/// line every gate writes.
fn parse_evidence_line(line: &str) -> Option<(Vec<String>, i64, u64)> {
    let (command, status) = line.split_once(" => ")?;
    let argv: Vec<String> = command.split(' ').map(str::to_owned).collect();
    if argv.first().map(String::as_str) != Some("cargo") || argv.len() < 2 {
        return None;
    }
    let mut exit_code = None;
    let mut elapsed = None;
    for field in status.split("; ") {
        if let Some(code) = field.strip_prefix("exit code: ") {
            exit_code = code.trim().parse::<i64>().ok();
        } else if let Some(ms) = field.strip_prefix("elapsed-ms=") {
            elapsed = ms.trim().parse::<u64>().ok();
        }
    }
    Some((argv, exit_code?, elapsed?))
}

fn evidence_gate_id(subcommand: &str, taken: &mut BTreeMap<String, usize>) -> String {
    let base = match subcommand {
        "fmt" => "format",
        "deny" => "licenses",
        "clippy" => "clippy",
        "test" | "nextest" => "tests",
        other => other,
    };
    let count = taken.entry(base.to_owned()).or_insert(0);
    *count += 1;
    if *count == 1 {
        base.to_owned()
    } else {
        format!("{base}-{count}")
    }
}

/// Gate records from the evidence lines alone (a gate that keeps no native
/// gate plan, such as the commercial gate): commands, exit codes, durations.
pub fn gates_from_evidence(evidence: &[String]) -> Vec<GateRecord> {
    let mut taken = BTreeMap::new();
    evidence
        .iter()
        .filter_map(|line| parse_evidence_line(line))
        .map(|(argv, exit_code, elapsed_ms)| {
            let (argv, isolated_target_dir) = sanitize_argv(&argv);
            let id = evidence_gate_id(argv.get(1).map_or("", String::as_str), &mut taken);
            GateRecord {
                id,
                argv,
                isolated_target_dir,
                exit_code,
                elapsed_ms,
                tests: None,
                log_sha256: None,
                binaries: Vec::new(),
            }
        })
        .collect()
}

/// Gate records from the native gate plan the Susi gate writes
/// (`native-gates.json`: ids, argv, status, duration, test counts, the log's
/// Git blob). The logs are read back from the object store for the
/// per-binary results.
pub fn gates_from_native(root: &Path, native: &Value) -> Result<Vec<GateRecord>> {
    let gates = native
        .get("gates")
        .and_then(Value::as_array)
        .ok_or("native gate plan has no gates")?;
    let mut records = Vec::with_capacity(gates.len());
    for gate in gates {
        let id = gate
            .get("id")
            .and_then(Value::as_str)
            .ok_or("native gate without id")?;
        let raw: Vec<String> = gate
            .get("argv")
            .and_then(Value::as_array)
            .ok_or("native gate without argv")?
            .iter()
            .map(|arg| {
                arg.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| "non-string native gate argument".into())
            })
            .collect::<Result<Vec<_>>>()?;
        let exit_code = gate
            .get("exit_code")
            .and_then(Value::as_i64)
            .ok_or("native gate without exit code")?;
        let elapsed_ms = gate
            .get("elapsed_ms")
            .and_then(Value::as_u64)
            .ok_or("native gate without elapsed_ms")?;
        let log_sha256 = gate
            .get("log_sha256")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let runs_tests = matches!(raw.get(1).map(String::as_str), Some("test" | "nextest"));
        let tests = runs_tests.then(|| Counts::from_json(gate)).flatten();
        let mut binaries = Vec::new();
        if runs_tests {
            if let (Some(blob), Some(target_dir)) = (
                gate.get("log_blob").and_then(Value::as_str),
                option_value(&raw, "--target-dir"),
            ) {
                if !valid_oid(blob) {
                    return Err(format!("native gate {id} has an invalid log blob").into());
                }
                let log = git_output(root, &["cat-file", "blob", blob], None)?;
                if let Some(expected) = &log_sha256 {
                    if &sha256_hex(&log) != expected {
                        return Err(
                            format!("native gate {id} log blob does not match its digest").into(),
                        );
                    }
                }
                // The gate's working directory is read to resolve
                // cwd-relative `Running` paths; it is never recorded.
                let outer = OuterBinaries {
                    target_dir,
                    triple: option_value(&raw, "--target"),
                    cwd: gate.get("cwd").and_then(Value::as_str),
                };
                let doc_gate = raw.iter().any(|arg| arg == "--doc");
                binaries = parse_binaries(&String::from_utf8_lossy(&log), &outer, doc_gate);
            }
        }
        let (argv, isolated_target_dir) = sanitize_argv(&raw);
        records.push(GateRecord {
            id: id.to_owned(),
            argv,
            isolated_target_dir,
            exit_code,
            elapsed_ms,
            tests,
            log_sha256,
            binaries,
        });
    }
    Ok(records)
}

/// What `rustc -vV` reports, run from `dir` so a pinned toolchain applies.
pub fn rustc_version(dir: &Path) -> Result<(String, String)> {
    let output = Command::new("rustc")
        .current_dir(dir)
        .arg("-vV")
        .output()
        .map_err(|error| format!("rustc -vV in {}: {error}", dir.display()))?;
    if !output.status.success() {
        return Err("rustc -vV failed".into());
    }
    let text = String::from_utf8(output.stdout)?.replace("\r\n", "\n");
    let host = text
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .ok_or("rustc did not report its host")?
        .trim()
        .to_owned();
    Ok((text, host))
}

/// A landing to write the receipt for.
pub struct Landing<'a> {
    /// Repository the objects are read from (any worktree of it).
    pub root: &'a Path,
    /// The verified integration head `H`.
    pub head: &'a str,
    /// The `origin/main` the landing merged onto (`H^1`).
    pub origin_main: &'a str,
    /// Agents and sources of the landing: `[{agent, commit, claim_base, task}]`.
    pub sources: Vec<Value>,
    /// The gate's native gate plan (`native-gates.json`), when it kept one.
    pub native_gates: Option<&'a Value>,
    /// The gate's evidence lines (always present).
    pub evidence: &'a [String],
}

/// Build the receipt of `landing`. Fails, so the landing is not published,
/// when the tree, the lockfile, the toolchain or the gate results cannot be
/// read, or when no gate ran.
pub fn build(landing: &Landing<'_>) -> Result<Value> {
    let Landing {
        root,
        head,
        origin_main,
        sources,
        native_gates,
        evidence,
    } = landing;
    if !valid_oid(head) || !valid_oid(origin_main) {
        return Err("receipt needs full commit ids".into());
    }
    let tree = git(root, &["rev-parse", &format!("{head}^{{tree}}")])?;
    let cargo_lock_sha256 = match tree_blob(root, head, "Cargo.lock")? {
        Some(lock) => Value::String(sha256_hex(&lock)),
        None if tree_blob(root, head, "Cargo.toml")?.is_some() => {
            return Err(format!("{head} has a Cargo.toml but no Cargo.lock to record").into());
        }
        None => Value::Null,
    };
    let (rustc, host) = rustc_version(root)?;
    let gates = match native_gates {
        Some(native) => gates_from_native(root, native)?,
        None => gates_from_evidence(evidence),
    };
    if gates.is_empty() {
        return Err("no gate results to record".into());
    }
    if let Some(failed) = gates.iter().find(|gate| gate.exit_code != 0) {
        return Err(format!("gate {} did not pass; nothing to publish", failed.id).into());
    }
    let selection = evidence
        .iter()
        .rev()
        .find_map(|line| line.strip_prefix("gate-selection="))
        .map(str::to_owned);
    let recorded_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut receipt = json!({
        "schema": SCHEMA,
        "integration": {"commit": head, "tree": tree, "origin_main": origin_main},
        "sources": sources,
        "toolchain": {
            "host": host,
            "rustc": rustc,
            "rustc_sha256": sha256_hex(rustc.as_bytes()),
            "cargo_lock_sha256": cargo_lock_sha256,
        },
        "selection": selection,
        "gates": gates.iter().map(GateRecord::to_json).collect::<Vec<_>>(),
        "recorded_unix": recorded_unix,
    });
    let digest = digest_of(&receipt)?;
    if let Some(object) = receipt.as_object_mut() {
        object.insert("digest".into(), Value::String(digest));
    }
    Ok(receipt)
}

/// The bytes written to `.agents/receipts/<H>.json`: pretty JSON, LF-ended.
pub fn render(receipt: &Value) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(receipt)?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// Write `receipt` as the receipt-only commit `R` on top of `head`, with
/// Git plumbing alone (no worktree is touched): `H`'s tree plus
/// `.agents/receipts/<H>.json`. `index_file` is a scratch path for the
/// temporary index. Returns `R`.
pub fn commit_receipt(
    root: &Path,
    head: &str,
    receipt: &Value,
    message: &str,
    index_file: &Path,
) -> Result<String> {
    if !valid_oid(head) {
        return Err("receipt commit needs a full head id".into());
    }
    let path = path_for(head);
    let bytes = render(receipt)?;
    let blob = String::from_utf8(git_output(
        root,
        &["hash-object", "-w", "--stdin"],
        Some(&bytes),
    )?)?
    .trim()
    .to_owned();
    let _ = fs::remove_file(index_file);
    if let Some(parent) = index_file.parent() {
        fs::create_dir_all(parent)?;
    }
    let env = [("GIT_INDEX_FILE", index_file)];
    let result = (|| -> Result<String> {
        git_in(root, &env, &["read-tree", head])?;
        git_in(
            root,
            &env,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("100644,{blob},{path}"),
            ],
        )?;
        let tree = git_in(root, &env, &["write-tree"])?;
        git_in(
            root,
            &env,
            &[
                "-c",
                "user.name=Susi Local Integration",
                "-c",
                "user.email=susi-local@localhost",
                "-c",
                "commit.gpgsign=false",
                "commit-tree",
                &tree,
                "-p",
                head,
                "-m",
                message,
            ],
        )
    })();
    let _ = fs::remove_file(index_file);
    let commit = result?;
    if !valid_oid(&commit) {
        return Err("commit-tree did not return a commit id".into());
    }
    Ok(commit)
}

/// The receipt commit `R` recorded for verified head `head`, checked to be
/// `head`'s child carrying exactly the receipt file. `Ok(None)` when no
/// receipt commit was recorded (a head verified by an older ship tool).
pub fn receipt_commit(root: &Path, head: &str) -> Result<Option<String>> {
    let reference = ref_for(head);
    let exists = Command::new("git")
        .arg("--literal-pathspecs")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .current_dir(root)
        .args(["show-ref", "--verify", "--quiet", &reference])
        .status()?
        .success();
    if !exists {
        return Ok(None);
    }
    let commit = git(root, &["rev-parse", &format!("{reference}^{{commit}}")])?;
    check_receipt_only(root, head, &commit)?;
    Ok(Some(commit))
}

/// Error unless `commit` is `head`'s child whose only change is
/// `.agents/receipts/<head>.json`, and that file verifies.
pub fn check_receipt_only(root: &Path, head: &str, commit: &str) -> Result<()> {
    let parents = git(root, &["rev-list", "--parents", "-n", "1", commit])?;
    let mut ids = parents.split_whitespace();
    if ids.next() != Some(commit) || ids.next() != Some(head) || ids.next().is_some() {
        return Err(format!("{commit} is not a single-parent child of {head}").into());
    }
    let path = path_for(head);
    let changed = git(
        root,
        &[
            "diff-tree",
            "--no-commit-id",
            "--name-only",
            "-r",
            head,
            commit,
        ],
    )?;
    let changed: Vec<_> = changed.lines().collect();
    if changed != [path.as_str()] {
        return Err(format!(
            "{commit} changes {} instead of only {path}",
            if changed.is_empty() {
                "nothing".to_owned()
            } else {
                changed.join(", ")
            }
        )
        .into());
    }
    let bytes =
        tree_blob(root, commit, &path)?.ok_or("receipt file missing from receipt commit")?;
    let receipt: Value = serde_json::from_slice(&bytes)?;
    check_digest(&receipt)?;
    if receipt
        .get("integration")
        .and_then(|i| i.get("commit"))
        .and_then(Value::as_str)
        != Some(head)
    {
        return Err("receipt does not describe its parent commit".into());
    }
    Ok(())
}

/// Commit message of the receipt commit for `head`.
pub fn commit_message(head: &str, sources: &[Value]) -> String {
    let agents: Vec<&str> = sources
        .iter()
        .filter_map(|s| s.get("agent").and_then(Value::as_str))
        .collect();
    let mut message = format!(
        "receipt: gated landing {head}\n\nGate receipt {} for the landing of {}.\nVerify with `cargo xtask verify-receipt {head}`.\n",
        path_for(head),
        if agents.is_empty() {
            "the integration".to_owned()
        } else {
            agents.join(", ")
        }
    );
    for source in sources {
        let task = source.get("task").and_then(Value::as_str);
        let agent = source.get("agent").and_then(Value::as_str);
        match (task, agent) {
            (Some(task), _) => message.push_str(&format!("\nTask: {task}")),
            (None, Some(agent)) => message.push_str(&format!("\nTask: LOCAL-{agent}")),
            (None, None) => {}
        }
    }
    message.push('\n');
    message
}

// ---------------------------------------------------------------------------
// Verification
// ---------------------------------------------------------------------------

/// Outcome of one check.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Outcome {
    Match,
    Mismatch,
    /// Not checked, with the reason.
    Skipped(String),
}

/// One line of the verification report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Check {
    pub name: String,
    pub recorded: String,
    pub observed: String,
    pub outcome: Outcome,
}

impl Check {
    fn new(
        name: &str,
        recorded: impl Into<String>,
        observed: impl Into<String>,
        matched: bool,
    ) -> Self {
        Self {
            name: name.to_owned(),
            recorded: recorded.into(),
            observed: observed.into(),
            outcome: if matched {
                Outcome::Match
            } else {
                Outcome::Mismatch
            },
        }
    }

    fn skipped(name: &str, recorded: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            name: name.to_owned(),
            recorded: recorded.into(),
            observed: String::new(),
            outcome: Outcome::Skipped(reason.into()),
        }
    }

    fn render(&self) -> String {
        match &self.outcome {
            Outcome::Match => format!(
                "ok        {}: {}",
                self.name,
                if self.observed.is_empty() || self.observed == self.recorded {
                    self.recorded.clone()
                } else {
                    format!("{} (recorded {})", self.observed, self.recorded)
                }
            ),
            Outcome::Mismatch => format!(
                "MISMATCH  {}: observed {}; recorded {}",
                self.name, self.observed, self.recorded
            ),
            Outcome::Skipped(reason) => {
                format!(
                    "skipped   {}: {reason}; recorded {}",
                    self.name, self.recorded
                )
            }
        }
    }
}

/// The verification report: every check, and whether all of them matched.
#[derive(Debug, Default)]
pub struct Report {
    pub checks: Vec<Check>,
}

impl Report {
    pub fn all_match(&self) -> bool {
        self.checks
            .iter()
            .all(|check| check.outcome == Outcome::Match)
    }

    pub fn mismatches(&self) -> usize {
        self.checks
            .iter()
            .filter(|check| check.outcome == Outcome::Mismatch)
            .count()
    }

    pub fn render(&self) -> String {
        let mut out: String = self
            .checks
            .iter()
            .map(|check| format!("{}\n", check.render()))
            .collect();
        let skipped = self
            .checks
            .iter()
            .filter(|check| matches!(check.outcome, Outcome::Skipped(_)))
            .count();
        out.push_str(&format!(
            "{} checks: {} match, {} mismatch, {} skipped\n",
            self.checks.len(),
            self.checks.len() - self.mismatches() - skipped,
            self.mismatches(),
            skipped
        ));
        out
    }
}

/// How `verify` runs.
#[derive(Debug, Default)]
pub struct Options {
    /// Read the receipt from this file instead of the repository.
    pub receipt_path: Option<PathBuf>,
    /// Check the receipt against the repository only; re-run nothing.
    pub static_only: bool,
    /// Keep the scratch worktree and home after the run.
    pub keep: bool,
    /// Re-run only these gate ids (all when empty).
    pub only: Vec<String>,
    /// Re-run even when the local toolchain is not the recorded one.
    pub ignore_toolchain: bool,
    /// Scratch directory root (default `<root>/target/verify-receipt`).
    pub scratch: Option<PathBuf>,
}

/// Compare the recorded gates with observed re-runs. Exit codes, test
/// totals and the per-harness counts must agree; durations are reported,
/// never compared.
pub fn compare_gate(recorded: &GateRecord, observed: &GateRecord) -> Vec<Check> {
    let mut checks = vec![Check::new(
        &format!("gate {} exit code", recorded.id),
        recorded.exit_code.to_string(),
        observed.exit_code.to_string(),
        recorded.exit_code == observed.exit_code,
    )];
    checks.push(Check::skipped(
        &format!("gate {} duration", recorded.id),
        format!("{:.1} s", recorded.elapsed_ms as f64 / 1000.0),
        format!(
            "{:.1} s observed; durations are informational",
            observed.elapsed_ms as f64 / 1000.0
        ),
    ));
    if let Some(tests) = recorded.tests {
        let name = format!("gate {} test totals", recorded.id);
        match observed.tests {
            Some(seen) => checks.push(Check::new(
                &name,
                format!(
                    "{} passed, {} failed, {} ignored",
                    tests.passed, tests.failed, tests.ignored
                ),
                format!(
                    "{} passed, {} failed, {} ignored",
                    seen.passed, seen.failed, seen.ignored
                ),
                tests == seen,
            )),
            None => checks.push(Check::new(
                &name,
                format!(
                    "{} passed, {} failed, {} ignored",
                    tests.passed, tests.failed, tests.ignored
                ),
                "no complete test totals in the re-run log",
                false,
            )),
        }
    }
    if !recorded.binaries.is_empty() {
        let mut want = recorded.binaries.clone();
        let mut have = observed.binaries.clone();
        want.sort();
        have.sort();
        let describe = |list: &[BinaryResult]| {
            let mut text = format!("{} harnesses", list.len());
            let missing: Vec<String> = list
                .iter()
                .map(|b| {
                    format!(
                        "{} ({}): {}",
                        b.name,
                        b.source,
                        b.counts.map_or_else(
                            || "no result".to_owned(),
                            |c| format!("{}/{}/{}", c.passed, c.failed, c.ignored)
                        )
                    )
                })
                .collect();
            if !missing.is_empty() {
                text.push_str(": ");
                text.push_str(&missing.join("; "));
            }
            text
        };
        let differing: Vec<&BinaryResult> = want
            .iter()
            .filter(|w| !have.contains(w))
            .chain(have.iter().filter(|h| !want.contains(h)))
            .collect();
        checks.push(Check::new(
            &format!("gate {} harnesses", recorded.id),
            format!("{} harnesses", want.len()),
            if differing.is_empty() {
                format!("{} harnesses", have.len())
            } else {
                format!(
                    "{} harnesses; differing: {}",
                    have.len(),
                    describe(&differing.into_iter().cloned().collect::<Vec<_>>())
                )
            },
            want == have,
        ));
    }
    checks
}

fn isolated_env(command: &mut Command, home: &Path) {
    for (key, _) in std::env::vars_os() {
        let name = key.to_string_lossy().to_ascii_uppercase();
        if name.starts_with("SUSI_")
            || [
                "TOKEN",
                "SECRET",
                "PASSWORD",
                "CREDENTIAL",
                "API_KEY",
                "ACCESS_KEY",
                "API_BASE",
                "BASE_URL",
                "API_URL",
                "CUSTOM_HEADERS",
                "HTTP_PROXY",
                "HTTPS_PROXY",
                "ALL_PROXY",
            ]
            .iter()
            .any(|part| name.contains(part))
        {
            command.env_remove(key);
        }
    }
    let original_home =
        std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from);
    let cargo_home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| original_home.as_ref().map(|h| h.join(".cargo")));
    let rustup_home = std::env::var_os("RUSTUP_HOME")
        .map(PathBuf::from)
        .or_else(|| original_home.as_ref().map(|h| h.join(".rustup")));
    command
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("SUSI_HOME", home.join("susi"))
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env("APPDATA", home.join("appdata"))
        .env("LOCALAPPDATA", home.join("localappdata"))
        .env("CARGO_TERM_COLOR", "never");
    if let Some(cargo_home) = cargo_home {
        command.env("CARGO_HOME", cargo_home);
    }
    if let Some(rustup_home) = rustup_home {
        command.env("RUSTUP_HOME", rustup_home);
    }
}

/// Re-run one recorded gate in `worktree` with `target_dir`, logging to
/// `log_path`, and return what was observed.
pub fn rerun_gate(
    recorded: &GateRecord,
    worktree: &Path,
    target_dir: &Path,
    home: &Path,
    log_path: &Path,
) -> Result<GateRecord> {
    if recorded.argv.first().map(String::as_str) != Some("cargo") {
        return Err(format!("gate {} is not a cargo command", recorded.id).into());
    }
    let target_text = target_dir
        .to_str()
        .ok_or("target directory must be valid UTF-8")?
        .to_owned();
    let argv = if recorded.isolated_target_dir {
        with_target_dir(&recorded.argv, &target_text)
    } else {
        recorded.argv.clone()
    };
    let _ = fs::remove_dir_all(home);
    fs::create_dir_all(home.join("AppData/Local"))?;
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let log = fs::File::create(log_path)?;
    let mut command = Command::new("cargo");
    isolated_env(&mut command, home);
    // The gate's explicit compiler environment: the selected toolchain's
    // own tools and no inherited wrapper or encoded flags (a wrapper such
    // as sccache cannot start with the home moved).
    crate::compiler_env::apply_checked(&mut command)?;
    command
        .current_dir(worktree)
        .args(&argv[1..])
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log));
    if !recorded.isolated_target_dir {
        command.env("CARGO_TARGET_DIR", &target_text);
    }
    let started = Instant::now();
    let status = command
        .status()
        .map_err(|error| format!("cannot run cargo for gate {}: {error}", recorded.id))?;
    let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let exit_code = i64::from(status.code().unwrap_or(-1));
    let bytes = fs::read(log_path)?;
    let text = String::from_utf8_lossy(&bytes);
    let (tests, binaries) = if recorded.runs_tests() {
        let outer = OuterBinaries {
            target_dir: &target_text,
            triple: option_value(&argv, "--target"),
            cwd: worktree.to_str(),
        };
        let doc_gate = argv.iter().any(|arg| arg == "--doc");
        let binaries = observed_harnesses(recorded, parse_binaries(&text, &outer, doc_gate));
        let tests = if argv.get(1).map(String::as_str) == Some("nextest") {
            nextest_totals(&text)
        } else {
            totals(&binaries)
        };
        (tests, binaries)
    } else {
        (None, Vec::new())
    };
    Ok(GateRecord {
        id: recorded.id.clone(),
        argv: recorded.argv.clone(),
        isolated_target_dir: recorded.isolated_target_dir,
        exit_code,
        elapsed_ms,
        tests,
        log_sha256: Some(sha256_hex(&bytes)),
        binaries,
    })
}

/// `argv` with `--target-dir <dir>` added where Cargo reads it: before the
/// first `--`, after which arguments belong to the tool Cargo runs (clippy's
/// `-- -D warnings`, a test harness's flags).
pub fn with_target_dir(argv: &[String], dir: &str) -> Vec<String> {
    let at = argv
        .iter()
        .position(|arg| arg == "--")
        .unwrap_or(argv.len());
    let mut out = Vec::with_capacity(argv.len() + 2);
    out.extend_from_slice(&argv[..at]);
    out.push("--target-dir".to_owned());
    out.push(dir.to_owned());
    out.extend_from_slice(&argv[at..]);
    out
}

/// Settings that make a checkout byte-identical to the committed blobs
/// whatever the operator's Git configuration says. Git for Windows ships
/// `core.autocrlf=true` system-wide; a verifier run with its home moved (so
/// a user's `autocrlf=false` is not read) then checked the scratch tree out
/// with CRLF, and the GMCP ledger's pinned hashes of vendored sources failed.
/// The ship tool checks its candidate out with the same settings
/// (`CHECKOUT_AS_COMMITTED` in local-workflow).
pub const CHECKOUT_AS_COMMITTED: [&str; 4] = ["-c", "core.autocrlf=false", "-c", "core.eol=lf"];

/// The `git worktree` invocation the verifier uses: no hooks, the committed
/// bytes ([`CHECKOUT_AS_COMMITTED`]), and long paths allowed so a deep clone
/// on Windows can check the tree out (the ship tool's worktree commands pass
/// the same setting).
fn worktree_git<'a>(rest: &[&'a str]) -> Vec<&'a str> {
    let mut args = vec!["-c", "core.hooksPath=", "-c", "core.longpaths=true"];
    args.extend_from_slice(&CHECKOUT_AS_COMMITTED);
    args.push("worktree");
    args.extend_from_slice(rest);
    args
}

/// The harnesses of a re-run that the recorded gate covered. A narrowed
/// test gate builds its binaries with `--no-run` and runs them through the
/// concurrent runner, so its receipt lists no doctests (the separate
/// `doc-tests` gate covers them), while a plain re-run of the same `cargo
/// test` argv prints a trailing doctest phase. When the receipt recorded
/// harnesses but no `doc-tests`, that phase is dropped from the observed
/// list and its totals; a receipt that recorded doctests (the serial
/// whole-workspace gate) is compared in full.
pub fn observed_harnesses(recorded: &GateRecord, observed: Vec<BinaryResult>) -> Vec<BinaryResult> {
    let recorded_doctests = recorded.binaries.iter().any(|b| b.source == "doc-tests");
    if recorded.binaries.is_empty() || recorded_doctests {
        return observed;
    }
    observed
        .into_iter()
        .filter(|b| b.source != "doc-tests")
        .collect()
}

/// `Summary [ 12.345s] 42 tests run: 40 passed, 1 failed, 1 skipped`.
fn nextest_totals(text: &str) -> Option<Counts> {
    let line = strip_ansi(text)
        .lines()
        .rev()
        .find(|line| line.trim_start().starts_with("Summary ["))
        .map(str::to_owned)?;
    let (_, rest) = line.split_once("run: ")?;
    let mut counts = Counts {
        passed: 0,
        failed: 0,
        ignored: 0,
    };
    for field in rest.split(", ") {
        let mut parts = field.trim().splitn(2, ' ');
        let count: u64 = parts.next()?.parse().ok()?;
        let label = parts.next()?;
        if label.starts_with("passed") {
            counts.passed = count;
        } else if label.starts_with("failed") || label.starts_with("timed out") {
            counts.failed = count;
        } else if label.starts_with("skipped") {
            counts.ignored = count;
        }
    }
    Some(counts)
}

fn receipt_from_bytes(bytes: &[u8]) -> Result<Value> {
    Ok(serde_json::from_slice(bytes)?)
}

/// Locate the receipt for `commit`: `options.receipt_path`, else the
/// working tree's `.agents/receipts/<commit>.json`, else `HEAD`'s copy. When
/// `commit` is itself a receipt-only commit (`R`), its parent `H` is the
/// verified head and the receipt is read from `R`'s tree. Returns
/// `(verified head, receipt, where it came from)`.
pub fn locate(root: &Path, commit: &str, options: &Options) -> Result<(String, Value, String)> {
    let resolved = git(
        root,
        &["rev-parse", "--verify", &format!("{commit}^{{commit}}")],
    )?;
    if let Some(path) = &options.receipt_path {
        let receipt = receipt_from_bytes(&fs::read(path)?)?;
        return Ok((resolved, receipt, path.display().to_string()));
    }
    let own = path_for(&resolved);
    let in_tree = root.join(&own);
    if in_tree.is_file() {
        let receipt = receipt_from_bytes(&fs::read(&in_tree)?)?;
        return Ok((resolved, receipt, own));
    }
    if let Some(bytes) = tree_blob(root, "HEAD", &own)? {
        return Ok((resolved, receipt_from_bytes(&bytes)?, format!("HEAD:{own}")));
    }
    // `commit` may be the receipt commit itself.
    if let Ok(parent) = git(root, &["rev-parse", "--verify", &format!("{resolved}^1")]) {
        let parent_path = path_for(&parent);
        if let Some(bytes) = tree_blob(root, &resolved, &parent_path)? {
            check_receipt_only(root, &parent, &resolved)?;
            return Ok((
                parent,
                receipt_from_bytes(&bytes)?,
                format!("{}:{parent_path}", &resolved[..12.min(resolved.len())]),
            ));
        }
    }
    Err(format!(
        "no receipt for {resolved}: expected {own} in the working tree or at HEAD (pass --receipt <file> to supply one)"
    )
    .into())
}

fn string_at<'a>(value: &'a Value, path: &[&str]) -> Option<&'a str> {
    let mut current = value;
    for key in path {
        current = current.get(key)?;
    }
    current.as_str()
}

/// Verify the receipt of `commit` against the repository at `root` and, unless
/// `options.static_only`, re-run its gates in a scratch worktree of the
/// verified head.
pub fn verify(root: &Path, commit: &str, options: &Options) -> Result<Report> {
    let (head, receipt, origin) = locate(root, commit, options)?;
    let mut report = Report::default();
    report.checks.push(Check::new(
        "receipt",
        format!("{} from {origin}", path_for(&head)),
        "",
        true,
    ));
    let schema = receipt.get("schema").and_then(Value::as_str).unwrap_or("");
    report
        .checks
        .push(Check::new("schema", SCHEMA, schema, schema == SCHEMA));
    match check_digest(&receipt) {
        Ok(()) => report.checks.push(Check::new(
            "digest",
            receipt.get("digest").and_then(Value::as_str).unwrap_or(""),
            "",
            true,
        )),
        Err(error) => report.checks.push(Check::new(
            "digest",
            receipt.get("digest").and_then(Value::as_str).unwrap_or(""),
            error.to_string(),
            false,
        )),
    }
    let recorded_commit = string_at(&receipt, &["integration", "commit"]).unwrap_or("");
    report.checks.push(Check::new(
        "integration commit",
        recorded_commit,
        &head,
        recorded_commit == head,
    ));
    let tree = git(root, &["rev-parse", &format!("{head}^{{tree}}")])?;
    let recorded_tree = string_at(&receipt, &["integration", "tree"]).unwrap_or("");
    report.checks.push(Check::new(
        "integration tree",
        recorded_tree,
        &tree,
        recorded_tree == tree,
    ));
    let recorded_main = string_at(&receipt, &["integration", "origin_main"]).unwrap_or("");
    match git(root, &["rev-parse", "--verify", &format!("{head}^1")]) {
        Ok(first_parent) => report.checks.push(Check::new(
            "origin/main merged onto (first parent)",
            recorded_main,
            &first_parent,
            recorded_main == first_parent,
        )),
        Err(_) => report.checks.push(Check::new(
            "origin/main merged onto (first parent)",
            recorded_main,
            "verified head has no parent",
            false,
        )),
    }
    let recorded_lock = receipt
        .get("toolchain")
        .and_then(|t| t.get("cargo_lock_sha256"));
    let lock = tree_blob(root, &head, "Cargo.lock")?;
    let observed_lock = lock.as_deref().map(sha256_hex);
    report.checks.push(Check::new(
        "Cargo.lock sha256",
        recorded_lock
            .and_then(Value::as_str)
            .unwrap_or("null")
            .to_owned(),
        observed_lock.clone().unwrap_or_else(|| "absent".to_owned()),
        match (recorded_lock, observed_lock) {
            (Some(Value::String(recorded)), Some(observed)) => *recorded == observed,
            (Some(Value::Null) | None, None) => true,
            (Some(Value::String(_)), None)
            | (Some(Value::Null) | None, Some(_))
            | (Some(Value::Bool(_) | Value::Number(_) | Value::Array(_) | Value::Object(_)), _) => {
                false
            }
        },
    ));
    // The receipt commit: `H`'s child adding exactly this file.
    let own = path_for(&head);
    let adding = git(
        root,
        &[
            "log",
            "--format=%H",
            "--diff-filter=A",
            "-n",
            "1",
            "HEAD",
            "--",
            &own,
        ],
    )
    .unwrap_or_default();
    if adding.is_empty() {
        report.checks.push(Check::skipped(
            "receipt commit",
            "receipt-only child of the verified head",
            "no commit in HEAD's history adds this receipt",
        ));
    } else {
        match check_receipt_only(root, &head, &adding) {
            Ok(()) => report.checks.push(Check::new(
                "receipt commit",
                "receipt-only child of the verified head",
                &adding,
                true,
            )),
            Err(error) => report.checks.push(Check::new(
                "receipt commit",
                "receipt-only child of the verified head",
                format!("{adding}: {error}"),
                false,
            )),
        }
    }
    let gates = receipt
        .get("gates")
        .and_then(Value::as_array)
        .ok_or("receipt has no gates")?
        .iter()
        .map(GateRecord::from_json)
        .collect::<Result<Vec<_>>>()?;
    report.checks.push(Check::new(
        "gates recorded",
        format!("{} gates, all exit code 0", gates.len()),
        format!(
            "{} gates, {} with exit code 0",
            gates.len(),
            gates.iter().filter(|g| g.exit_code == 0).count()
        ),
        !gates.is_empty() && gates.iter().all(|g| g.exit_code == 0),
    ));
    if options.static_only {
        return Ok(report);
    }
    // Scratch worktree of the verified head.
    let scratch_root = options
        .scratch
        .clone()
        .unwrap_or_else(|| root.join("target").join("verify-receipt"));
    fs::create_dir_all(&scratch_root)?;
    let short = &head[..12.min(head.len())];
    let worktree = scratch_root.join(format!("{short}-tree"));
    let target_dir = scratch_root.join(format!("{short}-target"));
    let home = scratch_root.join(format!("{short}-home"));
    let logs = scratch_root.join(format!("{short}-logs"));
    let worktree_text = worktree
        .to_str()
        .ok_or("scratch path must be valid UTF-8")?
        .to_owned();
    let _ = Command::new("git")
        .current_dir(root)
        .args(worktree_git(&["remove", "--force", &worktree_text]))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = fs::remove_dir_all(&worktree);
    git(
        root,
        &worktree_git(&["add", "--detach", &worktree_text, &head]),
    )?;
    let outcome = rerun_all(
        &receipt,
        &gates,
        &worktree,
        &target_dir,
        &home,
        &logs,
        options,
        &mut report,
    );
    if !options.keep {
        let _ = Command::new("git")
            .current_dir(root)
            .args(worktree_git(&["remove", "--force", &worktree_text]))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = fs::remove_dir_all(&home);
    } else {
        eprintln!(
            "verify-receipt: kept worktree {}, target {}, logs {}",
            worktree.display(),
            target_dir.display(),
            logs.display()
        );
    }
    outcome?;
    Ok(report)
}

#[allow(clippy::too_many_arguments)]
// The re-run owns five scratch paths plus the receipt, its gates and the
// report it appends to; bundling them into a struct would only rename them.
fn rerun_all(
    receipt: &Value,
    gates: &[GateRecord],
    worktree: &Path,
    target_dir: &Path,
    home: &Path,
    logs: &Path,
    options: &Options,
    report: &mut Report,
) -> Result<()> {
    let (rustc, host) = rustc_version(worktree)?;
    let recorded_rustc = string_at(receipt, &["toolchain", "rustc_sha256"]).unwrap_or("");
    let recorded_host = string_at(receipt, &["toolchain", "host"]).unwrap_or("");
    let rustc_sha = sha256_hex(rustc.as_bytes());
    let same_toolchain = recorded_rustc == rustc_sha && recorded_host == host;
    report.checks.push(Check::new(
        "toolchain (rustc -vV sha256, host)",
        format!("{recorded_rustc}, {recorded_host}"),
        format!("{rustc_sha}, {host}"),
        same_toolchain,
    ));
    if !same_toolchain && !options.ignore_toolchain {
        for gate in gates {
            report.checks.push(Check::skipped(
                &format!("gate {}", gate.id),
                format!("exit code {}", gate.exit_code),
                "not re-run: this host's toolchain is not the recorded one (pass --ignore-toolchain to run anyway)",
            ));
        }
        return Ok(());
    }
    for gate in gates {
        if !options.only.is_empty() && !options.only.contains(&gate.id) {
            report.checks.push(Check::skipped(
                &format!("gate {}", gate.id),
                format!("exit code {}", gate.exit_code),
                "not selected by --only",
            ));
            continue;
        }
        eprintln!(
            "verify-receipt: re-running gate {}: {}",
            gate.id,
            gate.argv.join(" ")
        );
        let log_path = logs.join(format!("{}.log", gate.id.replace([':', '/', '\\'], "_")));
        let observed = rerun_gate(gate, worktree, target_dir, home, &log_path)?;
        let checks = compare_gate(gate, &observed);
        for check in &checks {
            eprintln!("verify-receipt: {}", check.render());
        }
        report.checks.extend(checks);
    }
    Ok(())
}

/// `cargo xtask verify-receipt <commit> [--receipt <file>] [--static-only]
/// [--keep] [--only <gate-id>]... [--ignore-toolchain] [--scratch <dir>]`.
/// Prints the report; `Err` when any check mismatches or the run fails.
pub fn run(args: &[String]) -> Result<()> {
    let mut commit = None;
    let mut options = Options::default();
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--receipt" => {
                options.receipt_path = Some(PathBuf::from(
                    rest.next().ok_or("--receipt requires a file")?,
                ));
            }
            "--static-only" => options.static_only = true,
            "--keep" => options.keep = true,
            "--ignore-toolchain" => options.ignore_toolchain = true,
            "--only" => options
                .only
                .push(rest.next().ok_or("--only requires a gate id")?.clone()),
            "--scratch" => {
                options.scratch = Some(PathBuf::from(
                    rest.next().ok_or("--scratch requires a directory")?,
                ));
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown verify-receipt argument: {other}").into());
            }
            other => {
                if commit.replace(other.to_owned()).is_some() {
                    return Err("verify-receipt takes one commit".into());
                }
            }
        }
    }
    let commit = commit.ok_or(
        "usage: cargo xtask verify-receipt <commit> [--receipt <file>] [--static-only] [--keep] [--only <gate-id>]... [--ignore-toolchain] [--scratch <dir>]",
    )?;
    let root = PathBuf::from(git(
        &std::env::current_dir()?,
        &["rev-parse", "--show-toplevel"],
    )?);
    let report = verify(&root, &commit, &options)?;
    print!("{}", report.render());
    if report.all_match() {
        println!("verify-receipt: every check matched");
        Ok(())
    } else if report.mismatches() == 0 {
        println!("verify-receipt: no mismatch; some checks were skipped");
        Ok(())
    } else {
        Err(format!(
            "verify-receipt: {} check(s) did not match",
            report.mismatches()
        )
        .into())
    }
}

/// Every string in `value`, for leak checks.
pub fn strings_in(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(text) => out.push(text.clone()),
        Value::Array(items) => items.iter().for_each(|item| strings_in(item, out)),
        Value::Object(map) => map.values().for_each(|item| strings_in(item, out)),
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

/// Keys of a JSON object in order, for schema checks.
pub fn member_names(value: &Value) -> Vec<String> {
    value
        .as_object()
        .map(Map::keys)
        .map(|keys| keys.cloned().collect())
        .unwrap_or_default()
}

#[cfg(test)]
// Tests report fixture and assertion failures directly; a panic is the test's result.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static SEQUENCE: AtomicUsize = AtomicUsize::new(0);

    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "susi-gate-receipt-{}-{}",
                std::process::id(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn repo(&self) -> PathBuf {
            let repo = self.0.join("repo");
            fs::create_dir(&repo).unwrap();
            git(&repo, &["init", "-q"]).unwrap();
            git(&repo, &["config", "user.name", "Test"]).unwrap();
            git(&repo, &["config", "user.email", "test@example.invalid"]).unwrap();
            git(&repo, &["config", "commit.gpgsign", "false"]).unwrap();
            repo
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            if !std::thread::panicking() {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
    }

    fn commit_file(repo: &Path, path: &str, content: &str, message: &str) -> String {
        let full = repo.join(path);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&full, content).unwrap();
        git(repo, &["add", "--", path]).unwrap();
        git(
            repo,
            &[
                "-c",
                "core.hooksPath=",
                "commit",
                "-q",
                "--no-verify",
                "-m",
                message,
            ],
        )
        .unwrap();
        git(repo, &["rev-parse", "HEAD"]).unwrap()
    }

    /// A repository whose HEAD is a merge of a worker branch onto `main`,
    /// with a `Cargo.toml` and `Cargo.lock`, like a real landing.
    fn landing_repo() -> (Scratch, PathBuf, String, String) {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        commit_file(&repo, "Cargo.toml", "[package]\nname = \"x\"\n", "manifest");
        let base = commit_file(&repo, "Cargo.lock", "# lock\n", "lock");
        git(&repo, &["checkout", "-q", "-b", "worker"]).unwrap();
        commit_file(&repo, "owned.rs", "fn main() {}\n", "worker");
        git(&repo, &["checkout", "-q", "-"]).unwrap();
        git(
            &repo,
            &[
                "-c",
                "core.hooksPath=",
                "merge",
                "--no-ff",
                "--no-edit",
                "-q",
                "worker",
            ],
        )
        .unwrap();
        let head = git(&repo, &["rev-parse", "HEAD"]).unwrap();
        (scratch, repo, base, head)
    }

    fn evidence() -> Vec<String> {
        vec![
            "cargo fmt --all --check => exit code: 0; elapsed-ms=1200".into(),
            "gate-share=format live=1 jobs=8 test-threads=8".into(),
            "cargo deny --offline --locked check licenses => exit code: 0; elapsed-ms=800".into(),
            "cargo clippy -p x --all-targets --offline --locked --target x86_64-pc-windows-msvc --target-dir F:/ops/slot/target -- -D warnings => exit code: 0; elapsed-ms=90000".into(),
            "cargo test -p x --offline --locked --target x86_64-pc-windows-msvc --target-dir F:/ops/slot/target => exit code: 0; elapsed-ms=300000; observed-passes=7".into(),
            "gate-selection=packages:x".into(),
        ]
    }

    const RUNNER_LOG: &str = "     Running unittests src/lib.rs (F:/ops/slot/target/x86_64-pc-windows-msvc/debug/deps/x-0123456789abcdef.exe)\n\nrunning 3 tests\ntest a ... ok\ntest b ... ok\ntest c ... ignored\n\ntest result: ok. 2 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.01s\n\n     Running tests/cli.rs (F:\\ops\\slot\\target\\x86_64-pc-windows-msvc\\debug\\deps\\cli-fedcba9876543210.exe)\n\nrunning 2 tests\n     Running unittests src/main.rs (C:/tmp/fixture/target/debug/deps/fixture-0000000000000000.exe)\nrunning 1 test\ntest nested ... ok\n\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n   Doc-tests fixture\n\nrunning 0 tests\n\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\ntest one ... ok\ntest two ... ok\n\ntest result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.20s\n\n   Doc-tests x\n\nrunning 1 test\ntest src/lib.rs - f (line 3) ... ok\n\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.50s\n";

    #[test]
    fn sanitize_argv_strips_only_the_target_dir() {
        let argv: Vec<String> = [
            "cargo",
            "test",
            "-p",
            "x",
            "--target",
            "t",
            "--target-dir",
            "F:/slot/target",
            "--",
            "--nocapture",
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
        let (clean, stripped) = sanitize_argv(&argv);
        assert!(stripped);
        assert_eq!(
            clean,
            [
                "cargo",
                "test",
                "-p",
                "x",
                "--target",
                "t",
                "--",
                "--nocapture"
            ]
        );
        let (same, stripped) = sanitize_argv(&clean);
        assert!(!stripped);
        assert_eq!(same, clean);
        let eq_form: Vec<String> = vec![
            "cargo".into(),
            "clippy".into(),
            "--target-dir=/x/target".into(),
        ];
        assert_eq!(
            sanitize_argv(&eq_form),
            (vec!["cargo".to_owned(), "clippy".to_owned()], true)
        );
    }

    /// Regression (first receipt, fe3521e36): the serial task gate ran with
    /// its target directory inside the candidate, Cargo printed cwd-relative
    /// `Running` paths, and the receipt recorded 280 passes but no harness.
    #[test]
    fn parse_binaries_reads_cwd_relative_running_paths() {
        let log = "     Running unittests src\\main.rs (target\\check-gate\\x86_64-pc-windows-msvc\\debug\\deps\\xtask-0123456789abcdef.exe)\n\nrunning 2 tests\ntest a ... ok\ntest b ... ok\n\ntest result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n\n     Running tests\\cli.rs (target\\check-gate\\x86_64-pc-windows-msvc\\debug\\deps\\cli-fedcba9876543210.exe)\n\nrunning 1 test\n     Running unittests src/lib.rs (target/debug/deps/fixture-0000000000000000.exe)\ntest c ... ok\n\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n";
        let with_cwd = OuterBinaries {
            target_dir: "F:\\ops\\candidate\\target\\check-gate",
            triple: Some("x86_64-pc-windows-msvc"),
            cwd: Some("F:\\ops\\candidate"),
        };
        let parsed = parse_binaries(log, &with_cwd, false);
        let names: Vec<(&str, &str)> = parsed
            .iter()
            .map(|b| (b.name.as_str(), b.source.as_str()))
            .collect();
        assert_eq!(
            names,
            [
                ("xtask", "unittests src\\main.rs"),
                ("cli", "tests\\cli.rs")
            ]
        );
        assert_eq!(
            totals(&parsed),
            Some(Counts {
                passed: 3,
                failed: 0,
                ignored: 0
            })
        );
        // Without the working directory the relative form is not matched,
        // and a target directory outside the cwd adds no relative form.
        let no_cwd = OuterBinaries {
            cwd: None,
            ..with_cwd
        };
        assert!(parse_binaries(log, &no_cwd, false).is_empty());
        let outside = OuterBinaries {
            target_dir: "F:\\elsewhere\\target",
            triple: Some("x86_64-pc-windows-msvc"),
            cwd: Some("F:\\ops\\candidate"),
        };
        assert_eq!(outside.prefixes().len(), 1);
    }

    #[test]
    fn the_rerun_target_dir_goes_before_the_tool_arguments() {
        let clippy: Vec<String> = [
            "cargo",
            "clippy",
            "-p",
            "x",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
        assert_eq!(
            with_target_dir(&clippy, "T"),
            [
                "cargo",
                "clippy",
                "-p",
                "x",
                "--all-targets",
                "--target-dir",
                "T",
                "--",
                "-D",
                "warnings"
            ]
        );
        let test: Vec<String> = ["cargo", "test", "-p", "x"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        assert_eq!(
            with_target_dir(&test, "T"),
            ["cargo", "test", "-p", "x", "--target-dir", "T"]
        );
        // The receipt's argv round-trips: sanitizing the re-run argv gives it back.
        assert_eq!(
            sanitize_argv(&with_target_dir(&clippy, "T")),
            (clippy, true)
        );
        assert_eq!(
            worktree_git(&["add", "--detach", "p", "h"]),
            [
                "-c",
                "core.hooksPath=",
                "-c",
                "core.longpaths=true",
                "-c",
                "core.autocrlf=false",
                "-c",
                "core.eol=lf",
                "worktree",
                "add",
                "--detach",
                "p",
                "h"
            ]
        );
    }

    /// Regression (first full verification of fe3521e36): with the home
    /// moved, Git read only the system `core.autocrlf=true`, the scratch tree
    /// was checked out with CRLF, and the GMCP ledger's pinned source hashes
    /// failed 40 tests that passed in the gate.
    #[test]
    fn the_scratch_checkout_keeps_the_committed_bytes_under_autocrlf() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        let head = commit_file(&repo, "spec.mdx", "one\ntwo\n", "spec");
        git(&repo, &["config", "core.autocrlf", "true"]).unwrap();
        let converted = scratch.0.join("converted");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "--detach",
                converted.to_str().unwrap(),
                &head,
            ],
        )
        .unwrap();
        assert_eq!(
            fs::read(converted.join("spec.mdx")).unwrap(),
            b"one\r\ntwo\r\n",
            "the fixture must reproduce the conversion"
        );
        let exact = scratch.0.join("exact");
        let exact_text = exact.to_str().unwrap().to_owned();
        git(
            &repo,
            &worktree_git(&["add", "--detach", &exact_text, &head]),
        )
        .unwrap();
        assert_eq!(fs::read(exact.join("spec.mdx")).unwrap(), b"one\ntwo\n");
    }

    #[test]
    fn parse_binaries_reads_outer_harnesses_and_skips_nested_fixture_output() {
        let outer = OuterBinaries {
            target_dir: "F:\\ops\\slot\\target",
            triple: Some("x86_64-pc-windows-msvc"),
            cwd: None,
        };
        let binaries = parse_binaries(RUNNER_LOG, &outer, false);
        let summary: Vec<(String, String, Option<Counts>)> = binaries
            .iter()
            .map(|b| (b.name.clone(), b.source.clone(), b.counts))
            .collect();
        assert_eq!(
            summary,
            vec![
                (
                    "x".into(),
                    "unittests src/lib.rs".into(),
                    Some(Counts {
                        passed: 2,
                        failed: 0,
                        ignored: 1
                    })
                ),
                (
                    "cli".into(),
                    "tests/cli.rs".into(),
                    Some(Counts {
                        passed: 2,
                        failed: 0,
                        ignored: 0
                    })
                ),
                (
                    "x".into(),
                    "doc-tests".into(),
                    Some(Counts {
                        passed: 1,
                        failed: 0,
                        ignored: 0
                    })
                ),
            ]
        );
        assert_eq!(
            totals(&binaries),
            Some(Counts {
                passed: 5,
                failed: 0,
                ignored: 1
            })
        );
        // A crashed harness has no result line.
        let crashed = "     Running unittests src/lib.rs (F:/ops/slot/target/x86_64-pc-windows-msvc/debug/deps/x-0123456789abcdef.exe)\nrunning 1 test\n";
        let parsed = parse_binaries(crashed, &outer, false);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].counts, None);
        assert_eq!(totals(&parsed), None);
        // Another target directory is not this gate's binary.
        let elsewhere = OuterBinaries {
            target_dir: "/other",
            triple: None,
            cwd: None,
        };
        assert!(parse_binaries(crashed, &elsewhere, false).is_empty());
    }

    #[test]
    fn evidence_lines_become_gate_records_without_operator_paths() {
        let gates = gates_from_evidence(&evidence());
        let ids: Vec<&str> = gates.iter().map(|g| g.id.as_str()).collect();
        assert_eq!(ids, ["format", "licenses", "clippy", "tests"]);
        assert_eq!(gates[3].exit_code, 0);
        assert_eq!(gates[3].elapsed_ms, 300_000);
        assert!(gates[3].isolated_target_dir);
        assert!(!gates[0].isolated_target_dir);
        for gate in &gates {
            assert!(
                !gate.argv.iter().any(|arg| arg.contains("F:/ops")),
                "{:?}",
                gate.argv
            );
            assert!(!gate.argv.iter().any(|arg| arg == "--target-dir"));
        }
        assert_eq!(gates[2].argv.last().map(String::as_str), Some("warnings"));
        let twice: Vec<String> = evidence().into_iter().chain(evidence()).collect();
        let ids: Vec<String> = gates_from_evidence(&twice)
            .into_iter()
            .map(|g| g.id)
            .collect();
        assert_eq!(
            ids,
            [
                "format",
                "licenses",
                "clippy",
                "tests",
                "format-2",
                "licenses-2",
                "clippy-2",
                "tests-2"
            ]
        );
    }

    #[test]
    fn digest_binds_the_body_and_detects_tampering() {
        let (_scratch, repo, base, head) = landing_repo();
        let landing = Landing {
            root: &repo,
            head: &head,
            origin_main: &base,
            sources: vec![
                json!({"agent": "one", "commit": head, "claim_base": base, "task": "T-ONE-1"}),
            ],
            native_gates: None,
            evidence: &evidence(),
        };
        let receipt = build(&landing).unwrap();
        check_digest(&receipt).unwrap();
        assert_eq!(receipt["schema"], SCHEMA);
        assert_eq!(receipt["integration"]["commit"], head);
        assert_eq!(receipt["integration"]["origin_main"], base);
        assert_eq!(
            receipt["toolchain"]["cargo_lock_sha256"],
            sha256_hex(b"# lock\n")
        );
        assert_eq!(
            receipt["toolchain"]["rustc_sha256"],
            sha256_hex(receipt["toolchain"]["rustc"].as_str().unwrap().as_bytes())
        );
        assert!(receipt["toolchain"]["host"].as_str().unwrap().contains('-'));
        assert_eq!(receipt["selection"], "packages:x");
        assert_eq!(receipt["gates"].as_array().unwrap().len(), 4);
        // Round trip through the written bytes.
        let parsed: Value = serde_json::from_slice(&render(&receipt).unwrap()).unwrap();
        check_digest(&parsed).unwrap();
        // Any change to the body is detected.
        let mut tampered = receipt.clone();
        tampered["gates"][3]["exit_code"] = json!(1);
        assert!(check_digest(&tampered)
            .unwrap_err()
            .to_string()
            .contains("digest"));
        let mut tampered = receipt.clone();
        tampered["integration"]["commit"] = json!(base);
        assert!(check_digest(&tampered).is_err());
        let mut wrong_schema = receipt.clone();
        wrong_schema["schema"] = json!("other");
        assert!(check_digest(&wrong_schema).is_err());
        // No operator path or name beyond the host triple.
        let mut strings = Vec::new();
        strings_in(&receipt, &mut strings);
        for text in &strings {
            assert!(
                !text.contains("F:/ops") && !text.contains("F:\\ops"),
                "{text}"
            );
            assert!(!text.contains(&repo.display().to_string()), "{text}");
        }
    }

    #[test]
    fn build_refuses_a_failed_gate_a_missing_lockfile_and_an_empty_plan() {
        let (_scratch, repo, base, head) = landing_repo();
        let mut failed = evidence();
        failed[0] = "cargo fmt --all --check => exit code: 1; elapsed-ms=1".into();
        let landing = Landing {
            root: &repo,
            head: &head,
            origin_main: &base,
            sources: vec![],
            native_gates: None,
            evidence: &failed,
        };
        assert!(build(&landing)
            .unwrap_err()
            .to_string()
            .contains("did not pass"));
        let empty: Vec<String> = vec!["gate-selection=workspace:x".into()];
        let landing = Landing {
            root: &repo,
            head: &head,
            origin_main: &base,
            sources: vec![],
            native_gates: None,
            evidence: &empty,
        };
        assert!(build(&landing)
            .unwrap_err()
            .to_string()
            .contains("no gate results"));
        // A manifest without a lockfile is refused; a tree without either records null.
        git(&repo, &["rm", "-q", "Cargo.lock"]).unwrap();
        git(
            &repo,
            &[
                "-c",
                "core.hooksPath=",
                "commit",
                "-q",
                "--no-verify",
                "-m",
                "drop lock",
            ],
        )
        .unwrap();
        let no_lock = git(&repo, &["rev-parse", "HEAD"]).unwrap();
        let landing = Landing {
            root: &repo,
            head: &no_lock,
            origin_main: &head,
            sources: vec![],
            native_gates: None,
            evidence: &evidence(),
        };
        assert!(build(&landing)
            .unwrap_err()
            .to_string()
            .contains("no Cargo.lock"));
        git(&repo, &["rm", "-q", "Cargo.toml"]).unwrap();
        git(
            &repo,
            &[
                "-c",
                "core.hooksPath=",
                "commit",
                "-q",
                "--no-verify",
                "-m",
                "drop manifest",
            ],
        )
        .unwrap();
        let bare = git(&repo, &["rev-parse", "HEAD"]).unwrap();
        let landing = Landing {
            root: &repo,
            head: &bare,
            origin_main: &no_lock,
            sources: vec![],
            native_gates: None,
            evidence: &evidence(),
        };
        assert_eq!(
            build(&landing).unwrap()["toolchain"]["cargo_lock_sha256"],
            Value::Null
        );
    }

    #[test]
    fn native_gates_carry_counts_and_harnesses_from_the_retained_log() {
        let (_scratch, repo, base, head) = landing_repo();
        let log_blob = String::from_utf8(
            git_output(
                &repo,
                &["hash-object", "-w", "--stdin"],
                Some(RUNNER_LOG.as_bytes()),
            )
            .unwrap(),
        )
        .unwrap()
        .trim()
        .to_owned();
        let test_argv = [
            "cargo",
            "test",
            "-p",
            "x",
            "--offline",
            "--locked",
            "--target",
            "x86_64-pc-windows-msvc",
            "--target-dir",
            "F:/ops/slot/target",
        ];
        let native = json!({
            "plan": [{"id": "format", "argv": ["cargo", "fmt", "--all", "--check"]}, {"id": "workspace-tests", "argv": test_argv}],
            "gates": [
                {"id": "format", "argv": ["cargo", "fmt", "--all", "--check"], "cwd": "F:/ops/candidate", "exit_code": 0, "elapsed_ms": 1200, "passed": 0, "failed": 0, "ignored": 0, "log_blob": log_blob, "log_sha256": sha256_hex(RUNNER_LOG.as_bytes())},
                {"id": "workspace-tests", "argv": test_argv, "cwd": "F:/ops/candidate", "exit_code": 0, "elapsed_ms": 300000, "passed": 5, "failed": 0, "ignored": 1, "log_blob": log_blob, "log_sha256": sha256_hex(RUNNER_LOG.as_bytes())}
            ]
        });
        let landing = Landing {
            root: &repo,
            head: &head,
            origin_main: &base,
            sources: vec![],
            native_gates: Some(&native),
            evidence: &evidence(),
        };
        let receipt = build(&landing).unwrap();
        check_digest(&receipt).unwrap();
        let gates: Vec<GateRecord> = receipt["gates"]
            .as_array()
            .unwrap()
            .iter()
            .map(|g| GateRecord::from_json(g).unwrap())
            .collect();
        assert_eq!(gates.len(), 2);
        assert_eq!(gates[0].tests, None);
        assert!(gates[0].binaries.is_empty());
        assert_eq!(
            gates[1].tests,
            Some(Counts {
                passed: 5,
                failed: 0,
                ignored: 1
            })
        );
        assert_eq!(gates[1].binaries.len(), 3);
        assert!(gates[1].isolated_target_dir);
        assert!(!gates[1].argv.iter().any(|a| a.contains("F:/ops")));
        let mut strings = Vec::new();
        strings_in(&receipt, &mut strings);
        assert!(
            strings
                .iter()
                .all(|s| !s.contains("F:/ops") && !s.contains("candidate")),
            "{strings:?}"
        );
        // A log blob that does not match its recorded digest is refused.
        let mut forged = native.clone();
        forged["gates"][1]["log_sha256"] = json!(sha256_hex(b"other"));
        let landing = Landing {
            root: &repo,
            head: &head,
            origin_main: &base,
            sources: vec![],
            native_gates: Some(&forged),
            evidence: &evidence(),
        };
        assert!(build(&landing)
            .unwrap_err()
            .to_string()
            .contains("does not match its digest"));
    }

    #[test]
    fn receipt_commit_is_the_heads_child_with_only_the_receipt_file() {
        let (scratch, repo, base, head) = landing_repo();
        let landing = Landing {
            root: &repo,
            head: &head,
            origin_main: &base,
            sources: vec![
                json!({"agent": "one", "commit": head, "claim_base": base, "task": Value::Null}),
            ],
            native_gates: None,
            evidence: &evidence(),
        };
        let receipt = build(&landing).unwrap();
        let message = commit_message(&head, &landing.sources);
        assert!(message.contains("Task: LOCAL-one"), "{message}");
        let commit =
            commit_receipt(&repo, &head, &receipt, &message, &scratch.0.join("index")).unwrap();
        check_receipt_only(&repo, &head, &commit).unwrap();
        assert_eq!(
            git(&repo, &["rev-parse", &format!("{commit}^1")]).unwrap(),
            head
        );
        let stored: Value = serde_json::from_slice(
            &tree_blob(&repo, &commit, &path_for(&head))
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(stored, receipt);
        assert!(!scratch.0.join("index").exists());
        // The working tree and HEAD were not touched.
        assert_eq!(git(&repo, &["rev-parse", "HEAD"]).unwrap(), head);
        assert!(git(&repo, &["status", "--porcelain"]).unwrap().is_empty());
        // Not a receipt-only commit: another file changed.
        let extra = commit_file(&repo, "other.rs", "", "extra");
        assert!(check_receipt_only(&repo, &head, &extra)
            .unwrap_err()
            .to_string()
            .contains("instead of only"));
        // The ref lookup returns the recorded commit and refuses a wrong one.
        assert_eq!(receipt_commit(&repo, &head).unwrap(), None);
        git(&repo, &["update-ref", &ref_for(&head), &commit]).unwrap();
        assert_eq!(receipt_commit(&repo, &head).unwrap(), Some(commit.clone()));
        git(&repo, &["update-ref", &ref_for(&head), &extra]).unwrap();
        assert!(receipt_commit(&repo, &head).is_err());
    }

    #[test]
    fn verify_static_checks_detect_a_mismatched_receipt() {
        let (scratch, repo, base, head) = landing_repo();
        let landing = Landing {
            root: &repo,
            head: &head,
            origin_main: &base,
            sources: vec![],
            native_gates: None,
            evidence: &evidence(),
        };
        let receipt = build(&landing).unwrap();
        let commit =
            commit_receipt(&repo, &head, &receipt, "receipt", &scratch.0.join("index")).unwrap();
        git(&repo, &["reset", "-q", "--hard", &commit]).unwrap();
        let options = Options {
            static_only: true,
            ..Options::default()
        };
        // By the verified head, by the receipt commit, and by a short id.
        for spec in [head.clone(), commit.clone(), head[..10].to_owned()] {
            let report = verify(&repo, &spec, &options).unwrap();
            assert!(report.all_match(), "{spec}: {}", report.render());
            assert!(report.render().contains("receipt commit"));
        }
        // A tampered copy supplied by path: digest, tree and lockfile mismatch.
        let mut tampered = receipt.clone();
        tampered["integration"]["tree"] = json!("0".repeat(40));
        tampered["toolchain"]["cargo_lock_sha256"] = json!(sha256_hex(b"forged"));
        let path = scratch.0.join("tampered.json");
        fs::write(&path, render(&tampered).unwrap()).unwrap();
        let options = Options {
            static_only: true,
            receipt_path: Some(path),
            ..Options::default()
        };
        let report = verify(&repo, &head, &options).unwrap();
        assert!(!report.all_match());
        let mismatched: Vec<&str> = report
            .checks
            .iter()
            .filter(|c| c.outcome == Outcome::Mismatch)
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(
            mismatched,
            ["digest", "integration tree", "Cargo.lock sha256"]
        );
        // A receipt for another commit does not describe this one.
        assert!(verify(
            &repo,
            &base,
            &Options {
                static_only: true,
                ..Options::default()
            }
        )
        .is_err());
    }

    #[test]
    fn compare_gate_reports_exit_code_total_and_harness_differences() {
        let recorded = GateRecord {
            id: "workspace-tests".into(),
            argv: vec!["cargo".into(), "test".into()],
            isolated_target_dir: true,
            exit_code: 0,
            elapsed_ms: 10,
            tests: Some(Counts {
                passed: 5,
                failed: 0,
                ignored: 1,
            }),
            log_sha256: None,
            binaries: vec![
                BinaryResult {
                    name: "x".into(),
                    source: "unittests src/lib.rs".into(),
                    counts: Some(Counts {
                        passed: 2,
                        failed: 0,
                        ignored: 1,
                    }),
                },
                BinaryResult {
                    name: "cli".into(),
                    source: "tests/cli.rs".into(),
                    counts: Some(Counts {
                        passed: 3,
                        failed: 0,
                        ignored: 0,
                    }),
                },
            ],
        };
        let same = GateRecord {
            elapsed_ms: 99,
            ..recorded.clone()
        };
        let checks = compare_gate(&recorded, &same);
        assert!(
            checks
                .iter()
                .filter(|c| c.outcome == Outcome::Match)
                .count()
                == 3,
            "{checks:?}"
        );
        assert!(
            matches!(checks[1].outcome, Outcome::Skipped(_)),
            "durations are informational"
        );
        let mut different = recorded.clone();
        different.exit_code = 101;
        different.tests = Some(Counts {
            passed: 4,
            failed: 1,
            ignored: 1,
        });
        different.binaries[1].counts = Some(Counts {
            passed: 2,
            failed: 1,
            ignored: 0,
        });
        let checks = compare_gate(&recorded, &different);
        let mismatched: Vec<&str> = checks
            .iter()
            .filter(|c| c.outcome == Outcome::Mismatch)
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(
            mismatched,
            [
                "gate workspace-tests exit code",
                "gate workspace-tests test totals",
                "gate workspace-tests harnesses"
            ]
        );
        let harness = checks
            .iter()
            .find(|c| c.name.ends_with("harnesses"))
            .unwrap();
        assert!(
            harness.observed.contains("cli (tests/cli.rs): 2/1/0"),
            "{}",
            harness.observed
        );
        // A missing harness in the re-run is a mismatch too.
        let mut fewer = recorded.clone();
        fewer.binaries.pop();
        let checks = compare_gate(&recorded, &fewer);
        assert!(checks
            .iter()
            .any(|c| c.name.ends_with("harnesses") && c.outcome == Outcome::Mismatch));
    }

    #[test]
    fn rerun_gate_executes_the_recorded_command_in_isolation() {
        let scratch = Scratch::new();
        let worktree = scratch.0.join("tree");
        fs::create_dir_all(&worktree).unwrap();
        // `cargo --version` needs no project and no network.
        let recorded = GateRecord {
            id: "probe".into(),
            argv: vec!["cargo".into(), "--version".into()],
            isolated_target_dir: false,
            exit_code: 0,
            elapsed_ms: 1,
            tests: None,
            log_sha256: None,
            binaries: Vec::new(),
        };
        let observed = rerun_gate(
            &recorded,
            &worktree,
            &scratch.0.join("target"),
            &scratch.0.join("home"),
            &scratch.0.join("logs/probe.log"),
        )
        .unwrap();
        assert_eq!(observed.exit_code, 0);
        assert!(fs::read_to_string(scratch.0.join("logs/probe.log"))
            .unwrap()
            .starts_with("cargo "));
        assert!(compare_gate(&recorded, &observed)
            .iter()
            .all(|c| c.outcome != Outcome::Mismatch));
        let failing = GateRecord {
            argv: vec!["cargo".into(), "no-such-subcommand-for-receipts".into()],
            ..recorded.clone()
        };
        let observed = rerun_gate(
            &failing,
            &worktree,
            &scratch.0.join("target"),
            &scratch.0.join("home"),
            &scratch.0.join("logs/fail.log"),
        )
        .unwrap();
        assert_ne!(observed.exit_code, 0);
        assert_eq!(
            compare_gate(&failing, &observed)[0].outcome,
            Outcome::Mismatch
        );
    }

    #[test]
    fn a_rerun_doctest_phase_is_dropped_when_the_gate_ran_binaries_only() {
        let outer = OuterBinaries {
            target_dir: "F:\\ops\\slot\\target",
            triple: Some("x86_64-pc-windows-msvc"),
            cwd: None,
        };
        let observed = parse_binaries(RUNNER_LOG, &outer, false);
        assert!(observed.iter().any(|b| b.source == "doc-tests"));
        let binaries_only = GateRecord {
            id: "workspace-tests".into(),
            argv: vec!["cargo".into(), "test".into()],
            isolated_target_dir: true,
            exit_code: 0,
            elapsed_ms: 1,
            tests: Some(Counts {
                passed: 4,
                failed: 0,
                ignored: 1,
            }),
            log_sha256: None,
            binaries: observed
                .iter()
                .filter(|b| b.source != "doc-tests")
                .cloned()
                .collect(),
        };
        let kept = observed_harnesses(&binaries_only, observed.clone());
        assert_eq!(kept.len(), 2);
        assert_eq!(totals(&kept), binaries_only.tests);
        let rerun = GateRecord {
            tests: totals(&kept),
            binaries: kept,
            ..binaries_only.clone()
        };
        assert!(compare_gate(&binaries_only, &rerun)
            .iter()
            .all(|c| c.outcome != Outcome::Mismatch));
        // A receipt that recorded doctests keeps them; an evidence-only
        // receipt (no harnesses) compares nothing.
        let with_docs = GateRecord {
            binaries: observed.clone(),
            ..binaries_only.clone()
        };
        assert_eq!(observed_harnesses(&with_docs, observed.clone()).len(), 3);
        let evidence_only = GateRecord {
            binaries: Vec::new(),
            ..binaries_only
        };
        assert_eq!(
            observed_harnesses(&evidence_only, observed.clone()),
            observed
        );
    }

    #[test]
    fn nextest_summary_counts_are_read() {
        let text = "   Summary [  12.345s] 42 tests run: 40 passed, 1 failed, 1 skipped\n";
        assert_eq!(
            nextest_totals(text),
            Some(Counts {
                passed: 40,
                failed: 1,
                ignored: 1
            })
        );
        assert_eq!(nextest_totals("nothing"), None);
    }

    #[test]
    fn run_rejects_unknown_arguments_and_needs_a_commit() {
        assert!(run(&[]).unwrap_err().to_string().contains("usage"));
        assert!(run(&["abc".into(), "--bogus".into()])
            .unwrap_err()
            .to_string()
            .contains("unknown"));
        assert!(run(&["abc".into(), "def".into()])
            .unwrap_err()
            .to_string()
            .contains("one commit"));
    }
}
