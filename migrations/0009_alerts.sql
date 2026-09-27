-- Alert centre (0005). Failure state used to be scattered across six places: jobs.last_error,
-- documents.status, documents.graph_status, sources.last_sync_status, source_sync_runs.status,
-- and the log. Every new class of failure meant one more column on the corresponding table --
-- and the inference layer, the execution layer, OCR and lakehouse connections had not even
-- arrived yet.
--
-- What really hurts is not failure, it is **silent failure**: you drag in 100 PDFs, 12 of them
-- are scans, and the UI shows all 100 green.

-- **One row per incident, and once written it never changes.**
--
-- This table deliberately has no state machine: no "resolved", no self-healing, no folding
-- several incidents into a single row. It used to, and the price was that every new kind of
-- alert had to implement "what counts as fixed" for itself -- source.sync_failed has a natural
-- success signal, llm.unreachable does not, so it needed a background probe built just for it;
-- a third kind of alert would need a third apparatus, and a missing clear is invisible at
-- compile time.
--
-- More fundamentally, **that is not the question an alert centre is there to answer**: whether
-- it is still broken right now is written on the source page and in the document status. An
-- alert's job is to get somebody to go and look, not to be a live dashboard.
CREATE TABLE alerts (
    id           UUID PRIMARY KEY,
    -- NULL = system-level (endpoint unreachable, connection pool exhausted), is_admin only
    kb_id        UUID REFERENCES knowledge_bases(id) ON DELETE CASCADE,
    severity     TEXT NOT NULL CHECK (severity IN ('info', 'warning', 'error')),
    -- 'source.sync_failed' / 'llm.unreachable' / ...
    kind         TEXT NOT NULL,
    -- Kept on the row rather than hard-coded per kind: the same class of alert needs a
    -- different person in different situations. Configuration alerts (endpoints, quotas) go to
    -- admin; content alerts (parsing, extraction, sync) have to reach the editor -- whoever
    -- uploaded those 12 scans needs to hear "what you uploaded did not go in" more than the
    -- admin does
    min_role     TEXT NOT NULL CHECK (min_role IN ('viewer', 'editor', 'admin', 'owner')),
    -- The object that went wrong: document / source / system. System-level leaves both empty
    subject_type TEXT,
    subject_id   UUID,
    -- The part a human reads: names, the raw error text
    detail       JSONB NOT NULL DEFAULT '{}',
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- The list is newest first, and that is the only ordering there is
CREATE INDEX alerts_recent_idx ON alerts (created_at DESC);
CREATE INDEX alerts_kb_idx ON alerts (kb_id);

-- Read state belongs to each person.
--
-- The rejected design was shared read state (any admin opening it marks it read for everybody).
-- How that fails: three admins; A casually opens it in the morning, glances at it and does
-- nothing, and the alert **disappears forever** from B's and C's unread lists -- they never
-- learn it happened, while A thinks they will get to it later. Everyone assumes somebody else
-- is handling it, and afterwards no trace is left by which to notice it was missed.
--
-- An alert row never changes once written, so a read is one-shot too: read is read.
CREATE TABLE alert_reads (
    alert_id UUID NOT NULL REFERENCES alerts(id) ON DELETE CASCADE,
    user_id  UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    read_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (alert_id, user_id)
);
