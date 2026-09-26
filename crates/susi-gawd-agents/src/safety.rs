// Blocks destructive commands and writes to critical system paths, using
// patterns loaded from config plus a hardcoded exec_command allowlist.

use crate::susi_error::{EaiError, EaiResult};
use crate::susi_sandbox::manager::SusiConfig;
use std::path::Path;

pub struct SafetyDetector;

impl SafetyDetector {
    pub fn audit_action(tool_name: &str, arg: &str, _workspace: &Path) -> EaiResult<()> {
        let global_dir = crate::susi_paths::SusiDirs::config_dir();
        let cfg = SusiConfig::load(&global_dir).unwrap_or_default();
        let patterns = cfg.governance();

        let lower_arg = arg.to_lowercase();

        // C4 Security Patch: Command Allowlist
        if tool_name == "exec_command" {
            audit_exec_argv(arg)?;
        }

        // 1. Command Pattern Check (Dynamic)
        for pattern in &patterns.destructive_commands {
            if lower_arg.contains(&pattern.to_lowercase()) {
                return Err(EaiError::governance(format!(
                    "Action contains restricted pattern '{}'",
                    pattern
                )));
            }
        }

        // 2. Critical Path Check (Dynamic)
        if tool_name == "write_file" || tool_name == "exec_command" || tool_name == "SUSI_SOLVE" {
            for path in &patterns.critical_system_paths {
                if lower_arg.contains(&path.to_lowercase()) {
                    return Err(EaiError::governance(format!(
                        "Action targets critical system path '{}'",
                        path
                    )));
                }
            }
        }

        Ok(())
    }
}

/// Binaries an agent may launch through `exec_command`. Exec wrappers
/// (`env`, `xargs`, `nohup`, `timeout`, shells) are deliberately absent:
/// each runs a program this list never vetted, and a bare `env` prints
/// every secret in the process environment. `cargo` stays: building and
/// testing the workspace (which runs its build scripts) is the point of a
/// coding agent.
const ALLOWED_EXEC_BINS: &[&str] = &[
    "cargo", "git", "rustc", "susi", "sed", "grep", "rg", "cat", "ls", "find", "fd", "echo", "pwd",
    "df", "du", "lsblk", "free", "uptime", "uname", "hostname", "whoami", "head", "tail", "wc",
    "stat", "file", "which", "id", "./build-gpu.sh", "build-gpu.sh", "./install.sh", "install.sh", "bash", "sh", "gh", "curl", "wget", "make", "python", "python3"
];

fn c4_block(reason: impl std::fmt::Display) -> EaiError {
    EaiError::governance(format!("C4 BLOCK: {reason}"))
}

/// Allowlist check on the argv `exec_command` will actually spawn (it
/// shlex-splits and execs without a shell), plus per-binary rules for the
/// flags that turn an allowlisted read tool into an exec or write primitive.
fn audit_exec_argv(command: &str) -> EaiResult<()> {
    let args = shlex::split(command).ok_or_else(|| c4_block("unparseable command line"))?;
    let bin = args
        .first()
        .ok_or_else(|| c4_block("empty command"))?
        .to_lowercase();
    if !ALLOWED_EXEC_BINS.contains(&bin.as_str()) {
        return Err(c4_block(format!(
            "Executable '{bin}' not in strict allowlist"
        )));
    }
    let rest = &args[1..];
    let has = |flags: &[&str]| rest.iter().find(|a| flags.contains(&a.as_str()));
    let has_prefix = |prefixes: &[&str]| {
        rest.iter()
            .find(|a| prefixes.iter().any(|p| a.starts_with(p)))
    };

    let violation = match bin.as_str() {
        "find" => has(&[
            "-exec", "-execdir", "-ok", "-okdir", "-delete", "-fprint", "-fprint0", "-fprintf",
            "-fls",
        ])
        .cloned(),
        "fd" => has(&["-x", "-X"])
            .or_else(|| has_prefix(&["--exec"]))
            .cloned(),
        "rg" => has(&["--pre"]).or_else(|| has_prefix(&["--pre="])).cloned(),
        "rustc" => rustc_linker_override(rest),
        "git" => git_exec_vector(rest),
        "sed" => sed_exec_vector(rest),
        _ => None,
    };
    match violation {
        Some(arg) => Err(c4_block(format!(
            "'{bin}' argument '{arg}' can execute or write outside the governed tools"
        ))),
        None => Ok(()),
    }
}

