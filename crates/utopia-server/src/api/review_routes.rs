//! Review queue API: suspected duplicate-pair resolution + low-confidence facts + merge log/revert.

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::Deserialize;
use serde_json::json;
use utopia_core::models::Role;
use uuid::Uuid;

use super::graph_routes::require_kb;
use crate::auth::AuthUser;
use crate::error::ApiResult;
use crate::state::AppState;

/// How many items per page. **A server-side default, not a cap** -- the front end may ask for
/// fewer; ask for more and the clamp stops it
const REVIEW_PAGE: i64 = 10;

/// A triple snapshot of both sides of a conflict: (old subject, old object, new subject,
/// new object, predicate label).
type ConflictSnapshot = (String, Option<String>, String, Option<String>, String);

/// A fact snapshot (for the decision ledger): after a reject the fact disappears from the graph,
/// so the ledger has to carry self-contained display text.
/// Always taken before the action runs.
async fn fact_snapshot(state: &AppState, kb_id: Uuid, fact_id: Uuid) -> Option<serde_json::Value> {
    // The object may be an entity or a literal; structured literals prefer summary (the same
    // display convention as the queue card)
    let row: Option<(String, Option<String>, Option<String>, f32)> = sqlx::query_as(
        "SELECT s.canonical_name, COALESCE(r.label, fact_surface_predicate(f.id)),
                COALESCE(o.canonical_name, f.object_value ->> 'summary',
                         f.object_value #>> '{}'), f.confidence
         FROM facts f
         JOIN entities s ON s.id = f.subject_id
         LEFT JOIN relation_types r ON r.id = f.predicate_id
         LEFT JOIN entities o ON o.id = f.object_id
         WHERE f.id = $1 AND f.kb_id = $2",
    )
    .bind(fact_id)
    .bind(kb_id)
    .fetch_optional(&state.pool)
    .await
    .ok()
    .flatten();
    row.map(|(s, p, o, c)| json!({ "subject": s, "predicate": p, "object": o, "confidence": c }))
}

#[derive(Deserialize)]
pub struct ReviewQuery {
    /// Which queue to look at. Defaults to duplicates
    #[serde(default)]
    pub queue: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub offset: Option<i64>,
}

/// The review queue: **counts and contents are fetched separately**.
///
/// This used to hand back all eight queues in one go, a fixed 100 rows each, with the front end
/// paginating client-side -- so the badge in the left column was the truncated number (164
/// low-confidence facts in the database, 100 on screen), and anything past page eleven did not
/// exist as far as the UI was concerned.
///
/// Now: the counts come back every time (eight COUNTs in one query, going through the same WHERE
/// clauses as the list), and the contents are only one page of the current queue. Switching
/// queues or turning a page each sends its own request; the price is a few more round trips, and
/// what it buys is **numbers that no longer lie**, plus a KB with a hundred thousand pending
/// items that you can actually page to the end of.
pub async fn list(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(kb_id): Path<Uuid>,
    Query(q): Query<ReviewQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Viewer).await?;
    let counts = utopia_store::review::counts(&state.pool, kb_id).await?;
    let queue = q.queue.as_deref().unwrap_or("duplicates");
    // The cap stops "limit=1000000 drags the database down" while leaving ample room for a page
    let limit = q.limit.unwrap_or(REVIEW_PAGE).clamp(1, 200);
    let offset = q.offset.unwrap_or(0).max(0);

    let items = match queue {
        // Facts extracted from a memory, waiting for a human nod (0015). First in the list:
        // these are the human's own words
        "pending" => json!(utopia_store::pending::list(&state.pool, kb_id, limit, offset).await?),
        "duplicates" => {
            json!(utopia_store::resolution::list_reviews(&state.pool, kb_id, limit, offset).await?)
        }
        "conflicts" => {
            json!(utopia_store::temporal::list_conflicts(&state.pool, kb_id, limit, offset).await?)
        }
        "unconfirmed" => {
            json!(utopia_store::graph::stale_facts(&state.pool, kb_id, limit, offset).await?)
        }
        "lowconf" => json!(
            utopia_store::graph::low_confidence_facts(
                &state.pool,
                kb_id,
                utopia_store::review::LOW_CONFIDENCE_BELOW,
                limit,
                offset,
            )
            .await?
        ),
        "mappings" => {
            json!(utopia_store::mappings::proposed(&state.pool, kb_id, limit, offset).await?)
        }
        "violations" => {
            json!(
                utopia_store::reasoning::open_violations(&state.pool, kb_id, limit, offset).await?
            )
        }
        "defects" => {
            json!(utopia_store::reasoning::open_defects(&state.pool, kb_id, limit, offset).await?)
        }
        "merges" => {
            json!(utopia_store::resolution::list_merges(&state.pool, kb_id, limit, offset).await?)
        }
        // An unrecognised queue name is reported as a contract error rather than quietly coming
        // back empty -- coming back quietly empty means one mistyped letter in the front end
        // looks like "this queue has been emptied"
        other => {
            return Err(utopia_core::AppError::invalid(
                "unknown_queue",
                format!("no review queue named {other}"),
            )
            .into())
        }
    };

    Ok(Json(json!({
        "counts": counts,
        "queue": queue,
        "items": items,
    })))
}

