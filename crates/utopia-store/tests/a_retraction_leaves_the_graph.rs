//! #202: "the data is wrong" has to really retract the fact, and the queue has to be honest
//! about it too.
//!
//! `decide` used to change only `axiom_violations`, leaving the fact alive in the graph; a
//! re-run of the check then hit the resolved row and did `DO NOTHING`, so the violation neither
//! disappeared nor showed up again. Four things are guarded here:
//!
//! 1. **The named one is the one retracted.** A two-fact violation (asymmetry) has to say which
//!    one to retract, and afterwards the fact's `invalidated_at` is non-null, the violation is
//!    resolved, and a re-run does not report it again.
//! 2. **A fact that is not part of the violation cannot be retracted.**
//! 3. **A single-fact violation does not need to be told.** A self loop has only one, so
//!    retract left directly.
//! 4. **A promise that was not kept reopens it.** `axiom_relaxed` says the ontology is going to
//!    be changed; if it was not and the violation is computed again, that row goes back to
//!    open; `accepted` means they are meant to stand together, so a re-run stays silent as
//!    before.
//!
//! Skipped rather than failed when there is no `UTOPIA_DATABASE_URL`. It builds its own fixture
//! and tears it down, and never touches an existing database.

use sqlx::PgPool;
use utopia_store::reasoning;
use uuid::Uuid;

struct Fx {
    org: Uuid,
    user: Uuid,
    kb: Uuid,
    reports_to: Uuid,
    etype: Uuid,
}

async fn seed(pool: &PgPool) -> anyhow::Result<Fx> {
    let (org, ws, kb, user) = (
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
    );
    let (etype, reports_to) = (Uuid::now_v7(), Uuid::now_v7());
    sqlx::query("INSERT INTO organizations (id, name) VALUES ($1, 'retract-test')")
        .bind(org)
        .execute(pool)
        .await?;
    sqlx::query("INSERT INTO workspaces (id, org_id, name) VALUES ($1, $2, 'retract-test')")
        .bind(ws)
        .bind(org)
        .execute(pool)
        .await?;
    sqlx::query(
        "INSERT INTO users (id, org_id, email, display_name, password_hash)
         VALUES ($1, $2, $1 || '@retract.test', 'r', 'x')",
    )
    .bind(user)
    .bind(org)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO knowledge_bases (id, workspace_id, name) VALUES ($1, $2, 'retract-test')",
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
    // Asymmetric and irreflexive: asserting both directions is a contradiction, and so is a
    // self loop
    sqlx::query(
        "INSERT INTO relation_types (id, kb_id, key, label, is_asymmetric, is_irreflexive)
         VALUES ($1, $2, 'reports_to', 'reports to', TRUE, TRUE)",
    )
    .bind(reports_to)
    .bind(kb)
    .execute(pool)
    .await?;
    Ok(Fx {
        org,
        user,
        kb,
        reports_to,
        etype,
    })
}

async fn entity(pool: &PgPool, f: &Fx, name: &str) -> anyhow::Result<Uuid> {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO entities (id, kb_id, type_id, canonical_name) VALUES ($1, $2, $3, $4)",
    )
    .bind(id)
    .bind(f.kb)
    .bind(f.etype)
    .bind(name)
    .execute(pool)
    .await?;
    Ok(id)
}

async fn fact(pool: &PgPool, f: &Fx, s: Uuid, o: Uuid) -> anyhow::Result<Uuid> {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO facts (id, kb_id, subject_id, predicate_id, object_id, confidence)
         VALUES ($1, $2, $3, $4, $5, 0.9)",
    )
    .bind(id)
    .bind(f.kb)
    .bind(s)
    .bind(f.reports_to)
    .bind(o)
    .execute(pool)
    .await?;
    Ok(id)
}

/// (id, kind, status, resolution, left, right) ordered by detection time
async fn violations(
    pool: &PgPool,
    f: &Fx,
) -> anyhow::Result<Vec<(Uuid, String, String, Option<String>, Uuid, Uuid)>> {
    Ok(sqlx::query_as(
        "SELECT id, kind, status, resolution, left_fact, right_fact FROM axiom_violations
          WHERE kb_id = $1 ORDER BY detected_at, id",
    )
    .bind(f.kb)
    .fetch_all(pool)
    .await?)
}

