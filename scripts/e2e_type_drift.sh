#!/usr/bin/env bash
# Type-drift E2E orchestration: start the server on its own database and its own port, then run
# scripts/e2e_type_drift.mjs.
# Prerequisites: the compose db container is already running (docker compose up -d db), node ≥ 18.
# Usage: ./scripts/e2e_type_drift.sh
set -euo pipefail
cd "$(dirname "$0")/.."

DB_CONTAINER="${E2E_DB_CONTAINER:-landscapebi-db-1}"
DB_NAME="utopia_e2e"
PORT="${E2E_PORT:-8317}"
DATA_DIR="$(mktemp -d)"

echo "--- rebuilding isolated database $DB_NAME"
docker exec "$DB_CONTAINER" psql -U utopia -d postgres \
  -c "DROP DATABASE IF EXISTS $DB_NAME;" -c "CREATE DATABASE $DB_NAME;" >/dev/null

echo "--- building utopia-server"
cargo build -p utopia-server

echo "--- starting server (127.0.0.1:$PORT, db=$DB_NAME)"
UTOPIA_DATABASE_URL="postgres://utopia:utopia@localhost:1517/$DB_NAME" \
UTOPIA_BIND_ADDR="127.0.0.1:$PORT" \
UTOPIA_DATA_DIR="$DATA_DIR" \
  ./target/debug/utopia-server &
SERVER_PID=$!
trap 'kill $SERVER_PID 2>/dev/null || true; rm -rf "$DATA_DIR"' EXIT

for i in $(seq 1 30); do
  curl -sf "http://127.0.0.1:$PORT/api/v1/health" >/dev/null 2>&1 && break
  sleep 1
done
curl -sf "http://127.0.0.1:$PORT/api/v1/health" >/dev/null || { echo "server not ready" >&2; exit 1; }

echo "--- running E2E"
E2E_BASE="http://127.0.0.1:$PORT" node scripts/e2e_type_drift.mjs
