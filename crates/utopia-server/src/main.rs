mod adjudication;
mod alerting;
mod api;
mod auth;
mod blob;
mod bootstrap_ontology;
mod client_ctx;
mod docs_corpus;
mod error;
mod extraction;
mod github_issues;
mod ingest_sources;
mod jira_issues;
mod live;
mod llm_util;
mod mappings;
mod notion;
mod object_storage;
mod ontology_index;
mod ontology_packs;
mod owl_import;
mod pack_alignment;
mod pipeline;
mod predicate_match;
mod query_engine;
mod retrieval;
mod state;
mod type_resolution;
mod webdav;

use state::AppState;
use std::sync::Arc;
use tracing_subscriber::EnvFilter;
use utopia_core::config::AppConfig;
use utopia_search::SearchIndex;
use uuid::Uuid;

/// The memory path carries "who said it" (0015); other jobs have no such field, so getting None
/// here is exactly as it should be
fn payload_proposed_by(payload: &serde_json::Value) -> Option<Uuid> {
    payload
        .get("proposed_by")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok())
}

fn payload_document_id(payload: &serde_json::Value) -> anyhow::Result<Uuid> {
    payload
        .get("document_id")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| anyhow::anyhow!("payload is missing document_id"))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,utopia=debug".into()),
        )
        .init();

    let cfg = AppConfig::load()?;

    // Migrations create tables and triggers; the runtime does not need those privileges. Keeping
    // the two apart is what lets the app connect with a restricted role that only reads and writes
    // business tables and can only append to the ledger. The migration pool is released the moment
    // it is done, so that high-privilege connection does not sit around at runtime.
    let migration_url = cfg.migration_url().to_string();
    let separate_migration_role = cfg.migration_url.is_some();
    {
        let mig_pool = utopia_store::db::connect(&migration_url, Some(2)).await?;
        utopia_store::db::migrate(&mig_pool).await?;
        mig_pool.close().await;
    }
    if separate_migration_role {
        tracing::info!("database migrations complete (migration role separate from runtime role)");
    } else {
        tracing::info!("database migrations complete");
    }

    let pool = utopia_store::db::connect(&cfg.database_url, cfg.db_max_connections).await?;

    let index_dir = std::path::Path::new(&cfg.data_dir).join("index");
    let search = Arc::new(SearchIndex::open(&index_dir)?);
    tracing::info!("full-text index ready: {}", index_dir.display());

    // **If the index comes up empty, rebuild it ourselves -- no button.**
    //
    // The index is a file independent of the database: a new machine, a volume that did not get
    // mounted, a corrupted directory -- any one of those leaves it empty. And once it is empty,
    // retrieval just silently returns nothing -- nothing looks out of place in the UI, and the
    // user assumes there "really are no matches". That kind of failure should not depend on a
    // human noticing first and then going to click a button.
    //
    // The test is "there are chunks in the database while the index is empty", not "do the counts
    // match": the latter is briefly unequal during normal operation too (a document is being
    // indexed), and using it as the test would rebuild on every single startup.
    reindex_if_empty(&pool, &search).await;

    // JWT secret: the environment variable wins (rotation and explicitly aligning several
    // instances go through this path), otherwise the one in the database; if the database has none
    // either, generate one now and store it. Generation lives here rather than in store because
    // OsRng is already among the server's dependencies via argon2, so store needs no extra
    // dependency for it.
    // An empty string is treated as unset: with ${UTOPIA_JWT_SECRET:-} in compose the environment
    // variable exists but is empty, and read literally that yields Some("") -- an empty secret
    // identical across every deployment, which is worse than a default value.
    let jwt_secret = match cfg.jwt_secret.clone().filter(|s| !s.trim().is_empty()) {
        Some(s) => s,
        None => {
            let secret =
                utopia_store::access::ensure_jwt_secret(&pool, &auth::generate_jwt_secret())
                    .await?;
            tracing::info!("JWT secret taken from deployment settings (UTOPIA_JWT_SECRET unset)");
            secret
        }
    };

    let state = AppState::new(pool.clone(), &cfg, search, jwt_secret);

    // Worker concurrency: persisted in the system settings and loaded at startup; tuned live at
    // runtime through this same AtomicUsize
    let n = utopia_store::access::worker_concurrency(&pool)
        .await
        .unwrap_or(32);
    state.worker_concurrency.store(
        n.clamp(1, 256) as usize,
        std::sync::atomic::Ordering::Relaxed,
    );

    // Job dispatch: new job kinds get registered here
    let worker_state = state.clone();
    tokio::spawn(utopia_store::jobs::run_worker(
        pool,
        state.worker_concurrency.clone(),
        move |job| {
            let st = worker_state.clone();
            async move {
                // When a job fails, take a look at whether the model endpoint is unreachable --
                // that is a system-level failure, and right now it only ends up in
                // jobs.last_error, where no UI can see it
                //
                // Hopeless failures get tagged here so the queue stops retrying with backoff
                // (#195): the test lives on this side, because `utopia-store` cannot see the
                // LLM's error types
                let result = dispatch(&st, &job)
                    .await
                    .map_err(|e| match alerting::hopeless(&e) {
                        true => e.context(utopia_core::Terminal),
                        false => e,
                    });
                if let Err(e) = &result {
                    alerting::observe_job_failure(&st, &job, e).await;
                }
                result
            }
        },
    ));

    alerting::spawn_retention_sweep(state.clone());

    // Scheduled ingestion scheduler: scan for due sources once a minute, enqueue sync jobs
    let sched_state = state.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            match utopia_store::sources::due_sources(&sched_state.pool).await {
                Ok(due) => {
                    for s in due {
                        match utopia_store::sources::mark_queued(&sched_state.pool, s.id).await {
                            Ok(true) => {
                                if let Err(e) = utopia_store::jobs::enqueue(
                                    &sched_state.pool,
                                    "sync_source",
                                    serde_json::json!({ "source_id": s.id }),
                                )
                                .await
                                {
                                    tracing::warn!(source_id = %s.id, error = %e, "failed to enqueue the sync job");
                                }
                            }
                            Ok(false) => {}
                            Err(e) => {
                                tracing::warn!(source_id = %s.id, error = %e, "failed to mark as enqueued")
                            }
                        }
                    }
                }
                Err(e) => tracing::warn!(error = %e, "failed to scan for due sources"),
            }
        }
    });

    // Scheduled inference scheduler (0002 R1).
    //
    // **It has to be scheduled, it cannot rely on a manual click**: facts change continuously --
    // every document extraction is adding edges -- while derivations are only computed at the
    // moment they run. Without a schedule, the derivations in the graph are missing as soon as
    // the next document comes in, and that gap is invisible in the UI (nothing is wrong, the new
    // chains simply were not derived).
    //
    // It shares one tick with source sync: scan once a minute, enqueue whatever is due. The real
    // derivation runs inside the job, not in this loop -- a full re-derivation of a large KB can
    // take seconds, and stalling the scheduling loop would hold up the other KBs
    let infer_state = state.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            match utopia_store::reasoning::due_for_inference(&infer_state.pool).await {
                Ok(due) => {
                    for kb_id in due {
                        if let Err(e) = utopia_store::jobs::enqueue(
                            &infer_state.pool,
                            "materialize_inferences",
                            serde_json::json!({ "kb_id": kb_id }),
                        )
                        .await
                        {
                            tracing::warn!(kb_id = %kb_id, error = %e, "inference enqueue failed");
                        }
                    }
                }
                Err(e) => tracing::warn!(error = %e, "failed to scan for due inferences"),
            }
        }
    });

    let app = api::router(state, &cfg);
    let listener = tokio::net::TcpListener::bind(&cfg.bind_addr).await?;
    tracing::info!("Utopia server listening on http://{}", cfg.bind_addr);

    // A browser may resolve localhost to ::1 (IPv6) -- when configured with an IPv4 address, add
    // an IPv6 loopback listener on the same port to avoid "cannot find localhost". A failed bind
    // (port taken / no IPv6) only warns.
    if let Ok(addr) = cfg.bind_addr.parse::<std::net::SocketAddrV4>() {
        let v6_addr = format!("[::1]:{}", addr.port());
        match tokio::net::TcpListener::bind(&v6_addr).await {
            Ok(v6_listener) => {
                tracing::info!("also listening on http://{v6_addr}");
                let app_v6 = app.clone();
                tokio::spawn(async move {
                    let svc = app_v6.into_make_service_with_connect_info::<std::net::SocketAddr>();
                    if let Err(e) = axum::serve(v6_listener, svc).await {
                        tracing::warn!(error = %e, "IPv6 listener exited");
                    }
                });
            }
            Err(e) => tracing::warn!(error = %e, "IPv6 loopback bind failed (IPv4 unaffected)"),
        }
    }

    // with_connect_info: auditing needs the real TCP peer address. In a direct-connection
    // deployment it is the only truth there is -- headers like X-Forwarded-For do not exist at
    // that point, and were never to be trusted lightly anyway.
    let svc = app.into_make_service_with_connect_info::<std::net::SocketAddr>();
    axum::serve(listener, svc).await?;
    Ok(())
}

