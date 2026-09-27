use pgvector::Vector;
use sqlx::PgPool;
use utopia_core::models::{ChunkView, Document, DocumentPage};
use utopia_core::{AppError, AppResult};
use utopia_ingest::ChunkPiece;
use uuid::Uuid;

#[allow(clippy::too_many_arguments)]
pub async fn create(
    pool: &PgPool,
    kb_id: Uuid,
    filename: &str,
    mime: &str,
    size_bytes: i64,
    sha256: &str,
    source_id: Option<Uuid>,
    doc_time: Option<chrono::DateTime<chrono::Utc>>,
    external_key: Option<&str>,
) -> AppResult<Document> {
    sqlx::query_as(
        "INSERT INTO documents (id, kb_id, filename, mime, size_bytes, sha256, source_id,
                                doc_time, doc_time_source, external_key)
         VALUES ($1, $2, $3, $4, $5, $6, $7, COALESCE($8, now()), $9, $10) RETURNING *",
    )
    .bind(Uuid::now_v7())
    .bind(kb_id)
    .bind(filename)
    .bind(mime)
    .bind(size_bytes)
    .bind(sha256)
    .bind(source_id)
    .bind(doc_time)
    .bind(if doc_time.is_some() {
        "source"
    } else {
        "upload_time"
    })
    .bind(external_key)
    .fetch_one(pool)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(db) if db.is_unique_violation() => AppError::Conflict(format!(
            "File already exists (identical content): {filename}"
        )),
        _ => AppError::Db(e),
    })
}

pub async fn list(pool: &PgPool, kb_id: Uuid) -> AppResult<Vec<Document>> {
    let rows = sqlx::query_as("SELECT * FROM documents WHERE kb_id = $1 ORDER BY created_at DESC")
        .bind(kb_id)
        .fetch_all(pool)
        .await?;
    Ok(rows)
}

/// One page of the library, **with filters and stats**.
///
/// This used to be `SELECT * FROM documents WHERE kb_id = $1` with no limit and pagination on
/// the client. 27 documents is fine; twenty thousand shoves the whole table into the browser.
/// And client-side filtering has a subtler flaw on top of that -- it can only filter the ones
/// that have already been fetched.
///
/// The stats are computed separately, and **computed over the source scope only, unaffected
/// by the name/status filters**: "how many documents in this source can be extracted" is the
/// reach of those two bulk buttons, and has nothing to do with what you are searching for at
/// this moment.
#[allow(clippy::too_many_arguments)]
pub async fn page(
    pool: &PgPool,
    kb_id: Uuid,
    // None = everything; Some(None) = only those with no source; Some(Some(id)) = one source
    source: Option<Option<Uuid>>,
    q: Option<&str>,
    graph_status: Option<&str>,
    limit: i64,
    offset: i64,
) -> AppResult<DocumentPage> {
    // All three filters are written as "an empty parameter has no effect", so one SQL
    // statement covers every combination. The `$2 = 'any'` branch is "do not filter by
    // source", `'none'` is "only those with no source" -- two sentinel strings rather than
    // two nullable parameters, because NULL is ambiguous here: it could mean "do not filter"
    // and it could equally mean "filter for source_id IS NULL"
    const WHERE: &str = "WHERE kb_id = $1
           AND ($2 = 'any'
                OR ($2 = 'none' AND source_id IS NULL)
                OR source_id::text = $2)
           AND ($3::text IS NULL OR filename ILIKE '%' || $3 || '%')
           AND ($4::text IS NULL OR graph_status = $4)";
    let scope = match source {
        None => "any".to_string(),
        Some(None) => "none".to_string(),
        Some(Some(id)) => id.to_string(),
    };

    let docs: Vec<Document> = sqlx::query_as(&format!(
        "SELECT * FROM documents {WHERE} ORDER BY created_at DESC LIMIT $5 OFFSET $6"
    ))
    .bind(kb_id)
    .bind(&scope)
    .bind(q)
    .bind(graph_status)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await?;

    let (total,): (i64,) = sqlx::query_as(&format!("SELECT count(*) FROM documents {WHERE}"))
        .bind(kb_id)
        .bind(&scope)
        .bind(q)
        .bind(graph_status)
        .fetch_one(pool)
        .await?;

    // The stats are computed over the source scope only: those two bulk buttons act on the
    // whole source, not on the few rows your search turned up
    let stats: (i64, i64, i64) = sqlx::query_as(
        "SELECT
           count(*) FILTER (WHERE status = 'ready'),
           count(*) FILTER (WHERE graph_status IN ('queued', 'extracting')),
           count(*) FILTER (WHERE graph_status = 'failed')
         FROM documents
         WHERE kb_id = $1
           AND ($2 = 'any'
                OR ($2 = 'none' AND source_id IS NULL)
                OR source_id::text = $2)",
    )
    .bind(kb_id)
    .bind(&scope)
    .fetch_one(pool)
    .await?;

    Ok(DocumentPage {
        docs,
        total,
        ready: stats.0,
        extracting: stats.1,
        failed: stats.2,
    })
}

