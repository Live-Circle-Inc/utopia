//! Lands the predicate the model came out with on a relation the ontology **already has**.
//!
//! Extraction used to do nothing but an exact key comparison (`rel_ids.get(f.predicate)`); when
//! that missed it downgraded to `related_to` and left the original word in
//! `fact_evidence.proposed_predicate` for a human to adopt.
//!
//! Measured, that leaks badly. Halfway through the ai-timeline corpus (Wikipedia articles on AI
//! companies), 49.8% of the facts were `related_to`, and `ontology_misses` held 398 distinct
//! relation names across 895 uses. One class of those is not missing vocabulary at all, it is
//! **vocabulary that is right there in the ontology, with the model merely saying it backwards
//! or in a different tense**: `produced_by | ChatGPT → OpenAI` means the `produces` we already
//! have, just with subject and object swapped.
//!
//! That 49.8% is the number **after the measurement harness turned off automatic extension of
//! the ontology** (`run.mjs` sets `auto_extend_ontology=FALSE` at base creation, while the
//! column default is true). The product's actual cold-start path is `bootstrap_ontology`
//! filling the ontology in afterwards, so do not use this proportion to say how bad the product
//! is -- it measures "how often we downgrade when vocabulary is missing", and that is exactly
//! the part this module is here to reduce.
//!
//! Three stages are added here, **order is priority**, and the wide ones always come after the
//! narrow ones:
//!
//! 1. Exact key -- the existing behaviour, not a character changed
//! 2. Spelling alignment -- `acquiredFrom` / `acquired_from` / `Acquired From` are one and the same
//! 3. Inflection folding -- `produced` and `produces` fold to the same string (only tense and
//!    plurals get shaved, see `inflect_base`)
//!
//! Stages 2 and 3 each get one more try at "drop a trailing by", and on a hit they **swap
//! subject and object**: in English `_by` is an explicit marker of the passive, and
//! `X produced_by Y` is the same edge as `Y produces X`.
//!
//! **A collision means no match.** If the ontology holds both `produces` and `produced`, the two
//! fold to the same string, and picking either is a guess -- better to downgrade and let a human
//! adopt it. Only the exact key is exempt from this; it is unique to begin with.
//!
//! **What was measured** (the same 895 misses, two vocabularies): a seed ontology of 10
//! relations recovered 49 of them; schema.org's 629 relations recovered 59. Of the 9 recovered
//! wordings 6 are correct and 3 have a dubious direction (`addresses→address`,
//! `funds→funding`, `sponsors→sponsor`), one use each, and **all of them come from the path
//! that shaves off a single `s`** -- in English `sponsors` is both a third-person verb and a
//! plural noun, and a suffix cannot tell them apart. No further rule was added for those three:
//! the sample is too small, and adding one would just be overfitting. Their counterfactual is
//! not "correct" either, it is `related_to` -- the original word is still in
//! proposed_predicate.
//!
//! No synonym judgement (`partners_with` against `collaborates_with`): that is the job of
//! retrieval and of the model. Putting it here would quietly turn "spelling alignment" into
//! "means roughly the same thing", and when the latter is wrong nobody can see it.

use std::collections::HashMap;
use utopia_core::models::RelationType;
use uuid::Uuid;

