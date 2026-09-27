/**
 * The kinds of source -- **on the frontend this list is written exactly once, right here**.
 *
 * The backend's copy is the `SourceKind` enum in `crates/utopia-core`; both the validation at
 * creation time and the dispatch at sync time come out of it. `utopia-store`'s tests read this
 * file and check the two sides against each other -- one kind too many or too few on either
 * side and `cargo test` goes red. Before that the two sides were each written by hand, and five
 * connectors got their sync branch and made it into the UI but never made it into the creation
 * allowlist: selectable, not creatable (#247).
 *
 * The order here is the order in the create-source dialog.
 */
export const CREATABLE_SOURCE_KINDS = [
  "folder",
  "url",
  "rss",
  "github_issues",
  "jira_issues",
  "s3",
  "azure_blob",
  "gcs",
  "webdav",
  "notion",
  "api",
  "custom",
] as const;

export type CreatableSourceKind = (typeof CREATABLE_SOURCE_KINDS)[number];

/** A KB holds two more kinds nobody can create: its own `memory`, and `upload` from old data */
export type SourceKind = CreatableSourceKind | "memory" | "upload";
