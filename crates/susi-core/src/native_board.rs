//! Susi Native Board (`susi-board-item-v1`) client and bridge for `susi-core`.
//!
//! Every local, parallel, swarm, harness, A2A, ACP, and cloud agent collaborates
//! through the Susi Native Board by default and fails closed when no board
//! transport is reachable or when an open `stop` note / unanswered `request`
//! blocks work.
//!
//! Two transports share the same schema and semantics:
//! 1. **HTTP/HTTPS Gateway or Daemon Bridge (`SUSI_BOARD_URL`)**: for cloud,
//!    container, remote peer, and Core-only agents that talk to
//!    `/v1/board/*` and `/v1/blackboard/*`.
//! 2. **Git Native Board (`refs/board/items/<n>`)**: append-only JSON blobs
//!    created with compare-and-swap ref leases on the configured board storage
//!    remote (`SUSI_BOARD_REMOTE`, `SUSI_BOARD_REMOTE_URL`, `susi.board.remote`,
//!    or remote `board`), or in a blob-only bare cache. Public
//!    `intellibitz/susi` remotes are strictly refused per Mandate 76.

use crate::plain_http;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{OnceLock, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Where board items live on the storage remote and in every clone.
pub const ITEM_PREFIX: &str = "refs/board/items/";
/// Canonical board item schema version (parity with `susi-dev-workflow`).
pub const SCHEMA: &str = "susi-board-item-v1";
/// Environment variable selecting the HTTP/HTTPS gateway or daemon board URL.
pub const BOARD_URL_ENV: &str = "SUSI_BOARD_URL";
/// Git configuration key selecting the HTTP/HTTPS board bridge URL.
pub const BOARD_URL_CONFIG: &str = "susi.board.url";
/// Environment variable supplying bearer auth for `SUSI_BOARD_URL`.
pub const BOARD_TOKEN_ENV: &str = "SUSI_BOARD_TOKEN";
/// Environment override selecting the Git remote name or URL for the board.
pub const REMOTE_ENV: &str = "SUSI_BOARD_REMOTE";
/// Environment override supplying a direct Git board URL for blob-only caching.
pub const REMOTE_URL_ENV: &str = "SUSI_BOARD_REMOTE_URL";
/// Git configuration key supplying a direct Git board URL for blob-only caching.
pub const REMOTE_URL_CONFIG: &str = "susi.board.remote-url";
/// Git configuration key for the board storage remote.
pub const REMOTE_CONFIG: &str = "susi.board.remote";
/// Environment override identifying the acting agent.
pub const AGENT_ENV: &str = "SUSI_AGENT";
/// Allowed board item kinds (Mandate 76).
pub const KINDS: [&str; 12] = [
    "claim", "landed", "cut", "heads-up", "request", "answer", "handoff", "decision", "blocked",
    "stop", "lift", "note",
];
/// Maximum body payload size per item.
pub const MAX_BODY_BYTES: usize = 64 * 1024;
/// Maximum title length per item.
pub const MAX_TITLE_CHARS: usize = 200;
/// Compare-and-swap post retries with jitter before reporting contention.
pub const POST_ATTEMPTS: usize = 32;
/// How many item refs to fetch per batch.
pub const FETCH_CHUNK: usize = 200;
/// Default timeout for HTTP/HTTPS board calls.
const HTTP_TIMEOUT_SECS: u64 = 15;
const HTTP_MAX_RESPONSE_BYTES: u64 = 16 * 1024 * 1024;

/// Callback signature for outbound HTTPS requests registered by higher crates
/// (such as `susi-http-transport`).
pub type HttpsCallFn = fn(
    method: &str,
    url: &str,
    headers: &[(&str, &str)],
    body: Option<&[u8]>,
    timeout_secs: u64,
) -> Result<(u16, Vec<u8>), String>;

static HTTPS_TRANSPORT: OnceLock<HttpsCallFn> = OnceLock::new();

/// Register an HTTPS transport implementation so `SUSI_BOARD_URL=https://...`
/// works transparently inside `susi-core`.
pub fn register_https_transport(func: HttpsCallFn) {
    let _ = HTTPS_TRANSPORT.set(func);
}

/// One stored item on the Susi Native Board (`susi-board-item-v1`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardItem {
    pub schema: String,
    pub n: u64,
    pub kind: String,
    pub agent: String,
    pub host: String,
    pub posted_unix: u64,
    pub title: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub re: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    pub prev: String,
}

/// Input draft for posting a new board item.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BoardDraft {
    pub kind: String,
    pub agent: String,
    pub host: String,
    pub title: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub re: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
}

/// Board item paired with its blob SHA and sequence number.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredBoardItem {
    pub n: u64,
    pub sha: String,
    pub item: BoardItem,
}

/// Collaborative board status seen by an agent before starting or shipping work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardStatus {
    pub items: usize,
    pub last: u64,
    pub stops: Vec<String>,
    pub requests: Vec<String>,
    pub findings: Vec<String>,
}

/// Payload for `/v1/board/post` HTTP requests.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardPostRequest {
    #[serde(flatten)]
    pub draft: BoardDraft,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<u64>,
}

/// Payload for `/v1/board/check` HTTP requests.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardCheckRequest {
    pub agent: String,
    #[serde(default)]
    pub allow_empty: bool,
}

/// Entry posted to or read from the shared Swarm/Gateway Blackboard bridge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlackboardEntry {
    pub key: String,
    pub value: serde_json::Value,
    pub author: String,
    pub updated_unix: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub board_item: Option<u64>,
}

/// Shared key-value blackboard post request over HTTP/MCP/RPC.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlackboardPostRequest {
    pub key: String,
    pub value: serde_json::Value,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub task: Option<String>,
    #[serde(default = "default_mirror_true")]
    pub mirror_to_board: bool,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
}

const fn default_mirror_true() -> bool {
    true
}

/// Shared process-wide blackboard store used by `susi-server`, GMCP tools, and CLI.
#[must_use]
pub fn global_blackboard() -> &'static NativeBlackboardStore {
    static STORE: OnceLock<NativeBlackboardStore> = OnceLock::new();
    STORE.get_or_init(NativeBlackboardStore::new)
}

/// Extract a query parameter value from a URL query string.
#[must_use]
pub fn query_param(query: Option<&str>, key: &str) -> Option<String> {
    let q = query?;
    for pair in q.split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        if k == key {
            let trimmed = v.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.replace("%20", " "));
            }
        }
    }
    None
}

/// Format an entire stored item for human inspection (`susi board show`).
#[must_use]
pub fn format_full_item(stored: &StoredBoardItem) -> String {
    let item = &stored.item;
    let host = if item.host.is_empty() {
        "an unnamed host"
    } else {
        &item.host
    };
    let mut text = format!(
        "#{} {} by {} on {} at {} UTC\n",
        stored.n, item.kind, item.agent, host, item.posted_unix
    );
    if let Some(to) = &item.to {
        text.push_str(&format!("To: {to}\n"));
    }
    if let Some(re) = item.re {
        text.push_str(&format!("Re: item {re}\n"));
    }
    if let Some(task) = &item.task {
        text.push_str(&format!("Task: {task}\n"));
    }
    text.push_str(&item.title);
    text.push('\n');
    if !item.body.is_empty() {
        text.push('\n');
        text.push_str(&item.body);
        text.push('\n');
    }
    text
}

