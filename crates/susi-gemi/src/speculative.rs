// Speculative decoding: raises GPU utilization and throughput for the
// native Qwen2 GGUF backend by batching several tokens into one target-
// model forward pass instead of one token at a time.
//
// Why this exists: profiling with `nvidia-smi dmon` during real generation
// showed GPU-Util bursting to 100% for roughly one second per token, then
// idling - even for a model fully resident on GPU (Qwen2.5-7B, all 28
// layers, zero CPU layers), utilization never sustained above ~55%. This is
// not a layer-placement problem; it's that batch-of-1, one-token-at-a-time
// autoregressive decoding is inherently memory-bandwidth-bound - there's
// too little parallel work in a single-token matmul to saturate the SM,
// on any GPU, in any engine (llama.cpp, vLLM, etc. all show the same
// shape at batch=1). Candle's own quantized CUDA matmul dispatch
// (`candle_core::quantized::cuda::QCudaStorage::fwd`) explicitly routes to
// a different, more parallel kernel once the batch dimension exceeds a
// small threshold - confirming a wider batch is the actual lever here, not
// a config knob.
//
// The fix (attempted): a small local "draft" model proposes several tokens
// ahead, autoregressively and cheaply; the big "target" model then verifies
// all of them in a single batched forward pass. Because generation here is
// plain greedy argmax decoding (no sampling), verification is exact and
// deterministic, not probabilistic: at each drafted position, we accept
// the draft model's token if and only if it matches what the target model
// itself would have greedily produced there, and fall back to the
// target's own choice at the first disagreement. This guarantees the
// emitted token sequence is bit-for-bit identical to what plain greedy
// decoding through the target model alone would have produced (see
// `tests::test_speculative_output_matches_plain_greedy_decoding`) - never
// a change in generation behavior, only (attempted) throughput.
//
// Measured result: WORSE, not better. Live on this host, a fully
// GPU-resident Qwen2.5-7B target with a 0.5B draft ran at 4.36 tok/s
// versus 18.47 tok/s for the classic per-token loop on the same model - a
// 4x regression. The CUDA-kernel-level batching win is real, but it's
// consumed by this design's *host*-level overhead: the draft model still
// needs `speculative_draft_tokens - 1` sequential single-token forwards
// per round (autoregressive on itself, can't be batched away) plus a
// resync forward, on top of the target's one batched verify call - more
// total GPU round trips (tensor creation, kernel dispatch, host sync to
// extract logits) than the classic loop needed for the same tokens, and
// per-call round-trip overhead dominates raw compute at these model sizes
// through candle's Rust API. `speculative_decoding_enabled` therefore
// defaults to `false` (see its doc comment in `sandbox::manager`) - this
// module is kept, and stays fully correctness-tested, because the
// trade-off could flip with CUDA-graph-style call batching, a fatter
// draft_chunk, or different hardware, but it must never default on
// without remeasuring first.

use crate::engine::{apply_repeat_penalty, InferenceHost};
use crate::hardware::HardwareProfiler;
use crate::models::ModelManager;
use crate::qwen2_split::ModelWeights as Qwen2Weights;
use candle_core::quantized::gguf_file;
use candle_core::{Device, IndexOp, Tensor};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use susi_agents::task_manager::TaskHandle;
use susi_error::{EaiError, EaiResult};
use tokenizers::Tokenizer;

/// Rules out llama.cpp's KB-scale `ggml-vocab-*.gguf` test fixtures (found
/// stray on this host under an IDE tool's data directory during discovery)
/// and any other non-model file that happens to end in `.gguf` - a real
/// draft model, even the smallest local one (Qwen2.5-0.5B), is hundreds of
/// megabytes.
const MIN_DRAFT_MODEL_BYTES: u64 = 50_000_000;

fn argmax(v: &[f32]) -> u32 {
    let mut best = 0usize;
    let mut best_v = f32::NEG_INFINITY;
    for (i, &x) in v.iter().enumerate() {
        if x > best_v {
            best_v = x;
            best = i;
        }
    }
    best as u32
}

