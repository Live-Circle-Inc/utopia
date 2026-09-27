//! The ingestion source repository: "a source is a folder" -- a source is a container holding
//! the documents it ingested, and it can sync on a schedule.

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use utopia_core::models::{Role, Source, SourceKind, SourceView, SyncRun, SOURCE_SECRET_KEYS};
use utopia_core::{AppError, AppResult};
use uuid::Uuid;

/// folder = a pure container (upload/drag things in, no sync semantics); url/rss = pull-style;
/// api = push-style.
/// Watching a local directory (watch_folder) was rejected -- self-hosting users cannot see the
/// server's disk; object storage / WebDAV / Notion are its replacement shapes (0013).
/// custom = a custom puller: any URL implementing the Utopia ingest interface (returning items
/// JSON) can be ingested on a schedule.
/// github_issues / jira_issues = tickets: one ticket, together with its status change history,
/// becomes one document.
///
/// The list of kinds is **not written here**: `SourceKind` (utopia-core) enumerates the whole lot
/// in one place -- the creation allowlist, the sync dispatch, the frontend dropdown (with a test
/// checking the tables against each other). There used to be a hand-written `KINDS` here, and
/// five connectors gained sync support without making it into that table: selectable in the UI,
/// impossible to create (#247)
pub fn creatable_kinds() -> Vec<&'static str> {
    SourceKind::creatable().map(|k| k.as_str()).collect()
}

/// Validates and normalizes a standard 5-field cron expression (internally the cron crate's
/// 6-field form: a seconds slot gets prepended).
pub fn validate_cron(expr: &str) -> AppResult<String> {
    let normalized = expr.split_whitespace().collect::<Vec<_>>().join(" ");
    let fields = normalized.split(' ').count();
    if fields != 5 {
        return Err(AppError::invalid_detail(
            "bad_cron_fields",
            "Cron expression must have 5 fields (minute hour day month weekday)",
            format!("got {fields}"),
        ));
    }
    use std::str::FromStr;
    cron::Schedule::from_str(&format!("0 {normalized}")).map_err(|e| {
        AppError::invalid_detail("bad_cron", "Invalid cron expression", e.to_string())
    })?;
    Ok(normalized)
}

/// The next firing time of a cron (in the server's local timezone).
fn cron_next_after(expr: &str, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
    use std::str::FromStr;
    let schedule = cron::Schedule::from_str(&format!("0 {expr}")).ok()?;
    let local_after = after.with_timezone(&chrono::Local);
    schedule
        .after(&local_after)
        .next()
        .map(|t| t.with_timezone(&Utc))
}

