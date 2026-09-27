//! Ontology editor API: type/relation CRUD + unmatched counts + LLM extension suggestions.
//! Reading = viewer; writing = editor (the ontology directly drives the whitelist that later
//! extraction is held to).

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::Deserialize;
use serde_json::json;
use utopia_core::models::Role;
use utopia_core::AppError;
use utopia_llm::ChatMessage;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::error::ApiResult;
use crate::llm_util;
use crate::state::AppState;

async fn require_kb(
    state: &AppState,
    user: &utopia_core::models::User,
    kb_id: Uuid,
    min: Role,
) -> Result<utopia_core::models::KnowledgeBase, AppError> {
    utopia_store::access::require_kb(&state.pool, user, kb_id, min).await
}

pub async fn get(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(kb_id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Viewer).await?;
    let entity_types = utopia_store::ontology::entity_type_views(&state.pool, kb_id).await?;
    let relation_types = utopia_store::ontology::relation_type_views(&state.pool, kb_id).await?;
    let misses = utopia_store::ontology::list_misses(&state.pool, kb_id).await?;
    // The dismissed ones get a list of their own: suppression still holds (proposals and the
    // automatic ontology extension only look at the list above), but a person can still see what
    // got suppressed and how far it has climbed since
    let dismissed_misses =
        utopia_store::ontology::list_dismissed_misses(&state.pool, kb_id).await?;
    Ok(Json(json!({
        "entity_types": entity_types,
        "relation_types": relation_types,
        "misses": misses,
        "dismissed_misses": dismissed_misses,
    })))
}

#[derive(Deserialize)]
pub struct InstancesQuery {
    #[serde(default)]
    pub page: i64,
    #[serde(default = "default_per")]
    pub per: i64,
}

fn default_per() -> i64 {
    12
}

/// The entity instances under one class (right-hand side of the detail pane, paginated).
pub async fn list_entity_instances(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path((kb_id, type_id)): Path<(Uuid, Uuid)>,
    Query(q): Query<InstancesQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Viewer).await?;
    let per = q.per.clamp(1, 100);
    let page = q.page.max(0);
    let (rows, total) =
        utopia_store::ontology::entity_instances(&state.pool, kb_id, type_id, per, page * per)
            .await?;
    Ok(Json(json!({ "entities": rows, "total": total })))
}

#[derive(Deserialize)]
pub struct EntityTypeReq {
    pub key: Option<String>,
    pub label: String,
    #[serde(default)]
    pub color: Option<String>,
    /// circle | square
    #[serde(default)]
    pub shape: Option<String>,
    /// All parents, **the first one counts as the primary parent** (the left column draws it
    /// under that branch).
    /// The UI spells this out, so there is no extra "pick the primary parent" control
    #[serde(default)]
    pub parents: Vec<Uuid>,
    /// The classes it is disjoint with. **Absent = leave alone** -- callers that do not care
    /// about disjointness (proposal adoption, cold start) should not wipe out declarations that
    /// are already there just because a class got created once
    #[serde(default)]
    pub disjoint: Option<Vec<Uuid>>,
    /// Semantic guidance, injected into the extraction prompt
    #[serde(default)]
    pub description: Option<String>,
}

pub async fn create_entity_type(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(kb_id): Path<Uuid>,
    Json(req): Json<EntityTypeReq>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Editor).await?;
    let key = req
        .key
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::invalid("key_required", "key is required"))?;
    let id = utopia_store::ontology::create_entity_type(
        &state.pool,
        kb_id,
        key,
        req.label.trim(),
        // No color given: pick one from the key, rather than every class sharing one grey-blue
        req.color
            .as_deref()
            .unwrap_or_else(|| utopia_store::palette::color_for_key(key)),
        req.shape.as_deref().unwrap_or("circle"),
        &req.parents,
        req.description.as_deref().unwrap_or("").trim(),
    )
    .await?;
    // Disjointness absent = leave alone: callers that do not care about it (proposal adoption,
    // cold start) should not wipe out declarations that are already there just because a class
    // got created
    if let Some(d) = req.disjoint.as_deref() {
        utopia_store::ontology::set_disjoint_for(&state.pool, kb_id, id, d).await?;
    }
    // The audit trail records, it never blocks
    let _ = utopia_store::audit::record(
        &state.pool,
        Some(kb_id),
        user.id,
        "entity_type.created",
        "entity_type",
        Some(id),
        json!({ "key": key, "label": req.label.trim() }),
    )
    .await;
    Ok(Json(json!({ "id": id })))
}

pub async fn update_entity_type(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path((kb_id, id)): Path<(Uuid, Uuid)>,
    Json(req): Json<EntityTypeReq>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Editor).await?;
    utopia_store::ontology::update_entity_type(
        &state.pool,
        kb_id,
        id,
        req.label.trim(),
        // **No color given = keep the color it has**, not reset it. This path updates by id, so
        // it cannot get at the key at all; and unconditionally writing "#8ea5bd" here, which is
        // what it used to do, meant every rename that did not carry a color wiped out the color
        // the user had picked
        req.color.as_deref(),
        req.shape.as_deref().unwrap_or("circle"),
        &req.parents,
        req.description.as_deref().unwrap_or("").trim(),
    )
    .await?;
    if let Some(d) = req.disjoint.as_deref() {
        utopia_store::ontology::set_disjoint_for(&state.pool, kb_id, id, d).await?;
    }
    let _ = utopia_store::audit::record(
        &state.pool,
        Some(kb_id),
        user.id,
        "entity_type.updated",
        "entity_type",
        Some(id),
        json!({ "label": req.label.trim(), "color": req.color, "shape": req.shape,
                "description": req.description }),
    )
    .await;
    Ok(Json(json!({ "ok": true })))
}

pub async fn delete_entity_type(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path((kb_id, id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Editor).await?;
    utopia_store::ontology::delete_entity_type(&state.pool, kb_id, id).await?;
    let _ = utopia_store::audit::record(
        &state.pool,
        Some(kb_id),
        user.id,
        "entity_type.deleted",
        "entity_type",
        Some(id),
        json!({}),
    )
    .await;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct RelationTypeReq {
    pub key: Option<String>,
    pub label: String,
    #[serde(default = "default_temporal")]
    pub temporal: String,
    #[serde(default)]
    pub functional: bool,
    #[serde(default)]
    pub inverse_functional: bool,
    /// The other four OWL axioms. **These have to be editable in the UI** -- every criterion the
    /// reasoner (0002) judges by comes from these bits, and they used to be reachable only by
    /// importing an OWL file: a user who builds an ontology by hand in the UI could never start
    /// that machine.
    ///
    /// They sit alongside `functional` / `inverse_functional`, because they were always the same
    /// family; those two just landed in the database first
    #[serde(default)]
    pub is_transitive: bool,
    #[serde(default)]
    pub is_symmetric: bool,
    #[serde(default)]
    pub is_asymmetric: bool,
    #[serde(default)]
    pub is_irreflexive: bool,
    /// The two that point at another relation (`inverseOf` / `subPropertyOf`).
    ///
    /// **Absent = cleared, the same rule as the six above.** They are one set of declarations
    /// submitted by one form in one go; overwriting half and keeping half would make "I removed
    /// the inverse" and "I never touched the inverse" look exactly alike. The attribute form
    /// does not fill these two in, and an attribute is not allowed to have them anyway --
    /// the store layer blocks that, and the column stays NULL either way
    #[serde(default)]
    pub inverse_of: Option<Uuid>,
    #[serde(default)]
    pub sub_property_of: Option<Uuid>,
    #[serde(default)]
    pub description: Option<String>,
    /// relation | attribute (fixed at creation, ignored on update)
    #[serde(default)]
    pub kind: Option<String>,
    /// The classes that may be the subject. An attribute needs at least one; empty on a relation
    /// = unrestricted.
    /// **Absent on update = leave alone**, which is why this is an Option and not a Vec -- a
    /// caller that does not care about domain (the attribute form) should not wipe the domain
    /// out over one rename
    #[serde(default)]
    pub domains: Option<Vec<Uuid>>,
    /// The classes that may be the object. Only meaningful for a relation
    #[serde(default)]
    pub ranges: Option<Vec<Uuid>>,
    /// Attributes only: text | number | date | bool
    #[serde(default)]
    pub datatype: Option<String>,
    #[serde(default)]
    pub unit: Option<String>,
}

impl RelationTypeReq {
    /// Pack the axioms up. **Passed loose, they get passed in the wrong order sooner or later**
    /// -- all six are bools, so the compiler cannot help; the last two are both `Option<Uuid>`,
    /// same story.
    fn axioms(&self) -> utopia_core::models::RelationAxioms {
        utopia_core::models::RelationAxioms {
            functional: self.functional,
            inverse_functional: self.inverse_functional,
            transitive: self.is_transitive,
            symmetric: self.is_symmetric,
            asymmetric: self.is_asymmetric,
            irreflexive: self.is_irreflexive,
            inverse_of: self.inverse_of,
            sub_property_of: self.sub_property_of,
        }
    }
}

fn default_temporal() -> String {
    "state".into()
}

pub async fn create_relation_type(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(kb_id): Path<Uuid>,
    Json(req): Json<RelationTypeReq>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Editor).await?;
    let key = req
        .key
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::invalid("key_required", "key is required"))?;
    let kind = req.kind.as_deref().unwrap_or("relation");
    let id = utopia_store::ontology::create_relation_type(
        &state.pool,
        kb_id,
        key,
        req.label.trim(),
        &req.temporal,
        req.axioms(),
        req.description.as_deref().unwrap_or("").trim(),
        kind,
        req.domains.as_deref().unwrap_or(&[]),
        req.ranges.as_deref().unwrap_or(&[]),
        req.datatype.as_deref(),
        req.unit.as_deref().map(str::trim).filter(|s| !s.is_empty()),
    )
    .await?;
    let _ = utopia_store::audit::record(
        &state.pool,
        Some(kb_id),
        user.id,
        "relation_type.created",
        "relation_type",
        Some(id),
        json!({ "key": key, "label": req.label.trim(), "temporal": req.temporal, "kind": kind }),
    )
    .await;
    Ok(Json(json!({ "id": id })))
}

