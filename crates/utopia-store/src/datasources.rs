//! Data sources for data questions: registration at the system level (credentials kept in one
//! place, reused across KBs) + mounting at the knowledge base level (permissions follow the KB).
//! The safety gates at query execution time (read-only session, allowlist from SQL parsing,
//! LIMIT/timeout) live on the server side.

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use utopia_core::models::DataSourceView;
use utopia_core::{AppError, AppResult};
use uuid::Uuid;

/// The row projection of data_sources, shared by list and mounted.
type DataSourceRow = (
    Uuid,
    String,
    String,
    String,
    DateTime<Utc>,
    Option<DateTime<Utc>>,
    Option<bool>,
);

/// Connection string → credential-free summary (host[:port]/path). On a parse failure give a
/// placeholder; never echo the original string back.
/// If the port was not written, do not fill one in: the four schemes each have a different
/// default port, and filling in the wrong one misleads more than filling in nothing
pub fn conn_summary(conn: &str) -> String {
    url::Url::parse(conn)
        .ok()
        .map(|u| {
            format!(
                "{}{}{}",
                u.host_str().unwrap_or("?"),
                u.port().map(|p| format!(":{p}")).unwrap_or_default(),
                u.path()
            )
        })
        .unwrap_or_else(|| "(unparsed)".into())
}

/// Row → view. **The connection string is swapped for the credential-free summary here**, which
/// is the gate that keeps it from leaking out; all four queries share this one copy, so that a
/// fifth one added some day cannot forget to do the swap
fn row_to_view(
    (id, name, engine, conn, created_at, last_test_at, last_test_ok): DataSourceRow,
) -> DataSourceView {
    DataSourceView {
        id,
        name,
        engine,
        summary: conn_summary(&conn),
        created_at,
        last_test_at,
        last_test_ok,
    }
}

/// Every source in the deployment. **For the system admin's registration desk only.**
/// The mountable list used to go through this too, and that is the door 0014 closed -- the KB
/// side now goes through `granted_to_workspace`.
pub async fn list(pool: &PgPool) -> AppResult<Vec<DataSourceView>> {
    let rows: Vec<DataSourceRow> = sqlx::query_as(
        "SELECT id, name, engine, conn_string, created_at, last_test_at, last_test_ok
             FROM data_sources ORDER BY name",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(row_to_view).collect())
}

pub async fn create(
    pool: &PgPool,
    name: &str,
    engine: &str,
    conn_string: &str,
    created_by: Uuid,
) -> AppResult<Uuid> {
    if name.trim().is_empty() {
        return Err(AppError::invalid(
            "ds_name_required",
            "Data source name is required",
        ));
    }
    // The engine is decided by the caller from the connection string's scheme
    // (`query_engine::engine_from_conn`); the permitted values live in the CHECK in migration
    // 0020, and are not copied a second time here
    if engine.is_empty() || conn_string.trim().is_empty() {
        return Err(AppError::invalid(
            "bad_conn_string",
            "A connection string is required",
        ));
    }
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO data_sources (id, name, engine, conn_string, created_by)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(id)
    .bind(name.trim())
    .bind(engine)
    .bind(conn_string.trim())
    .bind(created_by)
    .execute(pool)
    .await?;
    Ok(id)
}

pub async fn delete(pool: &PgPool, id: Uuid) -> AppResult<()> {
    let res = sqlx::query("DELETE FROM data_sources WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    Ok(())
}

/// The connection string only circulates inside the server (connection test / query execution).
pub async fn conn_string(pool: &PgPool, id: Uuid) -> AppResult<String> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT conn_string FROM data_sources WHERE id = $1")
            .bind(id)
            .fetch_optional(pool)
            .await?;
    row.map(|(c,)| c).ok_or(AppError::NotFound)
}

pub async fn record_test(pool: &PgPool, id: Uuid, ok: bool) -> AppResult<()> {
    sqlx::query("UPDATE data_sources SET last_test_at = now(), last_test_ok = $2 WHERE id = $1")
        .bind(id)
        .bind(ok)
        .execute(pool)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// KB mounting
// ---------------------------------------------------------------------------

pub async fn mount(pool: &PgPool, kb_id: Uuid, data_source_id: Uuid) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO kb_data_sources (kb_id, data_source_id) VALUES ($1, $2)
         ON CONFLICT DO NOTHING",
    )
    .bind(kb_id)
    .bind(data_source_id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn unmount(pool: &PgPool, kb_id: Uuid, data_source_id: Uuid) -> AppResult<()> {
    sqlx::query("DELETE FROM kb_data_sources WHERE kb_id = $1 AND data_source_id = $2")
        .bind(kb_id)
        .bind(data_source_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn mounted(pool: &PgPool, kb_id: Uuid) -> AppResult<Vec<DataSourceView>> {
    let rows: Vec<DataSourceRow> = sqlx::query_as(
        "SELECT d.id, d.name, d.engine, d.conn_string, d.created_at,
                    d.last_test_at, d.last_test_ok
             FROM kb_data_sources m JOIN data_sources d ON d.id = m.data_source_id
             WHERE m.kb_id = $1 ORDER BY d.name",
    )
    .bind(kb_id)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(row_to_view).collect())
}

/// (engine, conn_string): for query execution / testing / pulling the schema (credentials never
/// leave the server).
pub async fn engine_and_conn(pool: &PgPool, id: Uuid) -> AppResult<(String, String)> {
    let row: Option<(String, String)> =
        sqlx::query_as("SELECT engine, conn_string FROM data_sources WHERE id = $1")
            .bind(id)
            .fetch_optional(pool)
            .await?;
    row.ok_or(AppError::NotFound)
}

/// Which sources this workspace has been granted (0014).
///
/// **From now on the mountable list goes through here, not through `list`.** What that old path
/// showed a KB admin was `datasources::list(pool)` -- every single source in the deployment, with
/// no filtering. So an admin of any KB could mount any production database into their own KB, and
/// once mounted every Viewer of that KB could run read-only SQL against it.
pub async fn granted_to_workspace(
    pool: &PgPool,
    workspace_id: Uuid,
) -> AppResult<Vec<DataSourceView>> {
    let rows: Vec<DataSourceRow> = sqlx::query_as(
        "SELECT d.id, d.name, d.engine, d.conn_string, d.created_at,
                d.last_test_at, d.last_test_ok
           FROM data_source_grants g JOIN data_sources d ON d.id = g.data_source_id
          WHERE g.workspace_id = $1 ORDER BY d.name",
    )
    .bind(workspace_id)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(row_to_view).collect())
}

/// Which workspaces this source has been granted to. The admin console reads this.
pub async fn grants_for_source(
    pool: &PgPool,
    data_source_id: Uuid,
) -> AppResult<Vec<(Uuid, String)>> {
    Ok(sqlx::query_as(
        "SELECT w.id, w.name FROM data_source_grants g
           JOIN workspaces w ON w.id = g.workspace_id
          WHERE g.data_source_id = $1 ORDER BY w.name",
    )
    .bind(data_source_id)
    .fetch_all(pool)
    .await?)
}

/// Grant a workspace the use of this source. Idempotent.
pub async fn grant(
    pool: &PgPool,
    data_source_id: Uuid,
    workspace_id: Uuid,
    actor: Uuid,
) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO data_source_grants (data_source_id, workspace_id, granted_by)
         VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
    )
    .bind(data_source_id)
    .bind(workspace_id)
    .bind(actor)
    .execute(pool)
    .await?;
    Ok(())
}

