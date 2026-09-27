-- Ontology: unmatched-extraction counts, imports, proposals, and human-approved refinement
-- pairs.

-- Types/relations met during extraction that fall outside the allowlist: not garbage, but the
-- signal that the ontology wants extending
CREATE TABLE ontology_misses (
    kb_id      UUID NOT NULL REFERENCES knowledge_bases(id) ON DELETE CASCADE,
    -- attribute_type is kept apart from relation_type: when a predicate outside the vocabulary
    -- carries a literal value (`founding_date: "2015"`), what is missing is an attribute and not
    -- a relation, and recording it wrongly sends the ontology proposal off to create a relation
    kind       TEXT NOT NULL
               CHECK (kind IN ('entity_type', 'relation_type', 'attribute_type')),
    key        TEXT NOT NULL,
    example    TEXT,
    count      INT NOT NULL DEFAULT 1,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- A human said no. **Flag it, do not delete it**: dismiss used to be a DELETE, and the next
    -- extraction to meet the same word inserted it straight back -- the user's "no" did not
    -- survive one round of extraction. With automatic ontology extension switched on, that
    -- amounts to the system overriding a human's explicit decision
    dismissed_at TIMESTAMPTZ,
    PRIMARY KEY (kb_id, kind, key)
);

-- The first layer of an ontology import: faithful to the source text.
--
-- The projection covers only the part we can consume today (classes, labels, rdfs:comment,
-- subClassOf, object/data properties, functional, domain/range). **What we cannot read is not an
-- error, it is "not projected yet"** -- the source text is stored in the blob store addressed by
-- content, and gets re-run when a reasoner comes online or when we add a new consumer, with
-- nothing for the user to do. That demotes "we cannot express it" from a capability gap to "the
-- projection does not cover it yet".
CREATE TABLE ontology_imports (
    id            UUID PRIMARY KEY,
    kb_id         UUID NOT NULL REFERENCES knowledge_bases(id) ON DELETE CASCADE,
    -- The content fingerprint of the blob; importing the same file twice does not take up the
    -- space twice
    sha256        TEXT NOT NULL,
    filename      TEXT NOT NULL,
    -- turtle | rdfxml
    format        TEXT NOT NULL,
    byte_size     BIGINT NOT NULL,
    -- Projection version: when the projection logic changes later, this is how we know which
    -- imports should be re-run
    projection_version INT NOT NULL DEFAULT 1,
    -- What this projection did (counts and details of created/updated/not-projected-yet), shared
    -- by the preview and by after-the-fact auditing
    summary       JSONB NOT NULL DEFAULT '{}'::jsonb,
    -- Who imported it. Left NULL once the account is deleted, the same rule as the audit ledger
    imported_by   UUID REFERENCES users(id) ON DELETE SET NULL,
    imported_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX ontology_imports_kb_idx ON ontology_imports (kb_id, imported_at DESC);


-- Ontology proposals, persisted.
--
-- It used to live only in browser memory (`Ontology.tsx`'s `useState<OntologyProposals>`): one
-- refresh, one navigation away, one crash, and the whole batch of proposals was gone; seeing
-- them again meant re-running the model.
--
-- **What was lost was not the raw material.** The unmatched phrasings were in `ontology_misses`
-- all along. What was lost was the **clustering result** -- which phrasings ended up under the
-- same proposal, and that estimate of "adopting this will reclassify N of them". And that is
-- precisely the only thing by which a merge can be checked: 0003 records the model suggesting
-- that `optimized_for` be merged into `runs_on` ("optimized for RTX" is not the same as "runs on
-- RTX"), and **it was caught only because the tooltip made visible which phrasings had been
-- merged in**, and it became the direct evidence for "merging must not be fully automatic".
-- Something that can be checked should not live only for the lifetime of one page.
CREATE TABLE ontology_proposals (
    id         UUID PRIMARY KEY,
    kb_id      UUID NOT NULL REFERENCES knowledge_bases(id) ON DELETE CASCADE,
    -- The proposal's bucket, named after the four sections the API returns:
    -- entity_types | relation_types | attribute_types | map_to
    section    TEXT NOT NULL,
    key        TEXT NOT NULL,
    -- That one proposal exactly as it came: label, description, reason, forms, datatype,
    -- temporal...
    --
    -- **Stored as JSONB rather than split into columns**: the four sections genuinely have
    -- different shapes (relations have temporal and forms, attributes have datatype, map_to has
    -- a target), so splitting them means either four tables or one sparse wide table. This JSON
    -- is also exactly what the frontend consumes, so storing it verbatim means not changing the
    -- contract. Asking which phrasings got merged still works (`payload->'forms'`)
    payload    JSONB NOT NULL,
    -- open = still waiting for a human to look; adopted / rejected = somebody has taken a
    -- position
    status     TEXT NOT NULL DEFAULT 'open'
               CHECK (status IN ('open', 'adopted', 'rejected')),
    decided_by UUID REFERENCES users(id),
    decided_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- One row only per key, per bucket, per kb. Re-running Suggest refreshes it, it does not
    -- pile on another
    UNIQUE (kb_id, section, key)
);

-- "How many are still waiting to be looked at" is the question this table is asked most often
-- (0003's other gap: with the automatic-extension switch turned off there was no "N new
-- phrasings since last time" nudge, so the signal sat in a panel and nobody went to look)
CREATE INDEX ontology_proposals_open_idx
    ON ontology_proposals (kb_id, created_at DESC)
    WHERE status = 'open';

-- Human-approved "coarse class → fine class" pairs. Approved once, and the same pair never goes
-- to a human again.
--
-- Why it is needed: the awaiting-human bucket is triggered by "is the selected class inside the
-- coarse class's subtree", and measured in practice, what that criterion tests is often not risk
-- but **whether the seed classes are wired up to the imported vocabulary's classification tree
-- at all**. schema.org's Place started a separate key, place, and the built-in location has not
-- one single subclass, so every single location → city counts as crossing axes -- 14 reported
-- out of 24 entities, every one of them correct.
--
-- Crossing axes is a property of the (coarse class, target class) pair, not a property of the
-- entity. Once a human has seen that "a thing under location can be a city", the second city
-- should not ask all over again.
CREATE TABLE type_refinement_pairs (
    kb_id       uuid NOT NULL REFERENCES knowledge_bases(id) ON DELETE CASCADE,
    from_type_id uuid NOT NULL REFERENCES entity_types(id) ON DELETE CASCADE,
    to_type_id  uuid NOT NULL REFERENCES entity_types(id) ON DELETE CASCADE,
    approved_by uuid REFERENCES users(id),
    approved_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (kb_id, from_type_id, to_type_id)
);
