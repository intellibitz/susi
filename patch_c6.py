import re

with open('src/gemi/models.rs', 'r') as f:
    code = f.read()

# 1. Fix HF_TOKEN exfiltration
old_hf_token = """        if let Ok(token) = std::env::var("HF_TOKEN") {
            if !token.trim().is_empty() {
                request = request.header("Authorization", format!("Bearer {}", token.trim()));
            }
        }"""
new_hf_token = """        if let Ok(token) = std::env::var("HF_TOKEN") {
            if !token.trim().is_empty() && (target.starts_with("https://huggingface.co/") || target.starts_with("https://cdn-lfs.huggingface.co/")) {
                request = request.header("Authorization", format!("Bearer {}", token.trim()));
            }
        }"""
code = code.replace(old_hf_token, new_hf_token)


# 2. Fix SSRF and Sandbox escape on `browser_automate` inside `src/gmcp/tools/mod.rs`
