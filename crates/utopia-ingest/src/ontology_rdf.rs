//! Parsing and **projection** of OWL / RDFS files.
//!
//! The split of work follows the three layers set out in 0001: this module only does layer two
//! (projection), while layer one (the source text going into a blob verbatim) is the caller's job.
//! **Anything we cannot make sense of here is not an error, it is "not projected yet"** -- the
//! source text is kept, and once a consumer is added it can simply be re-run.
//!
//! Parse only, never reason: those two little Oxigraph parsers hand us triples and we pick out the
//! usable ones by list. No horned-owl -- the reasoner is 0002's business, and it reads the source
//! text, not the projection.

use std::collections::{BTreeMap, BTreeSet};

/// One projected class.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OwlClass {
    pub iri: String,
    /// Short label derived from the IRI (the token the model reads and writes); de-duplicating
    /// it with a suffix is the caller's job
    pub key: String,
    pub label: String,
    /// `rdfs:comment` -- a load-bearing field, goes into the extraction prompt verbatim
    pub description: String,
    /// Every parent-class IRI from `rdfs:subClassOf` (multiple inheritance is the norm here)
    pub parents: Vec<String>,
    /// The far-end IRI of `owl:disjointWith`. **Both directions are collected** -- the axiom is
    /// symmetric, while vocabularies usually only write it once
    pub disjoint_with: Vec<String>,
}

/// One projected property (object property → relation, data property → attribute).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OwlProperty {
    pub iri: String,
    pub key: String,
    pub label: String,
    pub description: String,
    /// true = takes the attribute channel (literal values), false = takes the relation channel.
    /// Two sources: an explicit `owl:DatatypeProperty`, or a range that is all datatypes
    pub is_datatype: bool,
    pub functional: bool,
    pub inverse_functional: bool,
    /// OWL property axioms. What the consistency check (0002 R0) decides on: without them there
    /// is no way to tell whether `A part_of B` and `B part_of A` existing at the same time is a
    /// contradiction or perfectly normal
    pub transitive: bool,
    pub symmetric: bool,
    pub asymmetric: bool,
    pub irreflexive: bool,
    /// The far-end IRI of `owl:inverseOf`. **Only collected in the direction it was written** --
    /// normalisation (filling in the reverse) happens when the axioms are read
    /// (`reasoning::axioms`), where there is no way around it; filling it in here would make
    /// "what the ontology itself wrote" and "what we inferred" impossible to tell apart
    pub inverse_of: Option<String>,
    /// The parent-property IRI from `rdfs:subPropertyOf`. If several are written only the first
    /// is kept -- OWL allows multiple parents, but R1's rule only climbs one level at a time, and
    /// multiple parents need a different shape entirely
    pub sub_property_of: Option<String>,
    pub domains: Vec<String>,
    pub ranges: Vec<String>,
    /// **Whether several ranges are a union or an intersection**. Several `rdfs:range` lines are
    /// an intersection ("must be both at once"), while several `schema:rangeIncludes` lines are a
    /// union ("either one will do"). Pour them into the same Vec without recording this bit and
    /// `author rangeIncludes Organization, Person` gets read as
    /// "must be both an organisation and a person", then degraded to text -- and an edge is gone
    pub ranges_union: bool,
}

/// The result of one parse. `unprojected` is a **report**, not an error -- see the module docs.
#[derive(Debug, Default)]
pub struct OwlProjection {
    pub classes: Vec<OwlClass>,
    pub properties: Vec<OwlProperty>,
    /// Predicates that turned up but that we do not consume today → count. For the "not
    /// projected yet" column on the preview page
    pub unprojected: BTreeMap<String, usize>,
    /// Total number of triples, so a human has a number for "how big is this file"
    pub triples: usize,
    /// **The IRIs this file itself declares to be datatypes** → one of our four.
    /// schema.org declares Text/Number/Date... as `a rdfs:Class, schema:DataType`, and looking
    /// only at `rdfs:Class` builds them as entity classes (`text` and `boolean` become entity
    /// types)
    pub vocab_datatypes: VocabDatatypes,
}

/// Datatype IRIs the vocabulary declares itself → our four (`text`/`number`/`date`/`bool`).
pub type VocabDatatypes = BTreeMap<String, &'static str>;

const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const RDFS: &str = "http://www.w3.org/2000/01/rdf-schema#";
const OWL: &str = "http://www.w3.org/2002/07/owl#";

/// schema.org's own take on domain/range. **Not standard vocabulary**, hence hardcoded here, but
/// worth recognising: schema.org and the vocabularies derived from it do not have one single
/// `rdfs:domain`, so not recognising these two predicates amounts to pretending its whole type
/// system is not there -- 1600-odd properties would all turn into unconstrained relations.
/// Both schemes are collected: old and new releases of the same vocabulary use https and http.
const SCHEMA_NS: [&str; 2] = ["https://schema.org/", "http://schema.org/"];

/// Whether a predicate/class is the one under schema.org with that name.
fn is_schema(iri: &str, local: &str) -> bool {
    SCHEMA_NS.iter().any(|ns| {
        iri.len() == ns.len() + local.len() && iri.starts_with(ns) && iri.ends_with(local)
    })
}

/// The input formats we support. v1 does only these two -- the vast majority of Protégé exports
/// are one of them, and OWL/XML and Manchester syntax are deliberately cut (see "cut from v1" in
/// 0001 P2).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RdfFormat {
    Turtle,
    RdfXml,
}

impl RdfFormat {
    /// The extension is a strong signal; the content only overrides it on an **outright
    /// contradiction**.
    ///
    /// It used to be the other way round (`.rdf` fell back to Turtle when it could not detect an
    /// XML marker), and the result was that FOAF's official file opens with dozens of lines of
    /// `<!--` comments, putting `<rdf:` outside the sniffing window, so it went into the parser as
    /// Turtle and reported "Invalid IRI code point" on the very first line.
    pub fn detect(filename: &str, bytes: &[u8]) -> Self {
        let lower = filename.to_ascii_lowercase();
        // Turtle's fingerprint is hard: only it starts with @prefix / @base / PREFIX
        let looks_turtle = {
            let head = &bytes[..bytes.len().min(4096)];
            let s = String::from_utf8_lossy(head);
            s.lines()
                .map(str::trim_start)
                .find(|l| !l.is_empty() && !l.starts_with('#'))
                .is_some_and(|l| {
                    l.starts_with("@prefix")
                        || l.starts_with("@base")
                        || l.starts_with("PREFIX")
                        || l.starts_with("BASE")
                })
        };
        if lower.ends_with(".ttl") || lower.ends_with(".turtle") || lower.ends_with(".n3") {
            return Self::Turtle;
        }
        if lower.ends_with(".rdf") || lower.ends_with(".owl") || lower.ends_with(".xml") {
            // Both encodings are common for .owl, so the content decides -- but only a genuine
            // "does look like Turtle" overrides it
            return if looks_turtle {
                Self::Turtle
            } else {
                Self::RdfXml
            };
        }
        if looks_turtle {
            Self::Turtle
        } else {
            Self::RdfXml
        }
    }
}

