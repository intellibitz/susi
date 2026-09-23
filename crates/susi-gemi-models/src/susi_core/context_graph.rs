// Universal Context Graph: a process-wide, evidence-backed graph of entities,
// observations, and relations so SUSI agents do not operate in isolated silos.
//
// The graph is stored as an append-only JSONL event log that can be replayed
// into an in-memory index. Storage path is supplied by the composition root;
// when none is set, the graph stays in-memory only.

use crate::susi_error::{EaiError, EaiResult};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

/// Stable identifier for a node in the context graph.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct NodeId(pub String);

impl NodeId {
    /// Derive a stable node id from a node kind and a natural key.
    pub fn stable(kind: &str, key: &str) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(kind.as_bytes());
        hasher.update(b":");
        hasher.update(key.as_bytes());
        Self(format!("{kind}_{}", hex::encode(hasher.finalize())))
    }
}

/// Categories of entities tracked in the universal context graph.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NodeType {
    /// Human operator.
    User,
    /// Project / working directory.
    Workspace,
    /// One SUSI invocation with a goal.
    Mission,
    /// Recruited swarm agent.
    Agent,
    /// Registered tool or MCP capability.
    Tool,
    /// Concrete invocation of a tool within a mission.
    ToolCall,
    /// File observed or touched in a workspace.
    File,
    /// Inference provider endpoint.
    Provider,
    /// Specific model identifier.
    Model,
    /// MCP server instance.
    McpServer,
    /// Free-form agent observation or intermediate result.
    Observation,
    /// Context captured outside SUSI (e.g. OS-level adapter in the future).
    ExternalContext,
    /// Host telemetry snapshot (thermal / battery / load).
    Telemetry,
}

/// Kinds of edges between context-graph nodes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EdgeType {
    /// User -> Mission: user invoked this mission.
    Invoked,
    /// Agent -> Mission: agent participated in this mission.
    ParticipatedIn,
    /// Mission -> ToolCall: mission produced a tool call.
    Called,
    /// ToolCall -> Observation/File: call produced an output or artifact.
    Produced,
    /// Node -> Node: generic reference.
    References,
    /// Node -> Node: dependency.
    DependsOn,
    /// Agent/Observation -> Node: observed something.
    Observed,
    /// Mission -> Mission: temporal continuation.
    Continues,
    /// Node -> Node: parent/child containment.
    HasParent,
    /// ToolCall -> Tool: call used a registered tool.
    Used,
    /// Mission -> Workspace: mission occurred inside a workspace.
    OccurredIn,
    /// ExternalContext -> Workspace/User: context was ingested from an adapter.
    IngestedBy,
}

/// One node in the universal context graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    /// Stable node identifier.
    pub id: NodeId,
    /// Entity category.
    pub kind: NodeType,
    /// Human-readable label.
    pub label: String,
    /// Unix seconds when the node was first recorded.
    pub created_at: u64,
    /// Arbitrary structured properties (tool arguments, hashes, summaries, etc.).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub properties: HashMap<String, serde_json::Value>,
}

/// One directed edge in the universal context graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    /// Stable edge identifier (sha256 of source + kind + target + created_at).
    pub id: String,
    /// Source node.
    pub source: NodeId,
    /// Target node.
    pub target: NodeId,
    /// Relation kind.
    pub kind: EdgeType,
    /// Unix seconds when the edge was recorded.
    pub created_at: u64,
    /// Optional properties (e.g. receipt id, summary hash).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub properties: HashMap<String, serde_json::Value>,
}

/// Event-sourced mutation log for the context graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextGraphEvent {
    /// A new node was recorded.
    NodeAdded(Node),
    /// A new edge was recorded.
    EdgeAdded(Edge),
}

/// Subset of the graph returned by queries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subgraph {
    /// Nodes in the subgraph.
    pub nodes: Vec<Node>,
    /// Edges connecting those nodes.
    pub edges: Vec<Edge>,
}

/// Simple statistics about the loaded graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextGraphStats {
    /// Total node count.
    pub node_count: usize,
    /// Total edge count.
    pub edge_count: usize,
    /// Breakdown of node counts by kind.
    pub nodes_by_kind: HashMap<String, usize>,
    /// Breakdown of edge counts by kind.
    pub edges_by_kind: HashMap<String, usize>,
}

