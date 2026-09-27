//! The ontology's own self-consistency: **does not touch facts, only looks at definitions**.
//!
//! A different thing from [`crate::check`]. That layer asks "do the facts clash with the
//! definitions"; this one asks "do the definitions stand up by themselves". Separating them is
//! not a habit of taxonomy: a self-contradictory ontology makes every conclusion at the fact
//! layer suspect -- if some predicate declares both symmetric and asymmetric, then every
//! asymmetry violation reported on its authority rests on a premise that never held in the first
//! place. So this layer's conclusions belong **first** in front of a human.
//!
//! Cheapness is a reason too: the input is only a few thousand lines of ontology, with no ledger
//! to scan.
//!
//! **Same "not declared means not checked".** Every check here corresponds to something written
//! down in the ontology; not one of them is an assumption we make on the user's behalf.

use crate::Axioms;
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

/// Depth cap for climbing the class hierarchy. Same reason as [`crate::MAX_DEPTH`]: the
/// ontology can contain rings (the table definition only holds off self-loops, `A → B → A`
/// gets through), and without a cap this does not terminate.
pub const MAX_ANCESTRY: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Defect {
    /// The same predicate declares both symmetric and asymmetric.
    ///
    /// In OWL the two can only hold together for the **empty** property -- the moment there is
    /// an edge `A p B`, symmetry says `B p A` must hold and asymmetry says it must not. So as
    /// soon as this predicate has any facts, one of the two declarations is certainly wrong.
    SymmetricAndAsymmetric,
    /// Transitive + functional. OWL 2 DL forbids it outright (a functional property may not be
    /// declared transitive), because the two together push inference out of the decidable
    /// fragment.
    ///
    /// It makes intuitive sense too: functionality says "only one value on the subject side",
    /// transitivity says "keep entailing along the chain", and the second hop on the chain
    /// entails a second value for that same subject.
    TransitiveAndFunctional,
    /// subClassOf closed into a ring. The CHECK on the table only holds off `A → A`.
    ///
    /// A ring means every class on it is a subclass of every other, i.e. they are really one
    /// class -- yet each has its own label, description and properties, and each draws its own
    /// row in the UI.
    SubclassCycle,
    /// A class declares itself disjoint with its own ancestor → the class **can never have an
    /// instance**. It inherits the ancestor's identity and declares itself disjoint from it.
    DisjointWithAncestor,
    /// Two of a class's ancestors are disjoint with each other → same as above, unsatisfiable.
    /// Under multiple inheritance this shape is not rare: each branch is reasonable on its own,
    /// and put together they contradict.
    InheritsDisjoint,
    /// A predicate declares itself its own inverse. **Equivalent to symmetric** -- inference
    /// runs either way, it only costs the reader an extra step of thought. We suggest rewriting
    /// it as `symmetric`, which says it more plainly
    InverseOfItself,
    /// `p⁻¹ = q` while `q⁻¹ = r` -- the two sides point at different things.
    ///
    /// Loading the axioms only fills gaps and never overwrites what a human wrote (**what a
    /// human wrote wins over what was inferred**), so this contradiction is not quietly smoothed
    /// over; it is left for here to report.
    InverseNotMutual,
    /// subPropertyOf closed into a ring. Same shape as [`Defect::SubclassCycle`]: every
    /// predicate on the ring is a sub-property of every other = they are really one predicate,
    /// and each has its own label and its own facts.
    SubPropertyCycle,
}

impl Defect {
    pub fn as_str(self) -> &'static str {
        match self {
            Defect::SymmetricAndAsymmetric => "symmetric_and_asymmetric",
            Defect::TransitiveAndFunctional => "transitive_and_functional",
            Defect::SubclassCycle => "subclass_cycle",
            Defect::DisjointWithAncestor => "disjoint_with_ancestor",
            Defect::InheritsDisjoint => "inherits_disjoint",
            Defect::InverseOfItself => "inverse_of_itself",
            Defect::InverseNotMutual => "inverse_not_mutual",
            Defect::SubPropertyCycle => "sub_property_cycle",
        }
    }
}

