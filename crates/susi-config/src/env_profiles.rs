//! Environment-scoped configuration profiles (VC-201-067).
//!
//! Profiles (`development`, `staging`, `production`, or shared) inherit
//! through an inspectable `extends` chain. Resolution records the source
//! profile of every key, and a lower environment can never inherit from a
//! higher one — a dev mission cannot accidentally resolve production
//! endpoints or credentials from ambient configuration.

use crate::susi_error::{eai_bail as bail, EaiError, EaiResult};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EnvKind {
    /// Profile values shareable by any environment.
    #[default]
    Shared,
    Development,
    Staging,
    Production,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    /// Environment this profile belongs to; defaults to shared.
    #[serde(default)]
    pub env: EnvKind,
    /// Parent profile names, applied in order (earlier wins ties among
    /// parents; the child always wins over every parent).
    #[serde(default)]
    pub extends: Vec<String>,
    #[serde(default)]
    pub values: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ProfileSet {
    #[serde(default)]
    pub profiles: BTreeMap<String, Profile>,
}

/// Which profile supplied a resolved key, and that profile's environment —
/// the inspectable half of the contract.
#[derive(Debug, Clone, PartialEq)]
pub struct Source {
    pub profile: String,
    pub env: EnvKind,
}

/// Accumulator for `apply` — keeps resolution state in one place.
#[derive(Default)]
struct ResolveAcc<'a> {
    values: BTreeMap<String, Value>,
    provenance: BTreeMap<String, Source>,
    chain: Vec<String>,
    seen: BTreeSet<&'a str>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Resolved {
    pub env: EnvKind,
    pub values: BTreeMap<String, Value>,
    /// Final supplier of each resolved key (last writer wins).
    pub provenance: BTreeMap<String, Source>,
    /// Ordered inheritance chain actually used, root parent first.
    pub chain: Vec<String>,
}

impl ProfileSet {
    /// Load profile definitions from a JSON file and validate structure.
    pub fn load(path: &Path) -> EaiResult<Self> {
        let content = std::fs::read_to_string(path).map_err(|e| {
            EaiError::io(format!(
                "failed to read profile set from {}: {e}",
                path.display()
            ))
        })?;
        let set: ProfileSet = serde_json::from_str(&content).map_err(|e| {
            EaiError::config(format!(
                "failed to parse profile set {}: {e}",
                path.display()
            ))
        })?;
        set.validate()?;
        Ok(set)
    }

    /// Load or discover profiles from `{workspace}/.susi/profiles.json` if present.
    pub fn load_workspace_or_default(workspace_dir: &Path) -> EaiResult<Option<Self>> {
        let path = workspace_dir.join(".susi").join("profiles.json");
        match path.try_exists() {
            Ok(false) => Ok(None),
            Ok(true) => Self::load(&path).map(Some),
            Err(e) => Err(EaiError::io(format!(
                "failed to access profile path {}: {e}",
                path.display()
            ))),
        }
    }

    /// Structural validation: every `extends` names a real profile, the
    /// graph is acyclic, and no lower environment inherits a higher one.
    pub fn validate(&self) -> EaiResult<()> {
        for (name, p) in &self.profiles {
            for parent in &p.extends {
                match self.profiles.get(parent) {
                    None => bail!("profile {name} extends unknown profile {parent}"),
                    Some(pp) if pp.env > p.env && pp.env != EnvKind::Shared => {
                        bail!(
                            "profile {name} ({:?}) extends higher-environment profile {parent} ({:?}); \
                             lower environments must not resolve higher-environment state",
                            p.env,
                            pp.env
                        );
                    }
                    Some(_) => {}
                }
            }
        }
        // Cycle check via DFS from every node.
        let mut visited = BTreeSet::new();
        let mut stack = BTreeSet::new();
        for name in self.profiles.keys() {
            self.visit(name, &mut visited, &mut stack)?;
        }
        Ok(())
    }

