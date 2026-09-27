//! Ingest pipeline: parse → chunk → full-text index → embedding (optional) → ready.
//! Every step is idempotent: a re-run clears out the old chunks and old index entries first.

use crate::llm_util;
use crate::state::AppState;
use uuid::Uuid;

const EMBED_BATCH: usize = 16;

pub async fn process_document(state: &AppState, document_id: Uuid) -> anyhow::Result<()> {
    match run(state, document_id).await {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ =
                utopia_store::documents::set_failed(&state.pool, document_id, &e.to_string()).await;
            if let Ok(doc) = utopia_store::documents::get(&state.pool, document_id).await {
                state.emit_document(doc.kb_id, document_id);
            }
            Err(e)
        }
    }
}

async fn run(state: &AppState, document_id: Uuid) -> anyhow::Result<()> {
    let doc = utopia_store::documents::get(&state.pool, document_id).await?;

    // 1. Parse (CPU-heavy, so it goes on a blocking thread)
    utopia_store::documents::set_status(&state.pool, document_id, "parsing").await?;
    state.emit_document(doc.kb_id, document_id);
    let bytes = state.blob.get(&doc.sha256).await?;
    let filename = doc.filename.clone();
    let parsed =
        tokio::task::spawn_blocking(move || utopia_ingest::parse(&filename, &bytes)).await??;
    let text_len = parsed.text.chars().count() as i32;

    // 2. Chunk + persist
    let pieces = utopia_ingest::chunk_text(&parsed.text);
    let chunk_pairs =
        utopia_store::documents::replace_chunks(&state.pool, doc.kb_id, document_id, &pieces)
            .await?;
    let chunk_count = chunk_pairs.len() as i32;

    // 3. Full-text index (Tantivy)
    utopia_store::documents::set_status(&state.pool, document_id, "indexing").await?;
    state.emit_document(doc.kb_id, document_id);
    let search = state.search.clone();
    let kb = doc.kb_id.to_string();
    let did = document_id.to_string();
    tokio::task::spawn_blocking(move || search.reindex_document(&kb, &did, &chunk_pairs)).await??;

    // 4. embedding (only done if the workspace has an embedding model configured; with none
    //    configured it still counts as ready, so you get BM25 search in the meantime)
    let kb_row = utopia_store::kbs::get(&state.pool, doc.kb_id).await?;
    let settings = utopia_store::settings::get(&state.pool, kb_row.workspace_id).await?;
    if let Some(client) = settings.as_ref().and_then(llm_util::embed_client) {
        utopia_store::documents::set_status(&state.pool, document_id, "embedding").await?;
        state.emit_document(doc.kb_id, document_id);
        let pending =
            utopia_store::documents::chunks_pending_embedding(&state.pool, document_id).await?;
        for batch in pending.chunks(EMBED_BATCH) {
            let texts: Vec<String> = batch.iter().map(|(_, t)| t.clone()).collect();
            let _permit = match settings.as_ref() {
                Some(s) => llm_util::acquire_embed(state, s).await,
                None => None,
            };
            let embeddings = client.embed(&texts).await?;
            if embeddings.len() != batch.len() {
                anyhow::bail!("Embedding returned a mismatched number of vectors");
            }
            let items: Vec<(Uuid, Vec<f32>)> =
                batch.iter().map(|(id, _)| *id).zip(embeddings).collect();
            utopia_store::documents::set_embeddings(&state.pool, &items).await?;
        }
    }

    utopia_store::documents::set_ready(&state.pool, document_id, text_len, chunk_count).await?;

    // Two-stage: once the index is ready, enqueue graph extraction if an
    // extraction model is configured (without blocking search and chat). The test
    // is `extract_ready`, not `chat_ready`: extraction has its own model slot, and
    // a deployment that configured only that should still extract
    if settings.as_ref().is_some_and(|s| s.extract_ready()) {
        utopia_store::documents::set_graph_status(&state.pool, document_id, "queued").await?;
        utopia_store::jobs::enqueue(
            &state.pool,
            "extract_document",
            serde_json::json!({ "document_id": document_id }),
        )
        .await?;
    }
    state.emit_document(doc.kb_id, document_id);

    tracing::info!(%document_id, chunks = chunk_count, "document processing complete");
    Ok(())
}

/// Memory ingest (the back half of the episodes fast path): fill in embeddings for the new
/// episode chunks, rebuild the full-text index, and trigger incremental extraction (only new
/// chunks whose extracted_at is null get extracted).
/// No parsing, no chunking -- an episode is already a chunk by the time it is persisted.
///
/// `proposed_by`: the person who said the sentence. Carried all the way to extraction, where
/// it lands in `pending_facts.proposed_by` (0015)
pub async fn memory_ingest(
    state: &AppState,
    document_id: Uuid,
    proposed_by: Option<Uuid>,
) -> anyhow::Result<()> {
    let doc = utopia_store::documents::get(&state.pool, document_id).await?;
    let kb_row = utopia_store::kbs::get(&state.pool, doc.kb_id).await?;
    let settings = utopia_store::settings::get(&state.pool, kb_row.workspace_id).await?;

    if let Some(client) = settings.as_ref().and_then(llm_util::embed_client) {
        let pending =
            utopia_store::documents::chunks_pending_embedding(&state.pool, document_id).await?;
        for batch in pending.chunks(EMBED_BATCH) {
            let texts: Vec<String> = batch.iter().map(|(_, t)| t.clone()).collect();
            let _permit = match settings.as_ref() {
                Some(s) => llm_util::acquire_embed(state, s).await,
                None => None,
            };
            let embeddings = client.embed(&texts).await?;
            if embeddings.len() != batch.len() {
                anyhow::bail!("Embedding returned a mismatched number of vectors");
            }
            let items: Vec<(Uuid, Vec<f32>)> =
                batch.iter().map(|(id, _)| *id).zip(embeddings).collect();
            utopia_store::documents::set_embeddings(&state.pool, &items).await?;
        }
    }

    let chunks = utopia_store::documents::chunks_full(&state.pool, document_id).await?;
    let pairs: Vec<(String, String)> = chunks
        .iter()
        .map(|c| (c.id.to_string(), c.text.clone()))
        .collect();
    let search = state.search.clone();
    let kb = doc.kb_id.to_string();
    let did = document_id.to_string();
    tokio::task::spawn_blocking(move || search.reindex_document(&kb, &did, &pairs)).await??;

    if settings.as_ref().is_some_and(|s| s.extract_ready()) {
        utopia_store::documents::set_graph_status(&state.pool, document_id, "queued").await?;
        utopia_store::jobs::enqueue(
            &state.pool,
            "extract_document",
            serde_json::json!({ "document_id": document_id, "proposed_by": proposed_by }),
        )
        .await?;
    }
    state.emit_document(doc.kb_id, document_id);
    Ok(())
}