/// One self-contradiction in the ontology.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OntologyDefect {
    pub kind: Defect,
    /// The object at fault: a predicate (the first two kinds) or a class (the last three)
    pub subject: Uuid,
    /// The other side: the class it is disjoint with. The first two kinds and the rings have no
    /// second side
    pub other: Option<Uuid>,
    /// The ring's path (as classes), or the path from the class up to that ancestor. Empty
    /// otherwise
    pub path: Vec<Uuid>,
}

/// Measure the ontology against itself.
///
/// `parents` are `(child, parent)` pairs, `disjoint` are disjointness pairs -- **the import side
/// has already expanded the symmetry into two rows**, so both directions show up here and
/// deduplication relies on an ordered key.
pub fn check_ontology(
    axioms: &HashMap<Uuid, Axioms>,
    parents: &[(Uuid, Uuid)],
    disjoint: &[(Uuid, Uuid)],
) -> Vec<OntologyDefect> {
    let mut out = Vec::new();

    // ---- The two self-contradictions on predicates. **Sort before reporting**: a HashMap
    // iterates in a different order every time, and two checks of the same ontology ought to
    // come out identical
    let mut preds: Vec<(&Uuid, &Axioms)> = axioms.iter().collect();
    preds.sort_by_key(|(id, _)| **id);
    for (&pred, ax) in preds {
        if ax.symmetric && ax.asymmetric {
            out.push(OntologyDefect {
                kind: Defect::SymmetricAndAsymmetric,
                subject: pred,
                other: None,
                path: Vec::new(),
            });
        }
        if ax.transitive && (ax.functional || ax.inverse_functional) {
            out.push(OntologyDefect {
                kind: Defect::TransitiveAndFunctional,
                subject: pred,
                other: None,
                path: Vec::new(),
            });
        }
        // Its own inverse = symmetric. **Not wrong, just the long way round** -- inference runs
        // either way, but whoever reads the ontology has to work that step out for themselves.
        // We suggest `symmetric`, which says it more plainly
        if ax.inverse_of == Some(pred) {
            out.push(OntologyDefect {
                kind: Defect::InverseOfItself,
                subject: pred,
                other: None,
                path: Vec::new(),
            });
        }
        // Inverse + asymmetric: `A p B` entails `B p A` (its own inverse), and asymmetry says
        // that does not hold. The same contradiction as symmetric+asymmetric, arriving in a
        // different spelling
        if ax.inverse_of == Some(pred) && ax.asymmetric {
            out.push(OntologyDefect {
                kind: Defect::SymmetricAndAsymmetric,
                subject: pred,
                other: None,
                path: Vec::new(),
            });
        }
        // Both sides declare an inverse, but they point at different predicates. **We do not
        // quietly make them agree at load time** (`reasoning::axioms` over there only fills gaps
        // and never overwrites what a human wrote), so it is reported here
        if let Some(inv) = ax.inverse_of {
            if let Some(back) = axioms.get(&inv).and_then(|a| a.inverse_of) {
                if back != pred {
                    out.push(OntologyDefect {
                        kind: Defect::InverseNotMutual,
                        subject: pred,
                        other: Some(inv),
                        path: vec![pred, inv, back],
                    });
                }
            }
        }
    }

    // ---- subPropertyOf closing into a ring. The same shape as a subClassOf ring: every
    // predicate on the ring is a sub-property of every other = they are really one predicate,
    // and each has its own label and its own facts
    {
        let parent_of: HashMap<Uuid, Uuid> = axioms
            .iter()
            .filter_map(|(id, ax)| ax.sub_property_of.map(|p| (*id, p)))
            .collect();
        let mut starts: Vec<Uuid> = parent_of.keys().copied().collect();
        starts.sort();
        let mut reported: HashSet<Uuid> = HashSet::new();
        for start in starts {
            if reported.contains(&start) {
                continue;
            }
            let mut seen: Vec<Uuid> = Vec::new();
            let mut cur = start;
            for _ in 0..MAX_ANCESTRY {
                if seen.contains(&cur) {
                    // Every member of the ring gets marked, so the whole ring is reported once
                    for m in &seen {
                        reported.insert(*m);
                    }
                    out.push(OntologyDefect {
                        kind: Defect::SubPropertyCycle,
                        subject: start,
                        other: None,
                        path: seen.clone(),
                    });
                    break;
                }
                seen.push(cur);
                match parent_of.get(&cur) {
                    Some(&p) => cur = p,
                    None => break,
                }
            }
        }
    }

    let up = adjacency(parents);
    out.extend(subclass_cycles(&up));
    out.extend(unsatisfiable(&up, disjoint));
    out
}

