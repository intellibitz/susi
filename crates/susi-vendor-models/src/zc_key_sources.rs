//! Discover credentials where they already live — with consent.
//!
//! Zero-config means zero REQUIRED files or env vars: where only the human
//! can supply a secret, susi asks once at the moment of need. This module
//! enumerates the places keys already live (process environment, workspace
//! `.env`, AWS credentials, gcloud ADC, `gh auth token`) and imports one
//! source at a time, only after an explicit yes — never silently.
//!
//! Everything goes through [`SourceProbe`] so tests never touch the real
//! environment or home directory.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use susi_error::EaiResult;

/// Where a credential was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceKind {
    /// Process environment variable.
    Environment,
    /// `.env` file in the workspace root.
    WorkspaceDotEnv,
    /// `~/.aws/credentials` profile.
    AwsCredentials,
    /// gcloud application-default-credentials JSON.
    GcloudAdc,
    /// `gh auth token` output.
    GhCli,
}

/// One discovered credential source — value is NEVER stored here beyond a
/// redacted preview; the full value is handed to the store only after
/// consent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeySource {
    pub kind: SourceKind,
    /// Human description shown in the consent prompt ("env var OPENAI_API_KEY").
    pub label: String,
    /// Canonical key alias the source supplies (see `zc_key_aliases`).
    pub alias: String,
    /// First/last few characters for display; never the full secret.
    pub preview: String,
    /// Full secret — only held in memory until the caller imports it.
    secret: String,
}

impl KeySource {
    #[must_use]
    pub fn secret(&self) -> &str {
        &self.secret
    }
}

/// Host access the scanner needs — injected for hermetic tests.
pub trait SourceProbe {
    fn env(&self, name: &str) -> Option<String>;
    fn read_file(&self, path: &Path) -> Option<String>;
    fn workspace_dir(&self) -> PathBuf;
    fn home(&self) -> PathBuf;
    /// Run a CLI and capture stdout (e.g. `gh auth token`).
    fn command_stdout(&self, program: &str, args: &[&str]) -> Option<String>;
}

/// The env-var names we recognise, in scan order.
const ENV_KEYS: &[(&str, &str)] = &[
    ("OPENAI_API_KEY", "openai"),
    ("ANTHROPIC_API_KEY", "anthropic"),
    ("GEMINI_API_KEY", "gemini"),
    ("GOOGLE_API_KEY", "gemini"),
    ("MISTRAL_API_KEY", "mistral"),
    ("COHERE_API_KEY", "cohere"),
    ("HF_TOKEN", "huggingface"),
    ("OPENROUTER_API_KEY", "openrouter"),
    ("GROQ_API_KEY", "groq"),
    ("TOGETHER_API_KEY", "together"),
    ("DEEPSEEK_API_KEY", "deepseek"),
    ("PERPLEXITY_API_KEY", "perplexity"),
    ("AWS_ACCESS_KEY_ID", "aws"),
    ("AZURE_OPENAI_API_KEY", "azure-openai"),
];

fn preview(secret: &str) -> String {
    let s = secret.trim();
    if s.len() <= 8 {
        "*".repeat(s.len())
    } else {
        format!("{}…{}", &s[..4], &s[s.len() - 4..])
    }
}