/// Process-wide universal context graph.
///
/// Stores nodes and edges in concurrent in-memory indexes and (when configured)
/// appends every mutation to a durable JSONL log. Queries return snapshots so
/// callers are isolated from concurrent writes.
pub struct ContextGraph {
    nodes: DashMap<NodeId, Node>,
    edges: DashMap<String, Edge>,
    adjacency: DashMap<NodeId, DashMap<NodeId, String>>,
    reverse: DashMap<NodeId, DashMap<NodeId, String>>,
    storage_path: parking_lot::Mutex<Option<PathBuf>>,
}

impl Default for ContextGraph {
    fn default() -> Self {
        Self::new()
    }
}

impl ContextGraph {
    /// Create an in-memory-only context graph.
    pub fn new() -> Self {
        Self {
            nodes: DashMap::new(),
            edges: DashMap::new(),
            adjacency: DashMap::new(),
            reverse: DashMap::new(),
            storage_path: parking_lot::Mutex::new(None),
        }
    }

    /// Create a context graph that persists to `path`.
    pub fn with_storage(path: PathBuf) -> Self {
        let graph = Self::new();
        *graph.storage_path.lock() = Some(path);
        graph
    }

    /// Global process-wide graph. Composition roots may bind durable storage
    /// with [`init_global_storage`]; until then the global graph is in-memory.
    pub fn global() -> &'static Self {
        static GRAPH: OnceLock<ContextGraph> = OnceLock::new();
        GRAPH.get_or_init(Self::new)
    }

    /// Set the durable storage path for the global graph. Call once from a
    /// composition root (CLI / daemon). Later calls are ignored.
    pub fn init_global_storage(path: PathBuf) {
        let graph = Self::global();
        {
            let mut guard = graph.storage_path.lock();
            if guard.is_some() {
                return;
            }
            *guard = Some(path);
        }
        // Best-effort replay of any existing log; released the storage lock first
        // because `replay` also needs to read it and parking_lot is not reentrant.
        let _ = graph.replay();
        // Auto-compact if the append-only log grew past 16 MiB.
        let _ = graph.compact_if_large(16 * 1024 * 1024);
    }

    /// Current unix timestamp, or 0 on failure.
    fn now() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    fn append_event(&self, event: &ContextGraphEvent) -> EaiResult<()> {
        let path = self.storage_path.lock().clone();
        let Some(path) = path else {
            return Ok(());
        };
        let line = serde_json::to_string(event)?;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| EaiError::filesystem(e.to_string()))?;
        use std::io::Write;
        writeln!(file, "{line}").map_err(|e| EaiError::filesystem(e.to_string()))?;
        Ok(())
    }

    fn index_edge(&self, edge: &Edge) {
        self.adjacency
            .entry(edge.source.clone())
            .or_default()
            .insert(edge.target.clone(), edge.id.clone());
        self.reverse
            .entry(edge.target.clone())
            .or_default()
            .insert(edge.source.clone(), edge.id.clone());
    }

    fn edge_id(source: &NodeId, kind: &EdgeType, target: &NodeId, at: u64) -> String {
        let mut hasher = Sha256::new();
        hasher.update(source.0.as_bytes());
        hasher.update(format!("{kind:?}").as_bytes());
        hasher.update(target.0.as_bytes());
        hasher.update(at.to_string().as_bytes());
        hex::encode(hasher.finalize())
    }

    /// Record a node and return its id. Duplicate nodes (same id) are ignored.
    pub fn record_node(&self, node: Node) -> NodeId {
        let id = node.id.clone();
        if !self.nodes.contains_key(&id) {
            self.nodes.insert(id.clone(), node.clone());
            let _ = self.append_event(&ContextGraphEvent::NodeAdded(node));
        }
        id
    }

    /// Record a directed edge and return its id. Duplicate edges are ignored.
    pub fn record_edge(&self, edge: Edge) -> String {
        if !self.edges.contains_key(&edge.id) {
            self.edges.insert(edge.id.clone(), edge.clone());
            self.index_edge(&edge);
            let _ = self.append_event(&ContextGraphEvent::EdgeAdded(edge.clone()));
        }
        edge.id.clone()
    }

    /// Convenience: ensure a workspace node exists and return its id.
    pub fn record_workspace(&self, workspace: &Path) -> NodeId {
        let canonical = workspace
            .canonicalize()
            .unwrap_or_else(|_| workspace.to_path_buf());
        let id = NodeId::stable("workspace", &canonical.to_string_lossy());
        self.record_node(Node {
            id,
            kind: NodeType::Workspace,
            label: canonical.to_string_lossy().into_owned(),
            created_at: Self::now(),
            properties: [("path".into(), canonical.to_string_lossy().into())]
                .into_iter()
                .collect(),
        })
    }

    /// Convenience: ensure a user node exists.
    pub fn record_user(&self, username: &str) -> NodeId {
        let id = NodeId::stable("user", username);
        self.record_node(Node {
            id,
            kind: NodeType::User,
            label: username.into(),
            created_at: Self::now(),
            properties: HashMap::new(),
        })
    }

    /// Record a mission node and link it to its workspace.
    pub fn record_mission(
        &self,
        mission_id: &str,
        goal: &str,
        workspace: &Path,
        user: Option<&str>,
    ) -> NodeId {
        let ws_id = self.record_workspace(workspace);
        let id = NodeId::stable("mission", mission_id);
        self.record_node(Node {
            id: id.clone(),
            kind: NodeType::Mission,
            label: goal.chars().take(120).collect(),
            created_at: Self::now(),
            properties: [
                ("mission_id".into(), mission_id.into()),
                ("goal".into(), goal.into()),
            ]
            .into_iter()
            .collect(),
        });
        let at = Self::now();
        self.record_edge(Edge {
            id: Self::edge_id(&id, &EdgeType::OccurredIn, &ws_id, at),
            source: id.clone(),
            target: ws_id,
            kind: EdgeType::OccurredIn,
            created_at: at,
            properties: HashMap::new(),
        });
        if let Some(user) = user {
            let user_id = self.record_user(user);
            let at = Self::now();
            self.record_edge(Edge {
                id: Self::edge_id(&user_id, &EdgeType::Invoked, &id, at),
                source: user_id,
                target: id.clone(),
                kind: EdgeType::Invoked,
                created_at: at,
                properties: HashMap::new(),
            });
        }
        id
    }

    /// Record a tool-call node and link it to the mission, workspace, and tool.
    #[allow(clippy::too_many_arguments)]
    pub fn record_tool_call(
        &self,
        mission_id: &str,
        receipt_id: &str,
        tool_name: &str,
        arguments: &serde_json::Value,
        workspace: &Path,
    ) -> NodeId {
        let mission = NodeId::stable("mission", mission_id);
        let ws_id = self.record_workspace(workspace);
        let tool_id = NodeId::stable("tool", tool_name);
        self.record_node(Node {
            id: tool_id.clone(),
            kind: NodeType::Tool,
            label: tool_name.into(),
            created_at: Self::now(),
            properties: HashMap::new(),
        });
        let call_key = format!("{mission_id}:{receipt_id}");
        let call_id = NodeId::stable("tool_call", &call_key);
        self.record_node(Node {
            id: call_id.clone(),
            kind: NodeType::ToolCall,
            label: format!("{tool_name} #{receipt_id}"),
            created_at: Self::now(),
            properties: [
                ("receipt_id".into(), receipt_id.into()),
                ("tool".into(), tool_name.into()),
                (
                    "arguments_digest".into(),
                    sha256_hex(&arguments.to_string()).into(),
                ),
            ]
            .into_iter()
            .collect(),
        });
        let at = Self::now();
        self.record_edge(Edge {
            id: Self::edge_id(&mission, &EdgeType::Called, &call_id, at),
            source: mission,
            target: call_id.clone(),
            kind: EdgeType::Called,
            created_at: at,
            properties: HashMap::new(),
        });
        let at = Self::now();
        self.record_edge(Edge {
            id: Self::edge_id(&call_id, &EdgeType::Used, &tool_id, at),
            source: call_id.clone(),
            target: tool_id,
            kind: EdgeType::Used,
            created_at: at,
            properties: HashMap::new(),
        });
        let at = Self::now();
        self.record_edge(Edge {
            id: Self::edge_id(&call_id, &EdgeType::OccurredIn, &ws_id, at),
            source: call_id.clone(),
            target: ws_id,
            kind: EdgeType::OccurredIn,
            created_at: at,
            properties: HashMap::new(),
        });
        call_id
    }

    /// Record that an agent produced an observation during a mission.
    /// `mission_id` is optional; when omitted the observation is linked only to
    /// the workspace.
    pub fn record_agent_observation(
        &self,
        mission_id: Option<&str>,
        agent_name: &str,
        observation: &str,
        workspace: &Path,
    ) -> NodeId {
        let agent_id = NodeId::stable("agent", agent_name);
        self.record_node(Node {
            id: agent_id.clone(),
            kind: NodeType::Agent,
            label: agent_name.into(),
            created_at: Self::now(),
            properties: HashMap::new(),
        });
        let obs_key = format!(
            "{}:{agent_name}:{}:{}",
            mission_id.unwrap_or("workspace"),
            Self::now(),
            sha256_hex(observation)
        );
        let obs_id = NodeId::stable("observation", &obs_key);
        self.record_node(Node {
            id: obs_id.clone(),
            kind: NodeType::Observation,
            label: observation.chars().take(200).collect(),
            created_at: Self::now(),
            properties: [
                ("agent".into(), agent_name.into()),
                (
                    "summary".into(),
                    observation.chars().take(500).collect::<String>().into(),
                ),
            ]
            .into_iter()
            .collect(),
        });
        if let Some(mission_id) = mission_id {
            let mission = NodeId::stable("mission", mission_id);
            let at = Self::now();
            self.record_edge(Edge {
                id: Self::edge_id(&agent_id, &EdgeType::ParticipatedIn, &mission, at),
                source: agent_id.clone(),
                target: mission,
                kind: EdgeType::ParticipatedIn,
                created_at: at,
                properties: HashMap::new(),
            });
        }
        let ws_id = self.record_workspace(workspace);
        let at = Self::now();
        self.record_edge(Edge {
            id: Self::edge_id(&agent_id, &EdgeType::Observed, &obs_id, at),
            source: agent_id.clone(),
            target: obs_id.clone(),
            kind: EdgeType::Observed,
            created_at: at,
            properties: HashMap::new(),
        });
        let at = Self::now();
        self.record_edge(Edge {
            id: Self::edge_id(&obs_id, &EdgeType::OccurredIn, &ws_id, at),
            source: obs_id.clone(),
            target: ws_id,
            kind: EdgeType::OccurredIn,
            created_at: at,
            properties: HashMap::new(),
        });
        obs_id
    }

    /// Record an external context observation from an adapter.
    /// `source` is the adapter identifier (e.g. "linux_proc", "active_window").
    #[allow(clippy::too_many_arguments)]
    pub fn record_external_context(
        &self,
        source: &str,
        label: &str,
        payload: &serde_json::Value,
        workspace: Option<&Path>,
        user: Option<&str>,
    ) -> NodeId {
        let source_id = NodeId::stable("external_source", source);
        self.record_node(Node {
            id: source_id.clone(),
            kind: NodeType::Agent,
            label: source.into(),
            created_at: Self::now(),
            properties: [("adapter".into(), source.into())].into_iter().collect(),
        });
        let ctx_key = format!(
            "{source}:{}:{}",
            Self::now(),
            sha256_hex(&payload.to_string())
        );
        let ctx_id = NodeId::stable("external_context", &ctx_key);
        self.record_node(Node {
            id: ctx_id.clone(),
            kind: NodeType::ExternalContext,
            label: label.chars().take(200).collect(),
            created_at: Self::now(),
            properties: [
                ("source".into(), source.into()),
                (
                    "payload_digest".into(),
                    sha256_hex(&payload.to_string()).into(),
                ),
                ("payload".into(), payload.clone()),
            ]
            .into_iter()
            .collect(),
        });
        let at = Self::now();
        self.record_edge(Edge {
            id: Self::edge_id(&source_id, &EdgeType::IngestedBy, &ctx_id, at),
            source: source_id,
            target: ctx_id.clone(),
            kind: EdgeType::IngestedBy,
            created_at: at,
            properties: HashMap::new(),
        });
        if let Some(user) = user {
            let user_id = self.record_user(user);
            let at = Self::now();
            self.record_edge(Edge {
                id: Self::edge_id(&ctx_id, &EdgeType::References, &user_id, at),
                source: ctx_id.clone(),
                target: user_id,
                kind: EdgeType::References,
                created_at: at,
                properties: HashMap::new(),
            });
        }
        if let Some(workspace) = workspace {
            let ws_id = self.record_workspace(workspace);
            let at = Self::now();
            self.record_edge(Edge {
                id: Self::edge_id(&ctx_id, &EdgeType::OccurredIn, &ws_id, at),
                source: ctx_id.clone(),
                target: ws_id,
                kind: EdgeType::OccurredIn,
                created_at: at,
                properties: HashMap::new(),
            });
        }
        ctx_id
    }

    /// Record a host telemetry snapshot into the graph.
    pub fn record_telemetry(
        &self,
        snapshot: &crate::susi_core::telemetry::TelemetrySnapshot,
        workspace: Option<&Path>,
    ) -> NodeId {
        let payload = serde_json::to_value(snapshot).unwrap_or_default();
        let label = format!(
            "telemetry: max {} C, {} battery, load {:?}",
            snapshot
                .max_temp_c()
                .map(|f| format!("{:.1}", f))
                .unwrap_or_else(|| "n/a".into()),
            snapshot.batteries.len(),
            snapshot.load_avg_1m,
        );
        let key = format!("{}:{}", Self::now(), sha256_hex(&payload.to_string()));
        let id = NodeId::stable("telemetry", &key);
        self.record_node(Node {
            id: id.clone(),
            kind: NodeType::Telemetry,
            label: label.chars().take(200).collect(),
            created_at: Self::now(),
            properties: [
                ("snapshot".into(), payload),
                (
                    "critical_battery".into(),
                    snapshot.critical_battery().into(),
                ),
            ]
            .into_iter()
            .collect(),
        });
        if let Some(workspace) = workspace {
            let ws_id = self.record_workspace(workspace);
            let at = Self::now();
            self.record_edge(Edge {
                id: Self::edge_id(&id, &EdgeType::OccurredIn, &ws_id, at),
                source: id.clone(),
                target: ws_id,
                kind: EdgeType::OccurredIn,
                created_at: at,
                properties: HashMap::new(),
            });
        }
        id
    }

    /// Lookup a node by id.
    pub fn node(&self, id: &NodeId) -> Option<Node> {
        self.nodes.get(id).map(|v| v.value().clone())
    }

    /// Lookup an edge by id.
    pub fn edge(&self, id: &str) -> Option<Edge> {
        self.edges.get(id).map(|v| v.value().clone())
    }

    /// Recent events by descending timestamp, capped at `n`.
    pub fn recent_events(&self, n: usize) -> Vec<ContextGraphEvent> {
        let mut events: Vec<ContextGraphEvent> =
            Vec::with_capacity(self.nodes.len().saturating_add(self.edges.len()).min(1024));
        for node in self.nodes.iter() {
            events.push(ContextGraphEvent::NodeAdded(node.value().clone()));
        }
        for edge in self.edges.iter() {
            events.push(ContextGraphEvent::EdgeAdded(edge.value().clone()));
        }
        events.sort_by(|a, b| {
            let ta = match a {
                ContextGraphEvent::NodeAdded(n) => n.created_at,
                ContextGraphEvent::EdgeAdded(e) => e.created_at,
            };
            let tb = match b {
                ContextGraphEvent::NodeAdded(n) => n.created_at,
                ContextGraphEvent::EdgeAdded(e) => e.created_at,
            };
            tb.cmp(&ta)
        });
        events.truncate(n);
        events
    }

    /// Subgraph containing all nodes reachable from `start` within `depth` hops.
    pub fn related(&self, start: &NodeId, depth: usize) -> Subgraph {
        let mut visited: HashSet<NodeId> = HashSet::new();
        let mut queue: VecDeque<(NodeId, usize)> = VecDeque::new();
        queue.push_back((start.clone(), 0));
        visited.insert(start.clone());

        while let Some((current, d)) = queue.pop_front() {
            if d >= depth {
                continue;
            }
            if let Some(neighbors) = self.adjacency.get(&current) {
                for target in neighbors.value().iter() {
                    let target = target.key().clone();
                    if visited.insert(target.clone()) {
                        queue.push_back((target, d + 1));
                    }
                }
            }
            if let Some(neighbors) = self.reverse.get(&current) {
                for source in neighbors.value().iter() {
                    let source = source.key().clone();
                    if visited.insert(source.clone()) {
                        queue.push_back((source, d + 1));
                    }
                }
            }
        }

        let nodes: Vec<Node> = visited.iter().filter_map(|id| self.node(id)).collect();
        let edge_ids: HashSet<String> = visited
            .iter()
            .filter_map(|id| self.adjacency.get(id))
            .flat_map(|m| {
                m.value()
                    .iter()
                    .map(|entry| entry.value().clone())
                    .collect::<Vec<_>>()
            })
            .collect();
        let edges: Vec<Edge> = edge_ids
            .into_iter()
            .filter_map(|id| self.edge(&id))
            .filter(|e| visited.contains(&e.source) && visited.contains(&e.target))
            .collect();
        Subgraph { nodes, edges }
    }

    /// Subgraph restricted to a single workspace.
    pub fn workspace_subgraph(&self, workspace: &Path) -> Subgraph {
        let canonical = workspace
            .canonicalize()
            .unwrap_or_else(|_| workspace.to_path_buf());
        let ws_id = NodeId::stable("workspace", &canonical.to_string_lossy());
        self.related(&ws_id, 3)
    }

    /// Aggregate statistics for the loaded graph.
    pub fn stats(&self) -> ContextGraphStats {
        let mut nodes_by_kind: HashMap<String, usize> = HashMap::new();
        for node in self.nodes.iter() {
            *nodes_by_kind
                .entry(format!("{:?}", node.value().kind))
                .or_insert(0) += 1;
        }
        let mut edges_by_kind: HashMap<String, usize> = HashMap::new();
        for edge in self.edges.iter() {
            *edges_by_kind
                .entry(format!("{:?}", edge.value().kind))
                .or_insert(0) += 1;
        }
        ContextGraphStats {
            node_count: self.nodes.len(),
            edge_count: self.edges.len(),
            nodes_by_kind,
            edges_by_kind,
        }
    }

    /// Replay an append-only JSONL log into this graph.
    pub fn replay(&self) -> EaiResult<()> {
        let path = self.storage_path.lock().clone();
        let Some(path) = path else {
            return Ok(());
        };
        if !path.is_file() {
            return Ok(());
        }
        let text =
            std::fs::read_to_string(&path).map_err(|e| EaiError::filesystem(e.to_string()))?;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let event: ContextGraphEvent = serde_json::from_str(line)?;
            match event {
                ContextGraphEvent::NodeAdded(node) => {
                    self.nodes.entry(node.id.clone()).or_insert(node);
                }
                ContextGraphEvent::EdgeAdded(edge) => {
                    self.edges.entry(edge.id.clone()).or_insert_with(|| {
                        self.index_edge(&edge);
                        edge
                    });
                }
            }
        }
        Ok(())
    }

    /// Persist the current in-memory graph by replaying it back to storage.
    /// Because every mutation is appended immediately, this normally does
    /// nothing; it is useful to compact/rewrite the log after bulk replay.
    pub fn persist(&self) -> EaiResult<()> {
        let path = self.storage_path.lock().clone();
        let Some(path) = path else {
            return Ok(());
        };
        let mut lines = Vec::new();
        for node in self.nodes.iter() {
            lines.push(ContextGraphEvent::NodeAdded(node.value().clone()));
        }
        for edge in self.edges.iter() {
            lines.push(ContextGraphEvent::EdgeAdded(edge.value().clone()));
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)
            .map_err(|e| EaiError::filesystem(e.to_string()))?;
        use std::io::Write;
        for event in lines {
            let line = serde_json::to_string(&event)?;
            writeln!(file, "{line}").map_err(|e| EaiError::filesystem(e.to_string()))?;
        }
        Ok(())
    }

    /// Compact the append-only log by rewriting it to contain only the
    /// deduplicated current state. Returns `(old_lines, new_lines)`.
    pub fn compact(&self) -> EaiResult<(usize, usize)> {
        let path = self.storage_path.lock().clone();
        let Some(path) = path else {
            return Ok((0, 0));
        };
        let old_lines = if path.is_file() {
            std::fs::read_to_string(&path)
                .map(|s| s.lines().count())
                .unwrap_or(0)
        } else {
            0
        };
        self.persist()?;
        let new_lines = self.nodes.len() + self.edges.len();
        Ok((old_lines, new_lines))
    }

    /// Best-effort auto-compaction when the log exceeds `max_bytes`.
    pub fn compact_if_large(&self, max_bytes: u64) -> EaiResult<()> {
        let path = self.storage_path.lock().clone();
        let Some(path) = path else {
            return Ok(());
        };
        let size = if path.is_file() {
            std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0)
        } else {
            0
        };
        if size > max_bytes {
            let _ = self.compact()?;
        }
        Ok(())
    }
}

fn sha256_hex(input: &str) -> String {
    hex::encode(Sha256::digest(input.as_bytes()))
}

