//! Ontology editor repository: type/relation CRUD (with usage counts and delete guards) + miss
//! stats.

use pgvector::Vector;
use sqlx::PgPool;
use utopia_core::models::{
    EntityInstance, EntityTypeView, OntologyImportView, OntologyMiss, RelationAxioms,
    RelationTypeView, TypeCandidate,
};
use utopia_core::{AppError, AppResult};
use uuid::Uuid;

pub async fn entity_type_views(pool: &PgPool, kb_id: Uuid) -> AppResult<Vec<EntityTypeView>> {
    Ok(sqlx::query_as(
        "SELECT t.id, t.key, t.label, t.color, t.shape, t.builtin, t.description,
                ARRAY(SELECT p.parent_id FROM entity_type_parents p
                      WHERE p.child_id = t.id) AS parents,
                (SELECT p.parent_id FROM entity_type_parents p
                  WHERE p.child_id = t.id AND p.is_primary) AS primary_parent,
                ARRAY(SELECT d.b_id FROM entity_type_disjoint d
                      WHERE d.kb_id = t.kb_id AND d.a_id = t.id) AS disjoint,
                (SELECT count(*) FROM entities e
                 WHERE e.type_id = t.id AND e.merged_into IS NULL) AS usage
         FROM entity_types t WHERE t.kb_id = $1 ORDER BY lower(t.label)",
    )
    .bind(kb_id)
    .fetch_all(pool)
    .await?)
}

/// Entity instances under a class (name order, paginated). Returns (rows, total).
pub async fn entity_instances(
    pool: &PgPool,
    kb_id: Uuid,
    type_id: Uuid,
    limit: i64,
    offset: i64,
) -> AppResult<(Vec<EntityInstance>, i64)> {
    let rows: Vec<EntityInstance> = sqlx::query_as(
        "SELECT e.id, e.canonical_name AS name,
                (SELECT count(*) FROM facts f
                 WHERE (f.subject_id = e.id OR f.object_id = e.id)
                   AND f.invalidated_at IS NULL) AS fact_count
         FROM entities e
         WHERE e.kb_id = $1 AND e.type_id = $2 AND e.merged_into IS NULL
         ORDER BY lower(e.canonical_name)
         LIMIT $3 OFFSET $4",
    )
    .bind(kb_id)
    .bind(type_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await?;
    let (total,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM entities e
         WHERE e.kb_id = $1 AND e.type_id = $2 AND e.merged_into IS NULL",
    )
    .bind(kb_id)
    .bind(type_id)
    .fetch_one(pool)
    .await?;
    Ok((rows, total))
}

fn validate_shape(shape: &str) -> AppResult<()> {
    if !matches!(shape, "circle" | "square") {
        return Err(AppError::Validation(
            "shape must be circle or square".into(),
        ));
    }
    Ok(())
}

pub async fn relation_type_views(pool: &PgPool, kb_id: Uuid) -> AppResult<Vec<RelationTypeView>> {
    Ok(sqlx::query_as(
        "SELECT r.id, r.key, r.label, r.temporal, r.functional, r.inverse_functional,
                r.is_transitive, r.is_symmetric, r.is_asymmetric, r.is_irreflexive,
                r.inverse_of, r.sub_property_of,
                r.builtin, r.description,
                r.kind, r.datatype, r.unit,
                ARRAY(SELECT d.entity_type_id FROM relation_type_domains d
                      WHERE d.relation_type_id = r.id) AS domains,
                ARRAY(SELECT g.entity_type_id FROM relation_type_ranges g
                      WHERE g.relation_type_id = r.id) AS ranges,
                (SELECT count(*) FROM facts f
                 WHERE f.predicate_id = r.id AND f.invalidated_at IS NULL) AS usage
         FROM relation_types r WHERE r.kb_id = $1 ORDER BY lower(r.label)",
    )
    .bind(kb_id)
    .fetch_all(pool)
    .await?)
}

fn validate_key(key: &str) -> AppResult<()> {
    let ok = !key.is_empty()
        && key.len() <= 40
        && key
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if !ok {
        return Err(AppError::invalid(
            "bad_key",
            "Key must be lowercase snake_case (a-z, 0-9, _), max 40 chars",
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn create_entity_type(
    pool: &PgPool,
    kb_id: Uuid,
    key: &str,
    label: &str,
    color: &str,
    shape: &str,
    parents: &[Uuid],
    description: &str,
) -> AppResult<Uuid> {
    validate_key(key)?;
    validate_shape(shape)?;
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO entity_types (id, kb_id, key, label, color, shape, description)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(id)
    .bind(kb_id)
    .bind(key)
    .bind(label)
    .bind(color)
    .bind(shape)
    .bind(description)
    .execute(pool)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(db) if db.is_unique_violation() => {
            AppError::Conflict(format!("Type key '{key}' already exists"))
        }
        _ => AppError::Db(e),
    })?;
    set_parents(pool, kb_id, id, parents).await?;
    Ok(id)
}

#[allow(clippy::too_many_arguments)]
/// Edit an entity class.
///
/// **`color: None` = keep the current colour**, not a reset. This used to be a `&str`, and when the
/// caller passed nothing it hard-coded a default grey-blue, so any rename that came without a
/// colour wiped out the colour the user had picked.
pub async fn update_entity_type(
    pool: &PgPool,
    kb_id: Uuid,
    id: Uuid,
    label: &str,
    color: Option<&str>,
    shape: &str,
    parents: &[Uuid],
    description: &str,
) -> AppResult<()> {
    validate_shape(shape)?;
    let res = sqlx::query(
        "UPDATE entity_types SET label = $3, color = COALESCE($4, color), shape = $5,
                description = $6
         WHERE id = $2 AND kb_id = $1",
    )
    .bind(kb_id)
    .bind(id)
    .bind(label)
    .bind(color)
    .bind(shape)
    .bind(description)
    .execute(pool)
    .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    set_parents(pool, kb_id, id, parents).await?;
    Ok(())
}

/// Set `parents` as this class's complete set of parents; the first one becomes the primary parent
/// (the left column draws it under that branch).
///
/// **Check for cycles before writing**. In the single-parent era all we had to block was a
/// self-loop, since a single chain can never close into a cycle; in a DAG A→B→A is entirely
/// possible, and `type_matches_domain` walks up the parent chain, so a cycle is an infinite loop.
/// SQL cannot stop this -- the foreign key only blocks self-loops, anything longer has to be
/// checked by the application.
pub async fn set_parents(
    pool: &PgPool,
    kb_id: Uuid,
    child: Uuid,
    parents: &[Uuid],
) -> AppResult<()> {
    if parents.contains(&child) {
        return Err(AppError::invalid(
            "self_parent",
            "A class cannot be its own parent",
        ));
    }
    if !parents.is_empty() {
        // If child shows up among all the ancestors of the candidate parents, this edge closes a
        // cycle
        let (cycles,): (i64,) = sqlx::query_as(
            "WITH RECURSIVE up(id) AS (
                 SELECT unnest($2::uuid[])
                 UNION
                 SELECT p.parent_id FROM entity_type_parents p JOIN up ON p.child_id = up.id
             )
             SELECT count(*) FROM up WHERE id = $1",
        )
        .bind(child)
        .bind(parents)
        .fetch_one(pool)
        .await?;
        if cycles > 0 {
            return Err(AppError::invalid(
                "parent_cycle",
                "That parent is already a subclass of this one",
            ));
        }
    }
    sqlx::query("DELETE FROM entity_type_parents WHERE child_id = $1")
        .bind(child)
        .execute(pool)
        .await?;
    for (i, p) in parents.iter().enumerate() {
        sqlx::query(
            "INSERT INTO entity_type_parents (child_id, parent_id, is_primary)
             VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
        )
        .bind(child)
        .bind(p)
        // The first one is the primary parent: the interface says "drawn under the first one", so
        // no extra control
        .bind(i == 0)
        .execute(pool)
        .await?;
    }
    let _ = kb_id;
    Ok(())
}

pub async fn delete_entity_type(pool: &PgPool, kb_id: Uuid, id: Uuid) -> AppResult<()> {
    let (usage,): (i64,) = sqlx::query_as("SELECT count(*) FROM entities WHERE type_id = $1")
        .bind(id)
        .fetch_one(pool)
        .await?;
    if usage > 0 {
        return Err(AppError::Conflict(format!(
            "Cannot delete: {usage} entities use this type"
        )));
    }
    // Attributes whose only remaining domain is this class go with it: an attribute has to hang off
    // a class, and leaving an attribute with no domain leaves a dead row that will never show up
    // anywhere.
    // Ones still attached to other classes only lose one association row (the foreign key's CASCADE
    // takes care of that)
    sqlx::query(
        "DELETE FROM relation_types r
         WHERE r.kind = 'attribute' AND r.kb_id = $1
           AND EXISTS (SELECT 1 FROM relation_type_domains d
                       WHERE d.relation_type_id = r.id AND d.entity_type_id = $2)
           AND NOT EXISTS (SELECT 1 FROM relation_type_domains d
                           WHERE d.relation_type_id = r.id AND d.entity_type_id <> $2)",
    )
    .bind(kb_id)
    .bind(id)
    .execute(pool)
    .await?;
    let res = sqlx::query("DELETE FROM entity_types WHERE id = $2 AND kb_id = $1 AND NOT builtin")
        .bind(kb_id)
        .bind(id)
        .execute(pool)
        .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::Conflict(
            "Built-in types cannot be deleted".into(),
        ));
    }
    Ok(())
}

