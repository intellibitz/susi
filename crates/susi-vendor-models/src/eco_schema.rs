//! Ecosystem knowledge schema (`eco-schema/v1`): a typed graph of the cloud
//! AI ecosystem.
//!
//! Entities: [`Component`]s (models, engines, runtimes, registries, MCP
//! servers/clients, extensions, services), the [`Vendor`]s that provide them,
//! the [`Standard`]s and [`Protocol`]s they implement, the exact
//! [`SpecVersion`]s of those specs, and the [`Capability`]s they expose.
//! [`Relation`]s are typed edges (`provides`, `publishes`, `implements`,
//! `implements-version`, `version-of`, `has-capability`, `depends-on`,
//! `compatible-with`, `supersedes`) with a fixed endpoint-type table
//! ([`allowed_endpoints`]).
//!
//! Every fact — entity or relation — carries [`Provenance`]: the source it
//! came from (an official machine-readable spec or a cited primary source),
//! the spec version the fact is stated against, the date it was retrieved,
//! and a [`Confidence`] level.
//!
//! Validation runs in two stages, mirroring the two artefacts this module
//! ships:
//!
//! 1. the JSON Schema contract ([`JSON_SCHEMA`], draft 2020-12) checked by
//!    [`validate_document`] over raw documents — shape, closed enums, field
//!    patterns;
//! 2. the semantic checks in [`validate`] over the typed model — referential
//!    integrity, relation typing, provenance presence, one `version-of` edge
//!    per spec-version, and acyclic `depends-on`/`supersedes` graphs.
//!
//! [`parse_document`] applies both stages to text and returns the typed
//! model or a single [`EaiError::config`] naming every issue found.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;
use susi_error::{EaiError, EaiResult, ResultExt as Context};

/// Version tag every conforming document must carry in its `version` field.
pub const SCHEMA_VERSION: &str = "eco-schema/v1";

/// The JSON Schema (draft 2020-12) a knowledge-base document is validated
/// against before the typed-model checks run. Bundled at build time; a parse
/// or compile failure of this asset is a build bug, not a runtime condition.
pub const JSON_SCHEMA: &str = include_str!("eco_schema.schema.json");

/// Which validation stage raised an [`Issue`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Stage {
    /// JSON Schema stage: the document's shape, enums and field patterns.
    Schema,
    /// Typed-model stage: references, relation typing, provenance, graph rules.
    Model,
    /// Policy stage: freshness windows and source citability
    /// (`crate::eco_provenance`).
    Policy,
}

/// A single validation failure. `path` is a JSON-pointer-ish location such as
/// `/entities/3/id` or `/relations/0`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Issue {
    pub stage: Stage,
    pub path: String,
    pub message: String,
}

impl Issue {
    fn schema(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            stage: Stage::Schema,
            path: path.into(),
            message: message.into(),
        }
    }
    fn model(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            stage: Stage::Model,
            path: path.into(),
            message: message.into(),
        }
    }
}

/// How strongly a fact is supported by its cited source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Confidence {
    /// Checked against the primary source by the recording agent.
    Verified,
    /// Stated by a cited primary source, not independently re-checked.
    Reported,
    /// Derived from other recorded facts rather than stated directly.
    Inferred,
    /// Plausible but weakly supported; never a basis for gating.
    Speculative,
}

/// Provenance carried by every fact. `spec_version` is the version of the
/// specification the fact is stated against (`"n/a"` when unversioned);
/// `retrieved` is an ISO 8601 date (`YYYY-MM-DD`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    pub source: String,
    pub spec_version: String,
    pub retrieved: String,
    pub confidence: Confidence,
}

/// What a component is in the ecosystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ComponentCategory {
    Model,
    InferenceEngine,
    ServingGateway,
    AgentRuntime,
    AgentFramework,
    ModelRegistry,
    McpServer,
    McpClient,
    Extension,
    CliTool,
    Service,
}

/// Wire family a protocol runs over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Transport {
    Http,
    HttpSse,
    Websocket,
    Grpc,
    Stdio,
    InProcess,
}

