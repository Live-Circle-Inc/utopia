//! Temporal engine (S3): contradiction detection and automatic closing for functional state
//! relations.
//!
//! Principles:
//! - Pure rule-based judgement, zero LLM -- the ambiguity has already been digested upstream
//!   (resolution merges the entities, the ontology marks the relation functional)
//! - Closing goes through "invalidate + rewrite" rather than editing in place: the old assertion
//!   records "when it was corrected" in invalidated_at, while the correction row closes the
//!   interval and chains back to the old row through supersedes -- which is what makes "replay
//!   the past with the knowledge we had at the time" hold up
//! - The closing point only ever uses world time (the new fact's valid_from), never the ingest
//!   moment as a stand-in
//! - When it cannot be sure (no time / same start / low confidence) it never forces a close; the
//!   case goes to fact_conflicts for a human to rule on

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use utopia_core::models::ConflictView;
use utopia_core::AppResult;
use uuid::Uuid;

/// A new fact below this confidence is not allowed to rewrite history automatically (it goes to
/// review).
const AUTO_CLOSE_MIN_CONFIDENCE: f32 = 0.75;

/// The open-interval fact that was hit (at most one under the invariant; dirty historical data
/// from before the engine shipped may give several).
#[derive(Debug, sqlx::FromRow)]
struct OpenFact {
    id: Uuid,
    valid_from: Option<DateTime<Utc>>,
    /// Used as the granularity of that moment when closing somebody else's interval
    valid_from_precision: Option<String>,
}

/// Direction of uniqueness: functional = the subject side (Zhang San only reports_to one person
/// at a time); inverse functional = the object side (a project only has one person who leads it
/// at a time).
#[derive(Debug, Clone, Copy)]
pub enum Uniqueness {
    SubjectSide,
    ObjectSide,
}

/// Reconciliation result: the ids of the correction rows that auto-closing produced (the caller
/// books them as needed -- a merge rollback, for one, has to undo them) and the number of
/// conflicts that went to human review.
#[derive(Debug, Default)]
pub struct ReconcileReport {
    pub corrected: Vec<Uuid>,
    pub conflicts: u32,
}

