//! Subject/object swapping on adoption, run against a real database.
//!
//! `X produced_by Y` and `Y produces X` are the same edge. Fail to swap when adopting the passive
//! wording and the graph grows an extra arrow pointing the other way -- one that will never join
//! up with the forward ones.
//!
//! This one can only be tested for real: the swap happens inside `adopt`'s INSERT, and
//! `cargo check` cannot see SQL. It really did slip through in practice -- in the `demo-b3` base,
//! `produced_by` and `produces` each became a relation of their own, because the adoption path
//! never went through the matcher at all.

use sqlx::PgPool;
use uuid::Uuid;

#[tokio::test]
async fn adopting_a_passive_wording_flips_subject_and_object() -> anyhow::Result<()> {
    let Some(url) = utopia_store::test_db::url() else {
        return Ok(());
    };
    let pool = PgPool::connect(&url).await?;
    let (org, ws, kb) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    let etype = Uuid::now_v7();
    let produces = Uuid::now_v7();
    let (openai, chatgpt) = (Uuid::now_v7(), Uuid::now_v7());
    let (doc, chunk, fact) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());

    sqlx::query("INSERT INTO organizations (id, name) VALUES ($1, 'swap-test')")
        .bind(org)
        .execute(&pool)
        .await?;
    sqlx::query("INSERT INTO workspaces (id, org_id, name) VALUES ($1, $2, 'swap-test')")
        .bind(ws)
        .bind(org)
        .execute(&pool)
        .await?;
    sqlx::query(
        "INSERT INTO knowledge_bases (id, workspace_id, name) VALUES ($1, $2, 'swap-test')",
    )
    .bind(kb)
    .bind(ws)
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO entity_types (id, kb_id, key, label) VALUES ($1, $2, 'thing', 'Thing')",
    )
    .bind(etype)
    .bind(kb)
    .execute(&pool)
    .await?;
    sqlx::query("INSERT INTO relation_types (id, kb_id, key, label) VALUES ($1, $2, $3, $3)")
        .bind(produces)
        .bind(kb)
        .bind("produces")
        .execute(&pool)
        .await?;
    for (id, name) in [(openai, "OpenAI"), (chatgpt, "ChatGPT")] {
        sqlx::query(
            "INSERT INTO entities (id, kb_id, type_id, canonical_name) VALUES ($1,$2,$3,$4)",
        )
        .bind(id)
        .bind(kb)
        .bind(etype)
        .bind(name)
        .execute(&pool)
        .await?;
    }
    sqlx::query("INSERT INTO documents (id, kb_id, filename, sha256) VALUES ($1,$2,'a.txt','a')")
        .bind(doc)
        .bind(kb)
        .execute(&pool)
        .await?;
    sqlx::query("INSERT INTO chunks (id, kb_id, document_id, seq, text) VALUES ($1,$2,$3,0,'x')")
        .bind(chunk)
        .bind(kb)
        .bind(doc)
        .execute(&pool)
        .await?;
    // the source text says "ChatGPT produced_by OpenAI", the ontology has no such relation, and
    // so this fact has **no predicate** -- the fallback predicate no longer exists
    sqlx::query(
        "INSERT INTO facts (id, kb_id, subject_id, predicate_id, object_id)
         VALUES ($1, $2, $3, NULL, $4)",
    )
    .bind(fact)
    .bind(kb)
    .bind(chatgpt)
    .bind(openai)
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO fact_evidence (fact_id, chunk_id, document_id, proposed_predicate)
         VALUES ($1, $2, $3, 'produced_by')",
    )
    .bind(fact)
    .bind(chunk)
    .bind(doc)
    .execute(&pool)
    .await?;

    let run = async {
        let moved = utopia_store::graph::adopt_proposed_predicates(
            &pool,
            kb,
            produces,
            &["produced_by".to_string()],
            true,
        )
        .await?
        .moved;
        assert_eq!(moved, 1);
        // after the rewrite it should read OpenAI -[produces]-> ChatGPT, **direction flipped**
        let (s, o): (Uuid, Option<Uuid>) = sqlx::query_as(
            "SELECT subject_id, object_id FROM facts
             WHERE kb_id = $1 AND predicate_id = $2 AND invalidated_at IS NULL",
        )
        .bind(kb)
        .bind(produces)
        .fetch_one(&pool)
        .await?;
        assert_eq!(
            s, openai,
            "the subject should be OpenAI (it used to be the object)"
        );
        assert_eq!(
            o,
            Some(chatgpt),
            "the object should be ChatGPT (it used to be the subject)"
        );
        Ok::<_, anyhow::Error>(())
    }
    .await;

    sqlx::query("DELETE FROM knowledge_bases WHERE id = $1")
        .bind(kb)
        .execute(&pool)
        .await?;
    run
}