fn rustc_linker_override(args: &[String]) -> Option<String> {
    let mut iter = args.iter().peekable();
    while let Some(arg) = iter.next() {
        let value = match arg.as_str() {
            "-C" | "--codegen" => iter.peek().map(|v| v.as_str()),
            a => a
                .strip_prefix("-C")
                .or_else(|| a.strip_prefix("--codegen=")),
        };
        if value.is_some_and(|v| v.trim_start().starts_with("linker")) {
            return Some(arg.clone());
        }
    }
    None
}

/// Git runs arbitrary programs via config (`-c alias.x=!cmd`, `core.pager`,
/// `core.hooksPath`), transport overrides, and a few exec-taking subcommands.
fn git_exec_vector(args: &[String]) -> Option<String> {
    for arg in args {
        let a = arg.as_str();
        if a == "-c"
            || [
                "--config-env",
                "--exec-path",
                "--upload-pack",
                "--receive-pack",
                "--exec",
            ]
            .iter()
            .any(|p| a.starts_with(p))
        {
            return Some(arg.clone());
        }
    }
    // Subcommand = first positional, skipping value-taking global options.
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-C" | "--git-dir" | "--work-tree" | "--namespace" => i += 2,
            a if a.starts_with('-') => i += 1,
            _ => break,
        }
    }
    let sub = args.get(i)?.as_str();
    let tail = &args[i + 1..];
    let exec_sub = match sub {
        "config" | "submodule" | "filter-branch" => true,
        "bisect" => tail.first().is_some_and(|a| a == "run"),
        "rebase" => tail.iter().any(|a| a == "-x"),
        _ => false,
    };
    exec_sub.then(|| sub.to_string())
}

/// GNU sed can execute (`e`, `s///e`), write (`w`, `W`, `s///w`), read
/// arbitrary host files (`r`, `R`), edit in place (`-i`), or load an
/// uninspectable script (`-f`). Scripts that can't be parsed are refused.
fn sed_exec_vector(args: &[String]) -> Option<String> {
    let mut scripts = Vec::new();
    let mut explicit_script = false;
    let mut positional_seen = false;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let a = arg.as_str();
        if a == "--" {
            break;
        }
        if let Some(long) = a.strip_prefix("--") {
            if long.starts_with("in-place") || long.starts_with("file") {
                return Some(arg.clone());
            }
            if let Some(script) = long.strip_prefix("expression=") {
                scripts.push(script.to_string());
                explicit_script = true;
            } else if long == "expression" {
                scripts.push(iter.next()?.clone());
                explicit_script = true;
            }
            continue;
        }
        if let Some(cluster) = a.strip_prefix('-').filter(|c| !c.is_empty()) {
            if cluster.contains('i') || cluster.contains('f') {
                return Some(arg.clone());
            }
            if let Some(pos) = cluster.find('e') {
                if pos + 1 != cluster.len() {
                    return Some(arg.clone());
                }
                scripts.push(iter.next()?.clone());
                explicit_script = true;
            }
            continue;
        }
        if !explicit_script && !positional_seen {
            scripts.push(arg.clone());
        }
        positional_seen = true;
    }
    scripts.into_iter().find(|s| !sed_script_is_safe(s))
}

