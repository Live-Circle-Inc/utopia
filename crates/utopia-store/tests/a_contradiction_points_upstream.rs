//! 0017: when a derivation runs into an assertion, make the yielding visible instead of
//! silent.
//!
//! `ceo_of ⊑ works_at`, `works_at` functional. Mira `ceo_of` Acme derives Mira `works_at`
//! Acme, while the ledger says she `works_at` Globex. Four things are pinned down here:
//!
//! 1. **The derivation does not land, and there is a row in the queue.** `run` records a
//!    `derived_contradiction` whose left is the assertion that was hit, whose right is the
//!    last premise, and whose detail holds the derived triple; `materialize` blocks it.
//! 2. **Fix it and it lands.** Give the old assertion an end date, the derivation's interval
//!    no longer overlaps it, it lands on the next round, and the row in the queue clears
//!    along with it.
//! 3. **Accept it and it lands.** A human says both sides are right, and after `accepted` the
//!    derivation lands as usual while that row stays and is no longer reported.
//! 4. **Derivations clashing with each other are aggregated by rule pair.** Two ceos derive
//!    two mutually exclusive works_at, which becomes one `rules_disagree` row in
//!    `ontology_defects`, and neither derivation lands.
//!
//! Skipped rather than failed when there is no `UTOPIA_DATABASE_URL`. Builds and tears down
//! its own data, and never touches an existing KB.

use sqlx::PgPool;
use utopia_store::reasoning;
use uuid::Uuid;

struct Fixture {
    org: Uuid,
    user: Uuid,
    kb: Uuid,
    ceo_of: Uuid,
    works_at: Uuid,
    mira: Uuid,
    acme: Uuid,
    globex: Uuid,
    initech: Uuid,
}

async fn seed(pool: &PgPool) -> anyhow::Result<Fixture> {
    let (org, ws, kb, user) = (
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
    );
    let etype = Uuid::now_v7();
    let (ceo_of, works_at) = (Uuid::now_v7(), Uuid::now_v7());
    let (mira, acme, globex, initech) = (
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
    );

    sqlx::query("INSERT INTO organizations (id, name) VALUES ($1, 'contradiction-test')")
        .bind(org)
        .execute(pool)
        .await?;
    sqlx::query("INSERT INTO workspaces (id, org_id, name) VALUES ($1, $2, 'contradiction-test')")
        .bind(ws)
        .bind(org)
        .execute(pool)
        .await?;
    sqlx::query(
        "INSERT INTO users (id, org_id, email, display_name, password_hash)
         VALUES ($1, $2, $1 || '@contradiction.test', 'c', 'x')",
    )
    .bind(user)
    .bind(org)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO knowledge_bases (id, workspace_id, name, materialize_inferences)
         VALUES ($1, $2, 'contradiction-test', TRUE)",
    )
    .bind(kb)
    .bind(ws)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO entity_types (id, kb_id, key, label) VALUES ($1, $2, 'thing', 'Thing')",
    )
    .bind(etype)
    .bind(kb)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO relation_types (id, kb_id, key, label, functional)
         VALUES ($1, $2, 'works_at', 'works at', TRUE)",
    )
    .bind(works_at)
    .bind(kb)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO relation_types (id, kb_id, key, label, sub_property_of)
         VALUES ($1, $2, 'ceo_of', 'CEO of', $3)",
    )
    .bind(ceo_of)
    .bind(kb)
    .bind(works_at)
    .execute(pool)
    .await?;
    for (id, name) in [
        (mira, "Mira"),
        (acme, "Acme"),
        (globex, "Globex"),
        (initech, "Initech"),
    ] {
        sqlx::query(
            "INSERT INTO entities (id, kb_id, type_id, canonical_name) VALUES ($1, $2, $3, $4)",
        )
        .bind(id)
        .bind(kb)
        .bind(etype)
        .bind(name)
        .execute(pool)
        .await?;
    }
    Ok(Fixture {
        org,
        user,
        kb,
        ceo_of,
        works_at,
        mira,
        acme,
        globex,
        initech,
    })
}

