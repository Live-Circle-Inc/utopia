//! Putting failed jobs back on the queue (#216).
//!
//! A failed job used to be re-runnable only through the object it belongs to: a document can be
//! re-extracted, a source can be re-synced; the likes of `bootstrap_ontology` and
//! `adjudicate_entities` have no object to click on. When the credit runs out (#201 makes them
//! fail on the very first attempt) a whole batch of documents stops, and after topping up you
//! have to click them one by one. This gives two entry points: per-KB (Editor) and global
//! (admin), with the scope narrowable by kind and by failure time -- the "run it again" on an
//! alert passes exactly the time window of that incident.

use axum::extract::{Path, State};
use axum::Json;
use serde::Deserialize;
use serde_json::json;
use utopia_core::models::Role;
use utopia_store::jobs::RequeueScope;
use uuid::Uuid;

use super::graph_routes::require_kb;
use crate::auth::AuthUser;
use crate::error::ApiResult;
use crate::state::AppState;

#[derive(Deserialize, Default)]
pub struct RequeueBody {
    #[serde(default)]
    pub kind: Option<String>,
    /// Only requeue the ones that failed after this instant
    #[serde(default)]
    pub failed_since: Option<chrono::DateTime<chrono::Utc>>,
}

pub async fn failed_in_kb(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(kb_id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Viewer).await?;
    let failed = utopia_store::jobs::failed_count(&state.pool, Some(kb_id)).await?;
    Ok(Json(json!({ "failed": failed })))
}

pub async fn requeue_in_kb(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(kb_id): Path<Uuid>,
    Json(body): Json<RequeueBody>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Editor).await?;
    let requeued = utopia_store::jobs::requeue_failed(
        &state.pool,
        RequeueScope {
            kb_id: Some(kb_id),
            kind: body.kind.as_deref(),
            failed_since: body.failed_since,
        },
    )
    .await?;
    let _ = utopia_store::audit::record(
        &state.pool,
        Some(kb_id),
        user.id,
        "jobs.requeued",
        "kb",
        Some(kb_id),
        json!({ "requeued": requeued, "kind": body.kind, "failed_since": body.failed_since }),
    )
    .await;
    Ok(Json(json!({ "requeued": requeued })))
}

/// Global requeue: system-level alerts (the ones with no KB) come through here. Admins only --
/// it touches the jobs of every KB
pub async fn requeue_all(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Json(body): Json<RequeueBody>,
) -> ApiResult<Json<serde_json::Value>> {
    if !user.is_admin {
        return Err(utopia_core::AppError::Forbidden.into());
    }
    let requeued = utopia_store::jobs::requeue_failed(
        &state.pool,
        RequeueScope {
            kb_id: None,
            kind: body.kind.as_deref(),
            failed_since: body.failed_since,
        },
    )
    .await?;
    let _ = utopia_store::audit::record(
        &state.pool,
        None,
        user.id,
        "jobs.requeued",
        "system",
        None,
        json!({ "requeued": requeued, "kind": body.kind, "failed_since": body.failed_since }),
    )
    .await;
    Ok(Json(json!({ "requeued": requeued })))
}