/// Child → parent adjacency. The same pair declared twice is kept once.
fn adjacency(parents: &[(Uuid, Uuid)]) -> HashMap<Uuid, Vec<Uuid>> {
    let mut up: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
    for &(child, parent) in parents {
        let slot = up.entry(child).or_default();
        if !slot.contains(&parent) {
            slot.push(parent);
        }
    }
    // Sorting guarantees the same ontology works out the same path every time
    for v in up.values_mut() {
        v.sort();
    }
    up
}

/// subClassOf rings. Each ring is reported once, keyed on the ordered set of classes on it.
fn subclass_cycles(up: &HashMap<Uuid, Vec<Uuid>>) -> Vec<OntologyDefect> {
    let mut reported: HashSet<Vec<Uuid>> = HashSet::new();
    let mut out = Vec::new();
    let mut starts: Vec<Uuid> = up.keys().copied().collect();
    starts.sort();
    for start in starts {
        let mut path = Vec::new();
        let mut on_path = HashSet::new();
        climb(
            start,
            start,
            up,
            &mut path,
            &mut on_path,
            &mut reported,
            &mut out,
        );
    }
    out
}

fn climb(
    start: Uuid,
    at: Uuid,
    up: &HashMap<Uuid, Vec<Uuid>>,
    path: &mut Vec<Uuid>,
    on_path: &mut HashSet<Uuid>,
    reported: &mut HashSet<Vec<Uuid>>,
    out: &mut Vec<OntologyDefect>,
) {
    if path.len() >= MAX_ANCESTRY {
        return;
    }
    let Some(ups) = up.get(&at) else {
        return;
    };
    for &parent in ups {
        if parent == start && !path.is_empty() {
            // Back at the start: this is a ring. `path` right now is the classes after start
            let mut ring = vec![start];
            ring.extend(path.iter().copied());
            let mut key = ring.clone();
            key.sort();
            key.dedup();
            if reported.insert(key) {
                out.push(OntologyDefect {
                    kind: Defect::SubclassCycle,
                    subject: start,
                    other: None,
                    path: ring,
                });
            }
            continue;
        }
        if on_path.contains(&parent) || parent == start {
            // A ring elsewhere; it gets reported on its own round -- here just don't walk in
            continue;
        }
        path.push(parent);
        on_path.insert(parent);
        climb(start, parent, up, path, on_path, reported, out);
        on_path.remove(&parent);
        path.pop();
    }
}

