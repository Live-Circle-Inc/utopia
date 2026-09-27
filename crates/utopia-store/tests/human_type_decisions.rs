//! A type a human has ruled on is one the engine may not change -- run against a real database.
//!
//! This whole line of defence lives in SQL `WHERE` clauses: three read sites each carry one
//! `type_source <> 'human'`, and missing one of them is a silent failure. `cargo check` cannot
//! see a word of it, and 0009 just stepped on the `NULL <> uuid` trap in this very area -- the
//! type system can count Rust, it cannot count its way into SQL.
//!
//! Four assertions for four paths:
//!
//! - Gathering candidates for type resolution (`entities_for_type_resolution`) must not scoop up
//!   the ones a human has ruled on
//! - Claiming after the ontology grows a new class (`adopt_proposed_types`) must not paint over
//!   the ones a human has ruled on
//! - Extraction promotion must not fit a type onto an entity where a human said "there just is
//!   no type" ← the 0009 × P4 crossover
//! - `retype_entities` uses the `actor` parameter it already has to tell human from inferred
//!
//! Skips instead of failing when `UTOPIA_DATABASE_URL` is absent. It builds its own fixtures and
//! tears them down again; it never touches an existing database.

use sqlx::PgPool;
use uuid::Uuid;

struct Fx {
    org: Uuid,
    kb: Uuid,
    org_type: Uuid,
    sub_type: Uuid,
}

/// Builds an ontology that just barely trips the "third candidate condition": `organization` has
/// the subtype `startup`. Once a human pins an entity to `organization`, it is precisely that
/// subtype which qualifies it for re-judgement.
async fn seed(pool: &PgPool) -> anyhow::Result<Fx> {
    let (org, ws, kb) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    let (org_type, sub_type) = (Uuid::now_v7(), Uuid::now_v7());

    sqlx::query("INSERT INTO organizations (id, name) VALUES ($1, 'p4a-test')")
        .bind(org)
        .execute(pool)
        .await?;
    sqlx::query("INSERT INTO workspaces (id, org_id, name) VALUES ($1, $2, 'p4a-test')")
        .bind(ws)
        .bind(org)
        .execute(pool)
        .await?;
    sqlx::query("INSERT INTO knowledge_bases (id, workspace_id, name) VALUES ($1, $2, 'p4a-test')")
        .bind(kb)
        .bind(ws)
        .execute(pool)
        .await?;
    for (id, key) in [(org_type, "organization"), (sub_type, "startup")] {
        sqlx::query("INSERT INTO entity_types (id, kb_id, key, label) VALUES ($1, $2, $3, $3)")
            .bind(id)
            .bind(kb)
            .bind(key)
            .execute(pool)
            .await?;
    }
    sqlx::query("INSERT INTO entity_type_parents (child_id, parent_id) VALUES ($1, $2)")
        .bind(sub_type)
        .bind(org_type)
        .execute(pool)
        .await?;
    Ok(Fx {
        org,
        kb,
        org_type,
        sub_type,
    })
}

async fn entity(
    pool: &PgPool,
    f: &Fx,
    name: &str,
    type_id: Option<Uuid>,
    source: &str,
) -> anyhow::Result<Uuid> {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO entities (id, kb_id, type_id, canonical_name, type_source)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(id)
    .bind(f.kb)
    .bind(type_id)
    .bind(name)
    .bind(source)
    .execute(pool)
    .await?;
    Ok(id)
}

async fn source_of(pool: &PgPool, id: Uuid) -> anyhow::Result<String> {
    Ok(
        sqlx::query_scalar("SELECT type_source FROM entities WHERE id = $1")
            .bind(id)
            .fetch_one(pool)
            .await?,
    )
}

/// The main battlefield: the candidate condition "take it in if its current class still has
/// subtypes" scoops up the entities a human has ruled on right along with the rest.
#[tokio::test]
async fn type_resolution_leaves_human_decisions_alone() -> anyhow::Result<()> {
    let Some(url) = utopia_store::test_db::url() else {
        return Ok(());
    };
    let pool = PgPool::connect(&url).await?;
    let f = seed(&pool).await?;

    let run = async {
        let by_human = entity(&pool, &f, "Acme", Some(f.org_type), "human").await?;
        let by_engine = entity(&pool, &f, "Globex", Some(f.org_type), "extracted").await?;

        let picked = utopia_store::resolution::entities_for_type_resolution(&pool, f.kb, 100)
            .await?
            .into_iter()
            .map(|c| c.id)
            .collect::<std::collections::HashSet<_>>();

        assert!(
            picked.contains(&by_engine),
            "an extracted type ought to be re-judged -- organization has the subtype startup, which is exactly where resolution earns its keep"
        );
        assert!(!picked.contains(&by_human), "no re-judging a human ruling");
        Ok::<_, anyhow::Error>(())
    }
    .await;

    sqlx::query("DELETE FROM knowledge_bases WHERE id = $1")
        .bind(f.kb)
        .execute(&pool)
        .await?;
    run
}

