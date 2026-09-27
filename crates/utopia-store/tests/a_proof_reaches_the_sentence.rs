//! R2: the proof of a derivation has to be readable all the way down to the original sentence
//! (`docs/decisions/0002`).
//!
//! Premises are always assertions (`fact_derivations` does not record derivations), so a proof is
//! a chain: derivation → assertions ordered by `seq` → the evidence of each assertion → chunk.
//! Three things are guarded here:
//!
//! 1. **The order is right**. `A part_of B` and `B part_of C` derive `A part_of C`, and the first
//!    step of the proof is A→B.
//! 2. **The leaf is the original sentence**. Every step carries its own evidence, and the quote is
//!    the sentence it was extracted from in the first place.
//! 3. **A retracted premise is still listed, and marked**. The derivation is invalidated with it,
//!    and `proof` can still show what it rested on at the time.
//!
//! Skips rather than fails when `UTOPIA_DATABASE_URL` is absent. Builds and tears down its own
//! data, and never touches anything already there.

use sqlx::PgPool;
use utopia_store::reasoning;
use uuid::Uuid;

struct Fixture {
    org: Uuid,
    kb: Uuid,
    part_of: Uuid,
    a: Uuid,
    b: Uuid,
    c: Uuid,
    doc: Uuid,
    chunk_ab: Uuid,
    chunk_bc: Uuid,
}