/// Post a blackboard entry into the shared store, persist workspace visibility
/// (`.susi/last_blackboard.json`), and mirror onto the Susi Native Board when
/// `req.mirror_to_board` is true (default). Fails closed if native board
/// collaboration is unreachable when mirroring is enabled.
pub fn post_blackboard_local(
    workspace: Option<&Path>,
    req: &BlackboardPostRequest,
) -> Result<BlackboardEntry, String> {
    let key = req.key.trim();
    if key.is_empty() {
        return Err("blackboard key must not be empty".to_owned());
    }
    let author = match req
        .author
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(a) => {
            validate_agent_word(a)?;
            a.to_owned()
        }
        None => resolve_agent(None, workspace)?,
    };
    let value_str = serde_json::to_string(&req.value).unwrap_or_default();
    check_no_credentials(key, &value_str)?;

    if !req.mirror_to_board {
        return Err(
            "blackboard writes must always mirror to the Susi Native Board; opt-out is forbidden"
                .to_owned(),
        );
    }

    let board_item = {
        let target = resolve_board_target(workspace)?;
        check_board(&target, &author, true)?;
        let kind = req
            .kind
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("note")
            .to_owned();
        let title = req
            .title
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("blackboard {key}"));
        let stored = post_item(
            &target,
            &BoardDraft {
                kind,
                agent: author.clone(),
                host: resolve_host(None),
                title,
                body: value_str.clone(),
                to: None,
                re: None,
                task: req.task.clone(),
            },
            None,
        )?;
        Some(stored.n)
    };

    let entry = global_blackboard().upsert(
        key.to_owned(),
        req.value.clone(),
        author.clone(),
        (req.task.clone(), board_item),
    )?;

    if let Some(ws) = workspace.filter(|p| p.is_dir()) {
        let entries = global_blackboard().list(None).unwrap_or_default();
        let redacted_entries: Vec<serde_json::Value> = entries
            .iter()
            .map(|e| {
                let raw = serde_json::to_string(&e.value).unwrap_or_default();
                let clean = crate::susi_config::redact_credentials(&raw);
                serde_json::json!({
                    "agent": e.key,
                    "author": e.author,
                    "bytes": clean.len(),
                    "output": clean,
                    "task": e.task,
                    "board_item": e.board_item,
                    "updated_unix": e.updated_unix,
                })
            })
            .collect();
        let doc = serde_json::json!({
            "kind": "mission_blackboard",
            "entries": redacted_entries,
            "agent_count": redacted_entries.len(),
        });
        let dir = ws.join(".susi");
        let _ = fs::create_dir_all(&dir);
        let _ = crate::susi_config::atomic_write_bytes(
            &dir.join("last_blackboard.json"),
            serde_json::to_string_pretty(&doc)
                .unwrap_or_else(|_| "{}".to_owned())
                .as_bytes(),
        );
    }

    Ok(entry)
}

/// Resolved collaborative board backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoardTarget {
    Http {
        base_url: String,
        token: Option<String>,
    },
    Git {
        work_dir: PathBuf,
        remote: String,
    },
}

/// Minimal thread-safe in-memory blackboard store usable anywhere in `susi-core`
/// or by HTTP gateway bridges.
#[derive(Debug, Default)]
pub struct NativeBlackboardStore {
    entries: RwLock<BTreeMap<String, BlackboardEntry>>,
}

impl NativeBlackboardStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn upsert(
        &self,
        key: String,
        value: serde_json::Value,
        author: String,
        task_and_item: (Option<String>, Option<u64>),
    ) -> Result<BlackboardEntry, String> {
        let (task, board_item) = task_and_item;
        let trimmed_key = key.trim();
        if trimmed_key.is_empty() {
            return Err("blackboard key must not be empty".to_owned());
        }
        let serialized = serde_json::to_string(&value).unwrap_or_default();
        check_no_credentials(trimmed_key, &serialized)?;
        let entry = BlackboardEntry {
            key: trimmed_key.to_owned(),
            value,
            author: author.trim().to_owned(),
            updated_unix: unix_now(),
            task,
            board_item,
        };
        let mut map = self
            .entries
            .write()
            .map_err(|_| "blackboard lock poisoned".to_owned())?;
        map.insert(trimmed_key.to_owned(), entry.clone());
        Ok(entry)
    }

    pub fn get(&self, key: &str) -> Result<Option<BlackboardEntry>, String> {
        let map = self
            .entries
            .read()
            .map_err(|_| "blackboard lock poisoned".to_owned())?;
        Ok(map.get(key.trim()).cloned())
    }

    pub fn list(&self, prefix: Option<&str>) -> Result<Vec<BlackboardEntry>, String> {
        let map = self
            .entries
            .read()
            .map_err(|_| "blackboard lock poisoned".to_owned())?;
        let p = prefix.map(str::trim).unwrap_or_default();
        Ok(map
            .values()
            .filter(|e| p.is_empty() || e.key.starts_with(p))
            .cloned()
            .collect())
    }
}

/// Timestamp in Unix seconds.
#[must_use]
pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Resolve the active agent identity or fail closed.
pub fn resolve_agent(explicit: Option<&str>, root: Option<&Path>) -> Result<String, String> {
    if let Some(raw) = explicit.map(str::trim).filter(|s| !s.is_empty()) {
        validate_agent_word(raw)?;
        return Ok(raw.to_owned());
    }
    if let Ok(env_agent) = std::env::var(AGENT_ENV) {
        let trimmed = env_agent.trim();
        if !trimmed.is_empty() {
            validate_agent_word(trimmed)?;
            return Ok(trimmed.to_owned());
        }
    }
    if let Some(dir) = root.filter(|p| p.exists()) {
        if let Ok(cfg) = git(dir, &["config", "--get", "susi.agent"]) {
            let trimmed = cfg.trim();
            if !trimmed.is_empty() {
                validate_agent_word(trimmed)?;
                return Ok(trimmed.to_owned());
            }
        }
    }
    Err(
        "agent identity is required for collaborative board access (--agent, SUSI_AGENT, or git config susi.agent)"
            .to_owned(),
    )
}

fn validate_agent_word(agent: &str) -> Result<(), String> {
    if agent.is_empty() || agent.chars().any(char::is_whitespace) {
        return Err("agent identity must be a single non-empty word without whitespace".to_owned());
    }
    Ok(())
}

/// Resolve the host name for board attribution.
#[must_use]
pub fn resolve_host(explicit: Option<&str>) -> String {
    if let Some(raw) = explicit.map(str::trim).filter(|s| !s.is_empty()) {
        return raw.to_owned();
    }
    for key in ["SUSI_HOST", "HOSTNAME", "COMPUTERNAME"] {
        if let Ok(val) = std::env::var(key) {
            let trimmed = val.trim();
            if !trimmed.is_empty() {
                return trimmed.to_owned();
            }
        }
    }
    if let Ok(text) = fs::read_to_string("/etc/hostname") {
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            return trimmed.to_owned();
        }
    }
    "local".to_owned()
}

