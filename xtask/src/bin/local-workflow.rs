//! Local-only, cooperative worktree and scope reservations for parallel agents.
//! Reservations prevent overlaps between users of this launcher; they are not a
//! filesystem sandbox or a substitute for testing the integrated tree.
#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git").current_dir(dir).args(args).output()?;
    if !out.status.success() {
        return Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )
        .into());
    }
    Ok(String::from_utf8(out.stdout)?.trim().to_owned())
}

fn text(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| "path must be valid UTF-8".into())
}

fn name(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 48
        || !value
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        || value.starts_with('-')
        || value.ends_with('-')
    {
        return Err(
            "name must contain 1–48 lowercase ASCII letters, digits or interior hyphens".into(),
        );
    }
    if reserved(value) {
        return Err("Windows device names are not permitted".into());
    }
    Ok(())
}

fn reserved(value: &str) -> bool {
    let first = value.split('.').next().unwrap_or("").to_ascii_lowercase();
    matches!(first.as_str(), "con" | "prn" | "aux" | "nul")
        || (first.len() == 4
            && (first.starts_with("com") || first.starts_with("lpt"))
            && matches!(first.as_bytes()[3], b'1'..=b'9'))
}

fn scope(value: &str) -> Result<String> {
    let value = value.replace('\\', "/");
    if value.is_empty() || !value.is_ascii() || value.starts_with('/') || value.ends_with('/') {
        return Err("scope must be a nonempty ASCII relative file or directory path".into());
    }
    for component in value.split('/') {
        if component.is_empty()
            || component == "."
            || component == ".."
            || component.eq_ignore_ascii_case(".git")
            || component.ends_with('.')
            || component.ends_with(' ')
            || reserved(component)
            || component
                .chars()
                .any(|c| c.is_control() || ":*?\"<>|".contains(c))
        {
            return Err(format!("unsafe scope path: {value}").into());
        }
    }
    // Conservative on every platform so a claim shared with Windows is safe.
    Ok(value.to_ascii_lowercase())
}

fn overlaps(a: &str, b: &str) -> bool {
    a == b
        || a.strip_prefix(b).is_some_and(|s| s.starts_with('/'))
        || b.strip_prefix(a).is_some_and(|s| s.starts_with('/'))
}

struct Lock(PathBuf);
impl Lock {
    fn acquire(state: &Path) -> Result<Self> {
        fs::create_dir_all(state)?;
        let path = state.join("lock");
        fs::create_dir(&path).map_err(|e| {
            if e.kind() == io::ErrorKind::AlreadyExists {
                io::Error::new(io::ErrorKind::WouldBlock, format!("local workflow is locked at {}; retry after the current operation. If its process crashed, inspect the lock before manual recovery", path.display()))
            } else { e }
        })?;
        let guard = Self(path);
        fs::write(
            guard.0.join("owner"),
            format!("pid={}\n", std::process::id()),
        )?;
        Ok(guard)
    }
}
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.0.join("owner"));
        let _ = fs::remove_dir(&self.0);
    }
}

#[derive(Debug)]
struct Claim {
    name: String,
    worktree: PathBuf,
    base: String,
    scopes: Vec<String>,
}

fn read_claim(path: &Path) -> Result<Claim> {
    if fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err("claim must not be a symlink".into());
    }
    let data = fs::read_to_string(path)?;
    let mut lines = data.lines();
    if lines.next() != Some("susi-local-claim-v2") {
        return Err("claim requires v2 with a recorded base; preserve legacy work and migrate its claim explicitly".into());
    }
    let claimant = lines.next().ok_or("missing claim name")?.to_owned();
    name(&claimant)?;
    let worktree = PathBuf::from(lines.next().ok_or("missing worktree")?);
    if !worktree.is_absolute() {
        return Err("claim worktree must be absolute".into());
    }
    let base = lines.next().ok_or("missing claim base")?.to_owned();
    if !valid_oid(&base) {
        return Err("invalid claim base".into());
    }
    let scopes: Vec<String> = lines.map(scope).collect::<Result<_>>()?;
    if scopes.is_empty() {
        return Err("claim has no scopes".into());
    }
    if path.file_stem().and_then(|s| s.to_str()) != Some(&claimant) {
        return Err("claim filename mismatch".into());
    }
    Ok(Claim {
        name: claimant,
        worktree,
        base,
        scopes,
    })
}

fn claims(state: &Path) -> Result<Vec<Claim>> {
    let directory = state.join("claims");
    fs::create_dir_all(&directory)?;
    let mut result = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        result.push(read_claim(&entry.path())?);
    }
    result.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(result)
}

