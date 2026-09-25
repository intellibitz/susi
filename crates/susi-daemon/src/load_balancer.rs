//! Weighted Load Balancing across Cells (Swarm OS Bullet 19)
//!
//! Smooth weighted round-robin, the algorithm nginx upstreams use: each
//! pick advances every cell's counter by its weight, then returns (and
//! discounts) whichever counter is currently highest. This spreads picks
//! evenly across a cell's share of the rotation instead of bursting all
//! of a high-weight cell's turns together.

use std::sync::RwLock;

struct Weighted {
    cell_id: String,
    weight: i64,
    current: i64,
}

pub struct LoadBalancer {
    cells: RwLock<Vec<Weighted>>,
}

impl Default for LoadBalancer {
    fn default() -> Self {
        Self::new()
    }
}

impl LoadBalancer {
    pub fn new() -> Self {
        Self {
            cells: RwLock::new(Vec::new()),
        }
    }

    pub fn register_cell(&self, cell_id: &str, weight: i64) {
        self.cells
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .push(Weighted {
                cell_id: cell_id.to_string(),
                weight,
                current: 0,
            });
    }

    /// Picks the next cell via smooth weighted round-robin. `None` when no
    /// cells are registered.
    pub fn next_cell(&self) -> Option<String> {
        let mut cells = self.cells.write().unwrap_or_else(|e| e.into_inner());
        if cells.is_empty() {
            return None;
        }
        let total_weight: i64 = cells.iter().map(|c| c.weight).sum();
        for c in cells.iter_mut() {
            c.current += c.weight;
        }
        let idx = cells.iter().enumerate().max_by_key(|(_, c)| c.current)?.0;
        cells[idx].current -= total_weight;
        Some(cells[idx].cell_id.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn no_cells_registered_yields_none() {
        let lb = LoadBalancer::new();
        assert_eq!(lb.next_cell(), None);
    }

    #[test]
    fn picks_are_proportional_to_weight_over_a_full_cycle() {
        let lb = LoadBalancer::new();
        lb.register_cell("heavy", 3);
        lb.register_cell("light", 1);

        let mut counts: HashMap<String, u32> = HashMap::new();
        for _ in 0..4 {
            if let Some(id) = lb.next_cell() {
                *counts.entry(id).or_insert(0) += 1;
            }
        }
        assert_eq!(counts.get("heavy").copied(), Some(3));
        assert_eq!(counts.get("light").copied(), Some(1));
    }
}
