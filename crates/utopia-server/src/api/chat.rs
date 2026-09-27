//! Agentic conversation: the model calls tools on its own (document search / entity lookup /
//! temporal facts) to gather evidence, then answers.
//! Event sequence: step* (the action trail) | sources (the citation list, updated incrementally
//! as retrieval goes) | delta* (incremental text) → done | error.
//! When the model does not support tool-calling it degrades automatically to a one-shot RAG
//! injection.

use super::tools;
use crate::live::Frame;
use axum::extract::{Path, Query, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::Json;
use futures_util::{Stream, StreamExt};
use serde::Deserialize;
use serde_json::json;
use std::convert::Infallible;
use utopia_core::models::{ChunkView, Role};
use utopia_core::AppError;
use utopia_llm::tool_result_message;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::error::ApiResult;
use crate::llm_util;
use crate::retrieval;
use crate::state::AppState;

/// Replay a handful of already-identified entities. The cap is there because a long
/// conversation piles up dozens of them, and pasting them all back spends the context we just
/// saved; sorted by first appearance, since the ones identified early are usually this
/// conversation's protagonists.
const KNOWN_ENTITY_LIMIT: usize = 20;

const MAX_HISTORY: usize = 20;
const MAX_ROUNDS: usize = 6;

/// `remember` was disabled outright for a while (see `docs/decisions/0015`): back then it
/// turned a sentence straight into a live edge on the graph -- in practice "remember that Acme
/// moved its headquarters to Shenzhen" landed as an edge with an **empty predicate and 0.9
/// confidence**, so what the assistant claimed and what the graph got were not the same thing.
///
/// Extraction now goes through `pending_facts`: facts extracted from a memory wait for a human
/// nod before they enter the ledger. This switch stays so that the next time we find "a tool
/// quietly rewrites the graph" there is somewhere to pull the lever at once -- better no such
/// tool at all than a tool that quietly rewrites the graph.
pub(super) const REMEMBER_ENABLED: bool = true;

#[derive(Deserialize)]
pub struct ChatReq {
    /// Absent = create a new conversation (the first SSE `conversation` event returns the id)
    #[serde(default)]
    pub conversation_id: Option<Uuid>,
    pub message: String,
}

fn tools_schema(can_write: bool, data_source_names: &[String]) -> serde_json::Value {
    let mut tools = base_tools();
    if !data_source_names.is_empty() {
        if let Some(arr) = tools.as_array_mut() {
            arr.push(json!({
                "type": "function",
                "function": {
                    "name": "query_data",
                    "description": format!(
                        "Run a read-only SQL query against a mounted database. Available sources: \
                         {}. Search the source's schema document first (search_chunks) if unsure \
                         of tables/columns. Only a single SELECT/WITH statement is allowed; a \
                         LIMIT is enforced server-side; results come back as JSON lines. If the \
                         query errors, fix the SQL and retry once.",
                        data_source_names.join(", ")
                    ),
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "data_source": {
                                "type": "string",
                                "description": "Name of the mounted data source to query."
                            },
                            "sql": {
                                "type": "string",
                                "description": "One SELECT/WITH statement in the source's own SQL dialect (PostgreSQL, Trino, Databricks or Snowflake; the schema document names the engine)."
                            },
                            "purpose": {
                                "type": "string",
                                "description": "One short phrase: what this query answers (shown to the user)."
                            }
                        },
                        "required": ["data_source", "sql"]
                    }
                }
            }));
        }
    }
    if can_write && REMEMBER_ENABLED {
        if let Some(arr) = tools.as_array_mut() {
            arr.push(json!({
                "type": "function",
                "function": {
                    "name": "remember",
                    "description": "Record one memory episode into the knowledge base's temporal \
                        memory. Use ONLY when the user explicitly asks to remember/record \
                        something, or clearly states a decision or fact to keep. The sentence \
                        is stored immediately; facts extracted from it are PROPOSED and shown \
                        to the user for confirmation before they enter the knowledge graph. \
                        Never claim a fact was added to the graph. Do not use for casual \
                        conversation.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "text": {
                                "type": "string",
                                "description": "The episode to remember, one self-contained \
                                    statement (who/what, with names spelled out)."
                            },
                            "occurred_at": {
                                "type": "string",
                                "description": "Optional date the stated fact took effect \
                                    (YYYY-MM-DD). Omit to use today."
                            }
                        },
                        "required": ["text"]
                    }
                }
            }));
        }
    }
    tools
}

