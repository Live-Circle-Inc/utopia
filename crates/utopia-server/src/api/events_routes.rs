//! KB event stream (SSE): real-time push of document ingest/extraction status and review
//! queue changes.
//! When the frontend receives an event it only invalidates and refetches react-query -- the
//! event itself carries no business data, so it is idempotent by construction.

use axum::extract::{Path, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::Stream;
use std::convert::Infallible;
use tokio::sync::broadcast;
use utopia_core::models::Role;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::error::ApiResult;
use crate::state::AppState;

pub async fn kb_events(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(kb_id): Path<Uuid>,
) -> ApiResult<Sse<impl Stream<Item = Result<Event, Infallible>>>> {
    utopia_store::access::require_kb(&state.pool, &user, kb_id, Role::Viewer).await?;

    let mut rx = state.events.subscribe();
    let stream = async_stream::stream! {
        loop {
            match rx.recv().await {
                Ok(ev) if ev.kb_id == Some(kb_id) => {
                    yield Ok(Event::default()
                        .event(ev.kind)
                        .data(serde_json::to_string(&ev).unwrap_or_else(|_| "{}".into())));
                }
                Ok(_) => continue,
                // Consumer fell behind and frames were dropped: doesn't matter, an event is
                // only a "time to refresh" signal
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return,
            }
        }
    };
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}