/// Attribute field validation: an attribute must have an owning class and a valid datatype; for a
/// relation all three are forced to be empty.
fn validate_attribute_fields(
    kind: &str,
    domains: &[Uuid],
    datatype: Option<&str>,
) -> AppResult<()> {
    match kind {
        "relation" => Ok(()),
        "attribute" => {
            if domains.is_empty() {
                return Err(AppError::invalid(
                    "attr_needs_class",
                    "An attribute needs a class (domain)",
                ));
            }
            if !matches!(datatype, Some("text" | "number" | "date" | "bool")) {
                return Err(AppError::Validation(
                    "datatype must be text / number / date / bool".into(),
                ));
            }
            Ok(())
        }
        _ => Err(AppError::Validation(
            "kind must be relation / attribute".into(),
        )),
    }
}

/// The two axioms that point at another relation must go through here before they hit the database.
///
/// **The most important one is same-knowledge-base.** The foreign key on the column is `REFERENCES
/// relation_types(id)`, which knows nothing about knowledge bases -- at the database level, a
/// relation in KB A can point at a relation in KB B. The RDF import path cannot get there by
/// construction (it looks IRIs up inside this KB), but the API takes a bare UUID: without a check
/// here, any UUID you can get hold of lets the reasoner read axioms across KBs. **We cannot rely on
/// the frontend only listing options from this KB**, that is interface politeness, not a boundary.
///
/// The other two: an attribute has no inverse (its object is a literal value, so there is nothing
/// to turn around), and a sub-property cannot be itself (the DB has a CHECK, but hitting it is a
/// 500, so we have to say it in plain words here).
/// And **being its own inverse is allowed** -- that is the same as symmetric, and R0 will suggest
/// `symmetric` as more direct; it is not an error.
async fn validate_property_links(
    pool: &PgPool,
    kb_id: Uuid,
    self_id: Option<Uuid>,
    kind: &str,
    ax: RelationAxioms,
) -> AppResult<()> {
    let links = [ax.inverse_of, ax.sub_property_of];
    if links.iter().all(|l| l.is_none()) {
        return Ok(());
    }
    if kind == "attribute" {
        return Err(AppError::invalid(
            "attr_has_no_link",
            "An attribute cannot have an inverse or a super-property",
        ));
    }
    if self_id.is_some() && ax.sub_property_of == self_id {
        return Err(AppError::invalid(
            "sub_property_self",
            "A relation cannot be its own super-property",
        ));
    }
    for target in links.into_iter().flatten() {
        let ok: Option<(String,)> =
            sqlx::query_as("SELECT kind FROM relation_types WHERE id = $1 AND kb_id = $2")
                .bind(target)
                .bind(kb_id)
                .fetch_optional(pool)
                .await?;
        match ok {
            // Do not distinguish "does not exist" from "is in another KB": being able to probe
            // which UUID exists elsewhere is itself a piece of information we should not be handing
            // out
            None => {
                return Err(AppError::invalid(
                    "unknown_relation",
                    "That relation is not in this knowledge base",
                ));
            }
            Some((k,)) if k != "relation" => {
                return Err(AppError::invalid(
                    "link_target_is_attr",
                    "An attribute cannot be an inverse or a super-property",
                ));
            }
            Some(_) => {}
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn create_relation_type(
    pool: &PgPool,
    kb_id: Uuid,
    key: &str,
    label: &str,
    temporal: &str,
    ax: RelationAxioms,
    description: &str,
    kind: &str,
    domains: &[Uuid],
    ranges: &[Uuid],
    datatype: Option<&str>,
    unit: Option<&str>,
) -> AppResult<Uuid> {
    validate_key(key)?;
    if !matches!(temporal, "state" | "event" | "eternal") {
        return Err(AppError::Validation(
            "temporal must be state / event / eternal".into(),
        ));
    }
    validate_attribute_fields(kind, domains, datatype)?;
    // The new row's id does not exist yet, so pointing at itself is impossible -- hence self_id =
    // None
    validate_property_links(pool, kb_id, None, kind, ax).await?;
    let is_attr = kind == "attribute";
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO relation_types (id, kb_id, key, label, temporal,
                                     functional, inverse_functional, description,
                                     kind, datatype, unit,
                                     is_transitive, is_symmetric,
                                     is_asymmetric, is_irreflexive,
                                     inverse_of, sub_property_of)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15,
                 $16, $17)",
    )
    .bind(id)
    .bind(kb_id)
    .bind(key)
    .bind(label)
    .bind(temporal)
    .bind(ax.functional)
    .bind(ax.inverse_functional)
    .bind(description)
    .bind(kind)
    .bind(is_attr.then_some(datatype).flatten())
    .bind(is_attr.then_some(unit).flatten())
    .bind(ax.transitive)
    .bind(ax.symmetric)
    .bind(ax.asymmetric)
    .bind(ax.irreflexive)
    // Attributes carry neither of these (the non-empty case was rejected above; this is the
    // backstop)
    .bind(if is_attr { None } else { ax.inverse_of })
    .bind(if is_attr { None } else { ax.sub_property_of })
    .execute(pool)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(db) if db.is_unique_violation() => {
            AppError::Conflict(format!("Relation key '{key}' already exists"))
        }
        _ => AppError::Db(e),
    })?;
    // An attribute writes no range: its value space is a literal type, which lives in datatype
    set_domains_ranges(pool, id, domains, if is_attr { &[] } else { ranges }).await?;
    Ok(id)
}

/// Overwriting write of domain / range. **Delete first, then insert**, so it serves both creation
/// and re-import, and never leaves residue from the previous round.
async fn set_domains_ranges(
    pool: &PgPool,
    relation_type_id: Uuid,
    domains: &[Uuid],
    ranges: &[Uuid],
) -> AppResult<()> {
    for (table, ids) in [
        ("relation_type_domains", domains),
        ("relation_type_ranges", ranges),
    ] {
        sqlx::query(&format!("DELETE FROM {table} WHERE relation_type_id = $1"))
            .bind(relation_type_id)
            .execute(pool)
            .await?;
        if ids.is_empty() {
            continue;
        }
        // unnest inserts them all at once, saving a round trip per row
        sqlx::query(&format!(
            "INSERT INTO {table} (relation_type_id, entity_type_id)
             SELECT $1, x FROM unnest($2::uuid[]) AS x
             ON CONFLICT DO NOTHING"
        ))
        .bind(relation_type_id)
        .bind(ids)
        .execute(pool)
        .await?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn update_relation_type(
    pool: &PgPool,
    kb_id: Uuid,
    id: Uuid,
    label: &str,
    temporal: &str,
    ax: RelationAxioms,
    description: &str,
    datatype: Option<&str>,
    unit: Option<&str>,
    // None = leave alone. Callers that do not care about domain (the attribute form) pass None,
    // otherwise one unrelated rename would clear the attribute's domain
    domains: Option<&[Uuid]>,
    ranges: Option<&[Uuid]>,
) -> AppResult<()> {
    if !matches!(temporal, "state" | "event" | "eternal") {
        return Err(AppError::Validation(
            "temporal must be state / event / eternal".into(),
        ));
    }
    if datatype.is_some() && !matches!(datatype, Some("text" | "number" | "date" | "bool")) {
        return Err(AppError::Validation(
            "datatype must be text / number / date / bool".into(),
        ));
    }
    // The two that point at another relation have to be checked against the DB first: is the target
    // in this KB, and is it a relation.
    // Only check when they are actually set -- clearing them (both None) leaves no target to verify
    if ax.inverse_of.is_some() || ax.sub_property_of.is_some() {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT kind FROM relation_types WHERE id = $1 AND kb_id = $2")
                .bind(id)
                .bind(kb_id)
                .fetch_optional(pool)
                .await?;
        let (kind,) = row.ok_or(AppError::NotFound)?;
        validate_property_links(pool, kb_id, Some(id), &kind, ax).await?;
    }
    // kind is immutable (changing it would scramble the meaning of existing facts). domain/range
    // can change -- they are a signature, not an identity, and "this attribute also applies to
    // contractors" is a legitimate edit.
    // datatype/unit only take effect on attribute rows, and an absent datatype keeps the old value
    //
    // The two links are **overwritten together with the six axiom flags**: absent = cleared, not
    // "leave alone". Same rule as the `is_transitive` group -- they are one set of declarations
    // submitted together from a single form, and overwriting half while keeping half is the
    // semantics that really does get you into trouble
    let res = sqlx::query(
        "UPDATE relation_types
            SET label = $3, temporal = $4,
                functional = $5, inverse_functional = $6, description = $7,
                datatype = CASE WHEN kind = 'attribute' AND $8 IS NOT NULL THEN $8 ELSE datatype END,
                unit = CASE WHEN kind = 'attribute' THEN $9 ELSE unit END,
                is_transitive = $10, is_symmetric = $11,
                is_asymmetric = $12, is_irreflexive = $13,
                inverse_of = $14, sub_property_of = $15
         WHERE id = $2 AND kb_id = $1",
    )
    .bind(kb_id)
    .bind(id)
    .bind(label)
    .bind(temporal)
    .bind(ax.functional)
    .bind(ax.inverse_functional)
    .bind(description)
    .bind(datatype)
    .bind(unit)
    .bind(ax.transitive)
    .bind(ax.symmetric)
    .bind(ax.asymmetric)
    .bind(ax.irreflexive)
    .bind(ax.inverse_of)
    .bind(ax.sub_property_of)
    .execute(pool)
    .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    if domains.is_some() || ranges.is_some() {
        let (kind,): (String,) = sqlx::query_as("SELECT kind FROM relation_types WHERE id = $1")
            .bind(id)
            .fetch_one(pool)
            .await?;
        let next_domains = domains.unwrap_or(&[]);
        if kind == "attribute" && next_domains.is_empty() {
            return Err(AppError::invalid(
                "attr_needs_class",
                "An attribute needs a class (domain)",
            ));
        }
        // An attribute has no range: its value space is a literal type, which lives in datatype
        let next_ranges: &[Uuid] = if kind == "attribute" {
            &[]
        } else {
            ranges.unwrap_or(&[])
        };
        set_domains_ranges(pool, id, next_domains, next_ranges).await?;
    }
    Ok(())
}

pub async fn delete_relation_type(pool: &PgPool, kb_id: Uuid, id: Uuid) -> AppResult<()> {
    let (usage,): (i64,) = sqlx::query_as("SELECT count(*) FROM facts WHERE predicate_id = $1")
        .bind(id)
        .fetch_one(pool)
        .await?;
    if usage > 0 {
        return Err(AppError::Conflict(format!(
            "Cannot delete: {usage} facts use this relation"
        )));
    }
    let res =
        sqlx::query("DELETE FROM relation_types WHERE id = $2 AND kb_id = $1 AND NOT builtin")
            .bind(kb_id)
            .bind(id)
            .execute(pool)
            .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::Conflict(
            "Built-in relations cannot be deleted".into(),
        ));
    }
    Ok(())
}

