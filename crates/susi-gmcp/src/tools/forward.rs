//! Shared plumbing for the socket tools that forward a CLI command to the
//! daemon's composition root over the plane bus (zero-UI plan B4): argument
//! type checks, the forwarded payload, and the policy-confined workspace a
//! caller may name for a command the CLI runs in its working directory.

use std::path::{Path, PathBuf};

use crate::susi_core::mac_policy::MacPolicy;
use crate::susi_error::{EaiError, EaiResult};
use serde_json::{Map, Value};

/// The JSON type a forwarded argument must have.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    Str,
    Bool,
    U64,
    Object,
    StrArray,
}

impl Kind {
    fn admits(self, value: &Value) -> bool {
        match self {
            Self::Str => value.is_string(),
            Self::Bool => value.is_boolean(),
            Self::U64 => value.is_u64(),
            Self::Object => value.is_object(),
            Self::StrArray => value
                .as_array()
                .is_some_and(|items| items.iter().all(Value::is_string)),
        }
    }

    fn describe(self) -> &'static str {
        match self {
            Self::Str => "a string",
            Self::Bool => "a boolean",
            Self::U64 => "a non-negative integer",
            Self::Object => "an object",
            Self::StrArray => "an array of strings",
        }
    }
}

/// `{action, <each listed key the caller set>, ..extra}`, refusing an
/// argument of the wrong JSON type before anything is sent.
pub(super) fn payload(
    action: &str,
    arg: &Value,
    keys: &[(&str, Kind)],
    extra: Value,
) -> EaiResult<Value> {
    let mut out = Map::new();
    out.insert("action".into(), Value::from(action));
    for (key, kind) in keys {
        match arg.get(*key) {
            None | Some(Value::Null) => {}
            Some(value) if kind.admits(value) => {
                out.insert((*key).to_string(), value.clone());
            }
            Some(_) => {
                return Err(EaiError::config(format!(
                    "{key} must be {}",
                    kind.describe()
                )));
            }
        }
    }
    if let Value::Object(extra) = extra {
        out.extend(extra);
    }
    Ok(Value::Object(out))
}

/// Send `payload` through a `daemon.*` facade; a refusal is an error and
/// the reply comes back as pretty JSON.
pub(super) fn send(call: fn(Value) -> Result<Value, String>, payload: Value) -> EaiResult<String> {
    let reply = call(payload).map_err(EaiError::config)?;
    serde_json::to_string_pretty(&reply)
        .map_err(|error| EaiError::internal(format!("encode tool result: {error}")))
}

/// [`payload`] then [`send`].
pub(super) fn forward(
    call: fn(Value) -> Result<Value, String>,
    action: &str,
    arg: &Value,
    keys: &[(&str, Kind)],
    extra: Value,
) -> EaiResult<String> {
    send(call, payload(action, arg, keys, extra)?)
}

/// The workspace a tool acts on: the server workspace, or the absolute
/// directory the caller names in `key` — admitted only when the MAC policy
/// grants `filesystem.write` (`write`) or `filesystem.read` on it
/// ([`MacPolicy::confine_workspace`]). The CLI runs these commands in its
/// working directory; this argument is the socket's equivalent.
pub(super) fn policy_workspace(
    arg: &Value,
    key: &str,
    server: &Path,
    write: bool,
) -> EaiResult<PathBuf> {
    match arg.get(key) {
        None | Some(Value::Null) => Ok(server.to_path_buf()),
        Some(Value::String(path)) => MacPolicy::global().confine_workspace("susi", path, write),
        Some(_) => Err(EaiError::config(format!("{key} must be a string"))),
    }
}

