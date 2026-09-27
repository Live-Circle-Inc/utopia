-- Graph layer: ontology vocabulary + entities + bitemporal fact ledger + evidence chain.
-- Design in docs/DESIGN.md §3: facts are append-only; an extraction error sets invalidated_at,
-- a change in the fact itself closes valid_to.

-- Classes. **The two vectors are not redundant**: type resolution issues two kinds of query
-- (see `type_resolution.rs`), one the short phrasing the model gave (`district. place`), the
-- other a whole profile paragraph. Both used to be compared against the same batch of
-- `label + description` vectors -- the queries came in two shapes while the documents came in
-- only one, so the short query got taken over by tautological classes (a class that is one line
-- like `Park\nA park.` wins on length, not on meaning: the median source length of the classes
-- that got retrieved was 44, against a median of 89 across all of them).
--
-- "The shorter side systematically has the smaller distance" is a rule this repository has been
-- burned by four times (incomparable across entities, incomparable between the two paths,
-- incomparable between two queries on the same path, dangling classes with an empty description
-- getting an advantage); the first four fixes were all on the query side, this one is on the
-- document side: **two kinds of query ought to have two sets of documents**, short against
-- short, long against long.
--
-- The dimension is not fixed: like chunks.embedding it follows whatever model the workspace
-- picked, so no HNSW, just a sequential scan. The ontology has rows in the thousands, and a
-- sequential scan is plenty.
CREATE TABLE entity_types (
    id         UUID PRIMARY KEY,
    kb_id      UUID NOT NULL REFERENCES knowledge_bases(id) ON DELETE CASCADE,
    key        TEXT NOT NULL,
    label      TEXT NOT NULL,
    color      TEXT NOT NULL DEFAULT '#64748b',
    builtin    BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- Graph nodes render with a per-type shape (organization/product = square used to be
    -- hard-coded in the frontend)
    shape      TEXT NOT NULL DEFAULT 'circle' CHECK (shape IN ('circle', 'square')),
    -- Not decoration for humans to read, but the semantic guidance fed into the extraction
    -- prompt ("Event: an event with a definite point in time, such as a launch, an acquisition,
    -- a meeting"), which directly affects extraction quality
    description TEXT NOT NULL DEFAULT '',
    -- The IRI is the global identity, key is the short label the model reads (see 0001 P2, "the
    -- division of labour between IRI and key"). A re-import matches existing rows by IRI --
    -- matching by key means that when upstream edits rdfs:label the key changes with it, the
    -- same class gets created as a new one, and all the entities are left behind as orphans
    iri        TEXT,
    -- Long document: label + description
    embedding  vector,
    -- **Store "what was embedded at the time" rather than a timestamp.** A timestamp can only
    -- answer "has this been embedded", not "is what was embedded still the current text" --
    -- edit the description or swap the model and the vector is stale, and the timestamp cannot
    -- show it. Storing the source text and the model name means the backfill job only has to
    -- compare to know who needs re-embedding, so no hook has to be hung on every write path
    -- that edits a description (miss one and it rots silently)
    embedded_text  text,
    embedded_model text,
    -- Short document: embeds the label only
    label_embedding      vector,
    label_embedded_text  text,
    label_embedded_model text,
    UNIQUE (kb_id, key)
);
CREATE UNIQUE INDEX entity_types_iri_idx ON entity_types (kb_id, iri) WHERE iri IS NOT NULL;

CREATE TABLE relation_types (
    id         UUID PRIMARY KEY,
    kb_id      UUID NOT NULL REFERENCES knowledge_bases(id) ON DELETE CASCADE,
    key        TEXT NOT NULL,
    label      TEXT NOT NULL,
    -- Temporal semantics: state (interval) / event (point in time) / eternal (no time at all)
    temporal   TEXT NOT NULL DEFAULT 'state' CHECK (temporal IN ('state', 'event', 'eternal')),
    -- Cardinality uniqueness (single-valued at any one moment), the basis for temporal conflict
    -- detection (auto-closing valid_to): functional = unique on the subject side;
    -- inverse_functional = unique on the object side (one project, one leader)
    functional BOOLEAN NOT NULL DEFAULT FALSE,
    inverse_functional BOOLEAN NOT NULL DEFAULT FALSE,
    builtin    BOOLEAN NOT NULL DEFAULT FALSE,
    -- Attribute system: an attribute = a relation whose range is a literal (an RDF datatype
    -- property), one table serving two purposes. Attribute values travel through the
    -- facts.object_value channel and reuse the whole temporal/evidence/review machinery
    kind       TEXT NOT NULL DEFAULT 'relation' CHECK (kind IN ('relation', 'attribute')),
    datatype   TEXT CHECK (datatype IN ('text', 'number', 'date', 'bool')),
    unit       TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    description TEXT NOT NULL DEFAULT '',
    iri        TEXT,
    embedding  vector,
    embedded_text  text,
    embedded_model text,
    -- The remaining property axioms, the same family as `functional` / `inverse_functional` --
    -- those two merely landed in the database first. **Default false rather than NULL**: OWL is
    -- an open world, but the consistency check can only judge by what was written down, and
    -- "not declared" and "declared false" have the same consequence (neither is grounds for
    -- reporting a contradiction), so there is no need for a three-state column to tell apart a
    -- difference that does not change behaviour.
    --
    -- **The is_ prefix on these column names is not stylistic fussiness**: `symmetric` and
    -- `asymmetric` are both Postgres reserved words (`BETWEEN SYMMETRIC`), and using them bare
    -- is a syntax error on the CREATE TABLE line itself. Quoting gets around it, but then every
    -- piece of SQL that writes these two columns from then on has to remember the quotes --
    -- miss one and it only blows up at runtime
    is_transitive  BOOLEAN NOT NULL DEFAULT FALSE,
    is_symmetric   BOOLEAN NOT NULL DEFAULT FALSE,
    is_asymmetric  BOOLEAN NOT NULL DEFAULT FALSE,
    is_irreflexive BOOLEAN NOT NULL DEFAULT FALSE,
    UNIQUE (kb_id, key)
);
CREATE UNIQUE INDEX relation_types_iri_idx ON relation_types (kb_id, iri) WHERE iri IS NOT NULL;

-- domain / range are **join tables, not columns**: in OWL it is normal for one property to have
-- several rdfs:domain values (the subject of works_at may be a person or an organization), a
-- single column cannot express that, and on import you can only pick one and drop the other
-- (FOAF has properties exactly like this). Nor is there a "primary domain" column -- that would
-- be one fact recorded in two places, and two records drift apart sooner or later (preview and
-- the write path each judged key collisions on their own, and the preview ended up saying
-- something false; we have already fallen into that hole). One authority, so the reader does
-- not have to guess which one to trust.
--
-- range only serves object properties: the range of a data property is a literal type, which
-- lives on relation_types.datatype.
CREATE TABLE relation_type_domains (
    relation_type_id UUID NOT NULL REFERENCES relation_types(id) ON DELETE CASCADE,
    entity_type_id   UUID NOT NULL REFERENCES entity_types(id)   ON DELETE CASCADE,
    PRIMARY KEY (relation_type_id, entity_type_id)
);

CREATE TABLE relation_type_ranges (
    relation_type_id UUID NOT NULL REFERENCES relation_types(id) ON DELETE CASCADE,
    entity_type_id   UUID NOT NULL REFERENCES entity_types(id)   ON DELETE CASCADE,
    PRIMARY KEY (relation_type_id, entity_type_id)
);

-- Reverse lookup: the ontology page lists a class's properties by class, and extraction filters
-- the available properties by class. The primary key covers the forward direction, the reverse
-- one has to be built by hand
CREATE INDEX relation_type_domains_entity_idx ON relation_type_domains (entity_type_id);
CREATE INDEX relation_type_ranges_entity_idx  ON relation_type_ranges  (entity_type_id);

-- subClassOf. **A class can have more than one parent**, which is the norm in real vocabularies:
-- FOAF's Person is both a foaf:Agent and a geo:SpatialThing -- two directions, not ancestor and
-- descendant on one chain. Recognise only one parent and properties whose domain sits on the
-- other branch fail the check (latitude's domain is SpatialThing, while person hangs only under
-- agent, so the fact gets extracted and then blocked).
--
-- **is_primary is not redundant**: the left column shows a tree, a class can only be drawn in
-- one place, and "which branch it is drawn under" is a question the subClassOf set itself cannot
-- answer -- it is one extra piece of information, not the same fact recorded twice.
CREATE TABLE entity_type_parents (
    child_id   UUID NOT NULL REFERENCES entity_types(id) ON DELETE CASCADE,
    parent_id  UUID NOT NULL REFERENCES entity_types(id) ON DELETE CASCADE,
    -- The branch the left column follows when drawing the tree. No semantics, display only
    is_primary BOOLEAN NOT NULL DEFAULT FALSE,
    PRIMARY KEY (child_id, parent_id),
    -- Self-loops are blocked right here; longer cycles are checked by the application before
    -- the write (SQL cannot stop A→B→A)
    CONSTRAINT entity_type_parents_no_self CHECK (child_id <> parent_id)
);

-- At most one primary parent per class
CREATE UNIQUE INDEX entity_type_parents_primary_idx
    ON entity_type_parents (child_id) WHERE is_primary;

-- Reverse lookup: the left column finds children by parent, and domain checking walks up the
-- parent chain
CREATE INDEX entity_type_parents_parent_idx ON entity_type_parents (parent_id);

-- Disjoint classes. **Stored as a table rather than an array column**: the question being asked
-- is "are A and B disjoint", which is a point lookup; querying an array column means either a
-- full table scan or a GIN index, while the semantics here is simply an edge.
CREATE TABLE entity_type_disjoint (
    kb_id UUID NOT NULL REFERENCES knowledge_bases(id) ON DELETE CASCADE,
    a_id  UUID NOT NULL REFERENCES entity_types(id) ON DELETE CASCADE,
    b_id  UUID NOT NULL REFERENCES entity_types(id) ON DELETE CASCADE,
    -- One row per direction (the import side has already expanded the axiom's symmetry). The
    -- primary key therefore dedupes for free, and queries do not have to care which end the
    -- caller asks from
    PRIMARY KEY (kb_id, a_id, b_id),
    -- Declaring something disjoint with itself is meaningless; blocking it at the door beats
    -- keeping it around for the check to puzzle over
    CHECK (a_id <> b_id)
);

-- "Which classes are disjoint with this one" is the only way it gets queried (the consistency
-- check asks with an entity's class)
CREATE INDEX entity_type_disjoint_a_idx ON entity_type_disjoint (kb_id, a_id);

CREATE TABLE entities (
    id             UUID PRIMARY KEY,
    kb_id          UUID NOT NULL REFERENCES knowledge_bases(id) ON DELETE CASCADE,
    -- **Nullable**, see docs/decisions/0009: "not decided yet" is not a class. It used to be a
    -- sentinel row named concept, and a sentinel has a name, and anything with a name will
    -- collide -- the key derived from SKOS's skos:Concept is exactly concept, and the import
    -- rule "if the placeholder has no IRI, claim it" let it take the sentinel over, turning
    -- every unclassified entity into a bona fide skos:Concept overnight. Not a name collision
    -- that got skipped, a silent rewrite of the semantics. NULL has no name, so it cannot
    -- collide; nor can it be forgotten -- failing to filter out a sentinel gives no warning at
    -- all, while failing to handle a NULL blows up on the spot
    type_id        UUID REFERENCES entity_types(id) ON DELETE RESTRICT,
    canonical_name TEXT NOT NULL,
    aliases        TEXT[] NOT NULL DEFAULT '{}',
    attrs          JSONB NOT NULL DEFAULT '{}',
    -- Points at the surviving entity after a merge (merges are reversible, later in P2)
    merged_into    UUID REFERENCES entities(id),
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- Entity profile: an incremental centroid of the evidence chunk vectors (used for the
    -- contextual similarity judgement; reuses the chunk embeddings already computed during
    -- ingestion)
    profile_embedding vector,
    profile_n      INTEGER NOT NULL DEFAULT 0,
    -- Display disambiguation suffix for when same-named entities coexist (e.g. Zhang San ·
    -- Platform Engineering)
    disambiguator  TEXT,
    -- The type the model proposed: **the word it used when the ontology had no room for it**.
    -- Without recording it, there is no way to find those entities when you later want to add a
    -- model class -- they are mixed in with the unclassified ones, and the only option is to
    -- re-extract the whole knowledge base
    proposed_type  TEXT,
    -- The model's own words for this entity: what it thinks this most specifically is.
    --
    -- **It cannot share a column with proposed_type**: that column means "the model wanted
    -- something the ontology does not have", and the ontology growth loop sets its threshold on
    -- how rare that is; if every entity filled it in, the loop would propose a new class for
    -- every single entity.
    --
    -- Why it is needed: there is always something "close enough" on the list. The ontology has
    -- product, the model decides that will do and picks it, and the "vector database software"
    -- it had in mind is lost right there. And that name is exactly what type resolution needs
    -- most: a short name against a short label is far closer than taking a paragraph of Chinese
    -- prose and matching it against schema.org's "A software application."
    specific_type  text,
    -- **Where this type came from.** Protection has to come before the fact -- `entity_retypes`
    -- remembers "who changed it", but that is an after-the-fact ledger; the entity itself needs
    -- a bit that says whether a human ever made the call, otherwise every read path can only
    -- see `type_id`. What leaked through was the type resolution path: an entity a human set to
    -- `organization` would, as long as `organization` has subclasses, be picked up and
    -- re-judged by the next round of resolution all the same.
    --
    -- Since 0009 there is one more case: "no type" may now be **a human's decision** -- they
    -- looked at this entity and concluded the ontology has no suitable class. And
    -- `type_id IS NULL` cannot tell "not judged yet" from "a human judged, and the answer is
    -- none", so the next extraction would pin a type on it.
    --
    --   extracted  decided by extraction (the promotion in `resolve_type_drift`)
    --   inferred   adjudicated by the engine (type resolution, or claiming entities after the
    --              ontology grows a new class)
    --   human      called by a person (edited directly in the entity panel, approved in the
    --              review queue)
    --
    -- `inferred` is kept apart from `extracted` rather than merged into one "non-human": their
    -- trustworthiness differs, and if we ever want "the engine may overwrite what the engine
    -- decided, but not what extraction decided", the criterion is already there
    type_source    TEXT NOT NULL DEFAULT 'extracted'
                   CHECK (type_source IN ('extracted', 'human', 'inferred'))
);
-- **Not unique**: a name is not an identity (the two Zhang Weis in 0001 P0), same-named entities
-- are allowed to coexist, and this only does candidate recall.
--
-- Since 0009 type_id may also be NULL, and in Postgres NULL <> NULL -- two same-named entities
-- that both lack a type are even less likely to be stopped. That is the behaviour we want: while
-- they are unclassified we know less about whether they are the same thing, so there is even
-- less reason to merge them
CREATE INDEX entities_kb_type_name_idx
    ON entities (kb_id, type_id, lower(canonical_name)) WHERE merged_into IS NULL;
CREATE INDEX entities_kb_idx ON entities (kb_id);
-- Same-name recall across types (type drift handling): the index above is prefixed with type_id,
-- which a cross-type query cannot use
CREATE INDEX entities_kb_name_idx
    ON entities (kb_id, lower(canonical_name)) WHERE merged_into IS NULL;

-- Fact ledger: SPO + two time axes (append-only, never DELETE)
CREATE TABLE facts (
    id              UUID PRIMARY KEY,
    kb_id           UUID NOT NULL REFERENCES knowledge_bases(id) ON DELETE CASCADE,
    subject_id      UUID NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    -- **Nullable**: "cannot say what the relation is" should not be a kind of relation (the same
    -- move 0009 made for concept). It used to be a builtin relation row named related_to,
    -- sitting on the ontology page side by side with the real relations -- while what it encoded
    -- was "the extractor found an edge, but the ontology has no relation for it", and that is
    -- control flow, not vocabulary.
    --
    -- Deleting it loses not one word of information: the original meaning has always been in
    -- the evidence's `proposed_predicate`, merely covered up by that fake vocabulary row. After
    -- deleting it there is in fact more information -- everything used to display as "related
    -- to", and now each one displays what the source said: acquired / runs_on / sued (see
    -- fact_surface_predicate)
    predicate_id    UUID REFERENCES relation_types(id) ON DELETE CASCADE,
    object_id       UUID REFERENCES entities(id) ON DELETE CASCADE,
    object_value    JSONB,
    valid_from      TIMESTAMPTZ,
    valid_to        TIMESTAMPTZ,
    -- **Each end records its own precision, and no date means no precision.**
    --
    -- It used to be a single `valid_precision NOT NULL DEFAULT 'day'`, so facts with no date at
    -- all still landed in the database carrying 'day' -- the ledger filled in a definite value
    -- exactly where it was ignorant, and any UI that renders "accurate to the day" off this
    -- field would read it straight back out. The default value was itself the bug: it made
    -- "never measured" and "measured to the day" look exactly alike.
    --
    -- One column describing both endpoints does not work either: for a fact that only has a
    -- valid_to ("until 2023" and the like), the column name says from while what it describes
    -- is to.
    valid_from_precision TEXT,
    recorded_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    invalidated_at  TIMESTAMPTZ,
    confidence      REAL NOT NULL DEFAULT 1.0,
    derived_by_rule UUID,
    supersedes      UUID REFERENCES facts(id),
    -- The end of the interval. There is an extra 'unknown' because the ledger originally could
    -- not say "it ended, but nobody knows on what day": `valid_to IS NULL` has always carried
    -- two meanings, so a phrase like "former CEO of Weta Digital" -- **where the ending is
    -- stated outright by the source and the date is not given** -- could only be written as
    -- null, was then read by the system as "still is to this day", and the graph would
    -- confidently assert something the source said had already ended.
    --
    --   still ongoing        valid_to IS NULL   valid_to_precision IS NULL
    --   ended, day unknown   valid_to IS NULL   valid_to_precision = 'unknown'
    --   ended in 2023        valid_to = …       valid_to_precision = 'year'
    --
    -- **We do not adopt the "put the document's date into valid_to as an upper bound" approach**
    -- (the textbook indeterminate instant): that puts a timestamp in the column that looks
    -- definite, and every reader has to check the precision before they dare use it, while not
    -- misleading people is exactly what this product sells
    valid_to_precision text,
    CHECK (object_id IS NOT NULL OR object_value IS NOT NULL),
    -- The start end has no unknown -- "it started but nobody knows when" and "nobody knows
    -- whether it started" are indistinguishable in this ledger, and forcing in a state for it
    -- would only leave readers guessing
    CONSTRAINT facts_from_precision_matches_date
      CHECK ((valid_from IS NULL) = (valid_from_precision IS NULL)),
    -- That `IS NOT NULL` is not redundant. Without it, a row with a date in `valid_to` and a
    -- NULL precision would **be let through**: `NULL IN ('year',…)` evaluates to NULL,
    -- `TRUE AND NULL` is NULL, `NULL OR FALSE` is still NULL, and a CHECK constraint counts
    -- NULL as passing. Three-valued logic is silent here
    CONSTRAINT facts_to_precision_matches_date
      CHECK (
        (valid_to IS NOT NULL AND valid_to_precision IS NOT NULL
           AND valid_to_precision IN ('year', 'month', 'day'))
        OR (valid_to IS NULL AND (valid_to_precision IS NULL OR valid_to_precision = 'unknown'))
      )
);
-- Hot-path partial indexes: invalidated rows stay out of the index (ledger boundaries and
-- cleanup, DESIGN.md §3.1)
CREATE INDEX facts_live_subject_idx ON facts (kb_id, subject_id) WHERE invalidated_at IS NULL;
CREATE INDEX facts_live_object_idx  ON facts (kb_id, object_id)  WHERE invalidated_at IS NULL;
CREATE INDEX facts_live_time_idx    ON facts (kb_id, valid_from, valid_to) WHERE invalidated_at IS NULL;
-- The invariant point lookups for temporal conflict detection: one on the subject side and one
-- on the object side (open interval + not invalidated)
CREATE INDEX facts_open_pair_idx ON facts (kb_id, subject_id, predicate_id)
    WHERE valid_to IS NULL AND invalidated_at IS NULL;
CREATE INDEX facts_open_obj_pair_idx ON facts (kb_id, object_id, predicate_id)
    WHERE valid_to IS NULL AND invalidated_at IS NULL;
-- The knowledge axis. The indexes above all live on the world axis and all recognise only live
-- rows, serving "what was true at a given moment"; these two serve a different question --
-- **when it was written in, and when it was overturned** ("what changed in our knowledge last
-- quarter", see chat's changes tool). The invalidated one is a partial index with the condition
-- negated: live rows all have a NULL invalidated_at, so taking them in would only make the index
-- as big as the table, while overturned facts are naturally a minority
CREATE INDEX facts_recorded_idx ON facts (kb_id, recorded_at DESC);
CREATE INDEX facts_invalidated_idx ON facts (kb_id, invalidated_at DESC)
    WHERE invalidated_at IS NOT NULL;

-- Evidence chain: fact ↔ source chunk (provenance as a first-class citizen)
CREATE TABLE fact_evidence (
    fact_id  UUID NOT NULL REFERENCES facts(id) ON DELETE CASCADE,
    chunk_id UUID NOT NULL REFERENCES chunks(id) ON DELETE CASCADE,
    quote    TEXT,
    -- The provenance version of the evidence: which document and which version of it (the basis
    -- for version reconciliation and for showing "evidence is stale")
    document_id UUID REFERENCES documents(id) ON DELETE CASCADE,
    doc_version INT,
    -- The model's own wording. **It belongs on the evidence, not on the fact**: facts are
    -- deduped by (kb, subject, predicate, object), so one chunk saying "runs on" and another
    -- saying "optimized for" collapse into the same row, and putting it on the fact means first
    -- writer wins and the rest are silently dropped. Evidence is one row per chunk, which is
    -- the right granularity, and it already carries quote (that chunk's supporting source text)
    -- -- the wording is the same kind of thing: the raw form of each individual observation
    proposed_predicate TEXT,
    PRIMARY KEY (fact_id, chunk_id)
);


-- Temporal conflicts (S3): auto-closing sends the ones it cannot be sure about to review, and a
-- human rules close / keep / reject_new
CREATE TABLE fact_conflicts (
    id          UUID PRIMARY KEY,
    kb_id       UUID NOT NULL REFERENCES knowledge_bases(id) ON DELETE CASCADE,
    old_fact_id UUID NOT NULL REFERENCES facts(id) ON DELETE CASCADE,
    new_fact_id UUID NOT NULL REFERENCES facts(id) ON DELETE CASCADE,
    -- no_time | simultaneous | low_confidence
    reason      TEXT NOT NULL,
    status      TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'resolved')),
    -- closed | kept_both | rejected_new
    resolution  TEXT,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    resolved_at TIMESTAMPTZ,
    UNIQUE (old_fact_id, new_fact_id)
);
CREATE INDEX fact_conflicts_open_idx ON fact_conflicts (kb_id) WHERE status = 'open';

-- **What to display** for a fact with no predicate.
--
-- A function rather than a subquery pasted into every read query: there are more than six paths
-- that read facts (graph edges, the entity panel, change history, low-confidence review,
-- document output, resolution profiles), six copies of the same SQL will drift apart sooner or
-- later, and the consequence of drift here is that the same edge gets called different names on
-- different pages.
--
-- **Determinism**: ties on occurrence count break lexicographically, so the same fact displays
-- the same word every time.
CREATE FUNCTION fact_surface_predicate(fact uuid) RETURNS text
LANGUAGE sql STABLE AS $$
    SELECT e.proposed_predicate
      FROM fact_evidence e
     WHERE e.fact_id = fact AND e.proposed_predicate IS NOT NULL
     GROUP BY e.proposed_predicate
     ORDER BY count(*) DESC, e.proposed_predicate
     LIMIT 1
$$;

-- When an entity type is adopted, which entities had their class changed.
--
-- Symmetric with fact_adoptions, and for the same reason: create the type without touching the
-- entities and the ontology has grown while the graph is no better. And the change has to be
-- undoable afterwards, or nobody will dare let the system create classes on its own.
--
-- Entities are not append-only (they are mutable rows, P0's PATCH edits them in place), so undo
-- works by recording the type from before the change rather than by a supersedes chain.
CREATE TABLE entity_retypes (
    batch_id     UUID NOT NULL,
    kb_id        UUID NOT NULL REFERENCES knowledge_bases(id) ON DELETE CASCADE,
    entity_id    UUID NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    -- **Nullable**: since 0009 the most common retype of all is exactly "from no class to a
    -- class", and this table is the only basis for undo. Make it NOT NULL and the first
    -- assignment of a class cannot be written to the ledger at all, leaving that batch of
    -- changes irreversible
    from_type_id UUID REFERENCES entity_types(id) ON DELETE CASCADE,
    to_type_id   UUID NOT NULL REFERENCES entity_types(id) ON DELETE CASCADE,
    -- Consistent with fact_adoptions: a revert marker rather than a delete -- the adoption
    -- happened, and so did the revert
    reverted_at  TIMESTAMPTZ,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- Who changed it. Nullable = adjudicated automatically by the engine, the same convention
    -- as `entity_merges.merged_by`.
    --
    -- **It only answers "who initiated this change"**, not "was this class decided by a human"
    -- -- those two were conflated once: type resolution passed down the person who clicked
    -- "run", so every entity the engine adjudicated became `type_source = human` and was never
    -- resolved again (see #117)
    actor_id     UUID REFERENCES users(id),
    PRIMARY KEY (batch_id, entity_id)
);

CREATE INDEX entity_retypes_kb_idx ON entity_retypes (kb_id, created_at DESC);

-- When a surface predicate is adopted, which fact was rewritten into which.
--
-- Without this table a rewrite leaves only the facts.supersedes pointer, and that does not cover
-- the "merged into an existing fact" case: the old row is invalidated and no successor points at
-- it. That has two consequences -- undo cannot find where it went, and entity history judges
-- "invalidated with no successor" to be rejected, so the UI says "this record was withdrawn"
-- when in fact it was merged, entirely intact, into another assertion.
--
-- It also fills in the half governance asked for: the audit row previously recorded only the
-- total, "rewrote 49 facts", and could not answer "which 49".
CREATE TABLE fact_adoptions (
    -- The batch for one adoption action; undo works in units of it
    batch_id     UUID NOT NULL,
    kb_id        UUID NOT NULL REFERENCES knowledge_bases(id) ON DELETE CASCADE,
    predicate_id UUID NOT NULL REFERENCES relation_types(id) ON DELETE CASCADE,
    old_fact_id  UUID NOT NULL REFERENCES facts(id) ON DELETE CASCADE,
    new_fact_id  UUID NOT NULL REFERENCES facts(id) ON DELETE CASCADE,
    -- superseded = a new row written to replace the old one; merged = folded into an existing
    -- row
    mode         TEXT NOT NULL,
    -- Undo does not delete the row: erasing "what happened" is the opposite of how a ledger
    -- works, and the undo is itself a human decision that entity history has to attribute
    reverted_at  TIMESTAMPTZ,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (batch_id, old_fact_id)
);

-- Entity history asks fact by fact "was this one merged away", which goes through this index
CREATE INDEX fact_adoptions_old_idx ON fact_adoptions (old_fact_id);
-- Lists the revertible batches per knowledge base
CREATE INDEX fact_adoptions_kb_idx ON fact_adoptions (kb_id, created_at DESC);

-- Blocked facts left no trace. The extractor has seven `continue` sites: subject type unknown,
-- attribute domain mismatch, value not matching the datatype, confidence too low... the fact
-- gets extracted, gets blocked, nothing is said, and the user only sees that something is
-- missing from the graph. That conflicts directly with all three principles -- "the ledger is
-- append-only", "every fact has evidence", "uncertainty floats up to a human".

-- Grouped by document: this can both compute a single document's "how many did not land" and
-- aggregate at the KB level. It also fixes the lifecycle for free -- ontology_misses was only
-- cleared on a whole-knowledge-base rebuild (graph.rs), never on a source-level re-extraction,
-- so it accumulated stale counts; clearing per document means every re-extraction counts
-- afresh.
CREATE TABLE extraction_drops (
    kb_id       UUID NOT NULL REFERENCES knowledge_bases(id) ON DELETE CASCADE,
    document_id UUID NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
    -- Machine-aggregatable reason code (attr_domain_mismatch / low_confidence / ...)
    reason      TEXT NOT NULL,
    -- The specific object under that reason (an attribute key, a predicate name,
    -- "salary@organization")
    detail      TEXT NOT NULL,
    count       INT NOT NULL DEFAULT 1,
    -- One example, so a person can see at a glance what was dropped ("Acme Corp → salary")
    example     TEXT,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (kb_id, document_id, reason, detail)
);

CREATE INDEX extraction_drops_doc_idx ON extraction_drops (document_id);

