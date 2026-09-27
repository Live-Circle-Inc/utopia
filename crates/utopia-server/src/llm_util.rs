//! Building LLM clients from the workspace settings, plus the per-model concurrency gates.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use utopia_core::models::LlmSettings;
use utopia_llm::LlmClient;

use crate::state::AppState;

pub fn chat_client(s: &LlmSettings) -> Option<LlmClient> {
    if !s.chat_ready() {
        return None;
    }
    Some(LlmClient::new(
        s.chat_base_url.as_deref()?,
        s.chat_api_key.as_deref(),
        s.chat_model.as_deref()?,
    ))
}

/// The client extraction uses. **Blank means the chat one** -- the fallback lives
/// entirely in [`LlmSettings::effective_extract`], and both this and
/// `acquire_extract` read only that, so the (base_url, model) the gate counts
/// permits for is necessarily the one requests actually go to. If each resolved
/// the fallback separately, a half-configured deployment would have the gate
/// counting permits for a different model than the one being called.
pub fn extract_client(s: &LlmSettings) -> Option<LlmClient> {
    let (base, key, model) = s.effective_extract();
    Some(LlmClient::new(base?, key, model?))
}

pub fn embed_client(s: &LlmSettings) -> Option<LlmClient> {
    if !s.embed_ready() {
        return None;
    }
    Some(LlmClient::new(
        s.embed_base_url.as_deref()?,
        s.embed_api_key.as_deref(),
        s.embed_model.as_deref()?,
    ))
}

/// A registry of per-model semaphores. When the limit changes we swap in a fresh one -- the
/// in-flight permits on the old one run themselves out, and for the instant of the swap we
/// may briefly exceed the new limit, which is acceptable; what it buys us is "an edit takes
/// effect immediately" with no cache invalidation to write, and no wrestling with the fact
/// that a tokio Semaphore cannot be shrunk.
#[derive(Default)]
pub struct ModelGates {
    inner: std::sync::Mutex<HashMap<String, (usize, Arc<Semaphore>)>>,
}

impl ModelGates {
    fn gate(&self, key: &str, limit: usize) -> Arc<Semaphore> {
        let mut m = self.inner.lock().unwrap();
        match m.get(key) {
            Some((n, sem)) if *n == limit => sem.clone(),
            _ => {
                let sem = Arc::new(Semaphore::new(limit));
                m.insert(key.to_string(), (limit, sem.clone()));
                sem
            }
        }
    }
}

/// A background job takes one permit before calling a model and holds it until the call is
/// done.
///
/// **For background jobs only** (extraction, adjudication, ingest embedding, ontology
/// suggestions). User chat and search do not come through here -- making someone who is
/// typing wait behind ten background extractions is what makes a product bad; and the thing
/// that actually blows through a provider's rate limit was never one person typing.
///
/// When the limit cannot be read (table not created yet, database briefly unreachable) we
/// **let it through**: the concurrency limit is a safeguard, and it has no business wedging
/// the entire pipeline just because its configuration could not be read.
pub async fn acquire(
    state: &AppState,
    base_url: &str,
    model: &str,
) -> Option<OwnedSemaphorePermit> {
    let limit = utopia_store::model_limits::limit_for(&state.pool, base_url, model)
        .await
        .ok()?;
    let key = format!("{base_url}|{model}");
    state
        .model_gates
        .gate(&key, limit)
        .acquire_owned()
        .await
        .ok()
}

/// A convenience form of `acquire`: takes the chat model's identity straight from the
/// workspace settings.
pub async fn acquire_chat(state: &AppState, s: &LlmSettings) -> Option<OwnedSemaphorePermit> {
    let (base, model) = (s.chat_base_url.as_deref()?, s.chat_model.as_deref()?);
    acquire(state, base, model).await
}

/// Convenience form of `acquire`: the extraction model. Identity comes from
/// `effective_extract`, the same source as `extract_client` -- once extraction
/// moves to a cheaper model it counts permits against its own (base_url, model)
/// instead of competing for chat's gate
pub async fn acquire_extract(state: &AppState, s: &LlmSettings) -> Option<OwnedSemaphorePermit> {
    let (base, _, model) = s.effective_extract();
    acquire(state, base?, model?).await
}

/// A convenience form of `acquire`: the embedding model.
pub async fn acquire_embed(state: &AppState, s: &LlmSettings) -> Option<OwnedSemaphorePermit> {
    let (base, model) = (s.embed_base_url.as_deref()?, s.embed_model.as_deref()?);
    acquire(state, base, model).await
}