/* ---- Miss statistics ---- */

pub async fn record_miss(
    pool: &PgPool,
    kb_id: Uuid,
    kind: &str,
    key: &str,
    example: Option<&str>,
) -> AppResult<()> {
    sqlx::query(
        // **Dismissed ones keep counting up all the same.**
        //
        // This used to carry `WHERE dismissed_at IS NULL`, on the grounds that "otherwise the count
        // pushes a 'no thanks' back up into a pending signal". That reasoning was about
        // **presentation**, but the mechanism used was **stopping the count** -- the two got tied
        // together, and the price was that one click turned into permanent blindness: a phrase that
        // appeared once in the first document gets dismissed, the next twenty documents all use it,
        // the count is still stuck at 1, nobody knows the original judgement no longer holds, and
        // that batch of facts never gets a predicate.
        //
        // The user judged the **evidence visible at the time**, not all of time. So we keep
        // counting and leave suppression to the read side: `list_misses` still returns only the
        // non-dismissed ones, so proposals and automatic ontology extension are unchanged; the
        // dismissed ones, with their updated counts, go through `list_dismissed_misses` and get
        // their own single spot on the panel, where someone who sees it has climbed to 40 can undo
        // the dismissal themselves
        "INSERT INTO ontology_misses (kb_id, kind, key, example)
         VALUES ($1, $2, left($3, 80), left($4, 200))
         ON CONFLICT (kb_id, kind, key)
         DO UPDATE SET count = ontology_misses.count + 1,
                       example = COALESCE(EXCLUDED.example, ontology_misses.example),
                       updated_at = now()",
    )
    .bind(kb_id)
    .bind(kind)
    .bind(key)
    .bind(example)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn list_misses(pool: &PgPool, kb_id: Uuid) -> AppResult<Vec<OntologyMiss>> {
    Ok(sqlx::query_as(
        "SELECT kind, key, example, count FROM ontology_misses
         WHERE kb_id = $1 AND dismissed_at IS NULL
         ORDER BY count DESC, updated_at DESC LIMIT 50",
    )
    .bind(kb_id)
    .fetch_all(pool)
    .await?)
}

/// Phrases that have been dismissed, together with **the count they keep accumulating afterwards**.
///
/// The reason this exists is that dismissing used to be a one-way door: once clicked, the phrase
/// was neither shown nor counted again, so the moment the basis for the judgement -- "it only
/// appeared once at the time" -- went stale, nobody could see it.
/// This list is the window in that door -- suppression works as before, but you can see what is
/// being suppressed and how heavy it has become by now.
pub async fn list_dismissed_misses(pool: &PgPool, kb_id: Uuid) -> AppResult<Vec<OntologyMiss>> {
    Ok(sqlx::query_as(
        "SELECT kind, key, example, count FROM ontology_misses
         WHERE kb_id = $1 AND dismissed_at IS NOT NULL
         ORDER BY count DESC, updated_at DESC LIMIT 50",
    )
    .bind(kb_id)
    .fetch_all(pool)
    .await?)
}

/// Undo a dismissal: this phrase re-enters proposals and automatic ontology extension.
pub async fn restore_miss(pool: &PgPool, kb_id: Uuid, kind: &str, key: &str) -> AppResult<()> {
    sqlx::query(
        "UPDATE ontology_misses SET dismissed_at = NULL, updated_at = now()
         WHERE kb_id = $1 AND kind = $2 AND key = $3 AND dismissed_at IS NOT NULL",
    )
    .bind(kb_id)
    .bind(kind)
    .bind(key)
    .execute(pool)
    .await?;
    Ok(())
}

/// The user said "not this one". **Mark, do not delete** -- if we deleted it, the next extraction
/// run would hit the same word and insert it right back, and the user's rejection would not survive
/// a single round of extraction. The automatic extension path steers around it on the same basis.
///
/// Reversible (see [`restore_miss`]), and after the undo the count is continuous -- it kept being
/// recorded all through the dismissal.
pub async fn dismiss_miss(pool: &PgPool, kb_id: Uuid, kind: &str, key: &str) -> AppResult<()> {
    sqlx::query(
        "UPDATE ontology_misses SET dismissed_at = now()
         WHERE kb_id = $1 AND kind = $2 AND key = $3 AND dismissed_at IS NULL",
    )
    .bind(kb_id)
    .bind(kind)
    .bind(key)
    .execute(pool)
    .await?;
    Ok(())
}

/// The ontology now covers this phrase (called on adoption): unlike "the user rejected it", this
/// one really can be cleared -- next extraction it will hit the ontology and no longer be a miss.
pub async fn clear_miss(pool: &PgPool, kb_id: Uuid, kind: &str, key: &str) -> AppResult<()> {
    sqlx::query("DELETE FROM ontology_misses WHERE kb_id = $1 AND kind = $2 AND key = $3")
        .bind(kb_id)
        .bind(kind)
        .bind(key)
        .execute(pool)
        .await?;
    Ok(())
}

/* ---- OWL import ---- */

/// Create a class with an IRI. The IRI is the global identity; re-import matches on it (see 0001
/// P2).
pub async fn create_entity_type_with_iri(
    pool: &PgPool,
    kb_id: Uuid,
    key: &str,
    label: &str,
    description: &str,
    iri: &str,
) -> AppResult<Uuid> {
    validate_key(key)?;
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO entity_types (id, kb_id, key, label, color, shape, description, iri)
         VALUES ($1, $2, $3, $4, $7, $8, $5, $6)",
    )
    .bind(id)
    .bind(kb_id)
    .bind(key)
    .bind(label)
    .bind(description)
    .bind(iri)
    // Colour by key rather than one grey-blue for every class -- import a big ontology and you'll
    // see why
    .bind(crate::palette::color_for_key(key))
    // Shape says where it came from: this path carries an IRI, so it was declared by a vocabulary
    .bind(crate::palette::shape_for(iri))
    .execute(pool)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(db) if db.is_unique_violation() => {
            AppError::Conflict(format!("Type key '{key}' already exists"))
        }
        _ => AppError::Db(e),
    })?;
    Ok(id)
}

/// On re-import, update label and description by IRI. **key does not move** -- it may already be
/// referenced by extracted entities and by the prompts, so changing it would break the references
/// of existing data; upstream changing a label is normal, and the IRI is what the identity is.
pub async fn update_type_from_import(
    pool: &PgPool,
    kb_id: Uuid,
    iri: &str,
    label: &str,
    description: &str,
) -> AppResult<Option<Uuid>> {
    let row: Option<(Uuid,)> = sqlx::query_as(
        "UPDATE entity_types
         SET label = $3,
             -- An empty description does not overwrite an existing one: upstream may not have
             -- written rdfs:comment, while locally someone may already have tuned it to their own
             -- corpus, and that tuning is worth more than a blank
             description = CASE WHEN $4 = '' THEN description ELSE $4 END
         WHERE kb_id = $1 AND iri = $2 RETURNING id",
    )
    .bind(kb_id)
    .bind(iri)
    .bind(label)
    .bind(description)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(id,)| id))
}

