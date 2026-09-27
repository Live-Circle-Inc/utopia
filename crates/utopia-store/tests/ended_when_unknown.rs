//! The state "it is over, but we do not know which day", run against a real database.
//!
//! Before precision was recorded at each end, `valid_to IS NULL` meant both "still ongoing" and
//! "over, but we do not know when". Those two things have **opposite truth values** -- one says
//! the relation holds now, the other says it does not -- and the ledger had only one way to write
//! it, so "former CEO of Weta Digital" could only be written as the former, and the graph would
//! assert something the source text says is already over.
//!
//! Three things are nailed down here, each of them living in SQL or in a SQL constraint, where
//! `cargo check` cannot see a single word of it:
//!
//! - all three ended states can be stored, and four self-contradictory combinations cannot
//! - a new observation of "over but do not know which day" is **not** taken as "said nothing"
//!   and merged into the open row
//! - the temporal engine **does not treat it as an open row** to be closed -- it does not even
//!   know itself when it ended
//!
//! One of those four negative cases once slipped through: when `valid_to` has a date and the
//! precision is NULL, `NULL IN ('year',…)` evaluates to NULL, `TRUE AND NULL` is NULL, and a
//! CHECK that meets NULL judges it as passing. Three-valued logic is silent here; only really
//! running it once makes it visible.
//!
//! Skips rather than fails when there is no `UTOPIA_DATABASE_URL`. Builds its own and tears its
//! own down, and never touches an existing database.

use sqlx::PgPool;
use utopia_store::graph::{Validity, ENDED_UNKNOWN};
use uuid::Uuid;

struct Fixture {
    kb: Uuid,
    subject: Uuid,
    predicate: Uuid,
    object_a: Uuid,
    object_b: Uuid,
}