/// Unsatisfiable classes: a disjoint pair turns up inside the ancestor set.
///
/// The two shapes are reported separately because **what you tell a human differs**: disjoint
/// with your own ancestor means "this disjoint declaration was written backwards", whereas two
/// disjoint ancestors means "this class should not hang under both branches at once".
fn unsatisfiable(up: &HashMap<Uuid, Vec<Uuid>>, disjoint: &[(Uuid, Uuid)]) -> Vec<OntologyDefect> {
    let pairs: HashSet<(Uuid, Uuid)> = disjoint.iter().copied().collect();
    if pairs.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut classes: Vec<Uuid> = up.keys().copied().collect();
    classes.sort();
    for class in classes {
        let anc = ancestors(class, up);
        // One: disjoint with its own ancestor
        for &a in &anc {
            if pairs.contains(&(class, a)) {
                out.push(OntologyDefect {
                    kind: Defect::DisjointWithAncestor,
                    subject: class,
                    other: Some(a),
                    path: Vec::new(),
                });
            }
        }
        // Two: two ancestors disjoint with each other. The ordered pair deduplicates, otherwise
        // a disjoint expanded into two rows gets reported twice
        let mut sorted: Vec<Uuid> = anc.iter().copied().collect();
        sorted.sort();
        for (i, &a) in sorted.iter().enumerate() {
            for &b in &sorted[i + 1..] {
                if pairs.contains(&(a, b)) {
                    out.push(OntologyDefect {
                        kind: Defect::InheritsDisjoint,
                        subject: class,
                        other: Some(b),
                        path: vec![a],
                    });
                }
            }
        }
    }
    out
}