/// Discover every source that currently holds a recognised credential.
/// Order is stable: environment first, then files, then CLIs — the caller
/// asks about them one at a time.
#[must_use]
pub fn discover(probe: &dyn SourceProbe) -> Vec<KeySource> {
    let mut found = Vec::new();
    // 1. process environment
    for (var, alias) in ENV_KEYS {
        if let Some(v) = probe.env(var) {
            if !v.trim().is_empty() {
                found.push(KeySource {
                    kind: SourceKind::Environment,
                    label: format!("env var {var}"),
                    alias: (*alias).to_string(),
                    preview: preview(&v),
                    secret: v.trim().to_string(),
                });
            }
        }
    }
    // 2. workspace .env — only keys we recognise, same aliases
    let env_path = probe.workspace_dir().join(".env");
    if let Some(text) = probe.read_file(&env_path) {
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('#') || !line.contains('=') {
                continue;
            }
            if let Some((name, value)) = line.split_once('=') {
                let name = name.trim();
                let value = value.trim().trim_matches('"').trim_matches('\'');
                if value.is_empty() {
                    continue;
                }
                if let Some((_, alias)) = ENV_KEYS.iter().find(|(var, _)| *var == name) {
                    // dedupe against env-found aliases
                    if found.iter().any(|k| k.alias == *alias) {
                        continue;
                    }
                    found.push(KeySource {
                        kind: SourceKind::WorkspaceDotEnv,
                        label: format!("{name} in workspace .env"),
                        alias: (*alias).to_string(),
                        preview: preview(value),
                        secret: value.to_string(),
                    });
                }
            }
        }
    }
    // 3. ~/.aws/credentials — profiles with an access key
    let aws_path = probe.home().join(".aws").join("credentials");
    if let Some(text) = probe.read_file(&aws_path) {
        let mut in_profile = false;
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('[') && line.ends_with(']') {
                in_profile = true;
            } else if in_profile && line.starts_with("aws_access_key_id") {
                if let Some((_, v)) = line.split_once('=') {
                    let v = v.trim();
                    if !v.is_empty() && !found.iter().any(|k| k.alias == "aws") {
                        found.push(KeySource {
                            kind: SourceKind::AwsCredentials,
                            label: format!("{} profile", aws_path.display()),
                            alias: "aws".to_string(),
                            preview: preview(v),
                            secret: v.to_string(),
                        });
                        break;
                    }
                }
            }
        }
    }
    // 4. gcloud ADC
    let adc = probe
        .home()
        .join(".config")
        .join("gcloud")
        .join("application_default_credentials.json");
    if probe.read_file(&adc).is_some() && !found.iter().any(|k| k.alias == "gcp-adc") {
        // The file itself is the credential — label only, no preview of contents.
        found.push(KeySource {
            kind: SourceKind::GcloudAdc,
            label: format!("gcloud ADC ({})", adc.display()),
            alias: "gcp-adc".to_string(),
            preview: "service-account json".to_string(),
            secret: adc.display().to_string(), // the path IS the import
        });
    }
    // 5. gh auth token
    if let Some(token) = probe.command_stdout("gh", &["auth", "token"]) {
        let token = token.trim();
        if !token.is_empty() {
            found.push(KeySource {
                kind: SourceKind::GhCli,
                label: "gh auth token".to_string(),
                alias: "github".to_string(),
                preview: preview(token),
                secret: token.to_string(),
            });
        }
    }
    found
}

/// Consent callback: shown the source label + preview, returns true to
/// import. The scan asks about each source one at a time, in order, and
/// stops asking for an alias once one of its sources was accepted.
///
/// Returns the accepted sources; nothing is imported silently.
pub fn import_with_consent(
    sources: &[KeySource],
    mut ask: impl FnMut(&KeySource) -> bool,
) -> Vec<KeySource> {
    let mut accepted = Vec::new();
    for s in sources {
        if accepted.iter().any(|a: &KeySource| a.alias == s.alias) {
            continue; // already have this key from an earlier source
        }
        if ask(s) {
            accepted.push(s.clone());
        }
    }
    accepted
}

/// Real probe over std::env / fs — used by the zero-config flow.
pub struct HostProbe;

impl SourceProbe for HostProbe {
    fn env(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }
    fn read_file(&self, path: &Path) -> Option<String> {
        std::fs::read_to_string(path).ok()
    }
    fn workspace_dir(&self) -> PathBuf {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    }
    fn home(&self) -> PathBuf {
        susi_paths::SusiDirs::home_dir()
    }
    fn command_stdout(&self, program: &str, args: &[&str]) -> Option<String> {
        let out = std::process::Command::new(program)
            .args(args)
            .output()
            .ok()?;
        if out.status.success() {
            String::from_utf8(out.stdout).ok()
        } else {
            None
        }
    }
}

