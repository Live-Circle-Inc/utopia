//! Batched adjudication job for entity resolution: consumes the grey-area pairs in the
//! review queue whose stage=adjudicating.
//! The verdict cache is checked first, and the cache misses are gathered into one batch (a
//! single LLM call judges many pairs);
//! high-confidence same → auto-merge (revertible), high-confidence different → automatically
//! kept apart, everything else goes to a human. With no model configured everything goes to
//! a human -- this job failing or being absent affects neither extraction nor queries.

use crate::llm_util;
use crate::state::AppState;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use utopia_core::models::ReviewItem;
use utopia_core::AppError;
use uuid::Uuid;

const BATCH_SIZE: i64 = 12;
const AUTO_CONF: f32 = 0.8;
const MAX_ROUNDS: usize = 20;

/// Cache key: type + both sides' names + a digest of the facts (nothing to do with entity
/// ids -- re-uploading a document does not mean paying twice).
fn pair_key(item: &ReviewItem) -> String {
    let side = |s: &utopia_core::models::ReviewSide| {
        format!(
            "{}|{}|{}",
            s.name.to_lowercase(),
            // A side whose type could not be determined still has to be cacheable (0009)
            s.type_label.as_deref().unwrap_or("untyped"),
            s.top_facts.join(";")
        )
    };
    let mut sides = [side(&item.left), side(&item.right)];
    sides.sort();
    let digest = Sha256::digest(sides.join("##").as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

pub async fn adjudicate_entities(state: &AppState, kb_id: Uuid) -> anyhow::Result<()> {
    let kb = utopia_store::kbs::get(&state.pool, kb_id).await?;
    let settings = utopia_store::settings::get(&state.pool, kb.workspace_id).await?;
    let client = settings.as_ref().and_then(llm_util::chat_client);
    let model = settings
        .as_ref()
        .and_then(|s| s.chat_model.clone())
        .unwrap_or_default();

    let Some(client) = client else {
        // No model available: send everything to a human; the job itself finishes
        // successfully
        let items =
            utopia_store::resolution::pending_adjudications(&state.pool, kb_id, 500).await?;
        for item in items {
            utopia_store::resolution::escalate_review(&state.pool, item.id, "escalate_no_model")
                .await?;
        }
        state.emit_review(kb_id);
        return Ok(());
    };

    for _ in 0..MAX_ROUNDS {
        let items =
            utopia_store::resolution::pending_adjudications(&state.pool, kb_id, BATCH_SIZE).await?;
        if items.is_empty() {
            break;
        }

        // First layer: the verdict cache
        let mut to_ask: Vec<(ReviewItem, String)> = Vec::new();
        for item in items {
            let key = pair_key(&item);
            match utopia_store::resolution::get_verdict(&state.pool, kb_id, &key).await? {
                Some((same, conf)) => {
                    apply_verdict(state, kb_id, &item, same, conf, "cached").await?
                }
                None => to_ask.push((item, key)),
            }
        }
        if to_ask.is_empty() {
            continue;
        }

        // Second layer: batched LLM adjudication
        let pairs: Vec<utopia_extract::AdjudicationPair> = to_ask
            .iter()
            .map(|(item, _)| utopia_extract::AdjudicationPair {
                left: utopia_extract::AdjudicationSide {
                    name: item.left.name.clone(),
                    type_label: item
                        .left
                        .type_label
                        .clone()
                        .unwrap_or_else(|| "untyped".into()),
                    facts: item.left.top_facts.clone(),
                },
                right: utopia_extract::AdjudicationSide {
                    name: item.right.name.clone(),
                    type_label: item
                        .right
                        .type_label
                        .clone()
                        .unwrap_or_else(|| "untyped".into()),
                    facts: item.right.top_facts.clone(),
                },
            })
            .collect();
        let messages = utopia_extract::build_adjudication_messages(&pairs);
        // A failed call or a failed parse → the job retries with backoff; once the retries
        // are used up the row just stays in the queue, and a human can still settle it
        let _permit = settings.as_ref().map(|s| llm_util::acquire_chat(state, s));
        let _permit = match _permit {
            Some(f) => f.await,
            None => None,
        };
        let reply = client.chat(&messages).await?;
        let verdicts = utopia_extract::parse_adjudication(&reply)?;
        let by_i: HashMap<usize, &utopia_extract::AdjudicationVerdict> =
            verdicts.iter().map(|v| (v.i, v)).collect();

        for (idx, (item, key)) in to_ask.iter().enumerate() {
            match by_i.get(&idx) {
                Some(v) => {
                    let same = match v.verdict.as_str() {
                        "same" => Some(true),
                        "different" => Some(false),
                        _ => None,
                    };
                    let conf = v.confidence.unwrap_or(0.5).clamp(0.0, 1.0);
                    utopia_store::resolution::put_verdict(
                        &state.pool,
                        kb_id,
                        key,
                        same,
                        conf,
                        &model,
                    )
                    .await?;
                    apply_verdict(state, kb_id, item, same, conf, "adjudicated").await?;
                }
                None => {
                    utopia_store::resolution::escalate_review(
                        &state.pool,
                        item.id,
                        "escalate_no_verdict",
                    )
                    .await?;
                }
            }
        }
        // This round's verdicts are persisted; push to the frontend so the review queue
        // refreshes
        state.emit_review(kb_id);
    }
    Ok(())
}

async fn apply_verdict(
    state: &AppState,
    kb_id: Uuid,
    item: &ReviewItem,
    same: Option<bool>,
    conf: f32,
    via: &str,
) -> anyhow::Result<()> {
    match same {
        Some(true) if conf >= AUTO_CONF => {
            let (target, source) =
                utopia_store::resolution::merge_direction(&state.pool, item.left.id, item.right.id)
                    .await?;
            let reason = format!("auto_merged|{via} {conf:.2}");
            match utopia_store::resolution::merge_entities(
                &state.pool,
                kb_id,
                source,
                target,
                None,
                &reason,
            )
            .await
            {
                Ok(_) => {
                    utopia_store::resolution::close_review_auto(
                        &state.pool,
                        item.id,
                        "merged",
                        &reason,
                    )
                    .await?;
                    // Decision ledger: an AI auto-merge (empty actor = the system)
                    let _ = utopia_store::audit::record_opt(
                        &state.pool,
                        Some(kb_id),
                        None,
                        "review.merge",
                        "review",
                        Some(item.id),
                        serde_json::json!({
                            "left": item.left.name, "right": item.right.name,
                            "score": item.score, "confidence": conf, "via": via,
                        }),
                    )
                    .await;
                }
                // A chained merge in the same batch may already have swallowed one of the
                // sides: escalate to a human rather than failing the job
                Err(AppError::Conflict(_)) | Err(AppError::NotFound) => {
                    utopia_store::resolution::escalate_review(
                        &state.pool,
                        item.id,
                        "escalate_entity_changed",
                    )
                    .await?;
                }
                Err(e) => return Err(e.into()),
            }
        }
        Some(false) if conf >= AUTO_CONF => {
            utopia_store::resolution::close_review_auto(
                &state.pool,
                item.id,
                "kept",
                &format!("kept_apart|{via} {conf:.2}"),
            )
            .await?;
            let _ = utopia_store::audit::record_opt(
                &state.pool,
                Some(kb_id),
                None,
                "review.keep",
                "review",
                Some(item.id),
                serde_json::json!({
                    "left": item.left.name, "right": item.right.name,
                    "score": item.score, "confidence": conf, "via": via,
                }),
            )
            .await;
        }
        _ => {
            utopia_store::resolution::escalate_review(
                &state.pool,
                item.id,
                &format!("escalate_unsure|{via} {conf:.2}"),
            )
            .await?;
        }
    }
    Ok(())
}
