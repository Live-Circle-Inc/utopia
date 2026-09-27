//! The **real counts** for the review queue.
//!
//! The badge in the left rail used to read the length of the array the endpoint returned, and the
//! endpoint always returns at most 100 rows -- so a knowledge base with 164 low-confidence facts
//! displayed 100. Clear those 100 and the remaining 64 come back up, looking like they grew out
//! of nowhere.
//!
//! **Counting and fetching are two different things, and they have to be done separately.**
//! Fetching has a ceiling (ten per page, page forward for the next ten); counting does not:
//! `count(*)` goes through the same WHERE as the list, over the same index.
//!
//! The eight COUNTs are folded into one query instead of eight round trips: they are all on the
//! same kb, so a single round trip fills the left rail in one go, whereas sending them separately
//! makes the rail pop out one row at a time when you switch knowledge bases.

use sqlx::PgPool;
use utopia_core::models::ReviewCounts;
use utopia_core::AppResult;
use uuid::Uuid;

/// The low-confidence threshold. **Shared as one constant with `review_routes`** -- write the
/// number in two places and sooner or later they diverge into "the badge says 12, you click
/// through and there are 9".
pub const LOW_CONFIDENCE_BELOW: f32 = 0.75;

pub async fn counts(pool: &PgPool, kb_id: Uuid) -> AppResult<ReviewCounts> {
    Ok(sqlx::query_as(
        "SELECT
           (SELECT count(*) FROM pending_facts WHERE kb_id = $1) AS pending,
           (SELECT count(*) FROM resolution_reviews
             WHERE kb_id = $1 AND status = 'pending') AS duplicates,
           (SELECT count(*) FROM fact_conflicts
             WHERE kb_id = $1 AND status = 'open') AS conflicts,
           -- unconfirmed = has evidence, but every chunk it sits in was replaced by a newer version
           (SELECT count(*) FROM facts f
             WHERE f.kb_id = $1 AND f.invalidated_at IS NULL
               AND EXISTS (SELECT 1 FROM fact_evidence fe WHERE fe.fact_id = f.id)
               AND NOT EXISTS (SELECT 1 FROM fact_evidence fe
                                 JOIN chunks c ON c.id = fe.chunk_id
                                WHERE fe.fact_id = f.id
                                  AND c.superseded_at IS NULL)) AS unconfirmed,
           (SELECT count(*) FROM facts
             WHERE kb_id = $1 AND invalidated_at IS NULL
               AND confidence < $2 AND derived_by_rule IS NULL) AS lowconf,
           (SELECT count(*) FROM concept_mappings
             WHERE kb_id = $1 AND status = 'proposed') AS mappings,
           (SELECT count(*) FROM axiom_violations
             WHERE kb_id = $1 AND status = 'open') AS violations,
           (SELECT count(*) FROM ontology_defects
             WHERE kb_id = $1 AND status = 'open') AS defects,
           (SELECT count(*) FROM entity_merges WHERE kb_id = $1) AS merges",
    )
    .bind(kb_id)
    .bind(LOW_CONFIDENCE_BELOW)
    .fetch_one(pool)
    .await?)
}
