import re

with open('src/gawd/agents.rs', 'r') as f:
    content = f.read()

old_code = """        let url = format!(
            "https://crates.io/api/v1/crates?q={}&per_page=5",
            query_term
        );"""

new_code = """        let url_mask = crate::sandbox::manager::SusiConfig::load_global().unwrap_or_default().get_string("crates_io_url", "https://crates.io/api/v1/crates?q={}&per_page=5");
        let url = url_mask.replace("{}", &query_term);"""

content = content.replace(old_code, new_code)

with open('src/gawd/agents.rs', 'w') as f:
    f.write(content)

