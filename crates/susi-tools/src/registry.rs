use crate::susi_error::EaiResult;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, OnceLock, RwLock};
use susi_paths::sync::RwLock as ToolMap;

use crate::client::GmcpClient;
use crate::hooks::hooks;
use crate::susi_core::egress::{EgressGate, EgressPolicy, EgressRefusal};
use crate::susi_core::mac_policy::{actions, MacPolicy};
use crate::types::{McpTool, MetaCategory, MetaTool, SusiTool};

pub struct ToolRegistry {
    pub tools: ToolMap<HashMap<String, Arc<dyn SusiTool>>>,
}

impl ToolRegistry {
    pub fn global() -> &'static Self {
        static REGISTRY: OnceLock<ToolRegistry> = OnceLock::new();
        REGISTRY.get_or_init(|| {
            let registry = ToolRegistry {
                tools: ToolMap::default(),
            };
            crate::susi_core::registry::CapabilityRegistry::global()
                .with_batch(|| hooks().bootstrap_tools(&registry));
            registry
        })
    }

    /// Public so `EngineHooks::bootstrap_tools` implementations (in `gmcp`,
    /// where the concrete `CoreTools::*` handlers live) can register them.
    pub fn register_meta_tool<F>(
        registry: &ToolRegistry,
        name: &str,
        desc: &str,
        category: MetaCategory,
        handler: F,
    ) where
        F: Fn(&serde_json::Value, &Path) -> EaiResult<String> + Send + Sync + 'static,
    {
        let tool = MetaTool {
            tool_name: name.to_string(),
            tool_desc: desc.to_string(),
            category,
            handler: Arc::new(handler),
        };
        // Dual-mount: ToolRegistry dispatch + CapabilityRegistry catalog.
        struct CapTool {
            name: String,
            desc: String,
            handler: crate::types::MetaToolHandler,
        }
        impl crate::susi_core::registry::Tool for CapTool {
            fn name(&self) -> &str {
                &self.name
            }
            fn description(&self) -> &str {
                &self.desc
            }
            fn execute(
                &self,
                args: &serde_json::Value,
                workspace: &Path,
            ) -> crate::susi_core::susi_error::EaiResult<String> {
                (self.handler)(args, workspace)
            }
        }
        crate::susi_core::registry::CapabilityRegistry::global().register_tool(CapTool {
            name: name.to_string(),
            desc: desc.to_string(),
            handler: tool.handler.clone(),
        });
        registry
            .tools
            .write()
            .insert(name.to_string(), Arc::new(tool));
    }

    pub fn list_tools() -> Vec<McpTool> {
        let registry = Self::global();
        let mut tools: Vec<McpTool> = registry
            .tools
            .read()
            .iter()
            .map(|(name, tool)| McpTool {
                name: name.clone(),
                description: tool.description(),
                input_schema: hooks().tool_input_schema(name),
            })
            .collect();

        tools.extend(GmcpClient::list_managed_tools());

        // Pillar 8: surface live-discovered MCP tools from CapabilityRegistry
        let caps = crate::susi_core::registry::CapabilityRegistry::global();
        for name in caps.list_tools() {
            if let Some(tool) = caps.get_tool(&name) {
                tools.push(McpTool {
                    input_schema: hooks().tool_input_schema(&name),
                    name,
                    description: tool.description().to_string(),
                });
            }
        }

        let reflex_dir = susi_paths::SusiDirs::data_dir().join("reflexes");
        if let Ok(entries) = std::fs::read_dir(&reflex_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|ext| ext == "wasm") {
                    if let Ok(name) = entry.file_name().into_string() {
                        tools.push(McpTool {
                            name: format!("reflex_{}", name.replace(".wasm", "")),
                            description: "Dynamic Wasm neural reflex tool".to_string(),
                            input_schema: None,
                        });
                    }
                }
            }
        }

        tools.sort_by(|a, b| a.name.cmp(&b.name));
        tools.dedup_by(|a, b| a.name == b.name);
        tools
    }

    pub fn exists(name: &str) -> bool {
        let registry = Self::global();
        if registry.tools.read().contains_key(name) {
            return true;
        }
        if crate::susi_core::registry::CapabilityRegistry::global()
            .get_tool(name)
            .is_some()
        {
            return true;
        }
        let lower_name = name.to_lowercase();
        if lower_name.contains(':')
            || lower_name.starts_with("ext_")
            || lower_name.starts_with("reflex_")
        {
            let matches = |tools: &[McpTool]| {
                tools
                    .iter()
                    .any(|t| t.name == name || t.name.starts_with(name))
            };
            // The listing never waits on external servers; resolving a
            // concrete `server:tool` name may.
            return matches(&Self::list_tools())
                || matches(&GmcpClient::list_managed_tools_blocking());
        }
        false
    }

    pub fn execute_tool(name: &str, arg: &serde_json::Value, workspace: &Path) -> String {
        // The acting mission, when this dispatch runs inside one (evidence
        // sessions are bound per mission; worker threads resolve by
        // workspace). Its id is the dispatch subject: agent tokens are
        // checked per mission and refusals are recorded against it.
        let mission = Self::current_mission(workspace);

        // Process posture first — a mission's tokens can only ever narrow
        // what the host itself is allowed to do, never widen it, so a
        // mid-mission posture downgrade still binds.
        if let Err(e) = MacPolicy::global().authorize_tool(name, arg, workspace, None) {
            Self::record_authority_refusal(
                mission.as_deref().unwrap_or("susi"),
                name,
                &e.to_string(),
            );
            return format!("{e}");
        }

        if let Some(mission) = mission.as_deref() {
            if let Err(error) = Self::ensure_mission_authority(mission, workspace) {
                Self::record_authority_refusal(mission, name, &error.to_string());
                return error.to_string();
            }
            // The mission's own token set — minted at first dispatch as the
            // posture-bounded baseline for this subject.
            if let Err(e) = MacPolicy::global().authorize_tool(name, arg, workspace, Some(mission))
            {
                Self::record_authority_refusal(mission, name, &e.to_string());
                return format!("{e}");
            }
            // Per-mission egress allow-list — consulted on every
            // egress-class dispatch; refusals are recorded by the gate.
            if let Some(denied) = Self::check_mission_egress(mission, name, arg) {
                return denied;
            }
        }

        // Prefer CapabilityRegistry for discovered MCP tools so swarm dispatch
        // uses the same hot-plugged catalog as zero-config bootstrap.
        if let Some(tool) = crate::susi_core::registry::CapabilityRegistry::global().get_tool(name)
        {
            return match tool.execute(arg, workspace) {
                Ok(res) => res,
                Err(e) => format!("{}", e),
            };
        }

        if name.contains(':') && !name.starts_with("ext_") {
            let parts: Vec<&str> = name.splitn(2, ':').collect();
            let arg_str = if let Some(s) = arg.as_str() {
                s.to_string()
            } else {
                arg.to_string()
            };
            return crate::susi_core::capture::EvidenceSession::capture_call(
                name,
                arg,
                workspace,
                || GmcpClient::execute_external_tool_result(parts[0], parts[1], &arg_str),
            )
            .unwrap_or_else(|error| error.to_string());
        }

        if name.starts_with("reflex_") {
            let raw = name.trim_start_matches("reflex_");
            if raw.is_empty()
                || raw.contains("..")
                || raw.contains('/')
                || raw.contains('\\')
                || !raw
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            {
                return "Reflex Error: invalid reflex name".to_string();
            }
            let wasm_name = format!("{}.wasm", raw);
            let wasm_path = susi_paths::SusiDirs::data_dir()
                .join("reflexes")
                .join(wasm_name);
            if wasm_path.exists() {
                let arg_str = if let Some(s) = arg.as_str() {
                    s.to_string()
                } else {
                    arg.to_string()
                };
                match crate::susi_core::capture::EvidenceSession::capture_call(
                    name,
                    arg,
                    workspace,
                    || crate::susi_native::WasmHost::execute_untrusted_wasm(&wasm_path, &arg_str),
                ) {
                    Ok(res) => return res,
                    Err(e) => return format!("Reflex Error: {}", e),
                }
            }
        }

        let registry = Self::global();
        let tool = registry.tools.read().get(name).map(Arc::clone);
        if let Some(tool) = tool {
            match crate::susi_core::capture::EvidenceSession::capture_call(
                name,
                arg,
                workspace,
                || tool.execute(arg, workspace),
            ) {
                Ok(res) => res,
                Err(e) => format!("{}", e),
            }
        } else {
            // A typed capability-gap error (an MCP tool error). Core never
            // provisions a third-party package on its own (zero-UI plan A8);
            // the first-party reflex synthesis still runs as the fallback.
            let reflex = Self::resolve_capability_gap(name, workspace)
                .unwrap_or_else(|e| format!("reflex synthesis unavailable: {e}"));
            // One gap tag per message: the reflex report may carry its own.
            let reflex = reflex.strip_prefix("[CAPABILITY_GAP] ").unwrap_or(&reflex);
            format!(
                "[CAPABILITY_GAP] Tool '{name}' missing from Meta-Substrate. Reflex fallback: {reflex} \
                 Core does not provision third-party servers on its own; admit one explicitly \
                 (`susi mcp-add`, `susi mcp enable`) or load an extension pack."
            )
        }
    }

    /// Capability-gap resolution. Core never installs or launches a
    /// third-party MCP server to close a gap (zero-UI plan A8): the only
    /// autonomous fallback is first-party Wasm reflex synthesis, and the
    /// caller reports the gap as a typed `[CAPABILITY_GAP]` tool error.
    pub fn resolve_capability_gap(name: &str, workspace: &Path) -> EaiResult<String> {
        let server_name = name.split(':').next().unwrap_or(name);

        // VC-200-002 (roadmap.json): first-party autonomous hot-patch - delegated
        // to the engine hooks (was a direct call to
        // gawd::reflex_synth::ReflexSynthesizer::synthesize_wasm_reflex; see
        // hooks.rs for why this crate can't depend on gawd directly).
        hooks().resolve_capability_gap(server_name, workspace)
    }
}

