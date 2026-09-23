// Agent metadata persistence (list/register/re-rank). Deliberately does NOT
// include instantiate_native_agent/instantiate_agent - those construct
// concrete agent structs (HardwareAgent, LibraryScoutAgent, ...) that live
// in gawd and call into gemi/gmcp/daemon, so they stay in gawd as free
// functions there instead of methods here; nothing outside gawd ever called
// them (only .list_agents()/.register_agent(), verified before moving).

use crate::types::AgentProfile;
use std::sync::OnceLock;

pub struct AgentMetaRegistry {
    store: susi_sandbox::VersionedJsonStore<Vec<AgentProfile>>,
}

impl Default for AgentMetaRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentMetaRegistry {
    pub fn new() -> Self {
        AgentMetaRegistry {
            store: susi_sandbox::VersionedJsonStore::new(),
        }
    }

    pub fn global() -> &'static Self {
        static REGISTRY: OnceLock<AgentMetaRegistry> = OnceLock::new();
        REGISTRY.get_or_init(AgentMetaRegistry::new)
    }

    fn registry_path() -> std::path::PathBuf {
        susi_paths::SusiDirs::data_dir().join("agent_registry.json")
    }

    #[allow(clippy::expect_used)]
    fn bootstrap_data(&self) -> Vec<AgentProfile> {
        serde_json::from_str(include_str!("../../../config/agents.default.json"))
            .expect("Fatal: agents.default.json must be valid JSON.")
    }

    const MAX_NON_CORE_AGENTS: usize = 300;

    pub fn register_agent(&self, profile: AgentProfile) {
        susi_core::registry::CapabilityRegistry::global().register_agent_capability(
            susi_core::registry::AgentCapability {
                name: profile.name.clone(),
                description: profile.description.clone(),
                is_core: profile.is_core,
            },
        );
        let _ = self.store.modify(
            &Self::registry_path(),
            || Ok(self.bootstrap_data()),
            |_| false,
            false,
            |agents| {
                if !agents.iter().any(|a| a.name == profile.name) {
                    let non_core_count = agents.iter().filter(|a| !a.is_core).count();
                    if non_core_count >= Self::MAX_NON_CORE_AGENTS {
                        if let Some(idx) = agents
                            .iter()
                            .enumerate()
                            .filter(|(_, a)| !a.is_core)
                            .min_by(|(_, a), (_, b)| {
                                a.base_rank
                                    .partial_cmp(&b.base_rank)
                                    .unwrap_or(std::cmp::Ordering::Equal)
                            })
                            .map(|(i, _)| i)
                        {
                            agents.remove(idx);
                        }
                    }
                    agents.push(profile);
                }
            },
        );
    }

    pub fn update_rank(&self, name: &str, delta: f32, source: &str) {
        let name_owned = name.to_string();
        let source_owned = source.to_string();
        let _ = self.store.modify(
            &Self::registry_path(),
            || Ok(self.bootstrap_data()),
            |_| false,
            false,
            move |agents| {
                if let Some(agent) = agents.iter_mut().find(|a| a.name == name_owned) {
                    let old_rank = agent.base_rank;
                    agent.base_rank = (agent.base_rank + delta).clamp(0.1, 1.0);

                    let log_msg = format!(
                        "Agent '{}' rank mutation: {:.2} -> {:.2} (Source: {})",
                        name_owned, old_rank, agent.base_rank, source_owned
                    );
                    susi_sandbox::manager::SusiAuditLogger::log(
                        &susi_paths::SusiDirs::config_dir(),
                        susi_sandbox::manager::LogLevel::Info,
                        "AGENT_MUTATION",
                        &log_msg,
                    );
                }
            },
        );
    }

    pub fn list_agents(&self) -> Vec<AgentProfile> {
        let agents = self
            .store
            .load_with_healing(
                &Self::registry_path(),
                || Ok(self.bootstrap_data()),
                |unique_agents| {
                    let mut deduplicated = Vec::new();
                    for a in unique_agents.drain(..) {
                        if !deduplicated.iter().any(|x: &AgentProfile| x.name == a.name) {
                            deduplicated.push(a);
                        }
                    }
                    *unique_agents = deduplicated;

                    let mut changed = false;
                    for default_agent in self.bootstrap_data() {
                        if !unique_agents
                            .iter()
                            .any(|x: &AgentProfile| x.name == default_agent.name)
                        {
                            unique_agents.push(default_agent);
                            changed = true;
                        }
                    }
                    changed
                },
                false,
            )
            .unwrap_or_else(|_| self.bootstrap_data());
        let caps = susi_core::registry::CapabilityRegistry::global();
        for profile in &agents {
            caps.register_agent_capability(susi_core::registry::AgentCapability {
                name: profile.name.clone(),
                description: profile.description.clone(),
                is_core: profile.is_core,
            });
        }
        agents
    }

    pub fn get_checksum(&self) -> u64 {
        let agents = self.list_agents();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        use std::hash::{Hash, Hasher};
        for agent in agents.iter() {
            agent.name.hash(&mut hasher);
            agent.description.hash(&mut hasher);
        }
        hasher.finish()
    }
}
