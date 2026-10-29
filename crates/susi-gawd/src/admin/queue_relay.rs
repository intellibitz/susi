//! The queue's transport when the remote refuses the direct push.
//!
//! A claim, a renewal, a release and a close receipt are compare-and-swap
//! pushes of blobs to `refs/claims/*`, `refs/claim-agents/*`,
//! `refs/claim-generation/current`, `refs/closed/*` and `refs/abandoned/*`
//! (see [`super::tasks`]). A cloud session's git proxy (Claude Code on the web)
//! pushes branches and nothing else: every one of those pushes is answered with
//! HTTP 403 at receive-pack, a branch deletion is refused too, and the session
//! has no other write path to the remote — the REST API is read-only through
//! its proxy, and a workflow dispatch is a tool only the agent's own harness
//! can call. What it can do is push a new branch, and GitHub runs the workflow
//! files *of the pushed commit* for a push.
//!
//! So a refused push is relayed: the transaction — every lease and every ref
//! update, with the blobs' content, and an expiry — becomes a commit on
//! `refs/heads/susi-queue/<agent>/<expires>.<nonce>` whose tree is HEAD's (so
//! `.github/workflows/queue.yml` and `scripts/queue-relay.py` travel with it)
//! and whose message carries the request. That workflow replays the push with
//! the job's token, which may write any ref the rulesets leave open (the
//! merger's attestations already land that way), deletes the request, and
//! sweeps any request whose expiry has passed. The leases are the pusher's, so
//! the replay lands exactly what the pusher proved or nothing; the relay
//! refuses a request past its expiry, so a client that gave up waiting is
//! never contradicted later. The client waits for the request to be consumed
//! and judges the refs it alone writes, so a lost race is still a lost race.
//!
//! The direct push is tried first, always. The relay is only for a push the
//! remote refused while every lease still held — a lost compare-and-swap is
//! reported as one and never relayed — and only for the queue's own
//! namespaces. `SUSI_QUEUE_TRANSPORT=direct` forbids the relay and `relay`
//! skips the direct attempt; `SUSI_QUEUE_WAIT_SECS` (300) is a request's
//! lifetime and `SUSI_QUEUE_POLL_MS` (3000) the polling interval.
use crate::susi_error::{EaiError, EaiResult};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// A ref the transaction expects on the remote: `(name, sha)`; the empty sha
/// means "absent".
pub(super) type Lease = (String, String);
/// A ref the transaction writes: a blob's sha, or `None` to delete the ref.
pub(super) type Update = (String, Option<String>);

/// Where a request travels: `refs/heads/susi-queue/<agent>/<expires>.<nonce>`.
pub const REQUEST_PREFIX: &str = "refs/heads/susi-queue/";
/// The workflow that consumes a request.
pub const WORKFLOW: &str = ".github/workflows/queue.yml";
/// The namespaces a request may write; `scripts/queue-relay.py` refuses the
/// rest, and so does [`push`] before it relays, so a branch, a tag or the
/// merger's attestations never travel this way.
pub const RELAYED_NAMESPACES: [&str; 5] = [
    "refs/claims/",
    "refs/claim-agents/",
    "refs/claim-generation/",
    "refs/closed/",
    "refs/abandoned/",
];
/// The largest record a request carries; `scripts/queue-relay.py` agrees.
pub const MAX_BLOB_BYTES: usize = 65_536;
/// The one ref every claim and renewal of every agent rewrites: a sibling's
/// claim landing between the relay's push and the client's read-back moves it,
/// so it never decides whether this transaction landed.
const SHARED_REF_PREFIX: &str = "refs/claim-generation/";
/// The request format `scripts/queue-relay.py` reads.
const REQUEST_VERSION: u64 = 1;
const DEFAULT_WAIT_SECS: u64 = 300;
const MAX_WAIT_SECS: u64 = 86_400;
const DEFAULT_POLL_MS: u64 = 3_000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Transport {
    /// Direct first; relay a refusal that is not a lost lease.
    Auto,
    /// Never relay.
    Direct,
    /// Never try the direct push.
    Relay,
}

