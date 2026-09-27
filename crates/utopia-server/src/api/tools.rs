//! The **execution** of the seven tools, independent of who is calling them.
//!
//! These used to be seven arms of a `match` in `chat.rs`, 20–70 lines each, with closures
//! capturing locals of the streaming loop. Written that way it was fine while chat was the only
//! caller -- **and MCP is the second one**. Pulled out here, both sides share one implementation,
//! so you never get "the entity_facts in chat and the one in MCP are not the same thing".
//!
//! The tool definitions (the JSON schema the model sees) still live in `chat.rs`: those are part
//! of the prompt and follow the chat strategy; this module only cares about what to do once the
//! arguments are in hand.

use serde_json::json;
use utopia_core::models::{ChunkView, DataSourceView, EntityFact, GraphChange};
use uuid::Uuid;

use crate::retrieval;
use crate::state::AppState;

const SEARCH_TOP_K: usize = 6;
const TOOL_CHUNK_CHARS: usize = 800;
/// How many characters `get_document` returns at most in one go. The 800 chars on the retrieval
/// side are there so all six hits fit; here there is only one document, and whoever asked already
/// knows which one they want to read -- being cut off right before the answer is exactly the
/// failing this tool exists to eliminate
const DOCUMENT_CHARS: usize = 24_000;
/// How many rows one `changes` call returns at most. In a freshly loaded KB the asserted rows run
/// into the hundreds or thousands, and sending them all only fills up the context without adding
/// information -- what carries information is corrected/rejected, and events of that sort are rare
/// to begin with. When truncated, detail says "40+", so the model knows to narrow the window
const CHANGES_LIMIT: i64 = 40;

/// The world one tool call can see. **Read-only** -- a tool cannot change it.
pub struct ToolCtx<'a> {
    pub state: &'a AppState,
    pub kb_id: Uuid,
    pub workspace_id: Uuid,
    /// The data sources mounted on this KB. **This list *is* `query_data`'s security boundary**:
    /// credentials never leave the server, the model can only order by name
    pub mounted_sources: &'a [DataSourceView],
    /// Only editor and above get `remember`
    pub can_write: bool,
    /// The person speaking. `remember` records it as "who said it", and it follows all the way
    /// into the pending-confirmation queue (0015). Always set in chat; set for MCP too -- a token
    /// acts on behalf of a person (0014)
    pub actor: Option<Uuid>,
}

/// What gets accumulated on the way out while the tools run.
///
/// **Citation numbering is stateful**: the 3 in `[3]` depends on how many things were already
/// cited earlier in this turn, so each tool cannot number on its own and merge afterwards -- that
/// way the same chunk would end up with two numbers.
#[derive(Default)]
pub struct ToolSink {
    /// Dedup key (chunk uuid, or `charter:{slug}#{anchor}`); the index + 1 is the citation number
    pub source_ids: Vec<String>,
    /// The citation list sent to the front end, in the same order as `source_ids`
    pub sources: Vec<serde_json::Value>,
    /// The entities resolved this turn, persisted into the session for replay next turn
    pub resolved: Vec<serde_json::Value>,
}

/// What one tool call produces: text for the model + one step for the UI.
pub type ToolResult = (String, serde_json::Value);

/// Dispatch by name. **An unknown tool is not an error** -- the model occasionally invents a
/// name; tell it there is no such tool and it picks a different one next turn, which beats
/// aborting the whole conversation.
pub async fn dispatch(
    ctx: &ToolCtx<'_>,
    sink: &mut ToolSink,
    name: &str,
    args: &serde_json::Value,
) -> ToolResult {
    match name {
        "search_chunks" => search_chunks(ctx, sink, args).await,
        "get_document" => get_document(ctx, sink, args).await,
        "search_docs" => search_docs(ctx, sink, args).await,
        "find_entities" => find_entities(ctx, sink, args).await,
        "entity_facts" => entity_facts(ctx, args).await,
        "changes" => changes(ctx, args).await,
        "query_data" if !ctx.mounted_sources.is_empty() => query_data(ctx, args).await,
        "remember" if ctx.can_write => remember(ctx, args).await,
        other => (
            format!("Unknown tool: {other}"),
            json!({ "kind": "tool", "label": other, "detail": "unknown" }),
        ),
    }
}

