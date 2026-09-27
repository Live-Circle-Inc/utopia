-- When a derivation collides with an assertion, make the yielding visible instead of silent
-- (docs/decisions/0017).
--
-- 0002 settled asserted > derived: an inferred fact that collides with an assertion in the
-- ledger simply does not land. Until now that step left nothing behind -- the works_at inferred
-- from `ceo_of ⊑ works_at` was gone, nobody knew it had ever happened, and so nobody knew to go
-- and look at whether the extraction was wrong, whether the old assertion should be closed, or
-- whether the two "Mira"s are in fact one person.
--
-- The consistency check gains one more kind, `derived_contradiction`: left is the assertion that
-- was hit, right is the last premise of the derivation, path is all of the premises; the
-- inferred triple itself never landed and has no id to point at, so it goes into `detail`.
-- The resolutions gain one more, `fact_closed` -- the most common fix is to give the old
-- assertion an end date.
--
-- Derivations colliding with each other (two rules that together produce mutually exclusive
-- conclusions) are aggregated by rule pair into `ontology_defects` as a single `rules_disagree`,
-- with `detail` recording the rule pair and a few examples. One row per pair would only drown
-- the Review queue.

ALTER TABLE axiom_violations
    DROP CONSTRAINT axiom_violations_kind_check,
    ADD CONSTRAINT axiom_violations_kind_check CHECK (kind IN (
        'self_loop', 'asymmetry', 'cycle', 'functional', 'signature',
        'derived_contradiction'
    )),
    DROP CONSTRAINT axiom_violations_resolution_check,
    ADD CONSTRAINT axiom_violations_resolution_check CHECK (resolution IN (
        'fact_retracted', 'fact_closed', 'axiom_relaxed', 'accepted'
    )),
    ADD COLUMN detail JSONB NOT NULL DEFAULT '{}'::jsonb;

ALTER TABLE ontology_defects
    DROP CONSTRAINT ontology_defects_kind_check,
    ADD CONSTRAINT ontology_defects_kind_check CHECK (kind IN (
        'symmetric_and_asymmetric', 'transitive_and_functional', 'subclass_cycle',
        'disjoint_with_ancestor', 'inherits_disjoint',
        'inverse_of_itself', 'inverse_not_mutual', 'sub_property_cycle',
        'rules_disagree'
    )),
    ADD COLUMN detail JSONB NOT NULL DEFAULT '{}'::jsonb;
