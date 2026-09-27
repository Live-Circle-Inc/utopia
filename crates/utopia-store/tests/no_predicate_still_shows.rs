//! A fact without a predicate **must not disappear from the read paths** (see
//! `facts.predicate_id`).
//!
//! Why this insists on a real database: once `facts.predicate_id` became nullable, the
//! `JOIN relation_types` in twenty-odd read queries all turned into silent filters -- an inner
//! join drops NULL rows with no error and no warning, and neither `cargo check` nor clippy says
//! a single word about it. This is the same trap as 0009's `NULL <> uuid` seen from the other
//! side: under three-valued logic "no value" gets treated as "no match".
//!
//! This test guards exactly that line: **build a fact with no predicate, then demand that every
//! read path can still see it**. Turn any single `LEFT JOIN relation_types` back into a `JOIN`
//! and this has to go red.
//!
//! It guards the display wording along the way too: `fact_surface_predicate` takes the most
//! frequently occurring phrasing, so the same fact is called the same name in the graph, in the
//! entity panel, and in the change history.
//!
//! Skips instead of failing when `UTOPIA_DATABASE_URL` is absent. It builds its own fixtures and
//! tears them down again; it never touches an existing database.

use sqlx::PgPool;
use uuid::Uuid;

struct Fixture {
    kb: Uuid,
    subject: Uuid,
    doc: Uuid,
    /// A fact with no predicate, but whose evidence kept the original phrasing
    surfaced: Uuid,
    /// A fact with no predicate and not even an original phrasing (the older legacy rows look
    /// like this)
    mute: Uuid,
}

