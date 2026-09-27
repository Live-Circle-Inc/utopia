//! Persistence and boundaries of `inverseOf` / `subPropertyOf` -- run against a real
//! database.
//!
//! **The first thing guarded here is knowledge-base isolation.** The foreign key on the
//! column says `REFERENCES relation_types(id)`, which knows nothing about `kb_id`: at
//! the database level, a relation in KB A can perfectly well point at a relation in
//! KB B. The RDF import path cannot get there by construction (it looks up by IRI inside
//! the current KB), but the HTTP endpoint takes a bare UUID -- and if that is not
//! blocked, holding any id at all is enough to make the reasoner read axioms across KBs,
//! and the edges it infers land in this KB carrying another KB's semantics.
//!
//! The frontend only lists relations from the current KB. **That is interface politeness,
//! not a boundary**: the endpoint itself takes a UUID, and one curl goes straight around
//! it. So the check has to be in the store layer, and the test has to be here.
//!
//! The other three are shape constraints from the same family: an attribute has no
//! inverse (its object is a literal value, so there is nothing to talk about), an
//! attribute cannot serve as someone else's inverse, and a sub-property cannot be itself
//! (there is a CHECK in the database, but hitting it is a 500 -- we have to say something
//! human before the CHECK is reached).

use sqlx::PgPool;
use utopia_core::models::RelationAxioms;
use uuid::Uuid;

/// Two KBs under one org. **Two of them is mandatory** -- the protagonist of this test
/// set is precisely the cross-KB line.
async fn two_kbs(pool: &PgPool) -> anyhow::Result<(Uuid, Uuid, Uuid)> {
    let (org, ws, a, b) = (
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
    );
    sqlx::query("INSERT INTO organizations (id, name) VALUES ($1, 'link-test')")
        .bind(org)
        .execute(pool)
        .await?;
    sqlx::query("INSERT INTO workspaces (id, org_id, name) VALUES ($1, $2, 'link-test')")
        .bind(ws)
        .bind(org)
        .execute(pool)
        .await?;
    for (id, name) in [(a, "link-a"), (b, "link-b")] {
        sqlx::query("INSERT INTO knowledge_bases (id, workspace_id, name) VALUES ($1, $2, $3)")
            .bind(id)
            .bind(ws)
            .bind(name)
            .execute(pool)
            .await?;
    }
    Ok((org, a, b))
}

/// Create the most ordinary relation there is, with no links on it at all.
async fn plain(pool: &PgPool, kb: Uuid, key: &str) -> anyhow::Result<Uuid> {
    Ok(utopia_store::ontology::create_relation_type(
        pool,
        kb,
        key,
        key,
        "state",
        RelationAxioms::default(),
        "",
        "relation",
        &[],
        &[],
        None,
        None,
    )
    .await?)
}

