//! Entity resolution v2: same name ≠ same person (flow chart and three rounds of measured
//! hole-patching in `docs/pipeline.md`, section two).
//!
//! The funnel: name candidate recall (free, includes mutual inference through generic-suffix stems)
//! → profile vector similarity tiering (milliseconds, reuses the chunk embedding from the ingest
//! stage) → in the grey zone create a new entity + a suspected-duplicate review item (rather
//! split than wrongly merge); batched LLM adjudication runs in a separate background task, with
//! human final review as the backstop. The LLM is never on the critical path of an extraction
//! write.
//!
//! Type drift: when a name has no candidate under the same type, look under the other types (the
//! same team getting extracted as organization/project/concept is the norm) and route by how
//! strongly the type pair is disjoint -- the concept fallback type becomes a recall candidate and
//! goes through profile tiering, confusable concrete types still get their entity created but a
//! review pair enqueued, and hard-disjoint pairs (person vs organization) stay entirely apart.

use chrono::{DateTime, Utc};
use pgvector::Vector;
use sqlx::PgPool;
use std::collections::HashSet;
use utopia_core::models::{MergeLogView, ReviewItem, ReviewSide};
use utopia_core::{AppError, AppResult};
use uuid::Uuid;

/// Context similarity thresholds (empirical cosine values for bge-m3 class models, tunable later).
/// ≥ ATTACH attaches to the existing entity; < NEW is judged a different entity; the grey zone in
/// between splits rather than merges + files a review item.
pub const SIM_ATTACH: f32 = 0.55;
pub const SIM_NEW: f32 = 0.35;

/// Name normalization: full-width ASCII → half-width, ideographic space → plain space, and
/// whitespace collapsed.
/// Returns the display form (case preserved); matching always wraps it in SQL lower() anyway.
pub fn normalize_name(raw: &str) -> String {
    let mapped: String = raw
        .chars()
        .map(|c| match c {
            '\u{3000}' => ' ',
            '\u{FF01}'..='\u{FF5E}' => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
            _ => c,
        })
        .collect();
    mapped.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Generic suffix lists: Chinese suffixes attach straight onto the stem; English ones count as
/// standalone words and both orders are accepted ("Phoenix Project" / "Project Phoenix"). This
/// only affects recall; the decision still goes through profile similarity.
const GENERIC_SUFFIXES_CJK: &[&str] = &["项目", "公司", "集团", "部门", "团队"];
const GENERIC_WORDS_EN: &[&str] = &["project", "corp", "inc", "team"];

/// Stem: the lowercased form left after stripping one generic suffix. Returns None when nothing
/// matches, when stripping leaves nothing, or when the stem is itself a generic word
/// ("项目团队"). The input should already have been through normalize_name.
pub fn name_stem(name: &str) -> Option<String> {
    let lower = name.to_lowercase();
    let strip_punct = |w: &str| w.trim_end_matches(['.', ',']).to_string();
    let generic = |s: &str| {
        GENERIC_SUFFIXES_CJK.contains(&s) || GENERIC_WORDS_EN.contains(&strip_punct(s).as_str())
    };
    for suf in GENERIC_SUFFIXES_CJK {
        if let Some(stem) = lower.strip_suffix(suf) {
            let stem = stem.trim_end();
            if !stem.is_empty() && !generic(stem) {
                return Some(stem.to_string());
            }
        }
    }
    let words: Vec<&str> = lower.split(' ').collect();
    if words.len() >= 2 {
        if generic(words[words.len() - 1]) {
            let stem = words[..words.len() - 1].join(" ");
            if !generic(&stem) {
                return Some(stem);
            }
        }
        if generic(words[0]) {
            let stem = words[1..].join(" ");
            if !generic(&stem) {
                return Some(stem);
            }
        }
    }
    None
}

/// The recall key set for a mention (all lowercased): the name itself + its stem + the stem
/// augmented with generic suffixes. The augmentation covers the reverse direction (the store holds
/// "星尘项目" while the mention only says "星尘"); if the stem contains CJK we append Chinese
/// suffixes, otherwise English words (in both orders). At most 10 keys, served as a multi-point
/// lookup on the (kb,type,lower(name)) index.
pub fn recall_keys(name: &str) -> Vec<String> {
    let lower = name.to_lowercase();
    let base = name_stem(name).unwrap_or_else(|| lower.clone());
    let mut keys = vec![lower];
    fn add(keys: &mut Vec<String>, k: String) {
        if !keys.contains(&k) {
            keys.push(k);
        }
    }
    add(&mut keys, base.clone());
    if base.chars().any(|c| ('\u{4E00}'..='\u{9FFF}').contains(&c)) {
        for suf in GENERIC_SUFFIXES_CJK {
            add(&mut keys, format!("{base}{suf}"));
        }
    } else {
        for w in GENERIC_WORDS_EN {
            add(&mut keys, format!("{base} {w}"));
            add(&mut keys, format!("{w} {base}"));
        }
    }
    keys
}

// ---------------------------------------------------------------------------
// Type drift: same-name entities got extracted under different types ("Orion platform team" ↔
// organization/project)
// ---------------------------------------------------------------------------

/// Confusable concrete types: extraction keeps wavering between these (is a team an organization
/// or a project? is a platform a project or a product?).
/// A shared name across this set of types → create the entity anyway (rather split than wrongly
/// merge), but enqueue a review pair for LLM/human adjudication.
///
/// **The ontology has the last word; this table is only the fallback when nothing is declared.**
/// To judge whether two classes can refer to the same thing, look first at `owl:disjointWith`
/// (`entity_type_disjoint`, inheritance included: Person ⟂ Organization makes
/// Corporation ⟂ Person) -- anything declared disjoint is always kept apart, even if the two are
/// kin in the class hierarchy; for undeclared pairs look at the class hierarchy next (the same
/// branch counts as confusable, #226), and only then at these three hard keys.
/// In a knowledge base with no package installed and nothing declared these three keys do not
/// exist, so every cross-type name match is judged `Disjoint` -- stricter, never looser, so it
/// cannot merge wrongly (0016 B3)
pub const CONFUSABLE_TYPE_KEYS: &[&str] = &["organization", "project", "product"];

/// Most drift review pairs a single resolution may enqueue (keeps a big same-name group from
/// flooding the review queue).
const MAX_DRIFT_REVIEWS: usize = 4;

/// How a cross-type name match gets handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TypeDrift {
    /// One side is a fallback type: treat as a recall candidate and run profile similarity
    /// tiering (an ATTACH is possible)
    Recall,
    /// Two confusable concrete types: create a new entity + a review pair
    Review,
    /// Hard disjoint (person vs organization and the like, including unknown custom types):
    /// entirely apart
    Disjoint,
}

fn classify_type_drift(a: Option<&str>, b: Option<&str>) -> TypeDrift {
    // **Two things of the same type can of course be the same thing.**
    //
    // This arm did not exist originally, because the function was born to serve "type drift"
    // alone -- one name extracted under two types -- and there both sides being equal simply
    // cannot happen. Later containment_reviews borrowed it as a compatibility test, and there
    // **both sides being equal is the most common case**, so person against person fell through
    // to that last Disjoint line and was read as "can never be the same thing".
    //
    // The price was that not one of the most obvious coreference pairs in the text made it into
    // the queue: across the first six Holmes stories, Sherlock Holmes and Holmes were two
    // entities, and out of 488 entities only 14 got merged.
    // All 12 of the existing unit tests were testing cross-type pairs; not one tested the same
    // type.
    if a == b {
        return TypeDrift::Recall;
    }
    // **One side has not been determined yet** → treat as a recall candidate and run profile
    // similarity tiering.
    // This used to compare `== FALLBACK_TYPE_KEY`; that key is gone now:
    // "not determined yet" is expressed by `None` these days (0009). Both sides
    // being None is already caught by the `a == b` line above
    let (Some(a), Some(b)) = (a, b) else {
        return TypeDrift::Recall;
    };
    if CONFUSABLE_TYPE_KEYS.contains(&a) && CONFUSABLE_TYPE_KEYS.contains(&b) {
        return TypeDrift::Review;
    }
    TypeDrift::Disjoint
}

fn cosine(a: &[f32], b: &[f32]) -> Option<f32> {
    if a.len() != b.len() || a.is_empty() {
        return None;
    }
    let (mut dot, mut na, mut nb) = (0f32, 0f32, 0f32);
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        return None;
    }
    Some(dot / (na.sqrt() * nb.sqrt()))
}

#[derive(Debug, sqlx::FromRow)]
struct Candidate {
    id: Uuid,
    profile_embedding: Option<Vector>,
    profile_n: i32,
    degree: i64,
}

/// Resolution result: which entity the mention landed on, plus the suspected-duplicate review
/// pairs that need enqueuing (same-name grey zone / type drift); the caller writes them into the
/// review queue and kicks off the adjudication task.
#[derive(Debug)]
pub struct Resolution {
    pub entity_id: Uuid,
    pub created: bool,
    pub reviews: Vec<ReviewRequest>,
}

/// A review pair waiting to be enqueued: `Resolution::entity_id` vs `other_id`.
#[derive(Debug)]
pub struct ReviewRequest {
    pub other_id: Uuid,
    pub score: f32,
    pub reason: String,
}

/// Resolve a single mention. `context` is the vector of the chunk the mention sits in (None when
/// there is no embedding model, which degrades to v1 behavior: a name match attaches to the
/// candidate with the most facts).
pub async fn resolve_mention(
    pool: &PgPool,
    kb_id: Uuid,
    // None = the extractor's type is not in the ontology, or the KB has no classes at all (0009)
    type_id: Option<Uuid>,
    raw_name: &str,
    context: Option<&[f32]>,
) -> AppResult<Resolution> {
    let name = normalize_name(raw_name);
    // Recall keys = the name itself + the generic-suffix stem and its augmentations
    // ("星尘" ↔ "星尘项目" become candidates for each other).
    // This only widens recall; whether to merge is still settled by the profile similarity
    // tiering below.
    let keys = recall_keys(&name);
    let candidates: Vec<Candidate> = sqlx::query_as(
        "SELECT e.id, e.profile_embedding, e.profile_n,
                (SELECT count(*) FROM facts f
                 WHERE (f.subject_id = e.id OR f.object_id = e.id)
                   AND f.invalidated_at IS NULL) AS degree
         FROM entities e
         WHERE e.kb_id = $1 AND e.type_id = $2 AND e.merged_into IS NULL
           AND (lower(e.canonical_name) = ANY($3)
                OR EXISTS (SELECT 1 FROM unnest(e.aliases) a WHERE lower(a) = ANY($3)))",
    )
    .bind(kb_id)
    .bind(type_id)
    .bind(&keys)
    .fetch_all(pool)
    .await?;

    if candidates.is_empty() {
        // No candidate under the same type ≠ a new name: type labels drift (the same team gets
        // extracted as organization/project/concept), so look for same-name entities under the
        // other types first and route by how disjoint the type pair is.
        return resolve_type_drift(pool, kb_id, type_id, &name, &keys, context).await;
    }

    let Some(ctx) = context else {
        // No vector to compare: v1 compatibility -- attach to the same-name candidate that has
        // the most facts
        let best = candidates
            .iter()
            .max_by_key(|c| c.degree)
            .expect("non-empty");
        touch_entity(pool, best.id).await?;
        return Ok(Resolution {
            entity_id: best.id,
            created: false,
            reviews: Vec::new(),
        });
    };

    // Candidates with a profile get scored for similarity; the ones without (legacy data, or
    // created during a period with no embedding model) are bucketed separately
    let mut best_scored: Option<(&Candidate, f32)> = None;
    let mut unprofiled: Option<&Candidate> = None;
    for c in &candidates {
        match c
            .profile_embedding
            .as_ref()
            .and_then(|p| cosine(p.as_slice(), ctx))
        {
            Some(sim) => {
                if best_scored.map(|(_, s)| sim > s).unwrap_or(true) {
                    best_scored = Some((c, sim));
                }
            }
            None => {
                if unprofiled.map(|u| c.degree > u.degree).unwrap_or(true) {
                    unprofiled = Some(c);
                }
            }
        }
    }

    if let Some((best, sim)) = best_scored {
        if sim >= SIM_ATTACH {
            update_profile(pool, best.id, best.profile_n, ctx).await?;
            return Ok(Resolution {
                entity_id: best.id,
                created: false,
                reviews: Vec::new(),
            });
        }
    }
    if let Some(c) = unprofiled {
        // There is no way to judge a profile-less candidate: attach for v1 compatibility, and
        // initialize its profile from this context
        update_profile(pool, c.id, c.profile_n, ctx).await?;
        return Ok(Resolution {
            entity_id: c.id,
            created: false,
            reviews: Vec::new(),
        });
    }

    // Reaching here means every candidate has a profile and the top score is < ATTACH → create a
    // new entity (same name, not the same person)
    let id = create_entity(pool, kb_id, type_id, &name, context).await?;
    refresh_disambiguators(pool, kb_id, &name).await?;
    let mut reviews = best_scored
        .filter(|(_, sim)| *sim >= SIM_NEW)
        .map(|(c, sim)| {
            vec![ReviewRequest {
                other_id: c.id,
                score: sim,
                reason: format!("ambiguous_name|{sim:.2}"),
            }]
        })
        .unwrap_or_default();
    // Existing entities whose names contain each other: equality recall cannot see them (the
    // prefixes cannot all be enumerated), so a short form silently becomes a second entity.
    // Enqueue only, never merge
    reviews.extend(containment_reviews(pool, kb_id, type_id, &name, id, context).await?);
    Ok(Resolution {
        entity_id: id,
        created: true,
        reviews,
    })
}