// ── Per-mission authority (VC-202-014) ─────────────────────────────────────
//
// A mission is a dispatch subject of its own: when a tool call runs inside a
// mission (its `EvidenceSession`), dispatch checks the mission's own minted
// capability tokens — mirrored from the process posture at first dispatch,
// never wider — and consults the mission's `EgressGate` on every
// egress-class call. Both kinds of refusal are recorded against the mission.
//
// A mission declares its egress allow-list in `{workspace}/.susi/
// mission-egress.json`:
//
//     {"allow_hosts": ["api.openai.com", "*.example.com"]}
//
// An absent file is the documented permissive default (provider traffic
// keeps working without opt-in); a present but malformed file fails closed —
// an operator-declared restriction that cannot be parsed must not silently
// become open. A mission can only ever narrow its posture this way: even
// rewriting its own declaration cannot grant it anything the host denies.

/// A tool dispatch refusal recorded against the acting mission (or `"susi"`
/// when no mission is bound).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorityRefusal {
    /// The dispatch subject that was refused.
    pub mission: String,
    /// The tool whose dispatch was refused.
    pub tool: String,
    /// Why the dispatch was refused.
    pub reason: String,
}

/// Refusals recorded per mission, bounded per mission id.
const MAX_REFUSALS_PER_MISSION: usize = 256;
/// Minted mission tokens outlive the mission's own evidence session so a
/// long-running solve never loses authority mid-flight; they are in-memory
/// (process-lifetime) just like the grants map they live in.
const MISSION_TOKEN_TTL_SECS: u64 = 24 * 60 * 60;

