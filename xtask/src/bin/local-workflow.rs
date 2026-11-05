//! Local-only, cooperative worktree and scope reservations for parallel agents.
//! Reservations prevent overlaps between users of this launcher; they are not a
//! filesystem sandbox or a substitute for testing the integrated tree.
#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

#[cfg(windows)]
extern crate susi_windows_runtime;

use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

#[path = "../compiler_env.rs"]
mod compiler_env;
#[path = "../native_provenance.rs"]
mod native_provenance;
#[path = "../sysroot_sources.rs"]
mod sysroot_sources;
#[path = "../trusted_sysroot.rs"]
mod trusted_sysroot;
#[path = "../workspace_test_count.rs"]
mod workspace_test_count;
#[path = "../workspace_test_proof.rs"]
mod workspace_test_proof;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("--literal-pathspecs")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .current_dir(dir)
        .args(args)
        .output()?;
    if !out.status.success() {
        return Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )
        .into());
    }
    Ok(strip_ansi(&String::from_utf8(out.stdout)?)
        .trim()
        .to_owned())
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

#[derive(Debug, Eq, PartialEq)]
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

use susi_build_policy as vendor_regressions;

fn valid_oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn integration_head(root: &Path) -> Result<Option<String>> {
    let out = Command::new("git")
        .arg("--literal-pathspecs")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
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
        .arg("--literal-pathspecs")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
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
        .arg("--literal-pathspecs")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
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
    Ok(strip_ansi(&String::from_utf8(out.stdout)?)
        .trim()
        .to_owned())
}

fn receipt_matches(
    root: &Path,
    reference: &str,
    receipt: &str,
    accepted: &str,
    tree: &str,
) -> Result<bool> {
    let lines: Vec<_> = receipt.lines().collect();
    let keys = [
        "agent=",
        "source=",
        "claim-base=",
        "previous=",
        "commit=",
        "tree=",
    ];
    if lines.len() < 8 || lines[0] != "susi-local-integration-v1" {
        return Ok(false);
    }
    let fields: Option<Vec<_>> = keys
        .iter()
        .enumerate()
        .map(|(index, key)| lines[index + 1].strip_prefix(key))
        .collect();
    let Some(fields) = fields else {
        return Ok(false);
    };
    if name(fields[0]).is_err()
        || ![fields[1], fields[2], fields[4], fields[5]]
            .iter()
            .all(|field| valid_oid(field))
        || fields[4] != accepted
        || fields[5] != tree
        || reference != format!("refs/susi/integration/receipts/{}/{}", fields[0], fields[1])
        || lines[7..].iter().all(|line| line.trim().is_empty())
        || !lines[7..]
            .iter()
            .any(|line| line.contains(" => exit code: 0;"))
        || lines[7..]
            .iter()
            .any(|line| keys.iter().any(|key| line.starts_with(key)))
        || lines[7..].iter().any(|line| {
            line.split_once(" => ")
                .is_some_and(|(_, status)| !status.starts_with("exit code: 0;"))
        })
    {
        return Ok(false);
    }
    if !is_ancestor(root, fields[2], fields[1])? || !is_ancestor(root, fields[1], accepted)? {
        return Ok(false);
    }
    if fields[3] != "none"
        && (!valid_oid(fields[3])
            || !is_ancestor(root, fields[2], fields[3])?
            || !is_ancestor(root, fields[3], accepted)?)
    {
        return Ok(false);
    }
    Ok(true)
}

fn scope_base(root: &Path, claim: &Claim, source: &str) -> Result<String> {
    let Some(accepted) = integration_head(root)? else {
        return Ok(claim.base.clone());
    };
    if !is_ancestor(root, &claim.base, &accepted)? {
        return Err("accepted history does not descend from the reserved claim base".into());
    }
    let tree = git(root, &["rev-parse", &format!("{accepted}^{{tree}}")])?;
    let receipts = git(
        root,
        &[
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            "refs/susi/integration/receipts/",
        ],
    )?;
    let mut verified = false;
    for entry in receipts.lines() {
        let (reference, oid) = entry
            .split_once(' ')
            .ok_or("malformed receipt reference listing")?;
        let receipt = git(root, &["cat-file", "blob", oid])?;
        if receipt_matches(root, reference, &receipt, &accepted, &tree)? {
            verified = true;
            break;
        }
    }
    if !verified {
        return Err("accepted integration has no matching durable source/tree receipt".into());
    }
    // Only history shared with the tested integration is exempt. New changes
    // after that common ancestor remain subject to this worker's original scope.
    let bases = git(root, &["merge-base", "--all", source, &accepted])?;
    let bases: Vec<_> = bases.lines().collect();
    if bases.len() != 1 || !is_ancestor(root, &claim.base, bases[0])? {
        return Err("ambiguous or unrelated accepted merge base".into());
    }
    Ok(bases[0].to_owned())
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
    let scope_base = scope_base(root, claim, &source)?;
    let out = Command::new("git")
        .arg("--literal-pathspecs")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .current_dir(root)
        .args([
            "diff",
            "--no-renames",
            "--name-only",
            "-z",
            &scope_base,
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

fn sign_workspace_pass(root: &Path, key: &str) -> Result<String> {
    let signer = git(root, &["config", "--get", "user.signingkey"])?;
    let input = workspace_test_proof::signed_input(root, key)?;
    let out = workspace_test_proof::gpg(
        root,
        &["--local-user", &signer, "--armor", "--clearsign"],
        input.as_bytes(),
    )?;
    Ok(String::from_utf8(out.stdout)?)
}

/// Verify the exact merged checkout, using only native executable invocations.
/// The isolated home prevents tests from consuming the operator's runtime state;
/// Cargo and rustup retain their provisioned toolchain/cache locations.
fn verify_candidate(candidate: &Path, attempt: &Path) -> Result<Vec<String>> {
    native_provenance::require_unmodified_history(candidate)?;
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
    fs::create_dir_all(home.join("AppData/Local"))?;
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
    let mut gates: Vec<Vec<String>> = gates
        .iter()
        .map(|args| args.iter().map(|arg| (*arg).into()).collect())
        .collect();
    vendor_regressions::include_release_tooling(candidate, &mut gates);
    gates.extend(vendor_regressions::commands(candidate, host, &target)?);
    let mut gate_ids: Vec<String> = (0..gates.len())
        .map(|index| match index {
            0 => "format".into(),
            1 => "licenses".into(),
            2 => "clippy".into(),
            3 => "workspace-tests".into(),
            _ => format!("integration:{index}"),
        })
        .collect();
    let inputs: serde_json::Value =
        serde_json::from_slice(&fs::read(attempt.join("native-inputs.json"))?)?;
    for input in inputs
        .as_array()
        .ok_or("invalid native integration inputs")?
    {
        let worker = input["worker"].as_str().ok_or("missing native worker")?;
        let source = input["source"].as_str().ok_or("missing native source")?;
        let scopes = input["scopes"]
            .as_array()
            .ok_or("missing native scopes")?
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .ok_or("invalid native scope")
            })
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let task = native_provenance::task_for_claim(candidate, source, worker, &scopes)?;
        let mut args = task.argv[1..].to_vec();
        args.extend([
            "--target".into(),
            host.into(),
            "--target-dir".into(),
            text(&target)?.into(),
        ]);
        gate_ids.push(format!("task:{}", task.id));
        gates.push(args);
    }
    let plan: Vec<_> = gate_ids
        .iter()
        .zip(&gates)
        .map(|(id, args)| {
            let argv: Vec<_> = std::iter::once("cargo".to_owned())
                .chain(args.iter().cloned())
                .collect();
            serde_json::json!({"id":id,"argv":argv})
        })
        .collect();
    let mut native_gates = Vec::new();
    let mut sysroot = None;
    let mut evidence = Vec::new();
    for (index, args) in gates.iter().enumerate() {
        if sysroot.is_none()
            && matches!(
                args.first().map(String::as_str),
                Some("clippy" | "test" | "build")
            )
            && candidate.join("crates/susi-core/Cargo.toml").is_file()
        {
            sysroot = Some(trusted_sysroot::prepare(candidate)?);
        }
        let log_path = attempt.join(format!("gate-{index}.log"));
        let log = fs::File::create(&log_path)?;
        let started = std::time::Instant::now();
        let mut command = Command::new("cargo");
        compiler_env::apply_checked(&mut command)?;
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
        if let Some(sysroot) = &sysroot {
            sysroot.apply(&mut command)?;
        }
        let gate_target = args
            .windows(2)
            .find_map(|pair| (pair[0] == "--target-dir").then(|| PathBuf::from(&pair[1])))
            .unwrap_or_else(|| target.clone());
        command
            .current_dir(candidate)
            .args(args)
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
            .env("CARGO_TARGET_DIR", &gate_target)
            .env("CARGO_BUILD_TARGET_DIR", &gate_target)
            .env("CARGO_BUILD_BUILD_DIR", &gate_target)
            // Evidence logs are parsed; hosted CI exports CARGO_TERM_COLOR=always.
            .env("CARGO_TERM_COLOR", "never")
            // Host compiler wrappers may consult non-hermetic user-profile
            // configuration or run a shared daemon. The local gate is self-contained.
            .env("RUSTC_WRAPPER", "")
            .env("RUSTC_WORKSPACE_WRAPPER", "")
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log));
        let measure = |cmd: &Command| {
            sysroot.as_ref().filter(|_| index == 3).and_then(|s| {
                workspace_test_proof::fingerprint(
                    candidate,
                    args,
                    workspace_test_proof::Toolchain {
                        compiler: &compiler,
                        host,
                        sysroot: &s.evidence,
                    },
                    cmd,
                    &home,
                )
                .ok()
            })
        };
        let test_key = measure(&command);
        let status = command.status()?;
        // ExitStatus Display differs across OSes ("exit status" on Unix).
        // Durable receipts use one explicit numeric wire representation.
        let status_text = status
            .code()
            .map_or_else(|| status.to_string(), |code| format!("exit code: {code}"));
        let mut result = format!(
            "cargo {} => {status_text}; elapsed-ms={}",
            args.join(" "),
            started.elapsed().as_millis()
        );
        if status.success() && args.first().is_some_and(|arg| arg == "test") {
            let log_path = attempt.join(format!("gate-{index}.log"));
            let passed = workspace_test_count::verified_pass_count(&log_path).map_err(|error| {
                format!("gate failed: {result}; {}: {error}", log_path.display())
            })?;
            result.push_str(&format!("; observed-passes={passed}"));
        }
        evidence.push(result.clone());
        fs::write(attempt.join("verification.txt"), evidence.join("\n"))?;
        if !status.success() {
            let failed_log = native_provenance::store_blob(candidate, &fs::read(&log_path)?)?;
            fs::write(
                attempt.join(format!("gate-{index}.log.oid")),
                format!("{failed_log}\n"),
            )?;
            return Err(format!(
                "gate failed: {result}; log-blob={failed_log}; logs retained at {}",
                attempt.display(),
            )
            .into());
        }
        if let Some(key) = test_key.filter(|key| measure(&command).as_ref() == Some(key)) {
            evidence.push(workspace_test_proof::record(&key));
        }
        let argv: Vec<_> = std::iter::once("cargo".to_owned())
            .chain(args.iter().cloned())
            .collect();
        native_gates.push(native_provenance::gate(
            candidate,
            native_provenance::GateExecution {
                id: &gate_ids[index],
                argv: &argv,
                status,
                elapsed_ms: started.elapsed().as_millis(),
                log: &log_path,
            },
        )?);
        fs::write(
            attempt.join("native-gates.json"),
            serde_json::to_vec_pretty(&serde_json::json!({"plan":plan,"gates":native_gates}))?,
        )?;
    }
    if let Some(sysroot) = &sysroot {
        sysroot.recheck()?;
        evidence.push(format!(
            "sysroot={}",
            serde_json::to_string(&sysroot.evidence)?
        ));
    }
    Ok(evidence)
}

/// Serialize only integration, not agent edits/commits. Publish the tested commit
/// and its receipt together in one Git ref transaction. No primary checkout moves.
fn integrate_with<F>(root: &Path, claimants: &[&str], verify: F) -> Result<String>
where
    F: FnOnce(&Path, &Path) -> Result<Vec<String>>,
{
    integrate_many_with(root, claimants, verify)
}

