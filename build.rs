use std::env;
use std::fs;
use std::path::Path;

fn main() {
    let out_dir = env::var_os("OUT_DIR").unwrap();
    let dest_path = Path::new(&out_dir).join("generated_axioms.rs");

    let identity_md_raw = fs::read_to_string(".agents/IDENTITY.md").expect("Missing IDENTITY.md");
    let roadmap_md_raw = fs::read_to_string(".agents/ROADMAP.md").expect("Missing ROADMAP.md");
    let evidence_md_raw = fs::read_to_string(".agents/EVIDENCE.md").expect("Missing EVIDENCE.md");
    let readme_md_raw = fs::read_to_string("README.md").unwrap_or_default();

    // SUSI Version Synchronization Hook
    let cargo_toml = fs::read_to_string("Cargo.toml").expect("Missing Cargo.toml");
    let version = cargo_toml.lines()
        .find(|l| l.trim().starts_with("version = \""))
        .and_then(|l| l.split('"').nth(1))
        .expect("Could not find version in Cargo.toml");

    let identity_md = sync_version(".agents/IDENTITY.md", &identity_md_raw, version);
    let roadmap_md = sync_version(".agents/ROADMAP.md", &roadmap_md_raw, version);
    let evidence_md = sync_version(".agents/EVIDENCE.md", &evidence_md_raw, version);

    // Sync README badge
    if readme_md_raw.contains("https://img.shields.io/badge/version-v") {
        let mut updated = Vec::new();
        for line in readme_md_raw.lines() {
            if line.contains("https://img.shields.io/badge/version-v") {
                updated.push(format!("![SUSI Version](https://img.shields.io/badge/version-v{}-blue.svg) ![License](https://img.shields.io/badge/license-Apache%202.0-green.svg)", version));
            } else {
                updated.push(line.to_string());
            }
        }
        let new_readme = updated.join("\n") + "\n";
        if new_readme != readme_md_raw {
            fs::write("README.md", new_readme).ok();
        }
    }

    let mut generated_code = String::new();

    // 1. IDENTITY.md (Constitutional Mandates) -> GEN_AGENT_RULES (1-49)
    generated_code.push_str("pub const GEN_AGENT_RULES: &[SusiAxiomRule] = &[\n");
    let mut active_section = "";
    for line in identity_md.lines() {
        if line.starts_with("## 1. Constitutional Mandates") { active_section = "constitutional"; }
        else if line.starts_with("## 2. Substrate Topology") { active_section = "topology"; }
        else if line.starts_with("## 3. Realized Architectural Capabilities") { active_section = "topology"; }
        else if line.starts_with("## 4. Operational Workflow") { active_section = "topology"; }
        else if line.starts_with("## 5. Build & Deployment Protocols") { active_section = "build"; }

        if active_section == "constitutional" {
            if let Some(rule) = parse_list_item(line) {
                generated_code.push_str(&format!("    SusiAxiomRule {{ id: {}, title: {:?}, imperative: {:?} }},\n", rule.0, rule.1, rule.2));
            }
        }
    }
    generated_code.push_str("];\n\n");

    // 2. ROADMAP.md -> GEN_ENGINE_AXIOMS (50-99)
    generated_code.push_str("pub const GEN_ENGINE_AXIOMS: &[SusiAxiomRule] = &[\n");
    let mut current_id = None;
    let mut current_title = None;
    for line in roadmap_md.lines() {
        if line.starts_with("### [Aspiration") {
            if let Some(caps) = parse_aspiration_header(line) {
                current_id = Some(caps.0);
                current_title = Some(caps.1);
            }
        } else if line.trim().starts_with("* **Core Paradigm**:") {
            if let (Some(id), Some(title)) = (current_id, current_title.take()) {
                let paradigm = line.split_once(':').unwrap().1.trim();
                generated_code.push_str(&format!("    SusiAxiomRule {{ id: {}, title: {:?}, imperative: {:?} }},\n", id + 50, title, paradigm));
                current_id = None;
            }
        }
    }
    generated_code.push_str("];\n\n");

    // 3. IDENTITY.md (Build & Deployment Protocols) -> GEN_DEPLOYMENT_RULES (100-149)
    generated_code.push_str("pub const GEN_DEPLOYMENT_RULES: &[SusiAxiomRule] = &[\n");
    active_section = "";
    for line in identity_md.lines() {
        if line.starts_with("## 1. Constitutional Mandates") { active_section = "constitutional"; }
        else if line.starts_with("## 2. Substrate Topology") { active_section = "topology"; }
        else if line.starts_with("## 3. Realized Architectural Capabilities") { active_section = "topology"; }
        else if line.starts_with("## 4. Operational Workflow") { active_section = "topology"; }
        else if line.starts_with("## 5. Build & Deployment Protocols") { active_section = "build"; }

        if active_section == "build" {
            if let Some(rule) = parse_list_item(line) {
                generated_code.push_str(&format!("    SusiAxiomRule {{ id: {}, title: {:?}, imperative: {:?} }},\n", rule.0 + 100, rule.1, rule.2));
            }
        }
    }
    generated_code.push_str("];\n\n");

    // 5. EVIDENCE.md -> GEN_PULSE_AXIOMS (150-299)
    generated_code.push_str("pub const GEN_PULSE_AXIOMS: &[SusiAxiomRule] = &[\n");
    for line in evidence_md.lines() {
        let line = line.trim();
        if line.starts_with('|') && line.contains("EV-") {
            let parts: Vec<&str> = line.split('|').map(|s| s.trim()).collect();
            if parts.len() >= 6 {
                let id_str = parts[1];
                let seq_str = id_str.split('-').last().unwrap_or("0");
                let seq: usize = seq_str.parse().unwrap_or(0);
                if seq > 0 {
                    let title = format!("{} [{}]", parts[3], parts[2]);
                    let imperative = format!("Anchor: {}. Proof: {}", parts[4], parts[5]);
                    generated_code.push_str(&format!("    SusiAxiomRule {{ id: {}, title: {:?}, imperative: {:?} }},\n", seq + 150, title, imperative));
                }
            }
        }
    }
    generated_code.push_str("];\n\n");

    // 8. IDENTITY.md -> Pillar-based Components
    let mut current_pillar = "";
    generated_code.push_str("pub const GEN_AOA_COMPONENTS: &[SusiComponentSpec] = &[\n");
    let mut agents = String::from("pub const GEN_AGENT_COMPONENTS: &[SusiComponentSpec] = &[\n");
    let mut engines = String::from("pub const GEN_ENGINE_COMPONENTS: &[SusiComponentSpec] = &[\n");
    let mut models = String::from("pub const GEN_MODEL_COMPONENTS: &[SusiComponentSpec] = &[\n");
    let mut mcps = String::from("pub const GEN_MCP_COMPONENTS: &[SusiComponentSpec] = &[\n");
    let mut realized = String::from("pub const GEN_REALIZED_COMPONENTS: &[SusiComponentSpec] = &[\n");

    for line in identity_md.lines() {
        if line.starts_with("### 2.1 Agent of Agents") { current_pillar = "aoa"; }
        else if line.starts_with("### 2.2 Agents") { current_pillar = "agents"; }
        else if line.starts_with("### 2.3 Engines") { current_pillar = "engines"; }
        else if line.starts_with("### 2.4 Models") { current_pillar = "models"; }
        else if line.starts_with("### 2.5 MCPs") { current_pillar = "mcps"; }
        else if line.starts_with("## 3. Realized Architectural Capabilities") { current_pillar = "realized"; }
        else if line.starts_with("## 4. Operational Workflow") { current_pillar = "workflow"; }

        if current_pillar != "workflow" && current_pillar != "" {
            if let Some(comp) = parse_topology_item(line) {
                let tier = match comp.2.as_str() {
                    "0" => "SusiCoreTier::Tier0Reflex",
                    "2" => "SusiCoreTier::Tier2Reasoning",
                    _ => "SusiCoreTier::Tier1Swarm",
                };
                let entry = format!("    SusiComponentSpec {{ name: {:?}, tier: {}, description: {:?} }},\n", comp.0, tier, comp.1);
                match current_pillar {
                    "aoa" => generated_code.push_str(&entry),
                    "agents" => agents.push_str(&entry),
                    "engines" => engines.push_str(&entry),
                    "models" => models.push_str(&entry),
                    "mcps" => mcps.push_str(&entry),
                    "realized" => realized.push_str(&entry),
                    _ => {}
                }
            }
        }
    }
    generated_code.push_str("];\n\n");
    agents.push_str("];\n\n");
    engines.push_str("];\n\n");
    models.push_str("];\n\n");
    mcps.push_str("];\n\n");
    realized.push_str("];\n\n");
    generated_code.push_str(&agents);
    generated_code.push_str(&engines);
    generated_code.push_str(&models);
    generated_code.push_str(&mcps);
    generated_code.push_str(&realized);

    // Combined COMPONENTS for legacy support
    generated_code.push_str("pub const GEN_COMPONENTS: &[SusiComponentSpec] = &[\n");
    current_pillar = "";
    for line in identity_md.lines() {
        if line.starts_with("### 2.1 Agent of Agents") { current_pillar = "aoa"; }
        else if line.starts_with("### 2.2 Agents") { current_pillar = "agents"; }
        else if line.starts_with("### 2.3 Engines") { current_pillar = "engines"; }
        else if line.starts_with("### 2.4 Models") { current_pillar = "models"; }
        else if line.starts_with("### 2.5 MCPs") { current_pillar = "mcps"; }
        else if line.starts_with("## 3. Realized Architectural Capabilities") { current_pillar = "realized"; }
        else if line.starts_with("## 4. Operational Workflow") { current_pillar = "workflow"; }

        if current_pillar != "workflow" && current_pillar != "" {
            if let Some(comp) = parse_topology_item(line) {
                let tier = match comp.2.as_str() {
                    "0" => "SusiCoreTier::Tier0Reflex",
                    "2" => "SusiCoreTier::Tier2Reasoning",
                    _ => "SusiCoreTier::Tier1Swarm",
                };
                generated_code.push_str(&format!("    SusiComponentSpec {{ name: {:?}, tier: {}, description: {:?} }},\n", comp.0, tier, comp.1));
            }
        }
    }
    generated_code.push_str("];\n\n");

    // 9. Unified RULES List
    generated_code.push_str("pub const GEN_RULES: &[SusiAxiomRule] = &[\n");
    active_section = "";
    for line in identity_md.lines() {
        if line.starts_with("## 1. Constitutional Mandates") { active_section = "constitutional"; }
        else if line.starts_with("## 2. Substrate Topology") { active_section = "topology"; }
        else if line.starts_with("## 3. Realized Architectural Capabilities") { active_section = "topology"; }
        else if line.starts_with("## 4. Operational Workflow") { active_section = "topology"; }
        else if line.starts_with("## 5. Build & Deployment Protocols") { active_section = "build"; }

        if active_section == "constitutional" {
            if let Some(rule) = parse_list_item(line) {
                generated_code.push_str(&format!("    SusiAxiomRule {{ id: {}, title: {:?}, imperative: {:?} }},\n", rule.0, rule.1, rule.2));
            }
        }
    }
    let mut current_id = None;
    let mut current_title = None;
    for line in roadmap_md.lines() {
        if line.starts_with("### [Aspiration") {
            if let Some(caps) = parse_aspiration_header(line) {
                current_id = Some(caps.0);
                current_title = Some(caps.1);
            }
        } else if line.trim().starts_with("* **Core Paradigm**:") {
            if let (Some(id), Some(title)) = (current_id, current_title.take()) {
                let paradigm = line.split_once(':').unwrap().1.trim();
                generated_code.push_str(&format!("    SusiAxiomRule {{ id: {}, title: {:?}, imperative: {:?} }},\n", id + 50, title, paradigm));
                current_id = None;
            }
        }
    }
    active_section = "";
    for line in identity_md.lines() {
        if line.starts_with("## 1. Constitutional Mandates") { active_section = "constitutional"; }
        else if line.starts_with("## 2. Substrate Topology") { active_section = "topology"; }
        else if line.starts_with("## 3. Realized Architectural Capabilities") { active_section = "topology"; }
        else if line.starts_with("## 4. Operational Workflow") { active_section = "topology"; }
        else if line.starts_with("## 5. Build & Deployment Protocols") { active_section = "build"; }

        if active_section == "build" {
            if let Some(rule) = parse_list_item(line) {
                generated_code.push_str(&format!("    SusiAxiomRule {{ id: {}, title: {:?}, imperative: {:?} }},\n", rule.0 + 100, rule.1, rule.2));
            }
        }
    }
    for line in evidence_md.lines() {
        let line = line.trim();
        if line.starts_with('|') && line.contains("EV-") {
            let parts: Vec<&str> = line.split('|').map(|s| s.trim()).collect();
            if parts.len() >= 6 {
                let id_str = parts[1];
                let seq_str = id_str.split('-').last().unwrap_or("0");
                let seq: usize = seq_str.parse().unwrap_or(0);
                if seq > 0 {
                    let title = format!("{} [{}]", parts[3], parts[2]);
                    let imperative = format!("Anchor: {}. Proof: {}", parts[4], parts[5]);
                    generated_code.push_str(&format!("    SusiAxiomRule {{ id: {}, title: {:?}, imperative: {:?} }},\n", seq + 150, title, imperative));
                }
            }
        }
    }
    generated_code.push_str("];\n");

    fs::write(&dest_path, generated_code).unwrap();

    println!("cargo:rerun-if-changed=.agents/IDENTITY.md");
    println!("cargo:rerun-if-changed=.agents/ROADMAP.md");
    println!("cargo:rerun-if-changed=.agents/EVIDENCE.md");
}

