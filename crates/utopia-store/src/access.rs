//! KB-level access decisions -- the single authorization entry point for every KB-scoped
//! route.
//! The decision chain: system admin → everything allowed; a row in the kb_members matrix
//! → the matrix role; an open KB → the deployment role (membership of the invisible
//! workspace); a restricted KB with no row → NotFound (does not leak that the KB exists).

use sqlx::PgPool;
use utopia_core::models::{KbMemberView, KnowledgeBase, MyKbInfo, Role, User};
use utopia_core::{AppError, AppResult};
use uuid::Uuid;

/// A user's effective role in a given KB (None = not visible).
pub async fn kb_role(pool: &PgPool, user: &User, kb: &KnowledgeBase) -> AppResult<Option<Role>> {
    if user.is_admin {
        return Ok(Some(Role::Owner));
    }
    let matrix: Option<(String,)> =
        sqlx::query_as("SELECT role FROM kb_members WHERE kb_id = $1 AND user_id = $2")
            .bind(kb.id)
            .bind(user.id)
            .fetch_optional(pool)
            .await?;
    if let Some((r,)) = matrix {
        return Ok(Role::parse(&r));
    }
    if kb.visibility == "open" {
        // An open KB = everyone in the deployment can read; write access always comes
        // from this KB's matrix (system admins are already Owner at the head of the
        // chain; deployment roles no longer map write access into a KB)
        let ws: Option<(String,)> =
            sqlx::query_as("SELECT role FROM memberships WHERE workspace_id = $1 AND user_id = $2")
                .bind(kb.workspace_id)
                .bind(user.id)
                .fetch_optional(pool)
                .await?;
        return Ok(ws.map(|_| Role::Viewer));
    }
    Ok(None)
}

/// The set version of `kb_role`: every KB this person can see **across all workspaces**
/// → effective role.
///
/// **Change the function above and you have to change this one**. Why it is not assembled
/// by calling `kb_role` in a loop: the alert list is cross-KB, one query per row is an
/// N+1, and the number of alerts grows with the size of the deployment.
/// The three branches match `kb_role` one for one, in the same order:
///   1. is_admin → every KB, Owner
///   2. a row in kb_members → the matrix role
///   3. an open KB + a membership in that workspace → Viewer
///
/// Branch 3 queries `memberships` explicitly here, while `kbs::list_visible` does not --
/// that function has already been scoped by the workspace route, and this one has not.
pub async fn visible_kb_roles(pool: &PgPool, user: &User) -> AppResult<Vec<(Uuid, Role)>> {
    if user.is_admin {
        let ids: Vec<(Uuid,)> = sqlx::query_as("SELECT id FROM knowledge_bases")
            .fetch_all(pool)
            .await?;
        return Ok(ids.into_iter().map(|(id,)| (id, Role::Owner)).collect());
    }
    let rows: Vec<(Uuid, Option<String>, bool)> = sqlx::query_as(
        "SELECT k.id,
                m.role,
                (k.visibility = 'open'
                 AND EXISTS (SELECT 1 FROM memberships ws
                             WHERE ws.workspace_id = k.workspace_id AND ws.user_id = $1))
         FROM knowledge_bases k
         LEFT JOIN kb_members m ON m.kb_id = k.id AND m.user_id = $1",
    )
    .bind(user.id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|(id, matrix, open_member)| {
            // The matrix beats open -- as in kb_role, once there is a matrix row
            // visibility is no longer consulted
            match matrix {
                Some(r) => Role::parse(&r).map(|r| (id, r)),
                None if open_member => Some((id, Role::Viewer)),
                None => None,
            }
        })
        .collect())
}

/// Require at least the `min` role on the KB, and return the KB.
pub async fn require_kb(
    pool: &PgPool,
    user: &User,
    kb_id: Uuid,
    min: Role,
) -> AppResult<KnowledgeBase> {
    let kb = crate::kbs::get(pool, kb_id).await?;
    match kb_role(pool, user, &kb).await? {
        Some(r) if r >= min => Ok(kb),
        Some(_) => Err(AppError::Forbidden),
        None => Err(AppError::NotFound),
    }
}

// ---------------------------------------------------------------------------
// The KB member matrix
// ---------------------------------------------------------------------------

