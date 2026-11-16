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
use super::forward::forwarded_tools;
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
    forwarded_tools! {
        provider_list(provider_list_tool_attr, args::typed::<args::ProviderVendor>()),
        "Curated models (routes) of a provider control plane and their readiness, as `susi <openweight|frontier|openrouter> list`. Args: vendor",
        |arg, _workspace| providers("list", arg, &[VENDOR]);

        provider_doctor(provider_doctor_tool_attr, args::typed::<args::ProviderDoctor>()),
        "Readiness documents of a provider control plane (no paid call), as `susi <vendor> doctor`. Args: vendor, optional model, live (openrouter: list live model ids)",
        |arg, _workspace| providers("doctor", arg, &[VENDOR, MODEL, ("live", Kind::Bool)]);

        provider_setup(provider_setup_tool_attr, args::typed::<args::ProviderSetup>()),
        "Setup instructions and effective wiring, as `susi <vendor> setup`. Args: vendor, model (openweight and frontier)",
        |arg, _workspace| providers("setup", arg, &[VENDOR, MODEL]);

        provider_configure(provider_configure_tool_attr, args::typed::<args::ProviderConfigure>()),
        "Store a host override for a model, as `susi <openweight|frontier> configure`. Args: vendor, model, and override (object) or file (absolute path)",
        |arg, _workspace| providers_with_paths(
            "configure",
            arg,
            &[
                VENDOR,
                MODEL,
                ("override", Kind::Object),
                ("file", Kind::Str),
            ],
            &[("file", false)],
        );

        provider_reset(provider_reset_tool_attr, args::typed::<args::ProviderModel>()),
        "Drop a model's host override and return the bundled entry, as `susi <openweight|frontier> reset`. Args: vendor, model",
        |arg, _workspace| providers("reset", arg, &[VENDOR, MODEL]);

        provider_pull(provider_pull_tool_attr, args::typed::<args::ProviderPull>()),
        "Pull an open-weight model's weights through Ollama, as `susi openweight pull`. Args: model",
        |arg, _workspace| providers("pull", arg, &[MODEL]);

        provider_prefer(provider_prefer_tool_attr, args::typed::<args::ProviderPrefer>()),
        "Prefer (or clear) a provider model for routing, as `susi <vendor> prefer`. Args: vendor, model, clear",
        |arg, _workspace| providers("prefer", arg, &[VENDOR, MODEL, ("clear", Kind::Bool)]);

        provider_probe(provider_probe_tool_attr, args::typed::<args::ProviderProbe>()),
        "Short completion through a model's endpoint (cloud probes are paid), as `susi <vendor> probe` and `susi models probe` (vendor coding). Args: vendor, model, optional prompt",
        |arg, _workspace| providers("probe", arg, &[VENDOR, MODEL, ("prompt", Kind::Str)]);

        provider_models(provider_models_tool_attr, args::typed::<args::ProviderModels>()),
        "OpenRouter's curated routes, or the live model ids its key reaches, as `susi openrouter models`. Args: optional live, limit",
        |arg, _workspace| providers(
            "openrouter_models",
            arg,
            &[("live", Kind::Bool), ("limit", Kind::U64)],
        );

        keys_status(keys_status_tool_attr, args::no_args()),
        "Which cloud vendor keys are set (presence only, never values), the routing preference and the key file, as `susi keys list`",
        |arg, _workspace| providers("keys_status", arg, &[]);

        keys_prefer(keys_prefer_tool_attr, args::typed::<args::KeysPrefer>()),
        "Pin, clear or report the preferred cloud vendor, as `susi keys prefer`. Args: optional vendor, clear",
        |arg, _workspace| providers("keys_prefer", arg, &[VENDOR, ("clear", Kind::Bool)]);

        keys_check(keys_check_tool_attr, args::typed::<args::KeysCheck>()),
        "Each key's state against its vendor's model list (key values never returned), as `susi keys check`. Args: optional vendor",
        |arg, _workspace| providers("keys_check", arg, &[VENDOR]);

        keys_models(keys_models_tool_attr, args::typed::<args::KeysModels>()),
        "The models a vendor's valid key reaches, as `susi keys models`. Args: vendor",
        |arg, _workspace| providers("keys_models", arg, &[VENDOR]);

        brain_status(brain_status_tool_attr, args::typed::<args::BrainStatus>()),
        "Evidence-ranked providers per task class, as `susi brain status --json`. Args: optional top, class",
        |arg, _workspace| providers(
            "brain_status",
            arg,
            &[("top", Kind::U64), ("class", Kind::Str)],
        );

        brain_budget(brain_budget_tool_attr, args::typed::<args::BrainBudget>()),
        "Set or report the budget dial and shared spend policy, as `susi brain budget`. Args: optional level, free_only, allow_paid (explicit spend authorization), max_spend_microusd, max_tokens, max_requests, max_parallel, new_window",
        |arg, _workspace| providers(
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
        );

        brain_classify(brain_classify_tool_attr, args::typed::<args::BrainPrompt>()),
        "The task class a prompt routes as, as `susi brain classify`. Args: prompt",
        |arg, _workspace| providers("brain_classify", arg, &[("prompt", Kind::Str)]);

        brain_explain(brain_explain_tool_attr, args::typed::<args::BrainPrompt>()),
        "The brain's routing decision for a prompt, as `susi brain explain`. Args: prompt",
        |arg, _workspace| providers("brain_explain", arg, &[("prompt", Kind::Str)]);

        brain_export(brain_export_tool_attr, args::typed::<args::BrainExport>()),
        "The brain evidence bundle (or write it to an absolute path), as `susi brain export`. Args: optional out",
        |arg, _workspace| providers_with_paths("brain_export", arg, &[("out", Kind::Str)], &[("out", true)]);

        brain_import(brain_import_tool_attr, args::typed::<args::BrainImport>()),
        "Merge a brain evidence bundle from an absolute path, as `susi brain import`. Args: file",
        |arg, _workspace| providers_with_paths(
            "brain_import",
            arg,
            &[("file", Kind::Str)],
            &[("file", false)],
        );

        brain_matrix(brain_matrix_tool_attr, args::no_args()),
        "The measured capability matrix (success, latency and cost per verified outcome), as `susi brain matrix`",
        |arg, _workspace| providers("brain_matrix", arg, &[]);

        brain_cache(brain_cache_tool_attr, args::no_args()),
        "The measured prompt-cache hit rate and estimated saving, as `susi brain cache`",
        |arg, _workspace| providers("brain_cache", arg, &[]);

        brain_reset(brain_reset_tool_attr, args::no_args()),
        "Forget every recorded brain outcome, as `susi brain reset`",
        |arg, _workspace| providers("brain_reset", arg, &[]);

        brain_bench(brain_bench_tool_attr, args::typed::<args::BrainBench>()),
        "Run the fixed prompt set against the fake engine and record JSONL evidence, as `susi brain bench`. Args: optional out (absolute)",
        |arg, _workspace| providers_with_paths("brain_bench", arg, &[("out", Kind::Str)], &[("out", true)]);

        brain_probe(brain_probe_tool_attr, args::typed::<args::BrainProbe>()),
        "Probe a registered provider with the scout benchmark and record the verified outcomes, as `susi brain probe`. Args: provider, optional no_record",
        |arg, _workspace| providers(
            "brain_probe",
            arg,
            &[("provider", Kind::Str), ("no_record", Kind::Bool)],
        );
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

    forwarded_tools! {
        verify_download_agent(verify_download_agent_tool_attr, args::no_args()),
        "Network status and the hardware-selected model ladder's provisioning steps (all_verified is the verdict), as `susi verify-download-agent`; verification only, it never starts a download",
        |_arg, workspace| forward(
            daemon::providers,
            "verify_download_agent",
            &json!({}),
            &[],
            json!({ "workspace": workspace.display().to_string() }),
        );
    }
}

#[cfg(test)]
#[path = "../tests/provider_tools.rs"]
mod tests;