fn resolve_bearer_token() -> Option<String> {
    for key in [
        BOARD_TOKEN_ENV,
        "SUSI_API_TOKEN",
        "SUSI_GATEWAY_TOKEN",
        "SUSI_DAEMON_TOKEN",
    ] {
        if let Ok(val) = std::env::var(key) {
            let trimmed = val.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_owned());
            }
        }
    }
    None
}

/// Return true if `url` points to the public `intellibitz/susi` repository,
/// which must NEVER carry `refs/board/*` per Mandate 76.
#[must_use]
pub fn is_public_susi_remote_url(url: &str) -> bool {
    let norm = url.trim().trim_end_matches('/').to_ascii_lowercase();
    let without_git = norm.strip_suffix(".git").unwrap_or(&norm);
    without_git.ends_with("github.com/intellibitz/susi")
        || without_git.ends_with("github.com:intellibitz/susi")
}

/// Resolve how `workspace` reaches the collaborative Susi Native Board.
/// Fails closed if neither `SUSI_BOARD_URL` nor a non-public Git board remote is
/// configured.
pub fn resolve_board_target(workspace: Option<&Path>) -> Result<BoardTarget, String> {
    if let Ok(url) = std::env::var(BOARD_URL_ENV) {
        let trimmed = url.trim().trim_end_matches('/');
        if !trimmed.is_empty() {
            return Ok(BoardTarget::Http {
                base_url: trimmed.to_owned(),
                token: resolve_bearer_token(),
            });
        }
    }

    if let Some(dir) = workspace.filter(|p| p.exists()) {
        if let Ok(url) = git(dir, &["config", "--get", BOARD_URL_CONFIG]) {
            let trimmed = url.trim().trim_end_matches('/');
            if !trimmed.is_empty() {
                return Ok(BoardTarget::Http {
                    base_url: trimmed.to_owned(),
                    token: resolve_bearer_token(),
                });
            }
        }
    }

    if let Ok(url) = std::env::var(REMOTE_URL_ENV) {
        let trimmed = url.trim();
        if !trimmed.is_empty() {
            if is_public_susi_remote_url(trimmed) {
                return Err(format!(
                    "refusing `{REMOTE_URL_ENV}={trimmed}`: refs/board/* is forbidden on public intellibitz/susi"
                ));
            }
            let cache_dir = ensure_blob_cache_dir(trimmed)?;
            return Ok(BoardTarget::Git {
                work_dir: cache_dir,
                remote: trimmed.to_owned(),
            });
        }
    }

    if let Some(dir) = workspace.filter(|p| p.exists()) {
        if let Ok(url) = git(dir, &["config", "--get", REMOTE_URL_CONFIG]) {
            let trimmed = url.trim();
            if !trimmed.is_empty() {
                if is_public_susi_remote_url(trimmed) {
                    return Err(format!(
                        "refusing `{REMOTE_URL_CONFIG}={trimmed}`: refs/board/* is forbidden on public intellibitz/susi"
                    ));
                }
                let cache_dir = ensure_blob_cache_dir(trimmed)?;
                return Ok(BoardTarget::Git {
                    work_dir: cache_dir,
                    remote: trimmed.to_owned(),
                });
            }
        }
    }

    if let Some(dir) = workspace.filter(|p| p.exists()) {
        if let Ok(remote_env) = std::env::var(REMOTE_ENV) {
            let trimmed = remote_env.trim();
            if !trimmed.is_empty() {
                verify_non_public_remote(dir, trimmed)?;
                return Ok(BoardTarget::Git {
                    work_dir: dir.to_path_buf(),
                    remote: trimmed.to_owned(),
                });
            }
        }
        if let Ok(cfg) = git(dir, &["config", "--get", REMOTE_CONFIG]) {
            let trimmed = cfg.trim();
            if !trimmed.is_empty() {
                verify_non_public_remote(dir, trimmed)?;
                return Ok(BoardTarget::Git {
                    work_dir: dir.to_path_buf(),
                    remote: trimmed.to_owned(),
                });
            }
        }
        if let Ok(board_url) = git(dir, &["remote", "get-url", "board"]) {
            if !is_public_susi_remote_url(&board_url) {
                return Ok(BoardTarget::Git {
                    work_dir: dir.to_path_buf(),
                    remote: "board".to_owned(),
                });
            }
        }
        if let Ok(origin_url) = git(dir, &["remote", "get-url", "origin"]) {
            if !is_public_susi_remote_url(&origin_url) {
                return Ok(BoardTarget::Git {
                    work_dir: dir.to_path_buf(),
                    remote: "origin".to_owned(),
                });
            }
        }
    }

    Err(format!(
        "Susi Native Board access is mandatory and no board target is configured or reachable (set `{BOARD_URL_ENV}`, `{REMOTE_URL_ENV}`, `{REMOTE_ENV}`, or git remote `board`; public `intellibitz/susi` origin is never used for `refs/board/*`)"
    ))
}

fn verify_non_public_remote(work_dir: &Path, remote: &str) -> Result<(), String> {
    if is_public_susi_remote_url(remote) {
        return Err(format!(
            "refusing `{remote}`: refs/board/* is forbidden on public intellibitz/susi"
        ));
    }
    if let Ok(url) = git(work_dir, &["remote", "get-url", remote]) {
        if is_public_susi_remote_url(&url) {
            return Err(format!(
                "refusing remote `{remote}` ({url}): refs/board/* is forbidden on public intellibitz/susi"
            ));
        }
    }
    Ok(())
}

fn ensure_blob_cache_dir(remote_url: &str) -> Result<PathBuf, String> {
    static CACHE_INIT_LOCK: OnceLock<std::sync::Mutex<()>> = OnceLock::new();
    let _guard = CACHE_INIT_LOCK
        .get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let base = std::env::var_os("SUSI_HOME")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("susi-board-cache"));
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in remote_url.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    let dir = base.join("board-cache").join(format!("{hash:016x}.git"));
    if !dir.join("HEAD").is_file() {
        fs::create_dir_all(&dir)
            .map_err(|e| format!("failed to create board cache {}: {e}", dir.display()))?;
        let _ = git(&dir, &["init", "--quiet", "--bare"])?;
        let _ = git(&dir, &["config", "core.hooksPath", "no-hooks"]);
    }
    Ok(dir)
}

fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    git_with(root, args, None)
}