pub async fn update_relation_type(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path((kb_id, id)): Path<(Uuid, Uuid)>,
    Json(req): Json<RelationTypeReq>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Editor).await?;
    utopia_store::ontology::update_relation_type(
        &state.pool,
        kb_id,
        id,
        req.label.trim(),
        &req.temporal,
        req.axioms(),
        req.description.as_deref().unwrap_or("").trim(),
        req.datatype.as_deref(),
        req.unit.as_deref().map(str::trim).filter(|s| !s.is_empty()),
        // The request did not carry these two fields, so leave them alone -- the attribute form
        // has no business with domain
        req.domains.as_deref(),
        req.ranges.as_deref(),
    )
    .await?;
    let _ = utopia_store::audit::record(
        &state.pool,
        Some(kb_id),
        user.id,
        "relation_type.updated",
        "relation_type",
        Some(id),
        json!({ "label": req.label.trim(), "temporal": req.temporal,
                "functional": req.functional, "inverse_functional": req.inverse_functional,
                "description": req.description }),
    )
    .await;
    Ok(Json(json!({ "ok": true })))
}

pub async fn delete_relation_type(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path((kb_id, id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Editor).await?;
    utopia_store::ontology::delete_relation_type(&state.pool, kb_id, id).await?;
    let _ = utopia_store::audit::record(
        &state.pool,
        Some(kb_id),
        user.id,
        "relation_type.deleted",
        "relation_type",
        Some(id),
        json!({}),
    )
    .await;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct DismissMissReq {
    pub kind: String,
    pub key: String,
}

pub async fn dismiss_miss(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(kb_id): Path<Uuid>,
    Json(req): Json<DismissMissReq>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Editor).await?;
    utopia_store::ontology::dismiss_miss(&state.pool, kb_id, &req.kind, &req.key).await?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn restore_miss(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(kb_id): Path<Uuid>,
    Json(req): Json<DismissMissReq>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Editor).await?;
    utopia_store::ontology::restore_miss(&state.pool, kb_id, &req.kind, &req.key).await?;
    Ok(Json(json!({ "ok": true })))
}

/// LLM ontology extension suggestions: existing ontology + unmatched counts → proposals (merged
/// in through the create endpoints once a person has reviewed them).
/// `locale` is what the **caller** says, not a backend setting. The reason is read by a person,
/// and that person is at the other end of this very request; the UI language lives on the client
/// (docs/decisions/0004), so this is the only way it can get in here.
#[derive(Deserialize, Default)]
pub struct SuggestReq {
    #[serde(default)]
    pub locale: Option<String>,
}

pub async fn suggest(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(kb_id): Path<Uuid>,
    body: Option<Json<SuggestReq>>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Editor).await?;
    let locale = body
        .and_then(|Json(b)| b.locale)
        .filter(|l| matches!(l.as_str(), "en" | "zh"))
        .unwrap_or_else(|| "en".into());
    // The manual path passes min_docs = 0: that "appears in 1 document" number on the panel is
    // shown to a person, and he can judge it for himself. Filtering it out on his behalf only
    // takes one piece of information away from him
    let proposals = build_proposals(&state, kb_id, &locale, 0).await?;
    // Write them down as soon as they are computed (see `ontology_proposals`). This batch used
    // to go to the frontend and nowhere else, into a useState, and one refresh lost it -- while
    // recomputing means another model call, and not necessarily the same set of merges
    persist_proposals(&state, kb_id, &proposals).await;
    Ok(Json(proposals))
}

/// One row per item in each of the four sections. **A failure here does not block the response**
/// -- the proposals are already computed; not being able to store them only means recomputing
/// next time, whereas failing the whole request throws away what was computed too.
async fn persist_proposals(state: &AppState, kb_id: Uuid, proposals: &serde_json::Value) {
    const SECTIONS: [&str; 4] = [
        "entity_types",
        "relation_types",
        "attribute_types",
        "map_to",
    ];
    let mut rows: Vec<(String, String, serde_json::Value)> = Vec::new();
    for section in SECTIONS {
        let Some(items) = proposals.get(section).and_then(|v| v.as_array()) else {
            continue;
        };
        for it in items {
            // The key is this item's identity (it is what the unique constraint in the migration
            // is built on). One without a key cannot be stored, and cannot be matched back up at
            // adoption time -- so skip it rather than inventing one
            let Some(key) = it.get("key").and_then(|k| k.as_str()) else {
                continue;
            };
            rows.push((section.to_string(), key.to_string(), it.clone()));
        }
    }
    if let Err(e) = utopia_store::ontology::save_proposals(&state.pool, kb_id, &rows).await {
        tracing::warn!(%kb_id, error = %e, "Could not store the ontology proposals; this batch lives only in this response");
    }
}

/// The proposals still waiting on a person, reassembled into the shape the endpoint always had.
///
/// The frontend therefore does not have to tell "just computed" from "stored last time" -- they
/// are the same type, rendered by the same code.
pub async fn stored_proposals(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(kb_id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Viewer).await?;
    let stored = utopia_store::ontology::open_proposals(&state.pool, kb_id).await?;
    let mut out = json!({
        "entity_types": [], "relation_types": [], "attribute_types": [], "map_to": []
    });
    for p in stored {
        if let Some(arr) = out.get_mut(&p.section).and_then(|v| v.as_array_mut()) {
            arr.push(p.payload);
        }
    }
    Ok(Json(out))
}

#[derive(Deserialize)]
pub struct DecideProposalReq {
    pub section: String,
    pub key: String,
    /// adopted | rejected
    pub status: String,
}

/// Somebody has ruled on a proposal. **Change the status, never delete the row**: the adoption
/// happened, and so did the rejection -- and the trace a rejection leaves is exactly what keeps
/// the next round of Suggest from pushing it back into the waiting list.
pub async fn decide_proposal(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(kb_id): Path<Uuid>,
    Json(req): Json<DecideProposalReq>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Editor).await?;
    if !matches!(req.status.as_str(), "adopted" | "rejected") {
        return Err(AppError::invalid("bad_status", "status must be adopted or rejected").into());
    }
    utopia_store::ontology::decide_proposal(
        &state.pool,
        kb_id,
        &req.section,
        &req.key,
        &req.status,
        user.id,
    )
    .await?;
    Ok(Json(json!({ "ok": true })))
}

