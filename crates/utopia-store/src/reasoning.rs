//! Fetching the data for the consistency check and writing the results back. The judgement
//! itself lives in `utopia-reason` -- that layer never touches the database.
//!
//! Three steps: read the live facts as edges, read the axiom columns of `relation_types` into
//! `Axioms`, and write the violations `check()` spits out into `axiom_violations`.
//!
//! **A re-run is idempotent, and the recomputation is what counts.** After every run, the `open`
//! rows in this knowledge base that were not recomputed get deleted -- they are derived state,
//! and once the fact is retracted or the axiom is relaxed, that violation has no business still
//! sitting on the Review page. A row somebody has already ruled on (`resolved`) is left
//! untouched: that is a human's decision, not something we computed.
//!
//! This rule is the reverse of a hole we fell into once (see `ontology_proposals`): over there a
//! re-run pushed previously rejected proposals back into the pending list, which means every run
//! erased a human's veto once more. So `ON CONFLICT` here does nothing at all -- a row already
//! in the database, open or resolved, stays exactly as it is.

use serde_json::json;
use sqlx::PgPool;
use std::collections::{HashMap, HashSet};
use utopia_core::models::{AxiomViolation, DerivedFactView, OntologyDefect};
/// The literal for a rule kind. A &'static str rather than an enum: it goes straight into SQL
/// and serves directly as a key
type RuleKind = &'static str;
use utopia_core::AppResult;
use utopia_reason::derive::{Contradictions, Derivation, TimedEdge};
use utopia_reason::{check, Axioms, Edge, Kind, Violation};
use uuid::Uuid;

/// What one check produces, for the caller to write the audit record and to tell the user.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Report {
    /// The number of edges that took part in the check
    pub edges: usize,
    /// The number of predicates declaring at least one axiom. **When this is zero the conclusion
    /// is "there are no grounds to judge on", not "there are no contradictions"**
    pub predicates_with_axioms: usize,
    /// The total number of violations computed this time
    pub found: usize,
    /// Of those, the new ones (not previously in the database)
    pub inserted: usize,
    /// The stale open rows that were cleared
    pub cleared: usize,
    /// The number of derivations that clashed with an assertion (0017), already included in
    /// `found`
    pub contradictions: usize,
    /// The number of contradictions that hit the per-predicate cap and never made it into the
    /// queue. **When this is non-zero the root cause is the rule**: when hundreds of derivations
    /// on one predicate all clash, looking at them one by one is pointless
    pub contradictions_capped: usize,
    /// The number of rule pairs that clash with each other -- these go into `ontology_defects`,
    /// not into this table
    pub rules_disagree: usize,
    /// The resolved rows that were reopened: a human said it was retracted, or closed, or that
    /// they would go and change the ontology, and then the violation was computed again -- the
    /// promise was not kept, and the queue does not keep quiet on their behalf (#202)
    pub reopened: usize,
}

/// The cap on how many contradictions from a single predicate enter the queue (0017 §1).
/// Anything beyond it is only counted.
const MAX_CLASHES_PER_PREDICATE: usize = 50;

/// Reads this knowledge base's predicate axioms.
///
/// **Only the ones that declare at least one bit**: a predicate that declares none would not be
/// checked even if it made it into the map (`says_nothing` skips it), so it would only waste
/// memory. And the count itself means something -- it is the measure of "are there any grounds
/// to judge on", and the report needs it.
#[allow(clippy::type_complexity)]
async fn axioms(pool: &PgPool, kb_id: Uuid) -> AppResult<HashMap<Uuid, Axioms>> {
    let rows: Vec<(
        Uuid,
        bool,
        bool,
        bool,
        bool,
        bool,
        bool,
        Option<Uuid>,
        Option<Uuid>,
    )> = sqlx::query_as(
        "SELECT id, is_transitive, is_symmetric, is_asymmetric, is_irreflexive,
                    functional, inverse_functional, inverse_of, sub_property_of
               FROM relation_types
              WHERE kb_id = $1
                AND (is_transitive OR is_symmetric OR is_asymmetric OR is_irreflexive
                     OR functional OR inverse_functional
                     OR inverse_of IS NOT NULL OR sub_property_of IS NOT NULL)",
    )
    .bind(kb_id)
    .fetch_all(pool)
    .await?;
    let mut map: HashMap<Uuid, Axioms> = rows
        .into_iter()
        .map(
            |(
                id,
                transitive,
                symmetric,
                asymmetric,
                irreflexive,
                functional,
                inverse_functional,
                inverse_of,
                sub_property_of,
            )| {
                (
                    id,
                    Axioms {
                        transitive,
                        symmetric,
                        asymmetric,
                        irreflexive,
                        functional,
                        inverse_functional,
                        inverse_of,
                        sub_property_of,
                    },
                )
            },
        )
        .collect();

    // **An inverse is mutual, while the database only stores one direction.** Declare
    // `p⁻¹ = q` without backfilling `q⁻¹ = p` and `A p B` derives `B q A`, while `B q A` derives
    // nothing back to `A p B` -- "asking about the job and asking about the employment give
    // different answers" is exactly what R1 is out to eliminate, and fixing half of it is the
    // same as not fixing it.
    //
    // The normalisation goes here rather than in a database trigger: there is more than one way
    // around a trigger (RDF import, editing the table directly), while loading the axioms
    // happens in this one place that nobody can get around.
    let pairs: Vec<(Uuid, Uuid)> = map
        .iter()
        .filter_map(|(id, ax)| ax.inverse_of.map(|inv| (inv, *id)))
        .collect();
    for (target, source) in pairs {
        // If it already declares its own inverse, leave it alone -- **what a human wrote beats
        // what was inferred**; the two sides pointing at different things is a contradiction in
        // the ontology itself, which R0 reports, not something to quietly change here
        map.entry(target)
            .or_default()
            .inverse_of
            .get_or_insert(source);
    }
    Ok(map)
}

/// The live facts whose subject is not in the predicate's declared domain, or whose object is
/// not in its range (#190 / #196).
///
/// This is the **ledger-layer** half of the signature check: extraction and adoption straighten
/// the direction out or leave it empty at write time according to `ontology::judge_direction`,
/// but a merge swaps the subject out and the ontology can change a domain after the fact, and a
/// guard at write time cannot stop changes made after the write. So this measures the database
/// itself, and any path that wrote it backwards becomes visible in Review.
///
/// **An entity with no type does not count**: it has no type to compare, and "unknown" is not
/// "does not match" -- per 0009, being unclassified is an honest state and should not get
/// reported as a contradiction. Only predicates that declare a domain / range are queried, the
/// same discipline as the other four kinds: no axiom, no grounds to judge on.
///
/// If `only` is given, only those facts are looked at (right after a merge, to check the handful
/// that were moved); None means all of them.
pub async fn signature_breaks(
    pool: &PgPool,
    kb_id: Uuid,
    only: Option<&[Uuid]>,
) -> AppResult<Vec<Uuid>> {
    let rows: Vec<(Uuid,)> = sqlx::query_as(
        "WITH RECURSIVE anc(type_id, anc_id) AS (
             SELECT id, id FROM entity_types WHERE kb_id = $1
             UNION
             SELECT a.type_id, p.parent_id
               FROM anc a JOIN entity_type_parents p ON p.child_id = a.anc_id
         )
         SELECT f.id
           FROM facts f
           JOIN relation_types r ON r.id = f.predicate_id
           JOIN entities s ON s.id = f.subject_id
           JOIN entities o ON o.id = f.object_id
          WHERE f.kb_id = $1 AND f.invalidated_at IS NULL
            AND ($2::uuid[] IS NULL OR f.id = ANY($2))
            AND (
              (s.type_id IS NOT NULL
               AND EXISTS (SELECT 1 FROM relation_type_domains d WHERE d.relation_type_id = r.id)
               AND NOT EXISTS (SELECT 1 FROM relation_type_domains d
                                 JOIN anc a ON a.anc_id = d.entity_type_id
                                WHERE d.relation_type_id = r.id AND a.type_id = s.type_id))
              OR
              (o.type_id IS NOT NULL
               AND EXISTS (SELECT 1 FROM relation_type_ranges g WHERE g.relation_type_id = r.id)
               AND NOT EXISTS (SELECT 1 FROM relation_type_ranges g
                                 JOIN anc a ON a.anc_id = g.entity_type_id
                                WHERE g.relation_type_id = r.id AND a.type_id = o.type_id))
            )
          ORDER BY f.recorded_at",
    )
    .bind(kb_id)
    .bind(only)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