fn integrate_many_with<F>(root: &Path, claimants: &[&str], verify: F) -> Result<String>
where
    F: FnOnce(&Path, &Path) -> Result<Vec<String>>,
{
    native_provenance::require_unmodified_history(root)?;
    let first = *claimants
        .first()
        .ok_or("at least one claimant is required")?;
    let mut unique = std::collections::BTreeSet::new();
    for claimant in claimants {
        name(claimant)?;
        if !unique.insert(*claimant) {
            return Err("duplicate integration claimant".into());
        }
    }
    let state = state(root)?;
    let _integration = Lock::acquire(&state.join("integration"))?;
    let inputs = {
        let _claims = Lock::acquire(&state)?;
        claimants
            .iter()
            .map(|claimant| {
                let path = state.join("claims").join(format!("{claimant}.claim"));
                let claim = read_claim(&path)?;
                let source = check_source(root, &claim)?;
                let bytes = fs::read(&path)?;
                Ok((claim, source, path, bytes))
            })
            .collect::<Result<Vec<_>>>()?
    };
    let previous = integration_head(root)?;
    let base = previous.clone().unwrap_or_else(|| inputs[0].0.base.clone());
    // Dependency chains must be rooted in accepted history. Two unaccepted
    // workers cannot authorize each other's common, unowned starting commit.
    let mut admitted = vec![false; inputs.len()];
    loop {
        let before = admitted.clone();
        for (index, (claim, _, _, _)) in inputs.iter().enumerate() {
            if is_ancestor(root, &claim.base, &base)? {
                admitted[index] = true;
            }
            for (other, (_, source, _, _)) in inputs.iter().enumerate() {
                if before[other] && is_ancestor(root, &claim.base, source)? {
                    admitted[index] = true;
                }
            }
        }
        if admitted.iter().all(|value| *value) {
            break;
        }
        if admitted == before {
            return Err(
                "integration history does not contain the claim base; reconcile explicitly".into(),
            );
        }
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let attempt = state
        .join("attempts")
        .join(format!("{first}-{stamp}-{}", std::process::id()));
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
        inputs
            .iter()
            .map(|(claim, source, _, _)| {
                format!(
                    "agent={}\nclaim-base={}\nsource={source}\nintegration-base={base}\n",
                    claim.name, claim.base
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),
    )?;
    fs::write(
        attempt.join("native-inputs.json"),
        serde_json::to_vec_pretty(
            &inputs
                .iter()
                .map(|(claim, source, _, _)| {
                    serde_json::json!({
                        "worker":claim.name,"source":source,"scopes":claim.scopes
                    })
                })
                .collect::<Vec<_>>(),
        )?,
    )?;
    for (_, source, _, _) in &inputs {
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
                source,
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
    }
    let head = git(&candidate, &["rev-parse", "HEAD"])?;
    let tree = git(&candidate, &["rev-parse", "HEAD^{tree}"])?;
    let mut evidence = match verify(&candidate, &attempt) {
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
    for (claim, source, path, bytes) in &inputs {
        if fs::read(path)? != *bytes || check_source(root, claim)? != *source {
            return Err(
                "source or reservation changed during verification; retry against its new head"
                    .into(),
            );
        }
    }
    // Sign only after the full gate, sysroot recheck, clean tree and claim checks.
    if let Some(key) = evidence
        .iter()
        .find_map(|line| line.strip_prefix(workspace_test_proof::PREFIX))
        .map(str::to_owned)
    {
        if let Ok(attestation) = sign_workspace_pass(&candidate, &key) {
            evidence.push(format!(
                "workspace-test-attestation-v1={}",
                serde_json::to_string(&attestation)?
            ));
        }
    }
    let zero = "0".repeat(head.len());
    let mut transaction = format!(
        "start\nupdate {INTEGRATION_REF} {head} {}\n",
        previous.as_deref().unwrap_or(&zero)
    );
    let mut receipts = Vec::new();
    let native_gates = if attempt.join("native-gates.json").is_file() {
        Some(serde_json::from_slice::<serde_json::Value>(&fs::read(
            attempt.join("native-gates.json"),
        )?)?)
    } else {
        None
    };
    for (claim, source, _, bytes) in &inputs {
        let claimant = &claim.name;
        let receipt = format!("susi-local-integration-v1\nagent={claimant}\nsource={source}\nclaim-base={}\nprevious={}\ncommit={head}\ntree={tree}\n{}\n", claim.base, previous.as_deref().unwrap_or("none"), evidence.join("\n"));
        let oid = git_input(root, &["hash-object", "-w", "--stdin"], receipt.as_bytes())?;
        let receipt_ref = format!("refs/susi/integration/receipts/{claimant}/{source}");
        transaction.push_str(&format!("create {receipt_ref} {oid}\n"));
        receipts.push(receipt_ref);
        if let Some(gates) = &native_gates {
            let native = native_provenance::receipt(
                root,
                native_provenance::IntegrationSource {
                    worker: claimant,
                    claim_bytes: bytes,
                    source,
                    scope_base: &scope_base(root, claim, source)?,
                    previous: previous.as_deref(),
                    accepted: &head,
                },
                gates,
            )?;
            let native_oid = native_provenance::store_blob(root, &serde_json::to_vec(&native)?)?;
            let native_ref = format!("refs/susi/provenance/receipts/{claimant}/{source}");
            transaction.push_str(&format!("create {native_ref} {native_oid}\n"));
            receipts.push(native_ref);
        }
    }
    transaction.push_str("prepare\ncommit\n");
    git_input(root, &["update-ref", "--stdin"], transaction.as_bytes())?;
    fs::write(
        attempt.join("status"),
        format!(
            "accepted {head}\nreceipt {}\nreceipts {}\n",
            receipts.join("\nreceipt "),
            receipts.join(" ")
        ),
    )?;
    for (_, _, path, _) in &inputs {
        fs::remove_file(path)?;
    }
    // The accepted commit and receipt are durable; retain failure candidates,
    // but reclaim successful scratch builds without touching agent worktrees.
    if let Err(error) = git(
        root,
        &[
            "-c",
            "core.hooksPath=",
            "-c",
            "core.longpaths=true",
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
    println!(
        "Integrated {}: {head}; receipts {}. Primary checkout unchanged.",
        claimants.join(", "),
        receipts.join(", ")
    );
    Ok(head)
}

fn start(root: &Path, claimant: &str, raw_scopes: &[String]) -> Result<PathBuf> {
    native_provenance::require_unmodified_history(root)?;
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

enum ScopeChange {
    Extend,
    Shrink,
}

#[cfg(test)]
fn extend(root: &Path, claimant: &str, raw_scopes: &[String]) -> Result<()> {
    update_scopes(root, claimant, raw_scopes, ScopeChange::Extend)
}

fn update_scopes(
    root: &Path,
    claimant: &str,
    raw_scopes: &[String],
    change: ScopeChange,
) -> Result<()> {
    native_provenance::require_unmodified_history(root)?;
    name(claimant)?;
    let requested = raw_scopes
        .iter()
        .map(|value| scope(value))
        .collect::<Result<Vec<_>>>()?;
    if requested.is_empty() {
        return Err("at least one --scope is required".into());
    }
    let state = state(root)?;
    let _lock = Lock::acquire(&state)?;
    let path = state.join("claims").join(format!("{claimant}.claim"));
    let mut claim = read_claim(&path)?;
    let actual = PathBuf::from(git(root, &["rev-parse", "--show-toplevel"])?);
    if actual.canonicalize()? != claim.worktree.canonicalize()?
        || git(root, &["branch", "--show-current"])? != format!("local-agent/{claimant}")
    {
        return Err("scope changes must run inside the claimed agent branch and worktree".into());
    }
    if !git(root, &["status", "--porcelain"])?.is_empty() {
        return Err("scope changes require a clean agent worktree".into());
    }
    match change {
        ScopeChange::Extend => {
            for active in claims(&state)? {
                if active.name != claimant
                    && requested
                        .iter()
                        .any(|new| active.scopes.iter().any(|held| overlaps(new, held)))
                {
                    return Err(format!("requested scope overlaps agent {}", active.name).into());
                }
            }
            claim.scopes.extend(requested);
        }
        ScopeChange::Shrink => {
            if requested.iter().any(|scope| !claim.scopes.contains(scope)) {
                return Err("shrink requires an exact held scope".into());
            }
            claim.scopes.retain(|scope| !requested.contains(scope));
            if claim.scopes.is_empty() {
                return Err("use release to relinquish the last scope".into());
            }
            if git(&claim.worktree, &["rev-parse", "HEAD"])? != claim.base {
                // Validate the proposed claim against the actual committed
                // delta before atomically replacing the reservation.
                check_source(root, &claim)?;
            }
        }
    }
    claim.scopes.sort();
    claim.scopes.dedup();
    let data = format!(
        "susi-local-claim-v2\n{}\n{}\n{}\n{}\n",
        claim.name,
        text(&claim.worktree)?,
        claim.base,
        claim.scopes.join("\n")
    );
    let temporary = path.with_extension("claim-next");
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    output.write_all(data.as_bytes())?;
    output.sync_all()?;
    drop(output);
    fs::rename(temporary, path)?;
    Ok(())
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
fn changed_paths(root: &Path, base: &str) -> Result<Vec<String>> {
    let mut paths = Vec::new();
    for args in [
        vec![
            "diff",
            "--cached",
            "--no-renames",
            "--name-only",
            "-z",
            base,
            "--",
        ],
        vec!["diff", "--no-renames", "--name-only", "-z"],
        vec!["ls-files", "--others", "--exclude-standard", "-z"],
    ] {
        let out = Command::new("git")
            .arg("--literal-pathspecs")
            .env("GIT_NO_REPLACE_OBJECTS", "1")
            .current_dir(root)
            .args(args)
            .output()?;
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

fn claimed_changes(claim: &Claim) -> Result<Vec<String>> {
    claimed_changes_against(claim, "HEAD")
}

fn claimed_changes_against(claim: &Claim, base: &str) -> Result<Vec<String>> {
    let paths = changed_paths(&claim.worktree, base)?;
    for path in &paths {
        let normalized = scope(path)?;
        if !claim.scopes.iter().any(|held| owns_path(held, &normalized)) {
            return Err(format!(
                "changed file is outside {}'s reserved scopes: {path}",
                claim.name
            )
            .into());
        }
    }
    Ok(paths)
}

/// Scope-guarded local commit, not a test/release gate. It never changes the
/// repository's shared hook configuration or invokes its network-bound hooks.
fn commit(root: &Path, claimant: &str, message: &str, signer: Option<&str>) -> Result<()> {
    scoped_commit(root, claimant, message, signer, None)
}

fn scoped_commit(
    root: &Path,
    claimant: &str,
    message: &str,
    signer: Option<&str>,
    merge_parent: Option<&str>,
) -> Result<()> {
    native_provenance::require_unmodified_history(root)?;
    name(claimant)?;
    if message.trim().is_empty() || message.contains('\0') {
        return Err("commit message must be nonempty and contain no NUL".into());
    }
    if let Some(key) = signer {
        if !matches!(key.len(), 40 | 64) || !key.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("--sign requires a complete OpenPGP fingerprint".into());
        }
    }
    let state = state(root)?;
    let _lock = Lock::acquire(&state)?;
    let claim_path = state.join("claims").join(format!("{claimant}.claim"));
    let claim = read_claim(&claim_path)?;
    let actual_root = PathBuf::from(git(root, &["rev-parse", "--show-toplevel"])?);
    if actual_root.canonicalize()? != claim.worktree.canonicalize()? {
        return Err("commit must run inside the claimed agent's worktree".into());
    }
    if self::state(&claim.worktree)? != state
        || git(&claim.worktree, &["branch", "--show-current"])? != format!("local-agent/{claimant}")
    {
        return Err("claim repository or branch does not match".into());
    }
    if signer.is_some() && merge_parent.is_some() {
        return Err("finish-merge does not support signing".into());
    }
    let signing = match signer {
        Some(key) => Some((key, git(&claim.worktree, &["rev-parse", "HEAD"])?)),
        None => None,
    };
    // A normal task commit must never consume another Git operation.
    let mut forbidden = Vec::new();
    for marker in [
        "MERGE_HEAD",
        "MERGE_AUTOSTASH",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "sequencer",
        "rebase-merge",
        "rebase-apply",
    ] {
        if marker == "MERGE_HEAD" && merge_parent.is_some() {
            continue;
        }
        let path = git(
            &claim.worktree,
            &["rev-parse", "--path-format=absolute", "--git-path", marker],
        )?;
        if Path::new(&path).exists() {
            return Err("finish the existing Git operation before committing".into());
        }
        forbidden.push(PathBuf::from(path));
    }
    let paths = claimed_changes(&claim)?;
    if paths.is_empty() && merge_parent.is_none() {
        return Err("nothing to commit".into());
    }
    let hooks = state.join("empty-hooks");
    fs::create_dir_all(&hooks)?;
    if fs::symlink_metadata(&hooks)?.file_type().is_symlink()
        || fs::read_dir(&hooks)?.next().is_some()
    {
        return Err("local empty-hooks directory must be an empty real directory".into());
    }
    if let Some(expected) = merge_parent {
        return finish_scoped_merge(&claim, expected, message, &hooks, &forbidden);
    }
    // NUL-delimited literal pathspecs preserve spaces and metacharacters and
    // avoid the Windows command-line length limit for large tasks.
    let mut add = Command::new("git")
        .arg("--literal-pathspecs")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
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
    let output = if let Some((key, parent)) = signing {
        // Sign and verify a detached commit object before publishing it. A
        // missing signer, wrong identity or concurrent branch move leaves the
        // previous head intact and the worker can retry the staged change.
        let tree = git(&claim.worktree, &["write-tree"])?;
        let claim_bytes = fs::read(&claim_path)?;
        // An optional hardware signer can wait for its operator. Unrelated
        // workers must still be able to reserve, commit and release scopes.
        drop(_lock);
        let signed = git(
            &claim.worktree,
            &[
                "--no-replace-objects",
                "-c",
                "gpg.format=openpgp",
                "commit-tree",
                &tree,
                "-p",
                &parent,
                &format!("-S{key}"),
                "-m",
                message,
                "-m",
                &trailer,
            ],
        )?;
        git(
            &claim.worktree,
            &[
                "--no-replace-objects",
                "-c",
                "gpg.format=openpgp",
                "verify-commit",
                &signed,
            ],
        )?;
        let identities = git(
            &claim.worktree,
            &[
                "--no-replace-objects",
                "-c",
                "gpg.format=openpgp",
                "show",
                "-s",
                "--format=%GF%n%GP",
                &signed,
            ],
        )?;
        if !identities
            .lines()
            .any(|fingerprint| fingerprint.eq_ignore_ascii_case(key))
        {
            return Err("signed commit did not verify under the requested fingerprint".into());
        }
        let _publish = Lock::acquire(&state)?;
        if fs::read(&claim_path)? != claim_bytes
            || git(&claim.worktree, &["branch", "--show-current"])?
                != format!("local-agent/{claimant}")
            || git(&claim.worktree, &["write-tree"])? != tree
        {
            return Err("worker reservation, branch or staged tree changed while signing".into());
        }
        claimed_changes(&claim)?;
        git(
            &claim.worktree,
            &[
                "-c",
                &hook_config,
                "update-ref",
                &format!("refs/heads/local-agent/{claimant}"),
                &signed,
                &parent,
            ],
        )?;
        format!("Verified OpenPGP commit {signed} with fingerprint {key}.")
    } else {
        git(
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
        )?
    };
    println!(
        "{output}\nLocal scope guard passed; this commit is not a test or release attestation."
    );
    Ok(())
}

/// Finish only a resolved, explicitly pinned merge in the reserved worktree.
/// ORIG_HEAD lets a retry finish cleanup if publication already succeeded.
fn finish_scoped_merge(
    claim: &Claim,
    expected: &str,
    message: &str,
    hooks: &Path,
    forbidden: &[PathBuf],
) -> Result<()> {
    let root = &claim.worktree;
    let claim_path = state(root)?
        .join("claims")
        .join(format!("{}.claim", claim.name));
    if !matches!(expected.len(), 40 | 64) || !expected.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("finish-merge requires a full commit object ID".into());
    }
    let object = git(
        root,
        &["rev-parse", "--verify", &format!("{expected}^{{commit}}")],
    )?;
    if object != expected {
        return Err("expected parent must name the commit itself".into());
    }
    let original = git(root, &["rev-parse", "--verify", "ORIG_HEAD^{commit}"])?;
    if original == expected {
        return Err("merge parents must be distinct".into());
    }
    git(
        root,
        &["merge-base", "--is-ancestor", &claim.base, &original],
    )?;
    let merge_head = git(
        root,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "MERGE_HEAD",
        ],
    )?;
    let tree = git(root, &["write-tree"])?;
    let branch = format!("local-agent/{}", claim.name);
    let validate = || -> Result<()> {
        if read_claim(&claim_path)? != *claim
            || forbidden.iter().any(|p| p.exists())
            || fs::read_to_string(&merge_head)?.lines().collect::<Vec<_>>() != [expected]
            || git(root, &["branch", "--show-current"])? != branch
            || git(root, &["rev-parse", "--verify", "ORIG_HEAD^{commit}"])? != original
            || git(root, &["write-tree"])? != tree
        {
            return Err("merge parent, branch or staged tree changed".into());
        }
        for args in [
            &["ls-files", "--unmerged", "-z"][..],
            &["diff", "--name-only", "-z", "--"][..],
            &["ls-files", "--others", "--exclude-standard", "-z"][..],
        ] {
            if !git(root, args)?.is_empty() {
                return Err(
                    "finish-merge requires a resolved index and fully staged changes".into(),
                );
            }
        }
        claimed_changes_against(claim, &original)?;
        Ok(())
    };
    validate()?;
    let message = format!("{}\n\nTask: LOCAL-{}", message.trim(), claim.name);
    let expected_shape = format!("{tree}\n{original} {expected}\n{message}");
    let head = git(root, &["rev-parse", "HEAD"])?;
    let commit = if head == original {
        git_input(
            root,
            &[
                "--no-replace-objects",
                "commit-tree",
                &tree,
                "-p",
                &original,
                "-p",
                expected,
                "-F",
                "-",
            ],
            message.as_bytes(),
        )?
    } else {
        head.clone()
    };
    if git(
        root,
        &[
            "--no-replace-objects",
            "show",
            "-s",
            "--format=%T%n%P%n%B",
            &commit,
        ],
    )? != expected_shape
    {
        return Err("refusing a merge with different parents, tree or task message".into());
    }
    let hooks = format!("core.hooksPath={}", text(hooks)?);
    validate()?;
    if head == original {
        git(
            root,
            &[
                "-c",
                &hooks,
                "update-ref",
                &format!("refs/heads/{branch}"),
                &commit,
                &original,
            ],
        )?;
    }
    validate()?;
    if git(root, &["rev-parse", "HEAD"])? != commit {
        return Err("claimed branch moved before merge cleanup".into());
    }
    git(root, &["-c", &hooks, "merge", "--quit"])?;
    println!("Finished scoped merge {commit}, parents {original} {expected}, tree {tree}.");
    Ok(())
}

const SHIP_CONTEXTS: [&str; 4] = [
    "susi-local/format",
    "susi-local/licenses",
    "susi-local/clippy",
    "susi-local/tests",
];

fn status_description(text: &str) -> String {
    text.chars().take(140).collect()
}

fn require_ship_ref(git_ref: &str) -> Result<()> {
    let name = git_ref
        .strip_prefix("refs/heads/ship/")
        .ok_or("ship pushes only refs/heads/ship/<id>")?;
    if name.is_empty()
        || name.contains('/')
        || name == "main"
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err("ship pushes only refs/heads/ship/<id>".into());
    }
    Ok(())
}

trait Remote {
    fn fetch_main(&mut self, root: &Path) -> Result<String>;
    fn push_ref(&mut self, root: &Path, sha: &str, git_ref: &str) -> Result<()>;
    fn open_pr(&mut self, root: &Path, branch: &str, title: &str, body: &str) -> Result<u64>;
    fn post_status(
        &mut self,
        root: &Path,
        sha: &str,
        context: &str,
        description: &str,
    ) -> Result<()>;
    fn merge_pr(&mut self, root: &Path, number: u64, sha: &str) -> Result<String>;
}

struct GhRemote;

fn gh_command(root: &Path) -> Command {
    let mut command = Command::new("gh");
    command
        .current_dir(root)
        .env_remove("GH_FORCE_TTY")
        .env("NO_COLOR", "1")
        .env("CLICOLOR", "0");
    command
}

fn strip_ansi(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for next in chars.by_ref() {
                    if next.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(character);
    }
    out
}

fn gh_output(root: &Path, args: &[String]) -> Result<String> {
    let out = gh_command(root).args(args).output()?;
    if !out.status.success() {
        return Err(format!(
            "gh {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )
        .into());
    }
    Ok(strip_ansi(&String::from_utf8(out.stdout)?)
        .trim()
        .to_owned())
}

fn repo_slug(root: &Path) -> Result<String> {
    let slug = gh_output(
        root,
        &[
            "repo".into(),
            "view".into(),
            "--json".into(),
            "nameWithOwner".into(),
            "-q".into(),
            ".nameWithOwner".into(),
        ],
    )?;
    let mut parts = slug.split('/');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(owner), Some(name), None)
            if !owner.is_empty()
                && !name.is_empty()
                && owner
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
                && name.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_' || byte == b'.'
                }) =>
        {
            Ok(slug)
        }
        _ => Err("unexpected repository name".into()),
    }
}

fn parse_pr_number(text: &str) -> Result<u64> {
    let line = text.lines().last().unwrap_or("").trim();
    let number = line.rsplit('/').next().unwrap_or("");
    number
        .parse::<u64>()
        .map_err(|_| format!("pull request url did not end in a number: {line}").into())
}

impl Remote for GhRemote {
    fn fetch_main(&mut self, root: &Path) -> Result<String> {
        git(root, &["fetch", "--quiet", "origin", "main"])?;
        let oid = git(root, &["rev-parse", "origin/main"])?;
        if !valid_oid(&oid) {
            return Err("origin/main is not a commit oid".into());
        }
        Ok(oid)
    }

    fn push_ref(&mut self, root: &Path, sha: &str, git_ref: &str) -> Result<()> {
        require_ship_ref(git_ref)?;
        if !valid_oid(sha) {
            return Err("ship push requires a commit oid".into());
        }
        git(
            root,
            &[
                "-c",
                "core.hooksPath=",
                "push",
                "--quiet",
                "origin",
                &format!("{sha}:{git_ref}"),
            ],
        )?;
        Ok(())
    }

    fn open_pr(&mut self, root: &Path, branch: &str, title: &str, body: &str) -> Result<u64> {
        require_ship_ref(&format!("refs/heads/{branch}"))?;
        let url = gh_output(
            root,
            &[
                "pr".into(),
                "create".into(),
                "--base".into(),
                "main".into(),
                "--head".into(),
                branch.into(),
                "--title".into(),
                title.into(),
                "--body".into(),
                body.into(),
            ],
        )?;
        parse_pr_number(&url)
    }

    fn post_status(
        &mut self,
        root: &Path,
        sha: &str,
        context: &str,
        description: &str,
    ) -> Result<()> {
        if !valid_oid(sha) {
            return Err("status requires a commit oid".into());
        }
        if !SHIP_CONTEXTS.contains(&context) {
            return Err("status context is not a local ship gate".into());
        }
        let slug = repo_slug(root)?;
        let description = status_description(description);
        gh_output(
            root,
            &[
                "api".into(),
                format!("repos/{slug}/statuses/{sha}"),
                "-f".into(),
                "state=success".into(),
                "-f".into(),
                format!("context={context}"),
                "-f".into(),
                format!("description={description}"),
            ],
        )?;
        Ok(())
    }

    fn merge_pr(&mut self, root: &Path, number: u64, sha: &str) -> Result<String> {
        if !valid_oid(sha) {
            return Err("merge requires a commit oid".into());
        }
        gh_output(
            root,
            &[
                "pr".into(),
                "merge".into(),
                number.to_string(),
                "--merge".into(),
                "--match-head-commit".into(),
                sha.into(),
            ],
        )?;
        let merged = gh_output(
            root,
            &[
                "pr".into(),
                "view".into(),
                number.to_string(),
                "--json".into(),
                "mergeCommit".into(),
                "-q".into(),
                ".mergeCommit.oid".into(),
            ],
        )?;
        if !valid_oid(&merged) {
            return Err("merge did not report a commit oid".into());
        }
        Ok(merged)
    }
}

#[derive(Debug)]
struct ShipReport {
    pr: u64,
    head: String,
    merge: String,
}

fn gate_evidence<'a>(evidence: &'a [String], subcommand: &str) -> Result<&'a str> {
    evidence
        .iter()
        .find(|line| {
            let Some((command, status)) = line.split_once(" => ") else {
                return false;
            };
            status.starts_with("exit code: 0;")
                && command.split_whitespace().nth(1) == Some(subcommand)
        })
        .map(String::as_str)
        .ok_or_else(|| format!("missing passing {subcommand} evidence").into())
}

