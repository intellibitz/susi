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
    /// `argv` allowed by `verifier_policy`, run workspace-confined with a
    /// deadline, must exit 0. The policy exists because a verifier is a
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

/// Programs a `CommandExit` contract may run with any arguments: pure
/// readers that cannot write the workspace or reach the network.
const READ_ONLY_VERIFIERS: &[&str] = &[
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

/// `git` subcommands that only read the repository.
const GIT_READ_SUBCOMMANDS: &[&str] = &["status", "diff", "log", "show", "rev-parse", "ls-files"];

/// `cargo` subcommands a verifier may run: they build and test the
/// workspace's own code (writing only `target/`), never publish or install.
const CARGO_VERIFY_SUBCOMMANDS: &[&str] = &["test", "check", "build", "clippy"];

/// Why `argv` may not run as a verifier, or `None` when it may. `sh`/`bash`
/// used to be allowlisted wholesale, which made `sh -c "rm -rf ."` a valid
/// "probe" and contradicted the no-mutation guarantee above; `git` and
/// `cargo` were allowlisted with any subcommand (`git push`, `cargo
/// install`). The first global argument is the subcommand for both.
fn verifier_policy(argv: &[String]) -> Option<String> {
    let Some(program) = argv.first() else {
        return Some("empty verifier command".into());
    };
    let binary = Path::new(program)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let sub = argv.get(1).map(String::as_str).unwrap_or_default();
    let allowed = match binary.as_str() {
        "git" => GIT_READ_SUBCOMMANDS.contains(&sub),
        "cargo" => {
            CARGO_VERIFY_SUBCOMMANDS.contains(&sub)
                || (sub == "fmt" && argv.iter().any(|a| a == "--check"))
        }
        other => READ_ONLY_VERIFIERS.contains(&other),
    };
    (!allowed).then(|| format!("verifier '{}' not in the probe allowlist", argv.join(" ")))
}

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
            // A deletion claim about a path outside the workspace cannot be
            // vouched for: `root.join` of an absolute path *replaces* the
            // root, and `..` climbs out of it, so a nonexistent outside path
            // used to come back `Verified`.
            if path.is_absolute()
                || path
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
            {
                return ContractVerdict::Unverifiable {
                    reason: format!(
                        "deletion claim '{}' is not workspace-relative",
                        path.display()
                    ),
                };
            }
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
            match confined_file(workspace, path)
                .and_then(|p| stream_contains(&p, needle.as_bytes()))
            {
                Some(true) => ContractVerdict::Verified {
                    evidence: format!("{} contains claimed content", path.display()),
                },
                Some(false) => ContractVerdict::Violated {
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
            // A claimed digest that is not 64 hex chars (truncated or garbled
            // model output) cannot be checked; "hash differs" would blame the
            // workspace for a malformed claim.
            if sha256_hex.len() != 64 || !sha256_hex.chars().all(|c| c.is_ascii_hexdigit()) {
                return ContractVerdict::Unverifiable {
                    reason: format!("claimed sha256 '{sha256_hex}' is not a 64-char hex digest"),
                };
            }
            match confined_file(workspace, path).and_then(|p| stream_sha256(&p)) {
                Some(actual) => {
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

/// Chunk size for streamed content checks. `FileContains` / `FileHash`
/// used to `std::fs::read` the whole file, so a claim about a multi-GB
/// artifact made the verifier allocate all of it.
const STREAM_CHUNK: usize = 64 * 1024;

/// Whether the file contains `needle`, reading it in chunks and carrying the
/// last `needle.len() - 1` bytes over so matches spanning a chunk boundary
/// are found. `None` if the file cannot be read.
fn stream_contains(path: &Path, needle: &[u8]) -> Option<bool> {
    use std::io::Read;
    if needle.is_empty() {
        return std::fs::metadata(path).ok().map(|_| true);
    }
    let mut file = std::fs::File::open(path).ok()?;
    let mut window: Vec<u8> = Vec::with_capacity(STREAM_CHUNK + needle.len());
    let mut chunk = vec![0u8; STREAM_CHUNK];
    loop {
        let read = file.read(&mut chunk).ok()?;
        if read == 0 {
            return Some(false);
        }
        window.extend_from_slice(&chunk[..read]);
        if window.windows(needle.len()).any(|w| w == needle) {
            return Some(true);
        }
        let keep = needle.len() - 1;
        if window.len() > keep {
            window.drain(..window.len() - keep);
        }
    }
}

/// Lowercase hex SHA-256 of the file, streamed. `None` if unreadable.
fn stream_sha256(path: &Path) -> Option<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut file = std::fs::File::open(path).ok()?;
    let mut hasher = Sha256::new();
    let mut chunk = vec![0u8; STREAM_CHUNK];
    loop {
        let read = file.read(&mut chunk).ok()?;
        if read == 0 {
            return Some(hex::encode(hasher.finalize()));
        }
        hasher.update(&chunk[..read]);
    }
}

fn verify_command_exit(argv: &[String], timeout_secs: u64, workspace: &Path) -> ContractVerdict {
    if let Some(reason) = verifier_policy(argv) {
        return ContractVerdict::Unverifiable { reason };
    }
    let Some(program) = argv.first() else {
        return ContractVerdict::Unverifiable {
            reason: "empty verifier command".into(),
        };
    };
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

/// A bare (unquoted) path is the token after the claim verb, so it carries
/// whatever prose punctuation follows it. Only `.,;` used to be trimmed:
/// "I saved to notes.txt:" mined `notes.txt:` (a truthful claim failed
/// verification) and "removed old.log)" mined `old.log)` — a path that is
/// always absent, so the deletion "verified" even with `old.log` present.
fn trim_bare_path(path: &str) -> String {
    path.trim_start_matches(['(', '[', '{'])
        .trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '}', '"', '\'', '`'])
        .to_string()
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
                trim_bare_path(path)
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
                trim_bare_path(path)
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
    fn file_absent_never_vouches_for_paths_outside_the_workspace() {
        let dir = ws();
        for outside in [
            "/nonexistent/susi-verify-probe",
            "../susi-verify-probe-sibling",
            "sub/../../escape",
        ] {
            let v = verify_contract(
                &Contract::FileAbsent {
                    path: outside.into(),
                },
                dir.path(),
            );
            assert!(
                matches!(v, ContractVerdict::Unverifiable { .. }),
                "{outside}: {v:?}"
            );
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
    fn verifier_policy_refuses_shells_and_mutating_subcommands() {
        let argv = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        for refused in [
            argv(&["sh", "-c", "rm -rf ."]),
            argv(&["bash", "-c", "true"]),
            argv(&["/bin/sh", "-c", "true"]),
            argv(&["git", "push", "origin", "main"]),
            argv(&["git", "checkout", "."]),
            argv(&["git"]),
            argv(&["cargo", "install", "anything"]),
            argv(&["cargo", "publish"]),
            argv(&["cargo", "fmt"]),
            argv(&["curl", "http://example.test"]),
        ] {
            assert!(
                verifier_policy(&refused).is_some(),
                "{refused:?} must be refused"
            );
        }
        for allowed in [
            argv(&["git", "diff", "--quiet"]),
            argv(&["git", "status", "--porcelain"]),
            argv(&["cargo", "test", "--locked"]),
            argv(&["cargo", "fmt", "--all", "--check"]),
            argv(&["grep", "-q", "needle", "file"]),
            argv(&["/usr/bin/true"]),
        ] {
            assert!(
                verifier_policy(&allowed).is_none(),
                "{allowed:?} must be allowed"
            );
        }
        // Refusal happens before any process spawns.
        let dir = ws();
        let marker = dir.path().join("marker");
        let verdict = verify_contract(
            &Contract::CommandExit {
                argv: vec![
                    "sh".into(),
                    "-c".into(),
                    format!("touch {}", marker.display()),
                ],
                timeout_secs: 5,
            },
            dir.path(),
        );
        assert!(matches!(verdict, ContractVerdict::Unverifiable { .. }));
        assert!(!marker.exists(), "a refused verifier must never run");
    }

    #[test]
    fn bare_claim_paths_shed_surrounding_prose_punctuation() {
        let exists = |p: &str| Contract::FileExists { path: p.into() };
        for (text, path) in [
            ("I saved to notes.txt: done", "notes.txt"),
            ("(saved to out/report.md)", "out/report.md"),
            ("I wrote to report.md!", "report.md"),
            ("wrote to data.json), then stopped", "data.json"),
            ("saved to x.md?", "x.md"),
        ] {
            assert_eq!(contracts_from_text(text), [exists(path)], "{text}");
        }
        // The deletion side: a trailing ')' must not turn a present file
        // into an always-absent path that "verifies".
        let dir = ws();
        std::fs::write(dir.path().join("old.log"), "x").unwrap();
        let mined = contracts_from_text("Done (removed old.log)");
        assert_eq!(
            mined,
            [Contract::FileAbsent {
                path: "old.log".into()
            }]
        );
        assert!(verify_contract(&mined[0], dir.path()).violation().is_some());
        // Quoted paths are taken verbatim.
        assert_eq!(
            contracts_from_text("wrote to `a b.txt`."),
            [exists("a b.txt")]
        );
    }

    #[test]
    fn streamed_checks_find_boundary_spanning_content_and_match_digests() {
        let dir = ws();
        let path = dir.path().join("big.bin");
        // Needle straddles the first chunk boundary.
        let mut body = vec![b'a'; STREAM_CHUNK - 3];
        body.extend_from_slice(b"NEEDLE");
        body.extend(vec![b'b'; STREAM_CHUNK * 2]);
        std::fs::write(&path, &body).unwrap();
        assert_eq!(stream_contains(&path, b"NEEDLE"), Some(true));
        assert_eq!(stream_contains(&path, b"MISSING"), Some(false));
        assert_eq!(stream_contains(&path, b""), Some(true));
        assert_eq!(stream_contains(&dir.path().join("absent"), b"x"), None);

        use sha2::{Digest, Sha256};
        assert_eq!(
            stream_sha256(&path).unwrap(),
            hex::encode(Sha256::digest(&body))
        );
    }

    #[test]
    fn malformed_claimed_digests_are_unverifiable_not_violations() {
        let dir = ws();
        std::fs::write(dir.path().join("f.txt"), "x").unwrap();
        for bad in ["abc123", "", &"z".repeat(64), &"a".repeat(65)] {
            let v = verify_contract(
                &Contract::FileHash {
                    path: "f.txt".into(),
                    sha256_hex: bad.to_string(),
                },
                dir.path(),
            );
            assert!(
                matches!(v, ContractVerdict::Unverifiable { .. }),
                "{bad:?}: {v:?}"
            );
        }
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
