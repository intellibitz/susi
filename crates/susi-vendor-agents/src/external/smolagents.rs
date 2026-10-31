//! Hugging Face smolagents peer via an operator-supplied script. The OS ships no sample program.

use super::python_engine::EngineProfile;

pub const ENGINE_ID: &str = "smolagents";
pub const CONFIG_ENV: &str = "SUSI_SMOLAGENTS_CONFIG";
pub const IMPORT_NAME: &str = "smolagents";

pub const PROFILE: EngineProfile = EngineProfile {
    engine_id: ENGINE_ID,
    config_env: CONFIG_ENV,
    import_name: IMPORT_NAME,
    package_hint: "pip/uv install smolagents",
    documentation: "https://huggingface.co/docs/smolagents",
    credential_envs: &[
        "OPENAI_API_KEY",
        "OPENROUTER_API_KEY",
        "HF_TOKEN",
        "HUGGINGFACEHUB_API_TOKEN",
        "ANTHROPIC_API_KEY",
    ],
    credential_hint:
        "export OPENAI_API_KEY=… (OpenAI-compatible) or HF_TOKEN (InferenceClientModel)",
    process_banner: "susi-smolagents",
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
        let source = include_str!("smolagents.rs");
        assert!(!source.contains(&["#!", "/usr/bin/env python3"].concat()));
        assert!(!source.contains(&["example", "_script"].concat()));
        assert!(!source.contains(&["init", "_workspace"].concat()));
    }
}