async fn seed(pool: &PgPool) -> anyhow::Result<Fixture> {
    let (org, ws, kb) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    let etype = Uuid::now_v7();
    let part_of = Uuid::now_v7();
    let (a, b, c) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    let (src, doc, chunk_ab, chunk_bc) = (
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
    );

    sqlx::query("INSERT INTO organizations (id, name) VALUES ($1, 'proof-test')")
        .bind(org)
        .execute(pool)
        .await?;
    sqlx::query("INSERT INTO workspaces (id, org_id, name) VALUES ($1, $2, 'proof-test')")
        .bind(ws)
        .bind(org)
        .execute(pool)
        .await?;
    sqlx::query(
        "INSERT INTO knowledge_bases (id, workspace_id, name, materialize_inferences)
         VALUES ($1, $2, 'proof-test', TRUE)",
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
    sqlx::query(
        "INSERT INTO relation_types (id, kb_id, key, label, is_transitive)
         VALUES ($1, $2, 'part_of', 'part of', TRUE)",
    )
    .bind(part_of)
    .bind(kb)
    .execute(pool)
    .await?;
    for (id, name) in [(a, "FarmBeats"), (b, "Azure"), (c, "Microsoft")] {
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
    sqlx::query("INSERT INTO sources (id, kb_id, name) VALUES ($1, $2, 'proof-test')")
        .bind(src)
        .bind(kb)
        .execute(pool)
        .await?;
    sqlx::query(
        "INSERT INTO documents (id, kb_id, source_id, filename, sha256, status)
         VALUES ($1, $2, $3, 'press.md', 'proof', 'ready')",
    )
    .bind(doc)
    .bind(kb)
    .bind(src)
    .execute(pool)
    .await?;
    for (id, seq, text) in [
        (chunk_ab, 0i32, "FarmBeats is part of Azure."),
        (chunk_bc, 1i32, "Azure is part of Microsoft."),
    ] {
        sqlx::query(
            "INSERT INTO chunks (id, kb_id, document_id, seq, text) VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(id)
        .bind(kb)
        .bind(doc)
        .bind(seq)
        .bind(text)
        .execute(pool)
        .await?;
    }
    Ok(Fixture {
        org,
        kb,
        part_of,
        a,
        b,
        c,
        doc,
        chunk_ab,
        chunk_bc,
    })
}

/// One assertion, with a sentence of source text as its evidence
async fn asserted(
    pool: &PgPool,
    f: &Fixture,
    subject: Uuid,
    object: Uuid,
    chunk: Uuid,
    quote: &str,
) -> anyhow::Result<Uuid> {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO facts (id, kb_id, subject_id, predicate_id, object_id, confidence)
         VALUES ($1, $2, $3, $4, $5, 0.9)",
    )
    .bind(id)
    .bind(f.kb)
    .bind(subject)
    .bind(f.part_of)
    .bind(object)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO fact_evidence (fact_id, chunk_id, quote, proposed_predicate, document_id, doc_version)
         VALUES ($1, $2, $3, 'part of', $4, 1)",
    )
    .bind(id)
    .bind(chunk)
    .bind(quote)
    .bind(f.doc)
    .execute(pool)
    .await?;
    Ok(id)
}

#[tokio::test]
async fn a_proof_reaches_the_sentence() -> anyhow::Result<()> {
    let Some(url) = utopia_store::test_db::url() else {
        return Ok(());
    };
    let pool = PgPool::connect(&url).await?;
    let f = seed(&pool).await?;

    let run = async {
        let ab = asserted(
            &pool,
            &f,
            f.a,
            f.b,
            f.chunk_ab,
            "FarmBeats is part of Azure",
        )
        .await?;
        let bc = asserted(
            &pool,
            &f,
            f.b,
            f.c,
            f.chunk_bc,
            "Azure is part of Microsoft",
        )
        .await?;
        reasoning::materialize(&pool, f.kb).await?;

        let derived: Vec<utopia_core::models::DerivedFactView> =
            reasoning::derived_for_entity(&pool, f.kb, f.a).await?;
        let ac = derived
            .iter()
            .find(|d| d.subject_id == f.a && d.object_id == f.c)
            .expect("A part_of C should be derived");

        // 1. the order is right, 2. the leaf is the original sentence
        let proof = reasoning::proof(&pool, f.kb, ac.id)
            .await?
            .expect("live derivation has a proof");
        assert_eq!(proof.derived.id, ac.id);
        assert_eq!(proof.steps.len(), 2);
        assert_eq!(
            proof.steps[0].fact_id, ab,
            "the chain starts where the derivation starts"
        );
        assert_eq!(proof.steps[1].fact_id, bc);
        assert_eq!(proof.steps[0].subject, "FarmBeats");
        assert_eq!(proof.steps[0].object.as_deref(), Some("Azure"));
        assert_eq!(proof.steps[0].predicate.as_deref(), Some("part of"));
        assert_eq!(proof.steps[0].evidence.len(), 1);
        assert_eq!(
            proof.steps[0].evidence[0].quote.as_deref(),
            Some("FarmBeats is part of Azure"),
            "the leaf of a proof is the sentence it was extracted from"
        );
        assert_eq!(proof.steps[0].evidence[0].chunk_id, f.chunk_ab);
        assert_eq!(proof.steps[1].evidence[0].chunk_id, f.chunk_bc);
        assert!(proof.steps.iter().all(|s| !s.retracted));

        // a nonexistent id is not an error, it is "no proof"
        assert!(reasoning::proof(&pool, f.kb, Uuid::now_v7())
            .await?
            .is_none());

        // 3. retract one premise: the derivation is invalidated, the proof is still there, and
        // that step is marked
        sqlx::query("UPDATE facts SET invalidated_at = now() WHERE id = $1")
            .bind(bc)
            .execute(&pool)
            .await?;
        reasoning::materialize(&pool, f.kb).await?;
        let (gone,): (bool,) =
            sqlx::query_as("SELECT invalidated_at IS NOT NULL FROM derived_facts WHERE id = $1")
                .bind(ac.id)
                .fetch_one(&pool)
                .await?;
        assert!(gone, "a derivation falls with its premise");
        let proof = reasoning::proof(&pool, f.kb, ac.id)
            .await?
            .expect("an invalidated derivation still explains itself");
        assert!(!proof.steps[0].retracted);
        assert!(
            proof.steps[1].retracted,
            "the retracted premise is marked, not hidden"
        );
        anyhow::Ok(())
    }
    .await;

    let _ = sqlx::query("DELETE FROM organizations WHERE id = $1")
        .bind(f.org)
        .execute(&pool)
        .await;
    run
}
