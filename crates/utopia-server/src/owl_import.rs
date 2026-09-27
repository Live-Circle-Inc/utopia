//! OWL import: faithful original → projection → preview → persist.
//!
//! **Preview and persist run the same plan**. Two independent code paths diverge sooner or later,
//! and the consequence of diverging is that what happens after the user clicks confirm is not what
//! they had just looked at -- which is worse than having no preview at all.
//!
//! Matching goes by **IRI**, not by key: one `rdfs:label` change upstream and the derived key
//! changes with it, so matching by key would build the same class over again as a new class and
//! leave all the entities on the orphan (see 0001 P2).

use std::collections::{BTreeMap, HashMap, HashSet};

use utopia_core::AppResult;
use utopia_ingest::ontology_rdf::{self, OwlProjection, RdfFormat};
use uuid::Uuid;

use crate::state::AppState;

/// Where a class/property ends up in this import.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    /// The ontology does not have this IRI → create
    Create,
    /// The IRI is already there → update label and description (key does not move: it may already
    /// be referenced)
    Update,
    /// The key is taken by another IRI → skip and report; no quiet rename and no overwrite
    KeyTaken,
    /// The key is taken by another IRI, but the alignment table says the two are **the same thing**
    /// → skip, and it does not count as a conflict.
    /// Kept apart from KeyTaken because it needs no human ruling: one duplicate class fewer is
    /// exactly the result we wanted
    Aligned,
}

/// Whether a property gets created in this import, and why.
///
/// **The preview has to be able to say why**. The previous version only reported "parsed 54
/// properties", from which the reader had no way to tell whether that meant "all of them will be
/// created" or "none of them will" -- and in fact it was the latter.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case", tag = "outcome", content = "detail")]
pub enum AttrNote {
    /// Will be created, with this datatype
    Datatype(&'static str),
    /// Will be created as text: no range was written, the vocabulary made no declaration, and text
    /// is the honest superset
    NoRange,
    /// Will be created as text: the range is a readable literal whose type we cannot express
    /// (`xsd:time` and the like).
    /// We report the original IRI so a human can change it to a more suitable type; but the values
    /// land first -- we do not throw knowledge away over a coarse type
    DegradedToText(String),
    /// Not created: the extractor could never read this kind of value out of prose (binary, XML
    /// fragments, XML-internal identifiers).
    /// Created, it would never be filled in, and would only add one more line of dead noise to
    /// every text chunk's prompt
    UnusableRange(String),
    /// Not created: no domain was written. An attribute has to hang off a class -- a hard
    /// constraint in the store layer
    NoDomain,

    /// Not created: the class the domain points at is **in this file, but was skipped** (most
    /// likely a key collision).
    /// Reported apart from UnknownDomain because the remedy differs: this one can be unblocked by
    /// renaming the existing class
    DomainSkipped(String),
    /// Not created: the domain points at a class that is not in this file at all (an external
    /// vocabulary)
    UnknownDomain(String),
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PlannedItem {
    pub iri: String,
    pub key: String,
    pub label: String,
    pub has_description: bool,
    pub disposition: Disposition,
    /// Relations only: the import declares it functional -- the preview must list these separately
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub functional: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conflict_with: Option<String>,
    /// Attributes only: what datatype it will be created with, or why it cannot be created
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attr: Option<AttrNote>,
}

/// The complete plan for one import. The preview returns it, and the persist step executes it.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ImportPlan {
    pub format: String,
    pub triples: usize,
    pub classes: Vec<PlannedItem>,
    pub relations: Vec<PlannedItem>,
    pub attributes: Vec<PlannedItem>,
    /// Axioms that showed up but that we do not consume today → how many times. **Not "skipped",
    /// but "not projected yet"**
    pub unprojected: Vec<(String, usize)>,
    /// How many classes have no `rdfs:comment`. Their quality in extraction is noticeably worse --
    /// the description goes into the prompt verbatim and is the model's only basis for deciding
    /// "what counts as this class"
    pub classes_without_description: usize,
    /// How many relations are declared functional. **This is the enterprise edition of the part_of
    /// trap**: the ontology declares uniqueness, the data does not obey, and the moment the import
    /// is done you have a queue of false conflicts
    pub functional_relations: usize,
}

/// Where a property ends up. **domain is judged first**: an attribute with no domain cannot be
/// created at all, and at that point what its range is no longer matters.
fn attr_note(
    p: &ontology_rdf::OwlProperty,
    vocab: &ontology_rdf::VocabDatatypes,
    resolvable: &HashSet<&str>,
    in_file: &HashSet<&str>,
) -> AttrNote {
    if p.domains.is_empty() {
        return AttrNote::NoDomain;
    }
    // **It only counts as a failure if not a single one resolves**: when some of several domains
    // are skipped, the attribute is still created, just attached to fewer classes -- better than
    // losing the whole thing, and the skipped classes each reported their key collision in the plan
    if !p.domains.iter().any(|d| resolvable.contains(d.as_str())) {
        let first = &p.domains[0];
        return if in_file.contains(first.as_str()) {
            AttrNote::DomainSkipped(first.clone())
        } else {
            AttrNote::UnknownDomain(first.clone())
        };
    }
    match ontology_rdf::map_range_of(p, vocab) {
        ontology_rdf::RangeMapping::Datatype(dt) => AttrNote::Datatype(dt),
        ontology_rdf::RangeMapping::Absent => AttrNote::NoRange,
        ontology_rdf::RangeMapping::Degraded(iri) => AttrNote::DegradedToText(iri),
        ontology_rdf::RangeMapping::Unusable(iri) => AttrNote::UnusableRange(iri),
    }
}

/// Whether a property gets created, and with what datatype. The persist step and the statistics
/// share it, so that "the preview said it would be created" and "it actually was created" are not
/// judged separately in two places and left to diverge.
fn attr_datatype(note: &AttrNote) -> Option<&'static str> {
    match note {
        AttrNote::Datatype(dt) => Some(dt),
        // No range written, or written but with a type we cannot express: the value is still a
        // literal, and text is the honest superset that stops nothing
        AttrNote::NoRange | AttrNote::DegradedToText(_) => Some("text"),
        _ => None,
    }
}