/// Generate ontology extension proposals. A person clicking Suggest and the cold-start automatic
/// extension go down the same path -- the automatic one should not be a second set of
/// judgements, it just skips the nod.
/// A handful of candidates retrieved per wording.
///
/// Small on purpose: the candidates are there for the model to judge "do we have this already",
/// not for it to pick whichever looks most alike and make do. Opening it up only makes it force
/// a mapping onto one entry out of a pile of barely related ones.
const CANDIDATES_PER_PROBE: i64 = 5;

/// `min_docs`: **hand the model only the wordings that appear in at least this many documents**.
/// 0 = hand over all of them.
///
/// This parameter exists because it once did not. The comment on `ProposedPredicate.doc_count`
/// read "the automatic extension sets its threshold from this; manual proposals only take it as
/// a hint and are never blocked by it", but the wiring was never finished: `bootstrap_ontology`
/// computed the threshold and used it only for the `forms.len()` check on whether an LLM call
/// was worth making, then called this function with nothing but kb_id, so this code re-queried
/// the **unfiltered** full set.
///
/// Measured consequences (ai-timeline, 348 chunks): 526 wordings handed to the model, 456 of
/// which appeared in a single document -- **86.7% noise**. Out of that pile the model picked
/// only 9; `runs_on` (in all 8 documents) and `founded_by` (4 documents) were not picked, and
/// 275 facts still have no predicate. It leaks the other way too: 5 single-document wordings
/// were adopted, two of which even became new attributes, so the threshold was a fiction.
pub async fn build_proposals(
    state: &AppState,
    kb_id: Uuid,
    reason_lang: &str,
    min_docs: i64,
) -> Result<serde_json::Value, AppError> {
    let kb = utopia_store::kbs::get(&state.pool, kb_id).await?;
    let settings = utopia_store::settings::get(&state.pool, kb.workspace_id)
        .await?
        .ok_or_else(|| AppError::invalid("no_chat_model", "Chat model not configured"))?;
    let client = llm_util::chat_client(&settings)
        .ok_or_else(|| AppError::invalid("no_chat_model", "Chat model not configured"))?;

    let mut misses = utopia_store::ontology::list_misses(&state.pool, kb_id).await?;
    // Surface predicates carry one thing misses do not: they are attached to concrete facts, so
    // a proposal can promise to "rewrite N of them".
    //
    // **The two lines stay strictly apart**, by whether the object is an entity or a literal:
    // `acquires` wants a relation, `founding_date = "2015"` wants an attribute. Mixing them has
    // a concrete consequence -- the latter gets proposed as a relation, and out grows an edge
    // pointing at "2015", an entity that does not exist
    let mut forms = utopia_store::graph::proposed_predicates(&state.pool, kb_id).await?;
    let mut value_forms = utopia_store::graph::proposed_attributes(&state.pool, kb_id).await?;
    if min_docs > 0 {
        forms.retain(|f| f.doc_count >= min_docs);
        value_forms.retain(|f| f.doc_count >= min_docs);
        // **All three lists have to be filtered; filtering one is the same as filtering none.**
        // The same wording appears twice in the prompt: as a miss line ("seen N times") and as a
        // surface-predicate line ("on N fact(s)"). `OntologyMiss` has no document dimension, so
        // filter it against the set of wordings that survived -- what is left is the ones that
        // both spanned documents and still have no predicate. The class route is left alone:
        // `ProposedType` has no doc_count either, so filtering it would be blocking on a
        // different criterion rather than on this one
        let kept: std::collections::HashSet<&str> = forms.iter().map(|f| f.form.as_str()).collect();
        misses.retain(|m| m.kind != "relation_type" || kept.contains(m.key.as_str()));
    }
    if misses.is_empty() && forms.is_empty() && value_forms.is_empty() {
        return Ok(json!({
            "entity_types": [], "relation_types": [], "attribute_types": [], "map_to": []
        }));
    }

    // **The relevant slice of the ontology, not the whole text of it.**
    //
    // This used to inline two full lists of keys. Two things wrong with that: a 965-class
    // ontology is a prompt of 1949 keys; and with keys but no descriptions, the model cannot
    // judge "is this wording a synonym of some existing type" from them, so all it ever does is
    // create, and duplicates grow in the ontology at a steady rate.
    //
    // Now a few nearest candidates get retrieved per wording and shown to it with their
    // descriptions. Prompt size is from here on independent of ontology size, and "there is one
    // already" became judgeable for the first time.
    let _ = crate::ontology_index::refresh(state, kb_id).await;
    // The predicate route: surface wordings + relation-type misses. An example gives the vector
    // more to hold on to -- acquires on its own is too short, but with "Nebula Technologies →
    // Deep Blue Storage" attached it has context
    let pred_probes: Vec<String> = forms
        .iter()
        .map(|f| match f.example.as_deref() {
            Some(ex) if !ex.is_empty() => format!("{} — {ex}", f.form),
            _ => f.form.clone(),
        })
        .chain(
            misses
                .iter()
                .filter(|m| m.kind == "relation_type")
                .map(|m| m.key.clone()),
        )
        .collect();
    // The class route. **The retrieval has to be separate**: the descriptions in the two tables
    // say entirely different things, and taking a class name that is not in the vocabulary and
    // searching the relations with it can only come back with a barely related relation
    let class_probes: Vec<String> = misses
        .iter()
        .filter(|m| m.kind == "entity_type")
        .map(|m| m.key.clone())
        .collect();
    let per_pred = crate::ontology_index::nearest_for_each(
        state,
        kb_id,
        &pred_probes,
        CANDIDATES_PER_PROBE,
        crate::ontology_index::Target::Predicate(None),
    )
    .await
    .unwrap_or_default();
    let per_class = crate::ontology_index::nearest_for_each(
        state,
        kb_id,
        &class_probes,
        CANDIDATES_PER_PROBE,
        crate::ontology_index::Target::Class,
    )
    .await
    .unwrap_or_default();
    // The attribute route: search only among the attributes. Without the kind restriction, the
    // nearest hit for "founding date" is usually some relation, and the model would then map a
    // literal value onto an edge
    let attr_probes: Vec<String> = value_forms
        .iter()
        .map(|f| match f.example.as_deref() {
            Some(ex) if !ex.is_empty() => format!("{} = {ex}", f.form),
            _ => f.form.clone(),
        })
        .chain(
            misses
                .iter()
                .filter(|m| m.kind == "attribute_type")
                .map(|m| m.key.clone()),
        )
        .collect();
    let per_attr = crate::ontology_index::nearest_for_each(
        state,
        kb_id,
        &attr_probes,
        CANDIDATES_PER_PROBE,
        crate::ontology_index::Target::Predicate(Some("attribute")),
    )
    .await
    .unwrap_or_default();
    // Union and dedupe: several wordings often point at the same candidate, and listing it once
    // per wording burns tokens for nothing
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut candidate_lines: Vec<String> = Vec::new();
    // The key the model copies back has to be matchable against the ontology: index the
    // candidate table by key, by normalized key and by label all at once
    let mut by_name: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    // Whether each candidate is a relation or an attribute. When mapping onto an existing type,
    // this is what decides which rewrite path adoption takes
    let mut kind_of_key: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for c in per_pred
        .iter()
        .chain(per_class.iter())
        .chain(per_attr.iter())
        .flatten()
    {
        if !seen.insert(c.key.clone()) {
            continue;
        }
        by_name.insert(normalize_name(&c.key), c.key.clone());
        by_name.insert(normalize_name(&c.label), c.key.clone());
        // Relation rows carry relation / attribute themselves; class rows have no kind
        let kind = c.kind.as_deref().unwrap_or("entity type");
        kind_of_key.insert(c.key.clone(), kind.to_string());
        let d = c.description.trim();
        // **Write the label out only when it genuinely says something more.**
        // In imported ontologies the label is often just the camelCase spelling of the key
        // (acquired_from / acquiredFrom), and with two nearly identical names sitting side by
        // side, the one the model copies is the wrong one. A Chinese label against an English
        // key is the case where a label does carry information; that is when it stays
        let name = if normalize_name(&c.label) == normalize_name(&c.key) {
            String::new()
        } else {
            format!(" ({})", c.label)
        };
        candidate_lines.push(if d.is_empty() {
            format!("- {} [{kind}]{name}", c.key)
        } else {
            format!("- {} [{kind}]{name}: {d}", c.key)
        });
    }
    // When there is not one candidate to be had (no embedding model configured, or the ontology
    // is empty), say so honestly -- do not let the model conclude "there is nothing in the
    // ontology" and start creating with a free hand
    let candidates_block = if candidate_lines.is_empty() {
        "(no candidates retrieved — the ontology may be empty, or embeddings are unavailable)"
            .to_string()
    } else {
        candidate_lines.join(
            "
",
        )
    };
    let miss_lines: Vec<String> = misses
        .iter()
        .map(|m| {
            format!(
                "- [{}] \"{}\" seen {} times, e.g. {}",
                m.kind,
                m.key,
                m.count,
                m.example.as_deref().unwrap_or("-")
            )
        })
        .collect();

    // Surface-predicate lines carry the fact count and an example: that is what the model judges
    // from whether this is a real relation, and the forms field is what tells adoption which
    // facts to rewrite
    let form_lines: Vec<String> = forms
        .iter()
        .map(|f| {
            format!(
                "- \"{}\" on {} fact(s), e.g. {}",
                f.form,
                f.fact_count,
                f.example.as_deref().unwrap_or("-")
            )
        })
        .collect();

    // The literal-value route carries two extra things: one sample value (the model judges from
    // it whether this should be number or date), and the classes this wording actually hangs
    // off. The latter is not for the model to read; adoption takes it directly as the domain --
    // guess an attribute's domain wrong and every fact whose subject type does not match is
    // thrown away whole
    let value_lines: Vec<String> = value_forms
        .iter()
        .map(|f| {
            format!(
                "- \"{}\" on {} fact(s), value e.g. {}, seen on: {}",
                f.form,
                f.fact_count,
                f.example.as_deref().unwrap_or("-"),
                if f.domain_keys.is_empty() {
                    "-".to_string()
                } else {
                    f.domain_keys.join(", ")
                }
            )
        })
        .collect();

    let prompt = format!(
        "You are an ontology engineer.\n\
         \n\
         Below are the ontology entries closest in meaning to the unmatched wordings that \
         follow. This is a retrieved slice, not the whole ontology, so \"not in this list\" \
         does not mean \"not in the ontology\" — it means nothing close to it was found:\n{}\n\
         \n\
         During extraction, the LLM repeatedly produced types/relations OUTSIDE this ontology:\n{}\n\
         \n\
         These predicates were taken from the source text because nothing in the ontology fit. \
         Their facts are currently filed under \"related_to\", which says nothing:\n{}\n\
         \n\
         These carried a literal VALUE rather than pointing at another entity, so each one \
         wants an attribute, never a relation. Turning one into a relation manufactures an \
         entity out of the value — a node named \"2015\" that stands for nothing:\n{}\n\
         \n\
         Each wording gets exactly ONE answer. Do not both map it and propose for it, and do \
         not propose it as a relation and as an attribute — a wording listed above as carrying \
         a value is an attribute, full stop.\n\
         \n\
         For each wording, decide one of two things.\n\
         \n\
         **If one of the candidates above already means it, map to it** — name that \
         candidate's key in \"map_to\". Do this whenever the meaning matches even though the \
         spelling differs (\"founding date\" is founding_date; \"headquartered in\" is \
         location). Adding a second entry for a meaning the ontology already carries is the \
         worst outcome available here: it splits the same facts across two keys permanently, \
         and nothing downstream can tell they were the same.\n\
         \n\
         **Otherwise propose a new entry.** Rules:\n\
         - Merge near-duplicates into ONE relation and list every spelling it covers in \
           \"forms\" (e.g. available_on / available_from / \"available through\" are one relation).\n\
         - Skip generic verbs that carry no domain meaning (is, has, includes, provides, brings).\n\
         - A relation is worth adding when the ontology genuinely lacks that meaning, not merely \
           because a word was frequent.\n\
         - \"functional\" must be false unless the relation truly permits at most one object per \
           subject at a time. Getting this wrong makes the temporal engine manufacture conflicts.\n\
         - An attribute needs a \"datatype\": text, number, date or bool. Read it off the example \
           value. Choose text when unsure — a value that will not convert to the declared type \
           is dropped, and a date stored as text is still the value.\n\
         \n\
         Every proposal needs a \"description\" as well as a \"reason\", and they are not the \
         same thing. The reason argues for adding it and is read by a person. **The description \
         is injected verbatim into the extraction prompt and is the only thing telling the model \
         what belongs here** — write it as a definition: say what the type is, then say what it \
         is not and which existing type those cases belong to. A type that arrives with a weak \
         description becomes the next dumping ground.\n\
         \n\
         Output exactly one JSON object:\n\
         {{\"entity_types\":[{{\"key\":\"snake_case\",\"label\":\"Display Name\",\"description\":\"what belongs here, and what does not\",\"reason\":\"why add it\"}}],\n\
          \"relation_types\":[{{\"key\":\"snake_case\",\"label\":\"display label\",\"temporal\":\"state|event|eternal\",\"functional\":false,\"forms\":[\"surface spellings this covers\"],\"description\":\"what this relation asserts, and what it does not\",\"reason\":\"why add it\"}}],\n\
          \"attribute_types\":[{{\"key\":\"snake_case\",\"label\":\"display label\",\"datatype\":\"text|number|date|bool\",\"unit\":\"optional, e.g. CNY\",\"forms\":[\"surface spellings this covers\"],\"description\":\"what this attribute records, and what it does not\",\"reason\":\"why add it\"}}],\n\
          \"map_to\":[{{\"key\":\"an existing key, copied from the candidate list above\",\"forms\":[\"surface spellings that mean it\"],\"reason\":\"why these are the same thing\"}}]}}\n\
         \n\
         Language, and it overrides the skeleton above — that skeleton is written in English \
         only because these instructions are. Write every \"label\" and \"description\" in {}: \
         they become this knowledge base's own ontology, and the description is read by the \
         extraction model while it reads documents in that language. Write every \"reason\" \
         in {}: a person reads it. \"key\" and \"forms\" stay lowercase ASCII either way.",
        candidates_block,
        miss_lines.join("\n"),
        form_lines.join("\n"),
        value_lines.join("\n"),
        lang_name(&kb.ontology_lang),
        lang_name(reason_lang)
    );

    let reply = client
        .chat(&[ChatMessage {
            role: "user".into(),
            content: prompt,
        }])
        .await
        .map_err(AppError::Other)?;
    let block = utopia_extract::json_block(&reply).map_err(AppError::Other)?;
    let mut proposals: serde_json::Value =
        serde_json::from_str(&block).map_err(|e| AppError::Other(e.into()))?;
    resolve_map_targets(&mut proposals, &by_name, &kind_of_key);
    // Which category a wording belongs in is the server's call -- the server is holding the
    // facts. In practice the model will propose the same \"founded_in\" as a relation and as an
    // attribute, and adopting both means the same batch of facts gets claimed twice
    let value_only: std::collections::HashSet<&str> =
        value_forms.iter().map(|f| f.form.as_str()).collect();
    let entity_only: std::collections::HashSet<&str> =
        forms.iter().map(|f| f.form.as_str()).collect();
    keep_forms(&mut proposals, "relation_types", &entity_only, &value_only);
    keep_forms(&mut proposals, "attribute_types", &value_only, &entity_only);
    Ok(proposals)
}

