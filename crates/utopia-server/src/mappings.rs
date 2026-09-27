//! Agentic exploration of the semantic mappings behind ask-your-data: read the schema of the
//! mounted sources plus the KB's existing concepts, and let the LLM propose mappings of
//! "business concept (Metric/Dimension entity) -> data asset definition".
//!
//! Proposals are written to `concept_mappings` (status = proposed) -> they form their own queue
//! on the Review page, and after Confirm / Reject the status becomes confirmed; ask-your-data
//! reads only the confirmed ones.
//!
//! **It used to be a `mapped_to` fact with confidence 0.6**, showing up in the "low-confidence
//! facts" queue. The reasoning for moving it out is in 0011: it is not an assertion about the
//! world, it is configuration -- and back then "confirming" it meant
//! `UPDATE facts SET confidence = 1.0`, an in-place edit of a table that must not be edited in
//! place. The agent only proposes; the power to put a definition into effect stays with a human
//! -- the same philosophy as resolution's "rather split than merge".

use crate::llm_util;
use crate::state::AppState;
use uuid::Uuid;

const MAX_SCHEMA_CHARS: usize = 12_000;

/// Exploration turns the quantities and dimensions in a schema into Metric / Dimension
/// entities, and neither of those classes is in any built-in ontology pack -- since 0009 a new
/// KB no longer ships with classes. Without them the `type_id` lookup below finds nothing,
/// every proposal gets swallowed by `continue`, and the page says "queued" and then goes silent
/// forever (#223).
/// So the two classes are created before exploring: builtin, with descriptions for the
/// extraction prompt, and editable on the ontology page
async fn ensure_concept_types(pool: &sqlx::PgPool, kb_id: Uuid) -> anyhow::Result<()> {
    for (key, label, description) in [
        (
            "metric",
            "Metric",
            "An aggregatable business quantity (revenue, order count, average ticket) that              maps to a definition in a mounted database.",
        ),
        (
            "dimension",
            "Dimension",
            "A group-by attribute (region, month, product line) that maps to a column in a              mounted database.",
        ),
    ] {
        sqlx::query(
            "INSERT INTO entity_types (id, kb_id, key, label, builtin, description)
             SELECT $1, $2, $3, $4, TRUE, $5
             WHERE NOT EXISTS (SELECT 1 FROM entity_types WHERE kb_id = $2 AND key = $3)",
        )
        .bind(Uuid::now_v7())
        .bind(kb_id)
        .bind(key)
        .bind(label)
        .bind(description)
        .execute(pool)
        .await?;
    }
    Ok(())
}