/// Coarse family a capability belongs to — the canonical vocabulary lives in
/// `crate::eco_taxonomy`, keyed by these classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CapabilityClass {
    TextGeneration,
    ToolCalling,
    StructuredOutput,
    Streaming,
    Embeddings,
    Vision,
    Audio,
    Reasoning,
    Batch,
    Realtime,
    PromptCaching,
    Files,
    Moderation,
    AgentDelegation,
    ResourceAccess,
    Discovery,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Component {
    pub id: String,
    pub name: String,
    pub category: ComponentCategory,
    pub provenance: Provenance,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Vendor {
    pub id: String,
    pub name: String,
    /// Vendor home page or primary developer portal.
    pub home: String,
    pub provenance: Provenance,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Standard {
    pub id: String,
    pub name: String,
    /// Organisation that publishes the standard (SDO, vendor or foundation).
    pub body: String,
    pub provenance: Provenance,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Protocol {
    pub id: String,
    pub name: String,
    /// Every transport the spec defines (MCP is `stdio` + `http-sse`).
    pub transports: Vec<Transport>,
    pub provenance: Provenance,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecVersion {
    pub id: String,
    pub name: String,
    /// Version label as published by the spec's body (`"2025-06-18"`, `"3.1.0"`).
    pub version: String,
    /// Publication date of this spec version, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub released: Option<String>,
    pub provenance: Provenance,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capability {
    pub id: String,
    pub name: String,
    pub class: CapabilityClass,
    pub provenance: Provenance,
}

/// Kind tag used in relation-typing checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntityKind {
    Component,
    Vendor,
    Standard,
    Protocol,
    SpecVersion,
    Capability,
}

impl EntityKind {
    /// Lowercase kebab-case name, matching the serde representation.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Component => "component",
            Self::Vendor => "vendor",
            Self::Standard => "standard",
            Self::Protocol => "protocol",
            Self::SpecVersion => "spec-version",
            Self::Capability => "capability",
        }
    }
}

/// A node in the knowledge graph; `kind` selects the payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Entity {
    Component(Component),
    Vendor(Vendor),
    Standard(Standard),
    Protocol(Protocol),
    SpecVersion(SpecVersion),
    Capability(Capability),
}

impl Entity {
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Self::Component(e) => &e.id,
            Self::Vendor(e) => &e.id,
            Self::Standard(e) => &e.id,
            Self::Protocol(e) => &e.id,
            Self::SpecVersion(e) => &e.id,
            Self::Capability(e) => &e.id,
        }
    }

    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Component(e) => &e.name,
            Self::Vendor(e) => &e.name,
            Self::Standard(e) => &e.name,
            Self::Protocol(e) => &e.name,
            Self::SpecVersion(e) => &e.name,
            Self::Capability(e) => &e.name,
        }
    }

    #[must_use]
    pub const fn kind(&self) -> EntityKind {
        match self {
            Self::Component(_) => EntityKind::Component,
            Self::Vendor(_) => EntityKind::Vendor,
            Self::Standard(_) => EntityKind::Standard,
            Self::Protocol(_) => EntityKind::Protocol,
            Self::SpecVersion(_) => EntityKind::SpecVersion,
            Self::Capability(_) => EntityKind::Capability,
        }
    }

    #[must_use]
    pub const fn provenance(&self) -> &Provenance {
        match self {
            Self::Component(e) => &e.provenance,
            Self::Vendor(e) => &e.provenance,
            Self::Standard(e) => &e.provenance,
            Self::Protocol(e) => &e.provenance,
            Self::SpecVersion(e) => &e.provenance,
            Self::Capability(e) => &e.provenance,
        }
    }
}

/// Typed edge between entities. The allowed endpoint kinds for each variant
/// are fixed by [`allowed_endpoints`] and enforced by [`validate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RelationKind {
    /// vendor → component: the vendor offers the component.
    Provides,
    /// vendor → standard|protocol: the vendor publishes the spec.
    Publishes,
    /// component → standard|protocol: the component implements the spec.
    Implements,
    /// component → spec-version: pinned to an exact spec version.
    ImplementsVersion,
    /// spec-version → standard|protocol: what this version is a version of.
    VersionOf,
    /// component → capability: the component exposes the capability.
    HasCapability,
    /// component → component: the source cannot run without the target.
    DependsOn,
    /// component → component: symmetric interoperability.
    CompatibleWith,
    /// spec-version → spec-version: the source replaces the target.
    Supersedes,
}

impl RelationKind {
    /// Lowercase kebab-case name, matching the serde representation.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Provides => "provides",
            Self::Publishes => "publishes",
            Self::Implements => "implements",
            Self::ImplementsVersion => "implements-version",
            Self::VersionOf => "version-of",
            Self::HasCapability => "has-capability",
            Self::DependsOn => "depends-on",
            Self::CompatibleWith => "compatible-with",
            Self::Supersedes => "supersedes",
        }
    }
}

