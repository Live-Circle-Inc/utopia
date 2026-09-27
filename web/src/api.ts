import type { SourceKind } from "./sourceKinds";
import { S, lang } from "./i18n";

export class ApiError extends Error {
  status: number;
  /** Stable error code from the server (absent = a contract guard not yet converted, and
   *  message is already the raw English sentence) */
  code?: string;
  constructor(status: number, message: string, code?: string) {
    super(message);
    this.status = status;
    this.code = code;
  }
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const res = await fetch(path, {
    credentials: "include",
    headers:
      init?.body instanceof FormData
        ? {}
        : { "Content-Type": "application/json" },
    ...init,
  });
  if (!res.ok) {
    // The wording is settled at this one choke point: not one of the toast.error(e.message)
    // calls in 22 files has to change.
    // With a code, look it up in i18n; without one (or when this code is not in the table yet),
    // fall back to the server's raw English -- the server always speaks English, because the
    // UI language lives on the client (docs/decisions/0004)
    let message = res.statusText;
    let code: string | undefined;
    try {
      const body = (await res.json()) as {
        error?: string;
        code?: string;
        detail?: string;
      };
      if (body.error) message = body.error;
      code = body.code;
      // code comes off the wire, it is not a literal -- this one cast buys full type
      // checking of the err table itself
      const worded = code
        ? (S.err as Record<string, string | undefined>)[code]
        : undefined;
      if (worded) message = worded;
      if (body.detail) message = S.errDetail(message, body.detail);
    } catch {
      // Non-JSON response body, keep statusText
    }
    throw new ApiError(res.status, message, code);
  }
  return res.json() as Promise<T>;
}

export interface User {
  id: string;
  org_id: string;
  email: string;
  display_name: string;
  is_admin: boolean;
  created_at: string;
}

/** Personal access token (0014): a long-lived key for MCP clients, acting as the person who
 *  issued it. Effective permissions = that person's role ∩ scope; an empty `kb_ids` = every
 *  base that person can reach. */
export interface TokenView {
  id: string;
  name: string;
  /** The short piece humans recognise (`utp_pat_ab12`): enough to match the string in a
   *  config file, not enough to reconstruct it */
  token_prefix: string;
  scope: "read" | "write";
  kb_ids: string[] | null;
  expires_at: string | null;
  last_used_at: string | null;
  /** Revoking stamps a timestamp, it does not delete the row */
  revoked_at: string | null;
  created_at: string;
}

export interface Workspace {
  id: string;
  org_id: string;
  name: string;
  created_at: string;
}

/** The optional prebuilt ontology packs offered when creating a base (`GET /ontology-packs`). */
export type OntologyPack = {
  id: string;
  name: string;
  summary: string;
  classes: number;
  properties: number;
};

export interface Kb {
  id: string;
  workspace_id: string;
  name: string;
  kind: string;
  description: string | null;
  visibility: "open" | "restricted";
  /** The deployment's public default space (the first base created): always open, never
   *  deletable */
  is_default: boolean;
  /** When extraction hits a form outside the ontology, may the system add it to the ontology
      on its own and rewrite the facts waiting on it. Turning it off does not stop the
      "noticing": the unmatched counts still accumulate and stay visible, they just become
      proposals you click */
  auto_extend_ontology: boolean;
  /** Write derived facts into the ledger (R1). **Off by default** -- inference adds things to
   *  the graph, and a declaration can be wrong; the graph should not be reshaped on it before
   *  the user has said anything */
  materialize_inferences: boolean;
  /** How often to re-run inference (minutes). Facts keep changing, and relying on a manual
   *  click alone leaves the derivations permanently missing */
  inference_interval_minutes: number;
  /** When the last inference run finished */
  last_inference_at: string | null;
  /** Which language the builtin ontology is seeded in, and new descriptions are written in.
      **Follows the corpus, not the UI** (the UI language lives on the client, see
      docs/decisions/0004) */
  ontology_lang: "en" | "zh";
  /** The caller's role in this base (returned by the detail endpoint only): the frontend gates
   *  destructive entry points on it */
  my_role?: "viewer" | "editor" | "admin" | "owner" | null;
}

/** An account-level "my knowledge bases" row: the base + my role + join info + overview
 *  stats. */
export interface MyKb {
  kb: Kb;
  my_role: "viewer" | "editor" | "admin" | "owner" | null;
  joined_at: string | null;
  added_by_name: string | null;
  doc_count: number;
  member_count: number;
}

/** An audit event (audit display only). */
export interface AuditEvent {
  id: string;
  action: string;
  target_kind: string;
  target_id: string | null;
  detail: Record<string, unknown>;
  /** null = the engine on its own (adjudicator, consistency check, inference); an id with no
   *  name = the account has been removed */
  actor_id: string | null;
  actor_name: string | null;
  created_at: string;
}

export interface KbMember {
  user_id: string;
  email: string;
  display_name: string;
  role: "viewer" | "editor" | "admin";
}

export interface Doc {
  id: string;
  kb_id: string;
  source_id: string | null;
  filename: string;
  mime: string;
  size_bytes: number;
  status: string;
  graph_status: string;
  /** Why the ingest pipeline failed */
  error: string | null;
  /** Why the graph extraction pipeline failed (a separate column from error: each pipeline
   *  stores its own) */
  graph_error: string | null;
  chunk_count: number;
  /** Document tags. **Nothing in the UI uses them today** -- deliberately kept, and the
   *  reason is written on the `tags` column in `migrations/0002_ingest.sql` */
  tags: string[];
  missing_since: string | null;
  created_at: string;
}

/** One kind of extraction drop, aggregated over one document: the fact was extracted, but it
 *  never landed. */
export interface ExtractionDrop {
  document_id: string;
  /** A stable reason code; the frontend looks the wording up by it (attr_domain_mismatch /
   *  low_confidence / ...) */
  reason: string;
  /** The specific object under that reason (an attribute key, a predicate name,
   *  "salary@organization") */
  detail: string;
  count: number;
  example: string | null;
}

export interface SourceView {
  id: string;
  kind: SourceKind;
  name: string;
  config: {
    urls?: string[];
    feed_url?: string;
    endpoint?: string;
    /** github_issues: owner/name */
    repo?: string;
    /** github_issues: in GitHub's model a PR is an issue too; not ingested by default */
    include_pull_requests?: boolean;
    /** jira_issues: the site address, e.g. https://issues.apache.org/jira */
    base_url?: string;
    /** jira_issues: the project key, e.g. KAFKA */
    project?: string;
  } | null;
  icon: string | null;
  sync_interval_minutes: number | null;
  sync_cron: string | null;
  last_sync_at: string | null;
  last_sync_status: "never" | "queued" | "running" | "ok" | "failed";
  last_sync_error: string | null;
  last_sync_added: number;
  doc_count: number;
  missing_count: number;
}

export interface SearchResult {
  id: string;
  document_id: string;
  seq: number;
  text: string;
  filename: string;
}

export interface LlmSettingsView {
  chat_base_url?: string | null;
  chat_model?: string | null;
  has_chat_key?: boolean;
  /** Extraction-only endpoint. All blank = follow chat */
  extract_base_url?: string | null;
  extract_model?: string | null;
  has_extract_key?: boolean;
  embed_base_url?: string | null;
  embed_model?: string | null;
  embed_dim?: number | null;
  has_embed_key?: boolean;
}

export interface Member {
  user_id: string;
  email: string;
  display_name: string;
  role: string;
  is_admin: boolean;
}

export interface OrgUser {
  id: string;
  email: string;
  display_name: string;
  is_admin: boolean;
}

/** A data source for Ask (credentials are never sent down, only a host:port/db summary). */
export interface DataSourceView {
  id: string;
  name: string;
  engine: string;
  summary: string;
  created_at: string;
  last_test_at: string | null;
  last_test_ok: boolean | null;
}

export interface GraphNode {
  id: string;
  name: string;
  // null when no type was decided (0009)
  type_key: string | null;
  type_label: string | null;
  color: string;
  shape: "circle" | "square";
  degree: number;
  disambiguator: string | null;
}

export interface SyncRun {
  id: string;
  started_at: string;
  finished_at: string | null;
  status: "running" | "ok" | "failed";
  created_docs: number;
  updated_docs: number;
  error: string | null;
}

/** A chunk's extraction output (the document viewer's right column). */
export interface ChunkFact {
  chunk_id: string;
  fact_id: string;
  subject_id: string;
  subject: string;
  /** The wording from the source text when the ontology does not recognise this relation;
   *  null when neither can be produced */
  predicate: string | null;
  /** true = the name comes from the source text, not from a relation the ontology
   *  recognises */
  inferred: boolean;
  object_id: string | null;
  object: string | null;
  valid_from: string | null;
  valid_to: string | null;
  confidence: number;
}

/** One derived fact together with its proof (the "derived" tab in the entity panel).
 *
 * `premises` is the reason that tab exists: without the premises, a derived edge and an
 * ordinary edge are indistinguishable in the UI, and that is exactly what "inference
 * polluting the knowledge" looks like. */
export interface DerivedFact {
  id: string;
  subject_id: string;
  subject: string;
  object_id: string;
  object: string;
  predicate: string;
  /** Which rule derived it */
  /** Which rule derived it. The last two are the cross-predicate rules added in 0017 */
  rule: "transitive" | "symmetric" | "inverse" | "sub_property";
  valid_from: string | null;
  valid_to: string | null;
  confidence: number;
  derived_at: string;
  /** The immediate premises, in derivation order */
  premises: string[];
}

