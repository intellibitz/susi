#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Build scripts are not production runtime modules: a failed invariant must
// abort the build loudly, so panic-on-error is the intended failure mode here.

//! Compile `.agents/{identity,roadmap,evidence}.json` into static axiom tables.
use serde_json::Value;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn agents_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.agents")
}

fn main() {
    let agents = agents_dir();
    let identity_path = agents.join("identity.json");
    let roadmap_path = agents.join("roadmap.json");
    let evidence_path = agents.join("evidence.json");

    let cargo_toml =
        fs::read_to_string(agents.join("../Cargo.toml")).expect("Missing workspace Cargo.toml");
    let version = cargo_toml
        .lines()
        .find(|l| l.trim().starts_with("version = \""))
        .and_then(|l| l.split('"').nth(1))
        .expect("Could not find version in Cargo.toml")
        .to_string();

    let identity = load_and_sync_version(&identity_path, "susi/identity/v1", &version);
    let roadmap = load_and_sync_version(&roadmap_path, "susi/roadmap/v1", &version);
    let evidence = load_and_sync_version(&evidence_path, "susi/evidence/v1", &version);

    validate_identity(&identity);
    validate_roadmap(&roadmap);
    validate_evidence(&evidence);

    sync_readme_badge(agents.join("../README.md"), &version);

    let out_dir = env::var_os("OUT_DIR").unwrap();
    let dest_path = Path::new(&out_dir).join("generated_axioms.rs");
    let generated = emit_axioms(&version, &identity, &roadmap, &evidence);
    fs::write(&dest_path, generated).unwrap();

    println!("cargo:rerun-if-changed={}", identity_path.display());
    println!("cargo:rerun-if-changed={}", roadmap_path.display());
    println!("cargo:rerun-if-changed={}", evidence_path.display());
    println!(
        "cargo:rerun-if-changed={}",
        agents.join("../Cargo.toml").display()
    );
}

fn load_and_sync_version(path: &Path, expected_schema: &str, version: &str) -> Value {
    let raw =
        fs::read_to_string(path).unwrap_or_else(|e| panic!("Missing {}: {e}", path.display()));
    let mut doc: Value = serde_json::from_str(&raw)
        .unwrap_or_else(|e| panic!("Invalid JSON {}: {e}", path.display()));
    let schema = doc
        .get("schema")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("{}: missing schema", path.display()));
    if schema != expected_schema {
        panic!(
            "{}: expected schema {:?}, got {:?}",
            path.display(),
            expected_schema,
            schema
        );
    }
    let cur = doc.get("version").and_then(|v| v.as_str()).unwrap_or("");
    if cur != version {
        doc["version"] = Value::String(version.to_string());
        let pretty = serde_json::to_string_pretty(&doc).expect("serialize");
        fs::write(path, pretty + "\n").ok();
    }
    doc
}

fn require_array<'a>(doc: &'a Value, path: &str) -> &'a Vec<Value> {
    doc.pointer(path)
        .and_then(|v| v.as_array())
        .unwrap_or_else(|| panic!("missing or non-array path {path}"))
}

fn validate_identity(doc: &Value) {
    assert_eq!(doc["audience"], "agent-governance");
    let mandates = require_array(doc, "/pillars/dna/mandates");
    assert!(!mandates.is_empty(), "identity: dna.mandates empty");
    let components = require_array(doc, "/pillars/body/components");
    assert!(!components.is_empty(), "identity: body.components empty");
    let protocols = require_array(doc, "/pillars/engine/protocols");
    assert!(!protocols.is_empty(), "identity: engine.protocols empty");
    let ports = require_array(doc, "/host_contract/ports");
    assert!(ports.len() >= 4, "identity: host_contract.ports needs ≥4");
    let foundation = require_array(doc, "/foundation_pillars");
    assert!(!foundation.is_empty(), "identity: foundation_pillars empty");
}

fn validate_roadmap(doc: &Value) {
    let vectors = require_array(doc, "/vectors");
    assert!(!vectors.is_empty(), "roadmap: vectors empty");
    for v in vectors {
        let id = v["id"].as_str().unwrap_or("");
        assert!(
            id.starts_with("VC-"),
            "roadmap vector id must start with VC-"
        );
    }
}

fn validate_evidence(doc: &Value) {
    let entries = require_array(doc, "/entries");
    assert!(!entries.is_empty(), "evidence: entries empty");
    for e in entries {
        let id = e["id"].as_str().unwrap_or("");
        assert!(id.starts_with("EV-"), "evidence id must start with EV-");
        assert!(e.get("proof").and_then(|p| p.as_str()).is_some());
        assert!(e
            .pointer("/anchor/label")
            .and_then(|l| l.as_str())
            .is_some());
    }
}

