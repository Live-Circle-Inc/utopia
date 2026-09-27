//! Preset ontology packs: the starting point when a KB is created.
//!
//! A new KB is laid out with **nothing at all**: after 0009 removed the built-in entity
//! classes, 0010 and `#125` removed the seed relations, and 0011 moved `mapped_to` off to the
//! semantic layer, the whole seeding mechanism left the stage. So these packs are not a
//! "supplement" -- they are the **entire source** of an ontology.
//!
//! Not one of those original ten seed relations had a type signature, even though the
//! extraction prompt supports signatures -- `- buys_from (employee|team → *)`. Without a
//! signature, direction can only be described in prose, and prose does not constrain
//! direction. Of schema.org's 1521 properties, 1488 carry domain + range: direction is
//! **declared**, not described. See `docs/decisions/0008`.
//!
//! **The files are embedded in the binary** rather than downloaded at runtime: the README
//! promises the whole system can run on a fully offline intranet, and fetching at runtime would
//! void that sentence. The sources are stored gzipped (1.7 MB → 316 KB) and decompressed in
//! [`bytes`].

use utopia_core::{AppError, AppResult};

/// One optional preset ontology.
///
/// `classes` / `properties` are **display numbers counted on the day the pack was fetched**,
/// for the create-KB screen; how many actually get created is whatever the plan returned by the
/// import says -- the projection only covers the constructs we can consume today.
pub struct Pack {
    pub id: &'static str,
    pub name: &'static str,
    pub summary: &'static str,
    /// The filename handed to `owl_import`. **The format is decided by the extension**
    /// (`RdfFormat::detect`), so the real suffix has to be kept here.
    pub filename: &'static str,
    pub classes: u32,
    pub properties: u32,
    gz: &'static [u8],
}

pub const PACKS: &[Pack] = &[
    Pack {
        id: "schema-org",
        name: "schema.org",
        summary: "People, organizations, products, events, creative works",
        filename: "schema-org.ttl",
        classes: 1010,
        properties: 1676,
        gz: include_bytes!("../packs/schema-org.ttl.gz"),
    },
    Pack {
        id: "w3c-org",
        name: "W3C Org",
        summary: "Departments, posts, memberships, reporting lines",
        filename: "w3c-org.ttl",
        classes: 13,
        properties: 34,
        gz: include_bytes!("../packs/w3c-org.ttl.gz"),
    },
    Pack {
        id: "prov-o",
        name: "PROV-O",
        summary: "Provenance: who produced what, when, from which source",
        filename: "prov-o.ttl",
        classes: 49,
        properties: 69,
        gz: include_bytes!("../packs/prov-o.ttl.gz"),
    },
    Pack {
        id: "foaf",
        name: "FOAF",
        summary: "People and social relations",
        filename: "foaf.rdf",
        classes: 12,
        properties: 62,
        gz: include_bytes!("../packs/foaf.rdf.gz"),
    },
    Pack {
        id: "iof-core",
        name: "IOF Core",
        summary: "Industrial manufacturing",
        filename: "iof-core.rdf",
        classes: 294,
        properties: 75,
        gz: include_bytes!("../packs/iof-core.rdf.gz"),
    },
];

pub fn get(id: &str) -> Option<&'static Pack> {
    PACKS.iter().find(|p| p.id == id)
}

/// Decompress the source. **Every call decompresses again** -- creating a KB is a rare action,
/// and it is not worth keeping 1.7 MB resident for it.
pub fn bytes(pack: &Pack) -> AppResult<Vec<u8>> {
    use std::io::Read;
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(pack.gz)
        .read_to_end(&mut out)
        .map_err(|e| {
            AppError::Other(anyhow::anyhow!("pack {} did not decompress: {e}", pack.id))
        })?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every pack decompresses, and what comes out is not an empty file.
    /// `include_bytes!` guarantees the file exists; it cannot guarantee it is valid gzip.
    #[test]
    fn every_pack_decompresses() {
        for p in PACKS {
            let b = bytes(p).unwrap_or_else(|e| panic!("{}: {e}", p.id));
            assert!(
                b.len() > 10_000,
                "{} decompressed to only {} bytes",
                p.id,
                b.len()
            );
        }
    }

    /// The filename suffix decides the format detection; get it wrong and the whole pack is
    /// handed to the parser as a different syntax.
    #[test]
    fn filenames_carry_a_format_suffix() {
        for p in PACKS {
            assert!(
                p.filename.ends_with(".ttl") || p.filename.ends_with(".rdf"),
                "{} has a filename with no detectable suffix: {}",
                p.id,
                p.filename
            );
        }
    }

    #[test]
    fn ids_are_unique() {
        let mut ids: Vec<_> = PACKS.iter().map(|p| p.id).collect();
        ids.sort_unstable();
        let n = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), n, "duplicate pack id");
    }
}