/// Writes the signature violations into `axiom_violations` (kind = `signature`, with left and
/// right being the same fact).
/// Idempotent: reporting the same fact twice does not insert it twice. Returns the number of
/// newly inserted rows.
pub async fn record_signature_breaks(
    pool: &PgPool,
    kb_id: Uuid,
    facts: &[Uuid],
) -> AppResult<usize> {
    let mut inserted = 0usize;
    for fact in facts {
        let id: Option<(Uuid,)> = sqlx::query_as(
            "INSERT INTO axiom_violations (id, kb_id, kind, left_fact, right_fact)
             VALUES ($1, $2, 'signature', $3, $3)
             ON CONFLICT (kb_id, kind, left_fact, right_fact) DO NOTHING
             RETURNING id",
        )
        .bind(Uuid::now_v7())
        .bind(kb_id)
        .bind(fact)
        .fetch_optional(pool)
        .await?;
        if id.is_some() {
            inserted += 1;
        }
    }
    Ok(inserted)
}

/// Runs the check once and writes the results to the database.
pub async fn run(pool: &PgPool, kb_id: Uuid) -> AppResult<Report> {
    let (timed, spans, _) = timed_edges(pool, kb_id).await?;
    let edges: Vec<Edge> = timed.iter().map(|t| t.edge).collect();
    let axioms = axioms(pool, kb_id).await?;
    let mut violations = check(&edges, &axioms);
    // The fifth kind is not in the pure logic engine: it has to look at an entity's type and a
    // predicate's domain / range, and those live in the database. Once computed it follows the
    // same write-and-clear-the-stale rules as the other four
    for fact in signature_breaks(pool, kb_id, None).await? {
        violations.push(Violation {
            kind: Kind::Signature,
            left: fact,
            right: fact,
            path: Vec::new(),
        });
    }

    // The sixth kind (0017): derivations that were inferred but could not land. Computed with
    // the same function `materialize` uses, so what is reported here is exactly what got blocked
    // over there -- compute it twice, once on each side, and the queue stops matching the graph
    let derivation = utopia_reason::derive::derive(&timed, &axioms);
    let clashes = utopia_reason::derive::contradictions(&derivation, &timed, &axioms, &spans);
    let names = names_for(pool, &derivation, &clashes).await?;
    let mut details: HashMap<(Uuid, Uuid), serde_json::Value> = HashMap::new();
    let mut per_pred: HashMap<Uuid, usize> = HashMap::new();
    let mut contradictions_capped = 0usize;
    for c in &clashes.with_assertions {
        let d = &derivation.facts[c.derived];
        let Some(&last) = d.premises.last() else {
            continue;
        };
        let key = (c.against, last);
        if details.contains_key(&key) {
            continue;
        }
        let n = per_pred.entry(d.predicate).or_default();
        if *n >= MAX_CLASHES_PER_PREDICATE {
            contradictions_capped += 1;
            continue;
        }
        *n += 1;
        let span = utopia_reason::derive::validity(&d.premises, &spans);
        details.insert(
            key,
            json!({
                "axiom": c.axiom.as_str(),
                "rule": d.rule.as_str(),
                "via": d.via,
                "via_label": names.predicate(d.via),
                "subject_id": d.subject,
                "subject": names.entity(d.subject),
                "predicate_id": d.predicate,
                "predicate": names.predicate(d.predicate),
                "object_id": d.object,
                "object": names.entity(d.object),
                "valid_from": span.and_then(|s| s.0).map(|t| stamp(t).to_rfc3339()),
                "valid_to": span.and_then(|s| s.1).map(|t| stamp(t).to_rfc3339()),
                "premises": d.premises,
            }),
        );
        violations.push(Violation {
            kind: Kind::DerivedContradiction,
            left: c.against,
            right: last,
            path: d.premises.clone(),
        });
    }

    let mut report = Report {
        edges: edges.len(),
        predicates_with_axioms: axioms.len(),
        found: violations.len(),
        contradictions: details.len(),
        contradictions_capped,
        rules_disagree: clashes.between_derivations.len(),
        ..Default::default()
    };

    // Done inside a transaction, otherwise there is a window between "insert the new ones" and
    // "clear the stale ones", and for that instant the Review page is briefly missing things
    let mut tx = pool.begin().await?;
    let mut fresh: Vec<Uuid> = Vec::with_capacity(violations.len());
    for v in &violations {
        let Violation {
            kind,
            left,
            right,
            path,
        } = v;
        let detail = details
            .get(&(*left, *right))
            .cloned()
            .unwrap_or_else(|| json!({}));
        let id: Option<(Uuid,)> = sqlx::query_as(
            "INSERT INTO axiom_violations (id, kb_id, kind, left_fact, right_fact, path, detail)
             VALUES ($1, $2, $3, $4, $5, $6, $7)
             ON CONFLICT (kb_id, kind, left_fact, right_fact) DO NOTHING
             RETURNING id",
        )
        .bind(Uuid::now_v7())
        .bind(kb_id)
        .bind(kind.as_str())
        .bind(left)
        .bind(right)
        .bind(path)
        .bind(&detail)
        .fetch_optional(&mut *tx)
        .await?;
        if id.is_some() {
            report.inserted += 1;
        }
        // Newly inserted or already there, either way it counts as "still holds this round".
        //
        // The ones that were already there and resolved get a second look: `fact_retracted` /
        // `fact_closed` / `axiom_relaxed` are all promises that "the world will change" -- the
        // fact is gone, the interval is closed, the axiom was relaxed, so the violation should
        // not be computed again. If it is, the promise was not kept, that row goes back to open,
        // and a human looks at it once more. `accepted` is deliberate coexistence and stays
        // silent however many times it is recomputed (#202)
        let (keep, status, resolution): (Uuid, String, Option<String>) = sqlx::query_as(
            "SELECT id, status, resolution FROM axiom_violations
              WHERE kb_id = $1 AND kind = $2 AND left_fact = $3 AND right_fact = $4",
        )
        .bind(kb_id)
        .bind(kind.as_str())
        .bind(left)
        .bind(right)
        .fetch_one(&mut *tx)
        .await?;
        if status == "resolved"
            && matches!(
                resolution.as_deref(),
                Some("fact_retracted" | "fact_closed" | "axiom_relaxed")
            )
        {
            sqlx::query(
                "UPDATE axiom_violations
                    SET status = 'open', resolution = NULL, decided_by = NULL,
                        decided_at = NULL, detected_at = now()
                  WHERE id = $1",
            )
            .bind(keep)
            .execute(&mut *tx)
            .await?;
            report.reopened += 1;
        }
        fresh.push(keep);
    }

    // An open row that was not computed this round is stale: the fact was retracted, or the
    // axiom was relaxed. The resolved ones are left alone -- those are human decisions, not
    // derived state
    let cleared = sqlx::query(
        "DELETE FROM axiom_violations
          WHERE kb_id = $1 AND status = 'open' AND NOT (id = ANY($2))",
    )
    .bind(kb_id)
    .bind(&fresh)
    .execute(&mut *tx)
    .await?;
    report.cleared = cleared.rows_affected() as usize;

    // Derivations clashing with each other go into `ontology_defects` by rule pair -- the root
    // cause is those two declarations, not any one fact. The same pair of predicates can clash
    // in several ways (functional and asymmetric each clash in their own way), and the unique
    // key only goes as far as the predicate pair, so they are merged into one row with every
    // kind of clash written into detail
    let mut by_pair: HashMap<(Uuid, Uuid), Vec<serde_json::Value>> = HashMap::new();
    let mut order: Vec<(Uuid, Uuid)> = Vec::new();
    for rc in &clashes.between_derivations {
        let triple = |i: usize| {
            let d = &derivation.facts[i];
            format!(
                "{} · {} · {}",
                names.entity(d.subject),
                names.predicate(d.predicate),
                names.entity(d.object)
            )
        };
        let examples: Vec<serde_json::Value> = rc
            .pairs
            .iter()
            .take(3)
            .map(|(i, j)| json!([triple(*i), triple(*j)]))
            .collect();
        let key = (rc.a.0, rc.b.0);
        if !by_pair.contains_key(&key) {
            order.push(key);
        }
        by_pair.entry(key).or_default().push(json!({
            "rule_a": rc.a.1.as_str(),
            "via_a": names.predicate(rc.a.0),
            "rule_b": rc.b.1.as_str(),
            "via_b": names.predicate(rc.b.0),
            "axiom": rc.axiom.as_str(),
            "count": rc.pairs.len(),
            "examples": examples,
        }));
    }
    let mut fresh_defects: Vec<Uuid> = Vec::with_capacity(order.len());
    for key in order {
        let rules = by_pair.remove(&key).unwrap_or_default();
        let count: usize = rules
            .iter()
            .map(|r| r["count"].as_u64().unwrap_or(0) as usize)
            .sum();
        // A row somebody has already accepted stays resolved and only has its detail refreshed:
        // 0017 says that once accepted it is not reported again
        let (id,): (Uuid,) = sqlx::query_as(
            "INSERT INTO ontology_defects (id, kb_id, kind, subject, other, path, detail)
             VALUES ($1, $2, 'rules_disagree', $3, $4, '{}', $5)
             ON CONFLICT (kb_id, kind, subject, other) DO UPDATE SET detail = EXCLUDED.detail
             RETURNING id",
        )
        .bind(Uuid::now_v7())
        .bind(kb_id)
        .bind(key.0)
        .bind(key.1)
        .bind(json!({ "count": count, "rules": rules }))
        .fetch_one(&mut *tx)
        .await?;
        fresh_defects.push(id);
    }
    sqlx::query(
        "DELETE FROM ontology_defects
          WHERE kb_id = $1 AND kind = 'rules_disagree' AND status = 'open'
            AND NOT (id = ANY($2))",
    )
    .bind(kb_id)
    .bind(&fresh_defects)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(report)
}