/// Lower bound for containment candidates: the **shorter** of the two names has to be at least
/// this long to count. Below that they are mostly generic words like "研究院" or "中心",
/// where a pairing carries no information at all and only floods the queue.
const MIN_CONTAIN_CHARS: i32 = 4;

/// Most containment review pairs produced in one pass. Same reasoning as `MAX_DRIFT_REVIEWS`:
/// one generic word can be contained in dozens of entities, and putting them all in drowns the
/// queue.
const MAX_CONTAIN_REVIEWS: usize = 4;

/// Fetch a few extra rows on the SQL side: hard-disjoint types can only be filtered out on the
/// Rust side, so if we took only 4 rows all 4 could be disjoint types and the pair that actually
/// mattered would be cut off by the LIMIT.
const CONTAIN_SCAN_LIMIT: i64 = 16;

/// After creating a new entity, find the existing entities whose **names contain each other** and
/// offer them as review candidates.
///
/// Chinese business text switches from the full name to a short form inside a single document
/// ("星云科技上海研究院" → "上海研究院"), while [`recall_keys`] is an equality lookup: it hits
/// the reverse direction by enumerating generic suffixes, but the prefix can be any
/// organization name, and **there is no enumerating that**. So all three of these slip through
/// and silently become a second entity:
///
/// ```text
/// 上海研究院       ⊂ 星云科技上海研究院      suffix gets qualified
/// 启明 X7 加速卡   ~ 启明 X7 推理加速卡      word inserted mid-name (LIKE misses it, below)
/// 沧海             ⊂ 沧海分布式推理平台 2.0  prefix gets extended
/// ```
///
/// **Candidates only, never an automatic merge.** The same-name candidate path merges outright
/// once similarity is ≥ [`SIM_ATTACH`]; containment must never take that road: `华瑞集团技术中心`
/// and `星云科技技术中心` both contain "技术中心", and inside one document their context
/// similarity clears the line easily -- yet they are two different departments. Rather split
/// than merge.
///
/// **Aliases take part as well**: merging moves names into `aliases`, so looking only at
/// `canonical_name` loses one recall bridge with every successful merge -- once `Holmes` has been
/// merged into `Sherlock Holmes`, a later `Mr. Holmes` can never hook on again (neither it nor
/// `Sherlock Holmes` contains the other).
/// The more successful merging is, the more we miss: a hole that aggravates itself.
///
/// **Known gap**: the case where two names neither contain each other nor share an alias to bridge
/// them (`启明 X7 加速卡` and `启明 X7 推理加速卡`). That would need trigram similarity,
/// and `CREATE EXTENSION pg_trgm` requires superuser -- this repo connects to the database with a
/// restricted role (see `migrations/0010_least_privilege_role.sql`), so installing the extension
/// would fail on deployment. Left until we actually need it.
///
/// **Performance**: the reverse half (the new name containing the old one) cannot use any index,
/// and the alias half is the same, so this narrows the row set by `kb_id` and caps it, and runs
/// once **only when a new entity is created**, not for every mention. If that is not enough on a
/// large knowledge base, the right answer is a "suffix key" table with an equality lookup,
/// not a fuzzy index.
/// One row of the containment scan: (id, name, type key, type id, profile). The type id is there to
/// compare against the disjointness the ontology declares
type ContainRow = (Uuid, String, Option<String>, Option<Uuid>, Option<Vector>);

async fn containment_reviews(
    pool: &PgPool,
    kb_id: Uuid,
    // None = the extractor's type is not in the ontology, or the KB has no classes at all (0009)
    type_id: Option<Uuid>,
    name: &str,
    new_id: Uuid,
    ctx: Option<&[f32]>,
) -> AppResult<Vec<ReviewRequest>> {
    let lower = name.to_lowercase();
    if lower.chars().count() < MIN_CONTAIN_CHARS as usize {
        return Ok(Vec::new());
    }
    // The entity may not have a type determined yet (0009), and then there is no key to look up
    let mention_key: Option<String> = match type_id {
        Some(t) => sqlx::query_as::<_, (String,)>("SELECT key FROM entity_types WHERE id = $1")
            .bind(t)
            .fetch_optional(pool)
            .await?
            .map(|(k,)| k),
        None => None,
    };
    // **No filtering by type**: the short form often falls into the concept fallback while the
    //  full name gets a concrete type (measured: 上海研究院→concept vs
    //  星云科技上海研究院→organization, 启明 X7 加速卡→concept vs
    //  启明 X7 推理加速卡→product), so looking them up by type equality catches neither
    //  pair. Compatibility is left to classify_type_drift below.
    //  Take a few extra rows, because the hard-disjoint ones get filtered out on the Rust side
    // The classes the ontology declares disjoint from this one (inheritance included), fetched in
    // one go and compared row by row on id
    let disjoint = declared_disjoint_from(pool, kb_id, type_id).await?;
    // Third column is nullable: unclassified entities have to join the containment scan too (0009)
    let rows: Vec<ContainRow> = sqlx::query_as(
        "SELECT e.id, e.canonical_name, t.key, e.type_id, e.profile_embedding
         FROM entities e LEFT JOIN entity_types t ON t.id = e.type_id
         WHERE e.kb_id = $1 AND e.merged_into IS NULL
           AND e.id <> $2
           AND lower(e.canonical_name) <> $3
           AND (
             (char_length(e.canonical_name) >= $4
              AND (lower(e.canonical_name) LIKE '%' || $3 || '%'
                   OR $3 LIKE '%' || lower(e.canonical_name) || '%'))
             -- **Aliases have to join recall too, or every merge tears down a bridge.**
             --
             -- Merging moves names into aliases: once Holmes is merged into Sherlock Holmes,
             -- the Holmes row has a non-null merged_into and is filtered out by the first
             -- condition above. But the Mr. Holmes that shows up ten minutes later contains
             -- neither Sherlock Holmes nor the other way round -- Holmes was exactly what
             -- bridged them. That is measurably how it leaked: after the same-type bug was
             -- fixed and Holmes merged correctly, Mr. Holmes could never get into the queue.
             --
             -- The more successful merging is, the more bridges it tears down. This hole
             -- aggravates itself.
             OR EXISTS (
               SELECT 1 FROM unnest(e.aliases) AS alias
               WHERE char_length(alias) >= $4
                 AND (lower(alias) LIKE '%' || $3 || '%'
                      OR $3 LIKE '%' || lower(alias) || '%'))
           )
         ORDER BY char_length(e.canonical_name)
         LIMIT $5",
    )
    .bind(kb_id)
    .bind(new_id)
    .bind(&lower)
    .bind(MIN_CONTAIN_CHARS)
    .bind(CONTAIN_SCAN_LIMIT)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        // Which type pairs can refer to the same thing has already been thought through by the
        // existing rules; do not invent a second set: what the ontology declares disjoint never
        // merges, person vs organization never merges, and the concept fallback can be one and
        // the same as anything
        .filter(|(_, _, type_key, other_type, _)| {
            !other_type.is_some_and(|t| disjoint.contains(&t))
                && classify_type_drift(mention_key.as_deref(), type_key.as_deref())
                    != TypeDrift::Disjoint
        })
        .take(MAX_CONTAIN_REVIEWS)
        .map(|(id, other_name, _, _, emb)| {
            // The score is only a hint for ordering the queue, it **takes no part in deciding
            // whether to merge** -- that decision was never on this path anyway
            let score = ctx
                .and_then(|x| emb.as_ref().and_then(|p| cosine(p.as_slice(), x)))
                .unwrap_or(0.0);
            ReviewRequest {
                other_id: id,
                score,
                reason: format!("contains|{other_name}"),
            }
        })
        .collect())
}

#[derive(Debug, sqlx::FromRow)]
struct CrossCandidate {
    id: Uuid,
    canonical_name: String,
    // None = this candidate has no type determined yet (0009)
    type_key: Option<String>,
    // Same as above; `types_are_kin` walks the class hierarchy by id
    type_id: Option<Uuid>,
    // Promotion during extraction has to consult this: when a human has said "it simply has no
    // type", that is a decision too
    type_source: String,
    profile_embedding: Option<Vector>,
    profile_n: i32,
}

/// `reason` stores a code, the wording belongs to the interface (docs/decisions/0004) -- this
/// column should not be half code and half English sentence, because on a Chinese interface that
/// comes out half translatable and half not.
fn drift_reason(mention_key: Option<&str>, other_key: Option<&str>, sim: Option<f32>) -> String {
    // An unclassified side is written as `(untyped)`. This column stores a code for the interface
    // to translate, and leaving it empty turns `a vs b` into `a vs `, which reads like truncation
    // rather than "absent"
    let a = mention_key.unwrap_or("(untyped)");
    let b = other_key.unwrap_or("(untyped)");
    match sim {
        Some(s) => format!("type_drift|{a} vs {b} {s:.2}"),
        None => format!("type_drift|{a} vs {b}"),
    }
}

/// Every class the ontology declares disjoint from this one (0016 B3).
///
/// **Disjointness is inherited**: one declaration of Person ⟂ Organization makes every subclass
/// of Person disjoint from every subclass of Organization. So we first walk up the parent chain to
/// collect this class's ancestors, take what they declare disjoint, and then expand down the child
/// chain. The table stores a row for each direction, so asking one direction is enough.
///
/// With no type determined (`None`) there is no class to ask about, so return the empty set: that
/// side goes into the recall-candidate arm anyway
async fn declared_disjoint_from(
    pool: &PgPool,
    kb_id: Uuid,
    type_id: Option<Uuid>,
) -> AppResult<HashSet<Uuid>> {
    let Some(type_id) = type_id else {
        return Ok(HashSet::new());
    };
    let rows: Vec<(Uuid,)> = sqlx::query_as(
        "WITH RECURSIVE up(id) AS (
             SELECT $2::uuid
             UNION
             SELECT p.parent_id FROM entity_type_parents p JOIN up ON p.child_id = up.id
         ), hit(id) AS (
             SELECT d.b_id FROM entity_type_disjoint d JOIN up ON d.a_id = up.id
              WHERE d.kb_id = $1
         ), down(id) AS (
             SELECT id FROM hit
             UNION
             SELECT p.child_id FROM entity_type_parents p JOIN down ON p.parent_id = down.id
         )
         SELECT id FROM down",
    )
    .bind(kb_id)
    .bind(type_id)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