/// The intermediate shape of a triple: only the parts we can make sense of are kept.
struct Triple {
    subject: String,
    predicate: String,
    /// Some when the object is an IRI
    object_iri: Option<String>,
    /// Some((value, language tag)) when the object is a literal
    object_lit: Option<(String, Option<String>)>,
}

fn read_triples(bytes: &[u8], format: RdfFormat) -> anyhow::Result<Vec<Triple>> {
    use oxrdf::Term;
    let mut out = Vec::new();
    let mut push = |t: oxrdf::Triple| {
        let subject = match &t.subject {
            oxrdf::NamedOrBlankNode::NamedNode(n) => n.as_str().to_string(),
            // Blank nodes are anonymous class expressions (owl:Restriction and the like) -- the
            // projection does not touch them, they stay in the source text for the reasoner
            oxrdf::NamedOrBlankNode::BlankNode(_) => return,
        };
        let (object_iri, object_lit) = match &t.object {
            Term::NamedNode(n) => (Some(n.as_str().to_string()), None),
            Term::Literal(l) => (
                None,
                Some((
                    l.value().to_string(),
                    l.language().map(|s| s.to_ascii_lowercase()),
                )),
            ),
            _ => (None, None),
        };
        out.push(Triple {
            subject,
            predicate: t.predicate.as_str().to_string(),
            object_iri,
            object_lit,
        });
    };
    // **Relative IRIs need a base before they can be resolved.** Reading from bytes there is no
    // document URL, and the Turtle spec says base defaults to the document's own address. So give
    // it a placeholder: relative IRIs in an ontology file are almost always document-level
    // metadata (PROV-O's `<#> a owl:Ontology`), we do not consume owl:Ontology nodes, and getting
    // through the parse is all that matters. Without one the whole file is wiped out on the first
    // `<#>` with "No scheme found in an absolute IRI"
    const BASE: &str = "urn:utopia:import";
    match format {
        RdfFormat::Turtle => {
            let p = oxttl::TurtleParser::new()
                .with_base_iri(BASE)
                .map_err(|e| anyhow::anyhow!("invalid base IRI: {e}"))?;
            for r in p.for_reader(bytes) {
                push(r?);
            }
        }
        RdfFormat::RdfXml => {
            let p = oxrdfxml::RdfXmlParser::new()
                .with_base_iri(BASE)
                .map_err(|e| anyhow::anyhow!("invalid base IRI: {e}"))?;
            for r in p.for_reader(bytes) {
                push(r?);
            }
        }
    }
    Ok(out)
}

