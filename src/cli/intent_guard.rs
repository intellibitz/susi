//! Refuses command-shaped input that is not a susi command before it can be
//! ingested as an autonomous mission.
//!
//! Bare arguments are mission intent by design, but a mission spends inference
//! money, boots the daemon and writes evidence into the repo. Two shapes are
//! never prose and are almost always a mistyped command:
//!
//! * flags — `susi release --help` (a leading or trailing `-x`/`--x` token
//!   with no spaces inside any token);
//! * a nested command word — `susi scan`, `susi ecosystem scan`,
//!   `susi release` — when the first word is a subcommand somewhere below the
//!   top level but not a top-level command itself.
//!
//! Multi-word sentences (`susi fix the login bug`) and quoted sentences pass.
use clap::Command;

/// Every command path below the top level whose final word is `word`
/// (`release` → `admin release`), including visible aliases.
pub(crate) fn nested_paths(root: &Command, word: &str) -> Vec<String> {
    fn walk(cmd: &Command, prefix: &str, word: &str, out: &mut Vec<String>) {
        for sub in cmd.get_subcommands() {
            let path = if prefix.is_empty() {
                sub.get_name().to_string()
            } else {
                format!("{prefix} {}", sub.get_name())
            };
            let names = std::iter::once(sub.get_name()).chain(sub.get_visible_aliases());
            if !prefix.is_empty() && names.into_iter().any(|n| n == word) {
                out.push(path.clone());
            }
            walk(sub, &path, word, out);
        }
    }
    let mut out = Vec::new();
    walk(root, "", word, &mut out);
    out.sort();
    out.dedup();
    out
}

fn is_top_level(root: &Command, word: &str) -> bool {
    root.get_subcommands()
        .any(|c| c.get_name() == word || c.get_visible_aliases().any(|a| a == word))
}

/// `Some(suggestions)` when `intent` must be refused (empty suggestions =
/// refuse without a specific hint), `None` when it may become a mission.
/// Application commands removed from the OS surface, with the plane that
/// still does the job. A bare `susi aider` must not become a swarm mission.
const RETIRED_APPLICATION_COMMANDS: &[(&str, &str)] = &[
    (
        "chat",
        "`susi run` executes one completion; quote a sentence to run a mission",
    ),
    (
        "code",
        "`susi patch` applies a tested edit; coding peers are `susi agents`",
    ),
    ("aider", "`susi agents run aider`"),
    ("openhands", "`susi agents run openhands`"),
    ("oh", "`susi agents run openhands`"),
    ("gemini", "`susi agents run gemini-cli`"),
    ("gcli", "`susi agents run gemini-cli`"),
    ("swe-agent", "`susi agents run swe-agent`"),
    ("sweagent", "`susi agents run swe-agent`"),
    ("browser-use", "`susi agents run browser-use`"),
    ("bu", "`susi agents run browser-use`"),
    ("openviking", "`susi agents run openviking`"),
    ("viking", "`susi agents run openviking`"),
    ("deerflow", "`susi agents run deerflow`"),
    ("deer", "`susi agents run deerflow`"),
    ("langgraph", "`susi frameworks run langgraph`"),
    ("lg", "`susi frameworks run langgraph`"),
    ("openai-agents", "`susi frameworks run openai-agents`"),
    ("oas", "`susi frameworks run openai-agents`"),
    ("autogen", "`susi frameworks run autogen`"),
    ("ag", "`susi frameworks run autogen`"),
    ("smolagents", "`susi frameworks run smolagents`"),
    ("smol", "`susi frameworks run smolagents`"),
    ("crewai", "`susi frameworks run crewai`"),
    ("llamaindex", "`susi frameworks run llamaindex`"),
    ("llama", "`susi frameworks run llamaindex`"),
    ("temporal", "`susi frameworks run temporal`"),
    ("e2b", "`susi frameworks run e2b`"),
    ("haystack", "`susi frameworks run haystack`"),
];

pub(crate) fn refuse(root: &Command, intent: &[String]) -> Option<Vec<String>> {
    let first = intent.first()?;
    if let Some((_, hint)) = RETIRED_APPLICATION_COMMANDS
        .iter()
        .find(|(name, _)| *name == first)
    {
        return Some(vec![(*hint).to_string()]);
    }
    let argv_like = intent.iter().all(|t| !t.chars().any(char::is_whitespace));
    if !argv_like {
        return None; // a quoted sentence is prose
    }
    let flag_shaped = intent.iter().any(|t| t.starts_with('-') && t.len() > 1);
    let nested = if is_top_level(root, first) {
        Vec::new()
    } else {
        nested_paths(root, first)
    };
    if flag_shaped || !nested.is_empty() {
        return Some(nested.into_iter().map(|p| format!("`susi {p}`")).collect());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    fn root() -> Command {
        crate::cli::defs::Cli::command()
    }
    fn args(s: &str) -> Vec<String> {
        s.split(' ').map(str::to_string).collect()
    }

    #[test]
    fn flag_shaped_input_is_refused() {
        assert!(refuse(&root(), &args("release --help")).is_some());
        assert!(refuse(&root(), &args("--version-please")).is_some());
        assert!(refuse(&root(), &args("do -x now")).is_some());
    }

    #[test]
    fn nested_command_words_are_refused_with_a_path_hint() {
        let hint = refuse(&root(), &args("release")).unwrap();
        assert!(
            hint.contains(&"`susi admin release`".to_string()),
            "{hint:?}"
        );
        let scan = refuse(&root(), &args("ecosystem scan"));
        assert!(
            scan.is_none(),
            "top-level command words dispatch, never reach the guard"
        );
        let nested = refuse(&root(), &args("scan")).unwrap();
        assert!(
            nested.iter().any(|h| h.contains("ecosystem scan")),
            "{nested:?}"
        );
    }

    #[test]
    fn prose_and_quoted_sentences_stay_missions() {
        assert!(refuse(&root(), &args("fix the login bug")).is_none());
        assert!(refuse(&root(), &["explain --release to me".to_string()]).is_none());
        assert!(refuse(&root(), &args("identity")).is_none());
        assert!(refuse(&root(), &[]).is_none());
    }

    #[test]
    fn retired_application_commands_point_at_the_os_plane() {
        let chat = refuse(&root(), &args("chat")).unwrap();
        assert!(chat.iter().any(|h| h.contains("susi run")), "{chat:?}");
        let aider = refuse(&root(), &args("aider")).unwrap();
        assert!(
            aider.iter().any(|h| h.contains("susi agents run aider")),
            "{aider:?}"
        );
        let crew = refuse(&root(), &args("crewai")).unwrap();
        assert!(
            crew.iter()
                .any(|h| h.contains("susi frameworks run crewai")),
            "{crew:?}"
        );
        assert!(refuse(&root(), &args("fix the login bug")).is_none());
    }

    #[test]
    fn top_level_words_are_never_treated_as_nested() {
        // `status` is both a top-level command and nested under others.
        assert!(refuse(&root(), &args("status")).is_none());
    }
}
