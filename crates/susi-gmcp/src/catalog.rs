//! Built-in discovery resources and prompts. Providers are registered through
//! the same public API available to embedders.
use crate::{protocol::GmcpService, tools::ToolRegistry};
use rmcp::{model::*, ErrorData};
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
