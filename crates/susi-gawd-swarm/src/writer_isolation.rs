//! Caller-owned workspace isolation state over the standard MCP tool socket.
//! Operations execute once; a lost mutation response never permits replay.

use crate::susi_error::{EaiError, EaiResult};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct Sig {
    len: u64,
    hash: [u8; 32],
}

/// A worker's private directory and the snapshot its writes are diffed against.
#[derive(Debug, Serialize, Deserialize)]
pub struct WorkerScope {
    pub owner: String,
    #[serde(with = "susi_config::native_path")]
    pub dir: PathBuf,
    manifest: BTreeMap<String, Sig>,
}

/// Applied changes or overlapping divergent paths requiring review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FoldOutcome {
    Clean {
        wrote: Vec<String>,
        deleted: Vec<String>,
    },
    Conflict {
        paths: BTreeSet<String>,
    },
}

/// Isolation state belongs to this caller, independent of provider lifetime.
#[derive(Serialize, Deserialize)]
#[serde(transparent)]
pub struct WriterIsolation(Value);

fn invalid_reply() -> EaiError {
    EaiError::protocol("invalid swarm storage MCP result")
}

fn call<T: DeserializeOwned>(operation: &str, config: Value) -> EaiResult<T> {
    // A deleted caller cwd still works; do not resolve it to an absolute name.
    let reply = susi_config::mcp_session::call_result(
        "swarm_storage_v1",
        &json!({"operation": operation, "config": config}),
        Some(Path::new(".")),
    )?;
    decode_result(reply)
}

fn decode_result<T: DeserializeOwned>(reply: Value) -> EaiResult<T> {
    let blocks = reply["content"]
        .as_array()
        .filter(|blocks| blocks.len() == 1 && blocks[0]["type"] == "text")
        .ok_or_else(invalid_reply)?;
    let text = blocks[0]["text"].as_str().ok_or_else(invalid_reply)?;
    if reply["isError"] == true {
        let value: Value = serde_json::from_str(text).map_err(|_| invalid_reply())?;
        let kind = value["kind"].as_str().ok_or_else(invalid_reply)?;
        let message = value["message"].as_str().ok_or_else(invalid_reply)?;
        let error = crate::susi_error::rewrap(kind, message.to_owned());
        if value["schema"] != "susi-swarm-storage-error-v1"
            || value["kind"] != error.kind_name()
            || value["code"] != error.code()
            || value["retryable"] != error.retryable()
        {
            return Err(invalid_reply());
        }
        return Err(error);
    }
    if reply
        .get("isError")
        .is_some_and(|flag| flag != &Value::Bool(false))
    {
        return Err(invalid_reply());
    }
    serde_json::from_str(text).map_err(|_| invalid_reply())
}

fn path(path: &Path) -> EaiResult<Value> {
    susi_paths::native_path::encode(path).map_err(EaiError::config)
}

/// Content revision of payload files, excluding orchestration state.
pub fn revision(root: &Path) -> EaiResult<String> {
    call("revision", json!({"workspace": path(root)?}))
}

impl WriterIsolation {
    pub fn new(base: &Path) -> EaiResult<Self> {
        let scopes_root = susi_paths::workspace_state_dir(base).join("dag-scopes");
        call(
            "open",
            json!({"workspace": path(base)?, "scopes_root": path(&scopes_root)?}),
        )
    }

    /// Retire the previous generation without reusing a delayed worker's scope.
    pub fn retire_stale_scopes(&self, generation: &str) -> EaiResult<Vec<PathBuf>> {
        let paths: Vec<Value> = call("retire", json!({"state": self, "generation": generation}))?;
        paths
            .iter()
            .map(|path| susi_paths::native_path::decode(path).map_err(EaiError::config))
            .collect()
    }

    /// Create a synced private workspace and its snapshot manifest.
    pub fn scope(&self, owner: &str) -> EaiResult<WorkerScope> {
        call("scope", json!({"state": self, "owner": owner}))
    }

    /// Apply nonconflicting changes and report divergent overlapping paths.
    pub fn fold(&self, scope: &WorkerScope) -> EaiResult<FoldOutcome> {
        call("fold", json!({"state": self, "scope": scope}))
    }

    /// Best-effort cleanup, preserving the native caller contract.
    pub fn cleanup(&self, scope: &WorkerScope) {
        let _ = call::<Value>("cleanup", json!({"state": self, "scope": scope}));
    }
}

#[cfg(test)]
#[path = "tests/writer_isolation.rs"]
mod tests;
