//! Just-in-time credential request (VC-201-066 / zero-config).
//!
//! When a mission needs a cloud provider key that is absent: on a TTY ask
//! once; otherwise emit a machine-readable `credential_needed` record and
//! return the distinct exit code [`CREDENTIAL_NEEDED_EXIT`] — never a stack
//! trace.

use serde::{Deserialize, Serialize};
use std::io::{self, IsTerminal, Write};

/// Process exit code when a credential is required and no TTY can supply it.
/// Distinct from generic config errors so agents/CI can detect and inject.
pub const CREDENTIAL_NEEDED_EXIT: i32 = 81;

/// Machine-readable record written to stdout (JSON) for non-TTY callers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialNeeded {
    pub kind: String,
    pub provider: String,
    pub env_var: String,
    pub message: String,
    pub exit_code: i32,
}

impl CredentialNeeded {
    #[must_use]
    pub fn for_provider(provider: &str, env_var: &str) -> Self {
        Self {
            kind: "credential_needed".to_string(),
            provider: provider.to_string(),
            env_var: env_var.to_string(),
            message: format!(
                "cloud provider `{provider}` needs {env_var}; set it or paste once when prompted"
            ),
            exit_code: CREDENTIAL_NEEDED_EXIT,
        }
    }

    #[must_use]
    pub fn to_json_line(&self) -> String {
        match serde_json::to_string(self) {
            Ok(s) => s,
            Err(_) => format!(
                "{{\"kind\":\"credential_needed\",\"env_var\":\"{}\",\"exit_code\":{CREDENTIAL_NEEDED_EXIT}}}",
                self.env_var
            ),
        }
    }
}

/// Outcome of a JIT credential request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JitCredentialOutcome {
    /// Secret already present (env / cloud.env).
    AlreadyPresent(String),
    /// Operator typed a value on a TTY (not persisted by this helper).
    SuppliedOnTty(String),
    /// No TTY — caller should print the record and exit with the code.
    NeedRecord(CredentialNeeded),
}

/// Inputs for a JIT credential request (keeps the public entry under clippy's
/// argument limit).
pub struct JitCredentialRequest<'a> {
    pub provider: &'a str,
    pub env_var: &'a str,
    pub stdin_is_tty: bool,
}

/// Look up `env_var` via `lookup`. If missing and `stdin` is a TTY, prompt
/// once on `stderr`/`stdin`; otherwise return [`JitCredentialOutcome::NeedRecord`].
pub fn request_credential_jit<F>(
    req: &JitCredentialRequest<'_>,
    mut lookup: F,
    prompt_out: &mut dyn Write,
    prompt_in: &mut dyn io::BufRead,
) -> io::Result<JitCredentialOutcome>
where
    F: FnMut(&str) -> Option<String>,
{
    if let Some(v) = lookup(req.env_var).filter(|s| !s.trim().is_empty()) {
        return Ok(JitCredentialOutcome::AlreadyPresent(v));
    }
    if req.stdin_is_tty {
        writeln!(
            prompt_out,
            "credential needed for `{}`: enter value for {} (not saved to disk): ",
            req.provider, req.env_var
        )?;
        prompt_out.flush()?;
        let mut line = String::new();
        prompt_in.read_line(&mut line)?;
        let value = line.trim().to_string();
        if value.is_empty() {
            return Ok(JitCredentialOutcome::NeedRecord(
                CredentialNeeded::for_provider(req.provider, req.env_var),
            ));
        }
        return Ok(JitCredentialOutcome::SuppliedOnTty(value));
    }
    Ok(JitCredentialOutcome::NeedRecord(
        CredentialNeeded::for_provider(req.provider, req.env_var),
    ))
}

/// Detect whether stdin is an interactive terminal.
#[must_use]
pub fn stdin_is_tty() -> bool {
    io::stdin().is_terminal()
}
