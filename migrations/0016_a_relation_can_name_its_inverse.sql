-- The inverse and super-property of a relation (the last two rule sources for R1, see
-- docs/decisions/0002).
--
-- ADR 0002 gives R1 four rule sources: `TransitiveProperty` / `SymmetricProperty` /
-- `inverseOf` / `subPropertyOf`. The first two have been running all along, the last two
-- **did not even have columns** -- the `rules.kind` comment in 0013 wrote that down:
--
-- > the projection side of `inverseOf` and `subPropertyOf` is not stored yet, so there is
-- > nothing to compile either
--
-- This migration adds the projection side. The consequence of the gap is not "a few fewer
-- inferences", it is **asymmetric answers**: the ontology declares `works_at⁻¹ = employs`,
-- and yet asking "who works at Acme" and "whom does Acme employ" gives different answers
-- unless both directions were asserted -- which is exactly the duplicated labour R1 is
-- there to eliminate.
ALTER TABLE relation_types
    -- `p⁻¹ = q`. **Stored one way, used both ways.**
    --
    -- No trigger to auto-backfill `q.inverse_of = p`: that would hide a semantic rule inside
    -- the database, and there is more than one way around it (RDF import, direct SQL).
    -- Normalise when the axioms are read instead -- that one load site owns this, cannot be
    -- bypassed, and can actually be tested (`reasoning::axioms_of`).
    ADD COLUMN inverse_of UUID REFERENCES relation_types(id) ON DELETE SET NULL,
    -- `p ⊑ q`: assert the specific one and the general one holds too (`ceo_of ⊑ works_at`).
    -- The chain has to be kept from cycling, the check goes in R0 -- the same class of
    -- problem as parent-class cycles in `entity_types`
    ADD COLUMN sub_property_of UUID REFERENCES relation_types(id) ON DELETE SET NULL;

-- Nothing can be its own super-property. **A property can be its own inverse** -- that is
-- the same as symmetric, and it is a legal declaration (R0 will suggest saying `symmetric`
-- instead, which is more direct, but it is not an error)
ALTER TABLE relation_types
    ADD CONSTRAINT relation_types_sub_property_not_self
        CHECK (sub_property_of IS NULL OR sub_property_of <> id);

-- Normalisation looks "who points at me" up in reverse, once per predicate when rules
-- are compiled
CREATE INDEX relation_types_inverse_idx ON relation_types (inverse_of)
    WHERE inverse_of IS NOT NULL;
CREATE INDEX relation_types_sub_property_idx ON relation_types (sub_property_of)
    WHERE sub_property_of IS NOT NULL;

-- Two CHECKs open up along with it: the new rules and the new defects are both values those
-- two tables in 0013 did not foresee.
--
-- **This is not patching an oversight, back then they genuinely did not exist yet.** The
-- comment in 0013 says "the projection side of `inverseOf` and `subPropertyOf` is not stored
-- yet, so there is nothing to compile either" -- this migration adds the projection side, so
-- naturally the constraints have to widen with it.
ALTER TABLE rules DROP CONSTRAINT IF EXISTS rules_kind_check;
ALTER TABLE rules ADD CONSTRAINT rules_kind_check
    CHECK (kind IN ('transitive', 'symmetric', 'inverse', 'sub_property'));

-- Three new ontology self-checks: a property that is its own inverse (the same as symmetric,
-- suggest rewriting it), an inverse that does not point back (loading only fills gaps and
-- never overwrites, so the contradiction is left to be reported here), and sub-property
-- cycles
ALTER TABLE ontology_defects DROP CONSTRAINT IF EXISTS ontology_defects_kind_check;
ALTER TABLE ontology_defects ADD CONSTRAINT ontology_defects_kind_check
    CHECK (kind IN (
        'symmetric_and_asymmetric', 'transitive_and_functional',
        'subclass_cycle', 'disjoint_with_ancestor', 'inherits_disjoint',
        'inverse_of_itself', 'inverse_not_mutual', 'sub_property_cycle'
    ));
