//! Colours for entity classes: a Morandi palette, plus a deterministic **colour-by-key**
//! function.
//!
//! ## Why this exists
//!
//! Every automatically created class used to get the same `#8ea5bd` -- imported ones, ones
//! built by type resolution, ones seeded at KB creation: all of them that one grey-blue. The
//! product plainly had a curated palette, but it was **only reachable when a human opened the
//! colour picker by hand**. So: install a schema.org (1010 classes) and what you get is 1010
//! classes sharing a single colour, a field of grey on the graph.
//!
//! The capability was always there, it just was never wired into the automatic path. This
//! module is that piece of wiring.
//!
//! ## Why hashing rather than round-robin
//!
//! Round-robin (the nth class takes the nth colour) needs a counter to be maintained, and
//! classes get created down several paths (by hand, by import, by resolution, at KB creation),
//! so the counter would have to be shared across those paths -- that is state that will go out
//! of sync. Hashing by key needs no state: **the same key always gets the same colour**, no
//! matter who created it, in what order it was created, or how many times it has been
//! recreated. Re-import the ontology and the colours do not jump.
//!
//! ## Why not `DefaultHasher`
//!
//! The seed of `std::collections::hash_map::DefaultHasher` is **different in every process**
//! (to defend against hash-collision attacks). Pick colours with it and the same class changes
//! colour the moment the server restarts -- and colour is what users recognise things by. So
//! this hand-rolls an FNV-1a: nailed-down constants, nailed-down results, identical across
//! processes and across machines.

/// The palette for entity classes. **This set is the pre-existing one; it was not swapped
/// out here** -- one Morandi version was tried, and a single look at a real graph rejected
/// it: low saturation plus medium lightness is tuned for paper, and against a near-black
/// canvas the whole thing goes grey, with no telling one class from another. A dark
/// background needs colours whose saturation can hold up.
///
/// **Change this and you have to change `ENTITY_PALETTE` in `web/src/ui/index.tsx` to
/// match**: hand-picked colours and automatically assigned colours must come from the same
/// set, or a single graph ends up with two colour schemes. A test watches this (see the end
/// of this file); miss one side and it goes red.
pub const ENTITY_PALETTE: &[&str] = &[
    "#7fd0ff", "#5fa8ff", "#5fd4d0", "#63e2b7", "#4cc38a", "#a8d878", "#ffd479", "#f2b66d",
    "#ff9d76", "#ff8a9e", "#ff9daf", "#e797d8", "#c4a5ff", "#9fa8ff", "#8ea5bd", "#b3b9c4",
];

/// The shape of a class: **square = declared by a vocabulary, circle = grown out of the
/// corpus**.
///
/// Every automatically created class used to be hard-coded to `circle` -- same as with the
/// colour, the capability was there (the canvas has `NodeSquareShellProgram`, and the legend
/// turns square along with it), nobody was giving it a value.
///
/// **Why not hash it the way colour is hashed**: shape has only two values, and hashing it
/// comes out random -- `person` being square or round would not stand for anything, it would
/// just be noise. Shape is scarce and conspicuous; it should carry a real distinction.
///
/// **Why the IRI rather than the "top-level class"**: top-level class sounds more natural,
/// but in many knowledge bases the classes are flat (nothing built by resolution has a
/// parent), so that would turn into "everything is square" -- merely the same problem flipped
/// over. Whether there is an IRI, by contrast, is **definite, and known on the spot**:
/// imported vocabularies carry IRIs, things grown out of the corpus do not.
///
/// And this is exactly what this product has been saying all along -- **be clear about where
/// a thing came from**. On a graph you can tell at a glance between "this is a class declared
/// in the ontology" and "this is a class grown out of the documents".
pub fn shape_for(iri: &str) -> &'static str {
    if iri.trim().is_empty() {
        "circle"
    } else {
        "square"
    }
}

