//! Graph store: the ontology, entity resolution (the first cut of P2: same KB, same type and
//! same name become one), the fact ledger, graph queries.

use sqlx::PgPool;
use std::collections::HashSet;
use utopia_core::models::{
    ChunkFactView, EntityFact, EntityHistoryEvent, EntityType, EvidenceView, FactReviewItem,
    GraphChange, GraphEdge, GraphNode, ProposedPredicate, RelationType,
};
use utopia_core::{AppError, AppResult};
use uuid::Uuid;

/// Row projection of the facts an assertion already has: (id, valid_from, valid_to).
type FactSpanRow = (
    Uuid,
    Option<chrono::DateTime<chrono::Utc>>,
    Option<chrono::DateTime<chrono::Utc>>,
);

/// Where the old fact goes on adoption (`fact_adoptions.mode`): a new row is written in its
/// place.
const ADOPT_SUPERSEDED: &str = "superseded";
/// The target assertion already exists -> merge into it. The old row is invalidated with no
/// successor, so entity history has to read it as "merged" rather than "withdrawn" on that basis,
/// otherwise the UI announces something that never happened.
const ADOPT_MERGED: &str = "merged";

// Creating a KB no longer seeds any relations, and `ensure_default_ontology` is gone as well.
//
// This used to hold ten seed relations, a table of Chinese wordings, a `localized` that picked
// the wording by language, and a seeding function that ran on KB creation / on the first read of
// the ontology / before every extraction. They left in three waves:
//
// - `related_to` (0010): a code-level fallback, which becomes an escape hatch once it is put in
//   the prompt
// - the other eight (`#125`): zero of them carried a signature, and installing an ontology pack
//   would not replace them by name either (`worksFor` and `works_at` have keys that do not
//   match, so the two coexist as two edges) -- and their axiom flag was permanently false, so
//   the consistency check could never find a contradiction on them
// - `mapped_to` (0011): it is "how this number is computed", not "what exists in the world",
//   and has moved to `concept_mappings`
//
// The one function left over was therefore just iterating an empty table. **From the first day a
// KB exists, the ontology holds nothing but the vocabulary the user imported themselves** -- the
// second half of the same move as deleting the built-in entity types in 0009.

pub async fn entity_types(pool: &PgPool, kb_id: Uuid) -> AppResult<Vec<EntityType>> {
    Ok(
        // SELECT * again: parents lives in a join table, `*` cannot reach it.
        // This is the third time in the same trap -- the SQL is in a string, cargo check is all
        // green, and only the first request reports no column found
        sqlx::query_as(
            "SELECT t.*,
                    ARRAY(SELECT p.parent_id FROM entity_type_parents p
                          WHERE p.child_id = t.id) AS parents,
                    (SELECT p.parent_id FROM entity_type_parents p
                      WHERE p.child_id = t.id AND p.is_primary) AS primary_parent
             FROM entity_types t WHERE t.kb_id = $1 ORDER BY t.created_at",
        )
        .bind(kb_id)
        .fetch_all(pool)
        .await?,
    )
}

pub async fn relation_types(pool: &PgPool, kb_id: Uuid) -> AppResult<Vec<RelationType>> {
    // Not SELECT *: domain/range live in join tables, `*` cannot reach them,
    // and sqlx only says "no column found" at runtime -- the compiler cannot see the SQL string
    Ok(sqlx::query_as(
        "SELECT r.*,
                ARRAY(SELECT d.entity_type_id FROM relation_type_domains d
                      WHERE d.relation_type_id = r.id) AS domains,
                ARRAY(SELECT g.entity_type_id FROM relation_type_ranges g
                      WHERE g.relation_type_id = r.id) AS ranges
         FROM relation_types r WHERE r.kb_id = $1 ORDER BY r.created_at",
    )
    .bind(kb_id)
    .fetch_all(pool)
    .await?)
}

/// Writes a fact. Returns (fact id, whether it is new).
///
/// Repeated observations of the same assertion (same subject, predicate and object) do not each
/// set up their own household:
/// - a live row with the same valid_from already exists -> reuse it (evidence accumulates on the
///   one row)
/// - the new observation **carries no time** and the assertion already has an open row -> the
///   weaker statement merges into the existing row ("affiliated with Nebula Tech" merges into
///   "affiliated with Nebula Tech since 2021-02", instead of producing a timeless duplicate)
/// - the new observation **carries a time** and what the assertion already has is a bare row with
///   no start and no end -> temporal refinement: once the new row is stored, the bare row is
///   invalidated and chained on via supersedes (invalidate + rewrite, so the epistemic history
///   stays complete), and the evidence is copied along with the row
/// - both sides carry a time and they differ -> conservatively keep both (they may genuinely be
///   two intervals, e.g. left the company and came back)
///
/// A fact's object: an entity (a relation) or a literal value (an attribute / a metric mapping).
/// The same folding and temporal-refinement logic for both.
#[derive(Debug, Clone, Copy)]
pub enum FactObject<'a> {
    Entity(Uuid),
    Value(&'a serde_json::Value),
}

#[allow(clippy::too_many_arguments)]
pub async fn insert_fact(
    pool: &PgPool,
    kb_id: Uuid,
    subject_id: Uuid,
    // None = the ontology has no matching relation. The original meaning is not lost -- it is in
    // the evidence's proposed_predicate, and fact_surface_predicate() fetches it back for display
    // (see `facts.predicate_id`)
    predicate_id: Option<Uuid>,
    object_id: Uuid,
    validity: Validity<'_>,
    confidence: f32,
) -> AppResult<(Uuid, bool)> {
    insert_fact_inner(
        pool,
        kb_id,
        subject_id,
        predicate_id,
        FactObject::Entity(object_id),
        validity,
        confidence,
    )
    .await
}

/// Where a fact sits on the **world axis**: the instant and the granularity of each end.
///
/// Packed into a struct rather than four parallel parameters: there are two `Option<DateTime>`
/// and two `Option<&str>`, and when two adjacent parameters of the same type get swapped the
/// compiler does not make a sound -- while what a swap costs here is a fact whose start and end
/// are reversed.
///
/// The three states of the end side (the database's `facts_to_precision_matches_date` constraint
/// is what holds the line):
///
/// | Meaning | `to` | `to_precision` |
/// |---|---|---|
/// | still going | `None` | `None` |
/// | **ended, day unknown** | `None` | `Some("unknown")` |
/// | ended at some time | `Some(t)` | `Some("year"/"month"/"day")` |
///
/// The second row was added later. Before it, `to = None` carried both "still going" and "we do
/// not know when it ended", so a sentence like "former CEO of Weta Digital" -- **clearly ended,
/// date missing** -- could only be written as the former, and the graph would assert something
/// the source says is already over.
#[derive(Debug, Clone, Copy, Default)]
pub struct Validity<'a> {
    pub from: Option<chrono::DateTime<chrono::Utc>>,
    pub from_precision: Option<&'a str>,
    pub to: Option<chrono::DateTime<chrono::Utc>>,
    pub to_precision: Option<&'a str>,
}

/// `valid_to_precision` meaning "it ended, but we do not know which day".
pub const ENDED_UNKNOWN: &str = "unknown";

impl<'a> Validity<'a> {
    /// Start end known, finish end unknown or not applicable.
    pub fn starting(
        from: Option<chrono::DateTime<chrono::Utc>>,
        from_precision: Option<&'a str>,
    ) -> Self {
        Self {
            from,
            from_precision,
            to: None,
            to_precision: None,
        }
    }

    /// The source says it ended, but does not say which day.
    pub fn ended_when_unknown(mut self) -> Self {
        self.to = None;
        self.to_precision = Some(ENDED_UNKNOWN);
        self
    }

    /// Whether this assertion no longer holds -- **both kinds of ending count**.
    ///
    /// The test lives here rather than as `valid_to.is_some()` scattered all over: that spelling
    /// misses "ended but we do not know which day" as "still going", and that is exactly what
    /// recording a precision at each end was there to fix.
    pub fn has_ended(&self) -> bool {
        self.to.is_some() || self.to_precision == Some(ENDED_UNKNOWN)
    }
}