pub async fn kb_members(pool: &PgPool, kb_id: Uuid) -> AppResult<Vec<KbMemberView>> {
    let rows: Vec<KbMemberView> = sqlx::query_as(
        "SELECT m.user_id, u.email, u.display_name, m.role
         FROM kb_members m JOIN users u ON u.id = m.user_id
         -- Deactivated people do not appear in the member list (see `users.deactivated_at`); the membership row itself stays
         WHERE m.kb_id = $1 AND u.deactivated_at IS NULL ORDER BY u.display_name",
    )
    .bind(kb_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn set_kb_member(
    pool: &PgPool,
    kb_id: Uuid,
    user_id: Uuid,
    role: &str,
    added_by: Option<Uuid>,
) -> AppResult<()> {
    if !matches!(role, "viewer" | "editor" | "admin") {
        return Err(AppError::Validation(
            "role must be viewer, editor or admin".into(),
        ));
    }
    // Changing the role does not rewrite the original inviter (the join info records
    // "who brought them in")
    sqlx::query(
        "INSERT INTO kb_members (kb_id, user_id, role, added_by) VALUES ($1, $2, $3, $4)
         ON CONFLICT (kb_id, user_id)
         DO UPDATE SET role = $3, added_by = COALESCE(kb_members.added_by, $4)",
    )
    .bind(kb_id)
    .bind(user_id)
    .bind(role)
    .bind(added_by)
    .execute(pool)
    .await?;
    Ok(())
}

/// My membership info for each visible KB + summary stats (the account-level Knowledge
/// bases page).
pub async fn my_kb_infos(
    pool: &PgPool,
    kb_ids: &[Uuid],
    user_id: Uuid,
) -> AppResult<Vec<MyKbInfo>> {
    let rows: Vec<MyKbInfo> = sqlx::query_as(
        "SELECT k.id AS kb_id,
                m.role AS member_role,
                m.created_at AS joined_at,
                inviter.display_name AS added_by_name,
                (SELECT count(*) FROM documents d WHERE d.kb_id = k.id) AS doc_count,
                (SELECT count(*) FROM kb_members mm WHERE mm.kb_id = k.id) AS member_count
         FROM knowledge_bases k
         LEFT JOIN kb_members m ON m.kb_id = k.id AND m.user_id = $2
         LEFT JOIN users inviter ON inviter.id = m.added_by
         WHERE k.id = ANY($1)",
    )
    .bind(kb_ids)
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn remove_kb_member(pool: &PgPool, kb_id: Uuid, user_id: Uuid) -> AppResult<()> {
    let res = sqlx::query("DELETE FROM kb_members WHERE kb_id = $1 AND user_id = $2")
        .bind(kb_id)
        .bind(user_id)
        .execute(pool)
        .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Deployment configuration
// ---------------------------------------------------------------------------

pub async fn open_registration(pool: &PgPool) -> AppResult<bool> {
    let row: Option<(bool,)> =
        sqlx::query_as("SELECT open_registration FROM deployment_settings LIMIT 1")
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|(v,)| v).unwrap_or(true))
}

pub async fn set_open_registration(pool: &PgPool, value: bool) -> AppResult<()> {
    sqlx::query("UPDATE deployment_settings SET open_registration = $1")
        .bind(value)
        .execute(pool)
        .await?;
    Ok(())
}

/// The default value of `ontology_lang` when a knowledge base is created.
///
/// **Deliberately not called "system language"**: under that name, sooner or later
/// someone tries to hang the interface language off it and then finds out they cannot
/// (the interface language lives in the client). The name itself ought to block the
/// misuse. See docs/decisions/0004.
pub async fn default_ontology_lang(pool: &PgPool) -> AppResult<String> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT default_ontology_lang FROM deployment_settings LIMIT 1")
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|(v,)| v).unwrap_or_else(|| "en".into()))
}

pub async fn set_default_ontology_lang(pool: &PgPool, value: &str) -> AppResult<()> {
    if !matches!(value, "en" | "zh") {
        return Err(AppError::invalid("bad_lang", "language must be en or zh"));
    }
    sqlx::query("UPDATE deployment_settings SET default_ontology_lang = $1")
        .bind(value)
        .execute(pool)
        .await?;
    Ok(())
}

/// The job worker concurrency (changeable in the system settings, and a change takes
/// effect immediately -- see jobs::run_worker).
pub async fn worker_concurrency(pool: &PgPool) -> AppResult<i32> {
    let row: Option<(i32,)> =
        sqlx::query_as("SELECT worker_concurrency FROM deployment_settings LIMIT 1")
            .fetch_optional(pool)
            .await?;
    // Kept in step with the column default of `deployment_settings.worker_concurrency`
    // (migration 0011). It is written in two places because one is in SQL and the other
    // in Rust, and changing one does not carry the other along -- the
    // `the_backstop_can_be_raised` test keeps an eye on exactly this
    Ok(row.map(|(v,)| v).unwrap_or(64))
}

pub async fn set_worker_concurrency(pool: &PgPool, value: i32) -> AppResult<()> {
    if !(1..=256).contains(&value) {
        return Err(AppError::invalid(
            "concurrency_range",
            "worker_concurrency must be between 1 and 256",
        ));
    }
    sqlx::query("UPDATE deployment_settings SET worker_concurrency = $1")
        .bind(value)
        .execute(pool)
        .await?;
    Ok(())
}

/// Persist the auto-generated JWT secret, and return the one that ends up in effect.
///
/// `COALESCE` makes several instances starting concurrently converge on the same value:
/// whoever writes first wins, and the latecomers get back the row already in the database
/// rather than the one they just generated. Splitting it into the two steps "read first,
/// write if absent" cannot achieve that -- two instances would both read NULL, each write
/// their own, and the one that wrote first would from then on be using a secret that is
/// no longer in the database, so every token it signs is invalid on the other instance.
pub async fn ensure_jwt_secret(pool: &PgPool, generated: &str) -> AppResult<String> {
    let (secret,): (Option<String>,) = sqlx::query_as(
        "UPDATE deployment_settings SET jwt_secret = COALESCE(jwt_secret, $1)
         RETURNING jwt_secret",
    )
    .bind(generated)
    .fetch_one(pool)
    .await?;
    secret.ok_or_else(|| {
        AppError::Other(anyhow::anyhow!(
            "deployment_settings has no singleton row, so the JWT secret has nowhere to go"
        ))
    })
}

/// The character budget for laying the ontology into the extraction prompt. Over budget,
/// it switches to retrieving candidates per chunk.
///
/// **A setting rather than a constant**, because pinning it down takes a curve: at every
/// ontology size, measure full inlining once and per-chunk retrieval once, and see where
/// the two cross. If measuring a single point takes a service restart, nobody will ever
/// run that curve a second time.
pub async fn ontology_prompt_budget(pool: &PgPool) -> AppResult<usize> {
    let row: Option<(i32,)> =
        sqlx::query_as("SELECT ontology_prompt_budget FROM deployment_settings LIMIT 1")
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|(v,)| v.max(0) as usize).unwrap_or(24_000))
}
