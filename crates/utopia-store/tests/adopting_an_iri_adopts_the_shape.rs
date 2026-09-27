//! Once a class is adopted by a vocabulary, **the shape has to tell the truth about it**.
//!
//! What the shape carries is provenance: square = declared by a vocabulary, circle = grown out
//! of the corpus. But `adopt_iri_onto_key` was written back when the shape did not yet mean
//! provenance, and it only writes the IRI -- so importing schema.org into a KB that already
//! has `person` / `organization` leaves those classes **holding an IRI while still being
//! circles**. Having an IRI and being a circle means the picture is lying.
//!
//! This gap can only be hit by actually running an import once: looking at the signature of
//! `adopt_iri_onto_key` and its call sites, nothing hints that "there is another field that
//! should change along with it". It has been hit for real (5 classes in the demo KB caught
//! it).
//!
//! Skipped rather than failed when there is no `UTOPIA_DATABASE_URL`. Builds and tears down
//! its own data, and never touches an existing KB.

use sqlx::PgPool;
use uuid::Uuid;

#[tokio::test]
async fn adopting_an_iri_turns_the_class_square() -> anyhow::Result<()> {
    let Some(url) = utopia_store::test_db::url() else {
        return Ok(());
    };
    let pool = PgPool::connect(&url).await?;
    // Sweep the floor before starting: a panicking assertion skips the teardown, so one
    // failure leaves rubbish behind for the next run
    sqlx::query("DELETE FROM organizations WHERE name = 'adopt-shape-test'")
        .execute(&pool)
        .await?;

    let (org, ws, kb) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    sqlx::query("INSERT INTO organizations (id, name) VALUES ($1, 'adopt-shape-test')")
        .bind(org)
        .execute(&pool)
        .await?;
    sqlx::query("INSERT INTO workspaces (id, org_id, name) VALUES ($1, $2, 'adopt-shape-test')")
        .bind(ws)
        .bind(org)
        .execute(&pool)
        .await?;
    sqlx::query(
        "INSERT INTO knowledge_bases (id, workspace_id, name) VALUES ($1, $2, 'adopt-shape-test')",
    )
    .bind(kb)
    .bind(ws)
    .execute(&pool)
    .await?;

    let run = async {
        // A class "grown out of the corpus": no IRI, so it is a circle
        let grown = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO entity_types (id, kb_id, key, label, color, shape)
             VALUES ($1, $2, 'person', 'Person', '#7fd0ff', 'circle')",
        )
        .bind(grown)
        .bind(kb)
        .execute(&pool)
        .await?;

        let shape: String = sqlx::query_scalar("SELECT shape FROM entity_types WHERE id = $1")
            .bind(grown)
            .fetch_one(&pool)
            .await?;
        assert_eq!(shape, "circle", "should be a circle before it is adopted");

        // Import a vocabulary, adopting schema.org's Person onto this key
        let adopted = utopia_store::ontology::adopt_iri_onto_key(
            &pool,
            kb,
            "person",
            "https://schema.org/Person",
        )
        .await?;
        assert_eq!(adopted, Some(grown), "adopted onto the existing class");

        let (iri, shape, color): (Option<String>, String, String) =
            sqlx::query_as("SELECT iri, shape, color FROM entity_types WHERE id = $1")
                .bind(grown)
                .fetch_one(&pool)
                .await?;
        assert_eq!(iri.as_deref(), Some("https://schema.org/Person"));
        assert_eq!(
            shape, "square",
            "after adoption it is a vocabulary-declared class, so the shape has to tell the truth"
        );
        // **The colour must not move**: colour is identity (the same key is always the same
        // colour), and what adoption changes is provenance, not identity
        assert_eq!(
            color, "#7fd0ff",
            "adoption must not touch the colour the user is used to"
        );

        // Idempotent: adopting again should not change anything (the iri is already non-null,
        // so the UPDATE does not match)
        let again = utopia_store::ontology::adopt_iri_onto_key(
            &pool,
            kb,
            "person",
            "https://example.org/OtherPerson",
        )
        .await?;
        assert_eq!(
            again, None,
            "an adopted class must not be stolen by another vocabulary"
        );
        let iri2: Option<String> = sqlx::query_scalar("SELECT iri FROM entity_types WHERE id = $1")
            .bind(grown)
            .fetch_one(&pool)
            .await?;
        assert_eq!(iri2.as_deref(), Some("https://schema.org/Person"));

        Ok::<(), anyhow::Error>(())
    }
    .await;

    sqlx::query("DELETE FROM organizations WHERE name = 'adopt-shape-test'")
        .execute(&pool)
        .await?;
    run
}