/// All of a class's ancestors (not itself). A ring cannot trap it -- `seen` holds it off.
fn ancestors(class: Uuid, up: &HashMap<Uuid, Vec<Uuid>>) -> HashSet<Uuid> {
    let mut seen = HashSet::new();
    let mut queue = vec![(class, 0usize)];
    while let Some((at, depth)) = queue.pop() {
        if depth >= MAX_ANCESTRY {
            continue;
        }
        let Some(ups) = up.get(&at) else {
            continue;
        };
        for &parent in ups {
            if parent != class && seen.insert(parent) {
                queue.push((parent, depth + 1));
            }
        }
    }
    seen
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(i: u8) -> Uuid {
        Uuid::from_bytes([i; 16])
    }
    fn kinds(v: &[OntologyDefect]) -> Vec<Defect> {
        let mut k: Vec<Defect> = v.iter().map(|d| d.kind).collect();
        k.sort();
        k
    }
    fn ax(f: impl Fn(&mut Axioms)) -> HashMap<Uuid, Axioms> {
        let mut a = Axioms::default();
        f(&mut a);
        HashMap::from([(c(99), a)])
    }

    #[test]
    fn a_property_cannot_be_both_symmetric_and_asymmetric() {
        let a = ax(|a| {
            a.symmetric = true;
            a.asymmetric = true;
        });
        let d = check_ontology(&a, &[], &[]);
        assert_eq!(kinds(&d), vec![Defect::SymmetricAndAsymmetric]);
        assert_eq!(d[0].subject, c(99));
    }

    #[test]
    fn either_one_alone_is_fine() {
        assert!(check_ontology(&ax(|a| a.symmetric = true), &[], &[]).is_empty());
        assert!(check_ontology(&ax(|a| a.asymmetric = true), &[], &[]).is_empty());
        // Transitive + asymmetric is **normal** -- it is precisely the premise that makes cycle
        // detection meaningful
        let both = ax(|a| {
            a.transitive = true;
            a.asymmetric = true;
        });
        assert!(check_ontology(&both, &[], &[]).is_empty());
    }

    #[test]
    fn transitive_and_functional_is_forbidden_by_owl2() {
        let a = ax(|a| {
            a.transitive = true;
            a.functional = true;
        });
        assert_eq!(
            kinds(&check_ontology(&a, &[], &[])),
            vec![Defect::TransitiveAndFunctional]
        );
        // Same for inverse functionality
        let b = ax(|a| {
            a.transitive = true;
            a.inverse_functional = true;
        });
        assert_eq!(
            kinds(&check_ontology(&b, &[], &[])),
            vec![Defect::TransitiveAndFunctional]
        );
    }

    #[test]
    fn subclass_of_can_form_a_ring() {
        let none = HashMap::new();
        // 1 → 2 → 3 → 1
        let ring = [(c(1), c(2)), (c(2), c(3)), (c(3), c(1))];
        let d = check_ontology(&none, &ring, &[]);
        assert_eq!(
            kinds(&d),
            vec![Defect::SubclassCycle],
            "three classes forming one ring"
        );
        assert_eq!(d.len(), 1, "one ring is reported once, not once per class");
        assert_eq!(
            d[0].path.len(),
            3,
            "the path carries all three classes on the ring"
        );
    }

    #[test]
    fn a_tree_is_not_a_ring() {
        let none = HashMap::new();
        // Multiple parents is not a ring either: 4 hangs under both 2 and 3
        let tree = [(c(1), c(2)), (c(1), c(3)), (c(4), c(2)), (c(4), c(3))];
        assert!(check_ontology(&none, &tree, &[]).is_empty());
    }

    #[test]
    fn a_class_disjoint_with_its_own_ancestor_can_never_exist() {
        let none = HashMap::new();
        let parents = [(c(1), c(2)), (c(2), c(3))];
        // The import side expands disjoint symmetry into two rows, so two rows are given here
        let dis = [(c(1), c(3)), (c(3), c(1))];
        let d = check_ontology(&none, &parents, &dis);
        assert_eq!(kinds(&d), vec![Defect::DisjointWithAncestor]);
        assert_eq!(d[0].subject, c(1));
        assert_eq!(
            d[0].other,
            Some(c(3)),
            "says which ancestor it is disjoint with"
        );
    }

    #[test]
    fn two_disjoint_ancestors_make_a_class_unsatisfiable() {
        let none = HashMap::new();
        // 1 is a subclass of both 2 and 3, and 2 and 3 are disjoint
        let parents = [(c(1), c(2)), (c(1), c(3))];
        let dis = [(c(2), c(3)), (c(3), c(2))];
        let d = check_ontology(&none, &parents, &dis);
        assert_eq!(kinds(&d), vec![Defect::InheritsDisjoint]);
        assert_eq!(
            d.len(),
            1,
            "a disjoint expanded to two rows must not be reported twice"
        );
        assert_eq!(d[0].subject, c(1));
    }

    #[test]
    fn disjoint_between_unrelated_branches_is_the_point_of_disjoint() {
        let none = HashMap::new();
        // 2 and 3 are disjoint, 1 hangs only under 2 and 4 only under 3 -- this is exactly the
        // normal use of disjoint
        let parents = [(c(1), c(2)), (c(4), c(3))];
        let dis = [(c(2), c(3)), (c(3), c(2))];
        assert!(check_ontology(&none, &parents, &dis).is_empty());
    }

    #[test]
    fn a_ring_does_not_hang_the_ancestor_walk() {
        let none = HashMap::new();
        let ring = [(c(1), c(2)), (c(2), c(1))];
        let dis = [(c(1), c(2)), (c(2), c(1))];
        // Ring and disjointness at once: the ring has to be reported, and the climb upwards
        // must not get stuck going round
        let d = check_ontology(&none, &ring, &dis);
        assert!(d.iter().any(|x| x.kind == Defect::SubclassCycle));
        assert!(d.iter().any(|x| x.kind == Defect::DisjointWithAncestor));
    }

    #[test]
    fn nothing_declared_means_nothing_reported() {
        assert!(check_ontology(&HashMap::new(), &[], &[]).is_empty());
        // A full class hierarchy but not one disjoint → no grounds to judge on
        let parents = [(c(1), c(2)), (c(2), c(3))];
        assert!(check_ontology(&HashMap::new(), &parents, &[]).is_empty());
    }
}

#[cfg(test)]
mod inverse_and_sub_property_tests {
    use super::*;

    fn c(i: u8) -> Uuid {
        Uuid::from_bytes([i; 16])
    }
    fn kinds(v: &[OntologyDefect]) -> Vec<Defect> {
        let mut k: Vec<Defect> = v.iter().map(|d| d.kind).collect();
        k.sort();
        k
    }
    fn only(id: Uuid, a: Axioms) -> HashMap<Uuid, Axioms> {
        HashMap::from([(id, a)])
    }

