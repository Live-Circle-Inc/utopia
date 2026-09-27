//! #247: a source kind is defined in exactly one place, and the two ends are checked against
//! each other.
//!
//! On the backend one enum, `SourceKind` (utopia-core), yields two lists: the allow-list used at
//! creation time and the dispatch used at sync time (the latter matches the enum exhaustively,
//! so the compiler guarantees that adding a kind forces you to decide how it syncs). The
//! frontend's copy lives in `web/src/sourceKinds.ts`, and this test reads it out and compares it
//! with the enum -- before, each side was hand-written, and five connectors made it into the UI
//! and into sync but not into the creation allow-list, so you could pick them in the UI and
//! creating one answered "kind must be one of...". That is the kind of drift neither the unit
//! tests nor tsc can see; here it is visible.
//!
//! No database needed.

use std::path::Path;
use utopia_core::models::SourceKind;

/// Read the quoted literals, in order, out of `CREATABLE_SOURCE_KINDS = [ "…", … ] as const`
fn frontend_kinds(src: &str) -> Vec<String> {
    let start = src
        .find("CREATABLE_SOURCE_KINDS = [")
        .expect("web/src/sourceKinds.ts declares CREATABLE_SOURCE_KINDS");
    let body = &src[start..];
    let end = body.find(']').expect("the array closes");
    body[..end]
        .split('"')
        .skip(1)
        .step_by(2)
        .map(str::to_string)
        .collect()
}

#[test]
fn the_frontend_list_matches_the_backend_enum() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/src/sourceKinds.ts");
    let src =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let frontend = frontend_kinds(&src);
    let backend: Vec<String> = SourceKind::creatable()
        .map(|k| k.as_str().to_string())
        .collect();
    assert_eq!(
        frontend, backend,
        "web/src/sourceKinds.ts and utopia_core::models::SourceKind list different kinds (order matters: it is the dialog's order)"
    );
}

#[test]
fn every_kind_round_trips_through_its_string() {
    for k in SourceKind::all() {
        assert_eq!(SourceKind::parse(k.as_str()), Some(k), "{k:?}");
    }
    assert_eq!(SourceKind::parse("watch_folder"), None);
    assert!(!SourceKind::Memory.creatable_by_hand());
    assert!(!SourceKind::Upload.creatable_by_hand());
    assert!(SourceKind::S3.creatable_by_hand());
}
