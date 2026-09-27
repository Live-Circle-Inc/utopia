//! Chat session persistence: the conversation/message repository. The trace (steps) and the
//! citations (sources) are persisted together with the assistant message, and history replay and
//! the live stream share the same data shape.

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use utopia_core::models::{ConversationMessage, ConversationView};
use utopia_core::{AppError, AppResult};
use uuid::Uuid;

pub async fn create(pool: &PgPool, kb_id: Uuid, user_id: Uuid, title: &str) -> AppResult<Uuid> {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO conversations (id, kb_id, user_id, title) VALUES ($1, $2, $3, $4)")
        .bind(id)
        .bind(kb_id)
        .bind(user_id)
        .bind(title.chars().take(80).collect::<String>())
        .execute(pool)
        .await?;
    Ok(id)
}

/// My conversations in this database. **Searchable and paginated** -- titles repeat (ask the same
/// question twice and there they are), and conversations past a fixed hundred simply do not exist
/// as far as the UI is concerned.
///
/// The search covers two places, the title and the message body: what a person remembers is
/// usually "I asked that thing about Q3", and that sentence is in the body, while the title may
/// have been truncated into something else entirely.
pub async fn list(
    pool: &PgPool,
    kb_id: Uuid,
    user_id: Uuid,
    q: Option<&str>,
    limit: i64,
    offset: i64,
) -> AppResult<(Vec<ConversationView>, i64)> {
    const WHERE: &str = "WHERE c.kb_id = $1 AND c.user_id = $2
           AND ($3::text IS NULL
                OR c.title ILIKE '%' || $3 || '%'
                OR EXISTS (SELECT 1 FROM conversation_messages m
                            WHERE m.conversation_id = c.id
                              AND m.content ILIKE '%' || $3 || '%'))";
    let rows: Vec<ConversationView> = sqlx::query_as(&format!(
        "SELECT c.id, c.title, c.created_at, c.updated_at,
                (SELECT count(*) FROM conversation_messages m
                 WHERE m.conversation_id = c.id) AS message_count
         FROM conversations c
         {WHERE}
         ORDER BY c.updated_at DESC
         LIMIT $4 OFFSET $5"
    ))
    .bind(kb_id)
    .bind(user_id)
    .bind(q)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await?;
    let (total,): (i64,) = sqlx::query_as(&format!("SELECT count(*) FROM conversations c {WHERE}"))
        .bind(kb_id)
        .bind(user_id)
        .bind(q)
        .fetch_one(pool)
        .await?;
    Ok((rows, total))
}

/// Rename a conversation.
///
/// **The title was taken automatically from the first sentence**, and that sentence is often not
/// what the conversation later turned into -- a conversation drifting off is the norm, and
/// renaming lets a person find it again the way they remember it.
pub async fn rename(
    pool: &PgPool,
    kb_id: Uuid,
    user_id: Uuid,
    conversation_id: Uuid,
    title: &str,
) -> AppResult<()> {
    let title = title.trim();
    if title.is_empty() || title.chars().count() > 120 {
        return Err(AppError::invalid(
            "bad_title",
            "Title must be 1-120 characters",
        ));
    }
    let res = sqlx::query(
        "UPDATE conversations SET title = $4
          WHERE id = $3 AND kb_id = $1 AND user_id = $2",
    )
    .bind(kb_id)
    .bind(user_id)
    .bind(conversation_id)
    .bind(title)
    .execute(pool)
    .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    Ok(())
}

/// Ownership check: the conversation must belong to this KB and this person.
pub async fn require_owned(
    pool: &PgPool,
    kb_id: Uuid,
    user_id: Uuid,
    conversation_id: Uuid,
) -> AppResult<()> {
    let found: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM conversations WHERE id = $1 AND kb_id = $2 AND user_id = $3",
    )
    .bind(conversation_id)
    .bind(kb_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await?;
    found.map(|_| ()).ok_or(AppError::NotFound)
}