/** A derivation that **never landed** (0017 §3): derived, then it hit an assertion and was
 *  held outside the graph. It has no id of its own, so the violation's id stands for it -- the
 *  panel, the ghost edge and the Review card all line up on that */
export interface BlockedDerivation {
  violation_id: string;
  subject_id: string;
  subject: string;
  object_id: string;
  object: string;
  predicate: string;
  rule: string;
  via_label: string;
  valid_from: string | null;
  valid_to: string | null;
  against_fact: string;
  against_text: string;
  premises: string[];
}

/** One step of a proof: one asserted premise with its evidence. Premises are always
 *  assertions, so a proof is a chain, not a tree */
export interface ProofStep {
  seq: number;
  fact_id: string;
  subject_id: string;
  subject: string;
  predicate_id: string | null;
  predicate: string | null;
  object_id: string | null;
  object: string | null;
  valid_from: string | null;
  valid_to: string | null;
  confidence: number;
  /** This premise was retracted later; the derivation went invalid with it, and the proof
   *  still has to show what it rested on at the time */
  retracted: boolean;
  evidence: Evidence[];
}

export interface Proof {
  derived: DerivedFact;
  steps: ProofStep[];
}

/** The buckets on the review page. **The same set of literals as the server's queue
 *  parameter** -- a typo gets you an explicit unknown_queue error, not a silent empty list. */
export type ReviewQueue =
  | "pending"
  | "duplicates"
  | "conflicts"
  | "unconfirmed"
  | "lowconf"
  | "mappings"
  | "violations"
  | "defects"
  | "merges";

/** The true count per bucket. The badge in the left column reads this, not the list length */
export interface ReviewCounts {
  /** Facts extracted from a memory, waiting for a human nod (0015) */
  pending: number;
  duplicates: number;
  conflicts: number;
  unconfirmed: number;
  lowconf: number;
  mappings: number;
  violations: number;
  defects: number;
  merges: number;
}

/** One type-resolution suggestion: an entity to refine, the profile sent to retrieval, and
 * the candidate classes.
 *
 * Returning `profile` to the caller is deliberate -- when retrieval finds nothing, the first
 * thing to look at is "what did we search with", instead of guessing whether the profile or
 * the class description is the wrong one. */
export interface TypeSuggestion {
  entity_id: string;
  name: string;
  /** The class attached right now, possibly none (0009) */
  coarse: string | null;
  coarse_description: string | null;
  proposed_type: string | null;
  specific_type: string | null;
  fact_count: number;
  profile: string;
  candidates: {
    id: string;
    key: string;
    label: string;
    description: string;
    distance: number;
  }[];
}

/** The result of one full run. **Reported in three separate buckets**: retyped automatically,
 *  left for a human, and the ones the adjudicator called "none of these".
 *  That last bucket carries its reason -- this step bets on "choosing none of these is a
 *  respectable answer", and without recorded reasons the largest bucket is opaque. */
export interface ResolutionOutcome {
  batch: string | null;
  retyped: number;
  for_review: {
    entity_id: string;
    name: string;
    coarse: string | null;
    from_type_id: string | null;
    to_type_id: string;
    choice: string;
    confidence: number;
    reason: string | null;
    /** The chosen class is not in the coarse class's subtree -- this switches classification
     *  axis rather than stepping one level down */
    crosses_axis: boolean;
  }[];
  left_alone: {
    name: string;
    coarse: string | null;
    specific_type: string | null;
    reason: string | null;
    top_candidate: string | null;
  }[];
}
export interface ReviewSide {
  id: string;
  name: string;
  // null when no type was decided (0009)
  type_label: string | null;
  color: string;
  disambiguator: string | null;
  degree: number;
  top_facts: string[];
}

export interface ReviewItem {
  id: string;
  score: number;
  reason: string | null;
  stage: "adjudicating" | "human";
  created_at: string;
  left: ReviewSide;
  right: ReviewSide;
}

/** One data-mapping definition: business concept → data asset definition (see
 * docs/decisions/0011).
 *
 * The fields are columns, not keys inside JSON -- this used to be a `mapped_to` fact, with all
 * of these crammed into `object_value`. */
export interface ConceptMapping {
  id: string;
  concept_id: string;
  concept_name: string;
  /** The mounted data source. The same concept can have a different definition per source */
  source: string;
  table_name: string | null;
  expr: string | null;
  sql: string | null;
  unit: string | null;
  summary: string | null;
  /** A derived metric: computed, not a column in a table */
  derived: boolean;
  status: "proposed" | "confirmed" | "rejected";
}
/** What a definition looked like before a change. **A whole-version snapshot, not a diff**
 *  (0006): reading it has to answer "what was it then", and a diff only answers that after
 *  being replayed from the beginning */
export interface MappingRevision {
  id: string;
  before: Record<string, unknown>;
  /** Who changed it; users are soft-deleted, so attribution is not lost when someone
   *  leaves */
  changed_by_name: string | null;
  changed_at: string;
}
/** One axiom violation (0002 R0). The criteria come from the axioms the ontology itself
 *  declares; nothing declared, nothing reported */
/** derived_contradiction only (0017): the triple that was derived -- it was never stored, so
 *  this is the only place it can be written out. Every other kind is `{}` */
export interface ViolationDetail {
  axiom?: "functional" | "asymmetry" | "self_loop";
  rule?: "transitive" | "symmetric" | "inverse" | "sub_property";
  via_label?: string;
  subject?: string;
  predicate?: string;
  object?: string;
  valid_from?: string | null;
  valid_to?: string | null;
  premises?: string[];
}
export type ViolationResolution =
  | "fact_retracted"
  | "fact_closed"
  | "axiom_relaxed"
  | "accepted";
export interface AxiomViolation {
  id: string;
  kind:
    | "self_loop"
    | "asymmetry"
    | "cycle"
    | "functional"
    | "signature"
    | "derived_contradiction";
  /** Which relation the criteria came from. When the verdict is "the axiom is wrong", this is
   *  the way into the ontology to fix it */
  predicate: string | null;
  left_fact: string;
  left_text: string;
  /** For the self-loop kind this equals left -- one fact contradicting itself */
  right_fact: string;
  right_text: string;
  /** The cycle length; 0 for the other three kinds */
  path_len: number;
  detected_at: string;
  detail: ViolationDetail;
  /** A review hint (0017 §2), one at a time: the old assertion has no end date, a same-named
   *  entity exists, extraction confidence is low. Empty when there is none */
  hint: "stale" | "duplicate" | "unsure" | null;
  /** Every fact on the cycle, in order; empty for the other kinds. Retracting a fact has to
   *  name which one (#202) */
  path: { id: string; text: string }[];
}
/** A self-contradiction inside the ontology itself. **Not the same thing as AxiomViolation**:
 *  that one says "a fact clashes with the definition", this one says "the definition does not
 *  stand up on its own", and the latter is more fundamental */
export interface OntologyDefect {
  id: string;
  kind:
    | "symmetric_and_asymmetric"
    | "transitive_and_functional"
    | "subclass_cycle"
    | "disjoint_with_ancestor"
    | "inherits_disjoint"
    // Three kinds added in 0017: all on predicates; the first two about inverses, the third
    // a sub-property cycle
    | "inverse_of_itself"
    | "inverse_not_mutual"
    | "sub_property_cycle"
    // 0017: two rules together produce mutually exclusive derivations; reported once,
    // aggregated per rule pair
    | "rules_disagree";
  subject_label: string | null;
  other_label: string | null;
  path_labels: string[];
  detected_at: string;
  detail: DefectDetail;
}
/** rules_disagree only: which two rules, which axiom they hit, how many pairs, a few
 *  examples */
export interface DefectDetail {
  count?: number;
  rules?: {
    rule_a: string;
    via_a: string;
    rule_b: string;
    via_b: string;
    axiom: string;
    count: number;
    examples: [string, string][];
  }[];
}
export interface FactReviewItem {
  id: string;
  subject_name: string;
  predicate_label: string | null;
  object_name: string | null;
  valid_from: string | null;
  valid_to: string | null;
  confidence: number;
  evidence_count: number;
  quote: string | null;
}

/** A fact extracted from one memory, waiting for a human nod (0015). `quote` is the full text
 *  of that memory -- the confirmation UI puts the original sentence above the triple, so the
 *  person judges against the sentence instead of judging a triple out of thin air. */
export interface PendingFactItem {
  id: string;
  subject_id: string;
  subject_name: string;
  predicate_id: string | null;
  /** The relation name in the ontology; when empty the UI shows `proposed_predicate`
   *  (italic, marked as the raw wording) */
  predicate_label: string | null;
  proposed_predicate: string | null;
  object_id: string | null;
  object_name: string | null;
  object_value: { value?: unknown; unit?: string; summary?: string } | null;
  valid_from: string | null;
  valid_to: string | null;
  confidence: number;
  chunk_id: string;
  quote: string;
  proposed_by: string | null;
  proposed_by_name: string | null;
  created_at: string;
}

