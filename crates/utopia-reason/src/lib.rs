//! Consistency checking: measuring the facts already in the ledger against the axioms the
//! ontology declares (see `docs/decisions/0002` R0).
//!
//! **Does not write the `facts` table, does not touch the database either.** This layer only
//! judges: the input is edges and axioms, the output is "which facts contradict each other".
//! Reading and persisting live in `utopia-store` / `utopia-server`.
//!
//! Splitting it this way is not fastidiousness. The ADR says R0's value is that "the hard parts
//! of the engine -- rule representation, evaluation, termination -- are all built and verified,
//! with a risk surface of zero", and those hard parts are pure logic: with no database running
//! you can put hundreds of cases through it, including shapes a real corpus may never happen to
//! produce (an eleven-node cycle, a self-loop nested inside a cycle, the same pair of nodes
//! joined by two different predicates).
//!
//! **No axiom, no grounds.** Each of the four kinds of check is decided by one boolean in the
//! ontology; not declared means not checked -- reporting no contradiction is safer than guessing
//! an axiom into existence. So a knowledge base with no ontology pack installed comes out at
//! zero, and that is the truth, not a malfunction.

pub mod derive;
pub mod ontology;

use std::collections::{HashMap, HashSet};
use uuid::Uuid;

/// One fact taking part in a check: who, what relation, pointing at whom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Edge {
    pub fact: Uuid,
    pub predicate: Uuid,
    pub subject: Uuid,
    pub object: Uuid,
}

/// Which axioms a predicate declares. A predicate with all four bits false never enters a check.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Axioms {
    pub transitive: bool,
    pub symmetric: bool,
    pub asymmetric: bool,
    pub irreflexive: bool,
    pub functional: bool,
    pub inverse_functional: bool,
    /// `p⁻¹ = q`: which predicate this one's inverse is. **This is where cross-predicate rules
    /// come from** -- `A p B` entails `B q A`, and the R0 checks look at it too (being its own
    /// inverse is the same as symmetric)
    pub inverse_of: Option<Uuid>,
    /// `p ⊑ q`: assert the specific one and the general one holds too. The chain has to be kept
    /// from closing into a ring, which R0 checks
    pub sub_property_of: Option<Uuid>,
}

impl Axioms {
    /// A predicate that declares not one bit needs no checking -- **that is a performance matter
    /// and a semantic one**: no axiom means no criterion, and scanning it is wasted work.
    fn says_nothing(&self) -> bool {
        *self == Axioms::default()
    }
}

/// One contradiction found. Persisted in `axiom_violations` -- **not in `fact_conflicts`**:
/// that table asks "which one is right", whereas an axiom violation asks "is the data wrong or
/// is the definition wrong", and the way out of the latter may be to go change the ontology.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub kind: Kind,
    /// The facts involved. **An irreflexivity violation has only one** -- it contradicts itself,
    /// no second fact needed. A cycle takes the first and the last (the ones in between are in
    /// `path`).
    pub left: Uuid,
    pub right: Uuid,
    /// The cycle's full path, as a list of facts; empty for the other three kinds.
    /// Kept because "A→B→C→A" is far more useful than "A contradicts C" -- a human has to walk
    /// it once to know which link to retract
    pub path: Vec<Uuid>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// `A p A`, and p declares irreflexive
    SelfLoop,
    /// `A p B` and `B p A`, and p declares asymmetric
    Asymmetry,
    /// `A p B p … p A`, and p declares transitive -- the closure entails `A p A`
    Cycle,
    /// The same subject and predicate pointing at two objects at once, and p declares functional
    /// (inverse_functional is the same object being pointed at by two subjects)
    Functional,
    /// `A p B`, but A is not in p's declared domain, or B is not in its range (#190 / #196).
    ///
    /// **This kind is not computed by this crate**: it needs the entities' types and the closure
    /// of domain / range, which live in the database, and
    /// `utopia-store::reasoning::signature_breaks` measures it in SQL. It is listed here so it
    /// travels the same persist / clear-stale / adjudicate path as the other four kinds -- left
    /// and right are the same fact, the same way as the irreflexive kind
    Signature,
    /// A derivation ran into an assertion (0017): the entailed `A p B` cannot coexist with some
    /// assertion in the ledger under p's axioms. The derivation is not persisted; this row puts
    /// it in front of a human. `left` is the assertion that was hit, `right` is the derivation's
    /// last premise, `path` is all the premises; the entailed triple itself sits in
    /// `axiom_violations.detail` -- it was never persisted, so there is no id to point at
    DerivedContradiction,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::SelfLoop => "self_loop",
            Kind::Asymmetry => "asymmetry",
            Kind::Cycle => "cycle",
            Kind::Functional => "functional",
            Kind::Signature => "signature",
            Kind::DerivedContradiction => "derived_contradiction",
        }
    }
}

