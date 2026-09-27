//! #216: a failed job can find its way back to the queue.
//!
//! `failed` used to be the end of the road -- run out of credit and a whole batch of documents
//! fails, and after topping up the only options were clicking them one at a time or
//! re-extracting the entire source (running the model again over the ones that already
//! succeeded). `jobs::requeue_failed` requeues by scope: base, kind and failure time, all three
//! conditions optional. Three things are guarded here:
//!
//! 1. **The base scope is drawn precisely.** A job's payload carries only one of
//!    `document_id` / `source_id` / `kb_id`, so requeueing by base has to resolve down to the
//!    base -- not a single row from another base is touched, and the ones with no base (system
//!    jobs) only move when the scope is not restricted to a base.
//! 2. **Only the `failed` ones move**, `done` and `queued` stay put; after requeueing
//!    `attempts` goes back to zero and the job is due immediately.
//! 3. **Kind and time window**: `kind` requeues only that one kind; `failed_since` only the
//!    ones that failed after that point -- a "run it again" off an alert is drawn around
//!    exactly the jobs inside that outage window.
//!
//! Skipped rather than failed when there is no `UTOPIA_DATABASE_URL`. It builds its own fixture
//! and tears it down, and never touches an existing database.

use chrono::{Duration, Utc};
use sqlx::PgPool;
use utopia_store::jobs::{self, RequeueScope};
use uuid::Uuid;

struct Fx {
    org: Uuid,
    kb1: Uuid,
    kb2: Uuid,
    doc1: Uuid,
    src2: Uuid,
}

async fn seed(pool: &PgPool) -> anyhow::Result<Fx> {
    let (org, ws, kb1, kb2) = (
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
    );
    let (src1, doc1, src2) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    sqlx::query("INSERT INTO organizations (id, name) VALUES ($1, 'requeue-test')")
        .bind(org)
        .execute(pool)
        .await?;
    sqlx::query("INSERT INTO workspaces (id, org_id, name) VALUES ($1, $2, 'requeue-test')")
        .bind(ws)
        .bind(org)
        .execute(pool)
        .await?;
    for kb in [kb1, kb2] {
        sqlx::query(
            "INSERT INTO knowledge_bases (id, workspace_id, name) VALUES ($1, $2, 'requeue-test')",
        )
        .bind(kb)
        .bind(ws)
        .execute(pool)
        .await?;
    }
    sqlx::query("INSERT INTO sources (id, kb_id, kind, name) VALUES ($1, $2, 'folder', 'f')")
        .bind(src1)
        .bind(kb1)
        .execute(pool)
        .await?;
    sqlx::query(
        "INSERT INTO documents (id, kb_id, source_id, filename, sha256, status)
         VALUES ($1, $2, $3, 'a.md', 'requeue', 'ready')",
    )
    .bind(doc1)
    .bind(kb1)
    .bind(src1)
    .execute(pool)
    .await?;
    sqlx::query("INSERT INTO sources (id, kb_id, kind, name) VALUES ($1, $2, 'url', 'u')")
        .bind(src2)
        .bind(kb2)
        .execute(pool)
        .await?;
    Ok(Fx {
        org,
        kb1,
        kb2,
        doc1,
        src2,
    })
}

async fn job(
    pool: &PgPool,
    kind: &str,
    payload: serde_json::Value,
    status: &str,
    failed_ago: Duration,
) -> anyhow::Result<i64> {
    let (id,): (i64,) = sqlx::query_as(
        "INSERT INTO jobs (kind, payload, status, attempts, last_error, updated_at)
         VALUES ($1, $2, $3, 3, 'out of credit', $4) RETURNING id",
    )
    .bind(kind)
    .bind(payload)
    .bind(status)
    .bind(Utc::now() - failed_ago)
    .fetch_one(pool)
    .await?;
    Ok(id)
}

async fn state(pool: &PgPool, id: i64) -> anyhow::Result<(String, i32)> {
    Ok(
        sqlx::query_as("SELECT status, attempts FROM jobs WHERE id = $1")
            .bind(id)
            .fetch_one(pool)
            .await?,
    )
}