/// The index is empty while the database has content → rebuild it from the database.
///
/// Failure only warns, it does not block startup: retrieval degrades to "cannot search for the
/// moment", whereas the whole service failing to come up is the worse outcome. That one line in
/// the log says what happened, and the next restart will try again.
async fn reindex_if_empty(pool: &sqlx::PgPool, search: &SearchIndex) {
    if !search.is_empty() {
        return;
    }
    let total = match utopia_store::documents::live_chunk_count(pool).await {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!(error = %e, "cannot count chunks, skipping index rebuild");
            return;
        }
    };
    if total == 0 {
        return;
    }
    tracing::info!(
        chunks = total,
        "full-text index is empty, rebuilding from the database"
    );
    let rows = match utopia_store::documents::all_chunks_for_index(pool).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "failed to read chunks, index not rebuilt");
            return;
        }
    };
    // Batch by document before writing: `reindex_document` replaces all the chunks of one
    // document at once, so calling it chunk by chunk would delete what the previous call wrote
    let mut current: Option<(uuid::Uuid, uuid::Uuid)> = None;
    let mut batch: Vec<(String, String)> = Vec::new();
    let mut done = 0usize;
    let flush = |key: Option<(uuid::Uuid, uuid::Uuid)>, batch: &mut Vec<(String, String)>| {
        if let Some((kb, doc)) = key {
            if let Err(e) = search.reindex_document(&kb.to_string(), &doc.to_string(), batch) {
                tracing::warn!(error = %e, document = %doc, "one document did not get indexed");
            }
        }
        batch.clear();
    };
    for (kb_id, doc_id, chunk_id, text) in rows {
        if current != Some((kb_id, doc_id)) {
            flush(current, &mut batch);
            current = Some((kb_id, doc_id));
        }
        batch.push((chunk_id.to_string(), text));
        done += 1;
    }
    // `reindex_document` commits on its own, so there is no need to commit again here
    flush(current, &mut batch);
    tracing::info!(chunks = done, "full-text index rebuild complete");
}
///
/// Job dispatch: new job kinds get registered here.
///
/// Pulled out into its own function rather than inlined in the closure so that failure **has one
/// single exit** -- the layer above wants to inspect the error chain on every failure, and
/// inlined, every arm would have to remember to do it itself.
async fn dispatch(st: &state::AppState, job: &utopia_store::jobs::Job) -> anyhow::Result<()> {
    match job.kind.as_str() {
        "noop" => {
            tracing::info!(job_id = job.id, "noop job executed successfully");
            Ok(())
        }
        "process_document" => {
            let id = payload_document_id(&job.payload)?;
            pipeline::process_document(st, id).await
        }
        "memory_ingest" => {
            let id = payload_document_id(&job.payload)?;
            pipeline::memory_ingest(st, id, payload_proposed_by(&job.payload)).await
        }
        "explore_mappings" => {
            let kb_id: Uuid = job
                .payload
                .get("kb_id")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse().ok())
                .ok_or_else(|| anyhow::anyhow!("payload is missing kb_id"))?;
            mappings::explore_mappings(st, kb_id).await
        }
        // Scheduled re-derivation (0002 R1). **Record the time before deriving** -- even when
        // derivation throws, this KB must not be scanned up again the very next minute; that
        // would turn into a loop that fails once a minute
        "materialize_inferences" => {
            let kb_id: Uuid = job
                .payload
                .get("kb_id")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse().ok())
                .ok_or_else(|| anyhow::anyhow!("payload is missing kb_id"))?;
            utopia_store::reasoning::mark_inference_ran(&st.pool, kb_id).await?;
            let report = utopia_store::reasoning::materialize(&st.pool, kb_id).await?;
            // **A comparison on schedule**: `materialize` is already matching what this round
            // computed against what the database already holds, so the "comparison" is not a new
            // mechanism. All this does is keep the result of that comparison around for people to
            // look at -- nothing is written when there is no change, so the ledger does not get
            // drowned under one "nothing changed" row an hour
            if report.inserted > 0 || report.invalidated > 0 {
                let _ = utopia_store::audit::record(
                    &st.pool,
                    Some(kb_id),
                    Uuid::nil(),
                    "inference.materialized",
                    "knowledge_base",
                    Some(kb_id),
                    serde_json::json!({
                        "scheduled": true,
                        "inserted": report.inserted,
                        "invalidated": report.invalidated,
                        "rules": report.rules,
                    }),
                )
                .await;
                st.emit_graph(kb_id);
            }
            Ok(())
        }
        "extract_document" => {
            let id = payload_document_id(&job.payload)?;
            extraction::extract_document(st, id, payload_proposed_by(&job.payload)).await
        }
        "bootstrap_ontology" => {
            let kb_id: Uuid = job
                .payload
                .get("kb_id")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse().ok())
                .ok_or_else(|| anyhow::anyhow!("payload is missing kb_id"))?;
            bootstrap_ontology::bootstrap_ontology(st, kb_id).await
        }
        // Ontology vector index: **built in the background, never blocking a request**.
        // A 965-class ontology needs 2600 rows embedded on the first pass, six to eight minutes;
        // put that in an interactive request and the first person to use retrieval after the
        // import just waits
        "embed_ontology" => {
            let kb_id: Uuid = job
                .payload
                .get("kb_id")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse().ok())
                .ok_or_else(|| anyhow::anyhow!("payload is missing kb_id"))?;
            ontology_index::refresh(st, kb_id).await.map(|_| ())
        }
        "adjudicate_entities" => {
            let kb_id: Uuid = job
                .payload
                .get("kb_id")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse().ok())
                .ok_or_else(|| anyhow::anyhow!("payload is missing kb_id"))?;
            adjudication::adjudicate_entities(st, kb_id).await
        }
        "sync_source" => {
            let source_id: Uuid = job
                .payload
                .get("source_id")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse().ok())
                .ok_or_else(|| anyhow::anyhow!("payload is missing source_id"))?;
            ingest_sources::sync_source(st, source_id).await
        }
        other => anyhow::bail!("unknown job kind: {other}"),
    }
}
