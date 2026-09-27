use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Organization {
    pub id: Uuid,
    pub name: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct User {
    pub id: Uuid,
    pub org_id: Uuid,
    pub email: String,
    #[serde(skip_serializing)]
    pub password_hash: String,
    pub display_name: String,
    /// Deployment administrator (the first user to register on this deployment)
    pub is_admin: bool,
    pub created_at: DateTime<Utc>,
}

/// Workspace member view (used by the member management page).
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct MemberView {
    pub user_id: Uuid,
    pub email: String,
    pub display_name: String,
    pub role: String,
    pub is_admin: bool,
}

/// Users in this deployment (used by the person picker when adding members).
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct OrgUser {
    pub id: Uuid,
    pub email: String,
    pub display_name: String,
    pub is_admin: bool,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Workspace {
    pub id: Uuid,
    pub org_id: Uuid,
    pub name: String,
    pub created_at: DateTime<Utc>,
}

/// Member roles, ordered by permission, highest first. Stored in the database as lowercase text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Viewer,
    Editor,
    Admin,
    Owner,
}

impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::Owner => "owner",
            Role::Admin => "admin",
            Role::Editor => "editor",
            Role::Viewer => "viewer",
        }
    }

    pub fn parse(s: &str) -> Option<Role> {
        match s {
            "owner" => Some(Role::Owner),
            "admin" => Some(Role::Admin),
            "editor" => Some(Role::Editor),
            "viewer" => Some(Role::Viewer),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Document {
    pub id: Uuid,
    pub kb_id: Uuid,
    pub source_id: Option<Uuid>,
    pub filename: String,
    pub mime: String,
    pub size_bytes: i64,
    pub sha256: String,
    /// pending → parsing → indexing → embedding → ready | failed
    pub status: String,
    pub error: Option<String>,
    pub doc_time: Option<DateTime<Utc>>,
    pub doc_time_source: String,
    /// Graph extraction status: none → queued → extracting → done | failed
    pub graph_status: String,
    /// Why extraction failed (only when it did). A column apart from error -- that one belongs to
    /// the parse pipeline and set_status wipes it; sharing one column has them erase each other.
    pub graph_error: Option<String>,
    pub text_len: i32,
    pub chunk_count: i32,
    pub tags: Vec<String>,
    /// Logical identity inside the source (relative path / url / rss guid / api external_id);
    /// NULL for uploads
    pub external_key: Option<String>,
    /// watch_folder sync found the source file gone (document kept by default, only flagged)
    pub missing_since: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// An ingestion source ("a source is a folder": a container + scheduled sync).
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Source {
    pub id: Uuid,
    pub kb_id: Uuid,
    /// upload | watch_folder | url | rss | api
    pub kind: String,
    pub name: String,
    /// kind-specific config: watch_folder {path} / url {urls:[..]} / rss {feed_url}
    pub config: serde_json::Value,
    /// lucide icon name (when NULL the frontend picks a default from kind)
    pub icon: Option<String>,
    /// NULL = manual sync only (mutually exclusive with sync_cron)
    pub sync_interval_minutes: Option<i32>,
    /// Standard 5-field cron (server local timezone; mutually exclusive with sync_interval_minutes)
    pub sync_cron: Option<String>,
    pub last_sync_at: Option<DateTime<Utc>>,
    /// never | queued | running | ok | failed
    pub last_sync_status: String,
    pub last_sync_error: Option<String>,
    pub last_sync_added: i32,
    /// Push secret for api sources (plaintext; read through a dedicated Editor-gated endpoint,
    /// never included in list responses)
    #[serde(skip_serializing)]
    pub ingest_token: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// The keys in a source's config that **are used to authenticate**. Credentials go in but never
/// come out: list responses and create / update responses strip them, on update a client that sends
/// nothing or an empty string keeps the value already in the database, and they never land in the
/// audit log.
///
/// **One table, four consumers.** That rule used to hold for exactly one key, `auth_header`, while
/// the object-store, WebDAV and Notion secrets each went out verbatim to every single Viewer
/// (#246). When you add a connector, **add it here first**, then write the code that reads it.
/// Things like `username` / `account_name` / `access_key_id` are identity labels: on their own they
/// authenticate nothing, and they stay so the UI can show "which account is this".
pub const SOURCE_SECRET_KEYS: &[&str] = &[
    "auth_header",
    "token",
    "password",
    "secret_access_key",
    "account_key",
    "service_account_key",
];

impl Source {
    /// This source minus its credentials -- every `Source` going back to a client passes here
    pub fn without_secrets(mut self) -> Self {
        if let Some(obj) = self.config.as_object_mut() {
            for key in SOURCE_SECRET_KEYS {
                obj.remove(*key);
            }
        }
        self
    }
}

/// The kinds of source. **Defined once, consumed in three places**: the allow-list at creation
/// time, the dispatch at sync time (an exhaustive match on the enum, so adding one forces you to
/// decide how it syncs), and the frontend's dropdown (`web/src/sourceKinds.ts`, checked against
/// this by a test in `utopia-store`).
///
/// Two hand-written lists on the backend used to drift apart on their own: five connectors got
/// sync branches and made it into the UI, but never into the creation allow-list, so you could
/// pick them in the UI and creating one answered "kind must be one of…" (#247). Variant order is
/// the order in the dialog; the string form is generated by strum as snake_case, no longer by hand
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    strum::EnumIter,
    strum::IntoStaticStr,
    strum::EnumString,
)]
#[strum(serialize_all = "snake_case")]
pub enum SourceKind {
    Folder,
    Url,
    Rss,
    GithubIssues,
    JiraIssues,
    S3,
    AzureBlob,
    Gcs,
    Webdav,
    Notion,
    Api,
    Custom,
    /// The memory source every knowledge base comes with; cannot be created or deleted (0015)
    Memory,
    /// The default for `sources.kind` in old data; it has no UI of its own
    Upload,
}

impl SourceKind {
    pub fn as_str(self) -> &'static str {
        self.into()
    }

    pub fn parse(s: &str) -> Option<Self> {
        s.parse().ok()
    }

    pub fn all() -> impl Iterator<Item = Self> {
        <Self as strum::IntoEnumIterator>::iter()
    }

    /// The ones a human can create from the UI: everything but `memory` and `upload`
    pub fn creatable_by_hand(self) -> bool {
        !matches!(self, Self::Memory | Self::Upload)
    }

    pub fn creatable() -> impl Iterator<Item = Self> {
        Self::all().filter(|k| k.creatable_by_hand())
    }
}

/// A source sync run (the channel's audit history).
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct SyncRun {
    pub id: Uuid,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    /// running | ok | failed
    pub status: String,
    pub created_docs: i32,
    pub updated_docs: i32,
    pub error: Option<String>,
}

/// What extraction produced from a chunk (document viewer right column: what this chunk yielded).
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct ChunkFactView {
    pub chunk_id: Uuid,
    pub fact_id: Uuid,
    pub subject_id: Uuid,
    pub subject: String,
    /// Falls back to the source's own wording when the ontology did not recognise the relation;
    /// None when neither can be produced (that is what older historical data looks like)
    pub predicate: Option<String>,
    pub inferred: bool,
    pub object_id: Option<Uuid>,
    pub object: Option<String>,
    pub valid_from: Option<DateTime<Utc>>,
    pub valid_to: Option<DateTime<Utc>>,
    pub confidence: f32,
}

/// Source list view (with document counts).
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct SourceView {
    pub id: Uuid,
    pub kind: String,
    pub name: String,
    pub config: serde_json::Value,
    pub icon: Option<String>,
    pub sync_interval_minutes: Option<i32>,
    pub sync_cron: Option<String>,
    pub last_sync_at: Option<DateTime<Utc>>,
    pub last_sync_status: String,
    pub last_sync_error: Option<String>,
    pub last_sync_added: i32,
    pub doc_count: i64,
    /// Documents already marked "not in source" (url full-set reconciliation / custom tombstones)
    pub missing_count: i64,
}

/// Audit event view (with actor display name; NULL once the account is deleted). Audit display only.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct AuditEventView {
    pub id: Uuid,
    pub action: String,
    pub target_kind: String,
    pub target_id: Option<Uuid>,
    pub detail: serde_json::Value,
    /// NULL = the engine acted by itself (adjudicator merges, consistency checks, materialized
    /// inference...). The UI uses it to tell "nobody" apart from "the person was removed": the
    /// latter still has an actor_id, it just cannot resolve a display name
    pub actor_id: Option<Uuid>,
    pub actor_name: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// A row for the account-level "my knowledge bases" list (the membership row may be absent: an
/// open base is entered on deployment identity, with no matrix record).
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct MyKbInfo {
    pub kb_id: Uuid,
    pub member_role: Option<String>,
    pub joined_at: Option<DateTime<Utc>>,
    pub added_by_name: Option<String>,
    pub doc_count: i64,
    pub member_count: i64,
}

/// A Chat conversation row (the left-column list).
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct ConversationView {
    pub id: Uuid,
    pub title: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub message_count: i64,
}

/// A Chat message (including the persisted action trail and citations, for replaying history).
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct ConversationMessage {
    pub id: Uuid,
    pub role: String,
    pub content: String,
    pub steps: serde_json::Value,
    pub sources: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

/// The chunk view search results use (with document information).
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct ChunkView {
    pub id: Uuid,
    pub document_id: Uuid,
    pub seq: i32,
    pub text: String,
    pub filename: String,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct LlmSettings {
    pub workspace_id: Uuid,
    pub chat_base_url: Option<String>,
    #[serde(skip_serializing)]
    pub chat_api_key: Option<String>,
    pub chat_model: Option<String>,
    pub embed_base_url: Option<String>,
    #[serde(skip_serializing)]
    pub embed_api_key: Option<String>,
    pub embed_model: Option<String>,
    pub embed_dim: Option<i32>,
    pub updated_at: DateTime<Utc>,
}

impl LlmSettings {
    pub fn chat_ready(&self) -> bool {
        self.chat_base_url.is_some() && self.chat_model.is_some()
    }
    pub fn embed_ready(&self) -> bool {
        self.embed_base_url.is_some() && self.embed_model.is_some()
    }
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct EntityType {
    pub id: Uuid,
    pub kb_id: Uuid,
    pub key: String,
    pub label: String,
    pub color: String,
    /// Graph node shape: circle | square
    pub shape: String,
    pub builtin: bool,
    /// subClassOf hierarchy (axiom reasoning lights it up in P4; the editor maintains data first)
    /// All parent classes (subClassOf can be multiple: FOAF's Person is both Agent and SpatialThing)
    pub parents: Vec<Uuid>,
    /// Which branch it hangs under when the left column draws the tree. No semantics, display only
    pub primary_parent: Option<Uuid>,
    /// Global identity from an OWL import. NULL for hand-built classes; re-imports match on this,
    /// not on key -- change rdfs:label upstream once and the derived key changes with it, so key
    /// matching would build the same class again as a new one
    pub iri: Option<String>,
    /// Semantic guidance: goes into the extraction prompt (what counts as this class, examples)
    pub description: String,
}

/// Ontology editor: an entity instance row under some class (for the detail pane's instance list).
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct EntityInstance {
    pub id: Uuid,
    pub name: String,
    pub fact_count: i64,
}

/// Ontology editor view: type + usage count (for delete protection and UX hints).
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct EntityTypeView {
    pub id: Uuid,
    pub key: String,
    pub label: String,
    pub color: String,
    pub shape: String,
    pub builtin: bool,
    /// All parent classes (subClassOf can be multiple: FOAF's Person is both Agent and SpatialThing)
    pub parents: Vec<Uuid>,
    /// Which branch it hangs under when the left column draws the tree. No semantics, display only
    pub primary_parent: Option<Uuid>,
    /// The classes disjoint with this one: **a declaration of "cannot be both at once"**. The
    /// consistency check uses it to report unsatisfiable classes -- inherit from two disjoint
    /// ancestors and the class can never have an instance (0002)
    pub disjoint: Vec<Uuid>,
    pub description: String,
    pub usage: i64,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct RelationTypeView {
    pub id: Uuid,
    pub key: String,
    pub label: String,
    pub temporal: String,
    pub functional: bool,
    pub inverse_functional: bool,
    /// The other four OWL axioms. **Every test the reasoner makes is in here** (0002) -- they used
    /// to arrive only by importing OWL, so anyone building an ontology in the UI could never switch
    /// that machine on
    pub is_transitive: bool,
    pub is_symmetric: bool,
    pub is_asymmetric: bool,
    pub is_irreflexive: bool,
    /// The two that point at another relation. **Must be sent back to the UI** -- the dropdown has
    /// to show what is currently selected, otherwise the form opens empty every time and one edit
    /// wipes out what was declared
    pub inverse_of: Option<Uuid>,
    pub sub_property_of: Option<Uuid>,
    pub builtin: bool,
    pub description: String,
    /// relation (the object is an entity) | attribute (the object is a literal value)
    pub kind: String,
    /// Classes that may be the subject. At least one for an attribute; may be empty for a
    /// relation (undeclared = unrestricted)
    pub domains: Vec<Uuid>,
    /// Classes that may be the object. **Only meaningful for a relation** -- an attribute's range
    /// is a literal type, which lands in datatype
    pub ranges: Vec<Uuid>,
    /// attribute only: text | number | date | bool
    pub datatype: Option<String>,
    pub unit: Option<String>,
    pub usage: i64,
}

/// Unmatched-extraction counts (the signal behind ontology extension suggestions).
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct OntologyMiss {
    pub kind: String,
    pub key: String,
    pub example: Option<String>,
    pub count: i32,
}

/// A surface predicate waiting to be claimed: the text said it, the ontology has no relation for
/// it, and the fact was downgraded to related_to. Unlike `OntologyMiss`, which is a plain count,
/// this one is attached to concrete facts -- so on adoption it can say "57 facts will be
/// reclassified" and actually go and change them.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct ProposedPredicate {
    pub form: String,
    /// How many live related_to facts came out of this wording
    pub fact_count: i64,
    /// How many documents it appears in. Something that showed up in only one document is that
    /// document's wording, not this organisation's vocabulary -- auto-extension sets its threshold
    /// on this, while for a manual proposal it is a hint only and never blocks
    pub doc_count: i64,
    /// One example ("Dino Crisis (Steam) → GeForce NOW"), so a reader can tell at a glance what
    /// relation this is
    pub example: Option<String>,
}

/// The record of one OWL import. The file itself is stored content-addressed in blob; this row is
/// only the ledger entry. `summary` notes what that projection did, including the axioms that were
/// **not projected yet** -- when a consumer for those is added later, this is how you know which
/// imports are worth re-running.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct OntologyImportView {
    pub id: Uuid,
    pub filename: String,
    pub format: String,
    pub byte_size: i64,
    pub summary: serde_json::Value,
    pub imported_at: DateTime<Utc>,
    pub imported_by_name: Option<String>,
}

/// One model's concurrency ceiling. The constraint comes from the provider's rate limit, and that
/// is counted per (base_url, model) -- a local Ollama and a hosted API sharing one number was never
/// right to begin with.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct ModelLimit {
    pub base_url: String,
    pub model: String,
    pub max_concurrent: i32,
}

/// An entity type waiting to be claimed: the model proposed it, the ontology does not have it, and
/// the entities were downgraded to concept as a result. The mirror of `ProposedPredicate` -- it is
/// attached to concrete entities, so on adoption it can say "43 will be reclassified" and actually
/// go and change them, instead of only creating an empty class.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct ProposedType {
    pub form: String,
    pub entity_count: i64,
    /// One example name, so a reader can tell at a glance what class this is
    pub example: Option<String>,
}

/// Extraction drop signals: facts that were extracted but never landed, and why.
/// Kept apart from `OntologyMiss` -- that one says "your ontology is missing these" (its reader
/// maintains the ontology), this one says "these facts did not land" (its reader uploaded the
/// document).
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct ExtractionDrop {
    pub document_id: Uuid,
    pub reason: String,
    pub detail: String,
    pub count: i32,
    pub example: Option<String>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct RelationType {
    pub id: Uuid,
    pub kb_id: Uuid,
    pub key: String,
    pub label: String,
    /// state | event | eternal
    pub temporal: String,
    /// Unique on the subject side: at any one moment a subject has at most one object
    pub functional: bool,
    /// Unique on the object side: at any one moment an object has at most one subject (a project
    /// has only one person who leads it)
    pub inverse_functional: bool,
    pub builtin: bool,
    /// Semantic guidance: goes into the extraction prompt
    pub description: String,
    /// Global identity from an OWL import; NULL for hand-built ones. Re-imports match on this, not
    /// on key -- change rdfs:label upstream once and the derived key moves with it, so key matching
    /// would take the same thing for a new one
    pub iri: Option<String>,
    /// relation (object is an entity) | attribute (object is a literal, via facts.object_value)
    pub kind: String,
    /// Classes that may be the subject (multi-valued: several rdfs:domain per property is normal)
    pub domains: Vec<Uuid>,
    /// Classes that may be the object. Only meaningful for a relation
    pub ranges: Vec<Uuid>,
    /// attribute only: text | number | date | bool
    pub datatype: Option<String>,
    pub unit: Option<String>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Entity {
    pub id: Uuid,
    pub kb_id: Uuid,
    /// `None` = not decided yet. See `docs/decisions/0009` -- it is not a class, it is the state
    /// "the extractor found something, but the ontology has no class for it"
    pub type_id: Option<Uuid>,
    pub canonical_name: String,
    pub aliases: Vec<String>,
    pub merged_into: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A node as the graph renders it.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct GraphNode {
    pub id: Uuid,
    pub name: String,
    /// Type key. **May be absent** (0009: undecided means NULL), which is how the frontend knows
    /// to show "untyped" instead of inventing a name
    pub type_key: Option<String>,
    pub type_label: Option<String>,
    pub color: String,
    /// Type shape: circle | square
    pub shape: String,
    pub degree: i64,
    /// The disambiguating suffix shown when two names coexist (the owning organisation, say)
    pub disambiguator: Option<String>,
}

/// An edge as the graph renders it (= one live fact).
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct GraphEdge {
    pub id: Uuid,
    pub source: Uuid,
    pub target: Uuid,
    /// Falls back to the source's wording when the ontology has no matching relation (see
    /// `facts.predicate_id`). None when neither source can supply one -- that is data from before
    /// add_evidence started recording the source wording
    pub predicate: Option<String>,
    pub label: Option<String>,
    /// true = this edge's name comes from the text, not from a relation the ontology recognised.
    /// The UI has to show it in a way that makes the difference visible
    pub inferred: bool,
    /// true = this edge was **derived**, not asserted by anyone (R1, it lives in `derived_facts`).
    ///
    /// **Not the same thing as `inferred`**, however close the two words sound: that one says "the
    /// name comes from the text rather than the ontology", this one says "nobody stated this edge
    /// at all, the engine derived it"
    pub derived: bool,
    /// The rule that derived it (`transitive` / `symmetric` / `inverse` / `sub_property`); None
    /// for asserted edges.
    ///
    /// **The UI needs to single out `inverse`**: `A works_at B` and the `B employs A` derived from
    /// it are two ways of saying the same thing, and drawing two edges only draws the redundancy
    /// twice; whereas `sub_property` derives a different fact at a different granularity, and each
    /// of those deserves its own edge
    pub rule: Option<String>,
    /// The premise facts used to derive it (in proof order). Empty for asserted edges.
    ///
    /// **The UI needs this to pin down the origin when it merges edges.** Matching on "the same
    /// pair of nodes" alone will hang `contains` off the `allied_with` that happens to join those
    /// two points as well -- that statement belongs to `part_of`, and the result of attaching it
    /// wrongly looks perfectly normal, which is exactly the hardest kind to spot
    pub premises: Vec<Uuid>,
    pub valid_from: Option<DateTime<Utc>>,
    pub valid_to: Option<DateTime<Utc>>,
    pub confidence: f32,
    /// Disputed (0017 §3): an open axiom violation or temporal conflict points at it. The whole
    /// edge is drawn in the warning colour -- a ring on the node with the edge still grey is
    /// something you cannot make out from the corner of your eye
    pub contested: bool,
    /// Ghost edge (0017 §3): a derivation that **did not land** -- derived, then blocked by an
    /// assertion. `id` is the id of that `derived_contradiction` violation, not of any fact;
    /// `derived` is true as well, so it follows the derived toggle
    pub blocked: bool,
}

/// A fact row on the entity detail page (the timeline).
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct EntityFact {
    pub id: Uuid,
    /// out = this entity is the subject; in = it is the object
    pub direction: String,
    /// Falls back to the source's own wording when the ontology did not recognise the relation;
    /// None when neither can be produced (that is what older historical data looks like)
    pub predicate_key: Option<String>,
    pub predicate_label: Option<String>,
    /// true = this fact's name comes from the text, not from a relation the ontology recognised.
    /// The UI has to show it in a way that makes the difference visible
    pub inferred: bool,
    /// The relation's temporal class (point/state/eternal). No predicate, nothing to ask, so None
    pub temporal: Option<String>,
    pub other_id: Option<Uuid>,
    pub other_name: Option<String>,
    /// Literal-valued object (attribute facts / Ask mappings): {"value":…,"unit":…} or {"summary":…}
    pub object_value: Option<serde_json::Value>,
    pub valid_from: Option<DateTime<Utc>>,
    pub valid_to: Option<DateTime<Utc>>,
    /// Precision describes the granularity of **the dates this fact actually has**. None when
    /// neither end has a date -- this used to be NOT NULL DEFAULT day, so facts with no dates claimed
    /// day precision too (see `facts.valid_from_precision`)
    /// Granularity of the start: year | month | day. None when there is no valid_from
    pub valid_from_precision: Option<String>,
    /// Granularity of the end, plus one extra value `unknown` -- **the text says it ended, but not
    /// on what day**. Only when `valid_to` and this are both None does it mean "still ongoing"
    /// (see `facts.valid_to_precision`)
    pub valid_to_precision: Option<String>,
    pub confidence: f32,
    pub evidence_count: i64,
    /// All the evidence sits on older versions of the source documents (not confirmed by the
    /// current content; this does not mean the fact is void)
    pub stale: bool,
    /// A correction row (on the supersedes chain): the interval was closed by engine
    /// reconciliation or a human decision, not by the extracted text
    pub corrected: bool,
    /// The newest document time in the evidence set -- an open fact's "last confirmed at"
    /// (making staleness visible)
    pub last_evidence_time: Option<DateTime<Utc>>,
    /// Disputed (0017 §3): `{ kind, ref_id, derived? }` -- which kind (the violation's kind, or
    /// `temporal_conflict`), the id of that item in Review, and the derived statement when a
    /// derivation collides with an assertion. Only the most recent one is reported per fact; the
    /// row is **not dimmed**, the assertion is still alive
    pub contested: Option<serde_json::Value>,
}

/// One change in what we believe about an entity (an event on the record timeline, orthogonal to
/// EntityFact's validity timeline).
///
/// The ledger is append-only, so everything we ever believed is kept: one fact row yields at most
/// two events -- the write (asserted / corrected) and the invalidation (rejected, and only when no
/// later correction row exists; if one does, that corrected row already explains this death and we
/// do not record it twice).
///
/// **Not every event comes from a fact.** A retype (`retyped` / `retype_reverted`) comes from
/// `entity_retypes`: it has no predicate, no counterpart and no direction, which is why those
/// fields are nullable. This used to hold fact events only, so "the type changed" left no trace at
/// all in an entity's history -- 0001 P3a notes that "revocable does not mean it will be revoked",
/// and a mistake does not surface on its own.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct EntityHistoryEvent {
    /// Only on fact events. None for retype events
    pub fact_id: Option<Uuid>,
    /// The record-time moment the event happened (write = recorded_at, invalidation =
    /// invalidated_at, retype = entity_retypes.created_at / reverted_at)
    pub at: DateTime<Utc>,
    /// asserted (first assertion) | corrected (interval corrected) | rejected (belief overturned)
    /// | merged (merged into another assertion) | retyped (type changed) | retype_reverted (undone)
    pub kind: String,
    pub direction: Option<String>,
    pub predicate_label: Option<String>,
    pub other_name: Option<String>,
    pub object_value: Option<serde_json::Value>,
    pub valid_from: Option<DateTime<Utc>>,
    pub valid_to: Option<DateTime<Utc>>,
    /// Precision describes the granularity of **the dates this fact actually has**. None when
    /// neither end has a date -- this used to be NOT NULL DEFAULT day, so facts with no dates claimed
    /// day precision too (see `facts.valid_from_precision`)
    /// Granularity of the start: year | month | day. None when there is no valid_from
    pub valid_from_precision: Option<String>,
    /// Granularity of the end, plus one extra value `unknown` -- **the text says it ended, but not
    /// on what day**. Only when `valid_to` and this are both None does it mean "still ongoing"
    /// (see `facts.valid_to_precision`)
    pub valid_to_precision: Option<String>,
    pub confidence: Option<f32>,
    /// The human who acted; NULL = the engine did it by itself (an extraction write / a temporal
    /// reconciliation closing an interval / a high-confidence retype)
    pub actor_name: Option<String>,
    /// The audit action that caused this change (fact.close / conflict.close_old / fact.reject …)
    pub action: Option<String>,
    pub document_id: Option<Uuid>,
    pub filename: Option<String>,
    pub quote: Option<String>,
    /// The two ends of a retype event. The origin may be absent -- since 0009, "from no class to
    /// a class" is the most common retype of all
    pub from_type_label: Option<String>,
    pub to_type_label: Option<String>,
}

/// The changes of belief across the whole knowledge base inside a window of **record time**.
///
/// The same set of events as `EntityHistoryEvent`, differing in two places:
/// 1. the window is opened on the **belief axis** (recorded_at / invalidated_at) and is not pinned
///    to a single entity -- a question like "what changed last quarter" has no entity to ask about
///    up front;
/// 2. subject and object are both written out (`direction` is a notion that only exists when some
///    entity is the centre, and here there is no centre).
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct GraphChange {
    pub fact_id: Uuid,
    /// Where the event falls on the belief axis (write = recorded_at, void = invalidated_at)
    pub at: DateTime<Utc>,
    /// asserted (new assertion) | corrected (fixed the previous one) | rejected (overturned) |
    /// merged (folded into another)
    pub kind: String,
    pub subject_id: Uuid,
    pub subject_name: String,
    pub predicate_label: Option<String>,
    pub object_name: Option<String>,
    pub object_value: Option<serde_json::Value>,
    /// Which span of the **world axis** this assertion states -- orthogonal to `at`, don't mix them up
    pub valid_from: Option<DateTime<Utc>>,
    pub valid_to: Option<DateTime<Utc>>,
    /// Precision describes the granularity of **the dates this fact actually has**. None when
    /// neither end has a date -- this used to be NOT NULL DEFAULT day, so facts with no dates claimed
    /// day precision too (see `facts.valid_from_precision`)
    /// Granularity of the start: year | month | day. None when there is no valid_from
    pub valid_from_precision: Option<String>,
    /// Granularity of the end, plus one extra value `unknown` -- **the text says it ended, but not
    /// on what day**. Only when `valid_to` and this are both None does it mean "still ongoing"
    /// (see `facts.valid_to_precision`)
    pub valid_to_precision: Option<String>,
    pub confidence: f32,
    pub document_id: Option<Uuid>,
    pub filename: Option<String>,
    pub quote: Option<String>,
}

/// The entity summary for one side of a resolution review item.
#[derive(Debug, Clone, Serialize)]
pub struct ReviewSide {
    pub id: Uuid,
    pub name: String,
    /// None when no type was decided (0009). Colour has a default of its own -- the canvas cannot
    /// do without it
    pub type_label: Option<String>,
    pub color: String,
    pub disambiguator: Option<String>,
    pub degree: i64,
    pub top_facts: Vec<String>,
}

/// A resolution review item: a grey-area pair that may be the same entity.
#[derive(Debug, Clone, Serialize)]
pub struct ReviewItem {
    pub id: Uuid,
    pub score: f32,
    pub reason: Option<String>,
    /// adjudicating = waiting on the LLM's verdict; human = waiting on a person's final call
    pub stage: String,
    pub created_at: DateTime<Utc>,
    pub left: ReviewSide,
    pub right: ReviewSide,
}

/// A merge log row (the history section of the Review page).
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct MergeLogView {
    pub id: Uuid,
    pub source_name: String,
    pub target_name: String,
    /// NULL = merged automatically by the LLM
    pub merged_by_name: Option<String>,
    pub reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub reverted_at: Option<DateTime<Utc>>,
}

/// A low-confidence fact review row.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct FactReviewItem {
    pub id: Uuid,
    pub subject_name: String,
    pub predicate_label: Option<String>,
    pub object_name: Option<String>,
    pub valid_from: Option<DateTime<Utc>>,
    pub valid_to: Option<DateTime<Utc>>,
    pub confidence: f32,
    pub evidence_count: i64,
    pub quote: Option<String>,
}

/// The evidence for a fact (the quote + where it sits in the source).
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct EvidenceView {
    /// The predicate wording the model actually used in this chunk. Once an out-of-vocabulary
    /// predicate is downgraded to related_to, all that is left on the fact row is "related to" --
    /// the original meaning lives only here
    pub proposed_predicate: Option<String>,
    pub quote: Option<String>,
    pub chunk_id: Uuid,
    pub document_id: Uuid,
    pub filename: String,
    pub seq: i32,
    /// Which version of the document this evidence came from
    pub doc_version: i32,
    /// A newer version of the document exists (the evidence sits on the old one; this does not
    /// make the fact void)
    pub stale: bool,
}

/// A temporal conflict (the ones S3's automatic closing was not sure about): old fact vs new
/// fact, decided by a human on the Review page.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct ConflictView {
    pub id: Uuid,
    /// no_time | simultaneous | low_confidence
    pub reason: String,
    pub created_at: DateTime<Utc>,
    pub predicate_label: String,
    /// The full triple on both sides: in a subject-side conflict the object is what changed, in
    /// an object-side conflict the subject is
    pub old_fact_id: Uuid,
    pub old_subject: String,
    pub old_object: Option<String>,
    pub old_valid_from: Option<DateTime<Utc>>,
    pub new_fact_id: Uuid,
    pub new_subject: String,
    pub new_object: Option<String>,
    pub new_valid_from: Option<DateTime<Utc>>,
    pub new_confidence: f32,
}

/// The chunk view the document viewer uses.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct ChunkFull {
    pub id: Uuid,
    pub seq: i32,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct KnowledgeBase {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub name: String,
    /// `knowledge` | `memory` (an agent's memory space)
    pub kind: String,
    pub description: Option<String>,
    /// open = everyone, by their deployment role; restricted = visible only to the kb_members list
    pub visibility: String,
    /// The deployment's shared default space (the first base created): always open, undeletable
    pub is_default: bool,
    /// Whether the system may add a wording to the ontology by itself when extraction meets one
    /// the ontology does not have, and rewrite the facts that were waiting on it.
    /// On by default -- nobody chose the ten seed relations a new base starts with, and until
    /// someone fills the gaps by hand the graph is barely usable.
    /// Turning it off does not affect noticing: the unmatched counts accumulate and stay visible
    /// as before, they just become a proposal you click.
    pub auto_extend_ontology: bool,
    /// Which language the built-in ontology is seeded in, and which language new class / relation
    /// descriptions are written in (`en` | `zh`).
    /// **Follow the corpus, not the interface** -- the reader of a description is the model while
    /// it reads those documents.
    /// See docs/decisions/0004.
    /// Whether derived facts are written into the ledger (R1). **Off by default** -- this step adds
    /// things to the graph, and 0001 criterion 2 says "the ontology guides, it does not enforce":
    /// a declaration can be wrong, and the graph should not be changed by it while the user has
    /// said nothing
    pub materialize_inferences: bool,
    /// How often to re-derive (minutes). See `knowledge_bases.inference_interval_minutes`
    pub inference_interval_minutes: i32,
    /// When the last derivation run finished. **It answers "when did we last look", not "when did
    /// we last change something"**
    pub last_inference_at: Option<DateTime<Utc>>,
    pub ontology_lang: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A row of the KB membership matrix (the Members section of a base's Settings).
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct KbMemberView {
    pub user_id: Uuid,
    pub email: String,
    pub display_name: String,
    /// viewer | editor | admin
    pub role: String,
}

/// The data source list view for Ask: the connection string is never sent down (credentials go
/// in but never come out), only a host:port/db summary is exposed.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct DataSourceView {
    pub id: Uuid,
    pub name: String,
    pub engine: String,
    /// Connection summary (host:port/db, no credentials)
    pub summary: String,
    pub created_at: DateTime<Utc>,
    pub last_test_at: Option<DateTime<Utc>>,
    pub last_test_ok: Option<bool>,
}

/// One candidate ontology row that vector search turned up (class / relation / attribute).
///
/// `distance` is cosine distance: the smaller, the closer. It is handed to the caller as-is rather
/// than folded into a "similarity" first -- where the threshold belongs is for the consumer to
/// decide against its own data, and we do not normalise on its behalf.
#[derive(Debug, Clone, Serialize)]
pub struct TypeCandidate {
    pub id: Uuid,
    pub key: String,
    pub label: String,
    pub description: String,
    /// Only on relation rows: `relation` or `attribute`
    pub kind: Option<String>,
    pub distance: f32,
}

/// A **literal-valued** wording that got recorded but has no matching attribute in the ontology.
///
/// A pair with [`ProposedPredicate`]: that one is for objects pointing at an entity ("acquired"),
/// this one for objects that are literal values ("founding date = 2015"). The two must not be
/// mixed -- a proposal produces a different thing in each case (relation vs attribute), and the
/// consequence of mixing them is concrete: one `founding_date` would turn into an edge pointing
/// at a fake entity called "2015".
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct ProposedAttribute {
    pub form: String,
    pub fact_count: i64,
    pub doc_count: i64,
    /// One example value (`"2015"`, `1200`), so a reader can see at a glance what sort of number
    /// this is
    pub example: Option<String>,
    /// Which classes this wording **is actually attached to** (the subject's types).
    ///
    /// An attribute must declare a domain, and guessing the domain wrong costs hard: a subject
    /// whose type does not match gets the whole item dropped (`attr_domain_mismatch`). So we do
    /// not ask the model, we read it straight out of the data -- the facts are already there, and
    /// what class their subjects are is a fact, not a judgement
    pub domain_keys: Vec<String>,
}

/// What one definition looked like before a change to it.
///
/// **A whole-version snapshot, not a diff** (0006): what a read has to answer is "what was it at
/// the time", and a diff can only answer that by replaying from the beginning. `before` is the
/// `to_jsonb` of that row before the change, with id and kb_id taken out.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct MappingRevision {
    pub id: Uuid,
    pub before: serde_json::Value,
    /// Who changed it. **A bare foreign key + soft-deleted users**, so attribution is not lost
    /// when someone leaves; it is only NULL if the row really was hard-deleted
    pub changed_by_name: Option<String>,
    pub changed_at: DateTime<Utc>,
}

/// One mapping in the semantic layer: a business concept → a data asset definition (see
/// `docs/decisions/0011`).
///
/// **These fields are columns, not keys inside JSON.** This used to be a `mapped_to` fact with all
/// of it crammed into `object_value` -- so "which concepts map to the orders table" meant digging
/// through JSON, and the constraint "one row per concept per source" was out of the database's
/// reach.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct ConceptMapping {
    pub id: Uuid,
    pub concept_id: Uuid,
    /// The concept's name. The read side always wants it (the Ask prompt, the Review list), and
    /// looking it up in the entity table every time is a wasted trip
    pub concept_name: String,
    /// The mounted data source. The same concept may be defined differently on different sources,
    /// which is supported on purpose
    pub source: String,
    pub table_name: Option<String>,
    pub expr: Option<String>,
    pub sql: Option<String>,
    pub unit: Option<String>,
    pub summary: Option<String>,
    /// A derived metric ("conversion rate = orders / visits"): computed, not a column in a table
    pub derived: bool,
    /// proposed | confirmed | rejected
    ///
    /// **A state, not a confidence.** This used to borrow a fact's confidence to say "proposed
    /// 0.6 / confirmed 1.0", which encodes a two-valued state as a float and, on the way, drops it
    /// into the "low-confidence facts" queue
    pub status: String,
}