/// Set parent classes. Self-loops and the already-a-parent case are silently skipped.
/// Create an attribute from an import (`kind='attribute'`), with an IRI.
///
/// The only differences from [`create_relation_type`] are the extra `iri` and how a key conflict is
/// handled: an import takes the IRI as the identity, so a key collision is "two different things
/// fighting over one short label", which the caller reports and skips during the planning phase; by
/// the time we get here there should be no collision left -- so on conflict we return None instead
/// of overwriting, and let the caller count it as "skipped".
///
/// `temporal` is fixed at `state`: an attribute is a value that changes over time (salary,
/// headcount), and having a new value close out the old one is exactly what we want. OWL has no
/// corresponding concept, and guessing event or eternal would both be worse.
#[allow(clippy::too_many_arguments)]
pub async fn create_attribute_with_iri(
    pool: &PgPool,
    kb_id: Uuid,
    key: &str,
    label: &str,
    description: &str,
    iri: &str,
    domains: &[Uuid],
    datatype: &str,
) -> AppResult<Option<Uuid>> {
    validate_key(key)?;
    let id = Uuid::now_v7();
    let row: Option<(Uuid,)> = sqlx::query_as(
        "INSERT INTO relation_types
             (id, kb_id, key, label, temporal, functional, inverse_functional,
              description, kind, datatype, iri)
         VALUES ($1, $2, $3, $4, 'state', FALSE, FALSE, $5, 'attribute', $6, $7)
         ON CONFLICT (kb_id, key) DO NOTHING
         RETURNING id",
    )
    .bind(id)
    .bind(kb_id)
    .bind(key)
    .bind(label)
    .bind(description)
    .bind(datatype)
    .bind(iri)
    .fetch_optional(pool)
    .await?;
    let Some((new_id,)) = row else {
        return Ok(None);
    };
    set_domains_ranges(pool, new_id, domains, &[]).await?;
    Ok(Some(new_id))
}

/// Create a relation from an import (`kind='relation'`), with an IRI.
///
/// `temporal` is fixed at `state`: OWL has no corresponding concept, and state (which has an
/// interval) is the only one of the three that loses no information -- event would squash the
/// interval into a point in time, and eternal would claim it never changes.
///
/// **`functional` / `inverse_functional` are written as the vocabulary has them**. They drive the
/// temporal engine's automatic closing of old facts, and guessing wrong manufactures false
/// conflicts in bulk (59 of them, that time with `part_of`). The preview already lists the
/// relations declared functional separately for a human to look over, so we no longer take it upon
/// ourselves to force them to false here.
#[allow(clippy::too_many_arguments)]
pub async fn create_relation_with_iri(
    pool: &PgPool,
    kb_id: Uuid,
    key: &str,
    label: &str,
    description: &str,
    iri: &str,
    functional: bool,
    inverse_functional: bool,
    domains: &[Uuid],
    ranges: &[Uuid],
) -> AppResult<Option<Uuid>> {
    validate_key(key)?;
    let id = Uuid::now_v7();
    let row: Option<(Uuid,)> = sqlx::query_as(
        "INSERT INTO relation_types
             (id, kb_id, key, label, temporal, functional, inverse_functional,
              description, kind, iri)
         VALUES ($1, $2, $3, $4, 'state', $5, $6, $7, 'relation', $8)
         ON CONFLICT (kb_id, key) DO NOTHING
         RETURNING id",
    )
    .bind(id)
    .bind(kb_id)
    .bind(key)
    .bind(label)
    .bind(functional)
    .bind(inverse_functional)
    .bind(description)
    .bind(iri)
    .fetch_optional(pool)
    .await?;
    let Some((new_id,)) = row else {
        return Ok(None);
    };
    set_domains_ranges(pool, new_id, domains, ranges).await?;
    Ok(Some(new_id))
}

/// On re-import, update a relation that has already been identified by IRI.
///
/// **key does not move** (it may already be referenced by facts), **an empty description does not
/// overwrite** (what a human wrote is more accurate than what upstream has), **domain/range are
/// rewritten wholesale** -- they are structure declared by upstream, not wording a human tuned.
pub async fn update_relation_from_import(
    pool: &PgPool,
    kb_id: Uuid,
    iri: &str,
    label: &str,
    description: &str,
    domains: &[Uuid],
    ranges: &[Uuid],
) -> AppResult<bool> {
    let row: Option<(Uuid,)> = sqlx::query_as(
        "UPDATE relation_types
            SET label = $3,
                description = CASE WHEN $4 = '' THEN description ELSE $4 END
          WHERE kb_id = $1 AND iri = $2
          RETURNING id",
    )
    .bind(kb_id)
    .bind(iri)
    .bind(label)
    .bind(description)
    .fetch_optional(pool)
    .await?;
    let Some((id,)) = row else {
        return Ok(false);
    };
    set_domains_ranges(pool, id, domains, ranges).await?;
    Ok(true)
}

/// Record an import. The original is already stored content-addressed in the blob store; this is
/// only the bookkeeping.
#[allow(clippy::too_many_arguments)]
pub async fn record_import(
    pool: &PgPool,
    kb_id: Uuid,
    sha256: &str,
    filename: &str,
    format: &str,
    byte_size: i64,
    summary: &serde_json::Value,
    actor: Uuid,
) -> AppResult<Uuid> {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO ontology_imports
            (id, kb_id, sha256, filename, format, byte_size, summary, imported_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(id)
    .bind(kb_id)
    .bind(sha256)
    .bind(filename)
    .bind(format)
    .bind(byte_size)
    .bind(summary)
    .bind(actor)
    .execute(pool)
    .await?;
    Ok(id)
}

/// One KB's import history (with the importer's display name; NULL once the account is deleted).
pub async fn list_imports(pool: &PgPool, kb_id: Uuid) -> AppResult<Vec<OntologyImportView>> {
    Ok(sqlx::query_as(
        "SELECT i.id, i.filename, i.format, i.byte_size, i.summary, i.imported_at,
                u.display_name AS imported_by_name
         FROM ontology_imports i
         LEFT JOIN users u ON u.id = i.imported_by
         WHERE i.kb_id = $1 ORDER BY i.imported_at DESC LIMIT 50",
    )
    .bind(kb_id)
    .fetch_all(pool)
    .await?)
}

/// An ontology row waiting to be embedded. `text` is the string to be sent off for embedding, and
/// `kind` decides which table it is written back to.
#[derive(Debug, Clone)]
pub struct TypeToEmbed {
    pub id: Uuid,
    pub kind: TypeKind,
    pub text: String,
    /// Which group of columns to write into. A class has two vectors: the full one (label +
    /// description) and the label-only one, serving long-profile queries and short-phrase queries
    /// respectively (see `entity_types.label_embedding`)
    pub field: EmbedField,
}

/// The two vectors for one row. **Short queries compare against Label, long profiles against Full**
/// -- queries come in two shapes, so the documents have to come in two shapes as well, otherwise
/// short queries get taken over by tautological classes (the `Map\nA map.` kind).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbedField {
    Full,
    Label,
}

/// Which table an ontology row belongs to. Classes go into `entity_types`; relations and attributes
/// share `relation_types`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeKind {
    Entity,
    Relation,
}

/// The string used for embedding: label first, description after.
///
/// **The key does not go in.** The key is the token the model reads and writes (`founding_date`);
/// the description is where this type's meaning lives. Mix the key in and retrieval gets pulled off
/// course by "these two keys look alike" -- and `position` (place in a list) versus `position` (a
/// job title) are precisely two things that look exactly alike.
fn embed_text(label: &str, description: &str) -> String {
    let d = description.trim();
    if d.is_empty() {
        label.trim().to_string()
    } else {
        format!("{}\n{}", label.trim(), d)
    }
}

/// Which ontology rows have stale vectors (never embedded, description changed, or the embedding
/// model changed).
///
/// The test is **comparing the text that was embedded and the model name**, not looking at a
/// timestamp: if the description changed or the model changed, the timestamp shows nothing. It also
/// means we do not have to hang a hook off every write site that edits a description -- miss one
/// and it rots silently.
///
/// `only` narrows it to half the work. Type resolution uses classes alone, and making it wait for
/// 1633 relations to finish embedding is six minutes wasted for nothing; the background backfill
/// job does not narrow it. Both sides fill in the same set of rows, and whoever gets there first
/// counts.
pub async fn types_needing_embedding(
    pool: &PgPool,
    kb_id: Uuid,
    model: &str,
    only: Option<TypeKind>,
) -> AppResult<Vec<TypeToEmbed>> {
    let mut out = Vec::new();
    if only == Some(TypeKind::Relation) {
        // Type resolution uses classes alone; waiting for 1633 relations to embed is six minutes
        // wasted
        return relations_needing_embedding(pool, kb_id, model).await;
    }
    let ents: Vec<(Uuid, String, String)> = sqlx::query_as(
        "SELECT id, label, coalesce(description, '') FROM entity_types
         WHERE kb_id = $1
           AND (embedding IS NULL
                OR embedded_model IS DISTINCT FROM $2
                OR embedded_text IS DISTINCT FROM
                   CASE WHEN coalesce(btrim(description), '') = '' THEN btrim(label)
                        ELSE btrim(label) || E'\n' || btrim(description) END)",
    )
    .bind(kb_id)
    .bind(model)
    .fetch_all(pool)
    .await?;
    out.extend(ents.into_iter().map(|(id, label, desc)| TypeToEmbed {
        id,
        kind: TypeKind::Entity,
        text: embed_text(&label, &desc),
        field: EmbedField::Full,
    }));
    // The label-only vector (see `entity_types.label_embedding`). Short queries use this index --
    // queries come in two shapes, so the documents have to as well, otherwise short queries get
    // taken over by tautological classes
    let labels: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT id, label FROM entity_types
         WHERE kb_id = $1
           AND (label_embedding IS NULL
                OR label_embedded_model IS DISTINCT FROM $2
                OR label_embedded_text IS DISTINCT FROM btrim(label))",
    )
    .bind(kb_id)
    .bind(model)
    .fetch_all(pool)
    .await?;
    out.extend(labels.into_iter().map(|(id, label)| TypeToEmbed {
        id,
        kind: TypeKind::Entity,
        text: label.trim().to_string(),
        field: EmbedField::Label,
    }));
    if only == Some(TypeKind::Entity) {
        return Ok(out);
    }
    out.extend(relations_needing_embedding(pool, kb_id, model).await?);
    Ok(out)
}

