//! How terms that share a name across packs are disposed of.
//!
//! When an import collides on a key, `owl_import`'s default verdict is
//! [`Disposition::KeyTaken`] -- skip it and report, do not append a suffix automatically. That
//! reasoning ("a re-import cannot tell which of them it created last time") is aimed at
//! **guessed** suffixes; what this module hands out are **declared** dispositions, which come
//! out the same on a re-import, so that constraint does not bind here.
//!
//! Why it is needed: the prebuilt packs collide with one another in about 20 places, and those
//! fall into two kinds --
//!
//! - `org:Organization` and `schema:Organization` are **the same thing**, so skipping is right,
//!   but it should not be reported as a "conflict" that makes the user adjudicate something
//!   there is nothing to adjudicate
//! - `org:role` (a position within an organisation) and `schema:role` (the part an actor plays)
//!   **merely share a name**, and skipping throws away the very reason W3C Org exists
//!
//! Only the prebuilt packs are covered. Vocabularies a user imports by hand are not in here --
//! that is explicit intent, and a name collision there ought to be reported to them.

/// The disposition for a term that shares a name.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Alignment {
    /// Synonym: the one already there is it, no need to create another. Skip it, but record it
    /// as "aligned" rather than "conflict"
    SameAs,
    /// Same name, different meaning: create it under this key instead
    Rename(&'static str),
}

/// (incoming IRI, IRI already taken) → disposition.
///
/// Alignments upstream has declared itself are copied first: the W3C Org documentation declares
/// its correspondence with FOAF, and PROV-O declares its correspondence with FOAF and Dublin
/// Core. Only the ones our own packs actually collide on are recorded here.
const TABLE: &[(&str, &str, Alignment)] = &[
    // ── W3C Org × schema.org ──────────────────────────────────────────
    (
        "http://www.w3.org/ns/org#Organization",
        "https://schema.org/Organization",
        Alignment::SameAs,
    ),
    (
        "http://www.w3.org/ns/org#identifier",
        "https://schema.org/identifier",
        Alignment::SameAs,
    ),
    (
        "http://www.w3.org/ns/org#location",
        "https://schema.org/location",
        Alignment::SameAs,
    ),
    // org:Role is "the role a post carries" (it comes as a set with Post and Membership),
    // schema:role is the playing-a-part relation inside a creative work. Same name, unrelated
    (
        "http://www.w3.org/ns/org#Role",
        "https://schema.org/Role",
        Alignment::Rename("org_role"),
    ),
    // org:member is membership with a term of office (reified through Membership),
    // schema:member is affiliation in the general sense. Different granularity, we want both
    (
        "http://www.w3.org/ns/org#member",
        "https://schema.org/member",
        Alignment::Rename("org_member"),
    ),
    (
        "http://www.w3.org/ns/org#memberOf",
        "https://schema.org/memberOf",
        Alignment::Rename("org_member_of"),
    ),
    // ── PROV-O × schema.org ───────────────────────────────────────────
    // prov:Agent is the **superclass** of Person and Organization, not a synonym.
    // Judging it SameAs wipes out the entire "agent" abstraction layer
    (
        "http://www.w3.org/ns/prov#Agent",
        "https://schema.org/agent",
        Alignment::Rename("prov_agent"),
    ),
    // ── FOAF × schema.org ─────────────────────────────────────────────
    (
        "http://xmlns.com/foaf/0.1/Person",
        "https://schema.org/Person",
        Alignment::SameAs,
    ),
    (
        "http://xmlns.com/foaf/0.1/Organization",
        "https://schema.org/Organization",
        Alignment::SameAs,
    ),
    (
        "http://xmlns.com/foaf/0.1/Project",
        "https://schema.org/Project",
        Alignment::SameAs,
    ),
    (
        "http://xmlns.com/foaf/0.1/name",
        "https://schema.org/name",
        Alignment::SameAs,
    ),
    (
        "http://xmlns.com/foaf/0.1/knows",
        "https://schema.org/knows",
        Alignment::SameAs,
    ),
    (
        "http://xmlns.com/foaf/0.1/givenName",
        "https://schema.org/givenName",
        Alignment::SameAs,
    ),
    (
        "http://xmlns.com/foaf/0.1/familyName",
        "https://schema.org/familyName",
        Alignment::SameAs,
    ),
    (
        "http://xmlns.com/foaf/0.1/gender",
        "https://schema.org/gender",
        Alignment::SameAs,
    ),
    (
        "http://xmlns.com/foaf/0.1/logo",
        "https://schema.org/logo",
        Alignment::SameAs,
    ),
    (
        "http://xmlns.com/foaf/0.1/thumbnail",
        "https://schema.org/thumbnail",
        Alignment::SameAs,
    ),
    (
        "http://xmlns.com/foaf/0.1/title",
        "https://schema.org/title",
        Alignment::SameAs,
    ),
    (
        "http://xmlns.com/foaf/0.1/member",
        "https://schema.org/member",
        Alignment::SameAs,
    ),
    // foaf:Agent and prov:Agent are synonyms (that is exactly how PROV-O aligns them
    // officially), but neither of them equals schema:agent
    (
        "http://xmlns.com/foaf/0.1/Agent",
        "https://schema.org/agent",
        Alignment::Rename("foaf_agent"),
    ),
    // foaf:status is the online presence of the instant-messaging era, schema:status is the
    // status of an order/action
    (
        "http://xmlns.com/foaf/0.1/status",
        "https://schema.org/status",
        Alignment::Rename("foaf_status"),
    ),
];

