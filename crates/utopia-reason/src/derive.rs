//! R1: materialised derivation. **This layer adds things to the graph**, so every one of
//! its constraints is necessary.
//!
//! The watershed against R0: R0 only points out problems and its risk surface is zero; R1
//! writes facts. ADR 0002 puts this step in a tier of its own and gives three hard rules,
//! each of which lands in the code below.
//!
//! **One: rules are compiled from ontology axioms only.** No user-defined DSL -- that is a
//! different product. All that can be compiled today is `TransitiveProperty` and
//! `SymmetricProperty`: `inverseOf` and `subPropertyOf` are not stored on the projection
//! side yet, so they are not here either. **One rule short is not a defect, it is the same
//! "no declaration, no inference"**.
//!
//! **Two: assertions beat derivations, hard.** A triple that has already been asserted is
//! not derived a second time -- not to save rows, but so that "who said this one" has a
//! single answer.
//!
//! **Three: a depth cap plus cycle detection, required by measurement.** The ADR measured it
//! on a real corpus: the transitive closure of `part_of` swelled from 185 rows to 828 and
//! **did not converge**, the depth distribution oscillating rather than decaying from level 5
//! onwards -- the shape of a cycle. So this code neither derives self-loops (`A → A` is a
//! contradiction, not knowledge; R0 reports it) nor leaves the round count unbounded.
//!
//! And one more thing the ADR lists under open questions that has to be answered here:
//! **validity time is the intersection**. Premise A `[2020,2023)`, premise B `[2022,∞)` →
//! derivation `[2022,2023)`. An empty intersection derives nothing -- when the two spans do
//! not overlap, the chain itself does not hold at any point in time.

use crate::{Axioms, Edge, Kind, MAX_DEPTH};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

/// Upper bound on how many derivations a single predicate may produce.
///
/// **Not defensive programming, this was measured**: the 4.5x blow-up showed up on a
/// predicate with 185 edges, and the blow-up is superlinear. Once the cap bites, how many
/// rows were cut off has to be **said out loud** (see [`Derivation::capped`]) -- truncating
/// silently makes "finished inferring" and "inferred part of it" look exactly alike.
pub const MAX_DERIVED_PER_PREDICATE: usize = 20_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Rule {
    /// `A p B` ∧ `B p C` ⟹ `A p C`
    Transitive,
    /// `A p B` ⟹ `B p A`
    Symmetric,
    /// `A p B` ∧ `p⁻¹ = q` ⟹ `B q A`. **The ends swap and the predicate changes** --
    /// both happen at once, and doing only one of them is the easiest way to get this rule
    /// wrong
    Inverse,
    /// `A p B` ∧ `p ⊑ q` ⟹ `A q B`. The ends stay put; only the predicate is lifted
    SubProperty,
}

impl Rule {
    pub fn as_str(self) -> &'static str {
        match self {
            Rule::Transitive => "transitive",
            Rule::Symmetric => "symmetric",
            Rule::Inverse => "inverse",
            Rule::SubProperty => "sub_property",
        }
    }
}

/// One derived fact to be written down, together with its proof.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Derived {
    pub predicate: Uuid,
    /// **Which predicate's declaration triggered it.**
    ///
    /// Transitivity and symmetry do not change the predicate, so `via == predicate`; whereas
    /// `inverseOf` and `subPropertyOf` do -- the fact derived from `ceo_of ⊑ works_at` has
    /// `works_at` as its predicate, while the declaration sits on `ceo_of`.
    ///
    /// The write looks the rule row up by `via`. **We tripped over this once**: it used to
    /// look up by `predicate`, which stayed right for the first two rules (the two are the
    /// same), and once the two cross-predicate rules were added the lookup found no rule, so
    /// `continue` dropped it silently -- derived but never written, the hardest kind to find.
    pub via: Uuid,
    pub subject: Uuid,
    pub object: Uuid,
    pub rule: Rule,
    /// The premises used, in derivation order. **This is one level of the proof tree** -- R2
    /// follows it when expanding an explanation, and when a premise is invalidated this is
    /// also how we know which derivations have to be invalidated with it
    pub premises: Vec<Uuid>,
}

/// The output of one derivation run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Derivation {
    pub facts: Vec<Derived>,
    /// Predicates that hit the cap and were not inferred to completion. **Must be returned
    /// to the caller**: the interface has to be able to say "this predicate is too dense,
    /// only twenty thousand rows were derived" rather than letting people think it finished
    pub capped: Vec<Uuid>,
}

/// One edge taking part in derivation, carrying a validity period on top of [`Edge`].
///
/// A type of its own rather than extra fields on `Edge`: R0 has no use for time at all --
/// whether an axiom is violated is unrelated to when it holds -- while R1 has to compute an
/// intersection at every step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimedEdge {
    pub edge: Edge,
    /// Half-open interval `[from, to)`. Either end may be empty = unknown / always
    pub from: Option<i64>,
    pub to: Option<i64>,
}

/// Intersection. `None` stands for the unbounded side.
fn overlap(
    a: (Option<i64>, Option<i64>),
    b: (Option<i64>, Option<i64>),
) -> Option<(Option<i64>, Option<i64>)> {
    let from = match (a.0, b.0) {
        (Some(x), Some(y)) => Some(x.max(y)),
        (Some(x), None) | (None, Some(x)) => Some(x),
        (None, None) => None,
    };
    let to = match (a.1, b.1) {
        (Some(x), Some(y)) => Some(x.min(y)),
        (Some(x), None) | (None, Some(x)) => Some(x),
        (None, None) => None,
    };
    // An empty intersection derives nothing. When the two spans do not overlap this chain
    // does not hold at any point in time -- what comes out is a fact that is never true,
    // which is worse than deriving nothing
    if let (Some(f), Some(t)) = (from, to) {
        if f >= t {
            return None;
        }
    }
    Some((from, to))
}