/// Allowed `(from, to)` entity kinds for each relation kind — the schema's
/// type table, enforced by [`validate`].
#[must_use]
pub fn allowed_endpoints(kind: RelationKind) -> (&'static [EntityKind], &'static [EntityKind]) {
    use EntityKind as K;
    use RelationKind as R;
    match kind {
        R::Provides => (&[K::Vendor], &[K::Component]),
        R::Publishes => (&[K::Vendor], &[K::Standard, K::Protocol]),
        R::Implements => (&[K::Component], &[K::Standard, K::Protocol]),
        R::ImplementsVersion => (&[K::Component], &[K::SpecVersion]),
        R::VersionOf => (&[K::SpecVersion], &[K::Standard, K::Protocol]),
        R::HasCapability => (&[K::Component], &[K::Capability]),
        R::DependsOn | R::CompatibleWith => (&[K::Component], &[K::Component]),
        R::Supersedes => (&[K::SpecVersion], &[K::SpecVersion]),
    }
}

/// A typed, provenanced edge between two entities by id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Relation {
    pub kind: RelationKind,
    pub from: String,
    pub to: String,
    pub provenance: Provenance,
}

/// A complete `eco-schema/v1` knowledge base.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnowledgeBase {
    pub version: String,
    #[serde(default)]
    pub entities: Vec<Entity>,
    #[serde(default)]
    pub relations: Vec<Relation>,
}

impl KnowledgeBase {
    #[must_use]
    pub fn new() -> Self {
        Self {
            version: SCHEMA_VERSION.to_string(),
            entities: Vec::new(),
            relations: Vec::new(),
        }
    }

    #[must_use]
    pub fn entity(&self, id: &str) -> Option<&Entity> {
        self.entities.iter().find(|e| e.id() == id)
    }
}

impl Default for KnowledgeBase {
    fn default() -> Self {
        Self::new()
    }
}

/// Slug rules mirrored by the JSON Schema `id` pattern: lowercase ASCII,
/// starts alphanumeric, at most 128 chars.
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && id.chars().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | ':' | '-')
        })
}

/// Parse a `YYYY-MM-DD` string into a real calendar `(year, month, day)`.
/// No year bound — spec release dates can predate this codebase.
fn parse_ymd(s: &str) -> Option<(u32, u32, u32)> {
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 3 || parts[0].len() != 4 || parts[1].len() != 2 || parts[2].len() != 2 {
        return None;
    }
    let (Ok(year), Ok(month), Ok(day)) = (
        parts[0].parse::<u32>(),
        parts[1].parse::<u32>(),
        parts[2].parse::<u32>(),
    ) else {
        return None;
    };
    if !(1..=12).contains(&month) {
        return None;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ][month as usize - 1];
    (1..=days).contains(&day).then_some((year, month, day))
}

/// ISO 8601 `YYYY-MM-DD` that is a real calendar date in 2000..=2100 — the
/// JSON Schema pattern only pins the shape, the calendar check lives here.
#[must_use]
pub fn valid_date(s: &str) -> bool {
    parse_ymd(s).is_some_and(|(year, _, _)| (2000..=2100).contains(&year))
}

