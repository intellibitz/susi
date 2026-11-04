//! Cluster model placement: which node hosts which model.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeCapacity {
    pub id: String,
    pub free_vram_mb: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Placement {
    pub model: String,
    pub node: String,
}

/// Place each model on the node with the most free VRAM that fits.
#[must_use]
pub fn place_models(models: &[(String, u32)], nodes: &[NodeCapacity]) -> Vec<Placement> {
    let mut free: Vec<_> = nodes.to_vec();
    let mut out = Vec::new();
    for (model, need) in models {
        if let Some((idx, _)) = free
            .iter()
            .enumerate()
            .filter(|(_, n)| n.free_vram_mb >= *need)
            .max_by_key(|(_, n)| n.free_vram_mb)
        {
            let node = free[idx].id.clone();
            free[idx].free_vram_mb -= *need;
            out.push(Placement {
                model: model.clone(),
                node,
            });
        }
    }
    out
}

#[cfg(test)]
mod model_placement_tests {
    use super::*;

    #[test]
    fn model_placement_picks_fit_nodes() {
        let nodes = [
            NodeCapacity {
                id: "a".into(),
                free_vram_mb: 8_000,
            },
            NodeCapacity {
                id: "b".into(),
                free_vram_mb: 24_000,
            },
        ];
        let p = place_models(&[("big".into(), 16_000)], &nodes);
        assert_eq!(p[0].node, "b");
    }

    #[test]
    fn model_placement_deducts_vram_sequentially_across_multiple_models() {
        let nodes = [
            NodeCapacity {
                id: "node-1".into(),
                free_vram_mb: 16_000,
            },
            NodeCapacity {
                id: "node-2".into(),
                free_vram_mb: 20_000,
            },
        ];
        let models = [
            ("m1".to_string(), 12_000),
            ("m2".to_string(), 12_000),
            ("m3".to_string(), 6_000),
        ];
        let placed = place_models(&models, &nodes);
        assert_eq!(
            placed,
            vec![
                Placement {
                    model: "m1".into(),
                    node: "node-2".into()
                },
                Placement {
                    model: "m2".into(),
                    node: "node-1".into()
                },
                Placement {
                    model: "m3".into(),
                    node: "node-2".into()
                },
            ]
        );
    }

    #[test]
    fn model_placement_skips_oversized_models_and_handles_empty_inputs() {
        let nodes = [NodeCapacity {
            id: "small".into(),
            free_vram_mb: 8_000,
        }];
        let models = [("huge".to_string(), 80_000), ("tiny".to_string(), 4_000)];
        let placed = place_models(&models, &nodes);
        assert_eq!(
            placed,
            vec![Placement {
                model: "tiny".into(),
                node: "small".into()
            }]
        );
        assert!(place_models(&[], &nodes).is_empty());
        assert!(place_models(&models, &[]).is_empty());
    }

    #[test]
    fn node_capacity_and_placement_serde_roundtrip_and_exact_fit() {
        let cap = NodeCapacity {
            id: "exact".into(),
            free_vram_mb: 8_192,
        };
        let cap_json = serde_json::to_string(&cap).unwrap();
        let cap_back: NodeCapacity = serde_json::from_str(&cap_json).unwrap();
        assert_eq!(cap_back, cap.clone());
        assert!(format!("{cap:?}").contains("exact"));

        let placed = place_models(&[("fit".into(), 8_192), ("extra".into(), 1)], &[cap]);
        assert_eq!(placed.len(), 1);
        let p_json = serde_json::to_string(&placed[0]).unwrap();
        let p_back: Placement = serde_json::from_str(&p_json).unwrap();
        assert_eq!(p_back, placed[0]);
    }

    #[test]
    fn place_models_does_not_mutate_caller_node_capacities_and_places_zero_vram_model() {
        let nodes = [NodeCapacity {
            id: "zero-node".into(),
            free_vram_mb: 0,
        }];
        let placed = place_models(&[("cpu-only".into(), 0)], &nodes);
        assert_eq!(
            placed,
            vec![Placement {
                model: "cpu-only".into(),
                node: "zero-node".into(),
            }]
        );
        assert_eq!(nodes[0].free_vram_mb, 0);
    }
}
