//! utopia-search: an embedded Tantivy full-text index (jieba Chinese tokenization) plus RRF
//! fusion.
//! One index, many KBs: kb_id is the filter field; chunk bodies live in Postgres, the index
//! only holds the id mapping.

mod docs;
pub use docs::{DocsIndex, DocsSection};

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use anyhow::Context;
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, Occur, Query, TermQuery};
use tantivy::schema::{
    Field, IndexRecordOption, Schema, TextFieldIndexing, TextOptions, Value, STORED, STRING,
};
use tantivy::tokenizer::TextAnalyzer;
use tantivy::{Index, IndexReader, IndexWriter, TantivyDocument, Term};

const JIEBA: &str = "jieba";
const WRITER_HEAP: usize = 64 * 1024 * 1024;

pub struct SearchIndex {
    writer: Mutex<IndexWriter>,
    reader: IndexReader,
    analyzer: TextAnalyzer,
    f_chunk_id: Field,
    f_kb_id: Field,
    f_document_id: Field,
    f_text: Field,
}

#[derive(Debug, Clone)]
pub struct Hit {
    pub chunk_id: String,
    pub score: f32,
}

impl SearchIndex {
    pub fn open(dir: &Path) -> anyhow::Result<Self> {
        std::fs::create_dir_all(dir)?;

        let mut schema_builder = Schema::builder();
        let text_indexing = TextFieldIndexing::default()
            .set_tokenizer(JIEBA)
            .set_index_option(IndexRecordOption::WithFreqsAndPositions);
        let f_chunk_id = schema_builder.add_text_field("chunk_id", STRING | STORED);
        let f_kb_id = schema_builder.add_text_field("kb_id", STRING);
        let f_document_id = schema_builder.add_text_field("document_id", STRING);
        let f_text = schema_builder.add_text_field(
            "text",
            TextOptions::default().set_indexing_options(text_indexing),
        );
        let schema = schema_builder.build();

        let mmap =
            tantivy::directory::MmapDirectory::open(dir).context("Failed to open index dir")?;
        let index =
            Index::open_or_create(mmap, schema).context("Failed to open/create Tantivy index")?;
        index
            .tokenizers()
            .register(JIEBA, tantivy_jieba::JiebaTokenizer::new());

        let writer = index.writer(WRITER_HEAP)?;
        let reader = index.reader()?;
        let analyzer = index
            .tokenizers()
            .get(JIEBA)
            .context("jieba tokenizer is not registered")?;

        Ok(Self {
            writer: Mutex::new(writer),
            reader,
            analyzer,
            f_chunk_id,
            f_kb_id,
            f_document_id,
            f_text,
        })
    }

    /// Rebuild one document's index entries (delete first, then add, so it is idempotent),
    /// then commit.
    pub fn reindex_document(
        &self,
        kb_id: &str,
        document_id: &str,
        chunks: &[(String, String)],
    ) -> anyhow::Result<()> {
        let mut writer = self.writer.lock().expect("writer lock poisoned");
        writer.delete_term(Term::from_field_text(self.f_document_id, document_id));
        for (chunk_id, text) in chunks {
            let mut doc = TantivyDocument::default();
            doc.add_text(self.f_chunk_id, chunk_id);
            doc.add_text(self.f_kb_id, kb_id);
            doc.add_text(self.f_document_id, document_id);
            doc.add_text(self.f_text, text);
            writer.add_document(doc)?;
        }
        writer.commit()?;
        // Readable immediately after the write: do not wait out the reader's async reload
        // window
        self.reader.reload()?;
        Ok(())
    }

    pub fn delete_document(&self, document_id: &str) -> anyhow::Result<()> {
        let mut writer = self.writer.lock().expect("writer lock poisoned");
        writer.delete_term(Term::from_field_text(self.f_document_id, document_id));
        writer.commit()?;
        self.reader.reload()?;
        Ok(())
    }

    /// How many chunks the index holds. **At startup, reconcile it against the count in the
    /// database** -- the index directory is a set of files independent of the database (a new
    /// machine, an unmounted volume, or corruption can all leave it empty), and once it is
    /// empty, search just silently returns nothing with no sign of anything wrong in the UI.
    pub fn len(&self) -> usize {
        self.reader.searcher().num_docs() as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// BM25 search (scoped to one kb).
    /// The query is tokenized with exactly the same jieba analyzer as the index and the terms
    /// are combined with OR -- we cannot go through QueryParser: a whole CJK sentence gets
    /// treated as a phrase query (the words must appear consecutively), and recall drops to
    /// zero.
    pub fn search(&self, kb_id: &str, query: &str, limit: usize) -> anyhow::Result<Vec<Hit>> {
        let mut analyzer = self.analyzer.clone();
        let mut stream = analyzer.token_stream(query);
        let mut term_queries: Vec<(Occur, Box<dyn Query>)> = Vec::new();
        while stream.advance() {
            let token = stream.token();
            let word = token.text.trim();
            if word.is_empty() || word.chars().all(|c| !c.is_alphanumeric()) {
                continue;
            }
            term_queries.push((
                Occur::Should,
                Box::new(TermQuery::new(
                    Term::from_field_text(self.f_text, word),
                    IndexRecordOption::WithFreqs,
                )),
            ));
        }
        if term_queries.is_empty() {
            return Ok(Vec::new());
        }

        let kb_filter: Box<dyn Query> = Box::new(TermQuery::new(
            Term::from_field_text(self.f_kb_id, kb_id),
            IndexRecordOption::Basic,
        ));
        let text_query: Box<dyn Query> = Box::new(BooleanQuery::new(term_queries));
        let combined = BooleanQuery::new(vec![(Occur::Must, kb_filter), (Occur::Must, text_query)]);

        let searcher = self.reader.searcher();
        let collector = TopDocs::with_limit(limit).order_by_score();
        let top = searcher.search(&combined, &collector)?;
        let mut hits = Vec::with_capacity(top.len());
        for (score, addr) in top {
            let doc: TantivyDocument = searcher.doc(addr)?;
            if let Some(chunk_id) = doc.get_first(self.f_chunk_id).and_then(|v| v.as_str()) {
                hits.push(Hit {
                    chunk_id: chunk_id.to_string(),
                    score,
                });
            }
        }
        Ok(hits)
    }
}

/// Reciprocal Rank Fusion: fuses the rankings from several retrieval paths (k=60 is the
/// usual empirical constant).
pub fn rrf_fuse(lists: &[Vec<String>], limit: usize) -> Vec<String> {
    const K: f64 = 60.0;
    let mut scores: HashMap<String, f64> = HashMap::new();
    for list in lists {
        for (rank, id) in list.iter().enumerate() {
            *scores.entry(id.clone()).or_default() += 1.0 / (K + rank as f64 + 1.0);
        }
    }
    let mut ranked: Vec<(String, f64)> = scores.into_iter().collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    ranked.into_iter().take(limit).map(|(id, _)| id).collect()
}