/// Days since the Unix epoch for a `YYYY-MM-DD` date (Howard Hinnant's
/// days-from-civil). `None` when the date is not a real calendar day.
#[must_use]
pub fn date_to_days(s: &str) -> Option<i64> {
    let (y, m, d) = parse_ymd(s)?;
    let y = i64::from(y) - i64::from(m <= 2);
    let (m, d) = (i64::from(m), i64::from(d));
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

fn check_provenance(p: &Provenance, path: &str, issues: &mut Vec<Issue>) {
    let base = format!("{path}/provenance");
    if p.source.trim().is_empty() || p.source.chars().any(char::is_control) {
        issues.push(Issue::model(
            format!("{base}/source"),
            "provenance.source must name a spec or cited primary source",
        ));
    }
    if p.spec_version.trim().is_empty() {
        issues.push(Issue::model(
            format!("{base}/spec_version"),
            "provenance.spec_version must name the spec version the fact is stated against (\"n/a\" if unversioned)",
        ));
    }
    if !valid_date(&p.retrieved) {
        issues.push(Issue::model(
            format!("{base}/retrieved"),
            format!(
                "provenance.retrieved {:?} is not a real YYYY-MM-DD date",
                p.retrieved
            ),
        ));
    }
}

fn check_entity_fields(e: &Entity, path: &str, issues: &mut Vec<Issue>) {
    if e.name().trim().is_empty() {
        issues.push(Issue::model(
            format!("{path}/name"),
            "name must not be empty",
        ));
    }
    match e {
        Entity::Component(_) | Entity::Capability(_) => {}
        Entity::Vendor(v)
            if v.home.trim().is_empty() || v.home.chars().any(char::is_whitespace) =>
        {
            issues.push(Issue::model(
                format!("{path}/home"),
                "vendor.home must be a URL or domain",
            ));
        }
        Entity::Vendor(_) => {}
        Entity::Standard(s) if s.body.trim().is_empty() => {
            issues.push(Issue::model(
                format!("{path}/body"),
                "standard.body must name the publishing organisation",
            ));
        }
        Entity::Standard(_) => {}
        Entity::Protocol(p) if p.transports.is_empty() => {
            issues.push(Issue::model(
                format!("{path}/transports"),
                "protocol must declare at least one transport",
            ));
        }
        Entity::Protocol(_) => {}
        Entity::SpecVersion(v) => {
            if v.version.trim().is_empty() {
                issues.push(Issue::model(
                    format!("{path}/version"),
                    "spec-version must name the published version label",
                ));
            }
            if let Some(released) = &v.released {
                if !valid_date(released) {
                    issues.push(Issue::model(
                        format!("{path}/released"),
                        format!("released {released:?} is not a real YYYY-MM-DD date"),
                    ));
                }
            }
        }
    }
}

/// Return the first cycle in the directed graph `edges` (as `from -> to`
/// pairs), or `None` when it is acyclic. Three-colour DFS; graphs here are
/// small enough that recursion depth is bounded by entity count.
fn find_cycle<'a>(edges: &[(&'a str, &'a str)]) -> Option<Vec<&'a str>> {
    fn visit<'a>(
        node: &'a str,
        adj: &HashMap<&'a str, Vec<&'a str>>,
        state: &mut HashMap<&'a str, u8>,
        stack: &mut Vec<&'a str>,
    ) -> Option<Vec<&'a str>> {
        state.insert(node, 1);
        stack.push(node);
        if let Some(nexts) = adj.get(node) {
            for &next in nexts {
                match state.get(next).copied().unwrap_or(0) {
                    1 => {
                        let pos = stack.iter().position(|&n| n == next).unwrap_or(0);
                        let mut cycle = stack[pos..].to_vec();
                        cycle.push(next);
                        return Some(cycle);
                    }
                    0 => {
                        if let hit @ Some(_) = visit(next, adj, state, stack) {
                            return hit;
                        }
                    }
                    _ => {}
                }
            }
        }
        stack.pop();
        state.insert(node, 2);
        None
    }

    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
    for &(from, to) in edges {
        adj.entry(from).or_default().push(to);
    }
    let mut state: HashMap<&str, u8> = HashMap::new();
    let mut stack: Vec<&str> = Vec::new();
    for &node in adj.keys() {
        if state.get(node).copied().unwrap_or(0) == 0 {
            if let hit @ Some(_) = visit(node, &adj, &mut state, &mut stack) {
                return hit;
            }
        }
    }
    None
}

