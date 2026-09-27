//! The consistency check run against a real database: fetch, judge, persist, re-run.
//!
//! The pure-logic part already has 12 cases in `utopia-reason` that run without standing up a
//! database. What this pins down is the four invisible things in that layer, each of which lives
//! in SQL or in a table constraint:
//!
//! - **The three filters on the fetch**. Facts that have been overturned, facts with no
//!   predicate, and attribute facts whose object is a literal value must all stay out of it --
//!   axioms talk about relations between entities
//! - **No axioms means no grounds for judgement**. A database with no ontology pack imported
//!   comes back zero, and that is the truth of the matter rather than a malfunction
//! - **Re-running is idempotent**. The same contradiction is not persisted twice, and the row a
//!   human took a position on is not wiped out by a re-run (`ontology_proposals` fell into this
//!   hole here: a re-run flushed rejected proposals back into the to-look-at list)
//! - **Stale ones get cleared**. Once the fact is withdrawn, that violation should not still be
//!   hanging on the Review page
//!
//! With no `UTOPIA_DATABASE_URL` it skips rather than fails. It builds its own and tears it down.

use sqlx::PgPool;
use utopia_store::reasoning;
use uuid::Uuid;

struct Fixture {
    org: Uuid,
    kb: Uuid,
    /// Declares asymmetric + irreflexive
    owns: Uuid,
    /// Declares not one single axiom
    mentions: Uuid,
    a: Uuid,
    b: Uuid,
}

async fn seed(pool: &PgPool) -> anyhow::Result<Fixture> {
    let (org, ws, kb) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    let etype = Uuid::now_v7();
    let (owns, mentions) = (Uuid::now_v7(), Uuid::now_v7());
    let (a, b) = (Uuid::now_v7(), Uuid::now_v7());

    sqlx::query("INSERT INTO organizations (id, name) VALUES ($1, 'axioms-test')")
        .bind(org)
        .execute(pool)
        .await?;
    sqlx::query("INSERT INTO workspaces (id, org_id, name) VALUES ($1, $2, 'axioms-test')")
        .bind(ws)
        .bind(org)
        .execute(pool)
        .await?;
    sqlx::query(
        "INSERT INTO knowledge_bases (id, workspace_id, name) VALUES ($1, $2, 'axioms-test')",
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
        "INSERT INTO relation_types (id, kb_id, key, label, is_asymmetric, is_irreflexive)
         VALUES ($1, $2, 'owns', 'owns', TRUE, TRUE)",
    )
    .bind(owns)
    .bind(kb)
    .execute(pool)
    .await?;
    // Not one single axiom -- its edges should never be judged a contradiction
    sqlx::query(
        "INSERT INTO relation_types (id, kb_id, key, label) VALUES ($1, $2, 'mentions', 'mentions')",
    )
    .bind(mentions)
    .bind(kb)
    .execute(pool)
    .await?;
    for (id, name) in [(a, "Acme"), (b, "Beta")] {
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
        org,
        kb,
        owns,
        mentions,
        a,
        b,
    })
}

/// Persist one relation fact and return its id.
async fn fact(pool: &PgPool, kb: Uuid, s: Uuid, p: Uuid, o: Uuid) -> anyhow::Result<Uuid> {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO facts (id, kb_id, subject_id, predicate_id, object_id)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(id)
    .bind(kb)
    .bind(s)
    .bind(p)
    .bind(o)
    .execute(pool)
    .await?;
    Ok(id)
}

async fn open_kinds(pool: &PgPool, kb: Uuid) -> anyhow::Result<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT kind FROM axiom_violations WHERE kb_id = $1 AND status = 'open' ORDER BY kind",
    )
    .bind(kb)
    .fetch_all(pool)
    .await?)
}

