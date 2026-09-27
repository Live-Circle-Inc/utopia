-- Chat session persistence -- conversations, messages, action traces and citations all land in
-- the database alongside the message. The server assembles the context from here (the frontend
-- only sends conversation_id + the new message); steps/sources are isomorphic to the live SSE
-- events, so history replay and streaming rendering share one set of components.

CREATE TABLE conversations (
    id         UUID PRIMARY KEY,
    kb_id      UUID NOT NULL REFERENCES knowledge_bases(id) ON DELETE CASCADE,
    user_id    UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    title      TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX conversations_kb_user_idx ON conversations (kb_id, user_id, updated_at DESC);

CREATE TABLE conversation_messages (
    id              UUID PRIMARY KEY,
    conversation_id UUID NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    role            TEXT NOT NULL CHECK (role IN ('user', 'assistant')),
    content         TEXT NOT NULL,
    -- Action trace (tool-call steps) and citation list (history replay; isomorphic to the
    -- SSE step/sources)
    steps           JSONB NOT NULL DEFAULT '[]',
    sources         JSONB NOT NULL DEFAULT '[]',
    -- Which entities this turn resolved (id, name, type), to be replayed on the next turn.
    -- **Do not replay the whole tool result**: that is chunk body text, and piling it into the
    -- context again every turn eats the window in a handful of turns. What gets replayed is
    -- identity -- with the id in hand the next turn calls entity_facts directly instead of
    -- looking it up by name again; that also cures a subtler defect: when a name is ambiguous
    -- two turns can resolve to different entities, so the two answers are not talking about the
    -- same node
    resolved        JSONB NOT NULL DEFAULT '[]'::jsonb,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX conversation_messages_conv_idx
    ON conversation_messages (conversation_id, created_at);
