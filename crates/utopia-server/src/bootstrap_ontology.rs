//! Automatic ontology extension: when extraction runs into a phrasing outside the ontology, add
//! it to the ontology and remap the facts that were waiting on it.
//!
//! A freshly created KB seeds no relations at all; the starting point is whichever ontology pack
//! the user picked (0008), or nothing. No pack, however big, can hold every phrasing in a
//! corpus: facts that don't land on the ontology leave the predicate empty and record the
//! original wording on the evidence (0010), so on the graph those edges carry only the source
//! wording and no vocabulary semantics -- until somebody sits down, clicks Suggest, reads the
//! proposals and Adds them one at a time. This module automates that: if a phrasing is common
//! enough, turn it into a relation and remap the facts waiting on it.
//!
//! What makes it acceptable to be this bold is that adoption is reversible (see graph::unadopt):
//! if it's wrong, one click takes it back, and the old facts were never destroyed. So the axis of
//! judgement is not "how confident are we" but "how expensive is it to be wrong".
//!
//! **Whether we do this on the human's behalf is declared by the human in the KB settings**
//! (`auto_extend_ontology`, on by default). We once tried to infer it from behaviour -- "has the
//! ontology been touched" -- which is guessing, and the consequence of guessing wrong was absurd:
//! a single Add click on a proposal would permanently switch the suggestion feature off. With the
//! switch, the guessing is gone, and so is the freeze.
//!
//! Turning it off does not affect "noticing": unmatched counts keep accumulating and stay visible
//! in the Unmatched panel, they just become proposals you click, with not one piece of
//! information lost.
//!
//! The sole exception is `functional`, which is never automatic, not even with the switch on: it
//! drives the temporal engine to auto-close facts and generate conflicts, and by the time you
//! notice, those closures are themselves a chain of supersedes -- not the "cheap to be wrong"
//! kind.

use std::collections::{HashMap, HashSet};
use uuid::Uuid;

use crate::api::ontology_routes;
use crate::predicate_match::{merge_key, PredicateIndex};
use crate::state::AppState;

/// Below this many qualifying signals (predicates + types) don't bother -- there is not enough
/// for a decent proposal, and it burns an LLM call for nothing.
const MIN_SIGNALS: usize = 3;
/// Only adopt phrasings that appear in at least this many documents. **Something that appeared in
/// only one document is that document's wording, not this organisation's vocabulary** -- and the
/// ontology feeds back into the extraction prompt, so one accident becomes a standing
/// instruction. The side effect is exactly right: with only one document nothing reaches the
/// threshold, so nothing happens, and we try again with the next one.
const MIN_DOCS: i64 = 2;

/// A set of phrasings merged by inflectional stem -- once adopted they are the same relation.
struct RelationGroup {
    /// Canonical key: the phrasing in the group with the most facts. **We do not ask the model to
    /// name it** -- every phrasing in the group is wording that really occurred in the source, and
    /// picking the most common one fits the corpus better than inventing a new word
    key: String,
    /// Every phrasing in the group; they are all remapped on adoption
    forms: Vec<String>,
    facts: i64,
    docs: usize,
    /// Where to land when the ontology already has an equivalent relation:
    /// `(relation id, whether subject and object must be swapped)`.
    /// `None` = the ontology really doesn't have it, so create it by vote count
    existing: Option<(Uuid, bool)>,
}

