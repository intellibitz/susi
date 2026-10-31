//! E2B sandbox peer via an operator-supplied script. The OS ships no sample program.

use super::python_engine::EngineProfile;

pub const ENGINE_ID: &str = "e2b";
pub const CONFIG_ENV: &str = "SUSI_E2B_CONFIG";
pub const IMPORT_NAME: &str = "e2b";

pub const PROFILE: EngineProfile = EngineProfile {
    engine_id: ENGINE_ID,
    config_env: CONFIG_ENV,
    import_name: IMPORT_NAME,
    package_hint: "pip/uv install e2b e2b-code-interpreter",
    documentation: "https://e2b.dev/docs",
    credential_envs: &["E2B_API_KEY"],
    credential_hint: "export E2B_API_KEY=… (from https://e2b.dev)",
    process_banner: "susi-e2b",
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_points_at_operator_config() {
        assert_eq!(PROFILE.engine_id, ENGINE_ID);
        assert_eq!(PROFILE.config_env, CONFIG_ENV);
        let source = include_str!("e2b.rs");
        assert!(!source.contains(&["#!", "/usr/bin/env python3"].concat()));
        assert!(!source.contains(&["example", "_script"].concat()));
        assert!(!source.contains(&["init", "_workspace"].concat()));
    }
}