fn git_with(root: &Path, args: &[&str], stdin: Option<&[u8]>) -> Result<String, String> {
    let mut child = Command::new("git")
        .current_dir(root)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run Git: {e}"))?;
    if let Some(bytes) = stdin {
        if let Some(mut pipe) = child.stdin.take() {
            pipe.write_all(bytes).map_err(|e| e.to_string())?;
        }
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!(
            "git {}: {}",
            args.first().copied().unwrap_or_default(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

fn check_no_credentials(title: &str, body: &str) -> Result<(), String> {
    for text in [title, body] {
        if crate::susi_config::redact_credentials(text) != *text
            || (text.contains("-----BEGIN") && text.contains("PRIVATE KEY"))
        {
            return Err(
                "this text looks like it contains a credential; the board never carries secrets"
                    .to_owned(),
            );
        }
    }
    Ok(())
}

/// Validate a draft before posting.
pub fn validate_draft(draft: &BoardDraft) -> Result<(), String> {
    if !KINDS.contains(&draft.kind.as_str()) {
        return Err(format!(
            "unknown kind `{}` (one of: {})",
            draft.kind,
            KINDS.join(", ")
        ));
    }
    validate_agent_word(draft.agent.trim())?;
    let title = draft.title.trim();
    if title.is_empty() || title.contains('\n') || title.chars().count() > MAX_TITLE_CHARS {
        return Err(format!(
            "the title must be one non-empty line of at most {MAX_TITLE_CHARS} characters"
        ));
    }
    if draft.body.len() > MAX_BODY_BYTES {
        return Err(format!(
            "the body is {} bytes; the limit is {MAX_BODY_BYTES}",
            draft.body.len()
        ));
    }
    if draft.kind == "request" && draft.to.as_deref().is_none_or(|to| to.trim().is_empty()) {
        return Err("a request names who is asked (--to <agent>)".to_owned());
    }
    if matches!(draft.kind.as_str(), "answer" | "lift") && draft.re.is_none() {
        return Err(format!(
            "`{}` names the item it refers to (--re <n>)",
            draft.kind
        ));
    }
    check_no_credentials(&draft.title, &draft.body)
}

fn parse_listing(listing: &str) -> Result<BTreeMap<u64, String>, String> {
    let mut items = BTreeMap::new();
    for line in listing.lines() {
        let mut fields = line.split_whitespace();
        let (Some(sha), Some(name)) = (fields.next(), fields.next()) else {
            continue;
        };
        let Some(suffix) = name.strip_prefix(ITEM_PREFIX) else {
            continue;
        };
        let number = suffix
            .parse::<u64>()
            .ok()
            .filter(|n| *n > 0 && n.to_string() == suffix)
            .ok_or_else(|| format!("`{name}` is not a board item ref (refs/board/items/<n>)"))?;
        items.insert(number, sha.to_owned());
    }
    Ok(items)
}

fn remote_items(root: &Path, remote: &str) -> Result<BTreeMap<u64, String>, String> {
    verify_non_public_remote(root, remote)?;
    let pattern = format!("{ITEM_PREFIX}*");
    let listing = git(root, &["ls-remote", "--refs", remote, &pattern])
        .map_err(|e| format!("cannot read the board on `{remote}`: {e}"))?;
    parse_listing(&listing)
}

fn local_items(root: &Path) -> Result<BTreeMap<u64, String>, String> {
    let listing = git(
        root,
        &[
            "for-each-ref",
            "--format=%(objectname) %(refname)",
            "refs/board/items",
        ],
    )?;
    parse_listing(&listing)
}

fn sync_git(root: &Path, remote: &str) -> Result<Vec<String>, String> {
    static SYNC_LOCK: OnceLock<std::sync::Mutex<()>> = OnceLock::new();
    let _guard = SYNC_LOCK
        .get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let theirs = remote_items(root, remote)?;
    let ours = local_items(root)?;
    let mut findings = Vec::new();
    for (n, sha) in &ours {
        match theirs.get(n) {
            None => findings.push(format!(
                "item {n} was removed from `{remote}` (local clone saw {sha})"
            )),
            Some(remote_sha) if remote_sha != sha => findings.push(format!(
                "item {n} was rewritten on `{remote}`: local clone saw {sha}, remote has {remote_sha}"
            )),
            Some(_) => {}
        }
    }
    let missing: Vec<String> = theirs
        .keys()
        .filter(|n| !ours.contains_key(n))
        .map(|n| format!("{ITEM_PREFIX}{n}:{ITEM_PREFIX}{n}"))
        .collect();
    for chunk in missing.chunks(FETCH_CHUNK) {
        let mut args = vec!["fetch", "--quiet", "--no-tags", remote];
        args.extend(chunk.iter().map(String::as_str));
        git(root, &args).map_err(|e| format!("cannot fetch the board from `{remote}`: {e}"))?;
    }
    Ok(findings)
}

/// Load and verify contiguous chain integrity of all locally cached board items
/// inside `root`.
pub fn load_git(root: &Path) -> Result<(Vec<StoredBoardItem>, Vec<String>), String> {
    let mut stored = Vec::new();
    let mut findings = Vec::new();
    let mut previous: Option<(u64, String)> = None;
    for (n, sha) in local_items(root)? {
        let parsed = git(root, &["cat-file", "blob", &sha])
            .and_then(|text| serde_json::from_str::<BoardItem>(&text).map_err(|e| e.to_string()));
        match parsed {
            Ok(item) => {
                if item.schema != SCHEMA {
                    findings.push(format!("item {n} has unexpected schema `{}`", item.schema));
                }
                if item.n != n {
                    findings.push(format!(
                        "item {n} claims to be item {} (wrong ref number)",
                        item.n
                    ));
                }
                match &previous {
                    Some((last, last_sha)) => {
                        if n != last + 1 {
                            findings.push(format!("items {} to {} are missing", last + 1, n - 1));
                        } else if item.prev != *last_sha {
                            findings.push(format!(
                                "item {n} does not chain from item {last} (claims {}, stored is {last_sha})",
                                if item.prev.is_empty() { "nothing" } else { &item.prev }
                            ));
                        }
                    }
                    None => {
                        if n != 1 {
                            findings
                                .push(format!("items 1 to {} are missing before item {n}", n - 1));
                        } else if !item.prev.is_empty() {
                            findings.push(format!(
                                "item 1 unexpectedly names prior SHA `{}`",
                                item.prev
                            ));
                        }
                    }
                }
                stored.push(StoredBoardItem {
                    n,
                    sha: sha.clone(),
                    item,
                });
            }
            Err(error) => findings.push(format!("item {n} cannot be decoded: {error}")),
        }
        previous = Some((n, sha));
    }
    Ok((stored, findings))
}

fn addressed_to(to: Option<&str>, agent: &str) -> bool {
    to.is_none_or(|t| t.eq_ignore_ascii_case("all") || t.eq_ignore_ascii_case(agent))
}

/// Unlifted stop notes binding `agent`.
#[must_use]
pub fn open_stops<'a>(items: &'a [StoredBoardItem], agent: &str) -> Vec<&'a StoredBoardItem> {
    items
        .iter()
        .filter(|s| s.item.kind == "stop" && addressed_to(s.item.to.as_deref(), agent))
        .filter(|s| {
            !items.iter().any(|later| {
                later.n > s.n && later.item.kind == "lift" && later.item.re == Some(s.n)
            })
        })
        .collect()
}

/// Requests addressed specifically to `agent` that `agent` has not yet answered.
#[must_use]
pub fn open_requests<'a>(items: &'a [StoredBoardItem], agent: &str) -> Vec<&'a StoredBoardItem> {
    items
        .iter()
        .filter(|s| {
            s.item.kind == "request"
                && s.item
                    .to
                    .as_deref()
                    .is_some_and(|to| to.eq_ignore_ascii_case(agent))
        })
        .filter(|s| {
            !items.iter().any(|later| {
                later.n > s.n
                    && later.item.re == Some(s.n)
                    && later.item.agent.eq_ignore_ascii_case(agent)
            })
        })
        .collect()
}