/// Decide which relations to adopt by vote count. **No model call.**
///
/// Three steps: merge phrasings by inflectional stem (`sued` and `sues` are the same relation),
/// take the union of documents, apply the threshold.
///
/// The union cannot be a sum: one and the same document may well have used both spellings, and
/// summing would let a single document push a phrasing past ">= 2 documents". That is why we need
/// `proposed_predicate_documents` to get the real document ids.
///
/// The output is **sorted** -- half the value of this path is determinism, and HashMap iteration
/// order is not that.
async fn counted_relation_groups(
    state: &AppState,
    kb_id: Uuid,
) -> anyhow::Result<Vec<RelationGroup>> {
    let forms = utopia_store::graph::proposed_predicates(&state.pool, kb_id).await?;
    let pairs = utopia_store::graph::proposed_predicate_documents(&state.pool, kb_id).await?;
    // **Ask the ontology before creating anything.**
    //
    // Without this step, adoption only creates by vote count and never checks "is there already an
    // equivalent". Measured consequence: in the demo-b3 KB, `produced_by` and `produces`, and
    // `developed_by` and `develops`, each became a relation of its own -- the two directions of
    // the same thing permanently split apart.
    //
    // And `produces` had 265 facts while `produced_by` had only 15 -- the one with more votes got
    // into the ontology first, and the one with fewer should have been caught by the `_by` rule in
    // `predicate_match`, but grew into an independent relation because **the adoption path never
    // went through the matcher at all**. The matcher was only ever used during extraction; this is
    // the second place it was absent.
    let rtypes = utopia_store::graph::relation_types(&state.pool, kb_id).await?;
    let index = PredicateIndex::build(&rtypes);

    let mut docs_of: HashMap<String, HashSet<Uuid>> = HashMap::new();
    for (form, doc) in pairs {
        docs_of.entry(form).or_default().insert(doc);
    }

    let mut grouped: HashMap<Vec<String>, Vec<utopia_core::models::ProposedPredicate>> =
        HashMap::new();
    for f in forms {
        grouped.entry(merge_key(&f.form)).or_default().push(f);
    }

    let mut out = Vec::new();
    for (_, mut members) in grouped {
        // More facts first, ties by lexicographic order -- the choice of canonical key must not
        // depend on HashMap ordering
        members.sort_by(|a, b| b.fact_count.cmp(&a.fact_count).then(a.form.cmp(&b.form)));
        let mut docs: HashSet<Uuid> = HashSet::new();
        for m in &members {
            if let Some(d) = docs_of.get(&m.form) {
                docs.extend(d);
            }
        }
        if (docs.len() as i64) < MIN_DOCS {
            continue;
        }
        // If any phrasing in the group lands on a relation the ontology already has, the whole
        // group lands there. Phrasings in a group share an inflectional stem, so whether they end
        // in `by` is necessarily consistent too (`produced_by` and `produces` are in different
        // groups), which makes "swap or not" **uniform across the group** -- no need to decide it
        // phrasing by phrasing
        let existing = members.iter().find_map(|m| index.lookup(&m.form));
        out.push(RelationGroup {
            key: members[0].form.clone(),
            facts: members.iter().map(|m| m.fact_count).sum(),
            forms: members.into_iter().map(|m| m.form).collect(),
            docs: docs.len(),
            existing,
        });
    }
    out.sort_by(|a, b| b.facts.cmp(&a.facts).then(a.key.cmp(&b.key)));
    Ok(out)
}