/// Before running a call, check whether it said what it wanted clearly.
///
/// **Both flavours of "did not say it clearly" used to turn silently into a normal call.**
///
/// The first is arguments that do not parse. When the model's output hits the token limit,
/// `arguments` gets cut off mid-way and that JSON string is incomplete. This used to be
/// `unwrap_or_else(|_| json!({}))` -- an empty object -- and then
/// `args["query"].as_str().unwrap_or(&query)` inside `search_chunks` fell back to
/// **the user's own sentence**, so a truncated call turned into "search with the user's
/// original question", while the trail showed a perfectly normal `search · 6 sources`.
///
/// The second is a required argument simply not given. Same fallback, same result.
///
/// Neither should be guessed at. **A fallback produces a wrong answer that looks fine**, and
/// that is far worse than an error -- on an error the model retries, whereas nobody goes and
/// checks an answer that was guessed.
///
/// The criteria come straight from `required` in the tool schema: add a required argument and
/// this follows automatically, with no second place to remember to change.
fn check_call(
    tools: &serde_json::Value,
    name: &str,
    raw_args: &str,
) -> Result<serde_json::Value, (String, serde_json::Value)> {
    let refuse = |detail: &str, message: String| {
        (
            message,
            json!({ "kind": "tool", "label": name, "detail": detail }),
        )
    };
    let Ok(args) = serde_json::from_str::<serde_json::Value>(raw_args) else {
        return Err(refuse(
            "bad arguments",
            format!(
                "The arguments for {name} were not valid JSON, so the call was not run. \
                 They were probably cut off. Call it again with complete arguments."
            ),
        ));
    };
    let function = tools
        .as_array()
        .into_iter()
        .flatten()
        .find(|t| t["function"]["name"] == name)
        .map(|t| &t["function"]);
    let required = function.and_then(|f| f["parameters"]["required"].as_array());
    for key in required.into_iter().flatten().filter_map(|k| k.as_str()) {
        // An empty string and null both count as not given: what `{"query": ""}` retrieves
        // has nothing to do with the question, and it shows up as a perfectly normal trail
        // entry all the same
        let missing = match args.get(key) {
            None | Some(serde_json::Value::Null) => true,
            Some(serde_json::Value::String(s)) => s.trim().is_empty(),
            Some(_) => false,
        };
        if missing {
            return Err(refuse(
                &format!("missing {key}"),
                format!(
                    "{name} needs `{key}`, and it was missing or empty, so the call was not \
                     run. Call it again with `{key}` set."
                ),
            ));
        }
        // The criteria come from the tool schema here too: an argument declared `format: uuid`
        // in the schema has its format stopped at this gate as well. A made-up id can only come
        // back from the tool as "this base has no such document", which the model reads as "the
        // base really does not have it" and gives up -- another wrong answer that looks fine
        let is_uuid =
            function.is_some_and(|f| f["parameters"]["properties"][key]["format"] == "uuid");
        if is_uuid
            && args[key]
                .as_str()
                .is_none_or(|s| s.trim().parse::<Uuid>().is_err())
        {
            return Err(refuse(
                &format!("invalid {key}"),
                format!(
                    "{name} needs `{key}` to be a uuid returned by another tool, and it was \
                     not, so the call was not run. Look the id up first, then call it again."
                ),
            ));
        }
    }
    Ok(args)
}

const MEMORY_PROMPT: &str = "\
    Memory: you can persist knowledge with the remember tool. Use it when the user says \
    \"remember/record this\" or states a decision meant to last. In your reply, say that the \
    sentence was recorded and that the facts extracted from it will be shown for the user's \
    confirmation before entering the graph; never say a fact is already in the graph. \
    Never invent memories, and never call it for small talk.";

/// The tools' JSON schema -- **the copy the model sees**.
///
/// It lives in `chat.rs` rather than `tools.rs`: it is part of the prompt and follows the
/// conversation strategy, while `tools.rs` only deals with what happens once the arguments are
/// in hand (see the note at the top of that module).
/// MCP reads it too, hence the visibility to the same module -- both sides describe the same
/// set of tools, and a separate copy on each side would inevitably diverge.
pub(super) fn base_tools() -> serde_json::Value {
    json!([
        {
            "type": "function",
            "function": {
                "name": "search_chunks",
                "description": "Full-text + semantic search over the knowledge base documents. \
                    Returns numbered source excerpts you can cite as [n], each with the \
                    document_id it came from. Excerpts are cut short and only the best-matching \
                    sections come back, so when the answer may sit elsewhere in a hit, call \
                    get_document with that document_id to read the whole document.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "Search query, phrased in the corpus language." }
                    },
                    "required": ["query"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "get_document",
                "description": "Read the full text of ONE knowledge base document, all sections \
                    in order, by the document_id shown in search_chunks results. Use it whenever \
                    a search hit looks like the right document but the excerpt does not contain \
                    the answer — action items, decisions and lists usually sit past the excerpt \
                    or in a section that did not match the query.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "document_id": {
                            "type": "string",
                            "format": "uuid",
                            "description": "Document id (uuid) from a search_chunks result line."
                        }
                    },
                    "required": ["document_id"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "search_docs",
                "description": "Search the Utopia product manual (the \"Utopia Charter\") — how \
                    the platform itself works (ingestion and sync, missing markers and versions, \
                    the graph and review flow, roles, settings) — and never the user's own \
                    documents, which live in search_chunks.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "What to look up in the manual." }
                    },
                    "required": ["query"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "find_entities",
                "description": "Look up entities in the knowledge graph by (partial) name. \
                    Returns id, name, type and a disambiguator when several entities share a name.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Entity name or a fragment of it." }
                    },
                    "required": ["name"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "entity_facts",
                "description": "Facts about one entity from the bi-temporal knowledge graph: \
                    relations with validity ranges (from → to; 'now' = still ongoing). \
                    The best tool for who/when/history questions. Use after find_entities. \
                    Pass `at` to see the world as of that date (server-side filter) — \
                    always do this for \"who was X in <year/month>\" questions.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "entity_id": { "type": "string", "description": "Entity id (uuid) from find_entities." },
                        "at": {
                            "type": "string",
                            "description": "Optional as-of date (YYYY-MM-DD). Only facts valid on \
                                this date are returned. Omit for the full history."
                        }
                    },
                    "required": ["entity_id"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "changes",
                "description": "What the graph LEARNED or REVISED in a window of record time —                     the belief axis. Answers \"what changed since X\", \"what did we get wrong\",                     \"what is new this quarter\", and needs no entity, so use it when the                     question names a period rather than a subject.                     Events: asserted (new claim), corrected (a claim replaced by a revised one),                     rejected (a claim withdrawn), merged (folded into another claim) — each with                     the document it came from.                     NOT the same axis as entity_facts(at): that asks \"what was true on date D\";                     this asks \"what did we change our mind about between D1 and D2\". A fact                     about 2019 can be recorded in 2026 — this windows on when we recorded it.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "since": {
                            "type": "string",
                            "description": "Start of the window (YYYY-MM-DD), inclusive."
                        },
                        "until": {
                            "type": "string",
                            "description": "End of the window (YYYY-MM-DD), inclusive of that                                 whole day. Omit for 'up to now'."
                        },
                        "entity_id": {
                            "type": "string",
                            "description": "Optional entity id from find_entities, to narrow the                                 window to changes touching that one entity."
                        },
                        "kinds": {
                            "type": "array",
                            "items": {
                                "type": "string",
                                "enum": ["asserted", "corrected", "rejected", "merged"]
                            },
                            "description": "Optional filter. A freshly ingested corpus is nearly                                 all 'asserted'; pass [\"corrected\", \"rejected\"] to isolate                                 the places we actually changed our mind."
                        }
                    },
                    "required": ["since"]
                }
            }
        }
    ])
}

