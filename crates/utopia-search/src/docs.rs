//! In-memory index over the built-in docs (the Charter): built from the bundled markdown at
//! startup, read-only for the life of the process.
//! Protocol-neutral -- chat's search_docs tool arm and the future MCP tool surface share this one
//! entry point.

use anyhow::Context;
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, Occur, Query, TermQuery};
use tantivy::schema::{
    Field, IndexRecordOption, Schema, TextFieldIndexing, TextOptions, Value, STORED,
};
use tantivy::tokenizer::TextAnalyzer;
use tantivy::{Index, IndexReader, TantivyDocument, Term};

const JIEBA: &str = "jieba";

/// One section of a doc (split at h2; the anchor is generated the same way as the heading
/// anchors on the frontend Docs page).
#[derive(Debug, Clone)]
pub struct DocsSection {
    pub slug: String,
    pub title: String,
    pub heading: String,
    pub anchor: String,
    pub body: String,
}

pub struct DocsIndex {
    reader: IndexReader,
    analyzer: TextAnalyzer,
    f_slug: Field,
    f_title: Field,
    f_heading: Field,
    f_anchor: Field,
    f_body: Field,
    f_text: Field,
}

impl DocsIndex {
    pub fn build(sections: &[DocsSection]) -> anyhow::Result<Self> {
        let mut schema_builder = Schema::builder();
        let text_indexing = TextFieldIndexing::default()
            .set_tokenizer(JIEBA)
            .set_index_option(IndexRecordOption::WithFreqsAndPositions);
        let f_slug = schema_builder.add_text_field("slug", STORED);
        let f_title = schema_builder.add_text_field("title", STORED);
        let f_heading = schema_builder.add_text_field("heading", STORED);
        let f_anchor = schema_builder.add_text_field("anchor", STORED);
        let f_body = schema_builder.add_text_field("body", STORED);
        // Search field = section heading + body (heading terms earn their weight naturally, by
        // appearing twice)
        let f_text = schema_builder.add_text_field(
            "text",
            TextOptions::default().set_indexing_options(text_indexing),
        );
        let schema = schema_builder.build();

        let index = Index::create_in_ram(schema);
        index
            .tokenizers()
            .register(JIEBA, tantivy_jieba::JiebaTokenizer::new());

        let mut writer = index.writer(16 * 1024 * 1024)?;
        for s in sections {
            let mut doc = TantivyDocument::default();
            doc.add_text(f_slug, &s.slug);
            doc.add_text(f_title, &s.title);
            doc.add_text(f_heading, &s.heading);
            doc.add_text(f_anchor, &s.anchor);
            doc.add_text(f_body, &s.body);
            doc.add_text(f_text, format!("{}\n{}", s.heading, s.body));
            writer.add_document(doc)?;
        }
        writer.commit()?;

        let reader = index.reader()?;
        let analyzer = index
            .tokenizers()
            .get(JIEBA)
            .context("jieba tokenizer is not registered")?;
        Ok(Self {
            reader,
            analyzer,
            f_slug,
            f_title,
            f_heading,
            f_anchor,
            f_body,
            f_text,
        })
    }

    /// BM25 search. Tokenizes the same way SearchIndex does (a hand-built OR combination, which
    /// sidesteps the CJK phrase-query trap).
    pub fn search(&self, query: &str, limit: usize) -> anyhow::Result<Vec<DocsSection>> {
        let mut analyzer = self.analyzer.clone();
        let mut stream = analyzer.token_stream(query);
        let mut term_queries: Vec<(Occur, Box<dyn Query>)> = Vec::new();
        while stream.advance() {
            let word = stream.token().text.trim().to_string();
            if word.is_empty() || word.chars().all(|c| !c.is_alphanumeric()) {
                continue;
            }
            term_queries.push((
                Occur::Should,
                Box::new(TermQuery::new(
                    Term::from_field_text(self.f_text, &word),
                    IndexRecordOption::WithFreqs,
                )),
            ));
        }
        if term_queries.is_empty() {
            return Ok(Vec::new());
        }

        let searcher = self.reader.searcher();
        let top = searcher.search(
            &BooleanQuery::new(term_queries),
            &TopDocs::with_limit(limit).order_by_score(),
        )?;
        let get = |doc: &TantivyDocument, f: Field| {
            doc.get_first(f)
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string()
        };
        let mut hits = Vec::with_capacity(top.len());
        for (_score, addr) in top {
            let doc: TantivyDocument = searcher.doc(addr)?;
            hits.push(DocsSection {
                slug: get(&doc, self.f_slug),
                title: get(&doc, self.f_title),
                heading: get(&doc, self.f_heading),
                anchor: get(&doc, self.f_anchor),
                body: get(&doc, self.f_body),
            });
        }
        Ok(hits)
    }
}
