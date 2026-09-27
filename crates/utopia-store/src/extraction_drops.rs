//! Extraction drop signals: which facts were extracted but never landed, and why.
//!
//! Keeping this apart from `ontology_misses` is deliberate -- that table says "your ontology is
//! missing these", its reader is the ontology maintainer and the action is to add a type; this
//! one says "these facts did not land", its reader is whoever uploaded the document and the
//! action is to fix the document or the ontology. Mixed into one panel, neither gets said
//! clearly.
//!
//! A failed record does not affect extraction (callers always use `let _ =`) -- one missing
//! signal is far better than breaking off the extraction of an entire document because
//! recording a signal failed.

use sqlx::PgPool;
use utopia_core::models::ExtractionDrop;
use utopia_core::AppResult;
use uuid::Uuid;

/// Reason codes. The frontend looks its wording up by these, so they are a stable contract:
/// do not change the literals.
pub mod reason {
    /// Subject not declared in entities → type unknown, an attribute's domain cannot be checked
    pub const SUBJECT_NOT_DECLARED: &str = "subject_not_declared";
    /// The attribute hangs off a class it has no business on (salary on Organization)
    pub const ATTR_DOMAIN_MISMATCH: &str = "attr_domain_mismatch";
    /// The attribute fact gave neither a value nor an object
    pub const ATTR_NO_VALUE: &str = "attr_no_value";
    /// The value does not fit the datatype; normalisation failed
    pub const ATTR_DATATYPE: &str = "attr_datatype";
    /// The model's self-reported confidence is below the threshold
    pub const LOW_CONFIDENCE: &str = "low_confidence";
    /// The relation fact is missing its object
    pub const OBJECT_MISSING: &str = "object_missing";
    /// This item from the model is structurally wrong (no predicate, say) → skip this one item
    /// only, without dragging the whole chunk down with it
    pub const MALFORMED_ITEM: &str = "malformed_item";
    /// The subject's type does not match the domain the relation declares, **and swapping the
    /// two is not legal either** -- that means the wrong relation was picked or the type was
    /// judged wrong, not that the direction is off. Store it as it came + record the signal,
    /// hand it to a human, do not guess
    pub const DOMAIN_MISMATCH: &str = "domain_mismatch";
    /// The "entity name" the model gave is really a whole sentence or clause -- not the name
    /// of a thing. Something like that never matches a mention anywhere else, is an isolated
    /// node in the graph, and drags resolution down as well
    pub const NOT_AN_ENTITY_NAME: &str = "not_an_entity_name";
    /// The subject violated the domain while the object fitted it, so subject and object were
    /// bent back into the direction the ontology declares.
    /// **The action has to leave a trace**: automatic, invisible rewriting is exactly the kind
    /// 0001 objects to
    pub const DIRECTION_CORRECTED: &str = "direction_corrected";
    /// The model's output was cut off (it hit max_tokens) → keep the ones that are complete,
    /// drop the tail
    pub const TRUNCATED_REPLY: &str = "truncated_reply";
}

pub async fn record(
    pool: &PgPool,
    kb_id: Uuid,
    document_id: Uuid,
    reason: &str,
    detail: &str,
    example: Option<&str>,
) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO extraction_drops (kb_id, document_id, reason, detail, example)
         VALUES ($1, $2, $3, left($4, 120), left($5, 200))
         ON CONFLICT (kb_id, document_id, reason, detail)
         DO UPDATE SET count = extraction_drops.count + 1,
                       example = COALESCE(EXCLUDED.example, extraction_drops.example),
                       updated_at = now()",
    )
    .bind(kb_id)
    .bind(document_id)
    .bind(reason)
    .bind(detail)
    .bind(example)
    .execute(pool)
    .await?;
    Ok(())
}

/// Clear this document's old signals when a re-extraction starts -- this round is going to
/// tell the document's story from the beginning.
pub async fn clear_for_document(pool: &PgPool, document_id: Uuid) -> AppResult<()> {
    sqlx::query("DELETE FROM extraction_drops WHERE document_id = $1")
        .bind(document_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Every drop signal in a KB. Aggregated by (document × reason × specific object) the row
/// count is small, so fetching it in one go lets Library both total up each document and expand
/// the details directly, instead of one request per row.
pub async fn for_kb(pool: &PgPool, kb_id: Uuid) -> AppResult<Vec<ExtractionDrop>> {
    Ok(sqlx::query_as(
        "SELECT document_id, reason, detail, count, example FROM extraction_drops
         WHERE kb_id = $1 ORDER BY count DESC, reason LIMIT 2000",
    )
    .bind(kb_id)
    .fetch_all(pool)
    .await?)
}
