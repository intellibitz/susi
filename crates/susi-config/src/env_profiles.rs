//! Environment-scoped configuration profiles (VC-201-067).
//!
//! Profiles (`development`, `staging`, `production`, or shared) inherit
//! through an inspectable `extends` chain. Resolution records the source
//! profile of every key, and a lower environment can never inherit from a
//! higher one — a dev mission cannot accidentally resolve production
//! endpoints or credentials from ambient configuration.

use crate::susi_error::{eai_bail as bail, EaiResult};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

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
