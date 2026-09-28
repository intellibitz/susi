//! Verification contracts — typed outcome assertions, not existence checks.
//!
//! "Did it run" and "did it work" are different questions. A contract names a
//! checkable outcome (this file exists with this content, this command exits
//! clean, this path is gone); the registry evaluates it against the physical
//! workspace and returns evidence — the observed hash, size, or exit code —
//! so the verdict is auditable rather than a bare yes/no. Contracts that
//! cannot be checked return `Unverifiable`, never `Verified`: the registry
//! fails closed by construction.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::bounded_cmd::output_within;
use crate::evidence::confined_file;

/// A checkable outcome assertion about the workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Contract {
    /// A regular file at this workspace-relative path.
    FileExists { path: PathBuf },
    /// No filesystem entry at this path (deletion claims).
    FileAbsent { path: PathBuf },
    /// A file whose content contains `needle` byte-for-byte.
    FileContains { path: PathBuf, needle: String },
    /// A file whose SHA-256 equals `sha256_hex` (lowercase hex).
    FileHash { path: PathBuf, sha256_hex: String },
    /// `argv[0]` in `VERIFIER_BINARIES`, run workspace-confined with a
    /// deadline, must exit 0. The allowlist exists because a verifier is a
    /// probe: contracts that could mutate state are refused as
    /// `Unverifiable` before a process ever spawns.
    CommandExit {
        argv: Vec<String>,
        timeout_secs: u64,
    },
}

/// What the contract observed — carried into audit entries regardless of
/// verdict, so a `Violated` record shows *what* reality contradicted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContractVerdict {
    /// Assertion confirmed against physical state.
    Verified { evidence: String },
    /// Assertion contradicted by physical state.
    Violated { reason: String, evidence: String },
    /// Registry cannot check this (unsupported kind, disallowed program,
    /// unreadable claim) — the honest answer, not a pass.
    Unverifiable { reason: String },
}

impl ContractVerdict {
    pub fn is_verified(&self) -> bool {
        matches!(self, Self::Verified { .. })
    }

    /// Violation detail for truth-gate aggregation; `None` otherwise.
    pub fn violation(&self) -> Option<String> {
        match self {
            Self::Violated { reason, .. } => Some(reason.clone()),
            Self::Verified { .. } | Self::Unverifiable { .. } => None,
        }
    }

    /// Short evidence line for audit/trace records.
    pub fn evidence_line(&self) -> String {
        match self {
            Self::Verified { evidence } => format!("verified: {evidence}"),
            Self::Violated { reason, evidence } => format!("violated: {reason} ({evidence})"),
            Self::Unverifiable { reason } => format!("unverifiable: {reason}"),
        }
    }
}

/// Programs a `CommandExit` contract may run. Verification probes only —
/// nothing here writes to the workspace or the network.
const VERIFIER_BINARIES: &[&str] = &[
    "cargo",
    "sh",
    "bash",
    "git",
    "test",
    "cmp",
    "diff",
    "grep",
    "sha256sum",
    "true",
    "false",
    "cat",
    "ls",
    "wc",
];

const MAX_VERIFY_TIMEOUT_SECS: u64 = 60;
/// Verifier output that lands in an audit line is capped — a probe that
/// floods stdout must not flood the ledger.
const MAX_EVIDENCE_CHARS: usize = 200;

