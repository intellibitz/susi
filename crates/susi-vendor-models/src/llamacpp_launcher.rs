//! llama-server launch planner — reads a GGUF's header, derives `-ngl` /
//! `-c` / `-t` from the host's memory and cores, and produces the argv and
//! health-check URL used to start the server. Pure planning: tests exercise
//! the maths without spawning anything.

use crate::gguf_inspector;
use crate::quant_recommender::Host;
use std::path::Path;
use susi_error::{EaiError, EaiResult};

/// Fraction of free VRAM the planner may spend on weights (rest is runtime,
/// KV growth, compositor).
const VRAM_BUDGET: f64 = 0.85;

/// What a planned llama-server launch looks like.
#[derive(Debug, Clone, PartialEq)]
pub struct Launch {
    pub argv: Vec<String>,
    /// `http://127.0.0.1:<port>/health` — poll until 200.
    pub health_url: String,
    /// Layers sent to the GPU (`-ngl`); `total` when fully offloaded.
    pub gpu_layers: u64,
    pub total_layers: u64,
    /// `-c` value actually chosen.
    pub context: u64,
    /// `-t` value actually chosen.
    pub threads: u32,
    /// `true` when every layer fits in VRAM.
    pub full_offload: bool,
    pub why: String,
}

/// Plan a llama-server launch for `gguf` on `host` listening on `port`.
///
/// - `-ngl` = layers whose per-layer weight share fits the VRAM budget
///   (all of them when the whole model fits, `0` when the host has no GPU
///   budget at all).
/// - `-c` = `min(requested_ctx, model ctx)` clamped `[512, 8192]`.
/// - `-t` = host cores minus one, clamped `[1, 64]`.
pub fn plan(gguf: &Path, spec: &Spec) -> EaiResult<Launch> {
    let bytes = std::fs::read(gguf)
        .map_err(|e| EaiError::io(format!("cannot read {}: {e}", gguf.display())))?;
    let info = gguf_inspector::inspect(&bytes)
        .map_err(|e| EaiError::config(format!("{}: {e}", gguf.display())))?;
    let size = bytes.len() as u64;
    Ok(plan_from(&info, size, gguf, spec))
}

/// Host-side launch parameters for `plan` / `plan_from`.
#[derive(Debug, Clone, Copy)]
pub struct Spec {
    pub host: Host,
    pub cores: u32,
    pub port: u16,
    /// Context the caller asked for; clamped into `[512, min(model_ctx, 8192)]`.
    pub requested_ctx: u64,
}

/// Planning maths split out so tests can feed synthetic headers/hosts without
/// touching the filesystem.
pub fn plan_from(
    info: &gguf_inspector::GgufInfo,
    file_bytes: u64,
    gguf: &Path,
    spec: &Spec,
) -> Launch {
    let layers = info.layer_count.unwrap_or(1).max(1);
    let bytes_per_layer = (file_bytes / layers).max(1);
    let vram_cap = (spec.host.vram as f64 * VRAM_BUDGET) as u64;
    let gpu_layers = if spec.host.vram == 0 {
        0
    } else {
        (vram_cap / bytes_per_layer).min(layers)
    };
    let full_offload = gpu_layers >= layers;
    let ngl = if full_offload { layers } else { gpu_layers };

    let model_ctx = info.context_length.unwrap_or(4096);
    let context = spec.requested_ctx.min(model_ctx).clamp(512, 8192);
    let threads = spec.cores.saturating_sub(1).clamp(1, 64);

    let mut argv = vec![
        "llama-server".to_string(),
        "-m".into(),
        gguf.display().to_string(),
        "--port".into(),
        spec.port.to_string(),
        "-c".into(),
        context.to_string(),
        "-t".into(),
        threads.to_string(),
        "-ngl".into(),
        ngl.to_string(),
    ];
    if !full_offload && gpu_layers > 0 {
        argv.push("--no-mmap".into()); // avoid pinning CPU pages we will not use
    }
    let why = if full_offload {
        format!(
            "model fits VRAM budget ({:.1} GiB file vs {:.1} GiB usable) — all {layers} layers on GPU",
            gib(file_bytes),
            gib(vram_cap)
        )
    } else if gpu_layers == 0 {
        format!("no VRAM budget — {layers} layers on CPU, {threads} threads")
    } else {
        format!(
            "VRAM holds {gpu_layers}/{layers} layers ({:.1} GiB usable) — rest on CPU",
            gib(vram_cap)
        )
    };
    Launch {
        argv,
        health_url: format!("http://127.0.0.1:{}/health", spec.port),
        gpu_layers: ngl,
        total_layers: layers,
        context,
        threads,
        full_offload,
        why,
    }
}