/// The ids of the documents whose extraction failed in this source (or the whole KB). **This
/// is exactly the list that one-click retry needs**.
pub async fn failed_ids(
    pool: &PgPool,
    kb_id: Uuid,
    source: Option<Option<Uuid>>,
) -> AppResult<Vec<Uuid>> {
    let scope = match source {
        None => "any".to_string(),
        Some(None) => "none".to_string(),
        Some(Some(id)) => id.to_string(),
    };
    Ok(sqlx::query_scalar(
        "SELECT id FROM documents
          WHERE kb_id = $1 AND graph_status = 'failed'
            AND ($2 = 'any'
                 OR ($2 = 'none' AND source_id IS NULL)
                 OR source_id::text = $2)",
    )
    .bind(kb_id)
    .bind(&scope)
    .fetch_all(pool)
    .await?)
}

pub async fn get(pool: &PgPool, id: Uuid) -> AppResult<Document> {
    sqlx::query_as("SELECT * FROM documents WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await?
        .ok_or(AppError::NotFound)
}

/// Fetch a document narrowed by kb. **When the id comes from the model, this is the only
/// branch to take**: `get` looks up by id alone, so an id from a different KB is found just
/// as happily.
pub async fn find_in_kb(pool: &PgPool, kb_id: Uuid, id: Uuid) -> AppResult<Option<Document>> {
    Ok(
        sqlx::query_as("SELECT * FROM documents WHERE id = $1 AND kb_id = $2")
            .bind(id)
            .bind(kb_id)
            .fetch_optional(pool)
            .await?,
    )
}

/// Look up a document by its logical identity within a source (for the three-way sync test).
pub async fn find_by_external_key(
    pool: &PgPool,
    source_id: Uuid,
    external_key: &str,
) -> AppResult<Option<Document>> {
    Ok(
        sqlx::query_as("SELECT * FROM documents WHERE source_id = $1 AND external_key = $2")
            .bind(source_id)
            .bind(external_key)
            .fetch_optional(pool)
            .await?,
    )
}

/// Look up a document by source + content hash (for recognising renames/moves).
pub async fn find_by_source_sha(
    pool: &PgPool,
    source_id: Uuid,
    sha256: &str,
) -> AppResult<Option<Document>> {
    Ok(
        sqlx::query_as("SELECT * FROM documents WHERE source_id = $1 AND sha256 = $2 LIMIT 1")
            .bind(source_id)
            .bind(sha256)
            .fetch_optional(pool)
            .await?,
    )
}

/// Legacy documents from before the migration (no external_key) are claimed by filename -- a
/// one-off backstop for rows created before 0008.
pub async fn find_legacy_by_filename(
    pool: &PgPool,
    source_id: Uuid,
    filename: &str,
) -> AppResult<Option<Document>> {
    Ok(sqlx::query_as(
        "SELECT * FROM documents
         WHERE source_id = $1 AND external_key IS NULL AND filename = $2 LIMIT 1",
    )
    .bind(source_id)
    .bind(filename)
    .fetch_optional(pool)
    .await?)
}

/// Give a legacy document its logical identity.
pub async fn adopt_external_key(pool: &PgPool, id: Uuid, external_key: &str) -> AppResult<()> {
    sqlx::query("UPDATE documents SET external_key = $2, updated_at = now() WHERE id = $1")
        .bind(id)
        .bind(external_key)
        .execute(pool)
        .await?;
    Ok(())
}

/// Change: replace the document's content in place (new sha), with the status back to pending
/// for the pipeline to rerun.
#[allow(clippy::too_many_arguments)]
pub async fn replace_content(
    pool: &PgPool,
    id: Uuid,
    filename: &str,
    mime: &str,
    size_bytes: i64,
    sha256: &str,
    doc_time: Option<chrono::DateTime<chrono::Utc>>,
) -> AppResult<()> {
    sqlx::query(
        "UPDATE documents SET filename = $2, mime = $3, size_bytes = $4, sha256 = $5,
                doc_time = COALESCE($6, doc_time),
                status = 'pending', graph_status = 'none', error = NULL,
                missing_since = NULL, updated_at = now()
         WHERE id = $1",
    )
    .bind(id)
    .bind(filename)
    .bind(mime)
    .bind(size_bytes)
    .bind(sha256)
    .bind(doc_time)
    .execute(pool)
    .await?;
    Ok(())
}

/// Move/rename: same content at a different path, so only the identity is updated and the
/// pipeline does not rerun.
pub async fn update_location(
    pool: &PgPool,
    id: Uuid,
    filename: &str,
    external_key: &str,
) -> AppResult<()> {
    sqlx::query(
        "UPDATE documents SET filename = $2, external_key = $3, missing_since = NULL,
                updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(filename)
    .bind(external_key)
    .execute(pool)
    .await?;
    Ok(())
}

/// Record one content version (the version number auto-increments).
pub async fn record_version(
    pool: &PgPool,
    document_id: Uuid,
    sha256: &str,
    size_bytes: i64,
) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO document_versions (id, document_id, version, sha256, size_bytes)
         VALUES ($1, $2,
                 (SELECT coalesce(max(version), 0) + 1 FROM document_versions WHERE document_id = $2),
                 $3, $4)",
    )
    .bind(Uuid::now_v7())
    .bind(document_id)
    .bind(sha256)
    .bind(size_bytes)
    .execute(pool)
    .await?;
    Ok(())
}

