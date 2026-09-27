//! The two counts in `proposed_predicates`, exercised against a real database.
//!
//! Same reason as the [`graph_changes`] test: this logic lives entirely inside SQL strings,
//! where `cargo check` and clippy cannot see a single character of it. This one needs it most
//! of all -- the two numbers it distinguishes look identical (both are `count(DISTINCT …)`),
//! you cannot tell them apart by reading the code, and they only come apart on a data shape
//! where "some of the facts have already left the backlog".
//!
//! The behavior being pinned down:
//!
//! - **`fact_count` counts from the backlog** -- those are the ones adoption will really
//!   rewrite, and counting one more means lying when we promise "will reclassify N facts"
//! - **`doc_count` counts from all evidence** -- it answers "how widespread is this wording in
//!   the corpus". Counting from the backlog is systematically low, and gets lower the more the
//!   system is used: once a wording is adopted, caught by predicate matching, or invalidated
//!   by a correction, its row leaves the backlog. A knowledge base fed one document at a time
//!   therefore never accumulates two documents, and the ontology freezes solid right there
//!
//! Skips instead of failing when `UTOPIA_DATABASE_URL` is absent. Builds and tears down its
//! own data; never touches an existing database.

use sqlx::PgPool;
use uuid::Uuid;

/// Build a ledger that is just enough to tell the two counting conventions apart:
///
/// The wording `acquired` shows up in **two** documents, but only one of those facts
/// **still has no predicate** -- the other one has already landed on a real relation (caught
/// by predicate matching, or carried off by an earlier round of adoption).
///
/// So: the backlog convention gives doc_count = 1 (killed off by the `>=2` threshold), the
/// full convention gives 2.
async fn seed(pool: &PgPool) -> anyhow::Result<Uuid> {
    let (org, ws, kb) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    let etype = Uuid::now_v7();
    let real = Uuid::now_v7();
    let (subj, obj) = (Uuid::now_v7(), Uuid::now_v7());
    let (doc1, doc2) = (Uuid::now_v7(), Uuid::now_v7());
    let (chunk1, chunk2) = (Uuid::now_v7(), Uuid::now_v7());
    let (f_backlog, f_moved) = (Uuid::now_v7(), Uuid::now_v7());

    sqlx::query("INSERT INTO organizations (id, name) VALUES ($1, 'proposal-counts-test')")
        .bind(org)
        .execute(pool)
        .await?;
    sqlx::query(
        "INSERT INTO workspaces (id, org_id, name) VALUES ($1, $2, 'proposal-counts-test')",
    )
    .bind(ws)
    .bind(org)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO knowledge_bases (id, workspace_id, name)
         VALUES ($1, $2, 'proposal-counts-test')",
    )
    .bind(kb)
    .bind(ws)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO entity_types (id, kb_id, key, label) VALUES ($1, $2, 'thing', 'Thing')",
    )
    .bind(etype)
    .bind(kb)
    .execute(pool)
    .await?;
    // The backlogged one hangs off no relation -- the query filters on predicate_id IS NULL
    // (see `facts.predicate_id`)
    sqlx::query("INSERT INTO relation_types (id, kb_id, key, label) VALUES ($1, $2, $3, $3)")
        .bind(real)
        .bind(kb)
        .bind("acquired_rel")
        .execute(pool)
        .await?;
    for (id, name) in [(subj, "Anthropic"), (obj, "Humanloop")] {
        sqlx::query(
            "INSERT INTO entities (id, kb_id, type_id, canonical_name) VALUES ($1, $2, $3, $4)",
        )
        .bind(id)
        .bind(kb)
        .bind(etype)
        .bind(name)
        .execute(pool)
        .await?;
    }
    for (id, name) in [(doc1, "one.txt"), (doc2, "two.txt")] {
        sqlx::query("INSERT INTO documents (id, kb_id, filename, sha256) VALUES ($1, $2, $3, $3)")
            .bind(id)
            .bind(kb)
            .bind(name)
            .execute(pool)
            .await?;
    }
    for (id, doc) in [(chunk1, doc1), (chunk2, doc2)] {
        sqlx::query(
            "INSERT INTO chunks (id, kb_id, document_id, seq, text) VALUES ($1, $2, $3, 0, 'x')",
        )
        .bind(id)
        .bind(kb)
        .bind(doc)
        .execute(pool)
        .await?;
    }

    // One fact still has no predicate, the other has already landed on a real relation
    for (id, pred) in [(f_backlog, None), (f_moved, Some(real))] {
        sqlx::query(
            "INSERT INTO facts (id, kb_id, subject_id, predicate_id, object_id)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(id)
        .bind(kb)
        .bind(subj)
        .bind(pred)
        .bind(obj)
        .execute(pool)
        .await?;
    }
    // Both pieces of evidence record the same original wording -- extraction records it
    // unconditionally, not only when it falls back
    for (fact, chunk, doc) in [(f_backlog, chunk1, doc1), (f_moved, chunk2, doc2)] {
        sqlx::query(
            "INSERT INTO fact_evidence (fact_id, chunk_id, document_id, proposed_predicate)
             VALUES ($1, $2, $3, 'acquired')",
        )
        .bind(fact)
        .bind(chunk)
        .bind(doc)
        .execute(pool)
        .await?;
    }

    Ok(kb)
}