/// A contradiction has to be written in words a human can read, but a derivation never landed in
/// the database and has no text to look up -- the names get filled in here.
struct Names {
    entities: HashMap<Uuid, String>,
    predicates: HashMap<Uuid, String>,
}

impl Names {
    fn entity(&self, id: Uuid) -> String {
        self.entities
            .get(&id)
            .cloned()
            .unwrap_or_else(|| "?".into())
    }
    fn predicate(&self, id: Uuid) -> String {
        self.predicates
            .get(&id)
            .cloned()
            .unwrap_or_else(|| "?".into())
    }
}

async fn names_for(
    pool: &PgPool,
    derivation: &Derivation,
    clashes: &Contradictions,
) -> AppResult<Names> {
    let mut ents: HashSet<Uuid> = HashSet::new();
    let mut preds: HashSet<Uuid> = HashSet::new();
    let mut want = |i: usize| {
        let d = &derivation.facts[i];
        ents.insert(d.subject);
        ents.insert(d.object);
        preds.insert(d.predicate);
        preds.insert(d.via);
    };
    for c in &clashes.with_assertions {
        want(c.derived);
    }
    for rc in &clashes.between_derivations {
        for (i, j) in rc.pairs.iter().take(3) {
            want(*i);
            want(*j);
        }
    }
    for rc in &clashes.between_derivations {
        preds.insert(rc.a.0);
        preds.insert(rc.b.0);
    }
    let ents: Vec<Uuid> = ents.into_iter().collect();
    let preds: Vec<Uuid> = preds.into_iter().collect();
    let entities: Vec<(Uuid, String)> =
        sqlx::query_as("SELECT id, canonical_name FROM entities WHERE id = ANY($1)")
            .bind(&ents)
            .fetch_all(pool)
            .await?;
    let predicates: Vec<(Uuid, String)> =
        sqlx::query_as("SELECT id, label FROM relation_types WHERE id = ANY($1)")
            .bind(&preds)
            .fetch_all(pool)
            .await?;
    Ok(Names {
        entities: entities.into_iter().collect(),
        predicates: predicates.into_iter().collect(),
    })
}

/// What the Review page needs: the violations nobody has ruled on yet, together with the triple
/// text of both facts.
///
/// Expanding into text happens in SQL rather than in a second round trip: a few dozen per page,
/// two triples each, and querying them separately is hundreds of round trips. The predicate
/// falls back to `fact_surface_predicate` -- a fact with no matching relation in the ontology is
/// displayed with the wording from the source (see `facts.predicate_id`).
pub async fn open_violations(
    pool: &PgPool,
    kb_id: Uuid,
    limit: i64,
    offset: i64,
) -> AppResult<Vec<AxiomViolation>> {
    Ok(sqlx::query_as(
        "WITH triple AS (
             SELECT f.id,
                    s.canonical_name || ' · '
                      || COALESCE(r.label, fact_surface_predicate(f.id), '?') || ' · '
                      || COALESCE(o.canonical_name, f.object_value ->> 'summary',
                                  f.object_value #>> '{}', '?') AS text,
                    r.label AS predicate
               FROM facts f
               JOIN entities s ON s.id = f.subject_id
               LEFT JOIN relation_types r ON r.id = f.predicate_id
               LEFT JOIN entities o ON o.id = f.object_id
              WHERE f.kb_id = $1
         )
         SELECT v.id, v.kind, l.predicate,
                v.left_fact, l.text AS left_text,
                v.right_fact, rt.text AS right_text,
                coalesce(array_length(v.path, 1), 0) AS path_len,
                v.detected_at, v.detail,
                COALESCE((SELECT jsonb_agg(jsonb_build_object('id', x.id, 'text', pt.text)
                                           ORDER BY x.ord)
                            FROM unnest(v.path) WITH ORDINALITY AS x(id, ord)
                            JOIN triple pt ON pt.id = x.id), '[]'::jsonb) AS path,
                lf.valid_to IS NULL AS left_open,
                lf.confidence AS left_confidence,
                EXISTS (
                    SELECT 1 FROM entities e
                    JOIN entities x ON x.kb_id = e.kb_id AND x.id <> e.id
                                   AND x.merged_into IS NULL
                                   AND lower(x.canonical_name) = lower(e.canonical_name)
                    WHERE e.id IN (lf.subject_id, lf.object_id)
                ) AS same_name_peers
           FROM axiom_violations v
           JOIN triple l  ON l.id  = v.left_fact
           JOIN triple rt ON rt.id = v.right_fact
           JOIN facts lf ON lf.id = v.left_fact
          WHERE v.kb_id = $1 AND v.status = 'open'
          ORDER BY v.detected_at DESC
          LIMIT $2 OFFSET $3",
    )
    .bind(kb_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|r: ViolationRow| {
        let hint = if r.kind == "derived_contradiction" {
            hint_for(&r).map(String::from)
        } else {
            None
        };
        AxiomViolation {
            id: r.id,
            kind: r.kind,
            predicate: r.predicate,
            left_fact: r.left_fact,
            left_text: r.left_text,
            right_fact: r.right_fact,
            right_text: r.right_text,
            path_len: r.path_len,
            detected_at: r.detected_at,
            detail: r.detail,
            hint,
            path: serde_json::from_value(r.path).unwrap_or_default(),
        }
    })
    .collect())
}

