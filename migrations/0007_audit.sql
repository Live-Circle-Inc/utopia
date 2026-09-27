-- Audit log: who did what to which thing and when (pure audit -- it carries no derived
-- features such as rollback).
--
-- **kb_id and actor_id are bare UUIDs, with no foreign key.** A ledger must not depend on the
-- objects it records staying alive: if kb_id cascaded on delete, deleting one knowledge base
-- would delete every audit record it has -- including the kb.deleted row just written; deletion
-- is the action that most needs a trace, and it would become the only one leaving none. If
-- actor_id were SET NULL, every confirmation, rejection and merge a user ever made would turn
-- anonymous the moment that user was deactivated.
-- Those two scenarios are precisely what a compliance audit is after.
--
-- This is also the precondition for the hash chain: the chain requires records to be
-- append-only, and a cascading delete would carve a stretch out of the middle of it, letting it
-- break "legitimately" and indistinguishably.
CREATE TABLE audit_events (
    id          UUID PRIMARY KEY,
    kb_id       UUID,
    actor_id    UUID,
    action      TEXT NOT NULL,
    target_kind TEXT NOT NULL,
    target_id   UUID,
    -- The gist of the change, shaped by whatever the action means
    detail      JSONB NOT NULL DEFAULT '{}',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- Where it was done from. ISO 27001 A.8.15 requires logs to cover Where / How, i.e. the
    -- origin of the request; without it, everything done with a stolen account looks no
    -- different from the owner's own activity.
    -- Both columns are nullable: background jobs (batched adjudication, scheduled sync) have
    -- no client, and are supposed to be empty
    client_ip   TEXT,
    user_agent  TEXT,
    -- Identity snapshot of the actor. actor_id has no foreign key, so the row survives when
    -- a user is deactivated, but a LEFT JOIN on users no longer yields a name and the UI is
    -- left with a bare UUID. Writing the email and display name of that moment into the record
    -- is what makes the ledger genuinely self-contained
    actor_label TEXT
);
CREATE INDEX audit_events_kb_time_idx ON audit_events (kb_id, created_at DESC);

-- Investigating by IP (login failures from one origin, activity at odd hours) needs this index
CREATE INDEX audit_events_client_ip_idx ON audit_events (client_ip, created_at DESC)
    WHERE client_ip IS NOT NULL;

-- The ledger is append-only. The application only ever INSERTs anyway; what this trigger
-- blocks is the route around the application: an operator connected straight to the database,
-- one slip-of-the-hand UPDATE, or somebody coming back to wipe their own traces.
--
-- It cannot stop a superuser -- that identity can DROP TRIGGER, or ALTER TABLE ... DISABLE
-- TRIGGER, and then edit at leisure. So the job of this layer is to raise the bar from "you can
-- change it on a whim" to "you have to touch DDL first", and DDL itself stays in the database
-- log. Leaving deliberate tampering nowhere to hide takes the hash chain that comes later: an
-- edit breaks the chain at exactly the record that was edited.
--
-- Deliberately no application-level bypass switch. The moment audit immutability has a switch,
-- it has none. If retention-period pruning is ever wanted, that is a privileged operations
-- action: DROP TRIGGER explicitly, prune, then rebuild, with the whole sequence left in the DDL
-- record.
CREATE FUNCTION audit_events_immutable() RETURNS trigger AS $$
BEGIN
    RAISE EXCEPTION 'audit_events is append-only (attempted %)', TG_OP
        USING HINT = 'Audit records cannot be modified or deleted.';
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER audit_events_no_update_delete
    BEFORE UPDATE OR DELETE ON audit_events
    FOR EACH ROW EXECUTE FUNCTION audit_events_immutable();

-- TRUNCATE does not fire row-level triggers, so it is blocked separately; otherwise a single
-- TRUNCATE walks around everything above.
CREATE TRIGGER audit_events_no_truncate
    BEFORE TRUNCATE ON audit_events
    FOR EACH STATEMENT EXECUTE FUNCTION audit_events_immutable();
