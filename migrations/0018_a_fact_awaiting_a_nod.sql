-- A fact awaiting a nod (see docs/decisions/0015).
--
-- This started with a real observation: the conversation said "remember that Acme moved its
-- headquarters to Shenzhen", the assistant replied "recorded", and what landed in the graph was
-- an edge with an **empty predicate and 0.9 confidence** -- the ontology has no relation like
-- "relocated to", and when extraction cannot land on one it leaves it empty (per 0010 that is
-- correct). **What was said and what went in were not the same thing, and the human had no way
-- to find out.**
--
-- **Its own table; it does not go in `facts`.**
--
-- The first version added a `nod` column to `facts`. That is exactly the wrong shape 0013 wrote
-- down:
--
-- > We tried stuffing it into `facts` as a `derived_by_rule` flag bit, and the problem with that
-- > version was that **the failure direction was backwards**: the repo has forty-odd queries that
-- > read `facts`, and only one of them knew about the flag...
-- > Once they are separate, forgetting the UNION means derived facts are **invisible**, not
-- > **mixed in**.
--
-- Today there are 27 queries that scoop up live facts via `invalidated_at IS NULL`, spread over
-- 6 files. Patching the filter into each one, and missing a single one, means a fact nobody
-- nodded at gets mixed into the graph -- and preventing exactly that is the entire reason this
-- table exists. Once they are separate, forgetting to read it means "the pending queue is
-- invisible", not "unconfirmed facts made it into the graph".
--
-- **It only stops interactive single-row writes; it does not stop bulk ingestion.** Load 500
-- documents, pull ten thousand facts out of them, and asking a human to confirm them one by one
-- is impossible; that path stays optimistic-write plus after-the-fact review. Whereas `remember`
-- is one sentence at a time with the human right there in the conversation -- the moment when
-- confirmation is cheapest is exactly the moment in front of you.
CREATE TABLE pending_facts (
    id           UUID PRIMARY KEY,
    kb_id        UUID NOT NULL REFERENCES knowledge_bases(id) ON DELETE CASCADE,
    subject_id   UUID NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    -- Nullable, consistent with `facts.predicate_id`: left empty when the ontology has no
    -- matching relation (0010).
    -- **And that emptiness is precisely what the human needs to see** -- in that real case the
    -- empty predicate was the very reason it should have been rejected
    predicate_id UUID REFERENCES relation_types(id) ON DELETE SET NULL,
    object_id    UUID REFERENCES entities(id) ON DELETE CASCADE,
    object_value JSONB,
    -- How the model itself phrased it. When the predicate comes up empty, this is what the human
    -- uses to judge "should the ontology grow this relation"
    proposed_predicate TEXT,

    valid_from   TIMESTAMPTZ,
    valid_from_precision TEXT,
    valid_to     TIMESTAMPTZ,
    valid_to_precision   TEXT,
    confidence   REAL NOT NULL DEFAULT 0.5,

    -- Which sentence of memory it came from. **The confirmation UI must show the original
    -- sentence side by side with the triple** -- listing the triple alone amounts to asking the
    -- human to judge its correctness out of thin air
    chunk_id     UUID NOT NULL REFERENCES chunks(id) ON DELETE CASCADE,
    -- Whose words. `remember` does not record this today; adding it here as well (the gap 0015
    -- called out by name)
    proposed_by  UUID REFERENCES users(id),
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- The pending queue is scooped up per database, and it also needs a count
CREATE INDEX pending_facts_kb_idx ON pending_facts (kb_id, created_at DESC);
-- The several facts pulled from one sentence of memory have to be shown together
CREATE INDEX pending_facts_chunk_idx ON pending_facts (chunk_id);

-- What has been rejected should not be flushed back in by the next round of re-extraction.
--
-- Over in `concept_mappings` this is handled by `status = 'rejected'` blocking repeat proposals;
-- same idea here, except **rejected records cannot stay in pending_facts** -- that table means
-- "waiting for a human to look", and mixing in things already looked at makes the counts lie.
-- So there is a second table that records only "this triple was rejected in this database".
CREATE TABLE rejected_facts (
    kb_id        UUID NOT NULL REFERENCES knowledge_bases(id) ON DELETE CASCADE,
    subject_id   UUID NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    -- The predicate is nullable, so it cannot go in the primary key; dedup lookups use a
    -- COALESCE'd expression index
    predicate_id UUID REFERENCES relation_types(id) ON DELETE SET NULL,
    object_id    UUID REFERENCES entities(id) ON DELETE CASCADE,
    rejected_by  UUID REFERENCES users(id),
    rejected_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX rejected_facts_lookup_idx
    ON rejected_facts (kb_id, subject_id, object_id);
