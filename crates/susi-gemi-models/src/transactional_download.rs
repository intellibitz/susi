//! Transactional model download publishing (VC-201-044).
//!
//! Downloads land in `staging/` and are verified there; only a verified
//! artifact is renamed into the model store and its manifest written.
//! An interrupted or checksum-mismatched download therefore never appears
//! as a ready model, and a failed attempt leaves staged bytes that
//! `recover` reports so the next publish resumes instead of restarting.

use crate::resumable_downloads::{self, DownloadPlan, RangeRequest, RangeResponse};
use crate::susi_error::{EaiError, EaiResult};
use std::path::{Path, PathBuf};

fn remove_if_present(path: &Path) -> EaiResult<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(EaiError::io(format!(
            "invalidate {}: {error}",
            path.display()
        ))),
    }
}

#[derive(Debug, Clone)]
pub struct Store {
    pub root: PathBuf,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PublishReport {
    /// False when the artifact could not be verified — nothing published.
    pub published: bool,
    pub path: Option<PathBuf>,
    /// Bytes already staged before this attempt (true resume).
    pub resumed_from: u64,
    pub verified: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StagedEntry {
    pub name: String,
    pub staged: PathBuf,
    pub bytes: u64,
}

impl Store {
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
        }
    }

    fn staging(&self, name: &str) -> PathBuf {
        self.root.join("staging").join(format!("{name}.part"))
    }

    /// Destination used by a streaming producer before the artifact is
    /// promoted into `models/`.  The producer's own `.part` file therefore
    /// lives under the same recoverable staging directory as `publish`.
    #[must_use]
    pub fn staging_target(&self, name: &str) -> PathBuf {
        self.root.join("staging").join(name)
    }

    fn final_path(&self, name: &str) -> PathBuf {
        self.root.join("models").join(name)
    }

    fn manifest(&self, name: &str) -> PathBuf {
        self.root
            .join("models")
            .join(format!("{name}.manifest.json"))
    }

    /// A model is ready only when its artifact AND manifest exist —
    /// staged or quarantined bytes never count.
    #[must_use]
    pub fn is_ready(&self, name: &str) -> bool {
        self.final_path(name).is_file() && self.manifest(name).is_file()
    }

    /// Run the resumable download into staging, verify, then publish
    /// atomically. Any failure leaves staged bytes for `recover`.
    pub fn publish(
        &self,
        name: &str,
        plan: &DownloadPlan,
        transport: &dyn Fn(&RangeRequest) -> EaiResult<RangeResponse>,
    ) -> EaiResult<PublishReport> {
        let staged = self.staging(name);
        let resumed_from = std::fs::metadata(&staged).map(|m| m.len()).unwrap_or(0);
        let mut staged_plan = plan.clone();
        staged_plan.dest = staged.clone();
        let report = resumable_downloads::download(&staged_plan, transport)?;
        if plan.sha256.is_some() && !report.verified {
            // Mismatched bytes are already quarantined by the downloader;
            // publish nothing.
            return Ok(PublishReport {
                published: false,
                path: None,
                resumed_from,
                verified: false,
            });
        }
        self.promote(name, plan, resumed_from, false)
    }

    /// Promote an artifact downloaded by a streaming producer.
    ///
    /// `download_target` is the path returned by [`Self::staging_target`]; the
    /// producer is expected to leave it there after streaming into its sibling
    /// `.part` file.  Verification and the readiness manifest are completed
    /// here so every managed installation has the same staged-to-ready
    /// transition, regardless of which HTTP client performed the transfer.
    pub fn commit_staged(
        &self,
        name: &str,
        plan: &DownloadPlan,
        validate: &dyn Fn(&Path) -> bool,
    ) -> EaiResult<PublishReport> {
        let staged = self.staging_target(name);
        let resumed_from = self.staging(name).metadata().map(|m| m.len()).unwrap_or(0);
        let size = std::fs::metadata(&staged)
            .map_err(|e| EaiError::io(format!("staged model {}: {e}", staged.display())))?
            .len();
        if plan.size.is_some_and(|expected| expected != size) {
            return Err(EaiError::io(format!(
                "staged model size {size} does not match declared size {:?}",
                plan.size
            )));
        }
        if let Some(expected) = plan.sha256.as_deref() {
            if !resumable_downloads::verify_file(&staged, expected)? {
                let quarantine = staged.with_extension("corrupt");
                std::fs::rename(&staged, &quarantine)
                    .map_err(|e| EaiError::io(format!("quarantine staged model: {e}")))?;
                return Ok(PublishReport {
                    published: false,
                    path: None,
                    resumed_from,
                    verified: false,
                });
            }
        }
        if !validate(&staged) {
            return Err(EaiError::io(format!(
                "staged model failed validation: {}",
                staged.display()
            )));
        }

        self.promote(name, plan, resumed_from, true)
    }