const SYSTEM_PROMPT: &str = "You are the assistant of Utopia, a temporal knowledge platform. \
    You have tools: search_chunks (document search) and get_document (the full text of one \
    document found by search), find_entities, entity_facts and changes (a bi-temporal \
    knowledge graph), and search_docs (Utopia's own manual, the Charter).\n\
    search_chunks returns short excerpts of the best-matching sections only. When a hit is \
    clearly the right document but the excerpt does not carry the answer, read the whole \
    document with get_document before saying the knowledge base does not have it.\n\
    The graph has TWO independent time axes, and each graph tool reads exactly one:\n\
    - World time — when something was true. Read with entity_facts (`at` = as of that date).\n\
    - Record time — when we came to believe it, and when we revised it. Read with changes.\n\
    \"Who was CTO in 2019\" is world time; \"what did we learn last month\" and \"what did \
    we get wrong\" are record time. The same fact has a position on both.\n\
    Boundary: search_docs answers questions about Utopia itself (features, ingestion, \
    permissions, what fields like 'missing' or validity ranges mean); the other tools answer \
    questions about the knowledge stored in it. Never mix the manual into answers about the \
    user's data unless they asked about Utopia's behavior.\n\
    \n\
    Method:\n\
    First decide what the message is about. A message about THIS CONVERSATION — translate it, \
    say it shorter, rephrase it, \"what did you just say\", \"why\" — is answered from the \
    transcript above with NO tool calls: the evidence is already in it. Gathering it again is \
    not merely wasted work — with several entities sharing a name the second pass can land on \
    a different one, and the \"translation\" then says something else. Just deliver it — no \
    preamble about what you are or are not looking up. Everything below is for messages about \
    the user's data.\n\
    1. For factual questions — questions about the user's data, never one about this \
       conversation — ALWAYS gather evidence with tools before answering. Prefer the \
       graph tools for questions about people/organizations/projects and time (\"who was X \
       when\", \"what changed\"), search_chunks for content and detail questions. Combine both \
       when useful.\n\
    2. Facts carry validity ranges (from → to). For \"as of <date>\" questions pass `at` to \
       entity_facts and the server filters to that moment; for history questions omit `at` \
       to see the full timeline. State dates in the answer.\n\
    2b. For \"what changed / what is new / what did we get wrong since <date>\", call changes — \
       it needs no entity. Name the document a correction came from in plain prose. Graph tools \
       return no [n] numbers and no URLs, so never write a bracketed citation or a placeholder \
       like [Link] after one — the document's name IS the attribution.\n\
    3. Several entities can share one name — check the disambiguator and pick the right one; \
       if genuinely ambiguous, ask the user which one they mean.\n\
    4. Stop calling tools as soon as you have enough evidence. Then answer concisely: cite \
       document sources with [n] (numbers from search results) at the end of supported \
       sentences. If the evidence is insufficient, say so explicitly — never fabricate.\n\
    5. Always respond in the same language as the user's question.";

