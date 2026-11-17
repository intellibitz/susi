//! A single-operation snapshot of board configuration, never refs or claims.

use std::collections::BTreeMap;
use std::path::Path;

pub(super) fn configuration(workspace: Option<&Path>) -> BTreeMap<String, String> {
    let Some(workspace) = workspace else {
        return BTreeMap::new();
    };
    let bytes = super::git_bytes(
        workspace,
        &[
            "config",
            "--null",
            "--get-regexp",
            "^susi\\.board\\.(url|remote-url|remote)$",
        ],
        None,
    )
    .unwrap_or_default();
    // Git's NUL format separates each key from its value with a newline;
    // values can contain newlines. Last value wins, as with `config --get`.
    String::from_utf8_lossy(&bytes)
        .split('\0')
        .filter_map(|record| record.split_once('\n'))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}
