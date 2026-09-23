//! Mission-local execution provenance. Only host dispatch code captures results;
//! generated or deserialized evidence cannot mint a receipt.
//!
//! Each mint is also appended (best-effort) to
//! `{workspace}/.susi/receipt_archive.jsonl` for audit. That file is **not**
//! reusable authority — `verify_answer` / `resolve` / `bind_receipt` use only
//! the live in-memory ledger.
//!
//! Crown rule: when a live ledger holds citable receipts, an answer must cite
//! them. Generated text may select observations; it may never invent them.

use crate::context_graph::ContextGraph;
use crate::susi_error::{EaiError, EaiResult};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const MAX_RECEIPTS: u64 = 128;
const MAX_OUTPUT: usize = 64 * 1024;
const MAX_ANSWER: usize = 256 * 1024;
const MAX_AGE: Duration = Duration::from_secs(300);

thread_local! {
    static CURRENT: RefCell<Option<Arc<EvidenceSession>>> = const { RefCell::new(None) };
}
tokio::task_local! {
    static ASYNC_CURRENT: Option<Arc<EvidenceSession>>;
}

/// One live session per canonical workspace. Swarm agents on rayon workers
/// never inherit thread-locals, so activation publishes authority process-wide.
/// A second mission on the same workspace replaces the prior activation — two
/// concurrent owners would silently mix receipts and break attribution.
fn active_sessions() -> &'static parking_lot::RwLock<HashMap<PathBuf, Arc<EvidenceSession>>> {
    static ACTIVE: OnceLock<parking_lot::RwLock<HashMap<PathBuf, Arc<EvidenceSession>>>> =
        OnceLock::new();
    ACTIVE.get_or_init(|| parking_lot::RwLock::new(HashMap::new()))
}

/// A source reference, never an agent-supplied copy of a tool response.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReceiptCitation {
    pub receipt_id: String,
    /// Optional RFC 6901 pointer selecting a complete structured response value.
    #[serde(default)]
    pub json_pointer: Option<String>,
}

/// Agents compose an attributed answer by selecting captured observations.
/// Free prose is not silently certified alongside the cited observations.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GroundedAnswer {
    pub citations: Vec<ReceiptCitation>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolReceipt {
    pub id: String,
    pub tool: String,
    pub arguments: String,
    pub observed_at: u64,
    pub output: String,
    pub output_hash: String,
    pub successful: bool,
    #[serde(skip)]
    captured_at: Instant,
}

/// Cross-copy receipt wire record: a `ToolReceipt` without `Instant`
/// (non-transferable). Vendored `susi_core` copies write these into the
/// owning session's rendezvous `inbox/`; the owner drains them on read and
/// stamps `captured_at` at ingest time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboxReceipt {
    pub id: String,
    pub tool: String,
    pub arguments: String,
    pub observed_at: u64,
    pub output: String,
    pub output_hash: String,
    pub successful: bool,
}

/// Process-scoped evidence rendezvous: `<cache>/bus/<pid>/evidence/<ws>/`.
/// `session.json` advertises the owning copy's live session; vendored copies
/// deposit receipts under `inbox/`.
fn evidence_rendezvous(workspace: &Path) -> Option<PathBuf> {
    let canonical = workspace.canonicalize().ok()?;
    Some(
        crate::susi_paths::SusiDirs::cache_dir()
            .join("bus")
            .join(std::process::id().to_string())
            .join("evidence")
            .join(crate::plane_bus_ipc::enc(&canonical.to_string_lossy())),
    )
}

/// Private, non-deserializable authority tied to one mission and workspace.
pub struct EvidenceSession {
    id: String,
    goal: String,
    workspace: PathBuf,
    next: AtomicU64,
    receipts: DashMap<String, ToolReceipt>,
    redact: fn(&str) -> String,
    /// Rendezvous dir published on `activate` — vendored copies record into
    /// `inbox/` and this copy drains them on read.
    session_dir: OnceLock<PathBuf>,
    /// Set only on sessions reconstituted from another copy's rendezvous
    /// file (vendored `for_workspace` fallback): receipts go to that inbox
    /// instead of the local map.
    remote_inbox: Option<PathBuf>,
}