#[allow(clippy::too_many_arguments)]
async fn insert_fact_inner(
    pool: &PgPool,
    kb_id: Uuid,
    subject_id: Uuid,
    // None = the ontology has no matching relation. The original meaning is not lost -- it is in
    // the evidence's proposed_predicate, and fact_surface_predicate() fetches it back for display
    // (see `facts.predicate_id`)
    predicate_id: Option<Uuid>,
    object: FactObject<'_>,
    validity: Validity<'_>,
    confidence: f32,
) -> AppResult<(Uuid, bool)> {
    let same_sql = match object {
        FactObject::Entity(_) => {
            "SELECT id, valid_from, valid_to FROM facts
             WHERE kb_id = $1 AND subject_id = $2 AND predicate_id = $3 AND object_id = $4
               AND invalidated_at IS NULL"
        }
        FactObject::Value(_) => {
            "SELECT id, valid_from, valid_to FROM facts
             WHERE kb_id = $1 AND subject_id = $2 AND predicate_id = $3 AND object_value = $4
               AND object_id IS NULL AND invalidated_at IS NULL"
        }
    };
    let mut q = sqlx::query_as(same_sql)
        .bind(kb_id)
        .bind(subject_id)
        .bind(predicate_id);
    q = match object {
        FactObject::Entity(id) => q.bind(id),
        FactObject::Value(v) => q.bind(v),
    };
    let same: Vec<FactSpanRow> = q.fetch_all(pool).await?;
    // Exact duplicate: same valid_from -> reuse
    if let Some((existing, _, _)) = same.iter().find(|(_, vf, _)| *vf == validity.from) {
        return Ok((*existing, false));
    }
    // Weaker statement: the new observation has no time and the assertion already has an open
    // row -> merge into it (take the open row with the latest start)
    if validity.from.is_none() && !validity.has_ended() {
        if let Some((existing, _, _)) = same
            .iter()
            .filter(|(_, _, vt)| vt.is_none())
            .max_by_key(|(_, vf, _)| *vf)
        {
            return Ok((*existing, false));
        }
    }
    // Temporal-refinement candidate: a bare row with no start and no end already exists and this
    // observation carries a start -> once stored, invalidate the bare row and chain on
    let refine_target = if validity.from.is_some() {
        same.iter()
            .find(|(_, vf, vt)| vf.is_none() && vt.is_none())
            .map(|(id, _, _)| *id)
    } else {
        None
    };

    let id = Uuid::now_v7();
    let insert_sql = match object {
        FactObject::Entity(_) => {
            "INSERT INTO facts (id, kb_id, subject_id, predicate_id, object_id,
                                valid_from, valid_from_precision,
                                valid_to, valid_to_precision, confidence)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)"
        }
        FactObject::Value(_) => {
            "INSERT INTO facts (id, kb_id, subject_id, predicate_id, object_value,
                                valid_from, valid_from_precision,
                                valid_to, valid_to_precision, confidence)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)"
        }
    };
    let mut ins = sqlx::query(insert_sql)
        .bind(id)
        .bind(kb_id)
        .bind(subject_id)
        .bind(predicate_id);
    ins = match object {
        FactObject::Entity(oid) => ins.bind(oid),
        FactObject::Value(v) => ins.bind(v),
    };
    ins.bind(validity.from)
        .bind(validity.from_precision)
        .bind(validity.to)
        .bind(validity.to_precision)
        .bind(confidence)
        .execute(pool)
        .await?;

    // Temporal refinement: the bare row (same assertion, no start, no end) is superseded by this
    // timed observation -- invalidate + chain on, and the evidence goes with it
    if let Some(old_id) = refine_target {
        sqlx::query("UPDATE facts SET invalidated_at = now() WHERE id = $1")
            .bind(old_id)
            .execute(pool)
            .await?;
        sqlx::query("UPDATE facts SET supersedes = $2 WHERE id = $1")
            .bind(id)
            .bind(old_id)
            .execute(pool)
            .await?;
        sqlx::query(
            // The surface predicate moves with the evidence: what is refined is the time, not
            // what the source said
            "INSERT INTO fact_evidence (fact_id, chunk_id, quote, proposed_predicate, document_id, doc_version)
             SELECT $1, chunk_id, quote, proposed_predicate, document_id, doc_version
             FROM fact_evidence WHERE fact_id = $2
             ON CONFLICT DO NOTHING",
        )
        .bind(id)
        .bind(old_id)
        .execute(pool)
        .await?;
    }
    Ok((id, true))
}

/// A fact with a literal-value object (the object_value channel, whose first consumer is the
/// metric mapping).
/// Dedup: for the same (S,P) with an exactly equal object_value, only one live fact is stored.
#[allow(clippy::too_many_arguments)]
pub async fn insert_value_fact(
    pool: &PgPool,
    kb_id: Uuid,
    subject_id: Uuid,
    // None = the ontology has no matching relation. The original meaning is not lost -- it is in
    // the evidence's proposed_predicate, and fact_surface_predicate() fetches it back for display
    // (see `facts.predicate_id`)
    predicate_id: Option<Uuid>,
    object_value: &serde_json::Value,
    validity: Validity<'_>,
    confidence: f32,
) -> AppResult<(Uuid, bool)> {
    insert_fact_inner(
        pool,
        kb_id,
        subject_id,
        predicate_id,
        FactObject::Value(object_value),
        validity,
        confidence,
    )
    .await
}

/// `proposed`: the predicate the model actually proposed in this chunk. When it hits the ontology
/// it equals the key; when a predicate outside the ontology does not land on a relation, this is
/// the only thing still holding the original meaning -- all the fact row has left is "related
/// to", and the "runs on" the source said survives right here.
pub async fn add_evidence(
    pool: &PgPool,
    fact_id: Uuid,
    chunk_id: Uuid,
    quote: Option<&str>,
    proposed: Option<&str>,
) -> AppResult<()> {
    // Evidence records the version the moment it is written: which document and which version of
    // it (the basis for S3 version reconciliation and for deciding evidence is "stale")
    // On conflict we fill in the surface predicate rather than skipping the whole row:
    // re-extraction mostly hits (fact, chunk) pairs that already exist, and DO NOTHING would
    // leave existing evidence never able to fill this column in. We only fill it when the old
    // value is empty, never overwrite -- for the same fact in the same chunk, the wording
    // recorded first is its wording
    sqlx::query(
        "INSERT INTO fact_evidence (fact_id, chunk_id, quote, proposed_predicate, document_id, doc_version)
         SELECT $1, $2, $3, left($4, 120), c.document_id, c.doc_version FROM chunks c WHERE c.id = $2
         ON CONFLICT (fact_id, chunk_id) DO UPDATE
           SET proposed_predicate = COALESCE(fact_evidence.proposed_predicate, EXCLUDED.proposed_predicate)",
    )
    .bind(fact_id)
    .bind(chunk_id)
    .bind(quote)
    .bind(proposed)
    .execute(pool)
    .await?;
    Ok(())
}

/// The node-fetching statement shared by every graph query.
///
/// **LEFT JOIN, not JOIN** (0009). An entity whose type was never decided is still a node on the
/// graph: it has a name, it has facts, it has evidence, and all it lacks is a label. An inner
/// join makes it disappear entirely -- the facts are still in the database while the graph has no
/// such person, and that is the hardest kind of data loss to notice.
///
/// key and label stay NULL, **while colour and shape get defaults**: the former is identity, and
/// when there is none we should say so; the latter is something the canvas has to be given, and
/// without making one up there is nothing to render. A grey dot is exactly what "not yet decided"
/// looks like
const NODE_SQL: &str = "SELECT e.id, e.canonical_name AS name, t.key AS type_key,
        t.label AS type_label,
        coalesce(t.color, '#94a3b8') AS color,
        coalesce(t.shape, 'circle') AS shape,
        e.disambiguator,
        (SELECT count(*) FROM facts f
         WHERE (f.subject_id = e.id OR f.object_id = e.id) AND f.invalidated_at IS NULL) AS degree
     FROM entities e LEFT JOIN entity_types t ON t.id = e.type_id";

/// Whole-graph overview: the top N entities by degree and the edges between them.
/// `at`: server-side as-of -- returns only the edges valid at instant T (start no later than T or
/// unknown, end later than T or open).
/// The frontend time slider filters locally and does not pass this parameter; this is the
/// time-travel entry point for API/MCP consumers.
/// Graph overview: the `limit` nodes with the highest degree, and the edges between them.
///
/// **Returns the totals as well.** How many we draw is a rendering matter, how many are in the
/// database is a knowledge-base matter, and the two used to be represented by the same number in
/// the UI -- a database with tens of thousands of entities would forever read 150 in the top
/// right corner, and that is a cap, not a size. The rendering cap itself is reasonable (nobody
/// can make sense of ten thousand dots); what lies is calling it the total.
pub async fn overview(
    pool: &PgPool,
    kb_id: Uuid,
    limit: i64,
    at: Option<chrono::DateTime<chrono::Utc>>,
) -> AppResult<(Vec<GraphNode>, Vec<GraphEdge>, i64, i64)> {
    let nodes: Vec<GraphNode> = sqlx::query_as(&format!(
        "{NODE_SQL} WHERE e.kb_id = $1 AND e.merged_into IS NULL ORDER BY degree DESC, e.created_at LIMIT $2"
    ))
    .bind(kb_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;

    let ids: Vec<Uuid> = nodes.iter().map(|n| n.id).collect();
    let edges = edges_among(pool, kb_id, &ids, at).await?;

    // The totals are counted by the same rules as the canvas: merged-away entities do not count,
    // invalidated facts do not count, and attribute facts (object is a literal value) draw no
    // edge so they do not count either. With different rules, the 325 in "150 / 325" would not
    // match the number the user sees elsewhere
    let total_nodes: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM entities WHERE kb_id = $1 AND merged_into IS NULL",
    )
    .bind(kb_id)
    .fetch_one(pool)
    .await?;
    let total_edges: i64 = sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM facts
                  WHERE kb_id = $1 AND invalidated_at IS NULL AND object_id IS NOT NULL)
              + (SELECT count(*) FROM derived_facts
                  WHERE kb_id = $1 AND invalidated_at IS NULL)",
    )
    .bind(kb_id)
    .fetch_one(pool)
    .await?;
    Ok((nodes, edges, total_nodes, total_edges))
}

