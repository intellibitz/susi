//! First-run planning: reuse admitted cached weights, otherwise fetch the
//! audited small CPU starter. Larger automatic downloads require later evidence.
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use susi_vendor_models::resumable_downloads::{DownloadPlan, RangeRequest, RangeResponse};
use susi_vendor_models::susi_error::{EaiError, EaiResult};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StarterModel {
    pub id: String,
    pub hf_repo: String,
    pub hf_file: String,
    pub size_bytes: u64,
    pub resident_bytes: u64,
    /// Used to rank already cached, verified models; never justifies a larger first download.
    pub quality: u32,
}
#[derive(Debug, Clone, Copy, Default)]
pub struct FirstRunHost {
    /// Available inference RAM after reserving memory for the OS.
    pub ram: u64,
    pub vram: u64,
    pub disk: u64,
    pub offline: bool,
}
pub(crate) const STARTER_REVISION: &str = "9217f5db79a29953eb74d5343926648285ec7e67";
pub(crate) const STARTER_SHA256: &str =
    "7671c0c304e6ce5a7fc577bcb12aba01e2c155cc2efd29b2213c95b18edaf6ed";
pub(crate) fn audited_starter() -> StarterModel {
    StarterModel {
        id: "qwen2.5-0.5b-instruct-q4".into(),
        hf_repo: "Qwen/Qwen2.5-0.5B-Instruct-GGUF".into(),
        hf_file: "qwen2.5-0.5b-instruct-q4_0.gguf".into(),
        size_bytes: 428_730_208,
        resident_bytes: 1_000_000_000,
        quality: 10,
    }
}
/// Only Apache-licensed candidates. The larger entry is useful when already
/// admitted and cached; an empty cache always starts with the smaller artifact.
#[must_use]
pub fn default_starters() -> Vec<StarterModel> {
    vec![
        audited_starter(),
        StarterModel {
            id: "qwen2.5-7b-instruct-q4".into(),
            hf_repo: "Qwen/Qwen2.5-7B-Instruct-GGUF".into(),
            hf_file: "qwen2.5-7b-instruct-q4_k_m.gguf".into(),
            size_bytes: 4_700_000_000,
            resident_bytes: 6_500_000_000,
            quality: 70,
        },
    ]
}
#[derive(Debug, Clone)]
pub enum FirstRunPlan {
    Fetch {
        model: StarterModel,
        plan: DownloadPlan,
    },
    UseCached {
        model: StarterModel,
        path: PathBuf,
    },
    /// No suitable model; report unavailable local inference, never pretend to infer.
    OfflineNoModel,
    TooSmall,
}
/// Verify cached artifacts before considering them; download the smallest
/// fitting candidate only when no admitted cached model fits.
#[must_use]
pub fn plan_first_run(
    host: &FirstRunHost,
    starters: &[StarterModel],
    cached: &[(String, PathBuf)],
    models_dir: &Path,
) -> FirstRunPlan {
    plan_with_verifier(host, starters, cached, models_dir, &|path| {
        let admitted = if host.offline {
            susi_vendor_models::model_license::verify_local_offline(path)
        } else {
            susi_vendor_models::model_license::verify_local(path)
        };
        admitted.is_ok() && crate::ModelManager::valid_gguf_payload(path)
    })
}
fn plan_with_verifier(
    host: &FirstRunHost,
    starters: &[StarterModel],
    cached: &[(String, PathBuf)],
    models_dir: &Path,
    verified: &dyn Fn(&Path) -> bool,
) -> FirstRunPlan {
    // CPU loading needs real RAM; do not pretend separate RAM and VRAM form
    // one contiguous memory pool. GPU execution is optional after first goal.
    let mut candidates: Vec<_> = starters
        .iter()
        .filter(|m| m.resident_bytes <= host.ram)
        .collect();
    candidates.sort_by_key(|m| std::cmp::Reverse(m.quality));
    for model in &candidates {
        if let Some((_, path)) = cached
            .iter()
            .find(|(id, path)| *id == model.id && verified(path))
        {
            return FirstRunPlan::UseCached {
                model: (*model).clone(),
                path: path.clone(),
            };
        }
    }
    if host.offline {
        return FirstRunPlan::OfflineNoModel;
    }
    candidates.retain(|m| m.size_bytes <= host.disk);
    candidates.sort_by_key(|m| (m.size_bytes, m.resident_bytes));
    let Some(model) = candidates.first() else {
        return FirstRunPlan::TooSmall;
    };
    let starter = audited_starter();
    let audited = model.hf_repo == starter.hf_repo && model.hf_file == starter.hf_file;
    FirstRunPlan::Fetch {
        model: (*model).clone(),
        plan: DownloadPlan {
            url: format!(
                "https://huggingface.co/{}/resolve/{}/{}",
                model.hf_repo,
                if audited { STARTER_REVISION } else { "main" },
                model.hf_file
            ),
            dest: models_dir.join(&model.hf_file),
            sha256: audited.then(|| STARTER_SHA256.into()),
            size: Some(model.size_bytes),
            max_retries: 8,
        },
    }
}
/// The injected transport cannot bypass publisher admission. Download only
/// to staging; publish and record provenance after complete hash verification.
pub fn fetch(
    plan: &DownloadPlan,
    transport: &dyn Fn(&RangeRequest) -> EaiResult<RangeResponse>,
    on_progress: &dyn Fn(u64),
) -> EaiResult<u64> {
    let receipt = susi_vendor_models::model_license::admit_download(&plan.url)
        .map_err(EaiError::governance)?;
    if plan
        .sha256
        .as_ref()
        .is_some_and(|hash| !hash.eq_ignore_ascii_case(&receipt.sha256))
    {
        return Err(EaiError::governance(
            "First-run plan hash disagrees with publisher evidence",
        ));
    }
    if plan.dest.exists() {
        receipt.record(&plan.dest).map_err(EaiError::governance)?;
        return Ok(std::fs::metadata(&plan.dest)?.len());
    }
    let mut staged = plan.clone();
    staged.url = receipt.artifact_url.clone();
    staged.sha256 = Some(receipt.sha256.clone());
    staged.dest = PathBuf::from(format!("{}.first-run.part", plan.dest.display()));
    let report = susi_vendor_models::resumable_downloads::download(&staged, &|request| {
        let response = transport(request)?;
        on_progress(request.from.saturating_add(response.body.len() as u64));
        Ok(response)
    })?;
    if !report.verified {
        return Err(EaiError::governance(
            "First-run download failed publisher hash verification",
        ));
    }
    // hard_link refuses an existing destination, so a concurrent/user file
    // is never overwritten. Staging and destination share their filesystem.
    std::fs::hard_link(&staged.dest, &plan.dest)?;
    receipt.record(&plan.dest).map_err(EaiError::governance)?;
    std::fs::remove_file(staged.dest)?;
    Ok(report.bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    const GB: u64 = 1_000_000_000;
    fn host(ram: u64, disk: u64, offline: bool) -> FirstRunHost {
        FirstRunHost {
            ram,
            vram: 24 * GB,
            disk,
            offline,
        }
    }
    #[test]
    fn empty_cache_uses_small_audited_starter_even_on_large_gpu_host() {
        for ram in [GB, 2 * GB, 8 * GB, 128 * GB] {
            match plan_first_run(
                &host(ram, 100 * GB, false),
                &default_starters(),
                &[],
                Path::new("models"),
            ) {
                FirstRunPlan::Fetch { model, plan } => {
                    assert_eq!(model, audited_starter());
                    assert!(plan.url.contains(STARTER_REVISION));
                    assert_eq!(plan.sha256.as_deref(), Some(STARTER_SHA256));
                    assert_eq!(plan.size, Some(428_730_208));
                }
                other => panic!("Expected small starter, got {other:?}"),
            }
        }
    }
    #[test]
    fn incompatible_defaults_are_not_candidates() {
        assert_eq!(default_starters().len(), 2);
        assert!(!default_starters()
            .iter()
            .any(|m| m.id.contains("llama") || m.id.contains("-3b-")));
        assert!(matches!(
            plan_first_run(
                &host(GB - 1, 100 * GB, false),
                &default_starters(),
                &[],
                Path::new("models")
            ),
            FirstRunPlan::TooSmall
        ));
        assert!(matches!(
            plan_first_run(
                &host(8 * GB, 428_730_207, false),
                &default_starters(),
                &[],
                Path::new("models")
            ),
            FirstRunPlan::TooSmall
        ));
    }
    #[test]
    fn cached_priority_does_not_require_free_disk_for_existing_bytes() {
        // Pure policy test: production supplies cryptographic verification,
        // independently exercised against actual pinned weights below.
        let cached = vec![(
            default_starters()[1].id.clone(),
            PathBuf::from("admitted-cache.gguf"),
        )];
        assert!(
            matches!(plan_with_verifier(&host(8*GB,0,true), &default_starters(), &cached, Path::new("models"), &|_| true), FirstRunPlan::UseCached {model,..} if model.quality == 70)
        );
        assert!(matches!(
            plan_with_verifier(
                &host(8 * GB, 0, true),
                &default_starters(),
                &cached,
                Path::new("models"),
                &|_| false
            ),
            FirstRunPlan::OfflineNoModel
        ));
    }
    #[test]
    fn synthetic_identifier_does_not_match_real_model_paths() {
        assert!(crate::ModelManager::is_synthetic_model_id(
            "susi-native-synthesis"
        ));
        for path in [
            "F:/src/susi-native-review/models/qwen.gguf",
            "C:/models/native-model.gguf",
            "C:/susi-native-synthesis/model.gguf",
            "native.gguf",
        ] {
            assert!(!crate::ModelManager::is_synthetic_model_id(path));
        }
    }
    #[test]
    fn arbitrary_cache_paths_are_not_trusted() {
        let cached = vec![(
            audited_starter().id,
            PathBuf::from("not-an-admitted-model.gguf"),
        )];
        assert!(matches!(
            plan_first_run(
                &host(8 * GB, 100 * GB, true),
                &default_starters(),
                &cached,
                Path::new("models")
            ),
            FirstRunPlan::OfflineNoModel
        ));
    }
    #[test]
    fn direct_fetch_cannot_bypass_license_admission_or_touch_destination() {
        let called = std::cell::Cell::new(false);
        let destination =
            std::env::temp_dir().join(format!("susi-untrusted-first-{}.gguf", std::process::id()));
        let plan = DownloadPlan {
            url: "test://unverified".into(),
            dest: destination.clone(),
            sha256: None,
            size: Some(10),
            max_retries: 1,
        };
        assert!(fetch(
            &plan,
            &|_| {
                called.set(true);
                Err(EaiError::network("unexpected transfer"))
            },
            &|_| {}
        )
        .is_err());
        assert!(!called.get());
        assert!(!destination.exists());
    }
    #[test]
    #[ignore = "requires explicit publisher-verified Qwen qualification cache"]
    fn real_pinned_starter_cache_is_admitted_offline_without_free_disk() {
        let path = PathBuf::from(
            std::env::var("SUSI_QWEN_TEST_MODEL").expect("isolated Qwen model required"),
        );
        let receipt = susi_vendor_models::model_license::admit_download(&format!(
            "https://huggingface.co/{}/resolve/{}/{}",
            audited_starter().hf_repo,
            STARTER_REVISION,
            audited_starter().hf_file
        ))
        .unwrap();
        receipt.record(&path).unwrap();
        let tokenizer_path = path.with_extension("tokenizer.json");
        let tokenizer_receipt = susi_vendor_models::model_license::admit_download("https://huggingface.co/Qwen/Qwen2.5-0.5B-Instruct/resolve/7ae557604adf67be50417f59c2c2f167def9a775/tokenizer.json").unwrap();
        tokenizer_receipt.record(&tokenizer_path).unwrap();
        susi_vendor_models::model_license::verify_local_offline(&tokenizer_path).unwrap();
        assert!(
            path.to_string_lossy().contains("native"),
            "regression qualification must include native in an ancestor directory"
        );
        let candidate = crate::susi_sandbox::manager::DynamicModelInfo::new(
            "Qwen audited fixture".into(),
            "SUSI".into(),
            path.to_string_lossy().into_owned(),
            "Real publisher-verified GGUF".into(),
            true,
            "LocalGGUF".into(),
            Some(0),
            "LocalGGUF".into(),
            None,
            None,
        );
        assert!(crate::ModelManager::admitted_local_candidate(&candidate), "real cached model under native-named ancestor must survive production candidate selection");
        let cached = vec![(audited_starter().id, path.clone())];
        assert!(
            matches!(plan_first_run(&host(2*GB,0,true),&default_starters(),&cached,Path::new("unused")),FirstRunPlan::UseCached {path:found,..} if found == path)
        );
    }
}
