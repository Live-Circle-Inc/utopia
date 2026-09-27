use axum::extract::{Path, Query, State};
use axum::Json;
use serde::Deserialize;
use serde_json::json;
use utopia_core::models::Role;
use utopia_core::AppError;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::error::ApiResult;
use crate::state::AppState;

pub(super) async fn require_kb(
    state: &AppState,
    user: &utopia_core::models::User,
    kb_id: Uuid,
    min: Role,
) -> Result<utopia_core::models::KnowledgeBase, AppError> {
    utopia_store::access::require_kb(&state.pool, user, kb_id, min).await
}

/// `at`: optional as-of date (YYYY-MM-DD or RFC3339) -- server-side time travel.
fn parse_at(raw: Option<&str>) -> Result<Option<chrono::DateTime<chrono::Utc>>, AppError> {
    let Some(s) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    if let Ok(d) = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Ok(Some(d.and_hms_opt(0, 0, 0).unwrap().and_utc()));
    }
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|t| Some(t.with_timezone(&chrono::Utc)))
        .map_err(|_| AppError::Validation("Invalid `at` (expected YYYY-MM-DD or RFC3339)".into()))
}

/// The **default** number of nodes the overview draws at once. The cap itself is reasonable --
/// nobody can make sense of ten thousand dots; what misleads is presenting it as the size, so the
/// endpoint returns the total alongside it
const GRAPH_NODE_CAP: i64 = 150;
/// However high you turn it up there still has to be a ceiling. **This number was not pulled out
/// of the air**: nodes are taken in descending degree order, so the further down you go the more
/// peripheral they are, and force-directed layout is on the order of O(n²) -- past this number
/// what breaks first is "you can still drag it around", not "you can still see it". If you really
/// want to look at tens of thousands of dots, that is a different view, not this knob turned up
const GRAPH_NODE_CAP_MAX: i64 = 1000;

#[derive(Deserialize)]
pub struct OverviewQuery {
    #[serde(default)]
    pub at: Option<String>,
    /// How many to draw. Absent, the default is used; supplied, it is still clamped to
    /// [10, GRAPH_NODE_CAP_MAX] -- the UI only offers a few steps, but the endpoint is public,
    /// and one `limit=999999` should not be able to drag the database down
    #[serde(default)]
    pub limit: Option<i64>,
}

pub async fn overview(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(kb_id): Path<Uuid>,
    Query(q): Query<OverviewQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Viewer).await?;
    let at = parse_at(q.at.as_deref())?;
    // How many get drawn is rendering's business; how many the database holds is the knowledge
    // base's business -- return both numbers and the UI can say "drew 150 of 325" instead of
    // passing the cap off as the size
    let limit = q
        .limit
        .unwrap_or(GRAPH_NODE_CAP)
        .clamp(10, GRAPH_NODE_CAP_MAX);
    let (nodes, edges, total_nodes, total_edges) =
        utopia_store::graph::overview(&state.pool, kb_id, limit, at).await?;
    Ok(Json(json!({
        "nodes": nodes, "edges": edges,
        "total_nodes": total_nodes, "total_edges": total_edges,
    })))
}

#[derive(Deserialize)]
pub struct NeighborhoodQuery {
    pub entity: Uuid,
    #[serde(default)]
    pub hops: Option<u8>,
    #[serde(default)]
    pub at: Option<String>,
}

pub async fn neighborhood(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(kb_id): Path<Uuid>,
    Query(q): Query<NeighborhoodQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Viewer).await?;
    let at = parse_at(q.at.as_deref())?;
    let (nodes, edges) =
        utopia_store::graph::neighborhood(&state.pool, kb_id, q.entity, q.hops.unwrap_or(2), at)
            .await?;
    Ok(Json(json!({ "nodes": nodes, "edges": edges })))
}

#[derive(Deserialize)]
pub struct EntitySearchQuery {
    pub q: String,
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub offset: Option<i64>,
}