/// Consulted on a name collision. If neither IRI is in a prebuilt pack it returns `None` and
/// falls through to the original `KeyTaken`.
pub fn lookup(incoming_iri: &str, existing_iri: &str) -> Option<Alignment> {
    TABLE
        .iter()
        .find(|(a, b, _)| *a == incoming_iri && *b == existing_iri)
        .map(|(_, _, al)| *al)
}

#[cfg(test)]
mod tests {
    use super::*;

    // The namespace prefixes are only used to build IRIs in the tests; the table spells out full
    // IRIs, because those are the thing compared character by character against the projection
    // results, and concatenation would hide a typo
    const SCHEMA: &str = "https://schema.org/";
    const ORG: &str = "http://www.w3.org/ns/org#";
    const PROV: &str = "http://www.w3.org/ns/prov#";
    const FOAF: &str = "http://xmlns.com/foaf/0.1/";

    #[test]
    fn same_as_and_rename_are_both_reachable() {
        assert_eq!(
            lookup(
                &format!("{ORG}Organization"),
                &format!("{SCHEMA}Organization")
            ),
            Some(Alignment::SameAs)
        );
        assert_eq!(
            lookup(&format!("{ORG}Role"), &format!("{SCHEMA}Role")),
            Some(Alignment::Rename("org_role"))
        );
    }

    /// A pair that is not in the table has to fall back to `KeyTaken` -- **the default is to
    /// report the conflict, not to guess**
    #[test]
    fn unknown_pairs_fall_through() {
        assert_eq!(
            lookup("http://example.com/a#Foo", &format!("{SCHEMA}Foo")),
            None
        );
        assert_eq!(
            lookup(&format!("{PROV}Entity"), "http://example.com/b#Entity"),
            None
        );
    }

    /// Direction-sensitive: the table is (incoming, already taken), and the reverse finds
    /// nothing. A different pack install order hits a different entry, so this must not be
    /// fudged by leaning on symmetry
    #[test]
    fn lookup_is_directional() {
        assert!(lookup(
            &format!("{SCHEMA}Organization"),
            &format!("{ORG}Organization")
        )
        .is_none());
    }

    /// A key produced by Rename has to satisfy the `validate_key` constraints: lowercase
    /// letters, digits and underscores, no longer than 40
    #[test]
    fn renamed_keys_are_valid() {
        for (_, _, al) in TABLE {
            if let Alignment::Rename(k) = al {
                assert!(k.len() <= 40, "{k} is over 40 characters");
                assert!(
                    k.chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
                    "{k} has an illegal character"
                );
            }
        }
    }

    #[test]
    fn no_duplicate_pairs() {
        let mut seen: Vec<(&str, &str)> = TABLE.iter().map(|(a, b, _)| (*a, *b)).collect();
        let n = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), n, "duplicate (incoming, taken) pairs in table");
    }

    /// The FOAF constant is not referenced outside the tests; this confirms it is spelled right
    #[test]
    fn foaf_namespace_is_used() {
        assert!(lookup(&format!("{FOAF}Person"), &format!("{SCHEMA}Person")).is_some());
    }
}

/// Every IRI in the alignment table has to genuinely appear in a pack.
///
/// The table is hand-written strings, and one wrong character makes it fail silently -- a name
/// collision still goes through `KeyTaken`, it just never matches again. An upstream prefix
/// change (which is what happened when schema.org moved from `http://` to `https://`) is the
/// same kind of failure. So project the real packs and check the table entry by entry.
#[cfg(test)]
mod against_real_packs {
    use super::*;
    use std::collections::HashSet;
    use utopia_ingest::ontology_rdf::{project, RdfFormat};

    /// Projects every prebuilt pack and collects the IRIs that show up
    fn all_iris() -> HashSet<String> {
        let mut out = HashSet::new();
        for p in crate::ontology_packs::PACKS {
            let bytes = crate::ontology_packs::bytes(p).expect(p.id);
            let fmt = RdfFormat::detect(p.filename, &bytes);
            let proj = project(&bytes, fmt).unwrap_or_else(|e| panic!("{} projection: {e}", p.id));
            out.extend(proj.classes.iter().map(|c| c.iri.clone()));
            out.extend(proj.properties.iter().map(|p| p.iri.clone()));
        }
        out
    }

    #[test]
    fn every_alignment_iri_exists_in_some_pack() {
        let iris = all_iris();
        let mut missing = Vec::new();
        for (incoming, existing, _) in TABLE {
            if !iris.contains(*incoming) {
                missing.push(*incoming);
            }
            if !iris.contains(*existing) {
                missing.push(*existing);
            }
        }
        assert!(
            missing.is_empty(),
            "these alignment-table IRIs are in no prebuilt pack at all, the table has gone stale:\n  {}",
            missing.join("\n  ")
        );
    }
}
