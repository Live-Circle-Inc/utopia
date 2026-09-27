//! A fact extracted from a memory waits for a human nod first (`docs/decisions/0015`, table in
//! migration 0018) -- run against a real database.
//!
//! What this guards is the reverse of that live observation: when the conversation says "Acme
//! moved its headquarters to Shenzhen", the graph must **not** immediately gain a live edge. The
//! proposal only enters `pending_facts`; only after a human nods does it go into the ledger by the
//! same path as extraction (the fact + evidence pointing back at that sentence + temporal
//! reconciliation); after a shake of the head, the same triple is never proposed again.

use sqlx::PgPool;
use uuid::Uuid;

struct Fixture {
    /// The org the fixture creates; teardown starts by deleting it (cascades)
    org: Uuid,
    kb: Uuid,
    acme: Uuid,
    shenzhen: Uuid,
    shanghai: Uuid,
    hq: Uuid,
    chunk: Uuid,
}

async fn fixture(pool: &PgPool) -> anyhow::Result<Fixture> {
    let (org, ws, kb) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    sqlx::query("INSERT INTO organizations (id, name) VALUES ($1, 'nod-test')")
        .bind(org)
        .execute(pool)
        .await?;
    sqlx::query("INSERT INTO workspaces (id, org_id, name) VALUES ($1, $2, 'nod-test')")
        .bind(ws)
        .bind(org)
        .execute(pool)
        .await?;
    sqlx::query("INSERT INTO knowledge_bases (id, workspace_id, name) VALUES ($1, $2, 'nod-test')")
        .bind(kb)
        .bind(ws)
        .execute(pool)
        .await?;
    // There is only one headquarters: functional gives temporal reconciliation something to do
    let hq = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO relation_types (id, kb_id, key, label, functional)
         VALUES ($1, $2, 'headquartered_in', 'headquartered in', TRUE)",
    )
    .bind(hq)
    .bind(kb)
    .execute(pool)
    .await?;
    let (acme, shenzhen, shanghai) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    for (id, name) in [
        (acme, "Acme"),
        (shenzhen, "Shenzhen"),
        (shanghai, "Shanghai"),
    ] {
        sqlx::query("INSERT INTO entities (id, kb_id, canonical_name) VALUES ($1, $2, $3)")
            .bind(id)
            .bind(kb)
            .bind(name)
            .execute(pool)
            .await?;
    }
    // That one memory, landed as a chunk by the product's own path
    let (_doc, chunk) = utopia_store::memory::append_episode(
        pool,
        kb,
        "Acme moved its headquarters to Shenzhen on 2026-03-15.",
        chrono::Utc::now(),
    )
    .await?;
    Ok(Fixture {
        org,
        kb,
        acme,
        shenzhen,
        shanghai,
        hq,
        chunk,
    })
}

fn day(s: &str) -> chrono::DateTime<chrono::Utc> {
    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc()
}