/// Whether two classes are kin: one is an ancestor of the other, or they share an ancestor that is
/// **not the root**.
///
/// The three-key hard table in `CONFUSABLE_TYPE_KEYS` is there for knowledge bases with no package
/// installed; once schema.org is installed the same company gets extracted as three types --
/// Organization / Corporation / OnlineBusiness -- all of them under Organization, yet not one of
/// those pairs passes the hard table, so three same-name entities coexist, the review queue holds
/// nothing at all, and the dashboard still points at Review saying "go merge them there" (#226).
///
/// The root does not count as a shared ancestor: in schema.org everything is a Thing, and counting
/// it would make Person and Organization kin as well. Vocabularies with no root (W3C Org's
/// Organization is its own top) are caught by the ancestor/descendant half
async fn types_are_kin(pool: &PgPool, a: Uuid, b: Uuid) -> AppResult<bool> {
    let (kin,): (bool,) = sqlx::query_as(
        "WITH RECURSIVE up_a(id) AS (
             SELECT $1::uuid
             UNION
             SELECT p.parent_id FROM entity_type_parents p JOIN up_a ON p.child_id = up_a.id
         ), up_b(id) AS (
             SELECT $2::uuid
             UNION
             SELECT p.parent_id FROM entity_type_parents p JOIN up_b ON p.child_id = up_b.id
         )
         SELECT EXISTS (SELECT 1 FROM up_a WHERE id = $2)
             OR EXISTS (SELECT 1 FROM up_b WHERE id = $1)
             OR EXISTS (SELECT 1 FROM up_a JOIN up_b USING (id)
                         WHERE EXISTS (SELECT 1 FROM entity_type_parents p
                                        WHERE p.child_id = up_a.id))",
    )
    .bind(a)
    .bind(b)
    .fetch_one(pool)
    .await?;
    Ok(kin)
}

fn confusable_reviews(
    // None = this side has no type determined yet (0009)
    mention_key: Option<&str>,
    cands: &[&CrossCandidate],
    ctx: Option<&[f32]>,
) -> Vec<ReviewRequest> {
    cands
        .iter()
        .map(|c| {
            let sim = ctx.and_then(|x| {
                c.profile_embedding
                    .as_ref()
                    .and_then(|p| cosine(p.as_slice(), x))
            });
            ReviewRequest {
                other_id: c.id,
                score: sim.unwrap_or(0.0),
                reason: drift_reason(mention_key, c.type_key.as_deref(), sim),
            }
        })
        .collect()
}

/// Cross-type handling for when same-type recall comes back empty (type drift).
/// Concept-fallback candidates run the existing profile tiering: high similarity ATTACHes outright
/// (recall repaired for the drift, and the concept side gets promoted to the concrete type), grey
/// zone / nothing to judge by → rather split than merge, so create + a review pair; confusable
/// concrete types always create + a review pair (a shared name plus a wavering type is itself the
/// signal, so there is no similarity threshold); hard-disjoint pairs are ignored.
async fn resolve_type_drift(
    pool: &PgPool,
    kb_id: Uuid,
    // None = the extractor's type is not in the ontology, or the KB has no classes at all (0009)
    type_id: Option<Uuid>,
    name: &str,
    keys: &[String],
    context: Option<&[f32]>,
) -> AppResult<Resolution> {
    // This side may have no type determined yet either (0009), and then there is no key to look up
    let mention_key: Option<String> = match type_id {
        Some(t) => sqlx::query_as::<_, (String,)>("SELECT key FROM entity_types WHERE id = $1")
            .bind(t)
            .fetch_optional(pool)
            .await?
            .map(|(k,)| k),
        None => None,
    };
    let cross: Vec<CrossCandidate> = sqlx::query_as(
        "SELECT e.id, e.canonical_name, t.key AS type_key, e.type_id, e.type_source,
                e.profile_embedding, e.profile_n
         FROM entities e LEFT JOIN entity_types t ON t.id = e.type_id
         -- IS DISTINCT FROM rather than <>: the latter returns NULL against NULL, WHERE takes
         -- that as false, and unclassified entities get missed entirely (0009)
         WHERE e.kb_id = $1 AND e.type_id IS DISTINCT FROM $2 AND e.merged_into IS NULL
           AND (lower(e.canonical_name) = ANY($3)
                OR EXISTS (SELECT 1 FROM unnest(e.aliases) a WHERE lower(a) = ANY($3)))",
    )
    .bind(kb_id)
    .bind(type_id)
    .bind(keys)
    .fetch_all(pool)
    .await?;

    // The classes the ontology declares disjoint, fetched in one go (inheritance included). A
    // declaration outranks every heuristic below
    let disjoint = declared_disjoint_from(pool, kb_id, type_id).await?;
    let mut recall_cands: Vec<&CrossCandidate> = Vec::new();
    let mut review_cands: Vec<&CrossCandidate> = Vec::new();
    for c in &cross {
        let mut drift = classify_type_drift(mention_key.as_deref(), c.type_key.as_deref());
        if c.type_id.is_some_and(|t| disjoint.contains(&t)) {
            // The ontology says these two classes are disjoint: keep them apart even if the
            // hard table calls them confusable and the class hierarchy calls them kin.
            // A declaration is a judgement a human wrote down; the heuristics are only a guess
            // for when nothing was declared
            drift = TypeDrift::Disjoint;
        } else if drift == TypeDrift::Disjoint {
            // What the hard table cannot place, look at the class hierarchy next: a shared name
            // within the same branch counts as confusable and goes to the review queue
            if let (Some(a), Some(b)) = (type_id, c.type_id) {
                if types_are_kin(pool, a, b).await? {
                    drift = TypeDrift::Review;
                }
            }
        }
        match drift {
            TypeDrift::Recall => recall_cands.push(c),
            TypeDrift::Review => review_cands.push(c),
            TypeDrift::Disjoint => {}
        }
    }

    if let Some(ctx) = context {
        let best = recall_cands
            .iter()
            .filter_map(|c| {
                c.profile_embedding
                    .as_ref()
                    .and_then(|p| cosine(p.as_slice(), ctx))
                    .map(|sim| (*c, sim))
            })
            .max_by(|a, b| a.1.total_cmp(&b.1));
        if let Some((best, sim)) = best {
            if sim >= SIM_ATTACH {
                update_profile(pool, best.id, best.profile_n, ctx).await?;
                // The candidate had no type determined yet and this extraction determined one
                // → promote. This is not a merge (there is no second entity), so nothing goes
                // into entity_merges; the ontology page can change it back by hand
                //
                // **A human saying "no type" is not the same as "not determined yet".** Since
                // 0009 both are NULL, and looking only at type_key.is_none() cannot tell them
                // apart -- so an entity a human looked at and judged to have no fitting class in
                // the ontology would get a type slapped on it at the next extraction
                if best.type_key.is_none() && best.type_source != "human" && type_id.is_some() {
                    sqlx::query(
                        "UPDATE entities
                         SET type_id = $2, type_source = 'extracted', updated_at = now()
                         WHERE id = $1",
                    )
                    .bind(best.id)
                    .bind(type_id)
                    .execute(pool)
                    .await?;
                    // The disambiguator that falls back to the type label may be stale now
                    refresh_disambiguators(pool, kb_id, &best.canonical_name).await?;
                }
                // The mention has settled on the recalled entity; the doubt about same-name
                // confusable-type entities still stands → enqueue as usual
                let mut reviews =
                    confusable_reviews(mention_key.as_deref(), &review_cands, Some(ctx));
                reviews.truncate(MAX_DRIFT_REVIEWS);
                return Ok(Resolution {
                    entity_id: best.id,
                    created: false,
                    reviews,
                });
            }
        }
    }

    let id = create_entity(pool, kb_id, type_id, name, context).await?;
    if !cross.is_empty() {
        // The same name now coexists across types: disambiguators group by name (not by type),
        // so they need refreshing
        refresh_disambiguators(pool, kb_id, name).await?;
    }
    let mut reviews = confusable_reviews(mention_key.as_deref(), &review_cands, context);
    for c in &recall_cands {
        let sim = context.and_then(|ctx| {
            c.profile_embedding
                .as_ref()
                .and_then(|p| cosine(p.as_slice(), ctx))
        });
        match sim {
            // The profiles are clearly unalike: keep them entirely apart, do not bother the
            // review queue
            Some(s) if s < SIM_NEW => {}
            // Grey zone, or nothing to judge by (no embedding / candidate has no profile):
            // rather split than merge + a review pair
            _ => reviews.push(ReviewRequest {
                other_id: c.id,
                score: sim.unwrap_or(0.0),
                reason: drift_reason(mention_key.as_deref(), c.type_key.as_deref(), sim),
            }),
        }
    }
    reviews.truncate(MAX_DRIFT_REVIEWS);
    // The drift path creates a new entity just the same, so run the containment check here too
    reviews.extend(containment_reviews(pool, kb_id, type_id, name, id, context).await?);
    Ok(Resolution {
        entity_id: id,
        created: true,
        reviews,
    })
}

async fn create_entity(
    pool: &PgPool,
    kb_id: Uuid,
    // None = the extractor's type is not in the ontology, or the KB has no classes at all (0009)
    type_id: Option<Uuid>,
    name: &str,
    context: Option<&[f32]>,
) -> AppResult<Uuid> {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO entities (id, kb_id, type_id, canonical_name, profile_embedding, profile_n)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(id)
    .bind(kb_id)
    .bind(type_id)
    .bind(name)
    .bind(context.map(|c| Vector::from(c.to_vec())))
    .bind(i32::from(context.is_some()))
    .execute(pool)
    .await?;
    Ok(id)
}

