-- Personal access tokens: a long-lived key for MCP clients (see docs/decisions/0014).
--
-- **It acts with this person's identity, but need not be all of that person.**
--
--     effective permissions = this person's role ∩ this token's scope
--
-- Intersection, not union: ticking write on a viewer's token still leaves it read-only. Scope is
-- a ceiling, not a grant.
--
-- Why not issue machine tokens (the fork in the road recorded in 0014): machine identities would
-- bring in a third authorization model, and `audit_events.actor_id` would gain a class of records
-- that "nobody did" -- while the reason the ledger exists is precisely "who signed off on what,
-- and when".
CREATE TABLE personal_tokens (
    id           UUID PRIMARY KEY,
    -- **It has a foreign key and it cascades**, the opposite of the bare UUID in
    -- `audit_events.actor_id`: the ledger has to outlive the user, a key should not. Once the
    -- person is gone, their keys should go with them
    user_id      UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    -- A name the person gives it themselves, "my laptop". When revoking you have to be able to
    -- tell which key it is you are revoking
    name         TEXT NOT NULL,

    -- **Stored hashed, the opposite of the plaintext in `sources.ingest_token`.**
    --
    -- The reasoning there was "by the time the DB is breached the documents themselves have long
    -- since leaked, so hashing buys nothing extra", and that holds because ingest_token can only
    -- **push documents in**. This key is different: through `query_data` it can read production
    -- databases outside Utopia. The warehouse is on another machine holding another set of data,
    -- and should not be lost along with Utopia's own database. **A different blast radius, so a
    -- different way of storing it.**
    token_hash   TEXT NOT NULL UNIQUE,
    -- The prefix humans recognise it by (`utp_ab12…`). In a listing it has to match the string
    -- in the config file at a glance, without keeping the whole plaintext around
    token_prefix TEXT NOT NULL,

    -- read = read-only tools; write = additionally opens up remember. **Read-only by default**:
    -- to let an agent write into the ledger, you have to tick it explicitly
    scope        TEXT NOT NULL DEFAULT 'read' CHECK (scope IN ('read', 'write')),
    -- Which knowledge bases it is limited to. NULL = all the ones this person can get into.
    -- A bare UUID array is not laziness: if a KB is deleted that entry merely goes void, and the
    -- token should not be deleted wholesale
    kb_ids       UUID[],

    -- NULL = never expires. The UI defaults to 90 days -- never expiring is selectable, but it
    -- is not the default
    expires_at   TIMESTAMPTZ,
    -- "Is this one still in use". You have to be able to answer that before revoking, or nobody
    -- dares revoke
    last_used_at TIMESTAMPTZ,
    -- **Revoking does not delete the row**: the fact of revocation has to leave a trace. Delete
    -- the row and "this key existed" becomes unanswerable, and that is exactly the first thing
    -- an after-the-fact investigation asks
    revoked_at   TIMESTAMPTZ,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Validation is a hot path: **it is looked up on every tool call**, not once at handshake time.
-- That is the lesson of 0014_data_source_grants -- a list filter is not a guard, and the MCP
-- shape of the same mistake is "trusting the lifetime of an entire connection", whereas when
-- revoked_at is written midway it has to take effect immediately
CREATE UNIQUE INDEX personal_tokens_hash_idx ON personal_tokens (token_hash);
-- "Which ones have I issued": the account page lists them by person, most recent first
CREATE INDEX personal_tokens_user_idx ON personal_tokens (user_id, created_at DESC);
