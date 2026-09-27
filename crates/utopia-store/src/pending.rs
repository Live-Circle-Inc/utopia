//! Facts awaiting a nod (see `docs/decisions/0015`; the table is in `migrations/0018`).
//!
//! The triples pulled out of one sentence of memory land here first, **they do not go into
//! `facts`**: not on the graph, not part of retrieval, not part of reasoning. Only once a human
//! has seen "the original sentence above, the triple below" -- in the conversation or on the
//! Review page -- and nodded does it go onto the ledger by the same route as extraction
//! (`insert_fact` + evidence + temporal reconciliation).
//!
//! **Why a second table rather than a column on `facts`**: fifty-odd queries scoop up live facts
//! by `invalidated_at IS NULL`, and patching the filter into each of them and missing a single
//! one means a fact nobody nodded at gets mixed into the graph -- and preventing exactly that is
//! the entire reason this table exists. Once they are separate, the consequence of forgetting to
//! read it is "the pending queue is invisible", and that direction is the right way round. 0013's
//! `derived_facts` is the same judgement.
//!
//! **It only stops interactive single-row writes.** Bulk ingestion is still optimistic-write plus
//! after-the-fact review -- that path does not come through here.

use sqlx::PgPool;
use utopia_core::models::PendingFactView;
use utopia_core::{AppError, AppResult};
use uuid::Uuid;

use crate::graph::Validity;

/// One proposal handed over by the extractor. The fields line up with `insert_fact` /
/// `insert_value_fact`, with `proposed_predicate` (the model's own words), `chunk_id` (that
/// sentence of memory) and `proposed_by` (who said it) on top.
pub struct Proposal<'a> {
    pub kb_id: Uuid,
    pub subject_id: Uuid,
    /// None = the ontology has no matching relation (0010). **And that emptiness is precisely
    /// what the human needs to see**
    pub predicate_id: Option<Uuid>,
    pub object_id: Option<Uuid>,
    pub object_value: Option<&'a serde_json::Value>,
    pub proposed_predicate: Option<&'a str>,
    pub validity: Validity<'a>,
    pub confidence: f32,
    pub chunk_id: Uuid,
    pub proposed_by: Option<Uuid>,
}

/// Where a proposal goes. None of the three "did not ask" outcomes is an error -- re-extracting a
/// sentence of memory will compute the same triple again, and not blocking it amounts to wiping
/// out the human's decision once per extraction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Proposed(Uuid),
    /// The graph already has an identical live fact -- no need to ask again
    AlreadyAsserted,
    /// Already sitting in the queue, waiting
    AlreadyPending,
    /// A human has rejected this triple (`rejected_facts`)
    Rejected,
}

pub async fn propose(pool: &PgPool, p: Proposal<'_>) -> AppResult<Outcome> {
    let asserted: Option<(i32,)> = sqlx::query_as(
        "SELECT 1 FROM facts
          WHERE kb_id = $1 AND subject_id = $2
            AND predicate_id IS NOT DISTINCT FROM $3
            AND object_id IS NOT DISTINCT FROM $4
            AND object_value IS NOT DISTINCT FROM $5
            AND invalidated_at IS NULL
          LIMIT 1",
    )
    .bind(p.kb_id)
    .bind(p.subject_id)
    .bind(p.predicate_id)
    .bind(p.object_id)
    .bind(p.object_value)
    .fetch_optional(pool)
    .await?;
    if asserted.is_some() {
        return Ok(Outcome::AlreadyAsserted);
    }
    let pending: Option<(i32,)> = sqlx::query_as(
        "SELECT 1 FROM pending_facts
          WHERE kb_id = $1 AND subject_id = $2
            AND predicate_id IS NOT DISTINCT FROM $3
            AND object_id IS NOT DISTINCT FROM $4
            AND object_value IS NOT DISTINCT FROM $5
          LIMIT 1",
    )
    .bind(p.kb_id)
    .bind(p.subject_id)
    .bind(p.predicate_id)
    .bind(p.object_id)
    .bind(p.object_value)
    .fetch_optional(pool)
    .await?;
    if pending.is_some() {
        return Ok(Outcome::AlreadyPending);
    }
    // Rejection records are only looked up by (subject, predicate, object entity). **Literal-value
    // facts are not looked up**: `rejected_facts` has no object_value column, and blocking by
    // (subject, predicate) would inflate "salary 28000 was rejected" into "never mention the
    // salary attribute again". Better to ask one more time than to reject a new value on the
    // human's behalf
    if let Some(object_id) = p.object_id {
        let rejected: Option<(i32,)> = sqlx::query_as(
            "SELECT 1 FROM rejected_facts
              WHERE kb_id = $1 AND subject_id = $2
                AND predicate_id IS NOT DISTINCT FROM $3
                AND object_id = $4
              LIMIT 1",
        )
        .bind(p.kb_id)
        .bind(p.subject_id)
        .bind(p.predicate_id)
        .bind(object_id)
        .fetch_optional(pool)
        .await?;
        if rejected.is_some() {
            return Ok(Outcome::Rejected);
        }
    }
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO pending_facts
             (id, kb_id, subject_id, predicate_id, object_id, object_value, proposed_predicate,
              valid_from, valid_from_precision, valid_to, valid_to_precision,
              confidence, chunk_id, proposed_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)",
    )
    .bind(id)
    .bind(p.kb_id)
    .bind(p.subject_id)
    .bind(p.predicate_id)
    .bind(p.object_id)
    .bind(p.object_value)
    .bind(p.proposed_predicate)
    .bind(p.validity.from)
    .bind(p.validity.from_precision)
    .bind(p.validity.to)
    .bind(p.validity.to_precision)
    .bind(p.confidence)
    .bind(p.chunk_id)
    .bind(p.proposed_by)
    .execute(pool)
    .await?;
    Ok(Outcome::Proposed(id))
}

