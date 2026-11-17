//! `susi commits`: the audit view of the replicated quorum-commit ledger.
//! Since 2026-11-17 (Mandate 84) Susi Commercial Apps serves it
//! (`susi-commercial commits`, Fleet Command): this forwards the same
//! subcommand and flags, in this working directory.

pub use susi_cli_defs::commits::CommitsCommands;

/// The `susi-commercial` words for `action`.
fn words(action: Option<CommitsCommands>) -> Vec<String> {
    let mut words = vec!["commits".to_string()];
    let flag = |words: &mut Vec<String>, on: bool, name: &str| {
        if on {
            words.push(name.to_string());
        }
    };
    match action {
        None => {}
        Some(CommitsCommands::List {
            limit,
            coordinator,
            term,
            kind,
            json,
        }) => {
            words.extend(["list".to_string(), "--limit".to_string(), limit.to_string()]);
            if let Some(coordinator) = coordinator {
                words.extend(["--coordinator".to_string(), coordinator]);
            }
            if let Some(term) = term {
                words.extend(["--term".to_string(), term.to_string()]);
            }
            if let Some(kind) = kind {
                words.extend(["--kind".to_string(), kind]);
            }
            flag(&mut words, json, "--json");
        }
        Some(CommitsCommands::Show { epoch, json }) => {
            words.extend(["show".to_string(), epoch]);
            flag(&mut words, json, "--json");
        }
        Some(CommitsCommands::Audit { strict, json }) => {
            words.push("audit".into());
            flag(&mut words, strict, "--strict");
            flag(&mut words, json, "--json");
        }
        Some(CommitsCommands::Replay { json }) => {
            words.push("replay".into());
            flag(&mut words, json, "--json");
        }
        Some(CommitsCommands::Sync) => words.push("sync".into()),
        Some(CommitsCommands::Compact) => words.push("compact".into()),
    }
    words
}

/// Execute `susi commits` by forwarding to `susi-commercial commits`.
pub fn execute(action: Option<CommitsCommands>) -> i32 {
    super::peers_cli::forward("commits", &words(action))
}