pub async fn search_entities(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(kb_id): Path<Uuid>,
    Query(query): Query<EntitySearchQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Viewer).await?;
    if query.q.trim().is_empty() {
        return Ok(Json(json!({ "entities": [], "total": 0 })));
    }
    // Return the total as well: "split rather than merge" inevitably produces a pile of
    // same-named entities, and with a fixed ten rows the one you are looking for may not be among
    // those ten at all -- and the UI gives no sign of it
    let (entities, total) = utopia_store::graph::search_entities(
        &state.pool,
        kb_id,
        &query.q,
        query.limit.unwrap_or(10).clamp(1, 100),
        query.offset.unwrap_or(0).max(0),
    )
    .await?;
    Ok(Json(json!({ "entities": entities, "total": total })))
}

pub async fn entity_detail(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path((kb_id, entity_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Viewer).await?;
    let (entity, facts) = utopia_store::graph::entity_detail(&state.pool, kb_id, entity_id).await?;
    // The inferred ones **come back under their own key**; they are not mixed into `facts`. The
    // frontend gives them their own tier on that basis: with a derived edge and an asserted edge
    // in the same list, the user cannot tell "this one is written in the document" from "this one
    // was inferred by the engine" -- and that is exactly what reasoning polluting knowledge looks
    // like
    let derived =
        utopia_store::reasoning::derived_for_entity(&state.pool, kb_id, entity_id).await?;
    // The same-named ones are **handed over when the panel opens**, not returned only after a
    // rename.
    //
    // It used to come back only in `update_entity`'s response, which meant the "merge the
    // same-named one in" action was reachable only by renaming something first -- and two Zhang
    // Weis coexisting is a legitimate product of "split rather than merge", not something a
    // rename created. The merge entry point belongs where the same-named ones are visible.
    let same_name = utopia_store::graph::same_name_peers(&state.pool, kb_id, entity_id).await?;
    // Derivations that never landed (0017 §3) get their own key too: they are not even in
    // `derived_facts`
    let blocked =
        utopia_store::reasoning::blocked_for_entity(&state.pool, kb_id, entity_id).await?;
    Ok(Json(json!({
        "entity": entity, "facts": facts,
        "derived": derived, "blocked": blocked, "same_name": same_name,
    })))
}

#[derive(Deserialize)]
pub struct EntityPatch {
    #[serde(default)]
    pub type_id: Option<Uuid>,
    #[serde(default)]
    pub canonical_name: Option<String>,
}

/// Manually correcting an entity's type or name. What extraction gives is a first judgement, and
/// before this a wrong one could only be fixed by re-extracting the whole database.
///
/// A rename colliding with a same-named entity is not blocked (two Zhang Weis are a legitimate
/// product of "split rather than merge"); once renamed, the same-named ones are reported back and
/// the UI asks whether to merge -- deciding whether they really are the same one is a human's
/// job.
pub async fn update_entity(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path((kb_id, entity_id)): Path<(Uuid, Uuid)>,
    Json(req): Json<EntityPatch>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Editor).await?;
    if req.type_id.is_none() && req.canonical_name.is_none() {
        return Err(AppError::invalid("nothing_to_update", "Nothing to update").into());
    }
    let (before, after) = utopia_store::graph::update_entity(
        &state.pool,
        kb_id,
        entity_id,
        req.type_id,
        req.canonical_name.as_deref(),
    )
    .await?;

    // The ledger snapshot is self-contained: if the type is deleted later, this record is still
    // readable.
    // P4 wants to aggregate by from/to -- "37 entities moved from Product to Concept this month"
    // -- so the two actions are recorded separately.
    if before.type_key != after.type_key {
        let _ = utopia_store::audit::record(
            &state.pool,
            Some(kb_id),
            user.id,
            "entity.retyped",
            "entity",
            Some(entity_id),
            json!({
                "name": after.name,
                "from": { "key": before.type_key, "label": before.type_label },
                "to": { "key": after.type_key, "label": after.type_label },
            }),
        )
        .await;
    }
    if before.name != after.name {
        let _ = utopia_store::audit::record(
            &state.pool,
            Some(kb_id),
            user.id,
            "entity.renamed",
            "entity",
            Some(entity_id),
            json!({ "from": before.name, "to": after.name, "type": after.type_label }),
        )
        .await;
    }

    let peers = utopia_store::graph::same_name_peers(&state.pool, kb_id, entity_id).await?;
    state.emit_review(kb_id);
    Ok(Json(json!({ "entity": after, "same_name": peers })))
}