async fn touch_entity(pool: &PgPool, id: Uuid) -> AppResult<()> {
    sqlx::query("UPDATE entities SET updated_at = now() WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Incremental profile centroid: profile ← (profile·n + ctx) / (n+1).
/// On a dimension mismatch (the embedding model was swapped) the profile is reset to the new
/// vector.
async fn update_profile(pool: &PgPool, id: Uuid, n: i32, ctx: &[f32]) -> AppResult<()> {
    let existing: Option<(Option<Vector>,)> =
        sqlx::query_as("SELECT profile_embedding FROM entities WHERE id = $1")
            .bind(id)
            .fetch_optional(pool)
            .await?;
    let old = existing.and_then(|(v,)| v);
    let (new_vec, new_n) = match old {
        Some(p) if p.as_slice().len() == ctx.len() && n > 0 => {
            let nf = n as f32;
            let merged: Vec<f32> = p
                .as_slice()
                .iter()
                .zip(ctx)
                .map(|(a, b)| (a * nf + b) / (nf + 1.0))
                .collect();
            (merged, n + 1)
        }
        _ => (ctx.to_vec(), 1),
    };
    sqlx::query(
        "UPDATE entities SET profile_embedding = $2, profile_n = $3, updated_at = now()
         WHERE id = $1",
    )
    .bind(id)
    .bind(Vector::from(new_vec))
    .bind(new_n)
    .execute(pool)
    .await?;
    Ok(())
}

/// Display disambiguation for a same-name group: when the group holds ≥2 live entities, each
/// takes its most distinguishing fact (the object name of works_at/part_of/located_in/leads),
/// otherwise it falls back to the type label; if it is alone in the group, clear it.
pub async fn refresh_disambiguators(pool: &PgPool, kb_id: Uuid, name: &str) -> AppResult<()> {
    let group: Vec<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM entities
         WHERE kb_id = $1 AND merged_into IS NULL AND lower(canonical_name) = lower($2)",
    )
    .bind(kb_id)
    .bind(name)
    .fetch_all(pool)
    .await?;

    if group.len() < 2 {
        for (id,) in &group {
            sqlx::query("UPDATE entities SET disambiguator = NULL WHERE id = $1")
                .bind(id)
                .execute(pool)
                .await?;
        }
        return Ok(());
    }

    for (id,) in &group {
        let label: Option<(String,)> = sqlx::query_as(
            "SELECT o.canonical_name FROM facts f
             JOIN relation_types r ON r.id = f.predicate_id
             JOIN entities o ON o.id = f.object_id
             WHERE f.kb_id = $1 AND f.subject_id = $2
               AND f.invalidated_at IS NULL AND f.object_id IS NOT NULL
               AND r.key IN ('works_at', 'part_of', 'located_in', 'leads')
             ORDER BY (r.key = 'works_at') DESC, f.confidence DESC, f.recorded_at DESC
             LIMIT 1",
        )
        .bind(kb_id)
        .bind(id)
        .fetch_optional(pool)
        .await?;
        // With no related fact to find, fall back to the type label; **there may be no type
        // either** (0009), and then there is no suffix to write -- leave it NULL and let the
        // interface list the identical names side by side rather than inventing one
        let disambiguator: Option<String> = match label {
            Some((l,)) => Some(l),
            None => sqlx::query_as::<_, (String,)>(
                "SELECT t.label FROM entities e JOIN entity_types t ON t.id = e.type_id
                     WHERE e.id = $1",
            )
            .bind(id)
            .fetch_optional(pool)
            .await?
            .map(|(l,)| l),
        };
        sqlx::query("UPDATE entities SET disambiguator = $2 WHERE id = $1")
            .bind(id)
            .bind(disambiguator)
            .execute(pool)
            .await?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Review queue
// ---------------------------------------------------------------------------

/// Enqueue a grey-zone suspected-duplicate pair (idempotent for the same pending pair).
pub async fn create_review(
    pool: &PgPool,
    kb_id: Uuid,
    left_id: Uuid,
    right_id: Uuid,
    score: f32,
    reason: &str,
) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO resolution_reviews (id, kb_id, left_id, right_id, score, reason)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (kb_id, least(left_id, right_id), greatest(left_id, right_id))
             WHERE status = 'pending'
         DO NOTHING",
    )
    .bind(Uuid::now_v7())
    .bind(kb_id)
    .bind(left_id)
    .bind(right_id)
    .bind(score)
    .bind(reason)
    .execute(pool)
    .await?;
    Ok(())
}

#[derive(Debug, sqlx::FromRow)]
struct ReviewRow {
    id: Uuid,
    left_id: Uuid,
    right_id: Uuid,
    score: f32,
    reason: Option<String>,
    stage: String,
    created_at: DateTime<Utc>,
}

async fn review_side(pool: &PgPool, kb_id: Uuid, entity_id: Uuid) -> AppResult<ReviewSide> {
    #[derive(sqlx::FromRow)]
    struct SideRow {
        id: Uuid,
        name: String,
        type_label: Option<String>,
        color: String,
        disambiguator: Option<String>,
        degree: i64,
    }
    let row: SideRow = sqlx::query_as(
        "SELECT e.id, e.canonical_name AS name, t.label AS type_label,
                coalesce(t.color, '#94a3b8') AS color, e.disambiguator,
                (SELECT count(*) FROM facts f
                 WHERE (f.subject_id = e.id OR f.object_id = e.id)
                   AND f.invalidated_at IS NULL) AS degree
         -- LEFT JOIN: entities with no type determined still have to be reviewable (0009).
         -- An inner join makes the whole review item unfetchable, and drift reviews are
         -- exactly what happens to them most often
         FROM entities e LEFT JOIN entity_types t ON t.id = e.type_id
         WHERE e.kb_id = $1 AND e.id = $2",
    )
    .bind(kb_id)
    .bind(entity_id)
    .fetch_optional(pool)
    .await?
    .ok_or(AppError::NotFound)?;

    Ok(ReviewSide {
        id: row.id,
        name: row.name,
        type_label: row.type_label,
        color: row.color,
        disambiguator: row.disambiguator,
        degree: row.degree,
        top_facts: entity_fact_lines(pool, kb_id, entity_id, 4).await?,
    })
}

/// Fact summary lines for an entity: "works at → 星云科技 (2023-01 → now)", shared by the
/// adjudication prompt and the review UI.
pub async fn entity_fact_lines(
    pool: &PgPool,
    kb_id: Uuid,
    entity_id: Uuid,
    limit: i64,
) -> AppResult<Vec<String>> {
    #[derive(sqlx::FromRow)]
    struct Line {
        direction: String,
        predicate_label: String,
        other_name: Option<String>,
        valid_from: Option<DateTime<Utc>>,
        valid_to: Option<DateTime<Utc>>,
    }
    let rows: Vec<Line> = sqlx::query_as(
        "SELECT CASE WHEN f.subject_id = $2 THEN 'out' ELSE 'in' END AS direction,
                COALESCE(r.label, fact_surface_predicate(f.id)) AS predicate_label,
                o.canonical_name AS other_name,
                f.valid_from, f.valid_to
         FROM facts f
         LEFT JOIN relation_types r ON r.id = f.predicate_id
         LEFT JOIN entities o
           ON o.id = CASE WHEN f.subject_id = $2 THEN f.object_id ELSE f.subject_id END
         WHERE f.kb_id = $1 AND f.invalidated_at IS NULL
           AND (f.subject_id = $2 OR f.object_id = $2)
           AND COALESCE(r.label, fact_surface_predicate(f.id)) IS NOT NULL
         ORDER BY f.confidence DESC, f.recorded_at DESC
         LIMIT $3",
    )
    .bind(kb_id)
    .bind(entity_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|l| {
            let other = l.other_name.unwrap_or_else(|| "?".into());
            let core = if l.direction == "out" {
                format!("{} → {}", l.predicate_label, other)
            } else {
                format!("{} ← {}", l.predicate_label, other)
            };
            match (l.valid_from, l.valid_to) {
                (Some(f), Some(t)) => {
                    format!("{core} ({} → {})", f.format("%Y-%m"), t.format("%Y-%m"))
                }
                (Some(f), None) => format!("{core} ({} → now)", f.format("%Y-%m")),
                _ => core,
            }
        })
        .collect())
}

async fn assemble_reviews(
    pool: &PgPool,
    kb_id: Uuid,
    rows: Vec<ReviewRow>,
) -> AppResult<Vec<ReviewItem>> {
    let mut items = Vec::with_capacity(rows.len());
    for r in rows {
        items.push(ReviewItem {
            id: r.id,
            score: r.score,
            reason: r.reason,
            stage: r.stage,
            created_at: r.created_at,
            left: review_side(pool, kb_id, r.left_id).await?,
            right: review_side(pool, kb_id, r.right_id).await?,
        });
    }
    Ok(items)
}