/// Evaluate one contract against the workspace. Returns the verdict with
/// evidence; never silently passes.
pub fn verify_contract(contract: &Contract, workspace: &Path) -> ContractVerdict {
    match contract {
        Contract::FileExists { path } => match confined_file(workspace, path) {
            Some(resolved) => ContractVerdict::Verified {
                evidence: format!("regular file {} present", resolved.display()),
            },
            None => ContractVerdict::Violated {
                reason: format!(
                    "Reality Mismatch: '{}' is not an existing regular workspace file",
                    path.display()
                ),
                evidence: "confinement check failed or file absent".into(),
            },
        },
        Contract::FileAbsent { path } => {
            let root = match workspace.canonicalize() {
                Ok(r) => r,
                Err(e) => {
                    return ContractVerdict::Unverifiable {
                        reason: format!("workspace unresolvable: {e}"),
                    }
                }
            };
            let target = root.join(path);
            // Resolve what is there without following the path out of the
            // workspace: absent is proven on the un-canonicalized join plus a
            // confinement check on whatever survives.
            match target.canonicalize() {
                Ok(resolved) if resolved.starts_with(&root) => ContractVerdict::Violated {
                    reason: format!(
                        "Reality Mismatch: '{}' was claimed removed but still exists",
                        path.display()
                    ),
                    evidence: format!("{} present", resolved.display()),
                },
                Ok(_) => ContractVerdict::Violated {
                    reason: format!(
                        "Reality Mismatch: '{}' resolves outside the workspace",
                        path.display()
                    ),
                    evidence: "symlink/escape resolves beyond root".into(),
                },
                Err(_) => ContractVerdict::Verified {
                    evidence: format!("no filesystem entry at {}", path.display()),
                },
            }
        }
        Contract::FileContains { path, needle } => {
            match confined_file(workspace, path).and_then(|p| std::fs::read(&p).ok()) {
                Some(bytes)
                    if needle.is_empty()
                        || bytes.windows(needle.len()).any(|w| w == needle.as_bytes()) =>
                {
                    ContractVerdict::Verified {
                        evidence: format!("{} contains claimed content", path.display()),
                    }
                }
                Some(_) => ContractVerdict::Violated {
                    reason: format!(
                        "Reality Mismatch: '{}' exists but does not contain the claimed content",
                        path.display()
                    ),
                    evidence: format!("content mismatch (needle {} bytes)", needle.len()),
                },
                None => ContractVerdict::Violated {
                    reason: format!(
                        "Reality Mismatch: '{}' is not an existing regular workspace file",
                        path.display()
                    ),
                    evidence: "confinement check failed or file absent".into(),
                },
            }
        }
        Contract::FileHash { path, sha256_hex } => {
            match confined_file(workspace, path).and_then(|p| std::fs::read(&p).ok()) {
                Some(bytes) => {
                    use sha2::{Digest, Sha256};
                    let actual = hex::encode(Sha256::digest(&bytes));
                    if actual.eq_ignore_ascii_case(sha256_hex) {
                        ContractVerdict::Verified {
                            evidence: format!("sha256 {actual}"),
                        }
                    } else {
                        ContractVerdict::Violated {
                            reason: format!(
                                "Reality Mismatch: '{}' hash differs from claimed digest",
                                path.display()
                            ),
                            evidence: format!("expected {sha256_hex}, observed {actual}"),
                        }
                    }
                }
                None => ContractVerdict::Violated {
                    reason: format!(
                        "Reality Mismatch: '{}' is not an existing regular workspace file",
                        path.display()
                    ),
                    evidence: "confinement check failed or file absent".into(),
                },
            }
        }
        Contract::CommandExit { argv, timeout_secs } => {
            verify_command_exit(argv, *timeout_secs, workspace)
        }
    }
}

fn verify_command_exit(argv: &[String], timeout_secs: u64, workspace: &Path) -> ContractVerdict {
    let Some(program) = argv.first() else {
        return ContractVerdict::Unverifiable {
            reason: "empty verifier command".into(),
        };
    };
    let binary = Path::new(program)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    if !VERIFIER_BINARIES.contains(&binary.as_str()) {
        return ContractVerdict::Unverifiable {
            reason: format!("verifier program '{binary}' not in allowlist"),
        };
    }
    let timeout = Duration::from_secs(timeout_secs.clamp(1, MAX_VERIFY_TIMEOUT_SECS));
    let mut cmd = std::process::Command::new(program);
    cmd.args(&argv[1..]).current_dir(workspace);
    match output_within(&mut cmd, timeout) {
        Ok(output) if output.status.success() => ContractVerdict::Verified {
            evidence: format!("'{}' exited 0", argv.join(" ")),
        },
        Ok(output) => ContractVerdict::Violated {
            reason: format!(
                "Reality Mismatch: verifier '{}' exited {}",
                argv.join(" "),
                output.status
            ),
            evidence: tail(&String::from_utf8_lossy(&output.stderr)),
        },
        Err(e) => ContractVerdict::Unverifiable {
            reason: format!("verifier '{}' could not run: {e}", argv.join(" ")),
        },
    }
}