#[tokio::test]
async fn spread_counts_all_evidence_while_rewrite_count_stays_on_the_backlog() -> anyhow::Result<()>
{
    let Some(url) = utopia_store::test_db::url() else {
        return Ok(());
    };
    let pool = PgPool::connect(&url).await?;
    let kb = seed(&pool).await?;

    let got = utopia_store::graph::proposed_predicates(&pool, kb).await;

    sqlx::query("DELETE FROM knowledge_bases WHERE id = $1")
        .bind(kb)
        .execute(&pool)
        .await?;

    let rows = got?;
    let row = rows
        .iter()
        .find(|r| r.form == "acquired")
        .expect("acquired should be among the proposals");

    // Only one fact still has no predicate -- that is the only one adoption will rewrite
    assert_eq!(
        row.fact_count, 1,
        "fact_count should count only the backlog"
    );
    // But this wording spans two documents. Counting from the backlog gives 1, so the `>=2`
    // threshold kills it off, even though it is plainly shared vocabulary in this corpus
    assert_eq!(row.doc_count, 2, "doc_count should count all evidence");
    Ok(())
}

/// The adoption path groups by inflectional stem, and the document count after grouping has
/// to be the **union** -- so it does not look at `doc_count`, it asks "which documents does
/// this wording appear in" instead. That is a different SQL statement, so it needs its own
/// test that really runs it: it once lost the quotes around `'relation_type'`
/// (`m.kind = relation_type`), clippy was all green, and the job only blew up at runtime.
#[tokio::test]
async fn document_ids_come_back_for_each_wording() -> anyhow::Result<()> {
    let Some(url) = utopia_store::test_db::url() else {
        return Ok(());
    };
    let pool = PgPool::connect(&url).await?;
    let kb = seed(&pool).await?;

    let got = utopia_store::graph::proposed_predicate_documents(&pool, kb).await;

    sqlx::query("DELETE FROM knowledge_bases WHERE id = $1")
        .bind(kb)
        .execute(&pool)
        .await?;

    let rows = got?;
    let docs: std::collections::HashSet<_> = rows
        .iter()
        .filter(|(form, _)| form == "acquired")
        .map(|(_, doc)| *doc)
        .collect();
    // Both documents used this wording, even though only one of those facts still has no
    // predicate
    assert_eq!(
        docs.len(),
        2,
        "both documents should come back, whichever predicate the fact hit"
    );
    Ok(())
}
