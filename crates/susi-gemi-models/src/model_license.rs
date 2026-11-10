//! Admission for model weights. Cloud inference calls do not distribute weights
//! and are deliberately outside this policy.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{self, Read, Seek, Write},
    path::Path,
};
#[path = "model_read_window.rs"]
mod reads;
pub use reads::{ModelReadWindow, ResidentLease};
#[cfg(all(test, windows))]
thread_local! { static HASHED_BYTES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) }; }

pub struct VerifiedArtifact {
    file: File,
    sha256: String,
}
impl VerifiedArtifact {
    pub fn digest(&self) -> &str {
        &self.sha256
    }
    /// Reserve a bounded handle for a pending cold load. Pending leases cannot
    /// authorize reuse. Failure falls back to the regular authentication path.
    pub fn reserve_resident_lease(&self) -> Option<ResidentLease> {
        ResidentLease::reserve(&self.file, &self.sha256)
    }
    pub fn metadata(&self) -> std::io::Result<std::fs::Metadata> {
        self.file.metadata()
    }
    fn protected(&self) -> bool {
        #[cfg(windows)]
        {
            susi_config::windows_model_identity::model_file_identity(&self.file).is_some()
        }
        #[cfg(not(windows))]
        {
            false
        }
    }
    /// Give the seek-based decoder the verified stream. For mutable sources,
    /// snapshot and hash the same chunks into an anonymous private file.
    pub fn into_decoder_reader(mut self) -> io::Result<impl Read + Seek> {
        if self.protected() {
            return Ok(self.file);
        }
        let mut snapshot = private_snapshot_file()?;
        if digest_stream(&mut self.file, &mut snapshot)? != self.sha256 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "model changed while creating decoder snapshot",
            ));
        }
        snapshot.rewind()?;
        Ok(snapshot)
    }
    pub fn into_bytes(mut self) -> std::io::Result<Vec<u8>> {
        let protected = self.protected();
        let mut bytes = Vec::new();
        self.file.read_to_end(&mut bytes)?;
        // Only a protected local stream can reuse its digest directly. Verify
        // every other consumed buffer before it enters the tokenizer cache.
        if !protected && susi_paths::hex::encode(Sha256::digest(&bytes)) != self.sha256 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "model artifact changed during consumption",
            ));
        }
        Ok(bytes)
    }
}

fn digest_stream(source: &mut File, output: &mut impl Write) -> io::Result<String> {
    let mut hash = Sha256::new();
    let mut chunk = [0u8; 64 * 1024];
    loop {
        let n = source.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        output.write_all(&chunk[..n])?;
        hash.update(&chunk[..n]);
        #[cfg(all(test, windows))]
        HASHED_BYTES.with(|total| total.set(total.get() + n as u64));
    }
    Ok(susi_paths::hex::encode(hash.finalize()))
}

fn private_snapshot_file() -> io::Result<File> {
    static NEXT_SNAP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nonce = NEXT_SNAP.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("susi-model-snap-{}-{nonce}", std::process::id()));
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let file = opts.open(&path)?;
    let _ = std::fs::remove_file(&path);
    Ok(file)
}

