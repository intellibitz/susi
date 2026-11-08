//! `susi tasks`: in open core only the RSI eval-corpus run. The shared task
//! queue (`.agents/tasks/`, claims as git refs, gated closes) moved to Susi
//! Commercial Apps in the lean-core program and is `susi-commercial tasks`;
//! every queue verb is refused here with that command named.
use crate::cli_json::print_json;
use anyhow::{bail, Result};
use std::path::Path;

pub use susi_cli_defs::tasks::*;

/// The refusal for a task-queue verb (`None`: bare `susi tasks`, the list).
pub(crate) fn moved_message(verb: Option<&str>) -> String {
    let verb = verb.unwrap_or("list");
    format!(
        "`susi tasks {verb}` moved to Susi Commercial Apps: the shared task queue is \
         `susi-commercial tasks {verb}` (open core keeps `susi tasks eval-corpus`)"
    )
}

pub fn execute(action: Option<TaskCommands>, cwd: &Path) -> Result<()> {
    match action {
        Some(TaskCommands::EvalCorpus { corpus, input }) => {
            let root = susi_daemon::host_views::repo_root(cwd);
            let corpus_json = std::fs::read_to_string(&corpus)
                .map_err(|e| anyhow::anyhow!("{}: {e}", corpus.display()))?;
            let corpus = susi_gawd::rsi_corpus::RsiCorpus::from_json(&corpus_json)
                .map_err(|e| anyhow::anyhow!("corpus {}: {e}", corpus.display()))?;
            let input_json = std::fs::read_to_string(&input)
                .map_err(|e| anyhow::anyhow!("{}: {e}", input.display()))?;
            let run_input: susi_gawd::eval_separation::EvalRunInput =
                serde_json::from_str(&input_json)
                    .map_err(|e| anyhow::anyhow!("{}: {e}", input.display()))?;
            let evidence =
                susi_gawd::eval_separation::evaluate_and_record(&root, &corpus, &run_input)?;
            print_json(&evidence)?;
            Ok(())
        }
        Some(TaskCommands::Moved(words)) => bail!(moved_message(words.first().map(String::as_str))),
        None => bail!(moved_message(None)),
    }
}

#[cfg(test)]
mod tests {
    use super::moved_message;

    #[test]
    fn a_queue_verb_is_refused_naming_the_commercial_command() {
        let msg = moved_message(Some("claim"));
        assert!(msg.contains("`susi-commercial tasks claim`"), "{msg}");
        assert!(moved_message(None).contains("`susi-commercial tasks list`"));
        assert!(
            !msg.contains("rebuild"),
            "a refusal never asks for a rebuild"
        );
    }
}
