//! Native ownership evidence. Object hashes bind facts; the independently
//! supplied permit hash and coordinator hash establish authority. Candidate
//! files and refs never select their own trust roots.
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

#[path = "native_json.rs"]
mod native_json;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
pub const POLICY: &str = "susi-native-provenance-policy-v1\nindependent-immutable-permit-and-coordinator\nforward-adoption-preserves-incomplete-historical-ownership\ncomplete-reservation-task-source-tree-ancestry-and-scope\nexact-gate-plan-success-and-nonempty-tests\ncontent-addressed-logs-and-claims\ngit-gc-rooted-immutable-evidence-objects\n";
pub const RECEIPT_SCHEMA: &str = "susi-native-provenance-receipt-v1";
pub const PERMIT_SCHEMA: &str = "susi-native-provenance-permit-v1";

pub fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn file_digest(path: &Path) -> Result<String> {
    Ok(digest(&fs::read(path)?))
}

pub fn policy_digest() -> String {
    digest(POLICY.as_bytes())
}

fn string<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing string field {field}").into())
}

fn array<'a>(value: &'a Value, field: &str) -> Result<&'a Vec<Value>> {
    value
        .get(field)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("missing array field {field}").into())
}

fn strings(value: &Value, field: &str) -> Result<Vec<String>> {
    array(value, field)?
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("non-string {field}").into())
        })
        .collect()
}

fn oid(value: &str) -> Result<()> {
    if !matches!(value.len(), 40 | 64)
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err("expected full lowercase Git object ID".into());
    }
    Ok(())
}

fn sha(value: &str) -> Result<()> {
    oid(value)?;
    if value.len() != 64 {
        return Err("expected SHA-256".into());
    }
    Ok(())
}

fn git_bytes(root: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("git")
        .arg("--literal-pathspecs")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .current_dir(root)
        .args(args)
        .output()
        .map_err(|error| {
            format!(
                "provenance git {} in {}: {error}",
                args.join(" "),
                root.display()
            )
        })?;
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
    Ok(String::from_utf8(git_bytes(root, args)?)?.trim().to_owned())
}

pub fn require_unmodified_history(root: &Path) -> Result<()> {
    if std::env::var_os("GIT_GRAFT_FILE").is_some() {
        return Err("native provenance refuses a GIT_GRAFT_FILE override".into());
    }
    let path = git(root, &["rev-parse", "--git-path", "info/grafts"])?;
    let path = Path::new(&path);
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        root.join(path)
    };
    if fs::symlink_metadata(path).is_ok() {
        return Err(
            "native provenance refuses legacy graft files; immutable object ancestry is required"
                .into(),
        );
    }
    Ok(())
}