async fn relations_needing_embedding(
    pool: &PgPool,
    kb_id: Uuid,
    model: &str,
) -> AppResult<Vec<TypeToEmbed>> {
    let mut out = Vec::new();
    let rels: Vec<(Uuid, String, String)> = sqlx::query_as(
        "SELECT id, label, coalesce(description, '') FROM relation_types
         WHERE kb_id = $1
           AND (embedding IS NULL
                OR embedded_model IS DISTINCT FROM $2
                OR embedded_text IS DISTINCT FROM
                   CASE WHEN coalesce(btrim(description), '') = '' THEN btrim(label)
                        ELSE btrim(label) || E'\n' || btrim(description) END)",
    )
    .bind(kb_id)
    .bind(model)
    .fetch_all(pool)
    .await?;
    out.extend(rels.into_iter().map(|(id, label, desc)| TypeToEmbed {
        id,
        kind: TypeKind::Relation,
        text: embed_text(&label, &desc),
        // Relations have no short-query path, only this full-text one
        field: EmbedField::Full,
    }));
    Ok(out)
}

/// Write the vector back, together with "which text was embedded and which model was used". All
/// three must be written in the same statement -- write the vector without its provenance and the
/// next round will still think it is stale, re-embedding it every round from then on.
pub async fn set_type_embeddings(
    pool: &PgPool,
    model: &str,
    items: &[(TypeToEmbed, Vec<f32>)],
) -> AppResult<()> {
    let mut tx = pool.begin().await?;
    for (item, emb) in items {
        let table = match item.kind {
            TypeKind::Entity => "entity_types",
            TypeKind::Relation => "relation_types",
        };
        // The two vectors each write their own columns (see `entity_types.label_embedding`).
        // Different column-name prefix, otherwise exactly the same
        let (vec_col, text_col, model_col) = match item.field {
            EmbedField::Full => ("embedding", "embedded_text", "embedded_model"),
            EmbedField::Label => (
                "label_embedding",
                "label_embedded_text",
                "label_embedded_model",
            ),
        };
        sqlx::query(&format!(
            "UPDATE {table} SET {vec_col} = $2, {text_col} = $3, {model_col} = $4
             WHERE id = $1"
        ))
        .bind(item.id)
        .bind(Vector::from(emb.clone()))
        .bind(&item.text)
        .bind(model)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// The k entity types nearest to the given vector.
///
/// **Classes with neither a description nor a place in the class tree do not take part.** When a
/// vocabulary is imported, every external IRI referenced by domainIncludes / rangeIncludes /
/// equivalentClass gets a row of its own (OMG, UNECE, GS1...), and those have no label body, no
/// parent and no child -- they are not vocabulary, they are dangling references.
///
/// And they happen to be very good at winning: when the description is empty, `embed_text` degrades
/// to embedding the label alone, so what that row embeds is the single word "Location". The shorter
/// side has a systematically smaller distance (we have already tripped over this same regularity
/// three times, in this file and in type resolution), and so an empty shell beat
/// `administrative_area` with its 83-character definition -- in a real run, `Hangzhou Gongshu
/// District` was lost in exactly this way.
///
/// And even if it were served up, it could not be judged: adjudication sees `- location
/// (location)`, with no definition to go on. In a KB with schema.org installed there are 50 rows
/// like this, and 43 of them do not have even a single inheritance edge.
///
/// **Only excluded as candidates, the rows are not deleted**: domain / range still point at them,
/// and deleting them would break those references.
pub async fn nearest_entity_types(
    pool: &PgPool,
    kb_id: Uuid,
    embedding: &[f32],
    limit: i64,
    // true = compare against the label-only vector (see `entity_types.label_embedding`). Short
    // phrases take this path: **short against short**, otherwise `district. place` loses to
    // one-line tautologies like `Map\nA map.`
    by_label: bool,
) -> AppResult<Vec<TypeCandidate>> {
    let col = if by_label {
        "label_embedding"
    } else {
        "embedding"
    };
    let rows: Vec<(Uuid, String, String, String, f64)> = sqlx::query_as(&format!(
        "SELECT id, key, label, coalesce(description, ''), ({col} <=> $2)::float8
         FROM entity_types t
         WHERE t.kb_id = $1 AND t.{col} IS NOT NULL
           AND (coalesce(btrim(t.description), '') <> ''
                OR EXISTS (SELECT 1 FROM entity_type_parents p
                           WHERE p.child_id = t.id OR p.parent_id = t.id))
         ORDER BY {col} <=> $2
         LIMIT $3"
    ))
    .bind(kb_id)
    .bind(Vector::from(embedding.to_vec()))
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, key, label, description, distance)| TypeCandidate {
            id,
            key,
            label,
            description,
            kind: None,
            distance: distance as f32,
        })
        .collect())
}

/// The k relations/attributes nearest to the given vector.
///
/// `only_kind` splits the lanes: a fact with a literal object is looking for an attribute, one with
/// an entity object is looking for a relation. Without the split we would push an attribute like
/// `founding_date` at a relation fact, and the other way round too.
pub async fn nearest_relation_types(
    pool: &PgPool,
    kb_id: Uuid,
    embedding: &[f32],
    limit: i64,
    only_kind: Option<&str>,
) -> AppResult<Vec<TypeCandidate>> {
    let rows: Vec<(Uuid, String, String, String, String, f64)> = sqlx::query_as(
        "SELECT id, key, label, coalesce(description, ''), kind, (embedding <=> $2)::float8
         FROM relation_types
         WHERE kb_id = $1 AND embedding IS NOT NULL
           AND ($4::text IS NULL OR kind = $4)
         ORDER BY embedding <=> $2
         LIMIT $3",
    )
    .bind(kb_id)
    .bind(Vector::from(embedding.to_vec()))
    .bind(limit)
    .bind(only_kind)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(
            |(id, key, label, description, kind, distance)| TypeCandidate {
                id,
                key,
                label,
                description,
                kind: Some(kind),
                distance: distance as f32,
            },
        )
        .collect())
}

/// Look up the id of a relation/attribute by key. For the "map onto an existing type" path.
///
/// It does not distinguish kind: attributes and relations share one table and one key namespace,
/// and once the caller has the id it knows perfectly well what to do with it (when rewriting a
/// fact, a predicate is a predicate).
pub async fn relation_type_id_by_key(
    pool: &PgPool,
    kb_id: Uuid,
    key: &str,
) -> AppResult<Option<Uuid>> {
    let row: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM relation_types WHERE kb_id = $1 AND key = $2")
            .bind(kb_id)
            .bind(key)
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|(id,)| id))
}

/// The datatype an attribute declares. Literal facts are converted according to it when rewritten.
///
/// **The row in the database** is authoritative, not the request: when pointing at an existing
/// attribute the request has no datatype at all, and even if it did, the ontology decides.
pub async fn relation_type_datatype(pool: &PgPool, id: Uuid) -> AppResult<Option<String>> {
    let row: Option<(Option<String>,)> =
        sqlx::query_as("SELECT datatype FROM relation_types WHERE id = $1")
            .bind(id)
            .fetch_optional(pool)
            .await?;
    Ok(row.and_then(|(d,)| d))
}

/// Adopt an IRI onto an existing **local** class (the kind that had no IRI before).
///
/// **Only the IRI and the shape are written; label, description and colour are left alone.** What
/// adoption is there to fix is "this tree is broken", not "overwrite the user's wording with the
/// vocabulary's": a seed class's description was tuned against extraction and follows the KB's
/// language, whereas schema.org's descriptions are English boilerplate. Overwriting it quietly
/// swaps out the single most load-bearing sentence in the extraction prompt.
///
/// **The shape does have to change with it, because the shape is exactly what states the
/// provenance** (square = declared by a vocabulary, round = grown out of the corpus). A class
/// adopted as "declared by a vocabulary" but still drawn round means the picture is lying. We hit
/// this gap for real: importing schema.org into a KB that already had `person` / `organization`,
/// those classes got their IRIs but stayed round -- having an IRI while being round is
/// self-contradictory.
///
/// Colour is left alone: colour is **identity** (the same key always gets the same colour), and
/// adoption does not change who it is. Shape is **provenance**, and adoption changes precisely
/// that.
///
/// It only writes when `iri IS NULL`, so a repeated import is idempotent, and it will never steal a
/// class another vocabulary has already adopted.
pub async fn adopt_iri_onto_key(
    pool: &PgPool,
    kb_id: Uuid,
    key: &str,
    iri: &str,
) -> AppResult<Option<Uuid>> {
    let row: Option<(Uuid,)> = sqlx::query_as(
        "UPDATE entity_types SET iri = $3, shape = 'square'
         WHERE kb_id = $1 AND key = $2 AND iri IS NULL
         RETURNING id",
    )
    .bind(kb_id)
    .bind(key)
    .bind(iri)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(id,)| id))
}