/// Anything already cited gets its original number back, anything new gets a fresh one. **One
/// chunk can only have one number within a single turn**, otherwise the model cites `[2]` while
/// the UI shows two different `[2]`s.
fn cite(sink: &mut ToolSink, key: String, make: impl FnOnce(usize) -> serde_json::Value) -> usize {
    match sink.source_ids.iter().position(|id| *id == key) {
        Some(i) => i + 1,
        None => {
            sink.source_ids.push(key);
            sink.sources.push(make(sink.source_ids.len()));
            sink.source_ids.len()
        }
    }
}

pub async fn search_chunks(
    ctx: &ToolCtx<'_>,
    sink: &mut ToolSink,
    args: &serde_json::Value,
) -> ToolResult {
    // Required arguments are rejected by `chat::check_call` before dispatch, so we no longer fall
    // back to the user's original sentence here -- that fallback produces a wrong answer that
    // looks perfectly fine
    let q = args["query"].as_str().unwrap_or_default().to_string();
    let chunks = retrieval::hybrid(ctx.state, ctx.kb_id, ctx.workspace_id, &q, SEARCH_TOP_K)
        .await
        .unwrap_or_default();
    let mut lines = Vec::new();
    for c in &chunks {
        let n = cite(sink, c.id.to_string(), |n| source_json(n, c));
        // The line carries document_id: when a hit gets cut at 800 chars, the model needs some
        // way to ask for the whole document back
        lines.push(format!(
            "[{n}] \"{}\" section {} (document_id: {}):\n{}",
            c.filename,
            c.seq + 1,
            c.document_id,
            truncate(&c.text, TOOL_CHUNK_CHARS)
        ));
    }
    let text = if lines.is_empty() {
        "No results.".to_string()
    } else {
        lines.join("\n\n")
    };
    (
        text,
        json!({ "kind": "search", "label": q, "detail": format!("{} sources", chunks.len()) }),
    )
}

/// The full text of one document. **Everything search_chunks cannot reach is here**: it returns
/// only the top six hits, each cut at 800 chars, and when the answer sits at character 801 or in a
/// chunk that did not rank, retrieval itself was not wrong -- what was wrong was having no second
/// step.
pub async fn get_document(
    ctx: &ToolCtx<'_>,
    sink: &mut ToolSink,
    args: &serde_json::Value,
) -> ToolResult {
    let refuse = |detail: &str| {
        (
            "No document with that id in this knowledge base.".to_string(),
            json!({ "kind": "document", "label": "?", "detail": detail }),
        )
    };
    let Some(id) = args["document_id"]
        .as_str()
        .and_then(|s| s.trim().parse::<Uuid>().ok())
    else {
        return refuse("invalid id");
    };
    // An id from outside this KB is uniformly treated as nonexistent -- not being able to tell
    // "does not exist" apart from "you are not allowed to see it" is the right behaviour
    let Ok(Some(doc)) = utopia_store::documents::find_in_kb(&ctx.state.pool, ctx.kb_id, id).await
    else {
        return refuse("not found");
    };
    let chunks = utopia_store::documents::chunks_in_document(&ctx.state.pool, ctx.kb_id, id)
        .await
        .unwrap_or_default();

    let mut lines = Vec::new();
    let mut used = 0usize;
    let mut omitted = 0usize;
    for c in &chunks {
        let body = c.text.trim();
        let len = body.chars().count();
        let room = DOCUMENT_CHARS.saturating_sub(used);
        // A chunk that does not fit is not sent at all, and not cited either -- handing out a
        // citation number with no body behind it lands in the UI as a citation pointing at nothing
        if room == 0 {
            omitted += len;
            continue;
        }
        let body: String = if len > room {
            omitted += len - room;
            body.chars().take(room).collect()
        } else {
            body.to_string()
        };
        used += body.chars().count();
        let n = cite(sink, c.id.to_string(), |n| source_json(n, c));
        lines.push(format!(
            "[{n}] \"{}\" section {}:\n{body}",
            c.filename,
            c.seq + 1
        ));
    }
    if omitted > 0 {
        lines.push(format!("… truncated, {omitted} chars omitted"));
    }

    let when = doc
        .doc_time
        .map(|t| t.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "no date".to_string());
    let header = format!(
        "\"{}\" ({when}) — {} section(s):",
        doc.filename,
        chunks.len()
    );
    let text = if lines.is_empty() {
        format!("{header}\n(no text)")
    } else {
        format!("{header}\n\n{}", lines.join("\n\n"))
    };
    (
        text,
        json!({
            "kind": "document", "label": doc.filename,
            "detail": format!("{} sections", chunks.len()),
        }),
    )
}

