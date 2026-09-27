-- Reasoner: finishing R0 + R1 materialised inference (see docs/decisions/0002).

-- ============ The other half of R0: the ontology's own self-consistency ============
--
-- `axiom_violations` is about "a fact contradicts a definition"; this one is about "a
-- definition cannot stand up on its own". Splitting them is not a taxonomy fetish: a
-- self-contradictory ontology makes **every** conclusion at the fact layer suspect -- if a
-- predicate declares both symmetric and asymmetric, then every asymmetry violation reported
-- against it rests on a premise that never held in the first place. That is why this tier
-- sorts first in the UI.
--
-- The shape differs too: that table's two columns are foreign keys into `facts`, whereas
-- these point at classes and predicates.
CREATE TABLE ontology_defects (
    id      UUID PRIMARY KEY,
    kb_id   UUID NOT NULL REFERENCES knowledge_bases(id) ON DELETE CASCADE,
    -- symmetric_and_asymmetric  a predicate declares both (only holds for the empty property)
    -- transitive_and_functional a combination OWL 2 DL explicitly forbids
    -- subclass_cycle            A ⊂ B ⊂ A; the CHECK on the table only stops self-loops
    -- disjoint_with_ancestor    a class disjoint from its own ancestor → can never have instances
    -- inherits_disjoint         two ancestors disjoint → same as above
    kind    TEXT NOT NULL CHECK (kind IN (
                'symmetric_and_asymmetric', 'transitive_and_functional',
                'subclass_cycle', 'disjoint_with_ancestor', 'inherits_disjoint')),
    -- **Both columns are bare UUIDs, no foreign key.** The first two kinds point at
    -- relation_types, the last three at entity_types -- one column pointing at two tables is
    -- not something a foreign key can express. And this is derived state: any ontology edit
    -- recomputes the whole batch, so rows pointing at deleted objects disappear on the next
    -- round by themselves; no need for a cascade as a backstop
    subject UUID NOT NULL,
    other   UUID,
    -- The path of the cycle (ordered by class). Empty for the rest
    path    UUID[] NOT NULL DEFAULT '{}',
    status  TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'resolved')),
    -- fixed     changed in the ontology (edit the declaration, break the
    --           inheritance, drop the disjoint)
    -- accepted  a human looked and decided no change was needed
    resolution  TEXT CHECK (resolution IN ('fixed', 'accepted')),
    decided_by  UUID REFERENCES users(id),
    decided_at  TIMESTAMPTZ,
    detected_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (kb_id, kind, subject, other)
);

CREATE INDEX ontology_defects_open_idx ON ontology_defects (kb_id, detected_at DESC)
    WHERE status = 'open';

-- ============ R1: rules and derivations ============

-- Rules are **compiled from ontology axioms only**; there is no user-defined DSL -- that is a
-- different product (0002).
--
-- Why a table instead of stuffing the rule kind into the derived row: `facts.derived_by_rule`
-- needs something it can point at, and "which rule did this one come from" has to be
-- aggregated by rule in two places -- explanation (R2), and "if we retract this axiom, which
-- derivations have to go with it".
--
-- It is derived state: recompiled from the ontology before every inference run. So identity
-- is `(kb, predicate, kind)` rather than a serial -- recompilation has to recognise "still the
-- same rule", otherwise every run points `derived_facts.rule_id` at a fresh id and the whole
-- history is severed.
--
-- **Rules whose axiom was retracted are not deleted.** Already-invalidated rows in
-- `derived_facts` still point at them, and explaining "which rule it was derived by at the
-- time" needs them to still be there. A KB only ever has a handful of rules; keeping them
-- takes up no room.
CREATE TABLE rules (
    id           UUID PRIMARY KEY,
    kb_id        UUID NOT NULL REFERENCES knowledge_bases(id) ON DELETE CASCADE,
    predicate_id UUID NOT NULL REFERENCES relation_types(id) ON DELETE CASCADE,
    -- transitive | symmetric. The projection side of `inverseOf` and `subPropertyOf` is not
    -- persisted yet, so there is nothing to compile -- a missing rule is not a defect, it is
    -- the same "no declaration, no inference" rule
    kind         TEXT NOT NULL CHECK (kind IN ('transitive', 'symmetric')),
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (kb_id, predicate_id, kind)
);