/// One axiom violation, with the triple text needed to display it (see `axiom_violations`).
///
/// **Both facts are expanded into subject-predicate-object text**: the Review page has to let
/// someone see at a glance where the contradiction is, and two UUIDs show nothing at all. For the
/// reflexive kind the two are identical -- it is a single fact.
#[derive(Debug, Clone, Serialize)]
pub struct AxiomViolation {
    pub id: Uuid,
    /// self_loop | asymmetry | cycle | functional | signature | derived_contradiction
    pub kind: String,
    /// Which relation the test came from. If a human decides "the axiom is wrong", this is the
    /// way into the ontology to change it
    pub predicate: Option<String>,
    pub left_fact: Uuid,
    pub left_text: String,
    pub right_fact: Uuid,
    pub right_text: String,
    /// Cycle length (ends included). 0 for the other three kinds -- the frontend uses it to decide
    /// whether to show "view the path"
    pub path_len: i32,
    pub detected_at: chrono::DateTime<chrono::Utc>,
    /// `derived_contradiction` only (0017): the triple that was derived -- it never landed in the
    /// database, so here is the only place it can be written out. Fields in `reasoning::run`. `{}`
    /// for every other kind
    pub detail: serde_json::Value,
    /// A review hint (0017 §2): `stale` (the old assertion has no end date), `duplicate` (there is
    /// an entity with the same name), `unsure` (extracted with low confidence). Only one is given,
    /// and none means empty
    pub hint: Option<String>,
    /// Every fact on the cycle, in order (empty for the other kinds). **An id for each**:
    /// retracting a fact means saying which one, and which fact on the cycle is the wrong one is
    /// something only a human can tell after looking (#202)
    pub path: Vec<ViolationFact>,
}

