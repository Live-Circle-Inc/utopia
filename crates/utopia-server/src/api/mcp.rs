//! MCP server (Streamable HTTP).
//!
//! One route: `POST /api/v1/kbs/{kb_id}/mcp`, which takes JSON-RPC 2.0.
//!
//! **Streamable HTTP as the transport, not stdio**; the reasoning is in
//! `docs/decisions/0014`: Utopia is already a server, so stdio would mean either
//! spawning a child process that connects back to it, or standing up a second
//! connection pool; and this is a multi-person deployment -- five people each
//! connecting on their own should be served by the one deployment.
//!
//! **Every POST authenticates again.** The spec allows carrying the result of a
//! single handshake across the whole connection, and the discipline in 0014 says
//! it very plainly:
//!
//! > Scope is checked at every tool entry point, not at the handshake. When
//! > `revoked_at` gets written mid-flight it must take effect immediately.
//!
//! Once this is stateless that property comes for free -- there is no such thing
//! as a "connection" here that could be trusted.
//!
//! **Responses use `application/json` rather than SSE.** The spec allows both, and
//! these tools are one question, one answer, with nothing the server pushes on its
//! own; SSE is for notifications, which this version does not need.

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::Json;
use serde_json::{json, Value};
use utopia_core::models::Role;
use uuid::Uuid;

use super::tools::{self, ToolCtx, ToolSink};
use crate::error::ApiResult;
use crate::state::AppState;

/// The protocol version we implement. When a client announces a different one we
/// still answer with this -- the spec requires the server to reply with the version
/// it supports and leaves it to the client to decide whether to accept it
const PROTOCOL_VERSION: &str = "2025-06-18";

/// The tools exposed in this version. **Five read-only ones** (0014):
///
/// `query_data` runs SQL against the production database and `remember` writes into
/// the ledger; each still has unanswered questions -- what evidence a fact written
/// by an external agent hangs off, and how a SQL run gets audited. Get the identity
/// path working first.
const EXPOSED: [&str; 5] = [
    "search_chunks",
    "get_document",
    "search_docs",
    "find_entities",
    "changes",
];
/// `entity_facts` is in there too; it sits on its own only because the array above
/// has to have a fixed length
const EXPOSED_EXTRA: &str = "entity_facts";

fn is_exposed(name: &str) -> bool {
    EXPOSED.contains(&name) || name == EXPOSED_EXTRA
}

/// Authentication + authorization. **Two gates, not one.**
///
/// The token says "who this is, and which KBs this key can reach"; `require_kb`
/// says "what role this person has in this KB". The former only narrows, the latter
/// is the actual permission -- a token with every scope open, landing in the hands
/// of a viewer, is still nothing more than a viewer.
async fn authorize(
    state: &AppState,
    headers: &HeaderMap,
    kb_id: Uuid,
) -> Result<
    (
        utopia_core::models::User,
        utopia_store::tokens::Authenticated,
    ),
    utopia_core::AppError,
> {
    let raw = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or(utopia_core::AppError::Unauthorized)?;
    let auth = utopia_store::tokens::authenticate(&state.pool, raw.trim()).await?;
    // The token's reach: one that was pinned to certain KBs cannot get to the others
    if !auth.covers(kb_id) {
        return Err(utopia_core::AppError::Forbidden);
    }
    // The person: deactivation takes effect immediately (find_user_by_id blocks it)
    let user = utopia_store::accounts::find_user_by_id(&state.pool, auth.user_id)
        .await?
        .ok_or(utopia_core::AppError::Unauthorized)?;
    // The role: the same guard the web side goes through, not a line changed
    utopia_store::access::require_kb(&state.pool, &user, kb_id, Role::Viewer).await?;
    Ok((user, auth))
}

