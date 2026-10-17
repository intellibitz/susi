#!/usr/bin/env python3
"""Validate the VC-203-001 normative requirements ledger and its pinned spec corpus.

Modes:
  (default)   check: verify the vendored corpus matches .agents/gmcp/sources.json
              byte-for-byte, deterministically re-extract every normative clause,
              and require .agents/gmcp/requirements-ledger.json to equal the
              extracted set with complete, rule-consistent classification.
  --write     regenerate sources.json hashes and requirements-ledger.json from
              the vendored corpus (authoring only; the output is what check
              validates).
  --drift     fetch every pinned upstream file and compare its sha256 against
              the manifest: any change fails closed until the ledger is
              reclassified. Network mode; CI/nightly use only.

Design contract (docs/gmcp/contract-203-001.md):
  - clauses are keyed at line granularity: one row per keyword-bearing source
    line, fenced code excluded;
  - every row carries revision, categories, RFC-2119 level, verbatim clause,
    source pin, disposition and full trace (vector, owner, production path,
    positive test, negative test, evidence artifact);
  - dispositions are deterministic: MAY-level -> optional, the 2025-11-25
    compatibility baseline and deprecated rows -> adapter, all other modern
    rules -> modern.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
import urllib.request
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[1]
GMCP = ROOT / ".agents" / "gmcp"
SPEC = GMCP / "spec"
LEDGER = GMCP / "requirements-ledger.json"
SOURCES = GMCP / "sources.json"
CONTRACT = ROOT / "docs" / "gmcp" / "contract-203-001.md"
ROADMAP_DOC = ROOT / "docs" / "gmcp-roadmap.md"
FIXTURES = GMCP / "fixtures"
EVIDENCE = GMCP / "evidence"

KEYWORD_RE = re.compile(
    r"\b(MUST NOT|MUST|SHALL|SHOULD NOT|SHOULD|REQUIRED|RECOMMENDED|MAY|OPTIONAL)\b"
)
LEVEL_ORDER = ["must_not", "must", "should_not", "should", "may"]
LEVEL_MAP = {
    "MUST NOT": "must_not",
    "MUST": "must",
    "SHALL": "must",
    "REQUIRED": "must",
    "SHOULD NOT": "should_not",
    "SHOULD": "should",
    "RECOMMENDED": "should",
    "MAY": "may",
    "OPTIONAL": "may",
}
CATEGORIES = {"behavior", "error", "deprecation", "security", "extension", "registry", "claim"}
DISPOSITIONS = {"modern", "adapter", "optional"}
VECTORS = {f"VC-203-{n:03d}" for n in range(1, 49)}

VECTOR_RULES = [
    ("security-considerations", "VC-203-025"),
    ("tutorials/security", "VC-203-032"),
    ("basic/authorization", "VC-203-025"),
    ("deprecated", "VC-203-004"),
    ("changelog", "VC-203-001"),
    ("lifecycle", "VC-203-004"),
    ("utilities/ping", "VC-203-004"),
    ("utilities/tasks", "VC-203-004"),
    ("transports/stdio", "VC-203-005"),
    ("streamable-http", "VC-203-006"),
    ("transports", "VC-203-007"),
    ("cancellation", "VC-203-002"),
    ("progress", "VC-203-012"),
    ("patterns/subscriptions", "VC-203-012"),
    ("mrtr", "VC-203-013"),
    ("patterns/index", "VC-203-012"),
    ("server/discover", "VC-203-003"),
    ("server/index", "VC-203-003"),
    ("server/tools", "VC-203-009"),
    ("server/resources", "VC-203-010"),
    ("server/prompts", "VC-203-011"),
    ("completion", "VC-203-011"),
    ("logging", "VC-203-012"),
    ("pagination", "VC-203-008"),
    ("caching", "VC-203-008"),
    ("client/", "VC-203-013"),
    ("versioning", "VC-203-002"),
    ("architecture", "VC-203-002"),
    ("basic/", "VC-203-002"),
    ("schema", "VC-203-008"),
    ("server/", "VC-203-003"),
]
TAG_VECTOR = {
    "EXT-TASKS": "VC-203-014",
    "EXT-APPS": "VC-203-015",
    "EXT-AUTH": "VC-203-027",
    "REG": "VC-203-035",
}
TAG_REVISION = {
    "MCP-2025-1125": "2025-11-25",
    "MCP-2026-0728": "2026-07-28",
    "EXT-TASKS": "ext-tasks@2026-07-28",
    "EXT-APPS": "ext-apps@2026-01-26",
    "EXT-AUTH": "ext-auth@stable",
    "REG": "registry@pinned",
    "CLAIM": "susi@baseline-2026-10-17",
}
OWNER = {
    "VC-203-001": ("gmcp-ledger", ".agents/gmcp/requirements-ledger.json"),
    "VC-203-005": ("susi-gmcp", "crates/susi-gmcp/src/stdio.rs"),
    "VC-203-006": ("susi-gmcp", "crates/susi-gmcp/src/server.rs"),
    "VC-203-007": ("susi-gmcp", "crates/susi-gmcp/src/server.rs"),
    "VC-203-009": ("susi-gmcp", "crates/susi-gmcp/src/tool_registry.rs"),
    "VC-203-014": ("susi-gmcp", "crates/susi-gmcp/src/eco_mcp_features.rs"),
    "VC-203-015": ("susi-gmcp", "crates/susi-gmcp/src/eco_mcp_features.rs"),
    "VC-203-016": ("susi-gmcp", "crates/susi-gmcp/src/eco_mcp_features.rs"),
    "VC-203-017": ("susi-vendor-mcp", "crates/susi-vendor-mcp"),
    "VC-203-018": ("susi-vendor-mcp", "crates/susi-vendor-mcp"),
    "VC-203-024": ("susi-gmcp", "crates/susi-gmcp/src/eco_mcp_auth.rs"),
    "VC-203-025": ("susi-gmcp", "crates/susi-gmcp/src/eco_mcp_auth.rs"),
    "VC-203-026": ("susi-gmcp", "crates/susi-gmcp/src/eco_mcp_auth.rs"),
    "VC-203-027": ("susi-gmcp", "crates/susi-gmcp/src/eco_mcp_auth.rs"),
    "VC-203-031": ("susi-gmcp", "crates/susi-gmcp/src"),
    "VC-203-032": ("susi-gmcp", "crates/susi-gmcp/src/server.rs"),
    "VC-203-035": ("susi-gmcp", "crates/susi-gmcp/src/mcp_registry_trust.rs"),
    "VC-203-038": ("susi-gmcp", "crates/susi-gmcp/src/catalog.rs"),
    "VC-203-039": ("susi-daemon", "crates/susi-daemon/src"),
    "VC-203-042": ("susi-gmcp", "crates/susi-gmcp/src/mcp_call_budgets.rs"),
    "VC-203-045": ("gmcp-tests", "tests/gmcp_requirements.rs"),
    "VC-203-048": ("gmcp-tests", "tests/gmcp_requirements.rs"),
}
DEFAULT_OWNER = ("susi-gmcp", "crates/susi-gmcp/src/protocol.rs")

# Implementation claims Susi publishes about itself; the clause must be a
# verbatim substring of docs/gmcp-roadmap.md (checked like any pinned source).
CLAIMS = [
    {
        "clause": "implements current discovery, core primitives, modern subscriptions, MRTR, tasks, schema checks, and older request handling",
        "vector": "VC-203-003",
        "owner": "susi-gmcp",
        "production_path": "crates/susi-gmcp/src/protocol.rs",
    },
    {
        "clause": "bounded stdio and Streamable HTTP, local auth, origin/host checks, rate and concurrency controls, TLS for remote use",
        "vector": "VC-203-006",
        "owner": "susi-gmcp",
        "production_path": "crates/susi-gmcp/src",
    },
    {
        "clause": "tools, resources, templates, prompts, completion, progress, notifications, and structured outputs have substantial tests",
        "vector": "VC-203-008",
        "owner": "susi-gmcp",
        "production_path": "crates/susi-gmcp/src",
    },
    {
        "clause": "`susi-vendor-mcp` connects over stdio or HTTP and lists/calls tools",
        "vector": "VC-203-017",
        "owner": "susi-vendor-mcp",
        "production_path": "crates/susi-vendor-mcp",
    },
    {
        "clause": "curated leading servers, a static registry, configuration import, health checks, and runtime hot-plugging exist",
        "vector": "VC-203-038",
        "owner": "susi-gmcp",
        "production_path": "crates/susi-gmcp/src/catalog.rs",
    },
    {
        "clause": "bearer/peer controls, untrusted-output policy, data classification, sandbox pieces, and signed audit foundations exist",
        "vector": "VC-203-024",
        "owner": "susi-gmcp",
        "production_path": "crates/susi-gmcp/src",
    },
    {
        "clause": "daemon integration, status/doctor commands, process launch, rate limits, and audit infrastructure exist",
        "vector": "VC-203-039",
        "owner": "susi-daemon",
        "production_path": "crates/susi-daemon/src",
    },
    {
        "clause": "unit/integration tests and a previous lifecycle probe exist",
        "vector": "VC-203-045",
        "owner": "gmcp-tests",
        "production_path": "tests/gmcp_requirements.rs",
    },
]


def sha256_file(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def load_json(path: Path) -> Any:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise SystemExit(f"cannot read valid JSON from {path}: {error}") from error


def source_tag(document: str) -> str:
    if document.startswith("docs/specification/2025-11-25/") or document.startswith("schema/2025-11-25/"):
        return "MCP-2025-1125"
    if document.startswith("docs/specification/2026-07-28/") or document.startswith("schema/2026-07-28/") or document.startswith("docs/docs/2026-07-28/"):
        return "MCP-2026-0728"
    if document.startswith("docs/docs/2025-11-25/"):
        return "MCP-2025-1125"
    if document.startswith("ext-tasks/"):
        return "EXT-TASKS"
    if document.startswith("ext-apps/"):
        return "EXT-APPS"
    if document.startswith("ext-auth/"):
        return "EXT-AUTH"
    if document.startswith("registry/"):
        return "REG"
    raise SystemExit(f"unclassified source document {document}")


def doc_slug(tag: str, document: str) -> str:
    rel = document
    for prefix in (
        "docs/specification/2025-11-25/",
        "docs/specification/2026-07-28/",
        "docs/docs/2025-11-25/",
        "docs/docs/2026-07-28/",
        "docs/",
        "ext-tasks/",
        "ext-apps/",
        "ext-auth/",
        "registry/",
    ):
        if rel.startswith(prefix):
            rel = rel[len(prefix):]
            break
    slug = re.sub(r"\.(mdx|md|ts|json)$", r"-\1", rel)
    slug = re.sub(r"[^A-Za-z0-9]+", "-", slug).strip("-").lower()
    return slug[:60]


def extract_clauses(text: str) -> list[tuple[int, str]]:
    """Return (line_number, clause) for every normative line outside code fences."""
    rows: list[tuple[int, str]] = []
    fenced = False
    for number, raw in enumerate(text.splitlines(), start=1):
        stripped = raw.strip()
        if stripped.startswith("```") or stripped.startswith("~~~"):
            fenced = not fenced
            continue
        if fenced:
            continue
        if KEYWORD_RE.search(stripped):
            rows.append((number, stripped))
    return rows


def level_of(clause: str) -> str:
    levels = {LEVEL_MAP[m] for m in KEYWORD_RE.findall(clause)}
    for level in LEVEL_ORDER:
        if level in levels:
            return level
    raise SystemExit(f"no RFC-2119 level in clause {clause!r}")


def categories_of(tag: str, document: str, clause: str) -> list[str]:
    cats: set[str] = set()
    if tag == "CLAIM":
        cats.add("claim")
    if tag in {"EXT-TASKS", "EXT-APPS", "EXT-AUTH"} or "utilities/tasks" in document:
        cats.add("extension")
    if tag == "REG":
        cats.add("registry")
    low = clause.lower()
    if "security" in document or "security" in low:
        cats.add("security")
    if "deprecat" in low or "deprecated" in document or "changelog" in document:
        cats.add("deprecation")
    if re.search(r"-32\d{3}\b", clause) or ("error" in low and "code" in low):
        cats.add("error")
    if not cats:
        cats.add("behavior")
    return sorted(cats)


def vector_of(tag: str, document: str, clause_vector: str | None) -> str:
    if tag == "CLAIM":
        return clause_vector or "VC-203-001"
    if tag in TAG_VECTOR:
        return TAG_VECTOR[tag]
    rel = document.lower()
    for needle, vector in VECTOR_RULES:
        if needle in rel:
            return vector
    return "VC-203-002"


def disposition_of(tag: str, revision: str, level: str, categories: list[str]) -> dict[str, str]:
    if level == "may":
        return {
            "status": "optional",
            "reason": "MAY/OPTIONAL latitude: conformant whether or not the behavior is implemented; the owning vector decides and records the choice in its evidence.",
        }
    if revision == "2025-11-25":
        return {
            "status": "adapter",
            "reason": "2025-11-25 is the compatibility baseline: honored only through the explicit version adapter with downgrade telemetry and a removal policy, never on the modern path.",
        }
    if "deprecation" in categories:
        return {
            "status": "adapter",
            "reason": "deprecated on the modern path; preserved solely inside the isolated legacy adapter.",
        }
    return {
        "status": "modern",
        "reason": "normative on the 2026-07-28 production path; implemented and evidenced by the owning vector's work packages.",
    }


def trace_of(vector: str, owner: str, production_path: str) -> dict[str, str]:
    suffix = vector.rsplit("-", 1)[1]
    return {
        "vector": vector,
        "owner": owner,
        "production_path": production_path,
        "positive_test": f"tests/gmcp_requirements.rs::gmcp_req_203_{suffix}_03_integration",
        "negative_test": f"tests/gmcp_requirements.rs::gmcp_req_203_{suffix}_04_adversarial",
        "evidence": f".agents/gmcp/evidence/{vector}.jsonl",
    }


def build_rows(files: list[dict[str, Any]]) -> list[dict[str, Any]]:
    rows: list[dict[str, Any]] = []
    for entry in files:
        document = entry["document"]
        path = SPEC / document
        text = path.read_text(encoding="utf-8")
        tag = source_tag(document)
        slug = doc_slug(tag, document)
        revision = TAG_REVISION[tag]
        for line_no, clause in extract_clauses(text):
            level = level_of(clause)
            cats = categories_of(tag, document, clause)
            vector = vector_of(tag, document, None)
            owner, prod = OWNER.get(vector, DEFAULT_OWNER)
            rows.append(
                {
                    "id": f"{tag}-{slug}-{line_no:04d}",
                    "revision": revision,
                    "categories": cats,
                    "level": level,
                    "source": {
                        "repository": entry["repo"],
                        "document": document,
                        "sha256": entry["sha256"],
                        "line": line_no,
                    },
                    "clause": clause,
                    "disposition": disposition_of(tag, revision, level, cats),
                    "trace": trace_of(vector, owner, prod),
                }
            )
    roadmap_text = ROADMAP_DOC.read_text(encoding="utf-8")
    for index, claim in enumerate(CLAIMS, start=1):
        line_no = 0
        for number, raw in enumerate(roadmap_text.splitlines(), start=1):
            if claim["clause"] in raw:
                line_no = number
                break
        if not line_no:
            raise SystemExit(f"claim {index} is not verbatim in docs/gmcp-roadmap.md: {claim['clause']!r}")
        vector = claim["vector"]
        rows.append(
            {
                "id": f"CLAIM-{index:02d}",
                "revision": TAG_REVISION["CLAIM"],
                "categories": ["claim"],
                "level": "must",
                "source": {
                    "repository": "susi",
                    "document": "docs/gmcp-roadmap.md",
                    "sha256": sha256_file(ROADMAP_DOC),
                    "line": line_no,
                },
                "clause": claim["clause"],
                "disposition": {
                    "status": "modern",
                    "reason": "published implementation claim: the program must prove it or retract the claim.",
                },
                "trace": trace_of(vector, claim["owner"], claim["production_path"]),
            }
        )
    rows.sort(key=lambda r: r["id"])
    return rows


def write_mode() -> None:
    sources = load_json(SOURCES)
    files = sources["files"]
    ledger = {
        "schema": "susi/gmcp-requirements/v1",
        "version": "1.0.0",
        "baseline": {
            "generated_unix": int(__import__("time").time()),
            "contract": "docs/gmcp/contract-203-001.md",
            "sources": ".agents/gmcp/sources.json",
            "revisions": sources["revisions"],
        },
        "requirements": build_rows(files),
    }
    for entry in sources.get("repository_documents", []):
        doc = ROOT / entry["document"]
        data = doc.read_bytes()
        entry["sha256"] = hashlib.sha256(data).hexdigest()
        entry["bytes"] = len(data)
    SOURCES.write_text(json.dumps(sources, indent=1) + "\n", encoding="utf-8")
    LEDGER.write_text(json.dumps(ledger, indent=1) + "\n", encoding="utf-8")
    print(f"wrote {LEDGER.relative_to(ROOT)} with {len(ledger['requirements'])} rows")


def check_mode(errors: list[str]) -> None:
    def require(cond: bool, message: str) -> None:
        if not cond:
            errors.append(message)

    require(SOURCES.is_file(), ".agents/gmcp/sources.json missing")
    require(LEDGER.is_file(), ".agents/gmcp/requirements-ledger.json missing")
    require(CONTRACT.is_file(), "docs/gmcp/contract-203-001.md missing")
    if errors:
        return
    sources = load_json(SOURCES)
    ledger = load_json(LEDGER)
    require(ledger.get("schema") == "susi/gmcp-requirements/v1", "ledger schema id wrong")

    doc_hash: dict[str, str] = {}
    seen: set[str] = set()
    for entry in sources.get("files", []):
        document = entry["document"]
        require(document not in seen, f"duplicate manifest entry {document}")
        seen.add(document)
        path = SPEC / document
        if not path.is_file():
            errors.append(f"vendored source missing: {document}")
            continue
        data = path.read_bytes()
        digest = hashlib.sha256(data).hexdigest()
        require(digest == entry["sha256"], f"{document} hash drifted from manifest")
        require(len(data) == entry["bytes"], f"{document} size drifted from manifest")
        doc_hash[document] = digest

    require(
        {entry["document"] for entry in sources["files"]} == seen,
        "manifest/file set mismatch",
    )
    for entry in sources.get("repository_documents", []):
        doc = ROOT / entry["document"]
        require(doc.is_file(), f"repository document missing: {entry['document']}")
        if doc.is_file():
            require(
                sha256_file(doc) == entry["sha256"],
                f"repository document {entry['document']} drifted from pinned hash",
            )

    generated = build_rows(sources["files"])
    rows = ledger.get("requirements", [])
    require(isinstance(rows, list) and len(rows) == len(generated), "ledger row count differs from corpus extraction")
    by_id = {r.get("id"): r for r in rows if isinstance(r, dict)}
    gen_by_id = {r["id"]: r for r in generated}
    require(
        set(by_id) == set(gen_by_id),
        f"ledger ids differ from corpus extraction (missing: {sorted(set(gen_by_id) - set(by_id))[:5]}, extra: {sorted(set(by_id) - set(gen_by_id))[:5]})",
    )
    require(len(by_id) == len(rows), "duplicate row ids in ledger")

    for row in rows:
        rid = row.get("id", "?")
        if not isinstance(row, dict) or rid not in gen_by_id:
            continue
        want = gen_by_id[rid]
        require(row == want, f"{rid}: row content differs from deterministic classification")
        require(row["level"] in LEVEL_MAP.values(), f"{rid}: bad level")
        require(set(row["categories"]) <= CATEGORIES, f"{rid}: bad categories")
        require(row["disposition"]["status"] in DISPOSITIONS, f"{rid}: bad disposition")
        require(bool(row["disposition"].get("reason")), f"{rid}: empty disposition reason")
        trace = row["trace"]
        require(trace["vector"] in VECTORS, f"{rid}: bad vector")
        require(bool(trace.get("owner")), f"{rid}: empty owner")
        require(bool(trace.get("production_path")), f"{rid}: empty production_path")
        require(bool(trace.get("positive_test")), f"{rid}: empty positive_test")
        require(bool(trace.get("negative_test")), f"{rid}: empty negative_test")
        require(bool(trace.get("evidence")), f"{rid}: empty evidence")
        doc = row["source"]["document"]
        require(row["source"]["sha256"] == doc_hash.get(doc) or doc == "docs/gmcp-roadmap.md", f"{rid}: source pin mismatch")
        text = (SPEC / doc).read_text(encoding="utf-8") if (SPEC / doc).is_file() else ROADMAP_DOC.read_text(encoding="utf-8") if doc == "docs/gmcp-roadmap.md" else ""
        require(row["clause"] in text, f"{rid}: clause not verbatim in {doc}")

    revisions = {r["revision"] for r in rows if isinstance(r, dict)}
    require("2025-11-25" in revisions, "compatibility revision unclassified")
    require("2026-07-28" in revisions, "modern revision unclassified")
    require({c for r in rows if isinstance(r, dict) for c in r["categories"]} >= {"behavior", "error", "deprecation", "security", "extension", "claim"}, "a required category has no rows")

    require((FIXTURES / "positive").is_dir() and any((FIXTURES / "positive").iterdir()), "no positive fixtures")
    require((FIXTURES / "negative").is_dir() and any((FIXTURES / "negative").iterdir()), "no negative fixtures")
    require((EVIDENCE / "README.md").is_file(), "evidence location convention undocumented")
    contract = CONTRACT.read_text(encoding="utf-8")
    for token in ("normative", "disposition", "threat model", "slo", "fixture", "evidence", "adapter", "drift"):
        require(token in contract.lower(), f"contract doc missing section token {token!r}")


def drift_mode(errors: list[str]) -> None:
    sources = load_json(SOURCES)
    upstream = sources["upstream"]
    for entry in sources["files"]:
        repo = entry["repo"]
        pin = upstream[repo]
        url = f"https://raw.githubusercontent.com/modelcontextprotocol/{repo}/{pin['commit']}/{entry['upstream_path']}"
        try:
            data = urllib.request.urlopen(url, timeout=30).read()
        except Exception as error:  # fail closed: an unfetchable source is unclassified
            errors.append(f"cannot refetch {url}: {error}")
            continue
        digest = hashlib.sha256(data).hexdigest()
        if digest != entry["sha256"]:
            errors.append(f"upstream drift in {entry['document']}: {digest[:12]} != pinned {entry['sha256'][:12]} — reclassify before closing")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--write", action="store_true", help="regenerate sources manifest hashes and the ledger")
    parser.add_argument("--drift", action="store_true", help="refetch pinned upstreams and fail on any change")
    args = parser.parse_args()
    if args.write:
        write_mode()
        return
    errors: list[str] = []
    if args.drift:
        drift_mode(errors)
    else:
        check_mode(errors)
    if errors:
        for error in errors[:50]:
            print(f"GMCP ledger error: {error}", file=sys.stderr)
        print(f"GMCP ledger invalid: {len(errors)} error(s)", file=sys.stderr)
        raise SystemExit(1)
    if args.drift:
        print("GMCP ledger drift check OK: every pinned upstream still matches")
    else:
        rows = len(load_json(LEDGER).get("requirements", []))
        docs = len(load_json(SOURCES).get("files", []))
        print(f"GMCP ledger OK: {rows} requirements rows across {docs} pinned documents, all classified")


if __name__ == "__main__":
    main()
