#!/bin/bash
# Creates the restricted role the application runs as. Runs only on the very first
# initialisation, when the data directory is empty (the docker-entrypoint-initdb.d convention of
# the official Postgres image), so existing deployments are unaffected.
#
# Privileges are not granted here -- migration 0031 grants them, which is how every upgrade gets
# the new tables filled in. All this script is responsible for is that the role itself exists and
# has a password it can log in with.
set -euo pipefail

APP_PASSWORD="${UTOPIA_APP_DB_PASSWORD:-}"
if [ -z "$APP_PASSWORD" ]; then
    echo "UTOPIA_APP_DB_PASSWORD is not set, skipping restricted role creation; the app runs as owner." >&2
    exit 0
fi

psql -v ON_ERROR_STOP=1 --username "$POSTGRES_USER" --dbname "$POSTGRES_DB" <<SQL
DO \$\$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'utopia_app') THEN
        CREATE ROLE utopia_app LOGIN PASSWORD '${APP_PASSWORD}';
        RAISE NOTICE 'created restricted role utopia_app';
    END IF;
END
\$\$;
GRANT CONNECT ON DATABASE "$POSTGRES_DB" TO utopia_app;
SQL
