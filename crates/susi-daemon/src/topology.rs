//! Swarm Topology Engine (Swarm OS Bullet 24)
//!
//! Allows cells to autonomously discover and form peer-to-peer topologies
//! (e.g. Ring, Star, Mesh) based on task requirements, without a central coordinator.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::RwLock;

/// Recognized peer-to-peer swarm topologies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TopologyKind {
    Star,
    Ring,
    Mesh,
    Tree,
}

/// A node in the swarm topology.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopologyNode {
    pub cell_id: String,
    pub endpoint: String,
    pub capabilities: HashSet<String>,
}

/// Autonomously formed swarm topology.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwarmTopology {
    pub id: String,
    pub kind: TopologyKind,
    pub nodes: Vec<TopologyNode>,
    pub leader: Option<String>,
    pub edges: HashMap<String, Vec<String>>, // node -> neighbors
}

impl SwarmTopology {
    /// Autonomously forms a topology from a list of discovered nodes based on the requested kind.
    pub fn form(id: String, kind: TopologyKind, nodes: Vec<TopologyNode>) -> Result<Self, String> {
        if nodes.is_empty() {
            return Err("Cannot form topology with 0 nodes".to_string());
        }

        let mut edges = HashMap::new();
        let mut leader = None;

        match kind {
            TopologyKind::Star => {
                // The first node acts as the center/leader
                let center = nodes[0].cell_id.clone();
                leader = Some(center.clone());

                let mut center_neighbors = Vec::new();
                for node in nodes.iter().skip(1) {
                    center_neighbors.push(node.cell_id.clone());
                    edges.insert(node.cell_id.clone(), vec![center.clone()]);
                }
                edges.insert(center, center_neighbors);
            }
            TopologyKind::Ring => {
                // Form a logical ring: A -> B -> C -> A
                let n = nodes.len();
                for i in 0..n {
                    let current = nodes[i].cell_id.clone();
                    let next = nodes[(i + 1) % n].cell_id.clone();
                    let prev = nodes[(i + n - 1) % n].cell_id.clone();

                    let mut neighbors = vec![next];
                    if n > 2 {
                        neighbors.push(prev); // Bidirectional ring
                    }
                    edges.insert(current, neighbors);
                }
            }
            TopologyKind::Mesh => {
                // Fully connected mesh
                for node in &nodes {
                    let mut neighbors = Vec::new();
                    for other in &nodes {
                        if node.cell_id != other.cell_id {
                            neighbors.push(other.cell_id.clone());
                        }
                    }
                    edges.insert(node.cell_id.clone(), neighbors);
                }
            }
            TopologyKind::Tree => {
                return Err("Tree topology auto-formation not yet implemented".to_string());
            }
        }

        Ok(Self {
            id,
            kind,
            nodes,
            leader,
            edges,
        })
    }
}

pub struct TopologyManager {
    active_topologies: RwLock<HashMap<String, SwarmTopology>>,
}

impl TopologyManager {
    pub fn new() -> Self {
        Self {
            active_topologies: RwLock::new(HashMap::new()),
        }
    }

    /// Forms and registers a new topology.
    pub fn create_topology(
        &self,
        id: String,
        kind: TopologyKind,
        nodes: Vec<TopologyNode>,
    ) -> Result<(), String> {
        let topo = SwarmTopology::form(id.clone(), kind, nodes)?;
        self.active_topologies
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, topo);
        Ok(())
    }

    /// Retrieves an active topology by ID.
    pub fn get_topology(&self, id: &str) -> Option<SwarmTopology> {
        self.active_topologies
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .cloned()
    }
}

impl Default for TopologyManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_node(id: &str) -> TopologyNode {
        TopologyNode {
            cell_id: id.to_string(),
            endpoint: format!("ipc:///tmp/{}", id),
            capabilities: HashSet::new(),
        }
    }

    #[test]
    fn test_ring_topology_formation() {
        let nodes = vec![dummy_node("A"), dummy_node("B"), dummy_node("C")];
        let topo = SwarmTopology::form("ring-1".to_string(), TopologyKind::Ring, nodes).unwrap();

        assert_eq!(topo.kind, TopologyKind::Ring);
        assert_eq!(topo.edges.get("A").unwrap().len(), 2);
        assert!(topo.edges.get("A").unwrap().contains(&"B".to_string()));
        assert!(topo.edges.get("A").unwrap().contains(&"C".to_string()));
    }

    #[test]
    fn test_star_topology_formation() {
        let nodes = vec![
            dummy_node("Center"),
            dummy_node("Leaf1"),
            dummy_node("Leaf2"),
        ];
        let topo = SwarmTopology::form("star-1".to_string(), TopologyKind::Star, nodes).unwrap();

        assert_eq!(topo.leader.as_deref(), Some("Center"));
        assert_eq!(topo.edges.get("Center").unwrap().len(), 2);
        assert_eq!(topo.edges.get("Leaf1").unwrap().len(), 1);
        assert_eq!(topo.edges.get("Leaf1").unwrap()[0], "Center");
    }

    #[test]
    fn test_mesh_topology_formation() {
        let nodes = vec![dummy_node("N1"), dummy_node("N2"), dummy_node("N3")];
        let topo = SwarmTopology::form("mesh-1".to_string(), TopologyKind::Mesh, nodes).unwrap();

        assert_eq!(topo.edges.get("N1").unwrap().len(), 2);
        assert!(topo.edges.get("N1").unwrap().contains(&"N2".to_string()));
        assert!(topo.edges.get("N1").unwrap().contains(&"N3".to_string()));
    }
}