async fn edges_among(
    pool: &PgPool,
    kb_id: Uuid,
    ids: &[Uuid],
    at: Option<chrono::DateTime<chrono::Utc>>,
) -> AppResult<Vec<GraphEdge>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    // **Derived edges are UNIONed in explicitly.** They live in `derived_facts`, not in `facts`
    // -- so every read path that wants to see inference results has to spell them out the way
    // this one does. Forgetting to means you do not see the derivations, rather than mistaking
    // them for somebody's assertion (which is exactly what the separate table bought us).
    //
    // The graph wants them, because "this edge was inferred" is one of the things the user ought
    // to see; the `derived` bit lets the UI draw the difference, and lets people filter them all
    // out.
    //
    // The third branch is the **ghost edges** (0017 §3): derivations that were inferred but never
    // landed, living in `axiom_violations`'s `detail`. Their id is the violation's id; `derived`
    // and `blocked` are both true, which is how the UI makes them follow the derivation toggle
    // and draws them in the contested colour, in the grade that blends into the background.
    //
    // The assertion branch computes one extra bit, `contested`: an open violation or temporal
    // conflict is pointing at it. When a derivation collides with an assertion the one hit is
    // left; right is only the last premise, and is not itself contested
    let edges: Vec<GraphEdge> = sqlx::query_as(
        "SELECT f.id, f.subject_id AS source, f.object_id AS target,
                COALESCE(r.key, fact_surface_predicate(f.id)) AS predicate,
                COALESCE(r.label, fact_surface_predicate(f.id)) AS label,
                r.id IS NULL AS inferred, FALSE AS derived, NULL::text AS rule,
                ARRAY[]::uuid[] AS premises,
                f.valid_from, f.valid_to, f.confidence,
                (EXISTS (SELECT 1 FROM axiom_violations v
                          WHERE v.status = 'open'
                            AND (v.left_fact = f.id
                                 OR (v.right_fact = f.id AND v.kind <> 'derived_contradiction')))
                 OR EXISTS (SELECT 1 FROM fact_conflicts c
                             WHERE c.status = 'open'
                               AND (c.old_fact_id = f.id OR c.new_fact_id = f.id))
                ) AS contested,
                FALSE AS blocked
         FROM facts f LEFT JOIN relation_types r ON r.id = f.predicate_id
         WHERE f.kb_id = $1 AND f.invalidated_at IS NULL AND f.object_id IS NOT NULL
           AND f.subject_id = ANY($2) AND f.object_id = ANY($2)
           AND ($3::timestamptz IS NULL
                OR ((f.valid_from IS NULL OR f.valid_from <= $3)
                    AND (f.valid_to IS NULL OR f.valid_to > $3)))
         UNION ALL
         SELECT d.id, d.subject_id AS source, d.object_id AS target,
                r.key AS predicate, r.label AS label,
                FALSE AS inferred, TRUE AS derived, ru.kind AS rule,
                ARRAY(SELECT fd.premise_fact_id FROM fact_derivations fd
                       WHERE fd.derived_fact_id = d.id ORDER BY fd.seq) AS premises,
                d.valid_from, d.valid_to, d.confidence,
                FALSE AS contested, FALSE AS blocked
         FROM derived_facts d JOIN relation_types r ON r.id = d.predicate_id
                              JOIN rules ru ON ru.id = d.rule_id
         WHERE d.kb_id = $1 AND d.invalidated_at IS NULL
           AND d.subject_id = ANY($2) AND d.object_id = ANY($2)
           AND ($3::timestamptz IS NULL
                OR ((d.valid_from IS NULL OR d.valid_from <= $3)
                    AND (d.valid_to IS NULL OR d.valid_to > $3)))
         UNION ALL
         SELECT v.id,
                (v.detail->>'subject_id')::uuid AS source,
                (v.detail->>'object_id')::uuid AS target,
                v.detail->>'predicate' AS predicate, v.detail->>'predicate' AS label,
                FALSE AS inferred, TRUE AS derived, v.detail->>'rule' AS rule,
                v.path AS premises,
                (v.detail->>'valid_from')::timestamptz AS valid_from,
                (v.detail->>'valid_to')::timestamptz AS valid_to,
                0::real AS confidence,
                TRUE AS contested, TRUE AS blocked
         FROM axiom_violations v
         WHERE v.kb_id = $1 AND v.kind = 'derived_contradiction' AND v.status = 'open'
           AND (v.detail->>'subject_id')::uuid = ANY($2)
           AND (v.detail->>'object_id')::uuid = ANY($2)
           AND ($3::timestamptz IS NULL
                OR (((v.detail->>'valid_from')::timestamptz IS NULL
                     OR (v.detail->>'valid_from')::timestamptz <= $3)
                    AND ((v.detail->>'valid_to')::timestamptz IS NULL
                         OR (v.detail->>'valid_to')::timestamptz > $3)))",
    )
    .bind(kb_id)
    .bind(ids)
    .bind(at)
    .fetch_all(pool)
    .await?;
    Ok(edges)
}

/// Neighbourhood expansion (BFS, at most 2 hops, with a cap on the node count).
pub async fn neighborhood(
    pool: &PgPool,
    kb_id: Uuid,
    entity_id: Uuid,
    hops: u8,
    at: Option<chrono::DateTime<chrono::Utc>>,
) -> AppResult<(Vec<GraphNode>, Vec<GraphEdge>)> {
    const MAX_NODES: usize = 300;
    let mut seen: HashSet<Uuid> = HashSet::from([entity_id]);
    let mut frontier: Vec<Uuid> = vec![entity_id];

    for _ in 0..hops.clamp(1, 2) {
        if frontier.is_empty() || seen.len() >= MAX_NODES {
            break;
        }
        let touching: Vec<(Uuid, Option<Uuid>)> = sqlx::query_as(
            "SELECT subject_id, object_id FROM facts
             WHERE kb_id = $1 AND invalidated_at IS NULL AND object_id IS NOT NULL
               AND (subject_id = ANY($2) OR object_id = ANY($2))",
        )
        .bind(kb_id)
        .bind(&frontier)
        .fetch_all(pool)
        .await?;

        let mut next = Vec::new();
        for (s, o) in touching {
            for id in [Some(s), o].into_iter().flatten() {
                if seen.len() >= MAX_NODES {
                    break;
                }
                if seen.insert(id) {
                    next.push(id);
                }
            }
        }
        frontier = next;
    }

    let ids: Vec<Uuid> = seen.into_iter().collect();
    let nodes: Vec<GraphNode> =
        sqlx::query_as(&format!("{NODE_SQL} WHERE e.kb_id = $1 AND e.id = ANY($2)"))
            .bind(kb_id)
            .bind(&ids)
            .fetch_all(pool)
            .await?;
    let edges = edges_among(pool, kb_id, &ids, at).await?;
    Ok((nodes, edges))
}

/// Finds entities by name. **Returns the total as well** -- "rather split than merge" produces a
/// pile of same-named entities by design, and with a fixed ten rows the one you want may not be
/// among those ten at all, with nothing in the UI to show it.
pub async fn search_entities(
    pool: &PgPool,
    kb_id: Uuid,
    q: &str,
    limit: i64,
    offset: i64,
) -> AppResult<(Vec<GraphNode>, i64)> {
    let pattern = format!("%{}%", q.trim());
    let nodes: Vec<GraphNode> = sqlx::query_as(&format!(
        "{NODE_SQL} WHERE e.kb_id = $1 AND e.merged_into IS NULL
         AND e.canonical_name ILIKE $2
         ORDER BY degree DESC, e.canonical_name LIMIT $3 OFFSET $4"
    ))
    .bind(kb_id)
    .bind(&pattern)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await?;
    let (total,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM entities e
          WHERE e.kb_id = $1 AND e.merged_into IS NULL AND e.canonical_name ILIKE $2",
    )
    .bind(kb_id)
    .bind(&pattern)
    .fetch_one(pool)
    .await?;
    Ok((nodes, total))
}

/// Entity detail: the node info plus the fact timeline.
pub async fn entity_detail(
    pool: &PgPool,
    kb_id: Uuid,
    entity_id: Uuid,
) -> AppResult<(GraphNode, Vec<EntityFact>)> {
    let node: GraphNode = sqlx::query_as(&format!("{NODE_SQL} WHERE e.kb_id = $1 AND e.id = $2"))
        .bind(kb_id)
        .bind(entity_id)
        .fetch_optional(pool)
        .await?
        .ok_or(AppError::NotFound)?;

    let facts: Vec<EntityFact> = sqlx::query_as(
        "SELECT f.id,
                CASE WHEN f.subject_id = $2 THEN 'out' ELSE 'in' END AS direction,
                COALESCE(r.key, fact_surface_predicate(f.id)) AS predicate_key,
                COALESCE(r.label, fact_surface_predicate(f.id)) AS predicate_label,
                r.id IS NULL AS inferred, r.temporal,
                CASE WHEN f.subject_id = $2 THEN f.object_id ELSE f.subject_id END AS other_id,
                o.canonical_name AS other_name, f.object_value,
                f.valid_from, f.valid_from_precision, f.valid_to, f.valid_to_precision, f.confidence,
                (SELECT count(*) FROM fact_evidence fe WHERE fe.fact_id = f.id) AS evidence_count,
                (EXISTS (SELECT 1 FROM fact_evidence fe WHERE fe.fact_id = f.id)
                 AND NOT EXISTS (SELECT 1 FROM fact_evidence fe
                                 JOIN chunks c ON c.id = fe.chunk_id
                                 WHERE fe.fact_id = f.id AND c.superseded_at IS NULL)
                ) AS stale,
                (f.supersedes IS NOT NULL) AS corrected,
                (SELECT MAX(COALESCE(d.doc_time, d.created_at))
                 FROM fact_evidence fe JOIN documents d ON d.id = fe.document_id
                 WHERE fe.fact_id = f.id) AS last_evidence_time,
                COALESCE(
                    (SELECT jsonb_build_object(
                                'kind', v.kind, 'ref_id', v.id,
                                'derived', CASE WHEN v.kind = 'derived_contradiction'
                                    THEN (v.detail->>'subject') || ' · '
                                         || (v.detail->>'predicate') || ' · '
                                         || (v.detail->>'object') END)
                       FROM axiom_violations v
                      WHERE v.status = 'open'
                        AND (v.left_fact = f.id
                             OR (v.right_fact = f.id AND v.kind <> 'derived_contradiction'))
                      ORDER BY v.detected_at DESC LIMIT 1),
                    (SELECT jsonb_build_object('kind', 'temporal_conflict', 'ref_id', c.id)
                       FROM fact_conflicts c
                      WHERE c.status = 'open'
                        AND (c.old_fact_id = f.id OR c.new_fact_id = f.id)
                      ORDER BY c.created_at DESC LIMIT 1)
                ) AS contested
         FROM facts f
         LEFT JOIN relation_types r ON r.id = f.predicate_id
         LEFT JOIN entities o
           ON o.id = CASE WHEN f.subject_id = $2 THEN f.object_id ELSE f.subject_id END
         WHERE f.kb_id = $1 AND f.invalidated_at IS NULL
           AND (f.subject_id = $2 OR f.object_id = $2)
         ORDER BY f.valid_from NULLS LAST, f.recorded_at",
    )
    .bind(kb_id)
    .bind(entity_id)
    .fetch_all(pool)
    .await?;

    Ok((node, facts))
}

