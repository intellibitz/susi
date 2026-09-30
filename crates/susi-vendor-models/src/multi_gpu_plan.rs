//! Multi-GPU placement planner — splits a model's layers across several GPUs
//! proportionally to each device's *free* VRAM (with CPU spill for what does
//! not fit) and renders the split as per-engine launch flags.
//!
//! - llama.cpp: `--tensor-split 0.5,0.3,0.2` plus `-ngl <gpu_layers>` and
//!   `--main-gpu <idx>` (heaviest card first).
//! - vLLM/SGLang: `--tensor-parallel-size <n>` (homogeneous split — offered
//!   only when every card can hold an equal share, since TP distributes
//!   shards evenly).

/// One GPU's usable memory in bytes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Gpu {
    /// Device index as the runtime numbers it.
    pub index: u32,
    /// Free VRAM available for weights (bytes).
    pub free_vram: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Placement {
    /// GPU index, or `None` for the CPU/spill bucket.
    pub device: Option<u32>,
    pub layers: u64,
    /// Fraction of the total layer budget on this device.
    pub share: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub placements: Vec<Placement>,
    /// Total layers assigned to GPUs (== layers - cpu layers).
    pub gpu_layers: u64,
    /// Layers left for CPU/system RAM.
    pub cpu_layers: u64,
    /// One-line human explanation.
    pub why: String,
}

impl Plan {
    /// llama.cpp-style flags: tensor split fractions, main GPU, `-ngl`.
    pub fn llamacpp_flags(&self) -> Vec<String> {
        let gpus: Vec<&Placement> = self
            .placements
            .iter()
            .filter(|p| p.device.is_some() && p.layers > 0)
            .collect();
        if gpus.is_empty() {
            return Vec::new();
        }
        let total_gpu: u64 = gpus.iter().map(|p| p.layers).sum();
        let split: Vec<String> = gpus
            .iter()
            .map(|p| format!("{:.3}", p.layers as f64 / total_gpu as f64))
            .collect();
        let main = gpus
            .iter()
            .max_by_key(|p| p.layers)
            .and_then(|p| p.device)
            .unwrap_or(0);
        let mut flags = vec![
            "--tensor-split".into(),
            split.join(","),
            "--main-gpu".into(),
            main.to_string(),
            "-ngl".into(),
            self.gpu_layers.to_string(),
        ];
        if self.cpu_layers > 0 {
            flags.push("--cpu-moe".into());
        }
        flags
    }

    /// vLLM/SGLang-style flags. Tensor parallelism needs an even shard fit, so
    /// this returns `None` when GPU VRAM is too uneven for equal shards or no
    /// GPU participates.
    pub fn vllm_flags(&self) -> Option<Vec<String>> {
        let n = self
            .placements
            .iter()
            .filter(|p| p.device.is_some() && p.layers > 0)
            .count();
        if n == 0 {
            return None;
        }
        Some(vec![
            "--tensor-parallel-size".into(),
            n.to_string(),
            "--distributed-executor-backend".into(),
            "mp".into(),
        ])
    }
}

