//! A relation's signature (domain / range) has to count on **all three write paths**
//! (#190 / #196).
//!
//! Extraction straightens the direction or leaves the predicate empty per #138 when it writes,
//! but more than one path writes a predicate: **adoption** hangs a predicate back onto an old
//! fact, and **merging** swaps out the subject's type. The guard was only installed on
//! extraction, and the other two each went around it -- measured, adoption pushed the violation
//! rate from 0 up to 12.3%. Three things are guarded here:
//!
//! 1. Adoption goes through the same judgement: subject does not fit but object does → hang it
//!    swapped; neither side fits → do not hang it, and the fact stays with an empty predicate.
//! 2. After a merge, if the fact whose subject was swapped breaks the signature → one more
//!    `signature` row in `axiom_violations`.
//! 3. The consistency check (R0) can measure signature violations too, and retracting the fact
//!    clears it.
//!
//! Skipped rather than failed when there is no `UTOPIA_DATABASE_URL`. It builds its own fixture
//! and tears it down, and never touches an existing database.

use sqlx::PgPool;
use utopia_store::graph::Adopted;
use uuid::Uuid;

struct Fixture {
    org: Uuid,
    kb: Uuid,
    company: Uuid,
    person: Uuid,
    /// schema.org's `employee (organization → person)`
    employee: Uuid,
    acme: Uuid,
    alice: Uuid,
    bob: Uuid,
    doc: Uuid,
    chunk: Uuid,
}

async fn seed(pool: &PgPool) -> anyhow::Result<Fixture> {
    let (org, ws, kb) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    let (company, person, employee) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    let (acme, alice, bob) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    let (src, doc, chunk) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());

    sqlx::query("INSERT INTO organizations (id, name) VALUES ($1, 'signature-test')")
        .bind(org)
        .execute(pool)
        .await?;
    sqlx::query("INSERT INTO workspaces (id, org_id, name) VALUES ($1, $2, 'signature-test')")
        .bind(ws)
        .bind(org)
        .execute(pool)
        .await?;
    sqlx::query(
        "INSERT INTO knowledge_bases (id, workspace_id, name) VALUES ($1, $2, 'signature-test')",
    )
    .bind(kb)
    .bind(ws)
    .execute(pool)
    .await?;
    for (id, key, label) in [
        (company, "company", "Company"),
        (person, "person", "Person"),
    ] {
        sqlx::query("INSERT INTO entity_types (id, kb_id, key, label) VALUES ($1, $2, $3, $4)")
            .bind(id)
            .bind(kb)
            .bind(key)
            .bind(label)
            .execute(pool)
            .await?;
    }
    sqlx::query(
        "INSERT INTO relation_types (id, kb_id, key, label) VALUES ($1, $2, 'employee', 'employee')",
    )
    .bind(employee)
    .bind(kb)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO relation_type_domains (relation_type_id, entity_type_id) VALUES ($1, $2)",
    )
    .bind(employee)
    .bind(company)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO relation_type_ranges (relation_type_id, entity_type_id) VALUES ($1, $2)",
    )
    .bind(employee)
    .bind(person)
    .execute(pool)
    .await?;
    for (id, ty, name) in [
        (acme, company, "Acme"),
        (alice, person, "Alice"),
        (bob, person, "Bob"),
    ] {
        sqlx::query(
            "INSERT INTO entities (id, kb_id, type_id, canonical_name) VALUES ($1, $2, $3, $4)",
        )
        .bind(id)
        .bind(kb)
        .bind(ty)
        .bind(name)
        .execute(pool)
        .await?;
    }
    sqlx::query("INSERT INTO sources (id, kb_id, name) VALUES ($1, $2, 'signature-test')")
        .bind(src)
        .bind(kb)
        .execute(pool)
        .await?;
    sqlx::query(
        "INSERT INTO documents (id, kb_id, source_id, filename, sha256, status)
         VALUES ($1, $2, $3, 'note.md', 'signature', 'ready')",
    )
    .bind(doc)
    .bind(kb)
    .bind(src)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO chunks (id, kb_id, document_id, seq, text)
         VALUES ($1, $2, $3, 0, 'Alice is an employee of Acme. Bob is an employee of Alice.')",
    )
    .bind(chunk)
    .bind(kb)
    .bind(doc)
    .execute(pool)
    .await?;
    Ok(Fixture {
        org,
        kb,
        company,
        person,
        employee,
        acme,
        alice,
        bob,
        doc,
        chunk,
    })
}

/// A fact with no predicate whose evidence keeps the original wording `employee` -- exactly
/// the kind adoption is meant to rewrite
async fn surfaced_fact(
    pool: &PgPool,
    f: &Fixture,
    subject: Uuid,
    object: Uuid,
) -> anyhow::Result<Uuid> {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO facts (id, kb_id, subject_id, predicate_id, object_id, confidence)
         VALUES ($1, $2, $3, NULL, $4, 0.9)",
    )
    .bind(id)
    .bind(f.kb)
    .bind(subject)
    .bind(object)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO fact_evidence (fact_id, chunk_id, quote, proposed_predicate, document_id, doc_version)
         VALUES ($1, $2, 'employee of', 'employee', $3, 1)",
    )
    .bind(id)
    .bind(f.chunk)
    .bind(f.doc)
    .execute(pool)
    .await?;
    Ok(id)
}