/** A temporal conflict (the ones auto-closing was unsure about): old fact vs new fact. */
export interface ConflictItem {
  id: string;
  reason: "no_time" | "simultaneous" | "low_confidence";
  created_at: string;
  predicate_label: string;
  old_fact_id: string;
  old_subject: string;
  old_object: string | null;
  old_valid_from: string | null;
  new_fact_id: string;
  new_subject: string;
  new_object: string | null;
  new_valid_from: string | null;
  new_confidence: number;
}

/** A decision-ledger event (the review slice of audit_events): detail is a self-contained
 *  snapshot taken at decision time. */
export interface ReviewHistoryEvent {
  id: string;
  action: string;
  target_kind: string;
  target_id: string | null;
  detail: Record<string, unknown>;
  /** null = the system (the AI adjudicator) */
  actor_name: string | null;
  created_at: string;
}

export interface MergeLog {
  id: string;
  source_name: string;
  target_name: string;
  merged_by_name: string | null;
  reason: string | null;
  created_at: string;
  reverted_at: string | null;
}

export interface GraphEdge {
  id: string;
  source: string;
  target: string;
  /** The wording from the source text when the ontology does not recognise this relation;
   *  null when neither can be produced (old data from before 0052) */
  predicate: string | null;
  label: string | null;
  /** true = this edge's name comes from the source text, not from a relation the ontology
   *  recognises */
  inferred: boolean;
  /** true = this edge was **derived**, nobody asserted it (R1).
   *  Not the same thing as `inferred`: that one says "the name comes from the source text",
   *  this one says "nobody said it" */
  derived: boolean;
  /** The rule that derived it; null for asserted edges.
   *  The UI uses it to spot `inverse` -- such an edge and its source edge are two phrasings of
   *  the same thing, and drawing both just draws the redundancy twice (see Graph's
   *  `layOutParallelEdges`) */
  rule: string | null;
  /** The premise fact ids used to derive it (in proof order); empty for asserted edges.
   *  Merging parallel edges needs it to pin down the source -- matching on the node pair alone
   *  hangs the wording on the wrong edge */
  premises: string[];
  valid_from: string | null;
  valid_to: string | null;
  confidence: number;
  /** Contested (0017 §3): an open axiom violation or temporal conflict points at it. The
   *  whole edge is drawn in the warning colour */
  contested: boolean;
  /** A ghost edge (0017 §3): a derivation that never landed. `id` is the id of that
   *  `derived_contradiction` violation; `derived` is true as well, so it follows the derived
   *  toggle. Clicking it opens the subject's panel */
  blocked: boolean;
}

export interface EntityFact {
  id: string;
  direction: "out" | "in";
  /** Same as GraphEdge: a relation outside the ontology falls back to the source wording;
   *  null when there is neither */
  predicate_key: string | null;
  predicate_label: string | null;
  /** true = the name comes from the source text, not from a relation the ontology
   *  recognises */
  inferred: boolean;
  /** The relation's temporal class. Without a predicate there is nothing to say, so null */
  temporal: string | null;
  other_id: string | null;
  other_name: string | null;
  /** A literal-value object (attribute facts / Ask mappings): {"value":…} or {"summary":…} */
  object_value: Record<string, unknown> | null;
  valid_from: string | null;
  valid_to: string | null;
  valid_from_precision: string | null;
  /** year | month | day, plus unknown = the text says it ended but not on which day */
  valid_to_precision: string | null;
  confidence: number;
  evidence_count: number;
  /** All the evidence sits on an older version of the source document (not confirmed by the
   *  current content; this does not mean the fact is invalid) */
  stale: boolean;
  /** A correction row: the interval was closed by engine reconciliation or a human ruling,
   *  not by the extracted text */
  corrected: boolean;
  /** Contested (0017 §3): which kind, the id of that item in Review, and the sentence derived
   *  when a derivation hit an assertion.
   *  The row is not dimmed -- the assertion is still alive */
  contested: {
    kind: string;
    ref_id: string;
    derived?: string | null;
  } | null;
  /** The newest document time in the evidence set (an open fact's "last confirmed at") */
  last_evidence_time: string | null;
}

/** One belief change on an entity (an event on the record-time axis, orthogonal to
 * EntityFact's valid-time axis).
 *
 * Not every event comes from a fact: retyped / retype_reverted come from the retype ledger,
 * with no predicate, no other side and no direction. */
export interface EntityHistoryEvent {
  /** Only fact events have it; null for retype events */
  fact_id: string | null;
  /** The record-time instant: asserted = recorded_at, invalidated = invalidated_at, retyped =
   *  the moment of the retype */
  at: string;
  kind:
    | "asserted"
    | "corrected"
    | "rejected"
    | "merged"
    | "retyped"
    | "retype_reverted";
  direction: "out" | "in" | null;
  predicate_label: string | null;
  other_name: string | null;
  object_value: Record<string, unknown> | null;
  valid_from: string | null;
  valid_to: string | null;
  valid_from_precision: string | null;
  /** year | month | day, plus unknown = the text says it ended but not on which day */
  valid_to_precision: string | null;
  confidence: number | null;
  /** null = the engine on its own (extraction write / temporal reconciliation close /
   *  high-confidence auto retype) */
  actor_name: string | null;
  action: string | null;
  document_id: string | null;
  filename: string | null;
  quote: string | null;
  /** The two ends of a retype event. A null source = retyped out of "unclassified" (the most
   *  common one since 0009) */
  from_type_label: string | null;
  to_type_label: string | null;
}

export interface Evidence {
  /** The predicate wording the model actually used in this chunk. A predicate outside the
   *  ontology never lands on a relation, so this is what the UI shows */
  proposed_predicate: string | null;
  quote: string | null;
  chunk_id: string;
  document_id: string;
  filename: string;
  seq: number;
  /** Which version of the document this evidence came from */
  doc_version: number;
  /** The document already has a newer version (the evidence sits on the old one; this does
   *  not mean the fact is invalid) */
  stale: boolean;
}

export interface ChunkFull {
  id: string;
  seq: number;
  text: string;
}

export interface EntityTypeView {
  id: string;
  key: string;
  label: string;
  color: string;
  shape: "circle" | "square";
  builtin: boolean;
  /** All parent classes (subClassOf may have several) */
  parents: string[];
  /** The classes it is disjoint with: **a declaration of "cannot be both at once"** */
  disjoint: string[];
  /** Which branch it hangs under when the left column draws the tree. No semantics, display
   *  only */
  primary_parent: string | null;
  description: string;
  usage: number;
}

export interface RelationTypeView {
  id: string;
  key: string;
  label: string;
  temporal: string;
  functional: boolean;
  inverse_functional: boolean;
  /** The other four OWL axioms. **All of the reasoner's criteria live here** */
  is_transitive: boolean;
  is_symmetric: boolean;
  is_asymmetric: boolean;
  is_irreflexive: boolean;
  /** The two that point at another relation: `p⁻¹ = q` and `p ⊑ q`. Ids rather than
   *  booleans, so the UI shows dropdowns */
  inverse_of: string | null;
  sub_property_of: string | null;
  builtin: boolean;
  description: string;
  /** relation (the object is an entity) | attribute (the object is a literal value) */
  kind: "relation" | "attribute";
  /** The classes that may be the subject. At least one for an attribute; left empty on a
   *  relation = unrestricted */
  domains: string[];
  /** The classes that may be the object. Only meaningful for a relation -- an attribute's
   *  range is its datatype */
  ranges: string[];
  datatype: "text" | "number" | "date" | "bool" | null;
  unit: string | null;
  usage: number;
}

export interface OntologyMiss {
  kind: "entity_type" | "relation_type";
  key: string;
  example: string | null;
  count: number;
}

/** `description` and `reason` are not the same thing: description goes into the extraction
    prompt verbatim and is the model's only basis for deciding "what counts as this class";
    reason is merely the human-facing "why it should be added". Feed the wrong one and this
    class becomes the next dumping ground -- technology is exactly how that happened here. */
export interface OntologyProposals {
  entity_types: {
    key: string;
    label: string;
    description?: string;
    reason?: string;
  }[];
  relation_types: {
    key: string;
    label: string;
    temporal?: string;
    functional?: boolean;
    description?: string;
    reason?: string;
    /** Which surface forms this relation folds together. Only with it can the waiting facts
     *  be rewritten onto it */
    forms?: string[];
  }[];
  /**
   * Forms whose object is a literal value ("founded date = 2015").
   *
   * Kept apart from relation_types because they need different things: an attribute has a
   * datatype, and building one as a relation turns that value into a fake entity. domain is
   * not here -- the server takes it from the subject type of the fact, and guessing wrong
   * gets the whole thing dropped
   */
  attribute_types?: {
    key: string;
    label: string;
    datatype?: string;
    unit?: string;
    description?: string;
    reason?: string;
    forms?: string[];
  }[];
  /**
   * The ontology **already has** this meaning; the form just needs to be hung onto it.
   *
   * The difference from relation_types is that nothing gets created: let one meaning grow a
   * second key and this batch of facts is split across two places forever, with nobody able
   * to tell they were ever the same thing.
   */
  map_to?: {
    key: string;
    /** Marked by the server: whether the target lands on a relation or an attribute. The two
        rewrite paths differ, and the model can only answer with a key, which does not say
        which bucket it is in */
    kind?: string;
    forms?: string[];
    reason?: string;
  }[];
}

/** Forms the source text used, the ontology does not have, and which therefore leave the fact
 *  without a predicate. */
export interface ProposedPredicate {
  form: string;
  fact_count: number;
  example: string | null;
}