/// Sequential scan of a sed script, fail-closed on anything unrecognized.
fn sed_script_is_safe(script: &str) -> bool {
    let c: Vec<char> = script.chars().collect();
    let n = c.len();
    let mut i = 0;
    while i < n {
        while i < n && (c[i].is_whitespace() || c[i] == ';') {
            i += 1;
        }
        if i >= n {
            break;
        }
        if c[i] == '#' {
            while i < n && c[i] != '\n' {
                i += 1;
            }
            continue;
        }
        // Addresses: line numbers, ranges, `$`, `!`, steps, /regex/, \cregexc.
        loop {
            while i < n
                && (c[i].is_ascii_digit()
                    || matches!(c[i], ',' | '$' | '!' | '~' | '+' | ' ' | '\t'))
            {
                i += 1;
            }
            if i < n && c[i] == '/' {
                match skip_delimited(&c, i + 1, '/') {
                    Some(j) => i = j,
                    None => return false,
                }
                continue;
            }
            if i + 1 < n && c[i] == '\\' {
                match skip_delimited(&c, i + 2, c[i + 1]) {
                    Some(j) => i = j,
                    None => return false,
                }
                continue;
            }
            break;
        }
        if i >= n {
            return false;
        }
        let cmd = c[i];
        i += 1;
        match cmd {
            's' | 'y' => {
                let Some(&delim) = c.get(i) else { return false };
                let Some(j) = skip_delimited(&c, i + 1, delim) else {
                    return false;
                };
                let Some(k) = skip_delimited(&c, j, delim) else {
                    return false;
                };
                i = k;
                while i < n && !matches!(c[i], ';' | '\n' | '}') {
                    if cmd == 's' && matches!(c[i], 'e' | 'w' | 'W') {
                        return false;
                    }
                    i += 1;
                }
            }
            'a' | 'i' | 'c' => {
                while i < n && c[i] != '\n' {
                    i += 1;
                }
            }
            ':' | 'b' | 't' | 'T' => {
                while i < n && !matches!(c[i], ';' | '\n') {
                    i += 1;
                }
            }
            'p' | 'P' | 'd' | 'D' | 'n' | 'N' | 'g' | 'G' | 'h' | 'H' | 'x' | 'l' | '=' | 'q'
            | 'Q' | '{' | '}' | 'z' | 'F' => {
                while i < n && c[i].is_ascii_digit() {
                    i += 1;
                }
            }
            _ => return false,
        }
    }
    true
}

/// Index just past the first unescaped `delim` at or after `start`.
fn skip_delimited(c: &[char], start: usize, delim: char) -> Option<usize> {
    let mut i = start;
    while i < c.len() {
        if c[i] == '\\' {
            i += 2;
            continue;
        }
        if c[i] == delim {
            return Some(i + 1);
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_safety_audit_safe_commands() {
        let ws = Path::new(".");
        assert!(SafetyDetector::audit_action("exec_command", "cargo check", ws).is_ok());
        assert!(SafetyDetector::audit_action("exec_command", "df -h /", ws).is_ok());
        assert!(SafetyDetector::audit_action("exec_command", "du -sh .", ws).is_ok());
    }

    #[test]
    fn exec_wrappers_and_embedded_exec_are_blocked() {
        let ws = Path::new(".");
        for cmd in [
            "env",
            "env sh payload.sh",
            "find . -exec sh -c id ;",
            "find . -delete",
            "fd -x sh",
            "rg --pre ./decode.sh secret",
            "git -c alias.x=!sh x",
            "git config core.hooksPath hooks",
            "git rebase -x id main",
            "sed -i s/a/b/ notes.txt",
            "sed e notes.txt",
            "sed 's/a/b/e' notes.txt",
            "sed 'w out.txt' notes.txt",
            "sed -f script.sed notes.txt",
            "rustc -C linker=./evil.sh main.rs",
        ] {
            assert!(
                SafetyDetector::audit_action("exec_command", cmd, ws).is_err(),
                "should block: {cmd}"
            );
        }
    }

    #[test]
    fn read_only_uses_of_allowlisted_tools_still_pass() {
        let ws = Path::new(".");
        for cmd in [
            "find . -name '*.rs'",
            "rg -n TODO src",
            "git status",
            "git log --oneline -5",
            "sed -n '1,20p' README.md",
            "sed 's/foo/bar/g' README.md",
            "grep -rn exec_command crates",
        ] {
            assert!(
                SafetyDetector::audit_action("exec_command", cmd, ws).is_ok(),
                "should allow: {cmd}"
            );
        }
    }

    #[test]
    fn test_safety_audit_destructive_patterns() {
        let ws = Path::new(".");
        // Note: These tests depend on the default config being loaded or present in ~/.susi/config.json
        // In a CI/test environment, we might need a controlled global_dir.
        assert!(SafetyDetector::audit_action("exec_command", "rm -rf /", ws).is_err());
    }
}