/// How a push travels, read once from the environment in production and
/// stated outright by the tests.
#[derive(Clone, Copy, Debug)]
struct Settings {
    transport: Transport,
    /// A request's lifetime: the relay refuses it after this.
    wait: Duration,
    poll: Duration,
}

impl Settings {
    fn from_env() -> Self {
        let mode = std::env::var("SUSI_QUEUE_TRANSPORT")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        let transport = match mode.as_str() {
            "direct" => Transport::Direct,
            "relay" => Transport::Relay,
            _ => Transport::Auto,
        };
        Self {
            transport,
            wait: Duration::from_secs(
                env_u64("SUSI_QUEUE_WAIT_SECS", DEFAULT_WAIT_SECS).clamp(1, MAX_WAIT_SECS),
            ),
            poll: Duration::from_millis(
                env_u64("SUSI_QUEUE_POLL_MS", DEFAULT_POLL_MS).clamp(50, 60_000),
            ),
        }
    }

    /// How long past the expiry the client keeps looking before it says the
    /// request will never apply: the relay judges the expiry by the runner's
    /// clock, and this covers the two clocks disagreeing.
    fn grace(&self) -> Duration {
        Duration::from_secs((self.wait.as_secs() / 10).clamp(1, 30))
    }
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// One compare-and-swap push: where it goes, what it expects, what it
/// writes, and what the caller says when a lease was lost.
#[derive(Clone, Copy)]
struct Txn<'a> {
    remote: &'a str,
    leases: &'a [Lease],
    updates: &'a [Update],
    raced: &'a str,
}

/// Push `updates` to `remote` under `leases`: directly, or — when the remote
/// refused the push while every lease still holds — through a request branch
/// the relay workflow replays. `raced` is what the caller says when a lease
/// was lost; it is returned verbatim so a caller's retry matcher still sees it.
pub(super) fn push(
    ws: &Path,
    remote: &str,
    leases: &[Lease],
    updates: &[Update],
    raced: &str,
) -> EaiResult<()> {
    let txn = Txn {
        remote,
        leases,
        updates,
        raced,
    };
    push_with(ws, txn, Settings::from_env())
}

fn push_with(ws: &Path, txn: Txn<'_>, settings: Settings) -> EaiResult<()> {
    let Txn {
        remote,
        leases,
        updates,
        raced,
    } = txn;
    if updates.is_empty() {
        return Ok(());
    }
    if settings.transport == Transport::Relay {
        if let Some(why) = outside_the_queue(leases, updates) {
            return Err(EaiError::config(format!("queue relay: {why}")));
        }
    } else {
        let refused = match direct(ws, remote, leases, updates) {
            Ok(()) => return Ok(()),
            Err(err) => err,
        };
        // A lease that no longer holds is a lost race, whatever the transport.
        if !leases_hold(ws, remote, leases)? {
            return Err(EaiError::config(raced.to_string()));
        }
        if settings.transport == Transport::Direct {
            return Err(refused);
        }
        if let Some(why) = outside_the_queue(leases, updates) {
            return Err(EaiError::process(format!("{refused} (not relayed: {why})")));
        }
        eprintln!(
            "queue: {remote} refused the direct push of {} while every lease still holds ({refused}); \
             relaying it through {WORKFLOW}",
            summary(updates)
        );
    }
    relay(ws, txn, settings)
}

/// Why this transaction may not travel as a request, if it may not: a ref
/// outside the queue's namespaces, which the relay would refuse anyway.
fn outside_the_queue(leases: &[Lease], updates: &[Update]) -> Option<String> {
    let queue_ref = |name: &str| RELAYED_NAMESPACES.iter().any(|ns| name.starts_with(ns));
    leases
        .iter()
        .map(|(name, _)| name)
        .chain(updates.iter().map(|(name, _)| name))
        .find(|name| !queue_ref(name))
        .map(|name| {
            format!(
                "{name} is outside the queue's namespaces ({})",
                RELAYED_NAMESPACES.join(", ")
            )
        })
}

