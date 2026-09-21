use std::env;
use std::fs;
use std::path::Path;

fn main() {
    let out_dir = env::var_os("OUT_DIR").unwrap();
    let dest_path = Path::new(&out_dir).join("generated_axioms.rs");

    let identity_md_raw =
        fs::read_to_string("../../.agents/IDENTITY.md").expect("Missing IDENTITY.md");
    let roadmap_md_raw =
        fs::read_to_string("../../.agents/ROADMAP.md").expect("Missing ROADMAP.md");
    let evidence_md_raw =
        fs::read_to_string("../../.agents/EVIDENCE.md").expect("Missing EVIDENCE.md");
    let readme_md_raw = fs::read_to_string("../../README.md").unwrap_or_default();

    // SUSI Version Synchronization Hook
    let cargo_toml = fs::read_to_string("../../Cargo.toml").expect("Missing Cargo.toml");
    let version = cargo_toml
        .lines()
        .find(|l| l.trim().starts_with("version = \""))
        .and_then(|l| l.split('"').nth(1))
        .expect("Could not find version in Cargo.toml");

    let identity_md = skip_frontmatter(&sync_version(
        "../../.agents/IDENTITY.md",
        &identity_md_raw,
        version,
    ));
    let roadmap_md = skip_frontmatter(&sync_version(
        "../../.agents/ROADMAP.md",
        &roadmap_md_raw,
        version,
    ));
    let evidence_md = skip_frontmatter(&sync_version(
        "../../.agents/EVIDENCE.md",
        &evidence_md_raw,
        version,
    ));

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
            fs::write("../../README.md", new_readme).ok();
        }
    }

    let mut generated_code = String::new();

    // Workspace engine version (root Cargo.toml), not this crate's 0.1.0.
    generated_code.push_str(&format!(
        "pub const GEN_ENGINE_VERSION: &str = {:?};\n\n",
        version
    ));

    // 1. IDENTITY.md (Constitutional Mandates) -> GEN_AGENT_RULES (1-49)
    generated_code.push_str("pub const GEN_AGENT_RULES: &[SusiAxiomRule] = &[\n");
    let mut active_section = "";
    for line in identity_md.lines() {
        if line.starts_with("## 1. Pillar I: THE DNA") {
            active_section = "constitutional";
        } else if line.starts_with("## 2. Pillar II: THE BODY") {
            active_section = "topology";
        } else if line.starts_with("## 3. Pillar III: THE MIND") {
            active_section = "mind";
        } else if line.starts_with("## 4. Pillar IV: THE ENGINE") {
            active_section = "build";
        }

        if active_section == "constitutional" {
            if let Some(rule) = parse_list_item(line) {
                generated_code.push_str(&format!(
                    "    SusiAxiomRule {{ id: {}, title: {:?}, imperative: {:?} }},\n",
                    rule.0, rule.1, rule.2
                ));
            }
        }
    }
    generated_code.push_str("];\n\n");

    // 2. ROADMAP.md -> GEN_ENGINE_AXIOMS (50-99)
    generated_code.push_str("pub const GEN_ENGINE_AXIOMS: &[SusiAxiomRule] = &[\n");
    for line in roadmap_md.lines() {
        let line = line.trim();
        if line.starts_with('|') && line.contains("VC-") {
            let parts: Vec<&str> = line.split('|').map(|s| s.trim()).collect();
            if parts.len() >= 5 {
                let id_str = parts[1];
                let seq_str = id_str.split('-').next_back().unwrap_or("0");
                let seq: usize = seq_str.parse().unwrap_or(0);
                if seq > 0 {
                    let title = format!("{} {}", parts[3], parts[2]);
                    let imperative = parts[4].to_string();
                    generated_code.push_str(&format!(
                        "    SusiAxiomRule {{ id: {}, title: {:?}, imperative: {:?} }},\n",
                        seq + 50,
                        title,
                        imperative
                    ));
                }
            }
        }
    }
    generated_code.push_str("];\n\n");

    // 3. IDENTITY.md (Build & Deployment Protocols) -> GEN_DEPLOYMENT_RULES (100-149)
    generated_code.push_str("pub const GEN_DEPLOYMENT_RULES: &[SusiAxiomRule] = &[\n");
    active_section = "";
    for line in identity_md.lines() {
        if line.starts_with("## 1. Pillar I: THE DNA") {
            active_section = "constitutional";
        } else if line.starts_with("## 2. Pillar II: THE BODY") {
            active_section = "topology";
        } else if line.starts_with("## 3. Pillar III: THE MIND") {
            active_section = "mind";
        } else if line.starts_with("## 4. Pillar IV: THE ENGINE") {
            active_section = "build";
        }

        if active_section == "build" {
            if let Some(rule) = parse_list_item(line) {
                generated_code.push_str(&format!(
                    "    SusiAxiomRule {{ id: {}, title: {:?}, imperative: {:?} }},\n",
                    rule.0 + 100,
                    rule.1,
                    rule.2
                ));
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
                let seq_str = id_str.split('-').next_back().unwrap_or("0");
                let seq: usize = seq_str.parse().unwrap_or(0);
                if seq > 0 {
                    let title = format!("{} [{}]", parts[3], parts[2]);
                    let imperative = format!("Anchor: {}. Proof: {}", parts[4], parts[5]);
                    generated_code.push_str(&format!(
                        "    SusiAxiomRule {{ id: {}, title: {:?}, imperative: {:?} }},\n",
                        seq + 150,
                        title,
                        imperative
                    ));
                }
            }
        }
    }
    generated_code.push_str("];\n\n");

    // 8. IDENTITY.md -> Pillar-based Components
    active_section = "";
    generated_code.push_str("pub const GEN_AOA_COMPONENTS: &[SusiComponentSpec] = &[\n");
    let mut agents = String::from("pub const GEN_AGENT_COMPONENTS: &[SusiComponentSpec] = &[\n");
    let mut engines = String::from("pub const GEN_ENGINE_COMPONENTS: &[SusiComponentSpec] = &[\n");
    let mut models = String::from("pub const GEN_MODEL_COMPONENTS: &[SusiComponentSpec] = &[\n");
    let mut mcps = String::from("pub const GEN_MCP_COMPONENTS: &[SusiComponentSpec] = &[\n");
    let mut realized =
        String::from("pub const GEN_REALIZED_COMPONENTS: &[SusiComponentSpec] = &[\n");

    for line in identity_md.lines() {
        if line.starts_with("## 1. Pillar I: THE DNA") {
            active_section = "constitutional";
        } else if line.starts_with("## 2. Pillar II: THE BODY") {
            active_section = "topology";
        } else if line.starts_with("## 3. Pillar III: THE MIND") {
            active_section = "mind";
        } else if line.starts_with("## 4. Pillar IV: THE ENGINE") {
            active_section = "build";
        }

        if active_section == "topology" {
            if let Some(comp) = parse_table_row(line) {
                let tier = match comp.2.as_str() {
                    "0" => "SusiCoreTier::Tier0Reflex",
                    "2" => "SusiCoreTier::Tier2Reasoning",
                    _ => "SusiCoreTier::Tier1Swarm",
                };
                let entry = format!(
                    "    SusiComponentSpec {{ name: {:?}, tier: {}, description: {:?} }},\n",
                    comp.0, tier, comp.1
                );

                // Heuristic categorization for legacy compatibility
                let name_lower = comp.0.to_lowercase();
                if name_lower.contains("gawd")
                    || name_lower.contains("admin")
                    || name_lower.contains("loader")
                    || name_lower.contains("daemon")
                    || name_lower.contains("evolutionmanager")
                {
                    generated_code.push_str(&entry); // AOA
                } else if name_lower.contains("agent")
                    || name_lower.contains("factory")
                    || name_lower.contains("scout")
                {
                    agents.push_str(&entry);
                } else if name_lower.contains("susi-")
                    || name_lower.contains("engine")
                    || name_lower.contains("substrate")
                    || name_lower.contains("gemi")
                    || name_lower.contains("synthesizer")
                {
                    if name_lower.contains("model") {
                        models.push_str(&entry);
                    } else {
                        engines.push_str(&entry);
                    }
                } else if name_lower.contains("mcp")
                    || name_lower.contains("server")
                    || name_lower.contains("host")
                    || name_lower.contains("evidence")
                {
                    mcps.push_str(&entry);
                } else {
                    realized.push_str(&entry);
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
    active_section = "";
    for line in identity_md.lines() {
        if line.starts_with("## 1. Pillar I: THE DNA") {
            active_section = "constitutional";
        } else if line.starts_with("## 2. Pillar II: THE BODY") {
            active_section = "topology";
        } else if line.starts_with("## 4. Pillar IV: THE ENGINE") {
            active_section = "build";
        }

        if active_section == "topology" {
            if let Some(comp) = parse_table_row(line) {
                let tier = match comp.2.as_str() {
                    "0" => "SusiCoreTier::Tier0Reflex",
                    "2" => "SusiCoreTier::Tier2Reasoning",
                    _ => "SusiCoreTier::Tier1Swarm",
                };
                generated_code.push_str(&format!(
                    "    SusiComponentSpec {{ name: {:?}, tier: {}, description: {:?} }},\n",
                    comp.0, tier, comp.1
                ));
            }
        }
    }
    generated_code.push_str("];\n\n");

    // 9. Unified RULES List
    generated_code.push_str("pub const GEN_RULES: &[SusiAxiomRule] = &[\n");
    active_section = "";
    for line in identity_md.lines() {
        if line.starts_with("## 1. Pillar I: THE DNA") {
            active_section = "constitutional";
        } else if line.starts_with("## 4. Pillar IV: THE ENGINE") {
            active_section = "build";
        }

        if active_section == "constitutional" {
            if let Some(rule) = parse_list_item(line) {
                generated_code.push_str(&format!(
                    "    SusiAxiomRule {{ id: {}, title: {:?}, imperative: {:?} }},\n",
                    rule.0, rule.1, rule.2
                ));
            }
        }
    }
    for line in roadmap_md.lines() {
        let line = line.trim();
        if line.starts_with('|') && line.contains("VC-") {
            let parts: Vec<&str> = line.split('|').map(|s| s.trim()).collect();
            if parts.len() >= 5 {
                let id_str = parts[1];
                let seq_str = id_str.split('-').next_back().unwrap_or("0");
                let seq: usize = seq_str.parse().unwrap_or(0);
                if seq > 0 {
                    let title = format!("{} {}", parts[3], parts[2]);
                    let imperative = parts[4].to_string();
                    generated_code.push_str(&format!(
                        "    SusiAxiomRule {{ id: {}, title: {:?}, imperative: {:?} }},\n",
                        seq + 50,
                        title,
                        imperative
                    ));
                }
            }
        }
    }
    active_section = "";
    for line in identity_md.lines() {
        if line.starts_with("## 1. Pillar I: THE DNA") {
            active_section = "constitutional";
        } else if line.starts_with("## 4. Pillar IV: THE ENGINE") {
            active_section = "build";
        }

        if active_section == "build" {
            if let Some(rule) = parse_list_item(line) {
                generated_code.push_str(&format!(
                    "    SusiAxiomRule {{ id: {}, title: {:?}, imperative: {:?} }},\n",
                    rule.0 + 100,
                    rule.1,
                    rule.2
                ));
            }
        }
    }
    for line in evidence_md.lines() {
        let line = line.trim();
        if line.starts_with('|') && line.contains("EV-") {
            let parts: Vec<&str> = line.split('|').map(|s| s.trim()).collect();
            if parts.len() >= 6 {
                let id_str = parts[1];
                let seq_str = id_str.split('-').next_back().unwrap_or("0");
                let seq: usize = seq_str.parse().unwrap_or(0);
                if seq > 0 {
                    let title = format!("{} [{}]", parts[3], parts[2]);
                    let imperative = format!("Anchor: {}. Proof: {}", parts[4], parts[5]);
                    generated_code.push_str(&format!(
                        "    SusiAxiomRule {{ id: {}, title: {:?}, imperative: {:?} }},\n",
                        seq + 150,
                        title,
                        imperative
                    ));
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
    if line.is_empty() || !line.chars().next().unwrap().is_ascii_digit() {
        return None;
    }
    let parts: Vec<&str> = line.splitn(2, '.').collect();
    if parts.len() < 2 {
        return None;
    }
    let id: usize = parts[0].parse().ok()?;
    let content = parts[1].trim();

    if content.starts_with("**") {
        let sub_parts: Vec<&str> = content.splitn(2, ':').collect();
        if sub_parts.len() < 2 {
            return None;
        }
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

fn parse_table_row(line: &str) -> Option<(String, String, String)> {
    let line = line.trim();
    if !line.starts_with('|') || line.contains("Symbol | Tier") || line.contains(":---") {
        return None;
    }
    let parts: Vec<&str> = line.split('|').map(|s| s.trim()).collect();
    if parts.len() >= 4 {
        let symbol = parts[1].trim_matches('*').trim().to_string();
        let tier = parts[2].to_string();
        let function = parts[3].to_string();
        return Some((symbol, function, tier));
    }
    None
}

fn sync_version(path: &str, content: &str, version: &str) -> String {
    let mut updated = Vec::new();
    let mut changed = false;
    let mut in_frontmatter = false;
    let mut frontmatter_count = 0;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed == "---" {
            frontmatter_count += 1;
            in_frontmatter = frontmatter_count == 1;
            updated.push(line.to_string());
            continue;
        }

        if in_frontmatter && trimmed.starts_with("version = \"") {
            let new_line = format!("version = \"{}\"", version);
            if new_line != trimmed {
                updated.push(new_line);
                changed = true;
            } else {
                updated.push(line.to_string());
            }
        } else if !in_frontmatter
            && (trimmed.starts_with("* **Current Engine Version**: `v")
                || trimmed.starts_with("* **Current Engine Version**: v"))
        {
            let new_line = format!("* **Current Engine Version**: `v{}`", version);
            if new_line != trimmed {
                updated.push(new_line);
                changed = true;
            } else {
                updated.push(line.to_string());
            }
        } else {
            updated.push(line.to_string());
        }

        if frontmatter_count == 2 {
            in_frontmatter = false;
        }
    }
    let result = updated.join("\n") + "\n";
    if changed {
        fs::write(path, &result).ok();
    }
    result
}

fn skip_frontmatter(content: &str) -> String {
    if content.starts_with("---") {
        let parts: Vec<&str> = content.splitn(3, "---").collect();
        if parts.len() == 3 {
            return parts[2].trim_start().to_string();
        }
    }
    content.to_string()
}
