//! `susi workflow`: the agent worktree workflow (check, start, sync, finish,
//! guard, park-primary, seed-target, ...) moved to Susi Commercial Apps in the
//! lean-core program and is `susi-commercial workflow`. Open core refuses
//! every verb here with that command named; the gated landing tool of this
//! repository is `cargo local-workflow`.
use crate::anyhow::{bail, Result};

/// The refusal for `susi workflow <args>`.
pub(crate) fn moved_message(args: &[String]) -> String {
    let verb = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .map_or("check", String::as_str);
    format!(
        "`susi workflow {verb}` moved to Susi Commercial Apps: run \
         `susi-commercial workflow {verb}`; this repository's gated landing is \
         `cargo local-workflow`"
    )
}

pub fn execute(args: &[String]) -> Result<()> {
    bail!(moved_message(args))
}

#[cfg(test)]
mod tests {
    use super::moved_message;

    #[test]
    fn a_workflow_verb_is_refused_naming_the_commercial_command() {
        let args = ["guard".to_string(), "commit".to_string()];
        let msg = moved_message(&args);
        assert!(msg.contains("`susi-commercial workflow guard`"), "{msg}");
        assert!(moved_message(&[]).contains("`susi-commercial workflow check`"));
        assert!(moved_message(&["--help".to_string()]).contains("workflow check"));
        assert!(
            !msg.contains("rebuild"),
            "a refusal never asks for a rebuild"
        );
    }
}