fn require_ship_evidence(evidence: &[String]) -> Result<()> {
    if evidence.is_empty() {
        return Err("verification returned no evidence".into());
    }
    for line in evidence {
        match line.split_once(" => ") {
            Some((_, status)) if status.starts_with("exit code: 0;") => {}
            _ => return Err(format!("refusing to publish a failed gate: {line}").into()),
        }
    }
    for subcommand in ["fmt", "deny", "clippy", "test"] {
        gate_evidence(evidence, subcommand)?;
    }
    Ok(())
}

struct SourceInput {
    claim: Claim,
    source: String,
    bytes: Vec<u8>,
    path: PathBuf,
}

fn ship_scope_base(root: &Path, claim: &Claim, source: &str, publish_base: &str) -> Result<String> {
    match scope_base(root, claim, source) {
        Ok(base) => Ok(base),
        Err(error) => {
            let on_publish_base =
                claim.base == publish_base || is_ancestor(root, &claim.base, publish_base)?;
            if error
                .to_string()
                .contains("accepted history does not descend")
                && is_ancestor(root, &claim.base, source)?
                && on_publish_base
            {
                Ok(claim.base.clone())
            } else {
                Err(error)
            }
        }
    }
}

fn check_ship_source(root: &Path, claim: &Claim, publish_base: &str) -> Result<String> {
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
    let scope_base = ship_scope_base(root, claim, &source, publish_base)?;
    let out = Command::new("git")
        .arg("--literal-pathspecs")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .current_dir(root)
        .args([
            "diff",
            "--no-renames",
            "--name-only",
            "-z",
            &scope_base,
            &source,
        ])
        .output()?;
    if !out.status.success() {
        return Err("cannot inspect committed scope".into());
    }
    for path in out
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        let path = scope(std::str::from_utf8(path)?)?;
        if !claim.scopes.iter().any(|held| owns_path(held, &path)) {
            return Err(format!("committed file is outside reserved scope: {path}").into());
        }
    }
    Ok(source)
}

fn load_sources(root: &Path, claimants: &[&str], publish_base: &str) -> Result<Vec<SourceInput>> {
    if claimants.is_empty() {
        return Err("at least one claimant is required".into());
    }
    let mut unique = std::collections::BTreeSet::new();
    for claimant in claimants {
        name(claimant)?;
        if !unique.insert(*claimant) {
            return Err("duplicate integration claimant".into());
        }
    }
    let workflow = state(root)?;
    let _claims = Lock::acquire(&workflow)?;
    claimants
        .iter()
        .map(|claimant| {
            let path = workflow.join("claims").join(format!("{claimant}.claim"));
            let bytes = fs::read(&path)?;
            let claim = read_claim(&path)?;
            let source = check_ship_source(root, &claim, publish_base)?;
            Ok(SourceInput {
                claim,
                source,
                bytes,
                path,
            })
        })
        .collect()
}

fn remove_candidate(root: &Path, candidate: &Path) {
    if let Ok(path) = text(candidate) {
        let _ = git(
            root,
            &[
                "-c",
                "core.hooksPath=",
                "-c",
                "core.longpaths=true",
                "worktree",
                "remove",
                "--force",
                path,
            ],
        );
    }
}

fn merge_onto(root: &Path, base: &str, inputs: &[SourceInput]) -> Result<PathBuf> {
    if !valid_oid(base) {
        return Err("ship base is not a commit oid".into());
    }
    let checkout = PathBuf::from(git(root, &["rev-parse", "--show-toplevel"])?);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
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
            base,
        ],
    )?;
    for input in inputs {
        if let Err(error) = git(
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
                &input.source,
            ],
        ) {
            remove_candidate(root, &candidate);
            return Err(error);
        }
    }
    Ok(candidate)
}

