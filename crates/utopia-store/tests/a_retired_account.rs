//! Four hard properties of a deactivated account -- run against a real database (see
//! `users.deactivated_at`).
//!
//! All the risk of a soft delete is in "missing one filter": the moment a single path that
//! reads `users` forgets to carry `deactivated_at IS NULL`, deactivation becomes a decoration,
//! and **there will not be any error at all**. So what these pin down is not functions, it is
//! **paths**.
//!
//! The other way round, the attribution sites must **still be able to find it**: the
//! `actor_id` of audit events, merge logs and the retype ledger points at this person, and
//! those are audit material -- a person leaving does not mean the thing never happened.

use sqlx::PgPool;
use uuid::Uuid;

async fn org_with_two_admins(pool: &PgPool) -> anyhow::Result<(Uuid, Uuid, Uuid)> {
    let (org, a, b) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    sqlx::query("INSERT INTO organizations (id, name) VALUES ($1, 'retire-test')")
        .bind(org)
        .execute(pool)
        .await?;
    for (id, mail) in [(a, "a"), (b, "b")] {
        sqlx::query(
            "INSERT INTO users (id, org_id, email, display_name, password_hash, is_admin)
             VALUES ($1, $2, $3, 'u', 'x', TRUE)",
        )
        .bind(id)
        .bind(org)
        .bind(format!("{}-{mail}@retire.test", id.simple()))
        .execute(pool)
        .await?;
    }
    Ok((org, a, b))
}

#[tokio::test]
async fn a_retired_account_cannot_get_back_in() -> anyhow::Result<()> {
    let Some(url) = utopia_store::test_db::url() else {
        return Ok(());
    };
    let pool = PgPool::connect(&url).await?;
    let (org, a, b) = org_with_two_admins(&pool).await?;

    let run = async {
        let email: String = sqlx::query_scalar("SELECT email FROM users WHERE id = $1")
            .bind(b)
            .fetch_one(&pool)
            .await?;
        assert!(
            utopia_store::accounts::find_user_by_email(&pool, &email)
                .await?
                .is_some(),
            "it should be findable before deactivation"
        );

        utopia_store::accounts::deactivate_user(&pool, b, a).await?;

        // The login path
        assert!(
            utopia_store::accounts::find_user_by_email(&pool, &email)
                .await?
                .is_none(),
            "a deactivated account is still findable by email -- login cannot be stopped"
        );
        // **The path of the tokens already issued.** Session validation goes through
        // find_user_by_id, so deactivation takes effect immediately, with no need to wait for
        // the token to expire
        assert!(
            utopia_store::accounts::find_user_by_id(&pool, b)
                .await?
                .is_none(),
            "a deactivated account is still findable by id -- issued tokens still work"
        );

        // Attribution as before: the row is still there, it just got a timestamp
        let still_there: i64 = sqlx::query_scalar("SELECT count(*) FROM users WHERE id = $1")
            .bind(b)
            .fetch_one(&pool)
            .await?;
        assert_eq!(
            still_there, 1,
            "a soft delete must not drop the row -- audit needs it"
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

#[tokio::test]
async fn the_last_admin_and_oneself_are_protected() -> anyhow::Result<()> {
    let Some(url) = utopia_store::test_db::url() else {
        return Ok(());
    };
    let pool = PgPool::connect(&url).await?;
    let (org, a, b) = org_with_two_admins(&pool).await?;

    let run = async {
        assert!(
            utopia_store::accounts::deactivate_user(&pool, a, a)
                .await
                .is_err(),
            "cannot deactivate yourself -- this system has no super-admin layer above it, so once you are out nobody can put you back"
        );

        utopia_store::accounts::deactivate_user(&pool, b, a).await?;
        assert!(
            utopia_store::accounts::deactivate_user(&pool, a, b)
                .await
                .is_err(),
            "the last admin cannot be deactivated, or from then on the org has nobody who can manage members"
        );

        // Idempotent: deactivating again is not an error
        utopia_store::accounts::deactivate_user(&pool, b, a).await?;

        // After reactivation everything is as before
        utopia_store::accounts::reactivate_user(&pool, b).await?;
        assert!(
            utopia_store::accounts::find_user_by_id(&pool, b)
                .await?
                .is_some(),
            "it should be possible to log in again after reactivation"
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
