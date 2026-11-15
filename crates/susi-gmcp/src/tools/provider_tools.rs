//! Socket reach for the provider, key and brain commands (zero-UI plan B4
//! group 3): `susi openweight|frontier|openrouter`, `susi models probe`,
//! `susi keys list|prefer|check|models`, `susi brain`, `susi benchmark` and
//! `susi verify-download-agent`.
//!
//! Each tool forwards to the daemon's `daemon.providers` topic, which calls
//! the same `susi_daemon::provider_ops` function the CLI command calls and
//! returns the document the command prints. Key values never cross: key
//! tools report presence only, and setting or removing a key stays local
//! (`tests/socket_parity.rs` exempts `keys set` and `keys remove`).

use serde_json::{json, Value};
use std::path::Path;

use crate::susi_core::plane_bus::daemon;
use crate::susi_error::{EaiError, EaiResult};

use super::args;
use super::forward::{forward, policy_paths, Kind};
use super::CoreTools;

const VENDOR: (&str, Kind) = ("vendor", Kind::Str);
const MODEL: (&str, Kind) = ("model", Kind::Str);

fn providers(action: &str, arg: &Value, keys: &[(&str, Kind)]) -> EaiResult<String> {
    forward(daemon::providers, action, arg, keys, json!({}))
}

/// [`providers`] with the caller's file paths (`(key, write)`) resolved
/// through the MAC policy.
fn providers_with_paths(
    action: &str,
    arg: &Value,
    keys: &[(&str, Kind)],
    paths: &[(&str, bool)],
) -> EaiResult<String> {
    forward(
        daemon::providers,
        action,
        arg,
        keys,
        policy_paths(arg, paths)?,
    )
}

impl CoreTools {
    crate::tool_def!(provider_list_tool_attr {
        name: "provider_list",
        description: "Curated models (routes) of a provider control plane and their readiness, as `susi <openweight|frontier|openrouter> list`. Args: vendor",
        input_schema: args::typed::<args::ProviderVendor>(),
    });
    pub fn provider_list(arg: &Value, _workspace: &Path) -> EaiResult<String> {
        providers("list", arg, &[VENDOR])
    }

    crate::tool_def!(provider_doctor_tool_attr {
        name: "provider_doctor",
        description: "Readiness documents of a provider control plane (no paid call), as `susi <vendor> doctor`. Args: vendor, optional model, live (openrouter: list live model ids)",
        input_schema: args::typed::<args::ProviderDoctor>(),
    });
    pub fn provider_doctor(arg: &Value, _workspace: &Path) -> EaiResult<String> {
        providers("doctor", arg, &[VENDOR, MODEL, ("live", Kind::Bool)])
    }

    crate::tool_def!(provider_setup_tool_attr {
        name: "provider_setup",
        description: "Setup instructions and effective wiring, as `susi <vendor> setup`. Args: vendor, model (openweight and frontier)",
        input_schema: args::typed::<args::ProviderSetup>(),
    });
    pub fn provider_setup(arg: &Value, _workspace: &Path) -> EaiResult<String> {
        providers("setup", arg, &[VENDOR, MODEL])
    }

    crate::tool_def!(provider_configure_tool_attr {
        name: "provider_configure",
        description: "Store a host override for a model, as `susi <openweight|frontier> configure`. Args: vendor, model, and override (object) or file (absolute path)",
        input_schema: args::typed::<args::ProviderConfigure>(),
    });
    pub fn provider_configure(arg: &Value, _workspace: &Path) -> EaiResult<String> {
        providers_with_paths(
            "configure",
            arg,
            &[
                VENDOR,
                MODEL,
                ("override", Kind::Object),
                ("file", Kind::Str),
            ],
            &[("file", false)],
        )
    }

    crate::tool_def!(provider_reset_tool_attr {
        name: "provider_reset",
        description: "Drop a model's host override and return the bundled entry, as `susi <openweight|frontier> reset`. Args: vendor, model",
        input_schema: args::typed::<args::ProviderModel>(),
    });
    pub fn provider_reset(arg: &Value, _workspace: &Path) -> EaiResult<String> {
        providers("reset", arg, &[VENDOR, MODEL])
    }

    crate::tool_def!(provider_pull_tool_attr {
        name: "provider_pull",
        description: "Pull an open-weight model's weights through Ollama, as `susi openweight pull`. Args: model",
        input_schema: args::typed::<args::ProviderPull>(),
    });
    pub fn provider_pull(arg: &Value, _workspace: &Path) -> EaiResult<String> {
        providers("pull", arg, &[MODEL])
    }

