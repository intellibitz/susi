//! Unified semantic index over the workspace's `.susi/` knowledge stores.
//!
//! Before this module, `semantic_search` opened a tantivy index that nothing
//! ever wrote to — every query returned "No matches" — and `rag_query` hit a
//! Qdrant collection (`susi_knowledge`) that nothing populated. Both were
//! query-only shells.
//!
//! This index actually ingests:
//! - `memory.jsonl`           — every interaction (intent + outcome)
//! - `reasoning_experience.jsonl` — distilled successful outcomes
//! - `audit.log`              — governance/security event lines
//! - workspace text files     — `.rs .md .txt .toml .json .py .ts` under the
//!   workspace root (skips `.git`, `target`, `.susi`; ≤256 KiB each)
//!
//! Two recall paths:
//! - [`SemanticIndex::search`] — tantivy BM25 full-text with source/snippets.
//! - [`SemanticIndex::vector_recall`] — fastembed cosine similarity over
//!   embeddings persisted to `.susi/vectors.jsonl` (no external vector DB).
//!
//! JSONL sources are append-only, so indexing is incremental via a line-count
//! watermark in `.susi/index/watermark.json`. Files are re-indexed when their
//! mtime changes (old doc deleted by `doc_id` term).

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tantivy::{
    collector::TopDocs, doc, query::QueryParser, schema::*, Index, TantivyDocument, Term,
};

use crate::susi_error::{EaiError, EaiResult};

const MAX_FILE_BYTES: u64 = 256 * 1024;
const FILE_EXTS: &[&str] = &["rs", "md", "txt", "toml", "json", "py", "ts"];
const SNIPPET_LEN: usize = 300;

pub struct SemanticIndex;

/// A single search/recall result.
pub struct Hit {
    pub doc_id: String,
    pub source: String,
    pub score: f32,
    pub snippet: String,
}

#[derive(Default, Serialize, Deserialize)]
struct Watermark {
    /// Append-only JSONL sources: how many lines have been indexed.
    jsonl_lines: HashMap<String, usize>,
    /// Workspace files: path → last-indexed mtime (secs since epoch).
    file_mtimes: HashMap<String, u64>,
    /// doc_ids that have a stored embedding vector.
    embedded: Vec<String>,
}

impl SemanticIndex {
    fn index_dir(workspace: &Path) -> PathBuf {
        workspace.join(".susi/index")
    }

    fn watermark_path(workspace: &Path) -> PathBuf {
        workspace.join(".susi/index/watermark.json")
    }