/// Class key → colour. The same key always gets the same colour.
///
/// FNV-1a, constants hard-coded. Do not swap in `DefaultHasher` -- that one reseeds per
/// process, so the colours all change the moment the server restarts, and users are relying
/// on colour to recognise things.
pub fn color_for_key(key: &str) -> &'static str {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325; // FNV offset basis
    for b in key.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3); // FNV prime
    }
    // **Avalanche mix**: plain FNV plus a modulo clusters -- measured in practice,
    // person/project/research_lab collided into the same bucket, and eight common keys spread
    // across only five colours. This step stirs the high bits into the low ones, and those
    // same eight keys spread out. (The palette length is not prime, so the low bits by
    // themselves do not carry much information)
    hash ^= hash >> 33;
    hash = hash.wrapping_mul(0xff51_afd7_ed55_8ccd);
    hash ^= hash >> 33;
    ENTITY_PALETTE[(hash % ENTITY_PALETTE.len() as u64) as usize]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The same key always gets the same colour** -- across calls, across processes.
    /// A few concrete values are nailed down here: change the hash implementation and the
    /// colours of existing knowledge bases jump all at once.
    #[test]
    fn the_same_key_always_gets_the_same_colour() {
        for _ in 0..3 {
            assert_eq!(color_for_key("organization"), color_for_key("organization"));
        }
        // Pin the concrete results: not because these few values are pretty in themselves,
        // but because "changing the implementation = recolouring every existing KB" has to be
        // an explicit decision
        assert_eq!(color_for_key("person"), "#ff9d76");
        assert_eq!(color_for_key("organization"), "#e797d8");
        assert_eq!(color_for_key("product"), "#f2b66d");
    }

    /// Neighbouring keys should not collide onto the same colour -- otherwise "person" and
    /// "product" cannot be told apart on a graph. Global collision-freedom is not promised
    /// (a dozen-odd colours cannot hold a thousand-odd classes), but the common few have to
    /// spread out.
    #[test]
    fn the_common_keys_spread_across_the_palette() {
        let keys = [
            "person",
            "organization",
            "product",
            "event",
            "place",
            "document",
            "project",
            "team",
        ];
        let colours: std::collections::HashSet<_> = keys.iter().map(|k| color_for_key(k)).collect();
        assert!(
            colours.len() >= 6,
            "eight common keys spread across only {} colours: {colours:?}",
            colours.len()
        );
    }

    /// Shape carries provenance: vocabulary-declared is square, corpus-grown is round.
    #[test]
    fn the_shape_says_where_the_class_came_from() {
        assert_eq!(shape_for("https://schema.org/Person"), "square");
        assert_eq!(shape_for(""), "circle");
        assert_eq!(shape_for("   "), "circle", "blank counts as having no IRI");
    }

    /// **The frontend's colorForKey must agree with this one bit for bit.**
    ///
    /// When a class is created the frontend first picks a colour by key to display, and if the
    /// user does not change it that is what gets stored; the import/resolution path, on the
    /// other hand, is computed on the backend. If the two sides compute differently, the same
    /// key gets different colours depending on "who created it" -- and this raises **no error
    /// whatsoever**.
    ///
    /// All this can check is whether the frontend copy of the code is there and shaped right;
    /// numerical agreement rests on "the same constants + the same arithmetic", with the
    /// comments on both sides pointing the way to each other. Chinese keys need particular
    /// care: the JS side has to iterate over UTF-8 bytes (TextEncoder) -- iterating over char
    /// codes computes something different.
    #[test]
    fn the_frontend_has_a_matching_hash() {
        let ts = include_str!("../../../web/src/ui/index.tsx");
        assert!(
            ts.contains("export function colorForKey"),
            "the frontend is missing colorForKey -- new class colours will not match the backend"
        );
        // Get any one of these three wrong and the computed colour silently drifts off
        assert!(ts.contains("0xcbf29ce484222325n"), "wrong FNV offset basis");
        assert!(ts.contains("0x100000001b3n"), "wrong FNV prime");
        assert!(
            ts.contains("0xff51afd7ed558ccdn"),
            "wrong avalanche mix constant"
        );
        assert!(
            ts.contains("TextEncoder"),
            "must iterate UTF-8 bytes: by char code, Chinese keys compute a different colour"
        );
    }

    /// **The frontend and backend palettes must be the same set.**
    ///
    /// Hand-picking a colour goes through the frontend's `ENTITY_PALETTE`, automatic colour
    /// assignment goes through this one. Let the two drift and a single graph shows two colour
    /// schemes, and not one compile-time check will say a word about it -- the same class of
    /// bug as `sources::KINDS` being out of sync between frontend and backend (the symptom
    /// that time was that the UI let you select it and creation then reported the kind as
    /// invalid; only end-to-end would run into it).
    #[test]
    fn the_frontend_palette_matches_this_one() {
        let ts = include_str!("../../../web/src/ui/index.tsx");
        let start = ts
            .find("export const ENTITY_PALETTE")
            .expect("ENTITY_PALETTE not found in the frontend");
        let body = &ts[start..start + ts[start..].find("];").expect("palette has no end") + 2];
        let front: Vec<&str> = body
            .lines()
            .filter_map(|l| {
                let t = l.trim().trim_end_matches(',').trim_matches('"');
                t.starts_with('#').then_some(t)
            })
            .collect();
        assert_eq!(
            front, ENTITY_PALETTE,
            "frontend and backend palettes disagree -- change one side, change the other"
        );
    }
}