    crate::tool_def!(provider_prefer_tool_attr {
        name: "provider_prefer",
        description: "Prefer (or clear) a provider model for routing, as `susi <vendor> prefer`. Args: vendor, model, clear",
        input_schema: args::typed::<args::ProviderPrefer>(),
    });
    pub fn provider_prefer(arg: &Value, _workspace: &Path) -> EaiResult<String> {
        providers("prefer", arg, &[VENDOR, MODEL, ("clear", Kind::Bool)])
    }

    crate::tool_def!(provider_probe_tool_attr {
        name: "provider_probe",
        description: "Short completion through a model's endpoint (cloud probes are paid), as `susi <vendor> probe` and `susi models probe` (vendor coding). Args: vendor, model, optional prompt",
        input_schema: args::typed::<args::ProviderProbe>(),
    });
    pub fn provider_probe(arg: &Value, _workspace: &Path) -> EaiResult<String> {
        providers("probe", arg, &[VENDOR, MODEL, ("prompt", Kind::Str)])
    }

    crate::tool_def!(provider_models_tool_attr {
        name: "provider_models",
        description: "OpenRouter's curated routes, or the live model ids its key reaches, as `susi openrouter models`. Args: optional live, limit",
        input_schema: args::typed::<args::ProviderModels>(),
    });
    pub fn provider_models(arg: &Value, _workspace: &Path) -> EaiResult<String> {
        providers(
            "openrouter_models",
            arg,
            &[("live", Kind::Bool), ("limit", Kind::U64)],
        )
    }

    crate::tool_def!(keys_status_tool_attr {
        name: "keys_status",
        description: "Which cloud vendor keys are set (presence only, never values), the routing preference and the key file, as `susi keys list`",
        input_schema: args::no_args(),
    });
    pub fn keys_status(arg: &Value, _workspace: &Path) -> EaiResult<String> {
        providers("keys_status", arg, &[])
    }

    crate::tool_def!(keys_prefer_tool_attr {
        name: "keys_prefer",
        description: "Pin, clear or report the preferred cloud vendor, as `susi keys prefer`. Args: optional vendor, clear",
        input_schema: args::typed::<args::KeysPrefer>(),
    });
    pub fn keys_prefer(arg: &Value, _workspace: &Path) -> EaiResult<String> {
        providers("keys_prefer", arg, &[VENDOR, ("clear", Kind::Bool)])
    }

    crate::tool_def!(keys_check_tool_attr {
        name: "keys_check",
        description: "Each key's state against its vendor's model list (key values never returned), as `susi keys check`. Args: optional vendor",
        input_schema: args::typed::<args::KeysCheck>(),
    });
    pub fn keys_check(arg: &Value, _workspace: &Path) -> EaiResult<String> {
        providers("keys_check", arg, &[VENDOR])
    }

    crate::tool_def!(keys_models_tool_attr {
        name: "keys_models",
        description: "The models a vendor's valid key reaches, as `susi keys models`. Args: vendor",
        input_schema: args::typed::<args::KeysModels>(),
    });
    pub fn keys_models(arg: &Value, _workspace: &Path) -> EaiResult<String> {
        providers("keys_models", arg, &[VENDOR])
    }

    crate::tool_def!(brain_status_tool_attr {
        name: "brain_status",
        description: "Evidence-ranked providers per task class, as `susi brain status --json`. Args: optional top, class",
        input_schema: args::typed::<args::BrainStatus>(),
    });
    pub fn brain_status(arg: &Value, _workspace: &Path) -> EaiResult<String> {
        providers(
            "brain_status",
            arg,
            &[("top", Kind::U64), ("class", Kind::Str)],
        )
    }

    crate::tool_def!(brain_budget_tool_attr {
        name: "brain_budget",
        description: "Set or report the budget dial and shared spend policy, as `susi brain budget`. Args: optional level, free_only, allow_paid (explicit spend authorization), max_spend_microusd, max_tokens, max_requests, max_parallel, new_window",
        input_schema: args::typed::<args::BrainBudget>(),
    });
    pub fn brain_budget(arg: &Value, _workspace: &Path) -> EaiResult<String> {
        providers(
            "brain_budget",
            arg,
            &[
                ("level", Kind::Str),
                ("free_only", Kind::Bool),
                ("allow_paid", Kind::Bool),
                ("max_spend_microusd", Kind::U64),
                ("max_tokens", Kind::U64),
                ("max_requests", Kind::U64),
                ("max_parallel", Kind::U64),
                ("new_window", Kind::Bool),
            ],
        )
    }

    crate::tool_def!(brain_classify_tool_attr {
        name: "brain_classify",
        description: "The task class a prompt routes as, as `susi brain classify`. Args: prompt",
        input_schema: args::typed::<args::BrainPrompt>(),
    });
    pub fn brain_classify(arg: &Value, _workspace: &Path) -> EaiResult<String> {
        providers("brain_classify", arg, &[("prompt", Kind::Str)])
    }

