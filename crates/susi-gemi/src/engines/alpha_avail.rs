//! Tool availability for Tier-0 cache checks.
//!
//! This does not load Candle. A cached `ACTION:` line is servable only while
//! the named tool or foundational intent can still run.

/// Base intents every checkpoint keeps; never reclaimed.
pub(crate) const FOUNDATIONAL_INTENTS: [&str; 9] = [
    "status",
    "version",
    "self_heal_build",
    "run_test_harness",
    "write_file",
    "read_file",
    "list_directory",
    "scout",
    "reason",
];

/// Every registered agent and installed tool name (sorted, deduplicated,
/// foundational intents excluded) — untruncated, unlike the vocabulary.
pub(crate) fn capability_names() -> Vec<String> {
    let mut names: Vec<String> = crate::susi_core::AgentMetaRegistry::global()
        .list_agents()
        .into_iter()
        .map(|agent| agent.name)
        .chain(crate::susi_core::registry::CapabilityRegistry::global().list_tools())
        .filter(|name| !FOUNDATIONAL_INTENTS.contains(&name.as_str()))
        .collect();
    names.sort();
    names.dedup();
    names
}

/// Whether a predicted `ACTION: <name>` names something that can run now:
/// a foundational intent, or a currently registered agent/tool. Vocabulary
/// slots outlive uninstalled tools (reclamation only reuses them later), so
/// a trained model could otherwise serve an action nothing can execute.
/// The capability set is snapshotted for `AVAILABILITY_TTL` so serving does
/// not rescan the registries per prompt.
pub(crate) fn action_available(action: &str) -> bool {
    const AVAILABILITY_TTL: std::time::Duration = std::time::Duration::from_secs(30);
    type Snapshot = Option<(
        std::time::Instant,
        std::sync::Arc<std::collections::HashSet<String>>,
    )>;
    static SNAPSHOT: std::sync::LazyLock<std::sync::RwLock<Snapshot>> =
        std::sync::LazyLock::new(|| std::sync::RwLock::new(None));
    let name = action
        .strip_prefix("ACTION: ")
        .unwrap_or(action)
        .to_lowercase();
    if FOUNDATIONAL_INTENTS.contains(&name.as_str()) {
        return true;
    }
    let fresh = SNAPSHOT
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .filter(|(at, _)| at.elapsed() < AVAILABILITY_TTL)
        .map(|(_, set)| set.clone());
    let set = fresh.unwrap_or_else(|| {
        let set: std::sync::Arc<std::collections::HashSet<String>> = std::sync::Arc::new(
            capability_names()
                .into_iter()
                .map(|n| n.to_lowercase())
                .collect(),
        );
        *SNAPSHOT.write().unwrap_or_else(|e| e.into_inner()) =
            Some((std::time::Instant::now(), set.clone()));
        set
    });
    set.contains(&name)
}
