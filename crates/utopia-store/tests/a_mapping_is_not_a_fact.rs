//! Two hard properties of semantic-layer mappings -- exercised against a real database (see
//! `docs/decisions/0011`).
//!
//! Both are what moving this out of the fact ledger bought, and both are exactly what could not
//! be done before:
//!
//! 1. **One (concept, source) has exactly one row.** That uniqueness used to hide inside the
//!    `object_value` JSONB, where the database could not see it, so it could only be closed
//!    explicitly by the confirmation flow -- that is, by process rather than by a constraint.
//!    Now it is the primary key.
//! 2. **Something already ruled on is not pushed back into the queue by the next round of
//!    exploration.** We stepped in this same hole once already (over in `ontology_proposals`):
//!    a re-run will inevitably compute the rejected row again, and not blocking it means every
//!    run wipes out a human's rejection once more.

use sqlx::PgPool;
use uuid::Uuid;

async fn fixture(pool: &PgPool) -> anyhow::Result<(Uuid, Uuid, Uuid)> {
    let (org, ws, kb, ent) = (
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
    );
    sqlx::query("INSERT INTO organizations (id, name) VALUES ($1, 'map-test')")
        .bind(org)
        .execute(pool)
        .await?;
    sqlx::query("INSERT INTO workspaces (id, org_id, name) VALUES ($1, $2, 'map-test')")
        .bind(ws)
        .bind(org)
        .execute(pool)
        .await?;
    sqlx::query("INSERT INTO knowledge_bases (id, workspace_id, name) VALUES ($1, $2, 'map-test')")
        .bind(kb)
        .bind(ws)
        .execute(pool)
        .await?;
    sqlx::query(
        "INSERT INTO entities (id, kb_id, canonical_name, aliases) VALUES ($1, $2, '营收', '{}')",
    )
    .bind(ent)
    .bind(kb)
    .execute(pool)
    .await?;
    Ok((org, kb, ent))
}

#[tokio::test]
async fn one_concept_one_source_one_mapping() -> anyhow::Result<()> {
    let Some(url) = utopia_store::test_db::url() else {
        return Ok(());
    };
    let pool = PgPool::connect(&url).await?;
    let (_org, kb, ent) = fixture(&pool).await?;

    let run = async {
        let p = |t: &'static str| {
            utopia_store::mappings::propose(
                &pool,
                kb,
                ent,
                "warehouse",
                Some(t),
                None,
                None,
                None,
                None,
                false,
            )
        };
        let a = p("orders").await?;
        let b = p("orders_v2").await?;
        assert_eq!(a, b, "one (concept, source) should be one row, not two");

        let got = utopia_store::mappings::proposed(&pool, kb, 100, 0).await?;
        assert_eq!(got.len(), 1, "there should be exactly one");
        assert_eq!(
            got[0].table_name.as_deref(),
            Some("orders_v2"),
            "re-running exploration should refresh the definition"
        );

        // A different source is a different row: the same concept having different definitions
        // in different sources is supported on purpose
        utopia_store::mappings::propose(
            &pool,
            kb,
            ent,
            "lakehouse",
            Some("f_orders"),
            None,
            None,
            None,
            None,
            false,
        )
        .await?;
        assert_eq!(
            utopia_store::mappings::proposed(&pool, kb, 100, 0)
                .await?
                .len(),
            2,
            "each source should have its own row"
        );
        Ok::<_, anyhow::Error>(())
    }
    .await;

    // **Only the knowledge base is deleted, not the org/user.** Users are soft-deleted, and
    // production code contains no `DELETE FROM users` -- so a test should not invent an action
    // the product does not have either; otherwise the foreign-key constraint it runs into is
    // the fixture's own problem and gets mistaken for a product defect (this has happened)
    sqlx::query("DELETE FROM knowledge_bases WHERE id = $1")
        .bind(kb)
        .execute(&pool)
        .await?;
    run
}

#[tokio::test]
async fn a_rejected_mapping_does_not_come_back() -> anyhow::Result<()> {
    let Some(url) = utopia_store::test_db::url() else {
        return Ok(());
    };
    let pool = PgPool::connect(&url).await?;
    let (org, kb, ent) = fixture(&pool).await?;

    let run = async {
        let user = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO users (id, org_id, email, display_name, password_hash)
             VALUES ($1, $2, $1 || '@m.test', 'm', 'x')",
        )
        .bind(user)
        .bind(org)
        .execute(&pool)
        .await?;

        let id = utopia_store::mappings::propose(
            &pool,
            kb,
            ent,
            "warehouse",
            Some("orders"),
            None,
            None,
            None,
            None,
            false,
        )
        .await?;
        utopia_store::mappings::decide(&pool, kb, id, "rejected", user).await?;

        // The next round of exploration will compute the same row again -- it must not be
        // pushed back into the queue
        utopia_store::mappings::propose(
            &pool,
            kb,
            ent,
            "warehouse",
            Some("orders"),
            None,
            None,
            None,
            None,
            false,
        )
        .await?;
        assert!(
            utopia_store::mappings::proposed(&pool, kb, 100, 0)
                .await?
                .is_empty(),
            "a rejected mapping should not be queued again"
        );
        // By the same token a confirmed one must not be overwritten by exploration
        assert!(
            utopia_store::mappings::confirmed(&pool, kb, 100)
                .await?
                .is_empty(),
            "a rejected mapping should not show up in the confirmed list either"
        );
        Ok::<_, anyhow::Error>(())
    }
    .await;

    // **Only the knowledge base is deleted, not the org/user.** Users are soft-deleted, and
    // production code contains no `DELETE FROM users` -- so a test should not invent an action
    // the product does not have either; otherwise the foreign-key constraint it runs into is
    // the fixture's own problem and gets mistaken for a product defect (this has happened)
    sqlx::query("DELETE FROM knowledge_bases WHERE id = $1")
        .bind(kb)
        .execute(&pool)
        .await?;
    run
}