/// Parse and project. Language tags prefer `@en`/`@zh`, then untagged, then whichever comes.
pub fn project(bytes: &[u8], format: RdfFormat) -> anyhow::Result<OwlProjection> {
    let triples = read_triples(bytes, format)?;
    let mut proj = OwlProjection {
        triples: triples.len(),
        ..Default::default()
    };

    // First classify: who is a class, who is an object property, who is a data property, who
    // carries a functionality marker
    let mut classes: BTreeSet<String> = BTreeSet::new();
    let mut obj_props: BTreeSet<String> = BTreeSet::new();
    let mut data_props: BTreeSet<String> = BTreeSet::new();
    let mut functional: BTreeSet<String> = BTreeSet::new();
    let mut inverse_functional: BTreeSet<String> = BTreeSet::new();
    let mut transitive: BTreeSet<String> = BTreeSet::new();
    let mut symmetric: BTreeSet<String> = BTreeSet::new();
    // The two relations between properties (not type declarations, so collected separately)
    let mut inverse_of: BTreeMap<String, String> = BTreeMap::new();
    let mut sub_property_of: BTreeMap<String, String> = BTreeMap::new();
    let mut asymmetric: BTreeSet<String> = BTreeSet::new();
    let mut irreflexive: BTreeSet<String> = BTreeSet::new();
    let mut plain_props: BTreeSet<String> = BTreeSet::new();
    let mut datatype_roots: BTreeSet<String> = BTreeSet::new();
    for t in &triples {
        if t.predicate != RDF_TYPE {
            continue;
        }
        let Some(o) = t.object_iri.as_deref() else {
            continue;
        };
        match o {
            x if x == format!("{OWL}Class") || x == format!("{RDFS}Class") => {
                classes.insert(t.subject.clone());
            }
            x if x == format!("{OWL}ObjectProperty") => {
                obj_props.insert(t.subject.clone());
            }
            x if x == format!("{OWL}DatatypeProperty") => {
                data_props.insert(t.subject.clone());
            }
            // rdf:Property **does not say** whether it is object or data. Stored apart from an
            // explicit owl:ObjectProperty: that one "said so" and must not be re-judged by range;
            // this one "did not say", so below the channel is decided by range, and only when
            // there is no range either does it fall back to being a relation
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#Property" => {
                plain_props.insert(t.subject.clone());
            }
            x if x == format!("{OWL}FunctionalProperty") => {
                functional.insert(t.subject.clone());
            }
            x if x == format!("{OWL}InverseFunctionalProperty") => {
                inverse_functional.insert(t.subject.clone());
            }
            // Property axioms: only with them can R0's consistency check decide anything at all.
            //
            // **The five bundled vocabularies only cover half of these** (14 TransitiveProperty,
            // 1 SymmetricProperty, and not a single Asymmetric or Irreflexive), but those five
            // are only the base for a cold start. The end of this road is importing **the
            // enterprise's own ontology** (0001's opening: FIBO, industry standards, hand-built
            // in Protégé), and in those, asymmetry and irreflexivity are common declarations.
            // Collect everything OWL has, not just what the few packs in hand happen to use.
            x if x == format!("{OWL}TransitiveProperty") => {
                transitive.insert(t.subject.clone());
            }
            x if x == format!("{OWL}SymmetricProperty") => {
                symmetric.insert(t.subject.clone());
            }
            x if x == format!("{OWL}AsymmetricProperty") => {
                asymmetric.insert(t.subject.clone());
            }
            x if x == format!("{OWL}IrreflexiveProperty") => {
                irreflexive.insert(t.subject.clone());
            }
            // Datatypes the vocabulary declares about itself. Only the explicitly declared roots
            // are collected here; subclasses (Integer ⊂ Number, URL ⊂ Text) are propagated down
            // once the parent graph is complete. **The marker class itself is collected too**:
            // schema:DataType is declared as `a rdfs:Class`, and not collecting it leaves behind
            // an entity type called data_type; it also makes the
            // `rdfs:subClassOf schema:DataType` spelling land inside the closure
            x if is_schema(x, "DataType") => {
                datatype_roots.insert(t.subject.clone());
                datatype_roots.insert(x.to_string());
            }
            _ => {}
        }
    }

    // Then collect labels, comments, parents, domain/range
    let mut labels: BTreeMap<String, Vec<(String, Option<String>)>> = BTreeMap::new();
    let mut comments: BTreeMap<String, Vec<(String, Option<String>)>> = BTreeMap::new();
    let mut parents: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut disjoint: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut domains: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut ranges: BTreeMap<String, Vec<String>> = BTreeMap::new();
    // Properties that used rangeIncludes; their range is read as a union
    let mut union_ranged: BTreeSet<String> = BTreeSet::new();
    let known = |p: &str| {
        p == RDF_TYPE
            || p == format!("{RDFS}label")
            || p == format!("{RDFS}comment")
            || p == format!("{RDFS}subClassOf")
            || p == format!("{RDFS}subPropertyOf")
            || p == format!("{RDFS}domain")
            || p == format!("{RDFS}range")
            || is_schema(p, "domainIncludes")
            || is_schema(p, "rangeIncludes")
            || p == format!("{OWL}disjointWith")
            || p == format!("{OWL}inverseOf")
    };
    for t in &triples {
        let p = t.predicate.as_str();
        if p == format!("{RDFS}label") {
            if let Some(l) = &t.object_lit {
                labels.entry(t.subject.clone()).or_default().push(l.clone());
            }
        } else if p == format!("{RDFS}comment") {
            if let Some(l) = &t.object_lit {
                comments
                    .entry(t.subject.clone())
                    .or_default()
                    .push(l.clone());
            }
        } else if p == format!("{OWL}disjointWith") {
            // **Recorded in both directions.** `owl:disjointWith` is symmetric, while
            // vocabularies usually write it once (W3C Org makes Role/Membership/Site/ChangeEvent
            // pairwise disjoint in only six lines). Store only the written direction and asking
            // "are A and B disjoint" comes down to which end the caller happens to ask from
            if let Some(o) = &t.object_iri {
                disjoint
                    .entry(t.subject.clone())
                    .or_default()
                    .push(o.clone());
                disjoint
                    .entry(o.clone())
                    .or_default()
                    .push(t.subject.clone());
            }
        } else if p == format!("{OWL}inverseOf") {
            // **Only collected in the direction it was written.** In OWL `p owl:inverseOf q`
            // implies the reverse, and vocabularies usually only write it once. Filling in the
            // reverse happens when the axioms are read (`reasoning::axioms`, where there is no
            // way around it) -- filling it in here would make "what the ontology itself wrote"
            // and "what we inferred" impossible to tell apart, and R0 has an
            // `inverse_not_mutual` check whose whole job is to tell those two apart
            if let Some(o) = &t.object_iri {
                inverse_of.entry(t.subject.clone()).or_insert(o.clone());
            }
        } else if p == format!("{RDFS}subPropertyOf") {
            // This used to appear only in the `known()` allowlist -- recognised, no warning,
            // **and then thrown away**. Import an ontology carrying subPropertyOf and that part
            // of the information vanished on the spot, with nothing said about it
            if let Some(o) = &t.object_iri {
                sub_property_of
                    .entry(t.subject.clone())
                    .or_insert(o.clone());
            }
        } else if p == format!("{RDFS}subClassOf") {
            if let Some(o) = &t.object_iri {
                parents
                    .entry(t.subject.clone())
                    .or_default()
                    .push(o.clone());
            }
        } else if p == format!("{RDFS}domain") {
            if let Some(o) = &t.object_iri {
                domains
                    .entry(t.subject.clone())
                    .or_default()
                    .push(o.clone());
            }
        } else if p == format!("{RDFS}range") {
            if let Some(o) = &t.object_iri {
                ranges.entry(t.subject.clone()).or_default().push(o.clone());
            }
        } else if is_schema(p, "domainIncludes") {
            // domainIncludes is a union ("may be used on these types"), and our domain list is
            // union semantics to begin with (the signature `person|organization`), so straight in
            // it goes
            if let Some(o) = &t.object_iri {
                domains
                    .entry(t.subject.clone())
                    .or_default()
                    .push(o.clone());
            }
        } else if is_schema(p, "rangeIncludes") {
            if let Some(o) = &t.object_iri {
                ranges.entry(t.subject.clone()).or_default().push(o.clone());
                union_ranged.insert(t.subject.clone());
            }
        } else if !known(p) {
            // Report rather than discard: the preview page has to be able to spell out "what
            // else is in this file that we did not consume"
            *proj.unprojected.entry(p.to_string()).or_insert(0) += 1;
        }
    }

    // Datatype closure: the roots are the ones explicitly `a schema:DataType`, and subclasses
    // are propagated down along subClassOf (Integer ⊂ Number, URL ⊂ Text).
    // **Which IRIs are datatypes is the file's own say**; the only hardcoded part is "which of
    // our four this datatype counts as" -- that half is naming convention, not derivable from RDF
    let mut datatype_classes = datatype_roots.clone();
    loop {
        let grown: Vec<String> = parents
            .iter()
            .filter(|(child, ps)| {
                !datatype_classes.contains(*child)
                    && ps.iter().any(|p| datatype_classes.contains(p))
            })
            .map(|(child, _)| child.clone())
            .collect();
        if grown.is_empty() {
            break;
        }
        datatype_classes.extend(grown);
    }
    for iri in &datatype_classes {
        if let Some(dt) = name_datatype(iri, &parents, &datatype_classes) {
            proj.vocab_datatypes.insert(iri.clone(), dt);
        }
    }

    for iri in &classes {
        // **A datatype is not an entity type.** schema:Text is declared as
        // `a rdfs:Class, schema:DataType`, and looking only at the first half builds entity types
        // called `text`, `number` and `boolean`
        if datatype_classes.contains(iri) {
            continue;
        }
        proj.classes.push(OwlClass {
            key: key_from_iri(iri),
            label: pick_lang(labels.get(iri)).unwrap_or_else(|| local_name(iri).to_string()),
            description: pick_lang(comments.get(iri)).unwrap_or_default(),
            parents: parents.get(iri).cloned().unwrap_or_default(),
            disjoint_with: {
                // De-duplicate: with both directions collected, a vocabulary that wrote both
                // ends gives a duplicate
                let mut d = disjoint.get(iri).cloned().unwrap_or_default();
                d.sort();
                d.dedup();
                d
            },
            iri: iri.clone(),
        });
    }
    // **A de-duplicated union, not two sets stuck end to end**: vocabularies often declare the
    // same property as both rdf:Property and owl:DatatypeProperty (FOAF's name, age, nick... all
    // do), each set collects it once, and chain then spits it out twice. Looking at data_props is
    // enough for the classification.
    let all_props: BTreeSet<&String> = obj_props
        .iter()
        .chain(data_props.iter())
        .chain(plain_props.iter())
        .collect();
    for iri in all_props {
        let rs = ranges.get(iri).cloned().unwrap_or_default();
        // Attribute channel or relation channel. An explicit declaration decides; without one,
        // look at the range: **only all-datatypes counts as an attribute**. A single class
        // anywhere in the union means relation -- `address` has range `PostalAddress|Text`, and
        // judging it an attribute loses that edge forever, whereas a relation can always give a
        // new entity a name. The rich side can fall back; the poor side cannot come back
        let is_datatype = if data_props.contains(iri) {
            true
        } else if obj_props.contains(iri) {
            false
        } else {
            !rs.is_empty() && rs.iter().all(|r| datatype_classes.contains(r))
        };
        proj.properties.push(OwlProperty {
            key: key_from_iri(iri),
            label: pick_lang(labels.get(iri))
                .unwrap_or_else(|| local_name(iri).replace('_', " ").to_string()),
            description: pick_lang(comments.get(iri)).unwrap_or_default(),
            is_datatype,
            functional: functional.contains(iri),
            inverse_functional: inverse_functional.contains(iri),
            transitive: transitive.contains(iri),
            symmetric: symmetric.contains(iri),
            asymmetric: asymmetric.contains(iri),
            inverse_of: inverse_of.get(iri).cloned(),
            sub_property_of: sub_property_of.get(iri).cloned(),
            irreflexive: irreflexive.contains(iri),
            domains: domains.get(iri).cloned().unwrap_or_default(),
            ranges: rs,
            ranges_union: union_ranged.contains(iri),
            iri: iri.clone(),
        });
    }
    order_by_home_namespace(&mut proj);
    Ok(proj)
}