/// The file paths a caller names in `keys` (`(key, write)`), each resolved
/// through the MAC policy ([`MacPolicy::confine_path`]): absolute, and
/// covered by a `filesystem.write` (`write`) or `filesystem.read` grant.
/// Returns `{key: resolved path}` for the keys the caller set, to send in
/// place of the caller's text; an unset key stays unset (the daemon's
/// default, which sits under the confined workspace or the substrate home).
pub(super) fn policy_paths(arg: &Value, keys: &[(&str, bool)]) -> EaiResult<Value> {
    let mut out = Map::new();
    for (key, write) in keys {
        match arg.get(*key) {
            None | Some(Value::Null) => {}
            Some(Value::String(path)) => {
                let resolved = MacPolicy::global()
                    .confine_path("susi", path, *write)
                    .map_err(|error| EaiError::config(format!("{key}: {error}")))?;
                out.insert(
                    (*key).to_string(),
                    Value::from(resolved.display().to_string()),
                );
            }
            Some(_) => return Err(EaiError::config(format!("{key} must be a string"))),
        }
    }
    Ok(Value::Object(out))
}

/// Merge two `extra` objects (`more` wins on a shared key).
pub(super) fn merged(mut base: Value, more: Value) -> Value {
    if let (Value::Object(base), Value::Object(more)) = (&mut base, more) {
        base.extend(more);
    }
    base
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn caller_file_paths_are_absolute_resolved_and_typed() {
        let keys = [("out", true), ("file", false)];
        assert_eq!(policy_paths(&json!({}), &keys).unwrap(), json!({}));
        let refused = policy_paths(&json!({"out": "rel.json"}), &keys).unwrap_err();
        assert!(refused.to_string().contains("absolute"), "{refused}");
        assert!(policy_paths(&json!({"file": 3}), &keys).is_err());
        let dir = std::env::temp_dir().join(format!("susi-forward-path-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("new").join("out.json");
        let resolved = policy_paths(&json!({"out": out.display().to_string()}), &keys).unwrap();
        let text = resolved["out"].as_str().unwrap();
        assert!(Path::new(text).ends_with(Path::new("new").join("out.json")));
        assert_eq!(
            merged(json!({"a": 1, "b": 1}), json!({"b": 2})),
            json!({"a": 1, "b": 2})
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn payload_copies_set_arguments_and_refuses_wrong_types() {
        let keys = [
            ("name", Kind::Str),
            ("live", Kind::Bool),
            ("limit", Kind::U64),
            ("doc", Kind::Object),
            ("args", Kind::StrArray),
        ];
        let sent = payload(
            "act",
            &json!({"name": "n", "live": true, "limit": 3, "doc": {}, "args": ["a"],
                "ignored": 1, "absent": null}),
            &keys,
            json!({"workspace": "/w"}),
        )
        .unwrap();
        assert_eq!(
            sent,
            json!({"action": "act", "name": "n", "live": true, "limit": 3, "doc": {},
                "args": ["a"], "workspace": "/w"})
        );
        for bad in [
            json!({"name": 1}),
            json!({"live": "yes"}),
            json!({"limit": -1}),
            json!({"doc": []}),
            json!({"args": [1]}),
        ] {
            assert!(payload("act", &bad, &keys, json!({})).is_err(), "{bad}");
        }
    }

    #[test]
    fn workspace_defaults_to_the_server_and_admits_granted_directories() {
        let server = std::env::temp_dir();
        assert_eq!(
            policy_workspace(&json!({}), "workspace", &server, false).unwrap(),
            server
        );
        assert!(policy_workspace(&json!({"workspace": 3}), "workspace", &server, false).is_err());
        assert!(
            policy_workspace(&json!({"workspace": "rel"}), "workspace", &server, false)
                .unwrap_err()
                .to_string()
                .contains("absolute")
        );
        let dir = std::env::temp_dir().join(format!("susi-forward-ws-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let resolved = policy_workspace(
            &json!({"workspace": dir.display().to_string()}),
            "workspace",
            &server,
            true,
        )
        .unwrap();
        assert_eq!(
            resolved.canonicalize().unwrap(),
            dir.canonicalize().unwrap()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
