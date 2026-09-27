//! Query-the-data data sources: system-level registration (admin; credentials go in and never
//! come out) + knowledge-base-level mounting (KB admin).
//! On mount, and on a manual refresh, the target database's schema is generated as markdown and
//! ingested into the KB (updated in place under the same key), so Chat can retrieve the table
//! structure before it writes SQL. The safety gate on query execution is at knife 2 (the
//! query_data tool).

use axum::extract::{Path, State};
use axum::Json;
use serde::Deserialize;
use serde_json::json;
use utopia_core::models::Role;
use utopia_core::AppError;
use uuid::Uuid;

use super::graph_routes::require_kb;
use crate::auth::AuthUser;
use crate::error::ApiResult;
use crate::state::AppState;

fn require_admin(user: &utopia_core::models::User) -> Result<(), AppError> {
    if user.is_admin {
        Ok(())
    } else {
        Err(AppError::Forbidden)
    }
}

// ---------------------------------------------------------------------------
// System level: register / test / delete
// ---------------------------------------------------------------------------

pub async fn list(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
) -> ApiResult<Json<serde_json::Value>> {
    require_admin(&user)?;
    let sources = utopia_store::datasources::list(&state.pool).await?;
    Ok(Json(json!({ "data_sources": sources })))
}

#[derive(Deserialize)]
pub struct CreateBody {
    pub name: String,
    #[serde(default = "default_engine")]
    pub engine: String,
    pub conn_string: String,
}
fn default_engine() -> String {
    "postgres".into()
}

pub async fn create(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Json(body): Json<CreateBody>,
) -> ApiResult<Json<serde_json::Value>> {
    require_admin(&user)?;
    // The engine follows the scheme; the UI has only one connection-string input box.
    // body.engine is kept only for compatibility with old callers
    let engine = crate::query_engine::engine_from_conn(&body.conn_string).ok_or_else(|| {
        utopia_core::AppError::invalid(
            "unsupported_conn_scheme",
            format!(
                "Connection string must start with one of: postgres://, trino://, databricks://, snowflake:// (engines: {})",
                crate::query_engine::ENGINES.join(", ")
            ),
        )
    })?;
    let _ = &body.engine;
    // The shape of the connection string is validated at registration time (missing token,
    // missing warehouse, ...) and the error message carries the correct syntax; otherwise you
    // only find out at "test", and that step only returns ok:false
    crate::query_engine::engine_for(engine, &body.conn_string)
        .map_err(|e| utopia_core::AppError::invalid("bad_conn_string", e.to_string()))?;
    let id = utopia_store::datasources::create(
        &state.pool,
        &body.name,
        engine,
        &body.conn_string,
        user.id,
    )
    .await?;
    Ok(Json(json!({ "id": id })))
}

