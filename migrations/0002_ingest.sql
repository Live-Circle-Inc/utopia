-- The ingestion pipeline: sources, documents, chunks, versions and sync records.

-- A source is a folder: a source is a container in the Library holding the documents it
-- ingested, and it can be synced on a schedule.
-- kind: upload (the virtual home for manual uploads, where source_id is usually NULL) |
-- watch_folder | url | rss | api.
-- For the design see docs/DESIGN.md section 4, ingestion channels
CREATE TABLE sources (
    id         UUID PRIMARY KEY,
    kb_id      UUID NOT NULL REFERENCES knowledge_bases(id) ON DELETE CASCADE,
    kind       TEXT NOT NULL DEFAULT 'upload',
    name       TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- Each kind of source's own configuration (url lists, rss addresses, selectors...). The
    -- shape varies with kind, hence JSONB rather than a pile of sparse columns
    config     JSONB NOT NULL DEFAULT '{}',
    -- NULL = manual sync only
    sync_interval_minutes INTEGER,
    last_sync_at     TIMESTAMPTZ,
    last_sync_status TEXT NOT NULL DEFAULT 'never'
                     CHECK (last_sync_status IN ('never', 'queued', 'running', 'ok', 'failed')),
    last_sync_error  TEXT,
    last_sync_added  INTEGER NOT NULL DEFAULT 0,
    icon       TEXT,
    -- A cron expression (the standard 5 fields), mutually exclusive with
    -- sync_interval_minutes. The UI builds it with a visual picker, and only Advanced mode
    -- exposes the raw expression
    sync_cron  TEXT,
    -- **Stored in the clear, not hashed.** Under a self-hosted threat model "you only see it
    -- once" is asking for trouble: storing it in the clear instead means it can be looked up at
    -- any time (via a dedicated Editor-permission endpoint). By the time the DB has fallen the
    -- documents themselves have long since leaked, so hashing the key buys nothing extra;
    -- Rotate is kept for dealing with a leak
    ingest_token TEXT
);
CREATE INDEX sources_kb_idx ON sources (kb_id);

CREATE TABLE documents (
    id              UUID PRIMARY KEY,
    kb_id           UUID NOT NULL REFERENCES knowledge_bases(id) ON DELETE CASCADE,
    source_id       UUID REFERENCES sources(id) ON DELETE SET NULL,
    filename        TEXT NOT NULL,
    mime            TEXT NOT NULL DEFAULT 'application/octet-stream',
    size_bytes      BIGINT NOT NULL DEFAULT 0,
    sha256          TEXT NOT NULL,
    -- pending → parsing → indexing → embedding → ready | failed
    status          TEXT NOT NULL DEFAULT 'pending'
                    CHECK (status IN ('pending', 'parsing', 'indexing', 'embedding', 'ready', 'failed')),
    error           TEXT,
    -- Document time: graded by trustworthiness and editable (see DESIGN.md 4.2)
    doc_time        TIMESTAMPTZ,
    doc_time_source TEXT NOT NULL DEFAULT 'file_mtime',
    text_len        INT NOT NULL DEFAULT 0,
    chunk_count     INT NOT NULL DEFAULT 0,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- Graph extraction status. **Kept separate from the ingestion pipeline status**: usable in
    -- two stages, so search works as soon as parsing is done while extraction takes its time
    graph_status    TEXT NOT NULL DEFAULT 'none'
                    CHECK (graph_status IN ('none', 'queued', 'extracting', 'done', 'failed')),
    -- Document tags (for filtering and bulk organisation; not entity folders).
    --
    -- **Empty at all four layers today, and deliberately left in place.** Nothing writes it,
    -- reads it or exposes it -- `set_document_tags` has zero callers, and the frontend has never
    -- so much as mentioned the field name. It came through three rounds of migration folding,
    -- 53 to 19 to 10, and in none of them did anybody remember it.
    --
    -- It is kept not because deleting it was forgotten: **tags would be the only dimension on
    -- this table that people attach themselves**. The source says where a document came from,
    -- the name and the status are given by the system, and none of the three expresses a
    -- grouping like "this batch needs redacting" or "that Q3 bundle" -- one that cuts across
    -- sources and is known only to a human.
    --
    -- The case against holds just as well: Utopia's thesis is that **the graph is the
    -- organising structure**, and what 0009 / 0010 / 0011 deleted was precisely "mechanisms
    -- that duplicate the ontology". Adding this means building a second organising system
    -- alongside the graph. And there is a sharper question still: the most common real need
    -- behind tags is "I have not checked this one yet", and that is not a tag, it is a **review
    -- state** -- which deserves a first-class representation of its own.
    --
    -- Unresolved, waiting on outside opinion. To the next person who wants to clean up dead
    -- code: this paragraph is the conclusion, do not just delete it.
    tags            TEXT[] NOT NULL DEFAULT '{}',
    -- The logical identity within a source (watch_folder relative path / url / rss guid / api
    -- external_id). Ingestion uses it for a three-way decision: new / changed / unchanged --
    -- when the content changed the document is replaced in place rather than piling up a new
    -- document, and the old version is recorded in document_versions
    external_key    TEXT,
    -- Files that have vanished from the directory get this stamp. **Kept, not deleted, by
    -- default**
    missing_since   TIMESTAMPTZ,
    -- Why extraction failed. Its own column rather than reusing error, because that column
    -- belongs to the parsing pipeline (set_status clears it), and the two must not interfere
    graph_error     TEXT,
    -- The ownership token for the extraction job. Bumping it on a re-extract "fires" the job
    -- that is currently running: it reads the value back after each chunk it finishes, and on
    -- seeing the epoch change it exits quietly and hands the document over to the new job.
    -- Going by graph_status alone is unreliable -- the job taking over writes the status back to
    -- extracting, and the old job has no way to tell the difference
    extract_epoch   INT NOT NULL DEFAULT 0
);
CREATE INDEX documents_kb_idx ON documents (kb_id, created_at DESC);
CREATE INDEX documents_tags_idx ON documents USING gin (tags);
CREATE UNIQUE INDEX documents_kb_sha_idx ON documents (kb_id, sha256);

