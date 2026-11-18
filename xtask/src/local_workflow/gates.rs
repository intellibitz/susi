use super::*;
use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use susi_build_os_ucrt::policy as vendor_regressions;

pub(in crate::local_workflow) fn valid_oid(value: &str) -> bool {
    common::is_hex_oid(value)
}

/// The fetched remote main (`refs/remotes/origin/main`), if this clone has it.
pub(in crate::local_workflow) const FETCHED_MAIN_REF: &str = "refs/remotes/origin/main";

/// The commit a new claim starts from: the newest Susi landing. That is the
/// local integration ref (`ship`) or the fetched origin/main (`local-snapshot
/// submit`), whichever descends from the other; when they have diverged the
/// integration ref wins, as before. Without either, the primary HEAD.
pub(in crate::local_workflow) fn claim_base(root: &Path) -> Result<String> {
    let integrated = integration_head(root)?;
    let fetched = fetched_main(root)?;
    Ok(match (integrated, fetched) {
        (Some(i), Some(f)) if i != f && is_ancestor(root, &i, &f)? => f,
        (Some(i), _) => i,
        (None, Some(f)) => f,
        (None, None) => git(root, &["rev-parse", "HEAD"])?,
    })
}

pub(in crate::local_workflow) fn fetched_main(root: &Path) -> Result<Option<String>> {
    let fetched = Command::new("git")
        .arg("--literal-pathspecs")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .current_dir(root)
        .args(["rev-parse", "--verify", "--quiet", FETCHED_MAIN_REF])
        .output()?;
    if !fetched.status.success() {
        return Ok(None);
    }
    Ok(Some(
        String::from_utf8_lossy(&fetched.stdout).trim().to_owned(),
    ))
}

/// Whether `head` has landed: `ship` records it in the local integration ref,
/// and `local-snapshot submit` (the development-main publisher) verifies it on
/// the remote main, which a fetch mirrors into `origin/main`. Either is
/// Susi's own landing, so either releases the reservation.
pub(in crate::local_workflow) fn landed(root: &Path, head: &str) -> Result<bool> {
    if let Some(accepted) = integration_head(root)? {
        if is_ancestor(root, head, &accepted)? {
            return Ok(true);
        }
    }
    match fetched_main(root)? {
        Some(main) => is_ancestor(root, head, &main),
        None => Ok(false),
    }
}

