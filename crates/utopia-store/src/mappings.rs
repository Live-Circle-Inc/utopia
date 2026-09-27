//! The semantic layer's "business concept → data asset" mapping (see
//! `docs/decisions/0011`).
//!
//! It used to be a `mapped_to` fact whose object was a blob of JSON stuffed into
//! `object_value`. The reason for moving it out is written in the create-table comment on
//! `concept_mappings`, in one sentence: **it is not an assertion about the world, it is
//! configuration**.
//!
//! The difference from the ledger is visible right here: this table **allows in-place
//! edits**. What `confirm` changes is "has this piece of configuration taken effect", not
//! "our understanding of the world has changed" -- so it does not need to be append-only;
//! it is enough that the version before the edit leaves a trace in
//! `concept_mapping_revisions`.

use sqlx::PgPool;
use utopia_core::models::{ConceptMapping, MappingRevision};
use utopia_core::{AppError, AppResult};
use uuid::Uuid;

/// The discovery job proposes a mapping.
///
/// **Only one row per (concept, source)**, enforced by the primary key -- this uniqueness
/// used to be buried inside `object_value` where the database could not see it, and could
/// only be closed explicitly by the confirmation flow.
///
/// What someone has already ruled on is not overwritten: rerunning discovery computes the
/// rejected one all over again, and without this clause it gets flipped back to pending,
/// which means every single run erases a person's veto once more (`ontology_proposals`
/// stepped into the same hole, see `ontology_proposals`).
#[allow(clippy::too_many_arguments)]
pub async fn propose(
    pool: &PgPool,
    kb_id: Uuid,
    concept_id: Uuid,
    source: &str,
    table_name: Option<&str>,
    expr: Option<&str>,
    sql: Option<&str>,
    unit: Option<&str>,
    summary: Option<&str>,
    derived: bool,
) -> AppResult<Uuid> {
    // **When `DO UPDATE ... WHERE` is not satisfied, `RETURNING` returns no row at all.**
    //
    // That is how Postgres really behaves, not what intuition says: the condition blocks
    // the update, so that row does not count as having been touched by this statement, and
    // therefore does not show up in RETURNING either. A test ran straight into it -- on
    // proposing an already-rejected mapping a second time, `fetch_one` reported
    // "no rows returned".
    //
    // So the id is looked up on its own: whether an update happens is one thing, and
    // "which row is this mapping" is another.
    let existing: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM concept_mappings
          WHERE kb_id = $1 AND concept_id = $2 AND source = $3",
    )
    .bind(kb_id)
    .bind(concept_id)
    .bind(source)
    .fetch_optional(pool)
    .await?;
    let id = existing.map(|(i,)| i).unwrap_or_else(Uuid::now_v7);
    sqlx::query(
        "INSERT INTO concept_mappings
             (id, kb_id, concept_id, source, table_name, expr, sql, unit, summary, derived)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
         ON CONFLICT (kb_id, concept_id, source) DO UPDATE
           SET table_name = EXCLUDED.table_name, expr = EXCLUDED.expr,
               sql = EXCLUDED.sql, unit = EXCLUDED.unit,
               summary = EXCLUDED.summary, derived = EXCLUDED.derived,
               updated_at = now()
           WHERE concept_mappings.status = 'proposed'",
    )
    .bind(id)
    .bind(kb_id)
    .bind(concept_id)
    .bind(source)
    .bind(table_name)
    .bind(expr)
    .bind(sql)
    .bind(unit)
    .bind(summary)
    .bind(derived)
    .execute(pool)
    .await?;
    Ok(id)
}