CREATE TABLE chunks (
    id           UUID PRIMARY KEY,
    kb_id        UUID NOT NULL REFERENCES knowledge_bases(id) ON DELETE CASCADE,
    document_id  UUID NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
    seq          INT NOT NULL,
    text         TEXT NOT NULL,
    heading      TEXT,
    char_start   INT NOT NULL DEFAULT 0,
    char_end     INT NOT NULL DEFAULT 0,
    -- The dimension is not fixed (it follows the chosen embedding model); P1 searches with a
    -- sequential scan, and once the volume grows an HNSW index is built for the configured
    -- dimension
    embedding    vector,
    -- Soft deletion by version: on a document update the old chunks are marked
    -- (superseded_at) rather than physically deleted -- fact_evidence references stay unbroken
    -- and the old text can be replayed; the embedding is cleared when marking, so old versions
    -- take no part in search
    doc_version   INT NOT NULL DEFAULT 1,
    superseded_at TIMESTAMPTZ,
    -- The graph-extraction-finished mark: on a document update, unchanged chunks that get
    -- "claimed" carry it over and skip re-extraction (incremental extraction), and it also lets
    -- an interrupted extraction resume where it stopped
    extracted_at  TIMESTAMPTZ,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX chunks_document_idx ON chunks (document_id, seq);
CREATE INDEX chunks_kb_idx ON chunks (kb_id);
CREATE INDEX chunks_live_idx ON chunks (document_id) WHERE superseded_at IS NULL;


CREATE UNIQUE INDEX documents_source_key_idx
    ON documents (source_id, external_key) WHERE external_key IS NOT NULL;

-- The raw material for replaying versions (file blobs are content-addressed and never deleted)
CREATE TABLE document_versions (
    id          UUID PRIMARY KEY,
    document_id UUID NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
    version     INTEGER NOT NULL,
    sha256      TEXT NOT NULL,
    size_bytes  BIGINT NOT NULL DEFAULT 0,
    ingested_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (document_id, version)
);

-- One row per sync (time/status/output/error), the auditable history of a channel.
-- Only the most recent 50 rows are kept per source (trimmed in finish_run)
CREATE TABLE source_sync_runs (
    id           UUID PRIMARY KEY,
    source_id    UUID NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
    started_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at  TIMESTAMPTZ,
    status       TEXT NOT NULL DEFAULT 'running' CHECK (status IN ('running', 'ok', 'failed')),
    created_docs INTEGER NOT NULL DEFAULT 0,
    updated_docs INTEGER NOT NULL DEFAULT 0,
    error        TEXT
);
CREATE INDEX source_sync_runs_source_idx ON source_sync_runs (source_id, started_at DESC);