fn mission_gates() -> &'static RwLock<HashMap<String, EgressGate>> {
    static GATES: OnceLock<RwLock<HashMap<String, EgressGate>>> = OnceLock::new();
    GATES.get_or_init(|| RwLock::new(HashMap::new()))
}

fn mission_refusals() -> &'static RwLock<HashMap<String, Vec<AuthorityRefusal>>> {
    static REFUSALS: OnceLock<RwLock<HashMap<String, Vec<AuthorityRefusal>>>> = OnceLock::new();
    REFUSALS.get_or_init(|| RwLock::new(HashMap::new()))
}

fn mission_tokens() -> &'static RwLock<HashSet<String>> {
    static TOKENS: OnceLock<RwLock<HashSet<String>>> = OnceLock::new();
    TOKENS.get_or_init(|| RwLock::new(HashSet::new()))
}

fn read_lock<T>(lock: &RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(|e| e.into_inner())
}

fn write_lock<T>(lock: &RwLock<T>) -> std::sync::RwLockWriteGuard<'_, T> {
    lock.write().unwrap_or_else(|e| e.into_inner())
}

/// The baseline (action, resource) pairs a mission's token set mirrors from
/// the process posture. Anything the host subject cannot do is skipped —
/// minting only ever narrows.
const MISSION_TOKEN_BASELINE: &[(&str, &str)] = &[
    (actions::FILESYSTEM_READ, "*"),
    (actions::FILESYSTEM_WRITE, "*"),
    (actions::SANDBOX_EXEC, "*"),
    (actions::TRANSMIT, "app://*"),
    (actions::PROCESS_EXEC, "*"),
    (actions::NETWORK_EGRESS, "*"),
    (actions::CLOUD_INFERENCE, "*"),
];

