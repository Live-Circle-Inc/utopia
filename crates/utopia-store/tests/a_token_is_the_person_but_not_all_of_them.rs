//! Personal access tokens (0014 / migration 0017).
//!
//! This kind of token is for MCP clients: long-lived, configured into a file on someone
//! else's machine, acting as the person who issued it. So it has to be revocable, it has
//! to be able to expire, and **its reach can only be smaller than that person's**.
//!
//! Five things nailed down:
//!
//! - **The plaintext appears exactly once**. The database holds only the hash; with the
//!   whole table in hand you still cannot reconstruct that string
//! - **Revocation takes effect immediately**. And it stamps rather than deletes the row --
//!   "this key once existed" has to stay answerable
//! - **Expiry takes effect immediately**, decided inside SQL, not after fetching the row
//! - **`kb_ids` only narrows**. A token pinned to one KB cannot reach another
//! - **`last_used_at` does get written**. Before revoking, a person has to be able to
//!   answer "is this one still in use"

use chrono::{Duration, Utc};
use sqlx::PgPool;
use utopia_store::tokens;
use uuid::Uuid;

struct Fixture {
    org: Uuid,
    user: Uuid,
    kb_a: Uuid,
    kb_b: Uuid,
}

async fn seed(pool: &PgPool) -> anyhow::Result<Fixture> {
    let org = Uuid::now_v7();
    let ws = Uuid::now_v7();
    let user = Uuid::now_v7();
    let (kb_a, kb_b) = (Uuid::now_v7(), Uuid::now_v7());

    sqlx::query("INSERT INTO organizations (id, name) VALUES ($1, 'token-test')")
        .bind(org)
        .execute(pool)
        .await?;
    sqlx::query("INSERT INTO workspaces (id, org_id, name) VALUES ($1, $2, 'token-test')")
        .bind(ws)
        .bind(org)
        .execute(pool)
        .await?;
    for (kb, name) in [(kb_a, "kb-a"), (kb_b, "kb-b")] {
        sqlx::query("INSERT INTO knowledge_bases (id, workspace_id, name) VALUES ($1, $2, $3)")
            .bind(kb)
            .bind(ws)
            .bind(name)
            .execute(pool)
            .await?;
    }
    sqlx::query(
        "INSERT INTO users (id, org_id, email, password_hash, display_name)
         VALUES ($1, $2, $3, 'x', 'Token Test')",
    )
    .bind(user)
    .bind(org)
    .bind(format!("{user}@token.test"))
    .execute(pool)
    .await?;

    Ok(Fixture {
        org,
        user,
        kb_a,
        kb_b,
    })
}

#[tokio::test]
async fn a_token_is_the_person_but_not_all_of_them() -> anyhow::Result<()> {
    let Some(url) = utopia_store::test_db::url() else {
        return Ok(());
    };
    let pool = PgPool::connect(&url).await?;
    let f = seed(&pool).await?;

    let run = async {
        // ---- 1. Issue one; the plaintext is there to be had this once and no more
        let (view, plain) = tokens::issue(&pool, f.user, "my laptop", "read", None, None).await?;
        assert!(plain.starts_with("utp_pat_"), "the prefix names the kind");
        assert_eq!(view.scope, "read", "**read-only by default**: tick write");
        assert!(view.revoked_at.is_none());

        let stored: String =
            sqlx::query_scalar("SELECT token_hash FROM personal_tokens WHERE id = $1")
                .bind(view.id)
                .fetch_one(&pool)
                .await?;
        assert_ne!(stored, plain, "what the database stores must be a hash");
        assert!(
            !stored.contains(&plain[8..24]),
            "**no fragment of the plaintext should ever appear in the database** -- \
             with the whole table in hand you still cannot reconstruct it"
        );

        // ---- 2. It authenticates back, and writes last_used_at while it is at it
        let auth = tokens::authenticate(&pool, &plain).await?;
        assert_eq!(auth.user_id, f.user, "a token acts as its issuer");
        assert!(!auth.can_write(), "a read token cannot write");
        let used: Option<chrono::DateTime<Utc>> =
            sqlx::query_scalar("SELECT last_used_at FROM personal_tokens WHERE id = $1")
                .bind(view.id)
                .fetch_one(&pool)
                .await?;
        assert!(used.is_some(), "**must answer: is this still in use**");

        // ---- 3. Forged ones, and ones with the wrong prefix, are all rejected
        assert!(
            tokens::authenticate(&pool, "utp_pat_deadbeef")
                .await
                .is_err(),
            "a made-up string must not be accepted"
        );
        assert!(
            tokens::authenticate(&pool, &plain.replace("utp_pat_", "utp_"))
                .await
                .is_err(),
            "an ingest token's prefix must not come down this path"
        );

        // ---- 4. kb_ids only narrows
        let (_, scoped) =
            tokens::issue(&pool, f.user, "KB A only", "write", Some(&[f.kb_a]), None).await?;
        let auth = tokens::authenticate(&pool, &scoped).await?;
        assert!(auth.covers(f.kb_a), "an authorized KB is within reach");
        assert!(
            !auth.covers(f.kb_b),
            "**a token pinned to one KB cannot reach another** -- even when the person \
             can get into both"
        );
        assert!(auth.can_write(), "a write token can write");
        // The unpinned one covers all of them as before
        assert!(tokens::authenticate(&pool, &plain).await?.covers(f.kb_b));

        // ---- 5. Revocation takes effect immediately, and the row is still there
        tokens::revoke(&pool, f.user, view.id).await?;
        assert!(
            tokens::authenticate(&pool, &plain).await.is_err(),
            "**once revoked it is refused immediately** -- decided inside SQL, not after \
             fetching the row back"
        );
        let still_there: i64 =
            sqlx::query_scalar("SELECT count(*) FROM personal_tokens WHERE id = $1")
                .bind(view.id)
                .fetch_one(&pool)
                .await?;
        assert_eq!(still_there, 1, "stamped not deleted: this key existed");
        assert!(
            tokens::revoke(&pool, f.user, view.id).await.is_err(),
            "the second of two revokes should say there is no such row left to revoke"
        );

        // ---- 6. Expiry
        let (expired_view, expired) = tokens::issue(
            &pool,
            f.user,
            "long since expired",
            "read",
            None,
            Some(Utc::now() - Duration::hours(1)),
        )
        .await?;
        assert!(
            tokens::authenticate(&pool, &expired).await.is_err(),
            "once it is past its expiry it is refused"
        );

        // ---- 7. The list shows the revoked ones and the expired ones too
        let all = tokens::list(&pool, f.user).await?;
        assert_eq!(all.len(), 3, "revoked ones are listed too, visibly");
        assert!(
            all.iter()
                .any(|t| t.id == view.id && t.revoked_at.is_some()),
            "the list has to mark which one was revoked"
        );
        assert!(
            all.iter().all(|t| t.token_prefix.starts_with("utp_pat_")),
            "the list gives the prefix only, never the plaintext"
        );
        assert!(all.iter().any(|t| t.id == expired_view.id));

        // Someone else's token cannot be revoked
        let other = Uuid::now_v7();
        assert!(
            tokens::revoke(&pool, other, expired_view.id).await.is_err(),
            "**only whoever owns a token can revoke it**"
        );
        Ok::<_, anyhow::Error>(())
    }
    .await;

    sqlx::query("DELETE FROM organizations WHERE id = $1")
        .bind(f.org)
        .execute(&pool)
        .await?;
    run
}