fn parse_list_item(line: &str) -> Option<(usize, String, String)> {
    let line = line.trim();
    if line.is_empty() || !line.chars().next().unwrap().is_ascii_digit() { return None; }
    let parts: Vec<&str> = line.splitn(2, '.').collect();
    if parts.len() < 2 { return None; }
    let id: usize = parts[0].parse().ok()?;
    let content = parts[1].trim();

    if content.starts_with("**") {
        let sub_parts: Vec<&str> = content.splitn(2, ':').collect();
        if sub_parts.len() < 2 { return None; }
        let title = sub_parts[0].trim_matches('*').trim();
        let imperative = sub_parts[1].trim();
        return Some((id, title.to_string(), imperative.to_string()));
    }

    let sub_parts: Vec<&str> = content.splitn(2, ':').collect();
    if sub_parts.len() >= 2 {
        let title = sub_parts[0].trim();
        let imperative = sub_parts[1].trim();
        return Some((id, title.to_string(), imperative.to_string()));
    }

    None
}

fn parse_aspiration_header(line: &str) -> Option<(usize, String)> {
    let line = line.trim_start_matches('#').trim();
    if !line.starts_with("[Aspiration") { return None; }
    let parts: Vec<&str> = line.splitn(2, ']').collect();
    if parts.len() < 2 { return None; }
    let id: usize = parts[0].trim_start_matches("[Aspiration").trim().parse().ok()?;
    let title = parts[1].trim();
    Some((id, title.to_string()))
}