fn gib(b: u64) -> f64 {
    b as f64 / (1 << 30) as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gguf_inspector::GgufInfo;

    const GIB: u64 = 1 << 30;

    fn info(layers: u64, ctx: u64) -> GgufInfo {
        GgufInfo {
            layer_count: Some(layers),
            context_length: Some(ctx),
            ..GgufInfo::default()
        }
    }

    fn host(vram_gb: u64, ram_gb: u64) -> Host {
        Host {
            vram: vram_gb * GIB,
            ram: ram_gb * GIB,
            disk: 0,
        }
    }

    fn spec(vram_gb: u64, ram_gb: u64, cores: u32, port: u16, ctx: u64) -> Spec {
        Spec {
            host: host(vram_gb, ram_gb),
            cores,
            port,
            requested_ctx: ctx,
        }
    }

    #[test]
    fn llamacpp_launcher_full_offload_when_model_fits() {
        // 4 GiB file, 32 layers, 24 GiB card → every layer offloads.
        let l = plan_from(
            &info(32, 4096),
            4 * GIB,
            Path::new("m.gguf"),
            &spec(24, 64, 8, 8080, 4096),
        );
        assert!(l.full_offload);
        assert_eq!(l.gpu_layers, 32);
        assert!(l.argv.join(" ").contains("-ngl 32"));
    }

    #[test]
    fn llamacpp_launcher_partial_offload_splits_layers() {
        // 8 GiB file / 32 layers = 256 MiB per layer; 4 GiB card → ~13 layers
        // (4 GiB × 0.85 = 3.4 GiB / 256 MiB = 13).
        let l = plan_from(
            &info(32, 4096),
            8 * GIB,
            Path::new("m.gguf"),
            &spec(4, 64, 8, 8080, 4096),
        );
        assert!(!l.full_offload);
        assert_eq!(l.gpu_layers, 13);
        assert!(l.argv.join(" ").contains("-ngl 13"));
        assert!(l.argv.join(" ").contains("--no-mmap"));
    }

    #[test]
    fn llamacpp_launcher_cpu_only_when_no_vram() {
        let l = plan_from(
            &info(32, 4096),
            4 * GIB,
            Path::new("m.gguf"),
            &spec(0, 64, 8, 8080, 4096),
        );
        assert_eq!(l.gpu_layers, 0);
        assert!(l.argv.join(" ").contains("-ngl 0"));
    }

    #[test]
    fn llamacpp_launcher_context_clamped_to_model_and_cap() {
        let l = plan_from(
            &info(32, 2048),
            GIB,
            Path::new("m.gguf"),
            &spec(24, 64, 8, 8080, 8192),
        );
        assert_eq!(l.context, 2048); // model ctx wins
        let l = plan_from(
            &info(32, 128_000),
            GIB,
            Path::new("m.gguf"),
            &spec(24, 64, 8, 8080, 16_000),
        );
        assert_eq!(l.context, 8192); // planner cap wins
        let l = plan_from(
            &info(32, 4096),
            GIB,
            Path::new("m.gguf"),
            &spec(24, 64, 8, 8080, 128),
        );
        assert_eq!(l.context, 512); // floor
    }

    #[test]
    fn llamacpp_launcher_threads_leave_one_core() {
        let l = plan_from(
            &info(32, 4096),
            GIB,
            Path::new("m.gguf"),
            &spec(0, 64, 8, 8080, 4096),
        );
        assert_eq!(l.threads, 7);
        let l = plan_from(
            &info(32, 4096),
            GIB,
            Path::new("m.gguf"),
            &spec(0, 64, 1, 8080, 4096),
        );
        assert_eq!(l.threads, 1);
    }

    #[test]
    fn llamacpp_launcher_health_url_uses_port() {
        let l = plan_from(
            &info(32, 4096),
            GIB,
            Path::new("m.gguf"),
            &spec(0, 64, 8, 9191, 4096),
        );
        assert_eq!(l.health_url, "http://127.0.0.1:9191/health");
        assert!(l.argv.join(" ").contains("--port 9191"));
    }
}