-- Inferred facts. **Their own table; they do not go into `facts`.**
--
-- We tried putting them in `facts` with a `derived_by_rule` flag; that version's problem was
-- that **the failure direction was backwards**: there are forty-odd queries in this repo that
-- read `facts`, exactly one of which knew about the flag, so a newly written query treats
-- derivations as assertions by default and you have to remember to add the filter. The person
-- who wrote the feature (me) missed two spots on the spot -- the low-confidence review queue
-- would hand derived facts to a human to Confirm/Reject (confirming an inference is
-- meaningless, and rejecting it just derives it back unchanged next round, because the
-- premises are still there), and temporal reconciliation would use an inference to close out
-- an assertion (the engine editing a human's data with its own conclusions -- precisely what
-- criterion 2 of 0001 forbids).
--
-- Once they are split apart, forgetting a UNION means derivations are **invisible**, not
-- **mixed in**.
--
-- Two more reasons:
--
-- One, **the columns are genuinely different**. A derivation has no `supersedes` (it has no
--    "correction" semantics), no `fact_evidence` (its evidence is its premises, over in
--    `fact_derivations`), and `confidence` means something else (computed, not self-reported
--    by the model). In one shared table every one of those columns is borrowed.
--
-- Two, **they are a tier apart in volume**. 0002 measured 185 → 828 on real corpora;
--    derivations can be more than four times the assertions. Making every single `facts`
--    query filter out most of the rows is a price paid for nothing.
CREATE TABLE derived_facts (
    id           UUID PRIMARY KEY,
    kb_id        UUID NOT NULL REFERENCES knowledge_bases(id) ON DELETE CASCADE,
    subject_id   UUID NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    -- **NOT NULL**, unlike `facts.predicate_id`: rules hang off predicates, so with no
    -- predicate there is no rule, and this row could not have been derived
    predicate_id UUID NOT NULL REFERENCES relation_types(id) ON DELETE CASCADE,
    -- NOT NULL likewise: axioms talk about relations between entities, so attribute facts
    -- with a literal object take no part in inference
    object_id    UUID NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    rule_id      UUID NOT NULL REFERENCES rules(id),
    -- The validity interval is the intersection of the premises' (the semantics given in
    -- 0002's open questions). Precision follows the same invariant as `facts`: a precision
    -- only where there is a date
    valid_from   TIMESTAMPTZ,
    valid_to     TIMESTAMPTZ,
    valid_from_precision TEXT,
    valid_to_precision   TEXT,
    -- The smallest one among the premises. A chain is only as credible as its weakest link
    confidence   REAL NOT NULL DEFAULT 1.0,
    derived_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- Set when the premises are gone, **the row is not deleted**: exactly isomorphic to
    -- rejecting a fact, it leaves "we once derived this, and later the premise went away" on
    -- the record axis, which the entity history page can display as-is (0002 section 3)
    invalidated_at TIMESTAMPTZ,
    CONSTRAINT derived_from_precision_matches_date
      CHECK ((valid_from IS NULL) = (valid_from_precision IS NULL)),
    CONSTRAINT derived_to_precision_matches_date
      CHECK ((valid_to IS NULL) = (valid_to_precision IS NULL))
);

-- The graph fetches edges by (subject, object); reconciliation recognises "still the same one"
-- by triple + interval
CREATE INDEX derived_facts_live_idx
    ON derived_facts (kb_id, subject_id, object_id) WHERE invalidated_at IS NULL;
CREATE UNIQUE INDEX derived_facts_identity_idx
    ON derived_facts (kb_id, subject_id, predicate_id, object_id, valid_from, valid_to)
    WHERE invalidated_at IS NULL;

-- One layer of the proof tree: which premises this derivation used.
--
-- **The whole tree is not stored, only the direct premises.** Expanding recursively along this
-- table is the complete proof (what R2 wants). Storing the whole tree records the same
-- information N times, where N is the number of paths.
--
-- Premises are always assertions (`facts`): derivations are excluded from the inference input,
-- otherwise one call's output becomes the next call's input and a rerun's result depends on
-- the previous round's leftovers.
--
-- `seq` guarantees the order: the proof of `A→B→C→D` has to read in that order for a human to
-- follow how the chain runs.
CREATE TABLE fact_derivations (
    derived_fact_id UUID NOT NULL REFERENCES derived_facts(id) ON DELETE CASCADE,
    premise_fact_id UUID NOT NULL REFERENCES facts(id) ON DELETE CASCADE,
    seq             INT  NOT NULL,
    PRIMARY KEY (derived_fact_id, seq)
);

-- "this premise was retracted, which derivations have to be invalidated with it" -- a reverse
-- lookup, which the primary key does not cover
CREATE INDEX fact_derivations_premise_idx ON fact_derivations (premise_fact_id);

-- The inference switch. **Off by default**: R1 adds things to the graph, and criterion 2 of
-- 0001 says "the ontology guides, it does not enforce" -- a declaration can be wrong, so we
-- should not be editing the graph according to it before the user has said anything.
--
-- On the KB rather than the deployment: one KB's ontology carries axioms, another is entirely
-- extracted from free text; whether inference is appropriate differs per KB.
ALTER TABLE knowledge_bases
    ADD COLUMN materialize_inferences BOOLEAN NOT NULL DEFAULT FALSE;

-- How often to re-infer. **It must be scheduled, hand-clicking is not enough**: facts change
-- continuously (every document extraction adds edges) while derivations are only computed at
-- the moment the run happens. Without a schedule, the derivations in the graph are **missing**
-- as soon as the next document arrives -- not wrong (the premises are still there), just new
-- chains never derived, and that kind of absence is invisible in the UI.
--
-- Same shape as source sync: one interval + one last-run time, and the scheduler sweeps for
-- due rows every minute. 60 minutes is a guess: inference is pure computation and costs no
-- money, but until incremental maintenance (0002 R3) is built every run is a full-KB
-- recompute, so it should not be too dense either.
ALTER TABLE knowledge_bases
    ADD COLUMN inference_interval_minutes INT NOT NULL DEFAULT 60
        CHECK (inference_interval_minutes BETWEEN 5 AND 10080);

-- When the last inference finished. **The comparison at each due time is just this run's
-- result against what is already in the database** -- `materialize` was already doing exactly
-- that (match the computed set against the existing one, insert the extras, retire the
-- missing), so "comparison" is not a new mechanism, it is recording the result of that
-- comparison for a human to see
ALTER TABLE knowledge_bases ADD COLUMN last_inference_at TIMESTAMPTZ;