/// Sort **this file's own** vocabulary to the front.
///
/// On a key collision the caller is first-come-first-served (that `claimed` in
/// `owl_import::plan`), and "first" used to come from the lexicographic order of the IRI -- so
/// `http://` sorted ahead of `https://`, and small cited vocabularies systematically beat the main
/// one. The schema.org file merges 50 namespaces; the main vocabulary declares 94% of the terms in
/// it, yet lost 114 of the 141 collisions: `location` lost to OMG Commons, `country` to
/// unece.org, `organization` to purl.org. What got dropped was exactly the terms most worth using.
///
/// The test is that **the namespace with the most declarations owns the file** -- it does not
/// recognise the name schema.org, so it applies to any vocabulary. Ties go to the
/// lexicographically smaller one, which keeps it reproducible.
fn order_by_home_namespace(proj: &mut OwlProjection) {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for iri in proj
        .classes
        .iter()
        .map(|c| c.iri.as_str())
        .chain(proj.properties.iter().map(|p| p.iri.as_str()))
    {
        *counts.entry(namespace_of(iri)).or_insert(0) += 1;
    }
    // max_by_key takes the last maximum, and BTreeMap is in ascending key order -- so on a tie it
    // would hand back the lexicographically largest. We want the smallest, so we compare by hand
    let Some(home) = counts
        .into_iter()
        .fold(None::<(&str, usize)>, |best, (ns, n)| match best {
            Some((_, bn)) if bn >= n => best,
            _ => Some((ns, n)),
        })
        .map(|(ns, _)| ns.to_string())
    else {
        return;
    };
    // Stable sort: only the main vocabulary is lifted to the front, everything else keeps its
    // existing lexicographic order
    proj.classes.sort_by_key(|c| namespace_of(&c.iri) != home);
    proj.properties
        .sort_by_key(|p| namespace_of(&p.iri) != home);
}

/// What is left of an IRI once the local name is removed (including the trailing `#` or `/`).
fn namespace_of(iri: &str) -> &str {
    match iri.rfind(['#', '/']) {
        Some(i) => &iri[..=i],
        None => iri,
    }
}

/// Datatype IRI → our four. If it can be named by itself, use itself; otherwise climb
/// `rdfs:subClassOf` upwards (Integer → Number, URL → Text).
///
/// Anything that cannot be named returns `None` -- `schema:Time`, for instance, which has a time
/// of day but no date, gets the same treatment as `xsd:time`: [`map_range`] takes it through
/// [`RangeMapping::Degraded`], building it as text **and reporting it**.
fn name_datatype(
    iri: &str,
    parents: &BTreeMap<String, Vec<String>>,
    datatypes: &BTreeSet<String>,
) -> Option<&'static str> {
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut stack: Vec<&str> = vec![iri];
    while let Some(cur) = stack.pop() {
        if !seen.insert(cur) {
            continue;
        }
        if let Some(dt) = well_known_datatype(cur) {
            return Some(dt);
        }
        // Only climb through datatypes: a datatype's parent may be an ordinary rdfs:Class
        // (schema:DataType ⊂ rdfs:Class), and stepping past it walks into the whole class
        // hierarchy
        for p in parents.get(cur).into_iter().flatten() {
            if datatypes.contains(p) {
                stack.push(p);
            }
        }
    }
    None
}

/// The hardcoded half: which of our four a datatype's **name** corresponds to.
///
/// This is not derivable from RDF -- a file can say "Integer is a datatype", but not "it is a
/// number and not a date". Only the roots go in the table; subclasses are reached by climbing
/// subClassOf.
///
/// Matching by local name is safe here and does not conflict with [`datatype_of`]'s "must match
/// the full IRI": an IRI that gets this far was declared a datatype by **this file itself**, and a
/// file that says "Date is a datatype" will not also use Date as an entity class.
fn well_known_datatype(iri: &str) -> Option<&'static str> {
    [
        ("Text", "text"),
        ("Number", "number"),
        ("Date", "date"),
        ("DateTime", "date"),
        ("Boolean", "bool"),
    ]
    .into_iter()
    .find(|(local, _)| is_schema(iri, local))
    .map(|(_, dt)| dt)
}

/// Pick one of the multilingual labels: en / zh first, then no language tag, then the first one.
fn pick_lang(vals: Option<&Vec<(String, Option<String>)>>) -> Option<String> {
    let vals = vals?;
    for want in ["en", "zh"] {
        if let Some((v, _)) = vals
            .iter()
            .find(|(_, l)| l.as_deref().is_some_and(|l| l.starts_with(want)))
        {
            return Some(v.clone());
        }
    }
    vals.iter()
        .find(|(_, l)| l.is_none())
        .or_else(|| vals.first())
        .map(|(v, _)| v.clone())
}

/// The local name of an IRI: the part after the last `#` or `/`.
pub fn local_name(iri: &str) -> &str {
    iri.rsplit(['#', '/']).next().unwrap_or(iri)
}