async fn asserted(
    pool: &PgPool,
    f: &Fixture,
    subject: Uuid,
    predicate: Uuid,
    object: Uuid,
    from: Option<&str>,
) -> anyhow::Result<Uuid> {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO facts (id, kb_id, subject_id, predicate_id, object_id, confidence,
                            valid_from, valid_from_precision)
         VALUES ($1, $2, $3, $4, $5, 0.9, $6::timestamptz, CASE WHEN $6 IS NULL THEN NULL ELSE 'day' END)",
    )
    .bind(id)
    .bind(f.kb)
    .bind(subject)
    .bind(predicate)
    .bind(object)
    .bind(from)
    .execute(pool)
    .await?;
    Ok(id)
}

async fn live_derived(pool: &PgPool, f: &Fixture) -> anyhow::Result<Vec<(Uuid, Uuid)>> {
    Ok(sqlx::query_as(
        "SELECT subject_id, object_id FROM derived_facts
          WHERE kb_id = $1 AND invalidated_at IS NULL ORDER BY subject_id, object_id",
    )
    .bind(f.kb)
    .fetch_all(pool)
    .await?)
}

async fn open_contradictions(
    pool: &PgPool,
    f: &Fixture,
) -> anyhow::Result<Vec<(Uuid, Uuid, serde_json::Value)>> {
    Ok(sqlx::query_as(
        "SELECT id, left_fact, detail FROM axiom_violations
          WHERE kb_id = $1 AND kind = 'derived_contradiction' AND status = 'open'",
    )
    .bind(f.kb)
    .fetch_all(pool)
    .await?)
}

