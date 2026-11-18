//! `clap` definitions for what stays of the `tasks` command family in open
//! core: the RSI eval-corpus run. The shared task queue (`list`, `add`,
//! `claim`, `close`, ...) moved to Susi Commercial Apps in the lean-core
//! program and is `susi-commercial tasks`; core refuses those verbs naming
//! it. The handlers live in the root crate (`src/cli/tasks_cli.rs`).
use clap::Subcommand;
use std::path::PathBuf;

/// Subcommands of `susi tasks`.
#[derive(Debug, Subcommand)]
pub enum TaskCommands {
    /// Load the versioned RSI corpus, judge one candidate run against its
    /// held-out suite, and durably record the contamination/promotion
    /// evidence (VC-201-001). `--input` is a JSON file shaped
    /// {"candidate":{"id","actual_output","expected_results"},
    ///  "candidate_can_write","training_access","memory_access"}.
    #[command(name = "eval-corpus")]
    EvalCorpus {
        /// Path to the corpus JSON document
        #[arg(long, value_name = "FILE")]
        corpus: PathBuf,
        /// Path to the JSON-encoded eval run (candidate + access sets)
        #[arg(long, value_name = "FILE")]
        input: PathBuf,
    },
    /// A task-queue verb: the queue is `susi-commercial tasks` (Susi
    /// Commercial Apps); core refuses it naming that command.
    #[command(external_subcommand)]
    Moved(Vec<String>),
}

#[cfg(test)]
mod tests {
    use super::TaskCommands;
    use clap::Parser;

    #[derive(Parser)]
    struct Tasks {
        #[command(subcommand)]
        action: Option<TaskCommands>,
    }

    #[test]
    fn eval_corpus_parses_and_queue_verbs_are_captured_as_moved() {
        let eval = Tasks::try_parse_from(["tasks", "eval-corpus", "--corpus", "c", "--input", "i"])
            .unwrap();
        assert!(matches!(eval.action, Some(TaskCommands::EvalCorpus { .. })));
        let moved = Tasks::try_parse_from(["tasks", "claim", "T-A-1", "--scope", "src"]).unwrap();
        let Some(TaskCommands::Moved(words)) = moved.action else {
            panic!("a queue verb must be captured for its refusal");
        };
        assert_eq!(words, ["claim", "T-A-1", "--scope", "src"]);
        assert!(Tasks::try_parse_from(["tasks"]).unwrap().action.is_none());
    }
}
