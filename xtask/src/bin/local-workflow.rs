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
    scopes: Vec<String>,
}

fn read_claim(path: &Path) -> Result<Claim> {
    if fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err("claim must not be a symlink".into());
    }
    let data = fs::read_to_string(path)?;
    let mut lines = data.lines();
    if lines.next() != Some("susi-local-claim-v1") {
        return Err("unrecognized claim format; refusing to ignore it".into());
    }
    let claimant = lines.next().ok_or("missing claim name")?.to_owned();
    name(&claimant)?;
    let worktree = PathBuf::from(lines.next().ok_or("missing worktree")?);
    if !worktree.is_absolute() {
        return Err("claim worktree must be absolute".into());
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
    let claim_path = state.join("claims").join(format!("{claimant}.claim"));
    let worktree_text = text(&worktree)?;
    if worktree_text.chars().any(char::is_control) {
        return Err("worktree path contains control characters".into());
    }
    fs::write(
        &claim_path,
        format!(
            "susi-local-claim-v1\n{claimant}\n{worktree_text}\n{}\n",
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
            "HEAD",
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
        &["-c", &hook_config, "commit", "-m", message, "-m", &trailer],
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
        Some("list") if args.len() == 1 => {
            let state = state(&root)?;
            let _lock = Lock::acquire(&state)?;
            for claim in claims(&state)? { println!("{}\t{}\t{}", claim.name, claim.worktree.display(), claim.scopes.join(", ")); }
        }
        _ => return Err("usage: local-workflow start <name> --scope <path> [--scope <path> ...] | list | commit <name> <message> | release <name>".into()),
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
}