#[derive(sqlx::FromRow)]
struct ViolationRow {
    id: Uuid,
    kind: String,
    predicate: Option<String>,
    left_fact: Uuid,
    left_text: String,
    right_fact: Uuid,
    right_text: String,
    path_len: i32,
    detected_at: chrono::DateTime<chrono::Utc>,
    detail: serde_json::Value,
    path: serde_json::Value,
    left_open: bool,
    left_confidence: f32,
    same_name_peers: bool,
}

/// The hints are ordered by the most common way things go wrong (0017 §2): the old assertion has
/// no end date written down, two entities share a name, extraction was unsure to begin with.
/// Only one at a time -- three of them side by side is the same as giving none
fn hint_for(r: &ViolationRow) -> Option<&'static str> {
    if r.left_open && r.detail.get("valid_from").is_some_and(|v| !v.is_null()) {
        Some("stale")
    } else if r.same_name_peers {
        Some("duplicate")
    } else if r.left_confidence < 0.75 {
        Some("unsure")
    } else {
        None
    }
}

/// A human rules on one violation.
///
/// **Three ways out, not two.** A temporal conflict asks "which one is right", whereas here the
/// definition may be what is wrong -- the ontology the user imported declares some property
/// asymmetric, while in their own corpus that relation actually runs both ways.
/// `axiom_relaxed` records exactly that case: what needs changing is the ontology, not twenty
/// facts.
///
/// The status changes and the row is not deleted, the same rule as the ledger: the act of ruling
/// has to leave a trace, and `run` relies on `status = 'open'` to tell which rows are derived
/// and may be recomputed away -- a human's decision has to survive a re-run.
/// Which fact to retract within one violation.
///
/// The single-fact kinds (self-loop, signature, a derivation clashing with an assertion) only
/// have the one, so there is nothing to say; the two-fact ones and the ones on a cycle need a
/// human to name it, and only from the handful the violation itself lists -- retracting an
/// unrelated fact is not a ruling, it is an operator error
pub fn pick_retraction(
    left: Uuid,
    right: Uuid,
    path: &[Uuid],
    requested: Option<Uuid>,
) -> Option<Uuid> {
    if left == right {
        return match requested {
            None => Some(left),
            Some(r) if r == left => Some(left),
            Some(_) => None,
        };
    }
    let r = requested?;
    (r == left || r == right || path.contains(&r)).then_some(r)
}

/// "The data is wrong": **actually retract that fact**, then mark the violation resolved (#202).
///
/// This used to change only `axiom_violations`, leaving the fact alive in the graph; a re-run
/// then hit the resolved row and did nothing, so the violation neither disappeared nor showed up
/// again. The retraction goes down the `reject_fact` path -- `invalidated_at`, the evidence
/// untouched, a trace left in the ledger. Returns the id of the fact that was retracted, which
/// the caller uses to write the audit record
pub async fn retract_from_violation(
    pool: &PgPool,
    kb_id: Uuid,
    violation_id: Uuid,
    requested: Option<Uuid>,
    actor: Uuid,
) -> AppResult<Uuid> {
    let row: Option<(Uuid, Uuid, Vec<Uuid>)> = sqlx::query_as(
        "SELECT left_fact, right_fact, path FROM axiom_violations
          WHERE id = $1 AND kb_id = $2 AND status = 'open'",
    )
    .bind(violation_id)
    .bind(kb_id)
    .fetch_optional(pool)
    .await?;
    let Some((left, right, path)) = row else {
        return Err(utopia_core::AppError::NotFound);
    };
    let Some(target) = pick_retraction(left, right, &path, requested) else {
        return Err(utopia_core::AppError::invalid(
            "fact_required",
            "this violation involves several facts; name which one to retract, from those it lists",
        ));
    };
    crate::graph::reject_fact(pool, kb_id, target).await?;
    decide(pool, kb_id, violation_id, "fact_retracted", actor).await?;
    Ok(target)
}

pub async fn decide(
    pool: &PgPool,
    kb_id: Uuid,
    violation_id: Uuid,
    resolution: &str,
    actor: Uuid,
) -> AppResult<()> {
    let res = sqlx::query(
        "UPDATE axiom_violations
            SET status = 'resolved', resolution = $3, decided_by = $4, decided_at = now()
          WHERE id = $2 AND kb_id = $1 AND status = 'open'",
    )
    .bind(kb_id)
    .bind(violation_id)
    .bind(resolution)
    .bind(actor)
    .execute(pool)
    .await?;
    if res.rows_affected() == 0 {
        return Err(utopia_core::AppError::NotFound);
    }
    Ok(())
}

// ===================== R0's other half: the ontology itself =====================

/// What the ontology self-consistency check produces.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OntologyReport {
    pub classes: usize,
    pub found: usize,
    pub inserted: usize,
    pub cleared: usize,
}