/// One edge leaving some subject: (object, from, to, fact id).
type Hop = (Uuid, Option<i64>, Option<i64>, Uuid);

/// The intermediate state of a derivation: how a (subject, object) pair came about.
#[derive(Clone)]
struct Reached {
    from: Option<i64>,
    to: Option<i64>,
    premises: Vec<Uuid>,
}

/// The identity of a triple: (predicate, subject, object).
///
/// **The predicate is part of the key, and that is the most important change in this
/// version.** Derivation used to be grouped by predicate, each group self-contained, because
/// neither transitivity nor symmetry changes the predicate; whereas `inverseOf` and
/// `subPropertyOf` cross predicates by nature -- `A works_at B` derives `B employs A`, which
/// lands on a different predicate. Group by predicate and these two rules have nowhere to
/// live.
type Triple = (Uuid, Uuid, Uuid);

/// Run the axioms over this batch of edges.
///
/// **A global fixpoint, no longer grouped by predicate.** The three rules chain together:
///
/// ```text
/// A ceo_of B  --(subPropertyOf)-->  A works_at B  --(inverseOf)-->  B employs A
/// ```
///
/// If every predicate is computed on its own, this chain breaks at the first step. So it is
/// now semi-naive evaluation over the whole set: each round takes the previous round's new
/// edges (the frontier) and infers again, stopping when nothing is added.
///
/// The three one-hop rules (symmetric / inverse / sub-property) sit in the same round as
/// transitivity, because they are each other's input -- an edge the inverse produces may let
/// some transitive chain join up, and vice versa.
pub fn derive(edges: &[TimedEdge], axioms: &HashMap<Uuid, Axioms>) -> Derivation {
    let mut out = Derivation::default();

    // Triples that have been asserted. **A derivation that runs into one gives way** --
    // asserted > derived is hard
    let asserted: HashSet<Triple> = edges
        .iter()
        .map(|e| (e.edge.predicate, e.edge.subject, e.edge.object))
        .collect();

    // Already derived → how it came about. Only the first proof is kept for any one triple:
    // when several paths derive the same thing, which one is shown makes no difference to
    // the user, while keeping them all makes the proof tree's size track the path count
    let mut reached: HashMap<Triple, Reached> = HashMap::new();
    // **The cap is still counted per predicate**: that constant's meaning has not changed
    // (at most twenty thousand derivations per predicate), and `Derivation::capped` returns
    // a list of predicates too. Switching to one global number now that rules cross
    // predicates would leave "which predicate is too dense" unanswerable in the interface
    let mut per_pred: HashMap<Uuid, usize> = HashMap::new();
    let mut capped: HashSet<Uuid> = HashSet::new();

    // The edges walkable from (predicate, subject), used by transitivity. Derived ones come
    // in too -- they are our assertions now, and a chain should not break because something
    // "came from somewhere else"
    let mut adj: HashMap<(Uuid, Uuid), Vec<Hop>> = HashMap::new();
    for e in edges {
        adj.entry((e.edge.predicate, e.edge.subject))
            .or_default()
            .push((e.edge.object, e.from, e.to, e.edge.fact));
    }

    let mut frontier: Vec<(Triple, Reached)> = edges
        .iter()
        .map(|e| {
            (
                (e.edge.predicate, e.edge.subject, e.edge.object),
                Reached {
                    from: e.from,
                    to: e.to,
                    premises: vec![e.edge.fact],
                },
            )
        })
        .collect();
    // The starting frontier's order follows the arguments, and the argument order is not
    // guaranteed -- sort once, so two derivation runs over the same knowledge base can give
    // the same result
    frontier.sort_by_key(|(t, _)| *t);

    // The round cap is a **backstop**; the real bound is the `premises.len() >= MAX_DEPTH`
    // below: the constant means "paths at most 12 long", and only counting premises matches
    // that. The round count only guards against pathological input
    for _ in 0..MAX_DEPTH {
        if frontier.is_empty() {
            break;
        }
        let mut next: Vec<(Triple, Reached)> = Vec::new();

        for (triple, acc) in frontier.drain(..) {
            let (pred, subj, obj) = triple;
            let Some(ax) = axioms.get(&pred) else {
                continue;
            };
            // One more hop would go over: this one is not extended any further, but it
            // has already been produced itself
            if acc.premises.len() >= MAX_DEPTH {
                continue;
            }

            // ---- The three one-hop rules: swap the ends (symmetric), change the
            // predicate (inverse / sub-property)
            let mut hops: Vec<(Triple, Rule)> = Vec::new();
            if ax.symmetric {
                hops.push(((pred, obj, subj), Rule::Symmetric));
            }
            if let Some(inv) = ax.inverse_of {
                // `A p B ⟹ B p⁻¹ A`. The ends swap **and** the predicate changes -- both
                // happen at once, and doing only one of them is the easiest way to get this
                // rule wrong
                hops.push(((inv, obj, subj), Rule::Inverse));
            }
            if let Some(sup) = ax.sub_property_of {
                // `A p B ∧ p ⊑ q ⟹ A q B`. The ends stay put; only the predicate is lifted
                hops.push(((sup, subj, obj), Rule::SubProperty));
            }
            for (t, rule) in hops {
                if emit(
                    t,
                    pred,
                    rule,
                    &acc,
                    acc.from,
                    acc.to,
                    None,
                    &asserted,
                    &mut reached,
                    &mut per_pred,
                    &mut capped,
                    &mut out,
                    &mut next,
                    &mut adj,
                ) {
                    continue;
                }
            }

            // ---- Transitivity: needs an outgoing edge on the same predicate to join onto
            if ax.transitive {
                let outs = adj.get(&(pred, obj)).cloned().unwrap_or_default();
                for (c, from, to, fact) in outs {
                    // **No self-loops.** `A p A` on a transitive + asymmetric predicate
                    // is a contradiction rather than knowledge, and R0 reports that cycle
                    // along with its path
                    if subj == c {
                        continue;
                    }
                    let Some((nf, nt)) = overlap((acc.from, acc.to), (from, to)) else {
                        continue;
                    };
                    emit(
                        (pred, subj, c),
                        pred,
                        Rule::Transitive,
                        &acc,
                        nf,
                        nt,
                        Some(fact),
                        &asserted,
                        &mut reached,
                        &mut per_pred,
                        &mut capped,
                        &mut out,
                        &mut next,
                        &mut adj,
                    );
                }
            }
        }
        next.sort_by_key(|(t, _)| *t);
        frontier = next;
    }

    let mut capped: Vec<Uuid> = capped.into_iter().collect();
    capped.sort();
    out.capped = capped;
    out
}