/** Where one class/attribute ends up in this import. key_taken = the key is held by another
 *  IRI, so it is reported and left alone */
export interface PlannedItem {
  iri: string;
  key: string;
  label: string;
  has_description: boolean;
  disposition: "create" | "update" | "key_taken";
  functional?: boolean;
  conflict_with?: string | null;
}

/** Preview and commit return the same plan: what happens after you click confirm is exactly
 *  what you just looked at */
export interface ImportPlan {
  format: string;
  triples: number;
  classes: PlannedItem[];
  relations: PlannedItem[];
  attributes: PlannedItem[];
  /** Axioms that appeared but are not consumed today → how many times. Not skipped, just not
   *  projected yet */
  unprojected: [string, number][];
  classes_without_description: number;
  functional_relations: number;
}

export interface OntologyImportView {
  id: string;
  filename: string;
  format: string;
  byte_size: number;
  summary: Record<string, unknown>;
  imported_by_name: string | null;
  imported_at: string;
}

export interface Source {
  n: number;
  /** Absent = a document chunk citation; charter = the builtin manual (jumps to
   *  /docs/{slug}#{anchor}) */
  kind?: "charter";
  chunk_id?: string;
  document_id?: string;
  slug?: string;
  anchor?: string;
  heading?: string;
  filename: string;
  excerpt: string;
}

/** The action trail of an agentic conversation (one entry per tool call). */
export interface ChatStep {
  kind: "search" | "docs" | "entity" | "facts" | "changes" | "query" | "tool";
  label: string;
  detail: string;
  /** The `remember` step carries it: the chunk that memory landed in. The confirmation card
   *  in the conversation fetches the pending items by it (0015), and replay redraws from it
   *  too */
  chunk_id?: string;
  /** How long the answer text already was when this step happened (UTF-16 code units, the
   *  same unit as `string.length`).
   *  Used to thread the trail back into the prose instead of piling it all at the front.
   *  **Messages from before this migration do not have it** -- when absent the whole trail
   *  goes back to the top, i.e. the old look */
  at?: number;
}

/** A conversation row (the list in Chat's left column). */
export interface ConversationRow {
  id: string;
  title: string;
  created_at: string;
  updated_at: string;
  message_count: number;
}

/** A conversation message (with the stored action trail and citations, for history replay). */
export interface ConversationMessage {
  id: string;
  role: "user" | "assistant";
  content: string;
  steps: ChatStep[];
  sources: Source[];
  created_at: string;
}

/** One group in the alert centre (0005): **consecutive failures of the same kind**.
 *
 * Storage still keeps one row per failure; the folding happens on the server at read time --
 * that way paging counts groups, and a run of consecutive failures is not cut by a page
 * boundary. */
export type AlertGroup = {
  kb_id: string | null;
  /** System-level alerts have no base name */
  kb_name: string | null;
  /** `source.sync_failed` / `llm.unreachable` -- the wording is looked up in i18n by this */
  kind: string;
  severity: "info" | "warning" | "error";
  /** How many times in this group */
  count: number;
  /** How many of those I have not read */
  unread: number;
  latest_at: string;
  /** Together with latest_at it delimits this group; sent back verbatim when marking read */
  earliest_at: string;
  /** The details, a few at most, newest first */
  lines: { name?: string; error?: string; job?: string }[];
};