/// Summarize an item as one readable line.
#[must_use]
pub fn summary_line(stored: &StoredBoardItem) -> String {
    let item = &stored.item;
    let mut line = format!(
        "{:>5}  {}  {:<9} {:<14}",
        stored.n, item.posted_unix, item.kind, item.agent
    );
    if let Some(to) = &item.to {
        line.push_str(&format!(" to {to}:"));
    }
    if let Some(re) = item.re {
        line.push_str(&format!(" re {re}:"));
    }
    line.push(' ');
    line.push_str(&item.title);
    line
}

/// Compute `BoardStatus` from an already-fetched slice of items and findings.
#[must_use]
pub fn compute_status(
    items: &[StoredBoardItem],
    findings: Vec<String>,
    agent: &str,
) -> BoardStatus {
    BoardStatus {
        items: items.len(),
        last: items.last().map_or(0, |s| s.n),
        stops: open_stops(items, agent)
            .into_iter()
            .map(summary_line)
            .collect(),
        requests: open_requests(items, agent)
            .into_iter()
            .map(summary_line)
            .collect(),
        findings,
    }
}

/// Evaluate whether `status` permits work to start or land (fail closed).
pub fn enforce_status(status: &BoardStatus, agent: &str, allow_empty: bool) -> Result<(), String> {
    if !status.findings.is_empty() {
        return Err(format!(
            "Susi Native Board integrity check failed: {}",
            status.findings.join("; ")
        ));
    }
    if !allow_empty && status.items == 0 {
        return Err(
            "Susi Native Board is empty; agents must initialize/claim on the collaborative board before proceeding"
                .to_owned(),
        );
    }
    if !status.stops.is_empty() {
        return Err(format!(
            "open STOP note on Susi Native Board for `{agent}`:\n  {}",
            status.stops.join("\n  ")
        ));
    }
    if !status.requests.is_empty() {
        return Err(format!(
            "unanswered request(s) on Susi Native Board for `{agent}`:\n  {}",
            status.requests.join("\n  ")
        ));
    }
    Ok(())
}

fn jitter_backoff(attempt: usize) {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos() as u64);
    let base_ms = 12_u64.saturating_add((attempt as u64).saturating_mul(7));
    let jitter_ms = nanos % 37;
    std::thread::sleep(Duration::from_millis((base_ms + jitter_ms).min(150)));
}

/// Post a validated `BoardDraft` over Git compare-and-swap ref lease.
pub fn post_git(
    root: &Path,
    remote: &str,
    draft: &BoardDraft,
    now: u64,
    start: Option<u64>,
) -> Result<StoredBoardItem, String> {
    verify_non_public_remote(root, remote)?;
    validate_draft(draft)?;
    let mut last_error = String::new();
    for attempt in 0..POST_ATTEMPTS {
        let items = remote_items(root, remote)?;
        let (n, prev) = match items.iter().next_back() {
            Some((last, sha)) => {
                if start.is_some() {
                    return Err(format!(
                        "--start only numbers the first item of an empty board; this board is at item {last}"
                    ));
                }
                (last + 1, sha.clone())
            }
            None => (start.filter(|v| *v > 0).unwrap_or(1), String::new()),
        };
        if let Some(re) = draft.re {
            if !items.contains_key(&re) {
                return Err(format!("--re {re}: the board has no item {re}"));
            }
        }
        let item = BoardItem {
            schema: SCHEMA.to_owned(),
            n,
            kind: draft.kind.clone(),
            agent: draft.agent.trim().to_owned(),
            host: draft.host.trim().to_owned(),
            posted_unix: now,
            title: draft.title.trim().to_owned(),
            body: draft.body.trim_end().to_owned(),
            to: draft
                .to
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
            re: draft.re,
            task: draft
                .task
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
            prev,
        };
        let mut text = serde_json::to_string_pretty(&item).map_err(|e| e.to_string())?;
        text.push('\n');
        let sha = git_with(
            root,
            &["hash-object", "-w", "--stdin"],
            Some(text.as_bytes()),
        )?;
        let item_ref = format!("{ITEM_PREFIX}{n}");
        let lease = format!("--force-with-lease={item_ref}:");
        match git(
            root,
            &[
                "push",
                "--quiet",
                &lease,
                remote,
                &format!("{sha}:{item_ref}"),
            ],
        ) {
            Ok(_) => {
                let _ = git(root, &["update-ref", &item_ref, &sha]);
                let _ = sync_git(root, remote);
                return Ok(StoredBoardItem { n, sha, item });
            }
            Err(err) => {
                if remote_items(root, remote)?.contains_key(&n) {
                    last_error = err;
                    jitter_backoff(attempt);
                    continue;
                }
                return Err(format!("cannot post item {n} to `{remote}`: {err}"));
            }
        }
    }
    Err(format!(
        "no free item number after {POST_ATTEMPTS} attempts (contention): {last_error}"
    ))
}

fn http_call(
    method: &str,
    url: &str,
    token: Option<&str>,
    body: Option<&[u8]>,
) -> Result<(u16, Vec<u8>), String> {
    let mut headers: Vec<(&str, &str)> = vec![("Accept", "application/json")];
    let auth_header: String;
    if body.is_some() {
        headers.push(("Content-Type", "application/json"));
    }
    if let Some(tok) = token.filter(|t| !t.trim().is_empty()) {
        auth_header = format!("Bearer {}", tok.trim());
        headers.push(("Authorization", &auth_header));
    }

    if url.starts_with("https://") {
        let func = HTTPS_TRANSPORT.get().ok_or_else(|| {
            "HTTPS Susi Native Board transport is not registered in this binary (initialize susi-http-transport or pass http:// for local loopback)"
                .to_owned()
        })?;
        return func(method, url, &headers, body, HTTP_TIMEOUT_SECS);
    }

    let resp = plain_http::call(method, url, &headers, body, HTTP_TIMEOUT_SECS)?;
    let status = resp.status;
    let bytes = resp
        .into_bytes(HTTP_MAX_RESPONSE_BYTES)
        .map_err(|e| format!("failed reading board HTTP response from {url}: {e}"))?;
    Ok((status, bytes))
}

fn parse_http_error(status: u16, bytes: &[u8], url: &str) -> String {
    if let Ok(val) = serde_json::from_slice::<serde_json::Value>(bytes) {
        if let Some(msg) = val.get("error").and_then(|v| {
            v.as_str()
                .or_else(|| v.get("message").and_then(|m| m.as_str()))
        }) {
            return format!("board HTTP {status} from {url}: {msg}");
        }
    }
    let text = String::from_utf8_lossy(bytes);
    format!("board HTTP {status} from {url}: {}", text.trim())
}

