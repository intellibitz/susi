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
    // The host contract is compile-time constants in susi_paths::ports;
    // this document must name exactly that set — a stale subset passed
    // a bare >=4 check while omitting A2A 9094.
    let mut listed: Vec<u64> = ports.iter().filter_map(|p| p["port"].as_u64()).collect();
    listed.sort_unstable();
    assert_eq!(
        listed,
        vec![9090, 9091, 9092, 9093, 9094],
        "identity: host_contract.ports must equal susi_paths::ports::ALL"
    );
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

fn emit_rule(kind: &str, cite: &str, title: &str, imperative: &str) -> String {
    format!(
        "    SusiAxiomRule {{ kind: RuleKind::{kind}, cite: {cite:?}, title: {title:?}, imperative: {imperative:?} }},\n"
    )
}

/// Every compiled rule, each under its own canonical citation (identity.json
/// `canonical_addressing`): DNA mandates as `Mandate N`, engine protocols as
/// `Pillar IV item N`, roadmap vectors and ledger entries by their own IDs.
/// No synthetic numbering: ledger IDs span several counters, so any offset
/// scheme collides (EV-2022920-031 and EV-2022924-031 were both "181").
fn genome_rules(
    identity: &Value,
    roadmap: &Value,
    evidence: &Value,
) -> Vec<(&'static str, String)> {
    let mut rules = Vec::new();
    for m in require_array(identity, "/pillars/dna/mandates") {
        let id = m["id"].as_u64().unwrap();
        rules.push((
            "Mandate",
            emit_rule(
                "Mandate",
                &format!("Mandate {id}"),
                m["title"].as_str().unwrap_or(""),
                m["imperative"].as_str().unwrap_or(""),
            ),
        ));
    }
    for p in require_array(identity, "/pillars/engine/protocols") {
        let id = p["id"].as_u64().unwrap();
        rules.push((
            "EngineProtocol",
            emit_rule(
                "EngineProtocol",
                &format!("Pillar IV item {id}"),
                p["title"].as_str().unwrap_or(""),
                p["imperative"].as_str().unwrap_or(""),
            ),
        ));
    }
    for v in require_array(roadmap, "/vectors") {
        let title = format!(
            "{} {}",
            v["mastery_target"].as_str().unwrap_or(""),
            v["vector"].as_str().unwrap_or("")
        );
        rules.push((
            "RoadmapVector",
            emit_rule(
                "RoadmapVector",
                v["id"].as_str().unwrap_or(""),
                &title,
                v["progress"].as_str().unwrap_or(""),
            ),
        ));
    }
    for e in require_array(evidence, "/entries") {
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
        rules.push((
            "Evidence",
            emit_rule(
                "Evidence",
                e["id"].as_str().unwrap_or(""),
                &title,
                &imperative,
            ),
        ));
    }
    rules
}

fn emit_rule_const(name: &str, rules: &[(&'static str, String)], kinds: &[&str]) -> String {
    let mut out = format!("pub const {name}: &[SusiAxiomRule] = &[\n");
    for (kind, line) in rules {
        if kinds.contains(kind) {
            out.push_str(line);
        }
    }
    out.push_str("];\n\n");
    out
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

    let rules = genome_rules(identity, roadmap, evidence);
    out.push_str(&emit_rule_const("GEN_AGENT_RULES", &rules, &["Mandate"]));
    out.push_str(&emit_rule_const(
        "GEN_ENGINE_AXIOMS",
        &rules,
        &["RoadmapVector"],
    ));
    out.push_str(&emit_rule_const(
        "GEN_DEPLOYMENT_RULES",
        &rules,
        &["EngineProtocol"],
    ));
    out.push_str(&emit_rule_const("GEN_PULSE_AXIOMS", &rules, &["Evidence"]));

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

    out.push_str(&emit_rule_const(
        "GEN_RULES",
        &rules,
        &["Mandate", "EngineProtocol", "RoadmapVector", "Evidence"],
    ));
    out
}
