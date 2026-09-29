//! LM Studio model manage: load / unload / list via `lms` CLI output parsing.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LmStudioModel {
    pub id: String,
    pub loaded: bool,
}

/// Parse `lms ls` style table/JSON lines into model records.
#[must_use]
pub fn parse_lms_ls(output: &str) -> Vec<LmStudioModel> {
    let mut out = Vec::new();
    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("ID") {
            continue;
        }
        // JSON object per line
        if line.starts_with('{') {
            if let Ok(m) = serde_json::from_str::<LmStudioModel>(line) {
                out.push(m);
                continue;
            }
        }
        // csv-ish: id,loaded
        let parts: Vec<_> = line.split_whitespace().collect();
        if parts.is_empty() {
            continue;
        }
        let loaded = parts.iter().any(|p| {
            let p = p.to_ascii_lowercase();
            p == "loaded" || p == "true" || p == "yes"
        });
        out.push(LmStudioModel {
            id: parts[0].to_string(),
            loaded,
        });
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LmsAction {
    Load(String),
    Unload(String),
    List,
}

#[must_use]
pub fn lms_argv(action: &LmsAction) -> Vec<String> {
    match action {
        LmsAction::List => vec!["lms".into(), "ls".into()],
        LmsAction::Load(id) => vec!["lms".into(), "load".into(), id.clone()],
        LmsAction::Unload(id) => vec!["lms".into(), "unload".into(), id.clone()],
    }
}

#[cfg(test)]
mod lmstudio_models_tests {
    use super::*;

    #[test]
    fn lmstudio_models_parses_list_and_builds_argv() {
        let models = parse_lms_ls(
            "ID STATE\nllama-3B loaded\nphi-mini\n{\"id\":\"qwen\",\"loaded\":true}\n",
        );
        assert_eq!(models.len(), 3);
        assert!(models[0].loaded);
        assert!(!models[1].loaded);
        assert_eq!(models[2].id, "qwen");
        assert_eq!(lms_argv(&LmsAction::Load("x".into()))[1], "load");
        assert_eq!(lms_argv(&LmsAction::Unload("x".into()))[1], "unload");
        assert_eq!(lms_argv(&LmsAction::List), vec!["lms", "ls"]);
    }
}
