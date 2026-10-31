//! Temporal durable-workflow peer via an operator-supplied script. The OS ships no sample program.

use super::python_engine::EngineProfile;

pub const ENGINE_ID: &str = "temporal";
pub const CONFIG_ENV: &str = "SUSI_TEMPORAL_CONFIG";
pub const IMPORT_NAME: &str = "temporalio";

pub const PROFILE: EngineProfile = EngineProfile {
    engine_id: ENGINE_ID,
    config_env: CONFIG_ENV,
    import_name: IMPORT_NAME,
    package_hint: "pip/uv install temporalio",
    documentation: "https://docs.temporal.io/develop/python",
    credential_envs: &[],
    credential_hint:
        "optional: TEMPORAL_ADDRESS / TEMPORAL_NAMESPACE (default dry-run without cluster)",
    process_banner: "susi-temporal",
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_points_at_operator_config() {
        assert_eq!(PROFILE.engine_id, ENGINE_ID);
        assert_eq!(PROFILE.config_env, CONFIG_ENV);
        let source = include_str!("temporal.rs");
        assert!(!source.contains(&["#!", "/usr/bin/env python3"].concat()));
        assert!(!source.contains(&["example", "_script"].concat()));
        assert!(!source.contains(&["init", "_workspace"].concat()));
    }
}