impl ToolRegistry {
    /// The mission bound to this dispatch, if any: the thread-local evidence
    /// session first, then the newest active session for the workspace
    /// (swarm workers do not inherit thread-locals).
    fn current_mission(workspace: &Path) -> Option<String> {
        crate::susi_core::capture::EvidenceSession::current()
            .or_else(|| crate::susi_core::capture::EvidenceSession::for_workspace(workspace))
            .map(|session| session.id().to_string())
    }

    /// Mint the mission's token set (once per mission id) and install its
    /// egress gate from the workspace declaration — unless an orchestrator
    /// already declared one programmatically.
    fn ensure_mission_authority(
        mission: &str,
        workspace: &Path,
    ) -> crate::susi_error::EaiResult<()> {
        let mut tokens = write_lock(mission_tokens());
        if tokens.contains(mission) {
            return Ok(());
        }
        let mac = MacPolicy::global();
        for (action, resource) in MISSION_TOKEN_BASELINE {
            if mac.is_permitted("susi", action, resource) {
                mac.grant(mission, action, resource, Some(MISSION_TOKEN_TTL_SECS))?;
            }
        }
        let mut gates = write_lock(mission_gates());
        gates
            .entry(mission.to_owned())
            .or_insert_with(|| EgressGate::new(Self::mission_egress_policy(workspace)));
        tokens.insert(mission.to_owned());
        Ok(())
    }

    /// Load the mission's declared egress allow-list from
    /// `{workspace}/.susi/mission-egress.json`. Absent → permissive (the
    /// documented default); present but malformed → deny-all (fail closed).
    fn mission_egress_policy(workspace: &Path) -> EgressPolicy {
        let path = workspace.join(".susi").join("mission-egress.json");
        let Ok(text) = std::fs::read_to_string(&path) else {
            return EgressPolicy::permissive();
        };
        let Ok(decl) = serde_json::from_str::<serde_json::Value>(&text) else {
            return EgressPolicy::deny_all();
        };
        if decl.get("permissive").and_then(|v| v.as_bool()) == Some(true) {
            return EgressPolicy::permissive();
        }
        let mut policy = EgressPolicy::deny_all();
        if let Some(hosts) = decl.get("allow_hosts").and_then(|v| v.as_array()) {
            for host in hosts.iter().filter_map(|h| h.as_str()) {
                policy = policy.allow_host(host);
            }
        }
        policy
    }

    /// Consult the mission's egress gate for an egress-class tool dispatch.
    /// Returns the refusal text when the mission's policy denies the call.
    fn check_mission_egress(mission: &str, tool: &str, arg: &serde_json::Value) -> Option<String> {
        let needs_egress = MacPolicy::requirements_for_tool(tool)
            .iter()
            .any(|(action, _)| *action == actions::NETWORK_EGRESS);
        if !needs_egress {
            return None;
        }
        let mut gates = write_lock(mission_gates());
        let gate = gates.get_mut(mission)?;
        if gate.policy().is_permissive() {
            return None;
        }
        let mut hosts = Vec::new();
        collect_url_hosts(arg, &mut hosts);
        if hosts.is_empty() {
            // An egress-class tool whose destination is not visible in its
            // arguments cannot be verified against an allow-list — the
            // refusal is recorded under the tool's own name.
            if let Err(reason) = gate.authorize(mission, &format!("tool:{tool}")) {
                return Some(format!(
                    "Governance Violation: EGRESS DENY: mission `{mission}` policy allows no undeclared destination for `{tool}` ({reason})"
                ));
            }
            return None;
        }
        for host in hosts {
            if let Err(reason) = gate.authorize(mission, &host) {
                return Some(format!(
                    "Governance Violation: EGRESS DENY: mission `{mission}` may not reach `{host}` for `{tool}` ({reason})"
                ));
            }
        }
        None
    }

