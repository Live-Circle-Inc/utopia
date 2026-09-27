use sqlx::PgPool;
use utopia_core::models::{Role, User, Workspace};
use utopia_core::{AppError, AppResult};
use uuid::Uuid;

/// Registration (single-tenant model):
/// - no organization in the deployment yet → the first user: create the organization + a
///   default workspace, and become owner + system admin;
/// - an organization already exists → join it and enter the default (earliest) workspace
///   as viewer; refused when `open_registration = false` (bootstrapping the first user is
///   the only exception).
pub async fn register(
    pool: &PgPool,
    email: &str,
    password_hash: &str,
    display_name: &str,
    org_name: Option<&str>,
    open_registration: bool,
) -> AppResult<(User, Workspace)> {
    let mut tx = pool.begin().await?;

    let existing_org: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM organizations ORDER BY created_at LIMIT 1")
            .fetch_optional(&mut *tx)
            .await?;

    let result = match existing_org {
        None => {
            // The first user: bootstraps the whole deployment
            let org_id = Uuid::now_v7();
            sqlx::query("INSERT INTO organizations (id, name) VALUES ($1, $2)")
                .bind(org_id)
                .bind(org_name.unwrap_or("Default Organization"))
                .execute(&mut *tx)
                .await?;

            let user =
                insert_user(&mut tx, org_id, email, password_hash, display_name, true).await?;

            let workspace: Workspace = sqlx::query_as(
                "INSERT INTO workspaces (id, org_id, name) VALUES ($1, $2, $3) RETURNING *",
            )
            .bind(Uuid::now_v7())
            .bind(org_id)
            .bind("Default Workspace")
            .fetch_one(&mut *tx)
            .await?;

            insert_membership(&mut tx, user.id, workspace.id, Role::Owner).await?;
            insert_general_kb(&mut tx, workspace.id, user.id).await?;
            (user, workspace)
        }
        Some((org_id,)) => {
            if !open_registration {
                return Err(AppError::invalid(
                    "registration_closed",
                    "Registration is closed for this deployment. Contact your administrator.",
                ));
            }
            let user =
                insert_user(&mut tx, org_id, email, password_hash, display_name, false).await?;

            // Join the default (earliest) workspace; in the abnormal case where the
            // organization has no workspace, create one
            let default_ws: Option<Workspace> = sqlx::query_as(
                "SELECT * FROM workspaces WHERE org_id = $1 ORDER BY created_at LIMIT 1",
            )
            .bind(org_id)
            .fetch_optional(&mut *tx)
            .await?;

            match default_ws {
                Some(ws) => {
                    insert_membership(&mut tx, user.id, ws.id, Role::Viewer).await?;
                    (user, ws)
                }
                None => {
                    let ws: Workspace = sqlx::query_as(
                        "INSERT INTO workspaces (id, org_id, name) VALUES ($1, $2, $3) RETURNING *",
                    )
                    .bind(Uuid::now_v7())
                    .bind(org_id)
                    .bind("Default Workspace")
                    .fetch_one(&mut *tx)
                    .await?;
                    insert_membership(&mut tx, user.id, ws.id, Role::Owner).await?;
                    (user, ws)
                }
            }
        }
    };

    tx.commit().await?;
    Ok(result)
}

async fn insert_user(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    org_id: Uuid,
    email: &str,
    password_hash: &str,
    display_name: &str,
    is_admin: bool,
) -> AppResult<User> {
    sqlx::query_as(
        "INSERT INTO users (id, org_id, email, password_hash, display_name, is_admin)
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING *",
    )
    .bind(Uuid::now_v7())
    .bind(org_id)
    .bind(email)
    .bind(password_hash)
    .bind(display_name)
    .bind(is_admin)
    .fetch_one(&mut **tx)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(db) if db.is_unique_violation() => {
            AppError::Conflict("This email is already registered".into())
        }
        _ => AppError::Db(e),
    })
}