/// The nearest **class ids** to the given vector (ids only; the caller already has the full class
/// data in hand).
///
/// For extraction: the chunk vector is already there inside the extraction loop (entity resolution
/// uses it), so we use it to retrieve the classes this chunk might need and lay only those into the
/// prompt.
pub async fn nearest_entity_type_ids(
    pool: &PgPool,
    kb_id: Uuid,
    embedding: &[f32],
    limit: i64,
) -> AppResult<Vec<Uuid>> {
    let rows: Vec<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM entity_types
         WHERE kb_id = $1 AND embedding IS NOT NULL
         ORDER BY embedding <=> $2
         LIMIT $3",
    )
    .bind(kb_id)
    .bind(Vector::from(embedding.to_vec()))
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

/// Same as above, for relations and attributes. `only_kind` splits the lanes: the relation list and
/// the attribute list are two separate sections in the prompt.
pub async fn nearest_relation_type_ids(
    pool: &PgPool,
    kb_id: Uuid,
    embedding: &[f32],
    limit: i64,
    only_kind: Option<&str>,
) -> AppResult<Vec<Uuid>> {
    let rows: Vec<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM relation_types
         WHERE kb_id = $1 AND embedding IS NOT NULL
           AND ($4::text IS NULL OR kind = $4)
         ORDER BY embedding <=> $2
         LIMIT $3",
    )
    .bind(kb_id)
    .bind(Vector::from(embedding.to_vec()))
    .bind(limit)
    .bind(only_kind)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

/// Insert a batch of classes at once; returns key → id.
///
/// **The reason this exists is fsync.** Row-by-row `execute(pool)` commits each row on its own, and
/// importing something the size of schema.org (968 classes plus about 1500 properties) is five
/// thousand fsyncs -- 45 seconds, measured. The same rows in a single statement are 536
/// milliseconds. The difference is not the round trips, it is the commits.
///
/// `ON CONFLICT DO NOTHING` rather than an error: how to handle a key collision was already decided
/// during the planning phase (the comment on [`crate::ontology::create_entity_type_with_iri`]
/// explains why we do not overwrite), and this only puts the plan into effect, so a collision here
/// means the plan is out of sync with the database -- skip it and let the caller discover who is
/// missing from the returned map.
pub async fn create_entity_types_bulk(
    pool: &PgPool,
    kb_id: Uuid,
    rows: &[(String, String, String, String)],
) -> AppResult<std::collections::HashMap<String, Uuid>> {
    if rows.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    for (key, ..) in rows {
        validate_key(key)?;
    }
    let keys: Vec<&str> = rows.iter().map(|r| r.0.as_str()).collect();
    let labels: Vec<&str> = rows.iter().map(|r| r.1.as_str()).collect();
    let descs: Vec<&str> = rows.iter().map(|r| r.2.as_str()).collect();
    let iris: Vec<&str> = rows.iter().map(|r| r.3.as_str()).collect();
    // Colours are computed from the key on the Rust side and go in along with the UNNEST -- SQL
    // cannot call a Rust function, and this batch is exactly the path a big ontology import takes
    let colours: Vec<&str> = keys
        .iter()
        .map(|k| crate::palette::color_for_key(k))
        .collect();
    let shapes: Vec<&str> = iris.iter().map(|i| crate::palette::shape_for(i)).collect();
    let out: Vec<(Uuid, String)> = sqlx::query_as(
        "INSERT INTO entity_types (id, kb_id, key, label, color, shape, description, iri)
         SELECT gen_random_uuid(), $1, k, l, c, s, d, i
         FROM UNNEST($2::text[], $3::text[], $4::text[], $5::text[], $6::text[], $7::text[])
              AS t(k, l, d, i, c, s)
         ON CONFLICT (kb_id, key) DO NOTHING
         RETURNING id, key",
    )
    .bind(kb_id)
    .bind(&keys)
    .bind(&labels)
    .bind(&descs)
    .bind(&iris)
    .bind(&colours)
    .bind(&shapes)
    .fetch_all(pool)
    .await?;
    Ok(out.into_iter().map(|(id, k)| (k, id)).collect())
}

/// One row for bulk-creating relations/attributes.
///
/// **`functional` / `inverse_functional` must be written as the vocabulary has them; they must not
/// default to false.** They are what the temporal engine uses to close facts automatically, and
/// guessing wrong manufactures false conflicts in bulk -- the time `part_of` was mislabelled
/// functional it piled up 59 of them.
pub struct BulkRelation {
    pub key: String,
    pub label: String,
    pub description: String,
    pub iri: String,
    /// `relation` or `attribute`
    pub kind: &'static str,
    /// Only meaningful for attributes; relations pass `None`
    pub datatype: Option<String>,
    pub functional: bool,
    pub inverse_functional: bool,
    /// OWL property axioms, the basis on which the consistency check decides (0002 R0).
    /// These too must be written as the vocabulary has them -- `alias_of` being bidirectional is
    /// right, `produces` being bidirectional is wrong, and the only thing that tells the two apart
    /// is the ontology
    pub transitive: bool,
    pub symmetric: bool,
    pub asymmetric: bool,
    pub irreflexive: bool,
}

/// Insert a batch of relations or attributes at once; returns key → id. Same semantics as
/// [`create_entity_types_bulk`].
///
/// `kind` decides whether it goes down the relation lane or the attribute lane; `datatype` is only
/// meaningful for attributes, relations pass `None`.
/// `temporal` is fixed at `state` -- OWL has no corresponding concept, and guessing event or
/// eternal would both be worse (consistent with the single-row version's reasoning).
pub async fn create_relation_types_bulk(
    pool: &PgPool,
    kb_id: Uuid,
    rows: &[BulkRelation],
) -> AppResult<std::collections::HashMap<String, Uuid>> {
    if rows.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    for r in rows {
        validate_key(&r.key)?;
    }
    let keys: Vec<&str> = rows.iter().map(|r| r.key.as_str()).collect();
    let labels: Vec<&str> = rows.iter().map(|r| r.label.as_str()).collect();
    let descs: Vec<&str> = rows.iter().map(|r| r.description.as_str()).collect();
    let iris: Vec<&str> = rows.iter().map(|r| r.iri.as_str()).collect();
    let kinds: Vec<&str> = rows.iter().map(|r| r.kind).collect();
    let dts: Vec<Option<&str>> = rows.iter().map(|r| r.datatype.as_deref()).collect();
    let funcs: Vec<bool> = rows.iter().map(|r| r.functional).collect();
    let invs: Vec<bool> = rows.iter().map(|r| r.inverse_functional).collect();
    let trans: Vec<bool> = rows.iter().map(|r| r.transitive).collect();
    let syms: Vec<bool> = rows.iter().map(|r| r.symmetric).collect();
    let asyms: Vec<bool> = rows.iter().map(|r| r.asymmetric).collect();
    let irrefs: Vec<bool> = rows.iter().map(|r| r.irreflexive).collect();
    let out: Vec<(Uuid, String)> = sqlx::query_as(
        "INSERT INTO relation_types
             (id, kb_id, key, label, temporal, functional, inverse_functional,
              description, kind, datatype, iri,
              is_transitive, is_symmetric, is_asymmetric, is_irreflexive)
         SELECT gen_random_uuid(), $1, k, l, 'state', fu, iv, d, kind, dt, i,
                tr, sy, asym, irr
         FROM UNNEST($2::text[], $3::text[], $4::text[], $5::text[], $6::text[], $7::text[],
                     $8::bool[], $9::bool[], $10::bool[], $11::bool[], $12::bool[], $13::bool[])
              AS t(k, l, d, i, kind, dt, fu, iv, tr, sy, asym, irr)
         ON CONFLICT (kb_id, key) DO NOTHING
         RETURNING id, key",
    )
    .bind(kb_id)
    .bind(&keys)
    .bind(&labels)
    .bind(&descs)
    .bind(&iris)
    .bind(&kinds)
    .bind(&dts)
    .bind(&funcs)
    .bind(&invs)
    .bind(&trans)
    .bind(&syms)
    .bind(&asyms)
    .bind(&irrefs)
    .fetch_all(pool)
    .await?;
    Ok(out.into_iter().map(|(id, k)| (k, id)).collect())
}