    /// Its own inverse -- legal but the long way round; we suggest symmetric instead.
    #[test]
    fn a_predicate_that_is_its_own_inverse_should_just_say_symmetric() {
        let a = Axioms {
            inverse_of: Some(c(1)),
            ..Default::default()
        };
        let d = check_ontology(&only(c(1), a), &[], &[]);
        assert_eq!(kinds(&d), vec![Defect::InverseOfItself]);
    }

    /// Its own inverse + asymmetric = the same contradiction as symmetric+asymmetric, in a
    /// different spelling.
    #[test]
    fn its_own_inverse_and_asymmetric_is_the_same_contradiction_in_disguise() {
        let a = Axioms {
            inverse_of: Some(c(1)),
            asymmetric: true,
            ..Default::default()
        };
        let d = check_ontology(&only(c(1), a), &[], &[]);
        assert!(
            kinds(&d).contains(&Defect::SymmetricAndAsymmetric),
            "**report it as the same kind**: a different spelling should not make the reader think these are two different things"
        );
    }

    /// A mutual pair -- clean, nothing should be reported.
    #[test]
    fn a_mutual_pair_is_clean() {
        let ax = HashMap::from([
            (
                c(1),
                Axioms {
                    inverse_of: Some(c(2)),
                    ..Default::default()
                },
            ),
            (
                c(2),
                Axioms {
                    inverse_of: Some(c(1)),
                    ..Default::default()
                },
            ),
        ]);
        assert!(check_ontology(&ax, &[], &[]).is_empty());
    }

    /// `p⁻¹ = q` while `q⁻¹ = r` -- the two sides point at different things.
    ///
    /// **Loading the axioms only fills gaps, it does not overwrite**, so this contradiction is
    /// not quietly smoothed over; it has to be reported here, or nowhere will mention it.
    #[test]
    fn an_inverse_that_does_not_point_back_is_reported() {
        let ax = HashMap::from([
            (
                c(1),
                Axioms {
                    inverse_of: Some(c(2)),
                    ..Default::default()
                },
            ),
            (
                c(2),
                Axioms {
                    inverse_of: Some(c(3)),
                    ..Default::default()
                },
            ),
        ]);
        let d = check_ontology(&ax, &[], &[]);
        let one = d
            .iter()
            .find(|x| x.kind == Defect::InverseNotMutual)
            .expect("should report InverseNotMutual");
        assert_eq!(one.subject, c(1));
        assert_eq!(one.other, Some(c(2)));
        assert_eq!(
            one.path,
            vec![c(1), c(2), c(3)],
            "the path must spell out where it points"
        );
    }

    /// subPropertyOf closing into a ring.
    #[test]
    fn a_sub_property_ring_is_a_single_predicate_wearing_three_hats() {
        let mk = |parent: u8| Axioms {
            sub_property_of: Some(c(parent)),
            ..Default::default()
        };
        let ax = HashMap::from([(c(1), mk(2)), (c(2), mk(3)), (c(3), mk(1))]);
        let d = check_ontology(&ax, &[], &[]);
        let ring: Vec<&OntologyDefect> = d
            .iter()
            .filter(|x| x.kind == Defect::SubPropertyCycle)
            .collect();
        assert_eq!(
            ring.len(),
            1,
            "**the whole ring is reported once**, not once per member"
        );
        assert_eq!(ring[0].path.len(), 3);
    }

    /// A chain that does not close into a ring should not be reported by mistake.
    #[test]
    fn a_chain_that_ends_is_not_a_ring() {
        let ax = HashMap::from([
            (
                c(1),
                Axioms {
                    sub_property_of: Some(c(2)),
                    ..Default::default()
                },
            ),
            (
                c(2),
                Axioms {
                    sub_property_of: Some(c(3)),
                    ..Default::default()
                },
            ),
        ]);
        assert!(check_ontology(&ax, &[], &[]).is_empty());
    }
}