async fn live_employee_edges(pool: &PgPool, f: &Fixture) -> anyhow::Result<Vec<(Uuid, Uuid)>> {
    Ok(sqlx::query_as(
        "SELECT subject_id, object_id FROM facts
          WHERE kb_id = $1 AND predicate_id = $2 AND invalidated_at IS NULL
          ORDER BY recorded_at",
    )
    .bind(f.kb)
    .bind(f.employee)
    .fetch_all(pool)
    .await?)
}

async fn open_signature_breaks(pool: &PgPool, kb: Uuid) -> anyhow::Result<Vec<Uuid>> {
    Ok(sqlx::query_as::<_, (Uuid,)>(
        "SELECT left_fact FROM axiom_violations
          WHERE kb_id = $1 AND kind = 'signature' AND status = 'open'",
    )
    .bind(kb)
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|(id,)| id)
    .collect())
}

#[tokio::test]
async fn adoption_and_merge_respect_the_signature() -> anyhow::Result<()> {
    let Some(url) = utopia_store::test_db::url() else {
        return Ok(());
    };
    let pool = PgPool::connect(&url).await?;
    let f = seed(&pool).await?;

    let run = async {
        // The model wrote "Alice is an employee of Acme" -- the person subject breaks the
        // domain, the company object fits
        let reversed = surfaced_fact(&pool, &f, f.alice, f.acme).await?;
        // "Bob is an employee of Alice" -- both sides are person, so this relation simply does
        // not apply
        let hopeless = surfaced_fact(&pool, &f, f.bob, f.alice).await?;

        // 1. Adoption: one gets hung on swapped, the other does not get hung at all
        let Adopted {
            moved, left_off, ..
        } = utopia_store::graph::adopt_proposed_predicates(
            &pool,
            f.kb,
            f.employee,
            &["employee".to_string()],
            false,
        )
        .await?;
        assert_eq!(moved, 1, "the reversed one is adoptable once swapped");
        assert_eq!(left_off, 1, "the hopeless one must be left without a predicate");
        assert_eq!(
            live_employee_edges(&pool, &f).await?,
            vec![(f.acme, f.alice)],
            "adoption must write the edge in the ontology's direction: Acme employee Alice"
        );
        let (still_bare,): (bool,) =
            sqlx::query_as("SELECT predicate_id IS NULL AND invalidated_at IS NULL FROM facts WHERE id = $1")
                .bind(hopeless)
                .fetch_one(&pool)
                .await?;
        assert!(still_bare, "a fact that fits neither way stays live and predicate-less");
        let (old_gone,): (bool,) =
            sqlx::query_as("SELECT invalidated_at IS NOT NULL FROM facts WHERE id = $1")
                .bind(reversed)
                .fetch_one(&pool)
                .await?;
        assert!(old_gone, "the reversed row was superseded, not left beside the corrected one");
        assert!(open_signature_breaks(&pool, f.kb).await?.is_empty());

        // 2. Merge: fold Acme into Bob (a person), so the subject of Acme employee Alice
        //    becomes a person
        utopia_store::resolution::merge_entities(&pool, f.kb, f.acme, f.bob, None, "test")
            .await?;
        let edges = live_employee_edges(&pool, &f).await?;
        assert_eq!(edges, vec![(f.bob, f.alice)], "the merge moved the subject onto Bob");
        let (moved_fact,): (Uuid,) = sqlx::query_as(
            "SELECT id FROM facts WHERE kb_id = $1 AND predicate_id = $2 AND invalidated_at IS NULL",
        )
        .bind(f.kb)
        .bind(f.employee)
        .fetch_one(&pool)
        .await?;
        assert_eq!(
            open_signature_breaks(&pool, f.kb).await?,
            vec![moved_fact],
            "a merge that breaks the signature must show up as an open violation"
        );

        // 3. The consistency check measures it, and retracting the fact clears it
        let report = utopia_store::reasoning::run(&pool, f.kb).await?;
        assert_eq!(report.found, 1);
        assert_eq!(report.inserted, 0, "already recorded by the merge; the check must not duplicate it");
        sqlx::query("UPDATE facts SET invalidated_at = now() WHERE id = $1")
            .bind(moved_fact)
            .execute(&pool)
            .await?;
        let report = utopia_store::reasoning::run(&pool, f.kb).await?;
        assert_eq!(report.found, 0);
        assert_eq!(report.cleared, 1, "a retracted fact takes its violation with it");
        assert!(open_signature_breaks(&pool, f.kb).await?.is_empty());

        // An unclassified entity is not a violation: a subject whose type is NULL has nothing
        // to compare against
        let untyped = Uuid::now_v7();
        sqlx::query("INSERT INTO entities (id, kb_id, canonical_name) VALUES ($1, $2, 'Nobody')")
            .bind(untyped)
            .bind(f.kb)
            .execute(&pool)
            .await?;
        sqlx::query(
            "INSERT INTO facts (id, kb_id, subject_id, predicate_id, object_id, confidence)
             VALUES ($1, $2, $3, $4, $5, 0.9)",
        )
        .bind(Uuid::now_v7())
        .bind(f.kb)
        .bind(untyped)
        .bind(f.employee)
        .bind(f.alice)
        .execute(&pool)
        .await?;
        assert!(
            utopia_store::reasoning::signature_breaks(&pool, f.kb, None).await?.is_empty(),
            "an untyped subject is unknown, not wrong"
        );
        let _ = (f.company, f.person);
        anyhow::Ok(())
    }
    .await;

    let _ = sqlx::query("DELETE FROM organizations WHERE id = $1")
        .bind(f.org)
        .execute(&pool)
        .await;
    run
}
