//! The vector index over the ontology: embed the `label + description` of classes, relations
//! and attributes into vectors.
//!
//! What it serves is not one single feature but every place that has to ask "which thing in the
//! ontology does this phrasing correspond to": ontology proposals (which today inline all 1949
//! keys, and only the keys, with no descriptions), predicate resolution, type resolution. Only
//! once candidates have been retrieved and handed to the model to adjudicate does the prompt
//! become decoupled from the size of the ontology.
//!
//! **Self-healing, no hooks.** Filling in is decided by "do the text that was embedded at the
//! time and the model name still match what we have now", so no write site that changes a
//! description needs to remember to notify this module -- forgetting to wire up a hook rots
//! silently, whereas comparing the original text does not.

use crate::{llm_util, AppState};
use futures_util::{stream, StreamExt};
use utopia_store::ontology::TypeToEmbed;
use uuid::Uuid;

/// How many items get sent for embedding at a time.
///
/// **64 is the number that holds up under measurement.** It was once changed to 16, on the
/// grounds of a "faster per item" measured with synthetic short texts -- but real class texts
/// average 151 characters, so that set of numbers does not count. After shrinking it the request
/// count quadrupled, each one paying about 2.5 seconds of fixed overhead, and overall throughput
/// dropped by half instead.
///
/// Before changing the batch size, measure with the **real `embedded_text`**, not with
/// made-up short sentences.
const BATCH: usize = 64;
/// Ceiling on how many embedding batches are in flight at once.
///
/// **The point is not speed, it is not jamming the gate shut.** This is a background backfill
/// job, and the embedding model's concurrency gate (`model_concurrency`, default 10) is shared
/// globally -- chunking documents into the store goes through it too.
///
/// This used to be serial: one permit at a time, released between batches, so document
/// processing could squeeze in. After it was changed to hang 31 batches off a single `join_all`,
/// it turned into a permanently resident 10-deep queue with every batch holding a permit for 60
/// seconds: **5 documents all stuck in the `embedding` state, 15 minutes of extraction with zero
/// progress**. The API was not down; this job had filled the shared resource up.
///
/// So the ceiling here has to be **clearly smaller** than the gate, to leave room for other
/// work. 4 is the number measured to saturate throughput while still leaving slack: 4 batches of
/// 16 concurrently, 18.5 seconds wall clock (serial takes 50).
const EMBED_JOBS: usize = 4;

/// Backfill the stale ontology vectors in this KB. With no embedding model configured it just
/// returns `Ok(0)` -- retrieval is an enhancement, and should not block a deployment that has
/// no model configured.
pub async fn refresh(state: &AppState, kb_id: Uuid) -> anyhow::Result<usize> {
    refresh_scoped(state, kb_id, None).await
}

/// Backfill only one half. For callers that know which half they need -- type resolution only
/// uses classes, and waiting for the relations to finish embedding is waiting for nothing. The
/// half left behind is covered by the background job.
pub async fn refresh_scoped(
    state: &AppState,
    kb_id: Uuid,
    only: Option<utopia_store::ontology::TypeKind>,
) -> anyhow::Result<usize> {
    // The KB may already have been deleted -- the job was enqueued at import time, minutes
    // earlier. This is not a failure, there is simply nothing to do; treated as an error it
    // would retry three times before giving up
    let Ok(kb) = utopia_store::kbs::get(&state.pool, kb_id).await else {
        return Ok(0);
    };
    let Some(settings) = utopia_store::settings::get(&state.pool, kb.workspace_id).await? else {
        return Ok(0);
    };
    let (Some(client), Some(model)) = (
        llm_util::embed_client(&settings),
        // Take an owned copy: the futures below move settings into an Arc, and a borrow would
        // pin it in place
        settings.embed_model.clone(),
    ) else {
        return Ok(0);
    };

    let stale =
        utopia_store::ontology::types_needing_embedding(&state.pool, kb_id, &model, only).await?;
    if stale.is_empty() {
        return Ok(0);
    }
    let mut done = 0usize;
    let mut failed = 0usize;
    // **Owned data throughout**: these futures get driven concurrently across awaits, and
    // borrows do not make it past the Send bound
    let client = std::sync::Arc::new(client);
    let model = std::sync::Arc::new(model);
    let settings = std::sync::Arc::new(settings);
    let batches: Vec<Vec<TypeToEmbed>> = stale.chunks(BATCH).map(<[_]>::to_vec).collect();
    let mut jobs = stream::iter(batches.into_iter().map(|batch| {
        let (client, model, settings) = (client.clone(), model.clone(), settings.clone());
        async move {
            let texts: Vec<String> = batch.iter().map(|t| t.text.clone()).collect();
            let _permit = llm_util::acquire_embed(state, &settings).await;
            let vectors = client.embed(&texts).await?;
            // A count mismatch means abandoning the whole batch: pairing is by position, and
            // being off by one writes person's vector onto organization -- and once a mistake
            // like that lands in the database it can never be spotted again
            if vectors.len() != batch.len() {
                anyhow::bail!("embedding returned {}, sent {}", vectors.len(), batch.len());
            }
            let n = batch.len();
            let items: Vec<(TypeToEmbed, Vec<f32>)> = batch.into_iter().zip(vectors).collect();
            utopia_store::ontology::set_type_embeddings(&state.pool, &model, &items).await?;
            Ok::<usize, anyhow::Error>(n)
        }
    }))
    .buffer_unordered(EMBED_JOBS);
    // **One failed batch must not drag the rest down**: this is a backfill job, and whatever is
    // missing heals itself next round (the criterion is "does the text embedded at the time
    // still match", not a timestamp). Bailing out wholesale wastes the ones already embedded
    while let Some(r) = jobs.next().await {
        match r {
            Ok(n) => done += n,
            Err(e) => {
                failed += 1;
                tracing::warn!(
                    %kb_id,
                    error = %e,
                    "a batch of ontology vectors did not embed, left for the next round"
                );
            }
        }
    }
    // **The failure count has to appear on this line.** It used to report only how many were
    // backfilled, so the time all 24 batches failed the log still said "backfilled", and whoever
    // read it assumed everything was fine
    if failed > 0 {
        tracing::warn!(
            %kb_id,
            count = done,
            failed,
            "ontology vectors partly backfilled, the rest left for the next round"
        );
    } else {
        tracing::info!(%kb_id, count = done, "ontology vectors backfilled");
    }
    Ok(done)
}