/// Measures the ontology itself: the axiom combinations on predicates, cycles in subClassOf,
/// unsatisfiable classes.
///
/// The same re-run rules as [`run`]: `open` is derived state and can be recomputed away,
/// `resolved` is a human's decision and not one row of it is touched.
pub async fn check_ontology(pool: &PgPool, kb_id: Uuid) -> AppResult<OntologyReport> {
    let ax = axioms(pool, kb_id).await?;
    let parents: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT child_id, parent_id FROM entity_type_parents p
                          JOIN entity_types t ON t.id = p.child_id
                         WHERE t.kb_id = $1",
    )
    .bind(kb_id)
    .fetch_all(pool)
    .await?;
    let disjoint: Vec<(Uuid, Uuid)> =
        sqlx::query_as("SELECT a_id, b_id FROM entity_type_disjoint WHERE kb_id = $1")
            .bind(kb_id)
            .fetch_all(pool)
            .await?;
    let classes: i64 = sqlx::query_scalar("SELECT count(*) FROM entity_types WHERE kb_id = $1")
        .bind(kb_id)
        .fetch_one(pool)
        .await?;

    let defects = utopia_reason::ontology::check_ontology(&ax, &parents, &disjoint);
    let mut report = OntologyReport {
        classes: classes as usize,
        found: defects.len(),
        ..Default::default()
    };

    let mut tx = pool.begin().await?;
    let mut fresh: Vec<Uuid> = Vec::with_capacity(defects.len());
    for d in &defects {
        let existing: Option<(Uuid,)> = sqlx::query_as(
            "SELECT id FROM ontology_defects
              WHERE kb_id = $1 AND kind = $2 AND subject = $3
                AND other IS NOT DISTINCT FROM $4",
        )
        .bind(kb_id)
        .bind(d.kind.as_str())
        .bind(d.subject)
        .bind(d.other)
        .fetch_optional(&mut *tx)
        .await?;
        let id = match existing {
            Some((id,)) => id,
            None => {
                let id = Uuid::now_v7();
                sqlx::query(
                    "INSERT INTO ontology_defects (id, kb_id, kind, subject, other, path)
                     VALUES ($1, $2, $3, $4, $5, $6)",
                )
                .bind(id)
                .bind(kb_id)
                .bind(d.kind.as_str())
                .bind(d.subject)
                .bind(d.other)
                .bind(&d.path)
                .execute(&mut *tx)
                .await?;
                report.inserted += 1;
                id
            }
        };
        fresh.push(id);
    }
    let cleared = sqlx::query(
        "DELETE FROM ontology_defects
          WHERE kb_id = $1 AND status = 'open' AND kind <> 'rules_disagree'
            AND NOT (id = ANY($2))",
    )
    .bind(kb_id)
    .bind(&fresh)
    .execute(&mut *tx)
    .await?;
    report.cleared = cleared.rows_affected() as usize;
    tx.commit().await?;
    Ok(report)
}

// ===================== R1: materialising inference =====================

/// What one inference run produces.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DeriveReport {
    /// The number of rules that were compiled. **When this is zero the conclusion is "there are
    /// no rules", not "nothing can be inferred"**
    pub rules: usize,
    pub edges: usize,
    /// The total number of derivations computed this round
    pub derived: usize,
    /// The ones newly written to the database
    pub inserted: usize,
    /// The ones whose premises are gone and that were invalidated along with them
    pub invalidated: usize,
    /// The number of predicates that hit the per-predicate cap and so were not inferred to
    /// completion
    pub capped: usize,
    /// **The number that were inferred but had no matching rule row. In normal operation this
    /// should always be zero.**
    ///
    /// Non-zero means rule compilation and inference have stopped agreeing. This used to be a
    /// bare `continue`, so the `works_at` fact inferred from `ceo_of ⊑ works_at` **was inferred
    /// and then never written to the database**, while the `employs` that downstream inferred
    /// from it did get in -- a derivation's premise vanished into thin air. Count it, and do not
    /// let it go silent one more time
    pub unruled: usize,
    /// Inferred but clashing with an assertion or with another derivation, and so blocked from
    /// landing this round (0017). **Every one that was blocked has a matching row in Review** --
    /// `run` and this code compute it with the same function
    pub blocked: usize,
}

/// One fetch, three things: the edges with their intervals, each fact's interval, and the
/// precision plus confidence.
/// Shared by `run` and `materialize` -- the edges the two of them see have to be the same batch
type TimedEdges = (
    Vec<TimedEdge>,
    HashMap<Uuid, (Option<i64>, Option<i64>)>,
    HashMap<Uuid, (Option<String>, Option<String>, f32)>,
);

async fn timed_edges(pool: &PgPool, kb_id: Uuid) -> AppResult<TimedEdges> {
    // The input is **assertions only**. Derivations live in another table, so there is not even
    // a filter to write here -- that is exactly what splitting the tables bought: forgetting to
    // exclude them means nothing gets inferred, not that the output gets fed back into itself
    let rows: Vec<EdgeRow> = sqlx::query_as(
        "SELECT id, predicate_id, subject_id, object_id,
                valid_from, valid_to, valid_from_precision, valid_to_precision, confidence
           FROM facts
          WHERE kb_id = $1
            AND invalidated_at IS NULL
            AND predicate_id IS NOT NULL
            AND object_id IS NOT NULL",
    )
    .bind(kb_id)
    .fetch_all(pool)
    .await?;

    let mut edges = Vec::with_capacity(rows.len());
    let mut meta: HashMap<Uuid, (Option<String>, Option<String>, f32)> = HashMap::new();
    let mut spans: HashMap<Uuid, (Option<i64>, Option<i64>)> = HashMap::new();
    for (id, pred, subj, obj, from, to, fp, tp, conf) in rows {
        let (f, t) = (from.map(|x| x.timestamp()), to.map(|x| x.timestamp()));
        edges.push(TimedEdge {
            edge: Edge {
                fact: id,
                predicate: pred,
                subject: subj,
                object: obj,
            },
            from: f,
            to: t,
        });
        spans.insert(id, (f, t));
        meta.insert(id, (fp, tp, conf));
    }
    Ok((edges, spans, meta))
}

/// The (derived triple, assertion) pairs a human accepted as coexisting: these derivations land
/// as usual next round (0017 §2).
async fn accepted_clashes(
    pool: &PgPool,
    kb_id: Uuid,
) -> AppResult<HashSet<(Uuid, Uuid, Uuid, Uuid)>> {
    let rows: Vec<(Uuid, serde_json::Value)> = sqlx::query_as(
        "SELECT left_fact, detail FROM axiom_violations
          WHERE kb_id = $1 AND kind = 'derived_contradiction' AND resolution = 'accepted'",
    )
    .bind(kb_id)
    .fetch_all(pool)
    .await?;
    let id = |v: &serde_json::Value, k: &str| {
        v.get(k)
            .and_then(|x| x.as_str())
            .and_then(|s| s.parse::<Uuid>().ok())
    };
    Ok(rows
        .into_iter()
        .filter_map(|(against, d)| {
            Some((
                id(&d, "subject_id")?,
                id(&d, "predicate_id")?,
                id(&d, "object_id")?,
                against,
            ))
        })
        .collect())
}

/// The identity of a derived fact: the triple + the interval.
///
/// **Putting the interval into the key** is deliberate: a changed interval is a different
/// assertion, so the old one is invalidated and the new one lands, because the ledger does not
/// allow editing in place.
type DerivedKey = (Uuid, Uuid, Uuid, Option<i64>, Option<i64>);

/// The precision is taken as "the coarsest of them".
///
/// The two ends of a derived interval each come from one of the premises, and strictly speaking
/// each should follow its own precision. Taking the coarsest is **deliberately conservative**: a
/// chain is only as trustworthy as its least certain link, and labelling a conclusion inferred
/// from year-level premises as day is precisely what the comment on
/// `facts.valid_from_precision` calls "filling in a definite value where we are ignorant".
fn coarsest(a: Option<&str>, b: Option<&str>) -> Option<String> {
    let rank = |p: &str| match p {
        "year" => 0,
        "month" => 1,
        _ => 2,
    };
    match (a, b) {
        (Some(x), Some(y)) => Some(if rank(x) <= rank(y) { x } else { y }.to_string()),
        (Some(x), None) | (None, Some(x)) => Some(x.to_string()),
        (None, None) => None,
    }
}