    /// Both producers use the same commit point: readiness is invalidated
    /// before replacing bytes, and published atomically only after provenance.
    /// Both callers have already rejected any declared checksum mismatch.
    fn promote(
        &self,
        name: &str,
        plan: &DownloadPlan,
        resumed_from: u64,
        streamed: bool,
    ) -> EaiResult<PublishReport> {
        let staged = if streamed {
            self.staging_target(name)
        } else {
            self.staging(name)
        };
        let size = std::fs::metadata(&staged)?.len();
        let final_path = self.final_path(name);
        if let Some(parent) = final_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| EaiError::io(e.to_string()))?;
        }
        // Remove readiness metadata before replacing an existing artifact. A
        // crash after this point is unready and recoverable, never a ready
        // artifact paired with an old manifest.
        let manifest_path = self.manifest(name);
        remove_if_present(&manifest_path)?;
        let final_provenance = final_path.with_extension("provenance.json");
        let staged_provenance = staged.with_extension("provenance.json");
        remove_if_present(&final_provenance)?;
        std::fs::rename(&staged, &final_path)
            .map_err(|e| EaiError::io(format!("publish staged model: {e}")))?;
        if streamed && staged_provenance.is_file() {
            if let Err(error) = std::fs::rename(&staged_provenance, &final_provenance) {
                let _ = std::fs::rename(&final_path, &staged);
                return Err(EaiError::io(format!("publish model provenance: {error}")));
            }
        }
        let mut manifest = serde_json::json!({
            "name": name,
            "sha256": plan.sha256,
            "size": size,
        });
        // Preserve the established streaming schema. Range downloads neither
        // produce provenance nor previously persisted potentially signed URLs.
        if streamed {
            manifest["source_url"] = plan.url.clone().into();
        }
        let bytes = serde_json::to_vec_pretty(&manifest)?;
        if let Err(error) = crate::susi_config::atomic_write_bytes(&manifest_path, &bytes) {
            // Best-effort rollback: range staging is visible to `recover`;
            // streaming staging is retryable through `commit_staged`.
            let _ = std::fs::rename(&final_path, &staged);
            if final_provenance.is_file() {
                let _ = std::fs::rename(&final_provenance, &staged_provenance);
            }
            return Err(EaiError::io(error.to_string()));
        }
        Ok(PublishReport {
            published: true,
            path: Some(final_path),
            resumed_from,
            verified: plan.sha256.is_some(),
        })
    }

    /// Staged-but-unpublished downloads the next attempt can resume.
    /// A disk-full or crash mid-download surfaces here, never as ready.
    #[must_use]
    pub fn recover(&self) -> Vec<StagedEntry> {
        let dir = self.root.join("staging");
        let Ok(rd) = std::fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for e in rd.flatten() {
            let p = e.path();
            if !p.extension().is_some_and(|x| x == "part") {
                continue;
            }
            if let Some(name) = p.file_stem().and_then(|s| s.to_str()).map(str::to_string) {
                out.push(StagedEntry {
                    name,
                    bytes: e.metadata().map(|m| m.len()).unwrap_or(0),
                    staged: p,
                });
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }
}

#[cfg(test)]
mod inline_tests {
    use super::*;

    fn temp_store(tag: &str) -> Store {
        let dir = std::env::temp_dir().join(format!(
            "susi-tx-dl-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Store::new(&dir)
    }

    #[test]
    fn store_paths_and_is_ready_require_both_artifact_and_manifest() {
        let store = temp_store("ready");
        assert!(!store.is_ready("model.gguf"));
        assert!(store
            .staging_target("model.gguf")
            .ends_with("staging/model.gguf"));

        let models_dir = store.root.join("models");
        std::fs::create_dir_all(&models_dir).unwrap();
        std::fs::write(models_dir.join("model.gguf"), b"weights").unwrap();
        assert!(!store.is_ready("model.gguf"));

        std::fs::write(models_dir.join("model.gguf.manifest.json"), b"{}").unwrap();
        assert!(store.is_ready("model.gguf"));
        let _ = std::fs::remove_dir_all(&store.root);
    }

    #[test]
    fn recover_lists_sorted_part_files_and_ignores_other_extensions() {
        let store = temp_store("recover");
        let staging = store.root.join("staging");
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(staging.join("zeta.gguf.part"), b"12345").unwrap();
        std::fs::write(staging.join("alpha.gguf.part"), b"12").unwrap();
        std::fs::write(staging.join("ignored.corrupt"), b"999").unwrap();

        let entries = store.recover();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "alpha.gguf");
        assert_eq!(entries[0].bytes, 2);
        assert_eq!(entries[1].name, "zeta.gguf");
        assert_eq!(entries[1].bytes, 5);
        let _ = std::fs::remove_dir_all(&store.root);
    }

    #[test]
    fn commit_staged_promotes_validated_artifact_and_writes_manifest() {
        let store = temp_store("commit-ok");
        let staged = store.staging_target("tiny.bin");
        std::fs::create_dir_all(staged.parent().unwrap()).unwrap();
        std::fs::write(&staged, b"abcd").unwrap();

        let plan = DownloadPlan {
            url: "https://example.invalid/tiny.bin".to_string(),
            dest: staged.clone(),
            size: Some(4),
            sha256: None,
            max_retries: 1,
        };
        let report = store.commit_staged("tiny.bin", &plan, &|_| true).unwrap();
        assert!(report.published);
        assert!(store.is_ready("tiny.bin"));
        let _ = std::fs::remove_dir_all(&store.root);
    }

    #[test]
    fn commit_staged_quarantines_checksum_mismatch_without_publishing() {
        let store = temp_store("commit-bad-sha");
        let staged = store.staging_target("corrupt.bin");
        std::fs::create_dir_all(staged.parent().unwrap()).unwrap();
        std::fs::write(&staged, b"wrong-bytes").unwrap();

        let plan = DownloadPlan {
            url: "https://example.invalid/corrupt.bin".to_string(),
            dest: staged.clone(),
            size: Some(11),
            sha256: Some(
                "0000000000000000000000000000000000000000000000000000000000000000".to_string(),
            ),
            max_retries: 1,
        };
        let report = store
            .commit_staged("corrupt.bin", &plan, &|_| true)
            .unwrap();
        assert!(!report.published);
        assert!(!report.verified);
        assert!(!store.is_ready("corrupt.bin"));
        assert!(staged.with_extension("corrupt").is_file());
        let _ = std::fs::remove_dir_all(&store.root);
    }

    #[test]
    fn commit_staged_rejects_size_mismatch_and_leaves_artifact_unpublished() {
        let store = temp_store("commit-bad-size");
        let staged = store.staging_target("mismatch.bin");
        std::fs::create_dir_all(staged.parent().unwrap()).unwrap();
        std::fs::write(&staged, b"123").unwrap();

        let plan = DownloadPlan {
            url: "https://example.invalid/mismatch.bin".to_string(),
            dest: staged.clone(),
            size: Some(99),
            sha256: None,
            max_retries: 1,
        };
        let err = store
            .commit_staged("mismatch.bin", &plan, &|_| true)
            .unwrap_err();
        assert!(err.to_string().contains("does not match declared size"));
        assert!(!store.is_ready("mismatch.bin"));
        let _ = std::fs::remove_dir_all(&store.root);
    }
}