    fn load_watermark(workspace: &Path) -> Watermark {
        fs::read_to_string(Self::watermark_path(workspace))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn save_watermark(workspace: &Path, wm: &Watermark) {
        if let Ok(s) = serde_json::to_string(wm) {
            let _ = fs::write(Self::watermark_path(workspace), s);
        }
    }

    fn build_schema() -> (Schema, Field, Field, Field) {
        let mut b = Schema::builder();
        let doc_id = b.add_text_field("doc_id", STRING | STORED);
        let source = b.add_text_field("source", STRING | STORED);
        let content = b.add_text_field("content", TEXT | STORED);
        (b.build(), doc_id, source, content)
    }

    fn open(workspace: &Path) -> EaiResult<(Index, Field, Field, Field)> {
        let dir = Self::index_dir(workspace);
        fs::create_dir_all(&dir).map_err(|e| EaiError::filesystem(e.to_string()))?;
        let (schema, doc_id, source, content) = Self::build_schema();
        let index = Index::open_or_create(
            tantivy::directory::MmapDirectory::open(&dir)
                .map_err(|e| EaiError::filesystem(e.to_string()))?,
            schema,
        )
        .map_err(|e| EaiError::filesystem(e.to_string()))?;
        Ok((index, doc_id, source, content))
    }

    /// Index new content from all known `.susi/` stores and workspace files.
    /// Returns the number of documents added or refreshed.
    pub fn refresh(workspace: &Path) -> EaiResult<usize> {
        let (index, doc_id, source, content) = Self::open(workspace)?;
        let mut wm = Self::load_watermark(workspace);
        let mut writer = index
            .writer(50_000_000)
            .map_err(|e| EaiError::process(e.to_string()))?;
        let mut added = 0usize;

        // Append-only JSONL stores — index lines beyond the watermark.
        let susi_dir = workspace.join(".susi");
        for (name, file) in [
            ("memory", "memory.jsonl"),
            ("experience", "reasoning_experience.jsonl"),
            ("audit", "audit.log"),
        ] {
            let path = susi_dir.join(file);
            if !path.exists() {
                continue;
            }
            let already = *wm.jsonl_lines.get(name).unwrap_or(&0);
            let f = fs::File::open(&path).map_err(|e| EaiError::filesystem(e.to_string()))?;
            let mut count = 0usize;
            for line in BufReader::new(f).lines() {
                let line = match line {
                    Ok(l) => l,
                    Err(_) => continue,
                };
                count += 1;
                if count <= already || line.trim().is_empty() {
                    continue;
                }
                // Compact JSONL → readable text for indexing.
                let text = serde_json::from_str::<serde_json::Value>(&line)
                    .map(|v| {
                        let intent = v["intent"].as_str().unwrap_or("");
                        let outcome = v["outcome"]
                            .as_str()
                            .or_else(|| v["successful_outcome"].as_str())
                            .or_else(|| v["details"].as_str())
                            .unwrap_or("");
                        format!("{} {}", intent, outcome).trim().to_string()
                    })
                    .unwrap_or_else(|_| line.clone());
                if text.is_empty() {
                    continue;
                }
                writer
                    .add_document(doc!(
                        doc_id => format!("{}:{}", name, count),
                        source => name,
                        content => text,
                    ))
                    .map_err(|e| EaiError::process(e.to_string()))?;
                added += 1;
            }
            wm.jsonl_lines.insert(name.to_string(), count);
        }

        // Workspace text files — re-index on mtime change.
        let mut stack = vec![workspace.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let entries = match fs::read_dir(&dir) {
                Ok(e) => e,
                Err(_) => continue,
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().to_string();
                if path.is_dir() {
                    if name != ".git" && name != "target" && name != ".susi" {
                        stack.push(path);
                    }
                    continue;
                }
                let ext = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_lowercase();
                if !FILE_EXTS.contains(&ext.as_str()) {
                    continue;
                }
                let meta = match entry.metadata() {
                    Ok(m) if m.len() <= MAX_FILE_BYTES => m,
                    _ => continue,
                };
                let mtime = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let rel = path
                    .strip_prefix(workspace)
                    .unwrap_or(&path)
                    .display()
                    .to_string();
                let id = format!("file:{}", rel);
                if wm.file_mtimes.get(&rel) == Some(&mtime) {
                    continue;
                }
                let text = match fs::read_to_string(&path) {
                    Ok(t) => t,
                    Err(_) => continue,
                };
                writer.delete_term(Term::from_field_text(doc_id, &id));
                writer
                    .add_document(doc!(
                        doc_id => id,
                        source => "file",
                        content => text,
                    ))
                    .map_err(|e| EaiError::process(e.to_string()))?;
                wm.file_mtimes.insert(rel, mtime);
                added += 1;
            }
        }

        writer
            .commit()
            .map_err(|e| EaiError::process(e.to_string()))?;
        Self::save_watermark(workspace, &wm);
        Ok(added)
    }

    /// BM25 full-text search. Calls [`refresh`](Self::refresh) first so the
    /// index reflects the latest `.susi/` writes — incremental, so cheap on
    /// repeat queries.
    pub fn search(workspace: &Path, query_str: &str, limit: usize) -> EaiResult<Vec<Hit>> {
        Self::refresh(workspace)?;
        let (index, doc_id, source, content) = Self::open(workspace)?;
        let reader = index
            .reader()
            .map_err(|e| EaiError::process(e.to_string()))?;
        let searcher = reader.searcher();
        let parser = QueryParser::for_index(&index, vec![content]);
        let query = parser
            .parse_query(query_str)
            .map_err(|e| EaiError::process(e.to_string()))?;
        let top = searcher
            .search(&query, &TopDocs::with_limit(limit).order_by_score())
            .map_err(|e| EaiError::process(e.to_string()))?;

        let mut hits = Vec::new();
        for (score, addr) in top {
            let doc: TantivyDocument = searcher
                .doc(addr)
                .map_err(|e| EaiError::process(e.to_string()))?;
            let get = |f: Field| {
                doc.get_first(f)
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string()
            };
            let text = get(content);
            hits.push(Hit {
                doc_id: get(doc_id),
                source: get(source),
                score,
                snippet: text.chars().take(SNIPPET_LEN).collect(),
            });
        }
        Ok(hits)
    }

    /// Ensure every indexed doc has a stored embedding; embeds only docs not
    /// yet in `vectors.jsonl`. No-op-safe when the embedder can't load.
    fn ensure_embeddings(workspace: &Path) -> EaiResult<()> {
        let mut wm = Self::load_watermark(workspace);
        let embedded: HashMap<String, ()> = wm.embedded.iter().cloned().map(|d| (d, ())).collect();

        let (index, doc_id, _source, content) = Self::open(workspace)?;
        let reader = index
            .reader()
            .map_err(|e| EaiError::process(e.to_string()))?;
        let searcher = reader.searcher();

        // Collect unembedded docs (bounded — the index only grows with real data).
        let all = searcher
            .search(
                &tantivy::query::AllQuery,
                &TopDocs::with_limit(10_000).order_by_score(),
            )
            .map_err(|e| EaiError::process(e.to_string()))?;
        let mut pending: Vec<(String, String)> = Vec::new();
        for (_s, addr) in all {
            let doc: TantivyDocument = searcher
                .doc(addr)
                .map_err(|e| EaiError::process(e.to_string()))?;
            let id = doc
                .get_first(doc_id)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if id.is_empty() || embedded.contains_key(&id) {
                continue;
            }
            let text = doc
                .get_first(content)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .chars()
                .take(2000)
                .collect::<String>();
            if !text.is_empty() {
                pending.push((id, text));
            }
        }
        if pending.is_empty() {
            return Ok(());
        }

        let mut model = fastembed::TextEmbedding::try_new(Default::default())
            .map_err(|e| EaiError::inference(format!("embedder unavailable: {}", e)))?;
        let texts: Vec<String> = pending.iter().map(|(_, t)| t.clone()).collect();
        let vectors = model
            .embed(texts, None)
            .map_err(|e| EaiError::inference(e.to_string()))?;

        let vec_path = workspace.join(".susi/vectors.jsonl");
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&vec_path)
            .map_err(|e| EaiError::filesystem(e.to_string()))?;
        for ((id, _), vec) in pending.iter().zip(vectors.iter()) {
            let line = serde_json::json!({"doc_id": id, "vector": vec});
            let _ = writeln!(f, "{}", line);
            wm.embedded.push(id.clone());
        }
        Self::save_watermark(workspace, &wm);
        Ok(())
    }

