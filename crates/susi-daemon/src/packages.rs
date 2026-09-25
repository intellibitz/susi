//! Cell package resolution (Swarm OS Bullet 66)
//!
//! Exact-version dependencies. Two different versions of one name in the
//! same closure are a conflict. A cycle is an error.

use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    pub name: String,
    pub version: String,
    pub deps: Vec<(String, String)>,
}

pub fn resolve(
    available: &[Package],
    root_name: &str,
    root_version: &str,
) -> Result<Vec<(String, String)>, String> {
    let index: HashMap<(&str, &str), &Package> = available
        .iter()
        .map(|package| ((package.name.as_str(), package.version.as_str()), package))
        .collect();
    let mut selected: HashMap<String, String> = HashMap::new();
    let mut stack = HashSet::new();
    visit(&index, root_name, root_version, &mut selected, &mut stack)?;
    let mut resolved: Vec<(String, String)> = selected.into_iter().collect();
    resolved.sort();
    Ok(resolved)
}

fn visit(
    index: &HashMap<(&str, &str), &Package>,
    name: &str,
    version: &str,
    selected: &mut HashMap<String, String>,
    stack: &mut HashSet<String>,
) -> Result<(), String> {
    if stack.contains(name) {
        return Err(format!("dependency cycle at '{name}'"));
    }
    if let Some(existing) = selected.get(name) {
        if existing != version {
            return Err(format!(
                "version conflict for '{name}': {existing} vs {version}"
            ));
        }
        return Ok(());
    }
    stack.insert(name.to_string());
    let package = index
        .get(&(name, version))
        .copied()
        .ok_or_else(|| format!("package '{name}' version '{version}' is not in the catalog"))?;
    selected.insert(name.to_string(), version.to_string());
    for (dep_name, dep_version) in &package.deps {
        visit(index, dep_name, dep_version, selected, stack)?;
    }
    stack.remove(name);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_version_conflict_and_a_cycle_both_fail() {
        let conflict = vec![
            Package {
                name: "root".into(),
                version: "1".into(),
                deps: vec![("lib".into(), "1".into()), ("other".into(), "1".into())],
            },
            Package {
                name: "other".into(),
                version: "1".into(),
                deps: vec![("lib".into(), "2".into())],
            },
            Package {
                name: "lib".into(),
                version: "1".into(),
                deps: Vec::new(),
            },
            Package {
                name: "lib".into(),
                version: "2".into(),
                deps: Vec::new(),
            },
        ];
        assert!(
            resolve(&conflict, "root", "1")
                .unwrap_err()
                .contains("conflict")
        );

        let cycle = vec![
            Package {
                name: "a".into(),
                version: "1".into(),
                deps: vec![("b".into(), "1".into())],
            },
            Package {
                name: "b".into(),
                version: "1".into(),
                deps: vec![("a".into(), "1".into())],
            },
        ];
        assert!(resolve(&cycle, "a", "1").unwrap_err().contains("cycle"));
    }
}