/// Semantic validation over the typed model. An empty result means the
/// knowledge base is internally consistent; issues are returned in document
/// order rather than stopping at the first.
#[must_use]
pub fn validate(kb: &KnowledgeBase) -> Vec<Issue> {
    let mut issues = Vec::new();
    if kb.version != SCHEMA_VERSION {
        issues.push(Issue::model(
            "/version",
            format!(
                "unsupported schema version {:?}; expected {SCHEMA_VERSION:?}",
                kb.version
            ),
        ));
    }

    let mut kinds: HashMap<&str, EntityKind> = HashMap::new();
    for (i, e) in kb.entities.iter().enumerate() {
        let path = format!("/entities/{i}");
        if !valid_id(e.id()) {
            issues.push(Issue::model(
                format!("{path}/id"),
                format!(
                    "entity id {:?} must be 1-128 chars of [a-z0-9._:-] starting alphanumeric",
                    e.id()
                ),
            ));
        }
        if kinds.insert(e.id(), e.kind()).is_some() {
            issues.push(Issue::model(
                format!("{path}/id"),
                format!("duplicate entity id {:?}", e.id()),
            ));
        }
        check_entity_fields(e, &path, &mut issues);
        check_provenance(e.provenance(), &path, &mut issues);
    }

    let mut seen: HashSet<(RelationKind, &str, &str)> = HashSet::new();
    for (i, r) in kb.relations.iter().enumerate() {
        let path = format!("/relations/{i}");
        check_provenance(&r.provenance, &path, &mut issues);
        if r.from == r.to {
            issues.push(Issue::model(
                path.clone(),
                format!(
                    "{} relations cannot be self-loops ({:?})",
                    r.kind.label(),
                    r.from
                ),
            ));
        }
        // compatible-with is symmetric: (a,b) and (b,a) are the same edge.
        let (from, to) = if r.kind == RelationKind::CompatibleWith && r.to < r.from {
            (r.to.as_str(), r.from.as_str())
        } else {
            (r.from.as_str(), r.to.as_str())
        };
        if !seen.insert((r.kind, from, to)) {
            issues.push(Issue::model(
                path.clone(),
                format!(
                    "duplicate {} relation {:?} -> {:?}",
                    r.kind.label(),
                    r.from,
                    r.to
                ),
            ));
        }
        let (allowed_from, allowed_to) = allowed_endpoints(r.kind);
        match (kinds.get(r.from.as_str()), kinds.get(r.to.as_str())) {
            (Some(from_kind), Some(to_kind)) => {
                if !allowed_from.contains(from_kind) || !allowed_to.contains(to_kind) {
                    issues.push(Issue::model(
                        path,
                        format!(
                            "{} must go {} -> {}, not {} -> {}",
                            r.kind.label(),
                            describe_kinds(allowed_from),
                            describe_kinds(allowed_to),
                            from_kind.label(),
                            to_kind.label()
                        ),
                    ));
                }
            }
            (missing_from, missing_to) => {
                if missing_from.is_none() {
                    issues.push(Issue::model(
                        format!("{path}/from"),
                        format!("relation source {:?} is not a known entity id", r.from),
                    ));
                }
                if missing_to.is_none() {
                    issues.push(Issue::model(
                        format!("{path}/to"),
                        format!("relation target {:?} is not a known entity id", r.to),
                    ));
                }
            }
        }
    }

    // Every spec-version pins exactly one subject via version-of.
    for e in &kb.entities {
        if e.kind() != EntityKind::SpecVersion {
            continue;
        }
        let edges = kb
            .relations
            .iter()
            .filter(|r| r.kind == RelationKind::VersionOf && r.from == e.id())
            .count();
        if edges != 1 {
            issues.push(Issue::model(
                format!("/entities/{}", e.id()),
                format!(
                    "spec-version {:?} must have exactly one version-of edge, found {edges}",
                    e.id()
                ),
            ));
        }
    }

    // depends-on and supersedes are ordering relations: cycles are bugs.
    for (kind, label) in [
        (RelationKind::DependsOn, "depends-on"),
        (RelationKind::Supersedes, "supersedes"),
    ] {
        let edges: Vec<(&str, &str)> = kb
            .relations
            .iter()
            .filter(|r| r.kind == kind)
            .map(|r| (r.from.as_str(), r.to.as_str()))
            .collect();
        if let Some(cycle) = find_cycle(&edges) {
            issues.push(Issue::model(
                "/relations",
                format!("{label} graph contains a cycle: {}", cycle.join(" -> ")),
            ));
        }
    }

    issues
}

fn describe_kinds(kinds: &[EntityKind]) -> String {
    kinds
        .iter()
        .map(|k| k.label())
        .collect::<Vec<_>>()
        .join("|")
}

/// Compile the bundled JSON Schema once per process. A failure here is a
/// build bug (the asset is `include_str!`'d), surfaced as `internal`.
fn compiled_schema() -> EaiResult<&'static jsonschema::Validator> {
    static COMPILED: OnceLock<Result<jsonschema::Validator, String>> = OnceLock::new();
    COMPILED
        .get_or_init(|| {
            let schema: Value = serde_json::from_str(JSON_SCHEMA)
                .map_err(|e| format!("bundled eco-schema JSON Schema does not parse: {e}"))?;
            // The schema is fixed and bundled, so the draft is pinned rather
            // than negotiated from $schema.
            jsonschema::draft202012::options()
                .build(&schema)
                .map_err(|e| format!("bundled eco-schema JSON Schema does not compile: {e}"))
        })
        .as_ref()
        .map_err(|e| EaiError::internal(e.clone()))
}

/// Stage 1+2 over a raw JSON document: JSON Schema conformance first (shape,
/// closed enums, patterns), then the typed-model checks in [`validate`].
/// When the schema stage fails, its issues are returned alone — a malformed
/// document cannot be trusted to produce meaningful model errors.
#[must_use]
pub fn validate_document(doc: &Value) -> Vec<Issue> {
    let validator = match compiled_schema() {
        Ok(v) => v,
        Err(e) => return vec![Issue::schema("", e.to_string())],
    };
    let issues: Vec<Issue> = validator
        .iter_errors(doc)
        .map(|e| Issue::schema(e.instance_path().to_string(), e.to_string()))
        .collect();
    if !issues.is_empty() {
        return issues;
    }
    match serde_json::from_value::<KnowledgeBase>(doc.clone()) {
        Ok(kb) => validate(&kb),
        // The schema gates every field the typed model reads, so a divergence
        // means the two artefacts disagree — a defect in this crate, surfaced
        // loudly rather than silently accepting or rejecting.
        Err(e) => vec![Issue::model(
            "",
            format!("document passes the schema but not the typed model: {e}"),
        )],
    }
}