fn parse_topology_item(line: &str) -> Option<(String, String, String)> {
    let line = line.trim();
    if line.is_empty() || !line.chars().next().unwrap().is_ascii_digit() { return None; }
    let parts: Vec<&str> = line.splitn(2, '.').collect();
    if parts.len() < 2 { return None; }
    let content = parts[1].trim();
    let sub_parts: Vec<&str> = content.splitn(2, ':').collect();
    if sub_parts.len() < 2 { return None; }
    let name = sub_parts[0].trim_matches('*').trim();
    let desc_tier: Vec<&str> = sub_parts[1].splitn(2, "(Tier:").collect();
    let description = desc_tier[0].trim();
    let tier = if desc_tier.len() > 1 { desc_tier[1].trim_end_matches(')').trim() } else { "1" };
    Some((name.to_string(), description.to_string(), tier.to_string()))
}

fn sync_version(path: &str, content: &str, version: &str) -> String {
    let mut updated = Vec::new();
    let mut changed = false;
    for line in content.lines() {
        if line.trim().starts_with("* **Current Engine Version**: `v") {
            let new_line = format!("* **Current Engine Version**: `v{}`", version);
            if new_line != line.trim() {
                updated.push(new_line);
                changed = true;
            } else {
                updated.push(line.to_string());
            }
        } else {
            updated.push(line.to_string());
        }
    }
    let result = updated.join("\n") + "\n";
    if changed {
        fs::write(path, &result).ok();
    }
    result
}