pub async fn messages(pool: &PgPool, conversation_id: Uuid) -> AppResult<Vec<ConversationMessage>> {
    let rows: Vec<ConversationMessage> = sqlx::query_as(
        "SELECT id, role, content, steps, sources, created_at
         FROM conversation_messages WHERE conversation_id = $1 ORDER BY created_at",
    )
    .bind(conversation_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// The history for one round of replay.
///
/// Three things, each answering a different question: the body (what was said), the entities (who
/// was pinned down), and the most recent round of tool round-trips (**what was done**).
///
/// The third was added later. The original judgement was "replaying identity is enough: with the
/// id in hand, the next round calls entity_facts directly" -- what that saved was the chunk
/// bodies piling up every round, and that consideration was not wrong. But it also saved away the
/// fact that "I already looked this up": across rounds the model could only see its own prose, so
/// when the follow-up was "translate it" it looked everything up again, and landed on a different
/// batch of same-named entities.
///
/// The compromise is **replaying only the most recent round**: what is needed is "what I just
/// did", not the output of twenty rounds.
pub struct History {
    /// `(role, content)`, in time order
    pub turns: Vec<(String, String)>,
    /// The entities already pinned down in this conversation (deduplicated)
    pub entities: Vec<serde_json::Value>,
    /// **What the assistant did in the most recent round**: the assistant message carrying
    /// `tool_calls` plus the matching tool results.
    ///
    /// The most recent round only. This section exists so the model knows what it just did -- when
    /// the follow-up is "translate it" or "make it shorter", the evidence is right there and it
    /// does not have to look it up again (and so cannot look it up into a different batch of
    /// same-named entities). Hauling twenty rounds of tool output back is another matter, and that
    /// is exactly why only the body was stored to begin with.
    pub last_tool_exchange: Vec<serde_json::Value>,
}

pub async fn recent_context(pool: &PgPool, conversation_id: Uuid, n: i64) -> AppResult<History> {
    let mut rows: Vec<(
        String,
        String,
        serde_json::Value,
        serde_json::Value,
        DateTime<Utc>,
    )> = sqlx::query_as(
        "SELECT role, content, resolved, tool_exchange, created_at FROM conversation_messages
         WHERE conversation_id = $1 ORDER BY created_at DESC LIMIT $2",
    )
    .bind(conversation_id)
    .bind(n)
    .fetch_all(pool)
    .await?;
    rows.reverse();
    // Deduplicate entities by id while keeping first-appearance order: the same entity recurring
    // over several rounds is the norm, and listing it again each round is just saying the same
    // thing three times
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut entities: Vec<serde_json::Value> = Vec::new();
    for (_, _, res, _, _) in &rows {
        for e in res.as_array().into_iter().flatten() {
            let Some(id) = e["id"].as_str() else { continue };
            if seen.insert(id.to_string()) {
                entities.push(e.clone());
            }
        }
    }
    // The section from the last assistant message. **Searched backwards** -- the last message is
    // usually the user message that was just persisted
    let last_tool_exchange = rows
        .iter()
        .rev()
        .find(|(role, _, _, _, _)| role == "assistant")
        .and_then(|(_, _, _, ex, _)| ex.as_array().cloned())
        .unwrap_or_default();
    Ok(History {
        turns: rows.into_iter().map(|(r, c, _, _, _)| (r, c)).collect(),
        entities,
        last_tool_exchange,
    })
}

/// What a round leaves behind besides the body.
///
/// **All four are `serde_json::Value`, and passed loose the compiler cannot help you** -- get the
/// order wrong and you get a record that persists fine, reads back fine, and merely has the wrong
/// content under every heading. Same reasoning as `RelationAxioms`.
#[derive(Default)]
pub struct TurnRecord {
    /// The action trace: what was called and how much came back (shown in the UI)
    pub steps: serde_json::Value,
    /// The citation list
    pub sources: serde_json::Value,
    /// The entities pinned down this round (id / name / type). Replayed next round so the model
    /// carries on instead of searching again
    pub resolved: serde_json::Value,
    /// What was called this round and what came back (the already-truncated copy). The most
    /// recent one is replayed next round -- without it the model does not know, across rounds,
    /// that it already looked something up, so it looks it up again
    pub tool_exchange: serde_json::Value,
}

impl TurnRecord {
    /// A user message: all four empty.
    pub fn empty() -> Self {
        Self {
            steps: serde_json::json!([]),
            sources: serde_json::json!([]),
            resolved: serde_json::json!([]),
            tool_exchange: serde_json::json!([]),
        }
    }
}

pub async fn append_message(
    pool: &PgPool,
    conversation_id: Uuid,
    role: &str,
    content: &str,
    rec: &TurnRecord,
) -> AppResult<Uuid> {
    let id = Uuid::now_v7();
    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO conversation_messages
             (id, conversation_id, role, content, steps, sources, resolved, tool_exchange)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(id)
    .bind(conversation_id)
    .bind(role)
    .bind(content)
    .bind(&rec.steps)
    .bind(&rec.sources)
    .bind(&rec.resolved)
    .bind(&rec.tool_exchange)
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE conversations SET updated_at = now() WHERE id = $1")
        .bind(conversation_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(id)
}

pub async fn delete(
    pool: &PgPool,
    kb_id: Uuid,
    user_id: Uuid,
    conversation_id: Uuid,
) -> AppResult<()> {
    let res =
        sqlx::query("DELETE FROM conversations WHERE id = $1 AND kb_id = $2 AND user_id = $3")
            .bind(conversation_id)
            .bind(kb_id)
            .bind(user_id)
            .execute(pool)
            .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    Ok(())
}
