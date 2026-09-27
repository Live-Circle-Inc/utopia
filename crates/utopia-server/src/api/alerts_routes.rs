//! Alert centre (0005). **Cross-base**: one panel in the top bar, not hung under some KB.
//!
//! Visibility is not decided here -- it is decided in those few SQL statements in
//! `utopia_store::alerts`, and decided exactly once. The routing layer is only responsible for
//! getting hold of the current user.

use axum::extract::{Query, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::Json;
use chrono::{DateTime, Utc};
use futures_util::Stream;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::convert::Infallible;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::error::ApiResult;
use crate::state::AppState;

/// How many groups per page. As many as fit in the popover -- any more and it should be paged,
/// rather than making someone scroll a screenful.
const PAGE: i64 = 8;
/// The most a page may ask for: this guards against someone writing limit as 100000 and making
/// the server count the whole table
const MAX_PAGE: i64 = 50;

#[derive(Deserialize)]
pub struct ListQuery {
    /// Searches base names, object details and the kind code. **It cannot find the headline you
    /// see in the UI** -- that wording lives in the client and the server does not have it (see
    /// the comment on SEARCH in the store)
    #[serde(default)]
    pub q: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub offset: Option<i64>,
}

/// One group of consecutive failures of the same kind. The collapsing happens on **read**; on
/// the storage side it is still one row per failure.
#[derive(Serialize)]
pub struct GroupView {
    pub kb_id: Option<Uuid>,
    pub kb_name: Option<String>,
    pub kind: String,
    pub severity: String,
    pub count: i64,
    pub unread: i64,
    pub latest_at: DateTime<Utc>,
    /// Together with `latest_at` this fences off the group; send it back unchanged when marking
    /// as read
    pub earliest_at: DateTime<Utc>,
    /// The detail lines, a few at most, newest first
    pub lines: Vec<serde_json::Value>,
}

#[derive(Serialize)]
pub struct ListResponse {
    pub items: Vec<GroupView>,
    /// The total number of **groups** -- what the pager counts is groups, not rows
    pub total: i64,
}

pub async fn list(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<ListResponse>> {
    let limit = q.limit.unwrap_or(PAGE).clamp(1, MAX_PAGE);
    let offset = q.offset.unwrap_or(0).max(0);
    let page = utopia_store::alerts::list_groups(&state.pool, &user, q.q.as_deref(), limit, offset)
        .await?;
    Ok(Json(ListResponse {
        items: page
            .items
            .into_iter()
            .map(|g| GroupView {
                kb_id: g.kb_id,
                kb_name: g.kb_name,
                kind: g.kind,
                severity: g.severity,
                count: g.count,
                unread: g.unread,
                latest_at: g.latest_at,
                earliest_at: g.earliest_at,
                lines: g.lines,
            })
            .collect(),
        total: page.total,
    }))
}

/// The badge. A route of its own rather than counting it from the list: this one is needed on
/// every page load.
pub async fn unread(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
) -> ApiResult<Json<serde_json::Value>> {
    let n = utopia_store::alerts::unread_count(&state.pool, &user).await?;
    Ok(Json(json!({ "unread": n })))
}

#[derive(Deserialize)]
pub struct ReadGroupBody {
    pub kb_id: Option<Uuid>,
    pub kind: String,
    /// The group's time range, exactly as returned by the list in `earliest_at` / `latest_at`
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
}

/// Marks a whole group as read. **Per person** -- having read it does not mean the problem is
/// gone, and other people's unread counts are unaffected.
///
/// The group is fenced off by time range rather than by sending a list of ids: a group may hold
/// hundreds of rows. Visibility is still checked in the store, so guessing a kind cannot mark
/// off anything you cannot see yourself.
pub async fn mark_group_read(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Json(b): Json<ReadGroupBody>,
) -> ApiResult<Json<serde_json::Value>> {
    let n =
        utopia_store::alerts::mark_group_read(&state.pool, &user, b.kb_id, &b.kind, b.from, b.to)
            .await?;
    Ok(Json(json!({ "marked": n })))
}

/// Marks everything as read.
pub async fn mark_all_read(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
) -> ApiResult<Json<serde_json::Value>> {
    let n = utopia_store::alerts::mark_all_read(&state.pool, &user).await?;
    Ok(Json(json!({ "marked": n })))
}

/// The global event stream. The KB one is `/kbs/{id}/events`, filtered by base; the badge is
/// cross-base, so it cannot hang off that.
///
/// **No permission filtering happens here**: the events carry no data, everyone who receives one
/// goes back and re-fetches the list, and the query behind the list blocks whatever they should
/// not see. The price is that people without permission get woken up once too; what it buys is
/// not a single line of permission logic anywhere on the push path -- so "the push decides more
/// loosely than the list" cannot happen.
pub async fn stream(
    State(state): State<AppState>,
    AuthUser(_user): AuthUser,
) -> ApiResult<Sse<impl Stream<Item = Result<Event, Infallible>>>> {
    let mut rx = state.events.subscribe();
    let stream = async_stream::stream! {
        loop {
            match rx.recv().await {
                Ok(ev) if ev.kind == "alert" => {
                    yield Ok(Event::default().event("alert").data("{}"));
                }
                Ok(_) => continue,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return,
            }
        }
    };
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}
