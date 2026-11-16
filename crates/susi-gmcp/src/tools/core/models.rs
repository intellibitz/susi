//! CoreTools handlers for model inventory, placement, routing preferences, and model installation.

use std::path::Path;
use susi_config::JsonFields as _;

use super::CoreTools;
use crate::susi_core::plane_bus::gawd;
use crate::susi_core::plane_bus::gemi::ModelManager;
use crate::susi_error::{EaiError, EaiResult};
use crate::tools::args;

impl CoreTools {
    crate::tool_def!(list_models_tool_attr {
        name: "list_models",
        description: "List available model substrates",
        input_schema: args::no_args(),
    });
    pub fn list_models(_arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let models = ModelManager::list_models(workspace);
        let count = models.as_array().map(|a| a.len()).unwrap_or(0);
        Ok(format!(
            "Active Model Substrates (Count: {count})\n\n{models}"
        ))
    }

    crate::tool_def!(select_model_tool_attr {
        name: "select_model",
        description:
            "Select or override the active model substrate, as `susi select-model`. Args: model",
        input_schema: args::typed::<args::SelectModel>(),
    });
    pub fn select_model(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        // `{model}` over MCP; a bare string from in-process callers.
        let model = arg
            .field_str("model")
            .or_else(|| arg.as_str())
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .ok_or_else(|| EaiError::config("model is required"))?;
        let v = ModelManager::set_selected_model(model);
        if let Some(e) = v.field_str("error") {
            return Err(EaiError::config(e));
        }
        Ok(v.field_str("text").unwrap_or("model selected").to_string())
    }

    crate::tool_def!(inference_placement_tool_attr {
        name: "inference_placement",
        description: "Explain whether SUSI will run inference locally or in cloud and why. Args: optional requires, max_cost, allow_cloud (default true)",
        input_schema: args::typed::<args::InferencePlacement>(),
    });
    pub fn inference_placement(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        // The registry path does not enforce the typed schema, so the
        // constraints are validated here with the same rules a completion's
        // `susi` block gets: a malformed field is refused, never defaulted.
        let constraints = match arg {
            serde_json::Value::Null => None,
            serde_json::Value::Object(object) => Some(object),
            serde_json::Value::Bool(_)
            | serde_json::Value::Number(_)
            | serde_json::Value::String(_)
            | serde_json::Value::Array(_) => {
                return Err(EaiError::config("arguments must be a JSON object"))
            }
        };
        let constraints = crate::susi_core::plane_bus::gemi::placement_constraints(constraints, "")
            .map_err(EaiError::config)?;
        let placement = ModelManager::placement_with_cloud(
            constraints.requires.as_deref(),
            constraints.max_cost,
            constraints.allow_cloud,
        );
        serde_json::to_string_pretty(&placement)
            .map_err(|error| EaiError::internal(format!("encode placement: {error}")))
    }

    crate::tool_def!(inference_reset_provider_tool_attr {
        name: "inference_reset_provider",
        description: "Re-admit a repaired inference provider after routing quarantine. Args: {provider: exact provider id}",
        input_schema: args::typed::<args::InferenceResetProvider>(),
    });
    pub fn inference_reset_provider(
        arg: &serde_json::Value,
        workspace: &Path,
    ) -> EaiResult<String> {
        let provider = arg
            .field_str("provider")
            .map(str::trim)
            .filter(|value| crate::susi_core::plane_bus::gemi::valid_provider_id(value))
            .ok_or_else(|| EaiError::config("provider is required"))?;
        let cleared = ModelManager::clear_provider_cooldown(provider).map_err(|message| {
            EaiError::network(format!("reset provider quarantine: {message}"))
        })?;
        if cleared {
            crate::susi_sandbox::manager::SusiAuditLogger::log_event(
                workspace,
                "INFERENCE_PROVIDER_READMITTED",
                provider,
            );
        }
        serde_json::to_string_pretty(&serde_json::json!({
            "provider": provider,
            "cleared": cleared,
        }))
        .map_err(|error| EaiError::internal(format!("encode cooldown reset: {error}")))
    }