/// Record one derivation and wire it into the adjacency table for later transitivity.
/// Returns true = this predicate hit its cap.
///
/// The parameter list is ugly, but pulling it out was necessary: all four rules do exactly
/// the same thing when recording (check the assertions, check what is already derived,
/// compute the interval, record the proof, onto the frontier, into the adjacency table), and
/// in the previous version symmetry and transitivity each wrote it out separately, so the two
/// copies of the "skip condition" slowly grew apart.
#[allow(clippy::too_many_arguments)]
fn emit(
    t: Triple,
    // The declaration that triggered it. For all four rules this is "the predicate of the
    // edge currently being expanded"
    via: Uuid,
    rule: Rule,
    acc: &Reached,
    from: Option<i64>,
    to: Option<i64>,
    // The extra premise transitivity consumes; the one-hop rules have none
    extra_premise: Option<Uuid>,
    asserted: &HashSet<Triple>,
    reached: &mut HashMap<Triple, Reached>,
    per_pred: &mut HashMap<Uuid, usize>,
    capped: &mut HashSet<Uuid>,
    out: &mut Derivation,
    next: &mut Vec<(Triple, Reached)>,
    adj: &mut HashMap<(Uuid, Uuid), Vec<Hop>>,
) -> bool {
    let (pred, subj, obj) = t;
    // No self-loops, whatever the rule: `A p A` is a contradiction, not knowledge
    if subj == obj {
        return false;
    }
    // Assertions win; anything already derived is not derived again -- **this is what makes
    // mutually pointing inverses converge**: when `p⁻¹ = q` and `q⁻¹ = p`, the edge the
    // second round derives back is already in reached
    if asserted.contains(&t) || reached.contains_key(&t) {
        return false;
    }
    let n = per_pred.entry(pred).or_insert(0);
    if *n >= MAX_DERIVED_PER_PREDICATE {
        capped.insert(pred);
        return true;
    }
    *n += 1;

    let mut premises = acc.premises.clone();
    if let Some(p) = extra_premise {
        premises.push(p);
    }
    let r = Reached {
        from,
        to,
        premises: premises.clone(),
    };
    reached.insert(t, r.clone());
    // Derived edges can be joined onto by later transitivity too
    if let Some(&first) = premises.first() {
        adj.entry((pred, subj))
            .or_default()
            .push((obj, from, to, first));
    }
    out.facts.push(Derived {
        predicate: pred,
        via,
        subject: subj,
        object: obj,
        rule,
        premises,
    });
    next.push((t, r));
    false
}

/// The validity period of a derived fact, for the side that writes to the database.
///
/// Kept apart from [`derive`] because the intersection has already been computed during
/// derivation, while the [`Derived`] the caller gets carries only premises -- recomputing it
/// is less work than stuffing the interval into the result, and harder to get wrong: the
/// premises are just those few facts, and the intersection is a function of them.
pub fn validity(
    premises: &[Uuid],
    by_fact: &HashMap<Uuid, (Option<i64>, Option<i64>)>,
) -> Option<(Option<i64>, Option<i64>)> {
    let mut acc = (None, None);
    for p in premises {
        let span = *by_fact.get(p)?;
        acc = overlap(acc, span)?;
    }
    Some(acc)
}

// ================ Contradictions: what a derivation ran into (0017) ================

/// A derivation that ran into an assertion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clash {
    /// The index into `Derivation::facts`
    pub derived: usize,
    /// Which axiom it ran into: `Functional` (including inverse_functional), `Asymmetry`,
    /// `SelfLoop`
    pub axiom: Kind,
    /// The assertion that was hit. A self-loop has no counterpart, so the derivation's last
    /// premise is used
    pub against: Uuid,
}

/// Two rules that together produced derivations contradicting each other.
///
/// **Aggregated by rule pair, not reported pair by pair**: `ceo_of ⊑ works_at` plus
/// `works_at` functional, and every organisation with two ceos clashes once -- the root is
/// those two declarations, and queueing them pair by pair would only drown Review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleClash {
    /// (the predicate the declaration sits on, the kind of rule), the two sorted by
    /// (predicate, kind) so a ≤ b
    pub a: (Uuid, Rule),
    pub b: (Uuid, Rule),
    pub axiom: Kind,
    /// The clashing derivation pairs, as indices into `Derivation::facts`
    pub pairs: Vec<(usize, usize)>,
}

/// Half-open interval `[from, to)`, either end may be empty
type Span = (Option<i64>, Option<i64>);
/// (predicate, one end) → the edges at the other end: (other end, fact, interval).
/// functional gets one copy per direction
type ByEnd = HashMap<(Uuid, Uuid), Vec<(Uuid, Uuid, Span)>>;
/// The identity of a rule: the predicate the declaration sits on + the kind of rule
type RuleSide = (Uuid, Rule);
/// The clashing derivation pairs, grouped by (rule a, rule b, which axiom was hit)
type Grouped = HashMap<(RuleSide, RuleSide, Kind), Vec<(usize, usize)>>;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Contradictions {
    pub with_assertions: Vec<Clash>,
    pub between_derivations: Vec<RuleClash>,
}

