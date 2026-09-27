use axum::extract::{Path, State};
use axum::Json;
use serde::Deserialize;
use serde_json::json;
use utopia_core::models::Role;
use utopia_llm::ChatMessage;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::error::ApiResult;
use crate::llm_util;
use crate::state::AppState;

/// GET: the redacted view (for keys, only whether they are configured is returned).
pub async fn get(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(workspace_id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    utopia_store::workspaces::require_role(&state.pool, user.id, workspace_id, Role::Admin).await?;
    let s = utopia_store::settings::get(&state.pool, workspace_id).await?;
    Ok(Json(match s {
        None => json!({}),
        Some(s) => json!({
            "chat_base_url": s.chat_base_url,
            "chat_model": s.chat_model,
            "has_chat_key": s.chat_api_key.as_deref().is_some_and(|k| !k.is_empty()),
            "extract_base_url": s.extract_base_url,
            "extract_model": s.extract_model,
            "has_extract_key": s.extract_api_key.as_deref().is_some_and(|k| !k.is_empty()),
            "embed_base_url": s.embed_base_url,
            "embed_model": s.embed_model,
            "embed_dim": s.embed_dim,
            "has_embed_key": s.embed_api_key.as_deref().is_some_and(|k| !k.is_empty()),
        }),
    }))
}

#[derive(Deserialize)]
pub struct PutSettingsReq {
    pub chat_base_url: Option<String>,
    /// None or an empty string = keep the old key
    pub chat_api_key: Option<String>,
    pub chat_model: Option<String>,
    /// Blank = extraction follows chat
    pub extract_base_url: Option<String>,
    pub extract_api_key: Option<String>,
    pub extract_model: Option<String>,
    pub embed_base_url: Option<String>,
    pub embed_api_key: Option<String>,
    pub embed_model: Option<String>,
    pub embed_dim: Option<i32>,
}

pub async fn put(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(workspace_id): Path<Uuid>,
    Json(req): Json<PutSettingsReq>,
) -> ApiResult<Json<serde_json::Value>> {
    utopia_store::workspaces::require_role(&state.pool, user.id, workspace_id, Role::Admin).await?;
    let nonempty = |v: &Option<String>| -> Option<String> {
        v.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
    };
    use utopia_store::settings::{Endpoint, LlmSettingsInput};
    utopia_store::settings::upsert(
        &state.pool,
        workspace_id,
        LlmSettingsInput {
            chat: Endpoint {
                base_url: nonempty(&req.chat_base_url).as_deref(),
                api_key: nonempty(&req.chat_api_key).as_deref(),
                model: nonempty(&req.chat_model).as_deref(),
            },
            extract: Endpoint {
                base_url: nonempty(&req.extract_base_url).as_deref(),
                api_key: nonempty(&req.extract_api_key).as_deref(),
                model: nonempty(&req.extract_model).as_deref(),
            },
            embed: Endpoint {
                base_url: nonempty(&req.embed_base_url).as_deref(),
                api_key: nonempty(&req.embed_api_key).as_deref(),
                model: nonempty(&req.embed_model).as_deref(),
            },
            embed_dim: req.embed_dim,
        },
    )
    .await?;
    Ok(Json(json!({ "ok": true })))
}

/// One minimal chat probe. Shared by chat and extraction -- both ask the same
/// question ("can this address plus this key produce a completion")
async fn probe_chat(client: &utopia_llm::LlmClient) -> serde_json::Value {
    let msg = [ChatMessage {
        role: "user".into(),
        content: "Reply with exactly one word: OK".into(),
    }];
    match client.chat(&msg).await {
        Ok(reply) => json!({ "ok": true, "reply": reply.chars().take(50).collect::<String>() }),
        Err(e) => json!({ "ok": false, "error": e.to_string() }),
    }
}

/// Connectivity test: send chat one minimal message; compute one embedding and
/// return its dimension.
///
/// **Extraction is tested only when it has its own endpoint.** When unset it *is*
/// the chat one, so sending a second request just spends two shares of the same
/// endpoint's quota -- and the moment someone clicks this button is often exactly
/// the moment that endpoint is already rate limiting. Returning `null` lets the UI
/// know the row does not apply, which is distinct from "tested and failed"
pub async fn test(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(workspace_id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    utopia_store::workspaces::require_role(&state.pool, user.id, workspace_id, Role::Admin).await?;
    let Some(s) = utopia_store::settings::get(&state.pool, workspace_id).await? else {
        return Ok(Json(
            json!({ "chat": { "ok": false, "error": "Not configured" },
                               "extract": null,
                               "embed": { "ok": false, "error": "Not configured" } }),
        ));
    };

    let chat_result = match llm_util::chat_client(&s) {
        None => json!({ "ok": false, "error": "Not configured" }),
        Some(client) => probe_chat(&client).await,
    };

    let extract_result = match s.extract_overridden() {
        false => serde_json::Value::Null,
        true => match llm_util::extract_client(&s) {
            None => json!({ "ok": false, "error": "Not configured" }),
            Some(client) => probe_chat(&client).await,
        },
    };

    let embed_result = match llm_util::embed_client(&s) {
        None => json!({ "ok": false, "error": "Not configured" }),
        Some(client) => match client.embed(&["connectivity test".to_string()]).await {
            Ok(v) if !v.is_empty() => json!({ "ok": true, "dim": v[0].len() }),
            Ok(_) => json!({ "ok": false, "error": "Empty response" }),
            Err(e) => json!({ "ok": false, "error": e.to_string() }),
        },
    };

    Ok(Json(json!({
        "chat": chat_result,
        "extract": extract_result,
        "embed": embed_result
    })))
}