    crate::tool_def!(scout_model_tool_attr {
        name: "scout_model",
        description: "Download, license-admit and verify a model from an HTTPS URL on the daemon's download agent, as `susi scout-model`. Args: url",
        input_schema: args::typed::<args::ScoutModel>(),
    });
    pub fn scout_model(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        // `{url}` over MCP; a bare string from in-process callers.
        let url = arg
            .field_str("url")
            .or_else(|| arg.as_str())
            .map(str::trim)
            .filter(|u| !u.is_empty())
            .ok_or_else(|| EaiError::config("url is required"))?;
        // The CLI refuses anything but HTTPS; so does the socket, rather
        // than reporting a non-URL query as "enqueued".
        if !url.starts_with("https://") {
            return Err(EaiError::config("model installation requires an HTTPS URL"));
        }
        // The long-lived daemon downloads on its download agent (the CLI,
        // which exits, downloads in the foreground).
        let res = ModelManager::install_model(url);
        let text = res
            .field_str("text")
            .map(str::to_string)
            .unwrap_or_else(|| res.to_string());
        if let Some(error) = text.strip_prefix("ERROR: ") {
            return Err(EaiError::network(error.to_string()));
        }
        Ok(text)
    }

    crate::tool_def!(verify_model_download_agent_tool_attr {
        name: "verify_model_download_agent",
        description: "Verify model download agent, check network status, and report provisioning of the hardware-selected model ladder",
        input_schema: args::no_args(),
    });
    pub fn verify_model_download_agent(
        _arg: &serde_json::Value,
        workspace: &Path,
    ) -> EaiResult<String> {
        let report = ModelManager::verify_local_models(workspace);
        Ok(serde_json::to_string_pretty(&report).unwrap_or_else(|_| report.to_string()))
    }

    crate::tool_def!(train_reflexes_tool_attr {
        name: "train_reflexes",
        description: "Manually trigger native neural reflex distillation",
        input_schema: args::no_args(),
    });
    pub fn train_reflexes(_arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        gawd::train_reflexes(workspace).map_err(EaiError::governance)
    }

    crate::tool_def!(swarm_schedule_tool_attr {
        name: "swarm_schedule",
        description:
            "Show recent mission scheduler decisions: per-agent scores, admitted vs deferred",
        input_schema: args::no_args(),
    });
    pub fn swarm_schedule(_arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let decisions = gawd::scheduler_recent_decisions(50);
        if decisions.as_array().map(|a| a.is_empty()).unwrap_or(true) {
            return Ok("No missions scheduled yet.".to_string());
        }
        Ok(serde_json::to_string_pretty(&decisions).unwrap_or_else(|_| decisions.to_string()))
    }

    crate::tool_def!(coding_models_list_tool_attr {
        name: "coding_models_list",
        description: "List top developer/agent models and local setup readiness",
        input_schema: args::no_args(),
    });
    pub fn coding_models_list(_arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let rows = crate::susi_core::plane_bus::gemi::coding_catalog();
        serde_json::to_string(&rows).map_err(|e| EaiError::protocol(e.to_string()))
    }

    crate::tool_def!(coding_models_prefer_tool_attr {
        name: "coding_models_prefer",
        description: "Prefer a coding/agent model id for subsequent routing",
        input_schema: args::typed::<args::CodingModelsPrefer>(),
    });
    pub fn coding_models_prefer(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let model = arg
            .field_str("model")
            .ok_or_else(|| EaiError::protocol("model is required"))?;
        // Actually prefer it on the GEMI plane; a failed preflight is an
        // error, not a `{"preferred": …}` claim.
        let applied =
            crate::susi_core::plane_bus::gemi::coding_prefer(model).map_err(EaiError::process)?;
        serde_json::to_string(&applied).map_err(|e| EaiError::protocol(e.to_string()))
    }
}