#[tokio::test]
async fn a_relation_points_only_inside_its_own_kb() -> anyhow::Result<()> {
    let Some(url) = utopia_store::test_db::url() else {
        return Ok(());
    };
    let pool = PgPool::connect(&url).await?;
    let (org, kb_a, kb_b) = two_kbs(&pool).await?;

    let run = async {
        let works_at = plain(&pool, kb_a, "works_at").await?;
        let employs = plain(&pool, kb_a, "employs").await?;
        let ceo_of = plain(&pool, kb_a, "ceo_of").await?;
        // A relation in the other KB. Deliberately the same name -- **same key,
        // different KB, exactly the kind most easily taken for one of our own**
        let foreign = plain(&pool, kb_b, "employs").await?;

        // ---- 1. Pointing across KBs: it should be refused right at creation
        let err = utopia_store::ontology::create_relation_type(
            &pool,
            kb_a,
            "leaks",
            "leaks",
            "state",
            RelationAxioms {
                inverse_of: Some(foreign),
                ..Default::default()
            },
            "",
            "relation",
            &[],
            &[],
            None,
            None,
        )
        .await
        .expect_err("a relation pointing into another knowledge base must be refused");
        assert!(
            format!("{err:?}").contains("unknown_relation"),
            "**the reason for the refusal must not reveal that the id exists somewhere \
             else** -- no distinction between \"does not exist\" and \"is in another KB\", \
             got {err:?}"
        );
        let leaked: i64 =
            sqlx::query_scalar("SELECT count(*) FROM relation_types WHERE kb_id = $1 AND key = $2")
                .bind(kb_a)
                .bind("leaks")
                .fetch_one(&pool)
                .await?;
        assert_eq!(leaked, 0, "the refused attempt must not leave half a row behind");

        // ---- 2. Pointing across KBs: an update should be refused just the same
        let err = utopia_store::ontology::update_relation_type(
            &pool,
            kb_a,
            works_at,
            "works at",
            "state",
            RelationAxioms {
                sub_property_of: Some(foreign),
                ..Default::default()
            },
            "",
            None,
            None,
            None,
            None,
        )
        .await
        .expect_err("changing it to point into another knowledge base must be refused too");
        assert!(
            format!("{err:?}").contains("unknown_relation"),
            "got {err:?}"
        );

        // ---- 3. Inside the same KB: it goes in, and it reads back out
        utopia_store::ontology::update_relation_type(
            &pool,
            kb_a,
            works_at,
            "works at",
            "state",
            RelationAxioms {
                inverse_of: Some(employs),
                ..Default::default()
            },
            "",
            None,
            None,
            None,
            None,
        )
        .await?;
        utopia_store::ontology::update_relation_type(
            &pool,
            kb_a,
            ceo_of,
            "ceo of",
            "state",
            RelationAxioms {
                sub_property_of: Some(works_at),
                ..Default::default()
            },
            "",
            None,
            None,
            None,
            None,
        )
        .await?;
        let views = utopia_store::ontology::relation_type_views(&pool, kb_a).await?;
        let find = |id: Uuid| views.iter().find(|v| v.id == id).expect("persisted");
        assert_eq!(
            find(works_at).inverse_of,
            Some(employs),
            "**the view has to return these two values** -- the dropdown needs to show \
             which one is currently selected; if they do not read back, the form opens \
             empty, and a single save wipes the declaration"
        );
        assert_eq!(find(ceo_of).sub_property_of, Some(works_at));

        // ---- 4. Omitted = cleared, the same rule as the six axioms above
        utopia_store::ontology::update_relation_type(
            &pool,
            kb_a,
            works_at,
            "works at",
            "state",
            RelationAxioms::default(),
            "",
            None,
            None,
            None,
            None,
        )
        .await?;
        let views = utopia_store::ontology::relation_type_views(&pool, kb_a).await?;
        assert_eq!(
            views.iter().find(|v| v.id == works_at).unwrap().inverse_of,
            None,
            "not passing it = clearing it. Half overwrite, half retain would make \"I \
             took the inverse off\" and \"I never touched the inverse\" look exactly alike"
        );

        // ---- 5. Attributes: they cannot have links, nor be the target of one
        let salary = utopia_store::ontology::create_relation_type(
            &pool,
            kb_a,
            "salary",
            "salary",
            "state",
            RelationAxioms::default(),
            "",
            "attribute",
            // An attribute must hang off at least one class; borrow a ready-made one here
            &[class(&pool, kb_a).await?],
            &[],
            Some("number"),
            None,
        )
        .await?;
        let err = utopia_store::ontology::update_relation_type(
            &pool,
            kb_a,
            ceo_of,
            "ceo of",
            "state",
            RelationAxioms {
                inverse_of: Some(salary),
                ..Default::default()
            },
            "",
            None,
            None,
            None,
            None,
        )
        .await
        .expect_err("an attribute cannot be an inverse -- its object is a literal value, so pointing back at it makes no sense");
        assert!(
            format!("{err:?}").contains("link_target_is_attr"),
            "got {err:?}"
        );
        let err = utopia_store::ontology::update_relation_type(
            &pool,
            kb_a,
            salary,
            "salary",
            "state",
            RelationAxioms {
                sub_property_of: Some(ceo_of),
                ..Default::default()
            },
            "",
            None,
            None,
            None,
            None,
        )
        .await
        .expect_err("an attribute cannot have a parent property of its own either");
        assert!(
            format!("{err:?}").contains("attr_has_no_link"),
            "got {err:?}"
        );

        // ---- 6. A sub-property cannot be itself. **There is a CHECK in the database,
        // but that is a 500**
        let err = utopia_store::ontology::update_relation_type(
            &pool,
            kb_a,
            ceo_of,
            "ceo of",
            "state",
            RelationAxioms {
                sub_property_of: Some(ceo_of),
                ..Default::default()
            },
            "",
            None,
            None,
            None,
            None,
        )
        .await
        .expect_err("a relation cannot be its own parent property");
        assert!(
            format!("{err:?}").contains("sub_property_self"),
            "we have to say something human before hitting the CHECK, instead of showing someone a 500; got {err:?}"
        );

        // Being its own inverse is **not blocked**: that amounts to symmetric, which is a
        // legal declaration. R0 will suggest `symmetric` as more direct, but that is a
        // suggestion, not an error
        utopia_store::ontology::update_relation_type(
            &pool,
            kb_a,
            employs,
            "employs",
            "state",
            RelationAxioms {
                inverse_of: Some(employs),
                ..Default::default()
            },
            "",
            None,
            None,
            None,
            None,
        )
        .await?;

        Ok::<_, anyhow::Error>(())
    }
    .await;

    sqlx::query("DELETE FROM organizations WHERE id = $1")
        .bind(org)
        .execute(&pool)
        .await?;
    let _ = kb_b;
    run
}

/// Lend a class to the attribute as its domain -- an attribute with no class cannot be
/// created.
async fn class(pool: &PgPool, kb: Uuid) -> anyhow::Result<Uuid> {
    Ok(utopia_store::ontology::create_entity_type(
        pool,
        kb,
        "person",
        "Person",
        "#888888",
        "circle",
        &[],
        "",
    )
    .await?)
}
