//! What happens after an unmatched phrasing is dismissed, run against a real database.
//!
//! This is all SQL, which `cargo check` cannot see. And the behaviour of this stretch **used
//! to be wrong and invisible**: `record_miss` carried a `WHERE dismissed_at IS NULL`, so one
//! click on "dismiss" stopped both the presenting and the counting. After a phrasing that
//! appeared once in the first document was dismissed, the next twenty documents all used it
//! and the count still sat at 1 -- the basis for that original judgement had long stopped
//! holding, and nobody could see it.
//!
//! What has to be pinned down is that **suppression and counting are separate**:
//!
//! - after a dismissal `record_miss` keeps accumulating
//! - `list_misses` no longer returns it (the suggestion and the auto-extend-the-ontology step
//!   are unchanged)
//! - `list_dismissed_misses` does return it, and with the **updated** count
//! - after `restore_miss` undoes it, it is back in the normal list with a count that is
//!   continuous rather than starting over
//!
//! Skipped rather than failed when there is no `UTOPIA_DATABASE_URL`. Builds and tears down
//! its own data, and never touches an existing KB.

use sqlx::PgPool;
use uuid::Uuid;

async fn fresh_kb(pool: &PgPool) -> anyhow::Result<Uuid> {
    let (org, ws, kb) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    sqlx::query("INSERT INTO organizations (id, name) VALUES ($1, 'dismissal-test')")
        .bind(org)
        .execute(pool)
        .await?;
    sqlx::query("INSERT INTO workspaces (id, org_id, name) VALUES ($1, $2, 'dismissal-test')")
        .bind(ws)
        .bind(org)
        .execute(pool)
        .await?;
    sqlx::query(
        "INSERT INTO knowledge_bases (id, workspace_id, name) VALUES ($1, $2, 'dismissal-test')",
    )
    .bind(kb)
    .bind(ws)
    .execute(pool)
    .await?;
    Ok(kb)
}

fn count_of(rows: &[utopia_core::models::OntologyMiss], key: &str) -> Option<i32> {
    rows.iter().find(|m| m.key == key).map(|m| m.count)
}

#[tokio::test]
async fn dismissing_stops_the_suggestion_but_not_the_counting() -> anyhow::Result<()> {
    let Some(url) = utopia_store::test_db::url() else {
        return Ok(());
    };
    let pool = PgPool::connect(&url).await?;
    let kb = fresh_kb(&pool).await?;

    let run = async {
        use utopia_store::ontology as ont;
        // It appeared once in the first document
        ont::record_miss(&pool, kb, "relation_type", "acquired", Some("A → B")).await?;
        assert_eq!(
            count_of(&ont::list_misses(&pool, kb).await?, "acquired"),
            Some(1)
        );

        // The user looks at "appeared 1 time" and judges it a one-off phrasing
        ont::dismiss_miss(&pool, kb, "relation_type", "acquired").await?;
        assert_eq!(
            count_of(&ont::list_misses(&pool, kb).await?, "acquired"),
            None,
            "after a dismissal it should not be in the suggestion list any more"
        );

        // The next two documents say it too. **The key assertion**: the count has to keep
        // going, otherwise the basis for that judgement goes stale and nobody knows
        ont::record_miss(&pool, kb, "relation_type", "acquired", Some("C → D")).await?;
        ont::record_miss(&pool, kb, "relation_type", "acquired", Some("E → F")).await?;
        assert_eq!(
            count_of(&ont::list_dismissed_misses(&pool, kb).await?, "acquired"),
            Some(3),
            "occurrences during the dismissal must still be recorded"
        );
        assert_eq!(
            count_of(&ont::list_misses(&pool, kb).await?, "acquired"),
            None,
            "the count is climbing, but the suppression stands"
        );

        // The human sees it has climbed to 3 and undoes the dismissal
        ont::restore_miss(&pool, kb, "relation_type", "acquired").await?;
        assert_eq!(
            count_of(&ont::list_misses(&pool, kb).await?, "acquired"),
            Some(3),
            "after the undo the count is continuous, not started over"
        );
        assert_eq!(
            count_of(&ont::list_dismissed_misses(&pool, kb).await?, "acquired"),
            None
        );
        Ok::<_, anyhow::Error>(())
    }
    .await;

    sqlx::query("DELETE FROM knowledge_bases WHERE id = $1")
        .bind(kb)
        .execute(&pool)
        .await?;
    run
}