/// Full-set reconciliation (only for the source types whose complete present state is
/// visible, such as a url's configured list): keys seen this round have their missing mark
/// cleared, keys not seen get marked.
/// Unusable for rss (a sliding window) or for custom's incremental responses (?since=) --
/// absence ≠ deletion.
pub async fn reconcile_missing(
    pool: &PgPool,
    source_id: Uuid,
    seen_keys: &[String],
) -> AppResult<()> {
    clear_missing_keys(pool, source_id, seen_keys).await?;
    sqlx::query(
        "UPDATE documents SET missing_since = now(), updated_at = now()
         WHERE source_id = $1 AND missing_since IS NULL
           AND external_key IS NOT NULL AND NOT (external_key = ANY($2))",
    )
    .bind(source_id)
    .bind(seen_keys)
    .execute(pool)
    .await?;
    Ok(())
}

/// Clear the missing mark on items that turned up this round (an item lost and found again).
pub async fn clear_missing_keys(pool: &PgPool, source_id: Uuid, keys: &[String]) -> AppResult<()> {
    sqlx::query(
        "UPDATE documents SET missing_since = NULL, updated_at = now()
         WHERE source_id = $1 AND missing_since IS NOT NULL AND external_key = ANY($2)",
    )
    .bind(source_id)
    .bind(keys)
    .execute(pool)
    .await?;
    Ok(())
}

/// Explicit tombstones (the deleted[] of a custom response): mark only when the source
/// declares a deletion, never infer one from absence.
pub async fn mark_missing_keys(pool: &PgPool, source_id: Uuid, keys: &[String]) -> AppResult<u64> {
    let res = sqlx::query(
        "UPDATE documents SET missing_since = now(), updated_at = now()
         WHERE source_id = $1 AND missing_since IS NULL AND external_key = ANY($2)",
    )
    .bind(source_id)
    .bind(keys)
    .execute(pool)
    .await?;
    Ok(res.rows_affected())
}

/// The ids of every document under this source already marked missing (for bulk cleanup).
pub async fn list_missing(pool: &PgPool, source_id: Uuid) -> AppResult<Vec<Uuid>> {
    let rows: Vec<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM documents WHERE source_id = $1 AND missing_since IS NOT NULL",
    )
    .bind(source_id)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

