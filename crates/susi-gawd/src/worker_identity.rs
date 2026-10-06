//! One identity per worker: `<TOOL><WORKTREE-ID>`, never the human's git login.
//!
//! Every agent working on susi shares one machine, one clone and, until this
//! module, often one name. Three leaks were live at once:
//!
//! * `scripts/susi-worktree.sh` and `susi workflow start` named a new worker
//!   after `git config user.name`, so each came out `INTELLIBITZ<timestamp>`:
//!   the human's login, not the agent.
//! * A worktree the desktop app creates (`.claude/worktrees/<name>`) copies the
//!   creating tree's worktree config and answers to `PRIMARY`, the primary
//!   checkout's token, until someone trips the commit hook.
//! * Commits in such a worktree are authored by the clone-wide `user.name`.
//!
//! A task id is `T-<TOKEN>-<n>` and a claim belongs to its token, so a shared
//! token means colliding task ids and claims other workers can renew, close or
//! release. A token that names the tool *and* the worktree is unique by
//! construction, because a worktree's branch and directory name are unique in
//! its clone. [`ensure`] gives a worktree such a token (and a matching git
//! author) the first time any susi command runs in it.

use crate::admin::tasks;
use crate::admin::workflow::{Check, State};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Names of a role or a place, never of one worker.
const RESERVED: [&str; 6] = ["PRIMARY", "MAIN", "MASTER", "AGENT", "USER", "SUSI"];

/// The token of a name: upper-case ASCII letters and digits only, the alphabet
/// a task id allows (`tasks::valid_id`).
#[must_use]
pub fn token(raw: &str) -> String {
    raw.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

/// The tool a `<tool>/<name>` branch names (`claude/sweet-khorana-5d3fc4`).
fn tool_from_branch(branch: &str) -> Option<String> {
    let (prefix, _) = branch.split_once('/')?;
    crate::zc_agent_identity::detect_agent_from_chain(prefix)
}

/// A stable stand-in for a worktree whose branch and directory name carry no
/// letters or digits.
fn fnv(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0x811c_9dc5, |h, b| {
        (h ^ u32::from(*b)).wrapping_mul(0x0100_0193)
    })
}

/// The identity a worktree should carry: the tool, then the worktree's own
/// name, with a numeric suffix if `taken` already holds it. Never the git
/// login; with no tool to name, `AGENT` stands in.
#[must_use]
pub fn derive(tool: Option<&str>, branch: Option<&str>, dir: &Path, taken: &[String]) -> String {
    let tool = tool
        .map(token)
        .filter(|t| !t.is_empty())
        .or_else(|| branch.and_then(tool_from_branch))
        .unwrap_or_else(|| "AGENT".to_string());
    let name = |s: &str| Some(token(s)).filter(|t| !t.is_empty());
    let id = branch
        .and_then(|b| b.rsplit('/').next())
        .and_then(name)
        .or_else(|| dir.file_name().and_then(|n| n.to_str()).and_then(name))
        .unwrap_or_else(|| format!("{:08X}", fnv(dir.to_string_lossy().as_bytes())));
    // `susi-worktree.sh` already names a branch `<tool>-<date>-<time>-<pid>`.
    let base = if id.starts_with(&tool) {
        id
    } else {
        format!("{tool}{id}")
    };
    let mut candidate = base.clone();
    let mut n = 1;
    while RESERVED.contains(&candidate.as_str())
        || taken.iter().any(|t| t.eq_ignore_ascii_case(&candidate))
    {
        n += 1;
        candidate = format!("{base}{n}");
    }
    candidate
}

/// What a worktree's own config says about who it is.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Own {
    /// `susi.agent`: the claim token.
    pub agent: Option<String>,
    /// `user.name`: the git author of its commits.
    pub author: Option<String>,
}

