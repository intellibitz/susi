//! Versioned world model (Swarm OS Bullets 32, 34, 36, 37, 39)
//!
//! Context nodes are typed and versioned, with a parent link to the
//! previous version. `affects` edges answer which services a config
//! change touches. Compaction folds older versions into a summary node
//! so the latest state stays queryable. Evidence is an explicit receipt
//! id list. Reads and writes are gated per cell and per node type.

use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextNode {
    pub id: String,
    pub type_name: String,
    pub version: u64,
    pub parent_version: Option<u64>,
    pub namespace: String,
    pub region: String,
    pub payload: String,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypePerm {
    Read,
    Write,
}

#[derive(Debug, Clone)]
pub struct NodeDraft {
    pub id: String,
    pub type_name: String,
    pub namespace: String,
    pub region: String,
    pub payload: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub subject: String,
    pub reason: String,
}

pub struct WorldModel {
    versions: HashMap<String, Vec<ContextNode>>,
    affects: Vec<(String, String)>,
    type_acl: HashMap<(String, String), TypePerm>,
}

impl Default for WorldModel {
    fn default() -> Self {
        Self::new()
    }
}

impl WorldModel {
    pub fn new() -> Self {
        Self {
            versions: HashMap::new(),
            affects: Vec::new(),
            type_acl: HashMap::new(),
        }
    }

    pub fn grant(&mut self, cell_id: &str, type_name: &str, perm: TypePerm) {
        let key = (cell_id.to_string(), type_name.to_string());
        let merged = match (self.type_acl.get(&key).copied(), perm) {
            (Some(TypePerm::Write), _) | (_, TypePerm::Write) => TypePerm::Write,
            _ => TypePerm::Read,
        };
        self.type_acl.insert(key, merged);
    }

    fn allows(&self, cell_id: &str, type_name: &str, need: TypePerm) -> bool {
        match self
            .type_acl
            .get(&(cell_id.to_string(), type_name.to_string()))
            .copied()
        {
            Some(TypePerm::Write) => true,
            Some(TypePerm::Read) => need == TypePerm::Read,
            None => false,
        }
    }

    /// Appends a new version. The parent link is the previous version
    /// number when one exists.
    pub fn put(&mut self, cell_id: &str, draft: NodeDraft) -> Result<u64, String> {
        if !self.allows(cell_id, &draft.type_name, TypePerm::Write) {
            return Err(format!(
                "cell '{cell_id}' cannot write type '{}'",
                draft.type_name
            ));
        }
        let versions = self.versions.entry(draft.id.clone()).or_default();
        let parent_version = versions.last().map(|node| node.version);
        let version = parent_version.unwrap_or(0).saturating_add(1);
        versions.push(ContextNode {
            id: draft.id,
            type_name: draft.type_name,
            version,
            parent_version,
            namespace: draft.namespace,
            region: draft.region,
            payload: draft.payload,
            evidence: Vec::new(),
        });
        Ok(version)
    }

    pub fn read_as(&self, cell_id: &str, id: &str) -> Result<ContextNode, String> {
        let node = self.latest(id).ok_or_else(|| format!("no node '{id}'"))?;
        if !self.allows(cell_id, &node.type_name, TypePerm::Read) {
            return Err(format!(
                "cell '{cell_id}' cannot read type '{}'",
                node.type_name
            ));
        }
        Ok(node)
    }

    pub fn latest(&self, id: &str) -> Option<ContextNode> {
        self.versions
            .get(id)
            .and_then(|versions| versions.last().cloned())
    }

    pub fn latest_nodes(&self) -> Vec<ContextNode> {
        let mut nodes: Vec<ContextNode> = self
            .versions
            .values()
            .filter_map(|versions| versions.last().cloned())
            .collect();
        nodes.sort_by(|left, right| left.id.cmp(&right.id));
        nodes
    }

    /// Attaches a non-empty receipt id to one version (Bullet 37).
    pub fn attach_evidence(
        &mut self,
        id: &str,
        version: u64,
        receipt_id: &str,
    ) -> Result<(), String> {
        if receipt_id.is_empty() {
            return Err("evidence receipt id is empty".to_string());
        }
        let versions = self
            .versions
            .get_mut(id)
            .ok_or_else(|| format!("no node '{id}'"))?;
        let node = versions
            .iter_mut()
            .find(|node| node.version == version)
            .ok_or_else(|| format!("no version {version} of '{id}'"))?;
        if !node.evidence.iter().any(|existing| existing == receipt_id) {
            node.evidence.push(receipt_id.to_string());
        }
        Ok(())
    }

    pub fn link_affects(&mut self, from_id: &str, to_id: &str) {
        let edge = (from_id.to_string(), to_id.to_string());
        if !self.affects.contains(&edge) {
            self.affects.push(edge);
        }
    }

    /// Services this config node is recorded as affecting (Bullet 34).
    pub fn affected_by(&self, config_id: &str) -> Vec<String> {
        self.affects
            .iter()
            .filter(|(from, _)| from == config_id)
            .map(|(_, to)| to.clone())
            .collect()
    }

    pub fn affect_edges(&self) -> &[(String, String)] {
        &self.affects
    }

    /// Folds every version but the latest into `{id}#summary` (Bullet 36).
    /// Returns how many versions were folded. A single version is unchanged.
    pub fn compact(&mut self, id: &str) -> Result<u64, String> {
        let versions = self
            .versions
            .get_mut(id)
            .ok_or_else(|| format!("no node '{id}'"))?;
        if versions.len() <= 1 {
            return Ok(0);
        }
        let folded = versions.len() - 1;
        let dropped: Vec<ContextNode> = versions.drain(..folded).collect();
        let evidence_count: usize = dropped.iter().map(|node| node.evidence.len()).sum();
        let summary = ContextNode {
            id: format!("{id}#summary"),
            type_name: "summary".to_string(),
            version: 1,
            parent_version: None,
            namespace: dropped
                .first()
                .map(|node| node.namespace.clone())
                .unwrap_or_default(),
            region: dropped
                .first()
                .map(|node| node.region.clone())
                .unwrap_or_default(),
            payload: format!("folded:{folded};evidence:{evidence_count}"),
            evidence: Vec::new(),
        };
        self.versions
            .entry(summary.id.clone())
            .or_default()
            .push(summary);
        Ok(u64::try_from(folded).unwrap_or(u64::MAX))
    }
}

/// Structural anomalies: claims or receipts with no evidence, nodes outside
/// the allowed region set, and `affects` edges whose target was never written
/// (Bullet 59's scan input — the scan itself lives in `security_scan`).
pub fn structural_findings(model: &WorldModel, allowed_regions: &[String]) -> Vec<Finding> {
    let mut findings = Vec::new();
    for node in model.latest_nodes() {
        let needs_evidence = node.type_name == "claim" || node.type_name == "receipt";
        if needs_evidence && node.evidence.is_empty() {
            findings.push(Finding {
                subject: node.id.clone(),
                reason: format!("type '{}' has no evidence", node.type_name),
            });
        }
        if !allowed_regions.is_empty()
            && !allowed_regions.iter().any(|region| region == &node.region)
        {
            findings.push(Finding {
                subject: node.id.clone(),
                reason: format!("region '{}' is outside the allowed set", node.region),
            });
        }
    }
    for (from, to) in model.affect_edges() {
        if model.latest(to).is_none() {
            findings.push(Finding {
                subject: from.clone(),
                reason: format!("affects missing node '{to}'"),
            });
        }
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(id: &str, type_name: &str, payload: &str) -> NodeDraft {
        NodeDraft {
            id: id.to_string(),
            type_name: type_name.to_string(),
            namespace: "proj".into(),
            region: "eu".into(),
            payload: payload.to_string(),
        }
    }

    #[test]
    fn versions_link_to_their_parent_and_writes_are_type_gated() {
        let mut model = WorldModel::new();
        assert!(model.put("cell-a", draft("n", "config", "v1")).is_err());
        model.grant("cell-a", "config", TypePerm::Write);
        assert_eq!(model.put("cell-a", draft("n", "config", "v1")).unwrap(), 1);
        assert_eq!(model.put("cell-a", draft("n", "config", "v2")).unwrap(), 2);
        let latest = model.read_as("cell-a", "n").unwrap();
        assert_eq!(latest.parent_version, Some(1));
        assert_eq!(latest.payload, "v2");
    }

    #[test]
    fn evidence_compaction_and_affected_services() {
        let mut model = WorldModel::new();
        model.grant("cell-a", "config", TypePerm::Write);
        model.grant("cell-a", "service", TypePerm::Write);
        model.put("cell-a", draft("cfg", "config", "a")).unwrap();
        model.put("cell-a", draft("cfg", "config", "b")).unwrap();
        assert!(model.attach_evidence("cfg", 2, "").is_err());
        model.attach_evidence("cfg", 2, "receipt-9").unwrap();
        model.put("cell-a", draft("api", "service", "up")).unwrap();
        model.link_affects("cfg", "api");
        assert_eq!(model.affected_by("cfg"), vec!["api".to_string()]);
        assert_eq!(model.compact("cfg").unwrap(), 1);
        assert_eq!(model.latest("cfg").unwrap().version, 2);
        let summary = model.latest("cfg#summary").unwrap();
        assert!(summary.payload.contains("folded:1"));
    }
}
