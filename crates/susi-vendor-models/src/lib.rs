#![deny(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        unsafe_code,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::wildcard_enum_match_arm
    )
)]

//! Third-party model-provider integrations: cloud provider catalogs,
//! frontier-model registries, OpenRouter, open-weight registries, and
//! Hugging Face discovery. Re-exported through `susi_gemi_models` so the
//! consumer paths (`susi_gemi_models::openrouter::*`, …) are unchanged.

pub use susi_config;
pub use susi_core;
pub use susi_error;
pub use susi_sandbox_client as susi_sandbox;

pub mod accel_reserve;
pub mod azure_openai_provider;
pub mod bedrock_provider;
pub mod cloud;
pub mod cloud_cli_auth;
pub mod cloud_manage;
pub mod deepseek_default;
pub mod default_models;
pub mod disk_budget;
pub mod eco_anthropic_features;
pub mod eco_anthropic_messages;
pub mod eco_cohere_mistral;
pub mod eco_conflicts;
pub mod eco_consistency;
pub mod eco_coverage;
pub mod eco_gemini;
pub mod eco_matrix;
pub mod eco_openai_batch_files;
pub mod eco_openai_chat;
pub mod eco_openai_embed_media;
pub mod eco_openai_realtime;
pub mod eco_openai_responses;
pub mod eco_profile;
pub mod eco_provenance;
pub mod eco_relations;
pub mod eco_schema;
pub mod eco_signed_kb;
pub mod eco_store;
pub mod eco_taxonomy;
pub mod engine_autopull;
pub mod engine_format_match;
pub mod frontier;
pub mod gguf_inspector;
pub mod hf_discovery;
pub mod hf_gated_models;
pub mod hf_token_gate;
pub mod infer_batch;
pub mod key_detect;
pub mod key_rotation;
pub mod llamacpp_launcher;
pub mod lmstudio_models;
pub mod local_ecosystem;
pub mod model_deprecation;
pub mod model_store_dedupe;
pub mod more_openai_compat;
pub mod multi_gpu_plan;
pub mod npu_detection;
pub mod ollama_models;
pub mod open_weight;
pub mod openrouter;
pub mod port_conflicts;
pub mod price_catalog;
pub mod quant_recommender;
pub mod resource_inventory;
pub mod resumable_downloads;
pub mod runtime_lifecycle;
pub mod signed_catalog;
pub mod transactional_download;
pub mod vertex_provider;
pub mod vllm_launcher;
pub mod workload_drain;
pub mod zc_cloud_cli_auth;
pub mod zc_default_models;
pub mod zc_disk_default;
pub mod zc_engine_autopull;
pub mod zc_hf_token_jit;
pub mod zc_key_aliases;
pub mod zc_key_autodetect;
pub mod zc_key_sources;
pub mod zc_rotation_prompt;
pub mod zc_scan_paths;

#[cfg(test)]
#[path = "tests/resource_inventory.rs"]
mod resource_inventory_tests;
#[cfg(test)]
#[path = "tests/vc_201_042.rs"]
mod vc_201_042_tests;
#[cfg(test)]
#[path = "tests/vc_201_043.rs"]
mod vc_201_043_tests;
#[cfg(test)]
#[path = "tests/vc_201_044.rs"]
mod vc_201_044_tests;
#[cfg(test)]
#[path = "tests/vc_201_046.rs"]
mod vc_201_046_tests;
#[cfg(test)]
#[path = "tests/vc_201_049.rs"]
mod vc_201_049_tests;

/// Write the selected-model override every inference path reads
/// (`selected_model_override.txt` under the config dir). Vendor registries
/// call this directly; `susi_gemi_models::ModelManager::set_selected_model`
/// delegates here so the write contract lives in exactly one place.
pub fn set_selected_model_override(model_name: &str) -> Result<String, String> {
    // An empty name would silently clear the override while reporting
    // it "set", and a control character would corrupt the one-line
    // file every inference reads.
    let name = model_name.trim();
    if name.is_empty() || name.len() > 512 || name.chars().any(char::is_control) {
        return Err(format!(
            "invalid model name {model_name:?}: expected a non-empty single-line id"
        ));
    }
    // Atomic: every inference reads this; an empty read means "no
    // override" and silently reroutes to the default model.
    let model_file = susi_paths::SusiDirs::config_dir().join("selected_model_override.txt");
    susi_config::atomic_write_bytes(&model_file, name.as_bytes()).map_err(|e| e.to_string())?;
    Ok(format!("Selected active model override set to: '{name}'"))
}
pub mod eco_acronym_guard;
pub mod eco_ai_disclosures;
pub mod eco_aws_bedrock;
pub mod eco_aws_sagemaker;
pub mod eco_azure_foundry;
pub mod eco_changelog_feeds;
pub mod eco_cloudflare_ai;
pub mod eco_data_platforms;
pub mod eco_data_residency;
pub mod eco_embedding_specs;
pub mod eco_engine_capability_map;
pub mod eco_enterprise_clouds;
pub mod eco_eval_frameworks;
pub mod eco_fast_inference_hosts;
pub mod eco_gateways;
pub mod eco_gcp_vertex;
pub mod eco_gguf;
pub mod eco_gpu_clouds;
pub mod eco_guardrails;
pub mod eco_hf_platform;
pub mod eco_http_signatures;
pub mod eco_k8s_serving;
pub mod eco_licences;
pub mod eco_live_probe;
pub mod eco_llama_stack;
pub mod eco_llamacpp_api;
pub mod eco_lmstudio_api;
pub mod eco_memory_frameworks;
pub mod eco_ml_bom;
pub mod eco_mlflow;
pub mod eco_model_cards;
pub mod eco_model_lifecycle;
pub mod eco_new_model_detect;
pub mod eco_nvidia_nim;
pub mod eco_oauth_oidc;
pub mod eco_observability;
pub mod eco_oci_models;
pub mod eco_oip_v2;
pub mod eco_ollama_api;
pub mod eco_openai_compat_hosts;
pub mod eco_openapi_ingest;
pub mod eco_openapi_tools;
pub mod eco_prompt_formats;
pub mod eco_quant_adapters;
pub mod eco_rate_limit_models;
pub mod eco_replay_corpus;
pub mod eco_safetensors_onnx;
pub mod eco_sdk_versions;
pub mod eco_sunset_headers;
pub mod eco_tgi;
pub mod eco_tokenizers_templates;
pub mod eco_trace_otel;
pub mod eco_unknown_fields;
pub mod eco_vectordb_managed;
pub mod eco_vectordb_open;
pub mod eco_vllm_sglang;
pub mod eco_workload_identity;
