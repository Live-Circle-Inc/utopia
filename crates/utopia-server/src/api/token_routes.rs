//! Issuing and revoking personal access tokens (0014).
//!
//! **These are account-level routes, not knowledge-base-level** -- a token belongs to a person,
//! and a person can get into several knowledge bases. Which bases it covers is up to the token's
//! own `kb_ids`, and that is a narrowing, not a grant.

use axum::extract::{Path, State};
use axum::Json;
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::error::ApiResult;
use crate::state::AppState;

#[derive(Deserialize)]
pub struct IssueReq {
    pub name: String,
    /// read | write. Read-only by default -- letting an agent write into the ledger has to be
    /// ticked explicitly
    #[serde(default = "default_scope")]
    pub scope: String,
    /// Default = every knowledge base this person can get into
    #[serde(default)]
    pub kb_ids: Option<Vec<Uuid>>,
    /// How many days until it expires. 90 days by default; an explicit 0 means it never expires
    #[serde(default = "default_days")]
    pub expires_in_days: i64,
}
fn default_scope() -> String {
    "read".into()
}
/// 90 days. **Never-expiring is available, but it is not the default** -- for a key configured
/// on somebody else's laptop, forgetting it exists is the normal case
fn default_days() -> i64 {
    90
}

/// Issue one. **The plaintext appears only in this one response**; after that the database holds
/// nothing but the hash.
pub async fn issue(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Json(req): Json<IssueReq>,
) -> ApiResult<Json<serde_json::Value>> {
    let expires_at = (req.expires_in_days > 0)
        .then(|| chrono::Utc::now() + chrono::Duration::days(req.expires_in_days));
    let (view, plain) = utopia_store::tokens::issue(
        &state.pool,
        user.id,
        &req.name,
        &req.scope,
        req.kb_ids.as_deref(),
        expires_at,
    )
    .await?;
    let _ = utopia_store::audit::record(
        &state.pool,
        None,
        user.id,
        "token.issued",
        "personal_token",
        Some(view.id),
        json!({ "name": view.name, "scope": view.scope }),
    )
    .await;
    // the `token` field appears exactly once, right here. The list endpoint can never hand it out
    Ok(Json(json!({ "token": plain, "info": view })))
}

pub async fn list(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
) -> ApiResult<Json<serde_json::Value>> {
    let tokens = utopia_store::tokens::list(&state.pool, user.id).await?;
    Ok(Json(json!({ "tokens": tokens })))
}

pub async fn revoke(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(token_id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    utopia_store::tokens::revoke(&state.pool, user.id, token_id).await?;
    let _ = utopia_store::audit::record(
        &state.pool,
        None,
        user.id,
        "token.revoked",
        "personal_token",
        Some(token_id),
        json!({}),
    )
    .await;
    Ok(Json(json!({ "ok": true })))
}
