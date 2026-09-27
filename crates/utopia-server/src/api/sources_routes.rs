//! Source management API plus text-push ingestion (ingest).

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::Json;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::json;
use utopia_core::models::{Role, SOURCE_SECRET_KEYS};
use uuid::Uuid;

use super::graph_routes::require_kb;
use crate::auth::AuthUser;
use crate::error::ApiResult;
use crate::state::AppState;

/// Generates the push token for an api source.
fn new_ingest_token() -> String {
    format!("utp_{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

/// Fetches one source and confirms it belongs to the kb named in the path.
///
/// `require_kb` only checks a person's permission on the kb; the source id is a separate
/// dimension -- without comparing it, an Editor on kb A holding the id of a source in kb B could
/// sync it, clean it up, and delete it. Not belonging is treated as not existing (404), matching
/// what `get_token` has always done
async fn source_in_kb(
    state: &AppState,
    kb_id: Uuid,
    source_id: Uuid,
) -> ApiResult<utopia_core::models::Source> {
    let source = utopia_store::sources::get(&state.pool, source_id).await?;
    if source.kb_id != kb_id {
        return Err(utopia_core::AppError::NotFound.into());
    }
    Ok(source)
}

pub async fn list(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(kb_id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Viewer).await?;
    let sources = utopia_store::sources::list(&state.pool, kb_id).await?;
    Ok(Json(json!({ "sources": sources })))
}

#[derive(Deserialize)]
pub struct CreateBody {
    pub kind: String,
    pub name: String,
    #[serde(default)]
    pub config: serde_json::Value,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub sync_interval_minutes: Option<i32>,
    /// Standard 5-field cron (mutually exclusive with interval; pass one or the other)
    #[serde(default)]
    pub sync_cron: Option<String>,
}

pub async fn create(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(kb_id): Path<Uuid>,
    Json(body): Json<CreateBody>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Editor).await?;
    let source = utopia_store::sources::create(
        &state.pool,
        kb_id,
        &body.kind,
        &body.name,
        &body.config,
        body.icon.as_deref(),
        body.sync_interval_minutes,
        body.sync_cron.as_deref(),
    )
    .await?;
    // Pull-style sources sync once immediately after creation (configured means output; no need
    // to wait for the next scheduling cycle)
    if matches!(source.kind.as_str(), "url" | "rss" | "custom") {
        let _ = utopia_store::sources::mark_queued(&state.pool, source.id).await;
        utopia_store::jobs::enqueue(
            &state.pool,
            "sync_source",
            json!({ "source_id": source.id }),
        )
        .await?;
    }
    // api sources: generate a dedicated push token (viewable any time afterwards via get_token)
    let mut ingest_token: Option<String> = None;
    if source.kind == "api" {
        let token = new_ingest_token();
        utopia_store::sources::set_ingest_token(&state.pool, source.id, &token).await?;
        ingest_token = Some(token);
    }
    state.emit_source(kb_id);
    let _ = utopia_store::audit::record(
        &state.pool,
        Some(kb_id),
        user.id,
        "source.created",
        "source",
        Some(source.id),
        json!({ "kind": source.kind, "name": source.name }),
    )
    .await;
    Ok(Json(
        json!({ "source": mask_secrets(source), "ingest_token": ingest_token }),
    ))
}

/// Views the push token of an api source (Editor; list responses never carry it, viewing goes
/// through here).
pub async fn get_token(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path((kb_id, source_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Editor).await?;
    let source = source_in_kb(&state, kb_id, source_id).await?;
    if source.kind != "api" {
        return Err(utopia_core::AppError::NotFound.into());
    }
    Ok(Json(json!({ "ingest_token": source.ingest_token })))
}

/// Rotates the push token of an api source: the old token stops working immediately.
pub async fn rotate_token(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path((kb_id, source_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Editor).await?;
    let source = source_in_kb(&state, kb_id, source_id).await?;
    if source.kind != "api" {
        return Err(utopia_core::AppError::NotFound.into());
    }
    let token = new_ingest_token();
    utopia_store::sources::set_ingest_token(&state.pool, source_id, &token).await?;
    Ok(Json(json!({ "ingest_token": token })))
}

/// Strips credentials before responding (they go in only, never out; for the keys see
/// `SOURCE_SECRET_KEYS`).
fn mask_secrets(source: utopia_core::models::Source) -> utopia_core::models::Source {
    source.without_secrets()
}

/// The merge rule for credentials on update, identical for every key in `SOURCE_SECRET_KEYS`:
/// the key is **absent** from the new config or its value is an empty string → keep the value
/// stored in the database (leaving the form blank means "don't touch it"); an explicit `null` →
/// delete it; anything else takes the new value. Responses never echo them back, so the client
/// has no way to send the old value back verbatim; the rule can only live here
fn keep_secrets(next: &mut serde_json::Value, existing: &serde_json::Value) {
    let Some(obj) = next.as_object_mut() else {
        return;
    };
    for key in SOURCE_SECRET_KEYS {
        let keep = match obj.get(*key) {
            None => true,
            Some(serde_json::Value::Null) => {
                obj.remove(*key);
                false
            }
            Some(v) => v.as_str().is_some_and(|s| s.trim().is_empty()),
        };
        if keep {
            obj.remove(*key);
            if let Some(prev) = existing.get(*key) {
                obj.insert((*key).to_string(), prev.clone());
            }
        }
    }
}

#[derive(Deserialize)]
pub struct UpdateBody {
    pub name: Option<String>,
    pub config: Option<serde_json::Value>,
    pub icon: Option<String>,
    /// The presence of the schedule field overwrites the schedule wholesale (interval and cron
    /// are mutually exclusive; both null = timed syncing off)
    #[serde(default)]
    pub schedule: Option<ScheduleBody>,
}

#[derive(Deserialize)]
pub struct ScheduleBody {
    #[serde(default)]
    pub sync_interval_minutes: Option<i32>,
    #[serde(default)]
    pub sync_cron: Option<String>,
}

pub async fn update(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path((kb_id, source_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<UpdateBody>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Editor).await?;
    let existing = source_in_kb(&state, kb_id, source_id).await?;
    // Credentials go in only, never out: responses never echo them, and a blank form field / an
    // absent field = keep the value stored in the database
    let mut config = body.config;
    if let Some(cfg) = config.as_mut() {
        keep_secrets(cfg, &existing.config);
    }
    let source = utopia_store::sources::update(
        &state.pool,
        source_id,
        body.name.as_deref(),
        config.as_ref(),
        body.icon.as_deref(),
        body.schedule
            .map(|s| (s.sync_interval_minutes, s.sync_cron)),
    )
    .await?;
    state.emit_source(kb_id);
    // The audit log never stores credentials: for config it only records "changed or not"
    let _ = utopia_store::audit::record(
        &state.pool,
        Some(kb_id),
        user.id,
        "source.updated",
        "source",
        Some(source_id),
        json!({ "name": body.name, "config_changed": config.is_some() }),
    )
    .await;
    Ok(Json(json!({ "source": mask_secrets(source) })))
}

/// Bulk-deletes every document under this source that is "no longer in the source" (the ones
/// flagged by url reconciliation / custom tombstones).
pub async fn cleanup_missing(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path((kb_id, source_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Editor).await?;
    source_in_kb(&state, kb_id, source_id).await?;
    let ids = utopia_store::documents::list_missing(&state.pool, source_id).await?;
    for id in &ids {
        utopia_store::documents::delete(&state.pool, *id).await?;
        let search = state.search.clone();
        let did = id.to_string();
        tokio::task::spawn_blocking(move || search.delete_document(&did))
            .await
            .map_err(|e| utopia_core::AppError::Other(e.into()))?
            .map_err(utopia_core::AppError::Other)?;
    }
    state.emit_source(kb_id);
    Ok(Json(json!({ "deleted": ids.len() })))
}

pub async fn delete(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path((kb_id, source_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Editor).await?;
    // The Memory source is permanent: the memory space does not evaporate because someone tidied
    // up their sources (the memory documents themselves can be deleted in the Library)
    let source = source_in_kb(&state, kb_id, source_id).await?;
    if source.kind == utopia_store::memory::MEMORY_SOURCE_KIND {
        return Err(utopia_core::AppError::invalid(
            "memory_source_permanent",
            "The Memory source is permanent.",
        )
        .into());
    }
    utopia_store::sources::delete(&state.pool, source_id).await?;
    state.emit_source(kb_id);
    let _ = utopia_store::audit::record(
        &state.pool,
        Some(kb_id),
        user.id,
        "source.deleted",
        "source",
        Some(source_id),
        json!({}),
    )
    .await;
    Ok(Json(json!({ "ok": true })))
}

/// Sync run history (channel audit).
pub async fn runs(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path((kb_id, source_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Viewer).await?;
    source_in_kb(&state, kb_id, source_id).await?;
    let runs = utopia_store::sources::list_runs(&state.pool, source_id, 20).await?;
    Ok(Json(json!({ "runs": runs })))
}

pub async fn sync_now(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path((kb_id, source_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Editor).await?;
    source_in_kb(&state, kb_id, source_id).await?;
    let queued = utopia_store::sources::mark_queued(&state.pool, source_id).await?;
    if queued {
        utopia_store::jobs::enqueue(
            &state.pool,
            "sync_source",
            json!({ "source_id": source_id }),
        )
        .await?;
        state.emit_source(kb_id);
    }
    Ok(Json(json!({ "queued": queued })))
}

#[derive(Deserialize)]
pub struct IngestBody {
    pub filename: String,
    /// A tombstone push (`deleted: true`) may come without content -- that is what the guide has
    /// always said, while the field used to be required, so a missing one was rejected right at
    /// deserialization. Everything else still requires it: an empty string is caught by the
    /// validation below
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub doc_time: Option<DateTime<Utc>>,
    /// The caller's logical document ID: pushing the same ID again = updating that same document
    /// (replaced in place plus a version record). Omit it and the filename becomes the identity.
    #[serde(default)]
    pub external_id: Option<String>,
    /// Tombstone: true = flag the identity as "no longer in the source" (not a delete; only
    /// supported when pushing to an api source). content may be omitted in that case; pushing the
    /// same identity normally again takes the flag back off.
    #[serde(default)]
    pub deleted: bool,
}

fn action_str(action: crate::ingest_sources::IngestAction) -> &'static str {
    match action {
        crate::ingest_sources::IngestAction::Created => "created",
        crate::ingest_sources::IngestAction::Updated => "updated",
        crate::ingest_sources::IngestAction::Moved => "moved",
        crate::ingest_sources::IngestAction::Unchanged => "unchanged",
        crate::ingest_sources::IngestAction::Tombstoned => "marked_missing",
    }
}

/// Kb-level text push (session-authenticated): plain upload semantics -- it lands in Uploads,
/// with no identity tracking. When you want "same ID again = update" semantics, create an api
/// source and push with its token.
pub async fn ingest(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(kb_id): Path<Uuid>,
    Json(body): Json<IngestBody>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Editor).await?;
    if body.deleted {
        return Err(utopia_core::AppError::Validation(
            "tombstones need identity tracking — push to an api source instead".into(),
        )
        .into());
    }
    if body.filename.trim().is_empty() || body.content.trim().is_empty() {
        return Err(
            utopia_core::AppError::Validation("filename and content are required".into()).into(),
        );
    }
    let action = crate::ingest_sources::ingest_upload(
        &state,
        kb_id,
        body.filename.trim(),
        "text/plain",
        body.content.as_bytes(),
        body.doc_time,
    )
    .await
    .map_err(utopia_core::AppError::Other)?;
    Ok(Json(json!({ "action": action_str(action) })))
}

/// The two natures of a failed push. **The caller sent it wrong** and **we failed to catch it on
/// our end** have to be kept apart: the former is recorded into the run for integration
/// debugging, but does not count as a source sync failure -- the source is not broken, that one
/// request was unacceptable; only the latter should mark the source failed and land in the alert
/// centre. Both used to go through `finish_sync(error)`, so a single malformed payload made the
/// bell say "source sync failed, no new content came in"
enum PushError {
    /// 4xx: unacceptable payload (JSON parsing, missing fields)
    Rejected(String),
    /// Ingestion itself failed
    Failed(String),
}

impl PushError {
    fn message(&self) -> &str {
        match self {
            PushError::Rejected(m) | PushError::Failed(m) => m,
        }
    }
}

/// Push handling after authentication: parse + validate + ingest/tombstone. Errors always come
/// back as text (recorded into the run).
async fn handle_push(
    state: &AppState,
    source: &utopia_core::models::Source,
    bytes: &[u8],
) -> Result<crate::ingest_sources::IngestAction, PushError> {
    let body: IngestBody = serde_json::from_slice(bytes)
        .map_err(|e| PushError::Rejected(format!("Invalid JSON payload: {e}")))?;
    let identity = body
        .external_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| body.filename.trim())
        .to_string();
    if identity.is_empty() {
        return Err(PushError::Rejected(
            "external_id or filename is required".into(),
        ));
    }
    let key = format!("api:{identity}");

    // Tombstone: flag it as "no longer in the source" (the same path as custom's deleted[]);
    // content may be omitted
    if body.deleted {
        utopia_store::documents::mark_missing_keys(&state.pool, source.id, &[key])
            .await
            .map_err(|e| PushError::Failed(e.to_string()))?;
        return Ok(crate::ingest_sources::IngestAction::Tombstoned);
    }

    if body.filename.trim().is_empty() || body.content.trim().is_empty() {
        return Err(PushError::Rejected(
            "filename and content are required".into(),
        ));
    }
    let action = crate::ingest_sources::ingest_item(
        state,
        source.kb_id,
        source.id,
        &key,
        body.filename.trim(),
        "text/plain",
        body.content.as_bytes(),
        body.doc_time,
    )
    .await
    .map_err(|e| PushError::Failed(e.to_string()))?;
    // Lost and found: an identity that was once tombstoned is pushed normally again, so take the
    // missing flag back off
    utopia_store::documents::clear_missing_keys(&state.pool, source.id, &[key])
        .await
        .map_err(|e| PushError::Failed(e.to_string()))?;
    Ok(action)
}

/// Push to an api source (authenticated by the source's own token, no session): three-way
/// identity semantics -- a new external_id → insert; same ID same content → no-op; same ID new
/// content → updated in place plus a version record.
/// Every push that gets past authentication records one run (malformed payloads included -- that
/// is what integration debugging lives on); unauthenticated requests write no record at all (no
/// creating a path into the database for anonymous traffic).
pub async fn push(
    State(state): State<AppState>,
    Path(source_id): Path<Uuid>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<Json<serde_json::Value>> {
    let source = utopia_store::sources::get(&state.pool, source_id).await?;
    if source.kind != "api" {
        return Err(utopia_core::AppError::NotFound.into());
    }
    // Bearer token check
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or(utopia_core::AppError::Unauthorized)?;
    if source.ingest_token.as_deref() != Some(token) {
        return Err(utopia_core::AppError::Unauthorized.into());
    }

    let run = utopia_store::sources::start_run(&state.pool, source.id).await?;
    match handle_push(&state, &source, &bytes).await {
        Ok(action) => {
            let (created, updated) = match action {
                crate::ingest_sources::IngestAction::Created => (1, 0),
                crate::ingest_sources::IngestAction::Updated
                | crate::ingest_sources::IngestAction::Moved => (0, 1),
                crate::ingest_sources::IngestAction::Unchanged
                | crate::ingest_sources::IngestAction::Tombstoned => (0, 0),
            };
            utopia_store::sources::finish_run(&state.pool, run, source.id, None, created, updated)
                .await?;
            // The source row reflects "the last push" in step: the action bar / left-column
            // status dot work straight away
            utopia_store::sources::finish_sync(&state.pool, source.id, None, created).await?;
            state.emit_source(source.kb_id);
            Ok(Json(json!({ "action": action_str(action) })))
        }
        Err(err) => {
            let msg = err.message().to_string();
            utopia_store::sources::finish_run(&state.pool, run, source.id, Some(&msg), 0, 0)
                .await?;
            // Only our failing to catch it counts as a source failure; the caller sending it
            // wrong is fine left in the run history
            if let PushError::Failed(_) = err {
                utopia_store::sources::finish_sync(&state.pool, source.id, Some(&msg), 0).await?;
            }
            state.emit_source(source.kb_id);
            Err(utopia_core::AppError::Validation(msg).into())
        }
    }
}

/// Source-wide full re-extraction (incremental semantics): every ready document under this
/// source runs through extraction again. It takes the normal pipeline -- entity resolution, fact
/// deduplication and temporal conflicts all behave as usual, and every existing human decision
/// is preserved.
pub async fn re_extract(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path((kb_id, source_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Editor).await?;
    let source = utopia_store::sources::get(&state.pool, source_id).await?;
    if source.kb_id != kb_id {
        return Err(utopia_core::AppError::NotFound.into());
    }
    // queue_extraction creates the jobs in the same transaction as the status change; all this
    // does is emit
    let ids =
        utopia_store::documents::queue_extraction(&state.pool, kb_id, Some(source_id)).await?;
    for id in &ids {
        state.emit_document(kb_id, *id);
    }
    let _ = utopia_store::audit::record(
        &state.pool,
        Some(kb_id),
        user.id,
        "source.re_extract",
        "source",
        Some(source_id),
        json!({ "name": source.name, "documents": ids.len() }),
    )
    .await;
    Ok(Json(json!({ "queued": ids.len() })))
}

#[cfg(test)]
mod tests {
    use super::keep_secrets;
    use serde_json::json;

    #[test]
    fn a_blank_or_missing_secret_keeps_the_stored_one() {
        let existing = json!({ "bucket": "old", "secret_access_key": "s", "password": "p" });
        // absent → keep; empty string → keep; a value → replace; null → delete
        let mut next = json!({ "bucket": "new", "password": "  ", "token": null });
        keep_secrets(&mut next, &existing);
        assert_eq!(next["bucket"], "new");
        assert_eq!(
            next["secret_access_key"], "s",
            "missing keeps the stored value"
        );
        assert_eq!(next["password"], "p", "blank keeps the stored value");
        assert!(next.get("token").is_none(), "an explicit null removes it");
        let mut next = json!({ "secret_access_key": "fresh" });
        keep_secrets(&mut next, &existing);
        assert_eq!(next["secret_access_key"], "fresh");
        assert_eq!(next["password"], "p");
    }

    #[test]
    fn no_secret_reaches_a_response() {
        let source = utopia_core::models::Source {
            id: uuid::Uuid::nil(),
            kb_id: uuid::Uuid::nil(),
            kind: "s3".into(),
            name: "s".into(),
            config: json!({ "bucket": "b", "access_key_id": "AKIA", "secret_access_key": "x",
                            "account_key": "y", "service_account_key": "z", "password": "w",
                            "token": "t", "auth_header": "h" }),
            icon: None,
            sync_interval_minutes: None,
            sync_cron: None,
            last_sync_at: None,
            last_sync_status: "never".into(),
            last_sync_error: None,
            last_sync_added: 0,
            ingest_token: Some("utp_x".into()),
            created_at: chrono::Utc::now(),
        };
        let masked = super::mask_secrets(source);
        let obj = masked.config.as_object().unwrap();
        for key in utopia_core::models::SOURCE_SECRET_KEYS {
            assert!(!obj.contains_key(*key), "{key} leaked");
        }
        assert_eq!(obj["bucket"], "b");
        assert_eq!(
            obj["access_key_id"], "AKIA",
            "an identifier is not a secret"
        );
    }
}