/// One fact within a violation: its id and its triple text
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ViolationFact {
    pub id: Uuid,
    pub text: String,
}

/// A place where the ontology contradicts itself (see `ontology_defects`).
///
/// **Not the same thing as [`AxiomViolation`]**: that one says "a fact clashes with a definition",
/// this one says "the definition does not stand up by itself". The latter is the more fundamental
/// of the two -- a self-contradictory ontology makes every conclusion of the former suspect.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct OntologyDefect {
    pub id: Uuid,
    /// symmetric_and_asymmetric | transitive_and_functional | subclass_cycle
    /// | disjoint_with_ancestor | inherits_disjoint | inverse_of_itself
    /// | inverse_not_mutual | sub_property_cycle | rules_disagree
    pub kind: String,
    /// `rules_disagree` only (0017): which two rules, which axiom they collide on, how many pairs,
    /// and a few examples
    pub detail: serde_json::Value,
    /// The label of the object at fault (class or predicate). Not found means it has been deleted
    pub subject_label: Option<String>,
    /// The other party: the class it is disjoint with
    pub other_label: Option<String>,
    /// Labels of the classes on the cycle, in order
    pub path_labels: Vec<String>,
    pub detected_at: chrono::DateTime<chrono::Utc>,
}

/// One derived fact together with its proof (the "Derived" tab on the entity panel).
///
/// **`premises` is the reason that tab exists**: without the premises, a derived edge and an
/// ordinary one look no different in the UI, and that is exactly what "inference polluting
/// knowledge" looks like.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct DerivedFactView {
    pub id: Uuid,
    pub subject_id: Uuid,
    pub subject: String,
    pub object_id: Uuid,
    pub object: String,
    pub predicate: String,
    /// transitive | symmetric -- which rule derived it
    pub rule: String,
    pub valid_from: Option<DateTime<Utc>>,
    pub valid_to: Option<DateTime<Utc>>,
    pub confidence: f32,
    pub derived_at: DateTime<Utc>,
    /// The immediate premises, expanded into triple text in derivation order
    pub premises: Vec<String>,
}

