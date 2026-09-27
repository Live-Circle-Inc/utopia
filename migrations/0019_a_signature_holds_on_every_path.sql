-- Signature violations become a kind of consistency check (see #190 / #196, the first to-do item
-- in docs/decisions/0012).
--
-- #138 straightened the direction out at extraction-write time using the relation's domain: if
-- the subject doesn't fit and the object does, swap them; if neither fits, leave the predicate
-- empty. But extraction is not the only path that writes predicates: **adoption** hangs a
-- predicate back onto old facts (#190, measured to push the violation rate from 0 up to 12.3%),
-- and **merging** swaps out the subject's type (#196). The guard sat on one path only, and the
-- other two each went around it.
--
-- The fix has two layers: the write-time judgement is factored into one place
-- (`ontology::judge_direction`), shared by extraction and adoption; and the ledger layer gets a
-- backstop -- the consistency check (0002 R0) checks one more kind, `signature`: a live fact whose
-- subject is not in the domain the predicate declares, or whose object is not in its range. After
-- a merge we check the facts that were moved right away; a manual check run scans everything.
-- **Whichever path wrote it backwards, a human can see it in Review**, and the ways out are the
-- same as for the other violations: retract the fact, loosen the axiom (drop that domain
-- declaration), or accept that both stand.
--
-- Only the CHECK constraint changes: the shape of the table is good enough -- a signature
-- violation involves a single fact, with left and right being the same one, same as the
-- reflexive kind.
ALTER TABLE axiom_violations DROP CONSTRAINT axiom_violations_kind_check;
ALTER TABLE axiom_violations ADD CONSTRAINT axiom_violations_kind_check
    CHECK (kind IN ('self_loop', 'asymmetry', 'cycle', 'functional', 'signature'));