#[tokio::test]
async fn the_ontology_is_the_only_judge() -> anyhow::Result<()> {
    let Some(url) = utopia_store::test_db::url() else {
        return Ok(());
    };
    let pool = PgPool::connect(&url).await?;
    let f = seed(&pool).await?;

    let run = async {
        // ---- 1. a database with no contradictions comes back zero
        let quiet = fact(&pool, f.kb, f.a, f.owns, f.b).await?;
        let r = reasoning::run(&pool, f.kb).await?;
        assert_eq!(r.edges, 1);
        assert_eq!(
            r.predicates_with_axioms, 1,
            "mentions declares not one single axiom, it should not get in here"
        );
        assert_eq!(r.found, 0, "one one-way owns does not constitute any contradiction");

        // ---- 2. it only counts if an axiom says so
        // B owns A -- together with the one above this makes an asymmetry violation
        let back = fact(&pool, f.kb, f.b, f.owns, f.a).await?;
        // A mentions B / B mentions A -- bidirectional, but mentions has no axioms, so it
        // must not be reported
        fact(&pool, f.kb, f.a, f.mentions, f.b).await?;
        fact(&pool, f.kb, f.b, f.mentions, f.a).await?;
        // A owns A -- a reflexivity violation
        let loop_fact = fact(&pool, f.kb, f.a, f.owns, f.a).await?;

        let r = reasoning::run(&pool, f.kb).await?;
        assert_eq!(r.edges, 5);
        assert_eq!(
            open_kinds(&pool, f.kb).await?,
            vec!["asymmetry", "self_loop"],
            "bidirectional mentions must not be reported -- its predicate has no axioms at all"
        );
        assert_eq!(r.inserted, 2);

        // The reflexive one: both columns point at the same fact
        let (l, rr): (Uuid, Uuid) = sqlx::query_as(
            "SELECT left_fact, right_fact FROM axiom_violations
              WHERE kb_id = $1 AND kind = 'self_loop'",
        )
        .bind(f.kb)
        .fetch_one(&pool)
        .await?;
        assert_eq!(l, loop_fact);
        assert_eq!(l, rr, "a fact contradicting itself does not need a second one");

        // ---- 3. re-running does not persist duplicates
        let again = reasoning::run(&pool, f.kb).await?;
        assert_eq!(again.found, 2);
        assert_eq!(
            again.inserted, 0,
            "the second run must not insert another row for the same contradiction"
        );
        assert_eq!(again.cleared, 0, "nor should it clear the previous round's");

        // ---- 4. what a human took a position on is not wiped by a re-run
        sqlx::query(
            "UPDATE axiom_violations SET status = 'resolved', resolution = 'accepted'
              WHERE kb_id = $1 AND kind = 'asymmetry'",
        )
        .bind(f.kb)
        .execute(&pool)
        .await?;
        let after = reasoning::run(&pool, f.kb).await?;
        assert_eq!(
            after.inserted, 0,
            "the row someone has already taken a position on must not be inserted again"
        );
        let resolved: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM axiom_violations WHERE kb_id = $1 AND status = 'resolved'",
        )
        .bind(f.kb)
        .fetch_one(&pool)
        .await?;
        assert_eq!(resolved, 1, "a human's decision must survive a re-run");

        // ---- 5. once the fact is withdrawn, the stale open rows have to be cleared
        sqlx::query("UPDATE facts SET invalidated_at = now() WHERE id = $1")
            .bind(loop_fact)
            .execute(&pool)
            .await?;
        let swept = reasoning::run(&pool, f.kb).await?;
        assert_eq!(
            swept.cleared, 1,
            "an overturned fact should not still have a violation hanging off it"
        );
        assert!(
            !open_kinds(&pool, f.kb).await?.contains(&"self_loop".to_string()),
            "once that fact is withdrawn the reflexivity violation no longer holds"
        );

        // ---- 6. attribute facts stay out of it: the object is a literal value, and axioms
        //         talk about relations between entities
        sqlx::query(
            "INSERT INTO facts (id, kb_id, subject_id, predicate_id, object_value)
             VALUES ($1, $2, $3, $4, '\"2015\"'::jsonb)",
        )
        .bind(Uuid::now_v7())
        .bind(f.kb)
        .bind(f.a)
        .bind(f.owns)
        .execute(&pool)
        .await?;
        let attrs = reasoning::run(&pool, f.kb).await?;
        assert_eq!(
            attrs.edges, 4,
            "the one with a literal-value object should not be fetched as an edge"
        );

        // ---- 7. a database with no ontology pack: the conclusion is "no grounds for
        //         judgement", not "no contradictions"
        sqlx::query("UPDATE relation_types SET is_asymmetric = FALSE, is_irreflexive = FALSE WHERE kb_id = $1")
            .bind(f.kb)
            .execute(&pool)
            .await?;
        let blind = reasoning::run(&pool, f.kb).await?;
        assert_eq!(blind.predicates_with_axioms, 0);
        assert_eq!(blind.found, 0);
        assert!(
            open_kinds(&pool, f.kb).await?.is_empty(),
            "with the axiom withdrawn, the violations reported on its basis should go with it"
        );
        let _ = (quiet, back);
        Ok::<_, anyhow::Error>(())
    }
    .await;

    // Delete the org along with it -- deleting only the kb leaves organizations /
    // workspaces behind in the dev database
    sqlx::query("DELETE FROM organizations WHERE id = $1")
        .bind(f.org)
        .execute(&pool)
        .await?;
    run
}