/// Derive a key from an IRI. **The IRI is identity, the key is the label the model reads** (see
/// 0001 P2): only `[a-z0-9_]` is allowed and 40 characters at most, so the IRI itself cannot go in.
/// camelCase is split with underscores: `hasEmployee` → `has_employee`.
pub fn key_from_iri(iri: &str) -> String {
    let local = local_name(iri);
    let mut out = String::with_capacity(local.len() + 4);
    let mut prev_lower = false;
    for c in local.chars() {
        if c.is_ascii_uppercase() {
            if prev_lower && !out.is_empty() {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
            prev_lower = false;
        } else if c.is_ascii_alphanumeric() {
            out.push(c);
            prev_lower = true;
        } else if !out.ends_with('_') && !out.is_empty() {
            out.push('_');
            prev_lower = false;
        }
    }
    while out.ends_with('_') {
        out.pop();
    }
    out.chars().take(40).collect()
}

const XSD: &str = "http://www.w3.org/2001/XMLSchema#";
const RDF_NS: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";

/// The result of mapping `rdfs:range` onto our four datatypes. **Three ways, not two**.
#[derive(Debug, Clone, PartialEq)]
pub enum RangeMapping {
    /// Maps onto `text` / `number` / `date` / `bool`
    Datatype(&'static str),
    /// No range written at all: the vocabulary declared nothing, all we know is that it is a
    /// literal. `text` is the honest superset (it accepts any string and never blocks), so build
    /// it and list it in the preview
    Absent,
    /// A range was written, the value is a **short readable literal**, but none of our four types
    /// can express it (`time` has no year, `duration` is a length not a point in time). Built as
    /// `text` **and reported**: only the ordering semantics are lost and the value is still there,
    /// whereas skipping means this piece of knowledge is never captured at all, which is worse
    Degraded(String),
    /// A range was written, and **the extractor could never read this value out of prose**:
    /// binary blobs, XML fragments, XML-internal identifiers. Skipping it protects no data (there
    /// was never going to be a value); what it saves is prompt -- every property is one line in
    /// the extraction prompt, paid for once per chunk
    Unusable(String),
}

/// A data property's `rdfs:range` → datatype.
///
/// Matched on the **full IRI** rather than the local name: a custom vocabulary may perfectly well
/// have a class called `date`, and matching on the tail end would take it for `xsd:date`.
///
/// Several ranges are always [`RangeMapping::Degraded`] -- in RDFS that is **intersection**
/// semantics ("must be both at once"), almost always a modelling slip, but the spec says so, so we
/// do not guess.
pub fn map_range(ranges: &[String]) -> RangeMapping {
    resolve_range(ranges, false, &VocabDatatypes::new())
}

/// Resolve by the property's **own** range semantics.
///
/// Several `rdfs:range` lines are an intersection, several `schema:rangeIncludes` lines are a
/// union, and both land in the same `ranges` -- without looking at the
/// [`OwlProperty::ranges_union`] bit, `author rangeIncludes Organization, Person` reads as
/// "must be both an organisation and a person".
pub fn map_range_of(p: &OwlProperty, vocab: &VocabDatatypes) -> RangeMapping {
    resolve_range(&p.ranges, p.ranges_union, vocab)
}

fn resolve_range(ranges: &[String], union: bool, vocab: &VocabDatatypes) -> RangeMapping {
    // The vocabulary's self-declared datatypes are looked up first (schema:Text → text); failing
    // that, the standard xsd/owl set
    let named = |iri: &String| vocab.get(iri).copied().or_else(|| datatype_of(iri));
    match ranges {
        [] => RangeMapping::Absent,
        [one] => match named(one) {
            Some(dt) => RangeMapping::Datatype(dt),
            None if unusable(one) => RangeMapping::Unusable(one.clone()),
            None => RangeMapping::Degraded(one.clone()),
        },
        many if union => {
            // Union: if they all point at the same one, that is the one (Text ∪ URL are both
            // text). Inconsistent means degrade to text and report -- text is the honest upper
            // bound of any union
            let dts: Vec<Option<&'static str>> = many.iter().map(named).collect();
            match dts[0] {
                Some(dt) if dts.iter().all(|d| *d == Some(dt)) => RangeMapping::Datatype(dt),
                _ if many.iter().all(|r| unusable(r)) => RangeMapping::Unusable(many.join(" ∪ ")),
                _ => RangeMapping::Degraded(many.join(" ∪ ")),
            }
        }
        // Several rdfs:range lines are intersection semantics, so we do not guess the type -- but
        // the value is still a literal, so it is built as text
        many => RangeMapping::Degraded(many.join(" ∩ ")),
    }
}

/// The ones the extractor could never read out of prose: binary blobs and XML-internal plumbing.
///
/// The test is not "should this value be stored" -- attribute values do live in the graph (via
/// `facts.object_value`, with the full evidence, temporality and review machinery). The test is
/// **whether there will ever be a value**: "the store opens at 9:00 every day" contains `09:00`,
/// whereas a base64 floor plan never turns up in prose; and even if a document really does contain
/// a stretch of base64, extracting it as a fact would be wrong.
///
/// **Everything else degrades to text** -- for a value we can actually extract, a coarse type
/// beats having nowhere to put it.
fn unusable(iri: &str) -> bool {
    if let Some(local) = iri.strip_prefix(XSD) {
        return matches!(
            local,
            "base64Binary"
                | "hexBinary"
                | "QName"
                | "NOTATION"
                | "ID"
                | "IDREF"
                | "IDREFS"
                | "ENTITY"
                | "ENTITIES"
        );
    }
    if let Some(local) = iri.strip_prefix(RDF_NS) {
        return local == "XMLLiteral";
    }
    false
}

fn datatype_of(iri: &str) -> Option<&'static str> {
    if let Some(local) = iri.strip_prefix(XSD) {
        return match local {
            // Every bounded and unsigned variant goes into number: they differ in range, not in
            // meaning
            "decimal" | "integer" | "int" | "long" | "short" | "byte" | "nonNegativeInteger"
            | "positiveInteger" | "nonPositiveInteger" | "negativeInteger" | "unsignedLong"
            | "unsignedInt" | "unsignedShort" | "unsignedByte" | "double" | "float" => {
                Some("number")
            }
            // Our date format is already YYYY[-MM[-DD]] with each level optional, so gYear /
            // gYearMonth fit
            "date" | "dateTime" | "dateTimeStamp" | "gYear" | "gYearMonth" => Some("date"),
            "boolean" => Some("bool"),
            "string" | "normalizedString" | "token" | "language" | "Name" | "NCName"
            | "NMTOKEN" | "anyURI" => Some("text"),
            // time / gMonth / gDay / gMonthDay have no year, and the duration family is a length
            // not a point in time -- they fall to None and unusable() then sorts them out: they
            // are readable literals, so they degrade to text; only binary and XML-internal
            // identifiers are genuinely refused
            _ => None,
        };
    }
    if let Some(local) = iri.strip_prefix(RDF_NS) {
        // XMLLiteral is an XML fragment, not accepted
        return matches!(local, "PlainLiteral" | "langString").then_some("text");
    }
    if let Some(local) = iri.strip_prefix(RDFS) {
        return (local == "Literal").then_some("text");
    }
    if let Some(local) = iri.strip_prefix(OWL) {
        return matches!(local, "real" | "rational").then_some("number");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_maps_every_numeric_variant() {
        for local in [
            "decimal",
            "integer",
            "int",
            "long",
            "short",
            "byte",
            "nonNegativeInteger",
            "positiveInteger",
            "nonPositiveInteger",
            "negativeInteger",
            "unsignedLong",
            "unsignedInt",
            "unsignedShort",
            "unsignedByte",
            "double",
            "float",
        ] {
            assert_eq!(
                map_range(&[format!("{XSD}{local}")]),
                RangeMapping::Datatype("number"),
                "{local}"
            );
        }
        // owl:real / owl:rational are in OWL 2's datatype map too
        assert_eq!(
            map_range(&[format!("{OWL}rational")]),
            RangeMapping::Datatype("number")
        );
    }

    /// Our date format is YYYY[-MM[-DD]] with each level optional, so only the g types missing
    /// the low-order parts fit
    #[test]
    fn partial_dates_fit_only_when_the_year_is_there() {
        for local in ["date", "dateTime", "dateTimeStamp", "gYear", "gYearMonth"] {
            assert_eq!(
                map_range(&[format!("{XSD}{local}")]),
                RangeMapping::Datatype("date"),
                "{local}"
            );
        }
        // The ones with no year cannot become a date -- but they are readable literals, so they
        // degrade to text instead of being dropped
        for local in ["gMonth", "gDay", "gMonthDay", "time"] {
            assert!(
                matches!(
                    map_range(&[format!("{XSD}{local}")]),
                    RangeMapping::Degraded(_)
                ),
                "{local} should degrade to text, not be dropped"
            );
        }
    }

    /// The dividing line is not "can it be mapped exactly" but **"should this value be in the
    /// graph at all"**. Anything storable is kept -- a coarse type beats this piece of knowledge
    /// never being captured at all.
    #[test]
    fn a_value_we_can_store_is_kept_even_when_we_cannot_type_it() {
        // No range written: there is no declaration to lose, and text is the honest superset
        assert_eq!(map_range(&[]), RangeMapping::Absent);
        // Written but inexpressible: a duration is a readable literal, so degrade to text and
        // report it
        assert!(matches!(
            map_range(&[format!("{XSD}duration")]),
            RangeMapping::Degraded(_)
        ));
        // Values that never belonged in the graph: binary blobs and XML fragments -- these are
        // the ones we really do skip
        assert!(matches!(
            map_range(&[format!("{XSD}base64Binary")]),
            RangeMapping::Unusable(_)
        ));
        assert!(matches!(
            map_range(&[format!("{RDF_NS}XMLLiteral")]),
            RangeMapping::Unusable(_)
        ));
    }

    /// Several ranges in RDFS are an **intersection** ("must be both at once"), not a union.
    /// Almost always a modelling slip, but the spec says so -- we do not guess.
    #[test]
    fn several_ranges_are_an_intersection_we_refuse_to_guess() {
        let m = map_range(&[format!("{XSD}string"), format!("{XSD}integer")]);
        match m {
            // An intersection means no guessing the type, but the value is still a literal, so
            // it lands as text
            RangeMapping::Degraded(s) => assert!(s.contains('∩')),
            other => panic!("several ranges must not be mapped exactly: {other:?}"),
        }
    }

    /// Matched on the full IRI: a **class** called date in a custom vocabulary is not xsd:date
    #[test]
    fn a_class_that_happens_to_be_called_date_is_not_a_date() {
        assert!(matches!(
            map_range(&["http://acme.example/hr#date".into()]),
            RangeMapping::Degraded(_)
        ));
    }

    const TTL: &str = r#"
        @prefix owl: <http://www.w3.org/2002/07/owl#> .
        @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
        @prefix ex: <http://acme.example/hr#> .

        ex:Employee a owl:Class ;
            rdfs:label "Employee"@en ;
            rdfs:label "员工"@zh ;
            rdfs:comment "A person on the payroll."@en ;
            rdfs:subClassOf ex:Person .
        ex:Person a owl:Class ; rdfs:label "Person" .
        ex:hasManager a owl:ObjectProperty, owl:FunctionalProperty ;
            rdfs:label "has manager" ;
            rdfs:domain ex:Employee ;
            rdfs:range ex:Person .
        ex:salary a owl:DatatypeProperty ; rdfs:domain ex:Employee .
        ex:Employee owl:disjointWith ex:Contractor .
        ex:Employee owl:equivalentClass ex:Staff .
    "#;

    #[test]
    fn projects_classes_properties_and_reports_the_rest() {
        let p = project(TTL.as_bytes(), RdfFormat::Turtle).unwrap();
        let emp = p.classes.iter().find(|c| c.key == "employee").unwrap();
        // Multilingual labels prefer en
        assert_eq!(emp.label, "Employee");
        assert_eq!(emp.description, "A person on the payroll.");
        assert_eq!(emp.parents, vec!["http://acme.example/hr#Person"]);

        let mgr = p
            .properties
            .iter()
            .find(|x| x.key == "has_manager")
            .unwrap();
        assert!(mgr.functional && !mgr.is_datatype);
        assert_eq!(mgr.ranges, vec!["http://acme.example/hr#Person"]);

        let sal = p.properties.iter().find(|x| x.key == "salary").unwrap();
        assert!(sal.is_datatype);

        // Axioms we cannot consume go into the report -- not discarded, and not an error either.
        //
        // `disjointWith` used to be on this list and is now consumed (it is what the consistency
        // check decides on) -- so this switched to an axiom we still cannot consume, to guard the
        // property itself
        assert!(p
            .unprojected
            .contains_key("http://www.w3.org/2002/07/owl#equivalentClass"));
    }

    #[test]
    fn detects_rdfxml_that_opens_with_comments() {
        // FOAF's official file looks exactly like this: dozens of lines of <!-- --> before any
        // <rdf:RDF>. An earlier version fell back to Turtle when it could not sniff an XML marker,
        // and the parse then failed on the very first line
        let head = b"<!-- This is the FOAF vocabulary, expressed using RDFS and OWL. -->\n\
                     <!-- padding padding padding padding padding padding padding -->\n";
        assert_eq!(RdfFormat::detect("index.rdf", head), RdfFormat::RdfXml);
        // The other way round: Turtle inside a .owl is common too, so the content decides
        assert_eq!(
            RdfFormat::detect("x.owl", b"@prefix owl: <http://x#> .\n"),
            RdfFormat::Turtle
        );
        // Turtle that opens with comments is recognised just as well (# lines are skipped first)
        assert_eq!(
            RdfFormat::detect("x", b"# a note\n\n@base <http://x> .\n"),
            RdfFormat::Turtle
        );
    }

    #[test]
    fn key_derives_from_the_iri_local_name() {
        assert_eq!(key_from_iri("http://x/hr#hasEmployee"), "has_employee");
        assert_eq!(key_from_iri("http://x/ns/Person"), "person");
        assert_eq!(key_from_iri("http://x#HTTP_Server"), "http_server");
    }

    /// A minimal replica of the schema.org style: datatypes declare themselves, properties use
    /// domainIncludes / rangeIncludes, and everything is only ever declared as rdf:Property.
    const SCHEMA_ISH: &str = r#"
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix schema: <https://schema.org/> .

schema:DataType a rdfs:Class .
schema:Text a rdfs:Class, schema:DataType .
schema:Number a rdfs:Class, schema:DataType .
schema:Date a rdfs:Class, schema:DataType .
schema:Time a rdfs:Class, schema:DataType .
schema:URL a rdfs:Class ; rdfs:subClassOf schema:Text .
schema:Integer a rdfs:Class ; rdfs:subClassOf schema:Number .

schema:Organization a rdfs:Class ; rdfs:label "Organization" .
schema:Person a rdfs:Class ; rdfs:label "Person" .
schema:PostalAddress a rdfs:Class ; rdfs:label "PostalAddress" .

schema:foundingDate a rdf:Property ;
    schema:domainIncludes schema:Organization ;
    schema:rangeIncludes schema:Date .
schema:author a rdf:Property ;
    schema:domainIncludes schema:Organization ;
    schema:rangeIncludes schema:Organization, schema:Person .
schema:address a rdf:Property ;
    schema:domainIncludes schema:Organization ;
    schema:rangeIncludes schema:PostalAddress, schema:Text .
schema:homepage a rdf:Property ;
    schema:domainIncludes schema:Person ;
    schema:rangeIncludes schema:Text, schema:URL .
schema:opens a rdf:Property ;
    schema:domainIncludes schema:Organization ;
    schema:rangeIncludes schema:Time .
schema:knows a rdf:Property ;
    schema:domainIncludes schema:Person .
"#;

    fn schema_ish() -> OwlProjection {
        project(SCHEMA_ISH.as_bytes(), RdfFormat::Turtle).unwrap()
    }

    fn prop<'a>(p: &'a OwlProjection, key: &str) -> &'a OwlProperty {
        p.properties.iter().find(|x| x.key == key).unwrap()
    }

    #[test]
    fn schema_org_datatypes_are_not_entity_types() {
        let p = schema_ish();
        let keys: Vec<&str> = p.classes.iter().map(|c| c.key.as_str()).collect();
        // Text / Number / Date / Time are declared as `a rdfs:Class, schema:DataType`, and
        // looking only at the first half builds entity types called text and number
        for gone in [
            "text",
            "number",
            "date",
            "time",
            "url",
            "integer",
            "data_type",
        ] {
            assert!(
                !keys.contains(&gone),
                "{gone} must not be an entity type: {keys:?}"
            );
        }
        assert!(keys.contains(&"organization") && keys.contains(&"person"));
    }

    #[test]
    fn domain_includes_feeds_the_signature() {
        let p = schema_ish();
        // Without recognising schema:domainIncludes this is empty and the signature is (* → *)
        assert_eq!(
            prop(&p, "founding_date").domains,
            vec!["https://schema.org/Organization".to_string()]
        );
    }

    #[test]
    fn a_union_range_of_classes_stays_a_relation() {
        let p = schema_ish();
        // rangeIncludes Organization, Person -- a union, and both of them are classes
        assert!(!prop(&p, "author").is_datatype);
        // rangeIncludes PostalAddress, Text -- a common schema.org spelling, where Text means
        // "write a string if you cannot be bothered to build an entity". Judge it an attribute
        // and this edge is lost forever
        assert!(!prop(&p, "address").is_datatype);
    }

    #[test]
    fn a_union_range_of_datatypes_becomes_an_attribute() {
        let p = schema_ish();
        let fd = prop(&p, "founding_date");
        assert!(fd.is_datatype);
        assert_eq!(
            map_range_of(fd, &p.vocab_datatypes),
            RangeMapping::Datatype("date")
        );
        // Text ∪ URL: URL ⊂ Text, both resolve to text, so no degrading and no reporting needed
        let hp = prop(&p, "homepage");
        assert!(hp.is_datatype);
        assert_eq!(
            map_range_of(hp, &p.vocab_datatypes),
            RangeMapping::Datatype("text")
        );
    }

    #[test]
    fn a_union_is_not_an_intersection() {
        let p = schema_ish();
        // This is the easiest step in the whole business to get wrong: once the two ranges are
        // poured into the same Vec, not looking at ranges_union reads them as rdfs:range's
        // intersection semantics
        assert!(prop(&p, "author").ranges_union);
        assert_eq!(prop(&p, "author").ranges.len(), 2);
        // Whereas two rdfs:range lines are still an intersection
        assert!(matches!(
            map_range(&[format!("{XSD}string"), format!("{XSD}integer")]),
            RangeMapping::Degraded(ref s) if s.contains('∩')
        ));
    }

    #[test]
    fn an_unnamed_datatype_degrades_and_is_reported() {
        let p = schema_ish();
        // schema:Time has a time of day but no date, and gets the same treatment as xsd:time: it
        // is a datatype (so it takes the attribute channel and is not an entity type), but it
        // cannot be named → built as text **and reported**
        let o = prop(&p, "opens");
        assert!(o.is_datatype);
        assert!(matches!(
            map_range_of(o, &p.vocab_datatypes),
            RangeMapping::Degraded(ref s) if s.ends_with("Time")
        ));
    }

    #[test]
    fn a_bare_rdf_property_without_range_is_still_a_relation() {
        let p = schema_ish();
        // No range means nothing to decide on, and the fallback is still a relation -- objects
        // that are IRIs far outnumber literal ones
        let k = prop(&p, "knows");
        assert!(!k.is_datatype);
        assert!(k.ranges.is_empty());
    }

    #[test]
    fn schema_predicates_are_consumed_not_reported_as_unprojected() {
        let p = schema_ish();
        // Once recognised it must not show up under "not projected yet" again, or the preview
        // page says "2312 domainIncludes still unconsumed" when they were in fact consumed
        for consumed in ["domainIncludes", "rangeIncludes"] {
            assert!(
                !p.unprojected.keys().any(|k| k.ends_with(consumed)),
                "{consumed} should already be consumed: {:?}",
                p.unprojected
            );
        }
    }

    /// One file merging two vocabularies, the main one's IRIs on https and the cited one's on
    /// http -- lexicographically http comes first, so the main vocabulary loses. This is the shape
    /// of the schema.org file.
    const TWO_VOCABS: &str = r#"
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix home: <https://home.example/> .
@prefix cited: <http://cited.example/> .

home:Location a rdfs:Class ; rdfs:label "Location" .
home:Country a rdfs:Class ; rdfs:label "Country" .
home:Person a rdfs:Class ; rdfs:label "Person" .
home:Organization a rdfs:Class ; rdfs:label "Organization" .
cited:Location a rdfs:Class ; rdfs:label "Location (cited)" .

home:worksAt a rdf:Property ; rdfs:label "worksAt" .
home:knows a rdf:Property ; rdfs:label "knows" .
cited:worksAt a rdf:Property ; rdfs:label "worksAt (cited)" .
"#;

    #[test]
    fn the_files_own_vocabulary_wins_a_key_collision() {
        let p = project(TWO_VOCABS.as_bytes(), RdfFormat::Turtle).unwrap();
        // Collisions are settled first-come-first-served by the caller, so the order is the
        // ruling. The main vocabulary declares the most, so it has to come first -- otherwise the
        // `http://` < `https://` lexicographic order lets the cited vocabulary win location,
        // country and organization
        let first_location = p.classes.iter().find(|c| c.key == "location").unwrap();
        assert!(
            first_location.iri.starts_with("https://home.example/"),
            "lost to {}",
            first_location.iri
        );
        let first_works = p.properties.iter().find(|x| x.key == "works_at").unwrap();
        assert!(first_works.iri.starts_with("https://home.example/"));
    }
}

