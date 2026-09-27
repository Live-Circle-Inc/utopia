use sqlx::PgPool;
use utopia_core::models::LlmSettings;
use utopia_core::AppResult;
use uuid::Uuid;

pub async fn get(pool: &PgPool, workspace_id: Uuid) -> AppResult<Option<LlmSettings>> {
    let row = sqlx::query_as("SELECT * FROM llm_settings WHERE workspace_id = $1")
        .bind(workspace_id)
        .fetch_optional(pool)
        .await?;
    Ok(row)
}

/// 任取一个配了对话模型的工作区设置。给端点探针用：端点地址是部署共用的，
/// 从哪个工作区的配置读到的都是同一个地方，而探针没有"当前工作区"这个上下文。
pub async fn any_with_chat(pool: &PgPool) -> AppResult<Option<LlmSettings>> {
    let row = sqlx::query_as(
        "SELECT * FROM llm_settings
         WHERE chat_base_url IS NOT NULL AND chat_model IS NOT NULL
         ORDER BY workspace_id LIMIT 1",
    )
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// One role's triple: address, key, model.
///
/// **Named fields rather than positional arguments.** Flattened, the three roles
/// are nine consecutive `Option<&str>` -- get the order wrong and the compiler
/// says nothing, storing a record that inserts and reads back cleanly with the
/// embedding model name sitting in the extraction column. Same reasoning as
/// `TurnRecord`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Endpoint<'a> {
    pub base_url: Option<&'a str>,
    /// `None` or empty = keep the existing key (the frontend never sends keys back)
    pub api_key: Option<&'a str>,
    pub model: Option<&'a str>,
}

/// Input to upsert. Extraction left blank means follow chat -- the fallback lives
/// on the read side (see `LlmSettings::effective_extract`); this stores what was
/// given rather than guessing on the admin's behalf.
#[derive(Debug, Clone, Copy, Default)]
pub struct LlmSettingsInput<'a> {
    pub chat: Endpoint<'a>,
    pub extract: Endpoint<'a>,
    pub embed: Endpoint<'a>,
    pub embed_dim: Option<i32>,
}

/// Upsert; passing `None` for a role's api_key keeps the existing value (the
/// frontend never sends keys back).
pub async fn upsert(
    pool: &PgPool,
    workspace_id: Uuid,
    input: LlmSettingsInput<'_>,
) -> AppResult<LlmSettings> {
    let row = sqlx::query_as(
        "INSERT INTO llm_settings
             (workspace_id, chat_base_url, chat_api_key, chat_model,
              extract_base_url, extract_api_key, extract_model,
              embed_base_url, embed_api_key, embed_model, embed_dim, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, now())
         ON CONFLICT (workspace_id) DO UPDATE SET
             chat_base_url    = EXCLUDED.chat_base_url,
             chat_api_key     = COALESCE(EXCLUDED.chat_api_key, llm_settings.chat_api_key),
             chat_model       = EXCLUDED.chat_model,
             extract_base_url = EXCLUDED.extract_base_url,
             extract_api_key  = COALESCE(EXCLUDED.extract_api_key, llm_settings.extract_api_key),
             extract_model    = EXCLUDED.extract_model,
             embed_base_url   = EXCLUDED.embed_base_url,
             embed_api_key    = COALESCE(EXCLUDED.embed_api_key, llm_settings.embed_api_key),
             embed_model      = EXCLUDED.embed_model,
             embed_dim        = EXCLUDED.embed_dim,
             updated_at       = now()
         RETURNING *",
    )
    .bind(workspace_id)
    .bind(input.chat.base_url)
    .bind(input.chat.api_key)
    .bind(input.chat.model)
    .bind(input.extract.base_url)
    .bind(input.extract.api_key)
    .bind(input.extract.model)
    .bind(input.embed.base_url)
    .bind(input.embed.api_key)
    .bind(input.embed.model)
    .bind(input.embed_dim)
    .fetch_one(pool)
    .await?;
    Ok(row)
}