/// Every pending review item (both those under LLM adjudication and those waiting on a human are
/// shown; a human can step in and settle it at any time).
pub async fn list_reviews(
    pool: &PgPool,
    kb_id: Uuid,
    limit: i64,
    offset: i64,
) -> AppResult<Vec<ReviewItem>> {
    let rows: Vec<ReviewRow> = sqlx::query_as(
        "SELECT id, left_id, right_id, score, reason, stage, created_at
         FROM resolution_reviews
         WHERE kb_id = $1 AND status = 'pending'
         ORDER BY created_at DESC LIMIT $2 OFFSET $3",
    )
    .bind(kb_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await?;
    assemble_reviews(pool, kb_id, rows).await
}

/// Review items waiting on LLM adjudication (consumed by the background adjudication task).
pub async fn pending_adjudications(
    pool: &PgPool,
    kb_id: Uuid,
    limit: i64,
) -> AppResult<Vec<ReviewItem>> {
    let rows: Vec<ReviewRow> = sqlx::query_as(
        "SELECT id, left_id, right_id, score, reason, stage, created_at
         FROM resolution_reviews
         WHERE kb_id = $1 AND status = 'pending' AND stage = 'adjudicating'
         ORDER BY created_at LIMIT $2",
    )
    .bind(kb_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    assemble_reviews(pool, kb_id, rows).await
}

/// The LLM is unsure / no model is configured → hand it to a human.
/// `reason` stores a **code**, optionally followed by `|detail` -- not a sentence for people to
/// read.
///
/// The interface language lives on the client (see docs/decisions/0004) and the server has no
/// locale to phrase anything with; English prose written into this column would stay on a Chinese
/// interface forever. Wording belongs to i18n, and only a stable code lives here.
pub async fn escalate_review(pool: &PgPool, review_id: Uuid, reason: &str) -> AppResult<()> {
    sqlx::query(
        "UPDATE resolution_reviews SET stage = 'human', reason = $2
         WHERE id = $1 AND status = 'pending'",
    )
    .bind(review_id)
    .bind(reason)
    .execute(pool)
    .await?;
    Ok(())
}

/// Settled automatically (high-confidence LLM): merged / kept. The merge action itself is carried
/// out by the caller first.
pub async fn close_review_auto(
    pool: &PgPool,
    review_id: Uuid,
    status: &str,
    reason: &str,
) -> AppResult<()> {
    sqlx::query(
        "UPDATE resolution_reviews SET status = $2, reason = $3, decided_at = now()
         WHERE id = $1 AND status = 'pending'",
    )
    .bind(review_id)
    .bind(status)
    .bind(reason)
    .execute(pool)
    .await?;
    Ok(())
}

/// Settled by a human. Merge direction: the side with the higher degree (more facts) becomes the
/// surviving target; on a tie the earlier-created one wins.
pub async fn decide_review(
    pool: &PgPool,
    kb_id: Uuid,
    review_id: Uuid,
    action: &str,
    user_id: Uuid,
) -> AppResult<()> {
    let row: Option<ReviewRow> = sqlx::query_as(
        "SELECT id, left_id, right_id, score, reason, stage, created_at
         FROM resolution_reviews WHERE id = $1 AND kb_id = $2 AND status = 'pending'",
    )
    .bind(review_id)
    .bind(kb_id)
    .fetch_optional(pool)
    .await?;
    let row = row.ok_or(AppError::NotFound)?;

    match action {
        "merge" => {
            let (target, source) = merge_direction(pool, row.left_id, row.right_id).await?;
            merge_entities(
                pool,
                kb_id,
                source,
                target,
                Some(user_id),
                "review decision",
            )
            .await?;
            sqlx::query(
                "UPDATE resolution_reviews SET status = 'merged', decided_at = now(), decided_by = $2
                 WHERE id = $1",
            )
            .bind(review_id)
            .bind(user_id)
            .execute(pool)
            .await?;
        }
        "keep" => {
            sqlx::query(
                "UPDATE resolution_reviews SET status = 'kept', decided_at = now(), decided_by = $2
                 WHERE id = $1",
            )
            .bind(review_id)
            .bind(user_id)
            .execute(pool)
            .await?;
        }
        _ => return Err(AppError::Validation("action must be merge or keep".into())),
    }
    Ok(())
}

/// Merge direction: returns (target that survives, source that gets merged away).
pub async fn merge_direction(pool: &PgPool, a: Uuid, b: Uuid) -> AppResult<(Uuid, Uuid)> {
    let (deg_a,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM facts WHERE (subject_id = $1 OR object_id = $1) AND invalidated_at IS NULL",
    )
    .bind(a)
    .fetch_one(pool)
    .await?;
    let (deg_b,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM facts WHERE (subject_id = $1 OR object_id = $1) AND invalidated_at IS NULL",
    )
    .bind(b)
    .fetch_one(pool)
    .await?;
    // uuidv7 is time-ordered: on a degree tie the earlier-created side survives
    Ok(if deg_a > deg_b || (deg_a == deg_b && a < b) {
        (a, b)
    } else {
        (b, a)
    })
}

// ---------------------------------------------------------------------------
// Merge / revert
// ---------------------------------------------------------------------------

#[derive(Debug, sqlx::FromRow)]
struct EntityFull {
    // None = not determined yet (0009)
    type_id: Option<Uuid>,
    canonical_name: String,
    aliases: Vec<String>,
    profile_embedding: Option<Vector>,
    profile_n: i32,
    merged_into: Option<Uuid>,
}

async fn entity_full(pool: &PgPool, kb_id: Uuid, id: Uuid) -> AppResult<EntityFull> {
    sqlx::query_as(
        "SELECT type_id, canonical_name, aliases, profile_embedding, profile_n, merged_into
         FROM entities WHERE kb_id = $1 AND id = $2",
    )
    .bind(kb_id)
    .bind(id)
    .fetch_optional(pool)
    .await?
    .ok_or(AppError::NotFound)
}

/// Merge source → target: facts get re-hung on target, facts pointing at each other and the
/// duplicates the merge creates are invalidated, source's name joins target's aliases, the profiles
/// are merged with weights, and source is marked merged_into. The whole thing is logged and can be
/// reverted.
pub async fn merge_entities(
    pool: &PgPool,
    kb_id: Uuid,
    source_id: Uuid,
    target_id: Uuid,
    merged_by: Option<Uuid>,
    reason: &str,
) -> AppResult<Uuid> {
    if source_id == target_id {
        return Err(AppError::invalid(
            "self_merge",
            "Cannot merge an entity into itself",
        ));
    }
    let source = entity_full(pool, kb_id, source_id).await?;
    let target = entity_full(pool, kb_id, target_id).await?;
    if source.merged_into.is_some() || target.merged_into.is_some() {
        return Err(AppError::Conflict("Entity already merged".into()));
    }

    let mut tx = pool.begin().await?;

    // Facts pointing at each other (they become self-loops after the merge) → invalidate
    let cross: Vec<(Uuid,)> = sqlx::query_as(
        "UPDATE facts SET invalidated_at = now()
         WHERE kb_id = $1 AND invalidated_at IS NULL
           AND ((subject_id = $2 AND object_id = $3) OR (subject_id = $3 AND object_id = $2))
         RETURNING id",
    )
    .bind(kb_id)
    .bind(source_id)
    .bind(target_id)
    .fetch_all(&mut *tx)
    .await?;
    let mut invalidated: Vec<Uuid> = cross.into_iter().map(|(id,)| id).collect();

    let moved_subject: Vec<Uuid> = sqlx::query_as::<_, (Uuid,)>(
        "UPDATE facts SET subject_id = $2 WHERE kb_id = $3 AND subject_id = $1 RETURNING id",
    )
    .bind(source_id)
    .bind(target_id)
    .bind(kb_id)
    .fetch_all(&mut *tx)
    .await?
    .into_iter()
    .map(|(id,)| id)
    .collect();
    let moved_object: Vec<Uuid> = sqlx::query_as::<_, (Uuid,)>(
        "UPDATE facts SET object_id = $2 WHERE kb_id = $3 AND object_id = $1 RETURNING id",
    )
    .bind(source_id)
    .bind(target_id)
    .bind(kb_id)
    .fetch_all(&mut *tx)
    .await?
    .into_iter()
    .map(|(id,)| id)
    .collect();

    // Live facts that duplicate on SPO+valid_from after the merge: keep the one with the earliest
    // recorded_at, invalidate the rest.
    //
    // **Both object columns have to be in the grouping.** Literal-value facts all have object_id
    // NULL, so grouping on that alone treats **every value** under the same subject and predicate
    // as one and the same assertion: after a single merge, beyond
    // (company, founding year, 2015) versus (company, registered capital, …), multiple values of
    // the same predicate leave only the earliest-recorded one alive, the rest vanish silently,
    // and there is no supersedes to trace them by.
    let dups: Vec<(Vec<Uuid>,)> = sqlx::query_as(
        "SELECT (array_agg(id ORDER BY recorded_at))[2:] FROM facts
         WHERE kb_id = $1 AND invalidated_at IS NULL
           AND (subject_id = $2 OR object_id = $2)
         GROUP BY subject_id, predicate_id, object_id, object_value, valid_from
         HAVING count(*) > 1",
    )
    .bind(kb_id)
    .bind(target_id)
    .fetch_all(&mut *tx)
    .await?;
    let dup_ids: Vec<Uuid> = dups.into_iter().flat_map(|(ids,)| ids).collect();
    if !dup_ids.is_empty() {
        sqlx::query("UPDATE facts SET invalidated_at = now() WHERE id = ANY($1)")
            .bind(&dup_ids)
            .execute(&mut *tx)
            .await?;
        invalidated.extend(dup_ids);
    }

    // Source's name and aliases join target's aliases (deduplicated, target's own name excluded)
    let mut aliases = target.aliases.clone();
    let taken: std::collections::HashSet<String> = std::iter::once(&target.canonical_name)
        .chain(aliases.iter())
        .map(|s| s.to_lowercase())
        .collect();
    for a in std::iter::once(&source.canonical_name).chain(source.aliases.iter()) {
        if !taken.contains(&a.to_lowercase()) && !aliases.iter().any(|x| x.eq_ignore_ascii_case(a))
        {
            aliases.push(a.clone());
        }
    }

    // Weighted merge of the profiles
    let (profile, profile_n) = match (&target.profile_embedding, &source.profile_embedding) {
        (Some(t), Some(s)) if t.as_slice().len() == s.as_slice().len() => {
            let (nt, ns) = (
                target.profile_n.max(1) as f32,
                source.profile_n.max(1) as f32,
            );
            let merged: Vec<f32> = t
                .as_slice()
                .iter()
                .zip(s.as_slice())
                .map(|(a, b)| (a * nt + b * ns) / (nt + ns))
                .collect();
            (
                Some(Vector::from(merged)),
                target.profile_n + source.profile_n,
            )
        }
        (Some(t), _) => (Some(t.clone()), target.profile_n),
        (None, Some(s)) => (Some(s.clone()), source.profile_n),
        (None, None) => (None, 0),
    };

    // Type reconciliation: **the side without a type yields**. If the survivor has nothing
    // determined yet while the side being merged away does, bring that type over; otherwise keep
    // the survivor's.
    //
    // This used to query the database for the id of the concept row and then compare it against
    // both sides. "Not determined yet" is `None` now (0009), a single `or` says all of it, and
    // that query is gone too
    let new_type_id = target.type_id.or(source.type_id);

    sqlx::query(
        "UPDATE entities SET aliases = $2, profile_embedding = $3, profile_n = $4,
                type_id = $5, updated_at = now() WHERE id = $1",
    )
    .bind(target_id)
    .bind(&aliases)
    .bind(&profile)
    .bind(profile_n)
    .bind(new_type_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE entities SET merged_into = $2, updated_at = now() WHERE id = $1")
        .bind(source_id)
        .bind(target_id)
        .execute(&mut *tx)
        .await?;

    // The remaining pending review items that involve source: **repoint them at the merge target,
    // do not close them**.
    //
    // This used to close all of them, and the stated reason was "if the doubt still stands a later
    // mention will raise it again". **That sentence was wrong**: containment recall runs once
    // **only when an entity is created**, and these entities existed long ago, will never be
    // created again, so there is no "later mention" to raise anything. Closing them closes them
    // forever.
    //
    // Measured: `Mr. Holmes` was paired with `Holmes` and enqueued (0.70), then `Holmes` was
    // merged into `Sherlock Holmes` and that pair was closed as superseded by merge -- while the
    // question of `Mr. Holmes` vs `Sherlock Holmes`, which still stands, was never asked again.
    //
    // First close the two kinds that really are obsolete: the ones that would become self-loops
    // after redirection, and the ones whose target pair is already in the queue.
    sqlx::query(
        "UPDATE resolution_reviews AS r
         SET status = 'kept', reason = 'superseded by merge', decided_at = now()
         WHERE r.kb_id = $1 AND r.status = 'pending'
           AND (r.left_id = $2 OR r.right_id = $2)
           AND NOT (least(r.left_id, r.right_id) = least($2, $3)
                    AND greatest(r.left_id, r.right_id) = greatest($2, $3))
           AND (
             (CASE WHEN r.left_id = $2 THEN r.right_id ELSE r.left_id END) = $3
             OR EXISTS (
               SELECT 1 FROM resolution_reviews d
               WHERE d.kb_id = $1 AND d.status = 'pending' AND d.id <> r.id
                 AND least(d.left_id, d.right_id)
                     = least($3, CASE WHEN r.left_id = $2 THEN r.right_id ELSE r.left_id END)
                 AND greatest(d.left_id, d.right_id)
                     = greatest($3, CASE WHEN r.left_id = $2 THEN r.right_id ELSE r.left_id END))
           )",
    )
    .bind(kb_id)
    .bind(source_id)
    .bind(target_id)
    .execute(&mut *tx)
    .await?;
    // The rest get repointed at the target, and the question stays hanging in the queue awaiting
    // adjudication.
    // The (source, target) pair itself is the exception -- it is exactly what this merge decided,
    // the caller marks it merged, and touching it here would make it show up wrongly in the
    // history as "kept apart"
    sqlx::query(
        "UPDATE resolution_reviews
         SET left_id = CASE WHEN left_id = $2 THEN $3 ELSE left_id END,
             right_id = CASE WHEN right_id = $2 THEN $3 ELSE right_id END,
             reason = reason || '|redirected'
         WHERE kb_id = $1 AND status = 'pending' AND (left_id = $2 OR right_id = $2)
           AND NOT (least(left_id, right_id) = least($2, $3)
                    AND greatest(left_id, right_id) = greatest($2, $3))",
    )
    .bind(kb_id)
    .bind(source_id)
    .bind(target_id)
    .execute(&mut *tx)
    .await?;

    let merge_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO entity_merges (id, kb_id, source_id, target_id,
                moved_subject_facts, moved_object_facts, invalidated_facts,
                target_profile_before, target_profile_n_before, target_type_before,
                merged_by, reason)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
    )
    .bind(merge_id)
    .bind(kb_id)
    .bind(source_id)
    .bind(target_id)
    .bind(&moved_subject)
    .bind(&moved_object)
    .bind(&invalidated)
    .bind(&target.profile_embedding)
    .bind(target.profile_n)
    .bind(target.type_id)
    .bind(merged_by)
    .bind(reason)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    // Temporal reconciliation after the move: a fact whose subject/object changed is equivalent to
    // a new observation landing -- only once two objects are folded into one can the uniqueness
    // invariant see for the first time that an old open interval collides with its successor (e.g.
    // "星尘" merged into "星尘项目", where the former lead's leads should be closed at the
    // new lead's start point).
    // The ids of the corrected rows go into the merge ledger: this merge is the only cause of those
    // corrections, so a revert has to undo them along with it.
    // Note: this step is outside the transaction, and failure is self-healing -- a leftover old
    // open row will be run into and closed by the ordinary insert-time reconciliation as soon as
    // the next related fact lands.
    let moved_all: Vec<Uuid> = moved_subject
        .iter()
        .chain(moved_object.iter())
        .copied()
        .collect();

    // **Merging is the third path that rewrites facts** (#196): swapping the subject or the object
    // can turn a legal fact into a signature violation -- a direction that was straightened out at
    // extraction time can be flipped right back by one merge. This one does not straighten and
    // does not modify, it only reports: run domain / range over the facts that moved and put the
    // violations into `axiom_violations` (kind = signature), the same queue as every other axiom
    // violation, with the same three ways out.
    // Placed after the transaction: the target entity's type was only just reconciled inside it,
    // and is visible once committed
    match crate::reasoning::signature_breaks(pool, kb_id, Some(&moved_all)).await {
        Ok(broken) if !broken.is_empty() => {
            if let Err(e) = crate::reasoning::record_signature_breaks(pool, kb_id, &broken).await {
                tracing::warn!(%kb_id, error = %e, "post-merge signature breaks failed to land");
            }
        }
        Ok(_) => {}
        Err(e) => tracing::warn!(%kb_id, error = %e, "post-merge signature check failed"),
    }
    let report = crate::temporal::reconcile_moved_facts(pool, kb_id, &moved_all).await?;
    if !report.corrected.is_empty() {
        sqlx::query("UPDATE entity_merges SET temporal_corrections = $2 WHERE id = $1")
            .bind(merge_id)
            .bind(&report.corrected)
            .execute(pool)
            .await?;
    }

    refresh_disambiguators(pool, kb_id, &source.canonical_name).await?;
    if !source
        .canonical_name
        .eq_ignore_ascii_case(&target.canonical_name)
    {
        refresh_disambiguators(pool, kb_id, &target.canonical_name).await?;
    }
    Ok(merge_id)
}

#[derive(Debug, sqlx::FromRow)]
struct MergeRow {
    source_id: Uuid,
    target_id: Uuid,
    moved_subject_facts: Vec<Uuid>,
    moved_object_facts: Vec<Uuid>,
    invalidated_facts: Vec<Uuid>,
    temporal_corrections: Vec<Uuid>,
    target_profile_before: Option<Vector>,
    target_profile_n_before: i32,
    target_type_before: Option<Uuid>,
    reverted_at: Option<DateTime<Utc>>,
}

/// Revert one merge exactly: facts move back the way they came, invalidations are undone, target's
/// profile and type are restored from the snapshot, and source comes back to life.
pub async fn revert_merge(pool: &PgPool, kb_id: Uuid, merge_id: Uuid) -> AppResult<()> {
    let m: MergeRow = sqlx::query_as(
        "SELECT source_id, target_id, moved_subject_facts, moved_object_facts,
                invalidated_facts, temporal_corrections, target_profile_before,
                target_profile_n_before, target_type_before, reverted_at
         FROM entity_merges WHERE id = $1 AND kb_id = $2",
    )
    .bind(merge_id)
    .bind(kb_id)
    .fetch_optional(pool)
    .await?
    .ok_or(AppError::NotFound)?;
    if m.reverted_at.is_some() {
        return Err(AppError::Conflict("Merge already reverted".into()));
    }

    let mut tx = pool.begin().await?;
    sqlx::query("UPDATE facts SET subject_id = $1 WHERE id = ANY($2)")
        .bind(m.source_id)
        .bind(&m.moved_subject_facts)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE facts SET object_id = $1 WHERE id = ANY($2)")
        .bind(m.source_id)
        .bind(&m.moved_object_facts)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE facts SET invalidated_at = NULL WHERE id = ANY($1)")
        .bind(&m.invalidated_facts)
        .execute(&mut *tx)
        .await?;

    // The temporal corrections the merge caused are undone with it: this merge is their only cause
    // (the collision the invariant could only see once two entities were folded into one), so once
    // the cause is undone the corrections follow -- first restore the superseded original rows,
    // then invalidate the correction rows.
    // Only live corrections are undone: a chain that was rewritten again afterwards by a genuine
    // new observation stays put (that part rests on independent grounds).
    sqlx::query(
        "UPDATE facts SET invalidated_at = NULL WHERE id IN (
             SELECT supersedes FROM facts
             WHERE id = ANY($1) AND invalidated_at IS NULL AND supersedes IS NOT NULL)",
    )
    .bind(&m.temporal_corrections)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE facts SET invalidated_at = now()
         WHERE id = ANY($1) AND invalidated_at IS NULL",
    )
    .bind(&m.temporal_corrections)
    .execute(&mut *tx)
    .await?;

    let source = entity_full(pool, kb_id, m.source_id).await?;
    // Roll target's aliases back: strip out the names that came from source
    sqlx::query(
        "UPDATE entities SET
            aliases = (SELECT coalesce(array_agg(a), '{}') FROM unnest(aliases) a
                       WHERE lower(a) <> ALL($2)),
            profile_embedding = $3, profile_n = $4,
            type_id = coalesce($5, type_id), updated_at = now()
         WHERE id = $1",
    )
    .bind(m.target_id)
    .bind(
        std::iter::once(&source.canonical_name)
            .chain(source.aliases.iter())
            .map(|s| s.to_lowercase())
            .collect::<Vec<_>>(),
    )
    .bind(&m.target_profile_before)
    .bind(m.target_profile_n_before)
    .bind(m.target_type_before)
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE entities SET merged_into = NULL, updated_at = now() WHERE id = $1")
        .bind(m.source_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE entity_merges SET reverted_at = now() WHERE id = $1")
        .bind(merge_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    refresh_disambiguators(pool, kb_id, &source.canonical_name).await?;
    Ok(())
}

/// The merge log (history section of the review page).
pub async fn list_merges(
    pool: &PgPool,
    kb_id: Uuid,
    limit: i64,
    offset: i64,
) -> AppResult<Vec<MergeLogView>> {
    let rows: Vec<MergeLogView> = sqlx::query_as(
        "SELECT m.id, s.canonical_name AS source_name, t.canonical_name AS target_name,
                u.display_name AS merged_by_name, m.reason, m.created_at, m.reverted_at
         FROM entity_merges m
         JOIN entities s ON s.id = m.source_id
         JOIN entities t ON t.id = m.target_id
         LEFT JOIN users u ON u.id = m.merged_by
         WHERE m.kb_id = $1
         ORDER BY m.created_at DESC LIMIT $2 OFFSET $3",
    )
    .bind(kb_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

// ---------------------------------------------------------------------------
// LLM adjudication cache
// ---------------------------------------------------------------------------

pub async fn get_verdict(
    pool: &PgPool,
    kb_id: Uuid,
    pair_key: &str,
) -> AppResult<Option<(Option<bool>, f32)>> {
    let row: Option<(Option<bool>, f32)> = sqlx::query_as(
        "SELECT same, confidence FROM resolution_verdicts WHERE kb_id = $1 AND pair_key = $2",
    )
    .bind(kb_id)
    .bind(pair_key)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

pub async fn put_verdict(
    pool: &PgPool,
    kb_id: Uuid,
    pair_key: &str,
    same: Option<bool>,
    confidence: f32,
    model: &str,
) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO resolution_verdicts (kb_id, pair_key, same, confidence, model)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (kb_id, pair_key)
         DO UPDATE SET same = $3, confidence = $4, model = $5, created_at = now()",
    )
    .bind(kb_id)
    .bind(pair_key)
    .bind(same)
    .bind(confidence)
    .bind(model)
    .execute(pool)
    .await?;
    Ok(())
}

/// Record a type the model proposed that the ontology has no room for.
///
/// **Only the first write counts**: the same entity gets mentioned by several documents, and the
/// first proposal is taken as its proposal; letting later ones overwrite would make "which
/// entities are waiting on a model class" jitter with whichever document came last.
/// Cleared by the retype flow once the corresponding class is adopted -- at that point it is no
/// longer a "proposal" but an accomplished fact.
pub async fn set_proposed_type(pool: &PgPool, entity_id: Uuid, proposed: &str) -> AppResult<()> {
    sqlx::query(
        "UPDATE entities SET proposed_type = left($2, 60)
         WHERE id = $1 AND proposed_type IS NULL",
    )
    .bind(entity_id)
    .bind(proposed)
    .execute(pool)
    .await?;
    Ok(())
}

/// Entity types waiting to be claimed: proposed by the model, absent from the ontology, and the
/// entity was demoted to concept as a result.
///
/// Symmetric with `graph::proposed_predicates` on the predicate side -- it is tied to concrete
/// entities, so adoption can say "will reclassify 43 of them" and actually go and do it, instead of
/// just creating an empty class.
pub async fn proposed_types(
    pool: &PgPool,
    kb_id: Uuid,
) -> AppResult<Vec<utopia_core::models::ProposedType>> {
    Ok(sqlx::query_as(
        "SELECT e.proposed_type AS form,
                count(*) AS entity_count,
                (array_agg(e.canonical_name ORDER BY e.created_at))[1] AS example
         FROM entities e
         WHERE e.kb_id = $1 AND e.merged_into IS NULL AND e.proposed_type IS NOT NULL
           -- Types the user has dismissed no longer show up among the candidates
           AND NOT EXISTS (SELECT 1 FROM ontology_misses m
                           WHERE m.kb_id = $1 AND m.kind = 'entity_type'
                             AND m.key = e.proposed_type AND m.dismissed_at IS NOT NULL)
         GROUP BY e.proposed_type
         ORDER BY entity_count DESC, form",
    )
    .bind(kb_id)
    .fetch_all(pool)
    .await?)
}

/// Retype the entities that proposed any of the types in `forms` onto `type_id`. Returns
/// (batch id, number changed).
///
/// Symmetric with `graph::adopt_proposed_predicates` on the predicate side: **creating the type
/// without touching the entities means the ontology grew and the graph got no better** -- the
/// entities that proposed model would go on hanging under concept.
///
/// Entities are mutable rows (the P0 PATCH modifies them in place), so this is a plain UPDATE, and
/// the undo relies on the ledger recording the type from before the change rather than on a
/// supersedes chain.
pub async fn adopt_proposed_types(
    pool: &PgPool,
    kb_id: Uuid,
    type_id: Uuid,
    forms: &[String],
    // None = the engine on its own (the mop-up claim after the ontology grows a new class has
    // nobody pressing anything)
    actor: Option<Uuid>,
) -> AppResult<(Uuid, u32)> {
    let batch_id = Uuid::now_v7();
    if forms.is_empty() {
        return Ok((batch_id, 0));
    }
    // Entities already on the target class do not count as changed and do not enter the ledger --
    // a revert should not push them back anywhere.
    //
    // **`IS DISTINCT FROM`, not `<>`**: since 0009 type_id can be NULL, and `NULL <> uuid`
    // evaluates to NULL rather than true, so that row gets silently filtered out -- and of all
    // things, almost everything carrying a proposed_type is an entity with no type determined yet,
    // so the whole claim feature would spin without a sound and do nothing
    let targets: Vec<(Uuid, Option<Uuid>, String)> = sqlx::query_as(
        "SELECT id, type_id, canonical_name FROM entities
         WHERE kb_id = $1 AND merged_into IS NULL
           -- Do not claim what a human has settled. **This line incidentally makes unadopt
           -- correct for free**: human rows never enter an adoption batch, so a revert never
           -- meets them and does not have to restore type_source separately
           AND type_source <> 'human'
           AND proposed_type = ANY($2) AND type_id IS DISTINCT FROM $3",
    )
    .bind(kb_id)
    .bind(forms)
    .bind(type_id)
    .fetch_all(pool)
    .await?;

    let mut names: HashSet<String> = HashSet::new();
    let mut moved = 0u32;
    for (entity_id, from_type, name) in targets {
        let mut tx = pool.begin().await?;
        sqlx::query(
            // actor present = a human clicked approve in the interface and endorsed this type
            // → protected.
            // absent = the mop-up claim after the ontology grew a new class; nobody pressed
            // anything
            "UPDATE entities
                SET type_id = $2, proposed_type = NULL, updated_at = now(),
                    type_source = CASE WHEN $3::uuid IS NULL THEN 'inferred' ELSE 'human' END
             WHERE id = $1",
        )
        .bind(entity_id)
        .bind(type_id)
        .bind(actor)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO entity_retypes
                (batch_id, kb_id, entity_id, from_type_id, to_type_id, actor_id)
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(batch_id)
        .bind(kb_id)
        .bind(entity_id)
        .bind(from_type)
        .bind(type_id)
        .bind(actor)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        names.insert(name);
        moved += 1;
    }
    // The disambiguator's fallback value is the type label, so a class change means recomputing it
    // (same as the P0 entity retype)
    for n in &names {
        refresh_disambiguators(pool, kb_id, n).await?;
    }
    Ok((batch_id, moved))
}

/// Undo one batch of entity retypes: put them back on their original type.
///
/// The type itself is not deleted -- the same reason as on the predicate side: entities have
/// pointed at it, and "it existed" is history.
/// `proposed_type` is restored along with it, otherwise those entities could never be claimed
/// again after the undo.
pub async fn unadopt_types(pool: &PgPool, kb_id: Uuid, batch_id: Uuid) -> AppResult<u32> {
    // from_type_id can be NULL: since 0009 the most common retype is exactly "from no class to a
    // class", and undoing it pushes the entity back to having no class. Decoding it as Uuid would
    // blow up right here
    let rows: Vec<(Uuid, Option<Uuid>, String)> = sqlx::query_as(
        "SELECT r.entity_id, r.from_type_id, t.key
         FROM entity_retypes r JOIN entity_types t ON t.id = r.to_type_id
         WHERE r.batch_id = $1 AND r.kb_id = $2 AND r.reverted_at IS NULL",
    )
    .bind(batch_id)
    .bind(kb_id)
    .fetch_all(pool)
    .await?;
    if rows.is_empty() {
        return Err(AppError::NotFound);
    }
    let mut names: Vec<String> = Vec::new();
    let mut tx = pool.begin().await?;
    let mut reverted = 0u32;
    for (entity_id, from_type, adopted_key) in &rows {
        let row: Option<(String,)> = sqlx::query_as(
            "UPDATE entities SET type_id = $2, proposed_type = $3, updated_at = now()
             WHERE id = $1 RETURNING canonical_name",
        )
        .bind(entity_id)
        .bind(from_type)
        .bind(adopted_key)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some((name,)) = row {
            names.push(name);
        }
        reverted += 1;
    }
    sqlx::query(
        "UPDATE entity_retypes SET reverted_at = now()
         WHERE batch_id = $1 AND kb_id = $2 AND reverted_at IS NULL",
    )
    .bind(batch_id)
    .bind(kb_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    for n in &names {
        refresh_disambiguators(pool, kb_id, n).await?;
    }
    Ok(reverted)
}

/// Normalize a proposed type into the shape of a key: lowercased, non-alphanumerics turned into
/// underscores, repeats collapsed.
/// "AI Model" → "ai_model", aligned with the character set validate_key allows.
fn normalize_type_key(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_us = true; // a leading underscore counts as a repeat too
    for c in s.trim().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            last_us = false;
        } else if !last_us {
            out.push('_');
            last_us = true;
        }
    }
    while out.ends_with('_') {
        out.pop();
    }
    out.chars().take(40).collect()
}

/// Claim the entities whose "type is already in the ontology while the entity still hangs under
/// concept".
///
/// `adopt_proposed_types` is only called **at the moment a class is created**, so the case where
/// the class is built first and the entity is extracted afterwards never gets carried over -- and
/// that is exactly the norm: the ontology is built in the first round and later documents keep
/// producing proposals. This sweep mops them up.
///
/// It only matches on **an exact name match after normalization**, never approximately -- a wrong
/// guess puts an entity into the wrong class, while the cost of "wait one more round" is close to
/// zero.
pub async fn sweep_proposed_types(
    pool: &PgPool,
    kb_id: Uuid,
    actor: Option<Uuid>,
) -> AppResult<Vec<(Uuid, u32)>> {
    let pending = proposed_types(pool, kb_id).await?;
    let existing: Vec<(Uuid, String)> =
        sqlx::query_as("SELECT id, key FROM entity_types WHERE kb_id = $1")
            .bind(kb_id)
            .fetch_all(pool)
            .await?;
    let mut out = Vec::new();
    for p in &pending {
        let norm = normalize_type_key(&p.form);
        let Some((type_id, _)) = existing.iter().find(|(_, k)| *k == norm) else {
            continue;
        };
        let (batch, n) =
            adopt_proposed_types(pool, kb_id, *type_id, std::slice::from_ref(&p.form), actor)
                .await?;
        if n > 0 {
            out.push((batch, n));
        }
    }
    Ok(out)
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_fullwidth_and_whitespace() {
        assert_eq!(normalize_name("　ＡＣＭＥ  Corp　"), "ACME Corp");
        assert_eq!(normalize_name("张三"), "张三");
        assert_eq!(normalize_name("张  三"), "张 三");
    }

    #[test]
    fn stem_generic_suffixes() {
        assert_eq!(name_stem("星尘项目").as_deref(), Some("星尘"));
        assert_eq!(name_stem("星辰科技公司").as_deref(), Some("星辰科技"));
        assert_eq!(name_stem("Phoenix Project").as_deref(), Some("phoenix"));
        assert_eq!(name_stem("Project Phoenix").as_deref(), Some("phoenix"));
        assert_eq!(name_stem("Acme Inc.").as_deref(), Some("acme"));
        assert_eq!(name_stem("星尘"), None);
        assert_eq!(name_stem("项目"), None); // stripping to nothing is not a stem
        assert_eq!(name_stem("项目团队"), None); // the stem is itself a generic word
        assert_eq!(name_stem("Team Project"), None);
    }

    #[test]
    fn recall_keys_bidirectional() {
        // A suffix-less mention can recall a suffixed entity (augmentation); the other direction
        // relies on the stem
        let k = recall_keys("星尘");
        assert!(k.contains(&"星尘".to_string()));
        assert!(k.contains(&"星尘项目".to_string()));
        let k = recall_keys("星尘项目");
        assert!(k.contains(&"星尘项目".to_string()));
        assert!(k.contains(&"星尘".to_string()));
        let k = recall_keys("Phoenix");
        assert!(k.contains(&"phoenix project".to_string()));
        assert!(k.contains(&"project phoenix".to_string()));
        let k = recall_keys("Project Phoenix");
        assert!(k.contains(&"phoenix".to_string()));
        assert!(recall_keys("张三").len() <= 10);
    }

    #[test]
    fn type_drift_classes() {
        // The side with no type determined yet → recall candidate. Before 0009 this arm compared
        // against the `concept` key; now it compares whether there is a class at all
        assert_eq!(
            classify_type_drift(None, Some("organization")),
            TypeDrift::Recall
        );
        assert_eq!(
            classify_type_drift(Some("project"), None),
            TypeDrift::Recall
        );
        // Neither side determined yet: recall as well, and let profile similarity do the talking
        assert_eq!(classify_type_drift(None, None), TypeDrift::Recall);
        // Confusable concrete types, pairwise → a review pair
        assert_eq!(
            classify_type_drift(Some("organization"), Some("project")),
            TypeDrift::Review
        );
        assert_eq!(
            classify_type_drift(Some("project"), Some("product")),
            TypeDrift::Review
        );
        assert_eq!(
            classify_type_drift(Some("product"), Some("organization")),
            TypeDrift::Review
        );
        // Hard-disjoint and unknown custom types → entirely apart
        assert_eq!(
            classify_type_drift(Some("person"), Some("organization")),
            TypeDrift::Disjoint
        );
        assert_eq!(
            classify_type_drift(Some("person"), Some("project")),
            TypeDrift::Disjoint
        );
        assert_eq!(
            classify_type_drift(Some("event"), Some("location")),
            TypeDrift::Disjoint
        );
        assert_eq!(
            classify_type_drift(Some("team"), Some("organization")),
            TypeDrift::Disjoint
        );
    }

    #[test]
    fn cosine_basics() {
        assert!((cosine(&[1.0, 0.0], &[1.0, 0.0]).unwrap() - 1.0).abs() < 1e-6);
        assert!(cosine(&[1.0, 0.0], &[0.0, 1.0]).unwrap().abs() < 1e-6);
        assert!(cosine(&[1.0], &[1.0, 2.0]).is_none());
        assert!(cosine(&[0.0, 0.0], &[1.0, 1.0]).is_none());
    }
}

/// One entity waiting for its type to be refined, together with all the material used to judge
/// what it is.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct TypeCandidateSubject {
    pub id: Uuid,
    pub canonical_name: String,
    pub aliases: Vec<String>,
    /// The class currently attached, which **may be absent** (0009: not determined means NULL).
    /// The "coarse" in the name is history -- extraction can now hand back a fine class directly,
    /// and can get it wrong (measured: `绍兴 → address`), so this may just as well be the one
    /// that needs **correcting**
    pub coarse_key: Option<String>,
    pub coarse_id: Option<Uuid>,
    /// The description of the current class. Adjudication has to judge "is the current class
    /// right", and the key alone is not enough -- keys from an imported ontology often cannot
    /// explain themselves (what is an `entry_point`?)
    pub coarse_description: Option<String>,
    /// The type name the model itself reported at extraction time; it only stays here when the
    /// vocabulary does **not** have it
    pub proposed_type: Option<String>,
    /// What the model says this thing is, present on every entity.
    ///
    /// This is the strongest signal, because it turns the task from "work out what this is" back
    /// into "which class in the ontology is called this" -- a short name against a short label.
    /// The measured failure was at the other end: matching a paragraph of Chinese profile text
    /// against schema.org's "A software application." -- the two shapes are simply not comparable
    pub specific_type: Option<String>,
    /// The predicates it takes part in, each with the other side's name (`produces 深蓝`,
    /// `leads by 张伟`). **Accumulated across documents**, which is exactly what extraction does
    /// not have in the moment
    pub roles: Vec<String>,
    /// Evidence quotes, a few sentences at most. Extraction looked at the same sentences, but in
    /// that pass it had to do entity recognition, relation judgement, temporal parsing and JSON
    /// formatting all at once, while fighting dozens of other entities for attention
    /// Evidence quotes, **only the ones where it is the subject**. A quote from the object
    /// position is about the subject: "上海浦东新区" is the object of located_in, yet the quote
    /// reads "星云科技（上海）有限公司是一家注册于上海浦东新区的股份有限公司", whose centre
    /// of gravity sits entirely on "股份有限公司" -- measured, that version of the retrieval
    /// gave back candidates that were all corporation-ish
    pub quotes: Vec<String>,
    pub fact_count: i64,
}

/// The entities worth sending off for type refinement: **the ones with no class yet**, or the ones
/// where the model reported a type from outside the vocabulary.
///
/// Entities that are already typed very specifically are left alone -- re-adjudicating one can only
/// go downhill, there is no upside.
pub async fn entities_for_type_resolution(
    pool: &PgPool,
    kb_id: Uuid,
    limit: i64,
) -> AppResult<Vec<TypeCandidateSubject>> {
    Ok(sqlx::query_as(
        "SELECT e.id, e.canonical_name, e.aliases, t.key AS coarse_key, t.id AS coarse_id,
                t.description AS coarse_description,
                e.proposed_type, e.specific_type,
                -- Write the other side's **name**, not its type key.
                -- Writing the key is self-destruction: the words organization / product / event
                -- then appear in the query, while the target of the retrieval is those very
                -- classes, so the candidates are just those few words from the profile.
                -- Measured, the profile of \"张伟\" was leads→organization …
                -- works_at→organization, and the candidates that came back were corporation,
                -- organization, business_entity_type -- the retrieval found the profile itself,
                -- not what this person is
                ARRAY(
                  SELECT DISTINCT COALESCE(rt.key, fact_surface_predicate(f.id))
                                  || CASE WHEN f.subject_id = e.id THEN ' ' ELSE ' by ' END
                                  || coalesce(oe.canonical_name, 'a value')
                  FROM facts f
                  LEFT JOIN relation_types rt ON rt.id = f.predicate_id
                  LEFT JOIN entities oe ON oe.id = CASE WHEN f.subject_id = e.id
                                                       THEN f.object_id ELSE f.subject_id END
                  WHERE f.kb_id = $1 AND f.invalidated_at IS NULL
                    AND (f.subject_id = e.id OR f.object_id = e.id)
                    AND COALESCE(rt.key, fact_surface_predicate(f.id)) IS NOT NULL
                  LIMIT 12
                ) AS roles,
                ARRAY(
                  SELECT DISTINCT ev.quote FROM fact_evidence ev
                  JOIN facts f2 ON f2.id = ev.fact_id
                  WHERE f2.kb_id = $1 AND f2.invalidated_at IS NULL
                    AND f2.subject_id = e.id
                    AND ev.quote IS NOT NULL
                  LIMIT 3
                ) AS quotes,
                (SELECT count(*) FROM facts f3
                 WHERE f3.kb_id = $1 AND f3.invalidated_at IS NULL
                   AND (f3.subject_id = e.id OR f3.object_id = e.id)) AS fact_count
         FROM entities e
         LEFT JOIN entity_types t ON t.id = e.type_id
         WHERE e.kb_id = $1 AND e.merged_into IS NULL
           -- **What a human has settled is not re-adjudicated.**
           --
           -- Without this line the third condition below drags them all back in: an entity a
           -- human set to organization qualifies as soon as organization has any subclass, so
           -- every round the engine re-adjudicates what a person already decided, and the
           -- ledger only shows who changed it after the fact.
           --
           -- \"Do not write to the database\" is not the same as \"do not speak\": if the engine
           -- thinks the human got it wrong, it should go through the Review queue rather than
           -- change it outright. Bottling it up and changing it outright are both distortions,
           -- just in opposite directions
           AND e.type_source <> 'human'
           -- The three kinds worth looking at: **those with no class yet**, those where the
           -- model reported a type from outside the vocabulary, **and those whose current class
           -- still has subclasses**. The third kind is the main force: now that extraction only
           -- recognizes base classes, a large batch of more specific classes hangs under
           -- organization, and that is the only place an imported ontology gets to prove its
           -- worth. Looking only at the first two misses it entirely
           AND (e.type_id IS NULL OR e.proposed_type IS NOT NULL OR e.specific_type IS NOT NULL
                OR EXISTS (SELECT 1 FROM entity_type_parents p WHERE p.parent_id = t.id))
         ORDER BY fact_count DESC, e.created_at
         LIMIT $2",
    )
    .bind(kb_id)
    .bind(limit)
    .fetch_all(pool)
    .await?)
}

/// Every descendant of a class (including itself). Refinement may only move to a descendant of the
/// coarse class.
///
/// **Recursive, not one level**: the ontology is a DAG,
/// `software_application ⊂ creative_work ⊂ thing`, and querying a single level shuts the vast
/// majority of the correct answers out.
pub async fn descendants_of(pool: &PgPool, kb_id: Uuid, root: Uuid) -> AppResult<Vec<Uuid>> {
    let rows: Vec<(Uuid,)> = sqlx::query_as(
        "WITH RECURSIVE d(id) AS (
             SELECT $2::uuid
             UNION
             SELECT p.child_id FROM entity_type_parents p JOIN d ON d.id = p.parent_id
         )
         SELECT d.id FROM d JOIN entity_types t ON t.id = d.id WHERE t.kb_id = $1",
    )
    .bind(kb_id)
    .bind(root)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

/// Record what the model says this entity itself is.
///
/// **A later write does not overwrite an earlier one** (it only writes when `IS NULL`), the same
/// rule as `set_proposed_type`: the same entity gets mentioned by several chunks, the first
/// statement usually comes from the most complete sentence, and the later chunks are often just a
/// passing mention.
pub async fn set_specific_type(pool: &PgPool, entity_id: Uuid, value: &str) -> AppResult<()> {
    sqlx::query(
        "UPDATE entities SET specific_type = left($2, 80)
         WHERE id = $1 AND specific_type IS NULL",
    )
    .bind(entity_id)
    .bind(value)
    .execute(pool)
    .await?;
    Ok(())
}

/// The entities most like this one that **already have a class**, together with their classes.
///
/// The second candidate source for type resolution. The first is searching class descriptions with
/// the profile, and measurement makes its weak spot very clear: a Chinese profile against
/// schema.org's "A software application." -- the two shapes are simply not comparable.
/// This one sidesteps that hurdle -- **name against name, in the same language** -- and it gets
/// more accurate the larger the knowledge base: "深蓝向量数据库" is like "Milvus", and
/// Milvus is already tagged software_application.
///
/// What gets compared is both sides' `profile_embedding` (the running average of the contexts they
/// appear in), **the same kind of vector against the same kind of vector**, not a text profile
/// against a context vector. The cost is zero -- entity resolution maintains this vector anyway.
///
/// Known weakness: for an entity that appears in only one document, the context vector is just
/// that chunk's vector, so entities from the same document become each other's nearest neighbours.
/// The caller has to be able to see `same_document` and must not take it as evidence of type.
pub async fn nearest_typed_entities(
    pool: &PgPool,
    kb_id: Uuid,
    entity_id: Uuid,
    limit: i64,
) -> AppResult<Vec<(String, Uuid, String, f64, bool)>> {
    Ok(sqlx::query_as(
        "WITH me AS (
             SELECT profile_embedding AS v FROM entities WHERE id = $2 AND kb_id = $1
         ),
         my_docs AS (
             SELECT DISTINCT ev.document_id FROM fact_evidence ev
             JOIN facts f ON f.id = ev.fact_id
             WHERE f.kb_id = $1 AND (f.subject_id = $2 OR f.object_id = $2)
         )
         SELECT e.canonical_name, t.id, t.key,
                (e.profile_embedding <=> (SELECT v FROM me))::float8 AS distance,
                EXISTS (SELECT 1 FROM fact_evidence ev2
                        JOIN facts f2 ON f2.id = ev2.fact_id
                        WHERE f2.kb_id = $1 AND (f2.subject_id = e.id OR f2.object_id = e.id)
                          AND ev2.document_id IN (SELECT document_id FROM my_docs))
                AS same_document
         -- The inner join is that gate: an entity with no type determined (type_id IS NULL) is
         -- not an answer, and using it as neighbour evidence only spreads \"not determined\" around
         FROM entities e
         JOIN entity_types t ON t.id = e.type_id
         WHERE e.kb_id = $1 AND e.merged_into IS NULL AND e.id <> $2
           AND e.profile_embedding IS NOT NULL
           AND (SELECT v FROM me) IS NOT NULL
         ORDER BY e.profile_embedding <=> (SELECT v FROM me)
         LIMIT $3",
    )
    .bind(kb_id)
    .bind(entity_id)
    .bind(limit)
    .fetch_all(pool)
    .await?)
}