async fn insert_membership(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user_id: Uuid,
    workspace_id: Uuid,
    role: Role,
) -> AppResult<()> {
    sqlx::query("INSERT INTO memberships (user_id, workspace_id, role) VALUES ($1, $2, $3)")
        .bind(user_id)
        .bind(workspace_id)
        .bind(role.as_str())
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// The deployment's shared space. Created the moment the first user registers, in the
/// same transaction as the organization and the workspace -- otherwise the first screen
/// of a new deployment is an empty shell: Graph stuck on Loading, no KB to choose in the
/// switcher, and nobody ever told the user about "create your first KB".
///
/// The name is General and not Public: visibility is expressed by the visibility field,
/// so saying it again in the name is both redundant and liable to be misread by
/// self-hosting users as "public to the internet". is_default keeps it forever open and
/// undeletable (the CHECK in 0012 is the DB-level belt and braces).
async fn insert_general_kb(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    workspace_id: Uuid,
    owner_id: Uuid,
) -> AppResult<()> {
    let kb_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO knowledge_bases
            (id, workspace_id, name, kind, description, is_default, visibility)
         VALUES ($1, $2, 'General', 'knowledge', $3, TRUE, 'open')",
    )
    .bind(kb_id)
    .bind(workspace_id)
    .bind("Shared space for the whole deployment. Everyone can read it.")
    .execute(&mut **tx)
    .await?;
    // The creator is recorded as admin of this KB, same as the manual KB creation path.
    // The first user is a system admin anyway and would get in without this row, but the
    // system admin status can be revoked later on, and this KB's permission should not
    // evaporate along with it.
    sqlx::query(
        "INSERT INTO kb_members (kb_id, user_id, role, added_by)
         VALUES ($1, $2, 'admin', $2)",
    )
    .bind(kb_id)
    .bind(owner_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// An admin opening an account for someone: joins the existing organization + the default
/// workspace, with the deployment role specified by the admin.
pub async fn admin_create_user(
    pool: &PgPool,
    email: &str,
    password_hash: &str,
    display_name: &str,
    role: Role,
) -> AppResult<User> {
    let mut tx = pool.begin().await?;
    let (org_id,): (Uuid,) =
        sqlx::query_as("SELECT id FROM organizations ORDER BY created_at LIMIT 1")
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| AppError::Validation("Deployment not bootstrapped yet".into()))?;
    let user = insert_user(&mut tx, org_id, email, password_hash, display_name, false).await?;
    let ws: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM workspaces WHERE org_id = $1 ORDER BY created_at LIMIT 1")
            .bind(org_id)
            .fetch_optional(&mut *tx)
            .await?;
    if let Some((ws_id,)) = ws {
        insert_membership(&mut tx, user.id, ws_id, role).await?;
    }
    tx.commit().await?;
    Ok(user)
}

/// Find an account by email -- **only the active ones**.
///
/// That one `deactivated_at IS NULL` handles two things at once: deactivated people
/// cannot log in, and when "one email may repeat across deactivated accounts" it will not
/// return one of them at random (the unique index is partial now, constraining active
/// accounts only, see `users.deactivated_at`).
pub async fn find_user_by_email(pool: &PgPool, email: &str) -> AppResult<Option<User>> {
    let user = sqlx::query_as("SELECT * FROM users WHERE email = $1 AND deactivated_at IS NULL")
        .bind(email)
        .fetch_optional(pool)
        .await?;
    Ok(user)
}

/// Find an account by id -- **only the active ones**.
///
/// Session validation goes through here, so deactivation **takes effect immediately**
/// without waiting for a token to expire: an already-issued token finds no person on its
/// very next request. This is the one path soft deletion has to block; every other place
/// that reads users (audit, merge log, retype ledger) **must still find them as before**
/// -- they did the thing, and deactivating the person does not mean it never happened.
pub async fn find_user_by_id(pool: &PgPool, id: Uuid) -> AppResult<Option<User>> {
    let user = sqlx::query_as("SELECT * FROM users WHERE id = $1 AND deactivated_at IS NULL")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(user)
}

/// Change the display name (the profile page).
pub async fn update_display_name(pool: &PgPool, id: Uuid, display_name: &str) -> AppResult<User> {
    sqlx::query_as("UPDATE users SET display_name = $2 WHERE id = $1 RETURNING *")
        .bind(id)
        .bind(display_name)
        .fetch_optional(pool)
        .await?
        .ok_or(AppError::NotFound)
}

/// Change the password (the server layer has already verified the old one and done the
/// hashing).
pub async fn update_password(pool: &PgPool, id: Uuid, password_hash: &str) -> AppResult<()> {
    sqlx::query("UPDATE users SET password_hash = $2 WHERE id = $1")
        .bind(id)
        .bind(password_hash)
        .execute(pool)
        .await?;
    Ok(())
}

/// Deactivate an account (soft delete, see `users.deactivated_at`).
///
/// **The row is not deleted.** The `actor_id` of audit events, the merge log, the retype
/// ledger and mapping confirmations all point at this person, and those are audit
/// material -- once the person is gone it must still be possible to answer "who did it at
/// the time". Deactivation only cuts off access.
///
/// **You cannot deactivate yourself**: once an admin has switched themselves off, nobody
/// can put them back (this system has no "super admin" tier above it). Blocked here and
/// not only in the interface -- the interface can stop a misclick, it cannot stop someone
/// calling the endpoint directly.
///
/// **The last admin cannot be deactivated**: otherwise nobody in this organization can
/// manage members from then on. Same reasoning.
pub async fn deactivate_user(pool: &PgPool, target: Uuid, actor: Uuid) -> AppResult<()> {
    if target == actor {
        return Err(AppError::Validation("cannot deactivate yourself".into()));
    }
    let mut tx = pool.begin().await?;
    let victim: Option<(bool, Uuid, Option<chrono::DateTime<chrono::Utc>>)> =
        sqlx::query_as("SELECT is_admin, org_id, deactivated_at FROM users WHERE id = $1")
            .bind(target)
            .fetch_optional(&mut *tx)
            .await?;
    let Some((is_admin, org_id, already)) = victim else {
        tx.rollback().await?;
        return Err(AppError::NotFound);
    };
    if already.is_some() {
        // Idempotent: deactivating twice is not an error, and must not rewrite
        // deactivated_by to the second person
        tx.rollback().await?;
        return Ok(());
    }
    if is_admin {
        let (others,): (i64,) = sqlx::query_as(
            "SELECT count(*) FROM users
              WHERE org_id = $1 AND is_admin AND deactivated_at IS NULL AND id <> $2",
        )
        .bind(org_id)
        .bind(target)
        .fetch_one(&mut *tx)
        .await?;
        if others == 0 {
            tx.rollback().await?;
            return Err(AppError::Validation(
                "this is the last admin in the organization; deactivate them and nobody can manage members".into(),
            ));
        }
    }
    sqlx::query("UPDATE users SET deactivated_at = now(), deactivated_by = $2 WHERE id = $1")
        .bind(target)
        .bind(actor)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Put a deactivated account back.
///
/// **It can fail, and failing is right**: if someone created a new account with the same
/// email while this one was deactivated, that partial unique index blocks the restore.
/// What to do then is to let the admin see the conflict, not to quietly let two active
/// accounts share one email.
pub async fn reactivate_user(pool: &PgPool, target: Uuid) -> AppResult<()> {
    let res = sqlx::query(
        "UPDATE users SET deactivated_at = NULL, deactivated_by = NULL
          WHERE id = $1 AND deactivated_at IS NOT NULL",
    )
    .bind(target)
    .execute(pool)
    .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    Ok(())
}