/// Recompiles the rules from the ontology axioms, returning `(predicate, kind) → rule id`.
///
/// **Idempotent**: the identity is `(kb, predicate, kind)`, so a recompile recognises "it is
/// still the same rule" -- otherwise every run would point `derived_facts.rule_id` at a new id
/// and the whole history would be severed.
///
/// **A rule whose axiom was withdrawn is not deleted.** Invalidated derived rows still point at
/// it, and explaining "which rule it was inferred by at the time" needs it to still be there;
/// it simply stops appearing in the return value, and the facts inferred from it are invalidated
/// by the reconciliation below. There are only a few rules per knowledge base, so keeping them
/// takes up no space.
async fn compile_rules(
    pool: &PgPool,
    kb_id: Uuid,
    ax: &HashMap<Uuid, Axioms>,
) -> AppResult<HashMap<(Uuid, RuleKind), Uuid>> {
    let mut want: Vec<(Uuid, RuleKind)> = Vec::new();
    for (&pred, a) in ax {
        if a.transitive {
            want.push((pred, "transitive"));
        }
        if a.symmetric {
            want.push((pred, "symmetric"));
        }
        // The last two are rule sources added by migration 0016
        // (a_relation_can_name_its_inverse). **A rule hangs on "the side that has the
        // declaration"** -- a normalised inverse has a declaration on both sides, so each
        // direction gets a rule of its own, which lines up with the derivations each one infers
        if a.inverse_of.is_some() {
            want.push((pred, "inverse"));
        }
        if a.sub_property_of.is_some() {
            want.push((pred, "sub_property"));
        }
    }
    want.sort();
    let mut out = HashMap::new();
    for (pred, kind) in want {
        sqlx::query(
            "INSERT INTO rules (id, kb_id, predicate_id, kind) VALUES ($1, $2, $3, $4)
             ON CONFLICT (kb_id, predicate_id, kind) DO NOTHING",
        )
        .bind(Uuid::now_v7())
        .bind(kb_id)
        .bind(pred)
        .bind(kind)
        .execute(pool)
        .await?;
        let (id,): (Uuid,) = sqlx::query_as(
            "SELECT id FROM rules WHERE kb_id = $1 AND predicate_id = $2 AND kind = $3",
        )
        .bind(kb_id)
        .bind(pred)
        .bind(kind)
        .fetch_one(pool)
        .await?;
        out.insert((pred, kind), id);
    }
    Ok(out)
}

/// The information carried back along with the edges when they are fetched (precision and
/// confidence, needed when writing to the database).
type EdgeRow = (
    Uuid,
    Uuid,
    Uuid,
    Uuid,
    Option<chrono::DateTime<chrono::Utc>>,
    Option<chrono::DateTime<chrono::Utc>>,
    Option<String>,
    Option<String>,
    f32,
);

type LiveRow = (
    Uuid,
    Uuid,
    Uuid,
    Uuid,
    Option<chrono::DateTime<chrono::Utc>>,
    Option<chrono::DateTime<chrono::Utc>>,
);

/// Runs inference once and writes the derived facts into the ledger.
///
/// **The caller is responsible for checking the `materialize_inferences` switch.** This layer
/// does not judge -- it is also used by the "preview what would be inferred" path, and a preview
/// should not be constrained by the switch.
pub async fn materialize(pool: &PgPool, kb_id: Uuid) -> AppResult<DeriveReport> {
    let ax = axioms(pool, kb_id).await?;
    let rules = compile_rules(pool, kb_id, &ax).await?;
    let (edges, spans, meta) = timed_edges(pool, kb_id).await?;

    let derivation = utopia_reason::derive::derive(&edges, &ax);
    // asserted > derived is hard (0002): a derivation that clashes with an assertion does not
    // land. Except the ones a human accepted as coexisting; when derivations clash with each
    // other neither side lands, and acceptance only affects whether it is reported (0017)
    let clashes = utopia_reason::derive::contradictions(&derivation, &edges, &ax, &spans);
    let accepted = accepted_clashes(pool, kb_id).await?;
    let mut blocked: HashSet<usize> = HashSet::new();
    for c in &clashes.with_assertions {
        let d = &derivation.facts[c.derived];
        if !accepted.contains(&(d.subject, d.predicate, d.object, c.against)) {
            blocked.insert(c.derived);
        }
    }
    for rc in &clashes.between_derivations {
        for (i, j) in &rc.pairs {
            blocked.insert(*i);
            blocked.insert(*j);
        }
    }
    let mut report = DeriveReport {
        rules: rules.len(),
        edges: edges.len(),
        derived: derivation.facts.len(),
        capped: derivation.capped.len(),
        blocked: blocked.len(),
        ..Default::default()
    };

    let mut wanted: HashMap<DerivedKey, &utopia_reason::derive::Derived> = HashMap::new();
    for (i, d) in derivation.facts.iter().enumerate() {
        if blocked.contains(&i) {
            continue;
        }
        let Some((from, to)) = utopia_reason::derive::validity(&d.premises, &spans) else {
            continue;
        };
        wanted.insert((d.subject, d.predicate, d.object, from, to), d);
    }

    let mut tx = pool.begin().await?;
    let live: Vec<LiveRow> = sqlx::query_as(
        "SELECT id, subject_id, predicate_id, object_id, valid_from, valid_to
           FROM derived_facts
          WHERE kb_id = $1 AND invalidated_at IS NULL",
    )
    .bind(kb_id)
    .fetch_all(&mut *tx)
    .await?;

    let mut stale: Vec<Uuid> = Vec::new();
    for (id, s, p, o, from, to) in &live {
        let key = (
            *s,
            *p,
            *o,
            from.map(|x| x.timestamp()),
            to.map(|x| x.timestamp()),
        );
        if wanted.remove(&key).is_none() {
            stale.push(*id);
        }
    }

    // Premises gone → the derivation is invalidated with them. **Set invalidated_at rather than
    // delete**: exactly the same shape as rejecting a fact, it leaves "we once inferred this on
    // that basis, and later the premise went away" on the record-time axis, which the entity history
    // page can display directly (0002 section 3)
    if !stale.is_empty() {
        sqlx::query("UPDATE derived_facts SET invalidated_at = now() WHERE id = ANY($1)")
            .bind(&stale)
            .execute(&mut *tx)
            .await?;
        report.invalidated = stale.len();
    }

    for ((subject, predicate, object, from, to), d) in wanted {
        // **Look it up by `via`, not by `predicate`.** A rule row is compiled for "the predicate
        // that declares the axiom"; in the two cross-predicate rules, the predicate that comes
        // out of the derivation is a different one. It used to look up by predicate, which was
        // always right for the first two rules (there the two are the same), and once inverse /
        // sub_property were added, not finding a rule meant a `continue` -- inferred and then
        // never written to the database
        let Some(&rule_id) = rules.get(&(d.via, d.rule.as_str())) else {
            // Not finding the rule means **compilation and inference disagree**, which is not a
            // normal situation. Count it, and do not let it silently vanish one more time
            report.unruled += 1;
            continue;
        };
        // Both precision and confidence take the most conservative value among the premises
        let mut fp: Option<String> = None;
        let mut tp: Option<String> = None;
        let mut conf = 1.0f32;
        for p in &d.premises {
            if let Some((pf, pt, pc)) = meta.get(p) {
                fp = coarsest(fp.as_deref(), pf.as_deref());
                tp = coarsest(tp.as_deref(), pt.as_deref());
                conf = conf.min(*pc);
            }
        }
        // The constraint is "precision only where there is a date" -- when the intersection
        // makes one end unbounded, the precision on that end has to be cleared too
        let fp = from.and(fp);
        let tp = to.and(tp);
        let id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO derived_facts (id, kb_id, subject_id, predicate_id, object_id,
                                        valid_from, valid_to,
                                        valid_from_precision, valid_to_precision,
                                        confidence, rule_id)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        )
        .bind(id)
        .bind(kb_id)
        .bind(subject)
        .bind(predicate)
        .bind(object)
        .bind(from.map(stamp))
        .bind(to.map(stamp))
        .bind(&fp)
        .bind(&tp)
        .bind(conf)
        .bind(rule_id)
        .execute(&mut *tx)
        .await?;
        for (seq, premise) in d.premises.iter().enumerate() {
            sqlx::query(
                "INSERT INTO fact_derivations (derived_fact_id, premise_fact_id, seq)
                 VALUES ($1, $2, $3)",
            )
            .bind(id)
            .bind(premise)
            .bind(seq as i32)
            .execute(&mut *tx)
            .await?;
        }
        report.inserted += 1;
    }
    tx.commit().await?;
    Ok(report)
}

