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
}