/// Reconciliation along the given direction of uniqueness, once a new state fact has landed in
/// the database. The caller is responsible for deciding that the relation really does carry
/// uniqueness in that direction and that temporal = state (the ontology metadata is already
/// loaded in the extraction job).
///
/// The object may be an entity (object_id) or a literal value (object_value, an attribute fact)
/// -- the "the object differs" test compares the (object_id, object_value) pair as a whole: a
/// salary going from 30k to 35k takes the same closing path as "switching from Zhang San to Li
/// Si".
#[allow(clippy::too_many_arguments)]
pub async fn reconcile_new_fact(
    pool: &PgPool,
    kb_id: Uuid,
    new_fact_id: Uuid,
    subject_id: Uuid,
    predicate_id: Uuid,
    object_id: Option<Uuid>,
    object_value: Option<&serde_json::Value>,
    direction: Uniqueness,
    new_validity: crate::graph::Validity<'_>,
    new_confidence: f32,
) -> AppResult<ReconcileReport> {
    // A new fact whose interval is already closed is a historical statement and does not
    // threaten the "unique during the open interval" invariant -- it triggers no rewrite at all
    // (overlap contradictions between closed intervals are finer-grained interval algebra, not
    // adjudicated automatically for now, left to human eyes in Review)
    if new_validity.has_ended() {
        return Ok(ReconcileReport::default());
    }
    // Object-side uniqueness only makes sense for entity objects (a literal value does not get
    // "occupied")
    if matches!(direction, Uniqueness::ObjectSide) && object_id.is_none() {
        return Ok(ReconcileReport::default());
    }
    // The invariant point lookup: subject side = same (kb, S, P) with a different object;
    // object side = same (kb, P, O) with a different subject
    let sql = match direction {
        Uniqueness::SubjectSide => {
            "SELECT id, valid_from, valid_from_precision FROM facts
             WHERE kb_id = $1 AND subject_id = $2 AND predicate_id = $3
               AND valid_to IS NULL AND valid_to_precision IS NULL
               AND invalidated_at IS NULL
               AND id <> $4
               AND (object_id IS DISTINCT FROM $5 OR object_value IS DISTINCT FROM $6)"
        }
        Uniqueness::ObjectSide => {
            "SELECT id, valid_from, valid_from_precision FROM facts
             WHERE kb_id = $1 AND object_id = $5 AND predicate_id = $3
               AND valid_to IS NULL AND valid_to_precision IS NULL
               AND invalidated_at IS NULL
               AND id <> $4 AND subject_id IS DISTINCT FROM $2"
        }
    };
    let q = sqlx::query_as(sql)
        .bind(kb_id)
        .bind(subject_id)
        .bind(predicate_id)
        .bind(new_fact_id)
        .bind(object_id);
    let open: Vec<OpenFact> = match direction {
        Uniqueness::SubjectSide => q.bind(object_value).fetch_all(pool).await?,
        Uniqueness::ObjectSide => q.fetch_all(pool).await?,
    };
    if open.is_empty() {
        return Ok(ReconcileReport::default());
    }

    let mut report = ReconcileReport::default();
    for old in open {
        match (old.valid_from, new_validity.from) {
            // The new fact has no world time: there is no closing point to speak of → human
            // ruling
            (_, None) => {
                record_conflict(pool, kb_id, old.id, new_fact_id, "no_time").await?;
                report.conflicts += 1;
            }
            // Both start at the same moment: who succeeds whom cannot be told → human ruling
            (Some(of), Some(nf)) if of == nf => {
                record_conflict(pool, kb_id, old.id, new_fact_id, "simultaneous").await?;
                report.conflicts += 1;
            }
            // The new fact starts earlier: it is the historical predecessor, closed at the
            // start of the old fact
            (Some(of), Some(nf)) if nf < of => {
                if new_confidence < AUTO_CLOSE_MIN_CONFIDENCE {
                    record_conflict(pool, kb_id, old.id, new_fact_id, "low_confidence").await?;
                    report.conflicts += 1;
                } else {
                    report.corrected.push(
                        close_superseded(
                            pool,
                            new_fact_id,
                            of,
                            old.valid_from_precision.as_deref().unwrap_or("day"),
                        )
                        .await?,
                    );
                }
            }
            // The regular succession: the old fact closes at the new fact's start (this also
            // applies when the old fact has no start -- start unknown but it has ended)
            (_, Some(nf)) => {
                if new_confidence < AUTO_CLOSE_MIN_CONFIDENCE {
                    record_conflict(pool, kb_id, old.id, new_fact_id, "low_confidence").await?;
                    report.conflicts += 1;
                } else {
                    report.corrected.push(
                        close_superseded(
                            pool,
                            old.id,
                            nf,
                            new_validity.from_precision.unwrap_or("day"),
                        )
                        .await?,
                    );
                }
            }
        }
    }
    Ok(report)
}

/// Reconciliation after an entity merge has moved facts around: a fact whose subject/object was
/// swapped is equivalent to "a newly recorded observation" -- only once two objects are folded
/// into one does the uniqueness invariant get to see them collide for the first time.
/// Re-runs the insert-time reconciliation fact by fact in recorded_at order; non-unique
/// relations, already-closed intervals, and facts already invalidated by an earlier rewrite are
/// skipped automatically.
/// The caller books the returned correction row ids into the merge ledger -- the merge itself is
/// the sole cause of these corrections, so rolling the merge back must undo them along with it
/// (invalidate the correction rows, restore the original rows they replaced), or a correction row
/// ends up wrongly hanging off target while the grounds for it left with the rollback.
pub async fn reconcile_moved_facts(
    pool: &PgPool,
    kb_id: Uuid,
    fact_ids: &[Uuid],
) -> AppResult<ReconcileReport> {
    if fact_ids.is_empty() {
        return Ok(ReconcileReport::default());
    }
    #[derive(sqlx::FromRow)]
    struct MovedFact {
        id: Uuid,
        subject_id: Uuid,
        predicate_id: Uuid,
        object_id: Option<Uuid>,
        object_value: Option<serde_json::Value>,
        valid_from: Option<DateTime<Utc>>,
        valid_from_precision: Option<String>,
        confidence: f32,
        functional: bool,
        inverse_functional: bool,
    }
    let rows: Vec<MovedFact> = sqlx::query_as(
        "SELECT f.id, f.subject_id, f.predicate_id, f.object_id, f.object_value,
                f.valid_from, f.valid_from_precision,
                f.confidence, r.functional, r.inverse_functional
         FROM facts f JOIN relation_types r ON r.id = f.predicate_id
         WHERE f.kb_id = $1 AND f.id = ANY($2)
           AND f.invalidated_at IS NULL AND f.valid_to IS NULL
           AND r.temporal = 'state' AND (r.functional OR r.inverse_functional)
         ORDER BY f.recorded_at",
    )
    .bind(kb_id)
    .bind(fact_ids)
    .fetch_all(pool)
    .await?;

    let mut report = ReconcileReport::default();
    for f in rows {
        // Literal-value facts (attributes) get reconciled after a merge too: fold two "Zhang
        // San"s into one and a salary collision has to be closed as well
        if f.object_id.is_none() && f.object_value.is_none() {
            continue;
        }
        // An earlier close may already have rewritten and invalidated this one -- re-check that
        // each one is alive before using it as a "new fact"
        let (alive,): (bool,) = sqlx::query_as(
            "SELECT EXISTS (SELECT 1 FROM facts
                            WHERE id = $1 AND invalidated_at IS NULL AND valid_to IS NULL)",
        )
        .bind(f.id)
        .fetch_one(pool)
        .await?;
        if !alive {
            continue;
        }
        let mut directions = Vec::new();
        if f.functional {
            directions.push(Uniqueness::SubjectSide);
        }
        if f.inverse_functional {
            directions.push(Uniqueness::ObjectSide);
        }
        for dir in directions {
            let r = reconcile_new_fact(
                pool,
                kb_id,
                f.id,
                f.subject_id,
                f.predicate_id,
                f.object_id,
                f.object_value.as_ref(),
                dir,
                crate::graph::Validity::starting(f.valid_from, f.valid_from_precision.as_deref()),
                f.confidence,
            )
            .await?;
            report.corrected.extend(r.corrected);
            report.conflicts += r.conflicts;
        }
    }
    Ok(report)
}