/// Parse and fully validate a knowledge-base document. Invalid documents are
/// rejected with an [`EaiError::config`] summarising every issue found.
pub fn parse_document(text: &str) -> EaiResult<KnowledgeBase> {
    let doc: Value =
        serde_json::from_str(text).context("ecosystem knowledge base is not valid JSON")?;
    let issues = validate_document(&doc);
    if !issues.is_empty() {
        let summary = issues
            .iter()
            .take(3)
            .map(|i| format!("{}: {}", i.path, i.message))
            .collect::<Vec<_>>()
            .join("; ");
        return Err(EaiError::config(format!(
            "ecosystem knowledge base failed validation ({} issue(s)): {summary}",
            issues.len()
        )));
    }
    // The schema stage already gated every field; this parse cannot fail on a
    // valid document, but the Result keeps the two artefacts honest.
    serde_json::from_value(doc).context("ecosystem knowledge base typed parse")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prov() -> Provenance {
        Provenance {
            source: "https://specs.example.test/v1".into(),
            spec_version: "1.0".into(),
            retrieved: "2026-09-01".into(),
            confidence: Confidence::Verified,
        }
    }

    /// A small but complete valid knowledge base exercising every entity and
    /// relation kind that must type-check cleanly.
    fn fixture() -> KnowledgeBase {
        KnowledgeBase {
            version: SCHEMA_VERSION.into(),
            entities: vec![
                Entity::Vendor(Vendor {
                    id: "acme".into(),
                    name: "Acme AI".into(),
                    home: "https://acme.example.test".into(),
                    provenance: prov(),
                }),
                Entity::Component(Component {
                    id: "acme-engine".into(),
                    name: "Acme Engine".into(),
                    category: ComponentCategory::InferenceEngine,
                    provenance: prov(),
                }),
                Entity::Component(Component {
                    id: "acme-agent".into(),
                    name: "Acme Agent".into(),
                    category: ComponentCategory::AgentRuntime,
                    provenance: prov(),
                }),
                Entity::Protocol(Protocol {
                    id: "acme-api".into(),
                    name: "Acme API".into(),
                    transports: vec![Transport::Http, Transport::HttpSse],
                    provenance: prov(),
                }),
                Entity::SpecVersion(SpecVersion {
                    id: "acme-api-v1".into(),
                    name: "Acme API v1".into(),
                    version: "1.0".into(),
                    released: Some("2026-01-15".into()),
                    provenance: prov(),
                }),
                Entity::SpecVersion(SpecVersion {
                    id: "acme-api-v2".into(),
                    name: "Acme API v2".into(),
                    version: "2.0".into(),
                    released: None,
                    provenance: prov(),
                }),
                Entity::Capability(Capability {
                    id: "cap-tools".into(),
                    name: "Tool calling".into(),
                    class: CapabilityClass::ToolCalling,
                    provenance: prov(),
                }),
            ],
            relations: vec![
                Relation {
                    kind: RelationKind::Provides,
                    from: "acme".into(),
                    to: "acme-engine".into(),
                    provenance: prov(),
                },
                Relation {
                    kind: RelationKind::Publishes,
                    from: "acme".into(),
                    to: "acme-api".into(),
                    provenance: prov(),
                },
                Relation {
                    kind: RelationKind::Implements,
                    from: "acme-engine".into(),
                    to: "acme-api".into(),
                    provenance: prov(),
                },
                Relation {
                    kind: RelationKind::ImplementsVersion,
                    from: "acme-engine".into(),
                    to: "acme-api-v2".into(),
                    provenance: prov(),
                },
                Relation {
                    kind: RelationKind::VersionOf,
                    from: "acme-api-v1".into(),
                    to: "acme-api".into(),
                    provenance: prov(),
                },
                Relation {
                    kind: RelationKind::VersionOf,
                    from: "acme-api-v2".into(),
                    to: "acme-api".into(),
                    provenance: prov(),
                },
                Relation {
                    kind: RelationKind::Supersedes,
                    from: "acme-api-v2".into(),
                    to: "acme-api-v1".into(),
                    provenance: prov(),
                },
                Relation {
                    kind: RelationKind::HasCapability,
                    from: "acme-engine".into(),
                    to: "cap-tools".into(),
                    provenance: prov(),
                },
                Relation {
                    kind: RelationKind::DependsOn,
                    from: "acme-agent".into(),
                    to: "acme-engine".into(),
                    provenance: prov(),
                },
                Relation {
                    kind: RelationKind::CompatibleWith,
                    from: "acme-engine".into(),
                    to: "acme-agent".into(),
                    provenance: prov(),
                },
            ],
        }
    }

    fn doc_of(kb: &KnowledgeBase) -> Value {
        serde_json::to_value(kb).unwrap()
    }

    #[test]
    fn bundled_schema_parses_and_compiles() {
        let schema: Value = serde_json::from_str(JSON_SCHEMA).unwrap();
        assert_eq!(
            schema["$schema"],
            "https://json-schema.org/draft/2020-12/schema"
        );
        assert!(compiled_schema().is_ok());
    }

    #[test]
    fn fixture_passes_both_stages() {
        let kb = fixture();
        assert!(validate(&kb).is_empty(), "{:?}", validate(&kb));
        assert!(validate_document(&doc_of(&kb)).is_empty());
    }

    #[test]
    fn typed_model_roundtrips() {
        let kb = fixture();
        let doc = doc_of(&kb);
        assert_eq!(doc["entities"][0]["kind"], "vendor");
        assert_eq!(doc["relations"][3]["kind"], "implements-version");
        let back: KnowledgeBase = serde_json::from_value(doc).unwrap();
        assert_eq!(back, kb);
    }

    #[test]
    fn parse_document_accepts_valid_and_rejects_invalid() {
        let text = serde_json::to_string(&doc_of(&fixture())).unwrap();
        assert!(parse_document(&text).is_ok());
        let mut doc = doc_of(&fixture());
        doc["version"] = Value::from("eco-schema/v0");
        let err = parse_document(&doc.to_string()).unwrap_err();
        assert!(err.to_string().contains("failed validation"), "{err}");
        assert!(parse_document("{not json").is_err());
    }

    #[test]
    fn schema_stage_rejects_wrong_version_unknown_kind_and_extra_keys() {
        let mut doc = doc_of(&fixture());
        doc["version"] = Value::from("eco-schema/v0");
        assert!(validate_document(&doc)
            .iter()
            .any(|i| i.stage == Stage::Schema));

        let mut doc = doc_of(&fixture());
        doc["entities"][0]["kind"] = Value::from("widget");
        assert!(validate_document(&doc)
            .iter()
            .any(|i| i.stage == Stage::Schema));

        let mut doc = doc_of(&fixture());
        doc["entities"][0]["unexpected"] = Value::from(true);
        assert!(validate_document(&doc)
            .iter()
            .any(|i| i.stage == Stage::Schema));
    }

    #[test]
    fn schema_stage_requires_provenance_on_every_fact() {
        let mut doc = doc_of(&fixture());
        doc["entities"][0]["provenance"]
            .as_object_mut()
            .unwrap()
            .remove("source");
        let issues = validate_document(&doc);
        assert!(issues.iter().any(|i| i.stage == Stage::Schema));

        let mut doc = doc_of(&fixture());
        doc["relations"][0]
            .as_object_mut()
            .unwrap()
            .remove("provenance");
        assert!(validate_document(&doc)
            .iter()
            .any(|i| i.stage == Stage::Schema));
    }

    #[test]
    fn schema_stage_closes_confidence_and_id_patterns() {
        let mut doc = doc_of(&fixture());
        doc["entities"][0]["provenance"]["confidence"] = Value::from("maybe");
        assert!(validate_document(&doc)
            .iter()
            .any(|i| i.stage == Stage::Schema));

        let mut doc = doc_of(&fixture());
        doc["entities"][0]["id"] = Value::from("Bad ID");
        assert!(validate_document(&doc)
            .iter()
            .any(|i| i.stage == Stage::Schema));
    }

    #[test]
    fn model_stage_flags_invalid_ids() {
        let mut kb = fixture();
        if let Entity::Vendor(v) = &mut kb.entities[0] {
            v.id = "Bad ID".into();
        }
        assert!(validate(&kb)
            .iter()
            .any(|i| i.path.ends_with("/id") && i.message.contains("[a-z0-9._:-]")));
    }

    #[test]
    fn retrieved_must_be_a_real_date() {
        for bad in ["2026-02-30", "2026-13-01", "1024-01-01", "not-a-date"] {
            let mut kb = fixture();
            kb.entities[0] = Entity::Vendor(Vendor {
                id: "acme".into(),
                name: "Acme AI".into(),
                home: "https://acme.example.test".into(),
                provenance: Provenance {
                    retrieved: bad.into(),
                    ..prov()
                },
            });
            let issues = validate(&kb);
            assert!(
                issues.iter().any(|i| i.path.ends_with("/retrieved")),
                "{bad}: {issues:?}"
            );
        }
        // Leap years: 2024-02-29 is real, 2025-02-29 is not.
        assert!(valid_date("2024-02-29"));
        assert!(!valid_date("2025-02-29"));
    }

    #[test]
    fn relation_endpoints_must_resolve() {
        let mut kb = fixture();
        kb.relations[0].to = "ghost".into();
        let issues = validate(&kb);
        assert!(issues
            .iter()
            .any(|i| i.path == "/relations/0/to" && i.message.contains("ghost")));
    }

    #[test]
    fn relation_typing_is_enforced() {
        // provides must run vendor -> component, never the reverse.
        let mut kb = fixture();
        kb.relations[0].from = "acme-engine".into();
        kb.relations[0].to = "acme".into();
        assert!(validate(&kb)
            .iter()
            .any(|i| i.path == "/relations/0" && i.message.contains("provides")));

        // has-capability must target a capability entity.
        let mut kb = fixture();
        kb.relations[7].to = "acme-agent".into();
        assert!(validate(&kb)
            .iter()
            .any(|i| i.path == "/relations/7" && i.message.contains("has-capability")));

        // version-of cannot point at a component.
        let mut kb = fixture();
        kb.relations[4].to = "acme-engine".into();
        assert!(validate(&kb)
            .iter()
            .any(|i| i.path == "/relations/4" && i.message.contains("version-of")));
    }

    #[test]
    fn spec_version_needs_exactly_one_version_of() {
        let mut kb = fixture();
        kb.relations
            .retain(|r| !(r.kind == RelationKind::VersionOf && r.from == "acme-api-v1"));
        assert!(validate(&kb)
            .iter()
            .any(|i| i.message.contains("exactly one version-of")
                && i.message.contains("acme-api-v1")));

        let mut kb = fixture();
        kb.relations.push(Relation {
            kind: RelationKind::VersionOf,
            from: "acme-api-v1".into(),
            to: "acme-api".into(),
            provenance: prov(),
        });
        let issues = validate(&kb);
        assert!(issues
            .iter()
            .any(|i| i.message.contains("exactly one version-of")));
    }

    #[test]
    fn duplicate_entities_and_relations_are_rejected() {
        let mut kb = fixture();
        kb.entities.push(kb.entities[0].clone());
        assert!(validate(&kb)
            .iter()
            .any(|i| i.message.contains("duplicate entity id")));

        let mut kb = fixture();
        kb.relations.push(kb.relations[0].clone());
        assert!(validate(&kb)
            .iter()
            .any(|i| i.message.contains("duplicate provides")));

        // compatible-with is symmetric: the reverse edge is the same edge.
        let mut kb = fixture();
        kb.relations.push(Relation {
            kind: RelationKind::CompatibleWith,
            from: "acme-agent".into(),
            to: "acme-engine".into(),
            provenance: prov(),
        });
        assert!(validate(&kb)
            .iter()
            .any(|i| i.message.contains("duplicate compatible-with")));
    }

    #[test]
    fn self_loops_are_rejected() {
        let mut kb = fixture();
        kb.relations[8] = Relation {
            kind: RelationKind::DependsOn,
            from: "acme-agent".into(),
            to: "acme-agent".into(),
            provenance: prov(),
        };
        assert!(validate(&kb)
            .iter()
            .any(|i| i.message.contains("self-loops")));
    }

    #[test]
    fn depends_on_and_supersedes_are_acyclic() {
        let mut kb = fixture();
        kb.relations.push(Relation {
            kind: RelationKind::DependsOn,
            from: "acme-engine".into(),
            to: "acme-agent".into(),
            provenance: prov(),
        });
        assert!(validate(&kb)
            .iter()
            .any(|i| i.message.contains("depends-on graph contains a cycle")));

        let mut kb = fixture();
        kb.relations.push(Relation {
            kind: RelationKind::Supersedes,
            from: "acme-api-v1".into(),
            to: "acme-api-v2".into(),
            provenance: prov(),
        });
        assert!(validate(&kb)
            .iter()
            .any(|i| i.message.contains("supersedes graph contains a cycle")));
    }

    #[test]
    fn endpoint_table_covers_every_relation_kind() {
        // The schema and the Rust table must agree on the closed kind set.
        let kinds = [
            RelationKind::Provides,
            RelationKind::Publishes,
            RelationKind::Implements,
            RelationKind::ImplementsVersion,
            RelationKind::VersionOf,
            RelationKind::HasCapability,
            RelationKind::DependsOn,
            RelationKind::CompatibleWith,
            RelationKind::Supersedes,
        ];
        let schema: Value = serde_json::from_str(JSON_SCHEMA).unwrap();
        let declared = schema["$defs"]["relation"]["properties"]["kind"]["enum"]
            .as_array()
            .unwrap()
            .len();
        assert_eq!(kinds.len(), declared);
        for k in kinds {
            let (f, t) = allowed_endpoints(k);
            assert!(!f.is_empty() && !t.is_empty());
            assert!(serde_json::to_string(&k).is_ok());
        }
    }
}