impl Contradictions {
    /// The indices of derivations that must not be written: the ones that ran into an
    /// assertion, and the ones that ran into another derivation. **When writing to the graph,
    /// too few beats wrong** (0002)
    pub fn blocked(&self) -> HashSet<usize> {
        let mut out: HashSet<usize> = self.with_assertions.iter().map(|c| c.derived).collect();
        for rc in &self.between_derivations {
            for (i, j) in &rc.pairs {
                out.insert(*i);
                out.insert(*j);
            }
        }
        out
    }
}

/// Measure the derivations against the axioms: the ones clashing with an assertion are
/// listed one by one, the ones clashing with each other are aggregated by rule pair.
///
/// Only four kinds are checked -- `functional` (including inverse), `asymmetric`,
/// `irreflexive` -- because they are the only ones where a contradiction can be judged from
/// **two edges**; the transitive-cycle kind needs the closure, and since the derivations are
/// themselves part of the closure, R0 checking the assertions is enough. Both functional and
/// asymmetric require the **validity intervals to overlap**: Mira left and Devin took over,
/// so the two `ceo_of` intervals do not intersect -- that is a succession, not a
/// contradiction.
///
/// A derivation that runs into an assertion is **never written** (asserted > derived, hard);
/// this step turns "giving way" from something silent into something visible -- the row in
/// that table in 0002 that was written down and never done.
pub fn contradictions(
    derivation: &Derivation,
    edges: &[TimedEdge],
    axioms: &HashMap<Uuid, Axioms>,
    spans: &HashMap<Uuid, (Option<i64>, Option<i64>)>,
) -> Contradictions {
    // Three indices over the assertions: (predicate, subject) → object;
    // (predicate, object) → subject; (predicate, subject, object) → edge
    let mut by_ps: ByEnd = HashMap::new();
    let mut by_po: ByEnd = HashMap::new();
    let mut by_spo: HashMap<(Uuid, Uuid, Uuid), Vec<(Uuid, Span)>> = HashMap::new();
    for e in edges {
        let span = (e.from, e.to);
        let x = e.edge;
        by_ps
            .entry((x.predicate, x.subject))
            .or_default()
            .push((x.object, x.fact, span));
        by_po
            .entry((x.predicate, x.object))
            .or_default()
            .push((x.subject, x.fact, span));
        by_spo
            .entry((x.predicate, x.subject, x.object))
            .or_default()
            .push((x.fact, span));
    }

    let mut out = Contradictions::default();
    // The derivations' intervals: computed with the same function as the write side; the
    // ones it cannot compute (premise intervals do not intersect) would never be written
    // anyway
    let derived_spans: Vec<Option<Span>> = derivation
        .facts
        .iter()
        .map(|d| validity(&d.premises, spans))
        .collect();

    for (i, d) in derivation.facts.iter().enumerate() {
        let Some(span) = derived_spans[i] else {
            continue;
        };
        let Some(ax) = axioms.get(&d.predicate) else {
            continue;
        };
        let Some(&last) = d.premises.last() else {
            continue;
        };
        if ax.irreflexive && d.subject == d.object {
            out.with_assertions.push(Clash {
                derived: i,
                axiom: Kind::SelfLoop,
                against: last,
            });
        }
        if ax.asymmetric {
            if let Some(v) = by_spo.get(&(d.predicate, d.object, d.subject)) {
                for (fact, sp) in v {
                    if overlap(span, *sp).is_some() {
                        out.with_assertions.push(Clash {
                            derived: i,
                            axiom: Kind::Asymmetry,
                            against: *fact,
                        });
                    }
                }
            }
        }
        if ax.functional {
            if let Some(v) = by_ps.get(&(d.predicate, d.subject)) {
                for (obj, fact, sp) in v {
                    if *obj != d.object && overlap(span, *sp).is_some() {
                        out.with_assertions.push(Clash {
                            derived: i,
                            axiom: Kind::Functional,
                            against: *fact,
                        });
                    }
                }
            }
        }
        if ax.inverse_functional {
            if let Some(v) = by_po.get(&(d.predicate, d.object)) {
                for (subj, fact, sp) in v {
                    if *subj != d.subject && overlap(span, *sp).is_some() {
                        out.with_assertions.push(Clash {
                            derived: i,
                            axiom: Kind::Functional,
                            against: *fact,
                        });
                    }
                }
            }
        }
    }

    // Between derivations: the same three indices, only keyed by index
    let mut d_ps: HashMap<(Uuid, Uuid), Vec<usize>> = HashMap::new();
    let mut d_po: HashMap<(Uuid, Uuid), Vec<usize>> = HashMap::new();
    let mut d_spo: HashMap<(Uuid, Uuid, Uuid), Vec<usize>> = HashMap::new();
    for (i, d) in derivation.facts.iter().enumerate() {
        if derived_spans[i].is_none() {
            continue;
        }
        d_ps.entry((d.predicate, d.subject)).or_default().push(i);
        d_po.entry((d.predicate, d.object)).or_default().push(i);
        d_spo
            .entry((d.predicate, d.subject, d.object))
            .or_default()
            .push(i);
    }
    let mut grouped: Grouped = HashMap::new();
    let mut note = |i: usize, j: usize, axiom: Kind| {
        let (i, j) = if i < j { (i, j) } else { (j, i) };
        let ri = (derivation.facts[i].via, derivation.facts[i].rule);
        let rj = (derivation.facts[j].via, derivation.facts[j].rule);
        let (a, b) = if (ri.0, ri.1.as_str()) <= (rj.0, rj.1.as_str()) {
            (ri, rj)
        } else {
            (rj, ri)
        };
        grouped.entry((a, b, axiom)).or_default().push((i, j));
    };
    for (i, d) in derivation.facts.iter().enumerate() {
        let Some(span) = derived_spans[i] else {
            continue;
        };
        let Some(ax) = axioms.get(&d.predicate) else {
            continue;
        };
        let overlapping = |j: usize| derived_spans[j].is_some_and(|s| overlap(span, s).is_some());
        if ax.asymmetric {
            if let Some(v) = d_spo.get(&(d.predicate, d.object, d.subject)) {
                for &j in v {
                    if j > i && overlapping(j) {
                        note(i, j, Kind::Asymmetry);
                    }
                }
            }
        }
        if ax.functional {
            if let Some(v) = d_ps.get(&(d.predicate, d.subject)) {
                for &j in v {
                    if j > i && derivation.facts[j].object != d.object && overlapping(j) {
                        note(i, j, Kind::Functional);
                    }
                }
            }
        }
        if ax.inverse_functional {
            if let Some(v) = d_po.get(&(d.predicate, d.object)) {
                for &j in v {
                    if j > i && derivation.facts[j].subject != d.subject && overlapping(j) {
                        note(i, j, Kind::Functional);
                    }
                }
            }
        }
    }
    let mut rule_clashes: Vec<RuleClash> = grouped
        .into_iter()
        .map(|((a, b, axiom), mut pairs)| {
            pairs.sort_unstable();
            pairs.dedup();
            RuleClash { a, b, axiom, pairs }
        })
        .collect();
    // The output is sorted -- half the value of this path is determinism
    rule_clashes.sort_by(|x, y| {
        (x.a.0, x.a.1.as_str(), x.b.0, x.b.1.as_str()).cmp(&(
            y.a.0,
            y.a.1.as_str(),
            y.b.0,
            y.b.1.as_str(),
        ))
    });
    out.between_derivations = rule_clashes;
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(i: u8) -> Uuid {
        Uuid::from_bytes([i; 16])
    }
    fn f(i: u8) -> Uuid {
        Uuid::from_bytes([i; 16].map(|b| b ^ 0xF0))
    }
    /// An edge with no time on it
    fn e(fact: u8, s: u8, o: u8) -> TimedEdge {
        TimedEdge {
            edge: Edge {
                fact: f(fact),
                predicate: n(99),
                subject: n(s),
                object: n(o),
            },
            from: None,
            to: None,
        }
    }
    /// An edge with an interval
    fn te(fact: u8, s: u8, o: u8, from: Option<i64>, to: Option<i64>) -> TimedEdge {
        TimedEdge {
            from,
            to,
            ..e(fact, s, o)
        }
    }
    fn with(ax: Axioms) -> HashMap<Uuid, Axioms> {
        HashMap::from([(n(99), ax)])
    }
    fn transitive() -> HashMap<Uuid, Axioms> {
        with(Axioms {
            transitive: true,
            ..Default::default()
        })
    }
    fn pairs(d: &Derivation) -> Vec<(u8, u8)> {
        let mut p: Vec<(u8, u8)> = d
            .facts
            .iter()
            .map(|x| (x.subject.as_bytes()[0], x.object.as_bytes()[0]))
            .collect();
        p.sort();
        p
    }

    #[test]
    fn nothing_declared_derives_nothing() {
        let edges = [e(1, 1, 2), e(2, 2, 3)];
        assert!(derive(&edges, &HashMap::new()).facts.is_empty());
        // Declaring some other axiom makes no difference -- only transitive / symmetric
        // compile into rules
        let irr = with(Axioms {
            irreflexive: true,
            ..Default::default()
        });
        assert!(derive(&edges, &irr).facts.is_empty());
    }

    #[test]
    fn a_chain_closes() {
        // 1→2→3→4; transitivity should derive 1→3, 1→4 and 2→4
        let edges = [e(1, 1, 2), e(2, 2, 3), e(3, 3, 4)];
        let d = derive(&edges, &transitive());
        assert_eq!(pairs(&d), vec![(1, 3), (1, 4), (2, 4)]);
        assert!(d.capped.is_empty());
    }

    #[test]
    fn the_proof_is_the_premises_in_order() {
        let edges = [e(1, 1, 2), e(2, 2, 3), e(3, 3, 4)];
        let d = derive(&edges, &transitive());
        let long = d
            .facts
            .iter()
            .find(|x| x.subject == n(1) && x.object == n(4))
            .unwrap();
        assert_eq!(
            long.premises,
            vec![f(1), f(2), f(3)],
            "the proof must carry all three premises in derivation order"
        );
        assert_eq!(long.rule, Rule::Transitive);
    }

    #[test]
    fn asserted_beats_derived() {
        // 1→2 and 2→3 already derive 1→3, and 1→3 has been asserted as well → no
        // duplicate derivation
        let edges = [e(1, 1, 2), e(2, 2, 3), e(3, 1, 3)];
        let d = derive(&edges, &transitive());
        assert!(
            d.facts.is_empty(),
            "an asserted triple should not get a derived copy"
        );
    }

    #[test]
    fn a_ring_does_not_derive_self_loops_and_does_not_hang() {
        // 1→2→3→1: a cycle. The transitive closure would derive 1→1, and that is a
        // contradiction, not knowledge
        let edges = [e(1, 1, 2), e(2, 2, 3), e(3, 3, 1)];
        let d = derive(&edges, &transitive());
        assert!(
            d.facts.iter().all(|x| x.subject != x.object),
            "self-loops should not be derived -- R0 reports that cycle along with its path"
        );
        // But the rest of the derivations around the cycle do hold: 1→3, 2→1, 3→2
        assert_eq!(pairs(&d), vec![(1, 3), (2, 1), (3, 2)]);
    }

    #[test]
    fn symmetric_derives_the_other_direction_once() {
        let sym = with(Axioms {
            symmetric: true,
            ..Default::default()
        });
        let edges = [e(1, 1, 2)];
        let d = derive(&edges, &sym);
        assert_eq!(pairs(&d), vec![(2, 1)]);
        assert_eq!(d.facts[0].rule, Rule::Symmetric);
        assert_eq!(d.facts[0].premises, vec![f(1)]);
        // Both directions asserted → nothing left to derive
        let both = [e(1, 1, 2), e(2, 2, 1)];
        assert!(derive(&both, &sym).facts.is_empty());
    }

    #[test]
    fn validity_is_the_intersection() {
        // 1→2 over [10,30), 2→3 over [20,∞) ⟹ 1→3 over [20,30)
        let edges = [te(1, 1, 2, Some(10), Some(30)), te(2, 2, 3, Some(20), None)];
        let d = derive(&edges, &transitive());
        assert_eq!(pairs(&d), vec![(1, 3)]);
        let by_fact = HashMap::from([(f(1), (Some(10), Some(30))), (f(2), (Some(20), None))]);
        assert_eq!(
            validity(&d.facts[0].premises, &by_fact),
            Some((Some(20), Some(30)))
        );
    }

    #[test]
    fn no_overlap_derives_nothing() {
        // 1→2 only over [10,20), 2→3 only over [30,40) -- this chain does not hold at any
        // point in time
        let edges = [
            te(1, 1, 2, Some(10), Some(20)),
            te(2, 2, 3, Some(30), Some(40)),
        ];
        let d = derive(&edges, &transitive());
        assert!(
            d.facts.is_empty(),
            "when the two spans do not overlap, what comes out is a fact that is never true"
        );
    }

    #[test]
    fn a_touching_boundary_is_not_an_overlap() {
        // [10,20) and [20,30): half-open intervals, touching endpoints are not an overlap
        let edges = [
            te(1, 1, 2, Some(10), Some(20)),
            te(2, 2, 3, Some(20), Some(30)),
        ];
        assert!(derive(&edges, &transitive()).facts.is_empty());
    }

    #[test]
    fn depth_is_bounded() {
        // A 40-hop chain, with a depth cap of 12
        let edges: Vec<TimedEdge> = (1..=40).map(|i| e(i, i, i + 1)).collect();
        let d = derive(&edges, &transitive());
        let longest = d.facts.iter().map(|x| x.premises.len()).max().unwrap();
        assert!(
            longest <= MAX_DEPTH,
            "the proof length should not exceed the depth cap; actual {longest}"
        );
        assert!(
            !d.facts.is_empty(),
            "having a cap does not mean deriving nothing"
        );
    }

    #[test]
    fn symmetric_feeds_the_transitive_chain() {
        // Symmetry and transitivity declared together: 1→2 and 3→2 are asserted, symmetry
        // derives 2→3, and then transitivity can join up 1→3
        let both = with(Axioms {
            symmetric: true,
            transitive: true,
            ..Default::default()
        });
        let edges = [e(1, 1, 2), e(2, 3, 2)];
        let d = derive(&edges, &both);
        let got = pairs(&d);
        assert!(
            got.contains(&(2, 1)) && got.contains(&(2, 3)),
            "the two symmetric edges"
        );
        assert!(
            got.contains(&(1, 3)),
            "a symmetry-derived edge must still feed transitivity"
        );
    }

    #[test]
    fn each_predicate_is_closed_on_its_own() {
        // 99 is transitive, 98 is not: a chain should not form across predicates
        let mut other = e(2, 2, 3);
        other.edge.predicate = n(98);
        let edges = [e(1, 1, 2), other];
        let d = derive(&edges, &transitive());
        assert!(d.facts.is_empty(), "1 →(99) 2 →(98) 3 derives nothing");
    }

    // ---- The two cross-predicate rules (inverseOf / subPropertyOf) ----
    //
    // Everything above uses the single predicate `n(99)`; these two rules need three
    // predicates by nature before they can be stated, so they get their own constants and
    // constructors

    const P: Uuid = Uuid::from_bytes([1; 16]);
    const Q: Uuid = Uuid::from_bytes([2; 16]);
    const R: Uuid = Uuid::from_bytes([3; 16]);

    /// An edge with no time on it, on a given predicate
    fn ep(pred: Uuid, fact: u8, s: u8, o: u8) -> TimedEdge {
        TimedEdge {
            edge: Edge {
                fact: f(fact),
                predicate: pred,
                subject: n(s),
                object: n(o),
            },
            from: None,
            to: None,
        }
    }
    /// A given predicate, with an interval
    fn tep(pred: Uuid, fact: u8, s: u8, o: u8, from: Option<i64>, to: Option<i64>) -> TimedEdge {
        TimedEdge {
            from,
            to,
            ..ep(pred, fact, s, o)
        }
    }
    /// `p⁻¹ = q` and `q⁻¹ = p` -- mutually pointing; convergence is tested with this
    fn inverse_pair() -> HashMap<Uuid, Axioms> {
        HashMap::from([
            (
                P,
                Axioms {
                    inverse_of: Some(Q),
                    ..Default::default()
                },
            ),
            (
                Q,
                Axioms {
                    inverse_of: Some(P),
                    ..Default::default()
                },
            ),
        ])
    }
    /// `p ⊑ q`
    fn sub_property() -> HashMap<Uuid, Axioms> {
        HashMap::from([(
            P,
            Axioms {
                sub_property_of: Some(Q),
                ..Default::default()
            },
        )])
    }

    /// `A works_at B` ⟹ `B employs A`: the ends swap **and** the predicate changes.
    /// Doing only one of them is the commonest way to get this rule wrong, so both are asserted.
    #[test]
    fn the_inverse_swaps_the_ends_and_the_predicate() {
        let d = derive(&[ep(P, 1, 1, 2)], &inverse_pair());
        assert_eq!(d.facts.len(), 1, "one edge derives exactly one inverse");
        let got = &d.facts[0];
        assert_eq!(got.predicate, Q, "**the predicate changed**");
        assert_eq!(
            (got.subject, got.object),
            (n(2), n(1)),
            "**the ends swapped too**"
        );
        assert_eq!(got.rule, Rule::Inverse);
        assert_eq!(
            got.via, P,
            "the declaration sits on P, the output lands on Q"
        );
        assert_eq!(
            got.premises,
            vec![f(1)],
            "the proof is just that one original edge"
        );
    }

    /// `p⁻¹ = q` and `q⁻¹ = p` -- mutually pointing. The edge derived back has already been
    /// asserted, so this must converge instead of bouncing to and fro.
    #[test]
    fn a_mutual_inverse_settles_instead_of_bouncing() {
        let d = derive(&[ep(P, 1, 1, 2), ep(Q, 2, 2, 1)], &inverse_pair());
        assert!(
            d.facts.is_empty(),
            "both directions have already been asserted, nothing should be derived -- **assertions win**"
        );
    }

    /// `p ⊑ q`: assert the specific one and the general one holds too. The ends stay put.
    #[test]
    fn a_sub_property_lifts_the_predicate_and_keeps_the_ends() {
        let d = derive(&[ep(P, 1, 1, 2)], &sub_property());
        assert_eq!(d.facts.len(), 1);
        let got = &d.facts[0];
        assert_eq!(got.predicate, Q, "lifted to the parent property");
        assert_eq!((got.subject, got.object), (n(1), n(2)), "the ends stay put");
        assert_eq!(got.rule, Rule::SubProperty);
        assert_eq!(
            got.via, P,
            "**via is the predicate the axiom was declared on**, not the derived one -- the write finds the rule row by it"
        );
    }

    /// **For the two rules that do not change the predicate, `via` must equal `predicate`.**
    ///
    /// This looks like a truism, and it is exactly why that bug could hide: the write used to
    /// find the rule row by `predicate`, which was right for transitivity and symmetry all
    /// along, so nobody noticed the key was the wrong one. Add the two cross-predicate rules
    /// and the fact derived from `ceo_of ⊑ works_at` finds no rule and is dropped silently.
    #[test]
    fn for_the_same_predicate_rules_via_is_the_predicate() {
        let d = derive(&[e(1, 1, 2), e(2, 2, 3)], &transitive());
        assert!(!d.facts.is_empty());
        for f in &d.facts {
            assert_eq!(
                f.via, f.predicate,
                "transitivity does not change the predicate"
            );
        }
    }

    /// **This one is the reason for the whole rework**: three rules chained together.
    ///
    /// `A ceo_of B` ∧ `ceo_of ⊑ works_at` ∧ `works_at⁻¹ = employs`
    ///   ⟹ `A works_at B` ⟹ `B employs A`
    ///
    /// The old structure, grouped by predicate, broke at the first step.
    #[test]
    fn a_sub_property_feeds_the_inverse() {
        let mut ax = HashMap::new();
        // ceo_of ⊑ works_at
        ax.insert(
            P,
            Axioms {
                sub_property_of: Some(Q),
                ..Default::default()
            },
        );
        // works_at⁻¹ = employs
        ax.insert(
            Q,
            Axioms {
                inverse_of: Some(R),
                ..Default::default()
            },
        );
        let d = derive(&[ep(P, 1, 1, 2)], &ax);
        let mut got: Vec<(Uuid, u8, u8)> = d
            .facts
            .iter()
            .map(|x| (x.predicate, x.subject.as_bytes()[0], x.object.as_bytes()[0]))
            .collect();
        got.sort();
        assert!(got.contains(&(Q, 1, 2)), "first lifted to works_at");
        assert!(
            got.contains(&(R, 2, 1)),
            "**then turned into employs the other way round** -- across two predicates, which the old structure could not do"
        );
        assert_eq!(got.len(), 2);
        // The proof grows along with it: the second hop consumes the first of the two premises
        let employs = d.facts.iter().find(|x| x.predicate == R).unwrap();
        assert_eq!(
            employs.premises,
            vec![f(1)],
            "the root is still that original assertion"
        );
    }

    /// An edge the inverse produces must still be joinable by transitivity: `p` transitive,
    /// `q` its inverse, and `B q A` ∧ `C q B` should derive `C q A` (if q is transitive too).
    #[test]
    fn what_the_inverse_produces_can_still_be_chained() {
        let mut ax = HashMap::new();
        ax.insert(
            P,
            Axioms {
                inverse_of: Some(Q),
                ..Default::default()
            },
        );
        ax.insert(
            Q,
            Axioms {
                transitive: true,
                ..Default::default()
            },
        );
        // A p B, B p C  ⟹  B q A, C q B  ⟹ (q transitive) ⟹ C q A
        let d = derive(&[ep(P, 1, 1, 2), ep(P, 2, 2, 3)], &ax);
        let got: Vec<(Uuid, u8, u8)> = d
            .facts
            .iter()
            .map(|x| (x.predicate, x.subject.as_bytes()[0], x.object.as_bytes()[0]))
            .collect();
        assert!(got.contains(&(Q, 2, 1)));
        assert!(got.contains(&(Q, 3, 2)));
        assert!(
            got.contains(&(Q, 3, 1)),
            "**edges the inverse produces must go into the adjacency table**, otherwise transitivity cannot join onto them"
        );
    }

    /// Intervals are intersected as ever, across predicates too.
    #[test]
    fn the_inverse_carries_the_same_span() {
        let d = derive(&[tep(P, 1, 1, 2, Some(10), Some(20))], &inverse_pair());
        assert_eq!(d.facts.len(), 1);
        let v = validity(
            &d.facts[0].premises,
            &HashMap::from([(f(1), (Some(10), Some(20)))]),
        );
        assert_eq!(
            v,
            Some((Some(10), Some(20))),
            "the inverse leaves the validity period alone"
        );
    }

    /// Being its own inverse = symmetric, but it still must not derive self-loops.
    #[test]
    fn a_predicate_that_is_its_own_inverse_still_refuses_self_loops() {
        let ax = HashMap::from([(
            P,
            Axioms {
                inverse_of: Some(P),
                ..Default::default()
            },
        )]);
        let d = derive(&[ep(P, 1, 1, 1)], &ax);
        assert!(
            d.facts.is_empty(),
            "the inverse of `A p A` is still `A p A` -- no self-loops"
        );
    }

    // ---------- Contradictions (0017) ----------

    /// An edge with an interval, on a given predicate
    fn et(pred: Uuid, fact: u8, s: u8, o: u8, from: Option<i64>, to: Option<i64>) -> TimedEdge {
        TimedEdge {
            edge: Edge {
                fact: f(fact),
                predicate: pred,
                subject: n(s),
                object: n(o),
            },
            from,
            to,
        }
    }

    fn spans_of(edges: &[TimedEdge]) -> HashMap<Uuid, (Option<i64>, Option<i64>)> {
        edges
            .iter()
            .map(|e| (e.edge.fact, (e.from, e.to)))
            .collect()
    }

    /// `ceo_of ⊑ works_at` with works_at functional: Mira's ceo_of derives works_at Acme,
    /// while the ledger says she works_at Globex -- a derivation runs into an assertion, and
    /// names it
    #[test]
    fn a_derivation_that_breaks_functional_names_the_assertion_it_hit() {
        let ax = HashMap::from([
            (
                P,
                Axioms {
                    sub_property_of: Some(Q),
                    ..Default::default()
                },
            ),
            (
                Q,
                Axioms {
                    functional: true,
                    ..Default::default()
                },
            ),
        ]);
        let edges = [ep(P, 1, 1, 2), ep(Q, 2, 1, 3)];
        let d = derive(&edges, &ax);
        assert_eq!(d.facts.len(), 1);
        let c = contradictions(&d, &edges, &ax, &spans_of(&edges));
        assert_eq!(
            c.with_assertions,
            vec![Clash {
                derived: 0,
                axiom: Kind::Functional,
                against: f(2)
            }]
        );
        assert!(c.between_derivations.is_empty());
        assert_eq!(c.blocked(), HashSet::from([0]));
    }

    /// Disjoint intervals are not a contradiction: the predecessor and the successor
    #[test]
    fn disjoint_intervals_are_succession_and_stay_silent() {
        let ax = HashMap::from([
            (
                P,
                Axioms {
                    sub_property_of: Some(Q),
                    ..Default::default()
                },
            ),
            (
                Q,
                Axioms {
                    functional: true,
                    ..Default::default()
                },
            ),
        ]);
        let edges = [
            et(P, 1, 1, 2, Some(10), Some(20)),
            et(Q, 2, 1, 3, Some(30), None),
        ];
        let d = derive(&edges, &ax);
        let c = contradictions(&d, &edges, &ax, &spans_of(&edges));
        assert!(c.with_assertions.is_empty(), "{c:?}");
    }

    /// Symmetric and asymmetric: symmetry derives `B p A` from `A p B`, while p is also
    /// declared asymmetric -- every assertion runs into its own mirror image
    #[test]
    fn a_symmetric_derivation_hits_the_asymmetric_assertion() {
        let ax = HashMap::from([(
            P,
            Axioms {
                symmetric: true,
                asymmetric: true,
                ..Default::default()
            },
        )]);
        let edges = [ep(P, 1, 1, 2)];
        let d = derive(&edges, &ax);
        let c = contradictions(&d, &edges, &ax, &spans_of(&edges));
        assert_eq!(c.with_assertions.len(), 1);
        assert_eq!(c.with_assertions[0].axiom, Kind::Asymmetry);
        assert_eq!(c.with_assertions[0].against, f(1));
    }

    /// When two derivations clash they are aggregated by rule pair, and neither is written
    #[test]
    fn derivations_that_disagree_are_grouped_by_the_rules_that_made_them() {
        let ax = HashMap::from([
            (
                P,
                Axioms {
                    sub_property_of: Some(Q),
                    ..Default::default()
                },
            ),
            (
                Q,
                Axioms {
                    functional: true,
                    ..Default::default()
                },
            ),
        ]);
        // 1 ceo_of 2 and 1 ceo_of 3: two works_at edges derived by the same rule, mutually
        // exclusive
        let edges = [ep(P, 1, 1, 2), ep(P, 2, 1, 3), ep(P, 3, 4, 5)];
        let d = derive(&edges, &ax);
        assert_eq!(d.facts.len(), 3);
        let c = contradictions(&d, &edges, &ax, &spans_of(&edges));
        assert!(c.with_assertions.is_empty());
        assert_eq!(c.between_derivations.len(), 1);
        let rc = &c.between_derivations[0];
        assert_eq!(rc.a, (P, Rule::SubProperty));
        assert_eq!(rc.b, (P, Rule::SubProperty));
        assert_eq!(rc.axiom, Kind::Functional);
        assert_eq!(rc.pairs.len(), 1);
        // The third one (4 works_at 5) clashes with nobody and is written as usual
        assert_eq!(c.blocked().len(), 2);
        assert!(!c.blocked().contains(&2));
    }

    /// With no axioms on the predicate there is no contradiction to speak of
    #[test]
    fn a_predicate_without_axioms_cannot_contradict() {
        let ax = HashMap::from([(
            P,
            Axioms {
                sub_property_of: Some(Q),
                ..Default::default()
            },
        )]);
        let edges = [ep(P, 1, 1, 2), ep(Q, 2, 1, 3)];
        let d = derive(&edges, &ax);
        let c = contradictions(&d, &edges, &ax, &spans_of(&edges));
        assert_eq!(c, Contradictions::default());
    }
}