async fn live_facts(pool: &PgPool, kb: Uuid) -> anyhow::Result<i64> {
    let (n,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM facts WHERE kb_id = $1 AND invalidated_at IS NULL")
            .bind(kb)
            .fetch_one(pool)
            .await?;
    Ok(n)
}

#[tokio::test]
async fn a_remembered_fact_waits_for_a_nod() -> anyhow::Result<()> {
    let Some(url) = utopia_store::test_db::url() else {
        return Ok(());
    };
    let pool = PgPool::connect(&url).await?;
    let f = fixture(&pool).await?;

    let run = async {
        use utopia_store::pending::{self, Outcome, Proposal};
        let propose = |object: Uuid, from: &'static str| {
            pending::propose(
                &pool,
                Proposal {
                    kb_id: f.kb,
                    subject_id: f.acme,
                    predicate_id: Some(f.hq),
                    object_id: Some(object),
                    object_value: None,
                    proposed_predicate: Some("moved headquarters to"),
                    validity: utopia_store::graph::Validity {
                        from: Some(day(from)),
                        from_precision: Some("day"),
                        to: None,
                        to_precision: None,
                    },
                    confidence: 0.9,
                    chunk_id: f.chunk,
                    proposed_by: None,
                },
            )
        };

        // 1. A proposal does not reach the graph
        let first = propose(f.shenzhen, "2026-03-15").await?;
        assert!(matches!(first, Outcome::Proposed(_)), "the first one must enter the queue");
        assert_eq!(live_facts(&pool, f.kb).await?, 0, "the graph must have no live edge yet");
        let queued = pending::for_chunk(&pool, f.kb, f.chunk).await?;
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].subject_name, "Acme");
        assert!(queued[0].quote.contains("Acme moved its headquarters"), "the quote must be shown");

        // 2. Re-extracting the same sentence does not propose it twice
        assert_eq!(propose(f.shenzhen, "2026-03-15").await?, Outcome::AlreadyPending);

        // 3. A nod: into the ledger, evidence points back at that sentence, the queue empties
        let Outcome::Proposed(id) = first else { unreachable!() };
        let done = pending::confirm(&pool, f.kb, id).await?;
        assert!(done.created, "confirming must land one new fact");
        assert_eq!(live_facts(&pool, f.kb).await?, 1);
        let (ev_chunk, ev_proposed): (Uuid, Option<String>) = sqlx::query_as(
            "SELECT chunk_id, proposed_predicate FROM fact_evidence WHERE fact_id = $1",
        )
        .bind(done.fact_id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(ev_chunk, f.chunk, "the evidence must point back at that memory");
        assert_eq!(ev_proposed.as_deref(), Some("moved headquarters to"));
        assert!(pending::for_chunk(&pool, f.kb, f.chunk).await?.is_empty());

        // 4. What the graph already has is not asked about again
        assert_eq!(propose(f.shenzhen, "2026-03-15").await?, Outcome::AlreadyAsserted);

        // 5. A shake of the head is remembered: a rejected triple is not proposed next round
        let Outcome::Proposed(bad) = propose(f.shanghai, "2020-01-01").await? else {
            panic!("a different object must be a new proposal");
        };
        pending::reject(&pool, f.kb, bad, None).await?;
        assert_eq!(propose(f.shanghai, "2020-01-01").await?, Outcome::Rejected);
        assert_eq!(live_facts(&pool, f.kb).await?, 1, "rejection does not touch the graph");

        // 6. A nod takes the same path as extraction: a new value on a functional relation
        //    closes out the old one.
        //    The rejection record is looked up by (subject, predicate, object), so the same
        //    object with a different date gets blocked all the same -- hence a different object
        //    entity to act out the succession
        let beijing = Uuid::now_v7();
        sqlx::query("INSERT INTO entities (id, kb_id, canonical_name) VALUES ($1, $2, 'Beijing')")
            .bind(beijing)
            .bind(f.kb)
            .execute(&pool)
            .await?;
        let Outcome::Proposed(next) = propose(beijing, "2027-01-01").await? else {
            panic!("the succession must be a new proposal");
        };
        let moved = pending::confirm(&pool, f.kb, next).await?;
        assert!(moved.created);
        assert_eq!(moved.conflicts, 0, "clear order, high confidence: close, not conflict");
        let (closed,): (i64,) = sqlx::query_as(
            "SELECT count(*) FROM facts
              WHERE kb_id = $1 AND object_id = $2 AND invalidated_at IS NULL AND valid_to IS NOT NULL",
        )
        .bind(f.kb)
        .bind(f.shenzhen)
        .fetch_one(&pool)
        .await?;
        assert_eq!(closed, 1, "the Shenzhen one must be closed into an interval with an end");
        Ok::<_, anyhow::Error>(())
    }
    .await;

    // Delete the org the fixture built itself; the cascade takes the workspace, the knowledge
    // base and everything else with it.
    // It used to delete only the knowledge base, so every run left one more empty
    // org / workspace pair behind in the database -- "production code has no delete-an-org
    // action" is correct, but a fixture is not production code: what it builds, it clears up
    sqlx::query("DELETE FROM organizations WHERE id = $1")
        .bind(f.org)
        .execute(&pool)
        .await?;
    run
}