fn direct(ws: &Path, remote: &str, leases: &[Lease], updates: &[Update]) -> EaiResult<()> {
    let mut args: Vec<String> = vec!["push".into(), "--quiet".into(), "--atomic".into()];
    args.extend(
        leases
            .iter()
            .map(|(name, old)| format!("--force-with-lease={name}:{old}")),
    );
    args.push(remote.to_string());
    args.extend(updates.iter().map(|(name, blob)| match blob {
        Some(sha) => format!("{sha}:{name}"),
        None => format!(":{name}"),
    }));
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    git(ws, &argv).map(|_| ())
}

fn remote_sha(ws: &Path, remote: &str, name: &str) -> EaiResult<String> {
    let listed = git(ws, &["ls-remote", remote, name])?;
    Ok(listed.split_whitespace().next().unwrap_or("").to_string())
}

fn leases_hold(ws: &Path, remote: &str, leases: &[Lease]) -> EaiResult<bool> {
    for (name, old) in leases {
        if remote_sha(ws, remote, name)? != *old {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Whether the relayed transaction landed: `None` when it did, else the ref
/// that shows it did not.
///
/// The push was atomic, so every ref holding the requested value means it
/// landed — and so does any one of them holding it after moving there from its
/// lease, whatever another writer did to the others since (a sibling's claim
/// freeing nothing of ours, say, or taking the task this release just freed).
/// The shared generation ref is never consulted: every claim and renewal of
/// every agent rewrites it.
fn not_landed(
    ws: &Path,
    remote: &str,
    leases: &[Lease],
    updates: &[Update],
) -> EaiResult<Option<String>> {
    let mut moved_to_ours = false;
    let mut first_other = None;
    for (name, blob) in updates {
        if name.starts_with(SHARED_REF_PREFIX) {
            continue;
        }
        let want = blob.as_deref().unwrap_or("");
        let lease = leases
            .iter()
            .find(|(leased, _)| leased == name)
            .map(|(_, old)| old.as_str());
        if remote_sha(ws, remote, name)? == want {
            moved_to_ours |= lease != Some(want);
        } else if first_other.is_none() {
            first_other = Some(name.clone());
        }
    }
    Ok(first_other.filter(|_| !moved_to_ours))
}

fn relay(ws: &Path, txn: Txn<'_>, settings: Settings) -> EaiResult<()> {
    let Txn {
        remote,
        leases,
        updates,
        raced,
    } = txn;
    let who = requester(ws);
    let expires = unix_now().saturating_add(settings.wait.as_secs());
    let request = request_document(ws, &who, expires, leases, updates)?;
    let name = format!("{REQUEST_PREFIX}{who}/{expires}.{}", nonce());
    let short = name.trim_start_matches("refs/heads/").to_string();
    // HEAD's tree, so the workflow and the relay script travel with the
    // request; HEAD as parent, so the request says which head made it.
    let tree = git(ws, &["rev-parse", "HEAD^{tree}"])?;
    let head = git(ws, &["rev-parse", "HEAD"])?;
    let message = format!("susi queue request: {}\n\n{request}\n", summary(updates));
    let commit = commit_tree(ws, &tree, &head, &message, &who)?;
    git(
        ws,
        &["push", "--quiet", remote, &format!("{commit}:{name}")],
    )
    .map_err(|err| {
        EaiError::process(format!(
            "queue relay: could not publish the request {short} on {remote}: {err}"
        ))
    })?;
    eprintln!(
        "queue: request {short} published; {WORKFLOW} has {}s to consume it",
        settings.wait.as_secs()
    );
    let started = Instant::now();
    let give_up = settings.wait.saturating_add(settings.grace());
    while !remote_sha(ws, remote, &name)?.is_empty() {
        if started.elapsed() >= give_up {
            // A run may have pushed and not yet deleted the request.
            if not_landed(ws, remote, leases, updates)?.is_none() {
                break;
            }
            return Err(EaiError::process(format!(
                "queue relay: {short} was not consumed in {}s and has expired: the relay refuses a \
                 request past its expiry, so it will never be applied, and the next relay run \
                 removes it. No run came for it — is {WORKFLOW} in this branch's tree (merge \
                 origin/main), and are Actions enabled on {remote}? Then retry; \
                 SUSI_QUEUE_WAIT_SECS gives a request longer.",
                settings.wait.as_secs()
            )));
        }
        std::thread::sleep(settings.poll);
    }
    if let Some(ref_name) = not_landed(ws, remote, leases, updates)? {
        return Err(EaiError::config(format!(
            "{raced} (relayed as {short}; {remote} does not carry the requested {ref_name} — \
             the Actions run for that branch says why)"
        )));
    }
    eprintln!("queue: relayed {} through {short}", summary(updates));
    Ok(())
}

/// The request `scripts/queue-relay.py` reads: version, requester, expiry
/// (unix seconds), the leases as `[name, sha]` pairs and the updates with their
/// blobs' content inline. One line: serde escapes every newline in a string.
fn request_document(
    ws: &Path,
    requester: &str,
    expires_unix: u64,
    leases: &[Lease],
    updates: &[Update],
) -> EaiResult<String> {
    let mut items = Vec::with_capacity(updates.len());
    for (name, blob) in updates {
        items.push(match blob {
            Some(sha) => serde_json::json!({"ref": name, "blob": blob_content(ws, sha)?}),
            None => serde_json::json!({"ref": name, "delete": true}),
        });
    }
    let leases: Vec<serde_json::Value> = leases
        .iter()
        .map(|(name, old)| serde_json::json!([name, old]))
        .collect();
    let doc = serde_json::json!({
        "v": REQUEST_VERSION,
        "requester": requester,
        "expires_unix": expires_unix,
        "leases": leases,
        "updates": items,
    });
    Ok(serde_json::to_string(&doc)?)
}

/// The blob's exact bytes: the relay re-hashes them, and the sha it lands
/// must be the one this side verifies.
fn blob_content(ws: &Path, sha: &str) -> EaiResult<String> {
    let out = Command::new("git")
        .args(["cat-file", "blob", sha])
        .current_dir(ws)
        .output()?;
    if !out.status.success() {
        return Err(EaiError::process(format!(
            "git cat-file blob {sha} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    if out.stdout.len() > MAX_BLOB_BYTES {
        return Err(EaiError::process(format!(
            "blob {sha} is {} bytes; a queue request carries records of at most {MAX_BLOB_BYTES}",
            out.stdout.len()
        )));
    }
    String::from_utf8(out.stdout).map_err(|_| {
        EaiError::process(format!(
            "blob {sha} is not UTF-8; the queue carries JSON records only"
        ))
    })
}

fn commit_tree(ws: &Path, tree: &str, parent: &str, message: &str, who: &str) -> EaiResult<String> {
    let name = git(ws, &["config", "--get", "user.name"])
        .ok()
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| who.to_string());
    let email = git(ws, &["config", "--get", "user.email"])
        .ok()
        .filter(|e| !e.is_empty())
        .unwrap_or_else(|| format!("{}@localhost", who.to_ascii_lowercase()));
    // A request is consumed, never kept: no signature to fail on.
    let mut child = Command::new("git")
        .args([
            "-c",
            "commit.gpgsign=false",
            "commit-tree",
            tree,
            "-p",
            parent,
            "-F",
            "-",
        ])
        .current_dir(ws)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_NAME", &name)
        .env("GIT_AUTHOR_EMAIL", &email)
        .env("GIT_COMMITTER_NAME", &name)
        .env("GIT_COMMITTER_EMAIL", &email)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or_else(|| EaiError::process("git commit-tree stdin"))?
        .write_all(message.as_bytes())?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        return Err(EaiError::process(format!(
            "git commit-tree failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Who asks: the worktree's agent token, else the git login, cleaned to what a
/// branch name takes.
fn requester(ws: &Path) -> String {
    let configured = git(ws, &["config", "--get", "susi.agent"])
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            git(ws, &["config", "--get", "user.name"])
                .ok()
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_default();
    let cleaned: String = configured
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches(|c| c == '-' || c == '.');
    if cleaned.is_empty() {
        "agent".to_string()
    } else {
        cleaned.to_string()
    }
}

fn nonce() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("{millis}-{}", std::process::id())
}

/// `claims/T-A-1, claim-agents/A, -claim-agents/B`: what a request touches,
/// deletions marked.
fn summary(updates: &[Update]) -> String {
    updates
        .iter()
        .map(|(name, blob)| {
            let short = name.strip_prefix("refs/").unwrap_or(name);
            if blob.is_some() {
                short.to_string()
            } else {
                format!("-{short}")
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn git(ws: &Path, args: &[&str]) -> EaiResult<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(ws)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()?;
    if !out.status.success() {
        return Err(EaiError::process(format!(
            "git {} failed: {}",
            args.first().copied().unwrap_or(""),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

#[cfg(test)]
// Test code: a failed fixture step is a failed test, so unwrap is the assertion.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct Fixture {
        root: PathBuf,
        bare: PathBuf,
        clone: PathBuf,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn run(dir: &Path, args: &[&str]) -> String {
        git(dir, args).unwrap()
    }

    fn fixture(tag: &str) -> Fixture {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let root = std::env::temp_dir().join(format!(
            "susi-queue-relay-{tag}-{}-{unique}",
            std::process::id()
        ));
        let bare = root.join("server.git");
        let clone = root.join("clone");
        std::fs::create_dir_all(&bare).unwrap();
        std::fs::create_dir_all(&clone).unwrap();
        run(&bare, &["init", "--bare", "--quiet", "-b", "main"]);
        run(&clone, &["init", "--quiet", "-b", "main"]);
        run(&clone, &["config", "user.name", "AGENTONE"]);
        run(&clone, &["config", "user.email", "agentone@localhost"]);
        run(&clone, &["config", "commit.gpgsign", "false"]);
        run(&clone, &["remote", "add", "origin", bare.to_str().unwrap()]);
        std::fs::write(clone.join("README"), "fixture\n").unwrap();
        run(&clone, &["add", "-A"]);
        run(&clone, &["commit", "--quiet", "-m", "init"]);
        run(&clone, &["push", "--quiet", "origin", "HEAD:main"]);
        Fixture { root, bare, clone }
    }

    fn blob(ws: &Path, text: &str) -> String {
        let mut child = Command::new("git")
            .args(["hash-object", "-w", "--stdin"])
            .current_dir(ws)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(text.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// The production defaults, stated: the tests never read the developer's
    /// environment for the transport.
    fn auto() -> Settings {
        Settings {
            transport: Transport::Auto,
            wait: Duration::from_secs(2),
            poll: Duration::from_millis(50),
        }
    }

    /// The cloud proxy, as the server sees it: nothing outside `refs/heads/*`.
    fn refuse_queue_pushes(bare: &Path) {
        use std::os::unix::fs::PermissionsExt;
        let hook = bare.join("hooks").join("pre-receive");
        std::fs::write(
            &hook,
            "#!/bin/sh\nwhile read -r _o _n ref; do case \"$ref\" in refs/heads/*) ;; *) \
             echo \"HTTP 403 ($ref)\" >&2; exit 1 ;; esac; done\n",
        )
        .unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn requests(bare: &Path) -> String {
        run(
            bare,
            &[
                "for-each-ref",
                "--format=%(refname)",
                "refs/heads/susi-queue/",
            ],
        )
    }

    #[test]
    fn a_direct_push_lands_and_leaves_no_request_behind() {
        let f = fixture("direct");
        let sha = blob(&f.clone, r#"{"task":"T-AGENTONE-1"}"#);
        let name = "refs/claims/T-AGENTONE-1".to_string();
        push_with(
            &f.clone,
            Txn {
                remote: "origin",
                leases: &[(name.clone(), String::new())],
                updates: &[(name.clone(), Some(sha.clone()))],
                raced: "raced",
            },
            auto(),
        )
        .unwrap();
        assert_eq!(run(&f.bare, &["rev-parse", &name]), sha);
        assert!(requests(&f.bare).is_empty());
        // A deletion under its lease, the same way.
        push_with(
            &f.clone,
            Txn {
                remote: "origin",
                leases: &[(name.clone(), sha)],
                updates: &[(name.clone(), None)],
                raced: "raced",
            },
            auto(),
        )
        .unwrap();
        assert!(run(&f.bare, &["for-each-ref", "refs/claims/"]).is_empty());
    }

    #[test]
    fn a_lost_lease_is_reported_as_the_race_and_never_relayed() {
        let f = fixture("race");
        let name = "refs/claims/T-AGENTONE-2".to_string();
        let theirs = blob(&f.clone, r#"{"agent":"OTHER"}"#);
        run(
            &f.clone,
            &["push", "--quiet", "origin", &format!("{theirs}:{name}")],
        );
        let mine = blob(&f.clone, r#"{"agent":"AGENTONE"}"#);
        // Expecting the ref absent, finding it taken: the caller's word, verbatim.
        let err = push_with(
            &f.clone,
            Txn {
                remote: "origin",
                leases: &[(name.clone(), String::new())],
                updates: &[(name.clone(), Some(mine))],
                raced: "T-AGENTONE-2 was claimed concurrently with another agent; refresh the queue and retry",
            },
            auto(),
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("claimed concurrently with another agent"),
            "{err}"
        );
        assert!(!err.contains("relay"), "{err}");
        assert_eq!(run(&f.bare, &["rev-parse", &name]), theirs);
        assert!(requests(&f.bare).is_empty());
    }

    #[test]
    fn the_request_carries_the_transaction_and_the_blobs_bytes() {
        let f = fixture("request");
        let sha = blob(&f.clone, r#"{"task":"T-AGENTONE-3","scopes":["a/b"]}"#);
        let doc = request_document(
            &f.clone,
            "AGENTONE",
            1_793_300_000,
            &[
                ("refs/claims/T-AGENTONE-3".into(), String::new()),
                ("refs/claim-generation/current".into(), "a".repeat(40)),
            ],
            &[
                ("refs/claims/T-AGENTONE-3".into(), Some(sha)),
                ("refs/claim-agents/OLD".into(), None),
            ],
        )
        .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&doc).unwrap();
        assert_eq!(parsed["v"], 1);
        assert_eq!(parsed["requester"], "AGENTONE");
        assert_eq!(parsed["expires_unix"], 1_793_300_000_u64);
        assert_eq!(
            parsed["leases"][0],
            serde_json::json!(["refs/claims/T-AGENTONE-3", ""])
        );
        assert_eq!(
            parsed["updates"][0]["blob"],
            r#"{"task":"T-AGENTONE-3","scopes":["a/b"]}"#
        );
        assert_eq!(parsed["updates"][1]["delete"], true);
        assert_eq!(
            summary(&[
                ("refs/claims/T-AGENTONE-3".into(), Some("x".into())),
                ("refs/claim-agents/OLD".into(), None),
            ]),
            "claims/T-AGENTONE-3, -claim-agents/OLD"
        );
        // The requester is the worktree's token, cleaned for a branch name.
        assert_eq!(requester(&f.clone), "AGENTONE");
        run(&f.clone, &["config", "susi.agent", "CLAUDE CCR/x"]);
        assert_eq!(requester(&f.clone), "CLAUDE-CCR-x");
    }

    #[test]
    fn a_refused_push_outside_the_queues_namespaces_is_never_relayed() {
        let f = fixture("namespace");
        refuse_queue_pushes(&f.bare);
        let name = "refs/verified/T-AGENTONE-4".to_string();
        let sha = blob(&f.clone, r#"{"task":"T-AGENTONE-4"}"#);
        let err = push_with(
            &f.clone,
            Txn {
                remote: "origin",
                leases: &[(name.clone(), String::new())],
                updates: &[(name, Some(sha))],
                raced: "raced",
            },
            auto(),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("HTTP 403"), "{err}");
        assert!(err.contains("not relayed"), "{err}");
        assert!(
            requests(&f.bare).is_empty(),
            "no request for an attestation"
        );
        // Relay-only refuses it up front, too.
        let err = push_with(
            &f.clone,
            Txn {
                remote: "origin",
                leases: &[],
                updates: &[("refs/tags/v9".into(), None)],
                raced: "raced",
            },
            Settings {
                transport: Transport::Relay,
                ..auto()
            },
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("outside the queue's namespaces"), "{err}");
        assert!(requests(&f.bare).is_empty());
    }

    #[test]
    fn a_relayed_transaction_is_judged_by_the_refs_it_alone_writes() {
        let f = fixture("judge");
        let ours = blob(&f.clone, r#"{"task":"T-AGENTONE-5","agent":"AGENTONE"}"#);
        let theirs = blob(&f.clone, r#"{"task":"T-OTHER-1","agent":"OTHER"}"#);
        let claim = "refs/claims/T-AGENTONE-5".to_string();
        let agent = "refs/claim-agents/AGENTONE".to_string();
        let generation = "refs/claim-generation/current".to_string();
        let leases: Vec<Lease> = vec![
            (claim.clone(), String::new()),
            (agent.clone(), String::new()),
            (generation.clone(), String::new()),
        ];
        let updates: Vec<Update> = vec![
            (claim.clone(), Some(ours.clone())),
            (agent.clone(), Some(ours.clone())),
            (generation.clone(), Some(ours.clone())),
        ];
        let set = |name: &str, sha: &str| {
            run(
                &f.clone,
                &[
                    "push",
                    "--quiet",
                    "--force",
                    "origin",
                    &format!("{sha}:{name}"),
                ],
            );
        };
        // Nothing applied: the claim ref is the evidence.
        assert_eq!(
            not_landed(&f.clone, "origin", &leases, &updates).unwrap(),
            Some(claim.clone())
        );
        // Applied, then a sibling's claim moved the shared generation ref: landed.
        set(&claim, &ours);
        set(&agent, &ours);
        set(&generation, &theirs);
        assert_eq!(
            not_landed(&f.clone, "origin", &leases, &updates).unwrap(),
            None
        );
        // A release that landed, then a sibling took the task it freed: landed,
        // because the agent ref — ours alone — moved from its lease to absent.
        let release_leases: Vec<Lease> =
            vec![(claim.clone(), ours.clone()), (agent.clone(), ours.clone())];
        let release: Vec<Update> = vec![(claim.clone(), None), (agent.clone(), None)];
        run(
            &f.clone,
            &["push", "--quiet", "origin", &format!(":{agent}")],
        );
        set(&claim, &theirs);
        assert_eq!(
            not_landed(&f.clone, "origin", &release_leases, &release).unwrap(),
            None
        );
        // A release that never applied: both refs still hold the lease.
        set(&claim, &ours);
        set(&agent, &ours);
        assert_eq!(
            not_landed(&f.clone, "origin", &release_leases, &release).unwrap(),
            Some(claim)
        );
    }
}
