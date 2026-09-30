//! First-run local model chosen and fetched for this hardware.
//!
//! On first run susi picks the best starter model that fits RAM/VRAM/disk,
//! fetches it resumably with progress (see
//! `susi_vendor_models::resumable_downloads`), and falls back cleanly when
//! the host is offline or too small — zero required config anywhere.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use susi_vendor_models::resumable_downloads::{DownloadPlan, RangeRequest, RangeResponse};
use susi_vendor_models::susi_error::{EaiError, EaiResult};

/// One candidate starter model (a GGUF file on Hugging Face).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StarterModel {
    pub id: String,
    pub hf_repo: String,
    pub hf_file: String,
    /// Weight file size in bytes (drives the disk check).
    pub size_bytes: u64,
    /// Approx. bytes of RAM/VRAM the model needs resident to serve.
    pub resident_bytes: u64,
    /// Quality rank — higher is better; the picker maximises this subject
    /// to fit.
    pub quality: u32,
}

/// The hardware the first run has to work with.
#[derive(Debug, Clone, Copy, Default)]
pub struct FirstRunHost {
    /// Free system RAM (bytes).
    pub ram: u64,
    /// Free VRAM across usable GPUs (bytes); counts toward residency too.
    pub vram: u64,
    /// Free disk for the weight file (bytes).
    pub disk: u64,
    /// No outbound network — only already-cached models may be used.
    pub offline: bool,
}

/// Default starter ladder, smallest first. Sizes are the weight file.
#[must_use]
pub fn default_starters() -> Vec<StarterModel> {
    vec![
        StarterModel {
            id: "qwen2.5-0.5b-instruct-q4".to_string(),
            hf_repo: "Qwen/Qwen2.5-0.5B-Instruct-GGUF".to_string(),
            hf_file: "qwen2.5-0.5b-instruct-q4_k_m.gguf".to_string(),
            size_bytes: 400_000_000,
            resident_bytes: 700_000_000,
            quality: 10,
        },
        StarterModel {
            id: "llama-3.2-1b-instruct-q4".to_string(),
            hf_repo: "bartowski/Llama-3.2-1B-Instruct-GGUF".to_string(),
            hf_file: "Llama-3.2-1B-Instruct-Q4_K_M.gguf".to_string(),
            size_bytes: 800_000_000,
            resident_bytes: 1_400_000_000,
            quality: 20,
        },
        StarterModel {
            id: "qwen2.5-3b-instruct-q4".to_string(),
            hf_repo: "Qwen/Qwen2.5-3B-Instruct-GGUF".to_string(),
            hf_file: "qwen2.5-3b-instruct-q4_k_m.gguf".to_string(),
            size_bytes: 1_900_000_000,
            resident_bytes: 3_200_000_000,
            quality: 40,
        },
        StarterModel {
            id: "qwen2.5-7b-instruct-q4".to_string(),
            hf_repo: "Qwen/Qwen2.5-7B-Instruct-GGUF".to_string(),
            hf_file: "qwen2.5-7b-instruct-q4_k_m.gguf".to_string(),
            size_bytes: 4_700_000_000,
            resident_bytes: 6_500_000_000,
            quality: 70,
        },
    ]
}

/// What the first run decided.
#[derive(Debug, Clone)]
pub enum FirstRunPlan {
    /// Fetch `model` resumably to `dest` — plan is ready for
    /// `resumable_downloads::download`.
    Fetch {
        model: StarterModel,
        plan: DownloadPlan,
    },
    /// A fitting model is already on disk — nothing to download.
    UseCached { model: StarterModel, path: PathBuf },
    /// Offline with nothing cached that fits — degrade to cloud-free stub.
    OfflineNoModel,
    /// Even the smallest starter cannot run on this hardware.
    TooSmall,
}

/// Choose the best model for `host` and produce the first-run plan.
///
/// `cached` lists `(model id, path)` pairs already on disk. Offline hosts
/// may only use cached models; online hosts fetch the best fitting one.
/// Models are tried best-first: the resident footprint must fit in
/// RAM+VRAM and the weight file in free disk.
#[must_use]
pub fn plan_first_run(
    host: &FirstRunHost,
    starters: &[StarterModel],
    cached: &[(String, PathBuf)],
    models_dir: &Path,
) -> FirstRunPlan {
    let resident_cap = host.ram.saturating_add(host.vram);
    let fits = |m: &StarterModel| -> bool {
        m.resident_bytes <= resident_cap && m.size_bytes <= host.disk
    };
    let mut candidates: Vec<&StarterModel> = starters.iter().filter(|m| fits(m)).collect();
    candidates.sort_by_key(|a| std::cmp::Reverse(a.quality));

    for m in &candidates {
        if let Some((_, path)) = cached.iter().find(|(id, _)| *id == m.id) {
            return FirstRunPlan::UseCached {
                model: (*m).clone(),
                path: path.clone(),
            };
        }
    }
    if host.offline {
        return FirstRunPlan::OfflineNoModel;
    }
    match candidates.first() {
        Some(m) => {
            let url = format!(
                "https://huggingface.co/{}/resolve/main/{}",
                m.hf_repo, m.hf_file
            );
            FirstRunPlan::Fetch {
                model: (*m).clone(),
                plan: DownloadPlan {
                    url,
                    dest: models_dir.join(&m.hf_file),
                    sha256: None,
                    size: Some(m.size_bytes),
                    max_retries: 8,
                },
            }
        }
        None => FirstRunPlan::TooSmall,
    }
}