    crate::tool_def!(brain_explain_tool_attr {
        name: "brain_explain",
        description:
            "The brain's routing decision for a prompt, as `susi brain explain`. Args: prompt",
        input_schema: args::typed::<args::BrainPrompt>(),
    });
    pub fn brain_explain(arg: &Value, _workspace: &Path) -> EaiResult<String> {
        providers("brain_explain", arg, &[("prompt", Kind::Str)])
    }

    crate::tool_def!(brain_export_tool_attr {
        name: "brain_export",
        description: "The brain evidence bundle (or write it to an absolute path), as `susi brain export`. Args: optional out",
        input_schema: args::typed::<args::BrainExport>(),
    });
    pub fn brain_export(arg: &Value, _workspace: &Path) -> EaiResult<String> {
        providers_with_paths("brain_export", arg, &[("out", Kind::Str)], &[("out", true)])
    }

    crate::tool_def!(brain_import_tool_attr {
        name: "brain_import",
        description: "Merge a brain evidence bundle from an absolute path, as `susi brain import`. Args: file",
        input_schema: args::typed::<args::BrainImport>(),
    });
    pub fn brain_import(arg: &Value, _workspace: &Path) -> EaiResult<String> {
        providers_with_paths(
            "brain_import",
            arg,
            &[("file", Kind::Str)],
            &[("file", false)],
        )
    }

    crate::tool_def!(brain_matrix_tool_attr {
        name: "brain_matrix",
        description: "The measured capability matrix (success, latency and cost per verified outcome), as `susi brain matrix`",
        input_schema: args::no_args(),
    });
    pub fn brain_matrix(arg: &Value, _workspace: &Path) -> EaiResult<String> {
        providers("brain_matrix", arg, &[])
    }

    crate::tool_def!(brain_cache_tool_attr {
        name: "brain_cache",
        description:
            "The measured prompt-cache hit rate and estimated saving, as `susi brain cache`",
        input_schema: args::no_args(),
    });
    pub fn brain_cache(arg: &Value, _workspace: &Path) -> EaiResult<String> {
        providers("brain_cache", arg, &[])
    }

    crate::tool_def!(brain_reset_tool_attr {
        name: "brain_reset",
        description: "Forget every recorded brain outcome, as `susi brain reset`",
        input_schema: args::no_args(),
    });
    pub fn brain_reset(arg: &Value, _workspace: &Path) -> EaiResult<String> {
        providers("brain_reset", arg, &[])
    }

    crate::tool_def!(brain_bench_tool_attr {
        name: "brain_bench",
        description: "Run the fixed prompt set against the fake engine and record JSONL evidence, as `susi brain bench`. Args: optional out (absolute)",
        input_schema: args::typed::<args::BrainBench>(),
    });
    pub fn brain_bench(arg: &Value, _workspace: &Path) -> EaiResult<String> {
        providers_with_paths("brain_bench", arg, &[("out", Kind::Str)], &[("out", true)])
    }

    crate::tool_def!(brain_probe_tool_attr {
        name: "brain_probe",
        description: "Probe a registered provider with the scout benchmark and record the verified outcomes, as `susi brain probe`. Args: provider, optional no_record",
        input_schema: args::typed::<args::BrainProbe>(),
    });
    pub fn brain_probe(arg: &Value, _workspace: &Path) -> EaiResult<String> {
        providers(
            "brain_probe",
            arg,
            &[("provider", Kind::Str), ("no_record", Kind::Bool)],
        )
    }

    crate::tool_def!(benchmark_run_tool_attr {
        name: "benchmark_run",
        description: "Measure real local inference latency and tokens/sec (and a configured cloud endpoint), as `susi benchmark`",
        input_schema: args::no_args(),
    });
    pub fn benchmark_run(_arg: &Value, workspace: &Path) -> EaiResult<String> {
        let reply = daemon::providers(json!({
            "action": "benchmark",
            "workspace": workspace.display().to_string(),
        }))
        .map_err(EaiError::config)?;
        Ok(reply
            .get("report")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string())
    }

    crate::tool_def!(verify_download_agent_tool_attr {
        name: "verify_download_agent",
        description: "Network status and the hardware-selected model ladder's provisioning steps (all_verified is the verdict), as `susi verify-download-agent`; verification only, it never starts a download",
        input_schema: args::no_args(),
    });
    pub fn verify_download_agent(_arg: &Value, workspace: &Path) -> EaiResult<String> {
        forward(
            daemon::providers,
            "verify_download_agent",
            &json!({}),
            &[],
            json!({ "workspace": workspace.display().to_string() }),
        )
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[path = "../tests/provider_tools.rs"]
mod tests;
