//! Admission for model weights. Cloud inference calls do not distribute weights
//! and are deliberately outside this policy.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{io::Read, path::Path};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublisherLicense {
    pub repository: String,
    pub revision: String,
    pub license: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelLicenseReceipt {
    pub artifact_url: String,
    pub sha256: String,
    pub publishers: Vec<PublisherLicense>,
}
fn authenticated(
) -> &'static std::sync::Mutex<std::collections::BTreeMap<String, ModelLicenseReceipt>> {
    static CACHE: std::sync::OnceLock<
        std::sync::Mutex<std::collections::BTreeMap<String, ModelLicenseReceipt>>,
    > = std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::BTreeMap::new()))
}
fn remember(receipt: &ModelLicenseReceipt) {
    let mut cache = authenticated().lock().unwrap_or_else(|e| e.into_inner());
    if cache.len() >= 128 {
        cache.clear();
    }
    cache.insert(receipt.artifact_url.clone(), receipt.clone());
}
fn verify_requested_revision(requested: &str, actual: &str) -> Result<(), String> {
    if hex(requested, 40) && !requested.eq_ignore_ascii_case(actual) {
        return Err("[MODEL LICENSE] Publisher returned a different immutable revision".into());
    }
    Ok(())
}
fn permitted(license: &str) -> bool {
    matches!(
        license.to_ascii_lowercase().as_str(),
        "apache-2.0" | "mit" | "bsd-2-clause" | "bsd-3-clause"
    )
}
fn component(s: &str) -> bool {
    !s.is_empty()
        && s != "."
        && s != ".."
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}
fn repository(s: &str) -> bool {
    let parts: Vec<_> = s.split('/').collect();
    parts.len() == 2 && parts.iter().all(|p| component(p))
}
fn hex(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| b.is_ascii_hexdigit())
}
fn artifact(url: &str) -> Result<(&str, &str, &str), String> {
    let path = url.strip_prefix("https://huggingface.co/").ok_or("[MODEL LICENSE] Only authenticated Hugging Face publisher provenance is currently supported for downloaded weights")?;
    let (repo, tail) = path
        .split_once("/resolve/")
        .ok_or("[MODEL LICENSE] Expected repository/resolve/revision/file URL")?;
    let (revision, file) = tail
        .split_once('/')
        .ok_or("[MODEL LICENSE] Missing artifact revision/file")?;
    if !repository(repo) || !component(revision) || !file.split('/').all(component) {
        return Err("[MODEL LICENSE] Invalid publisher artifact URL".into());
    }
    Ok((repo, revision, file))
}
fn publisher(repo: &str, metadata: &Value) -> Result<(PublisherLicense, Vec<String>), String> {
    let revision = metadata
        .get("sha")
        .and_then(Value::as_str)
        .filter(|s| hex(s, 40))
        .ok_or("[MODEL LICENSE] Publisher revision missing")?;
    let card = metadata
        .get("cardData")
        .ok_or("[MODEL LICENSE] Publisher model card missing")?;
    let license = card.get("license").and_then(Value::as_str).filter(|s| permitted(s)).ok_or_else(|| format!("[MODEL LICENSE] {repo} has unknown or incompatible weight license; Susi accepts Apache-2.0, MIT, BSD-2-Clause and BSD-3-Clause"))?;
    let bases = match card.get("base_model") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(values)) => values
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .ok_or("[MODEL LICENSE] Unsupported base-model provenance".to_string())
            })
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => return Err("[MODEL LICENSE] Unsupported base-model provenance".into()),
    };
    if bases.iter().any(|s| !repository(s)) {
        return Err("[MODEL LICENSE] Invalid base-model repository".into());
    }
    Ok((
        PublisherLicense {
            repository: repo.into(),
            revision: revision.into(),
            license: license.into(),
        },
        bases,
    ))
}
fn fetch(repo: &str, revision: &str) -> Result<Value, String> {
    fetch_with_policy(
        repo,
        revision,
        &crate::susi_core::mac_policy::egress_permitted,
    )
}
fn fetch_with_policy(
    repo: &str,
    revision: &str,
    egress_allowed: &dyn Fn(&str) -> bool,
) -> Result<Value, String> {
    let url = format!("https://huggingface.co/api/models/{repo}/revision/{revision}?blobs=true");
    if !egress_allowed(&url) {
        return Err(
            "[PRIVACY] Model license verification requires permitted publisher access".into(),
        );
    }
    let token = crate::susi_config::env_or_cloud_env("HF_TOKEN").ok();
    let authorization = token.map(|t| format!("Bearer {t}"));
    let headers: Vec<_> = authorization
        .as_deref()
        .map(|t| vec![("Authorization", t)])
        .unwrap_or_default();
    let response =
        susi_http_transport::http_call("GET", &url, &headers, 15, 0).map_err(|e| e.to_string())?;
    if response.status != 200 {
        return Err(format!(
            "[MODEL LICENSE] Publisher metadata unavailable (HTTP {})",
            response.status
        ));
    }
    serde_json::from_slice(
        &response
            .into_bytes(4 * 1024 * 1024)
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}
/// Resolve a mutable branch once, then bind the allowed license and digest to
/// its immutable revision. No weight bytes are downloaded by this function.
pub fn admit_download(url: &str) -> Result<ModelLicenseReceipt, String> {
    let (repo, revision, file) = artifact(url)?;
    let metadata = fetch(repo, revision)?;
    let (first, mut bases) = publisher(repo, &metadata)?;
    verify_requested_revision(revision, &first.revision)?;
    let artifact_url = format!(
        "https://huggingface.co/{repo}/resolve/{}/{file}",
        first.revision
    );
    let mut publishers = vec![first];
    while let Some(base) = bases.pop() {
        if publishers.iter().any(|p| p.repository == base) {
            continue;
        }
        if publishers.len() >= 16 {
            return Err("[MODEL LICENSE] Base-model lineage exceeds verification limit".into());
        }
        let (entry, ancestors) = publisher(&base, &fetch(&base, "main")?)?;
        publishers.push(entry);
        bases.extend(ancestors);
    }
    let published_digest = metadata
        .get("siblings")
        .and_then(Value::as_array)
        .and_then(|files| {
            files
                .iter()
                .find(|v| v.get("rfilename").and_then(Value::as_str) == Some(file))
        })
        .and_then(|v| v.get("lfs"))
        .and_then(|v| v.get("sha256"))
        .and_then(Value::as_str)
        .filter(|s| hex(s, 64));
    let sha256 = if let Some(digest) = published_digest {
        digest.to_owned()
    } else if matches!(
        file.rsplit('/').next(),
        Some(
            "tokenizer.json" | "tokenizer_config.json" | "config.json" | "special_tokens_map.json"
        )
    ) {
        // Small Git-managed companions have a Git SHA-1 rather than an LFS
        // SHA-256. Fetch only from the verified immutable publisher revision,
        // cap their size, and bind the receipt to the exact companion bytes.
        let git_blob = metadata
            .get("siblings")
            .and_then(Value::as_array)
            .and_then(|files| {
                files
                    .iter()
                    .find(|v| v.get("rfilename").and_then(Value::as_str) == Some(file))
            })
            .and_then(|v| v.get("blobId"))
            .and_then(Value::as_str)
            .filter(|s| hex(s, 40))
            .ok_or("[MODEL LICENSE] Publisher Git blob identity missing")?;
        let raw_url = artifact_url.replacen("/resolve/", "/raw/", 1);
        if !crate::susi_core::mac_policy::egress_permitted(&raw_url) {
            return Err("[PRIVACY] Companion download denied".into());
        }
        // Raw Git objects use no redirects. The authenticated Git blob ID
        // independently binds the exact returned bytes to publisher metadata.
        let response = susi_http_transport::http_call("GET", &raw_url, &[], 15, 0)
            .map_err(|e| e.to_string())?;
        if response.status != 200 {
            return Err("[MODEL LICENSE] Publisher companion unavailable or redirected".into());
        }
        let bytes = response
            .into_bytes(8 * 1024 * 1024)
            .map_err(|e| e.to_string())?;
        let mut git_hash = sha1::Sha1::new();
        git_hash.update(format!("blob {}\0", bytes.len()).as_bytes());
        git_hash.update(&bytes);
        if hex::encode(git_hash.finalize()) != git_blob.to_ascii_lowercase() {
            return Err(
                "[MODEL LICENSE] Companion differs from publisher Git blob identity".into(),
            );
        }
        hex::encode(Sha256::digest(bytes))
    } else {
        return Err("[MODEL LICENSE] Publisher SHA-256 missing for model weights".into());
    };
    let receipt = ModelLicenseReceipt {
        artifact_url,
        sha256,
        publishers,
    };
    remember(&receipt);
    Ok(receipt)
}
impl ModelLicenseReceipt {
    fn authenticate(&self, allow_lookup: bool) -> Result<(), String> {
        self.validate()?;
        // This exact artifact and publisher license were audited for the
        // built-in CPU starter. It remains usable without network access.
        const MODEL_URL: &str = "https://huggingface.co/Qwen/Qwen2.5-0.5B-Instruct-GGUF/resolve/9217f5db79a29953eb74d5343926648285ec7e67/qwen2.5-0.5b-instruct-q4_0.gguf";
        const TOKENIZER_URL: &str = "https://huggingface.co/Qwen/Qwen2.5-0.5B-Instruct/resolve/7ae557604adf67be50417f59c2c2f167def9a775/tokenizer.json";
        if self.artifact_url == MODEL_URL || self.artifact_url == TOKENIZER_URL {
            let mut audited = vec![
                (
                    "Qwen/Qwen2.5-0.5B-Instruct",
                    "7ae557604adf67be50417f59c2c2f167def9a775",
                ),
                (
                    "Qwen/Qwen2.5-0.5B",
                    "060db6499f32faf8b98477b0a26969ef7d8b9987",
                ),
            ];
            let digest = if self.artifact_url == MODEL_URL {
                audited.insert(
                    0,
                    (
                        "Qwen/Qwen2.5-0.5B-Instruct-GGUF",
                        "9217f5db79a29953eb74d5343926648285ec7e67",
                    ),
                );
                "7671c0c304e6ce5a7fc577bcb12aba01e2c155cc2efd29b2213c95b18edaf6ed"
            } else {
                "c0382117ea329cdf097041132f6d735924b697924d6f6fc3945713e96ce87539"
            };
            return if self.sha256 == digest
                && self.publishers.len() == audited.len()
                && self
                    .publishers
                    .iter()
                    .zip(audited)
                    .all(|(p, (repo, revision))| {
                        p.repository == repo && p.revision == revision && p.license == "apache-2.0"
                    })
            {
                Ok(())
            } else {
                Err("[MODEL LICENSE] Receipt disagrees with the audited starter evidence".into())
            };
        }
        let prior = authenticated()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&self.artifact_url)
            .cloned();
        if let Some(prior) = prior {
            return if prior == *self {
                Ok(())
            } else {
                Err("[MODEL LICENSE] Receipt differs from authenticated publisher evidence".into())
            };
        }
        if !allow_lookup {
            return Err(
                "[MODEL LICENSE] Receipt has not been authenticated against publisher evidence"
                    .into(),
            );
        }
        let canonical = admit_download(&self.artifact_url)?;
        if canonical != *self {
            return Err(
                "[MODEL LICENSE] Receipt differs from authenticated publisher evidence".into(),
            );
        }
        Ok(())
    }
    pub fn validate(&self) -> Result<(), String> {
        let (repo, revision, _) = artifact(&self.artifact_url)?;
        if !hex(&self.sha256, 64)
            || self.publishers.is_empty()
            || self.publishers.len() > 16
            || self.publishers.iter().any(|p| {
                !repository(&p.repository) || !hex(&p.revision, 40) || !permitted(&p.license)
            })
            || self.publishers[0].repository != repo
            || self.publishers[0].revision != revision
        {
            return Err("[MODEL LICENSE] Invalid or incompatible model license receipt".into());
        }
        Ok(())
    }
    pub fn record(&self, path: &Path) -> Result<(), String> {
        self.authenticate(false)?;
        self.verify_bytes(path)?;
        crate::susi_config::atomic_write_json_pretty(&path.with_extension("license.json"), self)
            .map_err(|e| e.to_string())
    }
    fn verify_bytes(&self, path: &Path) -> Result<(), String> {
        self.validate()?;
        let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
        let mut hash = Sha256::new();
        let mut chunk = [0u8; 64 * 1024];
        loop {
            let n = file.read(&mut chunk).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            hash.update(&chunk[..n]);
        }
        if hex::encode(hash.finalize()) != self.sha256.to_ascii_lowercase() {
            return Err(
                "[MODEL LICENSE] Weight bytes do not match publisher license receipt".into(),
            );
        }
        Ok(())
    }
}
/// User-owned files are never deleted or changed when provenance is unknown.
pub fn verify_local(path: &Path) -> Result<(), String> {
    let receipt_path = path.with_extension("license.json");
    if !receipt_path.exists() {
        // Migrate trusted older installs automatically. A filename or source
        // hint is not evidence: admission still verifies publisher metadata
        // and the full content digest before recording anything.
        let old_source = std::fs::File::open(path.with_extension("provenance.json"))
            .ok()
            .and_then(|file| {
                let mut bytes = Vec::new();
                file.take(64 * 1024 + 1).read_to_end(&mut bytes).ok()?;
                (bytes.len() <= 64 * 1024).then_some(bytes)
            })
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .and_then(|value| {
                value
                    .get("source_url")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            });
        let fallback = crate::susi_sandbox::manager::SusiConfig::load_global()
            .unwrap_or_default()
            .default_fallback_model();
        let source = old_source.or_else(|| {
            (path.file_name().and_then(|s| s.to_str()) == Some(fallback.hf_file.as_str())).then(
                || {
                    format!(
                        "https://huggingface.co/{}/resolve/main/{}",
                        fallback.hf_repo, fallback.hf_file
                    )
                },
            )
        });
        if let Some(source) = source {
            return admit_download(&source)?.record(path);
        }
        return Err("[MODEL LICENSE] This local model has no verified Apache/MIT/BSD publisher receipt. Import it from its publisher URL so Susi can verify its license automatically; the existing file is unchanged.".into());
    }
    let file = std::fs::File::open(receipt_path).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    file.take(64 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 64 * 1024 {
        return Err("[MODEL LICENSE] Oversized license receipt".into());
    }
    let receipt: ModelLicenseReceipt = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    receipt.authenticate(true)?;
    receipt.verify_bytes(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "explicit publisher metadata qualification; no weight download"]
    fn default_qwen_publisher_license_and_hash_are_verified() {
        let receipt = admit_download("https://huggingface.co/Qwen/Qwen2.5-0.5B-Instruct-GGUF/resolve/main/qwen2.5-0.5b-instruct-q4_0.gguf").unwrap();
        receipt.validate().unwrap();
        assert_eq!(
            receipt.sha256,
            "7671c0c304e6ce5a7fc577bcb12aba01e2c155cc2efd29b2213c95b18edaf6ed"
        );
        assert!(receipt.publishers.len() >= 2);
        receipt.authenticate(false).unwrap();
        let tokenizer = admit_download("https://huggingface.co/Qwen/Qwen2.5-0.5B-Instruct/resolve/7ae557604adf67be50417f59c2c2f167def9a775/tokenizer.json").unwrap();
        tokenizer.authenticate(false).unwrap();
        assert!(receipt.publishers.iter().all(|p| p.license == "apache-2.0"));
    }
    #[test]
    fn licenses_and_lineage_fail_closed() {
        assert!(fetch_with_policy("org/model", "main", &|_| false)
            .unwrap_err()
            .contains("PRIVACY"));
        for license in ["mit", "apache-2.0", "bsd-2-clause", "bsd-3-clause"] {
            assert!(permitted(license));
        }
        for license in [
            "",
            "other",
            "llama3",
            "gemma",
            "cc-by-4.0",
            "MIT AND proprietary",
            "mit-0",
        ] {
            assert!(!permitted(license));
        }
        let metadata = serde_json::json!({"sha":"a".repeat(40),"cardData":{"license":"mit","base_model":["org/base"]}});
        assert!(publisher(
            "org/model",
            &serde_json::json!({"sha":"a".repeat(40),"cardData":{"license":"mit","base_model":[7]}})
        )
        .is_err());
        assert_eq!(
            publisher("org/model", &metadata).unwrap().1,
            vec!["org/base"]
        );
        assert!(publisher(
            "org/model",
            &serde_json::json!({"sha":"a".repeat(40),"cardData":{"license":"llama3"}})
        )
        .is_err());
    }
    #[test]
    fn hostile_sources_and_receipt_mismatch_are_rejected() {
        for url in [
            "http://huggingface.co/o/r/resolve/main/x",
            "https://huggingface.co.evil/o/r/resolve/main/x",
            "https://huggingface.co/o/r/resolve/main/../x",
            "https://huggingface.co/o/r/resolve/main/x?override=1",
        ] {
            assert!(artifact(url).is_err());
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("model.gguf");
        std::fs::write(&path, b"weight fixture").unwrap();
        assert!(verify_local(&path).is_err());
        let receipt = ModelLicenseReceipt {
            artifact_url: format!(
                "https://huggingface.co/org/model/resolve/{}/model.gguf",
                "a".repeat(40)
            ),
            sha256: hex::encode(Sha256::digest(b"weight fixture")),
            publishers: vec![PublisherLicense {
                repository: "org/model".into(),
                revision: "a".repeat(40),
                license: "mit".into(),
            }],
        };
        assert!(
            receipt.record(&path).is_err(),
            "self-declared receipt must not establish publisher trust"
        );
        assert!(!path.with_extension("license.json").exists());
        remember(&receipt); // Simulate the authenticated metadata transport in this offline unit test.
        receipt.record(&path).unwrap();
        verify_local(&path).unwrap();
        let mut forged = receipt.clone();
        forged.publishers[0].license = "apache-2.0".into();
        assert!(forged.record(&path).is_err());
        std::fs::write(
            path.with_extension("license.json"),
            serde_json::to_vec(&forged).unwrap(),
        )
        .unwrap();
        assert!(verify_local(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"weight fixture");
        receipt.record(&path).unwrap();
        assert!(verify_requested_revision(&"a".repeat(40), &"b".repeat(40)).is_err());
        std::fs::write(&path, b"tampered weights").unwrap();
        assert!(verify_local(&path).is_err());
    }
}