fn sync_readme_badge(readme_path: PathBuf, version: &str) {
    let Ok(readme_md_raw) = fs::read_to_string(&readme_path) else {
        return;
    };
    if !readme_md_raw.contains("https://img.shields.io/badge/version-v") {
        return;
    }
    let mut updated = Vec::new();
    for line in readme_md_raw.lines() {
        if line.contains("https://img.shields.io/badge/version-v") {
            updated.push(format!(
                "![SUSI Version](https://img.shields.io/badge/version-v{}-blue.svg) ![License](https://img.shields.io/badge/license-Apache%202.0-green.svg)",
                version
            ));
        } else {
            updated.push(line.to_string());
        }
    }
    let new_readme = updated.join("\n") + "\n";
    if new_readme != readme_md_raw {
        let _ = fs::write(readme_path, new_readme);
    }
}

fn emit_rule(id: usize, title: &str, imperative: &str) -> String {
    format!("    SusiAxiomRule {{ id: {id}, title: {title:?}, imperative: {imperative:?} }},\n")
}

fn seq_suffix(id: &str) -> usize {
    id.split('-')
        .next_back()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

fn tier_enum(tier: i64) -> &'static str {
    match tier {
        0 => "SusiCoreTier::Tier0Reflex",
        2 => "SusiCoreTier::Tier2Reasoning",
        _ => "SusiCoreTier::Tier1Swarm",
    }
}

fn emit_component(name: &str, tier: i64, description: &str) -> String {
    format!(
        "    SusiComponentSpec {{ name: {name:?}, tier: {}, description: {description:?} }},\n",
        tier_enum(tier)
    )
}

fn categorize_component(name: &str) -> &'static str {
    let n = name.to_lowercase();
    if n.contains("gawd")
        || n.contains("admin")
        || n.contains("loader")
        || n.contains("daemon")
        || n.contains("evolutionmanager")
    {
        "aoa"
    } else if n.contains("agent") || n.contains("factory") || n.contains("scout") {
        "agents"
    } else if n.contains("model")
        || n.contains("frontier")
        || n.contains("openweight")
        || n.contains("openrouter")
        || n.contains("codingmodel")
    {
        // Model control planes and native weight specs — Models pillar.
        "models"
    } else if n.contains("susi-")
        || n.contains("engine")
        || n.contains("substrate")
        || n.contains("gemi")
        || n.contains("synthesizer")
    {
        "engines"
    } else if n.contains("mcp")
        || n.contains("server")
        || n.contains("host")
        || n.contains("evidence")
    {
        "mcps"
    } else {
        "realized"
    }
}