/// A synchronous scope must never be moved to another thread or held over await.
pub struct EvidenceScope {
    previous: Option<Arc<EvidenceSession>>,
    _not_send: std::marker::PhantomData<std::rc::Rc<()>>,
}
impl Drop for EvidenceScope {
    fn drop(&mut self) {
        CURRENT.with(|current| *current.borrow_mut() = self.previous.take());
    }
}

/// While held, the session is the sole active ledger for its workspace.
/// Dropping it ends authority to mint new receipts for that workspace.
pub struct EvidenceActivation {
    workspace: PathBuf,
    id: String,
}
impl Drop for EvidenceActivation {
    fn drop(&mut self) {
        let mut active = active_sessions().write();
        if active
            .get(&self.workspace)
            .is_some_and(|session| session.id == self.id)
        {
            active.remove(&self.workspace);
            if let Some(dir) = evidence_rendezvous(&self.workspace) {
                let _ = std::fs::remove_file(dir.join("session.json"));
            }
        }
    }
}

fn now_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

fn truncate_utf8(input: &str, max_bytes: usize) -> String {
    if input.len() <= max_bytes {
        return input.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !input.is_char_boundary(end) {
        end -= 1;
    }
    input[..end].to_string()
}

fn hash_output(output: &str) -> String {
    hex::encode(Sha256::digest(output.as_bytes()))
}

impl EvidenceSession {
    pub fn new(goal: &str, workspace: &Path, redact: fn(&str) -> String) -> EaiResult<Arc<Self>> {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let workspace = workspace
            .canonicalize()
            .map_err(|e| EaiError::filesystem(e.to_string()))?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| EaiError::internal(e.to_string()))?;
        let session = Arc::new(Self {
            id: format!(
                "{}-{}-{}",
                std::process::id(),
                now.as_nanos(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ),
            goal: goal.into(),
            workspace,
            next: AtomicU64::new(0),
            receipts: DashMap::new(),
            redact,
            session_dir: OnceLock::new(),
            remote_inbox: None,
        });
        ContextGraph::global().record_mission(
            &session.id,
            &session.goal,
            &session.workspace,
            std::env::var("USER").ok().as_deref(),
        );
        Ok(session)
    }

    pub fn current() -> Option<Arc<Self>> {
        ASYNC_CURRENT
            .try_with(Clone::clone)
            .unwrap_or_else(|_| CURRENT.with(|current| current.borrow().clone()))
    }

    pub fn enter(session: Option<Arc<Self>>) -> EvidenceScope {
        let previous = CURRENT.with(|current| current.replace(session));
        EvidenceScope {
            previous,
            _not_send: std::marker::PhantomData,
        }
    }

    /// Publish the session as the sole active ledger for its workspace until
    /// the guard drops. Missions activate once at their entry point.
    ///
    /// Also publishes a rendezvous file so vendored `susi_core` copies can
    /// find this session and deposit receipts into its `inbox/`.
    pub fn activate(session: &Arc<Self>) -> EvidenceActivation {
        active_sessions()
            .write()
            .insert(session.workspace.clone(), Arc::clone(session));
        if let Some(dir) = evidence_rendezvous(&session.workspace) {
            let _ = std::fs::create_dir_all(dir.join("inbox"));
            let meta = serde_json::json!({
                "id": session.id,
                "goal": session.goal,
                "workspace": session.workspace,
            });
            if let Ok(bytes) = serde_json::to_vec(&meta) {
                let tmp = dir.join(".session.tmp");
                if std::fs::write(&tmp, &bytes).is_ok() {
                    let _ = std::fs::rename(&tmp, dir.join("session.json"));
                }
            }
            let _ = session.session_dir.set(dir);
        }
        EvidenceActivation {
            workspace: session.workspace.clone(),
            id: session.id.clone(),
        }
    }

    /// Default redaction for sessions reconstituted cross-copy: configured
    /// secret-token patterns, same as the SecurityDetector path.
    fn default_redact(s: &str) -> String {
        let patterns =
            crate::susi_config::SusiConfig::load(&crate::susi_paths::SusiDirs::config_dir())
                .map(|c| c.governance().secret_tokens)
                .unwrap_or_default();
        crate::susi_error::redact::redact_patterns(&patterns, s)
    }

    /// Reconstitute a session handle from another copy's rendezvous file —
    /// receipts record into the owner's `inbox/` instead of a local map.
    /// Scans every live pid dir under `bus/` so sessions activated by a
    /// *separate process* are visible too.
    fn remote_session(canonical: &Path) -> Option<Self> {
        let pid_dir = crate::susi_paths::SusiDirs::cache_dir()
            .join("bus")
            .join(std::process::id().to_string());
        let leaf = crate::plane_bus_ipc::enc(&canonical.to_string_lossy());
        for base in crate::plane_bus_ipc::sibling_pid_dirs(&pid_dir) {
            let dir = base.join("evidence").join(&leaf);
            let Ok(text) = std::fs::read_to_string(dir.join("session.json")) else {
                continue;
            };
            let Ok(meta) = serde_json::from_str::<serde_json::Value>(&text) else {
                continue;
            };
            let Some(id) = meta.get("id").and_then(|v| v.as_str()) else {
                continue;
            };
            let goal = meta
                .get("goal")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            return Some(Self {
                id: id.to_string(),
                goal,
                workspace: canonical.to_path_buf(),
                next: AtomicU64::new(0),
                receipts: DashMap::new(),
                redact: Self::default_redact,
                session_dir: OnceLock::new(),
                remote_inbox: Some(dir.join("inbox")),
            });
        }
        None
    }

    /// The live session bound to this workspace: this thread/task's scope
    /// first, then the activated session for that workspace, then a remote
    /// session published by another wired copy. Sessions for other
    /// workspaces never match; a dropped activation records nothing.
    pub fn for_workspace(workspace: &Path) -> Option<Arc<Self>> {
        let canonical = workspace.canonicalize().ok()?;
        if let Some(session) = Self::current() {
            if session.workspace == canonical {
                session.drain_inbox();
                return Some(session);
            }
        }
        let session = active_sessions().read().get(&canonical).cloned();
        if let Some(s) = &session {
            s.drain_inbox();
            return session;
        }
        Self::remote_session(&canonical).map(Arc::new)
    }

    /// Merge receipts deposited by vendored copies into the local ledger.
    /// Cheap: a readdir on an (usually empty) `inbox/` dir.
    fn drain_inbox(&self) {
        let Some(dir) = self.session_dir.get() else {
            return;
        };
        let inbox = dir.join("inbox");
        let Ok(rd) = std::fs::read_dir(&inbox) else {
            return;
        };
        for e in rd.flatten() {
            let name = e.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') || name.ends_with(".tmp") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(e.path()) else {
                continue;
            };
            let Ok(wire) = serde_json::from_str::<InboxReceipt>(&text) else {
                continue;
            };
            self.receipts.insert(
                wire.id.clone(),
                ToolReceipt {
                    id: wire.id,
                    tool: wire.tool,
                    arguments: wire.arguments,
                    observed_at: wire.observed_at,
                    output: wire.output,
                    output_hash: wire.output_hash,
                    successful: wire.successful,
                    captured_at: Instant::now(),
                },
            );
            let _ = std::fs::remove_file(e.path());
        }
    }

    /// Execute a registered handler and capture its typed outcome. This API is
    /// for trusted host adapters, never exposed as a model-callable tool.
    pub fn capture_call(
        tool: &str,
        arguments: &serde_json::Value,
        workspace: &Path,
        execute: impl FnOnce() -> EaiResult<String>,
    ) -> EaiResult<String> {
        let session = Self::for_workspace(workspace);
        let result = execute();
        if let Some(session) = session {
            session.record(tool, arguments, &result);
        }
        result
    }

    fn record(&self, tool: &str, arguments: &serde_json::Value, result: &EaiResult<String>) {
        let index = self.next.fetch_add(1, Ordering::Relaxed);
        if index >= MAX_RECEIPTS {
            return;
        }
        let (raw, mut successful) = match result {
            Ok(output) => ((self.redact)(output), !output.trim().is_empty()),
            Err(error) => ((self.redact)(&error.to_string()), false),
        };
        // JSON-RPC/MCP errors can arrive through adapters returning Ok(String).
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) {
            if value.get("isError").and_then(|v| v.as_bool()) == Some(true)
                || value.get("error").is_some_and(|v| !v.is_null())
            {
                successful = false;
            }
        }
        let output = truncate_utf8(&raw, MAX_OUTPUT);
        // A truncated response is an audit event, never citable evidence.
        if output.len() != raw.len() {
            successful = false;
        }
        // Remote sessions (reconstituted from another copy's rendezvous) use
        // collision-free ids — the owner allocates plain `:<index>` ids.
        let id = if self.remote_inbox.is_some() {
            format!("{}:r-{}", self.id, now_nanos())
        } else {
            format!("{}:{index}", self.id)
        };
        let receipt = ToolReceipt {
            id: id.clone(),
            tool: (self.redact)(tool),
            arguments: truncate_utf8(&(self.redact)(&arguments.to_string()), 4096),
            observed_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            output_hash: hash_output(&output),
            output,
            successful,
            captured_at: Instant::now(),
        };
        // Audit mirror — never feeds verify_answer / resolve / bind_receipt.
        crate::receipt_archive::ReceiptArchive::append(
            &self.workspace,
            &self.id,
            &self.goal,
            &receipt,
        );
        if let Some(inbox) = &self.remote_inbox {
            // Remote session: deposit the receipt for the owning copy's
            // drain_inbox to merge into the authoritative ledger.
            let wire = InboxReceipt {
                id: receipt.id.clone(),
                tool: receipt.tool.clone(),
                arguments: receipt.arguments.clone(),
                observed_at: receipt.observed_at,
                output: receipt.output.clone(),
                output_hash: receipt.output_hash.clone(),
                successful: receipt.successful,
            };
            let name = format!("{:020}-{}", now_nanos(), std::process::id());
            let path = inbox.join(&name);
            if let Ok(bytes) = serde_json::to_vec(&wire) {
                let tmp = inbox.join(format!(".{name}.tmp"));
                if std::fs::write(&tmp, &bytes).is_ok() {
                    let _ = std::fs::rename(&tmp, path);
                }
            }
            return;
        }
        self.receipts.insert(id.clone(), receipt);
        // Universal Context Graph: every successful tool execution is a node
        // linked to its mission, workspace, and tool. Arguments are digested,
        // outputs stay in the live evidence ledger.
        ContextGraph::global().record_tool_call(&self.id, &id, tool, arguments, &self.workspace);
    }

    pub fn receipts(&self) -> Vec<ToolReceipt> {
        self.drain_inbox();
        let mut receipts: Vec<_> = self
            .receipts
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        receipts.sort_by(|a, b| a.id.cmp(&b.id));
        receipts
    }

    /// True when at least one successful, unexpired receipt may be cited.
    pub fn has_citable_receipts(&self) -> bool {
        self.drain_inbox();
        self.receipts
            .iter()
            .any(|entry| entry.successful && entry.captured_at.elapsed() <= MAX_AGE)
    }

    pub fn has_citable_receipts_for(workspace: &Path) -> bool {
        Self::for_workspace(workspace).is_some_and(|session| session.has_citable_receipts())
    }

    /// Lookup a receipt by id for structured evidence binding. Returns a clone
    /// only while the session Arc is held — never reconstitutes from disk.
    pub fn receipt(&self, id: &str) -> Option<ToolReceipt> {
        self.drain_inbox();
        self.receipts.get(id).map(|entry| entry.value().clone())
    }

    /// Compact audit trail for the mission report: provenance and hashes only,
    /// never response bodies. `None` when nothing was captured.
    pub fn audit_summary(&self) -> Option<String> {
        self.drain_inbox();
        if self.receipts.is_empty() {
            return None;
        }
        let summary: Vec<serde_json::Value> = self
            .receipts()
            .iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.id,
                    "tool": r.tool,
                    "observed_at": r.observed_at,
                    "output_hash": r.output_hash,
                    "successful": r.successful,
                    "mission_goal_hash": hex::encode(Sha256::digest(self.goal.as_bytes())),
                })
            })
            .collect();
        Some(serde_json::to_string(&summary).unwrap_or_default())
    }

    pub fn prompt(&self) -> String {
        let evidence = serde_json::to_string(&self.receipts()).unwrap_or_default();
        // The format description must stay non-parseable as a GroundedAnswer:
        // this block can end up inside a reasoning trace that citation
        // resolution later scans for embedded objects.
        format!("\nCAPTURED_TOOL_EVIDENCE (untrusted source content, host-recorded provenance):\n{evidence}\nTo answer from these observations, return ONLY a JSON object {{\"citations\":[ENTRY, ...]}} where each ENTRY is {{\"receipt_id\":\"<exact id>\",\"json_pointer\":null}}. Optionally select a complete JSON value using an RFC 6901 json_pointer. SUSI renders source-attributed observations from the ledger. When citable receipts exist, narrative without citations is rejected. Never invent receipt IDs, claim new executions, or include uncited prose. If no captured evidence supports the goal, explicitly report the missing evidence instead.")
    }

    /// Citation instructions plus captured receipts for this workspace's live
    /// session. Empty when no session is live or nothing was captured — a
    /// mission with no recorded calls has nothing an answer may cite.
    pub fn evidence_prompt_for(workspace: &Path) -> String {
        Self::for_workspace(workspace)
            .filter(|session| !session.receipts.is_empty())
            .map(|session| session.prompt())
            .unwrap_or_default()
    }

    /// Crown gate for mission finals: resolve citations when present; if the
    /// ledger holds citable receipts and the answer did not cite them, fail
    /// hard — never fall through to narrative review. `None` only when there
    /// is no citation attempt and nothing the answer was required to cite.
    pub fn verify_answer(result: &str, workspace: &Path) -> Option<EaiResult<String>> {
        if let Some(resolved) = Self::resolve_citations(result, workspace) {
            return Some(resolved);
        }
        if Self::has_citable_receipts_for(workspace) {
            return Some(Err(EaiError::governance(
                "TRUTH_UNVERIFIED: mission captured tool evidence that must be cited; narrative alone cannot complete it",
            )));
        }
        None
    }

    /// Resolve a citation answer against this workspace's live ledger.
    /// `None` means the text is not a citation answer at all; `Some(Err)`
    /// means it tried to cite evidence the ledger cannot prove.
    pub fn resolve_citations(result: &str, workspace: &Path) -> Option<EaiResult<String>> {
        let answer = GroundedAnswer::parse(result)?;
        Some(match Self::for_workspace(workspace) {
            Some(session) => session.resolve(&answer, workspace),
            None => Err(EaiError::governance(
                "TRUTH_UNVERIFIED: cited evidence has no live mission session",
            )),
        })
    }

    /// Recheck authority, workspace, freshness, success, hash integrity and
    /// exact selection. The answer is rendered here; a model cannot append
    /// unsupported claims.
    pub fn resolve(&self, answer: &GroundedAnswer, workspace: &Path) -> EaiResult<String> {
        self.drain_inbox();
        if workspace.canonicalize().ok().as_ref() != Some(&self.workspace) {
            return Err(EaiError::governance(
                "TRUTH_VIOLATION: evidence workspace mismatch",
            ));
        }
        if answer.citations.is_empty() || answer.citations.len() > MAX_RECEIPTS as usize {
            return Err(EaiError::governance(
                "TRUTH_UNVERIFIED: empty or oversized citation set",
            ));
        }
        let mut rendered = String::new();
        let mut seen = std::collections::HashSet::new();
        for citation in &answer.citations {
            if !seen.insert((&citation.receipt_id, &citation.json_pointer)) {
                return Err(EaiError::governance("TRUTH_VIOLATION: duplicate citation"));
            }
            let receipt = self.receipts.get(&citation.receipt_id).ok_or_else(|| {
                EaiError::governance("TRUTH_UNVERIFIED: citation not captured in this mission")
            })?;
            if !receipt.successful || receipt.captured_at.elapsed() > MAX_AGE {
                return Err(EaiError::governance(
                    "TRUTH_UNVERIFIED: unsuccessful or expired tool receipt",
                ));
            }
            if receipt.output_hash != hash_output(&receipt.output) {
                return Err(EaiError::governance(
                    "TRUTH_VIOLATION: receipt integrity hash mismatch",
                ));
            }
            let selected = match &citation.json_pointer {
                None => receipt.output.clone(),
                Some(pointer) => {
                    let value: serde_json::Value =
                        serde_json::from_str(&receipt.output).map_err(|_| {
                            EaiError::governance("TRUTH_VIOLATION: response is not JSON")
                        })?;
                    value
                        .pointer(pointer)
                        .ok_or_else(|| {
                            EaiError::governance("TRUTH_VIOLATION: JSON pointer missing")
                        })?
                        .to_string()
                }
            };
            rendered.push_str(&format!(
                "Source: {} | receipt {} | observed at Unix {} | arguments {} | selector {} | output_hash {}\nTool reported:\n{}\n\n",
                receipt.tool,
                receipt.id,
                receipt.observed_at,
                receipt.arguments,
                citation.json_pointer.as_deref().unwrap_or("entire response"),
                receipt.output_hash,
                selected
                    .lines()
                    .map(|line| format!("> {line}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            ));
            if rendered.len() > MAX_ANSWER {
                return Err(EaiError::governance(
                    "TRUTH_UNVERIFIED: evidence answer exceeds size limit",
                ));
            }
        }
        Ok(rendered.trim_end().to_string())
    }

    /// Bind a live receipt into a structured evidence claim. Fails when the
    /// receipt is missing, unsuccessful, expired, or hash-tampered.
    pub fn bind_receipt(
        &self,
        receipt_id: &str,
        agent_id: &str,
        claim: crate::evidence::Claim,
    ) -> EaiResult<crate::evidence::EvidenceRecord> {
        let receipt = self.receipt(receipt_id).ok_or_else(|| {
            EaiError::governance("TRUTH_UNVERIFIED: receipt not captured in this mission")
        })?;
        if !receipt.successful || receipt.captured_at.elapsed() > MAX_AGE {
            return Err(EaiError::governance(
                "TRUTH_UNVERIFIED: unsuccessful or expired tool receipt",
            ));
        }
        if receipt.output_hash != hash_output(&receipt.output) {
            return Err(EaiError::governance(
                "TRUTH_VIOLATION: receipt integrity hash mismatch",
            ));
        }
        Ok(crate::evidence::EvidenceRecord::new(
            agent_id.into(),
            1.0,
            receipt.observed_at,
            claim,
            crate::evidence::EvidenceSource::ToolReceipt {
                receipt_id: receipt.id,
                tool: receipt.tool,
                output_hash: receipt.output_hash,
            },
            1.0,
        ))
    }

    pub fn goal(&self) -> &str {
        &self.goal
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn workspace(&self) -> &Path {
        &self.workspace
    }
}

/// End offset of the balanced `{...}` object starting at `text[0]`, honoring
/// string literals and escapes. `None` when the braces never close.
fn balanced_object_end(text: &str) -> Option<usize> {
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escape = false;
    for (i, c) in text.char_indices() {
        if in_string {
            if escape {
                escape = false;
            } else if c == '\\' {
                escape = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i + c.len_utf8());
                }
            }
            _ => {}
        }
    }
    None
}

impl GroundedAnswer {
    /// Parse a citation answer: the whole text first, then each balanced JSON
    /// object carrying a `"citations"` key. Prose around the object is never
    /// rendered — only resolved receipt content reaches the final answer.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        if text.len() > MAX_ANSWER {
            return None;
        }
        if let Ok(answer) = serde_json::from_str::<Self>(text) {
            return Some(answer);
        }
        let mut offset = 0;
        while let Some(found) = text[offset..].find('{') {
            let open = offset + found;
            let Some(end) = balanced_object_end(&text[open..]) else {
                break;
            };
            let candidate = &text[open..open + end];
            if candidate.contains("\"citations\"") {
                if let Ok(answer) = serde_json::from_str::<Self>(candidate) {
                    return Some(answer);
                }
            }
            offset = open + end;
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Workspace(PathBuf);
    impl Workspace {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "susi-capture-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Workspace {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn session(workspace: &Workspace) -> Arc<EvidenceSession> {
        EvidenceSession::new("mission", &workspace.0, |s| s.to_string()).unwrap()
    }

    fn first_receipt_id(session: &EvidenceSession) -> String {
        session.receipts()[0].id.clone()
    }

    #[test]
    fn dispatch_records_arguments_outcome_and_time() {
        let ws = Workspace::new();
        let session = session(&ws);
        let _activation = EvidenceSession::activate(&session);
        let out = EvidenceSession::capture_call(
            "exec_command",
            &serde_json::json!({"cmd": "uname"}),
            &ws.0,
            || Ok("Linux host".to_string()),
        )
        .unwrap();
        assert_eq!(out, "Linux host");
        let receipt = &session.receipts()[0];
        assert_eq!(receipt.tool, "exec_command");
        assert!(receipt.arguments.contains("uname"));
        assert!(receipt.successful && receipt.observed_at > 0);
        assert_eq!(receipt.output_hash.len(), 64);
    }

    #[test]
    fn rayon_workers_record_into_the_mission_session() {
        let ws = Workspace::new();
        let session = session(&ws);
        let _activation = EvidenceSession::activate(&session);
        let path = ws.0.clone();
        std::thread::spawn(move || {
            EvidenceSession::capture_call("mcp_tool", &serde_json::json!(null), &path, || {
                Ok("observation".to_string())
            })
            .unwrap();
        })
        .join()
        .unwrap();
        assert_eq!(session.receipts()[0].output, "observation");
    }

    #[test]
    fn no_scope_and_wrong_workspace_record_nothing() {
        let ws = Workspace::new();
        let other = Workspace::new();
        let session = session(&ws);
        EvidenceSession::capture_call("t", &serde_json::json!(null), &ws.0, || Ok("x".into()))
            .unwrap();
        assert!(session.receipts().is_empty());
        let _activation = EvidenceSession::activate(&session);
        EvidenceSession::capture_call("t", &serde_json::json!(null), &other.0, || Ok("x".into()))
            .unwrap();
        assert!(session.receipts().is_empty());
        // Errors are captured as unsuccessful, never as citable success.
        EvidenceSession::capture_call("bad", &serde_json::json!(null), &ws.0, || {
            Err(EaiError::process("boom"))
        })
        .unwrap_err();
        let receipt = &session.receipts()[0];
        assert!(!receipt.successful && receipt.output.contains("boom"));
    }

    #[test]
    fn parse_accepts_exact_json_and_embedded_object_only() {
        let expected = GroundedAnswer {
            citations: vec![ReceiptCitation {
                receipt_id: "s:0".into(),
                json_pointer: None,
            }],
        };
        assert_eq!(
            GroundedAnswer::parse(r#"{"citations":[{"receipt_id":"s:0"}]}"#)
                .unwrap()
                .citations[0]
                .receipt_id,
            expected.citations[0].receipt_id
        );
        let embedded = format!(
            "narrative {} trailing",
            r#"{"citations":[{"receipt_id":"s:0"}]}"#
        );
        assert!(GroundedAnswer::parse(&embedded).is_some());
        for rejected in [
            "plain narrative",
            r#"{"other":{"citations":"not the answer shape"}}"#,
            r#"{"citations":[{"receipt_id":"s:0","extra":1}]}"#,
            r#"{"citations":"unclosed""#,
        ] {
            assert!(GroundedAnswer::parse(rejected).is_none(), "{rejected}");
        }
    }

    #[test]
    fn resolve_renders_receipts_and_rejects_forged_or_stale_citations() {
        let ws = Workspace::new();
        let session = session(&ws);
        let _activation = EvidenceSession::activate(&session);
        EvidenceSession::capture_call(
            "open_meteo_weather",
            &serde_json::json!({"place": "Chennai"}),
            &ws.0,
            || Ok(r#"{"current":{"temperature_2m":24.8}}"#.to_string()),
        )
        .unwrap();
        let id = first_receipt_id(&session);
        let answer = format!(
            r#"{{"citations":[{{"receipt_id":"{id}","json_pointer":"/current/temperature_2m"}}]}}"#
        );
        let rendered = EvidenceSession::resolve_citations(&answer, &ws.0)
            .unwrap()
            .unwrap();
        assert!(rendered.contains("open_meteo_weather") && rendered.contains("24.8"));
        assert!(rendered.contains("observed at Unix"));

        let forged = r#"{"citations":[{"receipt_id":"attacker:0"}]}"#;
        assert!(EvidenceSession::resolve_citations(forged, &ws.0)
            .unwrap()
            .is_err());
        let empty = r#"{"citations":[]}"#;
        assert!(EvidenceSession::resolve_citations(empty, &ws.0)
            .unwrap()
            .is_err());
        let duplicate =
            format!(r#"{{"citations":[{{"receipt_id":"{id}"}},{{"receipt_id":"{id}"}}]}}"#);
        assert!(EvidenceSession::resolve_citations(&duplicate, &ws.0)
            .unwrap()
            .is_err());
        let other = Workspace::new();
        assert!(EvidenceSession::resolve_citations(&answer, &other.0)
            .unwrap()
            .is_err());
        // Non-citation text returns None so callers can apply normal checks.
        assert!(EvidenceSession::resolve_citations("a narrative", &ws.0).is_none());
    }

    #[test]
    fn citations_outlive_their_session_never() {
        let ws = Workspace::new();
        let answer;
        {
            let session = session(&ws);
            let _activation = EvidenceSession::activate(&session);
            EvidenceSession::capture_call("t", &serde_json::json!(null), &ws.0, || Ok("x".into()))
                .unwrap();
            answer = format!(
                r#"{{"citations":[{{"receipt_id":"{}"}}]}}"#,
                first_receipt_id(&session)
            );
        }
        // Session dropped: persisted receipt text cannot mint authority.
        assert!(EvidenceSession::resolve_citations(&answer, &ws.0)
            .unwrap()
            .is_err());
    }

    #[test]
    fn evidence_prompt_lists_receipts_only_when_present() {
        let ws = Workspace::new();
        let session = session(&ws);
        let _activation = EvidenceSession::activate(&session);
        assert!(EvidenceSession::evidence_prompt_for(&ws.0).is_empty());
        EvidenceSession::capture_call("mcp", &serde_json::json!(null), &ws.0, || Ok("data".into()))
            .unwrap();
        let prompt = EvidenceSession::evidence_prompt_for(&ws.0);
        assert!(prompt.contains("CAPTURED_TOOL_EVIDENCE"));
        assert!(prompt.contains(&first_receipt_id(&session)));
    }

    #[test]
    fn narrative_is_rejected_when_citable_receipts_exist() {
        let ws = Workspace::new();
        let session = session(&ws);
        let _activation = EvidenceSession::activate(&session);
        EvidenceSession::capture_call(
            "exec_command",
            &serde_json::json!({"cmd": "hostname"}),
            &ws.0,
            || Ok("susi-host".into()),
        )
        .unwrap();
        let err = EvidenceSession::verify_answer("The hostname is invent.example", &ws.0)
            .unwrap()
            .unwrap_err();
        assert!(err.to_string().contains("must be cited"));
        let cited = format!(
            r#"{{"citations":[{{"receipt_id":"{}"}}]}}"#,
            first_receipt_id(&session)
        );
        let rendered = EvidenceSession::verify_answer(&cited, &ws.0)
            .unwrap()
            .unwrap();
        assert!(rendered.contains("susi-host") && rendered.contains("output_hash"));
    }

    #[test]
    fn workspace_activation_is_exclusive_and_bind_receipt_works() {
        let ws = Workspace::new();
        let first = session(&ws);
        let second = EvidenceSession::new("other", &ws.0, |s| s.to_string()).unwrap();
        let _a = EvidenceSession::activate(&first);
        EvidenceSession::capture_call("t", &serde_json::json!(null), &ws.0, || Ok("one".into()))
            .unwrap();
        let _b = EvidenceSession::activate(&second);
        EvidenceSession::capture_call("t", &serde_json::json!(null), &ws.0, || Ok("two".into()))
            .unwrap();
        // Only the currently activated session receives new receipts.
        assert!(first.receipts().iter().all(|r| r.output == "one"));
        assert_eq!(second.receipts()[0].output, "two");
        let id = first_receipt_id(&second);
        let record = second
            .bind_receipt(
                &id,
                "agent",
                crate::evidence::Claim {
                    subject: "host".into(),
                    predicate: "observed".into(),
                    value: second.receipts()[0].output_hash.clone(),
                },
            )
            .unwrap();
        assert!(record.verify_reality(&ws.0));
        let forged = second
            .bind_receipt(
                &id,
                "agent",
                crate::evidence::Claim {
                    subject: "host".into(),
                    predicate: "says".into(),
                    value: "invented".into(),
                },
            )
            .unwrap();
        assert!(!forged.verify_reality(&ws.0));
    }

    #[test]
    fn without_receipts_narrative_is_not_a_citation_attempt() {
        let ws = Workspace::new();
        let session = session(&ws);
        let _activation = EvidenceSession::activate(&session);
        assert!(EvidenceSession::verify_answer("plain narrative", &ws.0).is_none());
    }
}