pub async fn chat(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(kb_id): Path<Uuid>,
    Json(req): Json<ChatReq>,
) -> ApiResult<Sse<impl Stream<Item = Result<Event, Infallible>>>> {
    let kb = utopia_store::access::require_kb(&state.pool, &user, kb_id, Role::Viewer).await?;
    // Write tools follow the person: only an editor-or-above conversation carries remember, a
    // viewer is strictly read-only
    // The mounted data sources decide whether query_data joins the list (Ask is a read, so a
    // viewer may use it too)
    let mounted_sources = utopia_store::datasources::mounted(&state.pool, kb_id).await?;
    // The semantic layer: human-confirmed metric/dimension → data asset mappings, straight
    // into the system prompt -- Ask prefers a confirmed definition over guessing from the
    // schema every time.
    //
    // This used to pull facts by `confidence >= 0.75`, and that threshold was a float encoding
    // a binary state (proposed 0.6 / confirmed 1.0). It now reads `status = confirmed` (0011)
    let mappings = if mounted_sources.is_empty() {
        Vec::new()
    } else {
        utopia_store::mappings::confirmed(&state.pool, kb_id, 30).await?
    };
    let can_write = utopia_store::access::kb_role(&state.pool, &user, &kb)
        .await?
        .is_some_and(|r| r >= Role::Editor);

    const NO_MODEL: &str = "Chat model not configured. Go to Settings → Models.";
    let settings = utopia_store::settings::get(&state.pool, kb.workspace_id)
        .await?
        .ok_or_else(|| AppError::invalid("no_chat_model", NO_MODEL))?;
    let client = llm_util::chat_client(&settings)
        .ok_or_else(|| AppError::invalid("no_chat_model", NO_MODEL))?;

    let query = req.message.trim().to_string();
    if query.is_empty() {
        return Err(AppError::Validation("Missing user message".into()).into());
    }

    // Conversation persistence: with an id, check ownership; without one, create a
    // conversation titled after the first sentence. The user message is stored immediately, and
    // the context is assembled server-side from the database -- the frontend only sends the new
    // message
    let conversation_id = match req.conversation_id {
        Some(id) => {
            utopia_store::conversations::require_owned(&state.pool, kb_id, user.id, id).await?;
            id
        }
        None => utopia_store::conversations::create(&state.pool, kb_id, user.id, &query).await?,
    };
    utopia_store::conversations::append_message(
        &state.pool,
        conversation_id,
        "user",
        &query,
        &utopia_store::conversations::TurnRecord::empty(),
    )
    .await?;
    let history = utopia_store::conversations::recent_context(
        &state.pool,
        conversation_id,
        MAX_HISTORY as i64,
    )
    .await?;
    let workspace_id = kb.workspace_id;

    // Pull the registry out before the generator: the `async_stream!` below moves `state` in
    // its entirety
    let live = state.live.clone();

    // The generation does not hang off this connection.
    //
    // **Navigating away once loses an answer**, and loses it more thoroughly than it looks: the
    // whole generation lives in the generator below, and the assistant message is only stored
    // when it runs to the end. The moment the browser navigates, axum drops the response body
    // and the generator future is dropped, so the LLM call is cancelled on the spot and that
    // `append_message` never runs. Measured: 1219 bytes of prose had already arrived when the
    // connection was cut, and twenty seconds later the database held only the user's row --
    // **that answer was not there-but-unshown, it was never finished being generated**.
    //
    // So the generator is handed to an independent task to drive, and this connection is
    // demoted to a subscriber. The task does not vanish with the connection: the answer is
    // written out and stored as usual, and it is there when the person comes back.
    //
    // The cost, stated plainly: **we keep spending money while nobody is watching**. That is
    // deliberate -- losing an answer is more expensive than one extra round, and `MAX_ROUNDS`
    // already caps it. A failed send (no receiver left) does not interrupt anything, which is
    // exactly the point.
    let producer = async_stream::stream! {
        let ds_names: Vec<String> = mounted_sources.iter().map(|d| d.name.clone()).collect();
        let tools = tools_schema(can_write, &ds_names);
        let mut system_prompt = if can_write && REMEMBER_ENABLED {
            format!("{SYSTEM_PROMPT}\n{MEMORY_PROMPT}")
        } else {
            SYSTEM_PROMPT.to_string()
        };
        if !ds_names.is_empty() {
            system_prompt.push_str(&format!(
                "\nData: query_data runs read-only SQL (in each source's own dialect) against: {}. \
                 For questions about numbers/metrics, search for the source's schema document \
                 first, then query. State units and the time range you used in the answer.",
                ds_names.join(", ")
            ));
            if !mappings.is_empty() {
                system_prompt.push_str(
                    "\nSemantic layer (confirmed definitions — use these instead of guessing from schema):",
                );
                // This used to dump the whole JSON in. Now that the fields are columns, only
                // the few Ask has any use for are laid out -- `sql` and `expr` are "how it is
                // computed", `unit` is the dimension the answer has to carry, and `summary` is
                // one sentence of plain language for the model
                for m in &mappings {
                    let how = m
                        .sql
                        .as_deref()
                        .or(m.expr.as_deref())
                        .or(m.table_name.as_deref())
                        .unwrap_or("-");
                    let unit = m
                        .unit
                        .as_deref()
                        .map(|u| format!(" [{u}]"))
                        .unwrap_or_default();
                    let note = m
                        .summary
                        .as_deref()
                        .map(|s| format!(" — {s}"))
                        .unwrap_or_default();
                    system_prompt.push_str(&format!(
                        "\n- {} ({}){unit}: {how}{note}",
                        m.concept_name, m.source
                    ));
                }
            }
        }
        let mut msgs: Vec<serde_json::Value> =
            vec![json!({ "role": "system", "content": system_prompt })];
        /* **Put what the previous round did back where it actually happened.**
           The last assistant message is its conclusion; the message carrying `tool_calls` and
           the tool results happened before it, so they go in ahead of it -- the order is the
           real order, and the model reads it as "I was asked, I looked it up, I answered".
           Without this section, across rounds it only sees the prose it wrote itself, so when
           the next message says "translate" it looks everything up again (and may land on a
           different batch of same-named entities). */
        let last_assistant = history
            .turns
            .iter()
            .rposition(|(role, _)| role == "assistant");
        for (i, (role, content)) in history.turns.iter().enumerate() {
            if Some(i) == last_assistant {
                for m in &history.last_tool_exchange {
                    msgs.push(m.clone());
                }
            }
            msgs.push(json!({ "role": role, "content": content }));
        }
        // **The entities identified in earlier rounds, handed back with their ids.**
        //
        // Without this section the model only sees the final answer text of the previous round;
        // it does not know what it searched for or which ids it got, so it searches by name all
        // over again. More insidiously, under name ambiguity two rounds can land on
        // **different entities**, and the two answers are then not about the same node.
        //
        // Pasted after the history and before the current question -- position is compliance,
        // the same reason known_block sits right next to the text in extraction.
        if !history.entities.is_empty() {
            let lines: Vec<String> = history.entities
                .iter()
                .take(KNOWN_ENTITY_LIMIT)
                .map(|e| {
                    format!(
                        "{} | {} | {}",
                        e["id"].as_str().unwrap_or("?"),
                        e["name"].as_str().unwrap_or("?"),
                        e["type"].as_str().unwrap_or("?")
                    )
                })
                .collect();
            msgs.push(json!({
                "role": "user",
                "content": format!(
                    "Entities already identified earlier in this conversation                      (id | name | type). Call entity_facts with these ids directly;                      do not look them up by name again:
    {}",
                    lines.join("
    ")
                )
            }));
        }

        // The conversation id goes out first (this is how the frontend learns about a new one)
        yield Frame::new("conversation", json!({ "id": conversation_id }).to_string());

        // The citation list and the entities identified this round. **Accumulated outside the
        // tools** -- the 3 in `[3]` depends on how many have been cited already, and letting
        // each tool count for itself would give the same chunk two numbers
        let mut sink = tools::ToolSink::default();
        // Accumulated for storage: the assistant's full text and the action trail (for
        // history replay)
        let mut answer_acc = String::new();
        let mut steps_acc: Vec<serde_json::Value> = Vec::new();
        // This round's tool round-trip, a verbatim copy kept for storage: the next round
        // replays it, which is how the model knows what it did
        let mut exchange_acc: Vec<serde_json::Value> = Vec::new();

        let mut rounds = 0usize;
        loop {
            if rounds >= MAX_ROUNDS {
                // Out of ammunition: order the model to answer from the evidence gathered
                // above (streaming)
                msgs.push(json!({
                    "role": "user",
                    "content": "(system) Tool budget exhausted. Answer now from the evidence gathered above.",
                }));
                match client.chat_stream_raw(&msgs).await {
                    Ok(deltas) => {
                        let mut deltas = std::pin::pin!(deltas);
                        while let Some(item) = deltas.next().await {
                            match item {
                                Ok(text) => { answer_acc.push_str(&text); yield delta_event(&text); }
                                Err(e) => { yield error_event(&e.to_string()); return; }
                            }
                        }
                        let _ = utopia_store::conversations::append_message(
                            &state.pool, conversation_id, "assistant", &answer_acc,
                            &utopia_store::conversations::TurnRecord {
                                steps: serde_json::Value::Array(steps_acc.clone()),
                                sources: serde_json::Value::Array(sink.sources.clone()),
                                resolved: serde_json::Value::Array(sink.resolved.clone()),
                                tool_exchange: serde_json::Value::Array(exchange_acc.clone()),
                            },
                        ).await;
                        yield done_event();
                    }
                    Err(e) => yield error_event(&e.to_string()),
                }
                return;
            }

            // The main path streams throughout: prose deltas are forwarded immediately, and
            // tool calls arrive merged at the end of the stream
            let deltas = match client.chat_tools_stream(&msgs, &tools).await {
                Ok(s) => s,
                Err(e) => {
                    if rounds == 0 {
                        // The model may not support tool-calling: degrade to a one-shot RAG
                        // injection
                        tracing::warn!(error = %e, "tool-calling unavailable, using RAG once");
                        let chunks =
                            retrieval::hybrid(&state, kb_id, workspace_id, &query, 8)
                                .await
                                .unwrap_or_default();
                        let legacy_sources: Vec<serde_json::Value> = chunks
                            .iter()
                            .enumerate()
                            .map(|(i, c)| source_json(i + 1, c))
                            .collect();
                        yield Frame::new(
                            "sources",
                            serde_json::to_string(&legacy_sources).unwrap_or_else(|_| "[]".into()),
                        );
                        let mut lmsgs =
                            vec![json!({ "role": "system", "content": legacy_system_prompt(&chunks) })];
                        for (role, content) in &history.turns {
                            lmsgs.push(json!({ "role": role, "content": content }));
                        }
                        match client.chat_stream_raw(&lmsgs).await {
                            Ok(deltas) => {
                                let mut deltas = std::pin::pin!(deltas);
                                while let Some(item) = deltas.next().await {
                                    match item {
                                        Ok(text) => { answer_acc.push_str(&text); yield delta_event(&text); }
                                        Err(e2) => { yield error_event(&e2.to_string()); return; }
                                    }
                                }
                                let _ = utopia_store::conversations::append_message(
                                    &state.pool, conversation_id, "assistant", &answer_acc,
                                    &utopia_store::conversations::TurnRecord {
                                        steps: serde_json::Value::Array(steps_acc.clone()),
                                        sources: serde_json::Value::Array(legacy_sources.clone()),
                                        resolved: serde_json::Value::Array(sink.resolved.clone()),
                                        tool_exchange: serde_json::Value::Array(exchange_acc.clone()),
                                    },
                                ).await;
                                yield done_event();
                            }
                            Err(e2) => yield error_event(&e2.to_string()),
                        }
                        return;
                    }
                    yield error_event(&e.to_string());
                    return;
                }
            };
            let mut turn: Option<utopia_llm::AssistantTurn> = None;
            {
                let mut deltas = std::pin::pin!(deltas);
                while let Some(item) = deltas.next().await {
                    match item {
                        Ok(utopia_llm::ToolStreamItem::Delta(text)) => {
                            answer_acc.push_str(&text);
                            yield delta_event(&text);
                        }
                        Ok(utopia_llm::ToolStreamItem::Turn(t)) => turn = Some(t),
                        Err(e) => {
                            yield error_event(&e.to_string());
                            return;
                        }
                    }
                }
            }
            let Some(turn) = turn else {
                yield error_event("LLM stream ended unexpectedly");
                return;
            };

            if turn.tool_calls.is_empty() {
                if answer_acc.is_empty() {
                    yield error_event("Model returned an empty answer");
                } else {
                    let _ = utopia_store::conversations::append_message(
                        &state.pool, conversation_id, "assistant", &answer_acc,
                        &utopia_store::conversations::TurnRecord {
                            steps: serde_json::Value::Array(steps_acc.clone()),
                            sources: serde_json::Value::Array(sink.sources.clone()),
                            resolved: serde_json::Value::Array(sink.resolved.clone()),
                            tool_exchange: serde_json::Value::Array(exchange_acc.clone()),
                        },
                    ).await;
                    yield done_event();
                }
                return;
            }

            // The tool round carried narration text: add a paragraph break between it and the
            // prose of the rounds that follow
            if turn.content.is_some() && !answer_acc.is_empty() {
                answer_acc.push_str("\n\n");
                yield delta_event("\n\n");
            }

            let call_msg = turn.to_message();
            exchange_acc.push(call_msg.clone());
            msgs.push(call_msg);
            for call in &turn.tool_calls {
                // **A call that cannot say what it means to do is not run.** Hand the words
                // back to the model and let it start over
                let args = match check_call(&tools, &call.name, &call.arguments) {
                    Ok(args) => args,
                    Err((message, step)) => {
                        steps_acc.push(step.clone());
                        yield Frame::new("step", serde_json::to_string(&step).unwrap_or_default());
                        msgs.push(tool_result_message(&call.id, &message));
                        continue;
                    }
                };
                let ctx = tools::ToolCtx {
                    state: &state,
                    kb_id,
                    workspace_id,
                    mounted_sources: &mounted_sources,
                    can_write,
                    actor: Some(user.id),
                };
                let (result, step) = tools::dispatch(&ctx, &mut sink, &call.name, &args).await;
                // **Where in the prose this step happened.**
                //
                // The model talks and calls as it goes: say a sentence, look something up, say
                // another. On SSE, `delta` and `step` are already emitted alternately, so the
                // order needs no extra bookkeeping -- but **history replay has no such
                // timeline**: all that is stored is the assembled prose and a flat steps
                // array, so reopening a conversation piles every call at the very front of the
                // prose, reading as if it had looked things up seven times and then said
                // everything in one breath. Recording the offset is what lets replay break the
                // prose apart again.
                //
                // The unit is **UTF-16 code units**, because the splitting happens in the
                // browser, and that is what JS's `String.prototype.length` counts. A byte count
                // or `chars()` both cut in the wrong place on Chinese and emoji
                let mut step = step;
                if let Some(obj) = step.as_object_mut() {
                    obj.insert("at".into(), json!(answer_acc.encode_utf16().count()));
                }
                steps_acc.push(step.clone());
                yield Frame::new("step", serde_json::to_string(&step).unwrap_or_default());
                if step["kind"] == "search" || step["kind"] == "docs" {
                    yield Frame::new(
                        "sources",
                        serde_json::to_string(&sink.sources).unwrap_or_else(|_| "[]".into()),
                    );
                }
                let result_msg = tool_result_message(&call.id, &result);
                exchange_acc.push(result_msg.clone());
                msgs.push(result_msg);
            }
            rounds += 1;
        }
    };

    // The generation is registered, and then **this connection merely goes and "attaches" to
    // it** -- the same code path as the reconnect after a refresh. Written as two separate
    // paths, sooner or later only one of them would be right
    let handle = live.begin(conversation_id).await;
    let attached = live.attach(conversation_id).await;
    tokio::spawn(async move {
        let mut producer = std::pin::pin!(producer);
        while let Some(frame) = producer.next().await {
            // Having no subscriber is normal (the person left). **Emit regardless**: stopping
            // here would bring back "navigate away once, lose an answer" exactly as it was
            handle.emit(frame).await;
        }
        // Anyone attaching after deregistration gets "nothing is running", and by then the
        // answer is already stored
        handle.finish().await;
    });

    Ok(sse_from(attached))
}

/// Turn one "attach" into SSE: a snapshot first, then the deltas as usual.
///
/// `None` = no generation is running for this conversation. Reply with an `idle` rather than a
/// 404 -- **the client asks once every time a conversation is opened**, and "nothing is
/// running" is the most common answer, not an error
fn sse_from(
    attached: Option<(
        crate::live::Snapshot,
        tokio::sync::broadcast::Receiver<Frame>,
    )>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let stream = async_stream::stream! {
        let Some((snapshot, mut rx)) = attached else {
            yield to_event(&Frame::new("idle", "{}".into()));
            return;
        };
        yield to_event(&snapshot.to_frame());
        loop {
            match rx.recv().await {
                Ok(frame) => {
                    let done = frame.event == "done" || frame.event == "error";
                    yield to_event(&frame);
                    if done { return; }
                }
                // Generation over, the sender destroyed: a normal ending
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                // This client read too slowly and the broadcast buffer left it behind. **Say
                // so** -- carrying on silently would leave it missing a stretch in the middle
                // without knowing
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    yield to_event(&Frame::new(
                        "error",
                        format!("Fell behind the stream by {n} messages; reopen the conversation"),
                    ));
                    return;
                }
            }
        }
    };
    Sse::new(stream).keep_alive(KeepAlive::default())
}