/// Match the keys in `map_to` back to the ontology's real keys, and drop any item that does not
/// match.
///
/// The model copying it wrong is common, and what it copies is usually the label sitting right
/// next to it on the candidate line -- `acquiredFrom` rather than `acquired_from`. **This step
/// has to happen on the server**: that "use the existing one" button in the UI promises to hang
/// a batch of facts onto some predicate that already exists, and when the key does not match,
/// all it can do is throw an error -- with the promise already made. If it does not match, the
/// item should not be shown in the first place.
/// While we are at it, mark whether the target is a relation or an attribute: the two adoption
/// paths rewrite differently, and what the model answers with is a single key, which does not
/// reveal which table or which category that key lands in.
fn resolve_map_targets(
    proposals: &mut serde_json::Value,
    by_name: &std::collections::HashMap<String, String>,
    kind_of_key: &std::collections::HashMap<String, String>,
) {
    let Some(items) = proposals.get_mut("map_to").and_then(|v| v.as_array_mut()) else {
        return;
    };
    items.retain_mut(|m| {
        let Some(raw) = m.get("key").and_then(|v| v.as_str()) else {
            return false;
        };
        match by_name.get(&normalize_name(raw)) {
            Some(real) => {
                m["kind"] = json!(kind_of_key
                    .get(real)
                    .map(String::as_str)
                    .unwrap_or("relation"));
                m["key"] = json!(real);
                true
            }
            None => {
                tracing::debug!(
                    key = raw,
                    "map_to pointed at a key outside the candidates, dropping it"
                );
                false
            }
        }
    });
}

