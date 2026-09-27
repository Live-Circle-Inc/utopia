//! Retrieval candidates have to **bring their ancestors along**, otherwise the prompt holds no
//! generalising base class at all.
//!
//! Why this insists on a real database: `ancestors_of` lives entirely inside one stretch of
//! recursive SQL, and whether multiple inheritance and diamonds get through it, and whether it
//! expands the same ancestor twice, is something `cargo check` will not say one word about.
//!
//! What it guards is this measured causal chain: vector retrieval naturally favours the leaf
//! classes that literally appear in the text (in one chunk about Sutskever, out of 976 classes
//! `researcher` came 4th and `person` came 359th), and the top 40 held not one generalising
//! base class. So two symptoms turned up together -- the entity was judged a
//! `researcher` (in schema.org that is a subclass of `Audience`), and the signature of
//! `employee (organization → person)` decayed into `(* → *)`, so the model never saw the
//! direction constraint at all.
//!
//! This floor used to be held up by "the built-in classes are always there"; once the seeds
//! left the stage, the criterion was left hanging in the air.
//!
//! Skipped rather than failed when there is no `UTOPIA_DATABASE_URL`. Builds its own fixtures
//! and tears them down.

use sqlx::PgPool;
use uuid::Uuid;

/// Builds a diamond: `researcher → audience → thing`, `corporation → organization → thing`,
/// plus an `agent` with multiple inheritance (hanging under both thing and organization).
///
/// The diamond is the point: a recursion without de-duplication expands `thing` twice.
async fn seed(pool: &PgPool) -> anyhow::Result<(Uuid, Vec<(&'static str, Uuid)>)> {
    let (org, ws, kb) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    sqlx::query("INSERT INTO organizations (id, name) VALUES ($1, 'floor-test')")
        .bind(org)
        .execute(pool)
        .await?;
    sqlx::query("INSERT INTO workspaces (id, org_id, name) VALUES ($1, $2, 'floor-test')")
        .bind(ws)
        .bind(org)
        .execute(pool)
        .await?;
    sqlx::query(
        "INSERT INTO knowledge_bases (id, workspace_id, name) VALUES ($1, $2, 'floor-test')",
    )
    .bind(kb)
    .bind(ws)
    .execute(pool)
    .await?;

    let keys = [
        "thing",
        "audience",
        "researcher",
        "organization",
        "corporation",
        "agent",
    ];
    let mut ids: Vec<(&'static str, Uuid)> = Vec::new();
    for k in keys {
        let id = Uuid::now_v7();
        sqlx::query("INSERT INTO entity_types (id, kb_id, key, label) VALUES ($1, $2, $3, $3)")
            .bind(id)
            .bind(kb)
            .bind(k)
            .execute(pool)
            .await?;
        ids.push((k, id));
    }
    let get = |k: &str| ids.iter().find(|(n, _)| *n == k).unwrap().1;
    for (child, parent) in [
        ("audience", "thing"),
        ("researcher", "audience"),
        ("organization", "thing"),
        ("corporation", "organization"),
        // Multiple inheritance + diamond: agent reaches thing along both paths
        ("agent", "thing"),
        ("agent", "organization"),
    ] {
        sqlx::query(
            "INSERT INTO entity_type_parents (child_id, parent_id) VALUES ($1, $2)
             ON CONFLICT DO NOTHING",
        )
        .bind(get(child))
        .bind(get(parent))
        .execute(pool)
        .await?;
    }
    Ok((kb, ids))
}

#[tokio::test]
async fn a_retrieved_leaf_brings_its_ancestors() -> anyhow::Result<()> {
    let Some(url) = utopia_store::test_db::url() else {
        return Ok(());
    };
    let pool = PgPool::connect(&url).await?;
    // Sweep up before starting: a panicking assertion skips the teardown
    sqlx::query("DELETE FROM organizations WHERE name = 'floor-test'")
        .execute(&pool)
        .await?;
    let (kb, ids) = seed(&pool).await?;
    let id = |k: &str| ids.iter().find(|(n, _)| *n == k).unwrap().1;

    let run = async {
        // Retrieval only landed the leaves -- the text says "a researcher at the corporation"
        let leaves = vec![id("researcher"), id("corporation")];
        let anc = utopia_store::ontology::ancestors_of(&pool, &leaves).await?;

        for k in ["audience", "thing", "organization"] {
            assert!(
                anc.contains(&id(k)),
                "{k} missing from the ancestors -- no floor"
            );
        }
        // A class is not its own ancestor: the caller unions the two sets, so a duplicate
        // would only be noise
        for k in ["researcher", "corporation"] {
            assert!(
                !anc.contains(&id(k)),
                "{k} is itself; it must not be an ancestor"
            );
        }

        // **Diamond de-duplication**: agent reaches thing along two paths, so thing should
        // appear exactly once
        let anc2 = utopia_store::ontology::ancestors_of(&pool, &[id("agent")]).await?;
        let things = anc2.iter().filter(|x| **x == id("thing")).count();
        assert_eq!(
            things, 1,
            "diamond inheritance expanded thing {things} times"
        );
        assert!(
            anc2.contains(&id("organization")),
            "the other inheritance leg is gone"
        );

        // Empty input must not blow up, and must not scan the whole table
        assert!(utopia_store::ontology::ancestors_of(&pool, &[])
            .await?
            .is_empty());
        Ok::<(), anyhow::Error>(())
    }
    .await;

    sqlx::query("DELETE FROM organizations WHERE name = 'floor-test'")
        .execute(&pool)
        .await?;
    let _ = kb;
    run
}