fn to_event(frame: &Frame) -> Result<Event, Infallible> {
    Ok(Event::default().event(frame.event).data(&frame.data))
}

/// Attach to a generation that is still running (this is the path after a page refresh).
pub async fn reattach(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path((kb_id, conversation_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Sse<impl Stream<Item = Result<Event, Infallible>>>> {
    utopia_store::access::require_kb(&state.pool, &user, kb_id, Role::Viewer).await?;
    // **Ownership must be checked.** A conversation id can be guessed, and this stream would
    // read someone else's answer out word for word
    utopia_store::conversations::require_owned(&state.pool, kb_id, user.id, conversation_id)
        .await?;
    Ok(sse_from(state.live.attach(conversation_id).await))
}

/// The generator yields `Frame`, not `axum`'s `Event`.
/// **Both the broadcast and the snapshot need to read an event's contents back**, and an
/// `Event` cannot be read back (see `live`)
fn delta_event(text: &str) -> Frame {
    Frame::new(
        "delta",
        serde_json::to_string(&json!({ "text": text })).unwrap_or_default(),
    )
}

fn done_event() -> Frame {
    Frame::new("done", "{}".into())
}

fn error_event(message: &str) -> Frame {
    Frame::new("error", message.into())
}

fn source_json(n: usize, c: &ChunkView) -> serde_json::Value {
    json!({
        "n": n,
        "chunk_id": c.id,
        "document_id": c.document_id,
        "filename": c.filename,
        "excerpt": truncate(&c.text, 160),
    })
}

/// The system prompt for the degraded path (the one-shot injection when tool-calling is
/// unavailable).
fn legacy_system_prompt(chunks: &[ChunkView]) -> String {
    if chunks.is_empty() {
        return "You are an enterprise knowledge base assistant. No relevant sources were \
                retrieved for this question. Tell the user the knowledge base lacks material \
                on this topic, answer cautiously from general knowledge, and clearly separate \
                sourced statements from speculation. Always respond in the same language as \
                the user's question."
            .to_string();
    }
    let mut prompt = String::from(
        "You are an enterprise knowledge base assistant. Answer strictly based on the \
         numbered sources below. When a source supports a statement, append its citation \
         number, e.g. [1] or [2], at the end of the sentence. If the sources are \
         insufficient, say so explicitly — never fabricate. Always respond in the same \
         language as the user's question.\n\n### Sources\n",
    );
    for (i, c) in chunks.iter().enumerate() {
        prompt.push_str(&format!(
            "\n[{}] \"{}\" section {}:\n{}\n",
            i + 1,
            c.filename,
            c.seq + 1,
            c.text
        ));
    }
    prompt
}

fn truncate(text: &str, max_chars: usize) -> String {
    let t = text.trim();
    if t.chars().count() <= max_chars {
        t.to_string()
    } else {
        let cut: String = t.chars().take(max_chars).collect();
        format!("{cut}…")
    }
}

// ---------------------------------------------------------------------------
// Conversation management (list / replay / delete)
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
pub struct ConversationsQuery {
    /// Searches both the title and the message bodies: what a person remembers is usually the
    /// sentence they asked, not the title
    #[serde(default)]
    pub q: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub offset: Option<i64>,
}

pub async fn list_conversations(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(kb_id): Path<Uuid>,
    Query(q): Query<ConversationsQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    utopia_store::access::require_kb(&state.pool, &user, kb_id, Role::Viewer).await?;
    let (conversations, total) = utopia_store::conversations::list(
        &state.pool,
        kb_id,
        user.id,
        q.q.as_deref().map(str::trim).filter(|s| !s.is_empty()),
        q.limit.unwrap_or(30).clamp(1, 100),
        q.offset.unwrap_or(0).max(0),
    )
    .await?;
    Ok(Json(
        json!({ "conversations": conversations, "total": total }),
    ))
}

#[derive(serde::Deserialize)]
pub struct RenameConversationReq {
    pub title: String,
}

/// Rename a conversation.
///
/// **The title is taken automatically from the first sentence**, and a conversation drifting
/// off it is the norm -- renaming lets a person find it again the way they remember it.
pub async fn rename_conversation(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path((kb_id, conversation_id)): Path<(Uuid, Uuid)>,
    Json(req): Json<RenameConversationReq>,
) -> ApiResult<Json<serde_json::Value>> {
    utopia_store::access::require_kb(&state.pool, &user, kb_id, Role::Viewer).await?;
    utopia_store::conversations::rename(&state.pool, kb_id, user.id, conversation_id, &req.title)
        .await?;
    Ok(Json(json!({ "ok": true })))
}

/// History replay: messages carry the stored action trail (steps) and citations (sources).
pub async fn conversation_detail(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path((kb_id, conversation_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<serde_json::Value>> {
    utopia_store::access::require_kb(&state.pool, &user, kb_id, Role::Viewer).await?;
    utopia_store::conversations::require_owned(&state.pool, kb_id, user.id, conversation_id)
        .await?;
    let messages = utopia_store::conversations::messages(&state.pool, conversation_id).await?;
    Ok(Json(json!({ "messages": messages })))
}

pub async fn delete_conversation(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path((kb_id, conversation_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<serde_json::Value>> {
    utopia_store::access::require_kb(&state.pool, &user, kb_id, Role::Viewer).await?;
    utopia_store::conversations::delete(&state.pool, kb_id, user.id, conversation_id).await?;
    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- check_call ---------------------------------------------------------

    /// Arguments cut off mid-way -- this is exactly what it looks like when the model hits the
    /// token limit.
    ///
    /// **This used to fall back to an empty object, and then `search_chunks` went searching
    /// with the user's own sentence.** What came out was a perfectly normal-looking
    /// `search · 6 sources`, and an answer based on the wrong input. Nobody checks an answer
    /// that looks normal, so this one has to be a refusal
    #[test]
    fn arguments_cut_off_mid_json_do_not_become_a_search() {
        let tools = tools_schema(false, &[]);
        let err = check_call(&tools, "search_chunks", "{\"query\": \"Acme reven")
            .expect_err("truncated JSON must be refused, not fallen back on");
        assert_eq!(
            err.1["kind"], "tool",
            "the trail must show a call that did not run"
        );
        assert_eq!(err.1["detail"], "bad arguments");
        assert!(
            err.0.contains("not valid JSON") && err.0.contains("again"),
            "the reply to the model must say it did not run and tell it to retry: {}",
            err.0
        );
    }

    /// An empty string and a missing field are the same thing: what `{"query": ""}` retrieves
    /// has nothing to do with the question, and it shows up as a normal trail entry all the
    /// same
    #[test]
    fn an_empty_required_argument_counts_as_missing() {
        let tools = tools_schema(false, &[]);
        for raw in [
            "{}",
            "{\"query\": \"\"}",
            "{\"query\": \"   \"}",
            "{\"query\": null}",
        ] {
            let Err(err) = check_call(&tools, "search_chunks", raw) else {
                panic!("{raw} should be refused");
            };
            assert_eq!(err.1["detail"], "missing query", "{raw}");
        }
    }

    /// **The criteria come from the tool schema itself.** This one guards against "a required
    /// argument was added but the validation was forgotten" -- query_data only appears in the
    /// schema when a data source is mounted, and its two required arguments have never been
    /// written down separately anywhere else
    #[test]
    fn the_schema_is_the_only_place_required_is_written_down() {
        let tools = tools_schema(false, &["warehouse".into()]);
        let err = check_call(&tools, "query_data", "{\"data_source\": \"warehouse\"}")
            .expect_err("a missing sql must be refused");
        assert_eq!(err.1["detail"], "missing sql");
        check_call(
            &tools,
            "query_data",
            "{\"data_source\": \"warehouse\", \"sql\": \"SELECT 1\"}",
        )
        .expect("with both given it should pass");
    }

    /// A well-formed call passes through untouched, **with not one optional argument lost**.
    ///
    /// This gate only judges "was it said clearly", it does not filter -- reassembling args
    /// would mean every new optional argument has to be remembered here too, and the
    /// consequence of forgetting is that it silently stops working
    #[test]
    fn a_well_formed_call_passes_through_untouched() {
        let tools = tools_schema(false, &[]);
        let args = check_call(
            &tools,
            "entity_facts",
            "{\"entity_id\": \"1f8ac10b-58cc-4372-a567-0e02b2c3d479\", \"at\": \"2026-03-15\"}",
        )
        .expect("with the required ones given it should pass");
        assert_eq!(
            args["at"], "2026-03-15",
            "an optional argument must not be eaten here"
        );
    }

    /// The full text can only be read by id, and the id is copied by the model out of the
    /// search results -- copying it wrong and leaving it out both have to stop here
    #[test]
    fn get_document_without_an_id_does_not_run() {
        let tools = tools_schema(false, &[]);
        let err = check_call(&tools, "get_document", "{}")
            .expect_err("a missing document_id must be refused");
        assert_eq!(err.1["detail"], "missing document_id");
    }

    /// An id that is not a uuid can only come back from the tool as "this base has no such
    /// document" -- the model reads that as "the base really does not have it" and stops, so
    /// one mis-copied id turns into a negative answer
    #[test]
    fn a_document_id_that_is_not_a_uuid_does_not_run() {
        let tools = tools_schema(false, &[]);
        for raw in [
            "{\"document_id\": \"Notes- 'Scrum' 3 Sept 2026.txt\"}",
            "{\"document_id\": \"1f8ac10b-58cc-4372\"}",
        ] {
            let Err(err) = check_call(&tools, "get_document", raw) else {
                panic!("{raw} should be refused");
            };
            assert_eq!(err.1["detail"], "invalid document_id", "{raw}");
        }
        check_call(
            &tools,
            "get_document",
            "{\"document_id\": \"1f8ac10b-58cc-4372-a567-0e02b2c3d479\"}",
        )
        .expect("a real uuid should pass");
    }

    /// `changes` has long refused a missing `since` in its own branch -- **it is the only one
    /// that got this right**. This pins down two things: the new shared gate agrees with it,
    /// and its own check on the date format (things like `2026-13-45`) still has to stay,
    /// because check_call only looks at whether something is there, not whether it is right
    #[test]
    fn the_one_tool_that_already_refused_still_refuses() {
        let tools = tools_schema(false, &[]);
        assert!(check_call(&tools, "changes", "{}").is_err());
        check_call(&tools, "changes", "{\"since\": \"2026-13-45\"}")
            .expect("a malformed date is not this gate's job, that is changes_window's");
    }
}