export const api = {
  health: () =>
    request<{ status: string; name: string; version: string }>(
      "/api/v1/health",
    ),
  me: () => request<User>("/api/v1/auth/me"),
  alerts: (o: { q?: string; limit?: number; offset?: number }) => {
    const p = new URLSearchParams();
    if (o.q?.trim()) p.set("q", o.q.trim());
    if (o.limit != null) p.set("limit", String(o.limit));
    if (o.offset) p.set("offset", String(o.offset));
    return request<{ items: AlertGroup[]; total: number }>(
      `/api/v1/alerts?${p}`,
    );
  },
  alertsUnread: () => request<{ unread: number }>("/api/v1/alerts/unread"),
  /** Requeue failed jobs (#216). One endpoint per base, one global (admin); the scope can be
   *  narrowed by kind and failure time */
  failedJobs: (kbId: string) =>
    request<{ failed: number }>(`/api/v1/kbs/${kbId}/jobs/failed`),
  requeueJobs: (
    kbId: string | null,
    body: { kind?: string; failed_since?: string } = {},
  ) =>
    request<{ requeued: number }>(
      kbId ? `/api/v1/kbs/${kbId}/jobs/requeue` : "/api/v1/jobs/requeue",
      { method: "POST", body: JSON.stringify(body) },
    ),
  alertReadGroup: (g: {
    kb_id: string | null;
    kind: string;
    from: string;
    to: string;
  }) =>
    request<{ marked: number }>("/api/v1/alerts/read-group", {
      method: "POST",
      body: JSON.stringify(g),
    }),
  alertsReadAll: () =>
    request<{ ok: boolean }>("/api/v1/alerts/read-all", { method: "POST" }),
  login: (email: string, password: string) =>
    request<{ user: User }>("/api/v1/auth/login", {
      method: "POST",
      body: JSON.stringify({ email, password }),
    }),
  register: (email: string, password: string, displayName: string) =>
    request<{ user: User; workspace: Workspace }>("/api/v1/auth/register", {
      method: "POST",
      body: JSON.stringify({ email, password, display_name: displayName }),
    }),
  logout: () =>
    request<{ ok: boolean }>("/api/v1/auth/logout", { method: "POST" }),
  updateMe: (displayName: string) =>
    request<User>("/api/v1/auth/me", {
      method: "PATCH",
      body: JSON.stringify({ display_name: displayName }),
    }),
  changePassword: (currentPassword: string, newPassword: string) =>
    request<{ ok: boolean }>("/api/v1/auth/password", {
      method: "POST",
      body: JSON.stringify({
        current_password: currentPassword,
        new_password: newPassword,
      }),
    }),
  /** The personal tokens I have issued (0014). The revoked ones are here too -- the fact that
   *  something was revoked has to stay visible */
  tokens: () => request<{ tokens: TokenView[] }>("/api/v1/me/tokens"),
  /** Issue one. **The plaintext exists only in this one response**; the list can never
   *  produce it */
  issueToken: (body: {
    name: string;
    scope: "read" | "write";
    /** Absent = every base that person can reach */
    kb_ids?: string[] | null;
    /** 0 = never expires */
    expires_in_days: number;
  }) =>
    request<{ token: string; info: TokenView }>("/api/v1/me/tokens", {
      method: "POST",
      body: JSON.stringify(body),
    }),
  revokeToken: (tokenId: string) =>
    request<{ ok: boolean }>(`/api/v1/me/tokens/${tokenId}`, { method: "DELETE" }),
  workspaces: () => request<Workspace[]>("/api/v1/workspaces"),

  kbs: (workspaceId: string) =>
    request<Kb[]>(`/api/v1/workspaces/${workspaceId}/kbs`),
  /** The audit ledger. **Paged + filtered** -- the ledger is compliance material, and seeing
   *  only the last 100 rows means history cannot be searched at all.
   *  `action` is a prefix: `entity.` pulls in the entity.retyped / entity.renamed family. */
  kbAudit: (
    kbId: string,
    opts: {
      action?: string;
      actor?: string;
      since?: string;
      until?: string;
      limit: number;
      offset: number;
    },
  ) => {
    const p = new URLSearchParams({
      limit: String(opts.limit),
      offset: String(opts.offset),
    });
    if (opts.action) p.set("action", opts.action);
    if (opts.actor) p.set("actor", opts.actor);
    if (opts.since) p.set("since", opts.since);
    if (opts.until) p.set("until", opts.until);
    return request<{
      events: AuditEvent[];
      total: number;
      /** The actions that actually happened in this base; the filter dropdown is filled from
       *  it */
      actions: string[];
    }>(`/api/v1/kbs/${kbId}/audit?${p}`);
  },
  myKbs: (workspaceId: string) =>
    request<{ kbs: MyKb[] }>(`/api/v1/workspaces/${workspaceId}/my-kbs`),
  createKb: (
    workspaceId: string,
    body: {
      name: string;
      description?: string | null;
      visibility?: string;
      /** Ontology pack ids; the order matters: the first one claims same-named seed classes */
      ontology_packs?: string[];
    },
  ) =>
    request<Kb>(`/api/v1/workspaces/${workspaceId}/kbs`, {
      method: "POST",
      body: JSON.stringify(body),
    }),

  ontologyPacks: () =>
    request<{ packs: OntologyPack[] }>("/api/v1/ontology-packs"),

  kbDetail: (kbId: string) => request<Kb>(`/api/v1/kbs/${kbId}`),
  updateKb: (kbId: string, body: Record<string, unknown>) =>
    request<Kb>(`/api/v1/kbs/${kbId}`, {
      method: "PATCH",
      body: JSON.stringify(body),
    }),
  deleteKb: (kbId: string) =>
    request<{ ok: boolean }>(`/api/v1/kbs/${kbId}`, { method: "DELETE" }),
  kbMembers: (kbId: string) =>
    request<{ members: KbMember[] }>(`/api/v1/kbs/${kbId}/members`),
  setKbMember: (kbId: string, userId: string, role: string) =>
    request<{ ok: boolean }>(`/api/v1/kbs/${kbId}/members/${userId}`, {
      method: "PUT",
      body: JSON.stringify({ role }),
    }),
  removeKbMember: (kbId: string, userId: string) =>
    request<{ ok: boolean }>(`/api/v1/kbs/${kbId}/members/${userId}`, {
      method: "DELETE",
    }),

  adminDeployment: () =>
    request<{
      open_registration: boolean;
      /** An outer backstop against jobs piling up without bound; the real throttle is the
       *  per-model limit */
      worker_concurrency: number;
      default_model_concurrency: number;
      /** The default ontology language for new knowledge bases. **Not the UI language** --
       *  that one lives on the client */
      default_ontology_lang: "en" | "zh";
      model_limits: {
        base_url: string;
        model: string;
        max_concurrent: number;
      }[];
      models_in_use: { base_url: string; model: string; kind: string }[];
    }>("/api/v1/admin/deployment"),
  saveAdminDeployment: (
    openRegistration: boolean,
    workerConcurrency?: number,
    defaultModelConcurrency?: number,
    modelLimit?: {
      base_url: string;
      model: string;
      max_concurrent: number | null;
    },
    defaultOntologyLang?: "en" | "zh",
  ) =>
    request<{ ok: boolean }>("/api/v1/admin/deployment", {
      method: "PUT",
      body: JSON.stringify({
        open_registration: openRegistration,
        ...(workerConcurrency !== undefined
          ? { worker_concurrency: workerConcurrency }
          : {}),
        ...(defaultModelConcurrency !== undefined
          ? { default_model_concurrency: defaultModelConcurrency }
          : {}),
        ...(modelLimit ? { model_limit: modelLimit } : {}),
        ...(defaultOntologyLang
          ? { default_ontology_lang: defaultOntologyLang }
          : {}),
      }),
    }),
  adminCreateUser: (body: {
    email: string;
    display_name: string;
    password: string;
    role: string;
  }) =>
    request<{ user: User }>("/api/v1/admin/users", {
      method: "POST",
      body: JSON.stringify(body),
    }),

  /** Deactivated accounts. **Without this, reactivation is out of reach** -- that person has
   *  disappeared from every list, and the reactivate endpoint wants exactly their id */
  deactivatedUsers: () => request<OrgUser[]>("/api/v1/users/deactivated"),
  /** Reactivate a deactivated account */
  adminReactivateUser: (userId: string) =>
    request<{ ok: boolean }>(`/api/v1/admin/users/${userId}`, {
      method: "POST",
    }),
  /** Deactivate an account (soft delete). Attribution stays queryable -- the audit log, the
   *  merge log and the retype ledger all depend on it */
  adminDeactivateUser: (userId: string) =>
    request<{ ok: boolean }>(`/api/v1/admin/users/${userId}`, {
      method: "DELETE",
    }),
  adminDataSources: () =>
    request<{ data_sources: DataSourceView[] }>("/api/v1/admin/data-sources"),
  adminCreateDataSource: (body: { name: string; conn_string: string }) =>
    request<{ id: string }>("/api/v1/admin/data-sources", {
      method: "POST",
      body: JSON.stringify(body),
    }),
  adminDeleteDataSource: (id: string) =>
    request<{ ok: boolean }>(`/api/v1/admin/data-sources/${id}`, {
      method: "DELETE",
    }),
  adminTestDataSource: (id: string) =>
    request<{ ok: boolean }>(`/api/v1/admin/data-sources/${id}/test`, {
      method: "POST",
    }),
  /** Which workspaces this source is granted to (0014). Granting and mounting are two layers:
   *  a system admin writes the grant, and a KB admin picks from the granted set to mount */
  dataSourceGrants: (id: string) =>
    request<{ workspaces: { id: string; name: string }[] }>(
      `/api/v1/admin/data-sources/${id}/grants`,
    ),
  grantDataSource: (id: string, workspaceId: string) =>
    request<{ ok: boolean }>(
      `/api/v1/admin/data-sources/${id}/grants/${workspaceId}`,
      { method: "PUT" },
    ),
  /** Revoke a grant. **Whatever is already mounted in that workspace is unmounted with it**,
   *  and it returns how many were unmounted */
  revokeDataSource: (id: string, workspaceId: string) =>
    request<{ ok: boolean; unmounted: number }>(
      `/api/v1/admin/data-sources/${id}/grants/${workspaceId}`,
      { method: "DELETE" },
    ),

  /** One page of definitions. **A Viewer can see them** -- an Ask answer is decided directly
   *  by the definitions, and seeing the answer without the definition amounts to asking
   *  someone to trust an algorithm they are not shown */
  mappings: (
    kbId: string,
    opts: {
      status?: "proposed" | "confirmed" | "rejected";
      q?: string;
      limit?: number;
      offset?: number;
    } = {},
  ) => {
    const p = new URLSearchParams();
    if (opts.status) p.set("status", opts.status);
    if (opts.q) p.set("q", opts.q);
    if (opts.limit != null) p.set("limit", String(opts.limit));
    if (opts.offset != null) p.set("offset", String(opts.offset));
    const qs = p.toString();
    return request<{
      items: ConceptMapping[];
      total: number;
      counts: { proposed: number; confirmed: number; rejected: number };
    }>(`/api/v1/kbs/${kbId}/mappings${qs ? `?${qs}` : ""}`);
  },
  /** Revise one definition. The version before the change goes into revisions automatically */
  reviseMapping: (
    kbId: string,
    mappingId: string,
    body: {
      table_name?: string | null;
      expr?: string | null;
      sql?: string | null;
      unit?: string | null;
      summary?: string | null;
      derived: boolean;
    },
  ) =>
    request<{ ok: boolean }>(`/api/v1/kbs/${kbId}/mappings/${mappingId}`, {
      method: "PATCH",
      body: JSON.stringify(body),
    }),
  mappingRevisions: (kbId: string, mappingId: string) =>
    request<{ revisions: MappingRevision[] }>(
      `/api/v1/kbs/${kbId}/mappings/${mappingId}/revisions`,
    ),
  kbDataSources: (kbId: string) =>
    request<{ data_sources: DataSourceView[] }>(
      `/api/v1/kbs/${kbId}/data-sources`,
    ),
  kbDataSourcesAvailable: (kbId: string) =>
    request<{ data_sources: DataSourceView[] }>(
      `/api/v1/kbs/${kbId}/data-sources/available`,
    ),
  /** Mount. **A non-empty `schema_error` still means the mount succeeded** -- the source
   *  really is mounted, only its table structure was not ingested, so Ask cannot see which
   *  tables exist. The same event goes to the alert centre */
  mountDataSource: (kbId: string, dsId: string) =>
    request<{
      ok: boolean;
      schema_tables: number;
      schema_error?: string | null;
    }>(`/api/v1/kbs/${kbId}/data-sources/${dsId}`, {
      method: "PUT",
    }),
  unmountDataSource: (kbId: string, dsId: string) =>
    request<{ ok: boolean }>(`/api/v1/kbs/${kbId}/data-sources/${dsId}`, {
      method: "DELETE",
    }),
  exploreMappings: (kbId: string) =>
    request<{ ok: boolean }>(`/api/v1/kbs/${kbId}/data-sources/explore`, {
      method: "POST",
    }),
  syncDataSourceSchema: (kbId: string, dsId: string) =>
    request<{ ok: boolean; schema_tables: number }>(
      `/api/v1/kbs/${kbId}/data-sources/${dsId}/sync-schema`,
      { method: "POST" },
    ),

  /** One page of the document library. **Filtered and paged on the server** -- this used to
   *  fetch the whole base at once and slice it in the frontend, and client-side filtering only
   *  filters what has already been fetched */
  documents: (
    kbId: string,
    opts: {
      source?: string;
      q?: string;
      graph?: string;
      limit: number;
      offset: number;
    },
  ) => {
    const p = new URLSearchParams({
      limit: String(opts.limit),
      offset: String(opts.offset),
    });
    if (opts.source) p.set("source", opts.source);
    if (opts.q) p.set("q", opts.q);
    if (opts.graph) p.set("graph", opts.graph);
    return request<{
      docs: Doc[];
      total: number;
      /** The three below **are counted over the source scope only**, unaffected by the
       *  name/status filters -- they are the reach of the bulk buttons */
      ready: number;
      extracting: number;
      failed: number;
    }>(`/api/v1/kbs/${kbId}/documents?${p}`);
  },
  /** One click to retry every document in this scope whose extraction failed */
  retryFailedDocs: (kbId: string, source?: string) =>
    request<{ queued: number; found: number }>(
      `/api/v1/kbs/${kbId}/documents/retry-failed${source ? `?source=${source}` : ""}`,
      { method: "POST" },
    ),
  /** Fetched for the whole base in one go: aggregated by (document × reason × object) the row
   *  count is small, which avoids one request per row */
  extractionDrops: (kbId: string) =>
    request<{ drops: ExtractionDrop[] }>(
      `/api/v1/kbs/${kbId}/extraction-drops`,
    ),
  upload: (kbId: string, files: File[], sourceId?: string) => {
    const form = new FormData();
    for (const f of files) form.append("files", f, f.name);
    const qs = sourceId ? `?source=${sourceId}` : "";
    return request<{ created: Doc[]; skipped: unknown[] }>(
      `/api/v1/kbs/${kbId}/documents${qs}`,
      { method: "POST", body: form },
    );
  },
  deleteDocument: (id: string) =>
    request<{ ok: boolean }>(`/api/v1/documents/${id}`, { method: "DELETE" }),

  search: (kbId: string, q: string) =>
    request<{ results: SearchResult[] }>(`/api/v1/kbs/${kbId}/search`, {
      method: "POST",
      body: JSON.stringify({ q }),
    }),

  settings: (workspaceId: string) =>
    request<LlmSettingsView>(`/api/v1/workspaces/${workspaceId}/settings`),
  saveSettings: (workspaceId: string, body: Record<string, unknown>) =>
    request<{ ok: boolean }>(`/api/v1/workspaces/${workspaceId}/settings`, {
      method: "PUT",
      body: JSON.stringify(body),
    }),
  graphOverview: (kbId: string, limit?: number) =>
    request<{
      nodes: GraphNode[];
      edges: GraphEdge[];
      /** How many there are in the base in total. **Not the same thing as nodes.length** --
       *  the canvas only draws the highest-degree batch, and showing that cap as the size was
       *  the most misleading thing about this endpoint */
      total_nodes?: number;
      total_edges?: number;
    }>(`/api/v1/kbs/${kbId}/graph/overview${limit ? `?limit=${limit}` : ""}`),
  /** The neighbourhood view **has no totals**: it is only a small slice by design, and saying
   *  "325 in all" would mean nothing. The two fields are declared optional so callers can
   *  share one type with the overview */
  graphNeighborhood: (kbId: string, entityId: string) =>
    request<{
      nodes: GraphNode[];
      edges: GraphEdge[];
      total_nodes?: number;
      total_edges?: number;
    }>(`/api/v1/kbs/${kbId}/graph/neighborhood?entity=${entityId}&hops=2`),
  /** Find entities by name. **The total comes back with them** -- "split rather than merge"
   *  produces piles of same-named entities, and with a fixed ten rows the one you want may not
   *  be among those ten at all */
  searchEntities: (kbId: string, q: string, limit = 10) =>
    request<{ entities: GraphNode[]; total: number }>(
      `/api/v1/kbs/${kbId}/entities?q=${encodeURIComponent(q)}&limit=${limit}`,
    ),
  entityDetail: (kbId: string, entityId: string) =>
    request<{
      entity: GraphNode;
      facts: EntityFact[];
      /** The derived ones get **a key of their own** and are not mixed into facts: in one
       *  shared list the user cannot tell "written in a document" from "inferred by the
       *  engine" */
      derived: DerivedFact[];
      /** Derivations that never landed (0017 §3): they are not even in `derived_facts`, so
       *  they get their own key too */
      blocked: BlockedDerivation[];
      /** Other entities with the same name. **Given as soon as the panel opens** -- the merge
       *  entry point has to grow where the duplicates are visible, not hide behind "rename it
       *  once" */
      same_name: GraphNode[];
    }>(`/api/v1/kbs/${kbId}/entities/${entityId}`),
  /** Belief-change history (the record-time axis): paged on the server */
  /** Manually correct an entity's type or name. Duplicate names are not blocked -- the
   *  returned same_name lets the UI ask whether to merge. */
  updateEntity: (
    kbId: string,
    entityId: string,
    body: { type_id?: string; canonical_name?: string },
  ) =>
    request<{ entity: GraphNode; same_name: GraphNode[] }>(
      `/api/v1/kbs/${kbId}/entities/${entityId}`,
      { method: "PATCH", body: JSON.stringify(body) },
    ),

  entityHistory: (kbId: string, entityId: string, page: number, per = 30) =>
    request<{ events: EntityHistoryEvent[]; total: number }>(
      `/api/v1/kbs/${kbId}/entities/${entityId}/history?page=${page}&per=${per}`,
    ),
  factEvidence: (kbId: string, factId: string) =>
    request<{ evidence: Evidence[] }>(
      `/api/v1/kbs/${kbId}/facts/${factId}/evidence`,
    ),
  /** The proof of one derived fact (0002 R2): premises in derivation order, each with its
   *  evidence, all the way down to the original sentence.
   *  Returns null once the derivation has gone invalid -- that is not an error */
  derivedProof: (kbId: string, derivedId: string) =>
    request<{ proof: Proof | null }>(
      `/api/v1/kbs/${kbId}/derived/${derivedId}/proof`,
    ),
  /** The proof chain of a derivation that never landed (0017 §3): the premises are in the
   *  violation's path */
  blockedProof: (kbId: string, violationId: string) =>
    request<{ steps: ProofStep[] | null }>(
      `/api/v1/kbs/${kbId}/violations/${violationId}/proof`,
    ),
  documentDetail: (id: string) =>
    request<{ document: Doc; chunks: ChunkFull[] }>(`/api/v1/documents/${id}`),
  extractDocument: (id: string) =>
    request<{ job_id: number }>(`/api/v1/documents/${id}/extract`, {
      method: "POST",
    }),
  reprocessDocument: (id: string) =>
    request<{ job_id: number }>(`/api/v1/documents/${id}/reprocess`, {
      method: "POST",
    }),
  /** Full re-extraction at source level (incremental semantics: existing decisions kept) */
  reExtractSource: (kbId: string, sourceId: string) =>
    request<{ queued: number }>(
      `/api/v1/kbs/${kbId}/sources/${sourceId}/re-extract`,
      {
        method: "POST",
      },
    ),
  /** Graph rebuild (liquidation semantics: wipe the graph layer, then re-extract everything;
   *  KB admin) */
  rebuildGraph: (kbId: string) =>
    request<{
      entities_removed: number;
      facts_removed: number;
      queued: number;
    }>(`/api/v1/kbs/${kbId}/graph/rebuild`, { method: "POST" }),

  ontology: (kbId: string) =>
    request<{
      entity_types: EntityTypeView[];
      relation_types: RelationTypeView[];
      misses: OntologyMiss[];
      /** The dismissed ones, together with the count they keep accumulating afterwards.
       *  Suppression still applies, they are just visible */
      dismissed_misses: OntologyMiss[];
    }>(`/api/v1/kbs/${kbId}/ontology`),
  createEntityType: (kbId: string, body: Record<string, unknown>) =>
    request<{ id: string }>(`/api/v1/kbs/${kbId}/ontology/entity-types`, {
      method: "POST",
      body: JSON.stringify(body),
    }),
  updateEntityType: (kbId: string, id: string, body: Record<string, unknown>) =>
    request<{ ok: boolean }>(
      `/api/v1/kbs/${kbId}/ontology/entity-types/${id}`,
      {
        method: "PATCH",
        body: JSON.stringify(body),
      },
    ),
  deleteEntityType: (kbId: string, id: string) =>
    request<{ ok: boolean }>(
      `/api/v1/kbs/${kbId}/ontology/entity-types/${id}`,
      {
        method: "DELETE",
      },
    ),
  typeEntities: (kbId: string, typeId: string, page: number, per = 12) =>
    request<{
      entities: { id: string; name: string; fact_count: number }[];
      total: number;
    }>(
      `/api/v1/kbs/${kbId}/ontology/entity-types/${typeId}/entities?page=${page}&per=${per}`,
    ),
  createRelationType: (kbId: string, body: Record<string, unknown>) =>
    request<{ id: string }>(`/api/v1/kbs/${kbId}/ontology/relation-types`, {
      method: "POST",
      body: JSON.stringify(body),
    }),
  updateRelationType: (
    kbId: string,
    id: string,
    body: Record<string, unknown>,
  ) =>
    request<{ ok: boolean }>(
      `/api/v1/kbs/${kbId}/ontology/relation-types/${id}`,
      {
        method: "PATCH",
        body: JSON.stringify(body),
      },
    ),
  deleteRelationType: (kbId: string, id: string) =>
    request<{ ok: boolean }>(
      `/api/v1/kbs/${kbId}/ontology/relation-types/${id}`,
      {
        method: "DELETE",
      },
    ),
  /** Proposals computed last time that nobody has ruled on yet (0049). A page refresh reads
   *  them, with no need to re-run the model */
  storedProposals: (kbId: string) =>
    request<OntologyProposals>(`/api/v1/kbs/${kbId}/ontology/proposals`),
  /** Someone has ruled on a proposal. The status changes, the row is not deleted -- a
   *  rejection leaves a trace, and the next Suggest round does not surface it again */
  decideProposal: (
    kbId: string,
    section: string,
    key: string,
    status: "adopted" | "rejected",
  ) =>
    request<{ ok: boolean }>(`/api/v1/kbs/${kbId}/ontology/proposals`, {
      method: "POST",
      body: JSON.stringify({ section, key, status }),
    }),
  dismissMiss: (kbId: string, kind: string, key: string) =>
    request<{ ok: boolean }>(`/api/v1/kbs/${kbId}/ontology/misses/dismiss`, {
      method: "POST",
      body: JSON.stringify({ kind, key }),
    }),
  restoreMiss: (kbId: string, kind: string, key: string) =>
    request<{ ok: boolean }>(`/api/v1/kbs/${kbId}/ontology/misses/restore`, {
      method: "POST",
      body: JSON.stringify({ kind, key }),
    }),
  /** reason is for humans only, and the human is at the other end of this very request -- so
      the language is stated by the caller, not by a backend setting (docs/decisions/0004).
      The language of description follows the knowledge base, which the server knows itself */
  suggestOntology: (kbId: string) =>
    request<OntologyProposals>(`/api/v1/kbs/${kbId}/ontology/suggest`, {
      method: "POST",
      body: JSON.stringify({ locale: lang }),
    }),

  /** Uploading an ontology file only computes a plan; not one byte is written to the
   *  database */
  previewOntologyImport: (kbId: string, file: File) => {
    const form = new FormData();
    form.append("file", file, file.name);
    return request<{ filename: string; plan: ImportPlan }>(
      `/api/v1/kbs/${kbId}/ontology/imports/preview`,
      { method: "POST", body: form },
    );
  },
  /** Execute the very plan you just looked at -- the server recomputes it, so both paths
   *  share the same code */
  applyOntologyImport: (kbId: string, file: File) => {
    const form = new FormData();
    form.append("file", file, file.name);
    return request<{ import_id: string; plan: ImportPlan }>(
      `/api/v1/kbs/${kbId}/ontology/imports`,
      { method: "POST", body: form },
    );
  },
  ontologyImports: (kbId: string) =>
    request<{ imports: OntologyImportView[] }>(
      `/api/v1/kbs/${kbId}/ontology/imports`,
    ),

  proposedPredicates: (kbId: string) =>
    request<{ forms: ProposedPredicate[] }>(
      `/api/v1/kbs/${kbId}/ontology/proposed-predicates`,
    ),
  /** What the last automatic ontology extension did, and whether it can still be undone
   *  (returns null once it has been undone cleanly) */
  lastAutoExtension: (kbId: string) =>
    request<{
      run: {
        at: string;
        relations: string[] | null;
        classes: string[] | null;
        facts_remapped: number | null;
        batches: string[];
      } | null;
    }>(`/api/v1/kbs/${kbId}/ontology/auto-extension`),
  /** Create the relation **and** adopt the predicate-less facts waiting on it -- the second
   *  half is where the payoff is */
  adoptPredicate: (
    kbId: string,
    body: {
      key: string;
      /** true = key refers to an existing relation/attribute; only rewrite facts, create no
       *  new type */
      existing?: boolean;
      /** An attribute takes the other rewrite path: the value has to be converted per
       *  datatype */
      kind?: "relation" | "attribute";
      datatype?: string;
      unit?: string;
      label?: string;
      temporal?: string;
      functional?: boolean;
      description?: string;
      forms: string[];
    },
  ) =>
    request<{
      id: string;
      remapped: number;
      batch: string;
      /** How many were not rewritten because the value would not convert to that datatype.
          Rewriting 3 and leaving 2 behind, then reporting only the first half, is reporting
          the good news and hiding the bad */
      unconvertible?: number;
    }>(`/api/v1/kbs/${kbId}/ontology/adopt-predicate`, {
      method: "POST",
      body: JSON.stringify(body),
    }),
  /** Undo one adoption: the newly written rows are invalidated and the old rows come back.
   *  The relation type stays */
  unadoptPredicate: (kbId: string, batchId: string) =>
    request<{ reverted: number }>(
      `/api/v1/kbs/${kbId}/ontology/adopt-predicate/${batchId}`,
      { method: "DELETE" },
    ),

  sources: (kbId: string) =>
    request<{ sources: SourceView[] }>(`/api/v1/kbs/${kbId}/sources`),
  createSource: (kbId: string, body: Record<string, unknown>) =>
    request<{ source: SourceView; ingest_token?: string | null }>(
      `/api/v1/kbs/${kbId}/sources`,
      {
        method: "POST",
        body: JSON.stringify(body),
      },
    ),
  sourceToken: (kbId: string, sourceId: string) =>
    request<{ ingest_token: string | null }>(
      `/api/v1/kbs/${kbId}/sources/${sourceId}/token`,
    ),
  rotateSourceToken: (kbId: string, sourceId: string) =>
    request<{ ingest_token: string }>(
      `/api/v1/kbs/${kbId}/sources/${sourceId}/rotate-token`,
      { method: "POST" },
    ),
  updateSource: (
    kbId: string,
    sourceId: string,
    body: Record<string, unknown>,
  ) =>
    request<{ source: SourceView }>(`/api/v1/kbs/${kbId}/sources/${sourceId}`, {
      method: "PATCH",
      body: JSON.stringify(body),
    }),
  deleteSource: (kbId: string, sourceId: string) =>
    request<{ ok: boolean }>(`/api/v1/kbs/${kbId}/sources/${sourceId}`, {
      method: "DELETE",
    }),
  cleanupMissing: (kbId: string, sourceId: string) =>
    request<{ deleted: number }>(
      `/api/v1/kbs/${kbId}/sources/${sourceId}/missing/cleanup`,
      {
        method: "POST",
      },
    ),
  syncSource: (kbId: string, sourceId: string) =>
    request<{ queued: boolean }>(
      `/api/v1/kbs/${kbId}/sources/${sourceId}/sync`,
      {
        method: "POST",
      },
    ),
  sourceRuns: (kbId: string, sourceId: string) =>
    request<{ runs: SyncRun[] }>(
      `/api/v1/kbs/${kbId}/sources/${sourceId}/runs`,
    ),
  documentExtractions: (docId: string) =>
    request<{ facts: ChunkFact[] }>(`/api/v1/documents/${docId}/extractions`),

  /** The **true counts** per review-queue bucket. Fetched apart from the list -- the list has
   *  a page cap, counting does not.
   *  The badge used to read the array length, while the endpoint always returned at most 100
   *  rows, so 164 items were written as 100. */
  review: (kbId: string, queue: ReviewQueue, limit: number, offset: number) =>
    request<{
      counts: ReviewCounts;
      queue: ReviewQueue;
      /** Only one page of the current bucket. The type differs per bucket, so call sites
       *  narrow it by queue */
      items: unknown[];
    }>(
      `/api/v1/kbs/${kbId}/review?queue=${queue}&limit=${limit}&offset=${offset}`,
    ),
  closeFact: (kbId: string, factId: string, validTo: string) =>
    request<{ ok: boolean }>(`/api/v1/kbs/${kbId}/facts/${factId}/close`, {
      method: "POST",
      body: JSON.stringify({ valid_to: validTo }),
    }),
  resolveConflict: (
    kbId: string,
    conflictId: string,
    body: { action: "close" | "keep" | "reject_new"; close_at?: string },
  ) =>
    request<{ ok: boolean }>(`/api/v1/kbs/${kbId}/conflicts/${conflictId}`, {
      method: "POST",
      body: JSON.stringify(body),
    }),
  /** Every pending item extracted from one memory (0015). An empty array = this sentence has
   *  nothing left waiting on a human */
  pendingForChunk: (kbId: string, chunkId: string) =>
    request<{ items: PendingFactItem[] }>(
      `/api/v1/kbs/${kbId}/review/pending?chunk_id=${chunkId}`,
    ),
  decidePending: (kbId: string, pendingId: string, action: "confirm" | "reject") =>
    request<{ ok: boolean; fact_id?: string; created?: boolean; conflicts?: number }>(
      `/api/v1/kbs/${kbId}/review/pending/${pendingId}`,
      { method: "POST", body: JSON.stringify({ action }) },
    ),
  decideReview: (kbId: string, reviewId: string, action: "merge" | "keep") =>
    request<{ ok: boolean }>(`/api/v1/kbs/${kbId}/review/${reviewId}`, {
      method: "POST",
      body: JSON.stringify({ action }),
    }),
  /** Rule on one data-mapping definition (0011). The status changes, the row is not deleted
   *  -- a rejection leaves a trace, and the next exploration round stops proposing it */
  decideMapping: (
    kbId: string,
    mappingId: string,
    status: "confirmed" | "rejected",
  ) =>
    request<{ ok: boolean }>(
      `/api/v1/kbs/${kbId}/review/mappings/${mappingId}`,
      {
        method: "POST",
        body: JSON.stringify({ status }),
      },
    ),
  /** Run the consistency check. Returns synchronously -- pure computation, no model call and
   *  no network */
  runConsistencyCheck: (kbId: string) =>
    request<{
      edges: number;
      /** **Zero is not the same as zero**: with no axioms the conclusion is "there are no
       *  criteria", not "no contradictions found" */
      predicates_with_axioms: number;
      found: number;
      inserted: number;
      cleared: number;
      classes: number;
      /** The ontology's own contradictions are **returned separately** and not added into
       *  found: the two numbers are not the same kind of thing */
      defects_found: number;
      defects_new: number;
    }>(`/api/v1/kbs/${kbId}/consistency/check`, { method: "POST" }),
  /** Rule on one ontology defect. **Two ways out** -- it never looked at the data at all, so
   *  there is no "the data is wrong" option */
  decideDefect: (
    kbId: string,
    defectId: string,
    resolution: "fixed" | "accepted",
  ) =>
    request<{ ok: boolean }>(`/api/v1/kbs/${kbId}/review/defects/${defectId}`, {
      method: "POST",
      body: JSON.stringify({ resolution }),
    }),
  /** Run inference (R1). With the toggle off the backend returns inference_off */
  /** Type resolution: **compute only, write nothing**. The receipt carries the profile sent
   *  to retrieval -- when retrieval finds nothing, the first thing to look at is "what did we
   *  search with" */
  /** Manual merge: fold source into target. **The direction matters** -- source disappears
   *  and its facts move onto target; a merge can be rolled back as a whole (entity_merges
   *  keeps the snapshot) */
  mergeEntities: (kbId: string, source: string, target: string) =>
    request<{ ok: boolean }>(`/api/v1/kbs/${kbId}/entities/merge`, {
      method: "POST",
      body: JSON.stringify({ source, target }),
    }),
  typeResolutionPreview: (kbId: string) =>
    request<{ items: TypeSuggestion[] }>(
      `/api/v1/kbs/${kbId}/ontology/type-resolution/preview`,
      { method: "POST" },
    ),
  /** Run it and persist. Returned in three separate buckets: retyped automatically, left for
   *  a human, and the ones called "none of these" */
  typeResolutionApply: (kbId: string) =>
    request<ResolutionOutcome>(`/api/v1/kbs/${kbId}/ontology/type-resolution`, {
      method: "POST",
    }),
  /** Approve one "coarse class → fine class" pair and retype the entities that came with it.
   *  **What is approved is the class pair, what is changed is the entities** -- approve once
   *  and the same pair never reaches a human again */
  approveRefinement: (
    kbId: string,
    body: { from_type_id: string; to_type_id: string; entity_ids: string[] },
  ) =>
    request<{ retyped: number }>(
      `/api/v1/kbs/${kbId}/ontology/type-resolution/approve`,
      { method: "POST", body: JSON.stringify(body) },
    ),
  /** Undo a whole batch: put those entities back in their original class */
  typeResolutionUndo: (kbId: string, batchId: string) =>
    request<{ reverted: number }>(
      `/api/v1/kbs/${kbId}/ontology/type-resolution/${batchId}`,
      { method: "DELETE" },
    ),
  runInference: (kbId: string) =>
    request<{
      /** How many rules were compiled. Zero means "there are no rules", not "nothing could be
       *  derived" */
      rules: number;
      edges: number;
      derived: number;
      inserted: number;
      /** The ones invalidated because their premises went away */
      invalidated: number;
      /** How many predicates hit the per-predicate cap and were left unfinished */
      capped: number;
    }>(`/api/v1/kbs/${kbId}/inference/run`, { method: "POST" }),
  /** Rule on one axiom violation. Three ways out -- the third is unique to this bucket: the
   *  definition may be the wrong one */
  decideViolation: (
    kbId: string,
    violationId: string,
    resolution: ViolationResolution,
    opts: { closeAt?: string; factId?: string } = {},
  ) =>
    request<{ ok: boolean }>(
      `/api/v1/kbs/${kbId}/review/violations/${violationId}`,
      {
        method: "POST",
        body: JSON.stringify({
          resolution,
          close_at: opts.closeAt ?? null,
          fact_id: opts.factId ?? null,
        }),
      },
    ),
  confirmFact: (kbId: string, factId: string) =>
    request<{ ok: boolean }>(`/api/v1/kbs/${kbId}/facts/${factId}/confirm`, {
      method: "POST",
    }),
  rejectFact: (kbId: string, factId: string) =>
    request<{ ok: boolean }>(`/api/v1/kbs/${kbId}/facts/${factId}/reject`, {
      method: "POST",
    }),
  revertMerge: (kbId: string, mergeId: string) =>
    request<{ ok: boolean }>(`/api/v1/kbs/${kbId}/merges/${mergeId}/revert`, {
      method: "POST",
    }),
  reviewHistory: (kbId: string, page: number, per = 20) =>
    request<{ events: ReviewHistoryEvent[]; total: number }>(
      `/api/v1/kbs/${kbId}/review/history?page=${page}&per=${per}`,
    ),

  members: (workspaceId: string) =>
    request<Member[]>(`/api/v1/workspaces/${workspaceId}/members`),
  orgUsers: () => request<OrgUser[]>("/api/v1/users"),
  setMemberRole: (workspaceId: string, userId: string, role: string) =>
    request<{ ok: boolean }>(
      `/api/v1/workspaces/${workspaceId}/members/${userId}`,
      {
        method: "PUT",
        body: JSON.stringify({ role }),
      },
    ),
  removeMember: (workspaceId: string, userId: string) =>
    request<{ ok: boolean }>(
      `/api/v1/workspaces/${workspaceId}/members/${userId}`,
      {
        method: "DELETE",
      },
    ),

  testSettings: (workspaceId: string) =>
    request<{
      chat: { ok: boolean; reply?: string; error?: string };
      /** `null` = extraction has no endpoint of its own, so the row does not
       *  apply -- which is not the same as "tested and failed" */
      extract: { ok: boolean; reply?: string; error?: string } | null;
      embed: { ok: boolean; dim?: number; error?: string };
    }>(`/api/v1/workspaces/${workspaceId}/settings/test`, { method: "POST" }),
};