pub async fn explore_mappings(state: &AppState, kb_id: Uuid) -> anyhow::Result<()> {
    let kb = utopia_store::kbs::get(&state.pool, kb_id).await?;
    let settings = utopia_store::settings::get(&state.pool, kb.workspace_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("Chat model not configured"))?;
    let client = llm_util::chat_client(&settings)
        .ok_or_else(|| anyhow::anyhow!("Chat model not configured"))?;

    let sources = utopia_store::datasources::mounted(&state.pool, kb_id).await?;
    if sources.is_empty() {
        anyhow::bail!("No data sources mounted");
    }
    ensure_concept_types(&state.pool, kb_id).await?;

    // Each source's schema (read straight from the engine so it is fresh; capped so the prompt
    // cannot explode)
    let mut schema_txt = String::new();
    for ds in &sources {
        let (engine, conn) = utopia_store::datasources::engine_and_conn(&state.pool, ds.id).await?;
        let cols = crate::query_engine::engine_for(&engine, &conn)?
            .fetch_schema()
            .await?;
        schema_txt.push_str(&format!("\n=== source: {} ===\n", ds.name));
        let mut current = String::new();
        for c in cols {
            let key = format!("{}.{}", c.schema, c.table);
            if key != current {
                current = key.clone();
                schema_txt.push_str(&format!("table {key}:\n"));
            }
            schema_txt.push_str(&format!(
                "  {} {}{}\n",
                c.column,
                c.data_type,
                c.comment.map(|x| format!(" -- {x}")).unwrap_or_default()
            ));
            if schema_txt.len() > MAX_SCHEMA_CHARS {
                schema_txt.push_str("(truncated)\n");
                break;
            }
        }
    }

    // Existing concepts (offered for reuse, so the same thing does not get a second name)
    let existing: Vec<(String,)> = sqlx::query_as(
        "SELECT e.canonical_name FROM entities e
         JOIN entity_types t ON t.id = e.type_id
         WHERE e.kb_id = $1 AND e.merged_into IS NULL AND t.key IN ('metric','dimension')
         ORDER BY e.canonical_name LIMIT 100",
    )
    .bind(kb_id)
    .fetch_all(&state.pool)
    .await?;
    let existing_names: Vec<String> = existing.into_iter().map(|(n,)| n).collect();

    let prompt = format!(
        "You are building the semantic layer of a BI system. Given database schemas, propose \
         business concepts a user would ask about, each mapped to a concrete definition.\n\
         Existing concepts (reuse these names when the meaning matches): {}\n\
         Schemas:\n{}\n\
         Reply with ONLY a JSON array, each item:\n\
         {{\"name\": \"business concept name\", \"kind\": \"metric\"|\"dimension\", \
         \"source\": \"data source name\", \
         \"definition\": {{\"table\": \"schema.table\", \"expr\": \"SQL expression\", \
         \"sql\": \"full SELECT if joins are needed (optional)\", \"unit\": \"optional\"}}, \
         \"summary\": \"one line: source + expression, shown to reviewers\", \
         \"rationale\": \"why this mapping, citing column comments\"}}\n\
         Metrics are aggregatable quantities (use sum/count/avg in expr); dimensions are \
         group-by columns. Propose at most 12, only well-grounded ones.",
        if existing_names.is_empty() {
            "(none)".into()
        } else {
            existing_names.join(", ")
        },
        schema_txt
    );

    let _permit = llm_util::acquire_chat(state, &settings).await;
    let reply = client
        .chat(&[utopia_llm::ChatMessage {
            role: "user".into(),
            content: prompt,
        }])
        .await?;
    let json_str = reply
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    let proposals: Vec<serde_json::Value> = serde_json::from_str(json_str)
        .map_err(|e| anyhow::anyhow!("Mapping proposal parse error: {e}"))?;

    let source_names: Vec<&str> = sources.iter().map(|d| d.name.as_str()).collect();
    let mut accepted = 0usize;
    for p in proposals.iter().take(12) {
        let name = p["name"].as_str().map(str::trim).unwrap_or("");
        let kind = p["kind"].as_str().unwrap_or("");
        let source = p["source"].as_str().map(str::trim).unwrap_or("");
        if name.is_empty()
            || !matches!(kind, "metric" | "dimension")
            || !source_names.iter().any(|s| s.eq_ignore_ascii_case(source))
        {
            continue;
        }
        let type_id: Option<(Uuid,)> =
            sqlx::query_as("SELECT id FROM entity_types WHERE kb_id = $1 AND key = $2")
                .bind(kb_id)
                .bind(kind)
                .fetch_optional(&state.pool)
                .await?;
        let Some((type_id,)) = type_id else { continue };

        // Concept entities go through resolution (same name merges; with no vector context it
        // merges the v1-compatible way)
        let resolved = utopia_store::resolution::resolve_mention(
            &state.pool,
            kb_id,
            Some(type_id),
            name,
            None,
        )
        .await?;

        // The definition is split into columns in `concept_mappings` (0011). It used to be a
        // blob of JSON stuffed into `object_value`, with the object hanging off a relation
        // called mapped_to -- and that relation was a row in the ontology, sitting next to
        // works_at. **It is not an assertion about the world, it is configuration**, so it
        // moved to a table of its own
        let def = &p["definition"];
        if !def.is_object() {
            continue;
        }
        let s = |k: &str| {
            def[k]
                .as_str()
                .filter(|x| !x.is_empty())
                .map(str::to_string)
        };
        utopia_store::mappings::propose(
            &state.pool,
            kb_id,
            resolved.entity_id,
            source,
            s("table").as_deref(),
            s("expr").as_deref(),
            s("sql").as_deref(),
            s("unit").as_deref(),
            // summary is the one line a human reads: both the Review list and the
            // ask-your-data prompt lean on it
            p["summary"].as_str().or(def["summary"].as_str()),
            def["derived"].as_bool().unwrap_or(false),
        )
        .await?;
        accepted += 1;
    }

    tracing::info!(%kb_id, proposals = accepted, "mapping exploration complete, proposals queued for review");
    // When not a single proposal comes out, nothing on the page changes -- Pending is still 0,
    // and the "queued" message scrolled away long ago. Saying so through the alert centre is
    // what tells a person to go refresh the schema or add comments to the columns
    if accepted == 0 {
        if let Err(e) = utopia_store::alerts::raise(
            &state.pool,
            utopia_store::alerts::NewAlert {
                kb_id: Some(kb_id),
                severity: "info",
                kind: utopia_store::alerts::kind::MAPPING_EXPLORATION_EMPTY,
                min_role: utopia_core::models::Role::Editor,
                subject_type: None,
                subject_id: None,
                detail: serde_json::json!({ "proposals": 0, "sources": source_names }),
            },
        )
        .await
        {
            tracing::warn!(%kb_id, error = %e, "failed to raise the empty-result alert for mapping exploration");
        }
        state.emit_alert();
    }
    state.emit_review(kb_id);
    Ok(())
}