/// Retype entities one at a time, writing into the same ledger. Returns (batch id, number
/// changed).
///
/// The only difference from [`adopt_proposed_types`] is how they get picked: that one claims a
/// batch by the **wording** in `proposed_type`, this one takes the caller's picks -- what type
/// resolution adjudicates is "this entity is that class", not "everything called this wording is
/// that class".
///
/// The ledger format is identical down to the character, so [`unadopt_types`] can revert it as-is.
pub async fn retype_entities(
    pool: &PgPool,
    kb_id: Uuid,
    picks: &[(Uuid, Uuid)],
    // None = the engine adjudicated on its own. The same convention as entity_merges.merged_by,
    // and entity history uses it to tell "changed by so-and-so" from "changed automatically with
    // high confidence"
    actor: Option<Uuid>,
) -> AppResult<(Uuid, u32)> {
    let batch_id = Uuid::now_v7();
    let mut moved = 0u32;
    let mut names: std::collections::HashSet<String> = std::collections::HashSet::new();
    // An entity is retyped only once per batch. The ledger's primary key is
    // (batch_id, entity_id), so the same id arriving twice collides -- and what the caller gets is
    // a 500 with not a single row of the batch landing. Blocking it here is more reliable than
    // chasing down every path that produces picks over there: a duplicate second entry was
    // meaningless anyway
    let mut seen: std::collections::HashSet<Uuid> = std::collections::HashSet::new();
    for (entity_id, type_id) in picks {
        if !seen.insert(*entity_id) {
            continue;
        }
        let mut tx = pool.begin().await?;
        // Entities already on the target class do not count as changed and do not enter the
        // ledger -- a revert should not push them back anywhere.
        //
        // **The old type has to be read out of the CTE**: `UPDATE … RETURNING` hands back the new
        // value, while what the ledger needs is the one from before the change. A plain
        // RETURNING type_id gives exactly what was just written, so a revert would "put" the
        // entity back where it already is -- the ledger looks packed while nothing can actually
        // be undone.
        // The conditions go into the CTE as well: this row still exists, has not been merged, and
        // really is about to change
        //
        // **`IS DISTINCT FROM`, not `<>`**: since 0009 the starting point is often NULL, and
        // `NULL <> uuid` is NULL rather than true -- the CTE would come out empty, the UPDATE
        // would touch no rows, and the single most important case, "give a class to an entity
        // that has none", would turn into a complete no-op
        let row: Option<(Option<Uuid>, String)> = sqlx::query_as(
            "WITH before AS (
                 SELECT id, type_id, canonical_name FROM entities
                 WHERE id = $1 AND kb_id = $3 AND merged_into IS NULL
                   AND type_id IS DISTINCT FROM $2
             )
             UPDATE entities e SET type_id = $2, updated_at = now(),
                    type_source = CASE WHEN $4::uuid IS NULL THEN 'inferred' ELSE 'human' END
             FROM before
             WHERE e.id = before.id
             RETURNING before.type_id, before.canonical_name",
        )
        .bind(entity_id)
        .bind(type_id)
        .bind(kb_id)
        .bind(actor)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((from_type, name)) = row else {
            tx.rollback().await?;
            continue;
        };
        sqlx::query(
            "INSERT INTO entity_retypes
                (batch_id, kb_id, entity_id, from_type_id, to_type_id, actor_id)
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(batch_id)
        .bind(kb_id)
        .bind(entity_id)
        .bind(from_type)
        .bind(type_id)
        .bind(actor)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        names.insert(name);
        moved += 1;
    }
    // The disambiguator's fallback value is the type label, so a class change means recomputing it
    for n in &names {
        refresh_disambiguators(pool, kb_id, n).await?;
    }
    Ok((batch_id, moved))
}

