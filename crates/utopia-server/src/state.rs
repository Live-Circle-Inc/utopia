use sqlx::PgPool;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::broadcast;
use utopia_core::config::AppConfig;
use utopia_search::SearchIndex;
use uuid::Uuid;

/// In-process events (pushed to the front end over SSE for partial refreshes).
#[derive(Clone, Debug, serde::Serialize)]
pub struct AppEvent {
    /// None = does not belong to any KB. The alert badge is cross-KB, and a deployment-level
    /// alert has no KB at all
    pub kb_id: Option<Uuid>,
    /// document = the ingest/extraction status of a document changed; review = the review
    /// queue changed; alert = something changed in the alert centre
    pub kind: &'static str,
    pub document_id: Option<Uuid>,
}

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub jwt_secret: String,
    pub search: Arc<SearchIndex>,
    /// In-memory index over the Charter (the built-in docs): used by chat's search_docs tool
    pub docs: Arc<utopia_search::DocsIndex>,
    /// The seam for reading and writing raw file bytes (content-addressed, key = sha256);
    /// the current implementation is the local disk
    pub blob: Arc<dyn crate::blob::BlobStore>,
    pub open_registration: bool,
    /// Force Secure cookies (a config option); when not forced, it is decided per request
    /// from that request's X-Forwarded-Proto
    pub cookie_secure: bool,
    /// The worker concurrency: read hot on every turn of the scheduling loop -- a change in
    /// the system settings takes effect immediately
    pub worker_concurrency: Arc<std::sync::atomic::AtomicUsize>,
    /// The per-model concurrency gates: a background job takes a permit before calling the
    /// LLM. The limits live in the database, and an edit takes effect immediately
    pub model_gates: Arc<crate::llm_util::ModelGates>,
    pub events: broadcast::Sender<AppEvent>,
    /// The answers currently being generated, looked up by conversation. **It can be picked
    /// back up after a page refresh** (see `live`)
    pub live: Arc<crate::live::Registry>,
}

impl AppState {
    /// `jwt_secret` is resolved by the entry point: if the environment variable gives one it
    /// is that, otherwise the one in the database (generated on first boot). It is not taken
    /// from cfg, because by this point it has to already be one definite value, not an Option.
    pub fn new(
        pool: PgPool,
        cfg: &AppConfig,
        search: Arc<SearchIndex>,
        jwt_secret: String,
    ) -> Self {
        let (events, _) = broadcast::channel(256);
        let data_dir = PathBuf::from(&cfg.data_dir);
        let blob = Arc::new(crate::blob::LocalBlobStore::new(data_dir.join("files")));
        Self {
            pool,
            jwt_secret,
            search,
            docs: Arc::new(crate::docs_corpus::build_index()),
            blob,
            open_registration: cfg.open_registration,
            cookie_secure: cfg.cookie_secure,
            worker_concurrency: Arc::new(std::sync::atomic::AtomicUsize::new(32)),
            model_gates: Arc::new(crate::llm_util::ModelGates::default()),
            events,
            live: Arc::new(crate::live::Registry::default()),
        }
    }

    /// send returns Err when there are no subscribers -- that is normal, silently ignored.
    pub fn emit_document(&self, kb_id: Uuid, document_id: Uuid) {
        let _ = self.events.send(AppEvent {
            kb_id: Some(kb_id),
            kind: "document",
            document_id: Some(document_id),
        });
    }

    pub fn emit_review(&self, kb_id: Uuid) {
        let _ = self.events.send(AppEvent {
            kb_id: Some(kb_id),
            kind: "review",
            document_id: None,
        });
    }

    /// A remembered sentence extracted facts that are waiting for a human nod (0015). The
    /// confirmation card in the conversation refreshes off this -- extraction is asynchronous,
    /// so the card can only grow when the job finishes, not at the moment the assistant
    /// replies
    pub fn emit_pending(&self, kb_id: Uuid) {
        let _ = self.events.send(AppEvent {
            kb_id: Some(kb_id),
            kind: "pending",
            document_id: None,
        });
    }

    /// The graph changed. Reasoning has to emit one after it adds edges to the graph -- it
    /// does not go through the document pipeline, and the `document` event is reserved for
    /// the document pipeline
    pub fn emit_graph(&self, kb_id: Uuid) {
        let _ = self.events.send(AppEvent {
            kb_id: Some(kb_id),
            kind: "graph",
            document_id: None,
        });
    }

    pub fn emit_source(&self, kb_id: Uuid) {
        let _ = self.events.send(AppEvent {
            kb_id: Some(kb_id),
            kind: "source",
            document_id: None,
        });
    }

    /// Something changed in the alerts. **Carries no data and checks no permissions** --
    /// whoever receives it re-fetches the list, and "who can see what" is decided in the list
    /// query, once and only there.
    ///
    /// The cost is that people without permission also get woken up for one re-fetch and
    /// still get nothing back. What that buys is not one line of permission logic anywhere on
    /// the push path, so the "the push and the list decided differently" kind of hole cannot
    /// exist.
    pub fn emit_alert(&self) {
        let _ = self.events.send(AppEvent {
            kb_id: None,
            kind: "alert",
            document_id: None,
        });
    }
}
