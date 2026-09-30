//! Manage Ollama models from susi: list, pull, show, remove.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OllamaAction {
    List,
    Pull { name: String },
    Show { name: String },
    Remove { name: String },
}

/// Map a high-level intent to an `ollama` CLI argv.
#[must_use]
pub fn ollama_argv(action: &OllamaAction) -> Vec<String> {
    match action {
        OllamaAction::List => vec!["ollama".into(), "list".into()],
        OllamaAction::Pull { name } => vec!["ollama".into(), "pull".into(), name.clone()],
        OllamaAction::Show { name } => vec!["ollama".into(), "show".into(), name.clone()],
        OllamaAction::Remove { name } => vec!["ollama".into(), "rm".into(), name.clone()],
    }
}

#[cfg(test)]
mod ollama_models_tests {
    use super::*;

    #[test]
    fn ollama_models_builds_cli_actions() {
        assert_eq!(ollama_argv(&OllamaAction::List)[1], "list");
        assert_eq!(
            ollama_argv(&OllamaAction::Pull {
                name: "llama3".into()
            })[2],
            "llama3"
        );
        assert_eq!(
            ollama_argv(&OllamaAction::Remove { name: "old".into() })[1],
            "rm"
        );
    }
}