/// Claiming after the ontology grows a new class must not paint over a human's decision either.
///
/// This one also guarantees that `unadopt_types` is correct: human rows never enter an adoption
/// batch, so an undo never runs into them and there is no need to restore `type_source`
/// separately.
#[tokio::test]
async fn adopting_a_new_class_does_not_claim_human_typed_entities() -> anyhow::Result<()> {
    let Some(url) = utopia_store::test_db::url() else {
        return Ok(());
    };
    let pool = PgPool::connect(&url).await?;
    let f = seed(&pool).await?;

    let run = async {
        // Both entities have had startup proposed by the model, but one of them was typed by a
        // human
        for (name, source) in [("Acme", "human"), ("Globex", "extracted")] {
            let id = entity(&pool, &f, name, Some(f.org_type), source).await?;
            sqlx::query("UPDATE entities SET proposed_type = 'startup' WHERE id = $1")
                .bind(id)
                .execute(&pool)
                .await?;
        }

        let (_, moved) = utopia_store::resolution::adopt_proposed_types(
            &pool,
            f.kb,
            f.sub_type,
            &["startup".to_string()],
            None,
        )
        .await?;
        assert_eq!(moved, 1, "only the one not set by a human gets claimed");

        let human: Option<Uuid> = sqlx::query_scalar(
            "SELECT type_id FROM entities WHERE kb_id = $1 AND canonical_name = 'Acme'",
        )
        .bind(f.kb)
        .fetch_one(&pool)
        .await?;
        assert_eq!(human, Some(f.org_type), "the human-set type is untouched");
        Ok::<_, anyhow::Error>(())
    }
    .await;

    sqlx::query("DELETE FROM knowledge_bases WHERE id = $1")
        .bind(f.kb)
        .execute(&pool)
        .await?;
    run
}

/// **The 0009 × P4 crossover, the easiest one of the lot to miss.**
///
/// After 0009, "no type" may itself be a human's decision -- they looked at this entity and
/// concluded the ontology holds no fitting class. But the guard on extraction promotion used to
/// look only at `type_key.is_none()`, which cannot tell "not judged yet" apart from "a human
/// judged it, and the answer is none", so the next extraction would fit a type onto it.
#[tokio::test]
async fn extraction_does_not_fill_in_a_type_a_human_left_empty() -> anyhow::Result<()> {
    let Some(url) = utopia_store::test_db::url() else {
        return Ok(());
    };
    let pool = PgPool::connect(&url).await?;
    let f = seed(&pool).await?;

    let run = async {
        // A human looked at it and concluded the ontology holds no fitting class → no type, and
        // that is a decision
        let decided = entity(&pool, &f, "Ambiguous Thing", None, "human").await?;
        // The one whose turn to be judged has not come yet
        let pending = entity(&pool, &f, "Other Thing", None, "extracted").await?;

        // **The promotion branch is only reachable through profile similarity**: with an empty
        // ctx, best is None and the whole block gets skipped. That is exactly how the first
        // version was written, and pulling the guard out failed no test -- the test was not
        // testing the thing it was there to test
        let ctx: Vec<f32> = vec![1.0, 0.0, 0.0];
        for id in [decided, pending] {
            sqlx::query(
                "UPDATE entities SET profile_embedding = $2::vector, profile_n = 1 WHERE id = $1",
            )
            .bind(id)
            .bind("[1,0,0]")
            .execute(&pool)
            .await?;
        }

        // Extraction meets a mention of the same name again and judges it organization. Cosine
        // = 1.0, far above SIM_ATTACH
        for id in [decided, pending] {
            let name: String =
                sqlx::query_scalar("SELECT canonical_name FROM entities WHERE id = $1")
                    .bind(id)
                    .fetch_one(&pool)
                    .await?;
            let _ = utopia_store::resolution::resolve_mention(
                &pool,
                f.kb,
                Some(f.org_type),
                &name,
                Some(&ctx),
            )
            .await?;
        }

        // Control: the one no human has ruled on **should** be promoted, otherwise this test
        // proves nothing about the guard doing its job
        let after_pending: Option<Uuid> =
            sqlx::query_scalar("SELECT type_id FROM entities WHERE id = $1")
                .bind(pending)
                .fetch_one(&pool)
                .await?;
        assert_eq!(
            after_pending,
            Some(f.org_type),
            "one not yet judged ought to be promoted by extraction -- otherwise the assertion below is vacuous"
        );

        let after_decided: Option<Uuid> =
            sqlx::query_scalar("SELECT type_id FROM entities WHERE id = $1")
                .bind(decided)
                .fetch_one(&pool)
                .await?;
        assert_eq!(
            after_decided, None,
            "the human said \"there just is no type\"; extraction must not fill one in for them"
        );
        assert_eq!(source_of(&pool, decided).await?, "human");
        Ok::<_, anyhow::Error>(())
    }
    .await;

    sqlx::query("DELETE FROM knowledge_bases WHERE id = $1")
        .bind(f.kb)
        .execute(&pool)
        .await?;
    run
}

