//! Native ownership evidence. Object hashes bind facts; the independently
//! supplied permit hash and coordinator hash establish authority. Candidate
//! files and refs never select their own trust roots.
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
pub const POLICY: &str = "susi-native-provenance-policy-v1\nindependent-immutable-permit-and-coordinator\nforward-adoption-preserves-incomplete-historical-ownership\ncomplete-reservation-task-source-tree-ancestry-and-scope\nexact-gate-plan-success-and-nonempty-tests\ncontent-addressed-logs-and-claims\n";
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
    let output = Command::new("git").current_dir(root).args(args).output()?;
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

pub fn store_blob(root: &Path, bytes: &[u8]) -> Result<String> {
    let mut child = Command::new("git")
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
        return Err("cannot retain native evidence blob".into());
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

fn blob(root: &Path, object: &str) -> Result<Vec<u8>> {
    oid(object)?;
    git_bytes(root, &["cat-file", "blob", object])
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

#[derive(Clone)]
pub struct Task {
    pub id: String,
    pub path: String,
    pub blob: String,
    pub argv: Vec<String>,
}

pub fn task_for_claim(root: &Path, source: &str, worker: &str, scopes: &[String]) -> Result<Task> {
    oid(source)?;
    let held = scope_set(scopes)?;
    let mut found = None;
    for path in git(
        root,
        &["ls-tree", "-r", "--name-only", source, ".agents/tasks/"],
    )?
    .lines()
    {
        if !path.starts_with(".agents/tasks/T-")
            || !path.ends_with(".json")
            || path[14..].contains('/')
        {
            continue;
        }
        let object = git(root, &["rev-parse", &format!("{source}:{path}")])?;
        let value: Value = serde_json::from_slice(&blob(root, &object)?)?;
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
        acceptance(&argv)?;
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

pub fn gate(
    root: &Path,
    id: &str,
    argv: &[String],
    status: std::process::ExitStatus,
    elapsed_ms: u128,
    log: &Path,
) -> Result<Value> {
    let bytes = fs::read(log)?;
    let (passed, failed, ignored) = test_counts(&bytes, argv, root)?;
    if !status.success() || status.code() != Some(0) {
        return Err(format!("gate {id} did not exit successfully").into());
    }
    if argv.get(1).map(String::as_str) == Some("test") && (passed == 0 || failed != 0) {
        return Err(format!("gate {id} did not execute nonempty passing tests").into());
    }
    Ok(
        json!({"id":id,"argv":argv,"cwd":root,"exit_code":0,"elapsed_ms":elapsed_ms,
        "passed":passed,"failed":failed,"ignored":ignored,
        "log_blob":store_blob(root,&bytes)?,"log_sha256":digest(&bytes)}),
    )
}

pub fn receipt(
    root: &Path,
    worker: &str,
    claim_bytes: &[u8],
    source: &str,
    scope_base: &str,
    previous: Option<&str>,
    accepted: &str,
    gates: &Value,
) -> Result<Value> {
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

fn verify_gates(root: &Path, receipt: &Value, task: &Task) -> Result<()> {
    let plan = array(receipt, "gate_plan")?;
    let gates = array(receipt, "gates")?;
    if plan.len() != gates.len() || gates.len() < 5 {
        return Err("incomplete native integration gate plan".into());
    }
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
        let log = blob(root, string(gate, "log_blob")?)?;
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
        if argv.get(1).map(String::as_str) == Some("test") {
            acceptance(&argv)?;
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
        if matches!(id, "clippy" | "workspace-tests") {
            if suffix.len() < 4
                || suffix[0] != "--target"
                || suffix[2] != "--target-dir"
                || suffix[1].is_empty()
                || suffix[3].is_empty()
            {
                return Err("required gate lost its exact isolated compiler arguments".into());
            }
            suffix.drain(..4);
            if suffix.starts_with(&["--features", "xtask/release-tooling"]) {
                suffix.drain(..2);
            }
        }
        let expected_suffix: &[&str] = if id == "clippy" {
            &["--", "-D", "warnings"]
        } else {
            &[]
        };
        if suffix != expected_suffix {
            return Err("required gate contains a filter or weakened command".into());
        }
    }
    Ok(())
}

/// Verify only against trust pins supplied independently of the candidate.
/// The caller must retain the immutable permit in its trusted external registry.
pub fn verify(
    root: &Path,
    permit_path: &Path,
    permit_sha256: &str,
    verifier_sha256: &str,
    policy_sha256: &str,
    head: &str,
) -> Result<Value> {
    for hash in [permit_sha256, verifier_sha256, policy_sha256] {
        sha(hash)?;
    }
    oid(head)?;
    let canonical_root = root.canonicalize()?;
    if fs::symlink_metadata(permit_path)?.file_type().is_symlink()
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
    let permit: Value = serde_json::from_slice(&bytes)?;
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
    let diagnostics = blob(root, string(baseline, "diagnostics_blob")?)?;
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
    for receipt_oid in &receipt_oids {
        let receipt: Value = serde_json::from_slice(&blob(root, receipt_oid)?)?;
        if string(&receipt, "schema")? != RECEIPT_SCHEMA
            || string(&receipt, "coordinator_sha256")? != verifier_sha256
            || string(&receipt, "policy_sha256")? != policy_sha256
        {
            return Err("untrusted coordinator, policy or legacy integration receipt".into());
        }
        let worker = string(&receipt, "worker")?;
        let claim = receipt.get("claim").ok_or("missing actual claim")?;
        let claim_data = blob(root, string(claim, "blob")?)?;
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
        if !identities.insert((worker.to_owned(), commit.to_owned(), task.id.clone())) {
            return Err("reused native task receipt".into());
        }
        verify_gates(root, &receipt, &task)?;
        for introduced in git(root, &["rev-list", &format!("{scope_base}..{commit}")])?.lines() {
            // Net-zero changes must not conceal an unscoped intermediate commit.
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
    fn acceptance_cannot_compile_or_list_without_executing() {
        for argv in [
            vec!["cargo", "test", "--no-run"],
            vec!["cargo", "test", "--", "--list"],
            vec!["echo", "test"],
        ] {
            assert!(acceptance(&argv.iter().map(|s| (*s).into()).collect::<Vec<_>>()).is_err());
        }
        assert!(acceptance(&["cargo".into(), "test".into(), "--workspace".into()]).is_ok());
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