pub async fn search_docs(
    ctx: &ToolCtx<'_>,
    sink: &mut ToolSink,
    args: &serde_json::Value,
) -> ToolResult {
    // Required arguments are rejected by `chat::check_call` before dispatch, so we no longer fall
    // back to the user's original sentence here -- that fallback produces a wrong answer that
    // looks perfectly fine
    let q = args["query"].as_str().unwrap_or_default().to_string();
    let hits = ctx.state.docs.search(&q, 4).unwrap_or_default();
    let mut lines = Vec::new();
    for h in &hits {
        let key = format!("charter:{}#{}", h.slug, h.anchor);
        let n = cite(sink, key, |n| charter_source_json(n, h));
        lines.push(format!(
            "[{n}] Utopia Charter — {} › {}:\n{}",
            h.title,
            h.heading,
            truncate(&h.body, 1600)
        ));
    }
    let text = if lines.is_empty() {
        "No matching manual sections.".to_string()
    } else {
        lines.join("\n\n")
    };
    (
        text,
        json!({ "kind": "docs", "label": q, "detail": format!("{} sections", hits.len()) }),
    )
}

pub async fn find_entities(
    ctx: &ToolCtx<'_>,
    sink: &mut ToolSink,
    args: &serde_json::Value,
) -> ToolResult {
    let name = args["name"].as_str().unwrap_or("").to_string();
    let (hits, _) = utopia_store::graph::search_entities(&ctx.state.pool, ctx.kb_id, &name, 8, 0)
        .await
        .unwrap_or_default();
    let text = if hits.is_empty() {
        "No matching entities.".to_string()
    } else {
        hits.iter()
            .map(|n| {
                let dis = n
                    .disambiguator
                    .as_deref()
                    .map(|d| format!(" ({d})"))
                    .unwrap_or_default();
                format!(
                    "{} | {}{} | {} | {} facts",
                    n.id,
                    n.name,
                    dis,
                    // Entities whose type was never determined can still be found and cited (0009)
                    n.type_label.as_deref().unwrap_or("untyped"),
                    n.degree
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    for n in &hits {
        sink.resolved.push(json!({
            "id": n.id.to_string(), "name": n.name, "type": n.type_label
        }));
    }
    (
        text,
        json!({ "kind": "entity", "label": name, "detail": format!("{} matches", hits.len()) }),
    )
}

pub async fn entity_facts(ctx: &ToolCtx<'_>, args: &serde_json::Value) -> ToolResult {
    let id = args["entity_id"]
        .as_str()
        .and_then(|s| s.parse::<Uuid>().ok());
    // as-of filter: valid at T = start no later than T (or unknown) and end later than T (or open)
    let at = args["at"]
        .as_str()
        .and_then(|s| chrono::NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").ok())
        .map(|d| d.and_hms_opt(0, 0, 0).unwrap().and_utc());
    let Some(id) = id else {
        return (
            "Invalid entity_id (expected the uuid returned by find_entities).".to_string(),
            json!({ "kind": "facts", "label": "?", "detail": "invalid id" }),
        );
    };
    match utopia_store::graph::entity_detail(&ctx.state.pool, ctx.kb_id, id).await {
        Ok((node, mut facts)) => {
            if let Some(t) = at {
                facts.retain(|f| {
                    f.valid_from.is_none_or(|from| from <= t) && f.valid_to.is_none_or(|to| to > t)
                });
            }
            let text = if facts.is_empty() {
                match at {
                    Some(t) => format!(
                        "{}: no facts valid as of {}.",
                        node.name,
                        t.format("%Y-%m-%d")
                    ),
                    None => format!("{}: no recorded facts.", node.name),
                }
            } else {
                facts.iter().map(fact_line).collect::<Vec<_>>().join("\n")
            };
            let detail = match at {
                Some(t) => format!("{} facts as of {}", facts.len(), t.format("%Y-%m-%d")),
                None => format!("{} facts", facts.len()),
            };
            (
                text,
                json!({ "kind": "facts", "label": node.name, "detail": detail }),
            )
        }
        Err(_) => (
            "Entity not found.".to_string(),
            json!({ "kind": "facts", "label": "?", "detail": "not found" }),
        ),
    }
}

pub async fn changes(ctx: &ToolCtx<'_>, args: &serde_json::Value) -> ToolResult {
    let day = |k: &str| {
        args[k]
            .as_str()
            .and_then(|s| chrono::NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").ok())
    };
    let Some((since, until, window)) =
        changes_window(day("since"), day("until"), chrono::Utc::now())
    else {
        return (
            "Invalid or missing `since` (expected YYYY-MM-DD).".to_string(),
            json!({ "kind": "changes", "label": "?", "detail": "invalid since" }),
        );
    };
    let entity = args["entity_id"]
        .as_str()
        .and_then(|s| s.parse::<Uuid>().ok());
    let kinds: Option<Vec<String>> = args["kinds"].as_array().map(|a| {
        a.iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect()
    });
    let kinds = kinds.filter(|k: &Vec<String>| !k.is_empty());
    let rows = utopia_store::graph::graph_changes(
        &ctx.state.pool,
        ctx.kb_id,
        since,
        until,
        entity,
        kinds.as_deref(),
        CHANGES_LIMIT,
    )
    .await
    .unwrap_or_default();
    let text = if rows.is_empty() {
        format!("No recorded changes in {window}.")
    } else {
        rows.iter().map(change_line).collect::<Vec<_>>().join("\n")
    };
    let detail = if rows.len() as i64 == CHANGES_LIMIT {
        format!("{CHANGES_LIMIT}+ changes")
    } else {
        format!("{} changes", rows.len())
    };
    (
        text,
        json!({ "kind": "changes", "label": window, "detail": detail }),
    )
}

pub async fn query_data(ctx: &ToolCtx<'_>, args: &serde_json::Value) -> ToolResult {
    let ds_name = args["data_source"].as_str().map(str::trim).unwrap_or("");
    let sql = args["sql"].as_str().map(str::trim).unwrap_or("");
    let purpose = args["purpose"].as_str().map(str::trim).unwrap_or("");
    // Security boundary: only sources mounted on this KB are allowed (credentials never leave
    // the server)
    let found = ctx
        .mounted_sources
        .iter()
        .find(|d| d.name.eq_ignore_ascii_case(ds_name));
    let text = match found {
        None => format!(
            "Unknown data source '{ds_name}'. Mounted sources: {}",
            ctx.mounted_sources
                .iter()
                .map(|d| d.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Some(ds) => match run_query(ctx.state, ds.id, sql).await {
            Ok(out) => out,
            // Errors pass straight through: the model can correct its SQL from them and retry
            Err(e) => format!("Query failed: {e}"),
        },
    };
    let detail = if purpose.is_empty() {
        sql.chars().take(60).collect::<String>()
    } else {
        purpose.to_string()
    };
    (
        text,
        json!({ "kind": "query", "label": ds_name, "detail": detail }),
    )
}

pub async fn remember(ctx: &ToolCtx<'_>, args: &serde_json::Value) -> ToolResult {
    let text = args["text"].as_str().map(str::trim).unwrap_or("");
    let occurred_at = args["occurred_at"]
        .as_str()
        .and_then(|s| chrono::NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").ok())
        .map(|d| d.and_hms_opt(12, 0, 0).unwrap().and_utc())
        .unwrap_or_else(chrono::Utc::now);
    if text.is_empty() {
        return (
            "remember requires non-empty text.".to_string(),
            json!({ "kind": "tool", "label": "remember", "detail": "empty" }),
        );
    }
    match utopia_store::memory::append_episode(&ctx.state.pool, ctx.kb_id, text, occurred_at).await
    {
        Ok((doc_id, chunk_id)) => {
            // Ingestion (embedding/indexing/incremental extraction) goes through the queue
            // asynchronously and does not block the conversation.
            // The extracted facts **wait for a human nod first** (0015) -- so all this can
            // honestly say is "the sentence is recorded", not how many facts came out of it:
            // extraction has not run yet. The card grows into the conversation when the job ends
            let _ = utopia_store::jobs::enqueue(
                &ctx.state.pool,
                "memory_ingest",
                json!({ "document_id": doc_id, "proposed_by": ctx.actor }),
            )
            .await;
            ctx.state.emit_document(ctx.kb_id, doc_id);
            (
                format!(
                    "Recorded the sentence (effective {}): {text}\n\
                     Facts extracted from it will be shown to the user for confirmation \
                     before entering the graph. Tell the user exactly that: the sentence is \
                     recorded, and the extracted facts await their confirmation. Do not claim \
                     any fact has been added to the knowledge graph.",
                    occurred_at.format("%Y-%m-%d")
                ),
                json!({
                    "kind": "tool", "label": "remember",
                    "detail": text.chars().take(60).collect::<String>(),
                    // The confirmation card in the chat pulls its pending items by this, and
                    // replay redraws from it too
                    "chunk_id": chunk_id,
                }),
            )
        }
        Err(e) => (
            format!("Failed to record: {e}"),
            json!({ "kind": "tool", "label": "remember", "detail": "failed" }),
        ),
    }
}

// ---------------------------------------------------------------------------
// Formatting and execution helpers. **The chat degradation path uses them too**, which is why
// they are pub(super) rather than private
// ---------------------------------------------------------------------------

pub(super) fn source_json(n: usize, c: &ChunkView) -> serde_json::Value {
    json!({
        "n": n,
        "chunk_id": c.id,
        "document_id": c.document_id,
        "filename": c.filename,
        "excerpt": truncate(&c.text, 160),
    })
}

/// A Charter citation: the front end renders it as a manual row, linked to /docs/{slug}#{anchor}.
pub(super) fn charter_source_json(n: usize, h: &utopia_search::DocsSection) -> serde_json::Value {
    json!({
        "n": n,
        "kind": "charter",
        "slug": h.slug,
        "anchor": h.anchor,
        "heading": h.heading,
        "filename": h.title,
        "excerpt": truncate(&h.body, 160),
    })
}

/// Data-question execution: safety gate (parse + allowlist) → engine execution (read-only session
/// + forced LIMIT + timeout) → JSON rows.
async fn run_query(state: &AppState, ds_id: Uuid, sql: &str) -> anyhow::Result<String> {
    let (engine, conn) = utopia_store::datasources::engine_and_conn(&state.pool, ds_id).await?;
    // The gate picks its dialect by engine: Databricks' backticks and Snowflake's :: casts both
    // have to get through the parser first
    let guarded = crate::query_engine::guard_sql_for(&engine, sql)?;
    let result = crate::query_engine::engine_for(&engine, &conn)?
        .execute(&guarded)
        .await?;
    let mut out = String::new();
    if result.rows.is_empty() {
        out.push_str("(no rows)");
    } else {
        out.push_str(&result.rows.join("\n"));
        out.push_str(&format!("\n({} rows", result.rows.len()));
        if result.truncated {
            out.push_str(&format!(
                ", truncated at {} — aggregate in SQL for totals",
                crate::query_engine::ROW_CAP
            ));
        }
        out.push(')');
    }
    Ok(out)
}

/// A fact line: "works at → Nebula Tech (2023-08 → now) [90%]"; the in direction uses ←.
fn fact_line(f: &EntityFact) -> String {
    let other = f.other_name.as_deref().unwrap_or("?");
    // "?" when the ontology never resolved it and no original phrasing was kept either -- the
    // same convention as other. Do not invent a "related to": that is exactly the thing deleting
    // related_to was meant to eliminate
    let pred = f.predicate_label.as_deref().unwrap_or("?");
    let core = if f.direction == "out" {
        format!("{pred} → {other}")
    } else {
        format!("{pred} ← {other}")
    };
    let range = match (&f.valid_from, &f.valid_to) {
        (Some(from), Some(to)) => {
            format!(" ({} → {})", from.format("%Y-%m-%d"), to.format("%Y-%m-%d"))
        }
        (Some(from), None) => format!(" ({} → now)", from.format("%Y-%m-%d")),
        (None, Some(to)) => format!(" (→ {})", to.format("%Y-%m-%d")),
        (None, None) => String::new(),
    };
    format!("{core}{range} [{}%]", (f.confidence * 100.0).round() as i32)
}

/// The time window for changes: turn two optional dates into (the half-open interval SQL wants,
/// the window string for display).
///
/// **This is a pure function because it got this wrong once.** `until` has to have a day added
/// before it goes into SQL (someone saying "up to March 31" wants the 31st included, while the SQL
/// side is `< $3`), and the first version printed the already-incremented value into the display
/// string as well, so the model duly answered "as of August 30" -- the question was about the
/// 29th. The two values have to be computed together and pinned down together; written in two
/// places they will diverge again sooner or later.
///
/// `now` is passed in from outside rather than read inside purely so that this function is
/// testable.
fn changes_window(
    since: Option<chrono::NaiveDate>,
    until: Option<chrono::NaiveDate>,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<(
    chrono::DateTime<chrono::Utc>,
    chrono::DateTime<chrono::Utc>,
    String,
)> {
    let from = since?;
    let start = from.and_hms_opt(0, 0, 0).unwrap().and_utc();
    let end = until
        .and_then(|d| d.succ_opt())
        .map(|d| d.and_hms_opt(0, 0, 0).unwrap().and_utc())
        .unwrap_or(now);
    // Display uses **the day that was asked about**, or now if none was asked -- never end
    let label = format!(
        "{} → {}",
        from.format("%Y-%m-%d"),
        match until {
            Some(d) => d.format("%Y-%m-%d").to_string(),
            None => "now".to_string(),
        }
    );
    Some((start, end, label))
}

/// One row on the epistemic axis.
///
/// The layout writes the two axes **separately**: `at` is prefixed with the event kind, and the
/// world-axis interval sits in brackets after the assertion. Lay them out as one run of dates and
/// the model will read "recorded in 2026" as "happened in 2026" -- which is precisely the
/// misreading this tool is meant to prevent.
fn change_line(c: &GraphChange) -> String {
    let object = match (&c.object_name, &c.object_value) {
        (Some(name), _) => name.clone(),
        (None, Some(v)) => match v {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        },
        (None, None) => "?".to_string(),
    };
    let range = match (&c.valid_from, &c.valid_to) {
        (Some(from), Some(to)) => {
            format!(
                " [valid {} → {}]",
                from.format("%Y-%m-%d"),
                to.format("%Y-%m-%d")
            )
        }
        (Some(from), None) => format!(" [valid {} → now]", from.format("%Y-%m-%d")),
        (None, Some(to)) => format!(" [valid → {}]", to.format("%Y-%m-%d")),
        (None, None) => String::new(),
    };
    // The filename carries no [n]: citation numbers belong to chunks, and here we only have a
    // document, so handing out a number would land in the UI as a citation pointing at nothing
    let src = match (&c.filename, &c.quote) {
        (Some(f), Some(q)) => format!(" — from \"{f}\": \"{}\"", truncate(q, 160)),
        (Some(f), None) => format!(" — from \"{f}\""),
        _ => String::new(),
    };
    format!(
        "{} {}: {} {} {}{}{}",
        c.at.format("%Y-%m-%d"),
        c.kind,
        c.subject_name,
        c.predicate_label.as_deref().unwrap_or("?"),
        object,
        range,
        src
    )
}

pub(super) fn truncate(text: &str, max_chars: usize) -> String {
    let t = text.trim();
    if t.chars().count() <= max_chars {
        t.to_string()
    } else {
        let cut: String = t.chars().take(max_chars).collect();
        format!("{cut}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> chrono::NaiveDate {
        chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }
    fn t(s: &str) -> chrono::DateTime<chrono::Utc> {
        s.parse().unwrap()
    }

    // --- changes_window -----------------------------------------------------

    /// Someone saying "up to March 31" wants **the 31st included**. The SQL side is `< end`, so
    /// end has to land at midnight on April 1 -- be a day off and asking about the 31st silently
    /// drops the 31st
    #[test]
    fn until_names_a_day_the_window_must_contain() {
        let (start, end, label) = changes_window(
            Some(d("2026-03-01")),
            Some(d("2026-03-31")),
            t("2026-06-01T00:00:00Z"),
        )
        .unwrap();
        assert_eq!(start, t("2026-03-01T00:00:00Z"));
        assert_eq!(end, t("2026-04-01T00:00:00Z"));
        // But **04-01 must never show up in the display string**: that is an internal detail of
        // the half-open interval, and printing it tells the model the window is a day wider than
        // it asked for, and the model will answer accordingly (this really did happen)
        assert_eq!(label, "2026-03-01 → 2026-03-31");
    }

    /// With no `until` given, the display string says now. Formatting `end` instead amounts to
    /// treating the server clock as the boundary the user asked about -- it looks like a precise
    /// answer, but it is really just "what time is it now"
    #[test]
    fn an_open_window_says_now_rather_than_the_clock() {
        let now = t("2026-06-01T13:45:00Z");
        let (_, end, label) = changes_window(Some(d("2026-03-01")), None, now).unwrap();
        assert_eq!(end, now);
        assert_eq!(label, "2026-03-01 → now");
    }

    #[test]
    fn without_since_there_is_no_window() {
        assert!(changes_window(None, Some(d("2026-03-31")), t("2026-06-01T00:00:00Z")).is_none());
    }

    // --- change_line --------------------------------------------------------

    fn change(kind: &str) -> GraphChange {
        GraphChange {
            fact_id: Uuid::nil(),
            at: t("2026-08-28T10:00:00Z"),
            kind: kind.to_string(),
            subject_id: Uuid::nil(),
            subject_name: "Acme".to_string(),
            predicate_label: Some("founded in".to_string()),
            object_name: None,
            object_value: None,
            valid_from: None,
            valid_to: None,
            // No date at either end means no precision -- the fixture has to hold that invariant
            // too (see `facts.valid_from_precision`)
            // No date at either end, so no precision at either end (see the two precision columns
            // on facts)
            valid_from_precision: None,
            valid_to_precision: None,
            confidence: 0.9,
            document_id: None,
            filename: None,
            quote: None,
        }
    }

    /// A literal value has to read as itself. Going through `Value::to_string()` prints the string
    /// together with its quotes, so the model treats `"1993"` as part of the answer
    #[test]
    fn a_literal_value_reads_as_itself_not_as_json() {
        let mut c = change("asserted");
        c.object_value = Some(serde_json::json!("1993"));
        assert!(
            change_line(&c).ends_with("Acme founded in 1993"),
            "{}",
            change_line(&c)
        );
    }

    /// Within one line the two axes must **read as two different things**: the record time first,
    /// unlabelled; the world-axis interval after it, carrying the word `valid`. Laid out as a run
    /// of bare dates, the model reads "recorded in 2026" as "happened in 2026" -- and preventing
    /// that is the entire reason this tool exists
    #[test]
    fn the_record_time_and_the_valid_range_do_not_read_as_one_date_run() {
        let mut c = change("corrected");
        c.object_name = Some("Berlin".to_string());
        c.predicate_label = Some("headquartered in".to_string());
        c.valid_from = Some(t("2019-01-01T00:00:00Z"));
        let line = change_line(&c);
        assert!(line.starts_with("2026-08-28 corrected: "), "{line}");
        assert!(line.contains("[valid 2019-01-01 → now]"), "{line}");
    }

    /// With no evidence, say nothing at all. Tacking on a from "…" out of thin air makes an
    /// assertion with no provenance look like it has one
    #[test]
    fn a_fact_with_no_evidence_claims_no_document() {
        let mut c = change("rejected");
        c.object_name = Some("Berlin".to_string());
        assert!(!change_line(&c).contains("from"), "{}", change_line(&c));
    }

    #[test]
    fn evidence_carries_the_filename_and_the_quote() {
        let mut c = change("corrected");
        c.object_name = Some("Berlin".to_string());
        c.filename = Some("annual-report.pdf".to_string());
        c.quote = Some("moved its head office to Berlin".to_string());
        let line = change_line(&c);
        assert!(line.contains("from \"annual-report.pdf\""), "{line}");
        assert!(
            line.contains("\"moved its head office to Berlin\""),
            "{line}"
        );
    }
}