fn state(root: &Path) -> Result<PathBuf> {
    Ok(PathBuf::from(git(
        root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?)
    .join("susi-local-workflow"))
}

const INTEGRATION_REF: &str = "refs/susi/integration/current";

fn valid_oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn integration_head(root: &Path) -> Result<Option<String>> {
    let out = Command::new("git")
        .current_dir(root)
        .args(["show-ref", "--verify", "--quiet", INTEGRATION_REF])
        .output()?;
    if out.status.code() == Some(1) {
        return Ok(None);
    }
    if !out.status.success() {
        return Err("could not inspect local integration ref".into());
    }
    Ok(Some(git(root, &["rev-parse", INTEGRATION_REF])?))
}

fn is_ancestor(root: &Path, ancestor: &str, descendant: &str) -> Result<bool> {
    let out = Command::new("git")
        .current_dir(root)
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .output()?;
    match out.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err("cannot establish commit ancestry".into()),
    }
}

fn git_input(root: &Path, args: &[&str], input: &[u8]) -> Result<String> {
    let mut child = Command::new("git")
        .current_dir(root)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or("missing git stdin")?
        .write_all(input)?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned().into());
    }
    Ok(String::from_utf8(out.stdout)?.trim().to_owned())
}

fn check_source(root: &Path, claim: &Claim) -> Result<String> {
    if state(&claim.worktree)? != state(root)?
        || git(&claim.worktree, &["branch", "--show-current"])?
            != format!("local-agent/{}", claim.name)
    {
        return Err("claim repository or branch does not match".into());
    }
    if !git(
        &claim.worktree,
        &["status", "--porcelain", "--untracked-files=all"],
    )?
    .is_empty()
    {
        return Err("integration requires a clean committed agent worktree".into());
    }
    let source = git(&claim.worktree, &["rev-parse", "HEAD"])?;
    if !is_ancestor(root, &claim.base, &source)? {
        return Err("source no longer descends from its reserved base".into());
    }
    let out = Command::new("git")
        .current_dir(root)
        .args([
            "diff",
            "--no-renames",
            "--name-only",
            "-z",
            &claim.base,
            &source,
        ])
        .output()?;
    if !out.status.success() {
        return Err("cannot inspect committed scope".into());
    }
    for path in out
        .stdout
        .split(|byte| *byte == 0)
        .filter(|p| !p.is_empty())
    {
        let path = scope(std::str::from_utf8(path)?)?;
        if !claim.scopes.iter().any(|held| owns_path(held, &path)) {
            return Err(format!("committed file is outside reserved scope: {path}").into());
        }
    }
    Ok(source)
}

/// Verify the exact merged checkout, using only native executable invocations.
/// The isolated home prevents tests from consuming the operator's runtime state;
/// Cargo and rustup retain their provisioned toolchain/cache locations.
fn verify_candidate(candidate: &Path, attempt: &Path) -> Result<Vec<String>> {
    let compiler = Command::new("rustc")
        .current_dir(candidate)
        .arg("-vV")
        .output()?;
    if !compiler.status.success() {
        return Err("cannot identify native compiler".into());
    }
    let compiler = String::from_utf8(compiler.stdout)?;
    let host = compiler
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .ok_or("compiler did not report host")?;
    if cfg!(windows) && !host.ends_with("-pc-windows-msvc") {
        return Err("Windows integration requires a native MSVC compiler".into());
    }
    let original_home = env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .ok_or("cannot identify toolchain home")?;
    let home = attempt.join("test-home");
    fs::create_dir_all(&home)?;
    let target = candidate.join("target");
    let cargo_home = env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| original_home.join(".cargo"));
    let rustup_home = env::var_os("RUSTUP_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| original_home.join(".rustup"));
    let gates: &[&[&str]] = &[
        &["fmt", "--all", "--check"],
        &["deny", "--offline", "--locked", "check", "licenses"],
        &[
            "clippy",
            "--workspace",
            "--all-targets",
            "--offline",
            "--locked",
            "--target",
            host,
            "--target-dir",
            text(&target)?,
            "--",
            "-D",
            "warnings",
        ],
        &[
            "test",
            "--workspace",
            "--offline",
            "--locked",
            "--target",
            host,
            "--target-dir",
            text(&target)?,
        ],
    ];
    let mut evidence = Vec::new();
    for (index, args) in gates.iter().enumerate() {
        let log = fs::File::create(attempt.join(format!("gate-{index}.log")))?;
        let mut command = Command::new("cargo");
        for (key, _) in env::vars_os() {
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
        let status = command
            .current_dir(candidate)
            .args(*args)
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env("SUSI_HOME", home.join("susi"))
            .env("XDG_CONFIG_HOME", home.join("config"))
            .env("XDG_DATA_HOME", home.join("data"))
            .env("XDG_CACHE_HOME", home.join("cache"))
            .env("APPDATA", home.join("appdata"))
            .env("LOCALAPPDATA", home.join("localappdata"))
            .env("CARGO_HOME", &cargo_home)
            .env("RUSTUP_HOME", &rustup_home)
            .env("CARGO_TARGET_DIR", &target)
            .env("CARGO_BUILD_TARGET_DIR", &target)
            .env("CARGO_BUILD_BUILD_DIR", &target)
            // Host compiler wrappers may consult non-hermetic user-profile
            // configuration or run a shared daemon. The local gate is self-contained.
            .env("RUSTC_WRAPPER", "")
            .env("RUSTC_WORKSPACE_WRAPPER", "")
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log))
            .status()?;
        let result = format!("cargo {} => {status}", args.join(" "));
        evidence.push(result.clone());
        fs::write(attempt.join("verification.txt"), evidence.join("\n"))?;
        if !status.success() {
            return Err(format!(
                "gate failed: {result}; logs retained at {}",
                attempt.display()
            )
            .into());
        }
    }
    Ok(evidence)
}