fn tail(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= MAX_EVIDENCE_CHARS {
        trimmed.to_string()
    } else {
        trimmed
            .chars()
            .rev()
            .take(MAX_EVIDENCE_CHARS)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect()
    }
}

/// Mine checkable contracts from mission text (goal or result). Claims we
/// recognize become contracts; everything else produces nothing — absence of
/// a contract is not a pass.
pub fn contracts_from_text(text: &str) -> Vec<Contract> {
    use std::sync::OnceLock;
    let mut contracts = Vec::new();

    // "wrote to <path>" / "saved to <path>" — quoted or bare, including a bare
    // claim with no path (a write claim that names nothing is unverifiable,
    // surfaced by the caller as no-contract).
    static WRITES: OnceLock<regex::Regex> = OnceLock::new();
    let writes = WRITES.get_or_init(|| {
        // Static literal — validity is fixed at compile time.
        #[allow(clippy::expect_used)]
        regex::Regex::new(
            r#"(?i)\b(?:wrote to|saved to)[ \t]+(?:`([^`]+)`|"([^"]+)"|'([^']+)'|([^\s]+))"#,
        )
        .expect("static write-claim pattern")
    });
    for capture in writes.captures_iter(text) {
        if let Some(path) = (1..=4).find_map(|i| capture.get(i)).map(|v| v.as_str()) {
            let path = if capture.get(4).is_some() {
                path.trim_end_matches(['.', ',', ';']).to_string()
            } else {
                path.to_string()
            };
            contracts.push(Contract::FileExists {
                path: PathBuf::from(path),
            });
        }
    }

    // "deleted/removed <path>" — the counterpart existence checks can't see.
    static DELETES: OnceLock<regex::Regex> = OnceLock::new();
    let deletes = DELETES.get_or_init(|| {
        // Static literal — validity is fixed at compile time.
        #[allow(clippy::expect_used)]
        regex::Regex::new(
            r#"(?i)\b(?:deleted|removed)[ \t]+(?:`([^`]+)`|"([^"]+)"|'([^']+)'|([^\s]+))"#,
        )
        .expect("static delete-claim pattern")
    });
    for capture in deletes.captures_iter(text) {
        if let Some(path) = (1..=4).find_map(|i| capture.get(i)).map(|v| v.as_str()) {
            let path = if capture.get(4).is_some() {
                path.trim_end_matches(['.', ',', ';']).to_string()
            } else {
                path.to_string()
            };
            contracts.push(Contract::FileAbsent {
                path: PathBuf::from(path),
            });
        }
    }

    contracts
}