impl ImportPlan {
    /// Properties that cannot be created, counted by reason. **Reporting a total in the preview is
    /// not enough** -- "54 properties" does not tell you whether all of them or none of them will
    /// be created, and it actually depends on the reason.
    pub fn attr_skips(&self) -> BTreeMap<&str, usize> {
        let mut out: BTreeMap<&str, usize> = BTreeMap::new();
        for a in &self.attributes {
            if a.disposition == Disposition::KeyTaken {
                *out.entry("key_taken").or_default() += 1;
                continue;
            }
            let reason = match a.attr.as_ref() {
                Some(AttrNote::Datatype(_))
                | Some(AttrNote::NoRange)
                | Some(AttrNote::DegradedToText(_)) => continue,
                Some(AttrNote::UnusableRange(_)) => "unusable_range",
                Some(AttrNote::NoDomain) => "no_domain",
                Some(AttrNote::DomainSkipped(_)) => "domain_skipped",
                Some(AttrNote::UnknownDomain(_)) => "unknown_domain",
                None => continue,
            };
            *out.entry(reason).or_default() += 1;
        }
        out
    }
}

/// Parse the file and compute the plan against the existing ontology. Writes nothing.
pub async fn plan(
    state: &AppState,
    kb_id: Uuid,
    filename: &str,
    bytes: &[u8],
) -> AppResult<(ImportPlan, OwlProjection, RdfFormat)> {
    let format = RdfFormat::detect(filename, bytes);
    let proj = ontology_rdf::project(bytes, format).map_err(|e| {
        utopia_core::AppError::invalid_detail(
            "bad_ontology_file",
            "Could not parse this ontology file",
            e.to_string(),
        )
    })?;

    // The existing ontology: one index by IRI and one by key, so the two kinds of conflict are
    // judged separately
    let etypes = utopia_store::graph::entity_types(&state.pool, kb_id).await?;
    let rtypes = utopia_store::graph::relation_types(&state.pool, kb_id).await?;
    let e_by_iri: HashMap<&str, &_> = etypes
        .iter()
        .filter_map(|t| t.iri.as_deref().map(|i| (i, t)))
        .collect();
    let e_by_key: HashMap<&str, &_> = etypes.iter().map(|t| (t.key.as_str(), t)).collect();
    let r_by_iri: HashMap<&str, &_> = rtypes
        .iter()
        .filter_map(|t| t.iri.as_deref().map(|i| (i, t)))
        .collect();
    let r_by_key: HashMap<&str, &_> = rtypes.iter().map(|t| (t.key.as_str(), t)).collect();

    // Class IRIs whose id will be resolvable once this run is over: the ones created or updated in
    // this file, plus the ones already in the database with the same IRI.
    // Ones skipped over a key collision are **not among them** -- they will not be created, so the
    // attributes hanging off them have nowhere to hang either
    // Keys collide inside this one import too: different IRIs deriving the same short label
    // (FOAF's familyName and family_name both became family_name).
    // Checking against the database alone is not enough -- that way the second one shows "will be
    // created" in the preview, gets quietly dropped by ON CONFLICT on persist, and the preview has
    // told a lie
    //
    // **Two namespaces, not one**: classes go into entity_types, relations and attributes into
    // relation_types, each with its own (kb_id, key) unique constraint. Merging them into one table
    // would invent an extra constraint out of nothing -- and the cost is concrete: schema.org's
    // location / address are properties, but the names had already been taken by OMG Commons'
    // Location / Address, two **classes** (classes are processed first), so at extraction time the
    // model asks for location by name and the database happens not to have it
    let mut claimed_class: HashMap<&str, &str> = HashMap::new();
    let mut claimed_prop: HashMap<&str, &str> = HashMap::new();

    let mut classes = Vec::new();
    for c in &proj.classes {
        // When the alignment table rules "same name, different meaning", use the key it declares in
        // place of the derived one
        let mut renamed_key: Option<&'static str> = None;
        let (disposition, conflict_with) = if let Some(prev) = claimed_class.get(c.key.as_str()) {
            (Disposition::KeyTaken, Some((*prev).to_string()))
        } else if e_by_iri.contains_key(c.iri.as_str()) {
            (Disposition::Update, None)
        } else if let Some(existing) = e_by_key.get(c.key.as_str()) {
            match existing.iri.as_deref() {
                // **The squatter has no IRI: adopt it.**
                //
                // No IRI means this class was named locally (the seed ontology, or hand-built) and
                // is not a same-named term from another vocabulary. The import is saying "this IRI
                // is that class", and without this step the whole tree is broken: schema.org's
                // Organization collides with the built-in organization and gets skipped, so
                // Corporation's parent points at something that was never created -- the built-in
                // base classes end up with not a single subclass attached, and type refinement is
                // out of the question.
                None => (Disposition::Update, None),
                // There is already a **different** IRI: two vocabularies fighting over the same
                // short label. Check the prebuilt pack's alignment table first -- that is a
                // **declared** disposition and a re-import gives the same result, so the "no
                // automatic suffix" reasoning below does not apply to it
                Some(other) => match crate::pack_alignment::lookup(&c.iri, other) {
                    // Same meaning: the existing one is it, and one duplicate class fewer is
                    // exactly what we wanted
                    Some(crate::pack_alignment::Alignment::SameAs) => {
                        (Disposition::Aligned, existing.iri.clone())
                    }
                    // Same name, different meaning: create it under a different, declared key
                    Some(crate::pack_alignment::Alignment::Rename(k)) => {
                        renamed_key = Some(k);
                        (Disposition::Create, None)
                    }
                    // Not in the table: this is a real conflict. No automatic suffix -- that would
                    // leave a re-import unable to recognise which one it created last time
                    None => (Disposition::KeyTaken, existing.iri.clone()),
                },
            }
        } else {
            (Disposition::Create, None)
        };
        // The renamed key and the original are both borrows (the former 'static, the latter from
        // proj), so unify them into one reference and materialise it at the end
        let key: &str = renamed_key.unwrap_or(c.key.as_str());
        if !matches!(disposition, Disposition::KeyTaken | Disposition::Aligned) {
            claimed_class.insert(key, c.iri.as_str());
        }
        classes.push(PlannedItem {
            iri: c.iri.clone(),
            key: key.to_string(),
            label: c.label.clone(),
            has_description: !c.description.trim().is_empty(),
            disposition,
            functional: false,
            conflict_with,
            attr: None,
        });
    }

    // Classes that appeared in the file (including the skipped ones) -- used to tell "it was
    // skipped" apart from "it is not in this file at all"
    let in_file: HashSet<&str> = proj.classes.iter().map(|c| c.iri.as_str()).collect();
    let resolvable: HashSet<&str> = classes
        .iter()
        .filter(|c| c.disposition != Disposition::KeyTaken)
        .map(|c| c.iri.as_str())
        .chain(e_by_iri.keys().copied())
        .collect();

    let mut relations = Vec::new();
    let mut attributes = Vec::new();
    for p in &proj.properties {
        let (disposition, conflict_with) = if let Some(prev) = claimed_prop.get(p.key.as_str()) {
            (Disposition::KeyTaken, Some((*prev).to_string()))
        } else if r_by_iri.contains_key(p.iri.as_str()) {
            (Disposition::Update, None)
        } else if let Some(existing) = r_by_key.get(p.key.as_str()) {
            (Disposition::KeyTaken, existing.iri.clone())
        } else {
            (Disposition::Create, None)
        };
        if disposition != Disposition::KeyTaken {
            claimed_prop.insert(p.key.as_str(), p.iri.as_str());
        }
        let mut item = PlannedItem {
            iri: p.iri.clone(),
            key: p.key.clone(),
            label: p.label.clone(),
            has_description: !p.description.trim().is_empty(),
            disposition,
            functional: p.functional,
            conflict_with,
            attr: None,
        };
        if p.is_datatype {
            item.attr = Some(attr_note(p, &proj.vocab_datatypes, &resolvable, &in_file));
            attributes.push(item);
        } else {
            relations.push(item);
        }
    }

    let mut unprojected: Vec<(String, usize)> = proj
        .unprojected
        .iter()
        .map(|(k, v)| (k.clone(), *v))
        .collect();
    unprojected.sort_by_key(|(_, n)| std::cmp::Reverse(*n));

    let plan = ImportPlan {
        format: format!("{format:?}").to_lowercase(),
        triples: proj.triples,
        classes_without_description: classes.iter().filter(|c| !c.has_description).count(),
        functional_relations: relations.iter().filter(|r| r.functional).count(),
        classes,
        relations,
        attributes,
        unprojected,
    };
    Ok((plan, proj, format))
}