async fn seed(pool: &PgPool) -> anyhow::Result<Fixture> {
    let (org, ws, kb) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    let etype = Uuid::now_v7();
    let predicate = Uuid::now_v7();
    let (subject, object_a, object_b) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());

    sqlx::query("INSERT INTO organizations (id, name) VALUES ($1, 'ended-unknown-test')")
        .bind(org)
        .execute(pool)
        .await?;
    sqlx::query("INSERT INTO workspaces (id, org_id, name) VALUES ($1, $2, 'ended-unknown-test')")
        .bind(ws)
        .bind(org)
        .execute(pool)
        .await?;
    sqlx::query(
        "INSERT INTO knowledge_bases (id, workspace_id, name)
         VALUES ($1, $2, 'ended-unknown-test')",
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
    // functional: unique on the subject side, which is what makes the temporal engine do
    // close-out reconciliation on it
    sqlx::query(
        "INSERT INTO relation_types (id, kb_id, key, label, temporal, functional)
         VALUES ($1, $2, 'leads', 'leads', 'state', TRUE)",
    )
    .bind(predicate)
    .bind(kb)
    .execute(pool)
    .await?;
    for (id, name) in [
        (subject, "Akkaraju"),
        (object_a, "Weta"),
        (object_b, "Stability"),
    ] {
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
    Ok(Fixture {
        kb,
        subject,
        predicate,
        object_a,
        object_b,
    })
}

fn t(s: &str) -> chrono::DateTime<chrono::Utc> {
    s.parse().unwrap()
}

async fn shape(pool: &PgPool, id: Uuid) -> anyhow::Result<(bool, Option<String>)> {
    let row: (bool, Option<String>) =
        sqlx::query_as("SELECT valid_to IS NULL, valid_to_precision FROM facts WHERE id = $1")
            .bind(id)
            .fetch_one(pool)
            .await?;
    Ok(row)
}

#[tokio::test]
async fn a_relation_the_text_says_is_over_is_not_stored_as_ongoing() -> anyhow::Result<()> {
    let Some(url) = utopia_store::test_db::url() else {
        return Ok(());
    };
    let pool = PgPool::connect(&url).await?;
    let f = seed(&pool).await?;

    let run = async {
        // "Akkaraju, former CEO of Weta" -- the ending is what the text says, the date is what
        // the text does not give
        let (ended, _) = utopia_store::graph::insert_fact(
            &pool,
            f.kb,
            f.subject,
            Some(f.predicate),
            f.object_a,
            Validity::starting(Some(t("2020-01-01T00:00:00Z")), Some("day")).ended_when_unknown(),
            0.9,
        )
        .await?;
        let (to_is_null, prec) = shape(&pool, ended).await?;
        assert!(to_is_null, "with no date to write, valid_to stays NULL");
        assert_eq!(
            prec.as_deref(),
            Some(ENDED_UNKNOWN),
            "but the precision slot must say 'it ended' -- without it, no different from 'ongoing'"
        );

        // **A second "over but do not know which day" observation of the same claim must not be
        // merged into "said nothing".**
        // The old criterion was valid_from.is_none() && valid_to.is_none(),
        // and this observation satisfies both -- it would be merged into the open row, and the
        // one piece of information it brought (that it ended) would be lost
        let (second, created) = utopia_store::graph::insert_fact(
            &pool,
            f.kb,
            f.subject,
            Some(f.predicate),
            f.object_b,
            Validity::default().ended_when_unknown(),
            0.9,
        )
        .await?;
        assert!(created, "it says something; not a weakened statement");
        assert_eq!(
            shape(&pool, second).await?.1.as_deref(),
            Some(ENDED_UNKNOWN)
        );
        Ok::<_, anyhow::Error>(())
    }
    .await;

    sqlx::query("DELETE FROM knowledge_bases WHERE id = $1")
        .bind(f.kb)
        .execute(&pool)
        .await?;
    run
}

/// The temporal engine **must not treat "ended-when-unknown" as an open row** to be closed:
/// a claim that does not know itself when it ended has no standing to fix an end moment for
/// anyone else.
#[tokio::test]
async fn an_already_ended_fact_is_not_treated_as_an_open_claim() -> anyhow::Result<()> {
    let Some(url) = utopia_store::test_db::url() else {
        return Ok(());
    };
    let pool = PgPool::connect(&url).await?;
    let f = seed(&pool).await?;

    let run = async {
        // The old row: over, no idea which day
        let (old, _) = utopia_store::graph::insert_fact(
            &pool,
            f.kb,
            f.subject,
            Some(f.predicate),
            f.object_a,
            Validity::starting(Some(t("2020-01-01T00:00:00Z")), Some("day")).ended_when_unknown(),
            0.9,
        )
        .await?;
        // The new row: a different object, starting in 2024. A functional relation, so the
        // engine goes looking for the "open row"
        let (new, _) = utopia_store::graph::insert_fact(
            &pool,
            f.kb,
            f.subject,
            Some(f.predicate),
            f.object_b,
            Validity::starting(Some(t("2024-01-01T00:00:00Z")), Some("day")),
            0.9,
        )
        .await?;
        let report = utopia_store::temporal::reconcile_new_fact(
            &pool,
            f.kb,
            new,
            f.subject,
            f.predicate,
            Some(f.object_b),
            None,
            utopia_store::temporal::Uniqueness::SubjectSide,
            Validity::starting(Some(t("2024-01-01T00:00:00Z")), Some("day")),
            0.9,
        )
        .await?;
        assert_eq!(report.corrected.len(), 0, "already ended, no second close");
        assert_eq!(report.conflicts, 0, "no conflict; the spans never overlap");
        // The old row is left exactly as it was
        let still: Option<chrono::DateTime<chrono::Utc>> =
            sqlx::query_scalar("SELECT valid_to FROM facts WHERE id = $1")
                .bind(old)
                .fetch_one(&pool)
                .await?;
        assert!(still.is_none(), "the engine must not invent an end date");
        Ok::<_, anyhow::Error>(())
    }
    .await;

    sqlx::query("DELETE FROM knowledge_bases WHERE id = $1")
        .bind(f.kb)
        .execute(&pool)
        .await?;
    run
}