/** RAG conversation: SSE streaming. Returns an abort function. */
export const conversationsApi = {
  /** **Searchable and pageable**: titles repeat (ask the same question twice and they do),
   *  and with a fixed hundred rows the conversations past it simply do not exist in the UI.
   *  The search covers both the title and the message bodies -- what a person remembers is
   *  usually the sentence they asked, not the title */
  list: (kbId: string, q = "", limit = 30, offset = 0) => {
    const p = new URLSearchParams({
      limit: String(limit),
      offset: String(offset),
    });
    if (q.trim()) p.set("q", q.trim());
    return request<{ conversations: ConversationRow[]; total: number }>(
      `/api/v1/kbs/${kbId}/conversations?${p}`,
    );
  },
  /** Rename. The title is taken automatically from the first sentence, and a conversation
   *  drifting off it is the norm */
  rename: (kbId: string, id: string, title: string) =>
    request<{ ok: boolean }>(`/api/v1/kbs/${kbId}/conversations/${id}`, {
      method: "PATCH",
      body: JSON.stringify({ title }),
    }),
  detail: (kbId: string, id: string) =>
    request<{ messages: ConversationMessage[] }>(
      `/api/v1/kbs/${kbId}/conversations/${id}`,
    ),
  remove: (kbId: string, id: string) =>
    request<{ ok: boolean }>(`/api/v1/kbs/${kbId}/conversations/${id}`, {
      method: "DELETE",
    }),
};

