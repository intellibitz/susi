//! Path-preserving transport for the separately hosted quorum ledger writer.

use super::{AppendOutcome, CommitRecord};
use crate::susi_error::{EaiError, EaiResult};
use serde_json::{json, Value};
use std::path::Path;

fn path(path: &Path) -> EaiResult<Value> {
    let path = std::path::absolute(path)
        .map_err(|error| EaiError::filesystem(format!("resolve {}: {error}", path.display())))?;
    if let Some(text) = path.to_str() {
        return Ok(json!({"encoding":"utf8", "value":text}));
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Ok(json!({"encoding":"unix_bytes", "value":path.as_os_str().as_bytes()}))
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        Ok(
            json!({"encoding":"windows_wide", "value":path.as_os_str().encode_wide().collect::<Vec<_>>()}),
        )
    }
}

fn call(request: Value, expected: &str) -> EaiResult<Value> {
    let signing_home = susi_paths::SusiDirs::config_dir();
    let home = match signing_home.to_str() {
        Some(home) => json!(home),
        None => path(&signing_home)?,
    };
    let response = crate::plane_bus::fleet_cluster("ledger_v1", json!({"home":home, "request":request}))
        .map_err(|error| EaiError::config(format!(
            "quorum ledger operations are served by Susi Commercial Apps; start `susi-plug-host` with fleet.cluster.v1: {error}"
        )))?;
    if response["result"] == "refused" {
        let kind = response["kind"]
            .as_str()
            .ok_or_else(|| EaiError::protocol("ledger refusal has no kind"))?;
        let message = response["message"]
            .as_str()
            .ok_or_else(|| EaiError::protocol("ledger refusal has no message"))?;
        let error = crate::susi_error::rewrap(kind, message.into());
        if kind != error.kind_name()
            || response["code"] != error.code()
            || response["retryable"] != error.retryable()
        {
            return Err(EaiError::protocol(
                "ledger refusal has inconsistent error metadata",
            ));
        }
        return Err(error);
    }
    if response["result"] != expected {
        return Err(EaiError::protocol(format!(
            "ledger socket expected {expected}, received {}",
            response["result"]
        )));
    }
    Ok(response)
}

pub(super) fn append(target: &Path, record: &CommitRecord) -> EaiResult<()> {
    call(
        json!({"op":"append_at", "path":path(target)?, "record":record}),
        "appended",
    )?;
    Ok(())
}

fn batch(target: &Path, records: &[CommitRecord]) -> EaiResult<Vec<AppendOutcome>> {
    let response = call(
        json!({"op":"append_many_at", "path":path(target)?, "records":records}),
        "batch",
    )?;
    let outcomes = response["outcomes"]
        .as_array()
        .filter(|outcomes| outcomes.len() == records.len())
        .ok_or_else(|| {
            EaiError::protocol("ledger batch response does not match the input count")
        })?;
    outcomes
        .iter()
        .map(|outcome| match outcome.as_str() {
            Some("applied") => Ok(AppendOutcome::Applied),
            Some("skipped") => Ok(AppendOutcome::Skipped),
            Some("refused") => Ok(AppendOutcome::Refused),
            _ => Err(EaiError::protocol(
                "ledger batch returned an unknown outcome",
            )),
        })
        .collect()
}

pub(super) fn append_many(target: &Path, records: &[CommitRecord]) -> Vec<AppendOutcome> {
    // The existing API reports one refusal per input for lock/I/O failure.
    // Transport/protocol failures preserve that contract; EaiError records the
    // concrete serving command or refusal in the ordinary error event sink.
    batch(target, records).unwrap_or_else(|_| vec![AppendOutcome::Refused; records.len()])
}

pub(super) fn compact(ledger: &Path, snapshot: &Path, archive: &Path) -> EaiResult<usize> {
    let response = call(
        json!({"op":"compact_at", "ledger":path(ledger)?, "snapshot":path(snapshot)?, "archive":path(archive)?}),
        "compacted",
    )?;
    response["archived"]
        .as_u64()
        .and_then(|count| usize::try_from(count).ok())
        .ok_or_else(|| EaiError::protocol("ledger compaction returned an invalid archive count"))
}

#[cfg(test)]
#[path = "tests/ledger_socket.rs"]
mod tests;