fn stamp(secs: i64) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::from_timestamp(secs, 0).unwrap_or_default()
}

/// The ontology defects the Review page needs, together with their labels.
///
/// The labels are fetched in SQL rather than in a second round trip: that one `subject` column
/// points at two tables (a predicate or a class), so querying separately would mean grouping by
/// kind first and then issuing two batches of queries, while one LEFT JOIN against both tables
/// is enough -- an id can only ever hit one of them.
pub async fn open_defects(
    pool: &PgPool,
    kb_id: Uuid,
    limit: i64,
    offset: i64,
) -> AppResult<Vec<OntologyDefect>> {
    Ok(sqlx::query_as(
        "SELECT d.id, d.kind, d.detail,
                COALESCE(st.label, sr.label) AS subject_label,
                COALESCE(ot.label, orr.label) AS other_label,
                COALESCE(
                    (SELECT array_agg(t.label ORDER BY x.ord)
                       FROM unnest(d.path) WITH ORDINALITY AS x(id, ord)
                       JOIN entity_types t ON t.id = x.id),
                    ARRAY[]::text[]
                ) AS path_labels,
                d.detected_at
           FROM ontology_defects d
           LEFT JOIN entity_types   st ON st.id = d.subject
           LEFT JOIN relation_types sr ON sr.id = d.subject
           LEFT JOIN entity_types   ot ON ot.id = d.other
           LEFT JOIN relation_types orr ON orr.id = d.other
          WHERE d.kb_id = $1 AND d.status = 'open'
          ORDER BY d.detected_at DESC
          LIMIT $2 OFFSET $3",
    )
    .bind(kb_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await?)
}

/// A human rules on one ontology defect.
///
/// Two ways out rather than three: an ontology defect has no "the data is wrong" option -- it
/// never looked at the data at all. `fixed` means "I went and changed the ontology", `accepted`
/// means "looked at it, no change needed".
pub async fn decide_defect(
    pool: &PgPool,
    kb_id: Uuid,
    defect_id: Uuid,
    resolution: &str,
    actor: Uuid,
) -> AppResult<()> {
    let res = sqlx::query(
        "UPDATE ontology_defects
            SET status = 'resolved', resolution = $3, decided_by = $4, decided_at = now()
          WHERE id = $2 AND kb_id = $1 AND status = 'open'",
    )
    .bind(kb_id)
    .bind(defect_id)
    .bind(resolution)
    .bind(actor)
    .execute(pool)
    .await?;
    if res.rows_affected() == 0 {
        return Err(utopia_core::AppError::NotFound);
    }
    Ok(())
}

/// The knowledge bases that are due for another inference run.
///
/// The same shape as source syncing: an interval + a last-run time. **Never having run counts as
/// due** -- a knowledge base that just had the switch turned on should not have to wait a whole
/// period for its first run.
pub async fn due_for_inference(pool: &PgPool) -> AppResult<Vec<Uuid>> {
    Ok(sqlx::query_scalar(
        "SELECT id FROM knowledge_bases
          WHERE materialize_inferences
            AND (last_inference_at IS NULL
                 OR last_inference_at < now()
                    - make_interval(mins => inference_interval_minutes))",
    )
    .fetch_all(pool)
    .await?)
}

/// Records the time this round of inference finished.
///
/// **Record it as soon as a run finishes, even if nothing changed**: this column answers "did we
/// look last time", not "did we change anything last time". Without it, a knowledge base with no
/// changes gets swept up and recomputed every minute.
pub async fn mark_inference_ran(pool: &PgPool, kb_id: Uuid) -> AppResult<()> {
    sqlx::query("UPDATE knowledge_bases SET last_inference_at = now() WHERE id = $1")
        .bind(kb_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// The proof of one derived fact, expanded down to the original sentence (0002 R2).
///
/// `fact_derivations` only records the direct premises, and premises are always assertions, so
/// "recursive expansion" degenerates into a single chain here: derivation → the assertions in
/// `seq` order → the evidence for each assertion. The leaves are chunks, and in the UI you click
/// all the way through to the document. **A retracted premise is still listed, with a marker**:
/// the derivation is invalidated along with its premise, but "what it rested on at the time" has
/// to be readable, and that is exactly why the record-time axis exists.
///
/// Returns None when the derivation is already invalidated or does not exist -- not an error,
/// the UI collapses the section on that basis.
pub async fn proof(
    pool: &PgPool,
    kb_id: Uuid,
    derived_id: Uuid,
) -> AppResult<Option<utopia_core::models::Proof>> {
    let Some(derived) = derived_one(pool, kb_id, derived_id).await? else {
        return Ok(None);
    };
    let premises: Vec<Uuid> = sqlx::query_scalar(
        "SELECT premise_fact_id FROM fact_derivations WHERE derived_fact_id = $1 ORDER BY seq",
    )
    .bind(derived_id)
    .fetch_all(pool)
    .await?;
    let steps = steps_for(pool, &premises).await?;
    Ok(Some(utopia_core::models::Proof { derived, steps }))
}

/// Expands a list of premises into the steps of a proof: the triple, the interval, whether it
/// was retracted, the evidence.
///
/// Derivations that landed (`fact_derivations`) and ones that did not (`axiom_violations.path`)
/// both come through here -- premises are the same kind of thing, and there is no reason for the
/// proof chain to look two different ways
async fn steps_for(
    pool: &PgPool,
    premises: &[Uuid],
) -> AppResult<Vec<utopia_core::models::ProofStep>> {
    #[allow(clippy::type_complexity)]
    let rows: Vec<(
        i64,
        Uuid,
        Uuid,
        String,
        Option<Uuid>,
        Option<String>,
        Option<Uuid>,
        Option<String>,
        Option<chrono::DateTime<chrono::Utc>>,
        Option<chrono::DateTime<chrono::Utc>>,
        f32,
        bool,
    )> = sqlx::query_as(
        "SELECT x.ord - 1, f.id, f.subject_id, s.canonical_name,
                f.predicate_id, r.label, f.object_id, o.canonical_name,
                f.valid_from, f.valid_to, f.confidence,
                f.invalidated_at IS NOT NULL
           FROM unnest($1::uuid[]) WITH ORDINALITY AS x(id, ord)
           JOIN facts f ON f.id = x.id
           JOIN entities s ON s.id = f.subject_id
           LEFT JOIN relation_types r ON r.id = f.predicate_id
           LEFT JOIN entities o ON o.id = f.object_id
          ORDER BY x.ord",
    )
    .bind(premises)
    .fetch_all(pool)
    .await?;
    let mut steps = Vec::with_capacity(rows.len());
    for (
        seq,
        fact_id,
        subject_id,
        subject,
        predicate_id,
        predicate,
        object_id,
        object,
        valid_from,
        valid_to,
        confidence,
        retracted,
    ) in rows
    {
        // A chain is at most MAX_DEPTH steps, so fetching the evidence one by one is a countable
        // handful of round trips
        let evidence = crate::graph::fact_evidence(pool, fact_id).await?;
        steps.push(utopia_core::models::ProofStep {
            seq: seq as i32,
            fact_id,
            subject_id,
            subject,
            predicate_id,
            predicate,
            object_id,
            object,
            valid_from,
            valid_to,
            confidence,
            retracted,
            evidence,
        });
    }
    Ok(steps)
}

/// Among the derivations that did not land, the ones that involve this entity (0017 §3) -- the
/// "did not land" section of the panel's "inferred" tab.
pub async fn blocked_for_entity(
    pool: &PgPool,
    kb_id: Uuid,
    entity_id: Uuid,
) -> AppResult<Vec<utopia_core::models::BlockedDerivation>> {
    Ok(sqlx::query_as(
        "SELECT v.id AS violation_id,
                (v.detail->>'subject_id')::uuid AS subject_id,
                COALESCE(v.detail->>'subject', '?') AS subject,
                (v.detail->>'object_id')::uuid AS object_id,
                COALESCE(v.detail->>'object', '?') AS object,
                COALESCE(v.detail->>'predicate', '?') AS predicate,
                COALESCE(v.detail->>'rule', '?') AS rule,
                COALESCE(v.detail->>'via_label', '?') AS via_label,
                (v.detail->>'valid_from')::timestamptz AS valid_from,
                (v.detail->>'valid_to')::timestamptz AS valid_to,
                v.left_fact AS against_fact,
                s.canonical_name || ' · '
                  || COALESCE(r.label, fact_surface_predicate(f.id), '?') || ' · '
                  || COALESCE(o.canonical_name, '?') AS against_text,
                v.path AS premises
           FROM axiom_violations v
           JOIN facts f ON f.id = v.left_fact
           JOIN entities s ON s.id = f.subject_id
           LEFT JOIN relation_types r ON r.id = f.predicate_id
           LEFT JOIN entities o ON o.id = f.object_id
          WHERE v.kb_id = $1 AND v.kind = 'derived_contradiction' AND v.status = 'open'
            AND (v.detail->>'subject_id' = $2::text OR v.detail->>'object_id' = $2::text)
          ORDER BY v.detected_at DESC",
    )
    .bind(kb_id)
    .bind(entity_id)
    .fetch_all(pool)
    .await?)
}

/// The proof chain of a derivation that did not land: its premises are right there in the
/// violation's `path`. `None` when that violation cannot be found
pub async fn blocked_proof(
    pool: &PgPool,
    kb_id: Uuid,
    violation_id: Uuid,
) -> AppResult<Option<Vec<utopia_core::models::ProofStep>>> {
    let path: Option<(Vec<Uuid>,)> = sqlx::query_as(
        "SELECT path FROM axiom_violations
          WHERE id = $1 AND kb_id = $2 AND kind = 'derived_contradiction'",
    )
    .bind(violation_id)
    .bind(kb_id)
    .fetch_optional(pool)
    .await?;
    match path {
        None => Ok(None),
        Some((p,)) => Ok(Some(steps_for(pool, &p).await?)),
    }
}

/// Fetches one derivation by id (invalidated ones too: a proof has to stay reviewable).
async fn derived_one(
    pool: &PgPool,
    kb_id: Uuid,
    derived_id: Uuid,
) -> AppResult<Option<DerivedFactView>> {
    Ok(sqlx::query_as(
        "SELECT d.id,
                d.subject_id, s.canonical_name AS subject,
                d.object_id,  o.canonical_name AS object,
                r.label AS predicate,
                ru.kind AS rule,
                d.valid_from, d.valid_to, d.confidence, d.derived_at,
                COALESCE(
                    (SELECT array_agg(
                                ps.canonical_name || ' · '
                                || COALESCE(pr.label, '?') || ' · '
                                || COALESCE(po.canonical_name, '?')
                                ORDER BY fd.seq)
                       FROM fact_derivations fd
                       JOIN facts pf       ON pf.id = fd.premise_fact_id
                       JOIN entities ps    ON ps.id = pf.subject_id
                       LEFT JOIN relation_types pr ON pr.id = pf.predicate_id
                       LEFT JOIN entities po ON po.id = pf.object_id
                      WHERE fd.derived_fact_id = d.id),
                    ARRAY[]::text[]
                ) AS premises
           FROM derived_facts d
           JOIN entities s ON s.id = d.subject_id
           JOIN entities o ON o.id = d.object_id
           JOIN relation_types r ON r.id = d.predicate_id
           JOIN rules ru ON ru.id = d.rule_id
          WHERE d.kb_id = $1 AND d.id = $2",
    )
    .bind(kb_id)
    .bind(derived_id)
    .fetch_optional(pool)
    .await?)
}

/// One derived fact, with the text needed to display it and to prove it (the "inferred" tab of
/// the entity panel).
///
/// **The proof comes back with it**: the whole reason this tab exists is "this edge is not
/// something somebody said, it was inferred like this", and without the premises it looks no
/// different from an ordinary edge -- which is exactly the contamination users worry about.
pub async fn derived_for_entity(
    pool: &PgPool,
    kb_id: Uuid,
    entity_id: Uuid,
) -> AppResult<Vec<DerivedFactView>> {
    Ok(sqlx::query_as(
        "SELECT d.id,
                d.subject_id, s.canonical_name AS subject,
                d.object_id,  o.canonical_name AS object,
                r.label AS predicate,
                ru.kind AS rule,
                d.valid_from, d.valid_to, d.confidence, d.derived_at,
                COALESCE(
                    (SELECT array_agg(
                                ps.canonical_name || ' · '
                                || COALESCE(pr.label, '?') || ' · '
                                || COALESCE(po.canonical_name, '?')
                                ORDER BY fd.seq)
                       FROM fact_derivations fd
                       JOIN facts pf       ON pf.id = fd.premise_fact_id
                       JOIN entities ps    ON ps.id = pf.subject_id
                       LEFT JOIN relation_types pr ON pr.id = pf.predicate_id
                       LEFT JOIN entities po ON po.id = pf.object_id
                      WHERE fd.derived_fact_id = d.id),
                    ARRAY[]::text[]
                ) AS premises
           FROM derived_facts d
           JOIN entities s ON s.id = d.subject_id
           JOIN entities o ON o.id = d.object_id
           JOIN relation_types r ON r.id = d.predicate_id
           JOIN rules ru ON ru.id = d.rule_id
          WHERE d.kb_id = $1 AND d.invalidated_at IS NULL
            AND (d.subject_id = $2 OR d.object_id = $2)
          ORDER BY d.derived_at DESC",
    )
    .bind(kb_id)
    .bind(entity_id)
    .fetch_all(pool)
    .await?)
}