/// Write the domain / range for a batch of relations at once.
///
/// The single-row version [`set_domains_ranges`] already uses unnest internally to save round
/// trips, but it has to run 4 statements for **every relation** (a DELETE and an INSERT on each of
/// the two tables). 1500 relations is 6000 separate commits, and the commits are the cost. This
/// flattens the association rows for all the relations into two statements.
///
/// **No DELETE**: the caller has just created these relations, so there cannot be old rows in the
/// association tables. Updating an existing relation still goes through the single-row version.
pub async fn link_domains_ranges_bulk(
    pool: &PgPool,
    domains: &[(Uuid, Uuid)],
    ranges: &[(Uuid, Uuid)],
) -> AppResult<()> {
    for (table, pairs) in [
        ("relation_type_domains", domains),
        ("relation_type_ranges", ranges),
    ] {
        if pairs.is_empty() {
            continue;
        }
        let rels: Vec<Uuid> = pairs.iter().map(|p| p.0).collect();
        let types: Vec<Uuid> = pairs.iter().map(|p| p.1).collect();
        sqlx::query(&format!(
            "INSERT INTO {table} (relation_type_id, entity_type_id)
             SELECT r, t FROM UNNEST($1::uuid[], $2::uuid[]) AS x(r, t)
             ON CONFLICT DO NOTHING"
        ))
        .bind(&rels)
        .bind(&types)
        .execute(pool)
        .await?;
    }
    Ok(())
}

/// Set the parents for a batch of classes at once. Same semantics as [`set_parents`], but it **does
/// not check for cycles**.
///
/// The single-row version has to run a recursive CTE for cycle detection every time, so a thousand
/// classes is a thousand recursive queries. This is only used for classes newly created by an
/// import: a cycle is the upstream vocabulary's problem, and `set_parents` only ever handled a
/// cycle by skipping that one edge (`let _ =`) anyway, not by aborting the import. Once the import
/// is done, the ontology page can still find it and let someone deal with it.
pub async fn set_parents_bulk(pool: &PgPool, pairs: &[(Uuid, Uuid)]) -> AppResult<()> {
    if pairs.is_empty() {
        return Ok(());
    }
    let children: Vec<Uuid> = pairs.iter().map(|p| p.0).collect();
    let parents: Vec<Uuid> = pairs.iter().map(|p| p.1).collect();
    sqlx::query(
        "INSERT INTO entity_type_parents (child_id, parent_id)
         SELECT c, p FROM UNNEST($1::uuid[], $2::uuid[]) AS x(c, p)
         WHERE c <> p
         ON CONFLICT DO NOTHING",
    )
    .bind(&children)
    .bind(&parents)
    .execute(pool)
    .await?;
    Ok(())
}

/// Persist class disjointness (see the axiom columns on `relation_types`). Same semantics as
/// [`set_parents_bulk`].
///
/// **Both directions are written.** The parsing side has already expanded the symmetry of
/// `owl:disjointWith` into two rows, so here we just write what we are given -- which means asking
/// "are A and B disjoint" does not have to care which end you ask from.
///
/// `a <> b` blocks self-reference: declaring something disjoint from itself is a meaningless
/// declaration, and it would make the consistency check report every entity as a contradiction. The
/// table has the same CHECK and we keep both -- the constraint is the last line of defence, and
/// filtering here is so that one dirty row does not fail an entire batch insert.
pub async fn set_disjoint_bulk(
    pool: &PgPool,
    kb_id: Uuid,
    pairs: &[(Uuid, Uuid)],
) -> AppResult<()> {
    if pairs.is_empty() {
        return Ok(());
    }
    let a: Vec<Uuid> = pairs.iter().map(|p| p.0).collect();
    let b: Vec<Uuid> = pairs.iter().map(|p| p.1).collect();
    sqlx::query(
        "INSERT INTO entity_type_disjoint (kb_id, a_id, b_id)
         SELECT $1, x, y FROM UNNEST($2::uuid[], $3::uuid[]) AS t(x, y)
         WHERE x <> y
         ON CONFLICT DO NOTHING",
    )
    .bind(kb_id)
    .bind(&a)
    .bind(&b)
    .execute(pool)
    .await?;
    Ok(())
}

/// Set `others` as **all** of this class's disjointness targets (anything not in the list is
/// released).
///
/// The division of labour with [`set_disjoint_bulk`]: that one is the import side's "only add,
/// never remove", this one is the editor's "this is the whole set". Editing has to be able to
/// cancel -- otherwise unchecking a box in the interface has no effect at all, while the user
/// believes they changed something.
///
/// One row per direction, same as the import side: asking "are A and B disjoint" therefore does not
/// have to care which end you ask from.
pub async fn set_disjoint_for(
    pool: &PgPool,
    kb_id: Uuid,
    class: Uuid,
    others: &[Uuid],
) -> AppResult<()> {
    let mut tx = pool.begin().await?;
    // First clear every disjointness edge this class takes part in -- both directions, because it
    // can show up on either side
    sqlx::query(
        "DELETE FROM entity_type_disjoint
          WHERE kb_id = $1 AND (a_id = $2 OR b_id = $2)",
    )
    .bind(kb_id)
    .bind(class)
    .execute(&mut *tx)
    .await?;
    for other in others {
        if *other == class {
            continue;
        }
        sqlx::query(
            "INSERT INTO entity_type_disjoint (kb_id, a_id, b_id)
             VALUES ($1, $2, $3), ($1, $3, $2)
             ON CONFLICT DO NOTHING",
        )
        .bind(kb_id)
        .bind(class)
        .bind(other)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Ontology proposals (see `ontology_proposals`)
// ---------------------------------------------------------------------------

/// One stored proposal. `payload` is the one the API returns verbatim.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct StoredProposal {
    pub section: String,
    pub key: String,
    pub payload: serde_json::Value,
}

/// Write down the results of one Suggest round.
///
/// **Anything a human has already ruled on is left alone.** The `WHERE status = 'open'` clause is
/// the entire point of this function: re-running Suggest computes the rejected proposal all over
/// again (the raw material is still sitting in `ontology_misses`), and without that clause it would
/// be flushed back to open -- which means every run wipes out a human's veto once more.
pub async fn save_proposals(
    pool: &PgPool,
    kb_id: Uuid,
    items: &[(String, String, serde_json::Value)],
) -> AppResult<()> {
    for (section, key, payload) in items {
        sqlx::query(
            "INSERT INTO ontology_proposals (id, kb_id, section, key, payload)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (kb_id, section, key) DO UPDATE
               SET payload = EXCLUDED.payload, created_at = now()
               WHERE ontology_proposals.status = 'open'",
        )
        .bind(Uuid::now_v7())
        .bind(kb_id)
        .bind(section)
        .bind(key)
        .bind(payload)
        .execute(pool)
        .await?;
    }
    Ok(())
}

/// Proposals still waiting for a human to look at them. Newest first -- the old batch has already
/// been looked at several times over.
pub async fn open_proposals(pool: &PgPool, kb_id: Uuid) -> AppResult<Vec<StoredProposal>> {
    Ok(sqlx::query_as(
        "SELECT section, key, payload FROM ontology_proposals
         WHERE kb_id = $1 AND status = 'open'
         ORDER BY created_at DESC, key",
    )
    .bind(kb_id)
    .fetch_all(pool)
    .await?)
}

/// A proposal was adopted or rejected.
///
/// **Change the status, do not delete the row**: the adoption happened, and so did the rejection.
/// The same path as `fact_adoptions` and `entity_retypes`. Leaving a trace of the rejection also
/// has an immediately useful effect -- the next Suggest round will not flush it back to pending.
pub async fn decide_proposal(
    pool: &PgPool,
    kb_id: Uuid,
    section: &str,
    key: &str,
    status: &str,
    actor: Uuid,
) -> AppResult<()> {
    sqlx::query(
        "UPDATE ontology_proposals
            SET status = $4, decided_by = $5, decided_at = now()
          WHERE kb_id = $1 AND section = $2 AND key = $3 AND status = 'open'",
    )
    .bind(kb_id)
    .bind(section)
    .bind(key)
    .bind(status)
    .bind(actor)
    .execute(pool)
    .await?;
    Ok(())
}

/// How many are still waiting to be looked at. The gap in 0003: once the automatic-extension switch
/// is off there is no "N since last time" reminder, so the signal is in the panel but nobody goes
/// looking -- with this table, the reminder is this one query.
pub async fn open_proposal_count(pool: &PgPool, kb_id: Uuid) -> AppResult<i64> {
    let (n,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM ontology_proposals WHERE kb_id = $1 AND status = 'open'",
    )
    .bind(kb_id)
    .fetch_one(pool)
    .await?;
    Ok(n)
}

/// Whether the subject's type fits the domain this relation declares. Searches up the inheritance
/// chain.
///
/// **It only answers; it does not touch data.** 0001 already ruled on this: a signature is a hint,
/// not a gate, and "driving automatic actions off a possibly-wrong declaration is risky". And the
/// entity type is itself something the model decided (in a real run Elon Musk came out as
/// `researcher`), so using it to flip the direction of a fact is two layers of uncertainty stacked
/// on top of each other and then rewritten silently. So the result here is only used to record a
/// signal.
///
/// Three answers, do not collapse them into two:
/// - `Some(true)`  fits
/// - `Some(false)` does not fit -- **this is the signal**
/// - `None`        nothing to judge (relation declares no domain, or the entity has no type yet)
pub async fn subject_fits_domain(
    pool: &PgPool,
    relation_type_id: Uuid,
    subject_type_id: Option<Uuid>,
) -> AppResult<Option<bool>> {
    let Some(subject_type_id) = subject_type_id else {
        return Ok(None);
    };
    let (declared, ok): (i64, i64) = sqlx::query_as(
        "WITH RECURSIVE up(id) AS (
             SELECT $2::uuid
             UNION
             SELECT p.parent_id FROM entity_type_parents p JOIN up ON p.child_id = up.id
         )
         SELECT (SELECT count(*) FROM relation_type_domains WHERE relation_type_id = $1),
                (SELECT count(*) FROM relation_type_domains d
                   JOIN up ON up.id = d.entity_type_id
                  WHERE d.relation_type_id = $1)",
    )
    .bind(relation_type_id)
    .bind(subject_type_id)
    .fetch_one(pool)
    .await?;
    Ok((declared > 0).then_some(ok > 0))
}

