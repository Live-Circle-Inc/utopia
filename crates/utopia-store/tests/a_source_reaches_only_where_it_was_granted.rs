//! Data source grants (0014).
//!
//! The hole this plugs: registration is a deployment-level action, while the guard on mounting is
//! `require_kb(kb_id, Role::Admin)` -- an admin of the requester's own knowledge base. And the
//! mountable list returned every single source in the whole deployment. So an admin of any one
//! knowledge base could mount any production database into their own KB, and once mounted every
//! Viewer of that KB could run read-only SQL against it through `query_data`.
//!
//! Three things are nailed down here:
//!
//! - **Cannot see it**: an ungranted source does not enter the mountable list
//! - **Cannot mount it either**: the list filter only blocks "seeing it", while the mount endpoint
//!   is called with an id -- the guard has to be on both sides, and this one tests the endpoint
//!   side
//! - **Revoking really revokes**: revoking a grant unmounts whatever was already mounted along
//!   with it. If you only delete the grant row, data questions still read `kb_data_sources`, which
//!   amounts to the revocation not taking effect -- and a revocation that does not take effect is
//!   more dangerous than none at all

use sqlx::PgPool;
use utopia_store::datasources;
use uuid::Uuid;

struct Fixture {
    org: Uuid,
    /// The workspace it is granted to
    ours: Uuid,
    /// The workspace with no grant -- its knowledge bases must not be able to reach this source
    theirs: Uuid,
    our_kb: Uuid,
    their_kb: Uuid,
    source: Uuid,
    actor: Uuid,
}

async fn seed(pool: &PgPool) -> anyhow::Result<Fixture> {
    let org = Uuid::now_v7();
    let (ours, theirs) = (Uuid::now_v7(), Uuid::now_v7());
    let (our_kb, their_kb) = (Uuid::now_v7(), Uuid::now_v7());
    let (source, actor) = (Uuid::now_v7(), Uuid::now_v7());

    sqlx::query("INSERT INTO organizations (id, name) VALUES ($1, 'grant-test')")
        .bind(org)
        .execute(pool)
        .await?;
    for (id, name) in [(ours, "ours"), (theirs, "theirs")] {
        sqlx::query("INSERT INTO workspaces (id, org_id, name) VALUES ($1, $2, $3)")
            .bind(id)
            .bind(org)
            .bind(name)
            .execute(pool)
            .await?;
    }
    for (kb, ws, name) in [(our_kb, ours, "our-kb"), (their_kb, theirs, "their-kb")] {
        sqlx::query("INSERT INTO knowledge_bases (id, workspace_id, name) VALUES ($1, $2, $3)")
            .bind(kb)
            .bind(ws)
            .bind(name)
            .execute(pool)
            .await?;
    }
    sqlx::query(
        "INSERT INTO users (id, org_id, email, password_hash, display_name, is_admin)
         VALUES ($1, $2, $3, 'x', 'Grant Test', TRUE)",
    )
    .bind(actor)
    .bind(org)
    .bind(format!("{actor}@grant.test"))
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO data_sources (id, name, engine, conn_string, created_by)
         VALUES ($1, $2, 'postgres', 'postgres://u:p@db.test:5432/w', $3)",
    )
    .bind(source)
    .bind(format!("warehouse-{source}"))
    .bind(actor)
    .execute(pool)
    .await?;

    Ok(Fixture {
        org,
        ours,
        theirs,
        our_kb,
        their_kb,
        source,
        actor,
    })
}

#[tokio::test]
async fn a_source_reaches_only_where_it_was_granted() -> anyhow::Result<()> {
    let Some(url) = utopia_store::test_db::url() else {
        return Ok(());
    };
    let pool = PgPool::connect(&url).await?;
    let f = seed(&pool).await?;

    let run = async {
        // ---- 1. No grant = nobody can reach it
        assert!(
            datasources::granted_to_workspace(&pool, f.ours)
                .await?
                .is_empty(),
            "with nothing granted, the mountable list must be empty"
        );
        assert!(
            !datasources::is_granted(&pool, f.our_kb, f.source).await?,
            "with no grant, the mount endpoint's guard must say no"
        );

        // ---- 2. Granting one workspace leaves the other unaffected
        datasources::grant(&pool, f.source, f.ours, f.actor).await?;
        let ours = datasources::granted_to_workspace(&pool, f.ours).await?;
        assert_eq!(ours.len(), 1, "a workspace that was granted can see it");
        assert!(
            !ours[0].summary.contains("p@"),
            "the list may only carry the host:port/db summary; credentials stay server-side"
        );
        assert!(
            datasources::granted_to_workspace(&pool, f.theirs)
                .await?
                .is_empty(),
            "**grants are per workspace**: giving one is not giving the whole deployment"
        );

        // ---- 3. The guard on the endpoint side: the list filter only blocks "seeing it"
        assert!(datasources::is_granted(&pool, f.our_kb, f.source).await?);
        assert!(
            !datasources::is_granted(&pool, f.their_kb, f.source).await?,
            "an ungranted workspace cannot mount it even by POSTing a uuid it made up"
        );

        // ---- 4. One source can be granted to several workspaces (many-to-many, not one-to-many)
        datasources::grant(&pool, f.source, f.theirs, f.actor).await?;
        assert_eq!(
            datasources::grants_for_source(&pool, f.source).await?.len(),
            2,
            "the same warehouse has to be able to serve several workspaces at once"
        );
        datasources::grant(&pool, f.source, f.theirs, f.actor).await?;
        assert_eq!(
            datasources::grants_for_source(&pool, f.source).await?.len(),
            2,
            "granting the same thing twice is idempotent"
        );

        // ---- 5. Revoking takes the mounts with it
        datasources::mount(&pool, f.our_kb, f.source).await?;
        datasources::mount(&pool, f.their_kb, f.source).await?;
        let unmounted = datasources::revoke(&pool, f.source, f.theirs).await?;
        assert_eq!(unmounted, 1, "revoking unmounts that workspace's mounts");
        assert!(
            datasources::mounted(&pool, f.their_kb).await?.is_empty(),
            "**keeping the mount = revocation has no effect**: data questions read kb_data_sources"
        );
        assert_eq!(
            datasources::mounted(&pool, f.our_kb).await?.len(),
            1,
            "the other workspace's mount is not dragged into it"
        );
        Ok::<_, anyhow::Error>(())
    }
    .await;

    sqlx::query("DELETE FROM data_sources WHERE id = $1")
        .bind(f.source)
        .execute(&pool)
        .await?;
    sqlx::query("DELETE FROM organizations WHERE id = $1")
        .bind(f.org)
        .execute(&pool)
        .await?;
    run
}