#[cfg(test)]
mod axioms {
    use super::*;

    /// OWL's property axioms and class disjointness both have to come out in the projection --
    /// they are **what the consistency check (0002 R0) decides on**. Without them there is no way
    /// to tell whether `A part_of B` and `B part_of A` holding at once is a contradiction or
    /// perfectly normal: `alias_of` both ways is right, `produces` both ways is almost certainly
    /// wrong, and the only thing that tells those apart comes from the ontology.
    const AX: &str = r#"
        @prefix owl: <http://www.w3.org/2002/07/owl#> .
        @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
        @prefix ex: <http://acme.example/ax#> .

        ex:Person a owl:Class ; owl:disjointWith ex:Organization .
        ex:Organization a owl:Class .
        ex:Document a owl:Class .

        ex:partOf   a owl:ObjectProperty, owl:TransitiveProperty, owl:AsymmetricProperty .
        ex:aliasOf  a owl:ObjectProperty, owl:SymmetricProperty .
        ex:reportsTo a owl:ObjectProperty, owl:IrreflexiveProperty .
        ex:plain    a owl:ObjectProperty .
    "#;

    fn proj() -> OwlProjection {
        project(AX.as_bytes(), RdfFormat::Turtle).unwrap()
    }

    #[test]
    fn property_axioms_survive_the_projection() {
        let p = proj();
        let by = |k: &str| p.properties.iter().find(|x| x.key == k).unwrap().clone();

        let part_of = by("part_of");
        assert!(part_of.transitive, "TransitiveProperty must land on it");
        assert!(part_of.asymmetric, "AsymmetricProperty likewise");
        assert!(!part_of.symmetric);

        assert!(
            by("alias_of").symmetric,
            "a symmetric property appearing in both directions is correct, not a contradiction"
        );
        assert!(
            by("reports_to").irreflexive,
            "irreflexive: reporting to yourself is a contradiction"
        );

        // Anything undeclared is false -- **the default is not "unknown" but "no such axiom"**.
        // OWL is open-world, but the consistency check can only judge by what is written down:
        // not written means no grounds, and reporting no contradiction when there are no grounds
        // is safer than inventing an axiom
        let plain = by("plain");
        assert!(!plain.transitive && !plain.symmetric);
        assert!(!plain.asymmetric && !plain.irreflexive);
    }