/// OpenAI shape → MCP shape.
///
/// **Shares the one definition in `chat.rs`** instead of writing a second set here.
/// The names and the parameter schema are the contract with the execution side in
/// `tools.rs`, and copied into two places they will drift sooner or later -- which
/// is exactly what pulling the tools out in the previous step was meant to avoid.
///
/// A known blemish: the descriptions were written for the in-app assistant, and the
/// `search_chunks` one still mentions "can be cited as [n]", while an MCP client
/// never gets citation numbers. Sharing one copy is worth more than that sentence
/// costs; we can revisit it when they genuinely have to be split.
fn to_mcp_tools(openai: &Value) -> Vec<Value> {
    openai
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|t| {
                    let f = t.get("function")?;
                    let name = f.get("name")?.as_str()?;
                    if !is_exposed(name) {
                        return None;
                    }
                    Some(json!({
                        "name": name,
                        "description": f.get("description").and_then(|d| d.as_str()).unwrap_or(""),
                        "inputSchema": f.get("parameters").cloned().unwrap_or(json!({
                            "type": "object", "properties": {}
                        })),
                    }))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn ok(id: Option<Value>, result: Value) -> Json<Value> {
    Json(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

/// A JSON-RPC error is not an HTTP error: **the transport succeeded, the method
/// failed**. Answer 200 with an error body, or the client cannot parse it.
fn rpc_err(id: Option<Value>, code: i64, message: &str) -> Json<Value> {
    Json(json!({
        "jsonrpc": "2.0", "id": id,
        "error": { "code": code, "message": message }
    }))
}

pub async fn handle(
    State(state): State<AppState>,
    Path(kb_id): Path<Uuid>,
    headers: HeaderMap,
    Json(req): Json<Value>,
) -> ApiResult<Json<Value>> {
    let (user, auth) = authorize(&state, &headers, kb_id).await?;
    let id = req.get("id").cloned();
    let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let params = req.get("params").cloned().unwrap_or(json!({}));

    Ok(match method {
        "initialize" => ok(
            id,
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": "utopia", "version": env!("CARGO_PKG_VERSION") },
            }),
        ),
        // A notification has no id, and per the spec should get no response body;
        // but the HTTP side has to return something. A 202 would be semantically more
        // accurate -- we return an empty result here to keep one handler signature
        "notifications/initialized" | "notifications/cancelled" => ok(None, json!({})),
        "ping" => ok(id, json!({})),
        "tools/list" => ok(
            id,
            json!({ "tools": to_mcp_tools(&super::chat::base_tools()) }),
        ),
        "tools/call" => {
            let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            if !is_exposed(name) {
                // For a tool we do not expose (query_data / remember), say clearly that
                // it is "not shipped in this version" rather than "no such tool" -- with
                // the former the client will not keep retrying
                return Ok(rpc_err(
                    id,
                    -32601,
                    &format!("Tool '{name}' is not exposed over MCP in this version"),
                ));
            }
            let kb = utopia_store::kbs::get(&state.pool, kb_id).await?;
            // This version exposes read-only tools only, so mounted_sources is empty
            // and can_write is false: **not opened up even if the token scope is write**
            // -- scope is a ceiling, not a grant, and this version's ceiling is set by
            // EXPOSED
            let ctx = ToolCtx {
                state: &state,
                kb_id,
                workspace_id: kb.workspace_id,
                mounted_sources: &[],
                can_write: false,
                actor: Some(user.id),
            };
            let mut sink = ToolSink::default();
            let (text, _step) = tools::dispatch(&ctx, &mut sink, name, &args).await;
            let _ = utopia_store::audit::record(
                &state.pool,
                Some(kb_id),
                user.id,
                "mcp.tool_called",
                "personal_token",
                Some(auth.token_id),
                json!({ "tool": name }),
            )
            .await;
            ok(
                id,
                json!({
                    "content": [{ "type": "text", "text": text }],
                    "isError": false,
                }),
            )
        }
        other => rpc_err(id, -32601, &format!("Unknown method: {other}")),
    })
}