    fn record_authority_refusal(mission: &str, tool: &str, reason: &str) {
        let mut log = write_lock(mission_refusals());
        let entries = log.entry(mission.to_owned()).or_default();
        if entries.len() < MAX_REFUSALS_PER_MISSION {
            entries.push(AuthorityRefusal {
                mission: mission.to_owned(),
                tool: tool.to_owned(),
                reason: reason.to_owned(),
            });
        }
    }

    /// The egress refusals a mission's gate has recorded, in order.
    pub fn mission_egress_refusals(mission: &str) -> Vec<EgressRefusal> {
        read_lock(mission_gates())
            .get(mission)
            .map(|gate| gate.refusals().to_vec())
            .unwrap_or_default()
    }

    /// Every authority refusal recorded for a dispatch subject (`"susi"` is
    /// the process-level subject used when no mission is bound).
    pub fn mission_authority_refusals(mission: &str) -> Vec<AuthorityRefusal> {
        read_lock(mission_refusals())
            .get(mission)
            .cloned()
            .unwrap_or_default()
    }

    /// The egress policy currently in force for a mission, if it has been
    /// initialized (or declared programmatically).
    pub fn mission_egress_policy_for(mission: &str) -> Option<EgressPolicy> {
        read_lock(mission_gates())
            .get(mission)
            .map(|gate| gate.policy().clone())
    }

    /// Declare a mission's egress allow-list directly — for orchestrators
    /// that carry the policy in the mission spec rather than the workspace
    /// declaration file. Installs (or replaces) the gate; token minting
    /// still happens on first dispatch, and a declared gate is never
    /// overwritten by the workspace file.
    pub fn declare_mission_egress(mission: &str, policy: EgressPolicy) {
        write_lock(mission_gates()).insert(mission.to_owned(), EgressGate::new(policy));
    }

    /// Release a finished mission's minted tokens and gate. The refusal
    /// records stay — they are the audit trail, not live state.
    pub fn end_mission_authority(mission: &str) -> crate::susi_error::EaiResult<()> {
        let mut tokens = write_lock(mission_tokens());
        let mac = MacPolicy::global();
        let mut failure = None;
        for (action, resource) in MISSION_TOKEN_BASELINE {
            if let Err(error) = mac.revoke(mission, action, resource) {
                failure.get_or_insert(error);
            }
        }
        if let Some(error) = failure {
            return Err(error);
        }
        tokens.remove(mission);
        write_lock(mission_gates()).remove(mission);
        Ok(())
    }
}

/// Collect the host of every `http(s)://` URL found in the argument tree —
/// string values, nested objects and arrays. Hosts are lowercased, without
/// port. Non-URL strings contribute nothing.
fn collect_url_hosts(value: &serde_json::Value, hosts: &mut Vec<String>) {
    match value {
        serde_json::Value::String(text) => collect_url_hosts_in_text(text, hosts),
        serde_json::Value::Array(items) => {
            for item in items {
                collect_url_hosts(item, hosts);
            }
        }
        serde_json::Value::Object(map) => {
            for item in map.values() {
                collect_url_hosts(item, hosts);
            }
        }
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
    }
}