/// Sync/list all stored items from either HTTP gateway or Git remote.
pub fn list_items(target: &BoardTarget) -> Result<(Vec<StoredBoardItem>, Vec<String>), String> {
    match target {
        BoardTarget::Http { base_url, token } => {
            let url = format!("{base_url}/v1/board/items");
            let (status, bytes) = http_call("GET", &url, token.as_deref(), None)?;
            if !(200..300).contains(&status) {
                return Err(parse_http_error(status, &bytes, &url));
            }
            if let Ok(items) = serde_json::from_slice::<Vec<StoredBoardItem>>(&bytes) {
                return Ok((items, Vec::new()));
            }
            let wrapped: serde_json::Value =
                serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
            let arr = wrapped
                .get("items")
                .cloned()
                .ok_or_else(|| "unexpected /v1/board/items JSON payload".to_owned())?;
            let items: Vec<StoredBoardItem> =
                serde_json::from_value(arr).map_err(|e| e.to_string())?;
            let findings: Vec<String> = wrapped
                .get("findings")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_default();
            Ok((items, findings))
        }
        BoardTarget::Git { work_dir, remote } => {
            let mut findings = sync_git(work_dir, remote)?;
            let (items, shape) = load_git(work_dir)?;
            findings.extend(shape);
            Ok((items, findings))
        }
    }
}

/// Query board status for `agent`.
pub fn board_status(target: &BoardTarget, agent: &str) -> Result<BoardStatus, String> {
    match target {
        BoardTarget::Http { base_url, token } => {
            let url = format!("{base_url}/v1/board/status?agent={}", agent.trim());
            let (status_code, bytes) = http_call("GET", &url, token.as_deref(), None)?;
            if !(200..300).contains(&status_code) {
                return Err(parse_http_error(status_code, &bytes, &url));
            }
            serde_json::from_slice::<BoardStatus>(&bytes)
                .map_err(|e| format!("invalid board status JSON from {url}: {e}"))
        }
        BoardTarget::Git { work_dir, remote } => {
            let mut findings = sync_git(work_dir, remote)?;
            let (items, shape) = load_git(work_dir)?;
            findings.extend(shape);
            Ok(compute_status(&items, findings, agent))
        }
    }
}

/// Check the Susi Native Board for `agent` and fail closed on any blocker or
/// missing connectivity.
pub fn check_board(
    target: &BoardTarget,
    agent: &str,
    allow_empty: bool,
) -> Result<BoardStatus, String> {
    match target {
        BoardTarget::Http { base_url, token } => {
            let url = format!("{base_url}/v1/board/check");
            let req = BoardCheckRequest {
                agent: agent.trim().to_owned(),
                allow_empty,
            };
            let body = serde_json::to_vec(&req).map_err(|e| e.to_string())?;
            let (status_code, bytes) = http_call("POST", &url, token.as_deref(), Some(&body))?;
            if !(200..300).contains(&status_code) {
                return Err(parse_http_error(status_code, &bytes, &url));
            }
            let status: BoardStatus = serde_json::from_slice(&bytes)
                .map_err(|e| format!("invalid /v1/board/check response: {e}"))?;
            enforce_status(&status, agent, allow_empty)?;
            Ok(status)
        }
        BoardTarget::Git { .. } => {
            let status = board_status(target, agent)?;
            enforce_status(&status, agent, allow_empty)?;
            Ok(status)
        }
    }
}

/// Post an item onto the Susi Native Board via HTTP or Git.
pub fn post_item(
    target: &BoardTarget,
    draft: &BoardDraft,
    start: Option<u64>,
) -> Result<StoredBoardItem, String> {
    validate_draft(draft)?;
    match target {
        BoardTarget::Http { base_url, token } => {
            let url = format!("{base_url}/v1/board/post");
            let req = BoardPostRequest {
                draft: draft.clone(),
                start,
            };
            let body = serde_json::to_vec(&req).map_err(|e| e.to_string())?;
            let (status_code, bytes) = http_call("POST", &url, token.as_deref(), Some(&body))?;
            if !(200..300).contains(&status_code) {
                return Err(parse_http_error(status_code, &bytes, &url));
            }
            serde_json::from_slice::<StoredBoardItem>(&bytes)
                .map_err(|e| format!("invalid /v1/board/post response: {e}"))
        }
        BoardTarget::Git { work_dir, remote } => {
            post_git(work_dir, remote, draft, unix_now(), start)
        }
    }
}

/// Post a key-value entry to the remote `/v1/blackboard/post` HTTP bridge.
pub fn post_blackboard_http(
    target: &BoardTarget,
    req: &BlackboardPostRequest,
) -> Result<BlackboardEntry, String> {
    match target {
        BoardTarget::Http { base_url, token } => {
            let url = format!("{base_url}/v1/blackboard/post");
            let body = serde_json::to_vec(req).map_err(|e| e.to_string())?;
            let (status_code, bytes) = http_call("POST", &url, token.as_deref(), Some(&body))?;
            if !(200..300).contains(&status_code) {
                return Err(parse_http_error(status_code, &bytes, &url));
            }
            serde_json::from_slice::<BlackboardEntry>(&bytes)
                .map_err(|e| format!("invalid /v1/blackboard/post response: {e}"))
        }
        BoardTarget::Git { .. } => Err(
            "remote HTTP blackboard requires SUSI_BOARD_URL (Git targets store refs/board/items/* only)"
                .to_owned(),
        ),
    }
}

/// Convenience helper for agent runtimes: verify collaborative board access
/// (`check_board`) AND post a validated draft onto the Susi Native Board.
/// Fails closed if the board is unreachable or blocked.
pub fn collaborate_and_post(
    workspace: Option<&Path>,
    draft: &BoardDraft,
    allow_empty_board: bool,
) -> Result<StoredBoardItem, String> {
    let target = resolve_board_target(workspace)?;
    check_board(&target, &draft.agent, allow_empty_board)?;
    post_item(&target, draft, None)
}

/// Sanitize and bound a single-line board title while redacting credentials.
#[must_use]
pub fn sanitize_title(raw: &str) -> String {
    let redacted = crate::susi_config::redact_credentials(raw);
    let collapsed: String = redacted.split_whitespace().collect::<Vec<_>>().join(" ");
    let clipped: String = collapsed.chars().take(MAX_TITLE_CHARS).collect();
    if clipped.is_empty() {
        "agent session update".to_owned()
    } else {
        clipped
    }
}

/// Redact credentials and bound a board item body under `MAX_BODY_BYTES`.
#[must_use]
pub fn sanitize_body(raw: &str) -> String {
    let redacted = crate::susi_config::redact_credentials(raw);
    let max = MAX_BODY_BYTES.saturating_sub(256);
    if redacted.len() <= max {
        return redacted;
    }
    let mut cut = max;
    while cut > 0 && !redacted.is_char_boundary(cut) {
        cut -= 1;
    }
    redacted[..cut].to_owned()
}

/// Specification for starting an isolated, board-collaborative agent session.
#[derive(Debug, Clone)]
pub struct SessionSpec<'a> {
    pub workspace: &'a Path,
    pub explicit_agent: Option<&'a str>,
    pub role_or_task: &'a str,
    pub title: &'a str,
    pub scope: Vec<String>,
    pub require_fresh: bool,
}