/// A derivation that **did not land** (0017 §3): derived, then it hit an assertion and was kept
/// out of the graph.
///
/// It has no id -- only what lands in the database gets one. Here the id of that
/// `derived_contradiction` violation stands in for it, and both the "Did not land" tab on the
/// panel and the ghost edges on the graph use that id to match up with the card in Review.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct BlockedDerivation {
    pub violation_id: Uuid,
    pub subject_id: Uuid,
    pub subject: String,
    pub object_id: Uuid,
    pub object: String,
    pub predicate: String,
    pub rule: String,
    /// The predicate the declaration sits on
    pub via_label: String,
    pub valid_from: Option<DateTime<Utc>>,
    pub valid_to: Option<DateTime<Utc>>,
    /// The assertion that blocked it, and that assertion's triple text
    pub against_fact: Uuid,
    pub against_text: String,
    /// Premise fact ids, in derivation order -- the proof chain unfolds from here
    pub premises: Vec<Uuid>,
}

/// One step of a proof: an asserted premise together with its evidence (0002 R2).
///
/// Premises are always assertions (`fact_derivations` does not record derivations), so a proof is
/// a chain and not a tree: derivation → assertions ordered by `seq` → the sentence behind each
/// assertion. The leaves are chunks.
#[derive(Debug, Clone, Serialize)]
pub struct ProofStep {
    pub seq: i32,
    pub fact_id: Uuid,
    pub subject_id: Uuid,
    pub subject: String,
    pub predicate_id: Option<Uuid>,
    /// The relation's name in the ontology; empty-predicate facts (0010) take no part in
    /// derivation, so in theory this always has a value -- it stays an Option so the read path
    /// does not lie
    pub predicate: Option<String>,
    pub object_id: Option<Uuid>,
    pub object: Option<String>,
    pub valid_from: Option<DateTime<Utc>>,
    pub valid_to: Option<DateTime<Utc>>,
    pub confidence: f32,
    /// This premise was retracted later. The derivation falls with it, but the proof still has to
    /// read out "what it rested on at the time"
    pub retracted: bool,
    pub evidence: Vec<EvidenceView>,
}