/// A human corrects an entity's type or name. Returns (snapshot before, state after) -- the
/// caller records the audit ledger from that.
///
/// A misjudged type or a badly extracted name used to leave only the sledgehammer of re-extracting
/// the whole database. What extraction gives is a first judgement, not a verdict.
///
/// Same names are not blocked: two entities with the same type and the same name are a legitimate
/// product of "rather split than merge" (two people both called Zhang Wei), and blocking it would
/// make the second one impossible to record. The caller looks the collision up and offers a
/// merge, see `same_name_peers`.
pub async fn update_entity(
    pool: &PgPool,
    kb_id: Uuid,
    entity_id: Uuid,
    type_id: Option<Uuid>,
    canonical_name: Option<&str>,
) -> AppResult<(GraphNode, GraphNode)> {
    let before: GraphNode = sqlx::query_as(&format!(
        "{NODE_SQL} WHERE e.kb_id = $1 AND e.id = $2 AND e.merged_into IS NULL"
    ))
    .bind(kb_id)
    .bind(entity_id)
    .fetch_optional(pool)
    .await?
    .ok_or(AppError::NotFound)?;

    let new_name = match canonical_name {
        Some(raw) => {
            let n = raw.trim();
            if n.is_empty() {
                return Err(AppError::invalid(
                    "entity_name_required",
                    "Name cannot be empty",
                ));
            }
            // The same cap as on the extraction side: anything past this line is usually a whole
            // sentence that got taken for a name
            if n.chars().count() > 100 {
                return Err(AppError::invalid(
                    "entity_name_too_long",
                    "Name is too long (max 100)",
                ));
            }
            Some(n)
        }
        None => None,
    };

    if let Some(t) = type_id {
        let exists: Option<(Uuid,)> =
            sqlx::query_as("SELECT id FROM entity_types WHERE id = $1 AND kb_id = $2")
                .bind(t)
                .bind(kb_id)
                .fetch_optional(pool)
                .await?;
        if exists.is_none() {
            return Err(AppError::invalid(
                "unknown_entity_type",
                "No such entity type in this KB",
            ));
        }
    }

    sqlx::query(
        // Only a type change marks it human -- this endpoint is also used to rename, and a
        // rename alone should not casually stamp the type's provenance as human. In this
        // interface `$3 IS NULL` means "no type was supplied this time", not "clear the type":
        // the routing layer requires at least one of the two fields, so it cannot give three
        // states.
        //
        // Which leaves one thing that cannot be done today: after 0009, "no type" may be a
        // human's decision (they looked, and the ontology has no suitable class), and this
        // interface cannot express that. Fixing it means having the request body distinguish
        // "not supplied" from "explicitly cleared", and that is a separate job
        "UPDATE entities
         SET type_id = COALESCE($3, type_id),
             canonical_name = COALESCE($4, canonical_name),
             type_source = CASE WHEN $3::uuid IS NULL THEN type_source ELSE 'human' END,
             updated_at = now()
         WHERE id = $1 AND kb_id = $2 AND merged_into IS NULL",
    )
    .bind(entity_id)
    .bind(kb_id)
    .bind(type_id)
    .bind(new_name)
    .execute(pool)
    .await?;

    // The disambiguator suffix depends on the name grouping and on the type label (the type
    // label is its fallback value), and both were just changed. A rename has to refresh two
    // groups: the old name's group may drop to 1 (where the suffix should be cleared) and the new
    // name's group may rise to 2.
    if let Some(n) = new_name.filter(|n| !n.eq_ignore_ascii_case(&before.name)) {
        crate::resolution::refresh_disambiguators(pool, kb_id, &before.name).await?;
        crate::resolution::refresh_disambiguators(pool, kb_id, n).await?;
    } else if type_id.is_some() {
        crate::resolution::refresh_disambiguators(pool, kb_id, &before.name).await?;
    }

    let after: GraphNode = sqlx::query_as(&format!("{NODE_SQL} WHERE e.kb_id = $1 AND e.id = $2"))
        .bind(kb_id)
        .bind(entity_id)
        .fetch_one(pool)
        .await?;
    Ok((before, after))
}

/// Other live entities with the same name (case-insensitive) as the given one -- used to ask
/// "merge them?" after a rename.
/// Reports only, never blocks: deciding whether they really are the same one is a human's job.
pub async fn same_name_peers(
    pool: &PgPool,
    kb_id: Uuid,
    entity_id: Uuid,
) -> AppResult<Vec<GraphNode>> {
    sqlx::query_as(&format!(
        "{NODE_SQL} WHERE e.kb_id = $1 AND e.merged_into IS NULL AND e.id <> $2
           AND lower(e.canonical_name) = (SELECT lower(canonical_name) FROM entities WHERE id = $2)
         ORDER BY degree DESC LIMIT 10"
    ))
    .bind(kb_id)
    .bind(entity_id)
    .fetch_all(pool)
    .await
    .map_err(Into::into)
}