/// Execute a `Fetch` plan via the resumable downloader, reporting
/// byte-level progress through `on_progress`.
///
/// # Errors
/// Propagates download/verification failures.
pub fn fetch(
    plan: &DownloadPlan,
    transport: &dyn Fn(&RangeRequest) -> EaiResult<RangeResponse>,
    on_progress: &dyn Fn(u64),
) -> EaiResult<u64> {
    let report = susi_vendor_models::resumable_downloads::download(plan, &|req: &RangeRequest| {
        let resp = transport(req)?;
        on_progress(req.from + resp.body.len() as u64);
        Ok(resp)
    })
    .map_err(|e| EaiError::network(e.to_string()))?;
    Ok(report.bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(ram: u64, vram: u64, disk: u64, offline: bool) -> FirstRunHost {
        FirstRunHost {
            ram,
            vram,
            disk,
            offline,
        }
    }

    const GB: u64 = 1_000_000_000;

    #[test]
    fn zc_first_model_picks_best_that_fits() {
        // 8 GB RAM, no VRAM, plenty of disk → 7B's 6.5GB fits
        let h = host(8 * GB, 0, 100 * GB, false);
        match plan_first_run(&h, &default_starters(), &[], &PathBuf::from("/m")) {
            FirstRunPlan::Fetch { model, plan } => {
                assert_eq!(model.id, "qwen2.5-7b-instruct-q4");
                assert!(plan.url.contains("resolve/main"));
            }
            other => panic!("expected Fetch, got {other:?}"),
        }
    }

    #[test]
    fn zc_first_model_scales_down_on_small_hosts() {
        let cases: &[(u64, u64, u64, &str)] = &[
            (1_200_000_000, 0, 50 * GB, "qwen2.5-0.5b-instruct-q4"),
            (2 * GB, 0, 50 * GB, "llama-3.2-1b-instruct-q4"),
            (4 * GB, 0, 50 * GB, "qwen2.5-3b-instruct-q4"),
            (GB, GB, 50 * GB, "llama-3.2-1b-instruct-q4"), // ram+vram 2GB
            (500_000_000, 0, 50 * GB, "TOO_SMALL"),
        ];
        for (ram, vram, disk, want) in cases {
            let h = host(*ram, *vram, *disk, false);
            match plan_first_run(&h, &default_starters(), &[], &PathBuf::from("/m")) {
                FirstRunPlan::Fetch { model, .. } => assert_eq!(&model.id, want),
                FirstRunPlan::TooSmall => assert_eq!(want, &"TOO_SMALL"),
                other => panic!("ram={ram}: expected Fetch/TooSmall, got {other:?}"),
            }
        }
    }

    #[test]
    fn zc_first_model_disk_limit_skips_big_models() {
        // RAM fits 7B but disk only fits the 0.5B file
        let h = host(8 * GB, 0, 500_000_000, false);
        match plan_first_run(&h, &default_starters(), &[], &PathBuf::from("/m")) {
            FirstRunPlan::Fetch { model, .. } => {
                assert_eq!(model.id, "qwen2.5-0.5b-instruct-q4")
            }
            other => panic!("expected Fetch, got {other:?}"),
        }
    }

    #[test]
    fn zc_first_model_prefers_cached_over_download() {
        let h = host(8 * GB, 0, 100 * GB, false);
        let cached = vec![(
            "qwen2.5-3b-instruct-q4".to_string(),
            PathBuf::from("/m/qwen3b.gguf"),
        )];
        match plan_first_run(&h, &default_starters(), &cached, &PathBuf::from("/m")) {
            FirstRunPlan::UseCached { model, path } => {
                assert_eq!(model.id, "qwen2.5-3b-instruct-q4");
                assert_eq!(path, PathBuf::from("/m/qwen3b.gguf"));
            }
            other => panic!("expected UseCached, got {other:?}"),
        }
    }

    #[test]
    fn zc_first_model_offline_uses_cache_or_nothing() {
        let cached = vec![(
            "qwen2.5-0.5b-instruct-q4".to_string(),
            PathBuf::from("/m/q.gguf"),
        )];
        let h = host(8 * GB, 0, 100 * GB, true);
        // cached model isn't the best that fits, but offline it wins anyway
        assert!(matches!(
            plan_first_run(&h, &default_starters(), &cached, &PathBuf::from("/m")),
            FirstRunPlan::UseCached { .. }
        ));
        assert!(matches!(
            plan_first_run(&h, &default_starters(), &[], &PathBuf::from("/m")),
            FirstRunPlan::OfflineNoModel
        ));
    }

    #[test]
    fn zc_first_model_fetch_resumes_and_reports() {
        // one-shot transport serving a 10-byte file in chunks
        let data = b"0123456789";
        let transport = move |req: &RangeRequest| -> EaiResult<RangeResponse> {
            let from = req.from as usize;
            if from >= data.len() {
                return Ok(RangeResponse {
                    status: 416,
                    body: Vec::new(),
                    accept_ranges: true,
                });
            }
            let end = (from + 4).min(data.len());
            Ok(RangeResponse {
                status: if from == 0 { 200 } else { 206 },
                body: data[from..end].to_vec(),
                accept_ranges: true,
            })
        };
        let dest = std::env::temp_dir().join(format!("zc-first-{}.gguf", std::process::id()));
        let progress = std::cell::RefCell::new(Vec::new());
        let plan = DownloadPlan {
            url: "test://model".to_string(),
            dest: dest.clone(),
            sha256: None,
            size: Some(10),
            max_retries: 2,
        };
        let bytes = fetch(&plan, &transport, &|n| progress.borrow_mut().push(n)).unwrap();
        assert_eq!(bytes, 10);
        assert_eq!(std::fs::read(&dest).unwrap(), data);
        assert!(!progress.borrow().is_empty());
        std::fs::remove_file(&dest).ok();
    }
}