/// A second pass once a set of packs has been installed: hook up the relations whose domain / range
/// point at **classes in another pack**.
///
/// A single import knows the classes in its own file and the classes already in the database (see
/// the resolve closure in [`apply`]), but packs are installed one at a time: when W3C Org is
/// installed FOAF has not arrived yet, so `headOf`'s `rdfs:domain foaf:Agent` came up empty; and
/// once FOAF is installed, nobody goes back to fill it in. `judge_direction` does not judge the
/// direction of a predicate with no domain, so a reversed `Project Aurora head_of Li Ting` goes
/// into the graph as-is (#222).
///
/// It only adds, never deletes; the association tables are ON CONFLICT DO NOTHING, so running it
/// again is harmless. Attributes are not in here: an attribute's fate was settled during the
/// planning phase (one with no domain cannot be created at all), and filling in a domain afterwards
/// cannot change the fact that it either is or is not already a column
pub async fn relink_domains_ranges(
    state: &AppState,
    kb_id: Uuid,
    filename: &str,
    bytes: &[u8],
) -> AppResult<(usize, usize)> {
    let format = RdfFormat::detect(filename, bytes);
    let proj = ontology_rdf::project(bytes, format).map_err(|e| {
        utopia_core::AppError::invalid_detail(
            "bad_ontology_file",
            "Could not parse this ontology file",
            e.to_string(),
        )
    })?;
    let classes: HashMap<String, Uuid> = utopia_store::graph::entity_types(&state.pool, kb_id)
        .await?
        .into_iter()
        .filter_map(|t| t.iri.clone().map(|i| (i, t.id)))
        .collect();
    let relations: HashMap<String, Uuid> = utopia_store::graph::relation_types(&state.pool, kb_id)
        .await?
        .into_iter()
        .filter_map(|r| r.iri.clone().map(|i| (i, r.id)))
        .collect();
    let mut link_d: Vec<(Uuid, Uuid)> = Vec::new();
    let mut link_r: Vec<(Uuid, Uuid)> = Vec::new();
    for p in &proj.properties {
        if p.is_datatype {
            continue;
        }
        let Some(&rid) = relations.get(&p.iri) else {
            continue;
        };
        link_d.extend(
            p.domains
                .iter()
                .filter_map(|d| classes.get(d).map(|&t| (rid, t))),
        );
        link_r.extend(
            p.ranges
                .iter()
                .filter_map(|r| classes.get(r).map(|&t| (rid, t))),
        );
    }
    utopia_store::ontology::link_domains_ranges_bulk(&state.pool, &link_d, &link_r).await?;
    Ok((link_d.len(), link_r.len()))
}