/// Keep only the proposals whose wordings really do belong in this category, and strip the ones
/// that do not out of `forms`.
///
/// **The criterion is in the server's hands**: whether a wording carries a literal or an entity
/// object is written in the facts, so there is no need to ask the model. In practice the model
/// does propose the same `founded_in` as a relation and as an attribute -- adopt both and the
/// same batch of facts gets claimed twice, whichever ran first wins, and the outcome depends on
/// loop order.
///
/// A proposal whose `forms` got stripped bare is dropped whole: the "rewrite N of them" it
/// promised is already zero.
fn keep_forms(
    proposals: &mut serde_json::Value,
    section: &str,
    mine: &std::collections::HashSet<&str>,
    theirs: &std::collections::HashSet<&str>,
) {
    let Some(items) = proposals.get_mut(section).and_then(|v| v.as_array_mut()) else {
        return;
    };
    items.retain_mut(|p| {
        let Some(forms) = p.get_mut("forms").and_then(|v| v.as_array_mut()) else {
            // A proposal with no forms is just "add a type": it rewrites no facts, so which
            // category it belongs in does not apply
            return true;
        };
        forms.retain(|f| {
            let Some(s) = f.as_str() else { return false };
            // Strip only the ones the other category explicitly claims. A wording neither side
            // recognizes (one that came from misses rather than from a surface predicate) stays
            // as it is -- stripping it would be vetoing one of the model's proposals for it
            !theirs.contains(s) || mine.contains(s)
        });
        !forms.is_empty()
    });
}

/// The comparison form of a key or a label: lowercased, alphanumerics only.
///
/// **Separators get thrown out entirely**, because the difference in separators is precisely
/// what has to line up: `acquiredFrom`, `acquired_from` and `Acquired From` all collapse to
/// `acquiredfrom`. Folding them into underscores is not enough
/// -- camelCase has no separator to fold in the first place.
///
/// This only lines spellings up, it makes no synonym judgement -- that is the job of retrieval
/// and of the model.
fn normalize_name(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

#[derive(Deserialize)]
pub struct AdoptReq {
    pub key: String,
    /// true = this key refers to a relation/attribute that **already exists**, do not create
    /// another one.
    ///
    /// This bit is where ontology consolidation lands: retrieval tells the model the ontology
    /// already has `founding_date`, the model says "these wordings are it", and adoption does
    /// only the rewrite half. Without this bit, one meaning grows a second key, and from then on
    /// this batch of facts is split across two places for good.
    #[serde(default)]
    pub existing: bool,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default = "default_temporal")]
    pub temporal: String,
    /// Defaults to false, and deliberately not the suggester's call -- a uniqueness declaration
    /// in the ontology drives the temporal engine to close facts off automatically, and guessing
    /// wrong means false conflicts by the batch (part_of got burned that way once)
    #[serde(default)]
    pub functional: bool,
    #[serde(default)]
    pub inverse_functional: bool,
    /// The surface wordings folded into this relation ("available_on", "available through", ...)
    pub forms: Vec<String>,
    /// `relation` (the default) or `attribute`.
    ///
    /// Attributes take the other rewrite path: the object is a literal, and it has to be
    /// converted per the datatype before it can land on the new attribute; the domain does not
    /// come from the request either, it is taken from the subject types of the facts
    #[serde(default)]
    pub kind: Option<String>,
    /// Attributes only: text | number | date | bool
    #[serde(default)]
    pub datatype: Option<String>,
    #[serde(default)]
    pub unit: Option<String>,
}

/// Adopt a surface predicate: create the relation type **and rewrite the related_to facts that
/// were waiting on it**.
///
/// The second half is the whole difference from a plain create. Create the type only and the
/// ontology has grown while the graph is no better -- those 57 facts go on saying "is related
/// to". The rewrite appends (a new row + supersedes), so the entity history reads "recorded as
/// related to first, refined to available on later".
///
/// When `existing` is true the first half is skipped: the ontology already carries this
/// meaning, and this time only the rewrite happens. That is the line between consolidation and
/// growth -- one entry point, because what it does to the graph is exactly the same.
pub async fn adopt_predicate(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(kb_id): Path<Uuid>,
    Json(req): Json<AdoptReq>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Editor).await?;
    let key = req.key.trim();
    if req.forms.is_empty() {
        return Err(AppError::invalid("forms_required", "forms cannot be empty").into());
    }
    if req.kind.as_deref() == Some("attribute") {
        return adopt_attribute(&state, &user, kb_id, &req).await;
    }
    let predicate_id = if req.existing {
        // Look up the one that already exists, by key. If it is not there, error out rather
        // than falling back to creating -- what the frontend said was "map onto the existing
        // one", and silently turning that into a create is the outcome it wants least
        utopia_store::ontology::relation_type_id_by_key(&state.pool, kb_id, key)
            .await?
            .ok_or_else(|| {
                AppError::invalid("unknown_relation_key", "no relation type with that key")
            })?
    } else {
        utopia_store::ontology::create_relation_type(
            &state.pool,
            kb_id,
            key,
            req.label.trim(),
            &req.temporal,
            // Adopting a proposal does not declare axioms on a person's behalf. **functional is
            // the one exception**, because it is something this form asked about anyway; the
            // other four have to be ticked explicitly on the relation page -- the criteria the
            // reasoner judges by have to be written down by a person, not tacked on in passing
            // at adoption time
            utopia_core::models::RelationAxioms {
                functional: req.functional,
                inverse_functional: req.inverse_functional,
                ..Default::default()
            },
            req.description.as_deref().unwrap_or("").trim(),
            "relation",
            // Proposals and cold start only create the relation, they declare no domain/range
            // -- left empty = no restriction on subject or object type
            &[],
            &[],
            None,
            None,
        )
        .await?
    };
    // **The manual path does not swap subject and object.**
    //
    // Not because it never needs to -- mapping `produced_by` onto `produces` ought to swap just
    // the same -- but because the forms here are what a person ticked on the panel, and he may
    // perfectly well have ticked both `produced` and `produced_by`, which one swap flag cannot
    // serve. The automatic path does not have this problem: wordings in one group share an
    // inflectional stem, so whether the ending carries `by` is necessarily consistent. Fixing
    // this properly means making adopt decide the direction wording by wording, which is a
    // different piece of work.
    let utopia_store::graph::Adopted {
        batch_id,
        moved: remapped,
        left_off,
        corrected,
    } = utopia_store::graph::adopt_proposed_predicates(
        &state.pool,
        kb_id,
        predicate_id,
        &req.forms,
        false,
    )
    .await?;
    // Adoption clears the matching unmatched counts at the same time -- the ontology covers them
    // now
    for form in &req.forms {
        let _ = utopia_store::ontology::clear_miss(&state.pool, kb_id, "relation_type", form).await;
    }
    let _ = utopia_store::audit::record(
        &state.pool,
        Some(kb_id),
        user.id,
        "ontology.predicate_adopted",
        "relation_type",
        Some(predicate_id),
        json!({
            "key": key, "label": req.label, "forms": req.forms,
            "facts_remapped": remapped, "facts_left_off": left_off,
            "facts_direction_corrected": corrected, "batch": batch_id,
        }),
    )
    .await;
    state.emit_review(kb_id);
    Ok(Json(json!({
        "id": predicate_id, "remapped": remapped, "left_off": left_off,
        "corrected": corrected, "batch": batch_id,
    })))
}

