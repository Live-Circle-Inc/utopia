//! System administration API (system admins only): deployment configuration + creating accounts
//! on someone's behalf.

use axum::extract::{Path, State};
use axum::Json;
use serde::Deserialize;
use serde_json::json;
use utopia_core::models::{Role, User};
use utopia_core::AppError;

use crate::auth::{self, AuthUser};
use crate::error::ApiResult;
use crate::state::AppState;
use uuid::Uuid;

fn require_admin(user: &User) -> Result<(), AppError> {
    if user.is_admin {
        Ok(())
    } else {
        Err(AppError::Forbidden)
    }
}

pub async fn get_deployment(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
) -> ApiResult<Json<serde_json::Value>> {
    require_admin(&user)?;
    let open = utopia_store::access::open_registration(&state.pool).await?;
    let workers = utopia_store::access::worker_concurrency(&state.pool).await?;
    let onto_lang = utopia_store::access::default_ontology_lang(&state.pool).await?;
    let (limits, dflt) = utopia_store::model_limits::list(&state.pool).await?;
    let in_use = utopia_store::model_limits::models_in_use(&state.pool).await?;
    Ok(Json(json!({
        "open_registration": open,
        "worker_concurrency": workers,
        "default_ontology_lang": onto_lang,
        "model_limits": limits,
        "default_model_concurrency": dflt,
        "models_in_use": in_use.into_iter().map(|(b, m, k)| json!({"base_url": b, "model": m, "kind": k})).collect::<Vec<_>>(),
    })))
}

#[derive(Deserialize)]
pub struct DeploymentReq {
    pub open_registration: bool,
    /// Job worker concurrency (1-256): **the outer backstop**, to stop jobs piling up without
    /// bound. The real throttling is the per-model limits, and this value should be clearly
    /// larger than the sum of those limits
    #[serde(default)]
    pub worker_concurrency: Option<i32>,
    /// The concurrency default used by models with no configuration of their own
    #[serde(default)]
    pub default_model_concurrency: Option<i32>,
    /// Concurrency for a single model; `max_concurrent` = null means delete the dedicated
    /// configuration and fall back to the default
    #[serde(default)]
    pub model_limit: Option<ModelLimitReq>,
    /// Which language the ontology is seeded in when a knowledge base is created. **Not the UI
    /// language** -- that one lives in the client
    #[serde(default)]
    pub default_ontology_lang: Option<String>,
}

#[derive(Deserialize)]
pub struct ModelLimitReq {
    pub base_url: String,
    pub model: String,
    pub max_concurrent: Option<i32>,
}

pub async fn put_deployment(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Json(req): Json<DeploymentReq>,
) -> ApiResult<Json<serde_json::Value>> {
    require_admin(&user)?;
    utopia_store::access::set_open_registration(&state.pool, req.open_registration).await?;
    if let Some(n) = req.worker_concurrency {
        utopia_store::access::set_worker_concurrency(&state.pool, n).await?;
        // Takes effect hot: the scheduling loop reads this value every round, no restart needed
        state
            .worker_concurrency
            .store(n as usize, std::sync::atomic::Ordering::Relaxed);
    }
    // The per-model limits take effect immediately: the gate reads the database before every
    // call, and when it finds the limit changed it swaps in a fresh semaphore
    if let Some(l) = &req.default_ontology_lang {
        utopia_store::access::set_default_ontology_lang(&state.pool, l).await?;
    }
    if let Some(n) = req.default_model_concurrency {
        utopia_store::model_limits::set_default(&state.pool, n).await?;
    }
    if let Some(m) = req.model_limit {
        utopia_store::model_limits::set(&state.pool, &m.base_url, &m.model, m.max_concurrent)
            .await?;
    }
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct CreateUserReq {
    pub email: String,
    pub display_name: String,
    pub password: String,
    /// Deployment role: admin | editor | viewer
    #[serde(default)]
    pub role: Option<String>,
}

/// An admin creating an account on someone's behalf (the only way in once registration is
/// closed).
pub async fn create_user(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Json(req): Json<CreateUserReq>,
) -> ApiResult<Json<serde_json::Value>> {
    require_admin(&user)?;
    if !req.email.contains('@') || req.email.len() > 254 {
        return Err(AppError::invalid("bad_email", "Invalid email address").into());
    }
    if req.password.chars().count() < 8 {
        return Err(AppError::invalid(
            "password_too_short",
            "Password must be at least 8 characters",
        )
        .into());
    }
    if req.display_name.trim().is_empty() || req.display_name.chars().count() > 64 {
        return Err(
            AppError::invalid("bad_display_name", "Display name must be 1-64 characters").into(),
        );
    }
    let role = match req.role.as_deref().unwrap_or("editor") {
        "admin" => Role::Admin,
        "editor" => Role::Editor,
        "viewer" => Role::Viewer,
        _ => {
            return Err(AppError::Validation("role must be admin, editor or viewer".into()).into())
        }
    };
    let hash = auth::hash_password(&req.password)?;
    let created = utopia_store::accounts::admin_create_user(
        &state.pool,
        req.email.trim(),
        &hash,
        req.display_name.trim(),
        role,
    )
    .await?;
    Ok(Json(json!({ "user": created })))
}

/// Deactivate an account (a soft delete, see `users.deactivated_at`).
///
/// **Not a DELETE.** The `actor_id` of audit events, merge logs, retype ledgers and definition
/// confirmations all point at this person, and those are audit material -- once the person is
/// gone, "who did it at the time" still has to be answerable. Deactivation only cuts off access:
/// login cannot find the person, an already-issued token cannot find them on its next request
/// either (session validation goes through the same function), they no longer appear in member
/// lists, while every attribution stays as it was.
///
/// Two guardrails sit in the store layer rather than here: you cannot deactivate yourself, and
/// you cannot deactivate the last admin. They sit down there because the UI can block a misclick
/// but cannot block a direct call to the API.
pub async fn deactivate_user(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(target): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    require_admin(&user)?;
    utopia_store::accounts::deactivate_user(&state.pool, target, user.id).await?;
    let _ = utopia_store::audit::record(
        &state.pool,
        None,
        user.id,
        "user.deactivated",
        "user",
        Some(target),
        serde_json::json!({}),
    )
    .await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// Put a deactivated account back.
///
/// **This can fail**: if somebody created a new account with the same email during the
/// deactivation, that partial unique index (`users_email_active_idx`) will block the restore.
/// Letting the admin see the conflict beats quietly letting two active accounts share one email.
pub async fn reactivate_user(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(target): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    require_admin(&user)?;
    utopia_store::accounts::reactivate_user(&state.pool, target).await?;
    let _ = utopia_store::audit::record(
        &state.pool,
        None,
        user.id,
        "user.reactivated",
        "user",
        Some(target),
        serde_json::json!({}),
    )
    .await;
    Ok(Json(serde_json::json!({ "ok": true })))
}