/// The ones still waiting for a person to rule. The Review page reads this.
pub async fn proposed(
    pool: &PgPool,
    kb_id: Uuid,
    limit: i64,
    offset: i64,
) -> AppResult<Vec<ConceptMapping>> {
    Ok(sqlx::query_as(
        "SELECT m.id, m.concept_id, e.canonical_name AS concept_name, m.source,
                m.table_name, m.expr, m.sql, m.unit, m.summary, m.derived, m.status
         FROM concept_mappings m
         JOIN entities e ON e.id = m.concept_id
         WHERE m.kb_id = $1 AND m.status = 'proposed'
         ORDER BY e.canonical_name, m.source
         LIMIT $2 OFFSET $3",
    )
    .bind(kb_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await?)
}

/// The ones a person confirmed. Data questions inject them into the system prompt --
/// **only confirmed definitions get used**, rather than guessing from the schema every
/// time.
pub async fn confirmed(pool: &PgPool, kb_id: Uuid, limit: i64) -> AppResult<Vec<ConceptMapping>> {
    Ok(sqlx::query_as(
        "SELECT m.id, m.concept_id, e.canonical_name AS concept_name, m.source,
                m.table_name, m.expr, m.sql, m.unit, m.summary, m.derived, m.status
         FROM concept_mappings m
         JOIN entities e ON e.id = m.concept_id
         WHERE m.kb_id = $1 AND m.status = 'confirmed'
         ORDER BY e.canonical_name, m.source
         LIMIT $2",
    )
    .bind(kb_id)
    .bind(limit)
    .fetch_all(pool)
    .await?)
}

/// Someone has ruled.
///
/// **The status changes, the row is not deleted**: the confirmation happened, and so did
/// the rejection. And the trace a rejection leaves has a use right now -- `propose`'s
/// `WHERE status = 'proposed'` relies on it to not flip the row back to pending.
pub async fn decide(
    pool: &PgPool,
    kb_id: Uuid,
    mapping_id: Uuid,
    status: &str,
    actor: Uuid,
) -> AppResult<()> {
    let res = sqlx::query(
        "UPDATE concept_mappings
            SET status = $3, decided_by = $4, decided_at = now(), updated_at = now()
          WHERE id = $2 AND kb_id = $1",
    )
    .bind(kb_id)
    .bind(mapping_id)
    .bind(status)
    .bind(actor)
    .execute(pool)
    .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    Ok(())
}

/// Edit a confirmed definition. **The version before the edit goes into revisions first**
/// -- when data questions look back at a historical report, they have to be able to
/// answer "how was this number computed last quarter".
///
/// A whole-version snapshot is stored rather than a diff: what the reader wants is "what
/// it was at the time", and a diff has to be replayed from the start to answer that.
#[allow(clippy::too_many_arguments)]
pub async fn revise(
    pool: &PgPool,
    kb_id: Uuid,
    mapping_id: Uuid,
    table_name: Option<&str>,
    expr: Option<&str>,
    sql: Option<&str>,
    unit: Option<&str>,
    summary: Option<&str>,
    derived: bool,
    actor: Uuid,
) -> AppResult<()> {
    let mut tx = pool.begin().await?;
    let before: Option<(serde_json::Value,)> = sqlx::query_as(
        "SELECT to_jsonb(m) - 'id' - 'kb_id' FROM concept_mappings m
          WHERE m.id = $2 AND m.kb_id = $1",
    )
    .bind(kb_id)
    .bind(mapping_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((before,)) = before else {
        tx.rollback().await?;
        return Err(AppError::NotFound);
    };
    sqlx::query(
        "INSERT INTO concept_mapping_revisions (id, mapping_id, before, changed_by)
         VALUES ($1, $2, $3, $4)",
    )
    .bind(Uuid::now_v7())
    .bind(mapping_id)
    .bind(before)
    .bind(actor)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE concept_mappings
            SET table_name = $3, expr = $4, sql = $5, unit = $6, summary = $7,
                derived = $8, updated_at = now()
          WHERE id = $2 AND kb_id = $1",
    )
    .bind(kb_id)
    .bind(mapping_id)
    .bind(table_name)
    .bind(expr)
    .bind(sql)
    .bind(unit)
    .bind(summary)
    .bind(derived)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// What the data mapping page reads: one page of definitions, filterable by status and
/// keyword.
///
/// **Neither the `proposed` nor the `confirmed` query is enough** -- the first only pulls
/// what is awaiting a ruling, and the second assembles the prompt for data questions (no
/// pagination, no filtering, capped at 30). What a person wants to see is all of them,
/// including the ones they rejected themselves: the rejection left a trace, so it should
/// be visible, otherwise "why was this concept never mapped" can never be answered.
pub async fn page(
    pool: &PgPool,
    kb_id: Uuid,
    status: Option<&str>,
    q: Option<&str>,
    limit: i64,
    offset: i64,
) -> AppResult<(Vec<ConceptMapping>, i64)> {
    // Both concept names and source names are searchable: what a person remembers is
    // "GMV", or possibly "the one hooked up to orders"
    const WHERE: &str = "WHERE m.kb_id = $1
           AND ($2::text IS NULL OR m.status = $2)
           AND ($3::text IS NULL
                OR e.canonical_name ILIKE '%' || $3 || '%'
                OR m.source ILIKE '%' || $3 || '%'
                OR m.table_name ILIKE '%' || $3 || '%')";
    let rows: Vec<ConceptMapping> = sqlx::query_as(&format!(
        "SELECT m.id, m.concept_id, e.canonical_name AS concept_name, m.source,
                m.table_name, m.expr, m.sql, m.unit, m.summary, m.derived, m.status
         FROM concept_mappings m
         JOIN entities e ON e.id = m.concept_id
         {WHERE}
         ORDER BY e.canonical_name, m.source
         LIMIT $4 OFFSET $5"
    ))
    .bind(kb_id)
    .bind(status)
    .bind(q)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await?;
    let (total,): (i64,) = sqlx::query_as(&format!(
        "SELECT count(*) FROM concept_mappings m
         JOIN entities e ON e.id = m.concept_id {WHERE}"
    ))
    .bind(kb_id)
    .bind(status)
    .bind(q)
    .fetch_one(pool)
    .await?;
    Ok((rows, total))
}

/// How many of each status. The page's filter bar has to show counts, and asking in three
/// separate queries is three full table scans.
pub async fn status_counts(pool: &PgPool, kb_id: Uuid) -> AppResult<(i64, i64, i64)> {
    let row: (i64, i64, i64) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE status = 'proposed'),
                count(*) FILTER (WHERE status = 'confirmed'),
                count(*) FILTER (WHERE status = 'rejected')
           FROM concept_mappings WHERE kb_id = $1",
    )
    .bind(kb_id)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

/// How many times a definition was edited, and what it looked like before each edit.
///
/// `revise` has been writing `concept_mapping_revisions` since the table was created, and
/// **until now nowhere read it** -- a trace kept for nobody. 0006 says the trace is there
/// so that "when data questions look back at a historical report they can answer 'how was
/// this number computed last quarter'", and for that somebody has to be able to see it.
pub async fn revisions(
    pool: &PgPool,
    kb_id: Uuid,
    mapping_id: Uuid,
) -> AppResult<Vec<MappingRevision>> {
    // kb_id verifies ownership through the JOIN: the revisions table has no kb_id of its
    // own, and without the check you could pull another KB's definition history
    Ok(sqlx::query_as(
        "SELECT r.id, r.before, u.display_name AS changed_by_name, r.changed_at
           FROM concept_mapping_revisions r
           JOIN concept_mappings m ON m.id = r.mapping_id
           LEFT JOIN users u ON u.id = r.changed_by
          WHERE r.mapping_id = $2 AND m.kb_id = $1
          ORDER BY r.changed_at DESC
          LIMIT 50",
    )
    .bind(kb_id)
    .bind(mapping_id)
    .fetch_all(pool)
    .await?)
}