pub async fn list(pool: &PgPool, kb_id: Uuid) -> AppResult<Vec<SourceView>> {
    // Credentials stripped out of config: the list is for Viewers to see, and no connector's
    // secret of any kind is handed out. The keys live in one table, `SOURCE_SECRET_KEYS` -- this
    // used to subtract `auth_header` alone, which is exactly how five connectors' secrets leaked
    // (#246)
    let rows: Vec<SourceView> = sqlx::query_as(
        "SELECT s.id, s.kind, s.name, s.config - $2::text[] AS config, s.icon,
                s.sync_interval_minutes, s.sync_cron,
                s.last_sync_at, s.last_sync_status, s.last_sync_error, s.last_sync_added,
                (SELECT count(*) FROM documents d WHERE d.source_id = s.id) AS doc_count,
                (SELECT count(*) FROM documents d
                 WHERE d.source_id = s.id AND d.missing_since IS NOT NULL) AS missing_count
         FROM sources s WHERE s.kb_id = $1 ORDER BY s.created_at",
    )
    .bind(kb_id)
    .bind(SOURCE_SECRET_KEYS)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn get(pool: &PgPool, id: Uuid) -> AppResult<Source> {
    sqlx::query_as("SELECT * FROM sources WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await?
        .ok_or(AppError::NotFound)
}

#[allow(clippy::too_many_arguments)]
pub async fn create(
    pool: &PgPool,
    kb_id: Uuid,
    kind: &str,
    name: &str,
    config: &serde_json::Value,
    icon: Option<&str>,
    sync_interval_minutes: Option<i32>,
    sync_cron: Option<&str>,
) -> AppResult<Source> {
    if !SourceKind::parse(kind).is_some_and(|k| k.creatable_by_hand()) {
        return Err(AppError::Validation(format!(
            "kind must be one of: {}",
            creatable_kinds().join(", ")
        )));
    }
    if name.trim().is_empty() {
        return Err(AppError::invalid(
            "source_name_required",
            "Source name is required",
        ));
    }
    // Mutually exclusive: cron wins (the UI only ever sends one of them)
    let cron_norm = sync_cron.map(validate_cron).transpose()?;
    let interval = if cron_norm.is_some() {
        None
    } else {
        sync_interval_minutes
    };
    // serde's default Value::Null lands in the database as a jsonb null, and the frontend
    // reading config.x blows up on the spot -- so normalize it to an empty object
    let config = if config.is_null() {
        serde_json::json!({})
    } else {
        config.clone()
    };
    let source = sqlx::query_as(
        "INSERT INTO sources (id, kb_id, kind, name, config, icon, sync_interval_minutes, sync_cron)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING *",
    )
    .bind(Uuid::now_v7())
    .bind(kb_id)
    .bind(kind)
    .bind(name.trim())
    .bind(config)
    .bind(icon)
    .bind(interval)
    .bind(cron_norm)
    .fetch_one(pool)
    .await?;
    Ok(source)
}

/// Sets the push token of an api source (on creation / rotation).
pub async fn set_ingest_token(pool: &PgPool, source_id: Uuid, token: &str) -> AppResult<()> {
    let res = sqlx::query("UPDATE sources SET ingest_token = $2 WHERE id = $1")
        .bind(source_id)
        .bind(token)
        .execute(pool)
        .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    Ok(())
}

/// Updates the schedule: interval and cron are mutually exclusive, and explicitly setting
/// either one overwrites both.
#[allow(clippy::too_many_arguments)]
pub async fn update(
    pool: &PgPool,
    id: Uuid,
    name: Option<&str>,
    config: Option<&serde_json::Value>,
    icon: Option<&str>,
    schedule: Option<(Option<i32>, Option<String>)>,
) -> AppResult<Source> {
    let schedule = match schedule {
        Some((interval, cron)) => {
            let cron_norm = cron.as_deref().map(validate_cron).transpose()?;
            let interval = if cron_norm.is_some() { None } else { interval };
            Some((interval, cron_norm))
        }
        None => None,
    };
    let source = sqlx::query_as(
        "UPDATE sources SET
            name = COALESCE($2, name),
            config = COALESCE($3, config),
            icon = COALESCE($4, icon),
            sync_interval_minutes = CASE WHEN $5 THEN $6 ELSE sync_interval_minutes END,
            sync_cron = CASE WHEN $5 THEN $7 ELSE sync_cron END
         WHERE id = $1 RETURNING *",
    )
    .bind(id)
    .bind(name)
    .bind(config)
    .bind(icon)
    .bind(schedule.is_some())
    .bind(schedule.as_ref().and_then(|(i, _)| *i))
    .bind(schedule.as_ref().and_then(|(_, c)| c.clone()))
    .fetch_optional(pool)
    .await?
    .ok_or(AppError::NotFound)?;
    Ok(source)
}

/// Deletes a source; its documents stay (source_id set to NULL, falling back into the Uploads
/// group).
pub async fn delete(pool: &PgPool, id: Uuid) -> AppResult<()> {
    let res = sqlx::query("DELETE FROM sources WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    Ok(())
}

/// Sources due for a sync (the scheduler scans every minute).
/// The interval kind is decided in SQL; the cron kind is fetched back and filtered on the Rust
/// side by computing the next firing time.
pub async fn due_sources(pool: &PgPool) -> AppResult<Vec<Source>> {
    let rows: Vec<Source> = sqlx::query_as(
        "SELECT * FROM sources
         WHERE last_sync_status NOT IN ('queued', 'running')
           AND ((sync_interval_minutes IS NOT NULL
                 AND (last_sync_at IS NULL
                      OR last_sync_at + make_interval(mins => sync_interval_minutes) <= now()))
                OR sync_cron IS NOT NULL)",
    )
    .fetch_all(pool)
    .await?;

    let now = chrono::Utc::now();
    Ok(rows
        .into_iter()
        .filter(|s| match &s.sync_cron {
            None => true, // the interval kind was already decided in SQL
            Some(expr) => {
                // The baseline is the last sync time (or the creation time if it never synced):
                // a missed firing point gets caught up on the next scan
                let anchor = s.last_sync_at.unwrap_or(s.created_at);
                cron_next_after(expr, anchor).is_some_and(|next| next <= now)
            }
        })
        .collect())
}

/// Marks it queued (idempotent: returns false if it is already queued/running, which avoids
/// double-queueing).
pub async fn mark_queued(pool: &PgPool, id: Uuid) -> AppResult<bool> {
    let res = sqlx::query(
        "UPDATE sources SET last_sync_status = 'queued'
         WHERE id = $1 AND last_sync_status NOT IN ('queued', 'running')",
    )
    .bind(id)
    .execute(pool)
    .await?;
    Ok(res.rows_affected() > 0)
}

pub async fn mark_running(pool: &PgPool, id: Uuid) -> AppResult<()> {
    sqlx::query("UPDATE sources SET last_sync_status = 'running' WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Wraps up one sync. On failure it records an alert; on success it does nothing at all --
/// **"is it fine now?" is not a question the alert centre should answer**, and the source page
/// says so right there.
///
/// The return value is "did it record one", which is how the caller decides whether to emit an
/// event.
pub async fn finish_sync(
    pool: &PgPool,
    id: Uuid,
    error: Option<&str>,
    added: i32,
) -> AppResult<bool> {
    let row: Option<(Uuid, String)> = sqlx::query_as(
        "UPDATE sources SET last_sync_status = $2, last_sync_error = $3,
                last_sync_added = $4, last_sync_at = now()
         WHERE id = $1
         RETURNING kb_id, name",
    )
    .bind(id)
    .bind(if error.is_some() { "failed" } else { "ok" })
    .bind(error)
    .bind(added)
    .fetch_optional(pool)
    .await?;
    let Some((kb_id, name)) = row else {
        return Ok(false);
    };
    let Some(msg) = error else {
        return Ok(false);
    };
    crate::alerts::raise(
        pool,
        crate::alerts::NewAlert {
            kb_id: Some(kb_id),
            severity: "error",
            kind: crate::alerts::kind::SOURCE_SYNC_FAILED,
            // Content-level alerts go to editors, not only admins: an admin needs to know the
            // connection wants fixing, but **whoever configured this source** needs even more to
            // know that your stuff did not come in
            min_role: Role::Editor,
            subject_type: Some("source"),
            subject_id: Some(id),
            // Keep a copy of the name: once the source is deleted, subject_id resolves to no
            // name, and the alert ought to outlive it
            detail: serde_json::json!({ "name": name, "error": msg }),
        },
    )
    .await?;
    Ok(true)
}

/// Tags a document (the whole set is replaced).
///
/// **Zero callers, deliberately kept**: there is no route, and no entry point in the UI either.
/// Tags would be the one dimension on a document that "a person stuck on it themselves" -- the
/// source is where it came from, the name and the status are given by the system, and none of the
/// three can express a grouping like "this batch needs redacting" that cuts across sources and is
/// known only to a human. Whether that dimension should exist is still undecided -- the full
/// argument for both sides is written on the `tags` column in `migrations/0002_ingest.sql`, so do
/// not delete this as dead code.
pub async fn set_document_tags(
    pool: &PgPool,
    kb_id: Uuid,
    document_id: Uuid,
    tags: &[String],
) -> AppResult<()> {
    let cleaned: Vec<String> = tags
        .iter()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect();
    let res = sqlx::query(
        "UPDATE documents SET tags = $3, updated_at = now() WHERE id = $1 AND kb_id = $2",
    )
    .bind(document_id)
    .bind(kb_id)
    .bind(&cleaned)
    .execute(pool)
    .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    Ok(())
}

/// Dedup for syncing: does this kb already hold a document with the same content.
pub async fn document_exists_by_sha(pool: &PgPool, kb_id: Uuid, sha256: &str) -> AppResult<bool> {
    let row: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM documents WHERE kb_id = $1 AND sha256 = $2 LIMIT 1")
            .bind(kb_id)
            .bind(sha256)
            .fetch_optional(pool)
            .await?;
    Ok(row.is_some())
}

/// Records the sync time (keeps the scheduler from firing again during a long sync and then
/// coming due immediately after).
pub async fn touch_sync_time(pool: &PgPool, id: Uuid, at: DateTime<Utc>) -> AppResult<()> {
    sqlx::query("UPDATE sources SET last_sync_at = $2 WHERE id = $1")
        .bind(id)
        .bind(at)
        .execute(pool)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Sync run records (the channel audit history)
// ---------------------------------------------------------------------------

pub async fn start_run(pool: &PgPool, source_id: Uuid) -> AppResult<Uuid> {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO source_sync_runs (id, source_id) VALUES ($1, $2)")
        .bind(id)
        .bind(source_id)
        .execute(pool)
        .await?;
    Ok(id)
}

pub async fn finish_run(
    pool: &PgPool,
    run_id: Uuid,
    source_id: Uuid,
    error: Option<&str>,
    created_docs: i32,
    updated_docs: i32,
) -> AppResult<()> {
    sqlx::query(
        "UPDATE source_sync_runs SET finished_at = now(), status = $2, error = $3,
                created_docs = $4, updated_docs = $5
         WHERE id = $1",
    )
    .bind(run_id)
    .bind(if error.is_some() { "failed" } else { "ok" })
    .bind(error)
    .bind(created_docs)
    .bind(updated_docs)
    .execute(pool)
    .await?;
    // Only the most recent 50 per source are kept
    sqlx::query(
        "DELETE FROM source_sync_runs WHERE source_id = $1 AND id NOT IN
         (SELECT id FROM source_sync_runs WHERE source_id = $1
          ORDER BY started_at DESC LIMIT 50)",
    )
    .bind(source_id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn list_runs(pool: &PgPool, source_id: Uuid, limit: i64) -> AppResult<Vec<SyncRun>> {
    let rows: Vec<SyncRun> = sqlx::query_as(
        "SELECT id, started_at, finished_at, status, created_docs, updated_docs, error
         FROM source_sync_runs WHERE source_id = $1
         ORDER BY started_at DESC LIMIT $2",
    )
    .bind(source_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cron_validation() {
        assert_eq!(validate_cron("30 9 * * *").unwrap(), "30 9 * * *");
        assert_eq!(
            validate_cron("  0  9 * * Mon,Thu ").unwrap(),
            "0 9 * * Mon,Thu"
        );
        assert!(validate_cron("9 * * *").is_err()); // 4 fields
        assert!(validate_cron("99 9 * * *").is_err()); // minute out of range
        assert!(validate_cron("0 0 0 0 0 0").is_err()); // 6 fields
    }

    #[test]
    fn cron_next_computes() {
        let after = chrono::Utc::now();
        let next = cron_next_after("*/5 * * * *", after).unwrap();
        assert!(next > after);
        assert!((next - after).num_minutes() <= 5);
    }
}