    fn visit<'a>(
        &'a self,
        name: &'a str,
        visited: &mut BTreeSet<&'a str>,
        stack: &mut BTreeSet<&'a str>,
    ) -> EaiResult<()> {
        if visited.contains(name) {
            return Ok(());
        }
        if !stack.insert(name) {
            bail!("profile inheritance cycle at {name}");
        }
        if let Some(p) = self.profiles.get(name) {
            for parent in &p.extends {
                self.visit(parent, visited, stack)?;
            }
        }
        stack.remove(name);
        visited.insert(name);
        Ok(())
    }

    /// Resolve a profile into a flat value map plus per-key provenance.
    /// Validates first — an illegal graph resolves nothing.
    pub fn resolve(&self, name: &str) -> EaiResult<Resolved> {
        self.validate()?;
        let Some(root) = self.profiles.get(name) else {
            bail!("unknown profile {name}");
        };
        let mut acc = ResolveAcc::default();
        self.apply(name, &mut acc);
        Ok(Resolved {
            env: root.env,
            values: acc.values,
            provenance: acc.provenance,
            chain: acc.chain,
        })
    }

    /// Depth-first application: parents first (in `extends` order), then
    /// the child's own values overwrite them.
    fn apply<'a>(&'a self, name: &'a str, acc: &mut ResolveAcc<'a>) {
        if !acc.seen.insert(name) {
            return; // shared parent reached via two edges — first wins
        }
        let Some(p) = self.profiles.get(name) else {
            return;
        };
        // Reverse order: a later `apply` call overwrites an earlier one on
        // key collision, so applying the LAST-listed parent first and the
        // FIRST-listed parent last is what makes the earlier parent win
        // ties, matching the documented contract.
        for parent in p.extends.iter().rev() {
            self.apply(parent, acc);
        }
        acc.chain.push(name.to_string());
        for (k, v) in &p.values {
            acc.values.insert(k.clone(), v.clone());
            acc.provenance.insert(
                k.clone(),
                Source {
                    profile: name.to_string(),
                    env: p.env,
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn validate_enforces_env_hierarchy_known_parents_and_acyclic_graph() {
        // Valid: Production -> Staging -> Shared, Development -> Shared.
        let mut valid = ProfileSet::default();
        valid.profiles.insert(
            "base".into(),
            Profile {
                env: EnvKind::Shared,
                extends: vec![],
                values: BTreeMap::new(),
            },
        );
        valid.profiles.insert(
            "stage".into(),
            Profile {
                env: EnvKind::Staging,
                extends: vec!["base".into()],
                values: BTreeMap::new(),
            },
        );
        valid.profiles.insert(
            "prod".into(),
            Profile {
                env: EnvKind::Production,
                extends: vec!["stage".into(), "base".into()],
                values: BTreeMap::new(),
            },
        );
        valid.profiles.insert(
            "dev".into(),
            Profile {
                env: EnvKind::Development,
                extends: vec!["base".into()],
                values: BTreeMap::new(),
            },
        );
        assert!(valid.validate().is_ok());

        // Lower environment (Development) extending higher environment (Production) fails.
        let mut bad_env = valid.clone();
        bad_env.profiles.get_mut("dev").unwrap().extends = vec!["prod".into()];
        let err = bad_env.validate().unwrap_err().to_string();
        assert!(err.contains("extends higher-environment profile"));

        // Unknown parent fails.
        let mut unknown_parent = ProfileSet::default();
        unknown_parent.profiles.insert(
            "dev".into(),
            Profile {
                env: EnvKind::Development,
                extends: vec!["missing".into()],
                values: BTreeMap::new(),
            },
        );
        assert!(unknown_parent
            .validate()
            .unwrap_err()
            .to_string()
            .contains("extends unknown profile missing"));

        // Cycle a -> b -> a fails.
        let mut cycle = ProfileSet::default();
        cycle.profiles.insert(
            "a".into(),
            Profile {
                env: EnvKind::Shared,
                extends: vec!["b".into()],
                values: BTreeMap::new(),
            },
        );
        cycle.profiles.insert(
            "b".into(),
            Profile {
                env: EnvKind::Shared,
                extends: vec!["a".into()],
                values: BTreeMap::new(),
            },
        );
        assert!(cycle
            .validate()
            .unwrap_err()
            .to_string()
            .contains("profile inheritance cycle"));
    }

    #[test]
    fn resolve_applies_earlier_parent_tiebreak_child_override_provenance_and_workspace_discovery() {
        let mut set = ProfileSet::default();
        set.profiles.insert(
            "first_parent".into(),
            Profile {
                env: EnvKind::Shared,
                extends: vec![],
                values: [
                    ("tie_key".into(), json!("from_first")),
                    ("first_only".into(), json!(1)),
                    ("child_key".into(), json!("overridden_by_child")),
                ]
                .into_iter()
                .collect(),
            },
        );
        set.profiles.insert(
            "second_parent".into(),
            Profile {
                env: EnvKind::Staging,
                extends: vec![],
                values: [
                    ("tie_key".into(), json!("from_second")),
                    ("second_only".into(), json!(2)),
                ]
                .into_iter()
                .collect(),
            },
        );
        set.profiles.insert(
            "prod".into(),
            Profile {
                env: EnvKind::Production,
                extends: vec!["first_parent".into(), "second_parent".into()],
                values: [("child_key".into(), json!("from_prod"))]
                    .into_iter()
                    .collect(),
            },
        );

        let resolved = set.resolve("prod").expect("resolve prod succeeds");
        assert_eq!(resolved.env, EnvKind::Production);
        assert_eq!(resolved.values.get("tie_key"), Some(&json!("from_first")));
        assert_eq!(resolved.values.get("first_only"), Some(&json!(1)));
        assert_eq!(resolved.values.get("second_only"), Some(&json!(2)));
        assert_eq!(resolved.values.get("child_key"), Some(&json!("from_prod")));
        assert_eq!(
            resolved.provenance.get("tie_key"),
            Some(&Source {
                profile: "first_parent".into(),
                env: EnvKind::Shared
            })
        );
        assert_eq!(
            resolved.provenance.get("second_only"),
            Some(&Source {
                profile: "second_parent".into(),
                env: EnvKind::Staging
            })
        );
        assert_eq!(
            resolved.provenance.get("child_key"),
            Some(&Source {
                profile: "prod".into(),
                env: EnvKind::Production
            })
        );
        assert_eq!(
            resolved.chain,
            vec!["second_parent", "first_parent", "prod"]
        );

        // Workspace discovery: absent returns Ok(None), present loads & validates.
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let ws = std::env::temp_dir().join(format!(
            "susi_env_prof_{}_{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&ws).unwrap();
        assert_eq!(ProfileSet::load_workspace_or_default(&ws).unwrap(), None);

        let dot_susi = ws.join(".susi");
        std::fs::create_dir_all(&dot_susi).unwrap();
        std::fs::write(
            dot_susi.join("profiles.json"),
            serde_json::to_string_pretty(&set).unwrap(),
        )
        .unwrap();
        let loaded = ProfileSet::load_workspace_or_default(&ws)
            .unwrap()
            .expect("profiles.json discovered");
        assert_eq!(loaded, set);
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn resolve_unknown_profile_self_inheritance_cycle_and_invalid_workspace_profiles_fail() {
        let mut set = ProfileSet::default();
        set.profiles.insert(
            "dev".into(),
            Profile {
                env: EnvKind::Development,
                extends: vec![],
                values: BTreeMap::new(),
            },
        );

        // 1. Resolving an unknown profile name returns Err.
        let unknown_err = set
            .resolve("nonexistent")
            .expect_err("unknown profile must fail")
            .to_string();
        assert!(unknown_err.contains("unknown profile nonexistent"));

        // 2. Self-inheritance (a extends a) is rejected as a profile inheritance cycle.
        let mut self_cycle = ProfileSet::default();
        self_cycle.profiles.insert(
            "a".into(),
            Profile {
                env: EnvKind::Shared,
                extends: vec!["a".into()],
                values: BTreeMap::new(),
            },
        );
        let cycle_err = self_cycle
            .validate()
            .expect_err("self-inheritance must fail")
            .to_string();
        assert!(cycle_err.contains("profile inheritance cycle"));

        // 3. load_workspace_or_default returns Err when .susi/profiles.json has an invalid graph.
        let ws = std::env::temp_dir().join(format!("susi_env_prof_invalid_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&ws);
        let dot_susi = ws.join(".susi");
        std::fs::create_dir_all(&dot_susi).unwrap();

        let mut bad_set = ProfileSet::default();
        bad_set.profiles.insert(
            "prod".into(),
            Profile {
                env: EnvKind::Production,
                extends: vec![],
                values: BTreeMap::new(),
            },
        );
        bad_set.profiles.insert(
            "dev".into(),
            Profile {
                env: EnvKind::Development,
                extends: vec!["prod".into()],
                values: BTreeMap::new(),
            },
        );
        std::fs::write(
            dot_susi.join("profiles.json"),
            serde_json::to_string_pretty(&bad_set).unwrap(),
        )
        .unwrap();

        let load_err = ProfileSet::load_workspace_or_default(&ws)
            .expect_err("invalid workspace profiles.json must fail")
            .to_string();
        assert!(load_err.contains("extends higher-environment profile"));

        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn diamond_inheritance_deduplicates_shared_ancestor_and_load_rejects_malformed_or_missing_files(
    ) {
        let mut set = ProfileSet::default();
        set.profiles.insert(
            "shared_root".into(),
            Profile {
                env: EnvKind::Shared,
                extends: vec![],
                values: [
                    ("root_only".into(), json!("from_root")),
                    ("diamond_key".into(), json!("from_root")),
                ]
                .into_iter()
                .collect(),
            },
        );
        set.profiles.insert(
            "parent_a".into(),
            Profile {
                env: EnvKind::Staging,
                extends: vec!["shared_root".into()],
                values: [("a_only".into(), json!("from_a"))].into_iter().collect(),
            },
        );
        set.profiles.insert(
            "parent_b".into(),
            Profile {
                env: EnvKind::Staging,
                extends: vec!["shared_root".into()],
                values: [("diamond_key".into(), json!("from_b"))]
                    .into_iter()
                    .collect(),
            },
        );
        set.profiles.insert(
            "prod".into(),
            Profile {
                env: EnvKind::Production,
                extends: vec!["parent_a".into(), "parent_b".into()],
                values: BTreeMap::new(),
            },
        );

        let resolved = set.resolve("prod").expect("resolve diamond prod");
        assert_eq!(
            resolved
                .chain
                .iter()
                .filter(|n| *n == "shared_root")
                .count(),
            1
        );
        assert_eq!(
            resolved.chain,
            vec!["shared_root", "parent_b", "parent_a", "prod"]
        );
        assert_eq!(resolved.values.get("root_only"), Some(&json!("from_root")));
        assert_eq!(resolved.values.get("a_only"), Some(&json!("from_a")));
        assert_eq!(resolved.values.get("diamond_key"), Some(&json!("from_b")));
        assert_eq!(
            resolved.provenance.get("diamond_key"),
            Some(&Source {
                profile: "parent_b".into(),
                env: EnvKind::Staging,
            })
        );

        let ws =
            std::env::temp_dir().join(format!("susi_env_prof_malformed_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&ws);
        std::fs::create_dir_all(&ws).unwrap();

        let missing_err = ProfileSet::load(&ws.join("missing.json"))
            .expect_err("missing profile file must fail")
            .to_string();
        assert!(missing_err.contains("failed to read profile set"));

        let bad_json_path = ws.join("malformed.json");
        std::fs::write(&bad_json_path, "{ not valid json").unwrap();
        let parse_err = ProfileSet::load(&bad_json_path)
            .expect_err("malformed profile JSON must fail")
            .to_string();
        assert!(parse_err.contains("failed to parse profile set"));

        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn env_kind_serde_roundtrips_all_variants_and_empty_profile_set_rejects_unknown_profile() {
        for (kind, tag) in [
            (EnvKind::Development, "\"development\""),
            (EnvKind::Staging, "\"staging\""),
            (EnvKind::Production, "\"production\""),
        ] {
            let s = serde_json::to_string(&kind).unwrap();
            assert_eq!(s, tag);
            assert_eq!(serde_json::from_str::<EnvKind>(&s).unwrap(), kind);
        }
        let set = ProfileSet::default();
        assert!(set.resolve("missing_profile").is_err());
    }
}

// ---- Wave 14 (T-CLAUDE-415) structural invariant verifiers ----

impl Profile {
    /// A profile lists no blank or duplicate parent in `extends` and no blank value key.
    pub fn verify_profile_invariants(&self) -> Result<(), String> {
        let mut seen = std::collections::BTreeSet::new();
        for parent in &self.extends {
            if parent.trim().is_empty() {
                return Err("Profile extends must not contain a blank parent".to_string());
            }
            if !seen.insert(parent.as_str()) {
                return Err(format!("Profile extends lists `{parent}` more than once"));
            }
        }
        if self.values.keys().any(|k| k.trim().is_empty()) {
            return Err("Profile values must not contain a blank key".to_string());
        }
        Ok(())
    }
}

impl ProfileSet {
    /// Every profile verifies, no profile extends itself, and every `extends` target
    /// names a profile present in the set (so resolution never dangles).
    pub fn verify_set_invariants(&self) -> Result<(), String> {
        for (name, profile) in &self.profiles {
            profile.verify_profile_invariants()?;
            for parent in &profile.extends {
                if parent == name {
                    return Err(format!("profile `{name}` extends itself"));
                }
                if !self.profiles.contains_key(parent) {
                    return Err(format!(
                        "profile `{name}` extends `{parent}`, which is not in the set"
                    ));
                }
            }
        }
        Ok(())
    }
}

impl Resolved {
    /// Every resolved value has exactly one provenance entry and vice versa, every
    /// provenance supplier is named, and the inheritance chain has no blank or
    /// repeated entry.
    pub fn verify_resolved_invariants(&self) -> Result<(), String> {
        for key in self.values.keys() {
            if !self.provenance.contains_key(key) {
                return Err(format!("resolved value `{key}` has no provenance"));
            }
        }
        for (key, source) in &self.provenance {
            if !self.values.contains_key(key) {
                return Err(format!("provenance key `{key}` has no resolved value"));
            }
            if source.profile.trim().is_empty() {
                return Err(format!("provenance for `{key}` names no profile"));
            }
        }
        let mut seen = std::collections::BTreeSet::new();
        for link in &self.chain {
            if link.trim().is_empty() {
                return Err("resolution chain must not contain a blank entry".to_string());
            }
            if !seen.insert(link.as_str()) {
                return Err(format!("resolution chain repeats `{link}`"));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod wave14_verifier_tests {
    use super::*;

    fn profile(extends: &[&str]) -> Profile {
        Profile {
            env: EnvKind::Development,
            extends: extends.iter().map(|s| (*s).to_string()).collect(),
            values: BTreeMap::new(),
        }
    }

    #[test]
    fn profile_set_and_resolved_verifiers_reject_corrupt_state() {
        assert_eq!(profile(&["base"]).verify_profile_invariants(), Ok(()));
        assert!(profile(&["base", "base"])
            .verify_profile_invariants()
            .is_err());

        let mut profiles = BTreeMap::new();
        profiles.insert("base".to_string(), profile(&[]));
        profiles.insert("dev".to_string(), profile(&["base"]));
        let set = ProfileSet { profiles };
        assert_eq!(set.verify_set_invariants(), Ok(()));

        let mut dangling = set.profiles.clone();
        dangling.insert("x".to_string(), profile(&["missing"]));
        assert!(ProfileSet { profiles: dangling }
            .verify_set_invariants()
            .is_err());

        let mut selfref = set.profiles.clone();
        selfref.insert("loop".to_string(), profile(&["loop"]));
        assert!(ProfileSet { profiles: selfref }
            .verify_set_invariants()
            .is_err());

        let mut values = BTreeMap::new();
        values.insert("A".to_string(), Value::from("1"));
        let mut provenance = BTreeMap::new();
        provenance.insert(
            "A".to_string(),
            Source {
                profile: "base".to_string(),
                env: EnvKind::Shared,
            },
        );
        let resolved = Resolved {
            env: EnvKind::Development,
            values: values.clone(),
            provenance,
            chain: vec!["base".to_string(), "dev".to_string()],
        };
        assert_eq!(resolved.verify_resolved_invariants(), Ok(()));

        let orphan = Resolved {
            env: EnvKind::Development,
            values,
            provenance: BTreeMap::new(),
            chain: vec!["base".to_string()],
        };
        assert!(orphan.verify_resolved_invariants().is_err());

        let repeated_chain = Resolved {
            env: EnvKind::Development,
            values: BTreeMap::new(),
            provenance: BTreeMap::new(),
            chain: vec!["base".to_string(), "base".to_string()],
        };
        assert!(repeated_chain.verify_resolved_invariants().is_err());
    }
}