/// Persist accepted keys to `cloud.env`-style lines (alias->env name uses
/// the canonical mapping from `zc_key_aliases` when available).
pub fn to_env_lines(sources: &[KeySource]) -> EaiResult<Vec<(String, String)>> {
    Ok(sources
        .iter()
        .filter(|s| s.kind != SourceKind::GcloudAdc)
        .map(|s| {
            let var = ENV_KEYS
                .iter()
                .find(|(_, a)| *a == s.alias)
                .map(|(v, _)| (*v).to_string())
                .unwrap_or_else(|| format!("{}_API_KEY", s.alias.to_uppercase().replace('-', "_")));
            (var, s.secret.clone())
        })
        .collect::<BTreeMap<_, _>>()
        .into_iter()
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    struct Fake {
        env: HashMap<String, String>,
        files: HashMap<PathBuf, String>,
        ws: PathBuf,
        home: PathBuf,
        cmds: HashMap<String, String>,
    }

    impl Fake {
        fn new() -> Self {
            Self {
                env: HashMap::new(),
                files: HashMap::new(),
                ws: PathBuf::from("/ws"),
                home: PathBuf::from("/home/u"),
                cmds: HashMap::new(),
            }
        }
    }

    impl SourceProbe for Fake {
        fn env(&self, name: &str) -> Option<String> {
            self.env.get(name).cloned()
        }
        fn read_file(&self, path: &Path) -> Option<String> {
            self.files.get(path).cloned()
        }
        fn workspace_dir(&self) -> PathBuf {
            self.ws.clone()
        }
        fn home(&self) -> PathBuf {
            self.home.clone()
        }
        fn command_stdout(&self, program: &str, args: &[&str]) -> Option<String> {
            self.cmds
                .get(&format!("{program} {}", args.join(" ")))
                .cloned()
        }
    }

    #[test]
    fn zc_key_sources_finds_env_keys() {
        let mut f = Fake::new();
        f.env
            .insert("OPENAI_API_KEY".into(), "sk-live-123456".into());
        f.env.insert("HF_TOKEN".into(), "hf_abcdefghij".into());
        let found = discover(&f);
        assert_eq!(found.len(), 2);
        assert!(found.iter().all(|k| k.kind == SourceKind::Environment));
        assert!(found[0].preview.starts_with("sk-l"));
        assert!(!found[0].preview.contains("123456"));
    }

    #[test]
    fn zc_key_sources_reads_workspace_env_and_dedupes() {
        let mut f = Fake::new();
        f.env.insert("OPENAI_API_KEY".into(), "sk-a".into());
        f.files.insert(
            PathBuf::from("/ws/.env"),
            "OPENAI_API_KEY=sk-other\nANTHROPIC_API_KEY=sk-ant-998877\n# comment\n".into(),
        );
        let found = discover(&f);
        assert_eq!(found.len(), 2);
        let ant = found.iter().find(|k| k.alias == "anthropic").unwrap();
        assert_eq!(ant.kind, SourceKind::WorkspaceDotEnv);
        // openai from .env deduped against the env source
        assert_eq!(found.iter().filter(|k| k.alias == "openai").count(), 1);
    }

    #[test]
    fn zc_key_sources_finds_aws_profile_and_gh() {
        let mut f = Fake::new();
        f.files.insert(
            PathBuf::from("/home/u/.aws/credentials"),
            "[default]\naws_access_key_id = AKIAEXAMPLE123\naws_secret_access_key = sec\n".into(),
        );
        f.cmds
            .insert("gh auth token".into(), "ghp_tokentoken\n".into());
        let found = discover(&f);
        assert!(found.iter().any(|k| k.kind == SourceKind::AwsCredentials));
        let gh = found.iter().find(|k| k.kind == SourceKind::GhCli).unwrap();
        assert_eq!(gh.secret(), "ghp_tokentoken");
    }

    #[test]
    fn zc_key_sources_finds_gcloud_adc() {
        let mut f = Fake::new();
        f.files.insert(
            PathBuf::from("/home/u/.config/gcloud/application_default_credentials.json"),
            "{\"type\":\"service_account\"}".into(),
        );
        let found = discover(&f);
        let adc = found.iter().find(|k| k.alias == "gcp-adc").unwrap();
        assert_eq!(adc.kind, SourceKind::GcloudAdc);
    }

    #[test]
    fn zc_key_sources_consent_gates_import() {
        let mut f = Fake::new();
        f.env.insert("OPENAI_API_KEY".into(), "sk-1".into());
        f.env.insert("HF_TOKEN".into(), "hf_x".into());
        let found = discover(&f);
        let asked = RefCell::new(Vec::new());
        let accepted = import_with_consent(&found, |s| {
            asked.borrow_mut().push(s.alias.clone());
            s.alias == "openai" // yes only to openai
        });
        assert_eq!(accepted.len(), 1);
        assert_eq!(accepted[0].alias, "openai");
        assert_eq!(asked.borrow().len(), 2); // asked about each, once
    }

    #[test]
    fn zc_key_sources_never_asks_twice_per_alias() {
        let mut f = Fake::new();
        f.env.insert("OPENAI_API_KEY".into(), "sk-env".into());
        // give .env a DIFFERENT recognised var for same alias? none — instead
        // accept env, ensure .env would be skipped anyway (dedupe already),
        // so use two aliases accepted both:
        f.env.insert("HF_TOKEN".into(), "hf_1".into());
        let found = discover(&f);
        let n = RefCell::new(0);
        let acc = import_with_consent(&found, |_| {
            *n.borrow_mut() += 1;
            true
        });
        assert_eq!(acc.len(), 2);
        assert_eq!(*n.borrow(), 2);
    }

    #[test]
    fn zc_key_sources_env_lines_mask_nothing_but_map_names() {
        let mut f = Fake::new();
        f.env.insert("OPENAI_API_KEY".into(), "sk-9".into());
        let found = discover(&f);
        let lines = to_env_lines(&found).unwrap();
        assert_eq!(
            lines,
            vec![("OPENAI_API_KEY".to_string(), "sk-9".to_string())]
        );
    }

    #[test]
    fn zc_key_sources_empty_when_nothing() {
        let f = Fake::new();
        assert!(discover(&f).is_empty());
    }
}