export interface ChatHandlers {
  onConversation: (id: string) => void;
  onSources: (s: Source[]) => void;
  onStep: (s: ChatStep) => void;
  onDelta: (text: string) => void;
  onDone: () => void;
  onError: (message: string) => void;
  /** Attaching to an answer already in flight: this is what it looks like right now,
   *  **replace, do not append** */
  onSnapshot?: (s: { content: string; steps: ChatStep[]; sources: Source[] }) => void;
  /** No generation is running for this conversation -- the most common answer, not an
   *  error */
  onIdle?: () => void;
}

/** Attach to an answer that is still being generated (this is the path after a page refresh).
 *
 *  It reads the same event stream as `streamChat`, only with a snapshot at the front. */
export function reattachChat(
  kbId: string,
  conversationId: string,
  handlers: ChatHandlers,
): () => void {
  return consumeChatStream(
    (signal) =>
      fetch(`/api/v1/kbs/${kbId}/conversations/${conversationId}/stream`, {
        credentials: "include",
        signal,
      }),
    handlers,
  );
}

export function streamChat(
  kbId: string,
  body: { conversation_id?: string; message: string },
  handlers: ChatHandlers,
): () => void {
  return consumeChatStream(
    (signal) =>
      fetch(`/api/v1/kbs/${kbId}/chat`, {
        method: "POST",
        credentials: "include",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(body),
        signal,
      }),
    handlers,
  );
}