/// The complete proof of one derived fact: the fact itself, plus its premises expanded in order
/// down to the source sentences.
#[derive(Debug, Clone, Serialize)]
pub struct Proof {
    pub derived: DerivedFactView,
    pub steps: Vec<ProofStep>,
}

/// The **real count** for each queue in Review.
///
/// Fetching these separately from the lists is deliberate: a list has a cap (ten per page), a
/// count does not. The left column used to read the array's length while the endpoint always
/// returned at most 100 -- a base with 164 items to work through showed 100, and more kept
/// appearing after you had cleared them.
/// One fact waiting for a human nod (0015). `quote` is the full text of that memory -- the
/// confirmation UI has to show the original sentence next to the triple, and listing only the
/// triple amounts to asking someone to judge it out of thin air.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct PendingFactView {
    pub id: Uuid,
    pub subject_id: Uuid,
    pub subject_name: String,
    pub predicate_id: Option<Uuid>,
    /// The relation's name in the ontology; when empty the frontend shows `proposed_predicate`
    /// (in italics, marked as the source's own words)
    pub predicate_label: Option<String>,
    pub proposed_predicate: Option<String>,
    pub object_id: Option<Uuid>,
    pub object_name: Option<String>,
    pub object_value: Option<serde_json::Value>,
    pub valid_from: Option<DateTime<Utc>>,
    pub valid_from_precision: Option<String>,
    pub valid_to: Option<DateTime<Utc>>,
    pub valid_to_precision: Option<String>,
    pub confidence: f32,
    pub chunk_id: Uuid,
    pub quote: String,
    pub proposed_by: Option<Uuid>,
    pub proposed_by_name: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, sqlx::FromRow)]
