#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

//! # susi-vendor-tantivy
//!
//! The one crate that links tantivy. A [`DocIndex`] stores documents with a
//! fixed three-field schema — `doc_id` (exact, stored), `source` (exact,
//! stored), `content` (tokenized, stored) — and offers BM25 search, lookup
//! by id, and full enumeration. No tantivy type crosses this boundary.

use std::path::Path;
use susi_error::{EaiError, EaiResult};
use tantivy::collector::TopDocs;
use tantivy::query::{AllQuery, QueryParser, TermQuery};
use tantivy::schema::{Field, IndexRecordOption, Schema, Value, STORED, STRING, TEXT};
use tantivy::{doc, Index, IndexWriter, TantivyDocument, Term};

/// One stored document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredDoc {
    pub doc_id: String,
    pub source: String,
    pub content: String,
}

/// An on-disk (mmap) document index.
pub struct DocIndex {
    index: Index,
    doc_id: Field,
    source: Field,
    content: Field,
}

fn err(e: impl std::fmt::Display) -> EaiError {
    EaiError::process(e.to_string())
}

impl DocIndex {
    /// Open the index in `dir`, creating the directory and index if absent.
    ///
    /// # Errors
    /// The directory cannot be created or holds an incompatible index.
    pub fn open_or_create(dir: &Path) -> EaiResult<Self> {
        std::fs::create_dir_all(dir).map_err(|e| EaiError::filesystem(e.to_string()))?;
        let mut builder = Schema::builder();
        let doc_id = builder.add_text_field("doc_id", STRING | STORED);
        let source = builder.add_text_field("source", STRING | STORED);
        let content = builder.add_text_field("content", TEXT | STORED);
        let directory = tantivy::directory::MmapDirectory::open(dir)
            .map_err(|e| EaiError::filesystem(e.to_string()))?;
        let index = Index::open_or_create(directory, builder.build())
            .map_err(|e| EaiError::filesystem(e.to_string()))?;
        Ok(Self {
            index,
            doc_id,
            source,
            content,
        })
    }

    /// A writer with a `heap_bytes` indexing budget. Changes are visible to
    /// readers after [`DocWriter::commit`].
    ///
    /// # Errors
    /// Another writer holds the index lock, or the budget is too small.
    pub fn writer(&self, heap_bytes: usize) -> EaiResult<DocWriter<'_>> {
        Ok(DocWriter {
            writer: self.index.writer(heap_bytes).map_err(err)?,
            index: self,
        })
    }

    fn to_doc(&self, doc: &TantivyDocument) -> StoredDoc {
        let get = |f: Field| {
            doc.get_first(f)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        };
        StoredDoc {
            doc_id: get(self.doc_id),
            source: get(self.source),
            content: get(self.content),
        }
    }

    /// BM25 search over `content`; best `limit` hits with scores.
    ///
    /// # Errors
    /// Unparseable query or index read failure.
    pub fn search(&self, query: &str, limit: usize) -> EaiResult<Vec<(f32, StoredDoc)>> {
        let searcher = self.index.reader().map_err(err)?.searcher();
        let query = QueryParser::for_index(&self.index, vec![self.content])
            .parse_query(query)
            .map_err(err)?;
        let top = searcher
            .search(&query, &TopDocs::with_limit(limit).order_by_score())
            .map_err(err)?;
        top.into_iter()
            .map(|(score, addr)| {
                let doc: TantivyDocument = searcher.doc(addr).map_err(err)?;
                Ok((score, self.to_doc(&doc)))
            })
            .collect()
    }

    /// Up to `limit` stored documents, in index order.
    ///
    /// # Errors
    /// Index read failure.
    pub fn all_docs(&self, limit: usize) -> EaiResult<Vec<StoredDoc>> {
        let searcher = self.index.reader().map_err(err)?.searcher();
        let top = searcher
            .search(&AllQuery, &TopDocs::with_limit(limit).order_by_score())
            .map_err(err)?;
        top.into_iter()
            .map(|(_, addr)| {
                let doc: TantivyDocument = searcher.doc(addr).map_err(err)?;
                Ok(self.to_doc(&doc))
            })
            .collect()
    }

    /// The document with exactly this `doc_id`, if present.
    ///
    /// # Errors
    /// Index read failure.
    pub fn get(&self, doc_id: &str) -> EaiResult<Option<StoredDoc>> {
        let searcher = self.index.reader().map_err(err)?.searcher();
        let query = TermQuery::new(
            Term::from_field_text(self.doc_id, doc_id),
            IndexRecordOption::Basic,
        );
        let top = searcher
            .search(&query, &TopDocs::with_limit(1).order_by_score())
            .map_err(err)?;
        match top.into_iter().next() {
            Some((_, addr)) => {
                let doc: TantivyDocument = searcher.doc(addr).map_err(err)?;
                Ok(Some(self.to_doc(&doc)))
            }
            None => Ok(None),
        }
    }
}

/// Buffered writes against a [`DocIndex`].
pub struct DocWriter<'a> {
    writer: IndexWriter,
    index: &'a DocIndex,
}

impl DocWriter<'_> {
    /// Append a document (no de-duplication).
    ///
    /// # Errors
    /// The writer rejected the document.
    pub fn add(&mut self, doc_id: &str, source: &str, content: &str) -> EaiResult<()> {
        let ix = self.index;
        self.writer
            .add_document(doc!(
                ix.doc_id => doc_id,
                ix.source => source,
                ix.content => content,
            ))
            .map(|_| ())
            .map_err(err)
    }

    /// Delete any document with this `doc_id`, then add the new version.
    ///
    /// # Errors
    /// The writer rejected the document.
    pub fn replace(&mut self, doc_id: &str, source: &str, content: &str) -> EaiResult<()> {
        self.writer
            .delete_term(Term::from_field_text(self.index.doc_id, doc_id));
        self.add(doc_id, source, content)
    }

    /// Make every buffered change durable and visible.
    ///
    /// # Errors
    /// The commit failed; buffered changes are lost.
    pub fn commit(mut self) -> EaiResult<()> {
        self.writer.commit().map(|_| ()).map_err(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_replace_search_get_roundtrip() {
        let dir = std::env::temp_dir().join(format!("susi_vendor_tantivy_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let index = DocIndex::open_or_create(&dir).unwrap();
        let mut w = index.writer(15_000_000).unwrap();
        w.add("a:1", "memory", "the quick brown fox").unwrap();
        w.replace("file:x", "file", "old text").unwrap();
        w.commit().unwrap();
        let mut w = index.writer(15_000_000).unwrap();
        w.replace("file:x", "file", "new lazy dog").unwrap();
        w.commit().unwrap();
        assert_eq!(index.search("fox", 5).unwrap()[0].1.doc_id, "a:1");
        assert_eq!(
            index.get("file:x").unwrap().unwrap().content,
            "new lazy dog"
        );
        assert_eq!(index.all_docs(10).unwrap().len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