#[tokio::test]
async fn a_contradiction_points_upstream() -> anyhow::Result<()> {
    let Some(url) = utopia_store::test_db::url() else {
        return Ok(());
    };
    let pool = PgPool::connect(&url).await?;
    let f = seed(&pool).await?;

    let run = async {
        // Mira works_at Globex (no end date written); Mira ceo_of Acme from 2024 on
        let old = asserted(&pool, &f, f.mira, f.works_at, f.globex, Some("2020-01-01")).await?;
        let ceo = asserted(&pool, &f, f.mira, f.ceo_of, f.acme, Some("2024-01-01")).await?;

        // 1. The derivation does not land, and there is a row in the queue
        let m = reasoning::materialize(&pool, f.kb).await?;
        assert_eq!(m.derived, 1);
        assert_eq!(
            m.blocked, 1,
            "the derivation that hits an assertion stays out"
        );
        assert_eq!(m.inserted, 0);
        assert!(live_derived(&pool, &f).await?.is_empty());

        let r = reasoning::run(&pool, f.kb).await?;
        assert_eq!(r.contradictions, 1);
        assert_eq!(r.rules_disagree, 0);
        let rows = open_contradictions(&pool, &f).await?;
        assert_eq!(rows.len(), 1);
        let (vid, left, detail) = &rows[0];
        assert_eq!(*left, old, "left is the assertion that was hit");
        assert_eq!(detail["axiom"], "functional");
        assert_eq!(detail["rule"], "sub_property");
        assert_eq!(detail["subject"], "Mira");
        assert_eq!(detail["predicate"], "works at");
        assert_eq!(detail["object"], "Acme");
        assert_eq!(detail["via_label"], "CEO of");
        assert_eq!(detail["premises"][0], serde_json::json!(ceo));
        let (right,): (Uuid,) =
            sqlx::query_as("SELECT right_fact FROM axiom_violations WHERE id = $1")
                .bind(vid)
                .fetch_one(&pool)
                .await?;
        assert_eq!(right, ceo, "right is the last premise");

        // The clue Review gives: the old assertion has no end date and the derivation starts
        // later → stale
        let page = reasoning::open_violations(&pool, f.kb, 50, 0).await?;
        let card = page
            .iter()
            .find(|v| v.id == *vid)
            .expect("card on the page");
        assert_eq!(card.hint.as_deref(), Some("stale"));
        assert_eq!(card.detail["subject"], "Mira");

        // The dispute is visible where it sits (0017 §3): the panel row carries contested,
        // there is a ghost edge on the graph, the "did not land" section has a row, and its
        // proof chain can be read back to the premise
        let (_, facts) = utopia_store::graph::entity_detail(&pool, f.kb, f.mira).await?;
        let hit = facts
            .iter()
            .find(|x| x.id == old)
            .expect("the assertion is on the panel");
        let c = hit
            .contested
            .as_ref()
            .expect("the hit assertion is contested");
        assert_eq!(c["kind"], "derived_contradiction");
        assert_eq!(c["ref_id"], serde_json::json!(vid));
        assert!(
            facts
                .iter()
                .find(|x| x.id == ceo)
                .unwrap()
                .contested
                .is_none(),
            "the premise is not the disputed one"
        );
        let (_, edges) = utopia_store::graph::neighborhood(&pool, f.kb, f.mira, 1, None).await?;
        let ghost = edges
            .iter()
            .find(|e| e.blocked)
            .expect("a ghost edge for the blocked derivation");
        assert_eq!(ghost.id, *vid);
        assert!(ghost.derived && ghost.contested);
        assert_eq!((ghost.source, ghost.target), (f.mira, f.acme));
        assert!(edges.iter().find(|e| e.id == old).unwrap().contested);
        assert!(!edges.iter().find(|e| e.id == ceo).unwrap().contested);
        let blocked = reasoning::blocked_for_entity(&pool, f.kb, f.acme).await?;
        assert_eq!(blocked.len(), 1);
        assert_eq!(blocked[0].violation_id, *vid);
        assert_eq!(blocked[0].against_fact, old);
        assert_eq!(blocked[0].premises, vec![ceo]);
        let steps = reasoning::blocked_proof(&pool, f.kb, *vid)
            .await?
            .expect("the ghost has a proof");
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].fact_id, ceo);

        // Rerunning is idempotent: still that one row
        reasoning::run(&pool, f.kb).await?;
        assert_eq!(open_contradictions(&pool, &f).await?.len(), 1);

        // 2. Fix it and it lands: give the old assertion an end date, and the intervals no
        //    longer overlap
        sqlx::query(
            "UPDATE facts SET valid_to = '2023-06-30'::timestamptz, valid_to_precision = 'day'
              WHERE id = $1",
        )
        .bind(old)
        .execute(&pool)
        .await?;
        let m = reasoning::materialize(&pool, f.kb).await?;
        assert_eq!(m.blocked, 0);
        assert_eq!(
            m.inserted, 1,
            "once the assertion ends, the derivation lands"
        );
        assert_eq!(live_derived(&pool, &f).await?, vec![(f.mira, f.acme)]);
        reasoning::run(&pool, f.kb).await?;
        assert!(
            open_contradictions(&pool, &f).await?.is_empty(),
            "the queue row clears with the contradiction"
        );

        // 3. Accept it and it lands: take the end date away and the contradiction is back; a
        //    human says both sides are right, and the derivation lands as usual
        sqlx::query("UPDATE facts SET valid_to = NULL, valid_to_precision = NULL WHERE id = $1")
            .bind(old)
            .execute(&pool)
            .await?;
        let m = reasoning::materialize(&pool, f.kb).await?;
        assert_eq!(m.blocked, 1);
        assert_eq!(m.invalidated, 1, "the landed derivation is withdrawn again");
        reasoning::run(&pool, f.kb).await?;
        let rows = open_contradictions(&pool, &f).await?;
        assert_eq!(rows.len(), 1);
        reasoning::decide(&pool, f.kb, rows[0].0, "accepted", f.user).await?;
        let m = reasoning::materialize(&pool, f.kb).await?;
        assert_eq!(m.blocked, 0, "an accepted pair lands");
        assert_eq!(live_derived(&pool, &f).await?, vec![(f.mira, f.acme)]);
        let r = reasoning::run(&pool, f.kb).await?;
        assert_eq!(r.contradictions, 1, "still counted");
        assert!(
            open_contradictions(&pool, &f).await?.is_empty(),
            "but the accepted row stays resolved and nothing new is opened"
        );

        // 4. Derivations clashing with each other: first close the old assertion off so the
        //    assertion no longer takes part; then add another ceo_of Initech, and the two
        //    works_at are derived by the same rule and are mutually exclusive -- reported once
        //    per rule pair, and neither lands
        sqlx::query(
            "UPDATE facts SET valid_to = '2023-06-30'::timestamptz, valid_to_precision = 'day'
              WHERE id = $1",
        )
        .bind(old)
        .execute(&pool)
        .await?;
        let ceo2 = asserted(&pool, &f, f.mira, f.ceo_of, f.initech, Some("2024-01-01")).await?;
        let m = reasoning::materialize(&pool, f.kb).await?;
        assert_eq!(m.derived, 2);
        assert_eq!(m.blocked, 2, "both sides of a rule clash stay out");
        assert_eq!(m.invalidated, 1, "the one that had landed is withdrawn");
        assert!(live_derived(&pool, &f).await?.is_empty());
        let r = reasoning::run(&pool, f.kb).await?;
        assert_eq!(r.contradictions, 0);
        assert_eq!(r.rules_disagree, 1);
        let defects: Vec<(Uuid, Option<Uuid>, serde_json::Value)> = sqlx::query_as(
            "SELECT subject, other, detail FROM ontology_defects
              WHERE kb_id = $1 AND kind = 'rules_disagree' AND status = 'open'",
        )
        .bind(f.kb)
        .fetch_all(&pool)
        .await?;
        assert_eq!(defects.len(), 1);
        assert_eq!(defects[0].0, f.ceo_of);
        assert_eq!(defects[0].1, Some(f.ceo_of));
        assert_eq!(defects[0].2["count"], 1);
        assert_eq!(defects[0].2["rules"][0]["axiom"], "functional");
        assert_eq!(defects[0].2["rules"][0]["rule_a"], "sub_property");
        assert_eq!(defects[0].2["rules"][0]["via_a"], "CEO of");
        let page = reasoning::open_defects(&pool, f.kb, 50, 0).await?;
        let card = page
            .iter()
            .find(|d| d.kind == "rules_disagree")
            .expect("the rule clash is on the page");
        assert_eq!(card.subject_label.as_deref(), Some("CEO of"));
        assert_eq!(card.other_label.as_deref(), Some("CEO of"));

        // Withdraw the second ceo: the rule-pair row clears, and the first derivation lands
        // again
        sqlx::query("UPDATE facts SET invalidated_at = now() WHERE id = $1")
            .bind(ceo2)
            .execute(&pool)
            .await?;
        reasoning::run(&pool, f.kb).await?;
        let (n,): (i64,) = sqlx::query_as(
            "SELECT count(*) FROM ontology_defects
              WHERE kb_id = $1 AND kind = 'rules_disagree' AND status = 'open'",
        )
        .bind(f.kb)
        .fetch_one(&pool)
        .await?;
        assert_eq!(n, 0, "a rule clash clears when its derivations go");
        let m = reasoning::materialize(&pool, f.kb).await?;
        assert_eq!(m.blocked, 0);
        assert_eq!(live_derived(&pool, &f).await?, vec![(f.mira, f.acme)]);
        anyhow::Ok(())
    }
    .await;

    let _ = sqlx::query("DELETE FROM organizations WHERE id = $1")
        .bind(f.org)
        .execute(&pool)
        .await;
    run
}