/// Undo an adoption: the newly written rows are voided, the old rows come back. The relation
/// type stays (facts have pointed at it, and a relation nobody uses is inert).
pub async fn unadopt_predicate(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path((kb_id, batch_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Editor).await?;
    // Attribution has to find this undo by relation type, so get hold of that first
    let predicate_id: Option<(Uuid,)> = sqlx::query_as(
        "SELECT predicate_id FROM fact_adoptions WHERE batch_id = $1 AND kb_id = $2 LIMIT 1",
    )
    .bind(batch_id)
    .bind(kb_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(utopia_core::AppError::Db)?;
    // One adoption can produce both fact-rewrite and entity-retype batches, and what the caller
    // is given is a list of batch ids with no kind attached -- so try both here, and whichever
    // claims it is the one that takes effect
    if predicate_id.is_none() {
        let n = utopia_store::resolution::unadopt_types(&state.pool, kb_id, batch_id).await?;
        let _ = utopia_store::audit::record(
            &state.pool,
            Some(kb_id),
            user.id,
            "ontology.adoption_reverted",
            "kb",
            Some(kb_id),
            json!({ "batch": batch_id, "entities_reverted": n }),
        )
        .await;
        state.emit_review(kb_id);
        return Ok(Json(json!({ "reverted": n })));
    }
    let reverted = utopia_store::graph::unadopt(&state.pool, kb_id, batch_id).await?;
    let _ = utopia_store::audit::record(
        &state.pool,
        Some(kb_id),
        user.id,
        "ontology.adoption_reverted",
        "relation_type",
        predicate_id.map(|(id,)| id),
        json!({ "batch": batch_id, "facts_reverted": reverted }),
    )
    .await;
    state.emit_review(kb_id);
    Ok(Json(json!({ "reverted": reverted })))
}

/// The surface predicates waiting to be claimed: the source text said it, the ontology does not
/// have it, and the facts got demoted to related_to.
pub async fn proposed_predicates(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(kb_id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Viewer).await?;
    let forms = utopia_store::graph::proposed_predicates(&state.pool, kb_id).await?;
    Ok(Json(json!({ "forms": forms })))
}

/// What the last automatic ontology extension did, and whether it can still be undone.
///
/// Being on by default is conditional on its actions being **visible and reversible**. Recorded
/// in the audit ledger and nowhere else does not count as visible -- that is for verifying after
/// the fact, not for telling anyone. This gives the Ontology page an explicit banner.
pub async fn last_auto_extension(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(kb_id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Viewer).await?;
    let row: Option<(serde_json::Value, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(
        "SELECT detail, created_at FROM audit_events
         WHERE kb_id = $1 AND action = 'ontology.bootstrapped'
         ORDER BY id DESC LIMIT 1",
    )
    .bind(kb_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(AppError::Db)?;
    let Some((detail, at)) = row else {
        return Ok(Json(json!({ "run": null })));
    };
    // A run that has been undone cleanly is not announced any more -- that round has left no
    // trace on the graph at all
    let batches: Vec<Uuid> = detail
        .get("batches")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str().and_then(|x| x.parse().ok()))
                .collect()
        })
        .unwrap_or_default();
    let (live,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM fact_adoptions
         WHERE kb_id = $1 AND batch_id = ANY($2) AND reverted_at IS NULL",
    )
    .bind(kb_id)
    .bind(&batches)
    .fetch_one(&state.pool)
    .await
    .map_err(AppError::Db)?;
    if live == 0 {
        return Ok(Json(json!({ "run": null })));
    }
    Ok(Json(json!({ "run": {
        "at": at,
        "relations": detail.get("relations"),
        "classes": detail.get("classes"),
        "facts_remapped": detail.get("facts_remapped"),
        "batches": batches,
    }})))
}

/// OWL import: see what will happen first, write only once it is confirmed.
///
/// **Uploading a file must never irreversibly change the ontology** -- preview and write go
/// through the same plan, because two independent paths drift apart sooner or later, and what
/// drift means is that what happens after you confirm is not what you just looked at.
pub async fn preview_import(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(kb_id): Path<Uuid>,
    multipart: axum::extract::Multipart,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Editor).await?;
    let (filename, bytes) = read_upload(multipart).await?;
    let (plan, _, _) = crate::owl_import::plan(&state, kb_id, &filename, &bytes).await?;
    Ok(Json(json!({ "filename": filename, "plan": plan })))
}

pub async fn apply_import(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(kb_id): Path<Uuid>,
    multipart: axum::extract::Multipart,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Editor).await?;
    let (filename, bytes) = read_upload(multipart).await?;
    let (import_id, plan) =
        crate::owl_import::apply(&state, kb_id, user.id, &filename, &bytes).await?;
    // The axioms have just changed, which makes this the moment consistency most wants
    // recomputing -- what the user imported is the criteria themselves. A failure does not
    // affect the import itself: the ontology is already stored, the check failing to run is a
    // separate matter, and that button on the Review page can still run it again
    let violations = match utopia_store::reasoning::run(&state.pool, kb_id).await {
        Ok(r) => r.found,
        Err(e) => {
            tracing::warn!(
                ?e,
                "The consistency check after the import did not go through"
            );
            0
        }
    };
    state.emit_review(kb_id);
    Ok(Json(
        json!({ "import_id": import_id, "plan": plan, "violations": violations }),
    ))
}

pub async fn list_imports(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(kb_id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Viewer).await?;
    let imports = utopia_store::ontology::list_imports(&state.pool, kb_id).await?;
    Ok(Json(json!({ "imports": imports })))
}

/// Take the first file out of the multipart. Capped at 8 MB -- FOAF is 44 KB, DCTerms 48 KB, and
/// even a doorstop like FIBO runs to a few hundred KB per module; bigger than that is most
/// likely the wrong file.
const MAX_ONTOLOGY_BYTES: usize = 8 * 1024 * 1024;

async fn read_upload(
    mut multipart: axum::extract::Multipart,
) -> Result<(String, Vec<u8>), AppError> {
    while let Some(field) = multipart.next_field().await.map_err(|e| {
        AppError::invalid_detail("bad_upload", "Could not read the upload", e.to_string())
    })? {
        let Some(filename) = field.file_name().map(String::from) else {
            continue;
        };
        let bytes = field.bytes().await.map_err(|e| {
            AppError::invalid_detail(
                "upload_read_failed",
                "Could not read the uploaded file",
                e.to_string(),
            )
        })?;
        if bytes.len() > MAX_ONTOLOGY_BYTES {
            return Err(AppError::invalid(
                "file_too_large",
                "Ontology file is too large (max 8 MB)",
            ));
        }
        if bytes.is_empty() {
            return Err(AppError::invalid("empty_file", "Ontology file is empty"));
        }
        return Ok((filename, bytes.to_vec()));
    }
    Err(AppError::invalid("no_files", "No file in the upload"))
}

/// Language code → the name written into the prompt for the model. A model knows what "Chinese"
/// means; it may well not know what "zh" means.
fn lang_name(code: &str) -> &'static str {
    match code {
        "zh" => "Chinese",
        _ => "English",
    }
}

