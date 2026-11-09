//! Built-in discovery resources and prompts. Providers are registered through
//! the same public API available to embedders.
use crate::mcp::{model::*, ErrorData};
use crate::{protocol::GmcpService, tools::ToolRegistry};
use serde_json::json;
use std::sync::Arc;

pub fn install(service: &GmcpService) -> Result<(), ErrorData> {
    service.register_resource(Resource::new("susi://tools", "tools").with_mime_type("application/json"), Arc::new(|request, _| Box::pin(async move {
        let content = serde_json::to_string(&ToolRegistry::list_tools()).map_err(internal)?;
        let result: ReadResourceResult = serde_json::from_value(json!({"contents":[{"uri":request.uri,"mimeType":"application/json","text":content}],"ttlMs":0,"cacheScope":"private"})).map_err(internal)?;
        Ok(result.into())
    })));
    let template = serde_json::from_value(
        json!({"uriTemplate":"susi://tools/{name}", "name":"tool", "mimeType":"application/json"}),
    )
    .map_err(internal)?;
    service.register_template(template, Arc::new(|request, _| Box::pin(async move {
        let tool = request.uri.strip_prefix("susi://tools/").and_then(|name| ToolRegistry::list_tools().into_iter().find(|t| t.name == name));
        let tool = tool.ok_or_else(|| ErrorData::resource_not_found("Unknown tool resource", None))?;
        let result: ReadResourceResult = serde_json::from_value(json!({"contents":[{"uri":request.uri,"mimeType":"application/json","text":serde_json::to_string(&tool).map_err(internal)?}],"ttlMs":0,"cacheScope":"private"})).map_err(internal)?;
        Ok(result.into())
    })));
    let prompt = serde_json::from_value(json!({"name":"use_tool","description":"Prepare a request using a SUSI tool", "arguments":[{"name":"name","description":"Tool name","required":true},{"name":"goal","description":"What to accomplish","required":true}]})).map_err(internal)?;
    service.register_prompt(prompt, Arc::new(|request, _| Box::pin(async move {
        let args = request.arguments.unwrap_or_default();
        let name = args.get("name").and_then(|v| v.as_str()).ok_or_else(|| ErrorData::invalid_params("name must be a string", None))?;
        if !ToolRegistry::list_tools().iter().any(|tool| tool.name == name) { return Err(ErrorData::invalid_params("Unknown tool", None)); }
        let goal = args.get("goal").and_then(|v| v.as_str()).ok_or_else(|| ErrorData::invalid_params("goal must be a string", None))?;
        let result: GetPromptResult = serde_json::from_value(json!({"messages":[{"role":"user","content":{"type":"text","text":format!("Use tool {name} to accomplish the following goal: {goal}")}}]})).map_err(internal)?;
        Ok(result.into())
    })));
    let completion: crate::protocol::CompletionHandler = Arc::new(|request, _| {
        Box::pin(async move {
            if request.argument.name != "name" {
                return Ok(CompleteResult::default());
            }
            let names: Vec<_> = ToolRegistry::list_tools()
                .into_iter()
                .map(|t| t.name)
                .filter(|n| n.starts_with(&request.argument.value))
                .collect();
            let total = names.len();
            serde_json::from_value(json!({"completion":{"values":names.into_iter().take(100).collect::<Vec<_>>(),"total":total,"hasMore":total>100}})).map_err(internal)
        })
    });
    service.register_completion("use_tool".into(), completion.clone());
    service.register_completion("susi://tools/{name}".into(), completion);
    Ok(())
}
fn internal(e: impl std::fmt::Display) -> ErrorData {
    ErrorData::internal_error(e.to_string(), None)
}

#[cfg(test)]
mod inline_tests {
    use super::*;

    #[test]
    fn internal_helper_builds_internal_error_data_with_message() {
        let err = internal("catalog serialization failure");
        assert!(err.message.contains("catalog serialization failure"));
        assert_eq!(err.code, ErrorCode::INTERNAL_ERROR);
    }

    #[test]
    fn install_registers_builtin_resources_templates_prompts_and_completions() {
        let ws = std::env::temp_dir().join("susi-gmcp-catalog-install");
        let service = GmcpService::new(ws);
        install(&service).unwrap();

        let discover = service.discover_result();
        let wire = serde_json::to_value(&discover).unwrap();
        assert!(wire.to_string().contains("susi-gmcp"));
    }

    #[test]
    fn install_is_idempotent_across_repeated_invocations_on_same_service() {
        let ws = std::env::temp_dir().join("susi-gmcp-catalog-idempotent");
        let service = GmcpService::new(ws);
        install(&service).unwrap();
        install(&service).unwrap();
    }

    #[test]
    fn session_clone_shares_installed_catalog_across_sessions() {
        let ws = std::env::temp_dir().join("susi-gmcp-catalog-session");
        let service = GmcpService::new(ws);
        install(&service).unwrap();
        let session = service.session();
        assert_eq!(
            serde_json::to_value(service.discover_result()).unwrap(),
            serde_json::to_value(session.discover_result()).unwrap()
        );
    }

    #[test]
    fn install_populates_discover_capabilities_for_resources_and_prompts() {
        let ws = std::env::temp_dir().join("susi-gmcp-catalog-caps");
        let service = GmcpService::new(ws);
        install(&service).unwrap();
        let wire = serde_json::to_value(service.discover_result()).unwrap();
        assert!(wire["capabilities"]["resources"].is_object());
        assert!(wire["capabilities"]["prompts"].is_object());
    }
}
