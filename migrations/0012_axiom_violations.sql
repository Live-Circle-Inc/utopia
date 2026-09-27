-- Axiom violations: the contradictions the consistency check (0002 R0) turns up.
--
-- **Deliberately not reusing `fact_conflicts`**, even though both are "two facts fighting,
-- waiting for a human to rule". The reason is that the ruling itself is a different act:
--
--   fact_conflicts    temporal conflict, asking "which one is right"
--                     → closed / kept_both / rejected_new, all three answers change facts
--   axiom_violations  axiom violation, asking "is the data wrong or the definition"
--                     → retract the fact, or go change that axiom in the ontology
--
-- That second way out is essential. A user imports a FOAF file in which some property is
-- declared asymmetric, while in their own corpus that relation really is bidirectional -- what
-- should change then is the ontology, not twenty facts. Force both into one table and the
-- `resolution` column has to express two sets of semantics at once, while the code reading it
-- has to look at `reason` first to know how to interpret `resolution`.
--
-- The shape does not match either: `fact_conflicts` assumes a conflict is always "the new one
-- displaces the old" (an old/new column pair), while a reflexivity violation has only **one**
-- fact (it contradicts itself), and a cycle is a **chain**.

CREATE TABLE axiom_violations (
    id         UUID PRIMARY KEY,
    kb_id      UUID NOT NULL REFERENCES knowledge_bases(id) ON DELETE CASCADE,
    -- self_loop  reflexive: subject and object are the same, predicate declares Irreflexive
    -- asymmetry  asymmetric: A→B and B→A coexist
    -- cycle      transitive cycle: A→B→C→A, predicate is both Transitive and Asymmetric
    -- functional cardinality: two values on a side (subject or object) that must be unique
    kind       TEXT NOT NULL
               CHECK (kind IN ('self_loop', 'asymmetry', 'cycle', 'functional')),
    -- The two facts involved. **Both columns are identical for the reflexive kind** -- a fact
    -- contradicts itself, no second fact needed; a cycle takes head and tail, the rest in path
    left_fact  UUID NOT NULL REFERENCES facts(id) ON DELETE CASCADE,
    right_fact UUID NOT NULL REFERENCES facts(id) ON DELETE CASCADE,
    -- The full path of the cycle, ordered by fact; empty for the other three kinds.
    -- Kept because "A→B→C→A" is far more useful than "A contradicts C" -- a human has to walk
    -- it once to know which one to retract.
    --
    -- A bare UUID array rather than a join table: it is a piece of **evidence** (that cycle
    -- looked like this at the time), not a set of relations that needs querying. There is no
    -- "which cycles pass through this fact" kind of query
    path       UUID[] NOT NULL DEFAULT '{}',
    status     TEXT NOT NULL DEFAULT 'open'
               CHECK (status IN ('open', 'resolved')),
    -- fact_retracted the data was judged wrong, the fact was retracted
    -- axiom_relaxed  the definition was judged wrong, that axiom was changed in the ontology
    -- accepted       both sides are right, a human accepts the coexistence (not reported again)
    resolution TEXT CHECK (resolution IN ('fact_retracted', 'axiom_relaxed', 'accepted')),
    decided_by UUID REFERENCES users(id),
    decided_at TIMESTAMPTZ,
    detected_at TIMESTAMPTZ NOT NULL DEFAULT now(),

    -- Re-running does not insert the same contradiction twice. The check is deterministic
    -- (cycles are deduplicated by sorting on fact id), so the same cycle yields the same
    -- head/tail pair every time
    UNIQUE (kb_id, kind, left_fact, right_fact)
);

-- The Review page only fetches the ones awaiting a verdict
CREATE INDEX axiom_violations_open_idx ON axiom_violations (kb_id, detected_at DESC)
    WHERE status = 'open';