/// Serialize only integration, not agent edits/commits. Publish the tested commit
/// and its receipt together in one Git ref transaction. No primary checkout moves.
fn integrate_with<F>(root: &Path, claimant: &str, verify: F) -> Result<String>
where
    F: FnOnce(&Path, &Path) -> Result<Vec<String>>,
{
    name(claimant)?;
    let state = state(root)?;
    let _integration = Lock::acquire(&state.join("integration"))?;
    let claim_path = state.join("claims").join(format!("{claimant}.claim"));
    let (claim, source, claim_bytes) = {
        let _claims = Lock::acquire(&state)?;
        let claim = read_claim(&claim_path)?;
        let source = check_source(root, &claim)?;
        let bytes = fs::read(&claim_path)?;
        (claim, source, bytes)
    };
    let previous = integration_head(root)?;
    let base = previous.clone().unwrap_or_else(|| claim.base.clone());
    if !is_ancestor(root, &claim.base, &base)? {
        return Err(
            "integration history does not contain the claim base; reconcile explicitly".into(),
        );
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let attempt = state
        .join("attempts")
        .join(format!("{claimant}-{stamp}-{}", std::process::id()));
    fs::create_dir_all(&attempt)?;
    let checkout = PathBuf::from(git(root, &["rev-parse", "--show-toplevel"])?);
    let candidate = checkout
        .parent()
        .ok_or("checkout has no parent")?
        .join(format!("susi-verify-{stamp}-{}", std::process::id()));
    git(
        root,
        &[
            "-c",
            "core.hooksPath=",
            "worktree",
            "add",
            "--detach",
            text(&candidate)?,
            &base,
        ],
    )?;
    fs::write(
        attempt.join("inputs.txt"),
        format!(
            "claim-base={}\nsource={source}\nintegration-base={base}\n",
            claim.base
        ),
    )?;
    let merged = git(
        &candidate,
        &[
            "-c",
            "core.hooksPath=",
            "-c",
            "user.name=Susi Local Integration",
            "-c",
            "user.email=susi-local@localhost",
            "-c",
            "commit.gpgsign=false",
            "merge",
            "--no-ff",
            "--no-edit",
            &source,
        ],
    );
    if let Err(error) = merged {
        fs::write(attempt.join("status"), format!("merge failed: {error}\n"))?;
        return Err(format!(
            "merge failed; source and reservation retained; repair evidence at {}: {error}",
            candidate.display()
        )
        .into());
    }
    let head = git(&candidate, &["rev-parse", "HEAD"])?;
    let tree = git(&candidate, &["rev-parse", "HEAD^{tree}"])?;
    let evidence = match verify(&candidate, &attempt) {
        Ok(evidence) => evidence,
        Err(error) => {
            fs::write(
                attempt.join("status"),
                format!("verification failed: {error}\n"),
            )?;
            return Err(error);
        }
    };
    if evidence.is_empty() {
        return Err("verification returned no evidence".into());
    }
    if git(&candidate, &["rev-parse", "HEAD"])? != head
        || !git(
            &candidate,
            &["status", "--porcelain", "--untracked-files=all"],
        )?
        .is_empty()
    {
        return Err("verification changed the candidate tree; refusing promotion".into());
    }
    let _claims = Lock::acquire(&state)?;
    if fs::read(&claim_path)? != claim_bytes || check_source(root, &claim)? != source {
        return Err(
            "source or reservation changed during verification; retry against its new head".into(),
        );
    }
    let receipt = format!("susi-local-integration-v1\nagent={claimant}\nsource={source}\nclaim-base={}\nprevious={}\ncommit={head}\ntree={tree}\n{}\n", claim.base, previous.as_deref().unwrap_or("none"), evidence.join("\n"));
    let receipt_oid = git_input(root, &["hash-object", "-w", "--stdin"], receipt.as_bytes())?;
    let receipt_ref = format!("refs/susi/integration/receipts/{claimant}/{source}");
    let zero = "0".repeat(head.len());
    let transaction = format!("start\nupdate {INTEGRATION_REF} {head} {}\ncreate {receipt_ref} {receipt_oid}\nprepare\ncommit\n", previous.as_deref().unwrap_or(&zero));
    git_input(root, &["update-ref", "--stdin"], transaction.as_bytes())?;
    fs::write(
        attempt.join("status"),
        format!("accepted {head}\nreceipt {receipt_ref}\n"),
    )?;
    fs::remove_file(claim_path)?;
    // The accepted commit and receipt are durable; retain failure candidates,
    // but reclaim successful scratch builds without touching agent worktrees.
    if let Err(error) = git(
        root,
        &[
            "-c",
            "core.hooksPath=",
            "worktree",
            "remove",
            text(&candidate)?,
        ],
    ) {
        eprintln!(
            "accepted integration candidate retained at {}: {error}",
            candidate.display()
        );
    }
    println!("Integrated {claimant}: {head}; receipt {receipt_ref}. Primary checkout unchanged.");
    Ok(head)
}

fn start(root: &Path, claimant: &str, raw_scopes: &[String]) -> Result<PathBuf> {
    name(claimant)?;
    let mut scopes = raw_scopes
        .iter()
        .map(|s| scope(s))
        .collect::<Result<Vec<_>>>()?;
    if scopes.is_empty() {
        return Err("at least one --scope is required".into());
    }
    scopes.sort();
    scopes.dedup();
    let state = state(root)?;
    let _lock = Lock::acquire(&state)?;
    for active in claims(&state)? {
        if active.name == claimant {
            return Err(format!("agent {claimant} already has a claim").into());
        }
        for requested in &scopes {
            if active.scopes.iter().any(|held| overlaps(requested, held)) {
                return Err(format!("scope {requested} overlaps agent {}", active.name).into());
            }
        }
    }
    let primary = PathBuf::from(git(root, &["rev-parse", "--show-toplevel"])?);
    let base = primary
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or("invalid repository name")?;
    let worktree = primary
        .parent()
        .ok_or("repository has no parent")?
        .join(format!("{base}-agent-{claimant}"));
    if fs::symlink_metadata(&worktree).is_ok() {
        return Err("destination already exists; refusing to reuse it".into());
    }
    let branch = format!("local-agent/{claimant}");
    let base_oid = integration_head(root)?.unwrap_or(git(root, &["rev-parse", "HEAD"])?);
    let claim_path = state.join("claims").join(format!("{claimant}.claim"));
    let worktree_text = text(&worktree)?;
    if worktree_text.chars().any(char::is_control) {
        return Err("worktree path contains control characters".into());
    }
    fs::write(
        &claim_path,
        format!(
            "susi-local-claim-v2\n{claimant}\n{worktree_text}\n{base_oid}\n{}\n",
            scopes.join("\n")
        ),
    )?;
    // Suppress legacy shell/network hooks for this local creation only. No
    // persistent Git setting changes and no remote operation are performed.
    if let Err(error) = git(
        root,
        &[
            "-c",
            "core.hooksPath=",
            "worktree",
            "add",
            "-b",
            &branch,
            worktree_text,
            &base_oid,
        ],
    ) {
        // Retain the reservation if Git left a checkout behind, for inspection.
        if !worktree.exists() {
            fs::remove_file(claim_path)?;
        }
        return Err(error);
    }
    fs::create_dir(worktree.join("target"))?;
    Ok(worktree)
}

fn release(root: &Path, claimant: &str) -> Result<()> {
    name(claimant)?;
    let state = state(root)?;
    let _lock = Lock::acquire(&state)?;
    let path = state.join("claims").join(format!("{claimant}.claim"));
    let claim = read_claim(&path)?;
    if self::state(&claim.worktree)? != state {
        return Err("claim points to another repository".into());
    }
    if git(&claim.worktree, &["branch", "--show-current"])? != format!("local-agent/{claimant}") {
        return Err("worktree branch changed; inspect before releasing".into());
    }
    if !git(
        &claim.worktree,
        &["status", "--porcelain", "--untracked-files=all"],
    )?
    .is_empty()
    {
        return Err("worktree is dirty; preserve and commit its work before releasing".into());
    }
    let head = git(&claim.worktree, &["rev-parse", "HEAD"])?;
    if head != claim.base {
        let accepted = integration_head(root)?
            .ok_or("committed work must be integrated before releasing its reservation")?;
        if !is_ancestor(root, &head, &accepted)? {
            return Err(
                "committed work has not passed local integration; reservation retained".into(),
            );
        }
    }
    fs::remove_file(path)?;
    println!(
        "Released {claimant}; retained branch and worktree {}",
        claim.worktree.display()
    );
    Ok(())
}

/// Collect each index and working-tree delta separately: their combined HEAD
/// diff can be empty even when the index still contains a staged change.
fn changed_paths(root: &Path) -> Result<Vec<String>> {
    let mut paths = Vec::new();
    for args in [
        vec!["diff", "--cached", "--no-renames", "--name-only", "-z"],
        vec!["diff", "--no-renames", "--name-only", "-z"],
        vec!["ls-files", "--others", "--exclude-standard", "-z"],
    ] {
        let out = Command::new("git").current_dir(root).args(args).output()?;
        if !out.status.success() {
            return Err(format!(
                "cannot inspect changes: {}",
                String::from_utf8_lossy(&out.stderr)
            )
            .into());
        }
        for path in out
            .stdout
            .split(|byte| *byte == 0)
            .filter(|p| !p.is_empty())
        {
            paths.push(String::from_utf8(path.to_vec())?);
        }
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn owns_path(held: &str, changed: &str) -> bool {
    held == changed
        || changed
            .strip_prefix(held)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// Scope-guarded local commit, not a test/release gate. It never changes the
/// repository's shared hook configuration or invokes its network-bound hooks.
fn commit(root: &Path, claimant: &str, message: &str) -> Result<()> {
    name(claimant)?;
    if message.trim().is_empty() || message.contains('\0') {
        return Err("commit message must be nonempty and contain no NUL".into());
    }
    let state = state(root)?;
    let _lock = Lock::acquire(&state)?;
    let claim = read_claim(&state.join("claims").join(format!("{claimant}.claim")))?;
    let actual_root = PathBuf::from(git(root, &["rev-parse", "--show-toplevel"])?);
    if actual_root.canonicalize()? != claim.worktree.canonicalize()? {
        return Err("commit must run inside the claimed agent's worktree".into());
    }
    if self::state(&claim.worktree)? != state
        || git(&claim.worktree, &["branch", "--show-current"])? != format!("local-agent/{claimant}")
    {
        return Err("claim repository or branch does not match".into());
    }
    // Do not convert an in-progress merge/cherry-pick/rebase into a task commit.
    for marker in [
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "rebase-merge",
        "rebase-apply",
    ] {
        let path = git(
            &claim.worktree,
            &["rev-parse", "--path-format=absolute", "--git-path", marker],
        )?;
        if Path::new(&path).exists() {
            return Err("finish the existing Git operation before committing".into());
        }
    }
    let paths = changed_paths(&claim.worktree)?;
    if paths.is_empty() {
        return Err("nothing to commit".into());
    }
    for path in &paths {
        let normalized = scope(path)?;
        if !claim.scopes.iter().any(|held| owns_path(held, &normalized)) {
            return Err(
                format!("changed file is outside {claimant}'s reserved scopes: {path}").into(),
            );
        }
    }
    let hooks = state.join("empty-hooks");
    fs::create_dir_all(&hooks)?;
    if fs::symlink_metadata(&hooks)?.file_type().is_symlink()
        || fs::read_dir(&hooks)?.next().is_some()
    {
        return Err("local empty-hooks directory must be an empty real directory".into());
    }
    // NUL-delimited literal pathspecs preserve spaces and metacharacters and
    // avoid the Windows command-line length limit for large tasks.
    let mut add = Command::new("git")
        .current_dir(&claim.worktree)
        .args([
            "--literal-pathspecs",
            "add",
            "-A",
            "--pathspec-from-file=-",
            "--pathspec-file-nul",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    if let Some(mut input) = add.stdin.take() {
        for path in &paths {
            input.write_all(path.as_bytes())?;
            input.write_all(&[0])?;
        }
    } else {
        return Err("could not open git add input".into());
    }
    let out = add.wait_with_output()?;
    if !out.status.success() {
        return Err(format!("git add failed: {}", String::from_utf8_lossy(&out.stderr)).into());
    }
    let hook_config = format!("core.hooksPath={}", text(&hooks)?);
    let trailer = format!("Task: LOCAL-{claimant}");
    let output = git(
        &claim.worktree,
        &[
            "-c",
            &hook_config,
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-m",
            message,
            "-m",
            &trailer,
        ],
    )?;
    println!(
        "{output}\nLocal scope guard passed; this commit is not a test or release attestation."
    );
    Ok(())
}

fn run(args: &[String]) -> Result<()> {
    let root = env::current_dir()?;
    match args.first().map(String::as_str) {
        Some("start") => {
            let claimant = args.get(1).ok_or("start requires an agent name")?;
            let mut scopes = Vec::new();
            let mut rest = args[2..].iter();
            while let Some(flag) = rest.next() {
                if flag != "--scope" { return Err(format!("unknown argument: {flag}").into()); }
                scopes.push(rest.next().ok_or("--scope requires a path")?.clone());
            }
            let worktree = start(&root, claimant, &scopes)?;
            println!("Worktree: {}\nTarget: {}\nUse the local-build entrypoint to override inherited target directories. Reservations cover cooperating launcher users only.", worktree.display(), worktree.join("target").display());
        }
        Some("release") if args.len() == 2 => release(&root, &args[1])?,
        Some("commit") if args.len() == 3 => commit(&root, &args[1], &args[2])?,
        Some("integrate") if args.len() == 2 => { integrate_with(&root, &args[1], verify_candidate)?; },
        Some("list") if args.len() == 1 => {
            let state = state(&root)?;
            let _lock = Lock::acquire(&state)?;
            for claim in claims(&state)? { println!("{}\t{}\t{}", claim.name, claim.worktree.display(), claim.scopes.join(", ")); }
        }
        _ => return Err("usage: local-workflow start <name> --scope <path> [--scope <path> ...] | list | commit <name> <message> | integrate <name> | release <name>".into()),
    }
    Ok(())
}

fn main() -> ExitCode {
    match run(&env::args().skip(1).collect::<Vec<_>>()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("local-workflow: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let path = env::temp_dir().join(format!(
                "susi-local-workflow-{}-{}",
                std::process::id(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn repo(&self) -> PathBuf {
            let repo = self.0.join("repo");
            fs::create_dir(&repo).unwrap();
            git(&repo, &["init", "-q"]).unwrap();
            git(&repo, &["config", "user.name", "Test"]).unwrap();
            git(&repo, &["config", "user.email", "test@example.invalid"]).unwrap();
            git(&repo, &["config", "commit.gpgsign", "false"]).unwrap();
            fs::write(repo.join(".gitignore"), "/target/\n").unwrap();
            git(&repo, &["add", ".gitignore"]).unwrap();
            git(
                &repo,
                &[
                    "-c",
                    "core.hooksPath=",
                    "-c",
                    "user.name=Test",
                    "-c",
                    "user.email=test@example.invalid",
                    "commit",
                    "-qm",
                    "initial",
                ],
            )
            .unwrap();
            repo
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            if std::thread::panicking() {
                return;
            }
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn path_validation_blocks_windows_aliases_and_traversal() {
        for bad in [
            "",
            ".",
            "../src",
            "a/../b",
            "C:/src",
            "\\\\host\\share",
            "src/",
            "src//x",
            "NUL.rs",
            "src/COM1",
            ".git/config",
            "src/a.",
            "src/a ",
            "src/a:b",
            "src/\nx",
        ] {
            assert!(scope(bad).is_err(), "{bad}");
        }
        assert_eq!(scope("Src\\CLI").unwrap(), "src/cli");
        assert!(overlaps("src", "src/cli"));
        assert!(!overlaps("src", "src2"));
        for bad in ["con", "aux", "a/b", "-abc", "abc-", "A", ""] {
            assert!(name(bad).is_err());
        }
    }
    #[test]
    fn reservation_is_exclusive_and_lock_is_released() {
        let scratch = Scratch::new();
        let lock = Lock::acquire(&scratch.0).unwrap();
        assert!(Lock::acquire(&scratch.0).is_err());
        drop(lock);
        assert!(Lock::acquire(&scratch.0).is_ok());
    }
    #[test]
    fn local_worktrees_reject_overlap_and_preserve_dirty_work() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        let first = start(&repo, "one", &["src".into()]).unwrap();
        assert!(first.join("target").is_dir());
        assert!(start(&repo, "two", &["SRC/cli".into()]).is_err());
        let second = start(&repo, "two", &["tests".into()]).unwrap();
        assert_ne!(first, second);
        fs::write(first.join("dirty.rs"), "preserve me").unwrap();
        assert!(release(&repo, "one").is_err());
        assert_eq!(
            fs::read_to_string(first.join("dirty.rs")).unwrap(),
            "preserve me"
        );
        release(&repo, "two").unwrap();
        assert!(second.exists());
        assert!(git(
            &repo,
            &["rev-parse", "--verify", "refs/heads/local-agent/two"]
        )
        .is_ok());
        assert!(start(&repo, "two", &["tests".into()]).is_err());
    }
    #[test]
    fn parallel_scope_claims_have_one_winner() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let handles: Vec<_> = ["left", "right"]
            .into_iter()
            .map(|agent| {
                let repo = repo.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    start(&repo, agent, &["src".into()]).is_ok()
                })
            })
            .collect();
        let winners = handles
            .into_iter()
            .map(|handle| usize::from(handle.join().unwrap()))
            .sum::<usize>();
        assert_eq!(winners, 1);
        assert_eq!(claims(&state(&repo).unwrap()).unwrap().len(), 1);
    }

    #[test]
    fn corrupt_claim_fails_closed() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        let state = state(&repo).unwrap();
        fs::create_dir_all(state.join("claims")).unwrap();
        fs::write(state.join("claims/broken.claim"), "bad").unwrap();
        assert!(start(&repo, "one", &["src".into()]).is_err());
    }

    #[test]
    fn commit_rejects_wrong_worktree_and_outside_scope_without_staging() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        let worktree = start(&repo, "one", &["src".into()]).unwrap();
        fs::create_dir(worktree.join("src")).unwrap();
        fs::write(worktree.join("src/allowed.rs"), "allowed").unwrap();
        assert!(commit(&repo, "one", "wrong owner").is_err());
        fs::write(worktree.join("outside.rs"), "outside").unwrap();
        assert!(commit(&worktree, "one", "reject").is_err());
        assert!(git(&worktree, &["diff", "--cached", "--name-only"])
            .unwrap()
            .is_empty());
        fs::remove_file(worktree.join("outside.rs")).unwrap();
        // A staged delta cancelled by an unstaged edit still must be inspected.
        fs::write(worktree.join(".gitignore"), "changed\n").unwrap();
        git(&worktree, &["add", ".gitignore"]).unwrap();
        fs::write(worktree.join(".gitignore"), "/target/\n").unwrap();
        assert!(commit(&worktree, "one", "reject staged outside").is_err());
    }

    #[test]
    fn local_commit_suppresses_only_its_own_hooks_and_preserves_shared_setting() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        let worktree = start(&repo, "one", &["src".into()]).unwrap();
        git(&repo, &["config", "user.name", "Test"]).unwrap();
        git(&repo, &["config", "user.email", "test@example.invalid"]).unwrap();
        let hooks = scratch.0.join("failing-hooks");
        fs::create_dir(&hooks).unwrap();
        let hook = hooks.join("pre-commit");
        fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
        }
        git(&repo, &["config", "core.hooksPath", text(&hooks).unwrap()]).unwrap();
        fs::create_dir(worktree.join("src")).unwrap();
        fs::write(worktree.join("src/a space.rs"), "allowed").unwrap();
        git(&worktree, &["add", "--", "src/a space.rs"]).unwrap();
        assert!(git(&worktree, &["commit", "-m", "blocked by hook"]).is_err());
        commit(&worktree, "one", "local change").unwrap();
        assert!(git(&worktree, &["status", "--porcelain"])
            .unwrap()
            .is_empty());
        assert_eq!(
            git(&repo, &["config", "--get", "core.hooksPath"]).unwrap(),
            text(&hooks).unwrap()
        );
        assert!(git(&worktree, &["log", "-1", "--format=%B"])
            .unwrap()
            .contains("Task: LOCAL-one"));
    }

    fn worker(repo: &Path, agent: &str, file: &str, content: &str) -> PathBuf {
        let worktree = start(repo, agent, &[file.into()]).unwrap();
        fs::write(worktree.join(file), content).unwrap();
        commit(&worktree, agent, "fixture change").unwrap();
        worktree
    }

    #[test]
    fn integration_tests_the_combined_tree_and_publishes_atomic_receipt() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        let original = git(&repo, &["rev-parse", "HEAD"]).unwrap();
        let left = worker(&repo, "left", "left.rs", "left");
        let right = worker(&repo, "right", "right.rs", "right");
        let source = git(&right, &["rev-parse", "HEAD"]).unwrap();
        fs::write(repo.join("primary-dirty.txt"), "preserve primary").unwrap();
        let first = integrate_with(&repo, "left", |candidate, _| {
            assert_eq!(fs::read_to_string(candidate.join("left.rs"))?, "left");
            assert!(!candidate.join("right.rs").exists());
            Ok(vec!["test-only content verification passed".into()])
        })
        .unwrap();
        let second = integrate_with(&repo, "right", |candidate, _| {
            assert_eq!(fs::read_to_string(candidate.join("left.rs"))?, "left");
            assert_eq!(fs::read_to_string(candidate.join("right.rs"))?, "right");
            Ok(vec!["test-only combined content verification passed".into()])
        })
        .unwrap();
        assert_ne!(first, second);
        assert_eq!(integration_head(&repo).unwrap(), Some(second.clone()));
        assert_eq!(git(&repo, &["rev-parse", "HEAD"]).unwrap(), original);
        assert_eq!(
            fs::read_to_string(repo.join("primary-dirty.txt")).unwrap(),
            "preserve primary"
        );
        assert!(left.exists() && right.exists());
        assert!(claims(&state(&repo).unwrap()).unwrap().is_empty());
        let receipt_ref = format!("refs/susi/integration/receipts/right/{source}");
        let receipt = git(&repo, &["cat-file", "blob", &receipt_ref]).unwrap();
        assert!(receipt.contains(&format!("commit={second}")));
        assert!(receipt.contains(&format!("previous={first}")));
        assert!(receipt.contains(&format!("source={source}")));
        let third = start(&repo, "third", &["third.rs".into()]).unwrap();
        assert_eq!(git(&third, &["rev-parse", "HEAD"]).unwrap(), second);
    }

    #[test]
    fn failed_gate_retains_source_claim_and_previous_integration() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        let worktree = worker(&repo, "one", "one.rs", "one");
        assert!(release(&repo, "one").is_err());
        let result = integrate_with(&repo, "one", |_, attempt| {
            fs::write(attempt.join("gate-0.log"), "test failure evidence")?;
            Err("deliberate test-only gate failure".into())
        });
        assert!(result.is_err());
        assert_eq!(integration_head(&repo).unwrap(), None);
        assert_eq!(claims(&state(&repo).unwrap()).unwrap().len(), 1);
        assert_eq!(fs::read_to_string(worktree.join("one.rs")).unwrap(), "one");
        let attempts = state(&repo).unwrap().join("attempts");
        assert!(fs::read_dir(attempts).unwrap().any(|entry| entry
            .unwrap()
            .path()
            .join("status")
            .is_file()));
    }

    #[test]
    fn integration_rechecks_committed_scope_and_candidate_mutation() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        let worktree = worker(&repo, "one", "one.rs", "one");
        assert!(integrate_with(&repo, "one", |candidate, _| {
            fs::write(candidate.join("one.rs"), "modified after verification")?;
            Ok(vec!["test-only check".into()])
        })
        .is_err());
        fs::write(worktree.join("outside.rs"), "escape").unwrap();
        git(&worktree, &["add", "outside.rs"]).unwrap();
        git(
            &worktree,
            &["-c", "core.hooksPath=", "commit", "-qm", "outside scope"],
        )
        .unwrap();
        assert!(
            integrate_with(&repo, "one", |_, _| panic!("scope violation reached gate")).is_err()
        );
        assert_eq!(integration_head(&repo).unwrap(), None);
    }

    #[test]
    fn integration_cas_refuses_external_advancement_without_publishing_receipt() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        let worktree = worker(&repo, "one", "one.rs", "one");
        let source = git(&worktree, &["rev-parse", "HEAD"]).unwrap();
        let external = git(&repo, &["rev-parse", "HEAD"]).unwrap();
        assert!(integrate_with(&repo, "one", |_, _| {
            git(&repo, &["update-ref", INTEGRATION_REF, &external])?;
            Ok(vec!["test-only check".into()])
        })
        .is_err());
        assert_eq!(integration_head(&repo).unwrap(), Some(external));
        assert!(git(
            &repo,
            &[
                "rev-parse",
                "--verify",
                &format!("refs/susi/integration/receipts/one/{source}")
            ]
        )
        .is_err());
        assert_eq!(claims(&state(&repo).unwrap()).unwrap().len(), 1);
    }

    #[test]
    fn source_advancement_during_gate_cannot_be_promoted_as_tested() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        let worktree = worker(&repo, "one", "one.rs", "one");
        assert!(integrate_with(&repo, "one", |_, _| {
            fs::write(worktree.join("one.rs"), "new untested source")?;
            commit(&worktree, "one", "advanced during verification")?;
            Ok(vec!["test-only prior-head check".into()])
        })
        .is_err());
        assert_eq!(integration_head(&repo).unwrap(), None);
    }

    #[test]
    fn real_native_cargo_gates_accept_a_passing_fixture() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        let native_worker = start(
            &repo,
            "native",
            &[
                "Cargo.toml".into(),
                "Cargo.lock".into(),
                "deny.toml".into(),
                "src".into(),
            ],
        )
        .unwrap();
        fs::create_dir(native_worker.join("src")).unwrap();
        fs::write(native_worker.join("Cargo.toml"), "[package]\nname = \"local-gate-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\nlicense = \"MIT\"\n") .unwrap();
        fs::write(
            native_worker.join("Cargo.lock"),
            "version = 4\n\n[[package]]\nname = \"local-gate-fixture\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        fs::write(
            native_worker.join("deny.toml"),
            "[licenses]\nallow = [\"MIT\"]\n",
        )
        .unwrap();
        fs::write(
            native_worker.join("src/lib.rs"),
            "#[test]\nfn actual_test() {\n    assert_eq!(2 + 2, 4);\n}\n",
        )
        .unwrap();
        commit(&native_worker, "native", "native gate fixture").unwrap();
        let head = integrate_with(&repo, "native", verify_candidate).unwrap();
        assert_eq!(integration_head(&repo).unwrap(), Some(head.clone()));
        let broken = worker(&repo, "broken", "src/lib.rs", "#[test]\nfn failing_test() {\n    let actual = std::hint::black_box(4);\n    assert_eq!(actual, 5);\n}\n");
        assert!(integrate_with(&repo, "broken", verify_candidate).is_err());
        assert_eq!(integration_head(&repo).unwrap(), Some(head));
        assert!(broken.exists());
        let attempts = state(&repo).unwrap().join("attempts");
        assert!(fs::read_dir(attempts).unwrap().any(|entry| {
            fs::read_to_string(entry.unwrap().path().join("gate-3.log"))
                .is_ok_and(|log| log.contains("test result: FAILED"))
        }));
    }

    #[test]
    fn merge_conflict_preserves_candidate_and_reservation() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        fs::write(repo.join("common.rs"), "original\n").unwrap();
        git(&repo, &["add", "common.rs"]).unwrap();
        git(&repo, &["-c", "core.hooksPath=", "commit", "-qm", "base"]).unwrap();
        worker(&repo, "one", "common.rs", "worker\n");
        fs::write(repo.join("common.rs"), "integrated\n").unwrap();
        git(&repo, &["add", "common.rs"]).unwrap();
        git(
            &repo,
            &["-c", "core.hooksPath=", "commit", "-qm", "other change"],
        )
        .unwrap();
        let head = git(&repo, &["rev-parse", "HEAD"]).unwrap();
        git(&repo, &["update-ref", INTEGRATION_REF, &head]).unwrap();
        assert!(
            integrate_with(&repo, "one", |_, _| panic!("conflict reached verification")).is_err()
        );
        assert_eq!(integration_head(&repo).unwrap(), Some(head));
        assert_eq!(claims(&state(&repo).unwrap()).unwrap().len(), 1);
        let worktrees = git(&repo, &["worktree", "list", "--porcelain"]).unwrap();
        let candidate = worktrees
            .lines()
            .filter_map(|line| line.strip_prefix("worktree "))
            .find(|path| path.contains("susi-verify-"))
            .unwrap();
        assert!(!git(
            Path::new(candidate),
            &["diff", "--name-only", "--diff-filter=U"]
        )
        .unwrap()
        .is_empty());
    }
}