/// Revoke a grant, **unmounting whatever is already mounted in that workspace along with it**.
///
/// Deleting the grant row alone is not enough: `mounted` reads `kb_data_sources`, and so do data
/// questions. Leaving the mount in place means the grant is revoked while access carries on as
/// before -- and a permission revocation that does not take effect is more dangerous than none.
///
/// Done inside one transaction: a crash between the two DELETEs would leave behind exactly the
/// "mounted without a grant" state, and that is precisely what this migration is meant to
/// eliminate.
pub async fn revoke(pool: &PgPool, data_source_id: Uuid, workspace_id: Uuid) -> AppResult<u64> {
    let mut tx = pool.begin().await?;
    let unmounted = sqlx::query(
        "DELETE FROM kb_data_sources
          WHERE data_source_id = $1
            AND kb_id IN (SELECT id FROM knowledge_bases WHERE workspace_id = $2)",
    )
    .bind(data_source_id)
    .bind(workspace_id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    sqlx::query("DELETE FROM data_source_grants WHERE data_source_id = $1 AND workspace_id = $2")
        .bind(data_source_id)
        .bind(workspace_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(unmounted)
}

/// Whether this source has been granted to this knowledge base. **Must be checked before
/// mounting** -- the guard cannot live only on the list side: the list filter blocks "being able
/// to see it", while the mount endpoint is called with an id, and anyone can assemble one.
pub async fn is_granted(pool: &PgPool, kb_id: Uuid, data_source_id: Uuid) -> AppResult<bool> {
    let found: Option<(i32,)> = sqlx::query_as(
        "SELECT 1 FROM data_source_grants g
           JOIN knowledge_bases kb ON kb.workspace_id = g.workspace_id
          WHERE kb.id = $1 AND g.data_source_id = $2",
    )
    .bind(kb_id)
    .bind(data_source_id)
    .fetch_optional(pool)
    .await?;
    Ok(found.is_some())
}
