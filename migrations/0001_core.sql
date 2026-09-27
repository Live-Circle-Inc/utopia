-- Core: the multi-tenant tables, the job queue, access control and deployment configuration.
-- The pgvector extension is created up front (P1's chunks.embedding depends on it); the
-- pgvector/pgvector image ships with it
CREATE EXTENSION IF NOT EXISTS vector;

CREATE TABLE organizations (
    id          UUID PRIMARY KEY,
    name        TEXT NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE users (
    id            UUID PRIMARY KEY,
    org_id        UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    -- **Uniqueness only constrains active accounts**, see the partial index below. Once
    -- deactivated the address is released again -- otherwise "deactivate" would mean "this
    -- email address is permanently scrapped", and the same person coming back could not even
    -- create a new account
    email         TEXT NOT NULL,
    password_hash TEXT NOT NULL,
    display_name  TEXT NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- The system administrator in a single-tenant deployment. The first person to register
    -- becomes one automatically (see accounts.rs)
    is_admin      BOOLEAN NOT NULL DEFAULT FALSE,
    -- **A soft delete, not a DELETE.** The `actor_id` on audit events, merge logs, retyping
    -- ledgers and rule confirmations all point at this person, and those are audit material --
    -- once someone has left we still have to be able to answer "who did that at the time".
    -- Deactivation only cuts off access: `find_user_by_email` and `find_user_by_id` each carry
    -- a `deactivated_at IS NULL` clause, the former blocking login and the latter blocking
    -- already-issued tokens (session validation goes through it, so deactivation takes effect
    -- immediately)
    deactivated_at TIMESTAMPTZ,
    -- Who deactivated them. A bare foreign key -- the deactivator may themselves be
    -- deactivated later, and that record still has to be there
    deactivated_by UUID REFERENCES users(id)
);

-- Emails are unique, **but only among the active**. Deactivated accounts may contain duplicate
-- addresses, so any query that looks a person up by email has to carry
-- `deactivated_at IS NULL` -- which it had to carry anyway (otherwise a deactivated person
-- could still log in), and here that same clause doubles as the correctness guarantee.
CREATE UNIQUE INDEX users_email_active_idx ON users (email) WHERE deactivated_at IS NULL;

CREATE TABLE workspaces (
    id          UUID PRIMARY KEY,
    org_id      UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    name        TEXT NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE memberships (
    user_id      UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    workspace_id UUID NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    role         TEXT NOT NULL CHECK (role IN ('owner', 'admin', 'editor', 'viewer')),
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, workspace_id)
);
CREATE INDEX memberships_workspace_idx ON memberships (workspace_id);

CREATE TABLE knowledge_bases (
    id           UUID PRIMARY KEY,
    workspace_id UUID NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    name         TEXT NOT NULL,
    kind         TEXT NOT NULL DEFAULT 'knowledge' CHECK (kind IN ('knowledge', 'memory')),
    description  TEXT,
    -- The deployment's shared default space (the first KB created in a workspace): always
    -- open and undeletable (enforced by the API)
    is_default   BOOLEAN NOT NULL DEFAULT FALSE,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- With no matrix row, an open KB falls back to the deployment role; a restricted KB is only
    -- visible to the members in kb_members (outsiders get NotFound)
    visibility   TEXT NOT NULL DEFAULT 'open'
                 CHECK (visibility IN ('open', 'restricted')),
    -- Whether to extend the ontology on the user's behalf. **An explicit switch rather than
    -- something inferred from behaviour**: this used to be decided by "has the ontology been
    -- touched", and inferring it wrong gets absurd -- clicking Add once on a proposal would
    -- permanently turn the suggestions off, because that records an ontology action with an
    -- actor. And once it was false it could never become true again, freezing the ontology on
    -- whatever vocabulary the first batch of documents happened to contain, when the sources
    -- keep feeding in documents every day
    auto_extend_ontology BOOLEAN NOT NULL DEFAULT TRUE,
    -- **This is not "the system language"** (see docs/decisions/0004). The UI language lives in
    -- the client; the backend has no locale. What this column governs is **the language of the
    -- corpus**: a class's description goes verbatim into the extraction prompt, and its reader
    -- is the model that is reading your documents -- when the description and the text being
    -- judged are in the same language, the judgement is steadier. So when a Chinese team reads
    -- English technical documents, the UI should be Chinese while this column should be 'en';
    -- one switch cannot press down on both of those.
    --
    -- The accepted values are pinned in a CHECK rather than in the application layer: this
    -- column is used to pick a compile-time constant table, and writing a value with no
    -- corresponding table silently falls back to English without raising an error -- and that
    -- is the hardest kind of mistake to track down
    ontology_lang TEXT NOT NULL DEFAULT 'en',
    -- The default KB is always open (the rule is enforced in the API; this is the DB-level
    -- belt and braces)
    CONSTRAINT kb_default_open CHECK (NOT is_default OR visibility = 'open'),
    CONSTRAINT knowledge_bases_ontology_lang_chk CHECK (ontology_lang IN ('en', 'zh'))
);
CREATE INDEX knowledge_bases_workspace_idx ON knowledge_bases (workspace_id);

-- The job queue: consumed with FOR UPDATE SKIP LOCKED, see section 2 of docs/DESIGN.md
CREATE TABLE jobs (
    id           BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    kind         TEXT NOT NULL,
    payload      JSONB NOT NULL DEFAULT '{}',
    status       TEXT NOT NULL DEFAULT 'queued' CHECK (status IN ('queued', 'running', 'done', 'failed')),
    attempts     INT NOT NULL DEFAULT 0,
    max_attempts INT NOT NULL DEFAULT 3,
    run_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    locked_at    TIMESTAMPTZ,
    last_error   TEXT,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX jobs_claim_idx ON jobs (run_at) WHERE status = 'queued';

-- Workspace-level LLM settings (chat and embedding are configured separately, over the
-- OpenAI-compatible protocol)
CREATE TABLE llm_settings (
    workspace_id   UUID PRIMARY KEY REFERENCES workspaces(id) ON DELETE CASCADE,
    chat_base_url  TEXT,
    chat_api_key   TEXT,
    chat_model     TEXT,
    embed_base_url TEXT,
    embed_api_key  TEXT,
    embed_model    TEXT,
    embed_dim      INT,
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- KB-level access control. Deployment roles hang off the invisible workspace (memberships are
-- left alone); every KB carries its own role matrix, configured in that KB's own Settings
CREATE TABLE kb_members (
    kb_id      UUID NOT NULL REFERENCES knowledge_bases(id) ON DELETE CASCADE,
    user_id    UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role       TEXT NOT NULL CHECK (role IN ('viewer', 'editor', 'admin')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- Who added this member (left NULL when there is nobody to attribute it to, and the
    -- display degrades to just the time)
    added_by   uuid REFERENCES users(id) ON DELETE SET NULL,
    PRIMARY KEY (kb_id, user_id)
);

CREATE TABLE deployment_settings (
    singleton         BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
    open_registration BOOLEAN NOT NULL DEFAULT TRUE,
    -- Job worker concurrency (changeable in system settings; the scheduling loop reads it hot,
    -- so changes take effect immediately).
    -- **This is the outer backstop, not the throttle**: the real throttling is left to the
    -- per-model semaphores (see model_concurrency), and this only stops jobs from piling up
    -- without limit. It has to be clearly larger than the sum of the per-model limits,
    -- otherwise rate-limited jobs fill the slots and starve everything else
    worker_concurrency INT NOT NULL DEFAULT 32
        CHECK (worker_concurrency BETWEEN 1 AND 32),
    -- The character budget for laying the ontology into the extraction prompt; past that it
    -- switches to retrieving candidates per chunk.
    -- It lives in deployment settings rather than an environment variable because this dial has
    -- to be changeable without a restart -- pinning it down needs, at every ontology size, one
    -- run with everything inlined and one with per-chunk retrieval side by side, and if moving
    -- the dial one notch means restarting the service, nobody will run that curve a second
    -- time. 24000 characters (roughly 6000 tokens) is a guess, waiting on that curve to settle
    -- it
    ontology_prompt_budget INTEGER NOT NULL DEFAULT 24000,
    -- Models never configured in model_concurrency fall back to this default
    default_model_concurrency INT NOT NULL DEFAULT 10,
    -- The JWT signing key. **Generated automatically on first boot**, so that "get it running
    -- by following the README" and "be secure" stop being two separate jobs -- accidents like
    -- the default dev-secret-change-me reaching production cannot be prevented by reminders.
    -- UTOPIA_JWT_SECRET still takes precedence over this column: to rotate the key, or to line
    -- several instances up explicitly, just set the environment variable -- that path has not
    -- been closed off
    jwt_secret TEXT,
    -- The default ontology_lang for new KBs; for the meaning see
    -- knowledge_bases.ontology_lang
    default_ontology_lang TEXT NOT NULL DEFAULT 'en',
    CONSTRAINT deployment_default_ontology_lang_chk
        CHECK (default_ontology_lang IN ('en', 'zh'))
);
INSERT INTO deployment_settings DEFAULT VALUES;

-- Concurrency limits are **per model, not per deployment**. The real constraint is the model
-- provider's rate limit, and that comes per model (together with the base_url): a local Ollama
-- may only take 2 concurrent requests while a hosted API can swallow 50 -- one global number
-- governing both was never right.
--
-- The throttle sits at the LLM call site rather than at job scheduling: jobs that do not call a
-- model (folder sync) should not be constrained by it, and jobs calling different models
-- (extraction uses chat, ingestion uses embedding) should not crowd each other out either.
CREATE TABLE model_concurrency (
    base_url        TEXT NOT NULL,
    model           TEXT NOT NULL,
    max_concurrent  INT  NOT NULL CHECK (max_concurrent BETWEEN 1 AND 256),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (base_url, model)
);