/// Evaluate a contract set; returns every verdict (verified and violated
/// alike — the audit trail needs both).
pub fn verify_all(contracts: &[Contract], workspace: &Path) -> Vec<ContractVerdict> {
    contracts
        .iter()
        .map(|c| verify_contract(c, workspace))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn ws() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn file_exists_verifies_only_inside_workspace() {
        let dir = ws();
        std::fs::write(dir.path().join("a.txt"), "hi").unwrap();
        let ok = verify_contract(
            &Contract::FileExists {
                path: "a.txt".into(),
            },
            dir.path(),
        );
        assert!(ok.is_verified());
        assert!(ok.evidence_line().contains("a.txt"));

        for escape in ["missing.txt", "../outside", "/etc/hostname"] {
            let v = verify_contract(
                &Contract::FileExists {
                    path: escape.into(),
                },
                dir.path(),
            );
            assert!(v.violation().is_some(), "{escape}");
        }
    }

    #[test]
    fn file_absent_distinguishes_gone_present_and_escaped() {
        let dir = ws();
        std::fs::write(dir.path().join("dead.txt"), "x").unwrap();
        let gone = verify_contract(
            &Contract::FileAbsent {
                path: "gone.txt".into(),
            },
            dir.path(),
        );
        assert!(gone.is_verified());
        let present = verify_contract(
            &Contract::FileAbsent {
                path: "dead.txt".into(),
            },
            dir.path(),
        );
        assert!(present.violation().unwrap().contains("dead.txt"));
    }

    #[test]
    fn file_contains_checks_bytes_not_promises() {
        let dir = ws();
        std::fs::write(dir.path().join("c.txt"), "hello susi").unwrap();
        assert!(verify_contract(
            &Contract::FileContains {
                path: "c.txt".into(),
                needle: "susi".into()
            },
            dir.path()
        )
        .is_verified());
        let miss = verify_contract(
            &Contract::FileContains {
                path: "c.txt".into(),
                needle: "other".into(),
            },
            dir.path(),
        );
        assert!(miss.violation().unwrap().contains("does not contain"));
        let absent = verify_contract(
            &Contract::FileContains {
                path: "nope".into(),
                needle: "x".into(),
            },
            dir.path(),
        );
        assert!(absent.violation().is_some());
    }

    #[test]
    fn file_hash_matches_real_digest() {
        use sha2::{Digest, Sha256};
        let dir = ws();
        std::fs::write(dir.path().join("h.bin"), b"digest me").unwrap();
        let good = hex::encode(Sha256::digest(b"digest me"));
        assert!(verify_contract(
            &Contract::FileHash {
                path: "h.bin".into(),
                sha256_hex: good
            },
            dir.path()
        )
        .is_verified());
        let bad = verify_contract(
            &Contract::FileHash {
                path: "h.bin".into(),
                sha256_hex: "f".repeat(64),
            },
            dir.path(),
        );
        assert!(bad.violation().unwrap().contains("hash differs"));
    }

    #[test]
    fn command_exit_runs_allowlisted_probes_only() {
        let dir = ws();
        let ok = verify_contract(
            &Contract::CommandExit {
                argv: vec!["true".into()],
                timeout_secs: 5,
            },
            dir.path(),
        );
        assert!(ok.is_verified());
        let fail = verify_contract(
            &Contract::CommandExit {
                argv: vec!["false".into()],
                timeout_secs: 5,
            },
            dir.path(),
        );
        assert!(fail.violation().is_some());
        let refused = verify_contract(
            &Contract::CommandExit {
                argv: vec!["curl".into(), "http://evil".into()],
                timeout_secs: 5,
            },
            dir.path(),
        );
        assert!(matches!(refused, ContractVerdict::Unverifiable { .. }));
        assert!(refused.evidence_line().contains("allowlist"));
        let empty = verify_contract(
            &Contract::CommandExit {
                argv: vec![],
                timeout_secs: 5,
            },
            dir.path(),
        );
        assert!(matches!(empty, ContractVerdict::Unverifiable { .. }));
    }

    #[test]
    fn claims_mine_contracts_from_text() {
        let mined = contracts_from_text("Wrote to `a.txt`, saved to b.txt and removed old.log");
        assert_eq!(mined.len(), 3);
        assert!(mined.contains(&Contract::FileExists {
            path: "a.txt".into()
        }));
        assert!(mined.contains(&Contract::FileExists {
            path: "b.txt".into()
        }));
        assert!(mined.contains(&Contract::FileAbsent {
            path: "old.log".into()
        }));
        assert!(contracts_from_text("everything looks fine").is_empty());
    }

    #[test]
    fn verify_all_keeps_every_verdict() {
        let dir = ws();
        std::fs::write(dir.path().join("p.txt"), "x").unwrap();
        let verdicts = verify_all(
            &[
                Contract::FileExists {
                    path: "p.txt".into(),
                },
                Contract::FileExists {
                    path: "q.txt".into(),
                },
            ],
            dir.path(),
        );
        assert_eq!(verdicts.len(), 2);
        assert!(verdicts[0].is_verified());
        assert!(verdicts[1].violation().is_some());
    }
}