pub async fn set_status(pool: &PgPool, id: Uuid, status: &str) -> AppResult<()> {
    sqlx::query("UPDATE documents SET status = $2, error = NULL, updated_at = now() WHERE id = $1")
        .bind(id)
        .bind(status)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn set_failed(pool: &PgPool, id: Uuid, error: &str) -> AppResult<()> {
    sqlx::query(
        "UPDATE documents SET status = 'failed', error = $2, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(error)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn set_ready(pool: &PgPool, id: Uuid, text_len: i32, chunk_count: i32) -> AppResult<()> {
    sqlx::query(
        "UPDATE documents SET status = 'ready', error = NULL, text_len = $2, chunk_count = $3,
                updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(text_len)
    .bind(chunk_count)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn delete(pool: &PgPool, id: Uuid) -> AppResult<()> {
    let res = sqlx::query("DELETE FROM documents WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    Ok(())
}

/// Rebuild a document's chunks (inside a transaction, idempotent). Returns (chunk_id, text)
/// for the full-text index.
///
/// Claim-based incrementalism: a new chunk first looks among the live chunks for one with the
/// same text -- finding one is a "claim" (the existing row only updates its
/// seq/offsets/version, and identity carries over: embedding, extracted_at and evidence links
/// all stay fresh in place, so an unchanged paragraph is neither re-extracted nor wrongly
/// marked "evidence stale"); only chunks with no match are created anew. Old chunks left
/// unclaimed are soft-deleted (marked with superseded_at) rather than physically deleted --
/// fact_evidence references keep their chain and old versions can be replayed -- with the
/// embedding cleared (old versions take no part in retrieval, and vectors are the bulk of the
/// storage, so they are not kept).
pub async fn replace_chunks(
    pool: &PgPool,
    kb_id: Uuid,
    document_id: Uuid,
    pieces: &[ChunkPiece],
) -> AppResult<Vec<(String, String)>> {
    let mut tx = pool.begin().await?;
    let (version,): (i32,) = sqlx::query_as(
        "SELECT COALESCE(MAX(version), 1) FROM document_versions WHERE document_id = $1",
    )
    .bind(document_id)
    .fetch_one(&mut *tx)
    .await?;

    // The claim pool: live chunks grouped by text (duplicate chunks with identical text are
    // paired up as a multiset, each claiming its own)
    let old: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT id, text FROM chunks WHERE document_id = $1 AND superseded_at IS NULL",
    )
    .bind(document_id)
    .fetch_all(&mut *tx)
    .await?;
    let mut claim_pool: std::collections::HashMap<String, Vec<Uuid>> =
        std::collections::HashMap::new();
    for (id, text) in old {
        claim_pool.entry(text).or_default().push(id);
    }

    // Phase one: the claims (updating existing rows) and the to-insert list -- new chunks
    // must wait for the soft delete to run before being inserted, otherwise the "unclaimed"
    // test would soft-delete the new chunks this very round just inserted
    let mut adopted: Vec<Uuid> = Vec::new();
    let mut to_insert: Vec<(Uuid, &ChunkPiece)> = Vec::new();
    let mut out = Vec::with_capacity(pieces.len());
    for piece in pieces {
        let claimed = claim_pool.get_mut(&piece.text).and_then(|ids| ids.pop());
        if let Some(id) = claimed {
            sqlx::query(
                "UPDATE chunks SET seq = $2, char_start = $3, char_end = $4, doc_version = $5
                 WHERE id = $1",
            )
            .bind(id)
            .bind(piece.seq)
            .bind(piece.char_start)
            .bind(piece.char_end)
            .bind(version)
            .execute(&mut *tx)
            .await?;
            adopted.push(id);
            out.push((id.to_string(), piece.text.clone()));
        } else {
            let id = Uuid::now_v7();
            to_insert.push((id, piece));
            out.push((id.to_string(), piece.text.clone()));
        }
    }

    // Phase two: old chunks left unclaimed (no matching text in the new version) → soft delete
    sqlx::query(
        "UPDATE chunks SET superseded_at = now(), embedding = NULL
         WHERE document_id = $1 AND superseded_at IS NULL AND NOT (id = ANY($2))",
    )
    .bind(document_id)
    .bind(&adopted)
    .execute(&mut *tx)
    .await?;

    // Phase three: insert the new chunks
    for (id, piece) in to_insert {
        sqlx::query(
            "INSERT INTO chunks
                (id, kb_id, document_id, seq, text, char_start, char_end, doc_version)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(id)
        .bind(kb_id)
        .bind(document_id)
        .bind(piece.seq)
        .bind(&piece.text)
        .bind(piece.char_start)
        .bind(piece.char_end)
        .bind(version)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(out)
}

/// Mark a chunk the moment its extraction finishes (a claimed chunk carries the mark and
/// skips re-extraction; it also lets an interrupted extraction resume).
pub async fn mark_chunk_extracted(pool: &PgPool, chunk_id: Uuid) -> AppResult<()> {
    sqlx::query("UPDATE chunks SET extracted_at = now() WHERE id = $1")
        .bind(chunk_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Queue one document for re-extraction (a manual Extract = a forced full run): clear the
/// incremental marks, fire the job that is running, set queued, create the extract job -- the
/// same semantics as `queue_extraction`, only acting on a single document. Returns the job id.
///
/// All of it one transaction. Done separately, a failure at any step leaves half-finished
/// state: the marks cleared but the epoch not swapped, so the old job never notices it has
/// been superseded; or the status set to queued with no job created, so nobody ever takes the
/// document up.
pub async fn queue_extraction_one(pool: &PgPool, document_id: Uuid) -> AppResult<i64> {
    let mut tx = pool.begin().await?;
    sqlx::query(
        "UPDATE chunks SET extracted_at = NULL
         WHERE document_id = $1 AND superseded_at IS NULL",
    )
    .bind(document_id)
    .execute(&mut *tx)
    .await?;
    // Old jobs that have not started are deleted while we are at it: clicking Extract twice
    // should not accumulate two jobs extracting the same document
    sqlx::query(
        "DELETE FROM jobs WHERE kind = 'extract_document' AND status = 'queued'
           AND payload->>'document_id' = $1",
    )
    .bind(document_id.to_string())
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE documents SET graph_status = 'queued', graph_error = NULL,
                extract_epoch = extract_epoch + 1
         WHERE id = $1",
    )
    .bind(document_id)
    .execute(&mut *tx)
    .await?;
    let (job_id,): (i64,) = sqlx::query_as(
        "INSERT INTO jobs (kind, payload)
         VALUES ('extract_document', jsonb_build_object('document_id', $1::text))
         RETURNING id",
    )
    .bind(document_id)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(job_id)
}

/// The document viewer: every chunk, in order.
pub async fn chunks_full(
    pool: &PgPool,
    document_id: Uuid,
) -> AppResult<Vec<utopia_core::models::ChunkFull>> {
    let rows = sqlx::query_as(
        "SELECT id, seq, text FROM chunks
         WHERE document_id = $1 AND superseded_at IS NULL ORDER BY seq",
    )
    .bind(document_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Chunks for extraction: the text plus the embedding already computed during ingestion
/// (reused by resolution v2, for zero extra embed calls).
#[derive(Debug, sqlx::FromRow)]
pub struct ChunkForExtract {
    pub id: Uuid,
    pub seq: i32,
    pub text: String,
    pub embedding: Option<Vector>,
}

pub async fn chunks_for_extraction(
    pool: &PgPool,
    document_id: Uuid,
) -> AppResult<Vec<ChunkForExtract>> {
    // Only take chunks that have not been extracted: claimed, unchanged paragraphs carry
    // extracted_at and are skipped (incremental extraction + resuming where it broke off)
    let rows = sqlx::query_as(
        "SELECT id, seq, text, embedding FROM chunks
         WHERE document_id = $1 AND superseded_at IS NULL AND extracted_at IS NULL
         ORDER BY seq",
    )
    .bind(document_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Advance the extraction status (clearing the previous round's failure reason along the way
/// -- a rerun turns the page).
pub async fn set_graph_status(pool: &PgPool, id: Uuid, status: &str) -> AppResult<()> {
    sqlx::query(
        "UPDATE documents SET graph_status = $2, graph_error = NULL, updated_at = now()
         WHERE id = $1",
    )
    .bind(id)
    .bind(status)
    .execute(pool)
    .await?;
    Ok(())
}

/// The chunks of this document that have no embedding yet (id + text).
pub async fn chunks_pending_embedding(
    pool: &PgPool,
    document_id: Uuid,
) -> AppResult<Vec<(Uuid, String)>> {
    let rows: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT id, text FROM chunks
         WHERE document_id = $1 AND embedding IS NULL AND superseded_at IS NULL ORDER BY seq",
    )
    .bind(document_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn set_embeddings(pool: &PgPool, items: &[(Uuid, Vec<f32>)]) -> AppResult<()> {
    let mut tx = pool.begin().await?;
    for (id, emb) in items {
        sqlx::query("UPDATE chunks SET embedding = $2 WHERE id = $1")
            .bind(id)
            .bind(Vector::from(emb.clone()))
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Vector nearest-neighbour search (cosine distance, sequential scan; enough at P1 scale).
pub async fn vector_search(
    pool: &PgPool,
    kb_id: Uuid,
    embedding: &[f32],
    limit: i64,
) -> AppResult<Vec<Uuid>> {
    let query_vec = Vector::from(embedding.to_vec());
    let rows: Vec<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM chunks
         WHERE kb_id = $1 AND embedding IS NOT NULL AND superseded_at IS NULL
           AND vector_dims(embedding) = vector_dims($2)
         ORDER BY embedding <=> $2
         LIMIT $3",
    )
    .bind(kb_id)
    .bind(&query_vec)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

/// Fetch chunks by a set of ids (with the document name), keeping the order they came in.
pub async fn chunks_by_ids(pool: &PgPool, kb_id: Uuid, ids: &[Uuid]) -> AppResult<Vec<ChunkView>> {
    let rows: Vec<ChunkView> = sqlx::query_as(
        "SELECT c.id, c.document_id, c.seq, c.text, d.filename
         FROM chunks c JOIN documents d ON d.id = c.document_id
         WHERE c.kb_id = $1 AND c.id = ANY($2)",
    )
    .bind(kb_id)
    .bind(ids)
    .fetch_all(pool)
    .await?;
    // Restore the RRF ranking order
    let mut by_id: std::collections::HashMap<Uuid, ChunkView> =
        rows.into_iter().map(|c| (c.id, c)).collect();
    Ok(ids.iter().filter_map(|id| by_id.remove(id)).collect())
}

/// The live chunks of one document (with the document name), ordered by seq. kb_id goes into
/// the WHERE clause rather than being read back and compared afterwards.
pub async fn chunks_in_document(
    pool: &PgPool,
    kb_id: Uuid,
    document_id: Uuid,
) -> AppResult<Vec<ChunkView>> {
    Ok(sqlx::query_as(
        "SELECT c.id, c.document_id, c.seq, c.text, d.filename
         FROM chunks c JOIN documents d ON d.id = c.document_id
         WHERE c.kb_id = $1 AND c.document_id = $2 AND c.superseded_at IS NULL
         ORDER BY c.seq",
    )
    .bind(kb_id)
    .bind(document_id)
    .fetch_all(pool)
    .await?)
}

/// Bulk-queue a full re-extraction: ready documents have their incremental marks cleared →
/// graph_status=queued → an extract job created, returning the ids of the documents to be
/// extracted. If `source_id` is given it is confined to that source, otherwise the whole KB.
///
/// Documents currently being extracted are requeued along with the rest -- bumping the epoch
/// is what "fires" the job that is running (see `extract_epoch`), so there is no need to skip
/// them and no chance of two workers extracting the same document.
/// Extract jobs that have not started yet are deleted while we are at it (the payload is
/// compared as text: historical dirty payloads cannot be cast to uuid).
///
/// Creating the jobs and setting the status share one transaction: done separately, an error
/// part-way leaves a batch of documents at graph_status=queued with no job -- no worker will
/// come for them, and the UI sits at "queued" forever.
pub async fn queue_extraction(
    pool: &PgPool,
    kb_id: Uuid,
    source_id: Option<Uuid>,
) -> AppResult<Vec<Uuid>> {
    let mut tx = pool.begin().await?;
    let ids: Vec<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM documents
         WHERE kb_id = $1 AND status = 'ready' AND ($2::uuid IS NULL OR source_id = $2)
         ORDER BY created_at",
    )
    .bind(kb_id)
    .bind(source_id)
    .fetch_all(&mut *tx)
    .await?;
    let ids: Vec<Uuid> = ids.into_iter().map(|(id,)| id).collect();
    if ids.is_empty() {
        tx.commit().await?;
        return Ok(ids);
    }

    sqlx::query(
        "DELETE FROM jobs WHERE kind = 'extract_document' AND status = 'queued'
           AND payload->>'document_id' = ANY($1)",
    )
    .bind(ids.iter().map(|i| i.to_string()).collect::<Vec<_>>())
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE chunks SET extracted_at = NULL
         WHERE document_id = ANY($1) AND superseded_at IS NULL",
    )
    .bind(&ids)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE documents SET graph_status = 'queued', graph_error = NULL,
                extract_epoch = extract_epoch + 1
         WHERE id = ANY($1)",
    )
    .bind(&ids)
    .execute(&mut *tx)
    .await?;
    // The payload shape matches jobs::enqueue(json!({"document_id": id})): a uuid serialised
    // as a string
    sqlx::query(
        "INSERT INTO jobs (kind, payload)
         SELECT 'extract_document', jsonb_build_object('document_id', id::text)
         FROM unnest($1::uuid[]) AS t(id)",
    )
    .bind(&ids)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(ids)
}

/// Extraction failed: the status and the reason land in the database together, so the UI has
/// something to show.
pub async fn set_graph_failed(pool: &PgPool, id: Uuid, error: &str) -> AppResult<()> {
    sqlx::query(
        "UPDATE documents SET graph_status = 'failed', graph_error = $2, updated_at = now()
         WHERE id = $1",
    )
    .bind(id)
    .bind(error)
    .execute(pool)
    .await?;
    Ok(())
}

/// The ownership token of an extraction job: incremented on every "start a new round of
/// extraction".
///
/// Claiming by graph_status alone does not work -- by the time the old job reads it back, the
/// new job that took over may already have written the status back to extracting, and the old
/// job will wrongly judge itself still on duty. The epoch rises monotonically, so one
/// comparison tells the old job it has been taken over.
pub async fn extract_epoch(pool: &PgPool, id: Uuid) -> AppResult<i32> {
    let (epoch,): (i32,) = sqlx::query_as("SELECT extract_epoch FROM documents WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await?;
    Ok(epoch)
}

/// Whether this KB still has extractions queued or running.
///
/// Cold-start automatic ontology expansion has to wait until a whole batch of documents has
/// finished extracting before it acts: go by the first document alone and the vocabulary of
/// whichever one arrived first monopolises the ontology. The job that finishes last is the one
/// responsible for triggering it.
pub async fn extraction_idle(pool: &PgPool, kb_id: Uuid) -> AppResult<bool> {
    let (pending,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM documents
         WHERE kb_id = $1 AND graph_status IN ('queued', 'extracting')",
    )
    .bind(kb_id)
    .fetch_one(pool)
    .await?;
    Ok(pending == 0)
}

/// Every chunk in the database, grouped by document, for rebuilding the search index.
///
/// **Fetched all in one go**: this only runs when startup finds the index empty, and at that
/// point all of it is precisely what is wanted; once it has run, it never runs again.
pub async fn all_chunks_for_index(pool: &PgPool) -> AppResult<Vec<(Uuid, Uuid, Uuid, String)>> {
    Ok(sqlx::query_as(
        "SELECT kb_id, document_id, id, text FROM chunks
          WHERE superseded_at IS NULL
          ORDER BY document_id, seq",
    )
    .fetch_all(pool)
    .await?)
}

/// How many live chunks the database holds in total. Used at startup to reconcile against the
/// index.
pub async fn live_chunk_count(pool: &PgPool) -> AppResult<i64> {
    Ok(
        sqlx::query_scalar("SELECT count(*) FROM chunks WHERE superseded_at IS NULL")
            .fetch_one(pool)
            .await?,
    )
}