pub async fn fact_evidence(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path((kb_id, fact_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Viewer).await?;
    let evidence = utopia_store::graph::fact_evidence(&state.pool, fact_id).await?;
    Ok(Json(json!({ "evidence": evidence })))
}

/// The proof of one derived fact (0002 R2): premises in derivation order, each with its evidence,
/// all the way down to the original sentence.
/// When the derivation has been invalidated or does not exist, `proof` is null -- that is not an
/// error, and the UI falls back to textual premises on that basis
pub async fn derived_proof(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path((kb_id, derived_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Viewer).await?;
    let proof = utopia_store::reasoning::proof(&state.pool, kb_id, derived_id).await?;
    Ok(Json(json!({ "proof": proof })))
}

/// The proof chain of a derivation that never landed (0017 §3): the premises live in the `path`
/// of that `derived_contradiction` violation, and are expanded the same way as for one that did
/// land. When the violation does not exist, `steps` is null
pub async fn blocked_proof(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path((kb_id, violation_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Viewer).await?;
    let steps = utopia_store::reasoning::blocked_proof(&state.pool, kb_id, violation_id).await?;
    Ok(Json(json!({ "steps": steps })))
}

/// Manually triggering extraction (retrying a failure / catching up after a model was
/// configured).
pub async fn extract(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(document_id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    let doc = utopia_store::documents::get(&state.pool, document_id).await?;
    require_kb(&state, &user, doc.kb_id, Role::Editor).await?;
    // Manual trigger = forced full run: clear the incremental marker, fire the running job, set
    // queued, create the job -- all in one transaction
    let job_id = utopia_store::documents::queue_extraction_one(&state.pool, document_id).await?;
    state.emit_document(doc.kb_id, document_id);
    Ok(Json(json!({ "job_id": job_id })))
}

/// Rebuilding the graph (settlement semantics, KB admin): wipe the entire graph layer, then
/// re-extract everything.
/// The division of labour against source-level re-extraction -- re-extraction preserves existing
/// decisions, a rebuild abandons them, in exchange for a deterministic replay of "current corpus
/// x current ontology" (the way out of early dirty extractions, or of a large ontology change).
/// The decision ledger and the adjudication cache are deliberately preserved (see
/// store::graph::purge_graph).
pub async fn rebuild(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(kb_id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Admin).await?;
    let (entities, facts) = utopia_store::graph::purge_graph(&state.pool, kb_id).await?;
    // The jobs are created by queue_extraction in the same transaction as the status, so all
    // this does is push
    let ids = utopia_store::documents::queue_extraction(&state.pool, kb_id, None).await?;
    for id in &ids {
        state.emit_document(kb_id, *id);
    }
    state.emit_review(kb_id);
    let _ = utopia_store::audit::record(
        &state.pool,
        Some(kb_id),
        user.id,
        "graph.rebuild",
        "kb",
        Some(kb_id),
        json!({ "entities_removed": entities, "facts_removed": facts, "documents": ids.len() }),
    )
    .await;
    Ok(Json(json!({
        "entities_removed": entities, "facts_removed": facts, "queued": ids.len()
    })))
}

#[derive(Deserialize)]
pub struct HistoryQuery {
    #[serde(default)]
    pub page: i64,
    #[serde(default = "default_history_per")]
    pub per: i64,
}

fn default_history_per() -> i64 {
    30
}

/// An entity's epistemic change history (the record-time axis): when we thought this, and when we
/// changed our mind.
/// Complementary to entity_detail (the valid-time axis, showing only what is current).
pub async fn entity_history(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path((kb_id, entity_id)): Path<(Uuid, Uuid)>,
    Query(q): Query<HistoryQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Viewer).await?;
    let per = q.per.clamp(1, 200);
    let page = q.page.max(0);
    let (events, total) =
        utopia_store::graph::entity_history(&state.pool, kb_id, entity_id, per, page * per).await?;
    Ok(Json(json!({ "events": events, "total": total })))
}