fn tensor_row_to_vec(t: &Tensor, row: usize) -> EaiResult<Vec<f32>> {
    t.i((0, row, ..))
        .and_then(|r| r.to_vec1::<f32>())
        .map_err(|e| {
            EaiError::inference(format!(
                "Speculative decoding: logits extraction failed: {e}"
            ))
        })
}

fn single_token_tensor(id: u32) -> EaiResult<Tensor> {
    Tensor::new(&[id], &Device::Cpu)
        .and_then(|t| t.unsqueeze(0))
        .map_err(|e| EaiError::inference(format!("Speculative decoding: tensor error: {e}")))
}

fn chunk_tensor(ids: &[u32]) -> EaiResult<Tensor> {
    Tensor::new(ids, &Device::Cpu)
        .and_then(|t| t.unsqueeze(0))
        .map_err(|e| EaiError::inference(format!("Speculative decoding: tensor error: {e}")))
}

/// Applies the repeat penalty (identical convention to the classic
/// one-token-at-a-time loop in `engine.rs`: windowed over generated tokens
/// only, most recent `repeat_last_n`) and returns the greedy argmax token.
fn penalized_argmax(
    logits: &mut [f32],
    repeat_penalty: f32,
    repeat_last_n: usize,
    window: &[u32],
) -> u32 {
    let start = window.len().saturating_sub(repeat_last_n);
    apply_repeat_penalty(logits, repeat_penalty, &window[start..]);
    argmax(logits)
}

pub struct SpeculativeDecoder;

impl SpeculativeDecoder {
    /// Looks for a smaller, qwen2-architecture GGUF in the managed models
    /// directory to draft for `target_path`. Restricted to the managed
    /// directory (not the broader cross-filesystem "discovered
    /// substrates" scan, which turns up non-model files) and to files
    /// strictly smaller than the target.
    ///
    /// Prefers the *smallest* qualifying candidate, not the largest: the
    /// draft only pays for itself if it's cheap enough to run several
    /// times per target verification round for close to free. A draft
    /// that's large enough to need its own CPU/GPU split competes with the
    /// target for the same limited VRAM budget and is slow in its own
    /// right - observed live picking the 14B model to draft for 32B
    /// (largest-smaller-than-target), which loaded onto only 4 GPU layers
    /// itself (VRAM already mostly claimed by the target's 22) and was
    /// nearly as CPU-bound as the target it was meant to accelerate,
    /// making the whole exercise pointless. A tiny (0.5B-class) draft's
    /// lower per-token agreement rate with the target is a far smaller
    /// cost than that.
    fn select_draft_model(target_path: &Path, target_size: u64) -> Option<(PathBuf, u64)> {
        Self::select_draft_model_in(&ModelManager::get_models_dir(), target_path, target_size)
    }