fn emit_axioms(version: &str, identity: &Value, roadmap: &Value, evidence: &Value) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "pub const GEN_ENGINE_VERSION: &str = {version:?};\n\n"
    ));

    // DNA mandates -> GEN_AGENT_RULES
    out.push_str("pub const GEN_AGENT_RULES: &[SusiAxiomRule] = &[\n");
    for m in require_array(identity, "/pillars/dna/mandates") {
        let id = m["id"].as_u64().unwrap() as usize;
        let title = m["title"].as_str().unwrap_or("");
        let imperative = m["imperative"].as_str().unwrap_or("");
        out.push_str(&emit_rule(id, title, imperative));
    }
    out.push_str("];\n\n");

    // Roadmap -> GEN_ENGINE_AXIOMS (50-99)
    out.push_str("pub const GEN_ENGINE_AXIOMS: &[SusiAxiomRule] = &[\n");
    for v in require_array(roadmap, "/vectors") {
        let id = v["id"].as_str().unwrap_or("");
        let seq = seq_suffix(id);
        if seq == 0 {
            continue;
        }
        let title = format!(
            "{} {}",
            v["mastery_target"].as_str().unwrap_or(""),
            v["vector"].as_str().unwrap_or("")
        );
        let imperative = v["progress"].as_str().unwrap_or("");
        out.push_str(&emit_rule(seq + 50, &title, imperative));
    }
    out.push_str("];\n\n");

    // Engine protocols -> GEN_DEPLOYMENT_RULES (100-149)
    out.push_str("pub const GEN_DEPLOYMENT_RULES: &[SusiAxiomRule] = &[\n");
    for p in require_array(identity, "/pillars/engine/protocols") {
        let id = p["id"].as_u64().unwrap() as usize;
        let title = p["title"].as_str().unwrap_or("");
        let imperative = p["imperative"].as_str().unwrap_or("");
        out.push_str(&emit_rule(id + 100, title, imperative));
    }
    out.push_str("];\n\n");

    // Evidence -> GEN_PULSE_AXIOMS (150+)
    out.push_str("pub const GEN_PULSE_AXIOMS: &[SusiAxiomRule] = &[\n");
    for e in require_array(evidence, "/entries") {
        let id = e["id"].as_str().unwrap_or("");
        let seq = seq_suffix(id);
        if seq == 0 {
            continue;
        }
        let typ = e["type"].as_str().unwrap_or("");
        let milestone = e["milestone"].as_str().unwrap_or("");
        let title = format!("{milestone} [{typ}]");
        let anchor_label = e
            .pointer("/anchor/label")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let proof = e["proof"].as_str().unwrap_or("");
        let imperative = format!("Anchor: {anchor_label}. Proof: {proof}");
        out.push_str(&emit_rule(seq + 150, &title, &imperative));
    }
    out.push_str("];\n\n");

    // Components by category
    let mut aoa = String::from("pub const GEN_AOA_COMPONENTS: &[SusiComponentSpec] = &[\n");
    let mut agents = String::from("pub const GEN_AGENT_COMPONENTS: &[SusiComponentSpec] = &[\n");
    let mut engines = String::from("pub const GEN_ENGINE_COMPONENTS: &[SusiComponentSpec] = &[\n");
    let mut models = String::from("pub const GEN_MODEL_COMPONENTS: &[SusiComponentSpec] = &[\n");
    let mut mcps = String::from("pub const GEN_MCP_COMPONENTS: &[SusiComponentSpec] = &[\n");
    let mut realized =
        String::from("pub const GEN_REALIZED_COMPONENTS: &[SusiComponentSpec] = &[\n");
    let mut all = String::from("pub const GEN_COMPONENTS: &[SusiComponentSpec] = &[\n");

    for c in require_array(identity, "/pillars/body/components") {
        let name = c["symbol"].as_str().unwrap_or("");
        let tier = c["tier"].as_i64().unwrap_or(1);
        let description = c["function"].as_str().unwrap_or("");
        let entry = emit_component(name, tier, description);
        all.push_str(&entry);
        match categorize_component(name) {
            "aoa" => aoa.push_str(&entry),
            "agents" => agents.push_str(&entry),
            "engines" => engines.push_str(&entry),
            "models" => models.push_str(&entry),
            "mcps" => mcps.push_str(&entry),
            _ => realized.push_str(&entry),
        }
    }
    for bucket in [
        &mut aoa,
        &mut agents,
        &mut engines,
        &mut models,
        &mut mcps,
        &mut realized,
        &mut all,
    ] {
        bucket.push_str("];\n\n");
    }
    out.push_str(&aoa);
    out.push_str(&agents);
    out.push_str(&engines);
    out.push_str(&models);
    out.push_str(&mcps);
    out.push_str(&realized);
    out.push_str(&all);

    // Unified GEN_RULES
    out.push_str("pub const GEN_RULES: &[SusiAxiomRule] = &[\n");
    for m in require_array(identity, "/pillars/dna/mandates") {
        let id = m["id"].as_u64().unwrap() as usize;
        out.push_str(&emit_rule(
            id,
            m["title"].as_str().unwrap_or(""),
            m["imperative"].as_str().unwrap_or(""),
        ));
    }
    for v in require_array(roadmap, "/vectors") {
        let id = v["id"].as_str().unwrap_or("");
        let seq = seq_suffix(id);
        if seq == 0 {
            continue;
        }
        let title = format!(
            "{} {}",
            v["mastery_target"].as_str().unwrap_or(""),
            v["vector"].as_str().unwrap_or("")
        );
        out.push_str(&emit_rule(
            seq + 50,
            &title,
            v["progress"].as_str().unwrap_or(""),
        ));
    }
    for p in require_array(identity, "/pillars/engine/protocols") {
        let id = p["id"].as_u64().unwrap() as usize;
        out.push_str(&emit_rule(
            id + 100,
            p["title"].as_str().unwrap_or(""),
            p["imperative"].as_str().unwrap_or(""),
        ));
    }
    for e in require_array(evidence, "/entries") {
        let id = e["id"].as_str().unwrap_or("");
        let seq = seq_suffix(id);
        if seq == 0 {
            continue;
        }
        let title = format!(
            "{} [{}]",
            e["milestone"].as_str().unwrap_or(""),
            e["type"].as_str().unwrap_or("")
        );
        let imperative = format!(
            "Anchor: {}. Proof: {}",
            e.pointer("/anchor/label")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
            e["proof"].as_str().unwrap_or("")
        );
        out.push_str(&emit_rule(seq + 150, &title, &imperative));
    }
    out.push_str("];\n");
    out
}