/// Invalidate + rewrite: the old row records invalidated_at (the record-time axis), a correction
/// row with a closed interval is inserted (the world axis), and the evidence references are
/// copied along with it. Returns the id of the correction row.
pub async fn close_superseded(
    pool: &PgPool,
    fact_id: Uuid,
    valid_to: DateTime<Utc>,
    valid_to_precision: &str,
) -> AppResult<Uuid> {
    let mut tx = pool.begin().await?;
    let corrected = Uuid::now_v7();
    let inserted: Option<(Uuid,)> = sqlx::query_as(
        "INSERT INTO facts (id, kb_id, subject_id, predicate_id, object_id, object_value,
                            valid_from, valid_from_precision,
                            valid_to, valid_to_precision, confidence, supersedes)
         SELECT $1, kb_id, subject_id, predicate_id, object_id, object_value,
                valid_from, valid_from_precision, $3, $4, confidence, id
         FROM facts WHERE id = $2 AND invalidated_at IS NULL
         RETURNING id",
    )
    .bind(corrected)
    .bind(fact_id)
    .bind(valid_to)
    .bind(valid_to_precision)
    .fetch_optional(&mut *tx)
    .await?;
    // Already corrected concurrently: do not do it a second time
    if inserted.is_none() {
        tx.rollback().await?;
        return Ok(fact_id);
    }
    sqlx::query("UPDATE facts SET invalidated_at = now() WHERE id = $1")
        .bind(fact_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        // The surface predicate moves along with the evidence: what is being corrected is the
        // time interval, not what the source said
        "INSERT INTO fact_evidence (fact_id, chunk_id, quote, proposed_predicate, document_id, doc_version)
         SELECT $1, chunk_id, quote, proposed_predicate, document_id, doc_version
         FROM fact_evidence WHERE fact_id = $2
         ON CONFLICT DO NOTHING",
    )
    .bind(corrected)
    .bind(fact_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(corrected)
}

async fn record_conflict(
    pool: &PgPool,
    kb_id: Uuid,
    old_fact_id: Uuid,
    new_fact_id: Uuid,
    reason: &str,
) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO fact_conflicts (id, kb_id, old_fact_id, new_fact_id, reason)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (old_fact_id, new_fact_id) DO NOTHING",
    )
    .bind(Uuid::now_v7())
    .bind(kb_id)
    .bind(old_fact_id)
    .bind(new_fact_id)
    .bind(reason)
    .execute(pool)
    .await?;
    Ok(())
}