    /// The scanning logic behind `select_draft_model`, parameterized on the
    /// directory to scan so it's testable against an arbitrary fixture
    /// directory instead of the shared, `cfg!(test)`-global models dir that
    /// `ModelManager::get_models_dir()` resolves to (which other tests may
    /// also touch).
    fn select_draft_model_in(
        models_dir: &Path,
        target_path: &Path,
        target_size: u64,
    ) -> Option<(PathBuf, u64)> {
        let entries = std::fs::read_dir(models_dir).ok()?;
        let mut best: Option<(PathBuf, u64)> = None;
        for entry in entries.flatten() {
            let path = entry.path();
            if path == target_path {
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("gguf") {
                continue;
            }
            let Ok(meta) = path.metadata() else { continue };
            let size = meta.len();
            if size >= target_size || size < MIN_DRAFT_MODEL_BYTES {
                continue;
            }
            let Ok(mut f) = std::fs::File::open(&path) else {
                continue;
            };
            let Ok(ct) = gguf_file::Content::read(&mut f) else {
                continue;
            };
            let arch = ct
                .metadata
                .get("general.architecture")
                .and_then(|v| v.to_string().ok())
                .map(|s| s.to_lowercase())
                .unwrap_or_default();
            if !arch.starts_with("qwen2") {
                continue;
            }
            if best.as_ref().map(|(_, s)| size < *s).unwrap_or(true) {
                best = Some((path, size));
            }
        }
        best
    }

    /// Attempts speculative decoding for this request. Returns `None` when
    /// it isn't applicable (disabled, no suitable local draft model, draft
    /// tokenizer doesn't match the target's) so the caller falls back to
    /// its normal one-token-at-a-time loop unchanged. Once it returns
    /// `Some(_)` it has committed to running the entire generation this
    /// way: `Some(Ok(text))` on success, `Some(Err(_))` on a genuine
    /// mid-generation failure (propagated, not swallowed - some tokens may
    /// already have reached `callback` by then, matching how the classic
    /// loop treats a cancellation mid-stream).
    #[allow(clippy::too_many_arguments)]
    pub fn try_generate(
        target: &mut Qwen2Weights,
        target_model_path: &Path,
        target_tokenizer_path: &Path,
        tokenizer: &Tokenizer,
        prompt_tokens: &[u32],
        eos_token_ids: &[u32],
        max_tokens: usize,
        repeat_penalty: f32,
        repeat_last_n: usize,
        task_handle: &Arc<TaskHandle>,
        callback: &dyn Fn(String),
    ) -> Option<EaiResult<String>> {
        let cfg = susi_sandbox::manager::SusiConfig::load_global().unwrap_or_default();
        if !cfg.speculative_decoding_enabled() {
            return None;
        }
        // Measured live (nvidia-smi dmon + direct timing): CUDA's quantized
        // matmul genuinely gets cheaper per-token with a wider batch, but
        // candle's CPU quantized matmul does not (~2s/token either way on
        // this host's 32B/41-CPU-layer split). Wherever any layer runs on
        // CPU, batching buys nothing on that portion and the technique's
        // added overhead (a whole extra draft model, rejected-draft waste)
        // is pure loss - only engage where every layer is GPU-resident.
        if !target.is_fully_gpu_resident() {
            return None;
        }
        let draft_chunk = cfg.speculative_draft_tokens().max(2);

        let target_size = std::fs::metadata(target_model_path)
            .map(|m| m.len())
            .unwrap_or(0);
        let (draft_path, draft_size) = Self::select_draft_model(target_model_path, target_size)?;

        // Draft tokens are compared directly against target-computed token
        // ids; that's only meaningful if both share one vocabulary.
        let draft_tokenizer_path = ModelManager::get_tokenizer_path(&draft_path.to_string_lossy())?;
        if draft_tokenizer_path != target_tokenizer_path {
            return None;
        }

        let draft_device = HardwareProfiler::get_dynamic_device(draft_size as usize);
        let draft_substrate =
            match InferenceHost::get_model(&draft_path, &draft_device, task_handle) {
                Ok(s) => s,
                Err(_) => return None, // draft failed to load: not fatal, just not applicable
            };
        let mut draft_guard = draft_substrate.write();
        let draft = draft_guard.weights.as_qwen2_mut()?;
        if !draft.is_fully_gpu_resident() {
            // The target left no VRAM headroom for even a tiny draft model
            // (or no GPU is actually active); a CPU-bound draft is just as
            // pointless to batch as a CPU-bound target.
            return None;
        }

        println!(
            "- [Inference Substrate] Speculative decoding engaged: draft={} target={} (draft_chunk={})",
            draft_path.display(),
            target_model_path.display(),
            draft_chunk
        );

        Some(Self::run(
            target,
            draft,
            tokenizer,
            prompt_tokens,
            eos_token_ids,
            max_tokens,
            repeat_penalty,
            repeat_last_n,
            draft_chunk,
            task_handle,
            callback,
        ))
    }

    #[allow(clippy::too_many_arguments)]
    fn run(
        target: &mut Qwen2Weights,
        draft: &mut Qwen2Weights,
        tokenizer: &Tokenizer,
        prompt_tokens: &[u32],
        eos_token_ids: &[u32],
        max_tokens: usize,
        repeat_penalty: f32,
        repeat_last_n: usize,
        draft_chunk: usize,
        task_handle: &Arc<TaskHandle>,
        callback: &dyn Fn(String),
    ) -> EaiResult<String> {
        let mut text_stream = crate::token_stream::TokenStream::new(tokenizer);
        let mut all_tokens: Vec<u32> = Vec::new();
        let mut pos = prompt_tokens.len();

        // Prefill both models on the prompt (index_pos = 0 resets any
        // stale KV cache from a prior request against these same cached
        // model handles - see LayerWeights::forward_attn).
        let prompt_tensor = chunk_tensor(prompt_tokens)?;
        let prefill_logits = target
            .forward(&prompt_tensor, 0)
            .map_err(|e| EaiError::inference(format!("Speculative target prefill failed: {e}")))?;
        draft
            .forward(&prompt_tensor, 0)
            .map_err(|e| EaiError::inference(format!("Speculative draft prefill failed: {e}")))?;

        let mut prefill_v: Vec<f32> = prefill_logits
            .flatten_all()
            .and_then(|t| t.to_vec1::<f32>())
            .map_err(|e| {
                EaiError::inference(format!(
                    "Speculative decoding: logits extraction failed: {e}"
                ))
            })?;
        let mut anchor_token =
            penalized_argmax(&mut prefill_v, repeat_penalty, repeat_last_n, &all_tokens);

        loop {
            if all_tokens.len() >= max_tokens {
                break;
            }
            task_handle.check_pause();
            if task_handle.is_cancelled() {
                task_handle.mark_failed("Inference cancelled or stalled");
                return Err(EaiError::inference(
                    "Inference task cancelled or stalled by Swarm Watchdog.",
                ));
            }
            if eos_token_ids.contains(&anchor_token) {
                break;
            }

            let round_start_pos = pos;

            // --- Draft: propose up to draft_chunk-1 more tokens beyond the anchor ---
            let mut spec_chunk = vec![anchor_token];
            let mut draft_history = all_tokens.clone();
            let mut current_tok = anchor_token;
            for feed_pos in (round_start_pos..).take(draft_chunk - 1) {
                let logits = draft
                    .forward(&single_token_tensor(current_tok)?, feed_pos)
                    .map_err(|e| {
                        EaiError::inference(format!("Speculative draft step failed: {e}"))
                    })?;
                let mut v: Vec<f32> = logits
                    .flatten_all()
                    .and_then(|t| t.to_vec1::<f32>())
                    .map_err(|e| {
                        EaiError::inference(format!(
                            "Speculative decoding: logits extraction failed: {e}"
                        ))
                    })?;
                draft_history.push(current_tok);
                current_tok =
                    penalized_argmax(&mut v, repeat_penalty, repeat_last_n, &draft_history);
                spec_chunk.push(current_tok);
                if eos_token_ids.contains(&current_tok) {
                    break;
                }
            }
            let k = spec_chunk.len();

            // --- Verify: one batched target forward over the whole chunk ---
            let verify_logits = target
                .forward_all_logits(&chunk_tensor(&spec_chunk)?, round_start_pos)
                .map_err(|e| {
                    EaiError::inference(format!("Speculative verification forward failed: {e}"))
                })?;

            let mut mismatch: Option<(usize, u32)> = None; // (index into spec_chunk of the wrong token, correct replacement)
            for j in 0..k - 1 {
                let mut row = tensor_row_to_vec(&verify_logits, j)?;
                let mut window = all_tokens.clone();
                window.extend_from_slice(&spec_chunk[0..=j]);
                let target_pick =
                    penalized_argmax(&mut row, repeat_penalty, repeat_last_n, &window);
                if target_pick != spec_chunk[j + 1] {
                    mismatch = Some((j + 1, target_pick));
                    break;
                }
            }

            let (accepted_count, next_anchor);
            match mismatch {
                None => {
                    // Every drafted token matched the target's own greedy
                    // choice; the batched call already grew the target's
                    // cache correctly by the full chunk, so no truncation
                    // is needed. Row k-1 (never consulted by the loop
                    // above, since it verifies what comes *after* the full
                    // chunk) gives the next anchor for free.
                    accepted_count = k;
                    let mut last_row = tensor_row_to_vec(&verify_logits, k - 1)?;
                    let mut window = all_tokens.clone();
                    window.extend_from_slice(&spec_chunk);
                    next_anchor =
                        penalized_argmax(&mut last_row, repeat_penalty, repeat_last_n, &window);
                }
                Some((bad_idx, corrective)) => {
                    let m = bad_idx; // spec_chunk[0..m] confirmed correct
                    target.truncate_kv_cache(round_start_pos + m).map_err(|e| {
                        EaiError::inference(format!(
                            "Speculative decoding: cache truncation failed: {e}"
                        ))
                    })?;

                    // Emit the confirmed prefix plus the corrective token,
                    // then feed the corrective token to advance the
                    // target's cache and learn the real next anchor.
                    let mut emitted_this_round = spec_chunk[0..m].to_vec();
                    emitted_this_round.push(corrective);
                    Self::emit(
                        &emitted_this_round,
                        &mut all_tokens,
                        max_tokens,
                        eos_token_ids,
                        &mut text_stream,
                        task_handle,
                        callback,
                    )?;
                    if all_tokens.len() >= max_tokens || eos_token_ids.contains(&corrective) {
                        // truncate_kv_cache already fixed the target; draft
                        // cache correctness no longer matters, we're done.
                        let output = tokenizer
                            .decode(&all_tokens, true)
                            .map_err(|e| EaiError::inference(format!("Decoding Error: {e}")))?;
                        text_stream.finish(&output, callback);
                        task_handle.mark_completed(&output);
                        return Ok(output);
                    }

                    let corrective_logits = target
                        .forward(&single_token_tensor(corrective)?, round_start_pos + m)
                        .map_err(|e| {
                            EaiError::inference(format!(
                                "Speculative correction forward failed: {e}"
                            ))
                        })?;
                    let mut v: Vec<f32> = corrective_logits
                        .flatten_all()
                        .and_then(|t| t.to_vec1::<f32>())
                        .map_err(|e| {
                            EaiError::inference(format!(
                                "Speculative decoding: logits extraction failed: {e}"
                            ))
                        })?;
                    next_anchor =
                        penalized_argmax(&mut v, repeat_penalty, repeat_last_n, &all_tokens);

                    // Resync draft: it only validly fed spec_chunk[0..m]
                    // (the confirmed prefix); discard whatever it fed
                    // beyond that, then feed it the corrective token so
                    // its cache matches the target's before next round.
                    draft.truncate_kv_cache(round_start_pos + m).map_err(|e| {
                        EaiError::inference(format!(
                            "Speculative decoding: draft cache truncation failed: {e}"
                        ))
                    })?;
                    draft
                        .forward(&single_token_tensor(corrective)?, round_start_pos + m)
                        .map_err(|e| {
                            EaiError::inference(format!(
                                "Speculative draft resync forward failed: {e}"
                            ))
                        })?;

                    pos = round_start_pos + m + 1;
                    anchor_token = next_anchor;
                    continue;
                }
            }

            // Full-acceptance path: emit the whole chunk, resync the draft
            // (it fed spec_chunk[0..k-1] while drafting; the last drafted
            // token was only ever its own *output*, never fed as input).
            Self::emit(
                &spec_chunk,
                &mut all_tokens,
                max_tokens,
                eos_token_ids,
                &mut text_stream,
                task_handle,
                callback,
            )?;
            // Mandate 42: safe - `spec_chunk` is initialized as
            // `vec![anchor_token]` (never `vec![]`) and only ever grows via
            // `.push` afterward, so it's non-empty for the rest of this
            // scope and `.last()` can never return `None` here or below.
            if all_tokens.len() >= max_tokens || eos_token_ids.contains(spec_chunk.last().unwrap())
            {
                let output = tokenizer
                    .decode(&all_tokens, true)
                    .map_err(|e| EaiError::inference(format!("Decoding Error: {e}")))?;
                text_stream.finish(&output, callback);
                task_handle.mark_completed(&output);
                return Ok(output);
            }
            draft
                .forward(
                    &single_token_tensor(*spec_chunk.last().unwrap())?,
                    round_start_pos + k - 1,
                )
                .map_err(|e| {
                    EaiError::inference(format!("Speculative draft resync forward failed: {e}"))
                })?;

            pos = round_start_pos + accepted_count;
            anchor_token = next_anchor;
        }

        // Loop exited via the top-of-loop max_tokens/EOS checks (anchor
        // itself is EOS, or the budget is already exhausted) rather than
        // mid-round, so nothing from this iteration was emitted.
        let output = tokenizer
            .decode(&all_tokens, true)
            .map_err(|e| EaiError::inference(format!("Decoding Error: {e}")))?;
        text_stream.finish(&output, callback);
        task_handle.mark_completed(&output);
        Ok(output)
    }

    #[allow(clippy::too_many_arguments)]
    fn emit(
        tokens: &[u32],
        all_tokens: &mut Vec<u32>,
        max_tokens: usize,
        eos_token_ids: &[u32],
        text_stream: &mut crate::token_stream::TokenStream<'_>,
        task_handle: &Arc<TaskHandle>,
        callback: &dyn Fn(String),
    ) -> EaiResult<()> {
        for &t in tokens {
            if all_tokens.len() >= max_tokens {
                return Ok(());
            }
            all_tokens.push(t);
            task_handle.report_progress();
            if eos_token_ids.contains(&t) {
                return Ok(());
            }
            text_stream.push(t, callback)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use candle_core::quantized::gguf_file;

    fn load(path: &Path) -> Qwen2Weights {
        let mut file = std::fs::File::open(path).expect("open gguf");
        let ct = gguf_file::Content::read(&mut file).expect("read gguf metadata");
        Qwen2Weights::from_gguf_split(ct, &mut file, &Device::Cpu, &Device::Cpu, 0, 2304)
            .expect("load model")
    }

    fn eos_ids_for(path: &Path) -> Vec<u32> {
        let mut file = std::fs::File::open(path).expect("open gguf");
        let ct = gguf_file::Content::read(&mut file).expect("read gguf metadata");
        ct.metadata
            .get("tokenizer.ggml.eos_token_id")
            .and_then(|v| v.to_u32().ok())
            .into_iter()
            .collect()
    }

    /// The master correctness proof for this whole feature: running
    /// `SpeculativeDecoder::run` (draft-and-verify, batched) against a real
    /// target+draft model pair must produce a token-for-token IDENTICAL
    /// result to plain greedy decoding through the target model alone,
    /// one token at a time. If any of the offset-mask, cache-truncation,
    /// or acceptance-boundary logic were subtly wrong, this is what would
    /// catch it - not "it ran without erroring". CPU-only (both models
    /// loaded with `n_gpu_layers = 0`) so it runs without a GPU and stays
    /// fully deterministic; skips honestly when the local models aren't
    /// present on this host.
    #[test]
    #[ignore = "real tensor computation against two local models, ~4.5 minutes solo - the dominant cost of the entire suite. Run via `cargo test -- --ignored` or the scheduled slow-tests CI workflow"]
    fn test_speculative_output_matches_plain_greedy_decoding() {
        let _home = match std::env::var_os("HOME") {
            Some(h) => std::path::PathBuf::from(h),
            None => return,
        };
        let models_dir = susi_paths::SusiDirs::data_dir().join("models");
        let target_path = models_dir.join("qwen2.5-1.5b-instruct-q4_k_m.gguf");
        let draft_path = models_dir.join("qwen2.5-0.5b-instruct-q4_k_m.gguf");
        let tokenizer_path = models_dir.join("tokenizer.json");
        if !target_path.exists() || !draft_path.exists() || !tokenizer_path.exists() {
            eprintln!("skipping: local model pair not present on this host");
            return;
        }

        let tokenizer = Tokenizer::from_file(&tokenizer_path).expect("load tokenizer");
        let prompt = "The capital of France is";
        let prompt_tokens: Vec<u32> = tokenizer
            .encode(prompt, true)
            .expect("encode prompt")
            .get_ids()
            .to_vec();
        let eos_ids = eos_ids_for(&target_path);
        let max_tokens = 10usize;
        let repeat_penalty = 1.15f32;
        let repeat_last_n = 64usize;
        let task_handle = susi_agents::task_manager::SwarmTaskManager::global()
            .register_task("test_speculative_equivalence", prompt);

        // Speculative path.
        let mut target = load(&target_path);
        let mut draft = load(&draft_path);
        let streamed = std::cell::RefCell::new(String::new());
        let spec_output = SpeculativeDecoder::run(
            &mut target,
            &mut draft,
            &tokenizer,
            &prompt_tokens,
            &eos_ids,
            max_tokens,
            repeat_penalty,
            repeat_last_n,
            4,
            &task_handle,
            &|piece| streamed.borrow_mut().push_str(&piece),
        )
        .expect("speculative run must succeed");
        assert_eq!(*streamed.borrow(), spec_output);

        // Reference: plain greedy decoding, one token at a time, on a
        // fresh instance of the same target model.
        let mut reference_model = load(&target_path);
        let mut all_tokens: Vec<u32> = Vec::new();
        let mut next_input = prompt_tokens.clone();
        let mut pos = 0usize;
        for step in 0..max_tokens {
            let input = Tensor::new(next_input.as_slice(), &Device::Cpu)
                .unwrap()
                .unsqueeze(0)
                .unwrap();
            let logits = reference_model.forward(&input, pos).unwrap();
            let mut v: Vec<f32> = logits.flatten_all().unwrap().to_vec1().unwrap();
            let start = all_tokens.len().saturating_sub(repeat_last_n);
            apply_repeat_penalty(&mut v, repeat_penalty, &all_tokens[start..]);
            let tok = argmax(&v);
            all_tokens.push(tok);
            if eos_ids.contains(&tok) {
                break;
            }
            pos = if step == 0 {
                prompt_tokens.len()
            } else {
                pos + 1
            };
            next_input = vec![tok];
        }
        let reference_output = tokenizer
            .decode(&all_tokens, true)
            .expect("decode reference");

        assert_eq!(
            spec_output, reference_output,
            "speculative decoding must produce byte-identical output to plain greedy decoding"
        );
    }

    #[test]
    fn test_select_draft_model_ignores_files_at_or_above_target_size() {
        // Purely structural: a "draft" the same size as (or bigger than)
        // the target model can never pay for itself, and must never be
        // selected even if it happens to satisfy every other filter.
        let tmp = std::env::temp_dir().join("susi_test_speculative_select_draft_size");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let target_path = tmp.join("target.gguf");
        let same_size_path = tmp.join("same_size.gguf");
        std::fs::write(&target_path, vec![0u8; 100_000_000]).unwrap();
        std::fs::write(&same_size_path, vec![0u8; 100_000_000]).unwrap();

        let result = SpeculativeDecoder::select_draft_model_in(&tmp, &target_path, 100_000_000);
        assert!(
            result.is_none(),
            "a same-size or larger file must never be selected as a draft"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_select_draft_model_ignores_tiny_vocab_only_fixtures() {
        // llama.cpp's ggml-vocab-*.gguf test fixtures are a few KB and are
        // not real models; even though the size filter alone would let a
        // small-enough real model through, these must stay excluded.
        let tmp = std::env::temp_dir().join("susi_test_speculative_select_draft_tiny");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let target_path = tmp.join("target.gguf");
        let tiny_path = tmp.join("ggml-vocab-qwen2.gguf");
        std::fs::write(&target_path, vec![0u8; 5_000_000_000]).unwrap();
        std::fs::write(&tiny_path, vec![0u8; 4_096]).unwrap();

        let result = SpeculativeDecoder::select_draft_model_in(&tmp, &target_path, 5_000_000_000);
        assert!(
            result.is_none(),
            "a KB-scale non-model file must never be selected as a draft"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
