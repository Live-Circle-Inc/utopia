-- Entity resolution: the same name is not the same person.
-- Design in docs/DESIGN.md §4: a name is only a clue for recalling candidates, identity is decided
-- by context (profile vector + relation compatibility); rather split than merge, the grey zone
-- lands in the review queue first, LLM batch adjudication runs in the background, and human final
-- review is the backstop.



-- Resolution review queue: grey-zone pairs suspected of being the same entity.
-- stage: adjudicating = waiting on LLM batch adjudication; human = the LLM was unsure or no model
-- is configured, waiting on human final review.
-- status: pending → merged / kept.
CREATE TABLE resolution_reviews (
    id         UUID PRIMARY KEY,
    kb_id      UUID NOT NULL REFERENCES knowledge_bases(id) ON DELETE CASCADE,
    left_id    UUID NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    right_id   UUID NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    score      REAL NOT NULL DEFAULT 0,
    reason     TEXT,
    stage      TEXT NOT NULL DEFAULT 'adjudicating' CHECK (stage IN ('adjudicating', 'human')),
    status     TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'merged', 'kept')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    decided_at TIMESTAMPTZ,
    decided_by UUID REFERENCES users(id)
);
CREATE UNIQUE INDEX resolution_reviews_pair_idx
    ON resolution_reviews (kb_id, least(left_id, right_id), greatest(left_id, right_id))
    WHERE status = 'pending';
CREATE INDEX resolution_reviews_kb_pending_idx
    ON resolution_reviews (kb_id, created_at) WHERE status = 'pending';

-- LLM adjudication cache: never pay twice for the same pair (name + context-digest hash); same
-- being NULL means the model was not sure either
CREATE TABLE resolution_verdicts (
    kb_id      UUID NOT NULL REFERENCES knowledge_bases(id) ON DELETE CASCADE,
    pair_key   TEXT NOT NULL,
    same       BOOLEAN,
    confidence REAL NOT NULL DEFAULT 0,
    model      TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (kb_id, pair_key)
);

-- Merge log: records the facts that were moved/invalidated plus a snapshot of the target
-- entity's profile, so a rollback can be exact
CREATE TABLE entity_merges (
    id                    UUID PRIMARY KEY,
    kb_id                 UUID NOT NULL REFERENCES knowledge_bases(id) ON DELETE CASCADE,
    source_id             UUID NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    target_id             UUID NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    moved_subject_facts   UUID[] NOT NULL DEFAULT '{}',
    moved_object_facts    UUID[] NOT NULL DEFAULT '{}',
    invalidated_facts     UUID[] NOT NULL DEFAULT '{}',
    -- correction rows produced by post-merge temporal reconciliation (the merge itself is the
    -- cause, so a rollback undoes them along with it)
    temporal_corrections  UUID[] NOT NULL DEFAULT '{}',
    target_profile_before vector,
    target_profile_n_before INTEGER NOT NULL DEFAULT 0,
    -- rollback snapshot for type reconciliation (a concept target promoted to a concrete type)
    target_type_before    UUID REFERENCES entity_types(id),
    -- NULL = automatic merge (high-confidence LLM adjudication)
    merged_by             UUID REFERENCES users(id),
    reason                TEXT,
    created_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    reverted_at           TIMESTAMPTZ
);
CREATE INDEX entity_merges_kb_idx ON entity_merges (kb_id, created_at DESC);