/// The "coarse class → fine class" pairings a human has approved.
///
/// The awaiting-human arm is triggered by "does this cross a classification axis", and what that
/// test usually measures is **whether the seed classes are wired up to the imported vocabulary at
/// all**, not risk: schema.org's Place gets its own key, the built-in location has zero subclasses,
/// and so every city has to be asked about again. A pairing is a matter between two classes and an
/// entity only happens to run into it -- approve it once and it should count from then on.
pub async fn approved_refinements(
    pool: &PgPool,
    kb_id: Uuid,
) -> AppResult<std::collections::HashSet<(Uuid, Uuid)>> {
    let rows: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT from_type_id, to_type_id FROM type_refinement_pairs WHERE kb_id = $1",
    )
    .bind(kb_id)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().collect())
}

/// Record an approved pairing. Approving the same one again is idempotent.
pub async fn approve_refinement(
    pool: &PgPool,
    kb_id: Uuid,
    from_type_id: Uuid,
    to_type_id: Uuid,
    by: Uuid,
) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO type_refinement_pairs (kb_id, from_type_id, to_type_id, approved_by)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (kb_id, from_type_id, to_type_id) DO NOTHING",
    )
    .bind(kb_id)
    .bind(from_type_id)
    .bind(to_type_id)
    .bind(by)
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod same_type_tests {
    use super::{classify_type_drift, TypeDrift};

    /// **The same type is the most common case, and it used to land on Disjoint.**
    ///
    /// This function was born to serve "type drift" (one name extracted under two types), where
    /// both sides being equal cannot happen; later `containment_reviews` borrowed it as a
    /// compatibility test, and there both sides being equal is the norm.
    /// The result was that `Sherlock Holmes` and `Holmes` never made it into the review queue as
    /// a pair.
    #[test]
    fn the_same_type_is_a_recall_candidate() {
        for k in ["person", "location", "organization", "product", "event"] {
            assert_eq!(
                classify_type_drift(Some(k), Some(k)),
                TypeDrift::Recall,
                "{k} against {k} must be recallable"
            );
        }
        // Custom types just the same -- what gets judged here is "are the two sides the same kind
        // of thing", not "is it on the allowlist"
        assert_eq!(
            classify_type_drift(Some("drug"), Some("drug")),
            TypeDrift::Recall
        );
        // Not one of the old cross-type conclusions changes
        assert_eq!(
            classify_type_drift(Some("person"), Some("organization")),
            TypeDrift::Disjoint
        );
    }
}