    /// Cosine-similarity recall over locally persisted embeddings. Refreshes
    /// the text index first, embeds anything new, then ranks by cosine.
    /// Errors clearly if the fastembed model can't be loaded/downloaded.
    pub fn vector_recall(workspace: &Path, query: &str, limit: usize) -> EaiResult<Vec<Hit>> {
        Self::refresh(workspace)?;
        Self::ensure_embeddings(workspace)?;

        let mut model = fastembed::TextEmbedding::try_new(Default::default())
            .map_err(|e| EaiError::inference(format!("embedder unavailable: {}", e)))?;
        let qv = model
            .embed(vec![query], None)
            .map_err(|e| EaiError::inference(e.to_string()))?
            .into_iter()
            .next()
            .ok_or_else(|| EaiError::inference("query embedding failed"))?;

        let vec_path = workspace.join(".susi/vectors.jsonl");
        let mut scored: Vec<(String, f32)> = Vec::new();
        if let Ok(f) = fs::File::open(&vec_path) {
            for line in BufReader::new(f).lines().map_while(Result::ok) {
                let v: serde_json::Value = match serde_json::from_str(&line) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let id = v["doc_id"].as_str().unwrap_or("").to_string();
                let vec: Vec<f32> = v["vector"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_f64().map(|f| f as f32))
                            .collect()
                    })
                    .unwrap_or_default();
                if vec.len() != qv.len() {
                    continue;
                }
                let dot: f32 = qv.iter().zip(vec.iter()).map(|(a, b)| a * b).sum();
                let na: f32 = qv.iter().map(|x| x * x).sum::<f32>().sqrt();
                let nb: f32 = vec.iter().map(|x| x * x).sum::<f32>().sqrt();
                if na > 0.0 && nb > 0.0 {
                    scored.push((id, dot / (na * nb)));
                }
            }
        }
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);

        // Resolve doc_ids back to stored docs for snippets.
        let (index, doc_id_f, source_f, content_f) = Self::open(workspace)?;
        let reader = index
            .reader()
            .map_err(|e| EaiError::process(e.to_string()))?;
        let searcher = reader.searcher();
        let mut hits = Vec::new();
        for (id, score) in scored {
            let term = Term::from_field_text(doc_id_f, &id);
            let tq =
                tantivy::query::TermQuery::new(term, tantivy::schema::IndexRecordOption::Basic);
            if let Ok(top) = searcher.search(&tq, &TopDocs::with_limit(1).order_by_score()) {
                if let Some((_, addr)) = top.into_iter().next() {
                    if let Ok(doc) = searcher.doc::<TantivyDocument>(addr) {
                        let get = |f: Field| {
                            doc.get_first(f)
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string()
                        };
                        hits.push(Hit {
                            doc_id: id,
                            source: get(source_f),
                            score,
                            snippet: get(content_f).chars().take(SNIPPET_LEN).collect(),
                        });
                    }
                }
            }
        }
        Ok(hits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_workspace(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("susi_semidx_{}_{}", std::process::id(), tag));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(".susi")).unwrap();
        dir
    }

    #[test]
    fn indexes_memory_jsonl_and_finds_it() {
        let ws = temp_workspace("mem");
        fs::write(
            ws.join(".susi/memory.jsonl"),
            "{\"intent\":\"fix the crash in scheduler\",\"outcome\":\"patched dispatch ordering\"}\n",
        )
        .unwrap();
        let hits = SemanticIndex::search(&ws, "crash scheduler", 5).unwrap();
        assert!(!hits.is_empty(), "memory entry must be searchable");
        assert_eq!(hits[0].source, "memory");
        assert!(hits[0].snippet.contains("patched dispatch"));
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn refresh_is_incremental() {
        let ws = temp_workspace("incr");
        fs::write(
            ws.join(".susi/memory.jsonl"),
            "{\"intent\":\"a\",\"outcome\":\"b\"}\n",
        )
        .unwrap();
        assert_eq!(SemanticIndex::refresh(&ws).unwrap(), 1);
        // Second refresh adds nothing.
        assert_eq!(SemanticIndex::refresh(&ws).unwrap(), 0);
        // Appending a line adds exactly one doc.
        use std::io::Write;
        let mut f = fs::OpenOptions::new()
            .append(true)
            .open(ws.join(".susi/memory.jsonl"))
            .unwrap();
        writeln!(f, "{{\"intent\":\"c\",\"outcome\":\"d\"}}").unwrap();
        assert_eq!(SemanticIndex::refresh(&ws).unwrap(), 1);
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn indexes_workspace_files() {
        let ws = temp_workspace("files");
        fs::write(ws.join("notes.md"), "the quorum commit lives in amas.rs").unwrap();
        let hits = SemanticIndex::search(&ws, "quorum commit", 5).unwrap();
        assert!(hits.iter().any(|h| h.doc_id == "file:notes.md"));
        let _ = fs::remove_dir_all(&ws);
    }
}