/// Depth cap for cycle detection.
///
/// **Required, not defensive programming.** 0002 measured it on a real corpus: the transitive
/// closure of `part_of` swelled from 185 rows to 828 and **did not converge** -- the depth
/// distribution was `1:185 2:181 3:141 4:45 5:52 6:40 7:52 8:40 9:52 10:40`, oscillating rather
/// than decaying from level 5 onwards, which is the shape of a cycle. Without a cap, one cycle
/// is enough to stop evaluation from terminating.
pub const MAX_DEPTH: usize = 12;

/// Measure this batch of edges against the axioms.
///
/// Each predicate is checked on its own: axioms hang off predicates, and edges of different
/// predicates are not comparable (`A part_of B` and `B produces A` holding at once is not a
/// contradiction).
pub fn check(edges: &[Edge], axioms: &HashMap<Uuid, Axioms>) -> Vec<Violation> {
    let mut out = Vec::new();
    let mut by_pred: HashMap<Uuid, Vec<Edge>> = HashMap::new();
    for e in edges {
        let Some(ax) = axioms.get(&e.predicate) else {
            continue;
        };
        if ax.says_nothing() {
            continue;
        }
        by_pred.entry(e.predicate).or_default().push(*e);
    }
    for (pred, group) in by_pred {
        let ax = axioms[&pred];
        if ax.irreflexive {
            out.extend(self_loops(&group));
        }
        if ax.asymmetric {
            out.extend(asymmetries(&group));
        }
        if ax.transitive {
            out.extend(cycles(&group));
        }
        if ax.functional {
            out.extend(too_many(&group, |e| e.subject, |e| e.object));
        }
        if ax.inverse_functional {
            out.extend(too_many(&group, |e| e.object, |e| e.subject));
        }
    }
    out
}

fn self_loops(edges: &[Edge]) -> Vec<Violation> {
    edges
        .iter()
        .filter(|e| e.subject == e.object)
        .map(|e| Violation {
            kind: Kind::SelfLoop,
            // Both columns get the same fact: it contradicts itself, there is no second fact to
            // point at
            left: e.fact,
            right: e.fact,
            path: Vec::new(),
        })
        .collect()
}

fn asymmetries(edges: &[Edge]) -> Vec<Violation> {
    let mut seen: HashMap<(Uuid, Uuid), Uuid> = HashMap::new();
    let mut out = Vec::new();
    for e in edges {
        if e.subject == e.object {
            // Self-loops are the irreflexive check's job; reporting them here too is a duplicate
            continue;
        }
        if let Some(&other) = seen.get(&(e.object, e.subject)) {
            out.push(Violation {
                kind: Kind::Asymmetry,
                left: other,
                right: e.fact,
                path: Vec::new(),
            });
        }
        seen.insert((e.subject, e.object), e.fact);
    }
    out
}

