//! Personal access tokens (see `docs/decisions/0014`).
//!
//! **A token acts with the identity of the person who issued it, but need not be all of that
//! person**:
//!
//! ```text
//! effective permissions = this person's role ∩ this token's scope
//! ```
//!
//! Intersection, not union -- ticking write on a viewer's token still leaves it read-only. So
//! this module only answers "which person does this string correspond to, and how far does this
//! token let them go"; **whether they may touch a given knowledge base is still decided by
//! `access::require_kb`**, and not one line of that changes.

use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use utopia_core::models::TokenView;
use utopia_core::{AppError, AppResult};
use uuid::Uuid;

/// Prefix of the plaintext token. Kept distinct from the `utp_` of `sources.ingest_token` --
/// the two can do very different things, and in a log or a config file you have to be able to
/// tell at a glance which kind it is
const PREFIX: &str = "utp_pat_";
/// The short slice shown in listings for humans to recognise (prefix included). Enough to match
/// the string in a config file, not enough to reconstruct it
const SHOWN: usize = 16;

/// What you get once validation passes: who, and how far this token lets them go.
pub struct Authenticated {
    pub user_id: Uuid,
    pub token_id: Uuid,
    /// read | write
    pub scope: String,
    /// None = every knowledge base this person can get into
    pub kb_ids: Option<Vec<Uuid>>,
}

impl Authenticated {
    /// Whether this token may touch this knowledge base.
    ///
    /// **This is not a permission decision, it is a scope decision.** Returning true only says
    /// "the token did not exclude it"; what role that person has in this knowledge base still
    /// has to be asked of `access::require_kb` as usual.
    pub fn covers(&self, kb_id: Uuid) -> bool {
        match &self.kb_ids {
            None => true,
            Some(ids) => ids.contains(&kb_id),
        }
    }

    pub fn can_write(&self) -> bool {
        self.scope == "write"
    }
}

/// **SHA-256, not argon2.** This one place stores things differently from passwords, for two
/// reasons:
///
/// 1. **A token is a high-entropy random string, not a human-chosen password.** argon2 is slow
///    in order to make brute-forcing a "password123" not worth it; against a 244-bit random
///    number, being a million times slower still gets you nowhere, so it buys nothing.
/// 2. **argon2 salts every row differently, so you cannot look it up.** Validation is a hot path
///    (once per tool call), and `WHERE token_hash = $1` on the unique index is a single hit;
///    switch to argon2 and you have to fetch every token and verify one by one -- the more you
///    have issued, the slower it gets.
fn hash(token: &str) -> String {
    Sha256::digest(token.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Issue one. **The returned plaintext is its only appearance** -- the database stores only the
/// hash, so if it is lost the only option is to issue a new one.
pub async fn issue(
    pool: &PgPool,
    user_id: Uuid,
    name: &str,
    scope: &str,
    kb_ids: Option<&[Uuid]>,
    expires_at: Option<DateTime<Utc>>,
) -> AppResult<(TokenView, String)> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 64 {
        return AppResult::Err(AppError::invalid(
            "bad_token_name",
            "Token name must be 1-64 characters",
        ));
    }
    if !matches!(scope, "read" | "write") {
        return Err(AppError::invalid(
            "bad_token_scope",
            "Scope must be read or write",
        ));
    }
    // Two v4s concatenated ≈ 244 bits of entropy. Same approach as `new_ingest_token`;
    // the different prefix is so they can be told apart in logs
    let plain = format!(
        "{PREFIX}{}{}",
        Uuid::new_v4().simple(),
        Uuid::new_v4().simple()
    );
    let id = Uuid::now_v7();
    let view: TokenView = sqlx::query_as(
        "INSERT INTO personal_tokens
             (id, user_id, name, token_hash, token_prefix, scope, kb_ids, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
         RETURNING id, name, token_prefix, scope, kb_ids, expires_at,
                   last_used_at, revoked_at, created_at",
    )
    .bind(id)
    .bind(user_id)
    .bind(name)
    .bind(hash(&plain))
    .bind(&plain[..SHOWN])
    .bind(scope)
    .bind(kb_ids)
    .bind(expires_at)
    .fetch_one(pool)
    .await?;
    Ok((view, plain))
}

/// Which ones I have issued. Revoked ones are listed too -- **the fact that something was
/// revoked has to be visible in itself**.
pub async fn list(pool: &PgPool, user_id: Uuid) -> AppResult<Vec<TokenView>> {
    Ok(sqlx::query_as(
        "SELECT id, name, token_prefix, scope, kb_ids, expires_at,
                last_used_at, revoked_at, created_at
           FROM personal_tokens WHERE user_id = $1 ORDER BY created_at DESC",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?)
}

/// Revoke one. **Stamp the row, do not delete it**: delete the row and "this key once existed"
/// becomes unanswerable, and that is exactly the first question an after-the-fact investigation
/// asks.
pub async fn revoke(pool: &PgPool, user_id: Uuid, token_id: Uuid) -> AppResult<()> {
    let res = sqlx::query(
        "UPDATE personal_tokens SET revoked_at = now()
          WHERE id = $2 AND user_id = $1 AND revoked_at IS NULL",
    )
    .bind(user_id)
    .bind(token_id)
    .execute(pool)
    .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    Ok(())
}

/// Plaintext → who this is, and how far they are allowed to go.
///
/// **Expiry and revocation are judged in SQL, not in Rust.** If you fetched the row and then
/// compared, a token revoked during the stretch of time between "fetch" and "compare" would get
/// through anyway -- and MCP connections are long-lived, so that stretch can grow long enough to
/// matter.
///
/// Writing `last_used_at` while we are here: before revoking, someone has to be able to answer
/// "is this one still in use", and without that number nobody dares revoke.
pub async fn authenticate(pool: &PgPool, plain: &str) -> AppResult<Authenticated> {
    if !plain.starts_with(PREFIX) {
        return Err(AppError::Unauthorized);
    }
    let row: Option<(Uuid, Uuid, String, Option<Vec<Uuid>>)> = sqlx::query_as(
        "UPDATE personal_tokens SET last_used_at = now()
          WHERE token_hash = $1
            AND revoked_at IS NULL
            AND (expires_at IS NULL OR expires_at > now())
        RETURNING id, user_id, scope, kb_ids",
    )
    .bind(hash(plain))
    .fetch_optional(pool)
    .await?;
    let Some((token_id, user_id, scope, kb_ids)) = row else {
        return Err(AppError::Unauthorized);
    };
    Ok(Authenticated {
        user_id,
        token_id,
        scope,
        kb_ids,
    })
}