async fn retracted(pool: &PgPool, id: Uuid) -> anyhow::Result<bool> {
    Ok(
        sqlx::query_scalar("SELECT invalidated_at IS NOT NULL FROM facts WHERE id = $1")
            .bind(id)
            .fetch_one(pool)
            .await?,
    )
}

#[tokio::test]
async fn a_retraction_leaves_the_graph() -> anyhow::Result<()> {
    let Some(url) = utopia_store::test_db::url() else {
        return Ok(());
    };
    let pool = PgPool::connect(&url).await?;
    let f = seed(&pool).await?;

    let run = async {
        let (a, b) = (entity(&pool, &f, "A").await?, entity(&pool, &f, "B").await?);
        let ab = fact(&pool, &f, a, b).await?;
        let ba = fact(&pool, &f, b, a).await?;
        reasoning::run(&pool, f.kb).await?;
        let rows = violations(&pool, &f).await?;
        assert_eq!(rows.len(), 1);
        let (vid, kind, ..) = &rows[0];
        assert_eq!(kind, "asymmetry");

        // 2. What is not in the violation cannot be retracted; and for a two-fact one, not
        //    saying which to retract will not do either
        let stranger = fact(&pool, &f, a, entity(&pool, &f, "Z").await?).await?;
        assert!(
            reasoning::retract_from_violation(&pool, f.kb, *vid, Some(stranger), f.user)
                .await
                .is_err()
        );
        assert!(
            reasoning::retract_from_violation(&pool, f.kb, *vid, None, f.user)
                .await
                .is_err()
        );

        // 1. Retract B→A: the fact is invalidated, the violation resolved, and a re-run does
        //    not report it any more
        let gone = reasoning::retract_from_violation(&pool, f.kb, *vid, Some(ba), f.user).await?;
        assert_eq!(gone, ba);
        assert!(retracted(&pool, ba).await?, "the button does what it says");
        assert!(!retracted(&pool, ab).await?, "the other fact stays");
        let r = reasoning::run(&pool, f.kb).await?;
        assert_eq!(r.reopened, 0);
        let rows = violations(&pool, &f).await?;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].2, "resolved");
        assert_eq!(rows[0].3.as_deref(), Some("fact_retracted"));

        // 3. A self loop has only one fact, so there is no need to say which to retract
        let e = entity(&pool, &f, "E").await?;
        let ee = fact(&pool, &f, e, e).await?;
        reasoning::run(&pool, f.kb).await?;
        let (loop_id, ..) = violations(&pool, &f)
            .await?
            .into_iter()
            .find(|v| v.1 == "self_loop")
            .expect("the self loop is reported");
        assert_eq!(
            reasoning::retract_from_violation(&pool, f.kb, loop_id, None, f.user).await?,
            ee
        );
        assert!(retracted(&pool, ee).await?);

        // 4. A promise that was not kept reopens it: after axiom_relaxed the ontology was not
        //    changed, so a re-run goes back to open; accepted stays silent
        let (c, d) = (entity(&pool, &f, "C").await?, entity(&pool, &f, "D").await?);
        fact(&pool, &f, c, d).await?;
        fact(&pool, &f, d, c).await?;
        reasoning::run(&pool, f.kb).await?;
        let (cd_id, ..) = violations(&pool, &f)
            .await?
            .into_iter()
            .find(|v| v.2 == "open")
            .expect("the new pair is open");
        reasoning::decide(&pool, f.kb, cd_id, "axiom_relaxed", f.user).await?;
        let r = reasoning::run(&pool, f.kb).await?;
        assert_eq!(
            r.reopened, 1,
            "the axiom is still declared, so the promise did not hold"
        );
        let row = violations(&pool, &f)
            .await?
            .into_iter()
            .find(|v| v.0 == cd_id)
            .unwrap();
        assert_eq!(row.2, "open");
        assert_eq!(row.3, None);
        reasoning::decide(&pool, f.kb, cd_id, "accepted", f.user).await?;
        let r = reasoning::run(&pool, f.kb).await?;
        assert_eq!(
            r.reopened, 0,
            "accepted means both stand; the queue stays quiet"
        );
        let row = violations(&pool, &f)
            .await?
            .into_iter()
            .find(|v| v.0 == cd_id)
            .unwrap();
        assert_eq!(row.2, "resolved");
        Ok::<_, anyhow::Error>(())
    }
    .await;

    let _ = sqlx::query("DELETE FROM organizations WHERE id = $1")
        .bind(f.org)
        .execute(&pool)
        .await;
    run
}