/// Execute the plan. Attributes are persisted after the classes -- they have to hang off a domain,
/// and the domain has to wait for the classes to be built first and the IRI → id resolution done
/// (that is the `id_of` below).
pub async fn apply(
    state: &AppState,
    kb_id: Uuid,
    actor: Uuid,
    filename: &str,
    bytes: &[u8],
) -> AppResult<(Uuid, ImportPlan)> {
    let (plan, proj, format) = plan(state, kb_id, filename, bytes).await?;

    // Layer one: the original goes into the blob store content-addressed. **Store the original
    // before touching the ontology** -- a failed projection can be redone, but a lost original is
    // gone forever
    let sha = sha256_hex(bytes);
    state
        .blob
        .put(&sha, bytes)
        .await
        .map_err(utopia_core::AppError::Other)?;

    let by_iri: HashMap<&str, &_> = proj.classes.iter().map(|c| (c.iri.as_str(), c)).collect();
    // Classes already in the database: IRI → id. Aligned relies on it to hook "the synonymous one"
    // up to the parent-class references
    let existing_by_iri: HashMap<String, Uuid> =
        utopia_store::graph::entity_types(&state.pool, kb_id)
            .await?
            .into_iter()
            .filter_map(|t| t.iri.clone().map(|i| (i, t.id)))
            .collect();
    let mut created_classes = 0usize;
    let mut updated_classes = 0usize;
    // IRI → id in the ontology, needed for resolving parents
    let mut id_of: HashMap<String, Uuid> = HashMap::new();

    // **Newly created classes are inserted in one go, not row by row.**
    //
    // Row-by-row `execute(pool)` commits each row on its own, and at schema.org's scale (968
    // classes plus about 1500 properties) that is five thousand fsyncs -- 45 seconds for the
    // import, measured. The same rows in a single UNNEST statement are 536 milliseconds. Update and
    // Aligned stay row by row below: on an empty database they are single digits, and each of them
    // has to read the current state, so they cannot be batched.
    let new_classes: Vec<(String, String, String, String)> = plan
        .classes
        .iter()
        .filter(|i| i.disposition == Disposition::Create)
        .filter_map(|i| by_iri.get(i.iri.as_str()).map(|c| (i, *c)))
        // Use the item's key: when the alignment table rules same-name-different-meaning, that is
        // the renamed one
        .map(|(i, c)| {
            (
                i.key.clone(),
                c.label.clone(),
                c.description.clone(),
                c.iri.clone(),
            )
        })
        .collect();
    let created_ids =
        utopia_store::ontology::create_entity_types_bulk(&state.pool, kb_id, &new_classes).await?;
    for (key, label, _, iri) in &new_classes {
        let _ = label;
        if let Some(id) = created_ids.get(key) {
            id_of.insert(iri.clone(), *id);
            created_classes += 1;
        }
    }

    for item in &plan.classes {
        let Some(c) = by_iri.get(item.iri.as_str()) else {
            continue;
        };
        match item.disposition {
            // Already created in bulk above
            Disposition::Create => {}
            Disposition::Update => {
                // Two kinds of Update: this IRI was imported last time (findable by IRI), or it is
                // adopting a same-named local class (found by key, with that row having no IRI yet)
                let updated = utopia_store::ontology::update_type_from_import(
                    &state.pool,
                    kb_id,
                    &c.iri,
                    &c.label,
                    &c.description,
                )
                .await?;
                let id = match updated {
                    Some(id) => Some(id),
                    None => {
                        utopia_store::ontology::adopt_iri_onto_key(
                            &state.pool,
                            kb_id,
                            &c.key,
                            &c.iri,
                        )
                        .await?
                    }
                };
                if let Some(id) = id {
                    id_of.insert(c.iri.clone(), id);
                    updated_classes += 1;
                }
            }
            // Key taken: already reported, leave it alone
            // Same meaning: do not create it. But the IRI has to point at the existing class's id,
            // otherwise subclasses that have it as a parent will not resolve and the whole tree
            // breaks right here
            Disposition::Aligned => {
                if let Some(target) = item.conflict_with.as_deref() {
                    if let Some(id) = existing_by_iri.get(target) {
                        id_of.insert(c.iri.clone(), *id);
                    }
                }
            }
            Disposition::KeyTaken => {}
        }
    }

    // Second pass for resolving parents: on the first pass the parent may not have been created yet
    // Collect the (child, parent) edges first, then insert them in one go
    let mut parent_edges: Vec<(Uuid, Uuid)> = Vec::new();
    for c in &proj.classes {
        let Some(&child) = id_of.get(&c.iri) else {
            continue;
        };
        // **All the parents**, not just the first one any more. FOAF's Person is both an Agent and
        // a SpatialThing, and dropping the latter branch makes properties whose domain is on that
        // branch fail the check. Parents pointing at classes that were not created drop out on
        // their own -- one branch short beats not being attached at all
        let parents: Vec<Uuid> = c
            .parents
            .iter()
            .filter_map(|iri| id_of.get(iri).copied())
            .collect();
        parent_edges.extend(parents.into_iter().map(|p| (child, p)));
    }
    // Inserted in one go. The single-row version runs a recursive CTE for cycle detection every
    // time, and schema.org has 975 parent edges -- that is 975 recursive queries plus 975 commits.
    //
    // **The bulk version does not check for cycles.** The single-row version only ever handled a
    // cycle by skipping that one edge (`let _ =`) anyway, without aborting the import; and once the
    // import is done the ontology page can still find it and let someone deal with it. Trading nine
    // hundred-odd recursive queries for "finding out a bit sooner" is not worth it
    utopia_store::ontology::set_parents_bulk(&state.pool, &parent_edges).await?;

    // Class disjointness is collected into a batch the same way (see the axiom columns on
    // `relation_types`). Ones pointing at classes that were not created drop out on their own --
    // both ends of a disjointness declaration have to be in this KB before it can be used to judge
    // a contradiction
    let mut disjoint_edges: Vec<(Uuid, Uuid)> = Vec::new();
    for c in &proj.classes {
        let Some(&a) = id_of.get(&c.iri) else {
            continue;
        };
        for other in &c.disjoint_with {
            if let Some(&b) = id_of.get(other) {
                disjoint_edges.push((a, b));
            }
        }
    }
    utopia_store::ontology::set_disjoint_bulk(&state.pool, kb_id, &disjoint_edges).await?;

    let by_prop_iri: HashMap<&str, &_> = proj
        .properties
        .iter()
        .map(|p| (p.iri.as_str(), p))
        .collect();
    // Relations. apply used to skip them entirely -- while the preview wrote "N new" on the
    // relations line, promising something that was never going to happen. The same kind of defect
    // as the key collision fixed today.
    //
    // **functional / inverse_functional are written exactly as the vocabulary declares them**: they
    // are what the temporal engine uses to close facts automatically, and guessing wrong
    // manufactures false conflicts in bulk (59 of them, that time with part_of). So we do not guess
    // -- if the vocabulary says so, it is so, and the preview already lists them separately for a
    // human to look over.
    let mut created_rels = 0usize;
    let mut updated_rels = 0usize;
    let mut new_rels: Vec<utopia_store::ontology::BulkRelation> = Vec::new();
    // (key, domains, ranges): relation ids only exist once the bulk insert is done, so keep track
    // by key for now
    let mut pending_links: Vec<(String, Vec<Uuid>, Vec<Uuid>)> = Vec::new();
    for item in &plan.relations {
        if item.disposition == Disposition::KeyTaken {
            continue;
        }
        let Some(p) = by_prop_iri.get(item.iri.as_str()) else {
            continue;
        };
        // When a domain/range points at a class that was not created, only that one is dropped, not
        // the whole relation: unlike an attribute, a relation does not have to hang off a class,
        // and no domain simply means "no restriction on the subject's type".
        //
        // **Classes already in the database count too.** This used to recognise only the classes in
        // the file at hand, so W3C Org's `headOf rdfs:domain foaf:Agent` lost its domain even in a
        // database where FOAF was already installed -- and `judge_direction` does not judge the
        // direction of a predicate with no domain at all (#222)
        let resolve = |iris: &[String]| -> Vec<Uuid> {
            iris.iter()
                .filter_map(|i| {
                    id_of
                        .get(i)
                        .copied()
                        .or_else(|| existing_by_iri.get(i).copied())
                })
                .collect()
        };
        let domains = resolve(&p.domains);
        let ranges = resolve(&p.ranges);

        if item.disposition == Disposition::Update {
            if utopia_store::ontology::update_relation_from_import(
                &state.pool,
                kb_id,
                &p.iri,
                &p.label,
                &p.description,
                &domains,
                &ranges,
            )
            .await?
            {
                updated_rels += 1;
            }
            continue;
        }
        // Collect the new ones and insert them in one go -- same reasoning as the classes above
        new_rels.push(utopia_store::ontology::BulkRelation {
            key: p.key.clone(),
            label: p.label.clone(),
            description: p.description.clone(),
            iri: p.iri.clone(),
            kind: "relation",
            datatype: None,
            functional: p.functional,
            inverse_functional: p.inverse_functional,
            transitive: p.transitive,
            symmetric: p.symmetric,
            asymmetric: p.asymmetric,
            irreflexive: p.irreflexive,
        });
        pending_links.push((p.key.clone(), domains, ranges));
    }
    let rel_ids =
        utopia_store::ontology::create_relation_types_bulk(&state.pool, kb_id, &new_rels).await?;
    created_rels += rel_ids.len();

    // Inverse and super-property point at **another relation type**, and the ids only exist once
    // everything has been inserted -- hence the second pass. A single pass can only handle files
    // where "the super-property happens to come first", and RDF triples have no order.
    //
    // Both the new and the already existing ones are collected: when an ontology is re-imported,
    // these two declarations may only have been added this time round
    let mut inv_pairs: Vec<(String, String)> = Vec::new();
    let mut sub_pairs: Vec<(String, String)> = Vec::new();
    for item in &plan.relations {
        let Some(p) = by_prop_iri.get(item.iri.as_str()) else {
            continue;
        };
        if let Some(o) = &p.inverse_of {
            inv_pairs.push((p.iri.clone(), o.clone()));
        }
        if let Some(o) = &p.sub_property_of {
            sub_pairs.push((p.iri.clone(), o.clone()));
        }
    }
    let (linked_inv, linked_sub) = utopia_store::ontology::link_property_axioms_bulk(
        &state.pool,
        kb_id,
        &inv_pairs,
        &sub_pairs,
    )
    .await?;
    // The association rows are flattened too: the single-row version runs 4 statements per relation
    // (a DELETE and an INSERT on each of the two tables), so fifteen hundred relations is six
    // thousand commits
    let mut link_d: Vec<(Uuid, Uuid)> = Vec::new();
    let mut link_r: Vec<(Uuid, Uuid)> = Vec::new();
    for (key, domains, ranges) in &pending_links {
        let Some(rid) = rel_ids.get(key) else {
            continue;
        };
        link_d.extend(domains.iter().map(|d| (*rid, *d)));
        link_r.extend(ranges.iter().map(|r| (*rid, *r)));
    }
    utopia_store::ontology::link_domains_ranges_bulk(&state.pool, &link_d, &link_r).await?;

    // Attributes: their turn comes only after the classes are created and id_of is filled in. The
    // plan has already worked out where each property ends up, and **this only executes, it does
    // not judge again** -- judging once in each of two places is how they diverge, and diverging
    // means what the preview says and what actually happens are not the same thing
    let mut created_attrs = 0usize;
    let mut new_attrs: Vec<utopia_store::ontology::BulkRelation> = Vec::new();
    let mut pending_attr_domains: Vec<(String, Vec<Uuid>)> = Vec::new();
    for item in &plan.attributes {
        if item.disposition == Disposition::KeyTaken {
            continue;
        }
        let (Some(note), Some(p)) = (item.attr.as_ref(), by_prop_iri.get(item.iri.as_str())) else {
            continue;
        };
        let Some(dt) = attr_datatype(note) else {
            continue;
        };
        // Ones judged resolvable during planning but with no id at persist time (the class was
        // skipped, or the update failed) drop out on their own; if all of them drop out we do not
        // create it, rather than manufacturing an attribute that hangs off nothing
        let domain_ids: Vec<Uuid> = p
            .domains
            .iter()
            .filter_map(|iri| id_of.get(iri).copied())
            .collect();
        if domain_ids.is_empty() {
            continue;
        }
        new_attrs.push(utopia_store::ontology::BulkRelation {
            key: p.key.clone(),
            label: p.label.clone(),
            description: p.description.clone(),
            iri: p.iri.clone(),
            kind: "attribute",
            datatype: Some(dt.to_string()),
            // Attributes take part in neither temporal closing nor the consistency check: those
            // judgements are all about the edges **between entities**, whereas an attribute's
            // object is a literal value
            functional: false,
            inverse_functional: false,
            transitive: false,
            symmetric: false,
            asymmetric: false,
            irreflexive: false,
        });
        pending_attr_domains.push((p.key.clone(), domain_ids));
    }
    let attr_ids =
        utopia_store::ontology::create_relation_types_bulk(&state.pool, kb_id, &new_attrs).await?;
    created_attrs += attr_ids.len();
    let mut attr_links: Vec<(Uuid, Uuid)> = Vec::new();
    for (key, domains) in &pending_attr_domains {
        let Some(aid) = attr_ids.get(key) else {
            continue;
        };
        attr_links.extend(domains.iter().map(|d| (*aid, *d)));
    }
    utopia_store::ontology::link_domains_ranges_bulk(&state.pool, &attr_links, &[]).await?;

    let summary = serde_json::json!({
        "classes_created": created_classes,
        "classes_updated": updated_classes,
        "classes_key_taken": plan.classes.iter().filter(|c| c.disposition == Disposition::KeyTaken).count(),
        "relations_seen": plan.relations.len(),
        // How many inverse / super-property links were made. **Report it** -- when the target IRI
        // is not in this KB it is skipped silently (referencing external vocabularies is the norm),
        // and without a number nobody knows how many links are missing
        "inverse_linked": linked_inv,
        "sub_property_linked": linked_sub,
        "relations_created": created_rels,
        "relations_updated": updated_rels,
        "attributes_seen": plan.attributes.len(),
        "attributes_created": created_attrs,
        "attributes_skipped": plan.attr_skips(),
        "classes_without_description": plan.classes_without_description,
        "functional_relations": plan.functional_relations,
        "unprojected": plan.unprojected.iter().take(30).collect::<Vec<_>>(),
        "triples": plan.triples,
    });
    let import_id = utopia_store::ontology::record_import(
        &state.pool,
        kb_id,
        &sha,
        filename,
        &format!("{format:?}").to_lowercase(),
        bytes.len() as i64,
        &summary,
        actor,
    )
    .await?;

    let _ = utopia_store::audit::record(
        &state.pool,
        Some(kb_id),
        actor,
        "ontology.imported",
        "kb",
        Some(kb_id),
        summary,
    )
    .await;
    // The ontology just changed, so all the vectors are stale. **Enqueue a job rather than running
    // it inline**: this pass has to embed a few thousand rows, and doing it inside the import
    // request would make the user wait another six to eight minutes after clicking confirm. It not
    // getting picked up does not matter either -- the index is self-healing, and the next person
    // who uses retrieval will fill it in
    let _ = utopia_store::jobs::enqueue(
        &state.pool,
        "embed_ontology",
        serde_json::json!({ "kb_id": kb_id }),
    )
    .await;

    Ok((import_id, plan))
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}