    /// **Both directions have to be there.** Vocabularies usually write it once (W3C Org makes
    /// four classes pairwise disjoint in six lines), and storing only the written direction makes
    /// "are A and B disjoint" depend on which end the caller happens to ask from.
    #[test]
    fn disjointness_is_recorded_from_both_ends() {
        let p = proj();
        let d = |k: &str| {
            p.classes
                .iter()
                .find(|c| c.key == k)
                .unwrap()
                .disjoint_with
                .clone()
        };
        assert_eq!(d("person"), vec!["http://acme.example/ax#Organization"]);
        assert_eq!(d("organization"), vec!["http://acme.example/ax#Person"]);
        assert!(
            d("document").is_empty(),
            "a class that declares no disjointness must not gain one out of thin air"
        );
    }
}

/// Verified against the **real packs**, not just fixtures.
///
/// The precedent is `pack_alignment::against_real_packs`: that round used the real packs to catch
/// four IRIs written wrong from memory. A fixture can only prove "the Turtle I wrote, I can parse
/// myself", while the real packs prove "we can take the spellings that are in the official files"
/// -- and those are not the same thing.
#[cfg(test)]
mod against_real_packs {
    use super::*;
    use std::io::Read;

    fn load(name: &str) -> Vec<u8> {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../utopia-server/packs/");
        let f = std::fs::File::open(format!("{path}{name}")).expect("pack file is there");
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(f)
            .read_to_end(&mut out)
            .expect("decompress");
        out
    }