/// `retype_entities` tells the sources apart with the `actor` parameter it already has: an
/// approval a human clicked is an endorsement and is protected; the engine's own automatic
/// ruling is not. No new parameter was needed for this -- it had been sitting there ever since
/// #112 added actor.
#[tokio::test]
async fn who_approved_a_retype_decides_whether_it_is_protected() -> anyhow::Result<()> {
    let Some(url) = utopia_store::test_db::url() else {
        return Ok(());
    };
    let pool = PgPool::connect(&url).await?;
    let f = seed(&pool).await?;
    let actor = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO users (id, org_id, email, display_name, password_hash)
         VALUES ($1, $2, $1 || '@p4a.test', 'p4a', 'x')",
    )
    .bind(actor)
    .bind(f.org)
    .execute(&pool)
    .await?;

    let run = async {
        let a = entity(&pool, &f, "Approved", Some(f.org_type), "extracted").await?;
        let b = entity(&pool, &f, "Auto", Some(f.org_type), "extracted").await?;

        utopia_store::resolution::retype_entities(&pool, f.kb, &[(a, f.sub_type)], Some(actor))
            .await?;
        utopia_store::resolution::retype_entities(&pool, f.kb, &[(b, f.sub_type)], None).await?;

        assert_eq!(source_of(&pool, a).await?, "human", "endorsed by a human");
        assert_eq!(source_of(&pool, b).await?, "inferred", "the engine is not");
        Ok::<_, anyhow::Error>(())
    }
    .await;

    sqlx::query("DELETE FROM knowledge_bases WHERE id = $1")
        .bind(f.kb)
        .execute(&pool)
        .await?;
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(actor)
        .execute(&pool)
        .await?;
    run
}

/// **An entity the engine has changed still has to be scooped up in the next round.**
///
/// This one guards against an accident that really happened: when type resolution wrote its
/// results, it passed "whoever clicked run" as `retype_entities`'s actor, and an actor present
/// means `type_source = 'human'` gets written. So **an entity that had been through resolution
/// once was never resolved again** -- a database without a single manual PATCH on record would,
/// one round later, return an empty preview list, and not a single error with it.
///
/// "Who clicked run" and "who judged what this entity is" are two different things. The former
/// is recorded in the `ontology.types_resolved` audit; only the latter should decide
/// `type_source`.
#[tokio::test]
async fn an_engine_retype_does_not_lock_the_entity_out_of_the_next_round() -> anyhow::Result<()> {
    let Some(url) = utopia_store::test_db::url() else {
        return Ok(());
    };
    let pool = PgPool::connect(&url).await?;
    let f = seed(&pool).await?;

    let run = async {
        let e = entity(&pool, &f, "Initech", Some(f.org_type), "extracted").await?;
        // Leave a specific_type behind: that keeps it qualifying under the "candidate condition"
        // no matter what, so whether it drops out comes down to type_source alone -- otherwise it
        // would rightly drop out once retyped to a leaf class, and the test would prove nothing
        sqlx::query("UPDATE entities SET specific_type = 'startup company' WHERE id = $1")
            .bind(e)
            .execute(&pool)
            .await?;
        // The engine's own automatic ruling: nobody endorsed this one
        utopia_store::resolution::retype_entities(&pool, f.kb, &[(e, f.sub_type)], None).await?;

        assert_eq!(
            source_of(&pool, e).await?,
            "inferred",
            "an engine ruling is not a human endorsement"
        );

        let picked = utopia_store::resolution::entities_for_type_resolution(&pool, f.kb, 100)
            .await?
            .into_iter()
            .map(|c| c.id)
            .collect::<std::collections::HashSet<_>>();
        assert!(
            picked.contains(&e),
            "one engine retype must not lock the entity out -- the ontology will keep growing and it has to stay re-judgeable"
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
