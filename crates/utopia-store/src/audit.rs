//! Audit log: who did what to which thing and when. Pure audit -- it only records and
//! displays, and carries no derived features such as rollback.
//! A failed record must never affect the business operation (callers always use `let _ =`).

use sqlx::PgPool;
use utopia_core::models::AuditEventView;
use utopia_core::AppResult;
use uuid::Uuid;

pub async fn record(
    pool: &PgPool,
    kb_id: Option<Uuid>,
    actor_id: Uuid,
    action: &str,
    target_kind: &str,
    target_id: Option<Uuid>,
    detail: serde_json::Value,
) -> AppResult<()> {
    record_opt(
        pool,
        kb_id,
        Some(actor_id),
        action,
        target_kind,
        target_id,
        detail,
    )
    .await
}

/// Where the request came from. The HTTP layer scopes it into [`CLIENT`] around every request
/// and `record_opt` reads it itself -- otherwise all 25 call sites would have to carry two more
/// parameters that have nothing to do with their business.
///
/// Background jobs (batched adjudication, scheduled sync) are inside no request at all, so they
/// read None, exactly as they should: those actions really do have no client.
#[derive(Debug, Clone, Default)]
pub struct ClientContext {
    pub ip: Option<String>,
    pub user_agent: Option<String>,
}

tokio::task_local! {
    pub static CLIENT: ClientContext;
}

/// The current request's origin; all empty when outside a request context (background jobs).
fn client_context() -> ClientContext {
    CLIENT.try_with(|c| c.clone()).unwrap_or_default()
}

/// System events with no human actor (an AI adjudication auto-merge, say) go through here:
/// actor is NULL.
pub async fn record_opt(
    pool: &PgPool,
    kb_id: Option<Uuid>,
    actor_id: Option<Uuid>,
    action: &str,
    target_kind: &str,
    target_id: Option<Uuid>,
    detail: serde_json::Value,
) -> AppResult<()> {
    let ctx = client_context();
    // Identity snapshot: when the user is deleted later (after 0025 the row survives), the
    // ledger can still tell who it was instead of being left with a bare UUID. Looking it up
    // right now is reliable -- the action is happening, the person is still there.
    let actor_label: Option<String> = match actor_id {
        Some(id) => sqlx::query_scalar("SELECT email FROM users WHERE id = $1")
            .bind(id)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten(),
        None => None,
    };
    sqlx::query(
        "INSERT INTO audit_events
            (id, kb_id, actor_id, action, target_kind, target_id, detail,
             client_ip, user_agent, actor_label)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(Uuid::now_v7())
    .bind(kb_id)
    .bind(actor_id)
    .bind(action)
    .bind(target_kind)
    .bind(target_id)
    .bind(detail)
    .bind(ctx.ip)
    .bind(ctx.user_agent)
    .bind(actor_label)
    .execute(pool)
    .await?;
    Ok(())
}

/// Ledger of review decisions: only actions in the review domain
/// (review./fact./conflict./merge.), paginated on the server.
pub async fn review_history(
    pool: &PgPool,
    kb_id: Uuid,
    limit: i64,
    offset: i64,
) -> AppResult<(Vec<AuditEventView>, i64)> {
    const COND: &str = "e.kb_id = $1 AND (e.action LIKE 'review.%' OR e.action LIKE 'fact.%'
                        OR e.action LIKE 'conflict.%' OR e.action LIKE 'merge.%')";
    let rows: Vec<AuditEventView> = sqlx::query_as(&format!(
        "SELECT e.id, e.action, e.target_kind, e.target_id, e.detail,
                e.actor_id, u.display_name AS actor_name, e.created_at
         FROM audit_events e LEFT JOIN users u ON u.id = e.actor_id
         WHERE {COND} ORDER BY e.created_at DESC LIMIT $2 OFFSET $3"
    ))
    .bind(kb_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await?;
    let (total,): (i64,) =
        sqlx::query_as(&format!("SELECT count(*) FROM audit_events e WHERE {COND}"))
            .bind(kb_id)
            .fetch_one(pool)
            .await?;
    Ok((rows, total))
}

/// A knowledge base's audit ledger, **with pagination and filters**.
///
/// It used to be a fixed most-recent 100 rows, no pagination and no filters -- and a ledger is
/// compliance material, where "you can only see the last hundred" amounts to not being able to
/// query history at all. The three filters were picked from how it actually gets queried:
///
/// - `action`: look up one class of action ("who has retyped things", "which rejections were
///   there"). Prefix match rather than equality, because action names are themselves layered
///   (`entity.retyped` / `entity.renamed`), so passing `entity.` scoops up the whole family
/// - `actor`: look up what one person did. The most common question in a compliance audit
/// - `since` / `until`: look up a stretch of time. Exactly what an incident review needs
///
/// The total count comes back with it, otherwise the pager does not know how many pages there
/// are -- and "not knowing how many pages there are" is just the old 100 in another guise.
#[allow(clippy::too_many_arguments)]
pub async fn list_for_kb(
    pool: &PgPool,
    kb_id: Uuid,
    action: Option<&str>,
    actor: Option<Uuid>,
    since: Option<chrono::DateTime<chrono::Utc>>,
    until: Option<chrono::DateTime<chrono::Utc>>,
    limit: i64,
    offset: i64,
) -> AppResult<(Vec<AuditEventView>, i64)> {
    // All four filters are written as "an empty parameter means no effect", so one SQL
    // statement covers every combination -- string concatenation would grow sixteen branches
    // here, and every branch is one more injection surface
    const WHERE: &str = "WHERE e.kb_id = $1
           AND ($2::text IS NULL OR e.action LIKE $2 || '%')
           AND ($3::uuid IS NULL OR e.actor_id = $3)
           AND ($4::timestamptz IS NULL OR e.created_at >= $4)
           AND ($5::timestamptz IS NULL OR e.created_at < $5)";

    let rows: Vec<AuditEventView> = sqlx::query_as(&format!(
        "SELECT e.id, e.action, e.target_kind, e.target_id, e.detail,
                e.actor_id, u.display_name AS actor_name, e.created_at
         FROM audit_events e LEFT JOIN users u ON u.id = e.actor_id
         {WHERE}
         ORDER BY e.created_at DESC LIMIT $6 OFFSET $7"
    ))
    .bind(kb_id)
    .bind(action)
    .bind(actor)
    .bind(since)
    .bind(until)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await?;

    let (total,): (i64,) = sqlx::query_as(&format!("SELECT count(*) FROM audit_events e {WHERE}"))
        .bind(kb_id)
        .bind(action)
        .bind(actor)
        .bind(since)
        .bind(until)
        .fetch_one(pool)
        .await?;
    Ok((rows, total))
}

/// Which actions have actually appeared in this KB's ledger. **The filter dropdown has to be
/// populated from what is really there** -- a hard-coded list of every action would show the
/// user a pile of options that never happened in this KB.
pub async fn actions_for_kb(pool: &PgPool, kb_id: Uuid) -> AppResult<Vec<String>> {
    Ok(
        sqlx::query_scalar("SELECT DISTINCT action FROM audit_events WHERE kb_id = $1 ORDER BY 1")
            .bind(kb_id)
            .fetch_all(pool)
            .await?,
    )
}