pub async fn bootstrap_ontology(state: &AppState, kb_id: Uuid) -> anyhow::Result<()> {
    // Two concurrent extraction jobs may both see "idle" and each enqueue once; and the switch
    // may have just been turned off
    let kb = utopia_store::kbs::get(&state.pool, kb_id).await?;
    if !kb.auto_extend_ontology {
        tracing::debug!(%kb_id, "auto ontology extension is off, skipping");
        return Ok(());
    }
    // The threshold asks "is there enough here to be worth an LLM call", and **predicates and
    // types are counted together**. It used to count predicates only, so a corpus that was short
    // on entity types but not on relations got skipped entirely -- even with platform x2 and
    // inference_engine x2 piled up in proposed_type, waiting.
    let forms: Vec<_> = utopia_store::graph::proposed_predicates(&state.pool, kb_id)
        .await?
        .into_iter()
        .filter(|f| f.doc_count >= MIN_DOCS)
        .collect();
    let types = utopia_store::resolution::proposed_types(&state.pool, kb_id).await?;
    if forms.len() + types.len() < MIN_SIGNALS {
        tracing::debug!(
            %kb_id, predicates = forms.len(), types = types.len(),
            "too few qualifying signals, skipping auto ontology extension"
        );
        return Ok(());
    }

    // **Relations are not put to the model; they are adopted by vote count.**
    //
    // This step used to hand the candidates to the LLM and have it answer "which of these are
    // worth making into relations". The data had already answered that question -- `runs_on`
    // appears in 8 documents and 13 facts, it is not a judgement call. And in practice the model
    // answered wrong: it missed `runs_on` and adopted `pledged_capital`, which had appeared in
    // exactly one document.
    //
    // Switching to counting has one further side effect that is crucial: **this stretch becomes
    // deterministic**. Re-running the same corpus yields the same ontology, and for the first time
    // the measurement bench can run a controlled comparison on it. Previously groups B and B3
    // differed by 3 percentage points and we could not say whether the fix had worked or it was
    // run-to-run variance -- purely because there was an LLM call sitting in the middle.
    //
    // The model has not been taken away, it has only been given a different question: see the
    // synonym merge below -- "which of these new relations mean the same thing as an existing
    // one". That one really does require understanding meaning, and if it answers wrong, unadopt
    // rolls it back with one click.
    let counted = counted_relation_groups(state, kb_id).await?;
    let proposals = ontology_routes::build_proposals(state, kb_id, "en", MIN_DOCS).await?;
    let classes = proposals
        .get("entity_types")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let mut added_relations = Vec::new();
    let mut added_classes = Vec::new();
    let mut moved_total = 0u32;
    let mut left_off_total = 0u32;
    let mut batches = Vec::new();

    for p in &classes {
        let (Some(key), Some(label)) = (str_of(p, "key"), str_of(p, "label")) else {
            continue;
        };
        match utopia_store::ontology::create_entity_type(
            &state.pool,
            kb_id,
            key,
            label,
            utopia_store::palette::color_for_key(key),
            "circle",
            // Classes created on cold start get no parent: the proposal carries no hierarchy
            // information, and guessing a parent is worse than attaching none
            &[],
            // The description goes into the extraction prompt; reason is only the rationale for
            // humans to read -- feed it the wrong one and this class becomes the new dumping ground
            str_of(p, "description")
                .or_else(|| str_of(p, "reason"))
                .unwrap_or(""),
        )
        .await
        {
            Ok(type_id) => {
                added_classes.push(key.to_string());
                // Once the class exists, move the entities waiting on it across -- create the
                // type but leave the entities alone and the ontology grows while the graph gets no
                // better: the entities that proposed model stay hanging under concept
                let forms: Vec<String> = p
                    .get("forms")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|s| s.as_str().map(String::from))
                            .collect()
                    })
                    // When the proposal gives no forms, at least claim the proposals that share
                    // the key's name
                    .unwrap_or_else(|| vec![key.to_string()]);
                match utopia_store::resolution::adopt_proposed_types(
                    &state.pool,
                    kb_id,
                    type_id,
                    &forms,
                    // A system action, not anybody's decision -- the same reasoning as writing
                    // NULL in the audit record further down this file
                    None,
                )
                .await
                {
                    Ok((batch, n)) => {
                        moved_total += n;
                        if n > 0 {
                            batches.push(batch);
                        }
                    }
                    Err(e) => tracing::warn!(%kb_id, key, error = %e, "failed to retype entities"),
                }
                for form in &forms {
                    let _ =
                        utopia_store::ontology::clear_miss(&state.pool, kb_id, "entity_type", form)
                            .await;
                }
            }
            // Key collisions and the like: skip this one, don't take the whole batch down
            Err(e) => tracing::warn!(%kb_id, key, error = %e, "cold-start class creation failed"),
        }
    }

    // Relations: adopted by vote count, one relation per group.
    //
    // Both key and label come from the corpus's own wording; description is left empty -- it goes
    // into the extraction prompt as a semantic hint, and there is no trustworthy source to write
    // here. Making one up would instead stuff an assertion nobody is accountable for into the
    // prompt, and the key itself (`runs_on`, `available_on`) already says it clearly enough.
    //
    // temporal is always state: it only drives the temporal engine when functional /
    // inverse_functional are true, and this path **never** sets those two bits automatically (see
    // the file header), so here it has no behavioural consequence.
    for g in &counted {
        // If the ontology already has an equivalent relation, don't create a new one, just remap
        // the facts onto it. Passive forms (`produced_by` matching `produces`) swap subject and
        // object when remapped
        let (predicate_id, swap) = match g.existing {
            Some(hit) => hit,
            None => {
                let label = g.key.replace('_', " ");
                match utopia_store::ontology::create_relation_type(
                    &state.pool,
                    kb_id,
                    &g.key,
                    &label,
                    "state",
                    // Cold start declares no axiom on anyone's behalf: the reasoner's criteria
                    // have to be something a human wrote down
                    Default::default(),
                    "",
                    "relation",
                    &[],
                    &[],
                    None,
                    None,
                )
                .await
                {
                    Ok(id) => {
                        added_relations.push(g.key.clone());
                        (id, false)
                    }
                    Err(e) => {
                        tracing::warn!(%kb_id, key = %g.key, error = %e, "cold-start relation creation failed");
                        continue;
                    }
                }
            }
        };
        let utopia_store::graph::Adopted {
            batch_id: batch,
            moved,
            left_off,
            corrected,
        } = utopia_store::graph::adopt_proposed_predicates(
            &state.pool,
            kb_id,
            predicate_id,
            &g.forms,
            swap,
        )
        .await?;
        tracing::info!(
            %kb_id, key = %g.key, forms = ?g.forms, docs = g.docs, facts = g.facts, moved, left_off,
            corrected, reused = g.existing.is_some(), swap,
            "adopted relation by vote count"
        );
        moved_total += moved;
        left_off_total += left_off;
        if moved > 0 {
            batches.push(batch);
        }
        for form in &g.forms {
            let _ =
                utopia_store::ontology::clear_miss(&state.pool, kb_id, "relation_type", form).await;
        }
    }

    // The attribute tier: phrasings whose object is a literal value.
    //
    // domain is not read from the proposal -- it is taken from the subject types of these facts,
    // see adopt_attribute. Facts whose value cannot be converted are not remapped; they stay
    // without a predicate and wait for next time
    let attrs = proposals
        .get("attribute_types")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    for p in &attrs {
        let (Some(key), Some(label)) = (str_of(p, "key"), str_of(p, "label")) else {
            continue;
        };
        let forms: Vec<String> = p
            .get("forms")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        if forms.is_empty() {
            continue;
        }
        match ontology_routes::adopt_attribute_auto(
            state,
            kb_id,
            key,
            label,
            str_of(p, "description")
                .or_else(|| str_of(p, "reason"))
                .unwrap_or(""),
            str_of(p, "datatype").unwrap_or("text"),
            str_of(p, "unit"),
            &forms,
        )
        .await
        {
            Ok((batch, moved)) => {
                added_relations.push(key.to_string());
                moved_total += moved;
                if moved > 0 {
                    batches.push(batch);
                }
            }
            Err(e) => {
                tracing::warn!(%kb_id, key, error = %e, "cold-start attribute creation failed")
            }
        }
    }

    // **Map to an existing type**: the ontology already holds this meaning, so only remap the
    // facts and leave the ontology untouched.
    //
    // Doing this automatically is the safe tier: it does not let the ontology grow, it only hangs
    // a batch of related_to edges onto a predicate that existed all along, and it goes through the
    // same batch mechanism as the create path, so it is just as reversible. Conversely, leaving
    // this tier out is the dangerous thing -- retrieval tells the model "we already have
    // founding_date", the model answers "these phrasings are it", and we do nothing at all, so
    // that batch of facts stays as "is related to".
    let mapped = proposals
        .get("map_to")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    for m in &mapped {
        let Some(key) = str_of(m, "key") else {
            continue;
        };
        let forms: Vec<String> = m
            .get("forms")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        if forms.is_empty() {
            continue;
        }
        // When the target is an attribute, remapping takes another path: the values have to be
        // converted according to its datatype. kind is stamped on by the server while parsing the
        // proposal (the model can only answer with a key)
        if str_of(m, "kind") == Some("attribute") {
            match ontology_routes::adopt_attribute_existing(state, kb_id, key, &forms).await {
                Ok((batch, moved)) => {
                    moved_total += moved;
                    if moved > 0 {
                        batches.push(batch);
                    }
                }
                Err(e) => {
                    tracing::warn!(%kb_id, key, error = %e, "mapping to an existing attribute failed")
                }
            }
            continue;
        }
        // Every so often the model copies in a key that was not on the candidate list (or simply
        // invents one). If it isn't found, skip it -- **do not create it**: the premise of this
        // path is that "it already exists"
        let Some(predicate_id) =
            utopia_store::ontology::relation_type_id_by_key(&state.pool, kb_id, key).await?
        else {
            tracing::warn!(%kb_id, key, "map target is not in the ontology, skipping");
            continue;
        };
        // The LLM's map_to proposals can just as easily mix active and passive, which a single
        // swap flag cannot serve
        // (same as the manual path, see that comment over in ontology_routes)
        let utopia_store::graph::Adopted {
            batch_id: batch,
            moved,
            left_off,
            corrected,
        } = utopia_store::graph::adopt_proposed_predicates(
            &state.pool,
            kb_id,
            predicate_id,
            &forms,
            false,
        )
        .await?;
        tracing::info!(
            %kb_id, key, forms = ?forms, moved, left_off, corrected,
            "adopted relation from a map proposal"
        );
        left_off_total += left_off;
        moved_total += moved;
        if moved > 0 {
            batches.push(batch);
        }
        for form in &forms {
            let _ =
                utopia_store::ontology::clear_miss(&state.pool, kb_id, "relation_type", form).await;
        }
    }

    // Classes created first and entities extracted afterwards is the normal case: sweep up the
    // ones waiting on types that already exist too
    match utopia_store::resolution::sweep_proposed_types(&state.pool, kb_id, None).await {
        Ok(swept) => {
            for (batch, n) in swept {
                moved_total += n;
                batches.push(batch);
            }
        }
        Err(e) => {
            tracing::warn!(%kb_id, error = %e, "sweeping up entities for existing types failed")
        }
    }

    if added_relations.is_empty() && added_classes.is_empty() && moved_total == 0 {
        return Ok(());
    }
    // actor is NULL: this is a system action, not anybody's decision. The ledger still shows what
    // was done, how many rows were changed, and the batch ids needed to undo it
    let _ = utopia_store::audit::record_opt(
        &state.pool,
        Some(kb_id),
        None,
        "ontology.bootstrapped",
        "kb",
        Some(kb_id),
        serde_json::json!({
            "relations": added_relations,
            "classes": added_classes,
            "facts_remapped": moved_total,
            // The ones where neither side of the signature matched and no predicate got attached
            // (#190). Without reporting this number, "remapped N facts" is reporting only the
            // good news
            "facts_left_off": left_off_total,
            "batches": batches,
        }),
    )
    .await;
    state.emit_review(kb_id);
    tracing::info!(
        %kb_id,
        relations = added_relations.len(),
        classes = added_classes.len(),
        facts = moved_total,
        "cold-start auto ontology extension complete"
    );
    Ok(())
}

fn str_of<'a>(v: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    v.get(key)
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}
