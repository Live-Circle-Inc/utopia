//! The axioms an ontology declares have to really land in the database -- run against a real
//! one.
//!
//! This line of defence comes from a defect that was already there: the docs on
//! `create_relation_types_bulk` said "`functional` / `inverse_functional` must be written down
//! the way the vocabulary has them, they must not default to false", while its SQL hard-coded
//! `FALSE, FALSE` -- the two arrays were bound but never made it into the `UNNEST`. Measured:
//! after installing FOAF (which declares 17 FunctionalProperty entries), the rows in the
//! database with functional true numbered **exactly zero**.
//!
//! Those two flags are what the temporal engine uses to close facts automatically. Getting them
//! wrong in either direction has different consequences: marking them true by mistake
//! manufactures conflicts in bulk (59 of them that time with `part_of`), while this was the
//! opposite direction -- everything that should have been true came out false, so **not one of
//! the temporal conflicts that should have been detected was**, silently.
//!
//! `cargo check` cannot see this kind of mistake (the types are all correct), and a unit test
//! cannot either (it lives inside a SQL string). The only thing that works is really writing a
//! row and reading it back.

use sqlx::PgPool;
use uuid::Uuid;

/// Build the smallest possible base: whether axioms land in the database has nothing to do
/// with the size of the ontology, one row is enough.
async fn kb(pool: &PgPool) -> anyhow::Result<(Uuid, Uuid)> {
    let (org, ws, kb) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    sqlx::query("INSERT INTO organizations (id, name) VALUES ($1, 'ax-test')")
        .bind(org)
        .execute(pool)
        .await?;
    sqlx::query("INSERT INTO workspaces (id, org_id, name) VALUES ($1, $2, 'ax-test')")
        .bind(ws)
        .bind(org)
        .execute(pool)
        .await?;
    sqlx::query("INSERT INTO knowledge_bases (id, workspace_id, name) VALUES ($1, $2, 'ax-test')")
        .bind(kb)
        .bind(ws)
        .execute(pool)
        .await?;
    Ok((kb, org))
}

#[tokio::test]
async fn every_axiom_survives_the_bulk_insert() -> anyhow::Result<()> {
    let Some(url) = utopia_store::test_db::url() else {
        return Ok(());
    };
    let pool = PgPool::connect(&url).await?;
    let (kb_id, org) = kb(&pool).await?;

    let run = async {
        let row = |key: &str, f: bool, i: bool, t: bool, s: bool, a: bool, r: bool| {
            utopia_store::ontology::BulkRelation {
                key: key.into(),
                label: key.into(),
                description: String::new(),
                iri: format!("http://ax.test/#{key}"),
                kind: "relation",
                datatype: None,
                functional: f,
                inverse_functional: i,
                transitive: t,
                symmetric: s,
                asymmetric: a,
                irreflexive: r,
            }
        };
        // One row all true, one all false: the all-false row guards "what was not declared
        // must not be written as true"
        utopia_store::ontology::create_relation_types_bulk(
            &pool,
            kb_id,
            &[
                row("all_true", true, true, true, true, true, true),
                row("all_false", false, false, false, false, false, false),
            ],
        )
        .await?;

        let got: Vec<(String, bool, bool, bool, bool, bool, bool)> = sqlx::query_as(
            "SELECT key, functional, inverse_functional,
                    is_transitive, is_symmetric, is_asymmetric, is_irreflexive
             FROM relation_types WHERE kb_id = $1 ORDER BY key",
        )
        .bind(kb_id)
        .fetch_all(&pool)
        .await?;

        let t = got
            .iter()
            .find(|r| r.0 == "all_true")
            .expect("all_true landed in the database");
        assert!(
            t.1 && t.2 && t.3 && t.4 && t.5 && t.6,
            "all six axiom flags should be written down as given, got {t:?}"
        );
        let f = got
            .iter()
            .find(|r| r.0 == "all_false")
            .expect("all_false landed in the database");
        assert!(
            !f.1 && !f.2 && !f.3 && !f.4 && !f.5 && !f.6,
            "an axiom that was not declared must not come out true from nowhere, got {f:?}"
        );
        Ok::<_, anyhow::Error>(())
    }
    .await;

    sqlx::query("DELETE FROM organizations WHERE id = $1")
        .bind(org)
        .execute(&pool)
        .await?;
    run
}