async fn seed(pool: &PgPool) -> anyhow::Result<Fixture> {
    let (org, ws, kb) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    let etype = Uuid::now_v7();
    let (subject, object) = (Uuid::now_v7(), Uuid::now_v7());
    let (src, doc, chunk_a, chunk_b) = (
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
    );
    let (surfaced, mute) = (Uuid::now_v7(), Uuid::now_v7());

    sqlx::query("INSERT INTO organizations (id, name) VALUES ($1, 'no-predicate-test')")
        .bind(org)
        .execute(pool)
        .await?;
    sqlx::query("INSERT INTO workspaces (id, org_id, name) VALUES ($1, $2, 'no-predicate-test')")
        .bind(ws)
        .bind(org)
        .execute(pool)
        .await?;
    sqlx::query(
        "INSERT INTO knowledge_bases (id, workspace_id, name) VALUES ($1, $2, 'no-predicate-test')",
    )
    .bind(kb)
    .bind(ws)
    .execute(pool)
    .await?;
    sqlx::query("INSERT INTO entity_types (id, kb_id, key, label) VALUES ($1, $2, 'org', '组织')")
        .bind(etype)
        .bind(kb)
        .execute(pool)
        .await?;
    for (id, name) in [(subject, "Acme"), (object, "Beta")] {
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

    sqlx::query("INSERT INTO sources (id, kb_id, name) VALUES ($1, $2, '临时来源')")
        .bind(src)
        .bind(kb)
        .execute(pool)
        .await?;
    sqlx::query(
        "INSERT INTO documents (id, kb_id, source_id, filename, sha256, status)
         VALUES ($1, $2, $3, 'note.md', 'nopredicate', 'ready')",
    )
    .bind(doc)
    .bind(kb)
    .bind(src)
    .execute(pool)
    .await?;
    for (id, seq, text) in [
        (chunk_a, 0i32, "Acme acquired Beta."),
        (chunk_b, 1i32, "Acme acquired Beta again."),
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

    // Neither fact has a predicate -- the ontology holds no matching relation, which is exactly
    // the state a nullable predicate is there to express
    for (id, from) in [
        (surfaced, "2020-01-01T00:00:00Z"),
        (mute, "2021-01-01T00:00:00Z"),
    ] {
        sqlx::query(
            "INSERT INTO facts (id, kb_id, subject_id, predicate_id, object_id,
                                valid_from, valid_from_precision, confidence)
             VALUES ($1, $2, $3, NULL, $4, $5, 'day', 0.9)",
        )
        .bind(id)
        .bind(kb)
        .bind(subject)
        .bind(object)
        .bind(from.parse::<chrono::DateTime<chrono::Utc>>()?)
        .execute(pool)
        .await?;
    }

    // surfaced has three pieces of evidence: acquired twice, bought once. The mode is acquired --
    // which also exercises fact_surface_predicate's "take the most frequent one".
    // mute has a single piece of evidence with no original phrasing, simulating the old data from
    // before add_evidence recorded it unconditionally
    for (fact, chunk, pred) in [
        (surfaced, chunk_a, Some("acquired")),
        (surfaced, chunk_b, Some("acquired")),
        (surfaced, chunk_a, Some("bought")),
        (mute, chunk_a, None),
    ] {
        sqlx::query(
            "INSERT INTO fact_evidence (fact_id, chunk_id, document_id, proposed_predicate, quote)
             VALUES ($1, $2, $3, $4, 'Acme acquired Beta.')
             ON CONFLICT DO NOTHING",
        )
        .bind(fact)
        .bind(chunk)
        .bind(doc)
        .bind(pred)
        .execute(pool)
        .await?;
    }

    Ok(Fixture {
        kb,
        subject,
        doc,
        surfaced,
        mute,
    })
}

#[tokio::test]
async fn a_fact_without_a_predicate_is_still_visible_everywhere() -> anyhow::Result<()> {
    let Some(url) = utopia_store::test_db::url() else {
        return Ok(());
    };
    let pool = PgPool::connect(&url).await?;
    // **Sweep up before starting.** A panicking assertion skips the teardown below (it only
    // catches the Err path), so one failing run leaves garbage behind for the next one. Cleaning
    // up here is cheaper than rewriting every assertion to return Err
    sqlx::query("DELETE FROM organizations WHERE name = 'no-predicate-test'")
        .execute(&pool)
        .await?;
    let f = seed(&pool).await?;

    let run = async {
        // 1. Display wording: the mode wins, and it is not just a random one of them
        let word: Option<String> = sqlx::query_scalar("SELECT fact_surface_predicate($1)")
            .bind(f.surfaced)
            .fetch_one(&pool)
            .await?;
        assert_eq!(word.as_deref(), Some("acquired"), "most frequent wins");

        let none: Option<String> = sqlx::query_scalar("SELECT fact_surface_predicate($1)")
            .bind(f.mute)
            .fetch_one(&pool)
            .await?;
        assert!(none.is_none(), "empty when there is no phrasing at all");

        // 2. Graph edges: both have to be there, and both marked inferred
        let (_, edges) = utopia_store::graph::neighborhood(&pool, f.kb, f.subject, 1, None).await?;
        let ours: Vec<_> = edges
            .iter()
            .filter(|e| e.id == f.surfaced || e.id == f.mute)
            .collect();
        assert_eq!(ours.len(), 2, "an inner join swallows the edge whole");
        let e = ours.iter().find(|e| e.id == f.surfaced).unwrap();
        assert_eq!(e.label.as_deref(), Some("acquired"));
        assert!(e.inferred, "the UI must show the word came from the text");
        let m = ours.iter().find(|e| e.id == f.mute).unwrap();
        assert!(
            m.label.is_none(),
            "if it cannot be named it stays unnamed; it must not fall back to \"related to\""
        );

        // 3. The entity panel
        let (_, facts) = utopia_store::graph::entity_detail(&pool, f.kb, f.subject).await?;
        let panel: Vec<_> = facts
            .iter()
            .filter(|x| x.id == f.surfaced || x.id == f.mute)
            .collect();
        assert_eq!(panel.len(), 2, "the entity panel dropped the fact");
        let s = panel.iter().find(|x| x.id == f.surfaced).unwrap();
        assert_eq!(s.predicate_label.as_deref(), Some("acquired"));
        assert!(s.inferred);
        assert!(s.temporal.is_none(), "no predicate, no temporal category");

        // 4. Document output (DocViewer's chunk-by-chunk review)
        let chunk_facts = utopia_store::graph::document_extractions(&pool, f.doc).await?;
        assert!(
            chunk_facts.iter().any(|c| c.fact_id == f.surfaced),
            "the document page cannot see the fact with no predicate"
        );
        assert!(
            chunk_facts.iter().any(|c| c.fact_id == f.mute),
            "the document page cannot see the fact with no phrasing"
        );

        // 5. Entity history (the outer FROM is a CTE; get the alias wrong and it errors out
        //    with missing FROM-clause)
        let (hist, _) = utopia_store::graph::entity_history(&pool, f.kb, f.subject, 50, 0).await?;
        let h = hist.iter().find(|e| e.fact_id == Some(f.surfaced));
        assert!(h.is_some(), "entity history dropped the fact");
        assert_eq!(h.unwrap().predicate_label.as_deref(), Some("acquired"));

        // 6. The ledger change stream
        let changes = utopia_store::graph::graph_changes(
            &pool,
            f.kb,
            "2000-01-01T00:00:00Z".parse()?,
            "2100-01-01T00:00:00Z".parse()?,
            None,
            None,
            50,
        )
        .await?;
        let c = changes.iter().find(|c| c.fact_id == f.surfaced);
        assert!(c.is_some(), "the change stream dropped the fact");
        assert_eq!(c.unwrap().predicate_label.as_deref(), Some("acquired"));

        // 7. **Nothing can grow it back any more.**
        //
        // The history of this one is worth keeping: the first version was a vacuous assertion
        // (just `SELECT … WHERE builtin`, while the fixture built its database from raw SQL and
        // had never been seeded, so the assertion held over an empty field); the second version
        // added a control group -- call `ensure_default_ontology` first to plant the seeds, then
        // confirm `related_to` is not among them. Because seven minutes after the deletion, the
        // code planted the row right back again.
        //
        // Now **even the seeding function is gone**: 0009 dropped the built-in entity types,
        // 0010 and `#125` dropped the seed relations, 0011 moved `mapped_to` over to
        // `concept_mappings`, and `ensure_default_ontology` left the stage along with them. So
        // this now guards the stronger property instead: **nowhere on the database-creation path
        // does any relation get hard-shoved in by code**, and the `builtin` column is forever
        // empty in a fresh database.
        let seeded: Vec<String> =
            sqlx::query_scalar("SELECT key FROM relation_types WHERE kb_id = $1 AND builtin")
                .bind(f.kb)
                .fetch_all(&pool)
                .await?;
        assert!(
            seeded.is_empty(),
            "the ontology should hold no relation hard-shoved in by code, got {seeded:?}"
        );

        // Scoped to this kb, not the whole database: a database-wide count gets polluted by
        // other tests running in parallel and by leftovers from the previous round, which makes
        // for an assertion that goes red for no reason. This one already guards what needs
        // guarding -- once the seed tables are planted, the ontology holds no "says nothing at
        // all" relation for the model to pick

        Ok::<(), anyhow::Error>(())
    }
    .await;

    // Tear the temporary data down whether or not the assertions blew up (everything
    // underneath is ON DELETE CASCADE)
    sqlx::query("DELETE FROM organizations WHERE name = 'no-predicate-test'")
        .execute(&pool)
        .await?;
    run
}
