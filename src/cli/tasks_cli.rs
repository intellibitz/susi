//! `susi tasks`: the shared task queue (`.agents/tasks/`, claims as git refs, gated
//! closes) moved to Susi Commercial Apps in the lean-core program and is
//! `susi-commercial tasks`; every queue verb is refused here with that command named.
//! `susi tasks eval-corpus` (the RSI eval-corpus run) is served there too (Mandate 84)
//! and forwards to `susi-commercial tasks eval-corpus` with the same arguments.
use crate::anyhow::{bail, Result};
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

/// What `susi tasks eval-corpus` says when `susi-commercial` is not installed.
fn eval_corpus_moved_message() -> String {
    "`susi tasks eval-corpus` is served by Susi Commercial Apps: install it and run \
     `susi-commercial tasks eval-corpus` (or `susi tasks eval-corpus`, which forwards to it)"
        .to_string()
}

fn eval_corpus_args(corpus: &Path, input: &Path) -> Vec<String> {
    vec![
        "tasks".to_string(),
        "eval-corpus".to_string(),
        "--corpus".to_string(),
        corpus.to_string_lossy().into_owned(),
        "--input".to_string(),
        input.to_string_lossy().into_owned(),
    ]
}

pub fn execute(action: Option<TaskCommands>, _cwd: &Path) -> Result<()> {
    match action {
        Some(TaskCommands::EvalCorpus { corpus, input }) => {
            if susi_config::vault_exec::commercial_executable().is_none() {
                bail!(eval_corpus_moved_message());
            }
            // The child prints the evidence JSON or its error and resolves the repository
            // root from the same working directory, exactly as this verb did in core.
            match susi_config::vault_exec::run_commercial(&eval_corpus_args(&corpus, &input), &[]) {
                Ok(0) => Ok(()),
                Ok(code) => std::process::exit(code),
                Err(e) => bail!(e),
            }
        }
        Some(TaskCommands::Moved(words)) => bail!(moved_message(words.first().map(String::as_str))),
        None => bail!(moved_message(None)),
    }
}

#[cfg(test)]
mod tests {
    use super::{eval_corpus_args, eval_corpus_moved_message, moved_message};
    use std::path::Path;

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

    #[test]
    fn eval_corpus_forwards_its_arguments_and_names_the_command_when_absent() {
        assert_eq!(
            eval_corpus_args(Path::new("c.json"), Path::new("i.json")),
            [
                "tasks",
                "eval-corpus",
                "--corpus",
                "c.json",
                "--input",
                "i.json"
            ]
        );
        assert!(eval_corpus_moved_message().contains("`susi-commercial tasks eval-corpus`"));
    }
}
