with open('src/gemi/server.rs', 'r') as f:
    content = f.read()

import re

# Remove the tool dispatch hijack
old_logic = """                let clean_cmd = trimmed_prompt
                    .trim_start_matches('/')
                    .trim_start_matches(':')
                    .to_string();
                let parts: Vec<&str> = clean_cmd.splitn(2, ' ').collect();
                let tool_name = parts[0].to_lowercase();
                let tool_arg = parts.get(1).copied().unwrap_or("").trim().to_string();

                let ws = (*workspace).clone();
                let prompt_for_task = trimmed_prompt.clone();
                let content = tokio::task::spawn_blocking(move || {
                    if ToolRegistry::exists(&tool_name) {
                        ToolRegistry::execute_tool(&tool_name, &serde_json::json!(tool_arg), &ws)
                    } else {
                        let ama = SusiMasterAgent::new();
                        let final_resp =
                            ama.solve_clean(&prompt_for_task, &ws, crate::SUSI_VERSION);
                        crate::sandbox::manager::SusiMemory::save_interaction(
                            &ws,
                            &prompt_for_task,
                            &final_resp,
                        );
                        final_resp
                    }
                })"""

new_logic = """                let ws = (*workspace).clone();
                let prompt_for_task = trimmed_prompt.clone();
                let content = tokio::task::spawn_blocking(move || {
                    let ama = SusiMasterAgent::new();
                    let final_resp = ama.solve_clean(&prompt_for_task, &ws, crate::SUSI_VERSION);
                    crate::sandbox::manager::SusiMemory::save_interaction(
                        &ws,
                        &prompt_for_task,
                        &final_resp,
                    );
                    final_resp
                })"""

content = content.replace(old_logic, new_logic)

# Replace CORS wildcards
content = content.replace('HeaderValue::from_static("*")', 'HeaderValue::from_static("http://127.0.0.1")')

with open('src/gemi/server.rs', 'w') as f:
    f.write(content)