/// One pending item as a human sees it: `utopia_core::models::PendingFactView`.
const VIEW_SELECT: &str = "\
    SELECT p.id, p.subject_id, s.canonical_name AS subject_name,
           p.predicate_id, r.label AS predicate_label, p.proposed_predicate,
           p.object_id, o.canonical_name AS object_name, p.object_value,
           p.valid_from, p.valid_from_precision, p.valid_to, p.valid_to_precision,
           p.confidence, p.chunk_id, c.text AS quote,
           p.proposed_by, u.display_name AS proposed_by_name, p.created_at
      FROM pending_facts p
      JOIN entities s ON s.id = p.subject_id
      LEFT JOIN relation_types r ON r.id = p.predicate_id
      LEFT JOIN entities o ON o.id = p.object_id
      JOIN chunks c ON c.id = p.chunk_id
      LEFT JOIN users u ON u.id = p.proposed_by";

pub async fn list(
    pool: &PgPool,
    kb_id: Uuid,
    limit: i64,
    offset: i64,
) -> AppResult<Vec<PendingFactView>> {
    Ok(sqlx::query_as(&format!(
        "{VIEW_SELECT} WHERE p.kb_id = $1 ORDER BY p.created_at DESC LIMIT $2 OFFSET $3"
    ))
    .bind(kb_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await?)
}

/// Every pending item pulled out of one sentence of memory -- the card in the conversation is
/// fetched by this.
pub async fn for_chunk(
    pool: &PgPool,
    kb_id: Uuid,
    chunk_id: Uuid,
) -> AppResult<Vec<PendingFactView>> {
    Ok(sqlx::query_as(&format!(
        "{VIEW_SELECT} WHERE p.kb_id = $1 AND p.chunk_id = $2 ORDER BY p.created_at"
    ))
    .bind(kb_id)
    .bind(chunk_id)
    .fetch_all(pool)
    .await?)
}

async fn get(pool: &PgPool, kb_id: Uuid, id: Uuid) -> AppResult<PendingFactView> {
    sqlx::query_as(&format!("{VIEW_SELECT} WHERE p.kb_id = $1 AND p.id = $2"))
        .bind(kb_id)
        .bind(id)
        .fetch_optional(pool)
        .await?
        .ok_or(AppError::NotFound)
}

/// The self-contained snapshot the decision ledger needs: once the row is deleted, the ledger can
/// still be read to see what was confirmed or rejected at the time.
fn snapshot(v: &PendingFactView) -> serde_json::Value {
    serde_json::json!({
        "subject": v.subject_name,
        "predicate": v.predicate_label,
        "proposed_predicate": v.proposed_predicate,
        "object": v.object_name,
        "object_value": v.object_value,
        "valid_from": v.valid_from,
        "valid_to": v.valid_to,
        "confidence": v.confidence,
        "quote": v.quote,
        "proposed_by": v.proposed_by_name,
    })
}

pub struct Confirmed {
    pub fact_id: Uuid,
    /// false = the graph already had the same live fact, and this only added evidence
    pub created: bool,
    /// The number that temporal reconciliation could not be sure about and that went into
    /// `fact_conflicts`
    pub conflicts: u32,
    pub snapshot: serde_json::Value,
}

/// A human nods: onto the ledger by the extraction route -- persist the fact, hang the evidence
/// off it, reconcile the temporal side -- then take it off the queue.
///
/// **The confidence is left alone.** A human's position is not expressed as a float (the lesson
/// of 0011); it lives in the audit ledger.
///
/// Not wrapped in a transaction: `insert_fact` deduplicates live facts by (subject, predicate,
/// object), so breaking off halfway and clicking again only adds evidence and never persists a
/// second row, and the row deletion is last -- idempotent, with nothing left to worry about.
pub async fn confirm(pool: &PgPool, kb_id: Uuid, id: Uuid) -> AppResult<Confirmed> {
    let v = get(pool, kb_id, id).await?;
    let validity = Validity {
        from: v.valid_from,
        from_precision: v.valid_from_precision.as_deref(),
        to: v.valid_to,
        to_precision: v.valid_to_precision.as_deref(),
    };
    let (fact_id, created) = match (v.object_id, v.object_value.as_ref()) {
        (Some(object_id), _) => {
            crate::graph::insert_fact(
                pool,
                kb_id,
                v.subject_id,
                v.predicate_id,
                object_id,
                validity,
                v.confidence,
            )
            .await?
        }
        (None, Some(value)) => {
            crate::graph::insert_value_fact(
                pool,
                kb_id,
                v.subject_id,
                v.predicate_id,
                value,
                validity,
                v.confidence,
            )
            .await?
        }
        (None, None) => {
            return Err(AppError::invalid(
                "pending_fact_has_no_object",
                "a pending fact must have an object entity or a literal value",
            ))
        }
    };
    // The evidence points back at that sentence of memory. The quote is the whole sentence -- an
    // episode only ever had one sentence in it anyway
    crate::graph::add_evidence(
        pool,
        fact_id,
        v.chunk_id,
        Some(&v.quote),
        v.proposed_predicate.as_deref(),
    )
    .await?;

    // The same temporal reconciliation as extraction: for a state relation with a uniqueness
    // constraint, a new fact closes the old one. This is exactly how memory ought to behave --
    // once "Mira handed it over to Devin" has been said, the Mira row should close
    let mut conflicts = 0u32;
    if created {
        if let Some(pid) = v.predicate_id {
            let meta: Option<(bool, bool, String)> = sqlx::query_as(
                "SELECT functional, inverse_functional, temporal FROM relation_types WHERE id = $1",
            )
            .bind(pid)
            .fetch_optional(pool)
            .await?;
            if let Some((functional, inverse_functional, temporal)) = meta {
                if temporal == "state" {
                    let mut directions = Vec::new();
                    if functional {
                        directions.push(crate::temporal::Uniqueness::SubjectSide);
                    }
                    // Object-side uniqueness only means anything for entity objects; a literal
                    // value has no "who is being pointed at"
                    if inverse_functional && v.object_id.is_some() {
                        directions.push(crate::temporal::Uniqueness::ObjectSide);
                    }
                    for dir in directions {
                        let report = crate::temporal::reconcile_new_fact(
                            pool,
                            kb_id,
                            fact_id,
                            v.subject_id,
                            pid,
                            v.object_id,
                            v.object_value.as_ref(),
                            dir,
                            validity,
                            v.confidence,
                        )
                        .await?;
                        conflicts += report.conflicts;
                    }
                }
            }
        }
    }
    sqlx::query("DELETE FROM pending_facts WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(Confirmed {
        fact_id,
        created,
        conflicts,
        snapshot: snapshot(&v),
    })
}

/// A human rejects: recorded into `rejected_facts` (which the next round of re-extraction checks
/// first), and taken off the queue.
/// The sentence of memory itself stays -- the person really did say it, it just did not yield a
/// usable fact.
pub async fn reject(
    pool: &PgPool,
    kb_id: Uuid,
    id: Uuid,
    rejected_by: Option<Uuid>,
) -> AppResult<serde_json::Value> {
    let v = get(pool, kb_id, id).await?;
    sqlx::query(
        "INSERT INTO rejected_facts (kb_id, subject_id, predicate_id, object_id, rejected_by)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(kb_id)
    .bind(v.subject_id)
    .bind(v.predicate_id)
    .bind(v.object_id)
    .bind(rejected_by)
    .execute(pool)
    .await?;
    sqlx::query("DELETE FROM pending_facts WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(snapshot(&v))
}