fn record_acceptance(
    root: &Path,
    inputs: &[SourceInput],
    head: &str,
    evidence: &[String],
) -> Result<()> {
    let tree = git(root, &["rev-parse", &format!("{head}^{{tree}}")])?;
    let previous = integration_head(root)?;
    if let Some(previous) = &previous {
        if !is_ancestor(root, previous, head)? {
            git(
                root,
                &["update-ref", "refs/susi/integration/displaced", previous],
            )?;
            eprintln!(
                "preserved unrelated integration ref {previous} at refs/susi/integration/displaced"
            );
        }
    }
    let zero = "0".repeat(head.len());
    let mut transaction = format!(
        "start\nupdate {INTEGRATION_REF} {head} {}\n",
        previous.as_deref().unwrap_or(&zero)
    );
    for input in inputs {
        let receipt = format!(
            "susi-local-integration-v1\nagent={}\nsource={}\nclaim-base={}\nprevious={}\ncommit={head}\ntree={tree}\n{}\n",
            input.claim.name,
            input.source,
            input.claim.base,
            previous.as_deref().unwrap_or("none"),
            evidence.join("\n")
        );
        let oid = git_input(root, &["hash-object", "-w", "--stdin"], receipt.as_bytes())?;
        let receipt_ref = format!(
            "refs/susi/integration/receipts/{}/{}",
            input.claim.name, input.source
        );
        transaction.push_str(&format!("create {receipt_ref} {oid}\n"));
    }
    transaction.push_str("prepare\ncommit\n");
    git_input(root, &["update-ref", "--stdin"], transaction.as_bytes())?;
    for input in inputs {
        fs::remove_file(&input.path)?;
    }
    Ok(())
}

fn prove_ship<F>(
    root: &Path,
    workflow: &Path,
    base: &str,
    inputs: &[SourceInput],
    verify: &mut F,
) -> Result<(PathBuf, String, Vec<String>)>
where
    F: FnMut(&Path, &Path) -> Result<Vec<String>>,
{
    let candidate = merge_onto(root, base, inputs)?;
    let before = git(&candidate, &["rev-parse", "HEAD"])?;
    let evidence = match verify(&candidate, &workflow.join("publish")) {
        Ok(evidence) => evidence,
        Err(error) => {
            remove_candidate(root, &candidate);
            return Err(error);
        }
    };
    let head = git(&candidate, &["rev-parse", "HEAD"])?;
    if head != before
        || !git(
            &candidate,
            &["status", "--porcelain", "--untracked-files=all"],
        )?
        .is_empty()
    {
        remove_candidate(root, &candidate);
        return Err("verification changed the candidate tree; refusing promotion".into());
    }
    if let Err(error) = require_ship_evidence(&evidence) {
        remove_candidate(root, &candidate);
        return Err(error);
    }
    Ok((candidate, head, evidence))
}

fn ship<F>(
    root: &Path,
    claimants: &[&str],
    remote: &mut dyn Remote,
    mut verify: F,
) -> Result<ShipReport>
where
    F: FnMut(&Path, &Path) -> Result<Vec<String>>,
{
    let workflow = state(root)?;
    let _publish = Lock::acquire(&workflow.join("publish"))?;
    let base = remote.fetch_main(root)?;
    let inputs = load_sources(root, claimants, &base)?;
    let proved = prove_ship(root, &workflow, &base, &inputs, &mut verify)?;
    let mut candidate = proved.0;
    let mut head = proved.1;
    let mut evidence = proved.2;
    let again = remote.fetch_main(root)?;
    if again != base {
        remove_candidate(root, &candidate);
        let proved = prove_ship(root, &workflow, &again, &inputs, &mut verify)?;
        candidate = proved.0;
        head = proved.1;
        evidence = proved.2;
        let third = remote.fetch_main(root)?;
        if third != again {
            remove_candidate(root, &candidate);
            return Err("origin/main moved twice".into());
        }
    }
    {
        let _claims = Lock::acquire(&workflow)?;
        for input in &inputs {
            if fs::read(&input.path)? != input.bytes
                || check_ship_source(root, &input.claim, &base)? != input.source
            {
                remove_candidate(root, &candidate);
                return Err(
                    "source or reservation changed during verification; retry against its new head"
                        .into(),
                );
            }
        }
        record_acceptance(root, &inputs, &head, &evidence)?;
    }
    let git_ref = format!(
        "refs/heads/ship/{}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs(),
        &head[..12]
    );
    let branch = git_ref
        .strip_prefix("refs/heads/")
        .ok_or("ship ref is missing heads")?;
    if let Err(error) = remote.push_ref(root, &head, &git_ref) {
        remove_candidate(root, &candidate);
        return Err(error);
    }
    let title = git(root, &["log", "-1", "--format=%s", &inputs[0].source])?;
    let pr = match remote.open_pr(root, branch, &title, &evidence.join("\n")) {
        Ok(pr) => pr,
        Err(error) => {
            remove_candidate(root, &candidate);
            return Err(error);
        }
    };
    for context in SHIP_CONTEXTS {
        let subcommand = match context {
            "susi-local/format" => "fmt",
            "susi-local/licenses" => "deny",
            "susi-local/clippy" => "clippy",
            "susi-local/tests" => "test",
            _ => return Err("unknown ship context".into()),
        };
        let line = gate_evidence(&evidence, subcommand)?;
        if let Err(error) = remote.post_status(root, &head, context, line) {
            remove_candidate(root, &candidate);
            return Err(error);
        }
    }
    let merge = match remote.merge_pr(root, pr, &head) {
        Ok(merge) => merge,
        Err(error) => {
            remove_candidate(root, &candidate);
            return Err(error);
        }
    };
    remove_candidate(root, &candidate);
    Ok(ShipReport { pr, head, merge })
}

fn ship_retry(root: &Path, remote: &mut dyn Remote) -> Result<ShipReport> {
    let workflow = state(root)?;
    let _publish = Lock::acquire(&workflow.join("publish"))?;
    let Some(head) = integration_head(root)? else {
        return Err("missing receipt".into());
    };
    let tree = git(root, &["rev-parse", &format!("{head}^{{tree}}")])?;
    let listing = git(
        root,
        &[
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            "refs/susi/integration/receipts/",
        ],
    )?;
    let mut evidence = None;
    for entry in listing.lines() {
        let Some((reference, oid)) = entry.split_once(' ') else {
            continue;
        };
        let receipt = git(root, &["cat-file", "blob", oid])?;
        if !receipt_matches(root, reference, &receipt, &head, &tree)? {
            continue;
        }
        let lines: Vec<String> = receipt
            .lines()
            .skip(7)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect();
        require_ship_evidence(&lines)?;
        evidence = Some(lines);
        break;
    }
    let evidence = evidence.ok_or("missing receipt")?;
    let base = remote.fetch_main(root)?;
    if !is_ancestor(root, &base, &head)? {
        return Err("integration head is not based on origin/main".into());
    }
    let again = remote.fetch_main(root)?;
    if again != base {
        return Err("origin/main moved twice".into());
    }
    publish_head(root, &head, &evidence, remote)
}

fn publish_head(
    root: &Path,
    head: &str,
    evidence: &[String],
    remote: &mut dyn Remote,
) -> Result<ShipReport> {
    let git_ref = format!(
        "refs/heads/ship/{}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs(),
        &head[..12]
    );
    let branch = git_ref
        .strip_prefix("refs/heads/")
        .ok_or("ship ref is missing heads")?;
    remote.push_ref(root, head, &git_ref)?;
    let title = git(root, &["log", "-1", "--format=%s", head])?;
    let pr = remote.open_pr(root, branch, &title, &evidence.join("\n"))?;
    for context in SHIP_CONTEXTS {
        let subcommand = match context {
            "susi-local/format" => "fmt",
            "susi-local/licenses" => "deny",
            "susi-local/clippy" => "clippy",
            "susi-local/tests" => "test",
            _ => return Err("unknown ship context".into()),
        };
        let line = gate_evidence(evidence, subcommand)?;
        remote.post_status(root, head, context, line)?;
    }
    let merge = remote.merge_pr(root, pr, head)?;
    Ok(ShipReport {
        pr,
        head: head.to_owned(),
        merge,
    })
}

fn print_ship(report: &ShipReport) {
    println!(
        "Shipped {} as pull request {}, merge {}.",
        report.head, report.pr, report.merge
    );
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RepoKind {
    Susi,
    Commercial,
}

fn repo_kind(root: &Path) -> Result<RepoKind> {
    if root.join("crates/susi-core/Cargo.toml").is_file() {
        return Ok(RepoKind::Susi);
    }
    let manifest = root.join("Cargo.toml");
    if manifest.is_file() {
        let text = fs::read_to_string(&manifest)?;
        if text
            .lines()
            .any(|line| line.trim() == "name = \"susi-commercial\"")
        {
            return Ok(RepoKind::Commercial);
        }
    }
    Err("unrecognized checkout".into())
}

fn commercial_gate_argv() -> Vec<Vec<String>> {
    vec![
        vec!["fmt".into(), "--all".into(), "--check".into()],
        vec![
            "clippy".into(),
            "--workspace".into(),
            "--all-targets".into(),
            "--locked".into(),
            "--".into(),
            "-D".into(),
            "warnings".into(),
        ],
        vec!["test".into(), "--workspace".into(), "--locked".into()],
        vec![
            "deny".into(),
            "check".into(),
            "licenses".into(),
            "advisories".into(),
            "bans".into(),
            "sources".into(),
        ],
    ]
}

fn commercial_gate(candidate: &Path, attempt: &Path) -> Result<Vec<String>> {
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
    let home = attempt.join("test-home");
    fs::create_dir_all(home.join("AppData/Local"))?;
    let target = candidate.join("target");
    let mut evidence = Vec::new();
    for args in commercial_gate_argv() {
        let started = std::time::Instant::now();
        let mut command = Command::new("cargo");
        command.current_dir(candidate).args(&args);
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
                ]
                .iter()
                .any(|part| name.contains(part))
            {
                command.env_remove(key);
            }
        }
        let log_path = attempt.join(format!("commercial-{}.log", evidence.len()));
        let log = fs::File::create(&log_path)?;
        command
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env("SUSI_HOME", home.join("susi"))
            .env("CARGO_TARGET_DIR", &target)
            .env("CARGO_TERM_COLOR", "never")
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log));
        let status = command.status()?;
        let code = status.code().unwrap_or(1);
        let line = format!(
            "cargo {} => exit code: {code}; elapsed-ms={}",
            args.join(" "),
            started.elapsed().as_millis()
        );
        evidence.push(line.clone());
        if !status.success() {
            return Err(format!("gate failed: {line}; log {}", log_path.display()).into());
        }
    }
    require_ship_evidence(&evidence)?;
    Ok(evidence)
}

fn ruleset_body(kind: RepoKind) -> serde_json::Value {
    let checks: Vec<_> = SHIP_CONTEXTS
        .iter()
        .map(|context| serde_json::json!({ "context": context }))
        .collect();
    let bypass = match kind {
        RepoKind::Susi => serde_json::json!([]),
        RepoKind::Commercial => serde_json::json!([{
            "actor_id": 5,
            "actor_type": "RepositoryRole",
            "bypass_mode": "always"
        }]),
    };
    let name = match kind {
        RepoKind::Susi => "susi-main-workflow",
        RepoKind::Commercial => "commercial-main-pr-only",
    };
    serde_json::json!({
        "name": name,
        "target": "branch",
        "enforcement": "active",
        "conditions": { "ref_name": { "include": ["refs/heads/main"], "exclude": [] } },
        "bypass_actors": bypass,
        "rules": [
            { "type": "deletion" },
            { "type": "non_fast_forward" },
            { "type": "pull_request", "parameters": {
                "required_approving_review_count": 0,
                "dismiss_stale_reviews_on_push": false,
                "require_code_owner_review": false,
                "require_last_push_approval": false,
                "required_review_thread_resolution": false,
                "require_extra_approval_for_unattributed_changes": true,
                "allowed_merge_methods": ["merge", "squash", "rebase"]
            } },
            { "type": "required_status_checks", "parameters": {
                "strict_required_status_checks_policy": false,
                "required_status_checks": checks
            } }
        ]
    })
}

fn merge_path_workflows(kind: RepoKind) -> &'static [&'static str] {
    match kind {
        RepoKind::Susi => &[
            "Test",
            "Auto PR + Merge",
            "Queue",
            "Main Failure Attribution",
            "Zero-Config Matrix",
            "Cloud target seed",
            "Nightly Coverage Audit",
            "Cleanup Workflow Runs",
        ],
        RepoKind::Commercial => &["Commercial Suite CI & Quality Gate", "Core Boundary Guard"],
    }
}

