use axum::extract::{Path, Query, State};
use axum::Json;
use serde::Deserialize;
use serde_json::json;
use utopia_core::models::{KnowledgeBase, Role, User};
use utopia_core::AppError;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::error::ApiResult;
use crate::state::AppState;

#[derive(Deserialize)]
pub struct CreateKbReq {
    pub name: String,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub visibility: Option<String>,
    /// The ids of the preset ontology packs, installed in the order given. Empty = only the
    /// ten seeds.
    ///
    /// **The order matters**: the classes of the first pack claim the seed classes of the
    /// same name (which have no IRI), and later packs consult the alignment table when names
    /// collide. Put schema.org first, or the other packs have nothing to line up with.
    #[serde(default)]
    pub ontology_packs: Vec<String>,
}

#[derive(Deserialize)]
pub struct UpdateKbReq {
    pub name: Option<String>,
    pub description: Option<String>,
    pub visibility: Option<String>,
    /// The auto-extend-ontology switch (on by default; turning it off does not affect the
    /// "noticing", it only turns the result into a proposal you have to click)
    #[serde(default)]
    pub auto_extend_ontology: Option<bool>,
    /// Ontology language (`en` | `zh`): follows the corpus, not the UI. See docs/decisions/0004
    #[serde(default)]
    pub ontology_lang: Option<String>,
    /// The materialised-inference switch (off by default). See docs/decisions/0002 R1
    #[serde(default)]
    pub materialize_inferences: Option<bool>,
    /// How often to re-infer (minutes). Facts keep changing, and relying on hand-clicks
    /// alone leaves the derivations permanently missing
    #[serde(default)]
    pub inference_interval_minutes: Option<i32>,
}

/// The KBs visible to a user (a restricted KB is visible only to matrix members and system
/// admins).
pub async fn list(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(workspace_id): Path<Uuid>,
) -> ApiResult<Json<Vec<KnowledgeBase>>> {
    utopia_store::workspaces::require_role(&state.pool, user.id, workspace_id, Role::Viewer)
        .await?;
    let list =
        utopia_store::kbs::list_visible(&state.pool, workspace_id, user.id, user.is_admin).await?;
    Ok(Json(list))
}

/// Creating a KB: a deployment admin (workspace Admin+ or a system admin). The creator
/// automatically enters the matrix as admin.
pub async fn create(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(workspace_id): Path<Uuid>,
    Json(req): Json<CreateKbReq>,
) -> ApiResult<Json<KnowledgeBase>> {
    let name = req.name.trim();
    if name.is_empty() || name.chars().count() > 64 {
        return Err(AppError::invalid("bad_name", "Name must be 1-64 characters").into());
    }
    let kind = req.kind.as_deref().unwrap_or("knowledge");
    if !matches!(kind, "knowledge" | "memory") {
        return Err(AppError::Validation("kind must be 'knowledge' or 'memory'".into()).into());
    }
    // Creating a KB is a deployment-administration action (the entry point is in System
    // settings): a system admin, or workspace Admin+.
    // A KB a user creates for themselves defaults to restricted in the frontend, so it does
    // not pollute everyone's switcher; General, created by the system at init, stays open
    let ws_role =
        utopia_store::workspaces::require_role(&state.pool, user.id, workspace_id, Role::Viewer)
            .await?;
    if !user.is_admin && ws_role < Role::Admin {
        return Err(AppError::Forbidden.into());
    }
    let kb = utopia_store::kbs::create(
        &state.pool,
        workspace_id,
        name,
        kind,
        req.description.as_deref(),
    )
    .await?;
    if let Some(v) = req.visibility.as_deref() {
        utopia_store::kbs::update(
            &state.pool,
            kb.id,
            None,
            None,
            Some(v),
            None,
            None,
            None,
            None,
        )
        .await?;
    }
    utopia_store::access::set_kb_member(&state.pool, kb.id, user.id, "admin", Some(user.id))
        .await?;
    install_packs(&state, kb.id, user.id, &req.ontology_packs).await?;
    let kb = utopia_store::kbs::get(&state.pool, kb.id).await?;
    Ok(Json(kb))
}