/// The conflict list for the Review page (both facts with their names and intervals).
/// Lazy cleanup: a conflict where either side has been invalidated (rejected, or rewritten by
/// some other close) is meaningless, so it is dequeued automatically and marked stale -- which
/// stops anyone mis-ruling on a zombie conflict (such as closing Eve against an already-rejected
/// Ivan).
pub async fn list_conflicts(
    pool: &PgPool,
    kb_id: Uuid,
    limit: i64,
    offset: i64,
) -> AppResult<Vec<ConflictView>> {
    sqlx::query(
        "UPDATE fact_conflicts c
         SET status = 'resolved', resolution = 'stale', resolved_at = now()
         WHERE c.kb_id = $1 AND c.status = 'open'
           AND EXISTS (SELECT 1 FROM facts f
                       WHERE f.id IN (c.old_fact_id, c.new_fact_id)
                         AND f.invalidated_at IS NOT NULL)",
    )
    .bind(kb_id)
    .execute(pool)
    .await?;
    let rows: Vec<ConflictView> = sqlx::query_as(
        "SELECT c.id, c.reason, c.created_at, r.label AS predicate_label,
                c.old_fact_id, os.canonical_name AS old_subject,
                oo.canonical_name AS old_object, fo.valid_from AS old_valid_from,
                c.new_fact_id, ns.canonical_name AS new_subject,
                no_.canonical_name AS new_object, fn_.valid_from AS new_valid_from,
                fn_.confidence AS new_confidence
         FROM fact_conflicts c
         JOIN facts fo ON fo.id = c.old_fact_id
         JOIN facts fn_ ON fn_.id = c.new_fact_id
         JOIN entities os ON os.id = fo.subject_id
         JOIN entities ns ON ns.id = fn_.subject_id
         JOIN relation_types r ON r.id = fo.predicate_id
         LEFT JOIN entities oo ON oo.id = fo.object_id
         LEFT JOIN entities no_ ON no_.id = fn_.object_id
         WHERE c.kb_id = $1 AND c.status = 'open'
         ORDER BY c.created_at DESC
         LIMIT $2 OFFSET $3",
    )
    .bind(kb_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Human ruling: close (the old fact closes at close_at or at the new fact's start) / keep (they
/// coexist without contradiction) / reject_new (the new fact is an extraction error, invalidate
/// it).
pub async fn resolve_conflict(
    pool: &PgPool,
    kb_id: Uuid,
    conflict_id: Uuid,
    resolution: &str,
    close_at: Option<DateTime<Utc>>,
) -> AppResult<()> {
    let row: Option<(Uuid, Uuid, Option<DateTime<Utc>>)> = sqlx::query_as(
        "SELECT c.old_fact_id, c.new_fact_id, fn_.valid_from
         FROM fact_conflicts c JOIN facts fn_ ON fn_.id = c.new_fact_id
         WHERE c.id = $1 AND c.kb_id = $2 AND c.status = 'open'",
    )
    .bind(conflict_id)
    .bind(kb_id)
    .fetch_optional(pool)
    .await?;
    let Some((old_fact_id, new_fact_id, new_from)) = row else {
        return Err(utopia_core::AppError::NotFound);
    };

    let stored = match resolution {
        "close" => {
            let at = close_at.or(new_from).ok_or_else(|| {
                utopia_core::AppError::invalid(
                    "close_at_required",
                    "close_at is required when the new fact has no start time",
                )
            })?;
            close_superseded(pool, old_fact_id, at, "day").await?;
            "closed"
        }
        "keep" => "kept_both",
        "reject_new" => {
            sqlx::query("UPDATE facts SET invalidated_at = now() WHERE id = $1")
                .bind(new_fact_id)
                .execute(pool)
                .await?;
            // Knock-on effect: the other open conflicts this same new fact collided into are
            // dequeued along with it (the new fact is dead, so there is nothing left to rule on)
            sqlx::query(
                "UPDATE fact_conflicts
                 SET status = 'resolved', resolution = 'rejected_new', resolved_at = now()
                 WHERE new_fact_id = $1 AND status = 'open' AND id <> $2",
            )
            .bind(new_fact_id)
            .bind(conflict_id)
            .execute(pool)
            .await?;
            "rejected_new"
        }
        other => {
            return Err(utopia_core::AppError::Validation(format!(
                "Unknown resolution: {other}"
            )))
        }
    };
    sqlx::query(
        "UPDATE fact_conflicts SET status = 'resolved', resolution = $3, resolved_at = now()
         WHERE id = $1 AND kb_id = $2",
    )
    .bind(conflict_id)
    .bind(kb_id)
    .bind(stored)
    .execute(pool)
    .await?;
    Ok(())
}