#[derive(Deserialize)]
pub struct CloseFactBody {
    pub valid_to: chrono::DateTime<chrono::Utc>,
}

/// Manually close a fact's validity interval ("this ended at such a time") -- via invalidate +
/// rewrite, the same mechanism as automatic closing, so the ledger stays replayable.
pub async fn close_fact(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path((kb_id, fact_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<CloseFactBody>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Editor).await?;
    // Ownership and status check: only a fact of this KB, not invalidated, with an open interval
    // can be closed
    let ok: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM facts
         WHERE id = $1 AND kb_id = $2 AND invalidated_at IS NULL AND valid_to IS NULL",
    )
    .bind(fact_id)
    .bind(kb_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(utopia_core::AppError::Db)?;
    if ok.is_none() {
        return Err(utopia_core::AppError::NotFound.into());
    }
    let snap = fact_snapshot(&state, kb_id, fact_id).await;
    // What a person picks in the UI is a date, so the closing point has day precision
    utopia_store::temporal::close_superseded(&state.pool, fact_id, body.valid_to, "day").await?;
    if let Some(mut d) = snap {
        d["valid_to"] = json!(body.valid_to.to_rfc3339());
        let _ = utopia_store::audit::record(
            &state.pool,
            Some(kb_id),
            user.id,
            "fact.close",
            "fact",
            Some(fact_id),
            d,
        )
        .await;
    }
    state.emit_review(kb_id);
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct ConflictBody {
    /// close | keep | reject_new
    pub action: String,
    /// Required when the action is close and the new fact has no start point
    #[serde(default)]
    pub close_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Temporal conflict adjudication (S3: the ones automatic closing could not be sure about).
pub async fn resolve_conflict(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path((kb_id, conflict_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<ConflictBody>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Editor).await?;
    // Snapshot both triples: adjudication invalidates/rewrites facts, so copy before touching
    let snap: Option<ConflictSnapshot> = sqlx::query_as(
        "SELECT os.canonical_name, oo.canonical_name, ns.canonical_name, no_.canonical_name,
                r.label
         FROM fact_conflicts c
         JOIN facts fo ON fo.id = c.old_fact_id
         JOIN facts fn_ ON fn_.id = c.new_fact_id
         JOIN entities os ON os.id = fo.subject_id
         LEFT JOIN entities oo ON oo.id = fo.object_id
         JOIN entities ns ON ns.id = fn_.subject_id
         LEFT JOIN entities no_ ON no_.id = fn_.object_id
         JOIN relation_types r ON r.id = fo.predicate_id
         WHERE c.id = $1 AND c.kb_id = $2",
    )
    .bind(conflict_id)
    .bind(kb_id)
    .fetch_optional(&state.pool)
    .await
    .ok()
    .flatten();
    utopia_store::temporal::resolve_conflict(
        &state.pool,
        kb_id,
        conflict_id,
        &body.action,
        body.close_at,
    )
    .await?;
    if let Some((os, oo, ns, no, pred)) = snap {
        let action = match body.action.as_str() {
            "close" => "conflict.close_old",
            "keep" => "conflict.keep_both",
            _ => "conflict.reject_new",
        };
        let _ = utopia_store::audit::record(
            &state.pool,
            Some(kb_id),
            user.id,
            action,
            "conflict",
            Some(conflict_id),
            json!({
                "predicate": pred,
                "old_subject": os, "old_object": oo,
                "new_subject": ns, "new_object": no,
                "close_at": body.close_at.map(|t| t.to_rfc3339()),
            }),
        )
        .await;
    }
    state.emit_review(kb_id);
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct DecideBody {
    /// merge | keep
    pub action: String,
}

pub async fn decide(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path((kb_id, review_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<DecideBody>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Editor).await?;
    let snap: Option<(String, String, f32)> = sqlx::query_as(
        "SELECT a.canonical_name, b.canonical_name, rr.score
         FROM resolution_reviews rr
         JOIN entities a ON a.id = rr.left_id
         JOIN entities b ON b.id = rr.right_id
         WHERE rr.id = $1 AND rr.kb_id = $2",
    )
    .bind(review_id)
    .bind(kb_id)
    .fetch_optional(&state.pool)
    .await
    .ok()
    .flatten();
    utopia_store::resolution::decide_review(&state.pool, kb_id, review_id, &body.action, user.id)
        .await?;
    if let Some((l, r, score)) = snap {
        let _ = utopia_store::audit::record(
            &state.pool,
            Some(kb_id),
            user.id,
            if body.action == "merge" {
                "review.merge"
            } else {
                "review.keep"
            },
            "review",
            Some(review_id),
            json!({ "left": l, "right": r, "score": score }),
        )
        .await;
    }
    state.emit_review(kb_id);
    Ok(Json(json!({ "ok": true })))
}

pub async fn confirm_fact(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path((kb_id, fact_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Editor).await?;
    let snap = fact_snapshot(&state, kb_id, fact_id).await;
    utopia_store::graph::confirm_fact(&state.pool, kb_id, fact_id).await?;
    if let Some(d) = snap {
        let _ = utopia_store::audit::record(
            &state.pool,
            Some(kb_id),
            user.id,
            "fact.confirm",
            "fact",
            Some(fact_id),
            d,
        )
        .await;
    }
    state.emit_review(kb_id);
    Ok(Json(json!({ "ok": true })))
}

pub async fn reject_fact(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path((kb_id, fact_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Editor).await?;
    let snap = fact_snapshot(&state, kb_id, fact_id).await;
    utopia_store::graph::reject_fact(&state.pool, kb_id, fact_id).await?;
    if let Some(d) = snap {
        let _ = utopia_store::audit::record(
            &state.pool,
            Some(kb_id),
            user.id,
            "fact.reject",
            "fact",
            Some(fact_id),
            d,
        )
        .await;
    }
    state.emit_review(kb_id);
    Ok(Json(json!({ "ok": true })))
}

pub async fn revert_merge(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path((kb_id, merge_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Editor).await?;
    let snap: Option<(String, String)> = sqlx::query_as(
        "SELECT s.canonical_name, t.canonical_name
         FROM entity_merges m
         JOIN entities s ON s.id = m.source_id
         JOIN entities t ON t.id = m.target_id
         WHERE m.id = $1 AND m.kb_id = $2",
    )
    .bind(merge_id)
    .bind(kb_id)
    .fetch_optional(&state.pool)
    .await
    .ok()
    .flatten();
    utopia_store::resolution::revert_merge(&state.pool, kb_id, merge_id).await?;
    if let Some((s, t)) = snap {
        let _ = utopia_store::audit::record(
            &state.pool,
            Some(kb_id),
            user.id,
            "merge.revert",
            "merge",
            Some(merge_id),
            json!({ "source": s, "target": t }),
        )
        .await;
    }
    state.emit_review(kb_id);
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct ManualMergeBody {
    pub source: Uuid,
    pub target: Uuid,
}

/// A manual merge (the entity panel's "Merge into…" entry point).
pub async fn manual_merge(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(kb_id): Path<Uuid>,
    Json(body): Json<ManualMergeBody>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Editor).await?;
    let snap: Option<(String,)> = sqlx::query_as(
        "SELECT s.canonical_name || ' → ' || t.canonical_name
         FROM entities s, entities t WHERE s.id = $1 AND t.id = $2",
    )
    .bind(body.source)
    .bind(body.target)
    .fetch_optional(&state.pool)
    .await
    .ok()
    .flatten();
    let merge_id = utopia_store::resolution::merge_entities(
        &state.pool,
        kb_id,
        body.source,
        body.target,
        Some(user.id),
        "manual merge",
    )
    .await?;
    if let Some((pair,)) = snap {
        let (s, t) = pair.split_once(" → ").unwrap_or((pair.as_str(), ""));
        let _ = utopia_store::audit::record(
            &state.pool,
            Some(kb_id),
            user.id,
            "merge.manual",
            "merge",
            Some(merge_id),
            json!({ "source": s, "target": t }),
        )
        .await;
    }
    state.emit_review(kb_id);
    Ok(Json(json!({ "merge_id": merge_id })))
}

#[derive(Deserialize)]
pub struct HistoryQuery {
    #[serde(default)]
    pub page: i64,
    #[serde(default = "default_history_per")]
    pub per: i64,
}

fn default_history_per() -> i64 {
    20
}

/// The decision ledger: audit events in the review domain, paginated on the server.
pub async fn history(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(kb_id): Path<Uuid>,
    Query(q): Query<HistoryQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Viewer).await?;
    let per = q.per.clamp(1, 100);
    let page = q.page.max(0);
    let (events, total) =
        utopia_store::audit::review_history(&state.pool, kb_id, per, page * per).await?;
    Ok(Json(json!({ "events": events, "total": total })))
}

#[derive(Deserialize)]
pub struct DecideMappingReq {
    /// confirmed | rejected
    pub status: String,
}

/// Take a position on one semantic-layer mapping (0011).
///
/// **The status changes, the row is not deleted**: a confirmation happened, and so did a
/// rejection. And keeping a trace of a rejection pays off immediately -- the next round of
/// exploration computes the rejected one again, and `propose`'s `WHERE status = 'proposed'`
/// uses that to keep it from being pushed back into the to-look-at pile.
pub async fn decide_mapping(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path((kb_id, mapping_id)): Path<(Uuid, Uuid)>,
    Json(req): Json<DecideMappingReq>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Editor).await?;
    if !matches!(req.status.as_str(), "confirmed" | "rejected") {
        return Err(utopia_core::AppError::invalid(
            "bad_status",
            "status must be confirmed or rejected",
        )
        .into());
    }
    utopia_store::mappings::decide(&state.pool, kb_id, mapping_id, &req.status, user.id).await?;
    let _ = utopia_store::audit::record(
        &state.pool,
        Some(kb_id),
        user.id,
        "mapping.decided",
        "concept_mapping",
        Some(mapping_id),
        json!({ "status": req.status }),
    )
    .await;
    state.emit_review(kb_id);
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct DecideViolationReq {
    /// fact_retracted | fact_closed | axiom_relaxed | accepted
    pub resolution: String,
    /// Required for `fact_closed`: which day the old assertion ends
    #[serde(default)]
    pub close_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Which one to retract for `fact_retracted`: required for two-fact and cycle violations,
    /// omittable for the single-fact ones (#202)
    #[serde(default)]
    pub fact_id: Option<Uuid>,
}

/// A human adjudicates one axiom violation.
///
/// **Three ways out, not two.** `axiom_relaxed` is unique to this queue: the contradiction may
/// lie in the definition rather than in the data -- the ontology the user imported declares some
/// property antisymmetric while in their own corpus that relation really is bidirectional. What
/// should change then is the ontology, not twenty facts.
///
/// **"The data is wrong" really does retract the fact** (#202): before, only the decision was
/// recorded, the fact went on living in the graph, and the queue stopped mentioning it. Which one
/// to retract is named by the request (`fact_id`), and may be omitted for the single-fact kinds;
/// after the retraction the check is recomputed, clearing the other violations that fact was
/// tangled up in. Changing an axiom still goes through the ontology page -- that is another
/// page's business.
pub async fn decide_violation(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path((kb_id, violation_id)): Path<(Uuid, Uuid)>,
    Json(req): Json<DecideViolationReq>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Editor).await?;
    if !matches!(
        req.resolution.as_str(),
        "fact_retracted" | "fact_closed" | "axiom_relaxed" | "accepted"
    ) {
        return Err(utopia_core::AppError::invalid(
            "bad_resolution",
            "resolution must be one of fact_retracted, fact_closed, axiom_relaxed, accepted",
        )
        .into());
    }
    let row: Option<(String, Uuid, Uuid, Vec<Uuid>)> = sqlx::query_as(
        "SELECT kind, left_fact, right_fact, path FROM axiom_violations
          WHERE id = $1 AND kb_id = $2 AND status = 'open'",
    )
    .bind(violation_id)
    .bind(kb_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(utopia_core::AppError::Db)?;
    let Some((kind, left, right, path)) = row else {
        return Err(utopia_core::AppError::NotFound.into());
    };
    let repaired = kind == "derived_contradiction";
    // The retract-the-fact path marks the violation resolved itself; the other ways out are all
    // recorded together below
    let mut decided = false;
    match (repaired, req.resolution.as_str()) {
        (_, "fact_retracted") => {
            let Some(target) =
                utopia_store::reasoning::pick_retraction(left, right, &path, req.fact_id)
            else {
                return Err(utopia_core::AppError::invalid(
                    "fact_required",
                    "several facts are involved: name which one to retract, from those listed",
                )
                .into());
            };
            let snap = fact_snapshot(&state, kb_id, target).await;
            utopia_store::reasoning::retract_from_violation(
                &state.pool,
                kb_id,
                violation_id,
                Some(target),
                user.id,
            )
            .await?;
            decided = true;
            if let Some(d) = snap {
                let _ = utopia_store::audit::record(
                    &state.pool,
                    Some(kb_id),
                    user.id,
                    "fact.reject",
                    "fact",
                    Some(target),
                    d,
                )
                .await;
            }
        }
        (true, "fact_closed") => {
            let Some(at) = req.close_at else {
                return Err(utopia_core::AppError::invalid(
                    "close_at_required",
                    "fact_closed requires an end date",
                )
                .into());
            };
            let open: Option<(Uuid,)> = sqlx::query_as(
                "SELECT id FROM facts
                  WHERE id = $1 AND invalidated_at IS NULL AND valid_to IS NULL",
            )
            .bind(left)
            .fetch_optional(&state.pool)
            .await
            .map_err(utopia_core::AppError::Db)?;
            if open.is_none() {
                return Err(utopia_core::AppError::invalid(
                    "not_open",
                    "this assertion already has an end date, or has been retracted",
                )
                .into());
            }
            let snap = fact_snapshot(&state, kb_id, left).await;
            utopia_store::temporal::close_superseded(&state.pool, left, at, "day").await?;
            if let Some(mut d) = snap {
                d["valid_to"] = json!(at.to_rfc3339());
                let _ = utopia_store::audit::record(
                    &state.pool,
                    Some(kb_id),
                    user.id,
                    "fact.close",
                    "fact",
                    Some(left),
                    d,
                )
                .await;
            }
        }
        (false, "fact_closed") => {
            return Err(utopia_core::AppError::invalid(
                "bad_resolution",
                "fact_closed is only for derived_contradiction",
            )
            .into());
        }
        _ => {}
    }
    if !decided {
        utopia_store::reasoning::decide(&state.pool, kb_id, violation_id, &req.resolution, user.id)
            .await?;
    }
    // Once the path is clear, let the derivations land so nobody has to go and click "derive
    // again". Retracting and closing move the assertion out of the way; accepting is let through
    // inside materialize
    if repaired {
        utopia_store::reasoning::materialize(&state.pool, kb_id).await?;
    }
    // A retracted fact may still be hanging off other violations: recompute, and those rows get
    // cleared with it, so the queue keeps no dead entries
    if req.resolution == "fact_retracted" {
        utopia_store::reasoning::run(&state.pool, kb_id).await?;
    }
    let _ = utopia_store::audit::record(
        &state.pool,
        Some(kb_id),
        user.id,
        "violation.decided",
        "axiom_violation",
        Some(violation_id),
        json!({ "resolution": req.resolution }),
    )
    .await;
    state.emit_review(kb_id);
    Ok(Json(json!({ "ok": true })))
}

/// Run the consistency check once, by hand.
///
/// **Runs synchronously rather than as a queued job**: this is pure computation, with no model
/// calls and no network -- a KB with tens of thousands of facts runs through in milliseconds.
/// Putting it in the job queue would only leave someone staring at a "queued" after clicking the
/// button, while what the queue really exists to solve is "this work takes minutes".
///
/// `predicates_with_axioms` goes back to the front end along with it: **zero and zero do not mean
/// the same thing**. With no axioms the conclusion is "there is nothing to judge by", and the UI
/// should say "import an ontology with axioms first", not "no contradictions found".
pub async fn run_consistency_check(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(kb_id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Editor).await?;
    let report = utopia_store::reasoning::run(&state.pool, kb_id).await?;
    // Ontology self-consistency is computed along with it. Both tests come from the same source
    // (the axioms the ontology declares), and two separate buttons would only make people click
    // twice
    let onto = utopia_store::reasoning::check_ontology(&state.pool, kb_id).await?;
    let _ = utopia_store::audit::record(
        &state.pool,
        Some(kb_id),
        user.id,
        "consistency.checked",
        "knowledge_base",
        Some(kb_id),
        json!({
            "edges": report.edges,
            "predicates_with_axioms": report.predicates_with_axioms,
            "found": report.found,
            "inserted": report.inserted,
            "cleared": report.cleared,
            "defects_found": onto.found,
        }),
    )
    .await;
    state.emit_review(kb_id);
    Ok(Json(json!({
        "edges": report.edges,
        "predicates_with_axioms": report.predicates_with_axioms,
        "found": report.found,
        "inserted": report.inserted,
        "cleared": report.cleared,
        // The ontology's own queue comes back on its own. **Not folded into found**: the two
        // numbers are not the same kind of thing, and once added up, "3 contradictions" could be
        // three facts clashing or the ontology itself written backwards in three places
        "classes": onto.classes,
        "defects_found": onto.found,
        "defects_new": onto.inserted,
    })))
}

#[derive(Deserialize)]
pub struct DecideDefectReq {
    /// fixed | accepted
    pub resolution: String,
}

/// A human takes a position on one ontology defect.
///
/// **Two ways out, not three**: an ontology defect never looked at the data at all, so there is
/// no "the data is wrong" option here.
pub async fn decide_defect(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path((kb_id, defect_id)): Path<(Uuid, Uuid)>,
    Json(req): Json<DecideDefectReq>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Editor).await?;
    if !matches!(req.resolution.as_str(), "fixed" | "accepted") {
        return Err(utopia_core::AppError::invalid(
            "bad_resolution",
            "resolution must be either fixed or accepted",
        )
        .into());
    }
    utopia_store::reasoning::decide_defect(&state.pool, kb_id, defect_id, &req.resolution, user.id)
        .await?;
    let _ = utopia_store::audit::record(
        &state.pool,
        Some(kb_id),
        user.id,
        "defect.decided",
        "ontology_defect",
        Some(defect_id),
        json!({ "resolution": req.resolution }),
    )
    .await;
    state.emit_review(kb_id);
    Ok(Json(json!({ "ok": true })))
}

/// Run inference once (R1).
///
/// **Gated by the `materialize_inferences` switch.** Off by default, because this step adds
/// things to the graph, while criterion 2 of 0001 says "the ontology guides, it does not
/// enforce" -- a declaration may be wrong, and the graph should not be changed by it while the
/// user has taken no position. When the switch is off we do not silently skip: an explicit error
/// comes back, so the UI can say why nothing happened.
///
/// Runs synchronously, for the same reason as the consistency check: pure computation, no model
/// calls and no network.
#[derive(Deserialize)]
pub struct PendingQuery {
    pub chunk_id: Uuid,
}

/// All the pending items extracted from one remembered sentence -- the confirmation card in the
/// chat fetches by this (0015).
/// A viewer can see it too: the proposals are visible, the buttons are not, the same convention
/// as the Review page
pub async fn pending_for_chunk(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(kb_id): Path<Uuid>,
    Query(q): Query<PendingQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Viewer).await?;
    let items = utopia_store::pending::for_chunk(&state.pool, kb_id, q.chunk_id).await?;
    Ok(Json(json!({ "items": items })))
}

#[derive(Deserialize)]
pub struct DecidePendingBody {
    /// `confirm` goes into the ledger; `reject` is recorded in `rejected_facts`, so the next
    /// round of re-extraction does not bring it up again
    pub action: String,
}

/// A human nods or shakes their head at one pending fact.
///
/// Both actions go into the decision ledger with self-contained snapshots -- once the row is
/// deleted, the ledger still reads back what it was that got confirmed at the time.
/// Confirming goes down the same path as extraction (fact + evidence + temporal reconciliation),
/// so after nodding at a sentence like "Mira handed it over to Devin", Mira's row gets closed
/// exactly as it would have been when extracted from a document
pub async fn decide_pending(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path((kb_id, pending_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<DecidePendingBody>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Editor).await?;
    match body.action.as_str() {
        "confirm" => {
            let done = utopia_store::pending::confirm(&state.pool, kb_id, pending_id).await?;
            let _ = utopia_store::audit::record(
                &state.pool,
                Some(kb_id),
                user.id,
                "fact.nod_confirmed",
                "fact",
                Some(done.fact_id),
                done.snapshot,
            )
            .await;
            state.emit_pending(kb_id);
            state.emit_review(kb_id);
            state.emit_graph(kb_id);
            Ok(Json(json!({
                "ok": true,
                "fact_id": done.fact_id,
                "created": done.created,
                "conflicts": done.conflicts,
            })))
        }
        "reject" => {
            let snap = utopia_store::pending::reject(&state.pool, kb_id, pending_id, Some(user.id))
                .await?;
            let _ = utopia_store::audit::record(
                &state.pool,
                Some(kb_id),
                user.id,
                "fact.nod_rejected",
                "pending_fact",
                Some(pending_id),
                snap,
            )
            .await;
            state.emit_pending(kb_id);
            state.emit_review(kb_id);
            Ok(Json(json!({ "ok": true })))
        }
        other => Err(utopia_core::AppError::invalid(
            "unknown_action",
            format!("action must be confirm or reject, got {other}"),
        )
        .into()),
    }
}

pub async fn run_inference(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(kb_id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    require_kb(&state, &user, kb_id, Role::Editor).await?;
    let on: bool =
        sqlx::query_scalar("SELECT materialize_inferences FROM knowledge_bases WHERE id = $1")
            .bind(kb_id)
            .fetch_one(&state.pool)
            .await
            .map_err(utopia_core::AppError::Db)?;
    if !on {
        return Err(utopia_core::AppError::invalid(
            "inference_off",
            "materialized inference is off for this knowledge base",
        )
        .into());
    }
    let report = utopia_store::reasoning::materialize(&state.pool, kb_id).await?;
    let _ = utopia_store::audit::record(
        &state.pool,
        Some(kb_id),
        user.id,
        "inference.materialized",
        "knowledge_base",
        Some(kb_id),
        json!({
            "rules": report.rules,
            "edges": report.edges,
            "derived": report.derived,
            "inserted": report.inserted,
            "invalidated": report.invalidated,
            "capped": report.capped,
        }),
    )
    .await;
    state.emit_graph(kb_id);
    Ok(Json(json!({
        "rules": report.rules,
        "edges": report.edges,
        "derived": report.derived,
        "inserted": report.inserted,
        "invalidated": report.invalidated,
        "capped": report.capped,
    })))
}
