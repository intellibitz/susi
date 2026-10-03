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
        // Atomic publish: rename into the store, then write the manifest
        // last so a crash between them never leaves a ready-looking
        // artifact without provenance.
        let final_path = self.final_path(name);
        if let Some(parent) = final_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| EaiError::io(e.to_string()))?;
        }
        std::fs::rename(&staged, &final_path).map_err(|e| EaiError::io(e.to_string()))?;
        let manifest = serde_json::json!({
            "name": name,
            "sha256": plan.sha256,
            "size": std::fs::metadata(&final_path).map(|m| m.len()).unwrap_or(0),
        });
        let manifest_path = self.manifest(name);
        std::fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest)?)
            .map_err(|e| EaiError::io(e.to_string()))?;
        Ok(PublishReport {
            published: true,
            path: Some(final_path),
            resumed_from,
            verified: report.verified,
        })
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

        let final_path = self.final_path(name);
        if let Some(parent) = final_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| EaiError::io(e.to_string()))?;
        }
        // Remove readiness metadata before replacing an existing artifact. A
        // crash after this point is unready and recoverable, never a ready
        // artifact paired with an old manifest.
        let manifest_path = self.manifest(name);
        let _ = std::fs::remove_file(&manifest_path);
        let final_provenance = final_path.with_extension("provenance.json");
        let staged_provenance = staged.with_extension("provenance.json");
        let _ = std::fs::remove_file(&final_provenance);
        std::fs::rename(&staged, &final_path)
            .map_err(|e| EaiError::io(format!("publish staged model: {e}")))?;
        if staged_provenance.is_file() {
            std::fs::rename(&staged_provenance, &final_provenance)
                .map_err(|e| EaiError::io(format!("publish model provenance: {e}")))?;
        }
        let manifest = serde_json::json!({
            "name": name,
            "source_url": plan.url,
            "sha256": plan.sha256,
            "size": size,
        });
        let bytes = serde_json::to_vec_pretty(&manifest)?;
        if let Err(error) = crate::susi_config::atomic_write_bytes(&manifest_path, &bytes) {
            // Keep a failed manifest write recoverable instead of leaving an
            // untracked payload outside `recover`.
            let _ = std::fs::rename(&final_path, &staged);
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
