-- The application used to connect as the database owner (a superuser). That identity can DROP
-- TRIGGER and alter any table -- which means the immutability trigger 0026 put on the ledger is
-- worth nothing against anyone holding the application's connection string: DISABLE TRIGGER,
-- edit the record, ENABLE TRIGGER, three statements, and not a trace afterwards.
--
-- The restricted role drops the application down to the privileges it actually needs: read and
-- write the business tables freely, and on the ledger only write and read.
-- So the first of those same three statements now fails with must be owner of table.
--
-- This layer stops the application's own bugs, the privileges SQL injection inherits, and
-- connection strings that leak out (logs, backups, an accidentally committed .env). It does not
-- stop someone who can log into the server -- psql inside the container uses trust
-- authentication, so no password means superuser. That level is server access control, and does
-- not belong here.
--
-- The whole block is skipped when the role does not exist: an existing deployment keeps running
-- as usual without creating the role or changing its connection string.
-- ALTER DEFAULT PRIVILEGES has to be wrapped in here too -- outside the DO block it would error
-- on the missing role and abort the migration, which would make every existing deployment fail
-- the moment it upgraded.
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'utopia_app') THEN
        RAISE NOTICE 'role utopia_app does not exist, skipping the restricted privilege setup (the application keeps running as its current identity)';
        RETURN;
    END IF;

    GRANT USAGE ON SCHEMA public TO utopia_app;
    GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO utopia_app;
    GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA public TO utopia_app;
    GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA public TO utopia_app;

    -- The ledger: writable and readable, not modifiable, not deletable
    REVOKE UPDATE, DELETE, TRUNCATE ON audit_events FROM utopia_app;

    -- Tables/sequences/functions created by later migrations are granted automatically,
    -- otherwise every new table makes the application hit a permission error.
    -- PL/pgSQL does not accept this utility command written out directly, so go via EXECUTE.
    EXECUTE 'ALTER DEFAULT PRIVILEGES IN SCHEMA public '
         || 'GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO utopia_app';
    EXECUTE 'ALTER DEFAULT PRIVILEGES IN SCHEMA public '
         || 'GRANT USAGE, SELECT ON SEQUENCES TO utopia_app';
    EXECUTE 'ALTER DEFAULT PRIVILEGES IN SCHEMA public '
         || 'GRANT EXECUTE ON FUNCTIONS TO utopia_app';

    RAISE NOTICE 'restricted privileges for utopia_app configured';
END $$;