fn apply_ruleset(root: &Path, apply: bool) -> Result<()> {
    let body = ruleset_body(repo_kind(root)?);
    if !apply {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    let slug = repo_slug(root)?;
    let name = body["name"].as_str().ok_or("ruleset is missing a name")?;
    let listed = gh_output(root, &["api".into(), format!("repos/{slug}/rulesets")])?;
    let parsed = strip_ansi(listed.trim())
        .trim()
        .trim_start_matches('\u{feff}')
        .to_owned();
    let parsed = parsed.as_str();
    let listed: serde_json::Value = serde_json::from_str(parsed)
        .map_err(|error| format!("ruleset list was not json ({error}): {parsed:.180}"))?;
    let id = listed.as_array().and_then(|items| {
        items.iter().find_map(|item| {
            (item["name"] == name)
                .then(|| item["id"].as_u64())
                .flatten()
        })
    });
    let payload = serde_json::to_vec(&body)?;
    let endpoint = match id {
        Some(id) => format!("repos/{slug}/rulesets/{id}"),
        None => format!("repos/{slug}/rulesets"),
    };
    let method = if id.is_some() { "PUT" } else { "POST" };
    gh_input(
        root,
        &[
            "api".into(),
            "--method".into(),
            method.into(),
            endpoint,
            "--input".into(),
            "-".into(),
        ],
        &payload,
    )?;
    println!("ruleset {name} {method}");
    Ok(())
}

fn disable_merge_path(root: &Path, dry_run: bool) -> Result<()> {
    for name in merge_path_workflows(repo_kind(root)?) {
        if dry_run {
            println!("{name}");
            continue;
        }
        gh_output(root, &["workflow".into(), "disable".into(), (*name).into()])?;
        println!("disabled {name}");
    }
    Ok(())
}

fn gh_input(root: &Path, args: &[String], input: &[u8]) -> Result<String> {
    let mut child = gh_command(root)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or("missing gh stdin")?
        .write_all(input)?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        return Err(format!(
            "gh {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )
        .into());
    }
    Ok(strip_ansi(&String::from_utf8(out.stdout)?)
        .trim()
        .to_owned())
}

fn run(args: &[String]) -> Result<()> {
    let root = env::current_dir()?;
    match args.first().map(String::as_str) {
        Some("provenance-policy") if args.len() == 1 => {
            println!("{}", serde_json::json!({"schema":"susi-native-provenance-policy-v1",
                "policy_sha256":native_provenance::policy_digest(),"policy":native_provenance::POLICY}));
        },
        Some("verify-provenance") => {
            let mut flags = std::collections::BTreeMap::new();
            let mut rest = args[1..].iter();
            while let Some(flag) = rest.next() {
                if !["--root","--permit","--permit-sha256","--verifier-sha256","--policy-sha256","--head"].contains(&flag.as_str())
                    || flags.insert(flag.as_str(), rest.next().ok_or("missing provenance flag value")?.as_str()).is_some() {
                    return Err("unknown or duplicate provenance argument".into());
                }
            }
            let get = |key| flags.get(key).copied().ok_or("missing required provenance argument");
            let report = native_provenance::verify(Path::new(get("--root")?), Path::new(get("--permit")?),
                &native_provenance::TrustPins {permit_sha256:get("--permit-sha256")?,
                    verifier_sha256:get("--verifier-sha256")?,policy_sha256:get("--policy-sha256")?}, get("--head")?)?;
            println!("{}",serde_json::to_string(&report)?);
        },
        Some("start" | "extend" | "shrink") => {
            let claimant = args.get(1).ok_or("start requires an agent name")?;
            let mut scopes = Vec::new();
            let mut rest = args[2..].iter();
            while let Some(flag) = rest.next() {
                if flag != "--scope" { return Err(format!("unknown argument: {flag}").into()); }
                scopes.push(rest.next().ok_or("--scope requires a path")?.clone());
            }
            if args[0] == "extend" { update_scopes(&root, claimant, &scopes, ScopeChange::Extend)?; return Ok(()); }
            if args[0] == "shrink" { update_scopes(&root, claimant, &scopes, ScopeChange::Shrink)?; return Ok(()); }
            let worktree = start(&root, claimant, &scopes)?;
            println!("Worktree: {}\nTarget: {}\nUse the local-build entrypoint to override inherited target directories. Reservations cover cooperating launcher users only.", worktree.display(), worktree.join("target").display());
        }
        Some("release") if args.len() == 2 => release(&root, &args[1])?,
        Some("commit") if args.len() == 3 => commit(&root, &args[1], &args[2], None)?,
        Some("finish-merge") if args.len() == 4 => scoped_commit(&root, &args[1], &args[3], None, Some(&args[2]))?,
        Some("finish-merge") if args.iter().any(|arg| arg == "--sign") => return Err("finish-merge does not support signing".into()),
        Some("commit") if args.len() == 5 && args[3] == "--sign" => commit(&root, &args[1], &args[2], Some(&args[4]))?,
        Some("integrate") if args.len() >= 2 => { let names: Vec<_> = args[1..].iter().map(String::as_str).collect(); integrate_with(&root, &names, verify_candidate)?; },
        Some("list") if args.len() == 1 => {
            let state = state(&root)?;
            let _lock = Lock::acquire(&state)?;
            for claim in claims(&state)? { println!("{}\t{}\t{}", claim.name, claim.worktree.display(), claim.scopes.join(", ")); }
        }
        Some("ship") if args.get(1).map(String::as_str) == Some("--retry") && args.len() == 2 => {
            print_ship(&ship_retry(&root, &mut GhRemote)?);
        }
        Some("ship") if args.len() >= 2 => {
            let names: Vec<_> = args[1..].iter().map(String::as_str).collect();
            let mut remote = GhRemote;
            let report = match repo_kind(&root)? {
                RepoKind::Susi => ship(&root, &names, &mut remote, verify_candidate)?,
                RepoKind::Commercial => ship(&root, &names, &mut remote, commercial_gate)?,
            };
            print_ship(&report);
        }
        Some("ruleset") if args.get(1).map(String::as_str) == Some("apply") && args.len() <= 3 => {
            let apply = args.len() == 3 && args[2] == "--apply";
            if args.len() == 3 && !apply {
                return Err("usage: local-workflow ruleset apply [--apply]".into());
            }
            apply_ruleset(&root, apply)?;
        }
        Some("workflows")
            if args.len() == 3 && args[1] == "disable" && args[2] == "--merge-path" =>
        {
            disable_merge_path(&root, false)?;
        }
        Some("workflows")
            if args.len() == 4
                && args[1] == "disable"
                && args[2] == "--merge-path"
                && args[3] == "--dry-run" =>
        {
            disable_merge_path(&root, true)?;
        }
        _ => return Err("usage: local-workflow start <name> --scope <path> [--scope <path> ...] | extend <name> --scope <path> | shrink <name> --scope <path> | list | commit <name> <message> [--sign <fingerprint>] | finish-merge <name> <parent-oid> <message> | integrate <name> [<name> ...] | release <name> | ship <name> [<name> ...] | ship --retry | ruleset apply | workflows disable --merge-path".into()),
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
    fn pinned_merge_requires_a_resolved_scoped_index_and_preserves_both_parents() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        git(&repo, &["config", "user.name", "Test"]).unwrap();
        git(&repo, &["config", "user.email", "test@example.invalid"]).unwrap();
        let worker = start(&repo, "merge", &["docs".into()]).unwrap();
        fs::create_dir(worker.join("docs")).unwrap();
        fs::write(worker.join("docs/record"), "worker").unwrap();
        commit(&worker, "merge", "worker change", None).unwrap();
        let own = git(&worker, &["rev-parse", "HEAD"]).unwrap();
        fs::create_dir(repo.join("docs")).unwrap();
        fs::write(repo.join("docs/record"), "incoming").unwrap();
        git(&repo, &["add", "docs/record"]).unwrap();
        git(
            &repo,
            &["-c", "core.hooksPath=", "commit", "-qm", "incoming change"],
        )
        .unwrap();
        let incoming = git(&repo, &["rev-parse", "HEAD"]).unwrap();
        let refs = || {
            git(
                &repo,
                &["for-each-ref", "--format=%(refname) %(objectname)"],
            )
            .unwrap()
        };
        let before = refs();
        assert!(git(&worker, &["merge", "--no-ff", "--no-commit", &incoming]).is_err());
        let finish = |parent: &str| scoped_commit(&worker, "merge", "join", None, Some(parent));
        assert!(finish(&incoming).is_err()); // Unresolved add/add conflict.
        fs::write(worker.join("docs/record"), "worker and incoming").unwrap();
        git(&worker, &["add", "docs/record"]).unwrap();
        assert!(commit(&worker, "merge", "normal commit", None).is_err());
        assert!(finish(&own).is_err());
        assert!(scoped_commit(
            &worker,
            "merge",
            "join",
            Some(&"a".repeat(40)),
            Some(&incoming)
        )
        .is_err());

        fs::write(worker.join("outside"), "untracked").unwrap();
        assert!(finish(&incoming).is_err());
        git(&worker, &["add", "outside"]).unwrap();
        assert!(finish(&incoming).is_err());
        git(&worker, &["reset", "--", "outside"]).unwrap();
        fs::remove_file(worker.join("outside")).unwrap();
        fs::write(worker.join("docs/record"), "unstaged").unwrap();
        assert!(finish(&incoming).is_err());
        git(&worker, &["restore", "--worktree", "--", "docs/record"]).unwrap();
        let git_path = |name: &str| {
            PathBuf::from(
                git(
                    &worker,
                    &["rev-parse", "--path-format=absolute", "--git-path", name],
                )
                .unwrap(),
            )
        };
        let merge_head = git_path("MERGE_HEAD");
        fs::write(&merge_head, format!("{incoming}\n{own}\n")).unwrap();
        assert!(finish(&incoming).is_err());
        fs::write(&merge_head, format!("{incoming}\n")).unwrap();
        let sequencer = git_path("sequencer");
        fs::create_dir(&sequencer).unwrap();
        assert!(finish(&incoming).is_err());
        fs::remove_dir(sequencer).unwrap();
        assert_eq!(before, refs());

        let tree = git(&worker, &["write-tree"]).unwrap();
        finish(&incoming).unwrap();
        let merged = git(&worker, &["rev-parse", "HEAD"]).unwrap();
        assert_eq!(
            git(&worker, &["show", "-s", "--format=%T %P", "HEAD"]).unwrap(),
            format!("{tree} {own} {incoming}")
        );
        assert_eq!(
            refs(),
            before.replace(
                &format!("refs/heads/local-agent/merge {own}"),
                &format!("refs/heads/local-agent/merge {merged}")
            )
        );
        assert!(git(&worker, &["status", "--porcelain"]).unwrap().is_empty());
        assert!(!merge_head.exists());

        // Simulate interruption after the compare-and-swap, before Git-state cleanup.
        fs::write(&merge_head, format!("{incoming}\n")).unwrap();
        finish(&incoming).unwrap();
        assert_eq!(git(&worker, &["rev-parse", "HEAD"]).unwrap(), merged);
        assert!(!merge_head.exists());
    }

    #[test]
    fn extension_preserves_base_and_rejects_conflicts_without_mutation() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        let first = start(&repo, "first", &["src/first".into()]).unwrap();
        start(&repo, "second", &["src/second".into()]).unwrap();
        let claim_path = state(&repo).unwrap().join("claims/first.claim");
        let base = read_claim(&claim_path).unwrap().base;
        update_scopes(&first, "first", &["tests".into()], ScopeChange::Extend).unwrap();
        let claim = read_claim(&claim_path).unwrap();
        assert_eq!(claim.base, base);
        assert!(claim.scopes.contains(&"tests".into()));
        let before = fs::read(&claim_path).unwrap();
        assert!(update_scopes(&first, "first", &["src".into()], ScopeChange::Extend).is_err());
        assert!(update_scopes(&repo, "first", &["docs".into()], ScopeChange::Extend).is_err());
        fs::write(first.join("dirty"), "pending").unwrap();
        assert!(update_scopes(&first, "first", &["docs".into()], ScopeChange::Extend).is_err());
        assert_eq!(fs::read(claim_path).unwrap(), before);
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
    fn unused_scope_can_transfer_without_orphaning_committed_changes() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        let worktree = start(
            &repo,
            "builder",
            &["runtime.rs".into(), "Cargo.toml".into()],
        )
        .unwrap();
        fs::write(worktree.join("runtime.rs"), "changed").unwrap();
        commit(&worktree, "builder", "retain changed runtime", None).unwrap();
        let path = state(&repo).unwrap().join("claims/builder.claim");
        let original = fs::read(&path).unwrap();
        for scopes in [
            vec!["runtime.rs".into()],
            vec!["absent".into()],
            vec!["runtime.rs".into(), "Cargo.toml".into()],
        ] {
            assert!(update_scopes(&worktree, "builder", &scopes, ScopeChange::Shrink).is_err());
            assert_eq!(fs::read(&path).unwrap(), original);
        }
        fs::write(worktree.join("pending"), "keep dirty changes").unwrap();
        assert!(update_scopes(
            &worktree,
            "builder",
            &["Cargo.toml".into()],
            ScopeChange::Shrink
        )
        .is_err());
        assert_eq!(fs::read(&path).unwrap(), original);
        fs::remove_file(worktree.join("pending")).unwrap();
        update_scopes(
            &worktree,
            "builder",
            &["Cargo.toml".into()],
            ScopeChange::Shrink,
        )
        .unwrap();
        assert_eq!(read_claim(&path).unwrap().scopes, vec!["runtime.rs"]);
        start(&repo, "version", &["Cargo.toml".into()]).unwrap();
        assert_eq!(
            git(&worktree, &["show", "HEAD:runtime.rs"]).unwrap(),
            "changed"
        );
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
        assert!(commit(&repo, "one", "wrong owner", None).is_err());
        fs::write(worktree.join("outside.rs"), "outside").unwrap();
        assert!(commit(&worktree, "one", "reject", None).is_err());
        assert!(git(&worktree, &["diff", "--cached", "--name-only"])
            .unwrap()
            .is_empty());
        fs::remove_file(worktree.join("outside.rs")).unwrap();
        // A staged delta cancelled by an unstaged edit still must be inspected.
        fs::write(worktree.join(".gitignore"), "changed\n").unwrap();
        git(&worktree, &["add", ".gitignore"]).unwrap();
        fs::write(worktree.join(".gitignore"), "/target/\n").unwrap();
        assert!(commit(&worktree, "one", "reject staged outside", None).is_err());
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
        commit(&worktree, "one", "local change", None).unwrap();
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

    #[test]
    fn signing_failure_preserves_the_previous_head_and_unsigned_retry() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        let worktree = start(&repo, "signer", &["owned.rs".into()]).unwrap();
        let original = git(&worktree, &["rev-parse", "HEAD"]).unwrap();
        fs::write(worktree.join("owned.rs"), "scoped change").unwrap();
        git(
            &repo,
            &[
                "config",
                "gpg.program",
                text(&scratch.0.join("missing-signer")).unwrap(),
            ],
        )
        .unwrap();
        for key in ["ABC123", "0000000000000000000000000000000000000000"] {
            assert!(commit(&worktree, "signer", "sign", Some(key)).is_err());
            assert_eq!(git(&worktree, &["rev-parse", "HEAD"]).unwrap(), original);
        }
        commit(&worktree, "signer", "unsigned retry", None).unwrap();
        assert_eq!(
            git(&worktree, &["show", "HEAD:owned.rs"]).unwrap(),
            "scoped change"
        );
    }

    fn worker(repo: &Path, agent: &str, file: &str, content: &str) -> PathBuf {
        let worktree = start(repo, agent, &[file.into()]).unwrap();
        fs::write(worktree.join(file), content).unwrap();
        commit(&worktree, agent, "fixture change", None).unwrap();
        worktree
    }

    #[test]
    fn batch_retries_all_claims_and_publishes_only_the_verified_combination() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        let workers = ["left", "right"];
        for name in workers {
            worker(&repo, name, &format!("{name}.rs"), name);
        }
        let receipts = "refs/susi/integration/receipts/";
        assert!(integrate_with(&repo, &workers, |candidate, _| {
            assert!(candidate.join("left.rs").is_file());
            assert!(candidate.join("right.rs").is_file());
            Err("combined fixture rejected".into())
        })
        .is_err());
        assert!(integration_head(&repo).unwrap().is_none());
        assert!(git(&repo, &["for-each-ref", receipts]).unwrap().is_empty());
        assert_eq!(claims(&state(&repo).unwrap()).unwrap().len(), 2);
        let head = integrate_with(&repo, &workers, |candidate, _| {
            for name in workers {
                assert_eq!(
                    fs::read_to_string(candidate.join(format!("{name}.rs")))?,
                    name
                );
            }
            Ok(vec![
                "test-only combined fixture => exit code: 0; both files verified".into(),
            ])
        })
        .unwrap();
        let tree = git(&repo, &["rev-parse", &format!("{head}^{{tree}}")]).unwrap();
        for name in workers {
            let source = git(&repo, &["rev-parse", &format!("local-agent/{name}")]).unwrap();
            let reference = format!("{receipts}{name}/{source}");
            let receipt = git(&repo, &["cat-file", "blob", &reference]).unwrap();
            assert!(receipt_matches(&repo, &reference, &receipt, &head, &tree).unwrap());
        }
        assert!(claims(&state(&repo).unwrap()).unwrap().is_empty());
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
        let mut first_candidate = PathBuf::new();
        let first = integrate_with(&repo, &["left"], |candidate, _| {
            first_candidate = candidate.to_path_buf();
            let mut build_output = candidate.join("target");
            for _ in 0..6 {
                build_output.push("nested-windows-artifact-directory-with-a-long-name");
            }
            fs::create_dir_all(&build_output)?;
            fs::write(build_output.join("probe"), "build output")?;
            assert_eq!(fs::read_to_string(candidate.join("left.rs"))?, "left");
            assert!(!candidate.join("right.rs").exists());
            Ok(vec![
                "test-only content verification passed => exit code: 0; fixture verifier completed"
                    .into(),
            ])
        })
        .unwrap();
        assert!(
            !first_candidate.exists(),
            "accepted scratch tree was retained"
        );
        let second = integrate_with(&repo, &["right"], |candidate, _| {
            assert_eq!(fs::read_to_string(candidate.join("left.rs"))?, "left");
            assert_eq!(fs::read_to_string(candidate.join("right.rs"))?, "right");
            Ok(vec!["test-only combined content verification passed => exit code: 0; fixture verifier completed".into()])
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
    fn combined_integration_keeps_all_claims_on_failure_and_publishes_all_receipts() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        let left = worker(&repo, "left", "left.rs", "left");
        // A dependent worker starts from the first worker's private source.
        let right = worker(&left, "right", "right.rs", "right");
        let left_source = git(&left, &["rev-parse", "HEAD"]).unwrap();
        let right_source = git(&right, &["rev-parse", "HEAD"]).unwrap();
        assert!(integrate_many_with(&repo, &["left", "right"], |_, _| {
            Err("deliberate gate failure".into())
        })
        .is_err());
        assert!(integration_head(&repo).unwrap().is_none());
        assert_eq!(claims(&state(&repo).unwrap()).unwrap().len(), 2);
        assert!(integrate_many_with(&repo, &["left", "left"], |_, _| {
            panic!("duplicate sources reached verification")
        })
        .is_err());
        let head = integrate_many_with(&repo, &["left", "right"], |candidate, _| {
            assert_eq!(fs::read_to_string(candidate.join("left.rs"))?, "left");
            assert_eq!(fs::read_to_string(candidate.join("right.rs"))?, "right");
            Ok(vec!["fixture combined verification => exit code: 0".into()])
        })
        .unwrap();
        for (name, source) in [("left", left_source), ("right", right_source)] {
            let receipt = git(
                &repo,
                &[
                    "cat-file",
                    "blob",
                    &format!("refs/susi/integration/receipts/{name}/{source}"),
                ],
            )
            .unwrap();
            assert!(receipt.contains(&format!("commit={head}")));
            assert!(receipt.contains(&format!("source={source}")));
        }
        assert!(claims(&state(&repo).unwrap()).unwrap().is_empty());
    }

    #[test]
    fn combined_integration_cannot_mutually_admit_unaccepted_bases() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        worker(&repo, "anchor", "anchor.rs", "anchor");
        fs::write(repo.join("unowned.rs"), "not accepted").unwrap();
        git(&repo, &["add", "unowned.rs"]).unwrap();
        git(
            &repo,
            &["-c", "core.hooksPath=", "commit", "-qm", "unowned base"],
        )
        .unwrap();
        worker(&repo, "left", "left.rs", "left");
        worker(&repo, "right", "right.rs", "right");
        let accepted = integrate_with(&repo, &["anchor"], |_, _| {
            Ok(vec!["fixture verification => exit code: 0".into()])
        })
        .unwrap();
        assert!(integrate_many_with(&repo, &["left", "right"], |_, _| {
            panic!("unaccepted shared base reached verification")
        })
        .is_err());
        assert_eq!(integration_head(&repo).unwrap(), Some(accepted));
        assert_eq!(claims(&state(&repo).unwrap()).unwrap().len(), 2);
    }

    #[test]
    fn combined_integration_rechecks_every_source_and_scope() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        worker(&repo, "left", "left.rs", "left");
        let right = worker(&repo, "right", "right.rs", "right");
        assert!(integrate_many_with(&repo, &["left", "right"], |_, _| {
            fs::write(right.join("right.rs"), "advanced")?;
            commit(&right, "right", "advance during gate", None)?;
            Ok(vec!["fixture verification => exit code: 0".into()])
        })
        .is_err());
        assert!(integration_head(&repo).unwrap().is_none());
        fs::write(right.join("outside.rs"), "unowned").unwrap();
        git(&right, &["add", "outside.rs"]).unwrap();
        git(
            &right,
            &["-c", "core.hooksPath=", "commit", "-qm", "outside"],
        )
        .unwrap();
        assert!(integrate_many_with(&repo, &["left", "right"], |_, _| {
            panic!("unowned source reached verification")
        })
        .is_err());
        assert_eq!(claims(&state(&repo).unwrap()).unwrap().len(), 2);
    }

    #[test]
    fn accepted_imports_do_not_expand_the_workers_reserved_scope() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        let right = worker(&repo, "right", "right.rs", "right");
        worker(&repo, "left", "left.rs", "left");
        let accepted = integrate_with(&repo, &["left"], |_, _| {
            Ok(vec![
                "fixture verification => exit code: 0; fixture verifier completed".into(),
            ])
        })
        .unwrap();
        git(
            &right,
            &["-c", "core.hooksPath=", "merge", "--no-edit", &accepted],
        )
        .unwrap();
        let claim = read_claim(&state(&repo).unwrap().join("claims/right.claim")).unwrap();
        assert!(
            check_source(&repo, &claim).is_ok(),
            "previously accepted left.rs is not new worker work"
        );
        fs::write(right.join("outside.rs"), "new unauthorized work").unwrap();
        git(&right, &["add", "outside.rs"]).unwrap();
        git(
            &right,
            &["-c", "core.hooksPath=", "commit", "-qm", "outside"],
        )
        .unwrap();
        assert!(check_source(&repo, &claim)
            .unwrap_err()
            .to_string()
            .contains("outside reserved scope"));
        // Even an accepted import cannot authorize a later edit to its file.
        git(&right, &["revert", "--no-edit", "HEAD"]).unwrap();
        fs::write(right.join("left.rs"), "tampered accepted import").unwrap();
        git(&right, &["add", "left.rs"]).unwrap();
        git(
            &right,
            &["-c", "core.hooksPath=", "commit", "-qm", "tamper"],
        )
        .unwrap();
        assert!(check_source(&repo, &claim)
            .unwrap_err()
            .to_string()
            .contains("outside reserved scope"));
    }

    #[test]
    fn scope_exemption_requires_related_accepted_history_and_receipt() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        let right = worker(&repo, "right", "right.rs", "right");
        worker(&repo, "left", "left.rs", "left");
        let accepted = integrate_with(&repo, &["left"], |_, _| {
            Ok(vec![
                "fixture verification => exit code: 0; fixture verifier completed".into(),
            ])
        })
        .unwrap();
        git(
            &right,
            &["-c", "core.hooksPath=", "merge", "--no-edit", &accepted],
        )
        .unwrap();
        let claim = read_claim(&state(&repo).unwrap().join("claims/right.claim")).unwrap();
        let receipts = git(
            &repo,
            &[
                "for-each-ref",
                "--format=%(refname)",
                "refs/susi/integration/receipts/",
            ],
        )
        .unwrap();
        for receipt in receipts.lines() {
            let raw = git(&repo, &["cat-file", "blob", receipt]).unwrap();
            let tree = git(&repo, &["rev-parse", &format!("{accepted}^{{tree}}")]).unwrap();
            assert!(receipt_matches(&repo, receipt, &raw, &accepted, &tree).unwrap());
            for malformed in [
                format!("susi-local-integration-v1\ncommit={accepted}\ntree={tree}\n"),
                format!("{raw}\ncommit={accepted}\n"),
                raw.replace(
                    "fixture verification => exit code: 0; fixture verifier completed",
                    "hello",
                ),
                raw.replace("agent=left", "agent=other"),
                raw.replace(
                    "fixture verification",
                    "cargo test => exit code: 1; elapsed-ms=1",
                ),
            ] {
                assert!(!receipt_matches(&repo, receipt, &malformed, &accepted, &tree).unwrap());
            }
            git(&repo, &["update-ref", "-d", receipt]).unwrap();
        }
        assert!(check_source(&repo, &claim)
            .unwrap_err()
            .to_string()
            .contains("matching durable"));
        let tree = git(&repo, &["rev-parse", "HEAD^{tree}"]).unwrap();
        let unrelated = git(&repo, &["commit-tree", &tree, "-m", "unrelated history"]).unwrap();
        git(&repo, &["update-ref", INTEGRATION_REF, &unrelated]).unwrap();
        assert!(check_source(&repo, &claim)
            .unwrap_err()
            .to_string()
            .contains("does not descend"));
    }

    #[test]
    fn failed_gate_retains_source_claim_and_previous_integration() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        let worktree = worker(&repo, "one", "one.rs", "one");
        assert!(release(&repo, "one").is_err());
        let result = integrate_with(&repo, &["one"], |_, attempt| {
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
        assert!(integrate_with(&repo, &["one"], |candidate, _| {
            fs::write(candidate.join("one.rs"), "modified after verification")?;
            Ok(vec![
                "test-only check => exit code: 0; fixture verifier completed".into(),
            ])
        })
        .is_err());
        fs::write(worktree.join("outside.rs"), "escape").unwrap();
        git(&worktree, &["add", "outside.rs"]).unwrap();
        git(
            &worktree,
            &["-c", "core.hooksPath=", "commit", "-qm", "outside scope"],
        )
        .unwrap();
        assert!(integrate_with(&repo, &["one"], |_, _| panic!(
            "scope violation reached gate"
        ))
        .is_err());
        assert_eq!(integration_head(&repo).unwrap(), None);
    }

    #[test]
    fn integration_cas_refuses_external_advancement_without_publishing_receipt() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        let worktree = worker(&repo, "one", "one.rs", "one");
        let source = git(&worktree, &["rev-parse", "HEAD"]).unwrap();
        let external = git(&repo, &["rev-parse", "HEAD"]).unwrap();
        assert!(integrate_with(&repo, &["one"], |_, _| {
            git(&repo, &["update-ref", INTEGRATION_REF, &external])?;
            Ok(vec![
                "test-only check => exit code: 0; fixture verifier completed".into(),
            ])
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
        assert!(integrate_with(&repo, &["one"], |_, _| {
            fs::write(worktree.join("one.rs"), "new untested source")?;
            commit(&worktree, "one", "advanced during verification", None)?;
            Ok(vec![
                "test-only prior-head check => exit code: 0; fixture verifier completed".into(),
            ])
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
                ".agents/tasks/T-FIXTURE-1.json".into(),
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
        fs::create_dir_all(native_worker.join(".agents/tasks")).unwrap();
        fs::write(native_worker.join(".agents/tasks/T-FIXTURE-1.json"),
            serde_json::to_vec(&serde_json::json!({"id":"T-FIXTURE-1","native_worker":"native",
                "status":"in_progress","scopes":["Cargo.toml","Cargo.lock","deny.toml","src",".agents/tasks/T-FIXTURE-1.json"],
                "accept":{"cmd":["cargo","test","--offline","--locked"]}})).unwrap()).unwrap();
        commit(&native_worker, "native", "native gate fixture", None).unwrap();
        let second = start(
            &repo,
            "second",
            &["tests".into(), ".agents/tasks/T-SECOND-1.json".into()],
        )
        .unwrap();
        fs::create_dir_all(second.join("tests")).unwrap();
        fs::create_dir_all(second.join(".agents/tasks")).unwrap();
        fs::write(
            second.join("tests/second.rs"),
            "#[test]\nfn second_actual_test() {\n    assert_eq!(3 + 3, 6);\n}\n",
        )
        .unwrap();
        fs::write(second.join(".agents/tasks/T-SECOND-1.json"),serde_json::to_vec(&serde_json::json!({
            "id":"T-SECOND-1","native_worker":"second","status":"in_progress",
            "scopes":["tests",".agents/tasks/T-SECOND-1.json"],"accept":{"cmd":["cargo","test","--offline","--locked"]}
        })).unwrap()).unwrap();
        commit(&second, "second", "second native gate fixture", None).unwrap();
        let head = integrate_many_with(&repo, &["native", "second"], verify_candidate).unwrap();
        assert_eq!(integration_head(&repo).unwrap(), Some(head.clone()));
        verify_real_native_provenance(&repo, &scratch.0, &head);
        let broken = worker(&repo, "broken", "src/lib.rs", "#[test]\nfn failing_test() {\n    let actual = std::hint::black_box(4);\n    assert_eq!(actual, 5);\n}\n");
        extend(&broken, "broken", &[".agents/tasks/T-BROKEN-1.json".into()]).unwrap();
        fs::write(
            broken.join(".agents/tasks/T-BROKEN-1.json"),
            serde_json::to_vec(
                &serde_json::json!({"id":"T-BROKEN-1","native_worker":"broken",
                "status":"in_progress","scopes":["src/lib.rs",".agents/tasks/T-BROKEN-1.json"],
                "accept":{"cmd":["cargo","test","--offline","--locked"]}}),
            )
            .unwrap(),
        )
        .unwrap();
        commit(&broken, "broken", "bind failing fixture acceptance", None).unwrap();
        let failure = integrate_with(&repo, &["broken"], verify_candidate).unwrap_err();
        eprintln!("failing fixture gate: {failure}");
        assert_eq!(integration_head(&repo).unwrap(), Some(head));
        assert!(broken.exists());
        let attempts = state(&repo).unwrap().join("attempts");
        let failed_blob = fs::read_dir(attempts)
            .unwrap()
            .find_map(|entry| {
                let path = entry.unwrap().path();
                fs::read_to_string(path.join("gate-3.log"))
                    .is_ok_and(|log| log.contains("test result: FAILED"))
                    .then(|| {
                        fs::read_to_string(path.join("gate-3.log.oid"))
                            .unwrap()
                            .trim()
                            .to_owned()
                    })
            })
            .expect("the genuine failing gate retains a content-addressed log");
        git(&repo, &["gc", "--prune=now"]).unwrap();
        assert!(git(&repo, &["cat-file", "blob", &failed_blob])
            .unwrap()
            .contains("test result: FAILED"));
    }

    fn verify_real_native_provenance(repo: &Path, scratch: &Path, head: &str) {
        let base = git(repo, &["rev-parse", "HEAD"]).unwrap();
        let base_tree = git(repo, &["rev-parse", "HEAD^{tree}"]).unwrap();
        let accepted_tree = git(repo, &["rev-parse", &format!("{head}^{{tree}}")]).unwrap();
        let diagnostics =
            b"test fixture: historical ownership incomplete; forward acceptance only\n";
        let receipts: Vec<_> = git(
            repo,
            &[
                "for-each-ref",
                "--format=%(objectname)",
                "refs/susi/provenance/receipts/",
            ],
        )
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
        assert_eq!(receipts.len(), 2);
        let verifier = native_provenance::file_digest(&env::current_exe().unwrap()).unwrap();
        let policy = native_provenance::policy_digest();
        let permit = serde_json::json!({"schema":native_provenance::PERMIT_SCHEMA,
            "verifier_sha256":verifier,"policy_sha256":policy,
            "source":{"commit":head,"tree":accepted_tree},
            "baseline":{"commit":base,"tree":base_tree,"historical_ownership":"incomplete",
                "diagnostics_blob":native_provenance::store_blob(repo,diagnostics).unwrap(),
                "diagnostics_sha256":native_provenance::digest(diagnostics)},"receipts":receipts});
        let path = scratch.join("independent-permit.json");
        let check = |value: &serde_json::Value, expected_head: &str| {
            let bytes = serde_json::to_vec(value).unwrap();
            fs::write(&path, &bytes).unwrap();
            native_provenance::verify(
                repo,
                &path,
                &native_provenance::TrustPins {
                    permit_sha256: &native_provenance::digest(&bytes),
                    verifier_sha256: &verifier,
                    policy_sha256: &policy,
                },
                expected_head,
            )
        };
        let report = check(&permit, head).unwrap();
        assert_eq!(report["verified"], true);
        assert_eq!(report["historical_ownership"], "incomplete");
        let failed_bytes = b"failed-attempt evidence not present in any accepted receipt\n";
        let failed_blob = native_provenance::store_blob(repo, failed_bytes).unwrap();
        assert_eq!(
            native_provenance::store_blob(repo, failed_bytes).unwrap(),
            failed_blob
        );
        let failed_reference = format!("refs/susi/evidence/objects/{failed_blob}");
        let missing_target = "refs/susi/evidence/missing-fixture-target";
        git(repo, &["symbolic-ref", &failed_reference, missing_target]).unwrap();
        assert!(native_provenance::store_blob(repo, failed_bytes).is_err());
        assert_eq!(
            git(repo, &["symbolic-ref", "--quiet", &failed_reference]).unwrap(),
            missing_target
        );
        git(repo, &["symbolic-ref", "--delete", &failed_reference]).unwrap();
        assert_eq!(
            native_provenance::store_blob(repo, failed_bytes).unwrap(),
            failed_blob
        );
        git(repo, &["repack", "-ad"]).unwrap();
        git(repo, &["gc", "--prune=now"]).unwrap();
        assert_eq!(check(&permit, head).unwrap()["verified"], true);
        assert_eq!(
            git(repo, &["cat-file", "blob", &failed_blob]).unwrap(),
            String::from_utf8_lossy(failed_bytes).trim()
        );
        let anchored: serde_json::Value =
            serde_json::from_str(&git(repo, &["cat-file", "blob", &receipts[0]]).unwrap()).unwrap();
        let claim_oid = anchored["claim"]["blob"].as_str().unwrap();
        let claim_reference = format!("refs/susi/evidence/objects/{claim_oid}");
        let claim_bytes = git(repo, &["cat-file", "blob", claim_oid]).unwrap() + "\n";
        git(
            repo,
            &[
                "update-ref",
                "--no-deref",
                "-d",
                &claim_reference,
                claim_oid,
            ],
        )
        .unwrap();
        assert!(check(&permit, head).is_err());
        assert_eq!(
            native_provenance::store_blob(repo, claim_bytes.as_bytes()).unwrap(),
            claim_oid
        );
        git(
            repo,
            &[
                "update-ref",
                "--no-deref",
                &claim_reference,
                &failed_blob,
                claim_oid,
            ],
        )
        .unwrap();
        assert!(native_provenance::store_blob(repo, claim_bytes.as_bytes()).is_err());
        assert!(check(&permit, head).is_err());
        git(
            repo,
            &[
                "update-ref",
                "--no-deref",
                &claim_reference,
                claim_oid,
                &failed_blob,
            ],
        )
        .unwrap();
        assert_eq!(check(&permit, head).unwrap()["verified"], true);
        // Candidate replacement refs cannot reinterpret independently pinned IDs.
        git(repo, &["replace", head, &base]).unwrap();
        assert_eq!(
            check(&permit, head).unwrap()["source"]["tree"],
            accepted_tree
        );
        let mut replaced_tree = permit.clone();
        replaced_tree["source"]["tree"] = base_tree.clone().into();
        assert!(check(&replaced_tree, head).is_err());
        git(repo, &["replace", "-d", head]).unwrap();
        let graft = repo.join(git(repo, &["rev-parse", "--git-path", "info/grafts"]).unwrap());
        fs::create_dir_all(graft.parent().unwrap()).unwrap();
        fs::write(&graft, format!("{head} {base}\n")).unwrap();
        assert!(check(&permit, head).is_err());
        fs::remove_file(graft).unwrap();
        assert!(check(&permit, &base).is_err());
        let original: serde_json::Value =
            serde_json::from_str(&git(repo, &["cat-file", "blob", &receipts[0]]).unwrap()).unwrap();
        let reject_receipt = |changed: serde_json::Value| {
            let mut altered = permit.clone();
            altered["receipts"][0] =
                native_provenance::store_blob(repo, &serde_json::to_vec(&changed).unwrap())
                    .unwrap()
                    .into();
            assert!(check(&altered, head).is_err());
        };
        let mut changed = original.clone();
        changed["source"]["tree"] = base_tree.clone().into();
        reject_receipt(changed);
        let mut changed = original.clone();
        changed["claim"]["scopes"] = serde_json::json!(["outside"]);
        reject_receipt(changed);
        let mut changed = original.clone();
        changed["gates"][0]["exit_code"] = 1.into();
        reject_receipt(changed);
        let mut changed = original.clone();
        changed["gates"][3]["passed"] = 0.into();
        reject_receipt(changed);
        let mut changed = original.clone();
        changed["gates"][0]["log_sha256"] = "0".repeat(64).into();
        reject_receipt(changed);
        let mut changed = original.clone();
        changed["gates"][3]["argv"]
            .as_array_mut()
            .unwrap()
            .extend(["--exclude".into(), "susi-core".into()]);
        changed["gate_plan"][3]["argv"] = changed["gates"][3]["argv"].clone();
        reject_receipt(changed);
        let mut changed = original.clone();
        changed["gate_plan"].as_array_mut().unwrap().remove(1);
        changed["gates"].as_array_mut().unwrap().remove(1);
        reject_receipt(changed);
        let mut duplicate = permit.clone();
        duplicate["receipts"]
            .as_array_mut()
            .unwrap()
            .push(receipts[0].clone().into());
        assert!(check(&duplicate, head).is_err());
        // A same-tree merge can hide an unrelated second parent: ancestry, not
        // final net diff, must still reject its unowned source commit.
        let rogue = git_input(
            repo,
            &[
                "-c",
                "commit.gpgsign=false",
                "commit-tree",
                &base_tree,
                "-p",
                &base,
            ],
            b"unowned source\n",
        )
        .unwrap();
        let hidden = git_input(
            repo,
            &[
                "-c",
                "commit.gpgsign=false",
                "commit-tree",
                &accepted_tree,
                "-p",
                head,
                "-p",
                &rogue,
            ],
            b"same-tree uncovered second parent\n",
        )
        .unwrap();
        let mut hidden_permit = permit.clone();
        hidden_permit["source"]["commit"] = hidden.clone().into();
        for (index, oid) in receipts.iter().enumerate() {
            let mut record: serde_json::Value =
                serde_json::from_str(&git(repo, &["cat-file", "blob", oid]).unwrap()).unwrap();
            record["integration"]["commit"] = hidden.clone().into();
            hidden_permit["receipts"][index] =
                native_provenance::store_blob(repo, &serde_json::to_vec(&record).unwrap())
                    .unwrap()
                    .into();
        }
        assert!(check(&hidden_permit, &hidden).is_err());
        // A source merge may not claim unrelated pre-reservation history whose
        // out-of-scope edit was subsequently reverted to a clean net diff.
        let outside_blob =
            native_provenance::store_blob(repo, b"unowned historical edit\n").unwrap();
        let outside_tree = git_input(
            repo,
            &["mktree"],
            format!(
                "{}\n100644 blob {outside_blob}\toutside.txt\n",
                git(repo, &["ls-tree", &base]).unwrap()
            )
            .as_bytes(),
        )
        .unwrap();
        let outside_commit = git_input(
            repo,
            &["-c", "commit.gpgsign=false", "commit-tree", &outside_tree],
            b"unrelated scope bypass\n",
        )
        .unwrap();
        let reverted = git_input(
            repo,
            &[
                "-c",
                "commit.gpgsign=false",
                "commit-tree",
                &base_tree,
                "-p",
                &outside_commit,
            ],
            b"hide unrelated edit\n",
        )
        .unwrap();
        let original_source = original["source"]["commit"].as_str().unwrap();
        let original_tree = original["source"]["tree"].as_str().unwrap();
        let mixed_source = git_input(
            repo,
            &[
                "-c",
                "commit.gpgsign=false",
                "commit-tree",
                original_tree,
                "-p",
                original_source,
                "-p",
                &reverted,
            ],
            b"source with hidden pre-scope history\n",
        )
        .unwrap();
        let mixed_head = git_input(
            repo,
            &[
                "-c",
                "commit.gpgsign=false",
                "commit-tree",
                &accepted_tree,
                "-p",
                head,
                "-p",
                &mixed_source,
            ],
            b"integrated hidden pre-scope history\n",
        )
        .unwrap();
        let mut mixed_permit = permit.clone();
        mixed_permit["source"]["commit"] = mixed_head.clone().into();
        for (index, oid) in receipts.iter().enumerate() {
            let mut record: serde_json::Value =
                serde_json::from_str(&git(repo, &["cat-file", "blob", oid]).unwrap()).unwrap();
            record["integration"]["commit"] = mixed_head.clone().into();
            if index == 0 {
                record["source"]["commit"] = mixed_source.clone().into();
            }
            mixed_permit["receipts"][index] =
                native_provenance::store_blob(repo, &serde_json::to_vec(&record).unwrap())
                    .unwrap()
                    .into();
        }
        assert!(check(&mixed_permit, &mixed_head).is_err());
        let original_bytes = serde_json::to_vec(&permit).unwrap();
        let original_digest = native_provenance::digest(&original_bytes);
        fs::write(&path, b"forged permit").unwrap();
        assert!(native_provenance::verify(
            repo,
            &path,
            &native_provenance::TrustPins {
                permit_sha256: &original_digest,
                verifier_sha256: &verifier,
                policy_sha256: &policy
            },
            head
        )
        .is_err());
        let candidate_permit = repo.join("self-authorized.json");
        fs::write(&candidate_permit, &original_bytes).unwrap();
        assert!(native_provenance::verify(
            repo,
            &candidate_permit,
            &native_provenance::TrustPins {
                permit_sha256: &original_digest,
                verifier_sha256: &verifier,
                policy_sha256: &policy
            },
            head
        )
        .is_err());
        fs::remove_file(candidate_permit).unwrap();
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
        // This fixture models a previously accepted conflicting change. Supply
        // its matching receipt so the test reaches the actual merge conflict.
        let tree = git(&repo, &["rev-parse", "HEAD^{tree}"]).unwrap();
        let receipt = format!(
            "susi-local-integration-v1\nagent=fixture\nsource={head}\nclaim-base={head}\nprevious=none\ncommit={head}\ntree={tree}\ntest-only conflict fixture => exit code: 0; fixture verified\n"
        );
        let blob = git_input(&repo, &["hash-object", "-w", "--stdin"], receipt.as_bytes()).unwrap();
        git(
            &repo,
            &[
                "update-ref",
                &format!("refs/susi/integration/receipts/fixture/{head}"),
                &blob,
            ],
        )
        .unwrap();
        assert!(integrate_with(&repo, &["one"], |_, _| panic!(
            "conflict reached verification"
        ))
        .is_err());
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

    #[test]
    fn ship_contexts_are_the_four_local_gates() {
        assert_eq!(
            SHIP_CONTEXTS,
            [
                "susi-local/format",
                "susi-local/licenses",
                "susi-local/clippy",
                "susi-local/tests",
            ]
        );
    }

    #[test]
    fn status_description_truncates_to_github_limit() {
        let long = "x".repeat(200);
        assert_eq!(status_description(&long).chars().count(), 140);
    }

    struct FakeRemote {
        mains: Vec<String>,
        fetches: usize,
        pushes: Vec<(String, String)>,
        statuses: Vec<(String, String, String)>,
        merges: Vec<(u64, String)>,
        merge_sha: String,
        fail_push: bool,
    }

    impl FakeRemote {
        fn single(main: String) -> Self {
            Self {
                mains: vec![main],
                fetches: 0,
                pushes: Vec::new(),
                statuses: Vec::new(),
                merges: Vec::new(),
                merge_sha: "a".repeat(40),
                fail_push: false,
            }
        }
    }

    impl Remote for FakeRemote {
        fn fetch_main(&mut self, _root: &Path) -> Result<String> {
            let sha = self
                .mains
                .get(self.fetches)
                .or_else(|| self.mains.last())
                .cloned()
                .ok_or("no origin/main")?;
            self.fetches += 1;
            Ok(sha)
        }
        fn push_ref(&mut self, _root: &Path, sha: &str, git_ref: &str) -> Result<()> {
            require_ship_ref(git_ref)?;
            if self.fail_push {
                return Err("push refused".into());
            }
            self.pushes.push((sha.to_owned(), git_ref.to_owned()));
            Ok(())
        }
        fn open_pr(
            &mut self,
            _root: &Path,
            _branch: &str,
            _title: &str,
            _body: &str,
        ) -> Result<u64> {
            Ok(1)
        }
        fn post_status(
            &mut self,
            _root: &Path,
            sha: &str,
            context: &str,
            description: &str,
        ) -> Result<()> {
            self.statuses
                .push((sha.to_owned(), context.to_owned(), description.to_owned()));
            Ok(())
        }
        fn merge_pr(&mut self, _root: &Path, number: u64, sha: &str) -> Result<String> {
            self.merges.push((number, sha.to_owned()));
            Ok(self.merge_sha.clone())
        }
    }

    fn local_gate_evidence() -> Vec<String> {
        vec![
            "cargo fmt --all --check => exit code: 0; elapsed-ms=1".into(),
            "cargo deny --offline --locked check licenses => exit code: 0; elapsed-ms=1".into(),
            "cargo clippy --workspace --all-targets --offline --locked => exit code: 0; elapsed-ms=1".into(),
            "cargo test --workspace --offline --locked => exit code: 0; elapsed-ms=1".into(),
        ]
    }

    fn ship_fixture(agent: &str) -> (Scratch, PathBuf, String) {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        let base = git(&repo, &["rev-parse", "HEAD"]).unwrap();
        worker(&repo, agent, "owned.rs", "shipped");
        (scratch, repo, base)
    }

    #[test]
    fn ship_publishes_when_local_integration_ref_is_unrelated() {
        let (_scratch, repo, base) = ship_fixture("one");
        let tree = git(&repo, &["rev-parse", "HEAD^{tree}"]).unwrap();
        let unrelated = git(&repo, &["commit-tree", &tree, "-m", "unrelated"]).unwrap();
        git(&repo, &["update-ref", INTEGRATION_REF, &unrelated]).unwrap();
        let mut remote = FakeRemote::single(base);
        ship(&repo, &["one"], &mut remote, |_candidate, _attempt| {
            Ok(local_gate_evidence())
        })
        .unwrap();
        assert!(!remote.pushes.is_empty());
        assert_eq!(
            git(&repo, &["rev-parse", "refs/susi/integration/displaced"]).unwrap(),
            unrelated
        );
    }

    #[test]
    fn ship_does_not_push_when_the_gate_fails() {
        let (_scratch, repo, base) = ship_fixture("one");
        let mut remote = FakeRemote::single(base);
        let error = ship(&repo, &["one"], &mut remote, |_candidate, _attempt| {
            Err("gate failed".into())
        })
        .unwrap_err();
        assert!(error.to_string().contains("gate failed"), "{error}");
        assert!(remote.pushes.is_empty());
        assert!(remote.statuses.is_empty());
        assert!(remote.merges.is_empty());
    }

    #[test]
    fn ship_pushes_only_a_ship_branch() {
        let (_scratch, repo, base) = ship_fixture("one");
        let mut remote = FakeRemote::single(base);
        ship(&repo, &["one"], &mut remote, |_candidate, _attempt| {
            Ok(local_gate_evidence())
        })
        .unwrap();
        assert!(!remote.pushes.is_empty());
        for (_sha, git_ref) in &remote.pushes {
            assert!(git_ref.starts_with("refs/heads/ship/"), "{git_ref}");
            assert_ne!(git_ref, "refs/heads/main");
        }
    }

    #[test]
    fn ship_posts_success_only_for_passed_gates() {
        let (_scratch, repo, base) = ship_fixture("one");
        let mut remote = FakeRemote::single(base);
        let mut verified = String::new();
        let report = ship(&repo, &["one"], &mut remote, |candidate, _attempt| {
            verified = git(candidate, &["rev-parse", "HEAD"])?;
            Ok(local_gate_evidence())
        })
        .unwrap();
        assert_eq!(report.head, verified);
        assert_eq!(report.pr, 1);
        assert_eq!(report.merge.len(), 40);
        assert_eq!(remote.statuses.len(), 4);
        for (sha, context, _description) in &remote.statuses {
            assert_eq!(sha, &verified);
            assert!(SHIP_CONTEXTS.contains(&context.as_str()), "{context}");
        }
        let mut posted: Vec<_> = remote
            .statuses
            .iter()
            .map(|(_, context, _)| context.as_str())
            .collect();
        posted.sort();
        let mut expected: Vec<_> = SHIP_CONTEXTS.to_vec();
        expected.sort();
        assert_eq!(posted, expected);
    }

    fn commit_on(repo: &Path, file: &str, content: &str) -> String {
        fs::write(repo.join(file), content).unwrap();
        git(repo, &["add", "--", file]).unwrap();
        git(repo, &["-c", "core.hooksPath=", "commit", "-qm", file]).unwrap();
        git(repo, &["rev-parse", "HEAD"]).unwrap()
    }

    #[test]
    fn ship_reverifies_once_when_main_moves() {
        let (_scratch, repo, base) = ship_fixture("one");
        let moved = commit_on(&repo, "later.rs", "later");
        let mut remote = FakeRemote::single(base.clone());
        remote.mains = vec![base, moved.clone(), moved];
        let runs = std::cell::Cell::new(0);
        let report = ship(&repo, &["one"], &mut remote, |_candidate, _attempt| {
            runs.set(runs.get() + 1);
            Ok(local_gate_evidence())
        })
        .unwrap();
        assert_eq!(runs.get(), 2);
        assert_eq!(remote.merges.len(), 1);
        assert_eq!(remote.merges[0].1, report.head);
    }

    #[test]
    fn ship_stops_when_main_moves_twice() {
        let (_scratch, repo, base) = ship_fixture("one");
        let moved = commit_on(&repo, "later.rs", "later");
        let third = commit_on(&repo, "third.rs", "third");
        let mut remote = FakeRemote::single(base.clone());
        remote.mains = vec![base, moved, third];
        let error = ship(&repo, &["one"], &mut remote, |_candidate, _attempt| {
            Ok(local_gate_evidence())
        })
        .unwrap_err();
        assert!(
            error.to_string().contains("origin/main moved twice"),
            "{error}"
        );
        assert!(remote.merges.is_empty());
    }

    #[test]
    fn ship_retry_requires_a_matching_receipt() {
        let (_scratch, repo, base) = ship_fixture("one");
        let mut remote = FakeRemote::single(base.clone());
        let missing = ship_retry(&repo, &mut remote).unwrap_err();
        assert!(missing.to_string().contains("missing receipt"), "{missing}");
        assert!(remote.pushes.is_empty());
        remote.fail_push = true;
        let pushed = ship(&repo, &["one"], &mut remote, |_candidate, _attempt| {
            Ok(local_gate_evidence())
        })
        .unwrap_err();
        assert!(pushed.to_string().contains("push refused"), "{pushed}");
        remote.fail_push = false;
        remote.pushes.clear();
        let report = ship_retry(&repo, &mut remote).unwrap();
        assert_eq!(remote.pushes.len(), 1);
        assert_eq!(remote.pushes[0].0, report.head);
        assert!(remote.pushes[0].1.starts_with("refs/heads/ship/"));
    }

    #[test]
    fn susi_checkout_is_susi_kind() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask parent");
        assert_eq!(repo_kind(root).unwrap(), RepoKind::Susi);
    }

    #[test]
    fn commercial_package_is_commercial_kind() {
        let scratch = Scratch::new();
        fs::write(
            scratch.0.join("Cargo.toml"),
            "[package]\nname = \"susi-commercial\"\nversion = \"0.0.0\"\n",
        )
        .unwrap();
        assert_eq!(repo_kind(&scratch.0).unwrap(), RepoKind::Commercial);
    }

    #[test]
    fn unrecognized_checkout_is_refused() {
        let scratch = Scratch::new();
        let error = repo_kind(&scratch.0).unwrap_err();
        assert!(
            error.to_string().contains("unrecognized checkout"),
            "{error}"
        );
    }

    #[test]
    fn commercial_gate_plan_does_not_call_susi_proofs() {
        let plan = commercial_gate_argv();
        let flat = plan
            .iter()
            .flat_map(|args| args.iter().cloned())
            .collect::<Vec<_>>()
            .join(" ");
        for needle in ["fmt", "clippy", "test", "deny"] {
            assert!(flat.contains(needle), "{flat}");
        }
        assert!(!flat.contains("trusted_sysroot"), "{flat}");
        assert!(!flat.contains("http://"), "{flat}");
        assert!(!flat.contains("https://"), "{flat}");
    }

    #[test]
    fn susi_ruleset_has_no_bypass_and_only_ship_contexts() {
        let body = ruleset_body(RepoKind::Susi);
        assert_eq!(body["bypass_actors"], serde_json::json!([]));
        let types: Vec<_> = body["rules"]
            .as_array()
            .unwrap()
            .iter()
            .map(|rule| rule["type"].as_str().unwrap())
            .collect();
        assert!(types.contains(&"deletion"));
        assert!(types.contains(&"non_fast_forward"));
        assert!(types.contains(&"pull_request"));
        let contexts: Vec<_> = body["rules"]
            .as_array()
            .unwrap()
            .iter()
            .find(|rule| rule["type"] == "required_status_checks")
            .unwrap()["parameters"]["required_status_checks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|check| check["context"].as_str().unwrap())
            .collect();
        assert_eq!(contexts, SHIP_CONTEXTS);
    }

    #[test]
    fn commercial_ruleset_keeps_admin_bypass() {
        let body = ruleset_body(RepoKind::Commercial);
        assert_eq!(body["bypass_actors"][0]["actor_id"], 5);
        let contexts: Vec<_> = body["rules"]
            .as_array()
            .unwrap()
            .iter()
            .find(|rule| rule["type"] == "required_status_checks")
            .unwrap()["parameters"]["required_status_checks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|check| check["context"].as_str().unwrap())
            .collect();
        assert_eq!(contexts, SHIP_CONTEXTS);
    }

    #[test]
    fn susi_merge_path_workflows_are_the_gate_and_relay() {
        assert_eq!(
            merge_path_workflows(RepoKind::Susi),
            [
                "Test",
                "Auto PR + Merge",
                "Queue",
                "Main Failure Attribution",
                "Zero-Config Matrix",
                "Cloud target seed",
                "Nightly Coverage Audit",
                "Cleanup Workflow Runs",
            ]
        );
    }

    #[test]
    fn commercial_merge_path_workflows_are_the_per_change_gate() {
        let names = merge_path_workflows(RepoKind::Commercial);
        assert!(names.contains(&"Commercial Suite CI & Quality Gate"));
        assert!(names.iter().all(|name| !name.contains("Release")));
    }

    #[test]
    fn disable_does_not_include_tag_release() {
        for kind in [RepoKind::Susi, RepoKind::Commercial] {
            let names = merge_path_workflows(kind);
            assert!(!names.contains(&"Release Binaries"));
            assert!(!names.contains(&"Dev Release (main)"));
        }
    }

    #[test]
    fn usage_lists_ship_commands() {
        let error = run(&["nope".into()]).unwrap_err();
        let text = error.to_string();
        for needle in [
            "ship <name>",
            "ship --retry",
            "ruleset apply",
            "workflows disable --merge-path",
        ] {
            assert!(text.contains(needle), "{text}");
        }
    }
}
