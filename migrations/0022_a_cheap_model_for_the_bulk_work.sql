-- Configure extraction separately from chat.
--
-- A single `chat_model` field used to serve seven callers: chat (api/chat.rs),
-- extraction (extraction.rs), adjudication, type resolution, semantic-layer
-- mappings, ontology suggestions, and the connectivity probe. Those seven do not
-- want the same thing -- **chat is the hardest of them, extraction is the most
-- expensive**.
--
-- Chat has to interleave narration with seven tools and hand uuids back unchanged
-- across rounds (`check_call`'s uuid guard rejects invented ids, and a rejection
-- costs another request). Extraction re-runs one prompt over every chunk of a
-- document, leaning on a byte-identical system prefix to hit the provider's prefix
-- cache: high volume, simple judgement.
--
-- One field serving both ends can only be pinned by the harder one, while the bill
-- is set by the one a cheap model would have handled. Split apart, the strong model
-- stays on chat and extraction moves to something cheap and fast -- and the saving
-- is multiplied by the chunk count.
--
-- **Leave blank to follow chat** (see `LlmSettings::effective_extract`): deployments
-- already running need no settings change and behave exactly as before.
ALTER TABLE llm_settings
    ADD COLUMN extract_base_url TEXT,
    ADD COLUMN extract_api_key  TEXT,
    ADD COLUMN extract_model    TEXT;

COMMENT ON COLUMN llm_settings.extract_model IS
    'Extraction-only model; takes effect only when extract_base_url is also set, otherwise falls back to chat_*';