pub fn store_blob(root: &Path, bytes: &[u8]) -> Result<String> {
    let mut child = Command::new("git")
        .arg("--literal-pathspecs")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .current_dir(root)
        .args(["hash-object", "-w", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or("missing blob input")?
        .write_all(bytes)?;
    let output = child.wait_with_output()?;
    if !output.status.success() {
        // Keep Git's own reason: a bare "cannot retain" left a failed ship
        // (2026-11-06) with nothing to diagnose its root cause from.
        return Err(format!(
            "cannot retain native evidence blob in {}: git hash-object -w {}: {}",
            root.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    let object = String::from_utf8(output.stdout)?.trim().to_owned();
    oid(&object)?;
    // Git does not trace object IDs embedded in JSON. Every evidence object
    // therefore has its own immutable direct ref, including failed attempts.
    let reference = format!("refs/susi/evidence/objects/{object}");
    let symbolic = Command::new("git")
        .arg("--literal-pathspecs")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .current_dir(root)
        .args(["symbolic-ref", "--quiet", &reference])
        .output()?;
    match symbolic.status.code() {
        Some(1) => {}
        Some(0) => return Err("symbolic durable evidence reference cannot be replaced".into()),
        _ => return Err("cannot inspect durable evidence reference kind".into()),
    }
    let existing = git(
        root,
        &[
            "for-each-ref",
            "--format=%(objectname) %(symref)",
            &reference,
        ],
    )?;
    if !existing.is_empty() {
        if existing != object {
            return Err("conflicting or symbolic durable evidence reference".into());
        }
        return Ok(object);
    }
    let zero = "0".repeat(object.len());
    if let Err(error) = git(
        root,
        &["update-ref", "--no-deref", &reference, &object, &zero],
    ) {
        // Concurrent writers may preserve identical immutable evidence. A
        // different object or symbolic redirect never authorizes replacement.
        if git(
            root,
            &[
                "for-each-ref",
                "--format=%(objectname) %(symref)",
                &reference,
            ],
        )? != object
        {
            return Err(error);
        }
    }
    Ok(object)
}

fn blob(root: &Path, object: &str) -> Result<Vec<u8>> {
    oid(object)?;
    git_bytes(root, &["cat-file", "blob", object])
}

/// `(path, blob oid)` for every blob under `prefix` in `commit`'s tree, from a
/// single `git ls-tree` (no per-file `rev-parse`).
fn tree_blobs(root: &Path, commit: &str, prefix: &str) -> Result<Vec<(String, String)>> {
    oid(commit)?;
    let listing = git_bytes(root, &["ls-tree", "-r", "-z", commit, "--", prefix])?;
    let mut entries = Vec::new();
    for record in listing.split(|byte| *byte == 0).filter(|r| !r.is_empty()) {
        let record = std::str::from_utf8(record)?;
        let (meta, path) = record
            .split_once('\t')
            .ok_or("malformed git ls-tree record")?;
        let mut fields = meta.split(' ');
        let (_mode, kind, object) = (
            fields.next().ok_or("ls-tree mode")?,
            fields.next().ok_or("ls-tree type")?,
            fields.next().ok_or("ls-tree object")?,
        );
        if kind != "blob" {
            continue;
        }
        oid(object)?;
        entries.push((path.to_owned(), object.to_owned()));
    }
    Ok(entries)
}

/// The contents of `objects` (blob oids) from one `git cat-file --batch`
/// process, in order. Each reply header must name the requested oid as a blob
/// of the stated size, so a missing or substituted object fails closed.
fn blobs(root: &Path, objects: &[String]) -> Result<Vec<Vec<u8>>> {
    if objects.is_empty() {
        return Ok(Vec::new());
    }
    for object in objects {
        oid(object)?;
    }
    let mut child = Command::new("git")
        .arg("--literal-pathspecs")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .current_dir(root)
        .args(["cat-file", "--batch"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|error| format!("provenance git cat-file --batch: {error}"))?;
    let mut stdin = child.stdin.take().ok_or("git cat-file stdin")?;
    let request: String = objects.iter().map(|object| format!("{object}\n")).collect();
    // Write from a thread: git streams replies while it reads requests, and a
    // full stdout pipe would otherwise deadlock a large request.
    let writer = std::thread::spawn(move || {
        use std::io::Write as _;
        stdin.write_all(request.as_bytes())
    });
    let output = child.wait_with_output()?;
    writer
        .join()
        .map_err(|_| "git cat-file writer panicked")??;
    if !output.status.success() {
        return Err(format!(
            "git cat-file --batch: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    let data = output.stdout;
    let mut at = 0;
    let mut contents = Vec::with_capacity(objects.len());
    for object in objects {
        let end = data[at..]
            .iter()
            .position(|byte| *byte == b'\n')
            .ok_or("truncated git cat-file reply")?
            + at;
        let header = std::str::from_utf8(&data[at..end])?;
        let mut fields = header.split(' ');
        if fields.next() != Some(object.as_str()) || fields.next() != Some("blob") {
            return Err(format!("git cat-file did not return blob {object}: {header}").into());
        }
        let size: usize = fields
            .next()
            .ok_or("git cat-file reply without size")?
            .parse()?;
        let body = end + 1;
        if data.len() < body + size + 1 || data[body + size] != b'\n' {
            return Err("truncated git cat-file blob".into());
        }
        contents.push(data[body..body + size].to_vec());
        at = body + size + 1;
    }
    if at != data.len() {
        return Err("unexpected trailing git cat-file output".into());
    }
    Ok(contents)
}

fn evidence_blob(root: &Path, object: &str) -> Result<Vec<u8>> {
    oid(object)?;
    let reference = format!("refs/susi/evidence/objects/{object}");
    if git(
        root,
        &[
            "for-each-ref",
            "--format=%(objectname) %(symref)",
            &reference,
        ],
    )? != object
    {
        return Err(format!(
            "durable direct evidence reference missing or substituted: {reference}"
        )
        .into());
    }
    blob(root, object)
}

fn tree(root: &Path, commit: &str) -> Result<String> {
    oid(commit)?;
    git(
        root,
        &["rev-parse", "--verify", &format!("{commit}^{{tree}}")],
    )
}

fn ancestor(root: &Path, before: &str, after: &str) -> Result<()> {
    oid(before)?;
    oid(after)?;
    let status = Command::new("git")
        .arg("--literal-pathspecs")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .current_dir(root)
        .args(["merge-base", "--is-ancestor", before, after])
        .status()?;
    if !status.success() {
        return Err(format!("unrelated provenance history: {before} -> {after}").into());
    }
    Ok(())
}

fn normalized(value: &str) -> Result<String> {
    if value.is_empty()
        || !value.is_ascii()
        || value.contains('\\')
        || value.starts_with('/')
        || value.ends_with('/')
    {
        return Err("invalid native scope path".into());
    }
    for part in value.split('/') {
        if part.is_empty()
            || matches!(part, "." | "..")
            || part.eq_ignore_ascii_case(".git")
            || part.ends_with('.')
            || part.ends_with(' ')
            || part
                .bytes()
                .any(|b| b.is_ascii_control() || b":*?\"<>|".contains(&b))
        {
            return Err("unsafe native scope path".into());
        }
    }
    Ok(value.to_ascii_lowercase())
}

fn scope_set(values: &[String]) -> Result<BTreeSet<String>> {
    let result: BTreeSet<_> = values
        .iter()
        .map(|v| normalized(v))
        .collect::<Result<_>>()?;
    if result.is_empty() || result.len() != values.len() {
        return Err("empty or duplicate native scopes".into());
    }
    Ok(result)
}

fn owns(scopes: &BTreeSet<String>, path: &str) -> bool {
    scopes
        .iter()
        .any(|scope| path == scope || path.strip_prefix(scope).is_some_and(|s| s.starts_with('/')))
}

fn changed_paths(root: &Path, before: &str, after: &str) -> Result<Vec<String>> {
    oid(before)?;
    oid(after)?;
    git_bytes(
        root,
        &["diff", "--no-renames", "--name-only", "-z", before, after],
    )?
    .split(|b| *b == 0)
    .filter(|p| !p.is_empty())
    .map(|p| normalized(std::str::from_utf8(p)?))
    .collect()
}

fn scoped_diff(root: &Path, before: &str, after: &str, scopes: &BTreeSet<String>) -> Result<()> {
    for path in changed_paths(root, before, after)? {
        if !owns(scopes, &path) {
            return Err(format!("unscoped provenance change: {path}").into());
        }
    }
    Ok(())
}

fn task_id(id: &str) -> Result<()> {
    let parts: Vec<_> = id.split('-').collect();
    if parts.len() != 3
        || parts[0] != "T"
        || parts[1].is_empty()
        || parts[2].is_empty()
        || !parts[1]
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        || !parts[2].bytes().all(|b| b.is_ascii_digit())
    {
        return Err("task must have namespaced T-<AGENT>-<number> identity".into());
    }
    Ok(())
}

fn acceptance(argv: &[String]) -> Result<()> {
    if argv.len() < 2
        || argv[0] != "cargo"
        || argv[1] != "test"
        || argv.iter().any(|arg| {
            matches!(arg.as_str(), "--" | "--list" | "--no-run")
                || arg.starts_with("--no-run=")
                || arg.starts_with("--list=")
        })
    {
        return Err("native task acceptance must execute cargo test, without --list/--no-run or a test-harness separator".into());
    }
    Ok(())
}

fn task_acceptance(argv: &[String]) -> Result<Vec<String>> {
    acceptance(argv)?;
    let mut manifests = Vec::new();
    let mut rest = argv[2..].iter();
    while let Some(argument) = rest.next() {
        match argument.as_str() {
            "--offline" | "--locked" | "--workspace" | "--all-targets" | "--all-features"
            | "--no-default-features" | "--lib" | "--tests" | "--bins" | "--examples" => {}
            "-p" | "--package" | "--bin" | "--test" | "--example" | "--features" => {
                let value = rest.next().ok_or("missing task acceptance option value")?;
                if value.is_empty() || value.starts_with('-') { return Err("invalid task acceptance option value".into()); }
            }
            "--manifest-path" => {
                let path = rest.next().ok_or("missing task manifest path")?;
                normalized(path)?;
                if !path.ends_with("Cargo.toml") { return Err("task acceptance requires an in-tree Cargo manifest".into()); }
                manifests.push(path.clone());
            }
            value if !value.starts_with('-') => {}
            _ => return Err("task acceptance cannot override Cargo configuration, toolchain, target, output directory or test execution".into()),
        }
    }
    Ok(manifests)
}

#[derive(Clone)]
pub struct Task {
    pub id: String,
    pub path: String,
    pub blob: String,
    pub argv: Vec<String>,
}

fn regular_manifest(root: &Path, commit: &str, manifest: &str) -> Result<()> {
    let entry = git(root, &["ls-tree", "--full-tree", commit, "--", manifest])?;
    let (metadata, path) = entry
        .split_once('\t')
        .ok_or("task manifest is absent from identified tree")?;
    if path != manifest
        || (!metadata.starts_with("100644 blob ") && !metadata.starts_with("100755 blob "))
    {
        return Err(
            "task acceptance manifest is not the exact regular file in its identified tree".into(),
        );
    }
    Ok(())
}

pub fn task_for_claim(root: &Path, source: &str, worker: &str, scopes: &[String]) -> Result<Task> {
    oid(source)?;
    let held = scope_set(scopes)?;
    let mut found = None;
    // One ls-tree and one batched cat-file instead of two git processes per
    // task record: ~400 records cost 76 s of process launches on Windows.
    let records: Vec<(String, String)> = tree_blobs(root, source, ".agents/tasks/")?
        .into_iter()
        .filter(|(path, _)| {
            path.starts_with(".agents/tasks/T-")
                && path.ends_with(".json")
                && !path[14..].contains('/')
        })
        .collect();
    let contents = blobs(
        root,
        &records
            .iter()
            .map(|(_, object)| object.clone())
            .collect::<Vec<_>>(),
    )?;
    for ((path, object), bytes) in records.into_iter().zip(contents) {
        let path = path.as_str();
        let value = native_json::from_slice(&bytes)?;
        if value.get("native_worker").and_then(Value::as_str) != Some(worker) {
            continue;
        }
        if found.is_some() {
            return Err("multiple tasks claim one native worker".into());
        }
        let id = string(&value, "id")?;
        task_id(id)?;
        if path != format!(".agents/tasks/{id}.json")
            || scope_set(&strings(&value, "scopes")?)? != held
        {
            return Err("task path/scopes do not match the actual native reservation".into());
        }
        if matches!(
            value.get("status").and_then(Value::as_str),
            Some("done" | "closed")
        ) {
            return Err("native task was already closed before acceptance".into());
        }
        let argv = strings(value.get("accept").ok_or("task has no acceptance")?, "cmd")?;
        for manifest in task_acceptance(&argv)? {
            regular_manifest(root, source, &manifest)?;
        }
        found = Some(Task {
            id: id.into(),
            path: path.into(),
            blob: object,
            argv,
        });
    }
    found.ok_or_else(|| format!("worker {worker} has no actual namespaced native task with acceptance and exact reservation scopes").into())
}

#[path = "native_test_counts.rs"]
mod native_test_counts;
use native_test_counts::test_counts;

/// Retain the exact plan and Cargo bytes before any test binary is launched.
pub fn retain_binary_plan(root: &Path, plan: &Value, raw: &[u8], metadata: &[u8]) -> Result<Value> {
    Ok(json!({"schema":super::test_binary_evidence::SCHEMA,
        "plan_blob":store_blob(root,&serde_json::to_vec(plan)?)?,
        "raw_blob":store_blob(root,raw)?,"metadata_blob":store_blob(root,metadata)?}))
}

pub fn finish_binary_plan(
    root: &Path,
    descriptor: &mut Value,
    execution: &super::test_binary_evidence::Execution,
) -> Result<()> {
    execution.recheck()?;
    let plan = evidence_blob(root, string(descriptor, "plan_blob")?)?;
    if plan != serde_json::to_vec(&execution.plan)? {
        return Err("retained binary plan was substituted".into());
    }
    descriptor["result_blob"] = json!(store_blob(root, &serde_json::to_vec(&execution.result)?)?);
    Ok(())
}

fn verify_binary_execution(root: &Path, gate: &Value, gates: &[Value]) -> Result<()> {
    let descriptor = gate.get("execution").ok_or("missing binary execution")?;
    if descriptor["schema"] != super::test_binary_evidence::SCHEMA
        || gate["id"] != "workspace-tests"
    {
        return Err("unknown native binary execution mode or gate".into());
    }
    let plan: Value =
        serde_json::from_slice(&evidence_blob(root, string(descriptor, "plan_blob")?)?)?;
    let result: Value =
        serde_json::from_slice(&evidence_blob(root, string(descriptor, "result_blob")?)?)?;
    let raw = evidence_blob(root, string(descriptor, "raw_blob")?)?;
    let metadata = evidence_blob(root, string(descriptor, "metadata_blob")?)?;
    let docs = super::test_binary_evidence::verify_plan(
        &plan,
        &result,
        &raw,
        &metadata,
        &strings(gate, "argv")?,
        Path::new(string(gate, "cwd")?),
    )?;
    let companions: Vec<_> = gates.iter().filter(|g| g["id"] == "doc-tests").collect();
    if companions.len() != 1
        || strings(companions[0], "argv")? != docs
        || companions[0].get("execution").is_some()
        || companions[0]["exit_code"] != 0
    {
        return Err("native binary execution omitted or weakened doctests".into());
    }
    Ok(())
}

pub struct GateExecution<'a> {
    pub id: &'a str,
    pub argv: &'a [String],
    pub status: std::process::ExitStatus,
    pub elapsed_ms: u128,
    pub log: &'a Path,
}

pub fn gate(root: &Path, execution: GateExecution<'_>) -> Result<Value> {
    let GateExecution {
        id,
        argv,
        status,
        elapsed_ms,
        log,
    } = execution;
    let bytes = fs::read(log)?;
    let log_blob = store_blob(root, &bytes)?;
    let (passed, failed, ignored) = test_counts(&bytes, argv, root)?;
    if !status.success() || status.code() != Some(0) {
        return Err(format!("gate {id} did not exit successfully").into());
    }
    let doc = argv.iter().any(|arg| arg == "--doc");
    if matches!(argv.get(1).map(String::as_str), Some("test" | "nextest"))
        && (failed != 0 || (!doc && passed == 0))
    {
        return Err(format!("gate {id} did not execute nonempty passing tests").into());
    }
    Ok(
        json!({"id":id,"argv":argv,"cwd":root,"exit_code":0,"elapsed_ms":elapsed_ms,
        "passed":passed,"failed":failed,"ignored":ignored,
        "log_blob":log_blob,"log_sha256":digest(&bytes)}),
    )
}

pub struct IntegrationSource<'a> {
    pub worker: &'a str,
    pub claim_bytes: &'a [u8],
    pub source: &'a str,
    pub scope_base: &'a str,
    pub previous: Option<&'a str>,
    pub accepted: &'a str,
}

pub fn receipt(root: &Path, input: IntegrationSource<'_>, gates: &Value) -> Result<Value> {
    let IntegrationSource {
        worker,
        claim_bytes,
        source,
        scope_base,
        previous,
        accepted,
    } = input;
    let text = std::str::from_utf8(claim_bytes)?;
    let fields: Vec<_> = text.lines().collect();
    if fields.len() < 5 || fields[0] != "susi-local-claim-v2" || fields[1] != worker {
        return Err("invalid actual native reservation".into());
    }
    let scopes: Vec<_> = fields[4..].iter().map(|s| (*s).to_owned()).collect();
    let task = task_for_claim(root, source, worker, &scopes)?;
    let task_at_acceptance = git(root, &["rev-parse", &format!("{accepted}:{}", task.path)])?;
    if task_at_acceptance != task.blob {
        return Err("task changed between source and integrated acceptance".into());
    }
    for manifest in task_acceptance(&task.argv)? {
        regular_manifest(root, accepted, &manifest)?;
    }
    Ok(json!({"schema":RECEIPT_SCHEMA,"worker":worker,
        "coordinator_sha256":file_digest(&std::env::current_exe()?)?,
        "policy_sha256":policy_digest(),
        "claim":{"blob":store_blob(root,claim_bytes)?,"base":fields[3],"scope_base":scope_base,"scopes":scopes},
        "task":{"id":task.id,"path":task.path,"blob":task.blob,"accept":task.argv},
        "source":{"commit":source,"tree":tree(root,source)?},
        "integration":{"previous":previous,"commit":accepted,"tree":tree(root,accepted)?},
        "gate_plan":gates.get("plan").ok_or("missing actual gate plan")?,
        "gates":gates.get("gates").ok_or("missing actual gates")?}))
}

fn verify_gates(root: &Path, receipt: &Value, task: &Task, tasks: &BTreeSet<String>) -> Result<()> {
    let plan = array(receipt, "gate_plan")?;
    let gates = array(receipt, "gates")?;
    if plan.len() != gates.len() || gates.len() < 5 {
        return Err("incomplete native integration gate plan".into());
    }
    let integrated = string(
        receipt.get("integration").ok_or("missing integration")?,
        "commit",
    )?;
    let product = Command::new("git")
        .arg("--literal-pathspecs")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .current_dir(root)
        .args([
            "cat-file",
            "-e",
            &format!("{integrated}:crates/susi-core/Cargo.toml"),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?
        .success();
    let mut ids = BTreeSet::new();
    let mut task_seen = false;
    for (expected, gate) in plan.iter().zip(gates) {
        let id = string(gate, "id")?;
        let argv = strings(gate, "argv")?;
        if !ids.insert(id) || string(expected, "id")? != id || strings(expected, "argv")? != argv {
            return Err("missing, extra, duplicate or substituted native gate".into());
        }
        if gate.get("exit_code").and_then(Value::as_i64) != Some(0)
            || gate.get("elapsed_ms").and_then(Value::as_u64).is_none()
        {
            return Err("failed or malformed native gate".into());
        }
        let log = evidence_blob(root, string(gate, "log_blob")?)?;
        if digest(&log) != string(gate, "log_sha256")? {
            return Err("native gate log digest mismatch".into());
        }
        let (passed, failed, ignored) = test_counts(&log, &argv, Path::new(string(gate, "cwd")?))?;
        if gate.get("passed").and_then(Value::as_u64) != Some(passed)
            || gate.get("failed").and_then(Value::as_u64) != Some(failed)
            || gate.get("ignored").and_then(Value::as_u64) != Some(ignored)
            || failed != 0
        {
            return Err("native gate test counts disagree with retained log".into());
        }
        if argv.first().map(String::as_str) != Some("cargo") {
            return Err("unknown native gate executable".into());
        }
        if (gate.get("execution").is_some() || expected.get("execution").is_some())
            && argv.get(1).map(String::as_str) != Some("test")
        {
            return Err("execution mode attached to a non-test native gate".into());
        }
        if argv.get(1).map(String::as_str) == Some("test") {
            if gate.get("execution").is_some() {
                if expected["execution"] != json!({"schema":super::test_binary_evidence::SCHEMA}) {
                    return Err("unplanned native binary execution mode".into());
                }
                verify_binary_execution(root, gate, gates)?;
                acceptance(&super::test_binary_evidence::test_argv(&argv)?)?;
            } else {
                if expected.get("execution").is_some() {
                    return Err("native gate lost its planned execution mode".into());
                }
                acceptance(&argv)?;
            }
            if passed == 0 && !argv.iter().any(|arg| arg == "--doc") {
                return Err("zero executed native tests".into());
            }
        }
        if argv.get(1).map(String::as_str) == Some("nextest") {
            if argv.get(2).map(String::as_str) != Some("run")
                || argv.iter().any(|arg| {
                    matches!(
                        arg.as_str(),
                        "--" | "--list" | "--no-run" | "-E" | "--filterset"
                    ) || arg.starts_with("--filterset=")
                        || arg.starts_with("--partition")
                })
            {
                return Err("native nextest gate must run the whole selection unfiltered".into());
            }
            if passed == 0 {
                return Err("zero executed native tests".into());
            }
        }
        if id == format!("task:{}", task.id) {
            if argv.len() != task.argv.len() + 4
                || !argv.starts_with(&task.argv)
                || argv[task.argv.len()] != "--target"
                || argv[task.argv.len() + 2] != "--target-dir"
            {
                return Err("task acceptance command was substituted".into());
            }
            task_seen = true;
        }
    }
    for id in ["format", "licenses", "clippy", "workspace-tests"] {
        if !ids.contains(id) {
            return Err(format!("missing required native gate {id}").into());
        }
    }
    if !task_seen {
        return Err("task acceptance was not executed on integrated tree".into());
    }
    let mut expected_ids: BTreeSet<String> = ["format", "licenses", "clippy", "workspace-tests"]
        .iter()
        .map(|id| (*id).into())
        .collect();
    expected_ids.extend(tasks.iter().map(|task| format!("task:{task}")));
    if product {
        expected_ids.extend((4..14).map(|index| format!("integration:{index}")));
    }
    // A nextest workspace gate skips doctests, so it must be accompanied by
    // the `doc-tests` gate; a `cargo test` workspace gate runs them itself.
    let nextest_workspace = gates.iter().any(|gate| {
        gate["id"] == "workspace-tests"
            && strings(gate, "argv")
                .is_ok_and(|argv| argv.get(1).map(String::as_str) == Some("nextest"))
    });
    let binary_workspace = gates
        .iter()
        .any(|gate| gate["id"] == "workspace-tests" && gate.get("execution").is_some());
    if nextest_workspace || binary_workspace {
        expected_ids.insert("doc-tests".into());
    }
    if ids
        .iter()
        .map(|id| (*id).to_owned())
        .collect::<BTreeSet<_>>()
        != expected_ids
    {
        return Err(
            "native gate plan does not exactly cover its required integration and task gates"
                .into(),
        );
    }
    let required: [(&str, &[&str]); 4] = [
        ("format", &["cargo", "fmt", "--all", "--check"]),
        (
            "licenses",
            &[
                "cargo",
                "deny",
                "--offline",
                "--locked",
                "check",
                "licenses",
            ],
        ),
        (
            "clippy",
            &[
                "cargo",
                "clippy",
                "--workspace",
                "--all-targets",
                "--offline",
                "--locked",
            ],
        ),
        (
            "workspace-tests",
            &["cargo", "test", "--workspace", "--offline", "--locked"],
        ),
    ];
    let mut required: Vec<(&str, &[&str])> = required.to_vec();
    if nextest_workspace {
        for entry in &mut required {
            if entry.0 == "workspace-tests" {
                entry.1 = &[
                    "cargo",
                    "nextest",
                    "run",
                    "--workspace",
                    "--offline",
                    "--locked",
                ];
            }
        }
    }
    if nextest_workspace || binary_workspace {
        required.push((
            "doc-tests",
            &[
                "cargo",
                "test",
                "--doc",
                "--workspace",
                "--offline",
                "--locked",
            ],
        ));
    }
    for (id, prefix) in required {
        let entry = gates
            .iter()
            .find(|v| v.get("id").and_then(Value::as_str) == Some(id))
            .ok_or("missing gate")?;
        let argv = strings(entry, "argv")?;
        if !argv
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .starts_with(prefix)
        {
            return Err("required gate command weakened".into());
        }
        let mut suffix: Vec<_> = argv[prefix.len()..].iter().map(String::as_str).collect();
        if matches!(id, "clippy" | "workspace-tests" | "doc-tests") {
            if suffix.len() < 4
                || suffix[0] != "--target"
                || suffix[2] != "--target-dir"
                || suffix[1].is_empty()
                || suffix[3].is_empty()
            {
                return Err("required gate lost its exact isolated compiler arguments".into());
            }
            suffix.drain(..4);
            if product {
                if !suffix.starts_with(&["--features", "xtask/release-tooling"]) {
                    return Err(
                        "product integration omitted native release-tooling tests or lint".into(),
                    );
                }
                suffix.drain(..2);
            }
        }
        let expected_suffix: &[&str] = if id == "workspace-tests" && binary_workspace {
            &["--no-run", "--message-format=json"]
        } else if id == "clippy" {
            &["--", "-D", "warnings"]
        } else {
            &[]
        };
        if suffix != expected_suffix {
            return Err("required gate contains a filter or weakened command".into());
        }
    }
    if product {
        let workspace = gates
            .iter()
            .find(|gate| gate["id"] == "workspace-tests")
            .ok_or("missing workspace gate")?;
        let argv = strings(workspace, "argv")?;
        let target = argv
            .windows(2)
            .find(|pair| pair[0] == "--target")
            .ok_or("missing target")?[1]
            .clone();
        let target_dir = argv
            .windows(2)
            .find(|pair| pair[0] == "--target-dir")
            .ok_or("missing target directory")?[1]
            .clone();
        let cwd = string(workspace, "cwd")?;
        let patch_path = Path::new(cwd)
            .join("vendor/idna-adapter")
            .to_string_lossy()
            .replace('\\', "/");
        let patch = format!(
            "patch.crates-io.idna_adapter.path=\"{}\"",
            patch_path.replace('"', "\\\"")
        );
        for (index, name) in [
            "idna-adapter",
            "unicode-ident",
            "htmlescape",
            "regex-syntax",
            "regex-syntax/os-unicode",
        ]
        .iter()
        .enumerate()
        {
            let manifest = format!("vendor/{name}/Cargo.toml");
            git(
                root,
                &["cat-file", "-e", &format!("{integrated}:{manifest}")],
            )?;
            let lock = vec![
                "cargo".into(),
                "generate-lockfile".into(),
                "--offline".into(),
                "--manifest-path".into(),
                manifest.clone(),
                "--config".into(),
                patch.clone(),
            ];
            let tests = vec![
                "cargo".into(),
                "test".into(),
                "--offline".into(),
                "--locked".into(),
                "--manifest-path".into(),
                manifest,
                "--config".into(),
                patch.clone(),
                "--target".into(),
                target.clone(),
                "--target-dir".into(),
                Path::new(&target_dir)
                    .join("platform-adapters")
                    .join(name)
                    .to_string_lossy()
                    .into_owned(),
            ];
            for (offset, expected) in [(0, lock), (1, tests)] {
                let id = format!("integration:{}", 4 + index * 2 + offset);
                let record = gates
                    .iter()
                    .find(|gate| gate["id"] == id)
                    .ok_or("missing mandatory adapter gate")?;
                if strings(record, "argv")? != expected || string(record, "cwd")? != cwd {
                    return Err("mandatory adapter gate command or checkout was substituted".into());
                }
            }
        }
    }
    Ok(())
}

/// Verify only against trust pins supplied independently of the candidate.
/// The caller must retain the immutable permit in its trusted external registry.
pub struct TrustPins<'a> {
    pub permit_sha256: &'a str,
    pub verifier_sha256: &'a str,
    pub policy_sha256: &'a str,
}

pub fn verify(root: &Path, permit_path: &Path, pins: &TrustPins<'_>, head: &str) -> Result<Value> {
    let permit_sha256 = pins.permit_sha256;
    let verifier_sha256 = pins.verifier_sha256;
    let policy_sha256 = pins.policy_sha256;
    require_unmodified_history(root)?;
    for hash in [permit_sha256, verifier_sha256, policy_sha256] {
        sha(hash)?;
    }
    oid(head)?;
    let canonical_root = root.canonicalize()?;
    if !fs::symlink_metadata(permit_path)?.file_type().is_file()
        || permit_path.canonicalize()?.starts_with(&canonical_root)
    {
        return Err("trusted permit must be a regular file outside candidate source".into());
    }
    let bytes = fs::read(permit_path)?;
    if digest(&bytes) != permit_sha256 {
        return Err("independently pinned permit digest mismatch".into());
    }
    if file_digest(&std::env::current_exe()?)? != verifier_sha256
        || policy_digest() != policy_sha256
    {
        return Err("independently pinned verifier or policy digest mismatch".into());
    }
    let permit = native_json::from_slice(&bytes)?;
    if string(&permit, "schema")? != PERMIT_SCHEMA
        || string(&permit, "verifier_sha256")? != verifier_sha256
        || string(&permit, "policy_sha256")? != policy_sha256
    {
        return Err("unsupported or substituted trusted provenance permit".into());
    }
    let source = permit.get("source").ok_or("permit source missing")?;
    if string(source, "commit")? != head || string(source, "tree")? != tree(root, head)? {
        return Err("permit authorizes another source commit/tree".into());
    }
    let baseline = permit
        .get("baseline")
        .ok_or("forward adoption baseline missing")?;
    let base = string(baseline, "commit")?;
    if string(baseline, "tree")? != tree(root, base)?
        || string(baseline, "historical_ownership")? != "incomplete"
    {
        return Err("forward adoption must preserve incomplete historical ownership".into());
    }
    ancestor(root, base, head)?;
    let diagnostics = evidence_blob(root, string(baseline, "diagnostics_blob")?)?;
    if diagnostics.is_empty() || digest(&diagnostics) != string(baseline, "diagnostics_sha256")? {
        return Err("historical diagnostic evidence missing or altered".into());
    }
    let receipt_oids = strings(&permit, "receipts")?;
    let unique: BTreeSet<_> = receipt_oids.iter().collect();
    if receipt_oids.is_empty() || unique.len() != receipt_oids.len() {
        return Err("missing or duplicate trusted receipt objects".into());
    }
    let mut covered = BTreeSet::new();
    let mut accepted = BTreeSet::new();
    let mut scopes = BTreeSet::new();
    let mut summaries = Vec::new();
    let mut identities = BTreeSet::new();
    let mut integration_tasks: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for object in &receipt_oids {
        let record = native_json::from_slice(&evidence_blob(root, object)?)?;
        let integrated = string(
            record.get("integration").ok_or("missing integration")?,
            "commit",
        )?;
        let task = string(record.get("task").ok_or("missing task")?, "id")?;
        if !integration_tasks
            .entry(integrated.into())
            .or_default()
            .insert(task.into())
        {
            return Err("duplicate task in one native integration".into());
        }
    }
    for receipt_oid in &receipt_oids {
        let receipt = native_json::from_slice(&evidence_blob(root, receipt_oid)?)?;
        if string(&receipt, "schema")? != RECEIPT_SCHEMA
            || string(&receipt, "coordinator_sha256")? != verifier_sha256
            || string(&receipt, "policy_sha256")? != policy_sha256
        {
            return Err("untrusted coordinator, policy or legacy integration receipt".into());
        }
        let worker = string(&receipt, "worker")?;
        let claim = receipt.get("claim").ok_or("missing actual claim")?;
        let claim_data = evidence_blob(root, string(claim, "blob")?)?;
        let claim_text = std::str::from_utf8(&claim_data)?;
        let fields: Vec<_> = claim_text.lines().collect();
        let held = strings(claim, "scopes")?;
        let held_set = scope_set(&held)?;
        if fields.len() < 5
            || fields[0] != "susi-local-claim-v2"
            || fields[1] != worker
            || fields[3] != string(claim, "base")?
            || scope_set(
                &fields[4..]
                    .iter()
                    .map(|v| (*v).to_owned())
                    .collect::<Vec<_>>(),
            )? != held_set
        {
            return Err("native reservation scope or owner was substituted".into());
        }
        let worker_source = receipt.get("source").ok_or("receipt source missing")?;
        let commit = string(worker_source, "commit")?;
        let integration = receipt
            .get("integration")
            .ok_or("receipt integration missing")?;
        let integrated = string(integration, "commit")?;
        if string(worker_source, "tree")? != tree(root, commit)?
            || string(integration, "tree")? != tree(root, integrated)?
        {
            return Err("receipt source or accepted tree mismatch".into());
        }
        ancestor(root, base, string(claim, "base")?)?;
        ancestor(root, string(claim, "base")?, commit)?;
        ancestor(root, commit, integrated)?;
        ancestor(root, integrated, head)?;
        let scope_base = string(claim, "scope_base")?;
        ancestor(root, string(claim, "base")?, scope_base)?;
        ancestor(root, scope_base, commit)?;
        let previous = integration
            .get("previous")
            .and_then(Value::as_str)
            .unwrap_or(base);
        ancestor(root, base, previous)?;
        ancestor(root, previous, integrated)?;
        ancestor(root, scope_base, previous)?;
        scoped_diff(root, scope_base, commit, &held_set)?;
        let task = task_for_claim(root, commit, worker, &held)?;
        let recorded_task = receipt.get("task").ok_or("receipt task missing")?;
        if string(recorded_task, "id")? != task.id
            || string(recorded_task, "path")? != task.path
            || string(recorded_task, "blob")? != task.blob
            || strings(recorded_task, "accept")? != task.argv
            || git(root, &["rev-parse", &format!("{integrated}:{}", task.path)])? != task.blob
        {
            return Err("task identity, source acceptance or integrated task changed".into());
        }
        for manifest in task_acceptance(&task.argv)? {
            regular_manifest(root, integrated, &manifest)?;
        }
        if !identities.insert((worker.to_owned(), commit.to_owned(), task.id.clone())) {
            return Err("reused native task receipt".into());
        }
        verify_gates(
            root,
            &receipt,
            &task,
            integration_tasks
                .get(integrated)
                .ok_or("missing integration task set")?,
        )?;
        for introduced in git(root, &["rev-list", &format!("{scope_base}..{commit}")])?.lines() {
            // Net-zero changes must not conceal an unscoped intermediate commit.
            ancestor(root, scope_base, introduced)?;
            let parents = git(root, &["show", "-s", "--format=%P", introduced])?;
            for parent in parents.split_whitespace() {
                if ancestor(root, scope_base, parent).is_ok() {
                    scoped_diff(root, parent, introduced, &held_set)?;
                }
            }
            covered.insert(introduced.to_owned());
        }
        accepted.insert(integrated.to_owned());
        scopes.extend(held_set);
        summaries.push(json!({"receipt":receipt_oid,"worker":worker,"task":task.id,"source":worker_source,"integration":integration,"claim_blob":string(claim,"blob")?}));
    }
    if !accepted.contains(head) {
        return Err("candidate has no exact-head accepted integration receipt".into());
    }
    let mut admitted_merges = BTreeSet::new();
    for commit in git(
        root,
        &[
            "rev-list",
            "--reverse",
            "--topo-order",
            &format!("{base}..{head}"),
        ],
    )?
    .lines()
    {
        if covered.contains(commit) {
            continue;
        }
        let parents = git(root, &["show", "-s", "--format=%P", commit])?;
        let parents: Vec<_> = parents.split_whitespace().collect();
        if parents.len() < 2 {
            return Err(format!("unowned source commit outside adopted history: {commit}").into());
        }
        // Intermediate serialized integration merges must join only covered
        // source/accepted history and may not introduce unreserved resolution edits.
        for parent in &parents {
            if !covered.contains(*parent)
                && !admitted_merges.contains(*parent)
                && ancestor(root, parent, base).is_err()
            {
                return Err("uncovered integration merge parent".into());
            }
        }
        scoped_diff(root, parents[0], commit, &scopes)?;
        admitted_merges.insert(commit.to_owned());
    }
    if fs::read(permit_path)? != bytes {
        return Err("trusted permit changed during verification".into());
    }
    Ok(
        json!({"schema":"susi-native-provenance-report-v1","verified":true,"permit_sha256":permit_sha256,"verifier_sha256":verifier_sha256,
        "policy_sha256":policy_sha256,"source":source,"baseline":baseline,"historical_ownership":"incomplete",
        "acceptance":"forward-revalidated","receipts":summaries}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_failed_blob_write_reports_gits_reason() {
        let outside =
            std::env::temp_dir().join(format!("susi-store-blob-not-a-repo-{}", std::process::id()));
        std::fs::create_dir_all(&outside).unwrap();
        let error = store_blob(&outside, b"evidence").unwrap_err().to_string();
        let _ = std::fs::remove_dir_all(&outside);
        assert!(
            error.starts_with("cannot retain native evidence blob"),
            "{error}"
        );
        assert!(error.contains("git hash-object -w"), "{error}");
        // Git's own diagnosis, not just the fact that it failed.
        assert!(
            error.to_ascii_lowercase().contains("not a git repository"),
            "{error}"
        );
    }
    #[test]
    fn acceptance_cannot_compile_or_list_without_executing() {
        for argv in [
            vec!["cargo", "test", "--no-run"],
            vec!["cargo", "test", "--", "--list"],
            vec!["echo", "test"],
        ] {
            assert!(acceptance(&argv.iter().map(|s| (*s).into()).collect::<Vec<_>>()).is_err());
        }
        assert!(acceptance(&["cargo".into(), "test".into(), "--workspace".into()]).is_ok());
        for argv in [
            vec!["cargo", "test", "--manifest-path", "/tmp/other/Cargo.toml"],
            vec!["cargo", "test", "--manifest-path", "../other/Cargo.toml"],
            vec!["cargo", "test", "--config", "build.rustc=other"],
            vec!["cargo", "test", "--target-dir", "/tmp/shared"],
            vec!["cargo", "test", "--manifest-path=/tmp/other/Cargo.toml"],
        ] {
            assert!(
                task_acceptance(&argv.iter().map(|v| (*v).into()).collect::<Vec<_>>()).is_err()
            );
        }
    }
    #[test]
    fn scope_and_task_identity_reject_ambiguous_or_unscoped_paths() {
        for path in [
            "../outside",
            "/absolute",
            ".git/config",
            "a//b",
            "a\\b",
            "a/../b",
            "a:b",
        ] {
            assert!(normalized(path).is_err());
        }
        let scopes = scope_set(&["src/cli".into()]).unwrap();
        assert!(owns(&scopes, "src/cli/main.rs"));
        assert!(!owns(&scopes, "src/client.rs"));
        assert!(task_id("LOCAL-native").is_err());
        assert!(task_id("T-NATIVE-1").is_ok());
    }
}