pub struct ReviewCounts {
    /// Facts extracted from memories, waiting for a human nod (0015). First in the list: these are
    /// the person's own words
    pub pending: i64,
    pub duplicates: i64,
    pub conflicts: i64,
    pub unconfirmed: i64,
    pub lowconf: i64,
    pub mappings: i64,
    pub violations: i64,
    pub defects: i64,
    pub merges: i64,
}

/// Which OWL axioms a relation declares.
///
/// **Passed as one thing, not as a string of parameters.** They were one family to begin with --
/// the reasoner (0002) uses them as its tests, and they should appear side by side in the UI too;
/// spread out as six bools in a parameter list, some call site will eventually pass them in the
/// wrong order, and between `bool`s the compiler cannot help.
///
/// The last two are not bools: `inverseOf` and `subPropertyOf` point at **another relation**, and
/// in the UI they are dropdowns rather than checkboxes. A different shape does not stop them
/// belonging to this family -- the reasoner's four rule sources are exactly two of the six above
/// plus these two (0002).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelationAxioms {
    /// Unique on the subject side (one person, one birthplace)
    pub functional: bool,
    /// Unique on the object side (one project, one leader)
    pub inverse_functional: bool,
    /// A→B ∧ B→C ⟹ A→C
    pub transitive: bool,
    /// A→B ⟹ B→A
    pub symmetric: bool,
    /// A→B ⟹ no B→A exists
    pub asymmetric: bool,
    /// No A→A exists
    pub irreflexive: bool,
    /// `p⁻¹ = q`: `A p B ⟹ B q A`. **Stored one way, used both ways** -- axioms are normalised as
    /// they load (`reasoning::axioms`), so declaring it on one side is enough and the reverse holds
    /// by itself
    pub inverse_of: Option<Uuid>,
    /// `p ⊑ q`: `A p B ⟹ A q B`. Assert the specific one and the general one holds too
    pub sub_property_of: Option<Uuid>,
}

