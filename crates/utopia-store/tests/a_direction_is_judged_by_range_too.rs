//! `judge_direction` looks at the range, not only at the domain (#222).
//!
//! It used to be that the moment the subject passed the domain check it was a Keep, and
//! nobody looked at the object violating the range: `headOf` has a domain of Agent, and in
//! schema.org a Project is an Agent too, so `Project Aurora head_of Li Ting` went into the
//! graph as-is. Here we reproduce that shape with the smallest possible ontology: a domain
//! that allows both ends, a range that only accepts companies, and a fact that only holds
//! when read the other way round must get swapped.

use sqlx::PgPool;
use utopia_store::ontology::{judge_direction, Fit};
use uuid::Uuid;

struct Fixture {
    /// domain = person | company, range = company
    leads: Uuid,
    alice: Uuid,
    acme: Uuid,
    globex: Uuid,
    /// An entity whose type has not been determined yet
    mystery: Uuid,
}

async fn seed(pool: &PgPool) -> anyhow::Result<Fixture> {
    let (org, ws, kb) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    let (person, company, leads) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    let (alice, acme, globex, mystery) = (
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
    );

    sqlx::query("INSERT INTO organizations (id, name) VALUES ($1, 'direction-test')")
        .bind(org)
        .execute(pool)
        .await?;
    sqlx::query("INSERT INTO workspaces (id, org_id, name) VALUES ($1, $2, 'direction-test')")
        .bind(ws)
        .bind(org)
        .execute(pool)
        .await?;
    sqlx::query(
        "INSERT INTO knowledge_bases (id, workspace_id, name) VALUES ($1, $2, 'direction-test')",
    )
    .bind(kb)
    .bind(ws)
    .execute(pool)
    .await?;
    for (id, key, label) in [
        (person, "person", "Person"),
        (company, "company", "Company"),
    ] {
        sqlx::query("INSERT INTO entity_types (id, kb_id, key, label) VALUES ($1, $2, $3, $4)")
            .bind(id)
            .bind(kb)
            .bind(key)
            .bind(label)
            .execute(pool)
            .await?;
    }
    sqlx::query(
        "INSERT INTO relation_types (id, kb_id, key, label) VALUES ($1, $2, 'leads', 'leads')",
    )
    .bind(leads)
    .bind(kb)
    .execute(pool)
    .await?;
    for ty in [person, company] {
        sqlx::query(
            "INSERT INTO relation_type_domains (relation_type_id, entity_type_id) VALUES ($1, $2)",
        )
        .bind(leads)
        .bind(ty)
        .execute(pool)
        .await?;
    }
    sqlx::query(
        "INSERT INTO relation_type_ranges (relation_type_id, entity_type_id) VALUES ($1, $2)",
    )
    .bind(leads)
    .bind(company)
    .execute(pool)
    .await?;
    for (id, ty, name) in [
        (alice, Some(person), "Alice"),
        (acme, Some(company), "Acme"),
        (globex, Some(company), "Globex"),
        (mystery, None, "Mystery"),
    ] {
        sqlx::query(
            "INSERT INTO entities (id, kb_id, type_id, canonical_name) VALUES ($1, $2, $3, $4)",
        )
        .bind(id)
        .bind(kb)
        .bind(ty)
        .bind(name)
        .execute(pool)
        .await?;
    }
    Ok(Fixture {
        leads,
        alice,
        acme,
        globex,
        mystery,
    })
}

#[tokio::test]
async fn direction_is_judged_by_range_too() -> anyhow::Result<()> {
    let Some(url) = utopia_store::test_db::url() else {
        return Ok(());
    };
    let pool = PgPool::connect(&url).await?;
    let f = seed(&pool).await?;

    // Both ends fit in the forward direction: business as usual
    assert_eq!(
        judge_direction(&pool, f.leads, f.alice, f.acme).await?,
        Fit::Keep
    );
    // The subject passes the domain check (a company is allowed as a subject too), but the
    // object is a person while the range only accepts companies.
    // This used to be a Keep -- exactly the Project Aurora head_of Li Ting shape; read the
    // other way round both ends hold, so it gets swapped
    assert_eq!(
        judge_direction(&pool, f.leads, f.acme, f.alice).await?,
        Fit::Swap
    );
    // The object's type has not been determined yet: "don't know" on the range side does not
    // count as a violation, and we must not drop the predicate over it
    assert_eq!(
        judge_direction(&pool, f.leads, f.alice, f.mystery).await?,
        Fit::Keep
    );
    // Two companies: forwards, the object fits the range and the subject fits the domain → Keep
    assert_eq!(
        judge_direction(&pool, f.leads, f.acme, f.globex).await?,
        Fit::Keep
    );
    Ok(())
}
