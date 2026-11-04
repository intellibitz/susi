//! Replicate only policy-eligible swarm memory (VC-201-084).

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

fn parse_provenance_tag(s: &str) -> (&str, Option<u64>) {
    if let Some(pos) = s.find(|c: char| c.is_ascii_digit()) {
        let (prefix, num_str) = s.split_at(pos);
        if let Ok(num) = num_str.parse::<u64>() {
            return (prefix, Some(num));
        }
    }
    (s, None)
}

/// Compare provenance identifiers using natural/numeric ordering so newer writes
/// (e.g. "p10" vs "p9") correctly outrank older writes instead of being compared lexically.
pub fn compare_provenance(a: &str, b: &str) -> Ordering {
    let (prefix_a, num_a) = parse_provenance_tag(a);
    let (prefix_b, num_b) = parse_provenance_tag(b);

    match (num_a, num_b) {
        (Some(na), Some(nb)) if prefix_a == prefix_b => na.cmp(&nb),
        _ => a.cmp(b),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopedRecord {
    pub id: String,
    pub body: String,
    pub provenance: String,
    pub local_only: bool,
    pub deleted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerAuth {
    pub peer_id: String,
    pub token: String,
}

impl PeerAuth {
    pub fn new(peer_id: impl Into<String>, token: impl Into<String>) -> Self {
        Self {
            peer_id: peer_id.into(),
            token: token.into(),
        }
    }

    #[must_use]
    pub fn is_valid(&self, expected_token: &str) -> bool {
        !self.peer_id.is_empty() && self.token == expected_token
    }
}

#[derive(Debug, Default)]
pub struct MemoryReplica {
    pub records: BTreeMap<String, ScopedRecord>,
    pub tombstones: BTreeSet<String>,
    pub peer_token: Option<String>,
}

impl MemoryReplica {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_peer_auth(mut self, token: impl Into<String>) -> Self {
        self.peer_token = Some(token.into());
        self
    }

    pub fn upsert(&mut self, rec: ScopedRecord) {
        if rec.deleted || self.tombstones.contains(&rec.id) {
            self.tombstones.insert(rec.id.clone());
            self.records.remove(&rec.id);
            return;
        }
        self.records.insert(rec.id.clone(), rec);
    }

    pub fn delete(&mut self, id: &str) {
        self.tombstones.insert(id.to_string());
        self.records.remove(id);
    }

    /// Export policy-eligible records (non-local-only) along with tombstones marked as deleted.
    #[must_use]
    pub fn export_for_peers(&self) -> Vec<ScopedRecord> {
        let mut out: Vec<ScopedRecord> = self
            .records
            .values()
            .filter(|r| !r.local_only && !r.deleted && !self.tombstones.contains(&r.id))
            .cloned()
            .collect();

        // Propagate deletions/tombstones so peers learn about deleted content
        for tomb in &self.tombstones {
            out.push(ScopedRecord {
                id: tomb.clone(),
                body: String::new(),
                provenance: String::new(),
                local_only: false,
                deleted: true,
            });
        }

        out
    }

    /// Authenticated peer export: verifies peer credentials before returning records.
    pub fn export_for_peer_authenticated(
        &self,
        auth: &PeerAuth,
    ) -> Result<Vec<ScopedRecord>, &'static str> {
        if let Some(expected) = &self.peer_token {
            if !auth.is_valid(expected) {
                return Err("peer authentication failed");
            }
        }
        Ok(self.export_for_peers())
    }

    /// Authenticated peer merge: verifies peer credentials before merging incoming records.
    pub fn merge_from_peer_authenticated(
        &mut self,
        incoming: &[ScopedRecord],
        auth: &PeerAuth,
    ) -> Result<(), &'static str> {
        if let Some(expected) = &self.peer_token {
            if !auth.is_valid(expected) {
                return Err("peer authentication failed");
            }
        }
        self.merge_from_peer(incoming);
        Ok(())
    }

    /// Merge peer export: never revive tombstones; skip local-only from peer;
    /// use natural provenance ordering so newer writes always outrank older ones.
    pub fn merge_from_peer(&mut self, incoming: &[ScopedRecord]) {
        for rec in incoming {
            if rec.local_only {
                continue;
            }
            if self.tombstones.contains(&rec.id) || rec.deleted {
                self.tombstones.insert(rec.id.clone());
                self.records.remove(&rec.id);
                continue;
            }
            match self.records.get(&rec.id) {
                Some(existing)
                    if compare_provenance(&existing.provenance, &rec.provenance)
                        == Ordering::Greater => {}
                _ => {
                    self.records.insert(rec.id.clone(), rec.clone());
                }
            }
        }
    }
}

/// Disconnected replicas that exchange exports converge on shared eligible state.
#[must_use]
pub fn converge(a: &MemoryReplica, b: &MemoryReplica) -> (MemoryReplica, MemoryReplica) {
    let mut a2 = MemoryReplica {
        records: a.records.clone(),
        tombstones: a.tombstones.clone(),
        peer_token: a.peer_token.clone(),
    };
    let mut b2 = MemoryReplica {
        records: b.records.clone(),
        tombstones: b.tombstones.clone(),
        peer_token: b.peer_token.clone(),
    };
    a2.merge_from_peer(&b.export_for_peers());
    b2.merge_from_peer(&a.export_for_peers());
    // Also sync tombstones both ways via deleted markers.
    for id in &a.tombstones {
        b2.delete(id);
    }
    for id in &b.tombstones {
        a2.delete(id);
    }
    a2.merge_from_peer(&b2.export_for_peers());
    b2.merge_from_peer(&a2.export_for_peers());
    (a2, b2)
}

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod inline_tests {
    use super::*;

    fn rec(id: &str, body: &str, prov: &str, local_only: bool) -> ScopedRecord {
        ScopedRecord {
            id: id.into(),
            body: body.into(),
            provenance: prov.into(),
            local_only,
            deleted: false,
        }
    }

    #[test]
    fn compare_provenance_uses_natural_numeric_ordering_for_shared_prefix() {
        assert_eq!(compare_provenance("p10", "p9"), Ordering::Greater);
        assert_eq!(compare_provenance("p2", "p10"), Ordering::Less);
        assert_eq!(compare_provenance("rev5", "rev5"), Ordering::Equal);
    }

    #[test]
    fn memory_replica_export_excludes_local_only_and_propagates_tombstones() {
        let mut r = MemoryReplica::new();
        r.upsert(rec("shared", "v1", "p1", false));
        r.upsert(rec("private", "secret", "p1", true));
        r.upsert(rec("gone", "old", "p1", false));
        r.delete("gone");

        let exported = r.export_for_peers();
        assert!(exported.iter().any(|x| x.id == "shared" && !x.deleted));
        assert!(!exported.iter().any(|x| x.id == "private"));
        assert!(exported.iter().any(|x| x.id == "gone" && x.deleted));
    }

    #[test]
    fn authenticated_export_and_merge_validate_peer_credentials() {
        let mut r = MemoryReplica::new().with_peer_auth("tok-123");
        r.upsert(rec("k1", "v1", "p1", false));

        let bad = PeerAuth::new("peer-1", "wrong");
        assert!(r.export_for_peer_authenticated(&bad).is_err());
        assert!(r.merge_from_peer_authenticated(&[], &bad).is_err());

        let good = PeerAuth::new("peer-1", "tok-123");
        let out = r.export_for_peer_authenticated(&good).unwrap();
        assert_eq!(out.len(), 1);
        r.merge_from_peer_authenticated(&[rec("k1", "v2", "p2", false)], &good)
            .unwrap();
        assert_eq!(r.records["k1"].body, "v2");
    }

    #[test]
    fn converge_synchronizes_eligible_records_and_tombstones_across_replicas() {
        let mut a = MemoryReplica::new();
        let mut b = MemoryReplica::new();
        a.upsert(rec("doc", "old", "p2", false));
        a.upsert(rec("del-me", "bye", "p1", false));
        b.upsert(rec("doc", "newer", "p10", false));
        b.delete("del-me");

        let (a_conv, b_conv) = converge(&a, &b);
        assert_eq!(a_conv.records["doc"].body, "newer");
        assert_eq!(b_conv.records["doc"].body, "newer");
        assert!(!a_conv.records.contains_key("del-me"));
        assert!(a_conv.tombstones.contains("del-me"));
    }
}