/// Low-confidence live facts (the review page).
pub async fn low_confidence_facts(
    pool: &PgPool,
    kb_id: Uuid,
    below: f32,
    limit: i64,
    offset: i64,
) -> AppResult<Vec<FactReviewItem>> {
    let rows: Vec<FactReviewItem> = sqlx::query_as(
        "SELECT f.id, s.canonical_name AS subject_name, COALESCE(r.label, fact_surface_predicate(f.id)) AS predicate_label,
                COALESCE(o.canonical_name, f.object_value->>'summary') AS object_name,
                f.valid_from, f.valid_to, f.confidence,
                (SELECT count(*) FROM fact_evidence fe WHERE fe.fact_id = f.id) AS evidence_count,
                (SELECT fe.quote FROM fact_evidence fe
                 WHERE fe.fact_id = f.id AND fe.quote IS NOT NULL LIMIT 1) AS quote
         FROM facts f
         JOIN entities s ON s.id = f.subject_id
         LEFT JOIN relation_types r ON r.id = f.predicate_id
         LEFT JOIN entities o ON o.id = f.object_id
         WHERE f.kb_id = $1 AND f.invalidated_at IS NULL AND f.confidence < $2
         ORDER BY f.confidence, f.recorded_at DESC
         LIMIT $3 OFFSET $4",
    )
    .bind(kb_id)
    .bind(below)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Live facts whose "evidence all stays on an old version" (the third cut of S3: knowledge a new
/// version of the document no longer confirms).
/// The decision derives purely from chunk liveness -- the claiming mechanism guarantees evidence
/// on unchanged paragraphs is not hit by mistake; nothing is ever deleted automatically (no
/// longer mentioned != no longer true), and the power to delete or close stays with the human in
/// Review.
pub async fn stale_facts(
    pool: &PgPool,
    kb_id: Uuid,
    limit: i64,
    offset: i64,
) -> AppResult<Vec<FactReviewItem>> {
    let rows: Vec<FactReviewItem> = sqlx::query_as(
        "SELECT f.id, s.canonical_name AS subject_name, COALESCE(r.label, fact_surface_predicate(f.id)) AS predicate_label,
                COALESCE(o.canonical_name, f.object_value->>'summary') AS object_name,
                f.valid_from, f.valid_to, f.confidence,
                (SELECT count(*) FROM fact_evidence fe WHERE fe.fact_id = f.id) AS evidence_count,
                (SELECT fe.quote FROM fact_evidence fe
                 WHERE fe.fact_id = f.id AND fe.quote IS NOT NULL LIMIT 1) AS quote
         FROM facts f
         JOIN entities s ON s.id = f.subject_id
         LEFT JOIN relation_types r ON r.id = f.predicate_id
         LEFT JOIN entities o ON o.id = f.object_id
         WHERE f.kb_id = $1 AND f.invalidated_at IS NULL
           AND EXISTS (SELECT 1 FROM fact_evidence fe WHERE fe.fact_id = f.id)
           AND NOT EXISTS (SELECT 1 FROM fact_evidence fe
                           JOIN chunks c ON c.id = fe.chunk_id
                           WHERE fe.fact_id = f.id AND c.superseded_at IS NULL)
         ORDER BY f.recorded_at DESC
         LIMIT $2 OFFSET $3",
    )
    .bind(kb_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// A human confirms a low-confidence fact: confidence is raised to 1.0.
pub async fn confirm_fact(pool: &PgPool, kb_id: Uuid, fact_id: Uuid) -> AppResult<()> {
    let res = sqlx::query(
        "UPDATE facts SET confidence = 1.0 WHERE id = $1 AND kb_id = $2 AND invalidated_at IS NULL",
    )
    .bind(fact_id)
    .bind(kb_id)
    .execute(pool)
    .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    // There used to be another block here: confirming a `mapped_to` fact invalidated the old
    // mapping for the same (concept, source). Mappings have moved out of the ledger (0011,
    // `concept_mappings` manages uniqueness itself), so that SQL always matched zero rows and was
    // deleted.
    Ok(())
}

/// A human rejects a fact: invalidate it (the ledger is append-only, no DELETE).
pub async fn reject_fact(pool: &PgPool, kb_id: Uuid, fact_id: Uuid) -> AppResult<()> {
    let res = sqlx::query(
        "UPDATE facts SET invalidated_at = now()
         WHERE id = $1 AND kb_id = $2 AND invalidated_at IS NULL",
    )
    .bind(fact_id)
    .bind(kb_id)
    .execute(pool)
    .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    Ok(())
}

/// The evidence chain in reverse: which live facts each chunk of this document produced (the
/// right-hand column of the document viewer).
pub async fn document_extractions(
    pool: &PgPool,
    document_id: Uuid,
) -> AppResult<Vec<ChunkFactView>> {
    let rows: Vec<ChunkFactView> = sqlx::query_as(
        "SELECT fe.chunk_id, f.id AS fact_id,
                f.subject_id, s.canonical_name AS subject,
                COALESCE(r.label, fact_surface_predicate(f.id)) AS predicate,
                r.id IS NULL AS inferred,
                f.object_id, o.canonical_name AS object,
                f.valid_from, f.valid_to, f.confidence
         FROM fact_evidence fe
         JOIN chunks c ON c.id = fe.chunk_id AND c.document_id = $1
              AND c.superseded_at IS NULL
         JOIN facts f ON f.id = fe.fact_id AND f.invalidated_at IS NULL
         JOIN entities s ON s.id = f.subject_id
         LEFT JOIN relation_types r ON r.id = f.predicate_id
         LEFT JOIN entities o ON o.id = f.object_id
         ORDER BY c.seq, f.recorded_at",
    )
    .bind(document_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// The evidence replay path: does not filter superseded out -- its whole job is being able to see
/// old versions.
/// stale = the evidence version lags the document's current version (the UI marks it "from
/// v{n}").
pub async fn fact_evidence(pool: &PgPool, fact_id: Uuid) -> AppResult<Vec<EvidenceView>> {
    let rows: Vec<EvidenceView> = sqlx::query_as(
        "SELECT fe.quote, fe.proposed_predicate, fe.chunk_id, c.document_id, d.filename, c.seq,
                c.doc_version,
                c.doc_version < COALESCE(
                    (SELECT MAX(version) FROM document_versions dv
                     WHERE dv.document_id = c.document_id), 1) AS stale
         FROM fact_evidence fe
         JOIN chunks c ON c.id = fe.chunk_id
         JOIN documents d ON d.id = c.document_id
         WHERE fe.fact_id = $1",
    )
    .bind(fact_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Wipes a KB's entire graph layer (the settlement semantics of Rebuild graph): entities /
/// facts / evidence / review queue / conflicts / merge records are all deleted, while the
/// ontology (class and relation definitions) and documents / chunks / embeddings are kept.
///
/// Two things are kept deliberately: the decision ledger (audit_events -- its snapshots are
/// self-contained, so the records stay readable once the graph is gone) and the verdict cache
/// (resolution_verdicts -- after a rebuild the same-name pairs reappear and hit it directly,
/// saving a batch of LLM calls).
/// Returns (entities deleted, facts deleted).
pub async fn purge_graph(pool: &PgPool, kb_id: Uuid) -> AppResult<(i64, i64)> {
    let mut tx = pool.begin().await?;
    let (entity_count,): (i64,) = sqlx::query_as("SELECT count(*) FROM entities WHERE kb_id = $1")
        .bind(kb_id)
        .fetch_one(&mut *tx)
        .await?;
    let (fact_count,): (i64,) = sqlx::query_as("SELECT count(*) FROM facts WHERE kb_id = $1")
        .bind(kb_id)
        .fetch_one(&mut *tx)
        .await?;

    // Most FKs are CASCADE, but two self-references are NO ACTION: dereference first, then
    // delete, with the order spelled out (this block is itself the definition of "what the graph
    // layer is made of")
    for sql in [
        "DELETE FROM fact_conflicts WHERE kb_id = $1",
        "DELETE FROM resolution_reviews WHERE kb_id = $1",
        "DELETE FROM entity_merges WHERE kb_id = $1",
        "UPDATE facts SET supersedes = NULL WHERE kb_id = $1",
        "DELETE FROM fact_evidence WHERE fact_id IN (SELECT id FROM facts WHERE kb_id = $1)",
        "DELETE FROM facts WHERE kb_id = $1",
        "UPDATE entities SET merged_into = NULL WHERE kb_id = $1",
        "DELETE FROM entities WHERE kb_id = $1",
        // The unmatched-ontology counts get accumulated again by extraction
        "DELETE FROM ontology_misses WHERE kb_id = $1",
    ] {
        sqlx::query(sql).bind(kb_id).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok((entity_count, fact_count))
}

/// An entity's history of epistemic change (the record-time axis).
///
/// The fundamental difference from entity_detail: there it is `invalidated_at IS NULL` and it
/// only answers "what do we believe now"; here nothing is filtered and it answers "when did we
/// believe this, and when did we change our mind". The data was always there -- the ledger is
/// append-only, a correction inserts a new row + marks the old one invalidated, and never
/// overwrites.
///
/// One fact row produces at most two events: the write (asserted / corrected) and the
/// invalidation (rejected). An invalidation that has a successor correction row is not recorded
/// separately -- that death is already explained by the successor's corrected event.
///
/// Attribution: in the audit ledger the target of fact.close is **the old row that was closed**,
/// while the correction row is a different, newly inserted row, so we look it up by
/// COALESCE(supersedes, id); for conflict adjudication the target is the conflict row, one more
/// hop away. No audit record found = the engine did it (an extraction write or temporal
/// reconciliation), and actor is NULL.
pub async fn entity_history(
    pool: &PgPool,
    kb_id: Uuid,
    entity_id: Uuid,
    limit: i64,
    offset: i64,
) -> AppResult<(Vec<EntityHistoryEvent>, i64)> {
    const EVENTS: &str = "
        WITH ef AS (
            SELECT f.*,
                   CASE WHEN f.subject_id = $2 THEN 'out' ELSE 'in' END AS direction,
                   CASE WHEN f.subject_id = $2 THEN f.object_id ELSE f.subject_id END AS other_id
            FROM facts f
            WHERE f.kb_id = $1 AND (f.subject_id = $2 OR f.object_id = $2)
        ),
        ev AS (
            SELECT ef.*, ef.recorded_at AS at,
                   CASE WHEN ef.supersedes IS NULL THEN 'asserted' ELSE 'corrected' END AS kind
            FROM ef
            UNION ALL
            -- Invalidated with no successor = overturned ... unless it was merged into another
            -- assertion. In that case not a word of content was lost, and calling it \"withdrawn\"
            -- is the UI stating something that never happened
            SELECT ef.*, ef.invalidated_at AS at,
                   CASE WHEN EXISTS (SELECT 1 FROM fact_adoptions fa
                                     WHERE fa.old_fact_id = ef.id AND fa.mode = 'merged'
                                       AND fa.reverted_at IS NULL)
                        THEN 'merged' ELSE 'rejected' END AS kind
            FROM ef
            WHERE ef.invalidated_at IS NOT NULL
              AND NOT EXISTS (SELECT 1 FROM facts s WHERE s.supersedes = ef.id)
        ),
        -- A retype is not a fact: no predicate, no other party, no direction. It comes from
        -- entity_retypes, and one row produces at most two events -- the change itself, and the
        -- revert.
        --
        -- **A reverted retype is still shown.** It reads as \"changed, then reverted\", not as
        -- never having happened. This repo has fallen for that same class of mistake twice
        -- (#37; a merge read as a withdrawal), so this is not defensive programming
        rt AS (
            SELECT r.created_at AS at, 'retyped' AS kind, r.actor_id,
                   tf.label AS from_type_label, tt.label AS to_type_label
            FROM entity_retypes r
            LEFT JOIN entity_types tf ON tf.id = r.from_type_id
            JOIN entity_types tt ON tt.id = r.to_type_id
            WHERE r.kb_id = $1 AND r.entity_id = $2
            UNION ALL
            SELECT r.reverted_at, 'retype_reverted', r.actor_id, tf.label, tt.label
            FROM entity_retypes r
            LEFT JOIN entity_types tf ON tf.id = r.from_type_id
            JOIN entity_types tt ON tt.id = r.to_type_id
            WHERE r.kb_id = $1 AND r.entity_id = $2 AND r.reverted_at IS NOT NULL
        )";
    let rows: Vec<EntityHistoryEvent> = sqlx::query_as(&format!(
        "{EVENTS}
         SELECT * FROM (
         SELECT ev.id AS fact_id, ev.at, ev.kind, ev.direction,
                COALESCE(r.label, fact_surface_predicate(ev.id)) AS predicate_label, o.canonical_name AS other_name,
                ev.object_value, ev.valid_from, ev.valid_from_precision,
                ev.valid_to, ev.valid_to_precision,
                ev.confidence, act.actor_name, act.action,
                src.document_id, src.filename, src.quote,
                NULL::text AS from_type_label, NULL::text AS to_type_label
         FROM ev
         LEFT JOIN relation_types r ON r.id = ev.predicate_id
         LEFT JOIN entities o ON o.id = ev.other_id
         LEFT JOIN LATERAL (
             SELECT u.display_name AS actor_name, a.action
             FROM audit_events a
             LEFT JOIN users u ON u.id = a.actor_id
             WHERE a.kb_id = $1
               -- Assertions are written by extraction and are never a human's decision:
               -- attribution only asks about the corrected and overturned kinds of event,
               -- otherwise a later human adjudication gets pinned on the original assertion
               AND ev.kind <> 'asserted'
               AND a.action = ANY(CASE ev.kind
                     WHEN 'corrected' THEN
                       ARRAY['fact.close', 'conflict.close_old', 'ontology.predicate_adopted']
                     -- A merge can only come from an adoption, never from a Review rejection
                     WHEN 'merged' THEN ARRAY['ontology.predicate_adopted']
                     ELSE ARRAY['fact.reject', 'conflict.reject_new',
                                'ontology.adoption_reverted'] END)
               AND (a.target_id = COALESCE(ev.supersedes, ev.id)
                    OR a.target_id IN (SELECT c.id FROM fact_conflicts c
                                       WHERE c.old_fact_id = COALESCE(ev.supersedes, ev.id)
                                          OR c.new_fact_id = ev.id)
                    -- Adoption and revert are both recorded against the relation type, and one
                    -- action rewrites a batch of facts, so fact_adoptions is what ties them to
                    -- exactly which rows (a corrected event is the new row, a merged one the old
                    -- row; both ends are accepted)
                    OR (a.action IN ('ontology.predicate_adopted',
                                     'ontology.adoption_reverted')
                        AND EXISTS (SELECT 1 FROM fact_adoptions fa
                                    WHERE fa.predicate_id = a.target_id
                                      AND (fa.new_fact_id = ev.id
                                           OR fa.old_fact_id = ev.id))))
             ORDER BY a.created_at DESC LIMIT 1
         ) act ON true
         LEFT JOIN LATERAL (
             SELECT d.id AS document_id, d.filename, fe.quote
             FROM fact_evidence fe
             JOIN chunks c ON c.id = fe.chunk_id
             JOIN documents d ON d.id = c.document_id
             WHERE fe.fact_id = ev.id
             ORDER BY fe.doc_version DESC NULLS LAST LIMIT 1
         ) src ON true
         UNION ALL
         SELECT NULL::uuid, rt.at, rt.kind, NULL::text,
                NULL::text, NULL::text,
                NULL::jsonb, NULL::timestamptz, NULL::text,
                NULL::timestamptz, NULL::text,
                NULL::real, u.display_name, NULL::text,
                NULL::uuid, NULL::text, NULL::text,
                rt.from_type_label, rt.to_type_label
         FROM rt LEFT JOIN users u ON u.id = rt.actor_id
         ) x
         ORDER BY x.at DESC, x.fact_id
         LIMIT $3 OFFSET $4"
    ))
    .bind(kb_id)
    .bind(entity_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await?;
    // The total counts the retype branch too, otherwise pagination comes up short
    let (total,): (i64,) = sqlx::query_as(&format!(
        "{EVENTS} SELECT (SELECT count(*) FROM ev) + (SELECT count(*) FROM rt)"
    ))
    .bind(kb_id)
    .bind(entity_id)
    .fetch_one(pool)
    .await?;
    Ok((rows, total))
}

/// Every epistemic change across the whole database within one record-time window.
///
/// **The window opens on the epistemic axis**: `since`/`until` are compared against recorded_at
/// and invalidated_at, not valid_from/valid_to. That is the only difference from
/// `entity_facts(at)`, and the whole of it -- that one asks "what did the world look like at
/// instant T", this one asks "what did we change our mind about during this period". The two
/// query the same table using different columns, and mixing them up quietly gives a wrong answer
/// that looks reasonable.
///
/// Event derivation has the same source as `entity_history` (see the comments there): one fact
/// row produces at most two events, and a death already corrected by a successor is not recorded
/// twice.
pub async fn graph_changes(
    pool: &PgPool,
    kb_id: Uuid,
    since: chrono::DateTime<chrono::Utc>,
    until: chrono::DateTime<chrono::Utc>,
    entity_id: Option<Uuid>,
    kinds: Option<&[String]>,
    limit: i64,
) -> AppResult<Vec<GraphChange>> {
    // Each branch opens the window on **its own time column** rather than unioning first and
    // filtering after: a fact written in February and overturned in August should have neither
    // event show up in a "March-April" window
    const EVENTS: &str = "
        WITH ev AS (
            SELECT f.id, f.subject_id, f.predicate_id, f.object_id, f.object_value,
                   f.valid_from, f.valid_from_precision, f.valid_to, f.valid_to_precision, f.confidence,
                   f.recorded_at AS at,
                   CASE WHEN f.supersedes IS NULL THEN 'asserted' ELSE 'corrected' END AS kind
            FROM facts f
            WHERE f.kb_id = $1 AND f.recorded_at >= $2 AND f.recorded_at < $3
              AND ($4::uuid IS NULL OR f.subject_id = $4 OR f.object_id = $4)
            UNION ALL
            SELECT f.id, f.subject_id, f.predicate_id, f.object_id, f.object_value,
                   f.valid_from, f.valid_from_precision, f.valid_to, f.valid_to_precision, f.confidence,
                   f.invalidated_at AS at,
                   CASE WHEN EXISTS (SELECT 1 FROM fact_adoptions fa
                                     WHERE fa.old_fact_id = f.id AND fa.mode = 'merged'
                                       AND fa.reverted_at IS NULL)
                        THEN 'merged' ELSE 'rejected' END AS kind
            FROM facts f
            WHERE f.kb_id = $1 AND f.invalidated_at >= $2 AND f.invalidated_at < $3
              AND NOT EXISTS (SELECT 1 FROM facts s WHERE s.supersedes = f.id)
              AND ($4::uuid IS NULL OR f.subject_id = $4 OR f.object_id = $4)
        )";
    Ok(sqlx::query_as(&format!(
        "{EVENTS}
         SELECT ev.id AS fact_id, ev.at, ev.kind,
                ev.subject_id, s.canonical_name AS subject_name,
                COALESCE(r.label, fact_surface_predicate(ev.id)) AS predicate_label, o.canonical_name AS object_name,
                ev.object_value, ev.valid_from, ev.valid_from_precision,
                ev.valid_to, ev.valid_to_precision,
                ev.confidence, src.document_id, src.filename, src.quote
         FROM ev
         LEFT JOIN relation_types r ON r.id = ev.predicate_id
         JOIN entities s ON s.id = ev.subject_id
         LEFT JOIN entities o ON o.id = ev.object_id
         LEFT JOIN LATERAL (
             SELECT d.id AS document_id, d.filename, fe.quote
             FROM fact_evidence fe
             JOIN chunks c ON c.id = fe.chunk_id
             JOIN documents d ON d.id = c.document_id
             WHERE fe.fact_id = ev.id
             ORDER BY fe.doc_version DESC NULLS LAST LIMIT 1
         ) src ON true
         WHERE $5::text[] IS NULL OR ev.kind = ANY($5)
         ORDER BY ev.at DESC, ev.id
         LIMIT $6"
    ))
    .bind(kb_id)
    .bind(since)
    .bind(until)
    .bind(entity_id)
    .bind(kinds)
    .bind(limit)
    .fetch_all(pool)
    .await?)
}

// ---------------------------------------------------------------------------
// Predicate resolution: claiming facts that have no predicate back into the ontology
// ---------------------------------------------------------------------------

// The view type ProposedPredicate is defined in utopia-core::models (store does not depend on
// serde directly)

/// Which wordings the source used on the facts that have no predicate.
///
/// This is the evidential basis for ontology-extension suggestions -- what it has over
/// `ontology_misses`'s bare counts is that it is connected to the actual facts, so adopting a
/// wording can say outright "will reclassify 57 rows" and really go and change them.
pub async fn proposed_predicates(pool: &PgPool, kb_id: Uuid) -> AppResult<Vec<ProposedPredicate>> {
    Ok(sqlx::query_as(
        // **Prevalence is counted from all the evidence, not from the backlog.**
        //
        // The WHEREs below narrow the row set to "has no predicate yet, still alive, object is an
        // entity" -- that is **what adoption is going to rewrite**, and that is how `fact_count`
        // should be counted. But `doc_count` answers a different question: how prevalent is this
        // wording in the corpus. Counting it off the leftovers is systematically low, and gets
        // lower the more it is used -- once a wording is adopted, caught by predicate matching,
        // or invalidated by a correction, its rows leave the backlog. A database fed one document
        // at a time suffers worst: every round carries a batch away, what is left never
        // accumulates two documents, and so the ontology never grows.
        //
        // Measured (ai-timeline, 348 chunks): under the two ways of counting, 8 wordings fall on
        // opposite sides of the threshold -- "in only 1 document" by the backlog count, "in ≥2
        // documents" by the full count.
        //
        // Via a CTE rather than a correlated subquery: the latter rescans the evidence table once
        // per group, 360ms against 7ms on the same data. This function runs on every Suggest and
        // on every automatic ontology extension.
        "WITH spread AS (
             SELECT e.proposed_predicate AS form,
                    count(DISTINCT e.document_id) AS doc_count
             FROM fact_evidence e
             JOIN facts ff ON ff.id = e.fact_id
             WHERE ff.kb_id = $1 AND e.proposed_predicate IS NOT NULL
             GROUP BY 1
         )
         SELECT fe.proposed_predicate AS form,
                count(DISTINCT f.id) AS fact_count,
                max(sp.doc_count) AS doc_count,
                (SELECT s.canonical_name || ' → ' || o.canonical_name
                 FROM fact_evidence e2
                 JOIN facts f2 ON f2.id = e2.fact_id
                 JOIN entities s ON s.id = f2.subject_id
                 JOIN entities o ON o.id = f2.object_id
                 WHERE e2.proposed_predicate = fe.proposed_predicate
                   AND f2.kb_id = $1 AND f2.predicate_id IS NULL AND f2.invalidated_at IS NULL
                 LIMIT 1) AS example
         FROM fact_evidence fe
         JOIN facts f ON f.id = fe.fact_id
         JOIN spread sp ON sp.form = fe.proposed_predicate
         WHERE f.kb_id = $1 AND f.predicate_id IS NULL
           AND f.invalidated_at IS NULL AND fe.proposed_predicate IS NOT NULL
           -- Literal-value objects do not count: they also have no predicate and also carry the
           -- source's wording, but what they want is an attribute, not a relation. Let them in
           -- and the suggestion builds a relation to match, and then `founding_date` becomes an
           -- edge pointing at \"2015\" -- exactly the thing this path is here to fix
           AND f.object_id IS NOT NULL
           -- Wordings the user has dismissed no longer appear among the candidates (both the
           -- manual and the automatic path steer around them on this basis)
           AND NOT EXISTS (SELECT 1 FROM ontology_misses m
                           WHERE m.kb_id = $1 AND m.kind = 'relation_type'
                             AND m.key = fe.proposed_predicate AND m.dismissed_at IS NOT NULL)
         GROUP BY fe.proposed_predicate
         ORDER BY fact_count DESC, form",
    )
    .bind(kb_id)
    .fetch_all(pool)
    .await?)
}

/// Which **documents** each wording waiting to be claimed appears in.
///
/// [`proposed_predicates`] already gives a `doc_count`, but the adoption path first folds
/// wordings together by inflectional stem (`sued` and `sues` are one relation), and the document
/// count after folding is a **union**, not a sum -- one document may perfectly well have used
/// both spellings, so adding them up double-counts, and a single document could push a wording
/// over the "≥2 documents" threshold.
///
/// **The fallback predicate is not filtered on.** This query and [`proposed_predicates`] answer
/// two different questions: that one asks "which wordings are still waiting to be adopted" and
/// looks at the backlog; this one asks "how prevalent is this wording" and looks at all the
/// evidence, with conditions matching that function's internal `spread` CTE.
///
/// The first version copied `rt.key = 'related_to'` over (the fallback relation still existed
/// then), with the reason given as "the conditions in the two places have to match" -- which was
/// wrong. What that counts is still the leftovers: once a wording is adopted, caught by predicate
/// matching, or invalidated by a correction, its rows leave the backlog and the document count
/// drops with them. A database fed one document at a time therefore never accumulates two
/// documents. The test caught it on the spot (only one of the two documents came back).
pub async fn proposed_predicate_documents(
    pool: &PgPool,
    kb_id: Uuid,
) -> AppResult<Vec<(String, Uuid)>> {
    Ok(sqlx::query_as(
        "SELECT DISTINCT fe.proposed_predicate, fe.document_id
         FROM fact_evidence fe
         JOIN facts f ON f.id = fe.fact_id
         WHERE f.kb_id = $1
           AND fe.proposed_predicate IS NOT NULL
           AND fe.document_id IS NOT NULL
           AND NOT EXISTS (SELECT 1 FROM ontology_misses m
                           WHERE m.kb_id = $1 AND m.kind = 'relation_type'
                             AND m.key = fe.proposed_predicate AND m.dismissed_at IS NOT NULL)",
    )
    .bind(kb_id)
    .fetch_all(pool)
    .await?)
}

/// Rewrites the predicate-less facts identified by `forms` onto `predicate_id`.
/// Returns (batch id, number of rows rewritten) -- the batch id is the handle for undoing it.
///
/// **Appends rather than editing in place**: inserts a new row carrying `supersedes` and
/// invalidates the old one, the same path human corrections and temporal closure take -- the
/// epistemic change is itself information, and entity history lets you read "first recorded as
/// related to, later refined to available on".
///
/// Only facts whose wordings **all** fall inside `forms` are rewritten: one fact may accumulate
/// several wordings (chunk A says "runs on", chunk B says "optimized for"), and rewriting when
/// only one of them has been claimed amounts to deciding for the other one too. Measured, facts
/// like that are under 1% -- better to miss them than to guess.
///
/// Every destination is written into `fact_adoptions`. One `supersedes` pointer is not enough --
/// when the target assertion already exists the path taken is "merge", the old row is invalidated
/// with no successor, and so it can neither be undone, nor kept from entity history judging it
/// rejected and announcing to the world that "this one was withdrawn" (when in fact it was merged
/// into another one without losing a word).
/// `swap = true`: these wordings are the **passive form** of the target relation, so subject and
/// object have to be exchanged in the rewrite.
///
/// `X produced_by Y` and `Y produces X` are the same edge. Without the exchange the graph grows
/// an extra arrow pointing the other way, and it can never be brought together with the
/// forward-facing ones -- the same thing, split across two directions.
pub async fn adopt_proposed_predicates(
    pool: &PgPool,
    kb_id: Uuid,
    predicate_id: Uuid,
    forms: &[String],
    swap: bool,
) -> AppResult<Adopted> {
    adopt(pool, kb_id, predicate_id, AdoptTargets::ByForm(forms), swap).await
}

/// The books for one adoption. `left_off` has to be passed up: rewrote 20, left 3 behind --
/// reporting only the first half is reporting the good news and hiding the bad.
#[derive(Debug, Clone, Copy)]
pub struct Adopted {
    pub batch_id: Uuid,
    /// How many rows were rewritten across
    pub moved: u32,
    /// How many matched the signature on neither side and so did **not** get a predicate
    /// attached (#190). They stay on the empty predicate as before, with the source's wording
    /// still in the evidence -- the same handling extraction applies in the same situation
    pub left_off: u32,
    /// How many had subject and object exchanged according to the signature.
    ///
    /// **Straightening something out should not be silent.** On the extraction path, every
    /// straightening drops a `direction_corrected` discard signal (#138 puts it as "never
    /// silently": an automatic action driven by a possibly wrong declaration only stays out of
    /// the category criterion 2 of 0001 objects to if it leaves a trace). Adoption makes the same
    /// judgement, so it should also say how many rows it moved, otherwise all the ledger has left
    /// is "rewrote N rows" and you cannot see how many of those the ontology turned around
    pub corrected: u32,
}

/// Which facts to rewrite, and where the new row's object comes from.
pub enum AdoptTargets<'a> {
    /// The relation route: found by surface wording, with the object carried over as it is.
    ByForm(&'a [String]),
    /// The attribute route: the caller has already picked the facts and normalised the values
    /// according to the datatype.
    ///
    /// **Normalisation has to happen in the caller**: that set of rules ("2015" -> a date,
    /// "1,200" -> a number) lives in the extraction module, which the store cannot reach and
    /// should not be able to reach. More importantly, it can **fail** -- a value that cannot be
    /// converted should not be forced into a date attribute, and that fact would rather go on
    /// having no predicate (the original word is still in the evidence). So the caller filters
    /// first and hands the result back.
    WithValues(&'a [(Uuid, serde_json::Value)]),
}

async fn adopt(
    pool: &PgPool,
    kb_id: Uuid,
    predicate_id: Uuid,
    targets: AdoptTargets<'_>,
    swap: bool,
) -> AppResult<Adopted> {
    let batch_id = Uuid::now_v7();
    let nothing = Adopted {
        batch_id,
        moved: 0,
        left_off: 0,
        corrected: 0,
    };
    let targets: Vec<(Uuid, Uuid, Option<Uuid>, Option<serde_json::Value>)> = match targets {
        AdoptTargets::ByForm(forms) => {
            if forms.is_empty() {
                return Ok(nothing);
            }
            sqlx::query_as(
                "SELECT f.id, f.subject_id, f.object_id, f.object_value
                 FROM facts f
                 WHERE f.kb_id = $1 AND f.predicate_id IS NULL AND f.invalidated_at IS NULL
                   -- **Only touch the ones whose object is an entity.** The same wording may
                   -- have both facts pointing at entities and facts carrying literal values
                   -- (location uses both), and the latter belong to the attribute route:
                   -- rehang one onto a relation and that value is no longer a value
                   AND f.object_id IS NOT NULL
                   AND EXISTS (SELECT 1 FROM fact_evidence e
                               WHERE e.fact_id = f.id AND e.proposed_predicate = ANY($2))
                   AND NOT EXISTS (SELECT 1 FROM fact_evidence e
                                   WHERE e.fact_id = f.id AND e.proposed_predicate IS NOT NULL
                                     AND NOT (e.proposed_predicate = ANY($2)))
                 ORDER BY f.recorded_at",
            )
            .bind(kb_id)
            .bind(forms)
            .fetch_all(pool)
            .await?
        }
        AdoptTargets::WithValues(items) => {
            if items.is_empty() {
                return Ok(nothing);
            }
            // The subject has to be read back from the database (the caller gives fact_id and
            // the new value), which also confirms these facts are still alive -- there may have
            // been a re-extraction between the picking and the adoption
            let ids: Vec<Uuid> = items.iter().map(|(id, _)| *id).collect();
            let live: Vec<(Uuid, Uuid)> = sqlx::query_as(
                "SELECT id, subject_id FROM facts
                 WHERE kb_id = $1 AND id = ANY($2) AND invalidated_at IS NULL",
            )
            .bind(kb_id)
            .bind(&ids)
            .fetch_all(pool)
            .await?;
            let subject_of: std::collections::HashMap<Uuid, Uuid> = live.into_iter().collect();
            items
                .iter()
                .filter_map(|(id, value)| {
                    Some((*id, *subject_of.get(id)?, None, Some(value.clone())))
                })
                .collect()
        }
    };

    let mut moved = 0u32;
    let mut left_off = 0u32;
    let mut corrected = 0u32;
    for (old_id, subject_id, object_id, object_value) in targets {
        // Passive-form rewrite: exchange subject and object. Facts with a literal-value object
        // cannot be exchanged (a value cannot be a subject), and the ByForm query only takes rows
        // where object_id is non-null anyway, so here it can only be an entity object
        let (subject_id, object_id) = match (swap, object_id) {
            (true, Some(o)) => (o, Some(subject_id)),
            _ => (subject_id, object_id),
        };
        // **Run the signature check before attaching a predicate** (#190). Extraction writes
        // straighten the direction by domain or leave it empty, and adoption is the second path
        // that writes predicates -- it used to attach the predicate straight back, which measured
        // out at raising the violation rate from 0 to 12.3%, all of it on containment relations.
        // The same judgement (`ontology::judge_direction`), the same three outcomes: if it fits,
        // attach it; if the subject does not fit but the object does, attach it exchanged; if
        // neither fits, **do not attach** -- that fact stays on the empty predicate with the
        // source's wording still in the evidence, the same handling extraction applies in the
        // same situation
        let (subject_id, object_id) = match object_id {
            Some(o) => {
                match crate::ontology::judge_direction(pool, predicate_id, subject_id, o).await? {
                    crate::ontology::Fit::Swap => {
                        corrected += 1;
                        (o, Some(subject_id))
                    }
                    crate::ontology::Fit::Neither => {
                        left_off += 1;
                        continue;
                    }
                    crate::ontology::Fit::Keep | crate::ontology::Fit::Unchecked => {
                        (subject_id, Some(o))
                    }
                }
            }
            None => (subject_id, None),
        };
        let mut tx = pool.begin().await?;
        // The target assertion may already exist (the same subject and object already have a
        // real relation): then merge into it, do not create a duplicate.
        //
        // **Both sides of the object have to be compared.** object_id is NULL on every
        // literal-value fact, so comparing only that amounts to treating every value under the
        // same subject and predicate as one assertion -- (Nebula Tech, founding_date, 2015) and
        // (Nebula Tech, founding_date, 2016) would be merged into one, and the second value would
        // silently disappear
        let existing: Option<(Uuid,)> = sqlx::query_as(
            "SELECT id FROM facts
             WHERE kb_id = $1 AND subject_id = $2 AND predicate_id = $3
               AND object_id IS NOT DISTINCT FROM $4
               AND object_value IS NOT DISTINCT FROM $5
               AND invalidated_at IS NULL",
        )
        .bind(kb_id)
        .bind(subject_id)
        .bind(predicate_id)
        .bind(object_id)
        .bind(&object_value)
        .fetch_optional(&mut *tx)
        .await?;

        let (new_id, mode) = match existing {
            Some((id,)) => (id, ADOPT_MERGED),
            None => {
                let id = Uuid::now_v7();
                // The object is bound explicitly rather than copied from the old row: the new
                // value on the attribute route has been normalised ("2015" -> a date), and
                // copying the old row would mean stuffing the unconverted original value in.
                // On the relation route what is bound is the old row's value, so the behaviour
                // does not change by a word
                let inserted: Option<(Uuid,)> = sqlx::query_as(
                    "INSERT INTO facts (id, kb_id, subject_id, predicate_id, object_id, object_value,
                                        valid_from, valid_from_precision,
                                        valid_to, valid_to_precision, confidence, supersedes)
                     SELECT $1, kb_id, $6, $3, $4, $5,
                            valid_from, valid_from_precision,
                            valid_to, valid_to_precision, confidence, id
                     FROM facts WHERE id = $2 AND invalidated_at IS NULL
                     RETURNING id",
                )
                .bind(id)
                .bind(old_id)
                .bind(predicate_id)
                .bind(object_id)
                .bind(&object_value)
                // The subject is bound explicitly too, no longer copied from the old row --
                // replacing it is exactly what a passive-form rewrite is for. The first version
                // missed this spot: the local variable had been exchanged while the SQL still
                // said subject_id, so the object changed and the subject did not, conjuring an
                // `OpenAI produces OpenAI` out of thin air
                .bind(subject_id)
                .fetch_optional(&mut *tx)
                .await?;
                // Already rewritten concurrently: do not act twice
                let Some((id,)) = inserted else {
                    tx.rollback().await?;
                    continue;
                };
                (id, ADOPT_SUPERSEDED)
            }
        };

        // The evidence moves over wholesale and the surface predicate is kept with it -- it is
        // the basis for this rewrite, and should not be lost in the rewrite
        sqlx::query(
            "INSERT INTO fact_evidence (fact_id, chunk_id, quote, proposed_predicate, document_id, doc_version)
             SELECT $1, chunk_id, quote, proposed_predicate, document_id, doc_version
             FROM fact_evidence WHERE fact_id = $2
             ON CONFLICT DO NOTHING",
        )
        .bind(new_id)
        .bind(old_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query("UPDATE facts SET invalidated_at = now() WHERE id = $1")
            .bind(old_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "INSERT INTO fact_adoptions
                (batch_id, kb_id, predicate_id, old_fact_id, new_fact_id, mode)
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(batch_id)
        .bind(kb_id)
        .bind(predicate_id)
        .bind(old_id)
        .bind(new_id)
        .bind(mode)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        moved += 1;
    }
    Ok(Adopted {
        batch_id,
        moved,
        left_off,
        corrected,
    })
}

/// Undoes one adoption: the newly written rows are invalidated and the old rows come back to
/// life.
///
/// The relation type is **not deleted** -- existing facts have pointed at it
/// (`delete_relation_type` refuses too), and under append-only rules "it existed" is itself
/// history; a relation nobody uses is inert. The evidence is not cleared either: the new rows are
/// invalidated, so their evidence is inert along with them, and deleting it would instead erase
/// "we once believed this".
///
/// The merged kind (mode = merged) only revives the old row and leaves the merge target alone --
/// it was already there, and the evidence copied over does no harm by staying (`ON CONFLICT DO
/// NOTHING` means it may well have been its own anyway).
pub async fn unadopt(pool: &PgPool, kb_id: Uuid, batch_id: Uuid) -> AppResult<u32> {
    let rows: Vec<(Uuid, Uuid, String)> = sqlx::query_as(
        "SELECT old_fact_id, new_fact_id, mode FROM fact_adoptions
         WHERE batch_id = $1 AND kb_id = $2 AND reverted_at IS NULL",
    )
    .bind(batch_id)
    .bind(kb_id)
    .fetch_all(pool)
    .await?;
    if rows.is_empty() {
        return Err(AppError::NotFound);
    }

    let mut tx = pool.begin().await?;
    let mut reverted = 0u32;
    for (old_id, new_id, mode) in &rows {
        if mode == ADOPT_SUPERSEDED {
            sqlx::query(
                "UPDATE facts SET invalidated_at = now() WHERE id = $1 AND invalidated_at IS NULL",
            )
            .bind(new_id)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query("UPDATE facts SET invalidated_at = NULL WHERE id = $1")
            .bind(old_id)
            .execute(&mut *tx)
            .await?;
        reverted += 1;
    }
    // Mark rather than delete: this adoption happened and so did the revert, and both are
    // history
    sqlx::query(
        "UPDATE fact_adoptions SET reverted_at = now()
         WHERE batch_id = $1 AND kb_id = $2 AND reverted_at IS NULL",
    )
    .bind(batch_id)
    .bind(kb_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(reverted)
}

/// **Literal-value** wordings outside the vocabulary: the ones with no predicate yet whose object
/// is a value rather than an entity.
///
/// Complementary to [`proposed_predicates`], the two strictly separated by whether `object_id` is
/// null. Mixed together, the suggestion would propose `founding_date` as a relation, and that is
/// exactly what this path is here to fix.
pub async fn proposed_attributes(
    pool: &PgPool,
    kb_id: Uuid,
) -> AppResult<Vec<utopia_core::models::ProposedAttribute>> {
    Ok(sqlx::query_as(
        // Same as proposed_predicates: prevalence is counted from all the evidence, the rewrite
        // volume from the backlog; via a CTE rather than a correlated subquery, since the latter
        // rescans the evidence table once per group
        "WITH spread AS (
             SELECT e.proposed_predicate AS form,
                    count(DISTINCT e.document_id) AS doc_count
             FROM fact_evidence e
             JOIN facts ff ON ff.id = e.fact_id
             WHERE ff.kb_id = $1 AND e.proposed_predicate IS NOT NULL
             GROUP BY 1
         )
         SELECT fe.proposed_predicate AS form,
                count(DISTINCT f.id) AS fact_count,
                max(sp.doc_count) AS doc_count,
                (SELECT f2.object_value::text
                 FROM fact_evidence e2
                 JOIN facts f2 ON f2.id = e2.fact_id
                 WHERE e2.proposed_predicate = fe.proposed_predicate
                   AND f2.kb_id = $1 AND f2.predicate_id IS NULL
                   AND f2.object_id IS NULL AND f2.invalidated_at IS NULL
                 LIMIT 1) AS example,
                -- What class the subject actually is: an attribute's domain comes from here,
                -- not from guessing
                ARRAY(SELECT DISTINCT t.key
                      FROM fact_evidence e3
                      JOIN facts f3 ON f3.id = e3.fact_id
                      JOIN entities s ON s.id = f3.subject_id
                      JOIN entity_types t ON t.id = s.type_id
                      WHERE e3.proposed_predicate = fe.proposed_predicate
                        AND f3.kb_id = $1 AND f3.predicate_id IS NULL
                        AND f3.object_id IS NULL AND f3.invalidated_at IS NULL) AS domain_keys
         FROM fact_evidence fe
         JOIN facts f ON f.id = fe.fact_id
         JOIN spread sp ON sp.form = fe.proposed_predicate
         WHERE f.kb_id = $1 AND f.predicate_id IS NULL
           AND f.invalidated_at IS NULL AND fe.proposed_predicate IS NOT NULL
           AND f.object_id IS NULL
           -- Dismissed wordings no longer appear among the candidates
           AND NOT EXISTS (SELECT 1 FROM ontology_misses m
                           WHERE m.kb_id = $1 AND m.kind = 'attribute_type'
                             AND m.key = fe.proposed_predicate AND m.dismissed_at IS NOT NULL)
         GROUP BY fe.proposed_predicate
         ORDER BY fact_count DESC, form",
    )
    .bind(kb_id)
    .fetch_all(pool)
    .await?)
}

/// The facts currently hanging off a few literal-value wordings: id, the subject's type, the raw
/// value.
///
/// For the adoption step to use. **Normalisation does not happen here** -- it turns "2015" into a
/// date and "1,200" into a number according to the datatype, and that set of rules lives in the
/// extraction module, which the store cannot reach and should not be able to reach. The caller
/// normalises and hands the result straight back.
pub async fn value_facts_for_forms(
    pool: &PgPool,
    kb_id: Uuid,
    forms: &[String],
) -> AppResult<Vec<(Uuid, Uuid, serde_json::Value)>> {
    if forms.is_empty() {
        return Ok(Vec::new());
    }
    Ok(sqlx::query_as(
        "SELECT DISTINCT f.id, s.type_id, f.object_value
         FROM facts f
         JOIN entities s ON s.id = f.subject_id
         WHERE f.kb_id = $1 AND f.predicate_id IS NULL AND f.invalidated_at IS NULL
           AND f.object_id IS NULL AND f.object_value IS NOT NULL
           AND EXISTS (SELECT 1 FROM fact_evidence e
                       WHERE e.fact_id = f.id AND e.proposed_predicate = ANY($2))",
    )
    .bind(kb_id)
    .bind(forms)
    .fetch_all(pool)
    .await?)
}

/// Adopts a batch of **literal-value** wordings: rehangs their facts onto some attribute.
///
/// Shares the rewrite, the batch and the undo with [`adopt_proposed_predicates`] -- what is done
/// to the graph is the same thing, and only "where the new object comes from" differs: the values
/// here have been normalised by the caller according to the attribute's datatype, and the ones
/// that cannot be converted never get passed in at all (they go on having no predicate, and wait
/// for next time).
pub async fn adopt_value_facts(
    pool: &PgPool,
    kb_id: Uuid,
    attribute_id: Uuid,
    rewrites: &[(Uuid, serde_json::Value)],
) -> AppResult<Adopted> {
    // There is no exchanging on the attribute route: the object is a literal value, and a value
    // cannot be a subject
    adopt(
        pool,
        kb_id,
        attribute_id,
        AdoptTargets::WithValues(rewrites),
        false,
    )
    .await
}
