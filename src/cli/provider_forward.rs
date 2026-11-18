//! Provider command implementations live in Susi Commercial Apps. Core keeps
//! its public parser and passes its accepted original words to that runtime.
use crate::anyhow::{Error, Result};
use std::ffi::OsString;
use std::path::Path;
use std::process::{Command, Stdio};

pub(super) fn execute(cwd: Option<&Path>) -> Result<()> {
    let words: Vec<OsString> = std::env::args_os().skip(1).collect();
    let executable = susi_config::vault_exec::commercial_executable().ok_or_else(|| {
        Error::msg("Provider commands are served by Susi Commercial Apps: install it and run `susi-commercial core-provider-cli <command> <arguments>`")
    })?;
    let current = std::env::current_dir()?;
    let core = std::env::current_exe()?;
    let status = command(&executable, cwd.unwrap_or(&current), &core, &words).status()?;
    match status.code().unwrap_or(1) {
        0 => Ok(()),
        code => std::process::exit(code),
    }
}

fn command(executable: &Path, cwd: &Path, core: &Path, words: &[OsString]) -> Command {
    let mut command = Command::new(executable);
    command
        .arg("core-provider-cli")
        .args(words)
        .current_dir(cwd)
        .env("SUSI_CORE_BIN", core)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    command
}

#[cfg(test)]
#[path = "tests/provider_forward.rs"]
mod tests;