/// One page of the library, together with counts that reach past this page.
///
/// **The counts are not affected by the name / status filter**: `ready` / `extracting` / `failed`
/// say how many there are in this source, which is the reach of the bulk buttons and has nothing
/// to do with what you happen to be searching for right now.
#[derive(Debug, Clone, Serialize)]
pub struct DocumentPage {
    pub docs: Vec<Document>,
    /// The total matching the filter (the paginator uses it)
    pub total: i64,
    pub ready: i64,
    pub extracting: i64,
    pub failed: i64,
}

/// The metadata of one personal access token (0014). **Never contains the plaintext** -- the
/// plaintext exists only in the one value `tokens::issue` returns; the database holds a hash.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct TokenView {
    pub id: Uuid,
    pub name: String,
    /// The short piece a human recognises it by (`utp_pat_ab12`). Enough to match the string in a
    /// config file, not enough to reconstruct it
    pub token_prefix: String,
    /// read | write. **A ceiling is not a grant**: effective rights = this person's role ∩ this scope
    pub scope: String,
    /// None = every base this person can get into
    pub kb_ids: Option<Vec<Uuid>>,
    pub expires_at: Option<DateTime<Utc>>,
    /// "Is this one still in use?" You have to be able to answer before revoking, or nobody dares
    pub last_used_at: Option<DateTime<Utc>>,
    /// **Revoking stamps the row, it does not delete it**: a revocation itself has to leave a trace
    pub revoked_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}