/// `susi.agent` and `user.name` out of a `config.worktree` body.
fn parse_own(text: &str) -> Own {
    let mut section = String::new();
    let mut own = Own::default();
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix('[') {
            section = rest.trim_end_matches(']').trim().to_ascii_lowercase();
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim().trim_matches('"').to_string();
        if value.is_empty() {
            continue;
        }
        match (section.as_str(), key.trim().to_ascii_lowercase().as_str()) {
            ("susi", "agent") => own.agent = Some(value),
            ("user", "name") => own.author = Some(value),
            _ => {}
        }
    }
    own
}

/// Whether a worktree's identity is its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Standing {
    /// Unique to this worktree.
    Own,
    /// No `susi.agent` of its own in this worktree's config.
    Missing,
    /// Names a role or the shared git login, not a worker.
    Reserved(String),
    /// Other worktrees answer to the same token.
    Shared(Vec<PathBuf>),
}

impl Standing {
    fn describe(&self, token: &str) -> String {
        match self {
            Standing::Own => format!("{token} is this worktree's own"),
            Standing::Missing => "no identity of its own".to_string(),
            Standing::Reserved(why) => why.clone(),
            Standing::Shared(paths) => format!(
                "{token} is also the identity of {}",
                paths
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
}

/// Pure verdict: is the token this worktree carries its own, and is its git
/// author the same worker? `others` are the other worktrees' tokens; `logins`
/// the tokens of the `user.name` every agent shares.
#[must_use]
pub fn judge(own: &Own, others: &[(PathBuf, String)], logins: &[String]) -> (Standing, bool) {
    let mine = own.agent.as_deref().map(token).filter(|t| !t.is_empty());
    let standing = match &mine {
        None => Standing::Missing,
        Some(t) if RESERVED.contains(&t.as_str()) => {
            Standing::Reserved(format!("{t} names a role, not a worker"))
        }
        Some(t) if logins.contains(t) => {
            Standing::Reserved(format!("{t} is the git login every agent shares"))
        }
        Some(t) => {
            let sharing: Vec<PathBuf> = others
                .iter()
                .filter(|(_, other)| other == t)
                .map(|(path, _)| path.clone())
                .collect();
            if sharing.is_empty() {
                Standing::Own
            } else {
                Standing::Shared(sharing)
            }
        }
    };
    let author_ok = mine
        .as_ref()
        .is_some_and(|t| own.author.as_deref().map(token).as_ref() == Some(t));
    (standing, author_ok)
}

/// A git invocation that sees the repository it is pointed at, not the one a
/// calling hook exported through `GIT_DIR`.
fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn canonical(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

/// A worktree's git directory: `.git` itself, or what its `.git` file names.
fn git_dir_of(worktree: &Path) -> Option<PathBuf> {
    let dot_git = worktree.join(".git");
    if dot_git.is_dir() {
        return Some(dot_git);
    }
    let text = std::fs::read_to_string(&dot_git).ok()?;
    let dir = PathBuf::from(text.trim().strip_prefix("gitdir:")?.trim());
    Some(if dir.is_absolute() {
        dir
    } else {
        worktree.join(dir)
    })
}

/// The tokens of the `user.name` set outside any worktree: the human's login,
/// shared by every agent on this machine.
fn login_tokens(root: &Path) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for scope in ["--system", "--global", "--local"] {
        for name in git(root, &["config", scope, "--get-all", "user.name"])
            .unwrap_or_default()
            .lines()
        {
            let t = token(name);
            if !t.is_empty() && !out.contains(&t) {
                out.push(t);
            }
        }
    }
    out
}

/// Where a worktree stands, with what [`ensure`] needs to act on it.
#[derive(Debug, Clone)]
pub struct Assessment {
    /// The primary checkout: nobody works there, so it carries no worker identity.
    pub primary: bool,
    pub branch: Option<String>,
    /// This worktree's own `susi.agent`, as a token.
    pub token: Option<String>,
    pub standing: Standing,
    /// Its own `user.name` is the same worker.
    pub author_ok: bool,
    /// Every other worktree's token, for picking one nobody holds.
    taken: Vec<String>,
}

#[must_use]
pub fn assess(root: &Path) -> Assessment {
    let mine = git_dir_of(root);
    let common = git(
        root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .map(PathBuf::from);
    let primary = matches!(
        (&mine, &common),
        (Some(g), Some(c)) if canonical(g) == canonical(c)
    );
    let own = mine
        .as_deref()
        .and_then(|g| std::fs::read_to_string(g.join("config.worktree")).ok())
        .map(|t| parse_own(&t))
        .unwrap_or_default();

    let here = canonical(root);
    let mut others: Vec<(PathBuf, String)> = Vec::new();
    for line in git(root, &["worktree", "list", "--porcelain"])
        .unwrap_or_default()
        .lines()
    {
        let Some(path) = line.strip_prefix("worktree ").map(PathBuf::from) else {
            continue;
        };
        if canonical(&path) == here {
            continue;
        }
        let Some(dir) = git_dir_of(&path) else {
            continue;
        };
        let token = std::fs::read_to_string(dir.join("config.worktree"))
            .ok()
            .and_then(|t| parse_own(&t).agent)
            .map(|a| self::token(&a))
            .filter(|t| !t.is_empty())
            // The primary checkout answers to PRIMARY whether or not it says so.
            .or_else(|| {
                common
                    .as_deref()
                    .is_some_and(|c| canonical(&dir) == canonical(c))
                    .then(|| "PRIMARY".to_string())
            });
        if let Some(token) = token {
            others.push((path, token));
        }
    }

    let (standing, author_ok) = judge(&own, &others, &login_tokens(root));
    Assessment {
        primary,
        branch: git(root, &["symbolic-ref", "-q", "--short", "HEAD"]).filter(|b| !b.is_empty()),
        token: own.agent.as_deref().map(token).filter(|t| !t.is_empty()),
        standing,
        author_ok,
        taken: others.into_iter().map(|(_, t)| t).collect(),
    }
}

/// What [`ensure`] found and did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// `SUSI_AGENT` or `--agent` names the worker; keeping it unique is the
    /// operator's call (`CODEX1`, `CODEX2`).
    Explicit(String),
    /// The primary checkout, or no repository: no worker identity to assign.
    Primary,
    /// Already this worktree's own.
    Own(String),
    /// Was borrowed or missing; is now this worktree's own.
    Assigned { token: String, was: String },
    /// Borrowed, and not replaceable yet.
    Blocked { why: String, fix: String },
}

/// A live claim held under `token` from this branch, which a new token would
/// orphan. `Err` when the claims cannot be read.
fn live_claim(root: &Path, token: &str, branch: Option<&str>) -> Result<Option<String>, String> {
    let claims = tasks::claims(root).map_err(|e| e.to_string())?;
    let now = tasks::now_unix();
    Ok(claims
        .iter()
        .find(|c| {
            !c.expired(now)
                && c.agent.eq_ignore_ascii_case(token)
                && c.branch
                    .as_deref()
                    .is_none_or(|owner| Some(owner) == branch)
        })
        .map(|c| c.task.clone()))
}

fn write_identity(root: &Path, new: &str) -> bool {
    git(root, &["config", "extensions.worktreeConfig", "true"]);
    let email = format!("{}@localhost", new.to_ascii_lowercase());
    git(root, &["config", "--worktree", "susi.agent", new]).is_some()
        && git(root, &["config", "--worktree", "user.name", new]).is_some()
        && git(root, &["config", "--worktree", "user.email", &email]).is_some()
}

/// Make this worktree's identity its own, if it is not already.
///
/// `explicit` is `SUSI_AGENT` / `--agent`; `tool` the agent tool this process
/// runs inside, if known. Safe to call on every susi command: an identity that
/// is already its own costs a few local reads.
#[must_use]
pub fn ensure(root: &Path, tool: Option<&str>, explicit: Option<&str>) -> Outcome {
    if let Some(name) = explicit.map(str::trim).filter(|n| !n.is_empty()) {
        return Outcome::Explicit(token(name));
    }
    let a = assess(root);
    if a.primary || git_dir_of(root).is_none() {
        return Outcome::Primary;
    }
    if a.standing == Standing::Own {
        let mine = a.token.clone().unwrap_or_default();
        if a.author_ok {
            return Outcome::Own(mine);
        }
        // Only the git author is missing: nothing is bound to it, so just set it.
        return if write_identity(root, &mine) {
            Outcome::Assigned {
                token: mine,
                was: "commits authored by the shared git login".to_string(),
            }
        } else {
            Outcome::Blocked {
                why: "could not write this worktree's git author".to_string(),
                fix: "git config --worktree extensions.worktreeConfig true".to_string(),
            }
        };
    }
    let was = a.token.clone().unwrap_or_default();
    // A claim travels with its token: replacing it under a live claim would
    // strand the task, so that case is left to the agent.
    if !was.is_empty() {
        match live_claim(root, &was, a.branch.as_deref()) {
            Ok(None) => {}
            Ok(Some(task)) => {
                return Outcome::Blocked {
                    why: format!(
                        "{} while holding {task}, and a claim travels with its token",
                        a.standing.describe(&was)
                    ),
                    fix: format!("susi tasks release {task} && susi workflow own-identity"),
                };
            }
            Err(e) => {
                return Outcome::Blocked {
                    why: format!(
                        "{}, and the claims could not be read to check it is safe to replace ({e})",
                        a.standing.describe(&was)
                    ),
                    fix: "susi workflow own-identity   # once the remote is reachable".to_string(),
                };
            }
        }
    }
    let new = derive(tool, a.branch.as_deref(), root, &a.taken);
    if write_identity(root, &new) {
        Outcome::Assigned {
            token: new,
            was: if was.is_empty() {
                a.standing.describe("")
            } else {
                a.standing.describe(&was)
            },
        }
    } else {
        Outcome::Blocked {
            why: "could not write this worktree's identity".to_string(),
            fix: format!("git config --worktree susi.agent {new}"),
        }
    }
}

/// The `own identity` row of `susi workflow check`.
#[must_use]
pub fn check(outcome: &Outcome) -> Check {
    let (state, detail, fix) = match outcome {
        Outcome::Own(t) => (
            State::Pass,
            format!("{t} is this worktree's token and git author alone"),
            String::new(),
        ),
        Outcome::Assigned { token, was } => (
            State::Pass,
            format!("assigned {token} as this worktree's token and git author (was: {was})"),
            String::new(),
        ),
        Outcome::Explicit(t) => (
            State::Pass,
            format!(
                "{t} comes from SUSI_AGENT/--agent; keep it unique per worker (CODEX1, CODEX2)"
            ),
            String::new(),
        ),
        Outcome::Primary => (
            State::Pass,
            "the primary checkout carries no worker identity".to_string(),
            String::new(),
        ),
        Outcome::Blocked { why, fix } => (State::Fail, why.clone(), fix.clone()),
    };
    Check {
        name: "own identity",
        state,
        detail,
        fix,
    }
}

/// `tasks add` mints ids and `tasks claim` takes leases under a token: refuse
/// to do either under one another worker also answers to.
///
/// # Errors
/// The message names the problem and the command that fixes it.
pub fn require_own(root: &Path, explicit: bool) -> Result<(), String> {
    if explicit {
        return Ok(());
    }
    let a = assess(root);
    if a.primary || a.standing == Standing::Own {
        return Ok(());
    }
    let token = a.token.clone().unwrap_or_default();
    Err(format!(
        "this worktree has no identity of its own ({}): task ids and claims are minted under it, \
         so another worker would collide with them and could renew, close or release them. \
         Run `susi workflow own-identity`.",
        a.standing.describe(&token)
    ))
}

#[cfg(test)]
mod worker_identity_tests {
    use super::*;

    fn tok(s: &str) -> String {
        s.to_string()
    }

    #[test]
    fn worker_identity_names_the_tool_and_the_worktree() {
        let dir = Path::new("/w/susi-parallel-agent-identity-6b076d");
        assert_eq!(
            derive(
                None,
                Some("claude/susi-parallel-agent-identity-6b076d"),
                dir,
                &[]
            ),
            "CLAUDESUSIPARALLELAGENTIDENTITY6B076D"
        );
        // An explicit tool beats the branch prefix; a script-made branch that
        // already starts with the tool is not doubled.
        assert_eq!(
            derive(Some("codex"), Some("codex/worktree-20260930"), dir, &[]),
            "CODEXWORKTREE20260930"
        );
        assert_eq!(
            derive(
                Some("claude"),
                Some("claude-20261006-180120-253507"),
                dir,
                &[]
            ),
            "CLAUDE20261006180120253507"
        );
    }

    #[test]
    fn worker_identity_is_never_the_git_login_or_a_role() {
        // No tool to name and a branch that is just a role: still not a role.
        let dir = Path::new("/w/x");
        assert_eq!(derive(None, Some("main"), dir, &[]), "AGENTMAIN");
        assert_eq!(derive(None, None, Path::new("/w/!!"), &[]).len(), 13);
        assert!(!RESERVED.contains(&derive(None, Some("user"), dir, &[]).as_str()));
    }

    #[test]
    fn worker_identity_avoids_a_token_another_worktree_holds() {
        let dir = Path::new("/w/foo");
        let taken = [tok("CLAUDEFOO"), tok("CLAUDEFOO2")];
        assert_eq!(
            derive(Some("claude"), Some("claude/foo"), dir, &taken),
            "CLAUDEFOO3"
        );
    }

    #[test]
    fn worker_identity_judges_shared_reserved_and_missing_tokens() {
        let other = |p: &str, t: &str| (PathBuf::from(p), tok(t));
        let own = |agent: &str, author: &str| Own {
            agent: Some(agent.into()),
            author: Some(author.into()),
        };
        let logins = [tok("INTELLIBITZ")];

        // Unique, and the author is the same worker.
        let (s, a) = judge(
            &own("CLAUDEX", "CLAUDEX"),
            &[other("/a", "CODEXY")],
            &logins,
        );
        assert_eq!((s, a), (Standing::Own, true));
        // Unique token, but commits are still authored by the login.
        let (s, a) = judge(&own("CLAUDEX", "IntelliBitz"), &[], &logins);
        assert_eq!((s, a), (Standing::Own, false));
        // Another worktree answers to it.
        let (s, _) = judge(
            &own("PRIMARYX", "PRIMARYX"),
            &[other("/a", "PRIMARYX")],
            &[],
        );
        assert_eq!(s, Standing::Shared(vec![PathBuf::from("/a")]));
        // A role and the login are not workers.
        let (s, _) = judge(&own("primary", "primary"), &[], &logins);
        assert!(matches!(s, Standing::Reserved(_)), "{s:?}");
        let (s, _) = judge(&own("IntelliBitz", "x"), &[], &logins);
        assert!(matches!(s, Standing::Reserved(_)), "{s:?}");
        // Nothing of its own.
        let (s, a) = judge(&Own::default(), &[], &logins);
        assert_eq!((s, a), (Standing::Missing, false));
    }

    #[test]
    fn worker_identity_reads_only_the_worktree_sections_it_needs() {
        let body = "[core]\n\thooksPath = .githooks\n[susi]\n\tagent = CLAUDEX\n[user]\n\tname = CLAUDEX\n\temail = x@localhost\n";
        assert_eq!(
            parse_own(body),
            Own {
                agent: Some("CLAUDEX".into()),
                author: Some("CLAUDEX".into())
            }
        );
        assert_eq!(parse_own("[user]\n\temail = x@y\n"), Own::default());
    }
}