#[tokio::test]
async fn a_failed_job_finds_its_way_back() -> anyhow::Result<()> {
    let Some(url) = utopia_store::test_db::url() else {
        return Ok(());
    };
    let pool = PgPool::connect(&url).await?;
    let f = seed(&pool).await?;
    let mut ids: Vec<i64> = Vec::new();

    let run = async {
        let m = Duration::minutes(1);
        // kb1: a document job, a base-level job, one that failed long ago, one that already
        // succeeded
        let d1 = job(
            &pool,
            "process_document",
            serde_json::json!({ "document_id": f.doc1 }),
            "failed",
            m,
        )
        .await?;
        let b1 = job(
            &pool,
            "bootstrap_ontology",
            serde_json::json!({ "kb_id": f.kb1 }),
            "failed",
            m,
        )
        .await?;
        let old1 = job(
            &pool,
            "process_document",
            serde_json::json!({ "document_id": f.doc1 }),
            "failed",
            Duration::days(3),
        )
        .await?;
        let done1 = job(
            &pool,
            "process_document",
            serde_json::json!({ "document_id": f.doc1 }),
            "done",
            m,
        )
        .await?;
        // kb2: a source job
        let s2 = job(
            &pool,
            "sync_source",
            serde_json::json!({ "source_id": f.src2 }),
            "failed",
            m,
        )
        .await?;
        // a system job with no base
        let sys = job(&pool, "noop", serde_json::json!({}), "failed", m).await?;
        ids.extend([d1, b1, old1, done1, s2, sys]);

        assert_eq!(jobs::failed_count(&pool, Some(f.kb1)).await?, 3);
        assert_eq!(jobs::failed_count(&pool, Some(f.kb2)).await?, 1);

        // 3. Time window: requeue only the kb1 jobs that failed within the last hour
        let n = jobs::requeue_failed(
            &pool,
            RequeueScope {
                kb_id: Some(f.kb1),
                kind: None,
                failed_since: Some(Utc::now() - Duration::hours(1)),
            },
        )
        .await?;
        assert_eq!(
            n, 2,
            "the document job and the base-level job, not the old one"
        );
        assert_eq!(state(&pool, d1).await?, ("queued".into(), 0));
        assert_eq!(state(&pool, b1).await?, ("queued".into(), 0));
        assert_eq!(state(&pool, old1).await?.0, "failed");
        assert_eq!(state(&pool, done1).await?.0, "done", "done stays done");
        assert_eq!(
            state(&pool, s2).await?.0,
            "failed",
            "another base is untouched"
        );
        assert_eq!(
            state(&pool, sys).await?.0,
            "failed",
            "a system job has no base"
        );

        // 3. Kind: inside kb1 requeue only process_document; the old one comes back this time
        let n = jobs::requeue_failed(
            &pool,
            RequeueScope {
                kb_id: Some(f.kb1),
                kind: Some("process_document"),
                failed_since: None,
            },
        )
        .await?;
        assert_eq!(n, 1);
        assert_eq!(state(&pool, old1).await?, ("queued".into(), 0));
        assert_eq!(jobs::failed_count(&pool, Some(f.kb1)).await?, 0);

        // 1. By base: kb2's source job
        let n = jobs::requeue_failed(
            &pool,
            RequeueScope {
                kb_id: Some(f.kb2),
                kind: None,
                failed_since: None,
            },
        )
        .await?;
        assert_eq!(n, 1);
        assert_eq!(state(&pool, s2).await?, ("queued".into(), 0));

        // Only a scope that is not restricted to a base moves the system job
        let before = jobs::failed_count(&pool, None).await?;
        let n = jobs::requeue_failed(
            &pool,
            RequeueScope {
                kb_id: None,
                kind: Some("noop"),
                failed_since: Some(Utc::now() - Duration::hours(1)),
            },
        )
        .await?;
        assert!(n >= 1);
        assert_eq!(state(&pool, sys).await?.0, "queued");
        assert!(jobs::failed_count(&pool, None).await? < before);
        Ok::<_, anyhow::Error>(())
    }
    .await;

    // Tear-down: the jobs table does not cascade with the organization, so clean it by hand
    let _ = sqlx::query("DELETE FROM jobs WHERE id = ANY($1)")
        .bind(&ids)
        .execute(&pool)
        .await;
    let _ = sqlx::query("DELETE FROM organizations WHERE id = $1")
        .bind(f.org)
        .execute(&pool)
        .await;
    run
}
