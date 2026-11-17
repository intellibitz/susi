//! `susi peers`: the verified cluster roster. Since 2026-11-17 (Mandate 84)
//! Susi Commercial Apps serves it (`susi-commercial peers`, Fleet Command):
//! this forwards the same subcommand and flags, in this working directory.

pub use susi_cli_defs::peers::*;

/// Run `susi-commercial <words>`, returning its exit code; when it is not
/// installed, name the command that serves `verb` and fail.
pub(crate) fn forward(verb: &str, words: &[String]) -> i32 {
    match susi_config::vault_exec::run_commercial(words, &[]) {
        Ok(code) => code,
        Err(e) => {
            eprintln!(
                "susi {verb} is served by Susi Commercial Apps (`susi-commercial {verb}`): {e}"
            );
            1
        }
    }
}

/// The `susi-commercial` words for `action`.
fn words(action: Option<PeersCommands>) -> Vec<String> {
    let mut words = vec!["peers".to_string()];
    let flag = |words: &mut Vec<String>, on: bool, name: &str| {
        if on {
            words.push(name.to_string());
        }
    };
    match action {
        None => {}
        Some(PeersCommands::List { json }) => {
            words.push("list".into());
            flag(&mut words, json, "--json");
        }
        Some(PeersCommands::Add { host, port }) => {
            words.extend(["add".to_string(), host]);
            if let Some(port) = port {
                words.extend(["--port".to_string(), port.to_string()]);
            }
        }
        Some(PeersCommands::Probe { host, port }) => {
            words.extend(["probe".to_string(), host]);
            if let Some(port) = port {
                words.extend(["--port".to_string(), port.to_string()]);
            }
        }
        Some(PeersCommands::Remove { peer }) => words.extend(["remove".to_string(), peer]),
        Some(PeersCommands::Unban { peer }) => words.extend(["unban".to_string(), peer]),
        Some(PeersCommands::Rekey { force }) => {
            words.push("rekey".into());
            flag(&mut words, force, "--force");
        }
        Some(PeersCommands::Status { json }) => {
            words.push("status".into());
            flag(&mut words, json, "--json");
        }
    }
    words
}

/// Execute `susi peers` by forwarding to `susi-commercial peers`.
pub fn execute(action: Option<PeersCommands>) -> i32 {
    forward("peers", &words(action))
}