/// Adopt a **literal-value** wording: create (or point at an existing) attribute, and move the
/// facts that were waiting on it across.
///
/// Three things differ from the relation path, and every one of them is specific to attributes:
///
/// 1. **The domain is taken from the data, not carried by the request.** An attribute has to
///    declare which classes it can hang off, and the cost of guessing wrong is hard -- a fact
///    whose subject type does not match is thrown away whole (`attr_domain_mismatch`).
///    What class the subjects of these facts are in right now is a fact, not a judgement: read
///    it.
/// 2. **The value has to be converted per the datatype.** What the database holds is exactly
///    what extraction saw at the time (the string "2015"); landing on a date attribute means
///    becoming a date first.
/// 3. **What will not convert does not get rewritten.** Better to leave it with no predicate
///    and wait for next time than to force a value that will not convert into a typed attribute
///    -- that is the same "rather empty than dirty" rule.
async fn adopt_attribute(
    state: &AppState,
    user: &utopia_core::models::User,
    kb_id: Uuid,
    req: &AdoptReq,
) -> ApiResult<Json<serde_json::Value>> {
    let spec = AttributeAdoption {
        key: req.key.trim(),
        label: req.label.trim(),
        description: req.description.as_deref().unwrap_or("").trim(),
        datatype: req.datatype.as_deref().unwrap_or("text"),
        unit: req.unit.as_deref().map(str::trim).filter(|s| !s.is_empty()),
        forms: &req.forms,
        existing: req.existing,
    };
    let done = adopt_attribute_core(state, kb_id, &spec).await?;
    let _ = utopia_store::audit::record(
        &state.pool,
        Some(kb_id),
        user.id,
        "ontology.attribute_adopted",
        "relation_type",
        Some(done.attribute_id),
        json!({ "key": spec.key, "forms": req.forms, "existing": req.existing,
                "remapped": done.remapped, "unconvertible": done.unconvertible }),
    )
    .await;
    // unconvertible goes back to the caller: 3 rewritten, 2 left behind -- the UI has to be able
    // to say the second half too
    Ok(Json(json!({
        "id": done.attribute_id, "batch": done.batch_id,
        "remapped": done.remapped, "unconvertible": done.unconvertible
    })))
}

/// Everything one attribute adoption takes as input.
pub(crate) struct AttributeAdoption<'a> {
    pub key: &'a str,
    pub label: &'a str,
    pub description: &'a str,
    pub datatype: &'a str,
    pub unit: Option<&'a str>,
    pub forms: &'a [String],
    /// true = the key refers to an attribute that exists; rewrite only, create nothing
    pub existing: bool,
}

pub(crate) struct AttributeAdopted {
    pub attribute_id: Uuid,
    pub batch_id: Uuid,
    pub remapped: u32,
    /// How many were **not** rewritten because their value would not convert to that datatype.
    /// This has to travel upwards: 3 rewritten, 2 left behind -- reporting only the first half
    /// is announcing the good news and burying the bad
    pub unconvertible: usize,
}

/// Create (or point at an existing) attribute, and move the literal-value facts that were
/// waiting on it across. Shared by the manual and the automatic path.
pub(crate) async fn adopt_attribute_core(
    state: &AppState,
    kb_id: Uuid,
    spec: &AttributeAdoption<'_>,
) -> Result<AttributeAdopted, AppError> {
    // Pull the facts to be rewritten out first: they settle the domain, and they settle whether
    // the values will convert
    let facts = utopia_store::graph::value_facts_for_forms(&state.pool, kb_id, spec.forms).await?;
    let attribute_id = if spec.existing {
        utopia_store::ontology::relation_type_id_by_key(&state.pool, kb_id, spec.key)
            .await?
            .ok_or_else(|| {
                AppError::invalid("unknown_relation_key", "no relation type with that key")
            })?
    } else {
        // **The domain is taken from the data.** An attribute has to declare which classes it
        // can hang off, and the cost of guessing wrong is hard: a fact whose subject type does
        // not match is thrown away whole. What class the subjects of these facts are in right
        // now is a fact, not a judgement
        let mut domains: Vec<Uuid> = facts.iter().map(|(_, type_id, _)| *type_id).collect();
        domains.sort_unstable();
        domains.dedup();
        if domains.is_empty() {
            return Err(AppError::invalid(
                "no_facts_for_forms",
                "nothing is waiting on those wordings",
            ));
        }
        utopia_store::ontology::create_relation_type(
            &state.pool,
            kb_id,
            spec.key,
            spec.label,
            "state",
            // The suggester does not decide for the temporal engine: functional drives it to
            // close old values off automatically, and the remaining axioms drive the reasoner --
            // both of which should be declared explicitly by a person
            Default::default(),
            spec.description,
            "attribute",
            &domains,
            &[],
            Some(spec.datatype),
            spec.unit,
        )
        .await?
    };

    // Conversion follows the datatype of **the row in the database**, not of the request -- when
    // pointing at an attribute that already exists the request has no datatype at all, and even
    // if it did, the ontology is what to listen to
    let datatype = utopia_store::ontology::relation_type_datatype(&state.pool, attribute_id)
        .await?
        .unwrap_or_else(|| "text".to_string());
    let mut rewrites: Vec<(Uuid, serde_json::Value)> = Vec::new();
    let mut unconvertible = 0usize;
    for (fact_id, _, object_value) in &facts {
        // The shape extraction wrote is {"value": ...}, so take the inner layer to convert
        let raw = object_value.get("value").unwrap_or(object_value);
        match utopia_extract::normalize_attr_value(&datatype, raw) {
            Some(v) => rewrites.push((*fact_id, json!({ "value": v }))),
            // What will not convert is **not rewritten**: better to leave it with no predicate
            // and wait for next time than to force a value that will not convert into a typed
            // attribute
            None => unconvertible += 1,
        }
    }
    // The attribute route has no signature to judge by (the object is a literal), so `left_off`
    // is always 0
    let utopia_store::graph::Adopted {
        batch_id,
        moved: remapped,
        ..
    } = utopia_store::graph::adopt_value_facts(&state.pool, kb_id, attribute_id, &rewrites).await?;
    for form in spec.forms {
        let _ =
            utopia_store::ontology::clear_miss(&state.pool, kb_id, "attribute_type", form).await;
    }
    Ok(AttributeAdopted {
        attribute_id,
        batch_id,
        remapped,
        unconvertible,
    })
}

/// Map onto an attribute that **already exists**: create nothing, just move these wordings'
/// literal-value facts across.
pub(crate) async fn adopt_attribute_existing(
    state: &AppState,
    kb_id: Uuid,
    key: &str,
    forms: &[String],
) -> Result<(Uuid, u32), AppError> {
    let done = adopt_attribute_core(
        state,
        kb_id,
        &AttributeAdoption {
            key,
            label: "",
            description: "",
            datatype: "text",
            unit: None,
            forms,
            existing: true,
        },
    )
    .await?;
    Ok((done.batch_id, done.remapped))
}

/// The entry point for the automatic ontology extension path. The parameters are spread out
/// rather than passed as an `AdoptReq` -- that struct is an HTTP request body, and the automatic
/// path has no request.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn adopt_attribute_auto(
    state: &AppState,
    kb_id: Uuid,
    key: &str,
    label: &str,
    description: &str,
    datatype: &str,
    unit: Option<&str>,
    forms: &[String],
) -> Result<(Uuid, u32), AppError> {
    let done = adopt_attribute_core(
        state,
        kb_id,
        &AttributeAdoption {
            key,
            label,
            description,
            datatype,
            unit,
            forms,
            existing: false,
        },
    )
    .await?;
    if done.unconvertible > 0 {
        tracing::info!(
            %kb_id, key, dropped = done.unconvertible,
            "Some values would not convert to this datatype; those facts still have no predicate"
        );
    }
    Ok((done.batch_id, done.remapped))
}

/// The **compute but do not write** step of type resolution: the profile and the candidate
/// classes for every entity waiting to be refined.
///
/// The same pattern as the ontology import: look at the plan, then decide whether to commit. It
/// earns one more use here -- when retrieval finds nothing, the response carries "this is what
/// we went looking with", so one glance tells you whether to fix the profile or fix the class
/// descriptions.
pub async fn type_resolution_preview(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(kb_id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Editor).await?;
    let items = crate::type_resolution::preview(&state, kb_id).await?;
    Ok(Json(json!({ "items": items })))
}

