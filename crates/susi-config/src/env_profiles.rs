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
}