/// All the ancestors of these classes (not including themselves). Walks up `subClassOf`; multiple
/// inheritance and diamonds both work.
///
/// Used to put a floor under the per-chunk retrieval candidates: vector retrieval favours the leaf
/// classes that appear literally in the text, and generalised base classes rank far down (in a real
/// run `person` came 359th out of 976 classes), so they are missing from the prompt -- the entity
/// has nowhere to land, and the relation signature degrades to `*`. Ancestors are the
/// generalisation relationships the ontology declares itself, so filling in from them is more
/// reliable than maintaining a list of "generic classes".
pub async fn ancestors_of(pool: &PgPool, ids: &[Uuid]) -> AppResult<Vec<Uuid>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    // UNION in the recursive term dedupes, so diamond inheritance never expands one ancestor twice
    let rows: Vec<(Uuid,)> = sqlx::query_as(
        "WITH RECURSIVE up(id) AS (
             SELECT unnest($1::uuid[])
             UNION
             SELECT p.parent_id FROM entity_type_parents p JOIN up ON p.child_id = up.id
         )
         SELECT id FROM up WHERE id <> ALL($1::uuid[])",
    )
    .bind(ids)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

/// Same as [`subject_fits_domain`], but the type is **read from the entity in the database**, not
/// taken from the copy the caller is holding.
///
/// The extractor's `entity_type_of` only covers the entities the model declared in this chunk; the
/// object is often an entity that already exists elsewhere, and this chunk did not re-declare its
/// type, so there is nothing to look up and nothing to judge. Resolution, however, has already
/// linked it to that row in the database -- the type is there, and only using it gives a complete
/// judgement.
pub async fn entity_fits_domain(
    pool: &PgPool,
    relation_type_id: Uuid,
    entity_id: Uuid,
) -> AppResult<Option<bool>> {
    let (declared, ok): (i64, i64) = sqlx::query_as(
        "WITH RECURSIVE up(id) AS (
             SELECT type_id FROM entities WHERE id = $2
             UNION
             SELECT p.parent_id FROM entity_type_parents p JOIN up ON p.child_id = up.id
         )
         SELECT (SELECT count(*) FROM relation_type_domains WHERE relation_type_id = $1),
                (SELECT count(*) FROM relation_type_domains d
                   JOIN up ON up.id = d.entity_type_id
                  WHERE d.relation_type_id = $1)",
    )
    .bind(relation_type_id)
    .bind(entity_id)
    .fetch_one(pool)
    .await?;
    Ok((declared > 0).then_some(ok > 0))
}

/// How a (subject, predicate, object) triple should land against the predicate's domain signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fit {
    /// The predicate declares no domain -- nothing to judge against, land it as-is
    Unchecked,
    /// The subject fits
    Keep,
    /// Subject does not fit, object does: swap subject and object to match the signature
    Swap,
    /// Neither side fits: this relation does not apply to this pair of entities, so the predicate
    /// should be left empty
    Neither,
}

/// **The one check shared by all three paths that write a predicate** (#190 / #196): extraction
/// landing a new fact, adoption hanging a predicate back onto an old fact, and a merge swapping out
/// the subject -- it used to be checked by extraction only, and the other two each went around it.
///
/// The test is deliberately narrow (0012): it looks at the signature alone, swaps only when the
/// **forward direction is violated and the reverse holds**, and leaves the predicate empty when
/// neither direction lines up. Argument order is not an assertion about the world, it is this key's
/// encoding convention, which is why the ontology is enforced at this one point; which types may
/// take part is still guidance, and is not adjudicated here.
///
/// **range counts too** (#222). This used to look at domain only: `headOf` has domain Agent, and in
/// schema.org a Project is an Organization and therefore an Agent as well, so `Project Aurora
/// head_of Li Ting` passed on the subject and got Keep, while nobody looked at the object being a
/// person when range wants an Organization. Now each end is checked on its own: Keep only if
/// neither end is violated in the forward direction; otherwise Swap only if neither end is violated
/// the other way round. An entity whose type was not decided does not count as a violation on the
/// range end ("don't know" is not "does not fit", the same discipline as `signature_breaks`); the
/// domain end keeps the old rule, which is what 0012 laid down
pub async fn judge_direction(
    pool: &PgPool,
    relation_type_id: Uuid,
    subject_id: Uuid,
    object_id: Uuid,
) -> AppResult<Fit> {
    let subject_in_domain = entity_fits_domain(pool, relation_type_id, subject_id).await?;
    let object_in_range = entity_fits_range(pool, relation_type_id, object_id).await?;
    if subject_in_domain.is_none() && object_in_range.is_none() {
        return Ok(Fit::Unchecked);
    }
    if subject_in_domain != Some(false) && object_in_range != Some(false) {
        return Ok(Fit::Keep);
    }
    let object_in_domain = entity_fits_domain(pool, relation_type_id, object_id).await?;
    let subject_in_range = entity_fits_range(pool, relation_type_id, subject_id).await?;
    if object_in_domain != Some(false) && subject_in_range != Some(false) {
        return Ok(Fit::Swap);
    }
    Ok(Fit::Neither)
}

/// The range version of [`entity_fits_domain`]. One extra rule: an entity whose type has not been
/// decided yet → None, and that does not count as a violation -- the range end is a newly added
/// test (#222), and it should not cost facts about unclassified entities their predicate
pub async fn entity_fits_range(
    pool: &PgPool,
    relation_type_id: Uuid,
    entity_id: Uuid,
) -> AppResult<Option<bool>> {
    let (declared, typed, ok): (i64, bool, i64) = sqlx::query_as(
        "WITH RECURSIVE up(id) AS (
             SELECT type_id FROM entities WHERE id = $2
             UNION
             SELECT p.parent_id FROM entity_type_parents p JOIN up ON p.child_id = up.id
         )
         SELECT (SELECT count(*) FROM relation_type_ranges WHERE relation_type_id = $1),
                (SELECT type_id IS NOT NULL FROM entities WHERE id = $2),
                (SELECT count(*) FROM relation_type_ranges g
                   JOIN up ON up.id = g.entity_type_id
                  WHERE g.relation_type_id = $1)",
    )
    .bind(relation_type_id)
    .bind(entity_id)
    .fetch_one(pool)
    .await?;
    Ok((declared > 0 && typed).then_some(ok > 0))
}

/// Resolve `owl:inverseOf` / `rdfs:subPropertyOf` from IRIs into ids.
///
/// **This has to be a second pass.** Both of these point at another relation type, and the ids only
/// exist once everything has been inserted -- a single-pass version can only handle files where
/// "the super-property happens to come first", and RDF triples have no order.
///
/// Paired by IRI rather than by key: a key can pick up a suffix because of a name collision
/// (`part_of_2`), whereas the IRI is the identity within this ontology.
pub async fn link_property_axioms_bulk(
    pool: &PgPool,
    kb_id: Uuid,
    inverse: &[(String, String)],
    sub_property: &[(String, String)],
) -> AppResult<(u64, u64)> {
    let run = |column: &'static str, pairs: &[(String, String)]| {
        let src: Vec<String> = pairs.iter().map(|(s, _)| s.clone()).collect();
        let dst: Vec<String> = pairs.iter().map(|(_, d)| d.clone()).collect();
        async move {
            if src.is_empty() {
                return AppResult::Ok(0);
            }
            // If the target IRI cannot be found in this KB, skip that one -- **partial imports are
            // the norm** (properties from external vocabularies get referenced), and one link that
            // cannot be made should not fail the whole import
            let sql = format!(
                "UPDATE relation_types r SET {column} = t.id
                   FROM UNNEST($2::text[], $3::text[]) AS p(src, dst)
                   JOIN relation_types t ON t.kb_id = $1 AND t.iri = p.dst
                  WHERE r.kb_id = $1 AND r.iri = p.src AND r.id <> t.id"
            );
            let n = sqlx::query(&sql)
                .bind(kb_id)
                .bind(&src)
                .bind(&dst)
                .execute(pool)
                .await?
                .rows_affected();
            AppResult::Ok(n)
        }
    };
    let inv = run("inverse_of", inverse).await?;
    let sub = run("sub_property_of", sub_property).await?;
    Ok((inv, sub))
}