fn collect_url_hosts_in_text(text: &str, hosts: &mut Vec<String>) {
    let mut rest = text;
    loop {
        let Some(scheme_at) = rest.find("://") else {
            return;
        };
        let scheme = &rest[..scheme_at];
        let scheme_start = scheme
            .rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.'))
            .map(|i| i + 1)
            .unwrap_or(0);
        let scheme = &scheme[scheme_start..];
        rest = &rest[scheme_at + 3..];
        if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
            continue;
        }
        let end = rest
            .find(|c: char| c == '/' || c == '?' || c == '#' || c.is_whitespace())
            .unwrap_or(rest.len());
        let authority = &rest[..end];
        // Strip optional userinfo and port: host[:port].
        let host = authority
            .rsplit('@')
            .next()
            .unwrap_or(authority)
            .split(':')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        if !host.is_empty() && !hosts.contains(&host) {
            hosts.push(host);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockCapabilityTool;

    impl crate::susi_core::registry::Tool for MockCapabilityTool {
        fn name(&self) -> &str {
            "mock_mcp:echo"
        }
        fn description(&self) -> &str {
            "test tool"
        }
        fn execute(
            &self,
            _args: &serde_json::Value,
            _workspace: &Path,
        ) -> crate::susi_core::susi_error::EaiResult<String> {
            Ok("from-capability-registry".to_string())
        }
    }

    #[test]
    fn execute_tool_routes_discovered_mcp_via_capability_registry() {
        crate::susi_core::registry::CapabilityRegistry::global().register_tool(MockCapabilityTool);
        assert!(ToolRegistry::exists("mock_mcp:echo"));
        let out =
            ToolRegistry::execute_tool("mock_mcp:echo", &serde_json::json!({}), Path::new("."));
        assert_eq!(out, "from-capability-registry");
        let listed = ToolRegistry::list_tools();
        assert!(
            listed.iter().any(|t| t.name == "mock_mcp:echo"),
            "discovered tool must appear in list_tools"
        );
    }

    /// VC-202-014: the per-mission egress gate is consulted on the real
    /// dispatch path, missions act on their own minted tokens, and every
    /// refusal is recorded against the mission.
    mod vc_202_014_mastery {
        use super::*;
        use crate::susi_core::capture::{EvidenceScope, EvidenceSession};

        struct MockEgressTool;

        impl crate::susi_core::registry::Tool for MockEgressTool {
            fn name(&self) -> &str {
                "mcp_probe_egress"
            }
            fn description(&self) -> &str {
                "egress-class probe for mastery tests"
            }
            fn execute(
                &self,
                _args: &serde_json::Value,
                _workspace: &Path,
            ) -> crate::susi_core::susi_error::EaiResult<String> {
                Ok("egress-ok".to_string())
            }
        }

        struct MockLocalTool;

        impl crate::susi_core::registry::Tool for MockLocalTool {
            fn name(&self) -> &str {
                "zz_local_probe"
            }
            fn description(&self) -> &str {
                "local probe for mastery tests"
            }
            fn execute(
                &self,
                _args: &serde_json::Value,
                _workspace: &Path,
            ) -> crate::susi_core::susi_error::EaiResult<String> {
                Ok("local-ok".to_string())
            }
        }

        fn test_session(workspace: &Path) -> (Arc<EvidenceSession>, EvidenceScope) {
            let session =
                EvidenceSession::new("vc-202-014 mastery mission", workspace, |s| s.to_string())
                    .unwrap();
            let scope = EvidenceSession::enter(Some(Arc::clone(&session)));
            (session, scope)
        }

        fn declare(workspace: &Path, body: &str) {
            let dir = workspace.join(".susi");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("mission-egress.json"), body).unwrap();
        }

        /// The declared allow-list is consulted at dispatch time: a denied
        /// host never reaches the tool, and the refusal lands in the
        /// mission's egress record.
        #[test]
        fn vc_202_014_mastery_gate_consulted_per_mission_refusals_recorded() {
            let dir = susi_paths::tempfile::tempdir().unwrap();
            declare(dir.path(), r#"{"allow_hosts": ["api.openai.com"]}"#);
            crate::susi_core::registry::CapabilityRegistry::global().register_tool(MockEgressTool);
            let (session, _scope) = test_session(dir.path());
            let mission = session.id().to_string();

            let denied = ToolRegistry::execute_tool(
                "mcp_probe_egress",
                &serde_json::json!({"url": "https://exfil.example.net/x"}),
                dir.path(),
            );
            assert!(denied.contains("EGRESS DENY"), "{denied}");
            let refusals = ToolRegistry::mission_egress_refusals(&mission);
            assert_eq!(refusals.len(), 1);
            assert_eq!(refusals[0].mission, mission);
            assert_eq!(refusals[0].host, "exfil.example.net");

            let allowed = ToolRegistry::execute_tool(
                "mcp_probe_egress",
                &serde_json::json!({"url": "https://api.openai.com/v1/models"}),
                dir.path(),
            );
            assert_eq!(allowed, "egress-ok");
            assert_eq!(ToolRegistry::mission_egress_refusals(&mission).len(), 1);

            // An egress-class call whose destination is not visible in its
            // args cannot be verified against an allow-list — refused, with
            // the tool's own name on the record.
            let unverifiable = ToolRegistry::execute_tool(
                "mcp_probe_egress",
                &serde_json::json!({"q": "nothing"}),
                dir.path(),
            );
            assert!(unverifiable.contains("EGRESS DENY"), "{unverifiable}");
            let refusals = ToolRegistry::mission_egress_refusals(&mission);
            assert_eq!(refusals.len(), 2);
            assert_eq!(refusals[1].host, "tool:mcp_probe_egress");
        }

        /// A mission without a declaration keeps the documented permissive
        /// posture — the gate is still installed and consulted, it simply
        /// refuses nothing.
        #[test]
        fn vc_202_014_mastery_permissive_mission_refuses_nothing() {
            let dir = susi_paths::tempfile::tempdir().unwrap();
            crate::susi_core::registry::CapabilityRegistry::global().register_tool(MockEgressTool);
            let (session, _scope) = test_session(dir.path());
            let mission = session.id().to_string();

            let out = ToolRegistry::execute_tool(
                "mcp_probe_egress",
                &serde_json::json!({"url": "https://anything.example.net/"}),
                dir.path(),
            );
            assert_eq!(out, "egress-ok");
            assert!(ToolRegistry::mission_egress_policy_for(&mission)
                .is_some_and(|p| p.is_permissive()));
            assert!(ToolRegistry::mission_egress_refusals(&mission).is_empty());
        }

        /// Dispatch checks the mission's own token set: strip one authority
        /// from the mission and the next dispatch for it is refused — while
        /// the process subject still holds it — and the refusal is recorded
        /// against the mission.
        #[test]
        fn vc_202_014_mastery_agent_tokens_checked_at_dispatch() {
            let dir = susi_paths::tempfile::tempdir().unwrap();
            crate::susi_core::registry::CapabilityRegistry::global().register_tool(MockLocalTool);
            let (session, _scope) = test_session(dir.path());
            let mission = session.id().to_string();

            // First dispatch mints the posture-bounded token set.
            let out =
                ToolRegistry::execute_tool("zz_local_probe", &serde_json::json!({}), dir.path());
            assert_eq!(out, "local-ok");
            assert!(MacPolicy::global().is_permitted(&mission, actions::FILESYSTEM_READ, "*"));

            // The mission's own token no longer covers exec — the process
            // subject still holds it, so this refusal is the mission's own.
            assert!(MacPolicy::global()
                .revoke(&mission, actions::PROCESS_EXEC, "*")
                .unwrap());
            let out = ToolRegistry::execute_tool(
                "exec_command",
                &serde_json::json!({"cmd": "echo hi"}),
                dir.path(),
            );
            assert!(out.contains("MAC DENY"), "{out}");
            let refusals = ToolRegistry::mission_authority_refusals(&mission);
            assert!(
                refusals
                    .iter()
                    .any(|r| r.tool == "exec_command" && r.mission == mission),
                "mission refusal must be recorded: {refusals:?}"
            );
        }

        /// The workspace declaration binds the mission: absent → permissive
        /// default, `allow_hosts` → closed allow-list, malformed → fail
        /// closed rather than silently open.
        #[test]
        fn vc_202_014_mastery_workspace_declaration_binds_policy() {
            let dir = susi_paths::tempfile::tempdir().unwrap();
            assert!(ToolRegistry::mission_egress_policy(dir.path()).is_permissive());

            declare(
                dir.path(),
                r#"{"allow_hosts": ["a.example.com", "*.b.example.com"]}"#,
            );
            let policy = ToolRegistry::mission_egress_policy(dir.path());
            assert!(!policy.is_permissive());
            assert!(policy.allows("a.example.com"));
            assert!(policy.allows("deep.b.example.com"));
            assert!(!policy.allows("c.example.net"));

            declare(dir.path(), "this is not json");
            let policy = ToolRegistry::mission_egress_policy(dir.path());
            assert!(!policy.is_permissive());
            assert!(!policy.allows("a.example.com"));
        }

        /// Host extraction sees nested args, strips scheme/port/userinfo and
        /// lowercases — the gate matches on exactly this form.
        #[test]
        fn vc_202_014_mastery_url_hosts_extracted_from_tool_args() {
            let mut hosts = Vec::new();
            collect_url_hosts(
                &serde_json::json!({
                    "url": "https://API.Example.com:8443/x?q=1",
                    "nested": {"a": ["http://user@b.example.org/p", "plain text"]},
                    "other": 42
                }),
                &mut hosts,
            );
            hosts.sort();
            assert_eq!(hosts, vec!["api.example.com", "b.example.org"]);
        }
    }
}