pub async fn delete(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    require_admin(&user)?;
    utopia_store::datasources::delete(&state.pool, id).await?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn test(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    require_admin(&user)?;
    let (engine, conn) = utopia_store::datasources::engine_and_conn(&state.pool, id).await?;
    let ok = match crate::query_engine::engine_for(&engine, &conn) {
        Ok(eng) => eng.test().await.is_ok(),
        Err(_) => false,
    };
    utopia_store::datasources::record_test(&state.pool, id, ok).await?;
    Ok(Json(json!({ "ok": ok })))
}

// ---------------------------------------------------------------------------
// Knowledge-base level: mount / unmount / schema refresh
// ---------------------------------------------------------------------------

pub async fn mounted(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(kb_id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Viewer).await?;
    let mounted = utopia_store::datasources::mounted(&state.pool, kb_id).await?;
    Ok(Json(json!({ "data_sources": mounted })))
}

/// Which ones a KB admin can mount (the list gives name/summary, with no credentials).
///
/// **Only lists the ones granted to this workspace (0014).** This used to return
/// `datasources::list` -- every source in the whole deployment, which meant any database's
/// administrator could see, and mount, any production database.
pub async fn mountable(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(kb_id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    let kb = require_kb(&state, &user, kb_id, Role::Admin).await?;
    let granted =
        utopia_store::datasources::granted_to_workspace(&state.pool, kb.workspace_id).await?;
    Ok(Json(json!({ "data_sources": granted })))
}

pub async fn mount(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path((kb_id, ds_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Admin).await?;
    // **Filtering the list is not a guard.** That only blocks "can see it", and this endpoint
    // is called by id -- anyone can put together a uuid of their own and fire it at us. The
    // grant is checked again right here
    if !utopia_store::datasources::is_granted(&state.pool, kb_id, ds_id).await? {
        return Err(AppError::invalid(
            "source_not_granted",
            "This data source is not available to this workspace",
        )
        .into());
    }
    utopia_store::datasources::mount(&state.pool, kb_id, ds_id).await?;
    // Mounting means ingesting the schema: Chat can retrieve the table structure before it
    // writes SQL
    //
    // **A failure in this step must not be reported back as a mount failure.** The line above
    // has already written to kb_data_sources, so the source really is mounted; this used to `?`
    // out of here and return a 500, so the human thought it had not mounted when in fact it had
    // -- and query-the-data could not see which tables it had. Changed to: say truthfully that
    // the mount succeeded and the schema did not, and report it to the alert centre, because
    // from that point on it is a silent missing state (0009)
    match sync_schema_doc(&state, kb_id, ds_id).await {
        Ok(synced) => Ok(Json(json!({ "ok": true, "schema_tables": synced }))),
        Err(e) => {
            let name = source_name(&state, ds_id).await;
            crate::alerting::observe_schema_sync_failure(&state, kb_id, ds_id, &name, &e).await;
            Ok(Json(json!({
                "ok": true,
                "schema_tables": 0,
                "schema_error": e.to_string(),
            })))
        }
    }
}

/// The alert has to hold on to the name: once the source is deleted, `subject_id` no longer
/// resolves to one. When it cannot be found, hand back a placeholder rather than letting the
/// alert itself fail -- **a failure on the alerting path must not drown out the thing it is
/// there to report**.
async fn source_name(state: &AppState, ds_id: Uuid) -> String {
    utopia_store::datasources::list(&state.pool)
        .await
        .ok()
        .and_then(|all| all.into_iter().find(|d| d.id == ds_id).map(|d| d.name))
        .unwrap_or_else(|| ds_id.to_string())
}

pub async fn unmount(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path((kb_id, ds_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Admin).await?;
    utopia_store::datasources::unmount(&state.pool, kb_id, ds_id).await?;
    Ok(Json(json!({ "ok": true })))
}

/// Refreshing the structure by hand.
///
/// **Here the error is returned to the caller as before** -- the person who pressed the button
/// is watching, and nothing happened halfway. But an alert is raised just the same: the
/// consequences left behind are exactly the same as for a failed mount (the source is mounted
/// and the table structure is stale or empty), and the person who pressed the button is not
/// necessarily the person who needs to know about it.
pub async fn sync_schema(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path((kb_id, ds_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Admin).await?;
    match sync_schema_doc(&state, kb_id, ds_id).await {
        Ok(synced) => Ok(Json(json!({ "ok": true, "schema_tables": synced }))),
        Err(e) => {
            let name = source_name(&state, ds_id).await;
            crate::alerting::observe_schema_sync_failure(&state, kb_id, ds_id, &name, &e).await;
            Err(AppError::Other(e).into())
        }
    }
}

/// Agentic exploration: a background job reads the mounted source's schema and proposes
/// metric/dimension -> field mappings (low-confidence ones go to Review).
pub async fn explore(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(kb_id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Admin).await?;
    if utopia_store::datasources::mounted(&state.pool, kb_id)
        .await?
        .is_empty()
    {
        return Err(AppError::invalid("no_data_sources", "No data sources mounted").into());
    }
    utopia_store::jobs::enqueue(&state.pool, "explore_mappings", json!({ "kb_id": kb_id })).await?;
    Ok(Json(json!({ "ok": true })))
}

/// Pull information_schema, generate markdown, and ingest it through the three-way decision
/// (updated in place under the same key).
/// The document hangs off the per-KB "Data schemas" folder source.
async fn sync_schema_doc(state: &AppState, kb_id: Uuid, ds_id: Uuid) -> anyhow::Result<usize> {
    const MAX_TABLES: usize = 200;
    let name = utopia_store::datasources::list(&state.pool)
        .await?
        .into_iter()
        .find(|d| d.id == ds_id)
        .map(|d| d.name)
        .ok_or_else(|| anyhow::anyhow!("Data source not found"))?;
    let (engine, conn) = utopia_store::datasources::engine_and_conn(&state.pool, ds_id).await?;
    let cols = crate::query_engine::engine_for(&engine, &conn)?
        .fetch_schema()
        .await?;

    let mut md = format!(
        "# Data source: {name}\n\nEngine: {engine}. Tables and columns available for SQL queries against this source; write SQL in this engine's dialect.\n"
    );
    let mut current = String::new();
    let mut tables = 0usize;
    for c in &cols {
        let key = format!("{}.{}", c.schema, c.table);
        if key != current {
            if tables >= MAX_TABLES {
                md.push_str("\n(further tables omitted)\n");
                break;
            }
            current = key.clone();
            tables += 1;
            md.push_str(&format!("\n## {key}\n"));
        }
        md.push_str(&format!(
            "- {} ({}){}\n",
            c.column,
            c.data_type,
            c.comment
                .as_deref()
                .map(|x| format!(" — {x}"))
                .unwrap_or_default()
        ));
    }

    // The per-KB "Data schemas" container source (folder: pure container semantics)
    let folder = match sqlx::query_as::<_, (Uuid,)>(
        "SELECT id FROM sources WHERE kb_id = $1 AND kind = 'folder' AND name = 'Data schemas'",
    )
    .bind(kb_id)
    .fetch_optional(&state.pool)
    .await?
    {
        Some((id,)) => id,
        None => {
            utopia_store::sources::create(
                &state.pool,
                kb_id,
                "folder",
                "Data schemas",
                &serde_json::json!({}),
                Some("database"),
                None,
                None,
            )
            .await?
            .id
        }
    };
    crate::ingest_sources::ingest_item(
        state,
        kb_id,
        folder,
        &format!("datasource:{ds_id}:schema"),
        &format!("{name}-schema.md"),
        "text/markdown",
        md.as_bytes(),
        None,
    )
    .await?;
    state.emit_source(kb_id);
    Ok(tables)
}

// ---------------------------------------------------------------------------
// System level: grants (0014)
//
// Granting and mounting are two layers, each with its own owner:
//   grant = a system administrator saying "which workspaces may use this source"  <- here
//   mount = a KB administrator saying "which ones my database mounts"             <- that group
// Both layers are many-to-many. Mounting can only pick from the granted set.
// ---------------------------------------------------------------------------

/// Which workspaces this source has been granted to.
pub async fn grants(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    require_admin(&user)?;
    let rows = utopia_store::datasources::grants_for_source(&state.pool, id).await?;
    let workspaces: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|(id, name)| json!({ "id": id, "name": name }))
        .collect();
    Ok(Json(json!({ "workspaces": workspaces })))
}

pub async fn grant(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path((id, workspace_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<serde_json::Value>> {
    require_admin(&user)?;
    utopia_store::datasources::grant(&state.pool, id, workspace_id, user.id).await?;
    let _ = utopia_store::audit::record(
        &state.pool,
        None,
        user.id,
        "data_source.granted",
        "data_source",
        Some(id),
        json!({ "workspace_id": workspace_id }),
    )
    .await;
    Ok(Json(json!({ "ok": true })))
}

/// Revoking a grant. **Along with it, unmount the ones already mounted in that workspace** --
/// delete only the grant row and query-the-data still reads `kb_data_sources`, so the revocation
/// does not take effect. Returns how many were unmounted, so the UI can say "3 databases were
/// unmounted along with it" instead of quietly cutting someone's connection.
pub async fn revoke(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path((id, workspace_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<serde_json::Value>> {
    require_admin(&user)?;
    let unmounted = utopia_store::datasources::revoke(&state.pool, id, workspace_id).await?;
    let _ = utopia_store::audit::record(
        &state.pool,
        None,
        user.id,
        "data_source.revoked",
        "data_source",
        Some(id),
        json!({ "workspace_id": workspace_id, "unmounted": unmounted }),
    )
    .await;
    Ok(Json(json!({ "ok": true, "unmounted": unmounted })))
}