/// Run type resolution and write it: retrieve candidates → adjudicate → three-way disposition.
///
/// Keeping it apart from preview is the shape this repository already has (the ontology import
/// shows the plan before writing too). There is one more reason here: a retype **does not go on
/// the timeline**, so unlike a fact rewrite it does not show itself in the entity history, and
/// looking before acting is the only chance there is to see it.
pub async fn type_resolution_apply(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(kb_id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Editor).await?;
    let outcome = crate::type_resolution::resolve(&state, kb_id).await?;
    let _ = utopia_store::audit::record(
        &state.pool,
        Some(kb_id),
        user.id,
        "ontology.types_resolved",
        "knowledge_base",
        Some(kb_id),
        json!({ "retyped": outcome.retyped, "for_review": outcome.for_review.len(),
                "left_alone": outcome.left_alone.len(), "batch": outcome.batch }),
    )
    .await;
    Ok(Json(json!(outcome)))
}

/// Undo one type resolution: put that batch of entities back into the class they were in.
pub async fn type_resolution_undo(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path((kb_id, batch_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Editor).await?;
    let reverted = utopia_store::resolution::unadopt_types(&state.pool, kb_id, batch_id).await?;
    let _ = utopia_store::audit::record(
        &state.pool,
        Some(kb_id),
        user.id,
        "ontology.types_resolution_reverted",
        "knowledge_base",
        Some(kb_id),
        json!({ "batch": batch_id, "reverted": reverted }),
    )
    .await;
    Ok(Json(json!({ "reverted": reverted })))
}

#[derive(Deserialize)]
pub struct ApproveRefinementReq {
    pub from_type_id: Uuid,
    pub to_type_id: Uuid,
    /// The entities to change over in the same call. **What gets approved is the class pair;
    /// what gets changed is the entities** -- the two are separate, so a caller can approve the
    /// rule and leave every entity alone for now
    #[serde(default)]
    pub entity_ids: Vec<Uuid>,
}

/// Approve a "coarse class → fine class" pair, and move the entities that came with the request.
///
/// The needs-a-person bucket is triggered by "did this cross a classification axis", and in
/// practice what that criterion mostly measures is **whether the seed classes got wired up to
/// the imported vocabulary**, not risk -- schema.org's Place starts a key of its own, so every
/// single city has to be asked about. Approve the pair once and it stops asking: that is a
/// judgement between one class and another, and the entities merely happen to run into it.
pub async fn approve_refinement(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(kb_id): Path<Uuid>,
    Json(req): Json<ApproveRefinementReq>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Editor).await?;
    utopia_store::resolution::approve_refinement(
        &state.pool,
        kb_id,
        req.from_type_id,
        req.to_type_id,
        user.id,
    )
    .await?;
    let picks: Vec<(Uuid, Uuid)> = req
        .entity_ids
        .iter()
        .map(|id| (*id, req.to_type_id))
        .collect();
    let (batch, moved) = if picks.is_empty() {
        (None, 0)
    } else {
        let (b, n) =
            utopia_store::resolution::retype_entities(&state.pool, kb_id, &picks, Some(user.id))
                .await?;
        (Some(b), n)
    };
    let _ = utopia_store::audit::record(
        &state.pool,
        Some(kb_id),
        user.id,
        "ontology.refinement_approved",
        "entity_type",
        Some(req.to_type_id),
        json!({ "from": req.from_type_id, "to": req.to_type_id, "moved": moved }),
    )
    .await;
    Ok(Json(json!({ "moved": moved, "batch": batch })))
}

#[cfg(test)]
mod tests {
    use super::{normalize_name, resolve_map_targets};
    use serde_json::json;
    use std::collections::{HashMap, HashSet};

    #[test]
    fn names_align_across_spellings() {
        // In imported ontologies the label is often just the camelCase spelling of the key, and
        // the two have to collapse together
        assert_eq!(
            normalize_name("acquiredFrom"),
            normalize_name("acquired_from")
        );
        assert_eq!(
            normalize_name("Acquired From"),
            normalize_name("acquired_from")
        );
        // Different names stay different -- this step only lines spellings up, it makes no
        // synonym judgement
        assert_ne!(normalize_name("acquired_from"), normalize_name("acquires"));
        // Chinese labels survive verbatim (they cannot be stripped, and should not be)
        assert_eq!(normalize_name("员工数"), "员工数");
    }

    #[test]
    fn a_map_target_outside_the_candidates_is_dropped() {
        let by_name: HashMap<String, String> = [
            ("acquiredfrom".to_string(), "acquired_from".to_string()),
            ("员工数".to_string(), "headcount".to_string()),
        ]
        .into_iter()
        .collect();
        let mut p = json!({"map_to": [
            // The label got copied instead of the key -- this is exactly what the model does in
            // practice, and it has to match back
            {"key": "acquiredFrom", "forms": ["acquired from"]},
            // Copied from the Chinese label; that matches back just the same
            {"key": "员工数", "forms": ["员工总数"]},
            // Not in the candidate table: that button in the UI would promise something it
            // cannot do, so the whole item is dropped
            {"key": "invented_key", "forms": ["whatever"]},
            // No key at all
            {"forms": ["x"]},
        ]});
        resolve_map_targets(&mut p, &by_name, &HashMap::new());
        let items = p["map_to"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["key"], "acquired_from");
        assert_eq!(items[1]["key"], "headcount");
    }

    #[test]
    fn proposals_without_a_map_to_section_are_left_alone() {
        let mut p = json!({"entity_types": [], "relation_types": []});
        resolve_map_targets(&mut p, &HashMap::new(), &HashMap::new());
        assert!(p.get("map_to").is_none());
    }

    #[test]
    fn a_wording_that_carries_a_value_cannot_also_become_a_relation() {
        // A shape seen in practice: the model proposes the same founded_in as a relation and as
        // an attribute. Adopt both and the same batch of facts gets claimed twice, whichever
        // runs first wins
        let value_only: HashSet<&str> = ["founded_in", "registered_capital"].into_iter().collect();
        let entity_only: HashSet<&str> = ["acquires"].into_iter().collect();
        let mut p = json!({
            "relation_types": [
                {"key": "founded", "forms": ["founded_in", "founding date"]},
                {"key": "acquires", "forms": ["acquires"]}
            ],
            "attribute_types": [
                {"key": "founding_year", "forms": ["founded_in"]},
                {"key": "registered_capital", "forms": ["registered_capital"]}
            ]
        });
        super::keep_forms(&mut p, "relation_types", &entity_only, &value_only);
        super::keep_forms(&mut p, "attribute_types", &value_only, &entity_only);

        let rels = p["relation_types"].as_array().unwrap();
        // founded is left with only "founding date" -- neither side recognizes that wording, and
        // it is not ours to veto for the model
        assert_eq!(rels.len(), 2);
        assert_eq!(rels[0]["forms"].as_array().unwrap().len(), 1);
        assert_eq!(rels[0]["forms"][0], "founding date");
        assert_eq!(rels[1]["key"], "acquires");
        // Nothing on the attribute side moves
        assert_eq!(p["attribute_types"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn a_proposal_left_with_no_wordings_is_dropped() {
        // forms stripped bare = the "rewrite N of them" it promised is already zero, and keeping
        // it only earns someone a click where nothing happens
        let value_only: HashSet<&str> = ["founded_in"].into_iter().collect();
        let mut p = json!({"relation_types": [{"key": "founded", "forms": ["founded_in"]}]});
        super::keep_forms(&mut p, "relation_types", &HashSet::new(), &value_only);
        assert!(p["relation_types"].as_array().unwrap().is_empty());
    }

    #[test]
    fn a_type_proposal_without_wordings_is_left_alone() {
        // A proposal with no forms is just "add a type": it rewrites no facts, so which category
        // it belongs in does not apply
        let mut p = json!({"entity_types": [{"key": "platform", "label": "Platform"}]});
        super::keep_forms(&mut p, "entity_types", &HashSet::new(), &HashSet::new());
        assert_eq!(p["entity_types"].as_array().unwrap().len(), 1);
    }
}
