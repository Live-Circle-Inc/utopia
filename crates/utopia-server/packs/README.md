# Prebuilt ontology packs

The source files are kept exactly as they came (gzip compressed) and embedded into the binary by
`src/ontology_packs.rs` with `include_bytes!`.

**Why embed instead of downloading at runtime**: the README promises that the whole system can run
in a fully offline intranet environment. Fetching at runtime would void that sentence, and would
also make the build irreproducible (upstream can publish a new version at any moment).

**Why compress**: 1.7 MB uncompressed in total, 316 KB compressed. This is a public repository, and
clone size is a real cost. `flate2` is already in the dependency tree, and decompressing is three
lines.

| File | Source | License | Fetched |
|---|---|---|---|
| `schema-org.ttl.gz` | https://schema.org/version/latest/schemaorg-current-https.ttl | CC BY-SA 3.0 | 2026-08-30 |
| `w3c-org.ttl.gz` | https://www.w3.org/ns/org.ttl | W3C Document License | 2026-08-30 |
| `prov-o.ttl.gz` | https://www.w3.org/ns/prov.ttl | W3C Document License | 2026-08-30 |
| `foaf.rdf.gz` | http://xmlns.com/foaf/spec/index.rdf | CC BY 1.0 | 2026-08-30 |
| `iof-core.rdf.gz` | https://spec.industrialontologies.org/ontology/core/Core/ | MIT | 2026-08-30 |

## Updating a pack

Re-fetch, overwrite with `gzip -9c`, and update both the counts in `ontology_packs.rs` and the
fetch date in this file.
**Do not alter the content of the original** -- the projection only covers the part we can consume
today, and fidelity to the original is criterion 1 of [0001](../../../docs/decisions/0001-ontology-import-and-governance.md).
