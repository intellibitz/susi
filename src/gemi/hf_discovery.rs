use serde::{Deserialize, Serialize};
use std::time::Duration;
use crate::sandbox::manager::ModelLadderConfigStep;
use reqwest::blocking::Client;
use std::sync::{Mutex, OnceLock};

#[derive(Deserialize)]
struct HfModel {
    id: String,
}

#[derive(Deserialize)]
struct HfTreeEntry {
    path: String,
    size: u64,
}

static CACHED_DYNAMIC_LADDER: OnceLock<Mutex<Option<Vec<ModelLadderConfigStep>>>> = OnceLock::new();

/// Dynamically discovers best GGUF models on Hugging Face relying on search API.
pub fn discover_dynamic_ladder() -> Vec<ModelLadderConfigStep> {
    let mutex = CACHED_DYNAMIC_LADDER.get_or_init(|| Mutex::new(None));
    if let Ok(guard) = mutex.lock() {
        if let Some(ref cached) = *guard {
            return cached.clone();
        }
    }

    let mut steps = Vec::new();
    let client = Client::builder().timeout(Duration::from_secs(5)).build().unwrap_or_default();

    // Query top downloaded Instruct GGUFs to form a dynamic base ladder
    let api_url = "https://huggingface.co/api/models?search=Instruct-GGUF&sort=downloads&limit=15";
    if let Ok(resp) = client.get(api_url).send() {
        if let Ok(models) = resp.json::<Vec<HfModel>>() {
            let mut step_idx = 1;
            for model in models {
                let repo = model.id;
                let tree_url = format!("https://huggingface.co/api/models/{}/tree/main", repo);
                if let Ok(tree_resp) = client.get(&tree_url).send() {
                    if let Ok(files) = tree_resp.json::<Vec<HfTreeEntry>>() {
                        let mut gguf_files: Vec<_> = files.into_iter().filter(|f| f.path.ends_with(".gguf")).collect();
                        // Prefer Q4_K_M for balanced quality vs size
                        gguf_files.sort_by(|a, b| {
                            let a_q4 = a.path.contains("Q4_K_M");
                            let b_q4 = b.path.contains("Q4_K_M");
                            b_q4.cmp(&a_q4)
                        });

                        if let Some(best) = gguf_files.first() {
                            let req_ram = (best.size as f32 / 1_000_000_000.0) * 1.25;
                            steps.push(ModelLadderConfigStep {
                                hf_file: best.path.clone(),
                                hf_repo: repo.clone(),
                                tokenizer_repo: repo.replace("-GGUF", ""), 
                                label: format!("Dynamic Auto-Step: {} ({}GB)", repo.split('/').last().unwrap_or(&repo), req_ram.ceil()),
                                min_ram_gb: req_ram.ceil(),
                                step: step_idx,
                                fields: Default::default(),
                                min_bytes: best.size.saturating_sub(500_000_000),
                                expected_bytes: best.size + 1_000_000_000,
                            });
                            step_idx += 1;
                        }
                    }
                }
            }
        }
    }
    
    // Sort steps strictly by minimum RAM required to maintain progressive ladder trait
    steps.sort_by(|a, b| a.min_ram_gb.partial_cmp(&b.min_ram_gb).unwrap_or(std::cmp::Ordering::Equal));
    
    // Re-index steps
    for (i, step) in steps.iter_mut().enumerate() {
        step.step = i + 1;
    }

    if let Ok(mut guard) = mutex.lock() {
        *guard = Some(steps.clone());
    }

    steps
}