/** There is only one way to read the SSE: the requests are opened differently, but everything
 *  after that is handled identically. */
function consumeChatStream(
  open: (signal: AbortSignal) => Promise<Response>,
  handlers: ChatHandlers,
): () => void {
  const controller = new AbortController();
  (async () => {
    try {
      const res = await open(controller.signal);
      if (!res.ok || !res.body) {
        let message = res.statusText;
        try {
          const body = (await res.json()) as { error?: string };
          if (body.error) message = body.error;
        } catch {
          /* ignore */
        }
        handlers.onError(message);
        return;
      }
      const reader = res.body.getReader();
      const decoder = new TextDecoder();
      let buf = "";
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        buf += decoder.decode(value, { stream: true });
        let idx: number;
        while ((idx = buf.indexOf("\n\n")) >= 0) {
          const frame = buf.slice(0, idx);
          buf = buf.slice(idx + 2);
          let event = "message";
          let data = "";
          for (const line of frame.split("\n")) {
            if (line.startsWith("event:")) event = line.slice(6).trim();
            else if (line.startsWith("data:")) data += line.slice(5).trim();
          }
          if (event === "conversation")
            handlers.onConversation((JSON.parse(data) as { id: string }).id);
          else if (event === "sources")
            handlers.onSources(JSON.parse(data || "[]"));
          else if (event === "step")
            handlers.onStep(JSON.parse(data) as ChatStep);
          else if (event === "delta")
            handlers.onDelta((JSON.parse(data) as { text: string }).text);
          else if (event === "snapshot") handlers.onSnapshot?.(JSON.parse(data));
          else if (event === "idle") handlers.onIdle?.();
          else if (event === "done") handlers.onDone();
          else if (event === "error") handlers.onError(data);
        }
      }
      handlers.onDone();
    } catch (e) {
      if (!controller.signal.aborted) handlers.onError(String(e));
    }
  })();
  return () => controller.abort();
}