#[allow(clippy::indexing_slicing)]
fn git_blob_sha1_hex(bytes: &[u8]) -> String {
    let mut msg = format!("blob {}\0", bytes.len()).into_bytes();
    msg.extend_from_slice(bytes);
    let bit_len = (msg.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());
    let mut h0: u32 = 0x6745_2301;
    let mut h1: u32 = 0xEFCD_AB89;
    let mut h2: u32 = 0x98BA_DCFE;
    let mut h3: u32 = 0x1032_5476;
    let mut h4: u32 = 0xC3D2_E1F0;
    for chunk in msg.as_chunks::<64>().0 {
        let mut w = [0u32; 80];
        for (i, word) in chunk.as_chunks::<4>().0.iter().enumerate() {
            w[i] = u32::from_be_bytes(*word);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h0, h1, h2, h3, h4);
        for (i, &wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A82_7999),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }
        h0 = h0.wrapping_add(a);
        h1 = h1.wrapping_add(b);
        h2 = h2.wrapping_add(c);
        h3 = h3.wrapping_add(d);
        h4 = h4.wrapping_add(e);
    }
    let mut out = [0u8; 20];
    out[0..4].copy_from_slice(&h0.to_be_bytes());
    out[4..8].copy_from_slice(&h1.to_be_bytes());
    out[8..12].copy_from_slice(&h2.to_be_bytes());
    out[12..16].copy_from_slice(&h3.to_be_bytes());
    out[16..20].copy_from_slice(&h4.to_be_bytes());
    susi_paths::hex::encode(out)
}

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
        if git_blob_sha1_hex(&bytes) != git_blob.to_ascii_lowercase() {
            return Err(
                "[MODEL LICENSE] Companion differs from publisher Git blob identity".into(),
            );
        }
        susi_paths::hex::encode(Sha256::digest(bytes))
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
        self.record_verified(path).map(drop)
    }
    fn record_verified(&self, path: &Path) -> Result<VerifiedArtifact, String> {
        self.authenticate(false)?;
        let verified = self.verify_bytes(path)?;
        crate::susi_config::atomic_write_json_pretty(&path.with_extension("license.json"), self)
            .map_err(|e| e.to_string())?;
        Ok(verified)
    }
    fn verify_bytes(&self, path: &Path) -> Result<VerifiedArtifact, String> {
        self.validate()?;
        let mut file = reads::open(path).map_err(|e| e.to_string())?;
        let sha256 = self.sha256.to_ascii_lowercase();
        #[cfg(windows)]
        let identity = reads::window::identity(&file);
        #[cfg(windows)]
        if identity.as_ref().is_some_and(|id| {
            reads::window::matches(id, &sha256) || reads::window::resident_matches(id, &sha256)
        }) {
            return Ok(VerifiedArtifact { file, sha256 });
        }
        if digest_stream(&mut file, &mut io::sink()).map_err(|e| e.to_string())? != sha256 {
            return Err(
                "[MODEL LICENSE] Weight bytes do not match publisher license receipt".into(),
            );
        }
        file.rewind().map_err(|e| e.to_string())?;
        #[cfg(windows)]
        if let Some(id) = identity {
            reads::window::retain(id, &sha256, &file);
        }
        Ok(VerifiedArtifact { file, sha256 })
    }
}
/// User-owned files are never deleted or changed when provenance is unknown.
pub fn verify_local(path: &Path) -> Result<(), String> {
    open_verified_local(path).map(drop)
}
pub fn open_verified_local(path: &Path) -> Result<VerifiedArtifact, String> {
    verify_local_with_lookup(path, true)
}
/// Verify admitted evidence without any metadata lookup or network request.
pub fn verify_local_offline(path: &Path) -> Result<(), String> {
    verify_local_with_lookup(path, false).map(drop)
}
fn verify_local_with_lookup(path: &Path, allow_lookup: bool) -> Result<VerifiedArtifact, String> {
    let receipt_path = path.with_extension("license.json");
    if !receipt_path.exists() {
        if !allow_lookup {
            return Err("[MODEL LICENSE] Offline admission needs an authenticated receipt".into());
        }
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
            return admit_download(&source)?.record_verified(path);
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
    receipt.authenticate(allow_lookup)?;
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
            sha256: susi_paths::hex::encode(Sha256::digest(b"weight fixture")),
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

    #[cfg(windows)]
    #[test]
    fn scoped_reads_authenticate_each_receipt_and_reject_replacements() {
        use std::fs;
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("admission.gguf");
        let alternate = std::path::PathBuf::from(format!("{}:other", path.display()));
        fs::write(&path, b"original").unwrap();
        fs::write(&alternate, b"tampered").unwrap();
        let receipt = ModelLicenseReceipt {
            artifact_url: format!(
                "https://huggingface.co/org/window-fixture/resolve/{}/admission.gguf",
                "c".repeat(40)
            ),
            sha256: susi_paths::hex::encode(Sha256::digest(b"original")),
            publishers: vec![PublisherLicense {
                repository: "org/window-fixture".into(),
                revision: "c".repeat(40),
                license: "mit".into(),
            }],
        };
        remember(&receipt);
        receipt.record(&path).unwrap();
        HASHED_BYTES.with(|n| n.set(0));
        let scope = ModelReadWindow::enter();
        let mut reader = open_verified_local(&path)
            .unwrap()
            .into_decoder_reader()
            .unwrap();
        {
            let _nested = ModelReadWindow::enter();
            verify_local_offline(&path.canonicalize().unwrap()).unwrap();
        }
        assert_eq!(HASHED_BYTES.with(|n| n.get()), 8);
        let receipt_path = path.with_extension("license.json");
        let good_receipt = fs::read(&receipt_path).unwrap();
        let mut forged = receipt.clone();
        forged.publishers[0].license = "proprietary".into();
        fs::write(&receipt_path, serde_json::to_vec(&forged).unwrap()).unwrap();
        assert!(verify_local_offline(&path).is_err());
        fs::write(&receipt_path, good_receipt).unwrap();
        assert!(verify_local_offline(&alternate).is_err());
        assert!(fs::OpenOptions::new().write(true).open(&path).is_err());
        let changed = d.path().join("changed");
        fs::write(&changed, b"tampered").unwrap();
        let stamp = fs::metadata(&path).unwrap().modified().unwrap();
        fs::File::options()
            .write(true)
            .open(&changed)
            .unwrap()
            .set_modified(stamp)
            .unwrap();
        assert_eq!(fs::metadata(&changed).unwrap().modified().unwrap(), stamp);
        fs::rename(&changed, &path).unwrap();
        assert!(verify_local_offline(&path).is_err());
        let mut revised = receipt.clone();
        revised.publishers[0].revision = "d".repeat(40);
        revised.artifact_url = format!(
            "https://huggingface.co/org/window-fixture/resolve/{}/admission.gguf",
            "d".repeat(40)
        );
        revised.sha256 = susi_paths::hex::encode(Sha256::digest(b"tampered"));
        remember(&revised);
        fs::write(&receipt_path, serde_json::to_vec(&revised).unwrap()).unwrap();
        verify_local_offline(&path).unwrap();
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"original");
        drop(reader);
        drop(scope);
        fs::write(&path, b"original").unwrap();
        fs::write(&receipt_path, serde_json::to_vec(&receipt).unwrap()).unwrap();
        verify_local_offline(&path).unwrap();
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires a private copied qualifying Windows starter model"]
    fn actual_starter_window_measures_authenticated_reads() {
        let path =
            std::path::PathBuf::from(std::env::var_os("SUSI_ADMISSION_PROOF_MODEL").unwrap());
        let tokenizer = path.with_extension("tokenizer.json");
        for (phase, model_checks, tokenizer_checks) in
            [("first goal", 7, 5), ("resident repeat", 9, 8)]
        {
            for reuse in [false, true] {
                HASHED_BYTES.with(|n| n.set(0));
                let _window = reuse.then(ModelReadWindow::enter);
                let clock = std::time::Instant::now();
                for i in 0..model_checks {
                    verify_local_offline(&path).unwrap();
                    if i < tokenizer_checks {
                        verify_local_offline(&tokenizer).unwrap();
                    }
                }
                let bytes = HASHED_BYTES.with(|n| n.get());
                let expected = if reuse {
                    fs_len(&path) + fs_len(&tokenizer)
                } else {
                    model_checks * fs_len(&path) + tokenizer_checks * fs_len(&tokenizer)
                };
                assert_eq!(bytes, expected);
                println!(
                "phase={phase} scope={reuse} authenticated={} hashed-bytes={bytes} elapsed-ms={}",
                model_checks+tokenizer_checks,clock.elapsed().as_millis()
            );
            }
        }
        fn fs_len(p: &Path) -> u64 {
            std::fs::metadata(p).unwrap().len()
        }
    }

    #[cfg(windows)]
    #[test]
    fn resident_lease_follows_live_bytes_and_authenticated_receipt_across_threads() {
        use std::fs;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("resident.gguf");
        fs::write(&path, b"original").unwrap();
        let alternate = std::path::PathBuf::from(format!("{}:other", path.display()));
        fs::write(&alternate, b"tampered").unwrap();
        let rev = "e".repeat(40);
        let receipt = ModelLicenseReceipt {
            artifact_url: format!(
                "https://huggingface.co/org/resident/resolve/{rev}/resident.gguf"
            ),
            sha256: susi_paths::hex::encode(Sha256::digest(b"original")),
            publishers: vec![PublisherLicense {
                repository: "org/resident".into(),
                revision: rev,
                license: "mit".into(),
            }],
        };
        remember(&receipt);
        receipt.record(&path).unwrap();
        let lease = open_verified_local(&path)
            .unwrap()
            .reserve_resident_lease()
            .unwrap();
        let access = || {
            std::thread::scope(|threads| {
                threads
                    .spawn(|| {
                        let _window = ModelReadWindow::enter();
                        HASHED_BYTES.with(|n| n.set(0));
                        (verify_local_offline(&path), HASHED_BYTES.with(|n| n.get()))
                    })
                    .join()
                    .unwrap()
            })
        };
        assert_eq!(access(), (Ok(()), 8));
        let lease = lease.publish();
        assert_eq!(access(), (Ok(()), 0));
        assert!(fs::OpenOptions::new().write(true).open(&path).is_err());
        // A named stream shares the file ID but is not the admitted default stream.
        let probe_scope = ModelReadWindow::enter();
        assert!(receipt.verify_bytes(&alternate).is_err());
        drop(probe_scope);
        let sidecar = path.with_extension("license.json");
        let mut forged = receipt.clone();
        forged.publishers[0].license = "proprietary".into();
        fs::write(&sidecar, serde_json::to_vec(&forged).unwrap()).unwrap();
        assert!(access().0.is_err());
        fs::write(&sidecar, serde_json::to_vec(&receipt).unwrap()).unwrap();
        let changed = dir.path().join("replace.gguf");
        fs::write(&changed, b"tampered").unwrap();
        let stamp = fs::metadata(&path).unwrap().modified().unwrap();
        fs::File::options()
            .write(true)
            .open(&changed)
            .unwrap()
            .set_modified(stamp)
            .unwrap();
        fs::rename(&changed, &path).unwrap();
        let (err, bytes) = access();
        assert!(err.is_err());
        assert_eq!(bytes, 8);
        fs::write(&path, b"original").unwrap();
        assert_eq!(access(), (Ok(()), 8));
        drop(lease);
        let mut leases = Vec::new();
        for _ in 0..4 {
            leases.push(
                open_verified_local(&path)
                    .unwrap()
                    .reserve_resident_lease()
                    .unwrap(),
            );
        }
        assert!(open_verified_local(&path)
            .unwrap()
            .reserve_resident_lease()
            .is_none());
        assert_eq!(access(), (Ok(()), 8));
        let leases: Vec<_> = leases.into_iter().map(ResidentLease::publish).collect();
        assert_eq!(access(), (Ok(()), 0));
        drop(leases);
        assert_eq!(access(), (Ok(()), 8));
        fs::write(&path, b"tampered").unwrap();
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires an explicitly selected private copy of the Apache starter model"]
    fn measure_real_resident_lease_across_goal_workers() {
        let path =
            std::path::PathBuf::from(std::env::var_os("SUSI_ADMISSION_PROOF_MODEL").unwrap());
        let tokenizer = path.with_extension("tokenizer.json");
        for use_lease in [false, true] {
            let lease = use_lease.then(|| {
                open_verified_local(&path)
                    .unwrap()
                    .reserve_resident_lease()
                    .unwrap()
                    .publish()
            });
            for worker in 0..3 {
                std::thread::scope(|threads| {
                    threads.spawn(|| {
                    HASHED_BYTES.with(|n|n.set(0)); let _window=ModelReadWindow::enter(); let start=std::time::Instant::now();
                    for check in 0..9 { verify_local_offline(&path).unwrap(); if check<8 {verify_local_offline(&tokenizer).unwrap();}}
                    let bytes=HASHED_BYTES.with(|n|n.get());
                    let expected=if use_lease{0}else{std::fs::metadata(&path).unwrap().len()}+std::fs::metadata(&tokenizer).unwrap().len();
                    assert_eq!(bytes,expected);
                    println!("lease={use_lease} worker={worker} authenticated=17 bytes={bytes} elapsed_ms={}",start.elapsed().as_millis());
                }).join().unwrap()
                });
            }
            drop(lease);
        }
    }
    #[test]
    fn unprotected_decoder_hashes_the_private_snapshot_and_consumed_tokenizer_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let plain = dir.path().join("weight");
        std::fs::write(&plain, b"plain").unwrap();
        #[cfg(windows)]
        let path = std::path::PathBuf::from(format!("{}:mutable", plain.display()));
        #[cfg(not(windows))]
        let path = plain;
        std::fs::write(&path, b"verified").unwrap();
        let read = || VerifiedArtifact {
            file: File::open(&path).unwrap(),
            sha256: susi_paths::hex::encode(Sha256::digest(b"verified")),
        };
        // The permissive File::open models a source with no immutable lease.
        // Changes after the first digest cannot enter either consumer.
        let decoder = read();
        let tokenizer = read();
        std::fs::write(&path, b"modified").unwrap();
        assert!(decoder.into_decoder_reader().is_err());
        assert!(tokenizer.into_bytes().is_err());
        std::fs::write(&path, b"verified").unwrap();
        let mut snapshot = read().into_decoder_reader().unwrap();
        std::fs::write(&path, b"modified").unwrap();
        let mut bytes = Vec::new();
        snapshot.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"verified");
    }
}