/// Find cycles. **Each cycle is reported once**, counted from the smallest node on it.
///
/// Depth-first rather than semi-naive closure evaluation: both find cycles, but the closure only
/// tells you "A entailed A", whereas what a human wants is the **path** -- you have to walk
/// `A→B→C→A` once to know which link to retract. That is exactly what the closure throws away.
///
/// R1's materialised inference wants the closure itself; build it then. R0 wants "which edges
/// add up to a cycle".
fn cycles(edges: &[Edge]) -> Vec<Violation> {
    let mut adj: HashMap<Uuid, Vec<&Edge>> = HashMap::new();
    for e in edges {
        adj.entry(e.subject).or_default().push(e);
    }
    let mut reported: HashSet<Vec<Uuid>> = HashSet::new();
    let mut out = Vec::new();
    let nodes: Vec<Uuid> = adj.keys().copied().collect();
    for start in nodes {
        let mut path: Vec<&Edge> = Vec::new();
        let mut on_path: HashSet<Uuid> = HashSet::new();
        walk(
            start,
            start,
            &adj,
            &mut path,
            &mut on_path,
            &mut reported,
            &mut out,
        );
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn walk<'a>(
    start: Uuid,
    at: Uuid,
    adj: &HashMap<Uuid, Vec<&'a Edge>>,
    path: &mut Vec<&'a Edge>,
    on_path: &mut HashSet<Uuid>,
    reported: &mut HashSet<Vec<Uuid>>,
    out: &mut Vec<Violation>,
) {
    if path.len() >= MAX_DEPTH {
        return;
    }
    let Some(next) = adj.get(&at) else { return };
    for e in next {
        if e.object == start && !path.is_empty() {
            // Back at the start: a cycle. **Deduplicate on sorted fact ids** -- the same cycle
            // gets walked n times, once from each node on it, and reporting it n times just
            // makes a human look at the same thing n times
            let mut facts: Vec<Uuid> = path.iter().map(|x| x.fact).collect();
            facts.push(e.fact);
            let mut key = facts.clone();
            key.sort();
            if reported.insert(key) {
                out.push(Violation {
                    kind: Kind::Cycle,
                    left: facts[0],
                    right: *facts.last().unwrap(),
                    path: facts,
                });
            }
            continue;
        }
        if e.object == start || on_path.contains(&e.object) {
            continue;
        }
        on_path.insert(e.object);
        path.push(e);
        walk(start, e.object, adj, path, on_path, reported, out);
        path.pop();
        on_path.remove(&e.object);
    }
}

/// A functionality violation: the same "one end" pointing at two different "other ends".
///
/// `functional` and `inverse_functional` are two directions of the same judgement, so they share
/// this one function and the caller decides which end is the key.
fn too_many(edges: &[Edge], key: fn(&Edge) -> Uuid, val: fn(&Edge) -> Uuid) -> Vec<Violation> {
    let mut by_key: HashMap<Uuid, Vec<&Edge>> = HashMap::new();
    for e in edges {
        by_key.entry(key(e)).or_default().push(e);
    }
    let mut out = Vec::new();
    for (_, group) in by_key {
        // Only report the first pair. When one subject points at five objects, reporting ten
        // pairs (every combination) just says the same thing ten times -- what a human has to
        // deal with is "there is a conflict here", and one pair is enough to go looking
        let mut distinct: Vec<&Edge> = Vec::new();
        for e in group {
            if !distinct.iter().any(|d| val(d) == val(e)) {
                distinct.push(e);
            }
        }
        if distinct.len() > 1 {
            out.push(Violation {
                kind: Kind::Functional,
                left: distinct[0].fact,
                right: distinct[1].fact,
                path: Vec::new(),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shorthand for building edges. Nodes get stable uuids out of small integers, so you can
    /// tell who is who when reading the assertions.
    fn n(i: u8) -> Uuid {
        Uuid::from_bytes([i; 16])
    }
    fn f(i: u8) -> Uuid {
        // Kept in a different range from node ids: nodes use the low end, facts the high end.
        // `u8` only holds 0..=255, and the tests build a chain of 30 edges -- `200 + i` would
        // overflow
        Uuid::from_bytes([i; 16].map(|b| b ^ 0xF0))
    }
    fn e(fact: u8, s: u8, o: u8) -> Edge {
        Edge {
            fact: f(fact),
            predicate: n(99),
            subject: n(s),
            object: n(o),
        }
    }
    fn with(ax: Axioms) -> HashMap<Uuid, Axioms> {
        HashMap::from([(n(99), ax)])
    }
    fn kinds(v: &[Violation]) -> Vec<Kind> {
        let mut k: Vec<Kind> = v.iter().map(|x| x.kind).collect();
        k.sort_by_key(|x| x.as_str());
        k
    }

    /// **A predicate that declares no axioms is not checked at all.** This is the bedrock of the
    /// whole set of checks: no grounds, no contradiction reported.
    ///
    /// The converse holds too -- a knowledge base with no ontology pack installed comes out at
    /// zero, and that is the truth, not a malfunction.
    #[test]
    fn a_predicate_that_declares_nothing_is_never_checked() {
        let edges = [e(1, 1, 1), e(2, 1, 2), e(3, 2, 1)];
        assert!(check(&edges, &with(Axioms::default())).is_empty());
        // Same when the predicate is not even in `axioms` (no such row in the ontology)
        assert!(check(&edges, &HashMap::new()).is_empty());
    }

    #[test]
    fn a_self_loop_needs_irreflexive_to_be_a_problem() {
        let edges = [e(1, 1, 1)];
        assert!(check(
            &edges,
            &with(Axioms {
                transitive: true,
                ..Default::default()
            })
        )
        .is_empty());
        let v = check(
            &edges,
            &with(Axioms {
                irreflexive: true,
                ..Default::default()
            }),
        );
        assert_eq!(kinds(&v), vec![Kind::SelfLoop]);
        // Only one fact: both columns get the same id
        assert_eq!(v[0].left, v[0].right);
    }

    /// The asymmetry check **does not re-report self-loops**. `A p A` also satisfies the literal
    /// reading of "has an edge the other way", and without the guard one self-loop would be
    /// reported once in each check, so a human sees two things to deal with when there is one.
    #[test]
    fn a_self_loop_is_reported_once_not_twice() {
        let edges = [e(1, 1, 1)];
        let v = check(
            &edges,
            &with(Axioms {
                irreflexive: true,
                asymmetric: true,
                ..Default::default()
            }),
        );
        assert_eq!(kinds(&v), vec![Kind::SelfLoop]);
    }

    #[test]
    fn a_pair_pointing_both_ways_needs_asymmetric() {
        let edges = [e(1, 1, 2), e(2, 2, 1)];
        assert!(check(
            &edges,
            &with(Axioms {
                transitive: true,
                ..Default::default()
            })
        )
        .iter()
        .all(|v| v.kind != Kind::Asymmetry));
        let v = check(
            &edges,
            &with(Axioms {
                asymmetric: true,
                ..Default::default()
            }),
        );
        assert_eq!(kinds(&v), vec![Kind::Asymmetry]);
        assert_eq!((v[0].left, v[0].right), (f(1), f(2)));
    }

    /// A long cycle has to report the **path**, not merely "the two ends contradict" -- a human
    /// has to walk it once to know which link to retract.
    #[test]
    fn a_long_cycle_reports_the_whole_path() {
        let edges = [e(1, 1, 2), e(2, 2, 3), e(3, 3, 4), e(4, 4, 1)];
        let v = check(
            &edges,
            &with(Axioms {
                transitive: true,
                ..Default::default()
            }),
        );
        assert_eq!(v.len(), 1, "one cycle is reported only once");
        assert_eq!(v[0].path.len(), 4, "all four edges belong in the path");
    }

    /// **The same cycle gets walked n times, once from each node on it.** Deduplication is half
    /// the reason this function exists: reporting it four times just makes a human look at the
    /// same thing four times.
    #[test]
    fn one_cycle_is_one_finding_however_many_ways_in() {
        let edges = [e(1, 1, 2), e(2, 2, 3), e(3, 3, 1)];
        let v = check(
            &edges,
            &with(Axioms {
                transitive: true,
                ..Default::default()
            }),
        );
        assert_eq!(v.len(), 1);
    }

    /// Two unrelated cycles are reported once each.
    #[test]
    fn separate_cycles_stay_separate() {
        let edges = [e(1, 1, 2), e(2, 2, 1), e(3, 5, 6), e(4, 6, 5)];
        let v = check(
            &edges,
            &with(Axioms {
                transitive: true,
                ..Default::default()
            }),
        );
        assert_eq!(v.len(), 2);
    }

    /// **The depth cap is what holds off non-convergence.** 0002 measured the `part_of` closure
    /// on a real corpus still oscillating at depth 10; without a cap, one long chain plus one
    /// cycle is enough to stop evaluation from terminating.
    ///
    /// Here we build a chain longer than the cap and then close it -- it must not hang the
    /// check. Whether that cycle gets reported is secondary; **not hanging is primary**.
    #[test]
    fn a_chain_longer_than_the_limit_still_terminates() {
        let mut edges: Vec<Edge> = (0..30).map(|i| e(i, i, i + 1)).collect();
        edges.push(e(60, 30, 0));
        let v = check(
            &edges,
            &with(Axioms {
                transitive: true,
                ..Default::default()
            }),
        );
        // What is asserted is "it ran to completion"; the length is not required to be anything
        assert!(v.len() <= 1);
    }

    #[test]
    fn functional_and_its_inverse_are_two_directions_of_one_check() {
        // One subject pointing at two objects
        let out = [e(1, 1, 2), e(2, 1, 3)];
        assert_eq!(
            kinds(&check(
                &out,
                &with(Axioms {
                    functional: true,
                    ..Default::default()
                })
            )),
            vec![Kind::Functional]
        );
        assert!(check(
            &out,
            &with(Axioms {
                inverse_functional: true,
                ..Default::default()
            })
        )
        .is_empty());

        // One object pointed at by two subjects
        let inn = [e(1, 2, 1), e(2, 3, 1)];
        assert_eq!(
            kinds(&check(
                &inn,
                &with(Axioms {
                    inverse_functional: true,
                    ..Default::default()
                })
            )),
            vec![Kind::Functional]
        );
        assert!(check(
            &inn,
            &with(Axioms {
                functional: true,
                ..Default::default()
            })
        )
        .is_empty());
    }

    /// One subject pointing at five objects reports one pair. Reporting ten pairs (every
    /// combination) says the same thing ten times -- what a human has to deal with is "there is
    /// a conflict here", and one pair is enough to go looking.
    #[test]
    fn one_finding_per_conflicting_key_not_one_per_pair() {
        let edges = [e(1, 1, 2), e(2, 1, 3), e(3, 1, 4), e(4, 1, 5), e(5, 1, 6)];
        let v = check(
            &edges,
            &with(Axioms {
                functional: true,
                ..Default::default()
            }),
        );
        assert_eq!(v.len(), 1);
    }

    /// The same subject pointing twice at the **same** object is not a conflict -- just a
    /// repeated assertion.
    #[test]
    fn saying_the_same_thing_twice_is_not_a_contradiction() {
        let edges = [e(1, 1, 2), e(2, 1, 2)];
        assert!(check(
            &edges,
            &with(Axioms {
                functional: true,
                ..Default::default()
            })
        )
        .is_empty());
    }

    /// **Axioms hang off predicates, and edges of different predicates are not comparable.**
    /// `A p B` and `B q A` holding at once is not a contradiction, even if p declares asymmetric.
    #[test]
    fn axioms_do_not_leak_across_predicates() {
        let a = Edge {
            fact: f(1),
            predicate: n(90),
            subject: n(1),
            object: n(2),
        };
        let b = Edge {
            fact: f(2),
            predicate: n(91),
            subject: n(2),
            object: n(1),
        };
        let ax = HashMap::from([
            (
                n(90),
                Axioms {
                    asymmetric: true,
                    ..Default::default()
                },
            ),
            (
                n(91),
                Axioms {
                    asymmetric: true,
                    ..Default::default()
                },
            ),
        ]);
        assert!(check(&[a, b], &ax).is_empty());
    }
}