/// The panorama of my knowledge bases (account level): visible KBs + my role + joining
/// information + overview stats.
pub async fn my_kbs(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(workspace_id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    utopia_store::workspaces::require_role(&state.pool, user.id, workspace_id, Role::Viewer)
        .await?;
    let kbs =
        utopia_store::kbs::list_visible(&state.pool, workspace_id, user.id, user.is_admin).await?;
    let ids: Vec<Uuid> = kbs.iter().map(|k| k.id).collect();
    let infos = utopia_store::access::my_kb_infos(&state.pool, &ids, user.id).await?;
    let mut rows = Vec::with_capacity(kbs.len());
    for kb in &kbs {
        let info = infos.iter().find(|i| i.kb_id == kb.id);
        let role = utopia_store::access::kb_role(&state.pool, &user, kb).await?;
        rows.push(json!({
            "kb": kb,
            "my_role": role.map(|r| r.as_str()),
            "joined_at": info.and_then(|i| i.joined_at),
            "added_by_name": info.and_then(|i| i.added_by_name.clone()),
            "doc_count": info.map(|i| i.doc_count).unwrap_or(0),
            "member_count": info.map(|i| i.member_count).unwrap_or(0),
        }));
    }
    Ok(Json(json!({ "kbs": rows })))
}

async fn kb_with_role(
    state: &AppState,
    user: &User,
    kb_id: Uuid,
    min: Role,
) -> Result<KnowledgeBase, AppError> {
    utopia_store::access::require_kb(&state.pool, user, kb_id, min).await
}

/// The detail response carries the caller's role in this KB: the frontend gates the entry
/// points to destructive operations (rebuild/delete) on it, instead of letting the user click
/// all the way through only to eat a 403.
pub async fn get_one(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    let kb = kb_with_role(&state, &user, id, Role::Viewer).await?;
    let role = utopia_store::access::kb_role(&state.pool, &user, &kb).await?;
    let mut body = serde_json::to_value(&kb).map_err(|e| AppError::Other(e.into()))?;
    body["my_role"] = json!(role.map(|r| r.as_str()));
    Ok(Json(body))
}

/// KB settings (name/description/visibility): KB admin and up.
pub async fn update(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateKbReq>,
) -> ApiResult<Json<KnowledgeBase>> {
    kb_with_role(&state, &user, id, Role::Admin).await?;
    let kb = utopia_store::kbs::update(
        &state.pool,
        id,
        req.name.as_deref().map(str::trim),
        req.description.as_deref(),
        req.visibility.as_deref(),
        req.auto_extend_ontology,
        req.ontology_lang.as_deref(),
        req.materialize_inferences,
        req.inference_interval_minutes,
    )
    .await?;
    // The audit is recorded, never blocking
    let _ = utopia_store::audit::record(
        &state.pool,
        Some(id),
        user.id,
        "kb.updated",
        "kb",
        Some(id),
        json!({ "name": req.name, "visibility": req.visibility }),
    )
    .await;
    Ok(Json(kb))
}

pub async fn delete(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    let kb = kb_with_role(&state, &user, id, Role::Admin).await?;
    utopia_store::kbs::delete(&state.pool, id).await?;
    // kb_id set to NULL: the KB is already cascade-deleted, so the event stays at the
    // deployment layer (the actor and the KB name are in detail)
    let _ = utopia_store::audit::record(
        &state.pool,
        None,
        user.id,
        "kb.deleted",
        "kb",
        Some(id),
        json!({ "name": kb.name }),
    )
    .await;
    Ok(Json(json!({ "ok": true })))
}

// ---------------------------------------------------------------------------
// The KB member matrix (the KB's own Settings → Members)
// ---------------------------------------------------------------------------

pub async fn members(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    kb_with_role(&state, &user, id, Role::Admin).await?;
    let members = utopia_store::access::kb_members(&state.pool, id).await?;
    Ok(Json(json!({ "members": members })))
}

#[derive(Deserialize)]
pub struct SetMemberReq {
    /// viewer | editor | admin
    pub role: String,
}

pub async fn set_member(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path((id, user_id)): Path<(Uuid, Uuid)>,
    Json(req): Json<SetMemberReq>,
) -> ApiResult<Json<serde_json::Value>> {
    kb_with_role(&state, &user, id, Role::Admin).await?;
    utopia_store::access::set_kb_member(&state.pool, id, user_id, &req.role, Some(user.id)).await?;
    let _ = utopia_store::audit::record(
        &state.pool,
        Some(id),
        user.id,
        "kb.member_set",
        "user",
        Some(user_id),
        json!({ "role": req.role }),
    )
    .await;
    Ok(Json(json!({ "ok": true })))
}

pub async fn remove_member(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path((id, user_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<serde_json::Value>> {
    kb_with_role(&state, &user, id, Role::Admin).await?;
    utopia_store::access::remove_kb_member(&state.pool, id, user_id).await?;
    let _ = utopia_store::audit::record(
        &state.pool,
        Some(id),
        user.id,
        "kb.member_removed",
        "user",
        Some(user_id),
        json!({}),
    )
    .await;
    Ok(Json(json!({ "ok": true })))
}

/// KB-level audit log (Admin and up; purely an audit display).
#[derive(Deserialize)]
pub struct AuditQuery {
    /// An action prefix. `entity.` scoops up the entity.retyped / entity.renamed family
    #[serde(default)]
    pub action: Option<String>,
    #[serde(default)]
    pub actor: Option<Uuid>,
    /// Start inclusive, end exclusive, in line with the half-open interval convention
    #[serde(default)]
    pub since: Option<String>,
    #[serde(default)]
    pub until: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub offset: Option<i64>,
}

/// Date parameters: by day (`2026-08-30`) or RFC3339, both accepted.
fn parse_day(raw: Option<&str>) -> Result<Option<chrono::DateTime<chrono::Utc>>, AppError> {
    let Some(s) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    if let Ok(d) = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Ok(Some(d.and_hms_opt(0, 0, 0).unwrap().and_utc()));
    }
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|t| Some(t.with_timezone(&chrono::Utc)))
        .map_err(|_| AppError::invalid("bad_date", "expected YYYY-MM-DD or RFC3339"))
}

/// The audit ledger. **Paged + filtered** -- this used to be a fixed most-recent 100, and a
/// ledger is compliance material, so "you can only see the last hundred" amounts to not being
/// able to look up history at all.
pub async fn audit_log(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
    Query(q): Query<AuditQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    kb_with_role(&state, &user, id, Role::Admin).await?;
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let offset = q.offset.unwrap_or(0).max(0);
    let action = q.action.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let (events, total) = utopia_store::audit::list_for_kb(
        &state.pool,
        id,
        action,
        q.actor,
        parse_day(q.since.as_deref())?,
        parse_day(q.until.as_deref())?,
        limit,
        offset,
    )
    .await?;
    // The dropdown is filled from the actions that have actually happened in this KB rather
    // than from a hard-coded list -- the latter would list a pile of options this KB has
    // never had
    let actions = utopia_store::audit::actions_for_kb(&state.pool, id).await?;
    Ok(Json(
        json!({ "events": events, "total": total, "actions": actions }),
    ))
}
/// Install the selected ontology packs at KB-creation time.
///
/// This used to run `ensure_default_ontology` first: the classes in a pack claim the seed
/// classes of the same name (`schema:Organization` takes over `organization`), and claiming
/// requires that row to already exist. **There are no seeds left to claim now** -- after 0009
/// deleted the built-in entity types, 0010 and `#125` deleted the seed relations, and 0011
/// moved `mapped_to` into the semantic layer, the seeding function itself left the stage too.
/// Packs land straight into an empty KB.
///
/// **One pack failing does not roll back the ones already installed**: an ontology is
/// additive, a half-installed KB is still usable, and rolling back would mean retracting
/// classes that have already been created -- which is exactly why 0008 decided against
/// undoing an import. The failure message names which pack it was, so a person knows where
/// to pick up.
async fn install_packs(
    state: &AppState,
    kb_id: Uuid,
    actor: Uuid,
    pack_ids: &[String],
) -> ApiResult<()> {
    if pack_ids.is_empty() {
        return Ok(());
    }
    let mut packs = Vec::with_capacity(pack_ids.len());
    for id in pack_ids {
        let pack = crate::ontology_packs::get(id)
            .ok_or_else(|| AppError::invalid("unknown_pack", format!("unknown pack: {id}")))?;
        packs.push((pack, crate::ontology_packs::bytes(pack)?));
    }
    for (pack, bytes) in &packs {
        crate::owl_import::apply(state, kb_id, actor, pack.filename, bytes)
            .await
            .map_err(|e| AppError::Other(anyhow::anyhow!("installing {} failed: {e}", pack.id)))?;
    }
    // Second pass: cross-pack domain / range. Packs are installed one by one, so an earlier
    // one cannot see the classes of a later one -- W3C Org's headOf has to wait for FOAF's
    // Agent (#222). With only one pack installed there is no "other pack"
    if packs.len() > 1 {
        for (pack, bytes) in &packs {
            let (d, r) =
                crate::owl_import::relink_domains_ranges(state, kb_id, pack.filename, bytes)
                    .await
                    .map_err(|e| {
                        AppError::Other(anyhow::anyhow!("{} relink failed: {e}", pack.id))
                    })?;
            tracing::debug!(%kb_id, pack = pack.id, domains = d, ranges = r, "cross-pack relink");
        }
    }
    Ok(())
}

/// The list of available ontology packs, for the KB-creation UI. Needs no permission beyond
/// being logged in -- it is static data.
pub async fn list_packs(AuthUser(_): AuthUser) -> ApiResult<Json<serde_json::Value>> {
    let packs: Vec<_> = crate::ontology_packs::PACKS
        .iter()
        .map(|p| {
            json!({
                "id": p.id,
                "name": p.name,
                "summary": p.summary,
                "classes": p.classes,
                "properties": p.properties,
            })
        })
        .collect();
    Ok(Json(json!({ "packs": packs })))
}
