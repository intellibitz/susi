import re

with open('src/gemi/server.rs', 'r') as f:
    code = f.read()

bad_stream = """                                    if ToolRegistry::exists(&tool_name_clone) {
                                        let resp = ToolRegistry::execute_tool(
                                            &tool_name_clone,
                                            &serde_json::json!(tool_arg_clone),
                                            &w_clone,
                                        );
                                        let _ = chunk_tx.send(format!("data: {{\\"id\\":\\"chatcmpl-susi-{now}\\",\\"object\\":\\"chat.completion.chunk\\",\\"created\\":{now},\\"model\\":\\"{m_name}\\",\\"choices\\":[{{\\"index\\":0,\\"delta\\":{{\\"content\\":{json_resp}}},\\"finish_reason\\":null}}]}}\\n\\n", json_resp=serde_json::to_string(&resp).unwrap_or_default()));
                                    } else {
                                        let _ = crate::gemi::engine::GemiEngine::generate_reasoning_stream(&prompt_clone, &w_clone, &|piece| {
                                            let _ = chunk_tx.send(format!("data: {{\\"id\\":\\"chatcmpl-susi-{now}\\",\\"object\\":\\"chat.completion.chunk\\",\\"created\\":{now},\\"model\\":\\"{m_name}\\",\\"choices\\":[{{\\"index\\":0,\\"delta\\":{{\\"content\\":{json_piece}}},\\"finish_reason\\":null}}]}}\\n\\n", json_piece=serde_json::to_string(&piece).unwrap_or_default()));
                                        });
                                    }"""

good_stream = """                                    let ama = SusiMasterAgent::new();
                                    let _ = ama.solve_stream(&prompt_clone, &w_clone, crate::SUSI_VERSION, &|piece| {
                                        let _ = chunk_tx.send(format!("data: {{\\"id\\":\\"chatcmpl-susi-{now}\\",\\"object\\":\\"chat.completion.chunk\\",\\"created\\":{now},\\"model\\":\\"{m_name}\\",\\"choices\\":[{{\\"index\\":0,\\"delta\\":{{\\"content\\":{json_piece}}},\\"finish_reason\\":null}}]}}\\n\\n", json_piece=serde_json::to_string(&piece).unwrap_or_default()));
                                    });"""

code = code.replace(bad_stream, good_stream)

bad_non_stream = """                                let content = if ToolRegistry::exists(&tool_name) {
                                    ToolRegistry::execute_tool(
                                        &tool_name,
                                        &serde_json::json!(tool_arg),
                                        &w_thread,
                                    )
                                } else {
                                    let ama = SusiMasterAgent::new();
                                    let final_resp =
                                        ama.solve_clean(trimmed_prompt, &w_thread, crate::SUSI_VERSION);
                                    crate::sandbox::manager::SusiMemory::save_interaction(
                                        &w_thread,
                                        trimmed_prompt,
                                        &final_resp,
                                    );
                                    final_resp
                                };"""
                                
good_non_stream = """                                let ama = SusiMasterAgent::new();
                                let content = ama.solve_clean(trimmed_prompt, &w_thread, crate::SUSI_VERSION);"""

code = code.replace(bad_non_stream, good_non_stream)

with open('src/gemi/server.rs', 'w') as f:
    f.write(code)

with open('src/gmcp/server.rs', 'r') as f:
    gmcp_code = f.read()

bad_gmcp = """                // Fully Meta Dispatch via ToolRegistry
                let result_text = ToolRegistry::execute_tool(&tool_name, &tool_arg, workspace);"""

good_gmcp = """                // SUSI Is Swarm: Route all GMCP operations through the Substrate Master Agent
                let intent = format!("{} {}", tool_name, tool_arg);
                let ama = crate::gawd::ama::SusiMasterAgent::new();
                let result_text = ama.solve_clean(&intent, workspace, crate::SUSI_VERSION);"""

gmcp_code = gmcp_code.replace(bad_gmcp, good_gmcp)

with open('src/gmcp/server.rs', 'w') as f:
    f.write(gmcp_code)