/// Word splitting: split at every non-alphanumeric character, and at camelCase boundaries too.
///
/// The camelCase half is not optional: an OWL-imported key looks exactly like `acquiredFrom`, and
/// without the split it will not fold together with a hand-written `acquired_from` -- and
/// "whether an imported ontology can actually be used" is precisely what this path protects.
fn words(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut prev_is_lower_or_digit = false;
    for c in s.chars() {
        if !c.is_alphanumeric() {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            prev_is_lower_or_digit = false;
            continue;
        }
        if c.is_uppercase() && prev_is_lower_or_digit && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        prev_is_lower_or_digit = c.is_lowercase() || c.is_numeric();
        cur.extend(c.to_lowercase());
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// The spelling-aligned form: concatenate the split words directly, keeping no separators. Same
/// rule as `api::ontology_routes::normalize_name`.
fn joined(words: &[String]) -> String {
    words.concat()
}

/// Inflection folding: shave off tense and plurals, **do not touch derivational suffixes**.
///
/// This was Snowball (`rust-stemmers`) at one point, and the measurement came out backwards: with
/// a vocabulary of 10 words it recovered 49, and swapping in schema.org's 629 words left only 18.
/// Because Snowball shaves derivational suffixes along with the rest, `producer` and `produces`
/// both became `produc`, so the collision rule refused to match -- **the bigger the vocabulary,
/// the less it dared touch**. And `producer` (a person) and `produces` (an action) ought to be
/// two relations in the first place; folding them together is the stemmer's fault, not the
/// collision rule's.
///
/// So inflection only: `-ies→y`, `-ing`, `-ed`, `-s` (except `-ss`), and once shaved, drop a
/// trailing `e`. That last step is there to make the `-s` and `-ed` paths meet --
/// `produces→produce→produc`, `produced→produc` -- otherwise the two tenses of one verb would
/// never line up.
///
/// The length guard looks at the result **after** shaving, not at the intermediate result.
/// Gating on the intermediate result makes the two paths diverge: `uses` shaves `-s`, passes
/// with three letters left, then drops an `e` and becomes two, while `used` shaves `-ed`, is two
/// letters on the spot and gets blocked -- so the two tenses of one verb would never line up.
/// If fewer than two letters would be left, nothing is shaved at all (`is` must not become `i`,
/// `led` must not become `l`).
///
/// Chinese has no inflectional suffixes, so coming through here is the identity transform, which
/// is why there is no need to fork on `ontology_lang`.
fn inflect_base(w: &str) -> String {
    let n = w.len();
    let mut s = if w.ends_with("ies") && n > 3 {
        format!("{}y", &w[..n - 3])
    } else if w.ends_with("ing") && n > 3 {
        w[..n - 3].to_string()
    } else if w.ends_with("ed") && n > 2 {
        w[..n - 2].to_string()
    } else if w.ends_with('s') && !w.ends_with("ss") && n > 1 {
        w[..n - 1].to_string()
    } else {
        w.to_string()
    };
    // A trailing e always goes: `produces→produce` and `produced→produc` meet through this step,
    // and it incidentally lands `note` and `notes` together
    if s.ends_with('e') {
        s.pop();
    }
    if s.chars().count() < 2 {
        return w.to_string();
    }
    s
}

/// Leading light verbs: `has_funding` and `funding` are the same relation, and the prefix is a
/// naming habit, not a meaning.
///
/// Measured (ai-timeline-ends × schema.org): among the original wordings of facts with an empty
/// predicate, `has_funding` came up empty ×4 while the ontology holds `funding`, and `product`
/// came up empty ×2 while the ontology holds `has_product` -- **the only difference is that one
/// prefix**.
///
/// Strip only while a word is left: `has` on its own is a word in its own right and must not be
/// stripped to nothing.
/// Over-merging cannot produce a wrong match -- `insert` voids a key the moment it lands on two
/// relations (a collision voids it), so the worst case falls back to "no match" rather than
/// matching the wrong one.
const LEADING_AUX: &[&str] = &[
    "has", "have", "had", "is", "are", "was", "were", "be", "been",
];

fn stems(words: &[String]) -> Vec<String> {
    let words = match words.split_first() {
        Some((head, rest)) if !rest.is_empty() && LEADING_AUX.contains(&head.as_str()) => rest,
        _ => words,
    };
    words.iter().map(|w| inflect_base(w)).collect()
}

/// The **merge key** of a wording: different tenses of one relation land on the same key.
///
/// This is for the ontology-adoption path. During extraction a wording is compared against the
/// relations that **already exist** (the three-stage match above); adoption has a different job
/// to do -- first merge the wordings that mean the same thing as each other, then count the
/// votes.
///
/// Not merging costs us, and the cost is visible: among the wordings still stuck on the fallback
/// predicate after an ai-timeline run, `sued` and `sues` are two entries (11 and 5),
/// `integrated_with` and `integrates_with` are two (5 each), and so are `announced`/`announces`,
/// `supported`/`supports` and `developed`/`develops`. Each counts its own votes and each falls
/// short of the "appears in ≥2 documents" threshold; once merged, the candidates that qualify go
/// from 70 groups to 85 and from 281 entries to 355.
///
/// **Prepositions are not merged**: `integrated_with` and `integrated_into` stay two groups.
/// `works_at` and `works_in` really may be two different things, and here we would rather miss.
///
/// **`_by` is not merged either** (to do): `founded_by` and `founded` are two directions of one
/// edge.
///
/// The original reason for not merging was "the adoption path copies the subject from the old row
/// as is and cannot swap it" -- **that reason no longer holds** (#109 made `adopt` bind the
/// subject explicitly and support swapping). Only one gap is left: while neither side is in the
/// ontology yet (`founded_by` 42 entries, `founded` 4, both with enough votes), the
/// `PredicateIndex` lookup before adoption matches neither of them, so each gets created, facing
/// opposite ways. The fix is to fold `_by` into the same group and mark the whole group as
/// needing a swap; nothing stands in the way any more.
pub fn merge_key(form: &str) -> Vec<String> {
    stems(&words(form))
}

/// A collision voids the entry: when one form lands on two different relations, picking either
/// is a guess.
fn insert<K: std::hash::Hash + Eq>(map: &mut HashMap<K, Option<Uuid>>, key: K, id: Uuid) {
    map.entry(key)
        .and_modify(|slot| {
            if *slot != Some(id) {
                *slot = None;
            }
        })
        .or_insert(Some(id));
}

pub struct PredicateIndex {
    exact: HashMap<String, Uuid>,
    by_joined: HashMap<String, Option<Uuid>>,
    by_stems: HashMap<Vec<String>, Option<Uuid>>,
}

impl PredicateIndex {
    /// **Only takes `kind == "relation"`.** Attributes go through the literal-value channel; let
    /// a fuzzy match cross over and `founding_date` turns into an edge pointing at an entity
    /// "2015" -- exactly the thing the ontology-adoption path already goes out of its way to
    /// block, and it must not be let back in through the back door here.
    pub fn build(rtypes: &[RelationType]) -> Self {
        let mut exact = HashMap::new();
        let mut by_joined = HashMap::new();
        let mut by_stems = HashMap::new();
        for r in rtypes.iter().filter(|r| r.kind == "relation") {
            exact.insert(r.key.clone(), r.id);
            let w = words(&r.key);
            if w.is_empty() {
                continue;
            }
            insert(&mut by_joined, joined(&w), r.id);
            insert(&mut by_stems, stems(&w), r.id);
        }
        Self {
            exact,
            by_joined,
            by_stems,
        }
    }

    fn widened(&self, w: &[String]) -> Option<Uuid> {
        if w.is_empty() {
            return None;
        }
        if let Some(hit) = self.by_joined.get(&joined(w)) {
            return *hit;
        }
        *self.by_stems.get(&stems(w))?
    }

    /// Returns `(relation id, whether subject and object must be swapped)`. `None` = genuinely
    /// not in the ontology, downgrade it.
    pub fn lookup(&self, proposed: &str) -> Option<(Uuid, bool)> {
        if let Some(id) = self.exact.get(proposed) {
            return Some((*id, false));
        }
        let w = words(proposed);
        if let Some(id) = self.widened(&w) {
            return Some((id, false));
        }
        // Passive form: `produced_by` only lines up with `produces` once the by is dropped, and
        // subject and object have to be turned around. At least two words are required -- a
        // lone `by` shaves down to nothing.
        if w.len() >= 2 && w[w.len() - 1] == "by" {
            if let Some(id) = self.widened(&w[..w.len() - 1]) {
                return Some((id, true));
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `has_funding` and `funding` are the same relation; the prefix is a naming habit, not a
    /// meaning.
    ///
    /// In the measurements both of these pairs came up empty: the ontology has `funding` and the
    /// model wrote `has_funding` (×4); the ontology has `has_product` and the model wrote
    /// `product` (×2). The only difference is that one prefix.
    #[test]
    fn a_leading_auxiliary_does_not_make_a_different_relation() {
        let rels = vec![rel("funding"), rel("has_product")];
        let idx = PredicateIndex::build(&rels);
        assert!(
            idx.lookup("has_funding").is_some(),
            "has_funding should land on funding"
        );
        assert!(
            idx.lookup("product").is_some(),
            "product should land on has_product"
        );
        // Both directions have to work
        assert!(idx.lookup("funding").is_some());
        assert!(idx.lookup("has_product").is_some());
    }

    /// **Strip only while a word is left.** `has` on its own is a word in its own right, and
    /// stripping it to nothing would make everything match.
    #[test]
    fn a_bare_auxiliary_is_still_a_word() {
        let rels = vec![rel("has")];
        let idx = PredicateIndex::build(&rels);
        assert!(idx.lookup("has").is_some(), "has should match itself");
        assert!(
            idx.lookup("owns").is_none(),
            "stripping to nothing lets unrelated words match too"
        );
    }

    /// A collision still voids: when the ontology holds both `funding` and `has_funding`, picking
    /// either is a guess.
    /// The worst outcome of over-merging is "no match", not "matched the wrong one".
    #[test]
    fn folding_the_prefix_never_produces_a_wrong_match() {
        let rels = vec![rel("funding"), rel("has_funding")];
        let idx = PredicateIndex::build(&rels);
        // The exact key still goes straight through
        assert!(idx.lookup("funding").is_some());
        assert!(idx.lookup("has_funding").is_some());
        // The collision is at the **stem** layer: with the prefix stripped, `funding` and
        // `has_funding` are both ["funding"]. To test it we need a form that cannot reach
        // spelling alignment and can only land on the stem -- `has_fundings` concatenates to
        // hasfundings, which is not in the ontology, so it falls through to the stem and the
        // collision voids it
        assert!(
            idx.lookup("has_fundings").is_none(),
            "the stem layer collided and yet one of them was still picked"
        );
    }

    fn rel(key: &str) -> RelationType {
        RelationType {
            id: Uuid::new_v4(),
            kb_id: Uuid::nil(),
            key: key.to_string(),
            label: key.to_string(),
            temporal: "state".into(),
            functional: false,
            inverse_functional: false,
            builtin: false,
            description: String::new(),
            iri: None,
            kind: "relation".into(),
            domains: Vec::new(),
            ranges: Vec::new(),
            datatype: None,
            unit: None,
        }
    }

    fn attr(key: &str) -> RelationType {
        RelationType {
            kind: "attribute".into(),
            ..rel(key)
        }
    }

    #[test]
    fn splits_camel_case_and_separators_the_same_way() {
        assert_eq!(words("acquiredFrom"), ["acquired", "from"]);
        assert_eq!(words("acquired_from"), ["acquired", "from"]);
        assert_eq!(words("Acquired From"), ["acquired", "from"]);
        // All caps is not a camelCase boundary; do not split IRI into i/r/i
        assert_eq!(words("IRI"), ["iri"]);
        assert_eq!(words("gpt4Model"), ["gpt4", "model"]);
    }

    #[test]
    fn exact_key_still_wins_unchanged() {
        let types = [rel("produces")];
        let idx = PredicateIndex::build(&types);
        assert_eq!(idx.lookup("produces"), Some((types[0].id, false)));
    }

    #[test]
    fn separator_and_case_differences_align() {
        let types = [rel("acquired_from")];
        let idx = PredicateIndex::build(&types);
        assert_eq!(idx.lookup("acquiredFrom"), Some((types[0].id, false)));
        assert_eq!(idx.lookup("Acquired From"), Some((types[0].id, false)));
    }

    #[test]
    fn tense_folds_without_swapping() {
        let types = [rel("produces")];
        let idx = PredicateIndex::build(&types);
        // The model wrote the past tense; it is still the same edge, and the direction has not
        // changed either
        assert_eq!(idx.lookup("produced"), Some((types[0].id, false)));
    }

    /// This one is the reason the module exists: `ChatGPT produced_by OpenAI` and
    /// `OpenAI produces ChatGPT` are the same edge, differing only in subject/object direction.
    #[test]
    fn passive_form_matches_and_asks_for_a_swap() {
        let types = [rel("produces")];
        let idx = PredicateIndex::build(&types);
        assert_eq!(idx.lookup("produced_by"), Some((types[0].id, true)));
        assert_eq!(idx.lookup("producedBy"), Some((types[0].id, true)));
    }

    #[test]
    fn multi_word_passive_matches() {
        let types = [rel("invests_in")];
        let idx = PredicateIndex::build(&types);
        assert_eq!(idx.lookup("invested_in"), Some((types[0].id, false)));
    }

    /// **Derivation is not inflection.** This one is the reason Snowball was replaced: in
    /// schema.org `producer` and `produces` coexist, the stemmer folds both into `produc`, and
    /// the collision rule then rejects `produced` along with them -- the bigger the vocabulary,
    /// the less it recovers (measured, 49 recoveries dropped to 18). Shaving inflectional
    /// suffixes only avoids that collision: `producer` stays as it is.
    #[test]
    fn derivational_forms_stay_separate_from_inflected_ones() {
        let types = [rel("produces"), rel("producer"), rel("production_company")];
        let idx = PredicateIndex::build(&types);
        assert_eq!(idx.lookup("produced"), Some((types[0].id, false)));
        assert_eq!(idx.lookup("produced_by"), Some((types[0].id, true)));
        assert_eq!(idx.lookup("producer"), Some((types[1].id, false)));
        // Derived words must not bleed into one another either
        assert_eq!(idx.lookup("producers"), Some((types[1].id, false)));
    }

    /// The plural path and the tense path have to meet at the same string, or two spellings of
    /// one verb will never line up.
    #[test]
    fn plural_and_past_forms_meet_at_the_same_base() {
        assert_eq!(inflect_base("produces"), inflect_base("produced"));
        assert_eq!(inflect_base("uses"), inflect_base("used"));
        assert_eq!(inflect_base("notes"), inflect_base("note"));
        assert_eq!(inflect_base("studies"), inflect_base("study"));
        // Short words are not shaved: is must not become i
        assert_eq!(inflect_base("is"), "is");
        // -ss is not a plural
        assert_eq!(inflect_base("address"), inflect_base("addresses"));
    }

    /// On a collision, rather no match: when the ontology holds both `produces` and `produced`,
    /// `producing` folds onto the stem the two share, and picking either is a guess.
    #[test]
    fn ambiguous_stem_declines_rather_than_guesses() {
        let types = [rel("produces"), rel("produced")];
        let idx = PredicateIndex::build(&types);
        assert_eq!(idx.lookup("producing"), None);
        // But the exact key is unaffected; it is unique to begin with
        assert_eq!(idx.lookup("produces"), Some((types[0].id, false)));
        assert_eq!(idx.lookup("produced"), Some((types[1].id, false)));
    }

    /// Attributes must not be matchable through this path: the object is a literal value, which
    /// cannot make an edge.
    #[test]
    fn attributes_are_not_reachable() {
        let types = [attr("founding_date")];
        let idx = PredicateIndex::build(&types);
        assert_eq!(idx.lookup("founding_date"), None);
        assert_eq!(idx.lookup("foundingDate"), None);
    }

    /// What genuinely is not in the ontology downgrades as before -- no synonym judgement here.
    #[test]
    fn genuinely_missing_vocabulary_still_declines() {
        let types = [rel("produces"), rel("works_at")];
        let idx = PredicateIndex::build(&types);
        assert_eq!(idx.lookup("partners_with"), None);
        assert_eq!(idx.lookup("acquired"), None);
        // A different preposition is a different relation; do not reword on the model's behalf
        assert_eq!(idx.lookup("works_in"), None);
    }

    #[test]
    fn lone_by_does_not_strip_to_nothing() {
        let types = [rel("produces")];
        let idx = PredicateIndex::build(&types);
        assert_eq!(idx.lookup("by"), None);
        assert_eq!(idx.lookup("_by_"), None);
    }

    /// A Chinese key through the stemmer is the identity transform: nothing should be shaved and
    /// nothing should be mismatched.
    #[test]
    fn chinese_keys_pass_through() {
        let types = [rel("隶属于"), rel("生产")];
        let idx = PredicateIndex::build(&types);
        assert_eq!(idx.lookup("隶属于"), Some((types[0].id, false)));
        assert_eq!(idx.lookup("生产"), Some((types[1].id, false)));
        assert_eq!(idx.lookup("收购"), None);
    }
}
