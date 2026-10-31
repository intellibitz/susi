//! LangGraph peer via an operator-supplied script. The OS ships no sample program.

use super::python_engine::EngineProfile;

pub const ENGINE_ID: &str = "langgraph";
pub const CONFIG_ENV: &str = "SUSI_LANGGRAPH_CONFIG";
pub const IMPORT_NAME: &str = "langgraph";

pub const PROFILE: EngineProfile = EngineProfile {
    engine_id: ENGINE_ID,
    config_env: CONFIG_ENV,
    import_name: IMPORT_NAME,
    package_hint: "pip/uv install langgraph",
    documentation: "https://langchain-ai.github.io/langgraph/",
    credential_envs: &[],
    credential_hint:
        "no API key required for local/stdlib entry (model keys stay in your graph script env)",
    process_banner: "susi-langgraph",
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_points_at_operator_config() {
        assert_eq!(PROFILE.engine_id, ENGINE_ID);
        assert_eq!(PROFILE.config_env, CONFIG_ENV);
    }

    #[test]
    fn profile_ships_no_python_source() {
        let source = include_str!("langgraph.rs");
        assert!(!source.contains(&["#!", "/usr/bin/env python3"].concat()));
        assert!(!source.contains(&["example", "_script"].concat()));
        assert!(!source.contains(&["init", "_workspace"].concat()));
    }
}
