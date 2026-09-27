-- The semantic layer: the data sources that questions-over-data are asked of, and the
-- "business concept → data asset" definition mapping.
--
-- Data sources are a two-level model:
-- the system level registers connections (credentials centralised, reused across KBs), the
-- knowledge base level mounts authorisation (permission to query follows the KB).

CREATE TABLE data_sources (
    id           UUID PRIMARY KEY,
    name         TEXT NOT NULL UNIQUE,
    -- postgres only for the first release; mysql/clickhouse get drivers later
    engine       TEXT NOT NULL CHECK (engine IN ('postgres')),
    -- Connection string (credentials included). Same treatment as the api key in llm_settings:
    -- encryption at rest is not implemented yet, see the Status section of the README
    conn_string  TEXT NOT NULL,
    created_by   UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_test_at TIMESTAMPTZ,
    last_test_ok BOOLEAN
);

-- KB mount: only this KB's Chat can reach the mounted sources
CREATE TABLE kb_data_sources (
    kb_id          UUID NOT NULL REFERENCES knowledge_bases(id) ON DELETE CASCADE,
    data_source_id UUID NOT NULL REFERENCES data_sources(id) ON DELETE CASCADE,
    mounted_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (kb_id, data_source_id)
);

-- The semantic layer's "concept → data asset" mapping moves out of the ontology, see
-- docs/decisions/0011.
--
-- It used to be a `mapped_to` fact: the subject was the concept entity, the object was a
-- JSON config stuffed into `object_value`, and `mapped_to` itself was a row in
-- `relation_types`, sitting alongside `works_at`.
--
-- Three reasons for pulling it apart, none of them fastidiousness:
--
-- 1. **It is not an assertion about the world.** The ontology answers "what is there in the
--    world"; this answers "how is this number computed in our database". The same sentence
--    applied for the third time, after 0009 said "concept is control flow, not a vocabulary"
--    and 0010 said "related_to is a fallback, not a relation".
--
-- 2. **The act of "confirming" already violates the ledger's foundation.** `confirm_fact` is
--    `UPDATE facts SET confidence = 1.0` -- an in-place edit. But the ledger is append-only:
--    correcting a fact means inserting a new row + supersedes, because a change of belief is
--    itself information (0001 P0). Confirming a definition does not change a belief, it
--    changes "has this config taken effect". A thing that needs its state edited in place
--    living in a table that forbids in-place edits is itself the evidence that it does not fit.
--
-- 3. **The shape does not match.** The real fields are source / table / expr / sql / unit /
--    summary, all stuffed into one JSONB: you cannot query it ("which concepts map to orders"
--    means digging through JSON) and you cannot constrain it (the uniqueness grain
--    (concept, source) hides inside object_value where the database cannot reach it, so today
--    it rests on process rather than on a constraint).

CREATE TABLE concept_mappings (
    id         UUID PRIMARY KEY,
    kb_id      UUID NOT NULL REFERENCES knowledge_bases(id) ON DELETE CASCADE,
    -- The business concept being mapped (entities of the Metric / Dimension sort)
    concept_id UUID NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    -- The mounted data source. **It goes into the primary key**: the same concept having
    -- different definitions on different sources is deliberately supported, while the same
    -- concept on the same source should have exactly one row -- that used to be closed
    -- explicitly by the confirmation flow, and is now the database's job
    source     TEXT NOT NULL,
    -- How it is computed. The fields spread out into columns instead of staying stuffed into
    -- JSON -- they are the reason this table exists
    table_name TEXT,
    expr       TEXT,
    sql        TEXT,
    unit       TEXT,
    summary    TEXT,
    -- Derived metrics (say "conversion rate = orders / visits"): computed, not a column in a
    -- table
    derived    BOOLEAN NOT NULL DEFAULT FALSE,

    -- **A status, not a confidence.** It used to borrow a fact's confidence to express
    -- "proposed 0.6 / confirmed 1.0", which encodes a two-valued state as a floating-point
    -- number and incidentally drops it into the "low-confidence facts" bucket. Here it says
    -- plainly what it is
    status     TEXT NOT NULL DEFAULT 'proposed'
               CHECK (status IN ('proposed', 'confirmed', 'rejected')),
    -- NULL = nobody has taken a position yet. Both confirmation and rejection leave a trace:
    -- something already rejected should not be swept back into the queue by the next round of
    -- exploration.
    --
    -- A bare foreign key, consistent with `entity_merges.merged_by` and
    -- `ontology_proposals.decided_by` -- **users are soft-deleted, there is no
    -- `DELETE FROM users` in production code**, so this key's delete rule will never be
    -- triggered, and attribution is preserved.
    decided_by UUID REFERENCES users(id),
    decided_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),

    UNIQUE (kb_id, concept_id, source)
);

-- Questions over data only read the confirmed ones (chat.rs injects the system prompt), while
-- Review only pulls the ones still awaiting a position. Both queries go by kb + status
CREATE INDEX concept_mappings_status_idx ON concept_mappings (kb_id, status);

-- A trace of how a definition evolved. **No bitemporality**: a definition does not have two
-- axes of "valid time" and "record time", it has only "when did it take effect". Forcing the
-- ledger's scheme onto it moves the complexity over here rather than solving it.
CREATE TABLE concept_mapping_revisions (
    id         UUID PRIMARY KEY,
    mapping_id UUID NOT NULL REFERENCES concept_mappings(id) ON DELETE CASCADE,
    -- The full text of the version before the edit. A snapshot rather than a diff: what you
    -- want when reading is "what was it then", and a diff has to be replayed from the start to
    -- answer that
    before     JSONB NOT NULL,
    changed_by UUID REFERENCES users(id),
    changed_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX concept_mapping_revisions_idx
    ON concept_mapping_revisions (mapping_id, changed_at DESC);

-- The old mapped_to facts are not migrated: the repo has not shipped yet and every KB holds
-- mock knowledge (same as #125). Leaving them in the ledger does no harm -- the
-- `confirmed_mappings` query gets changed along with the code, so nothing reads them any more.
--
-- **Once it has really shipped that convenience is gone**, written down here so nobody copies
-- this next time.