/// Active isolated worktree bound to an active claim on the Susi Native Board.
#[derive(Debug)]
pub struct CollaborativeAgentSession {
    pub worktree: crate::worktree_isolation::ProvisionedWorktree,
    pub target: BoardTarget,
    pub agent: String,
    pub host: String,
    pub task: String,
    pub claim_item: u64,
    finished: bool,
}

impl CollaborativeAgentSession {
    /// Fail-closed entry for every agent runtime: resolves agent + board target,
    /// verifies board health (`check_board`), provisions/acquires an exclusive
    /// isolated Git worktree, and posts an initial `claim` on the board.
    pub fn begin(spec: &SessionSpec<'_>) -> Result<Self, String> {
        let agent = resolve_agent(spec.explicit_agent, Some(spec.workspace))?;
        let host = resolve_host(None);
        let target = resolve_board_target(Some(spec.workspace))?;
        check_board(&target, &agent, true)?;

        let mut worktree = crate::worktree_isolation::acquire_isolated_worktree(
            spec.workspace,
            &agent,
            spec.role_or_task,
            spec.require_fresh,
        )?;
        worktree.set_cleanup_on_drop(true);

        let scope_line = if spec.scope.is_empty() {
            "none".to_owned()
        } else {
            spec.scope.join(", ")
        };
        let claim = post_item(
            &target,
            &BoardDraft {
                kind: "claim".to_owned(),
                agent: agent.clone(),
                host: host.clone(),
                title: sanitize_title(spec.title),
                body: sanitize_body(&format!(
                    "role/task: {}\nworktree: {}\nbranch: {}\nscope: {scope_line}",
                    spec.role_or_task,
                    worktree.path().display(),
                    worktree.branch
                )),
                to: None,
                re: None,
                task: Some(spec.role_or_task.to_owned()),
            },
            None,
        )?;

        Ok(Self {
            worktree,
            target,
            agent,
            host,
            task: spec.role_or_task.to_owned(),
            claim_item: claim.n,
            finished: false,
        })
    }

    /// Record a `landed` (if `ok`) or `blocked` item referencing the claim and
    /// release the worktree lock cleanly (preserving any dirty/committed work).
    pub fn finish(&mut self, ok: bool, summary: &str) -> Result<StoredBoardItem, String> {
        let kind = if ok { "landed" } else { "blocked" };
        let title = sanitize_title(&format!("{}: {}", self.task, kind));
        let body = sanitize_body(&format!(
            "worktree: {}\nbranch: {}\noutcome: {}",
            self.worktree.path().display(),
            self.worktree.branch,
            summary
        ));
        let stored = post_item(
            &self.target,
            &BoardDraft {
                kind: kind.to_owned(),
                agent: self.agent.clone(),
                host: self.host.clone(),
                title,
                body,
                to: None,
                re: Some(self.claim_item),
                task: Some(self.task.clone()),
            },
            None,
        )?;
        self.finished = true;
        self.worktree.release()?;
        Ok(stored)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // Test module: unwrap/expect assert ephemeral board/HTTP test fixtures.
    use super::*;
    use std::io::{BufRead, BufReader, Read};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    struct GitBoardFixture {
        root: PathBuf,
        remote: String,
        prev_susi_home: Option<std::ffi::OsString>,
        _env_lock: std::sync::MutexGuard<'static, ()>,
    }

    impl GitBoardFixture {
        fn new(tag: &str) -> Self {
            let env_lock = crate::commit_log::ENV_LOCK
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let prev_susi_home = std::env::var_os("SUSI_HOME");
            let id = SEQ.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir()
                .join(format!("susi-core-board-{tag}-{}-{id}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            let home = root.join("susi-home");
            fs::create_dir_all(&home).unwrap();
            std::env::set_var("SUSI_HOME", &home);
            let remote = root.join("board-remote.git");
            git(
                &root,
                &["init", "--quiet", "--bare", &remote.to_string_lossy()],
            )
            .unwrap();
            Self {
                root,
                remote: remote.to_string_lossy().into_owned(),
                prev_susi_home,
                _env_lock: env_lock,
            }
        }

        fn clone_work(&self, name: &str) -> PathBuf {
            let wt = self.root.join(name);
            fs::create_dir_all(&wt).unwrap();
            git(&wt, &["init", "--quiet", "-b", "wt/test"]).unwrap();
            git(&wt, &["config", "core.hooksPath", "no-hooks"]).unwrap();
            git(&wt, &["remote", "add", "board", &self.remote]).unwrap();
            wt
        }
    }

    impl Drop for GitBoardFixture {
        fn drop(&mut self) {
            match &self.prev_susi_home {
                Some(v) => std::env::set_var("SUSI_HOME", v),
                None => std::env::remove_var("SUSI_HOME"),
            }
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn public_susi_remote_url_is_always_blocked() {
        assert!(is_public_susi_remote_url(
            "https://github.com/intellibitz/susi.git"
        ));
        assert!(is_public_susi_remote_url("git@github.com:intellibitz/susi"));
        assert!(!is_public_susi_remote_url(
            "https://github.com/intellibitz/susi-commercial-apps.git"
        ));
        assert!(!is_public_susi_remote_url("/tmp/bare-board.git"));
    }

    #[test]
    fn git_board_posts_chain_enforce_stops_and_lift_cleanly() {
        let fx = GitBoardFixture::new("chain");
        let agent_a = fx.clone_work("agent-a");
        let agent_b = fx.clone_work("agent-b");

        let ta = BoardTarget::Git {
            work_dir: agent_a.clone(),
            remote: fx.remote.clone(),
        };
        let tb = BoardTarget::Git {
            work_dir: agent_b.clone(),
            remote: fx.remote.clone(),
        };

        let claim = post_item(
            &ta,
            &BoardDraft {
                kind: "claim".into(),
                agent: "antigravity".into(),
                host: "h1".into(),
                title: "claim task 1".into(),
                body: "working".into(),
                ..BoardDraft::default()
            },
            None,
        )
        .unwrap();
        assert_eq!(claim.n, 1);
        assert_eq!(claim.item.prev, "");

        let stop = post_item(
            &ta,
            &BoardDraft {
                kind: "stop".into(),
                agent: "antigravity".into(),
                host: "h1".into(),
                title: "freeze builds".into(),
                body: "fixing schema".into(),
                to: Some("codex".into()),
                ..BoardDraft::default()
            },
            None,
        )
        .unwrap();
        assert_eq!(stop.n, 2);
        assert_eq!(stop.item.prev, claim.sha);

        let blocked = check_board(&tb, "codex", false).expect_err("stop must block codex");
        assert!(blocked.contains("open STOP note"), "{blocked}");

        let _lift = post_item(
            &ta,
            &BoardDraft {
                kind: "lift".into(),
                agent: "antigravity".into(),
                host: "h1".into(),
                title: "resume".into(),
                body: "done".into(),
                re: Some(stop.n),
                ..BoardDraft::default()
            },
            None,
        )
        .unwrap();

        let status = check_board(&tb, "codex", false).expect("lift clears stop");
        assert_eq!(status.items, 3);
        assert_eq!(status.last, 3);
        assert!(status.findings.is_empty());
    }

    #[test]
    fn credentials_are_rejected_from_board_posts() {
        let draft = BoardDraft {
            kind: "note".into(),
            agent: "antigravity".into(),
            host: "h1".into(),
            title: "oops".into(),
            body: "token sk-1234567890abcdef1234567890abcdef".into(),
            ..BoardDraft::default()
        };
        let err = validate_draft(&draft).expect_err("credential must be refused");
        assert!(err.contains("credential"), "{err}");
    }

    #[test]
    fn http_board_target_round_trips_check_and_post() {
        let store = Arc::new(RwLock::new(Vec::<StoredBoardItem>::new()));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let store_srv = Arc::clone(&store);
        let handle = std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut req_line = String::new();
                reader.read_line(&mut req_line).unwrap();
                let mut content_len = 0usize;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" || line == "\n" || line.is_empty() {
                        break;
                    }
                    if let Some((k, v)) = line.split_once(':') {
                        if k.trim().eq_ignore_ascii_case("content-length") {
                            content_len = v.trim().parse().unwrap_or(0);
                        }
                    }
                }
                let mut body = vec![0u8; content_len];
                if content_len > 0 {
                    reader.read_exact(&mut body).unwrap();
                }

                let (status, resp_body) = if req_line.starts_with("POST /v1/board/post ") {
                    let parsed: BoardPostRequest = serde_json::from_slice(&body).unwrap();
                    let mut items = store_srv.write().unwrap();
                    let n = (items.len() as u64) + 1;
                    let prev = items.last().map(|s| s.sha.clone()).unwrap_or_default();
                    let item = BoardItem {
                        schema: SCHEMA.to_owned(),
                        n,
                        kind: parsed.draft.kind,
                        agent: parsed.draft.agent,
                        host: parsed.draft.host,
                        posted_unix: 100,
                        title: parsed.draft.title,
                        body: parsed.draft.body,
                        to: parsed.draft.to,
                        re: parsed.draft.re,
                        task: parsed.draft.task,
                        prev,
                    };
                    let stored = StoredBoardItem {
                        n,
                        sha: format!("sha-{n}"),
                        item,
                    };
                    items.push(stored.clone());
                    ("200 OK", serde_json::to_vec(&stored).unwrap())
                } else {
                    let items = store_srv.read().unwrap();
                    let st = compute_status(&items, Vec::new(), "cloud-agent");
                    ("200 OK", serde_json::to_vec(&st).unwrap())
                };
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    resp_body.len()
                );
                stream.write_all(head.as_bytes()).unwrap();
                stream.write_all(&resp_body).unwrap();
            }
        });