pub(in crate::local_workflow) fn integration_head(root: &Path) -> Result<Option<String>> {
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

pub(in crate::local_workflow) fn is_ancestor(
    root: &Path,
    ancestor: &str,
    descendant: &str,
) -> Result<bool> {
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

pub(in crate::local_workflow) fn git_input(
    root: &Path,
    args: &[&str],
    input: &[u8],
) -> Result<String> {
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

pub(in crate::local_workflow) fn receipt_matches(
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
    if fields[3] != "none" {
        if !valid_oid(fields[3]) {
            return Ok(false);
        }
        if is_ancestor(root, fields[3], accepted)? {
            // A local integration ref left behind origin/main (main advanced
            // through ships whose receipts this clone does not hold) is in the
            // accepted history but can be older than the claim base. Receipts
            // written before `record_acceptance` recorded the publish base in
            // that case named the stale ref; accept them when the claim base
            // lies on the published lineage, the rule used for a displaced ref.
            if !is_ancestor(root, fields[2], fields[3])? {
                let Ok(publish_base) = git(root, &["rev-parse", &format!("{accepted}^1")]) else {
                    return Ok(false);
                };
                if !is_ancestor(root, fields[3], &publish_base)?
                    || !is_ancestor(root, fields[2], &publish_base)?
                {
                    return Ok(false);
                }
            }
        } else {
            // `ship` merges onto origin/main, so an integration ref that is not
            // in the accepted history was displaced, never built upon. Receipts
            // written before `record_acceptance` recorded the publish base in
            // that case named the displaced ref here; accept them only when the
            // claim base lies on the published lineage (the accepted merge's
            // first parent). This is no weaker than `previous=none`.
            let Ok(publish_base) = git(root, &["rev-parse", &format!("{accepted}^1")]) else {
                return Ok(false);
            };
            if !is_ancestor(root, fields[2], &publish_base)? {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

pub(in crate::local_workflow) fn matching_receipt(
    root: &Path,
    accepted: &str,
    tree: &str,
) -> Result<Option<String>> {
    let listing = git(
        root,
        &[
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            "refs/susi/integration/receipts/",
        ],
    )?;
    let mut entries = Vec::new();
    for entry in listing.lines() {
        let (reference, oid) = entry
            .split_once(' ')
            .ok_or("malformed receipt reference listing")?;
        if !valid_oid(oid) {
            return Err("invalid receipt blob oid".into());
        }
        entries.push((reference, oid));
    }
    if entries.is_empty() {
        return Ok(None);
    }
    let mut child = Command::new("git")
        .arg("--literal-pathspecs")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .current_dir(root)
        .args(["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().ok_or("git cat-file stdin")?;
    let request: String = entries.iter().map(|(_, oid)| format!("{oid}\n")).collect();
    let writer = std::thread::spawn(move || stdin.write_all(request.as_bytes()));
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
    for (reference, oid) in entries {
        let end = data[at..]
            .iter()
            .position(|byte| *byte == b'\n')
            .ok_or("truncated git cat-file reply")?
            + at;
        let header = std::str::from_utf8(&data[at..end])?;
        let mut fields = header.split(' ');
        if fields.next() != Some(oid) || fields.next() != Some("blob") {
            return Err(format!("git cat-file did not return blob {oid}: {header}").into());
        }
        let size: usize = fields
            .next()
            .ok_or("git cat-file reply without size")?
            .parse()?;
        let body = end + 1;
        if data.len() < body + size + 1 || data[body + size] != b'\n' {
            return Err("truncated git cat-file blob".into());
        }
        let receipt = String::from_utf8_lossy(&data[body..body + size]);
        at = body + size + 1;
        if receipt_matches(root, reference, &receipt, accepted, tree)? {
            return Ok(Some(receipt.into_owned()));
        }
    }
    Ok(None)
}

pub(in crate::local_workflow) fn scope_base(
    root: &Path,
    claim: &Claim,
    source: &str,
) -> Result<String> {
    let Some(accepted) = integration_head(root)? else {
        return Ok(claim.base.clone());
    };
    // A claim reserved at or after the accepted integration exempts no
    // integration history, so no receipt is needed to justify the base.
    if accepted == claim.base || is_ancestor(root, &accepted, &claim.base)? {
        return Ok(claim.base.clone());
    }
    if !is_ancestor(root, &claim.base, &accepted)? {
        return Err("accepted history does not descend from the reserved claim base".into());
    }
    let tree = git(root, &["rev-parse", &format!("{accepted}^{{tree}}")])?;
    if matching_receipt(root, &accepted, &tree)?.is_none() {
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

pub(in crate::local_workflow) fn check_source(root: &Path, claim: &Claim) -> Result<String> {
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

pub(in crate::local_workflow) fn sign_workspace_pass(root: &Path, key: &str) -> Result<String> {
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
/// The environment every gate command and every test binary of a gate gets:
/// a private home tree, the operator's real cargo and rustup homes, the
/// gate's target directory, no wrappers, and no credentials or endpoints
/// from the operator's session.
#[derive(Clone, Copy)]
pub(in crate::local_workflow) struct GateEnv<'a> {
    pub(in crate::local_workflow) home: &'a Path,
    pub(in crate::local_workflow) cargo_home: &'a Path,
    pub(in crate::local_workflow) rustup_home: &'a Path,
    pub(in crate::local_workflow) target: &'a Path,
    pub(in crate::local_workflow) sysroot: Option<&'a trusted_sysroot::VerifiedSysroot>,
}

pub(in crate::local_workflow) fn apply_gate_env(
    command: &mut Command,
    gate: &GateEnv<'_>,
) -> Result<()> {
    compiler_env::apply_checked(command)?;
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
    if let Some(sysroot) = gate.sysroot {
        sysroot.apply(command)?;
    }
    let home = gate.home;
    command
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("SUSI_HOME", home.join("susi"))
        .env("SUSI_NO_REGISTRY_ENV", "1")
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env("APPDATA", home.join("appdata"))
        .env("LOCALAPPDATA", home.join("localappdata"))
        .env("CARGO_HOME", gate.cargo_home)
        .env("RUSTUP_HOME", gate.rustup_home)
        .env("CARGO_TARGET_DIR", gate.target)
        .env("CARGO_BUILD_TARGET_DIR", gate.target)
        .env("CARGO_BUILD_BUILD_DIR", gate.target)
        // Evidence logs are parsed; hosted CI exports CARGO_TERM_COLOR=always.
        .env("CARGO_TERM_COLOR", "never")
        // Host compiler wrappers may consult non-hermetic user-profile
        // configuration or run a shared daemon. The local gate is self-contained.
        .env("RUSTC_WRAPPER", "")
        .env("RUSTC_WORKSPACE_WRAPPER", "");
    Ok(())
}

/// An exit status with `code` for a gate whose outcome this process decided
/// (the runner's binaries each have their own status in the log).
pub(in crate::local_workflow) fn exit_status(code: i32) -> std::process::ExitStatus {
    #[cfg(windows)]
    {
        use std::os::windows::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(u32::from_ne_bytes(code.to_ne_bytes()))
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(code << 8)
    }
}

/// A narrowed test gate run as Cargo's build plus the concurrent binary
/// runner (`test_runner`).
pub(in crate::local_workflow) struct TestBinariesGate<'a> {
    pub(in crate::local_workflow) candidate: &'a Path,
    pub(in crate::local_workflow) attempt: &'a Path,
    /// The planned `cargo test ...` arguments (without `cargo`).
    pub(in crate::local_workflow) args: &'a [String],
    pub(in crate::local_workflow) host: &'a str,
    pub(in crate::local_workflow) target: &'a Path,
    pub(in crate::local_workflow) log_path: &'a Path,
    pub(in crate::local_workflow) share: GateShare,
    pub(in crate::local_workflow) env: GateEnv<'a>,
}

pub(in crate::local_workflow) fn complete_workspace_requested() -> bool {
    // Kept explicit during migration. Unsupported hosts preserve serial
    // coverage; publication requires typed replay rather than a v1 proof.
    cfg!(windows) && env::var("SUSI_LOCAL_TEST_RUNNER").is_ok_and(|v| v == "workspace-binaries")
}

pub(in crate::local_workflow) fn run_complete_binaries(
    gate: &TestBinariesGate<'_>,
) -> Result<(test_binary_evidence::Execution, serde_json::Value)> {
    let argv: Vec<String> = std::iter::once("cargo".into())
        .chain(gate.args.iter().cloned())
        .collect();
    let mut build = Command::new("cargo");
    apply_gate_env(&mut build, &gate.env)?;
    build.current_dir(gate.candidate).args(gate.args);
    apply_gate_share(&mut build, gate.share);
    let lanes = test_runner::concurrency(gate.share.jobs);
    let homes = gate.attempt.join("complete-test-homes");
    let durations = gate.attempt.join("complete-test-durations.json");
    let heavy = test_runner::heavy_binaries(gate.candidate);
    let settings = test_runner::Settings {
        concurrency: lanes,
        total_test_threads: gate.share.test_threads,
        build_jobs: test_runner::threads_per_binary(gate.share.jobs, lanes),
        home_root: &homes,
        target_dir: gate.target,
        triple: gate
            .args
            .windows(2)
            .find_map(|p| (p[0] == "--target").then_some(p[1].as_str())),
        durations: &durations,
        heavy: &heavy,
        log: gate.log_path,
    };
    let launch = |binary: &test_runner::Binary, home: &Path| {
        let mut command = Command::new(&binary.executable);
        let nested = home.join("target");
        apply_gate_env(
            &mut command,
            &GateEnv {
                home,
                target: &nested,
                ..gate.env
            },
        )?;
        Ok(command)
    };
    let retained = std::cell::RefCell::new(None);
    let execution = test_binary_evidence::execute(
        test_binary_evidence::ExecutionSpec {
            root: gate.candidate,
            argv: &argv,
            settings: &settings,
            callback_source: include_bytes!("gates.rs"),
        },
        &mut build,
        &launch,
        &|plan, raw, metadata| {
            *retained.borrow_mut() = Some(native_provenance::retain_binary_plan(
                gate.candidate,
                plan,
                raw,
                metadata,
            )?);
            Ok(())
        },
    )?;
    Ok((
        execution,
        retained
            .into_inner()
            .ok_or("binary plan was not retained before execution")?,
    ))
}

/// Build the gate's test binaries with `cargo test ... --no-run`, then run
/// them through the concurrent runner, each in its own home under the
/// attempt directory. Returns the gate's status (0 only when the build and
/// every binary succeeded) and a note for the evidence line.
pub(in crate::local_workflow) fn run_test_binaries(
    gate: &TestBinariesGate<'_>,
) -> Result<(std::process::ExitStatus, Option<String>)> {
    let mut log = fs::OpenOptions::new().append(true).open(gate.log_path)?;
    let mut build = Command::new("cargo");
    apply_gate_env(&mut build, &gate.env)?;
    build
        .current_dir(gate.candidate)
        .args(gate.args)
        .args(["--no-run", "--message-format=json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::from(log.try_clone()?));
    apply_gate_share(&mut build, gate.share);
    let started = std::time::Instant::now();
    let output = build.output()?;
    record_phase("test build", started.elapsed().as_millis());
    let (binaries, diagnostics) =
        test_runner::parse_artifacts(&String::from_utf8_lossy(&output.stdout))?;
    for rendered in &diagnostics {
        log.write_all(rendered.as_bytes())?;
    }
    writeln!(
        log,
        "test build: cargo {} --no-run => exit code: {}; {} test binaries",
        gate.args.join(" "),
        output
            .status
            .code()
            .map_or_else(|| "none".to_owned(), |code| code.to_string()),
        binaries.len()
    )?;
    if !output.status.success() {
        return Ok((output.status, Some("runner=binaries:build failed".into())));
    }
    let lanes = test_runner::concurrency(gate.share.jobs);
    let heavy = test_runner::heavy_binaries(gate.candidate);
    let home_root = gate.attempt.join("test-homes");
    let durations = gate
        .attempt
        .parent()
        .unwrap_or(gate.attempt)
        .join("test-durations.json");
    let settings = test_runner::Settings {
        concurrency: lanes,
        total_test_threads: gate.share.test_threads,
        build_jobs: test_runner::threads_per_binary(gate.share.jobs, lanes),
        home_root: &home_root,
        target_dir: gate.target,
        triple: Some(gate.host),
        durations: &durations,
        heavy: &heavy,
        log: gate.log_path,
    };
    eprintln!(
        "test runner: {} binaries on up to {} lanes, {} aggregate test-thread budget",
        binaries.len(),
        lanes,
        settings.total_test_threads
    );
    let env = gate.env;
    let launch = move |binary: &test_runner::Binary, home: &Path| -> Result<Command> {
        let mut command = Command::new(&binary.executable);
        apply_gate_env(&mut command, &GateEnv { home, ..env })?;
        Ok(command)
    };
    let outcome = test_runner::run(&binaries, &settings, &launch)?;
    record_phase("test binaries", outcome.elapsed_ms);
    let _ = fs::remove_dir_all(&home_root);
    for (id, code) in &outcome.failed {
        eprintln!(
            "test runner: {id} failed ({})",
            code.map_or_else(
                || "no exit code".to_owned(),
                |code| format!("exit code {code}")
            )
        );
    }
    let complete = outcome.failed.is_empty() && outcome.ran == binaries.len();
    Ok((
        exit_status(i32::from(!complete)),
        Some(format!(
            "runner=binaries:{lanes} lanes, {} of {} ran, {} failed",
            outcome.ran,
            binaries.len(),
            outcome.failed.len()
        )),
    ))
}

pub(in crate::local_workflow) const GIB: u64 = 1 << 30;

pub(in crate::local_workflow) fn gib(bytes: u64) -> String {
    format!("{:.1} GiB", bytes as f64 / GIB as f64)
}

/// Free bytes a gate must find on the filesystem of its target before it
/// starts, by the workspace it gates. Measured 2026-11-07 on Linux x86_64: a
/// whole-workspace clippy and test gate of Susi core filled a cold slot target
/// to 13 GB, the same gate on a warm target added about 2 GB, a gate narrowed
/// to a few packages stayed under 2 GB, and the commercial workspace's whole
/// gate filled a cold target to 2.5 GB. A workspace this tool does not know
/// (a test fixture) needs only room to compile at all. A gate that runs out
/// of disk fails only after its compile time, with an error that reads like
/// a compiler bug, so a doomed gate is refused first.
pub(in crate::local_workflow) fn gate_disk_floor(
    kind: Option<RepoKind>,
    whole_workspace: bool,
    warm: bool,
) -> u64 {
    match (kind, whole_workspace, warm) {
        (Some(RepoKind::Susi), true, false) => 12 * GIB,
        (Some(RepoKind::Susi), true, true) => 4 * GIB,
        (Some(RepoKind::Susi), false, _) => 2 * GIB,
        (Some(RepoKind::Commercial), true, false) => 3 * GIB,
        (Some(RepoKind::Commercial), _, _) => GIB,
        (None, _, _) => GIB / 2,
    }
}

/// Free bytes on the filesystem that holds `path`, or its nearest existing
/// ancestor when it does not exist yet. `None` when no mounted filesystem
/// claims the path: the check is then skipped, never a refusal.
pub(in crate::local_workflow) fn free_space(path: &Path) -> Option<u64> {
    let mut probe = path;
    let canonical = loop {
        if let Ok(canonical) = probe.canonicalize() {
            break canonical;
        }
        probe = probe.parent()?;
    };
    // On Windows `canonicalize` answers in verbatim form (`\\?\C:\...`),
    // which a mount point (`C:\`) is never a prefix of: compare the DOS form.
    let canonical = dos_path(&canonical);
    susi_config::disk_space::available(&canonical).map(|(available, _)| available)
}

/// `path` without Windows' verbatim prefix: `\\?\C:\x` becomes `C:\x` and
/// `\\?\UNC\host\share\x` becomes `\\host\share\x`; any other path is
/// returned as it is.
pub(in crate::local_workflow) fn dos_path(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = text.strip_prefix(r"\\?\") {
        return PathBuf::from(rest);
    }
    path.to_path_buf()
}

/// Whether a gate log says the disk ran out: ENOSPC on Linux and macOS, the
/// Windows ERROR_DISK_FULL text and code, and LLVM's output-stream failure,
/// which is how rustc reports a full disk while writing an object file.
pub(in crate::local_workflow) fn gate_out_of_space(log: &[u8]) -> bool {
    let text = String::from_utf8_lossy(log);
    [
        "No space left on device",
        "os error 28)",
        "There is not enough space on the disk",
        "os error 112)",
        "IO failure on output stream",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

pub(in crate::local_workflow) fn dir_size(path: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match entry.metadata() {
            Ok(meta) if meta.is_dir() => dir_size(&entry.path()),
            Ok(meta) if meta.is_file() => meta.len(),
            _ => 0,
        })
        .sum()
}

/// Remove the caches the ship machinery owns and rebuilds on demand: the warm
/// targets stashed for idle slots, those slots' leftover candidate targets, and
/// the legacy integration target. A slot another ship holds locked is busy and
/// is left alone, as is this candidate's own target. Returns what was removed
/// with its size.
pub(in crate::local_workflow) fn reclaim_gate_caches(
    workflow: &Path,
    candidate: &Path,
) -> Vec<(PathBuf, u64)> {
    let mut freed = Vec::new();
    let mut remove = |path: PathBuf| {
        if path.is_dir() {
            let size = dir_size(&path);
            if fs::remove_dir_all(&path).is_ok() {
                freed.push((path, size));
            }
        }
    };
    remove(workflow.join("integration-target"));
    let name = candidate
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let (stem, current) = name.rsplit_once("-verify-candidate-").unwrap_or((name, ""));
    let slots = workflow.join("slots");
    let Ok(entries) = fs::read_dir(&slots) else {
        return freed;
    };
    for entry in entries.flatten() {
        let slot = entry.file_name();
        let Some(slot) = slot.to_str() else {
            continue;
        };
        if slot.is_empty() || !slot.bytes().all(|byte| byte.is_ascii_digit()) || slot == current {
            continue;
        }
        // The slot's lock, taken without waiting: a busy slot is another ship's.
        let Ok(lock) = fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(slots.join(format!("{slot}.lock")))
        else {
            continue;
        };
        if lock.try_lock().is_err() {
            continue;
        }
        remove(entry.path().join("target"));
        if let Some(parent) = candidate.parent() {
            remove(
                parent
                    .join(format!("{stem}-verify-candidate-{slot}"))
                    .join("target"),
            );
        }
        release_probe(lock);
    }
    freed
}

/// Refuse a gate whose filesystem is too full to finish, after reclaiming the
/// ship's own idle caches. A gate that cannot be measured is not refused.
pub(in crate::local_workflow) fn require_gate_disk(
    candidate: &Path,
    target: &Path,
    whole_workspace: bool,
) -> Result<()> {
    let warm = target.join("CACHEDIR.TAG").is_file();
    let floor = gate_disk_floor(repo_kind(candidate).ok(), whole_workspace, warm);
    let Some(free) = free_space(candidate) else {
        return Ok(());
    };
    if free >= floor {
        return Ok(());
    }
    let workflow = state(candidate)?;
    for (path, size) in reclaim_gate_caches(&workflow, candidate) {
        eprintln!("gate: reclaimed {} ({})", path.display(), gib(size));
    }
    let Some(free) = free_space(candidate) else {
        return Ok(());
    };
    if free >= floor {
        return Ok(());
    }
    Err(format!(
        "gate refused before compiling: {} free on the filesystem holding {}, and a {} gate on a {} target needs about {}; free space (worktree target/ directories, old candidates) and run ship again",
        gib(free),
        candidate.display(),
        if whole_workspace { "whole-workspace" } else { "narrowed" },
        if warm { "warm" } else { "cold" },
        gib(floor)
    )
    .into())
}

/// A gate that ran out of disk runs once more after the ship's idle caches
/// are reclaimed: the first attempt's log is kept beside the rerun's, and the
/// command is pointed at a fresh log. `None` when nothing could be reclaimed,
/// so the gate fails as it did.
pub(in crate::local_workflow) fn reclaim_for_rerun(
    candidate: &Path,
    log_path: &Path,
    command: &mut Command,
    gate: &str,
) -> Result<Option<String>> {
    let workflow = state(candidate)?;
    let freed = reclaim_gate_caches(&workflow, candidate);
    if freed.is_empty() {
        return Ok(None);
    }
    let total: u64 = freed.iter().map(|(_, size)| *size).sum();
    for (path, size) in &freed {
        eprintln!("gate {gate}: reclaimed {} ({})", path.display(), gib(*size));
    }
    eprintln!(
        "gate {gate}: out of disk space; {} reclaimed, running it again",
        gib(total)
    );
    let _ = fs::rename(log_path, log_path.with_extension("out-of-space.log"));
    let log = fs::File::create(log_path)?;
    command
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log));
    Ok(Some(format!(
        "reclaim gate={gate} freed-bytes={total} caches={}",
        freed.len()
    )))
}

/// The disk-full prefix of a failed gate's error, or nothing.
pub(in crate::local_workflow) fn out_of_space_prefix(candidate: &Path, log: &[u8]) -> String {
    if !gate_out_of_space(log) {
        return String::new();
    }
    format!(
        "out of disk space ({} free on the filesystem holding {}); ",
        free_space(candidate).map_or_else(|| "unknown".to_owned(), gib),
        candidate.display()
    )
}

pub(in crate::local_workflow) fn verify_candidate(
    candidate: &Path,
    attempt: &Path,
) -> Result<Vec<String>> {
    // No queue: this gate runs now, beside any other, and each of its cargo
    // commands takes an even share of the host (`gate_share`).
    let _live = live_gate();
    timed("provenance check", || {
        native_provenance::require_unmodified_history(candidate)
    })
    .map_err(|error| format!("provenance: {error}"))?;
    let compiler = Command::new("rustc")
        .current_dir(candidate)
        .arg("-vV")
        .output()
        .map_err(|error| format!("rustc in {}: {error}", candidate.display()))?;
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
    let _ = fs::remove_dir_all(&home);
    fs::create_dir_all(home.join("AppData/Local"))?;
    let target = timed("restore warm target", || {
        restore_candidate_target(candidate)
    });
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
            "nextest",
            "run",
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
    let nextest = nextest_requested();
    select_test_runner(&mut gates[3], nextest);
    let (selection, libraries) =
        timed("affected selection", || gate_selection(candidate, attempt))?;
    match &selection {
        affected::Selection::Packages(packages) => {
            for args in &mut gates {
                if matches!(
                    args.first().map(String::as_str),
                    Some("clippy" | "test" | "nextest")
                ) {
                    affected::narrow(args, packages);
                }
            }
            include_release_tooling_for_xtask(&mut gates, packages);
            eprintln!(
                "local gate: {} of the workspace packages ({})",
                packages.len(),
                packages.iter().cloned().collect::<Vec<_>>().join(", ")
            );
        }
        affected::Selection::Workspace(reason) => {
            eprintln!("local gate: whole workspace ({reason})");
        }
    }
    vendor_regressions::include_release_tooling(candidate, &mut gates);
    timed("gate disk check", || {
        require_gate_disk(
            candidate,
            &target,
            matches!(selection, affected::Selection::Workspace(_)),
        )
    })?;
    if matches!(selection, affected::Selection::Workspace(_))
        && candidate.join("crates/susi-core/Cargo.toml").is_file()
    {
        // `include_release_tooling` recognizes clippy and test; the nextest
        // workspace gate needs the same feature.
        let all: BTreeSet<String> = ["xtask".to_owned()].into();
        include_release_tooling_for_xtask(&mut gates, &all);
    }
    // A narrowed `cargo test` gate runs its binaries through the concurrent
    // runner, which (like nextest) leaves doctests to a companion gate. The
    // whole-workspace gate keeps serial `cargo test`: its signed test proof
    // is keyed on that exact invocation.
    let complete_workspace = !nextest
        && complete_workspace_requested()
        && matches!(selection, affected::Selection::Workspace(_));
    if complete_workspace {
        test_binary_evidence::require_supported_source_inputs()?;
    }
    let parallel_tests = !nextest
        && !test_runner::serial_requested()
        && (matches!(selection, affected::Selection::Packages(_)) || complete_workspace);
    let doc = if complete_workspace {
        let logical: Vec<String> = std::iter::once("cargo".into())
            .chain(gates[3].iter().cloned())
            .collect();
        let docs = test_binary_evidence::doc_argv(&logical)?;
        gates[3] = test_binary_evidence::build_argv(&logical)?[1..].to_vec();
        Some(docs[1..].to_vec())
    } else if nextest {
        doc_gate(&gates[3], libraries.as_ref())
    } else if parallel_tests {
        // `doc_gate` skips the `nextest run` prefix; give the `test` gate
        // the same two-word prefix so its package flags survive.
        let mut as_runner: Vec<String> = vec!["test".into(), "run".into()];
        as_runner.extend(gates[3].iter().skip(1).cloned());
        doc_gate(&as_runner, libraries.as_ref())
    } else {
        None
    };
    // The standalone vendored adapters change only with `vendor/` or the
    // toolchain, both of which select the whole workspace.
    if matches!(selection, affected::Selection::Workspace(_)) {
        gates.extend(timed("vendored adapter plan", || {
            vendor_regressions::commands(candidate, host, &target)
        })?);
    }
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
        let task = timed("task acceptance plan", || {
            native_provenance::task_for_claim(candidate, source, worker, &scopes)
        })?;
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
    if let Some(doc) = doc {
        gate_ids.push("doc-tests".into());
        gates.push(doc);
    }
    let plan: Vec<_> = gate_ids
        .iter()
        .zip(&gates)
        .map(|(id, args)| {
            let argv: Vec<_> = std::iter::once("cargo".to_owned())
                .chain(args.iter().cloned())
                .collect();
            let mut entry = serde_json::json!({"id":id,"argv":argv});
            if complete_workspace && id == "workspace-tests" {
                entry["execution"] = serde_json::json!({"schema":test_binary_evidence::SCHEMA});
            }
            entry
        })
        .collect();
    let mut native_gates = Vec::new();
    let mut pending_binary: Option<(usize, test_binary_evidence::Execution, serde_json::Value)> =
        None;
    let mut pending_workspace_proof: Option<(String, Command)> = None;
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
            sysroot = Some(timed("trusted sysroot", || {
                trusted_sysroot::prepare(candidate)
            })?);
        }
        let log_path = attempt.join(format!("gate-{index}.log"));
        let log = fs::File::create(&log_path)?;
        let started = std::time::Instant::now();
        let gate_target = args
            .windows(2)
            .find_map(|pair| (pair[0] == "--target-dir").then(|| PathBuf::from(&pair[1])))
            .unwrap_or_else(|| target.clone());
        let gate_env = GateEnv {
            home: &home,
            cargo_home: &cargo_home,
            rustup_home: &rustup_home,
            target: &gate_target,
            sysroot: sysroot.as_ref(),
        };
        let mut command = Command::new("cargo");
        apply_gate_env(&mut command, &gate_env)?;
        command
            .current_dir(candidate)
            .args(args)
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log));
        let share = current_gate_share();
        apply_gate_share(&mut command, share);
        print_gate_share(args.first().map_or("?", String::as_str), share);
        let measure = |cmd: &Command| {
            sysroot
                .as_ref()
                .filter(|_| index == 3 && !parallel_tests)
                .and_then(|s| {
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
        let binaries_gate = parallel_tests
            && gate_ids
                .get(index)
                .is_some_and(|id| id == "workspace-tests")
            && args.first().map(String::as_str) == Some("test");
        let (mut status, runner_note) = if binaries_gate && complete_workspace {
            let (execution, descriptor) = run_complete_binaries(&TestBinariesGate {
                candidate,
                attempt,
                args,
                host,
                target: &gate_target,
                log_path: &log_path,
                share,
                env: gate_env,
            })?;
            pending_binary = Some((index, execution, descriptor));
            (
                exit_status(0),
                Some("runner=complete-inventory-v1; companion doctests pending".into()),
            )
        } else if binaries_gate {
            // Cargo builds the binaries; the runner executes them beside
            // each other. The planned `cargo test` argv stays the gate's
            // identity in the evidence and the receipt. (`command` stays
            // for the proof fingerprint re-measure below; it never runs.)
            run_test_binaries(&TestBinariesGate {
                candidate,
                attempt,
                args,
                host,
                target: &gate_target,
                log_path: &log_path,
                share,
                env: gate_env,
            })?
        } else {
            (command.status()?, None)
        };
        // Out of disk mid-gate: reclaim the ship's idle caches and run the
        // command once more (serial gates only; the runner's binaries each
        // report their own status in the log).
        if !binaries_gate
            && !status.success()
            && gate_out_of_space(&fs::read(&log_path).unwrap_or_default())
        {
            let gate = gate_ids.get(index).map_or("?", String::as_str);
            if let Some(line) = reclaim_for_rerun(candidate, &log_path, &mut command, gate)? {
                evidence.push(line);
                status = command.status()?;
            }
        }
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
        record_phase(
            &format!("gate {}", gate_ids.get(index).map_or("?", String::as_str)),
            started.elapsed().as_millis(),
        );
        let doc_tests = args.iter().any(|arg| arg == "--doc");
        if status.success() && !doc_tests {
            let log_path = attempt.join(format!("gate-{index}.log"));
            let counted = match args.first().map(String::as_str) {
                Some("test") => Some(workspace_test_count::verified_pass_count(&log_path)),
                Some("nextest") => {
                    Some(workspace_test_count::verified_nextest_pass_count(&log_path))
                }
                _ => None,
            };
            if let Some(counted) = counted {
                let passed = counted.map_err(|error| {
                    format!("gate failed: {result}; {}: {error}", log_path.display())
                })?;
                result.push_str(&format!("; observed-passes={passed}"));
            }
        }
        if let Some(note) = runner_note {
            result.push_str(&format!("; {note}"));
        }
        evidence.push(result.clone());
        evidence.push(format!(
            "gate-share={} live={} jobs={} test-threads={}",
            gate_ids.get(index).map_or("?", String::as_str),
            share.live,
            share.jobs,
            share.test_threads
        ));
        fs::write(attempt.join("verification.txt"), evidence.join("\n"))?;
        if !status.success() {
            let log_bytes = fs::read(&log_path)?;
            let failed_log = native_provenance::store_blob(candidate, &log_bytes)?;
            fs::write(
                attempt.join(format!("gate-{index}.log.oid")),
                format!("{failed_log}\n"),
            )?;
            return Err(format!(
                "gate failed: {}{result}; log-blob={failed_log}; logs retained at {}",
                out_of_space_prefix(candidate, &log_bytes),
                attempt.display(),
            )
            .into());
        }
        // A serial proof cannot attest to a substituted binary runner. Keep the
        // command that actually ran, but publish nothing until every planned
        // gate (including any companion doctests) and the final recheck pass.
        if !binaries_gate {
            if let Some(key) = test_key.filter(|key| measure(&command).as_ref() == Some(key)) {
                pending_workspace_proof = Some((key, command));
            }
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
    if let Some((index, execution, mut descriptor)) = pending_binary {
        native_provenance::finish_binary_plan(candidate, &mut descriptor, &execution)?;
        native_gates[index]["execution"] = descriptor;
        // Preliminary per-gate time. The enclosing phase below also includes
        // final receipt persistence and the last input/artifact guard pass.
        native_gates[index]["elapsed_ms"] = serde_json::json!(execution.elapsed_ms());
        fs::write(
            attempt.join("native-gates.json"),
            serde_json::to_vec_pretty(&serde_json::json!({"plan":plan,"gates":native_gates}))?,
        )?;
        execution.recheck()?;
        record_phase(
            "complete workspace binaries including persistence and final checks",
            execution.elapsed_ms(),
        );
        evidence.push(format!(
            "complete-binaries-inclusive-ms={}",
            execution.elapsed_ms()
        ));
    }
    if let (Some((key, command)), Some(sysroot)) = (pending_workspace_proof, sysroot.as_ref()) {
        let remeasured = workspace_test_proof::fingerprint(
            candidate,
            &gates[3],
            workspace_test_proof::Toolchain {
                compiler: &compiler,
                host,
                sysroot: &sysroot.evidence,
            },
            &command,
            &home,
        )
        .ok();
        if let Some(record) =
            completed_workspace_proof(&key, remeasured.as_deref(), native_gates.len(), plan.len())
        {
            evidence.push(record);
        }
    }
    evidence.push(match &selection {
        affected::Selection::Packages(packages) => format!(
            "gate-selection=packages:{}",
            packages.iter().cloned().collect::<Vec<_>>().join(",")
        ),
        affected::Selection::Workspace(reason) => format!("gate-selection=workspace:{reason}"),
    });
    Ok(evidence)
}

// Completion is deliberately independent of test counts: every planned gate
// must succeed, and the exact execution inputs must still match at publication.
pub(in crate::local_workflow) fn completed_workspace_proof(
    key: &str,
    remeasured: Option<&str>,
    completed: usize,
    planned: usize,
) -> Option<String> {
    (planned > 0 && completed == planned && remeasured == Some(key))
        .then(|| workspace_test_proof::record(key))
}
/// Which packages the clippy and test gates cover: those the change since the
/// recorded integration base can affect. Without a recorded base, or with
/// `SUSI_LOCAL_GATE=full`, the whole workspace. Production qualification
/// (`local-release qualify`) always runs the whole workspace.
/// The gate's package selection, and the workspace's library packages when
/// they were loaded (the doctest gate may only name those).
pub(in crate::local_workflow) type GateSelection = (affected::Selection, Option<BTreeSet<String>>);

pub(in crate::local_workflow) fn gate_selection(
    candidate: &Path,
    attempt: &Path,
) -> Result<GateSelection> {
    if env::var("SUSI_LOCAL_GATE").is_ok_and(|mode| mode == "full") {
        return Ok((
            affected::Selection::Workspace("SUSI_LOCAL_GATE=full".into()),
            None,
        ));
    }
    let Ok(base) = fs::read_to_string(attempt.join("selection-base")) else {
        return Ok((
            affected::Selection::Workspace("no recorded base".into()),
            None,
        ));
    };
    let base = base.trim();
    if !valid_oid(base) {
        return Err("recorded selection base is not a commit oid".into());
    }
    let changed = affected::changed(candidate, base, "HEAD")?;
    let workspace = affected::load(candidate)?;
    Ok((
        affected::select(&workspace, &changed),
        Some(workspace.libraries.clone()),
    ))
}

/// Test threads for nextest gates: enough to finish the suite in parallel,
/// fewer than the machine's logical cores so the process-spawning tests keep
/// the CPU their wall-clock deadlines assume (see `.config/nextest.toml`).
pub(in crate::local_workflow) const GATE_TEST_THREADS: usize = 16;

/// Whether the test gate runs through `cargo nextest` instead of `cargo test`:
/// opt-in with `SUSI_LOCAL_TEST_RUNNER=nextest`. Measured 2026-11-06 on the
/// same 7 packages, nextest ran the tests in 351 s against 736 s, but 2 of the
/// 3 ships that used it on main failed on tests that pass under `cargo test`
/// (a susi-config concurrent-writer test, a susi-gmcp fail-closed test, and
/// two deadline tests under CPU contention). The suite coordinates tests with
/// 21 process-local static locks (`AUDIT_TEST_LOCK`, `ENV_LOCK`, ...), which
/// one process per test cannot honour; until that coordination is
/// cross-process safe, `cargo test` stays the default.
pub(in crate::local_workflow) fn nextest_requested() -> bool {
    env::var("SUSI_LOCAL_TEST_RUNNER").is_ok_and(|runner| runner == "nextest")
}

/// Turn a `nextest run ...` test gate into `test ...` unless nextest was
/// requested. The rest of the arguments (selection, target, features) are the
/// same for both runners.
pub(in crate::local_workflow) fn select_test_runner(gate: &mut Vec<String>, nextest: bool) {
    if !nextest && gate.first().map(String::as_str) == Some("nextest") {
        gate.splice(..2, ["test".to_owned()]);
    }
}

/// The doctest companion of a nextest test gate (nextest does not run
/// doctests), narrowed to the selected packages that have a library target.
/// `None` when the selection holds no library, so there is nothing to run.
pub(in crate::local_workflow) fn doc_gate(
    test_gate: &[String],
    libraries: Option<&BTreeSet<String>>,
) -> Option<Vec<String>> {
    let mut gate: Vec<String> = vec!["test".into(), "--doc".into()];
    let mut packages = Vec::new();
    let mut features = Vec::new();
    let mut rest = test_gate.iter().skip(2).peekable();
    while let Some(arg) = rest.next() {
        if arg == "-p" {
            let package = rest.next()?;
            if libraries.is_none_or(|libs| libs.contains(package)) {
                packages.push(package.clone());
            }
            continue;
        }
        if arg == "--features" {
            features.push(rest.next()?.clone());
            continue;
        }
        gate.push(arg.clone());
    }
    let whole = test_gate.iter().any(|arg| arg == "--workspace");
    // Keep a `pkg/feature` only when its package is still selected: cargo
    // refuses a feature of a package the narrowed doctest gate dropped.
    for feature in features {
        let keep = whole
            || feature
                .split_once('/')
                .is_none_or(|(package, _)| packages.iter().any(|kept| kept == package));
        if keep {
            gate.push("--features".into());
            gate.push(feature);
        }
    }
    if !whole {
        if packages.is_empty() {
            return None;
        }
        // Keep the package flags where narrowing put them: before `--offline`.
        let at = gate
            .iter()
            .position(|arg| arg == "--offline")
            .unwrap_or(gate.len());
        let flags: Vec<String> = packages
            .into_iter()
            .flat_map(|package| ["-p".to_owned(), package])
            .collect();
        gate.splice(at..at, flags);
    }
    Some(gate)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::local_workflow) enum RepoKind {
    Susi,
    Commercial,
}

pub(in crate::local_workflow) fn repo_kind(root: &Path) -> Result<RepoKind> {
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

pub(in crate::local_workflow) fn commercial_gate_argv() -> Vec<Vec<String>> {
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

pub(in crate::local_workflow) fn commercial_gate(
    candidate: &Path,
    attempt: &Path,
) -> Result<Vec<String>> {
    let _live = live_gate();
    let compiler = Command::new("rustc")
        .current_dir(candidate)
        .arg("-vV")
        .output()
        .map_err(|error| format!("rustc in {}: {error}", candidate.display()))?;
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
    let cargo_home = env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| original_home.join(".cargo"));
    let rustup_home = env::var_os("RUSTUP_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| original_home.join(".rustup"));
    let home = attempt.join("test-home");
    let _ = fs::remove_dir_all(&home);
    fs::create_dir_all(home.join("AppData/Local"))?;
    let target = restore_candidate_target(candidate);
    let mut gates = commercial_gate_argv();
    let selection = gate_selection(candidate, attempt).map(|(selection, _)| selection);
    let whole_workspace = !matches!(selection, Ok(affected::Selection::Packages(_)));
    if let Ok(affected::Selection::Packages(packages)) = &selection {
        for args in &mut gates {
            affected::narrow(args, packages);
        }
    }
    require_gate_disk(candidate, &target, whole_workspace)?;
    let mut evidence = Vec::new();
    for args in gates {
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
            .env("SUSI_NO_REGISTRY_ENV", "1")
            .env("CARGO_HOME", &cargo_home)
            .env("RUSTUP_HOME", &rustup_home)
            .env("CARGO_TARGET_DIR", &target)
            .env("CARGO_TERM_COLOR", "never")
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log));
        let share = commercial_command_share(current_gate_share(), &args, cfg!(windows));
        apply_gate_share(&mut command, share);
        print_gate_share(args.first().map_or("?", String::as_str), share);
        let mut status = command.status()?;
        if !status.success() && gate_out_of_space(&fs::read(&log_path).unwrap_or_default()) {
            let gate = args.first().map_or("?", String::as_str);
            if let Some(line) = reclaim_for_rerun(candidate, &log_path, &mut command, gate)? {
                evidence.push(line);
                status = command.status()?;
            }
        }
        let code = status.code().unwrap_or(1);
        let line = format!(
            "cargo {} => exit code: {code}; elapsed-ms={}",
            args.join(" "),
            started.elapsed().as_millis()
        );
        record_phase(
            &format!("gate cargo {}", args.first().map_or("?", String::as_str)),
            started.elapsed().as_millis(),
        );
        evidence.push(line.clone());
        if !status.success() {
            let prefix = out_of_space_prefix(candidate, &fs::read(&log_path).unwrap_or_default());
            return Err(format!("gate failed: {prefix}{line}; log {}", log_path.display()).into());
        }
    }
    require_ship_evidence(&evidence)?;
    Ok(evidence)
}

/// The agent's own fast check: format, then clippy and tests for only the
/// packages its changes (committed, staged, unstaged and untracked) can affect
/// since it diverged from `origin/main`. It builds in the agent's own worktree
/// target with a private home, so parallel agents share no build directory,
/// runtime state or credentials. Advice before `commit`; the serialized
/// `integrate`/`ship` gate stays the acceptance.
pub(in crate::local_workflow) fn check(root: &Path) -> Result<()> {
    let top = PathBuf::from(git(root, &["rev-parse", "--show-toplevel"])?);
    move_running_tool_out_of_the_target(&top);
    let base = git(&top, &["merge-base", "HEAD", "origin/main"])?;
    let changed = changed_paths(&top, &base)?;
    let mut gates: Vec<Vec<String>> = vec![
        vec!["fmt".into(), "--all".into(), "--check".into()],
        [
            "clippy",
            "--workspace",
            "--all-targets",
            "--locked",
            "--",
            "-D",
            "warnings",
        ]
        .iter()
        .map(|arg| (*arg).into())
        .collect(),
        ["nextest", "run", "--workspace", "--locked"]
            .iter()
            .map(|arg| (*arg).into())
            .collect(),
    ];
    let nextest = nextest_requested();
    select_test_runner(&mut gates[2], nextest);
    let workspace = affected::load(&top)?;
    let selection = affected::select(&workspace, &changed);
    let workspace_selected = matches!(selection, affected::Selection::Workspace(_));
    let complete_workspace = !nextest && workspace_selected && complete_workspace_requested();
    if complete_workspace {
        test_binary_evidence::require_supported_source_inputs()?;
    }
    match selection {
        affected::Selection::Packages(packages) => {
            println!(
                "check: {} changed paths since {}; {} packages: {}",
                changed.len(),
                &base[..base.len().min(12)],
                packages.len(),
                packages.iter().cloned().collect::<Vec<_>>().join(", ")
            );
            for args in &mut gates {
                affected::narrow(args, &packages);
            }
            include_release_tooling_for_xtask(&mut gates, &packages);
        }
        affected::Selection::Workspace(reason) => {
            println!(
                "check: {} changed paths; whole workspace ({reason})",
                changed.len()
            );
            vendor_regressions::include_release_tooling(&top, &mut gates);
        }
    }
    if workspace_selected && top.join("crates/susi-core/Cargo.toml").is_file() {
        let all: BTreeSet<String> = ["xtask".to_owned()].into();
        include_release_tooling_for_xtask(&mut gates, &all);
    }
    // A narrowed `cargo test` gate runs through the concurrent binary runner
    // (see `test_runner`), which leaves doctests to the companion gate.
    let runner_tests =
        !nextest && !test_runner::serial_requested() && (!workspace_selected || complete_workspace);
    if complete_workspace {
        let logical: Vec<String> = std::iter::once("cargo".into())
            .chain(gates[2].iter().cloned())
            .collect();
        gates.push(test_binary_evidence::doc_argv(&logical)?[1..].to_vec());
    } else if nextest {
        if let Some(doc) = doc_gate(&gates[2], Some(&workspace.libraries)) {
            gates.push(doc);
        }
    } else if runner_tests {
        let mut as_runner: Vec<String> = vec!["test".into(), "run".into()];
        as_runner.extend(gates[2].iter().skip(1).cloned());
        if let Some(doc) = doc_gate(&as_runner, Some(&workspace.libraries)) {
            gates.push(doc);
        }
    }
    let original_home = env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .ok_or("cannot identify toolchain home")?;
    let cargo_home = env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| original_home.join(".cargo"));
    let rustup_home = env::var_os("RUSTUP_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| original_home.join(".rustup"));
    let target = top.join("target");
    if complete_workspace {
        let (_, host) = gate_receipt::rustc_version(&top)?;
        for args in gates
            .iter_mut()
            .filter(|args| args.first().is_some_and(|a| a == "test"))
        {
            args.extend([
                "--target".into(),
                host.clone(),
                "--target-dir".into(),
                text(&target)?.into(),
            ]);
        }
        let logical: Vec<String> = std::iter::once("cargo".into())
            .chain(gates[2].iter().cloned())
            .collect();
        gates[2] = test_binary_evidence::build_argv(&logical)?[1..].to_vec();
    }
    seed_worktree_target(&top, &top, false);
    // The same refusal a ship makes: a check that would run out of disk
    // fails after its compile time with an error that reads like a compiler
    // bug, so a doomed one is refused first, after the idle caches are
    // reclaimed.
    timed("gate disk check", || {
        require_gate_disk(&top, &target, workspace_selected)
    })?;
    let home = target.join("susi-check-home");
    fs::create_dir_all(home.join("AppData/Local"))?;
    // Only this worktree's own target is locked: checks in other worktrees
    // and ships run at the same time, each command on its share of the host.
    let _host = acquire_host_gate(
        &target.join("check-gate"),
        &format!("pid={} check={}", std::process::id(), top.display()),
    )?;
    let _live = live_gate();
    let total = std::time::Instant::now();
    let mut pending_binary = None;
    for args in &gates {
        let started = std::time::Instant::now();
        let mut command = check_command(&top, args, &target, &home, (&cargo_home, &rustup_home));
        let share = current_gate_share();
        apply_gate_share(&mut command, share);
        print_gate_share(args.first().map_or("?", String::as_str), share);
        let binaries_gate = runner_tests
            && args.first().map(String::as_str) == Some("test")
            && !args.iter().any(|arg| arg == "--doc");
        let status = if binaries_gate && complete_workspace {
            let log = home.join("complete-workspace.log");
            fs::write(&log, [])?;
            pending_binary = Some(run_complete_binaries(&TestBinariesGate {
                candidate: &top,
                attempt: &home,
                args,
                host: "",
                target: &target,
                log_path: &log,
                share,
                env: GateEnv {
                    home: &home,
                    cargo_home: &cargo_home,
                    rustup_home: &rustup_home,
                    target: &target,
                    sysroot: None,
                },
            })?);
            exit_status(0)
        } else if binaries_gate {
            check_test_binaries(
                &CheckGate {
                    top: &top,
                    target: &target,
                    home: &home,
                    homes: (&cargo_home, &rustup_home),
                    share,
                },
                args,
            )?
        } else {
            command.status()?
        };
        println!(
            "check: cargo {} => {} in {:.1}s",
            args.first().map_or("", String::as_str),
            if status.success() { "ok" } else { "FAILED" },
            started.elapsed().as_secs_f64()
        );
        if !status.success() {
            return Err(format!("check failed: cargo {}", args.join(" ")).into());
        }
    }
    if let Some((execution, mut descriptor)) = pending_binary {
        native_provenance::finish_binary_plan(&top, &mut descriptor, &execution)?;
        execution.recheck()?;
        record_phase(
            "check complete binaries including final input checks",
            execution.elapsed_ms(),
        );
    }
    println!("check: passed in {:.1}s", total.elapsed().as_secs_f64());
    Ok(())
}

/// One `check` gate command: the worktree's own target, an isolated home for
/// the tests (HOME, USERPROFILE, APPDATA, LOCALAPPDATA, XDG_*, SUSI_HOME), and
/// the operator's real CARGO_HOME and RUSTUP_HOME, resolved before the home
/// override. Without the last two, tests that run cargo themselves (such as
/// `tests/zc_matrix.rs`'s `cargo tree --offline`) resolve an empty private
/// cargo home and fail offline, as they did under `check` before 2026-11-06.
pub(in crate::local_workflow) fn check_command(
    top: &Path,
    args: &[String],
    target: &Path,
    home: &Path,
    homes: (&Path, &Path),
) -> Command {
    let mut command = Command::new("cargo");
    check_env(&mut command, target, home, homes);
    command.current_dir(top).args(args);
    command
}

/// `cargo local-workflow check` runs this executable from
/// `<worktree>/target/debug/`, and a check that selects `xtask` rebuilds
/// that very file: the alias builds it without `release-tooling`, the gate
/// with it, so each relinks the same path. Windows refuses to replace a
/// running program ("Access is denied"), which failed every xtask check
/// until 2026-11-07. A running program may be renamed, though, so the file
/// moves aside (to `<target>/check-exe/`) before the gates run: Cargo then
/// links a fresh `local-workflow.exe` beside it, and the next alias build
/// links one again. Best effort: when the move fails the gate runs as before.
pub(in crate::local_workflow) fn move_running_tool_out_of_the_target(top: &Path) {
    let Ok(exe) = env::current_exe() else {
        return;
    };
    let target = top.join("target");
    if !exe.starts_with(&target) {
        return;
    }
    let aside = target.join("check-exe");
    if fs::create_dir_all(&aside).is_err() {
        return;
    }
    // Files from earlier checks: whichever are no longer running go now.
    if let Ok(entries) = fs::read_dir(&aside) {
        for entry in entries.flatten() {
            let _ = fs::remove_file(entry.path());
        }
    }
    let moved = aside.join(format!(
        "local-workflow-{}{}",
        std::process::id(),
        if cfg!(windows) { ".exe" } else { "" }
    ));
    let _ = fs::rename(&exe, &moved);
}

/// The `check` environment for a cargo command or one of the test binaries
/// it built: see `check_command`.
pub(in crate::local_workflow) fn check_env(
    command: &mut Command,
    target: &Path,
    home: &Path,
    (cargo_home, rustup_home): (&Path, &Path),
) {
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
    command
        .env("CARGO_TARGET_DIR", target)
        .env("CARGO_HOME", cargo_home)
        .env("RUSTUP_HOME", rustup_home)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("SUSI_HOME", home.join("susi"))
        .env("SUSI_NO_REGISTRY_ENV", "1")
        .env("APPDATA", home.join("appdata"))
        .env("LOCALAPPDATA", home.join("localappdata"))
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_CACHE_HOME", home.join("cache"));
}

/// `check`'s narrowed test gate: Cargo builds the binaries, the concurrent
/// runner executes them (each in its own home under the worktree target),
/// and every binary's output goes to `<target>/check-tests.log`.
#[derive(Clone, Copy)]
pub(in crate::local_workflow) struct CheckGate<'a> {
    pub(in crate::local_workflow) top: &'a Path,
    pub(in crate::local_workflow) target: &'a Path,
    pub(in crate::local_workflow) home: &'a Path,
    pub(in crate::local_workflow) homes: (&'a Path, &'a Path),
    pub(in crate::local_workflow) share: GateShare,
}

pub(in crate::local_workflow) fn check_test_binaries(
    gate: &CheckGate<'_>,
    args: &[String],
) -> Result<std::process::ExitStatus> {
    let CheckGate {
        top,
        target,
        home,
        homes,
        share,
    } = *gate;
    let log_path = target.join("check-tests.log");
    fs::write(&log_path, b"")?;
    let mut build = check_command(top, args, target, home, homes);
    build
        .args(["--no-run", "--message-format=json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    apply_gate_share(&mut build, share);
    let started = std::time::Instant::now();
    let output = build.output()?;
    let (binaries, diagnostics) =
        test_runner::parse_artifacts(&String::from_utf8_lossy(&output.stdout))?;
    for rendered in &diagnostics {
        eprint!("{rendered}");
    }
    println!(
        "check: test build => {} in {:.1}s; {} test binaries",
        if output.status.success() {
            "ok"
        } else {
            "FAILED"
        },
        started.elapsed().as_secs_f64(),
        binaries.len()
    );
    if !output.status.success() {
        return Ok(output.status);
    }
    let lanes = test_runner::concurrency(share.jobs);
    let heavy = test_runner::heavy_binaries(top);
    let home_root = target.join("susi-check-homes");
    let durations = target.join("test-durations.json");
    let settings = test_runner::Settings {
        concurrency: lanes,
        total_test_threads: share.test_threads,
        build_jobs: test_runner::threads_per_binary(share.jobs, lanes),
        home_root: &home_root,
        target_dir: target,
        triple: None,
        durations: &durations,
        heavy: &heavy,
        log: &log_path,
    };
    println!(
        "check: {} test binaries on up to {} lanes, {} aggregate test-thread budget; output in {}",
        binaries.len(),
        lanes,
        settings.total_test_threads,
        log_path.display()
    );
    let launch = move |binary: &test_runner::Binary, bin_home: &Path| -> Result<Command> {
        let mut command = Command::new(&binary.executable);
        check_env(&mut command, target, bin_home, homes);
        Ok(command)
    };
    let outcome = test_runner::run(&binaries, &settings, &launch)?;
    let _ = fs::remove_dir_all(&home_root);
    for (id, code) in &outcome.failed {
        println!(
            "check: {id} FAILED ({})",
            code.map_or_else(
                || "no exit code".to_owned(),
                |code| format!("exit code {code}")
            )
        );
    }
    let complete = outcome.failed.is_empty() && outcome.ran == binaries.len();
    println!(
        "check: {} of {} test binaries ran in {:.1}s, {} failed",
        outcome.ran,
        binaries.len(),
        outcome.elapsed_ms as f64 / 1000.0,
        outcome.failed.len()
    );
    Ok(exit_status(i32::from(!complete)))
}
