//! Chat templates detected from model metadata.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatTemplate {
    pub name: String,
    pub source: String,
}

/// Prefer metadata chat_template; else family heuristics.
#[must_use]
pub fn detect_chat_template(family: &str, metadata_template: Option<&str>) -> ChatTemplate {
    if let Some(t) = metadata_template.filter(|s| !s.is_empty()) {
        return ChatTemplate {
            name: t.into(),
            source: "metadata".into(),
        };
    }
    let f = family.to_ascii_lowercase();
    let name = if f.contains("llama") {
        "llama3"
    } else if f.contains("qwen") {
        "qwen"
    } else if f.contains("mistral") {
        "mistral"
    } else {
        "chatml"
    };
    ChatTemplate {
        name: name.into(),
        source: "family".into(),
    }
}

#[cfg(test)]
mod zc_chat_templates_tests {
    use super::*;

    #[test]
    fn zc_chat_templates_prefer_metadata() {
        let t = detect_chat_template("llama", Some("custom-jinja"));
        assert_eq!(t.source, "metadata");
        assert_eq!(t.name, "custom-jinja");
        assert_eq!(detect_chat_template("qwen2", None).name, "qwen");
    }
}