        let target = BoardTarget::Http {
            base_url: format!("http://{addr}"),
            token: Some("test-tok".into()),
        };
        let posted = post_item(
            &target,
            &BoardDraft {
                kind: "claim".into(),
                agent: "cloud-agent".into(),
                host: "cloud-1".into(),
                title: "cloud work".into(),
                body: "running in container".into(),
                ..BoardDraft::default()
            },
            None,
        )
        .unwrap();
        assert_eq!(posted.n, 1);
        let status = check_board(&target, "cloud-agent", false).unwrap();
        assert_eq!(status.items, 1);
        handle.join().unwrap();
    }

    fn init_seeded_repo_with_board(fx: &GitBoardFixture, name: &str) -> PathBuf {
        let repo = fx.root.join(name);
        fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "--quiet", "-b", "main"]).unwrap();
        git(&repo, &["config", "user.email", "test@susi.local"]).unwrap();
        git(&repo, &["config", "user.name", "Susi Test"]).unwrap();
        git(&repo, &["config", "core.hooksPath", "no-hooks"]).unwrap();
        git(&repo, &["config", "susi.board.remote-url", &fx.remote]).unwrap();
        git(&repo, &["config", "susi.agent", "coordinator"]).unwrap();
        fs::write(repo.join("README.md"), "seed\n").unwrap();
        git(&repo, &["add", "README.md"]).unwrap();
        git(&repo, &["commit", "--quiet", "-m", "seed"]).unwrap();
        repo
    }

    #[test]
    fn blackboard_post_refuses_disabling_board_mirror() {
        let fx = GitBoardFixture::new("bb-mirror");
        let repo = init_seeded_repo_with_board(&fx, "repo");

        let refused = post_blackboard_local(
            Some(&repo),
            &BlackboardPostRequest {
                key: "step".into(),
                value: serde_json::json!({ "ok": true }),
                author: None,
                task: Some("t1".into()),
                mirror_to_board: false,
                kind: None,
                title: None,
            },
        )
        .expect_err("disabling mirror_to_board must fail closed");
        assert!(refused.contains("mirror"), "{refused}");

        let mirrored = post_blackboard_local(
            Some(&repo),
            &BlackboardPostRequest {
                key: "step".into(),
                value: serde_json::json!({ "ok": true }),
                author: Some("worker-a".into()),
                task: Some("t1".into()),
                mirror_to_board: true,
                kind: Some("note".into()),
                title: Some("step mirrored".into()),
            },
        )
        .expect("mirrored blackboard post must succeed");
        assert_eq!(mirrored.board_item, Some(1));
    }

    #[test]
    fn collaborative_session_isolates_parallel_agents_and_posts_claim_and_landed() {
        let fx = GitBoardFixture::new("parallel-sessions");
        let repo = init_seeded_repo_with_board(&fx, "repo");

        let paths: Vec<PathBuf> = std::thread::scope(|scope| {
            let repo_ref = &repo;
            let h1 = scope.spawn(move || {
                let mut sess = CollaborativeAgentSession::begin(&SessionSpec {
                    workspace: repo_ref,
                    explicit_agent: Some("worker-a"),
                    role_or_task: "task-a",
                    title: "parallel worker a",
                    scope: vec!["crates/a".into()],
                    require_fresh: true,
                })
                .expect("worker-a session");
                let wt = sess.worktree.path().to_path_buf();
                assert_ne!(wt, *repo_ref);
                sess.finish(true, "completed task-a").expect("finish a");
                wt
            });
            let h2 = scope.spawn(move || {
                let mut sess = CollaborativeAgentSession::begin(&SessionSpec {
                    workspace: repo_ref,
                    explicit_agent: Some("worker-b"),
                    role_or_task: "task-b",
                    title: "parallel worker b",
                    scope: vec!["crates/b".into()],
                    require_fresh: true,
                })
                .expect("worker-b session");
                let wt = sess.worktree.path().to_path_buf();
                assert_ne!(wt, *repo_ref);
                sess.finish(true, "completed task-b").expect("finish b");
                wt
            });
            vec![h1.join().unwrap(), h2.join().unwrap()]
        });

        assert_ne!(
            paths[0], paths[1],
            "parallel agents must get distinct worktrees"
        );
        let (items, warnings) = list_items(&BoardTarget::Git {
            work_dir: repo,
            remote: fx.remote.clone(),
        })
        .expect("list board items");
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(items.len(), 4);
        assert_eq!(items.iter().filter(|i| i.item.kind == "claim").count(), 2);
        assert_eq!(items.iter().filter(|i| i.item.kind == "landed").count(), 2);
    }
}
