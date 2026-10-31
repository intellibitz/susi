//! AutoGen (AgentChat) peer via an operator-supplied script. The OS ships no sample program.

use super::python_engine::EngineProfile;

pub const ENGINE_ID: &str = "autogen";
pub const CONFIG_ENV: &str = "SUSI_AUTOGEN_CONFIG";
pub const IMPORT_NAME: &str = "autogen_agentchat";

pub const PROFILE: EngineProfile = EngineProfile {
    engine_id: ENGINE_ID,
    config_env: CONFIG_ENV,
    import_name: IMPORT_NAME,
    package_hint: "pip/uv install \"autogen-agentchat\" \"autogen-ext[openai]\"",
    documentation: "https://microsoft.github.io/autogen/",
    credential_envs: &[
        "OPENAI_API_KEY",
        "AZURE_OPENAI_API_KEY",
        "OPENROUTER_API_KEY",
    ],
    credential_hint:
        "export OPENAI_API_KEY=… (or AZURE_OPENAI_API_KEY / OPENROUTER_API_KEY + base URL)",
    process_banner: "susi-autogen",
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
        let source = include_str!("autogen.rs");
        assert!(!source.contains(&["#!", "/usr/bin/env python3"].concat()));
        assert!(!source.contains(&["example", "_script"].concat()));
        assert!(!source.contains(&["init", "_workspace"].concat()));
    }
}