/// For a batch of phrasings, which relations/attributes in the ontology each one looks most like.
///
/// **Embed them all in one go and then query the database one by one**, rather than sending one
/// embedding request per phrasing: one round of ontology proposals handles a dozen to a few
/// dozen phrasings, and one request each means a dozen to a few dozen round trips.
pub async fn nearest_for_each(
    state: &AppState,
    kb_id: Uuid,
    queries: &[String],
    limit: i64,
    target: Target,
) -> anyhow::Result<Vec<Vec<utopia_core::models::TypeCandidate>>> {
    let empty = || vec![Vec::new(); queries.len()];
    if queries.is_empty() {
        return Ok(Vec::new());
    }
    let kb = utopia_store::kbs::get(&state.pool, kb_id).await?;
    let Some(settings) = utopia_store::settings::get(&state.pool, kb.workspace_id).await? else {
        return Ok(empty());
    };
    let Some(client) = llm_util::embed_client(&settings) else {
        return Ok(empty());
    };
    let mut vectors = Vec::with_capacity(queries.len());
    for batch in queries.chunks(BATCH) {
        let _permit = llm_util::acquire_embed(state, &settings).await;
        let got = client.embed(batch).await?;
        if got.len() != batch.len() {
            anyhow::bail!("embedding returned {}, sent {}", got.len(), batch.len());
        }
        vectors.extend(got);
    }
    let mut out = Vec::with_capacity(vectors.len());
    for v in &vectors {
        out.push(match target {
            Target::Class => {
                utopia_store::ontology::nearest_entity_types(&state.pool, kb_id, v, limit, false)
                    .await?
            }
            Target::ClassLabel => {
                utopia_store::ontology::nearest_entity_types(&state.pool, kb_id, v, limit, true)
                    .await?
            }
            Target::Predicate(kind) => {
                utopia_store::ontology::nearest_relation_types(&state.pool, kb_id, v, limit, kind)
                    .await?
            }
        });
    }
    Ok(out)
}

/// Which half of the ontology is being searched.
///
/// **They have to stay separate.** Classes go into `entity_types`, relations and attributes into
/// `relation_types`, and the descriptions on the two sides say completely different things
/// ("what kind of entity belongs here" vs "what this assertion states"). Search the relations
/// with a class name that is not in the vocabulary and what comes back is guaranteed to be a
/// barely related relation.
#[derive(Debug, Clone, Copy)]
pub enum Target {
    /// The whole-passage index (label + description). Long profiles go down this path
    Class,
    /// **The label-only index** (see `entity_types.label_embedding`). Short phrasings
    /// (`district. place`) go down this path. With the whole-passage index, short phrasings get
    /// taken over by tautological classes -- the description on the `Map` row is literally
    /// "A map.", which wins on length rather than on meaning
    ClassLabel,
    /// `Some("attribute")` searches attributes only, `Some("relation")` relations only, `None`
    /// both. A fact with a literal object wants an attribute, one with an entity object wants a
    /// relation
    Predicate(Option<&'static str>),
}
