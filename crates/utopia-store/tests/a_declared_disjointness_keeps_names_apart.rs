//! 0016 B3: `owl:disjointWith` reaches resolution -- when the ontology declares two classes
//! disjoint, even the same name does not enter the review queue.
//!
//! Resolution decides "can these two classes point at the same thing" in three layers: the
//! hard-coded `CONFUSABLE_TYPE_KEYS` table, the class hierarchy (the same lineage counts as
//! confusable, #226), and disjointness declared in the ontology. What is guarded here is that
//! **a declaration takes precedence over the first two layers**:
//!
//! 1. With nothing declared the behaviour is unchanged: organization vs project goes to the
//!    queue by the hard-coded table, corporation vs federal_agency goes to the queue by the
//!    class hierarchy (they share the non-root ancestor organization).
//! 2. After declaring organization ⟂ project, a same-named organization / project are kept
//!    apart and do not enter the queue.
//! 3. After declaring corporation ⟂ agency, federal_agency (a subclass of agency) is kept
//!    apart from corporation too -- **disjointness is inherited**, declaring it on the parent
//!    is enough.
//!
//! Skipped rather than failed when there is no `UTOPIA_DATABASE_URL`. It builds its own fixture
//! and tears it down, and never touches an existing database.

use sqlx::PgPool;
use utopia_store::{ontology, resolution};
use uuid::Uuid;

struct Fx {
    org: Uuid,
    kb: Uuid,
    organization: Uuid,
    project: Uuid,
    corporation: Uuid,
    agency: Uuid,
    federal_agency: Uuid,
}

async fn seed(pool: &PgPool) -> anyhow::Result<Fx> {
    let (org, ws, kb) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    // There has to be a root on top: in the class-hierarchy rule a "shared ancestor" does not
    // count the root (in schema.org everything is a Thing, and counting it would make Person
    // and Organization kin), so organization needs a parent
    let (thing, organization, project, corporation, agency, federal_agency) = (
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
    );
    sqlx::query("INSERT INTO organizations (id, name) VALUES ($1, 'disjoint-test')")
        .bind(org)
        .execute(pool)
        .await?;
    sqlx::query("INSERT INTO workspaces (id, org_id, name) VALUES ($1, $2, 'disjoint-test')")
        .bind(ws)
        .bind(org)
        .execute(pool)
        .await?;
    sqlx::query(
        "INSERT INTO knowledge_bases (id, workspace_id, name) VALUES ($1, $2, 'disjoint-test')",
    )
    .bind(kb)
    .bind(ws)
    .execute(pool)
    .await?;
    for (id, key) in [
        (thing, "thing"),
        (organization, "organization"),
        (project, "project"),
        (corporation, "corporation"),
        (agency, "agency"),
        (federal_agency, "federal_agency"),
    ] {
        sqlx::query("INSERT INTO entity_types (id, kb_id, key, label) VALUES ($1, $2, $3, $3)")
            .bind(id)
            .bind(kb)
            .bind(key)
            .execute(pool)
            .await?;
    }
    for (child, parent) in [
        (organization, thing),
        (project, thing),
        (corporation, organization),
        (agency, organization),
        (federal_agency, agency),
    ] {
        sqlx::query("INSERT INTO entity_type_parents (child_id, parent_id) VALUES ($1, $2)")
            .bind(child)
            .bind(parent)
            .execute(pool)
            .await?;
    }
    Ok(Fx {
        org,
        kb,
        organization,
        project,
        corporation,
        agency,
        federal_agency,
    })
}

async fn entity(pool: &PgPool, f: &Fx, name: &str, type_id: Uuid) -> anyhow::Result<Uuid> {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO entities (id, kb_id, type_id, canonical_name) VALUES ($1, $2, $3, $4)",
    )
    .bind(id)
    .bind(f.kb)
    .bind(type_id)
    .bind(name)
    .execute(pool)
    .await?;
    Ok(id)
}

/// Resolve one mention and return who the "type drift" review pairs it attached point at
async fn drift_reviews(
    pool: &PgPool,
    f: &Fx,
    name: &str,
    type_id: Uuid,
) -> anyhow::Result<Vec<Uuid>> {
    let r = resolution::resolve_mention(pool, f.kb, Some(type_id), name, None).await?;
    assert!(
        r.created,
        "a cross-type same name is a new entity: keep apart, never merge"
    );
    Ok(r.reviews
        .iter()
        .filter(|x| x.reason.starts_with("type_drift|"))
        .map(|x| x.other_id)
        .collect())
}

#[tokio::test]
async fn a_declared_disjointness_keeps_names_apart() -> anyhow::Result<()> {
    let Some(url) = utopia_store::test_db::url() else {
        return Ok(());
    };
    let pool = PgPool::connect(&url).await?;
    let f = seed(&pool).await?;

    let run = async {
        // 1. Nothing declared: the hard-coded table and the class hierarchy work as before
        let orion = entity(&pool, &f, "Orion", f.organization).await?;
        assert_eq!(
            drift_reviews(&pool, &f, "Orion", f.project).await?,
            vec![orion],
            "organization vs project is confusable by the hard-coded list"
        );
        let acme = entity(&pool, &f, "Acme", f.corporation).await?;
        assert_eq!(
            drift_reviews(&pool, &f, "Acme", f.federal_agency).await?,
            vec![acme],
            "corporation vs federal_agency share the ancestor organization: kin, so Review"
        );

        // 2. Declare organization ⟂ project: the hard-coded table says confusable, the
        //    ontology says disjoint -- the ontology wins
        ontology::set_disjoint_for(&pool, f.kb, f.organization, &[f.project]).await?;
        let _vega = entity(&pool, &f, "Vega", f.organization).await?;
        assert!(
            drift_reviews(&pool, &f, "Vega", f.project)
                .await?
                .is_empty(),
            "a declared disjointness wins over the hard-coded list"
        );

        // 3. Declare corporation ⟂ agency: federal_agency is a subclass of agency, so the
        //    disjointness is inherited and the class hierarchy calling them kin no longer
        //    counts
        ontology::set_disjoint_for(&pool, f.kb, f.corporation, &[f.agency]).await?;
        let _beta = entity(&pool, &f, "Beta", f.corporation).await?;
        assert!(
            drift_reviews(&pool, &f, "Beta", f.federal_agency)
                .await?
                .is_empty(),
            "a disjointness declared on the parent reaches the child and wins over kinship"
        );
        // Asking it the other way round is the same: the table holds a row per direction, and
        // the inheritance walks the ancestor chain from the other end
        let _gamma = entity(&pool, &f, "Gamma", f.federal_agency).await?;
        assert!(
            drift_reviews(&pool, &f, "Gamma", f.corporation)
                .await?
                .is_empty(),
            "the declaration holds from either side"
        );

        // 4. Drop the declaration and the undeclared behaviour comes back -- an edit has to be
        //    undoable
        ontology::set_disjoint_for(&pool, f.kb, f.corporation, &[]).await?;
        let delta = entity(&pool, &f, "Delta", f.corporation).await?;
        assert_eq!(
            drift_reviews(&pool, &f, "Delta", f.federal_agency).await?,
            vec![delta],
            "with the declaration gone, kinship sends the pair to Review again"
        );
        Ok::<_, anyhow::Error>(())
    }
    .await;

    let _ = sqlx::query("DELETE FROM organizations WHERE id = $1")
        .bind(f.org)
        .execute(&pool)
        .await;
    run
}