    /// W3C Org makes Organization / Role / Membership / Site / ChangeEvent pairwise disjoint,
    /// and the official file writes each pair only once. **All five classes should see four far
    /// ends each** -- store only the written direction and the ones written about first come up
    /// short on far ends, with which one is missing depending on the line order in the file.
    ///
    /// (Writing this assertion I eyeballed grep and counted "four classes"; the real pack
    /// corrected me on the spot: `Organization` is in the disjointness set too. A fixture cannot
    /// prove that kind of thing.)
    #[test]
    fn w3c_org_declares_four_mutually_disjoint_classes() {
        let p = project(&load("w3c-org.ttl.gz"), RdfFormat::Turtle).unwrap();
        for key in ["organization", "role", "membership", "site", "change_event"] {
            let c = p
                .classes
                .iter()
                .find(|c| c.key == key)
                .unwrap_or_else(|| panic!("{key} should be projected"));
            assert_eq!(
                c.disjoint_with.len(),
                4,
                "{key} should be disjoint with the other four, got {:?}",
                c.disjoint_with
            );
        }
    }

    /// IOF Core declares a batch of transitive properties (before/after/occursDuring...).
    /// This is the only place R0's cycle detection has real grounds to stand on
    #[test]
    fn iof_core_declares_transitive_properties() {
        let p = project(&load("iof-core.rdf.gz"), RdfFormat::RdfXml).unwrap();
        let n = p.properties.iter().filter(|x| x.transitive).count();
        assert!(
            n >= 8,
            "IOF should declare a batch of transitive properties, got {n}"
        );
    }

    /// FOAF's Person ⊥ Organization / Document -- **the pair that collides most often**.
    /// That hand-written table in `classify_type_drift` (person vs organization judged Disjoint)
    /// is trying to say exactly this, and its comment even says "once the axioms are in the
    /// database this table should read from the ontology instead"
    #[test]
    fn foaf_says_a_person_is_not_an_organization() {
        let p = project(&load("foaf.rdf.gz"), RdfFormat::RdfXml).unwrap();
        let person = p
            .classes
            .iter()
            .find(|c| c.key == "person")
            .expect("foaf:Person");
        assert!(
            person
                .disjoint_with
                .iter()
                .any(|d| d.ends_with("Organization")),
            "foaf:Person should be disjoint with Organization, got {:?}",
            person.disjoint_with
        );
    }
}

#[cfg(test)]
mod property_axiom_tests {
    use super::*;

    /// `owl:inverseOf` and `rdfs:subPropertyOf` have to be read out.
    ///
    /// `subPropertyOf` used to appear only in the `known()` allowlist -- recognised, no warning,
    /// **and then thrown away**; `inverseOf` did not even make the allowlist and was counted as
    /// "not projected yet". Either way, you import an ontology and that part of the information
    /// vanishes on the spot.
    #[test]
    fn the_two_property_relations_survive_projection() {
        let ttl = include_str!("../tests/inverse_and_sub.ttl");
        let p = project(ttl.as_bytes(), RdfFormat::Turtle).expect("parse");
        let by = |k: &str| {
            p.properties
                .iter()
                .find(|x| x.key == k)
                .unwrap_or_else(|| panic!("no {k}"))
        };
        assert_eq!(
            by("employs").inverse_of.as_deref(),
            Some("http://example.org/worksAt"),
            "**inverses are collected as written** -- filling the reverse is for axiom reading"
        );
        assert_eq!(
            by("ceo_of").sub_property_of.as_deref(),
            Some("http://example.org/worksAt")
        );
        // The reverse direction was not written, so it should be empty: what the ontology wrote
        // and what we inferred have to stay separable, and R0's inverse_not_mutual check relies
        // on that distinction
        assert!(
            by("works_at").inverse_of.is_none(),
            "a direction that was not written must not be filled in at import time"
        );
        assert!(
            !p.unprojected.keys().any(|k| k.contains("inverseOf")),
            "**once recognised it must not be counted as unprojected any more**"
        );
    }
}