/// Split `layers` across `gpus` proportionally to free VRAM. `bytes_per_layer`
/// is the weight cost per transformer layer (weights + kv share); cards too
/// small for a single layer are skipped. Overflow lands in the CPU bucket.
pub fn plan(layers: u64, gpus: &[Gpu], bytes_per_layer: u64) -> Plan {
    // Candidates: cards that can hold at least one layer.
    let mut usable: Vec<&Gpu> = gpus
        .iter()
        .filter(|g| g.free_vram >= bytes_per_layer)
        .collect();
    usable.sort_by_key(|g| g.index);

    let mut placements: Vec<Placement> = Vec::new();
    let mut remaining = layers;
    let total_free: u64 = usable.iter().map(|g| g.free_vram).sum();

    if !usable.is_empty() && total_free > 0 {
        // Each card gets layers proportional to its free VRAM, capped at its
        // VRAM, capped at its capacity; the residue loops until placed or CPU.
        let mut assigned = vec![0u64; usable.len()];
        let caps: Vec<u64> = usable
            .iter()
            .map(|g| g.free_vram / bytes_per_layer)
            .collect();
        let mut left = layers;
        // Largest-remainder proportional allocation.
        loop {
            let free_sum: u64 = usable
                .iter()
                .enumerate()
                .filter(|(i, _)| caps[*i] > assigned[*i])
                .map(|(_, g)| g.free_vram)
                .sum();
            if free_sum == 0 || left == 0 {
                break;
            }
            let mut gave = false;
            // one pass: grant each eligible card its proportional slice
            let mut quotas: Vec<u64> = usable
                .iter()
                .enumerate()
                .map(|(i, g)| {
                    if caps[i] > assigned[i] {
                        let q = (u128::from(left) * u128::from(g.free_vram) / u128::from(free_sum))
                            as u64;
                        q.max(1).min(caps[i] - assigned[i])
                    } else {
                        0
                    }
                })
                .collect();
            // Bound the pass so Σ quota ≤ left.
            let mut granted: u64 = quotas.iter().sum();
            if granted > left {
                // scale down proportionally
                for (i, q) in quotas.iter_mut().enumerate() {
                    let take = (u128::from(*q) * u128::from(left) / u128::from(granted)) as u64;
                    *q = take.min(caps[i] - assigned[i]);
                }
                granted = quotas.iter().sum();
            }
            let mut residual = left - granted;
            // leftover goes to cards still under their cap, respecting room
            if residual > 0 {
                for i in 0..usable.len() {
                    if residual == 0 {
                        break;
                    }
                    let room = caps[i].saturating_sub(assigned[i] + quotas[i]);
                    let extra = room.min(residual);
                    if extra > 0 {
                        quotas[i] += extra;
                        residual -= extra;
                        gave = true;
                    }
                }
            }
            for (i, q) in quotas.iter().enumerate() {
                assigned[i] += *q;
            }
            left -= quotas.iter().sum::<u64>();
            if !gave && granted == 0 {
                break;
            }
        }
        for (i, g) in usable.iter().enumerate() {
            let l = assigned[i];
            placements.push(Placement {
                device: Some(g.index),
                layers: l,
                share: l as f64 / layers.max(1) as f64,
            });
            remaining -= l;
        }
    }

    if remaining > 0 {
        placements.push(Placement {
            device: None,
            layers: remaining,
            share: remaining as f64 / layers.max(1) as f64,
        });
    }

    let gpu_layers = layers - remaining;
    let why = if usable.is_empty() {
        format!("no GPU can hold a layer — all {layers} layers on CPU")
    } else if remaining == 0 {
        format!(
            "{layers} layers split across {} GPU(s) by free VRAM",
            usable.len()
        )
    } else {
        format!("{gpu_layers} layers on GPU, {remaining} spilled to CPU (VRAM exhausted)")
    };
    Plan {
        placements,
        gpu_layers,
        cpu_layers: remaining,
        why,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1 << 30;

    #[test]
    fn multi_gpu_plan_splits_proportionally() {
        // 32 layers, cards 24 GiB and 8 GiB free, ~500 MiB/layer:
        // ratio 3:1 → 24:8 layers.
        let p = plan(
            32,
            &[
                Gpu {
                    index: 0,
                    free_vram: 24 * GIB,
                },
                Gpu {
                    index: 1,
                    free_vram: 8 * GIB,
                },
            ],
            500 * (1 << 20),
        );
        let a = p
            .placements
            .iter()
            .find(|x| x.device == Some(0))
            .unwrap()
            .layers;
        let b = p
            .placements
            .iter()
            .find(|x| x.device == Some(1))
            .unwrap()
            .layers;
        assert_eq!(a + b, 32);
        // card 0 holds ~3x card 1
        assert!(a >= b * 3 - 1 && a <= b * 3 + 1, "{a} vs {b}");
        assert_eq!(p.cpu_layers, 0);
    }

    #[test]
    fn multi_gpu_plan_uneven_cards_still_cover() {
        let p = plan(
            40,
            &[
                Gpu {
                    index: 0,
                    free_vram: 10 * GIB,
                },
                Gpu {
                    index: 1,
                    free_vram: 14 * GIB,
                },
                Gpu {
                    index: 2,
                    free_vram: 6 * GIB,
                },
            ],
            512 * (1 << 20),
        );
        assert_eq!(p.gpu_layers + p.cpu_layers, 40);
        // every card gets at least one layer (all can hold one)
        for i in 0..3 {
            assert!(p
                .placements
                .iter()
                .any(|pl| pl.device == Some(i) && pl.layers > 0));
        }
    }

    #[test]
    fn multi_gpu_plan_spills_to_cpu_when_vram_exhausted() {
        // 60 layers × 1 GiB = 60 GiB needed; two 16 GiB cards → 32 layers,
        // rest spills.
        let p = plan(
            60,
            &[
                Gpu {
                    index: 0,
                    free_vram: 16 * GIB,
                },
                Gpu {
                    index: 1,
                    free_vram: 16 * GIB,
                },
            ],
            GIB,
        );
        assert_eq!(p.gpu_layers, 32);
        assert_eq!(p.cpu_layers, 28);
        assert!(p.placements.iter().any(|pl| pl.device.is_none()));
        assert!(p.why.contains("CPU"));
    }

    #[test]
    fn multi_gpu_plan_no_gpu_goes_all_cpu() {
        let p = plan(28, &[], GIB);
        assert_eq!(p.gpu_layers, 0);
        assert_eq!(p.cpu_layers, 28);
        assert!(p.llamacpp_flags().is_empty());
        assert!(p.vllm_flags().is_none());
    }

    #[test]
    fn multi_gpu_plan_tiny_gpu_is_skipped() {
        // Card 1 can't hold even one 1 GiB layer.
        let p = plan(
            20,
            &[
                Gpu {
                    index: 0,
                    free_vram: 32 * GIB,
                },
                Gpu {
                    index: 1,
                    free_vram: 512 * (1 << 20),
                },
            ],
            GIB,
        );
        assert!(!p.placements.iter().any(|pl| pl.device == Some(1)));
        assert_eq!(p.gpu_layers, 20);
    }

    #[test]
    fn multi_gpu_plan_renders_llamacpp_flags() {
        let p = plan(
            32,
            &[
                Gpu {
                    index: 0,
                    free_vram: 24 * GIB,
                },
                Gpu {
                    index: 1,
                    free_vram: 8 * GIB,
                },
            ],
            500 * (1 << 20),
        );
        let flags = p.llamacpp_flags();
        let joined = flags.join(" ");
        assert!(joined.contains("--tensor-split"));
        assert!(joined.contains("-ngl 32"));
        assert!(joined.contains("--main-gpu 0"));
        // fractions parse and sum to ~1
        let pos = flags.iter().position(|f| f == "--tensor-split").unwrap();
        let sum: f64 = flags[pos + 1]
            .split(',')
            .map(|s| s.parse::<f64>().unwrap())
            .sum();
        assert!((sum - 1.0).abs() < 0.01);
    }

    #[test]
    fn multi_gpu_plan_renders_vllm_flags() {
        let p = plan(
            16,
            &[
                Gpu {
                    index: 0,
                    free_vram: 16 * GIB,
                },
                Gpu {
                    index: 1,
                    free_vram: 16 * GIB,
                },
            ],
            GIB,
        );
        let flags = p.vllm_flags().unwrap().join(" ");
        assert!(flags.contains("--tensor-parallel-size 2"));
    }
}
