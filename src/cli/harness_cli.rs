//! `susi harness` is served by Susi Commercial Apps since the harness left core
//! (Mandate 84; core T-CORESIZEVAULT-15). Core still parses the command with its
//! own definitions, then runs `susi-commercial core-harness` with the same words in
//! the same workspace, naming this binary in `SUSI_CORE_BIN` (a run's adapter
//! drives it, as this binary drove itself), and returns its output and exit code.
//! Without the suite installed, the command names what serves it.

use crate::anyhow::{bail, Result};
use std::path::Path;
pub use susi_cli_defs::harness::HarnessCommands;

pub fn execute(_action: HarnessCommands, cwd: &Path) -> Result<()> {
    let words: Vec<String> = std::env::args()
        .skip_while(|word| word != "harness")
        .skip(1)
        .collect();
    let typed = words.join(" ");
    if susi_config::vault_exec::commercial_executable().is_none() {
        bail!(
            "`susi harness {typed}` is served by Susi Commercial Apps: install it and run `susi-commercial core-harness {typed}`"
        );
    }
    let core = std::env::current_exe().unwrap_or_default();
    let mut forwarded = vec!["core-harness".to_string()];
    forwarded.extend(words);
    let envs = [("SUSI_CORE_BIN", core.as_os_str())];
    match susi_config::vault_exec::run_commercial_in(&forwarded, cwd, &envs) {
        Ok(0) => Ok(()),
        Ok(code) => std::process::exit(code),
        Err(e) => bail!(e),
    }
}

#[cfg(test)]
#[path = "tests/harness_cli.rs"]
mod tests;
