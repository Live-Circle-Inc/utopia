#!/usr/bin/env bash
# End-to-end smoke test: register → session → workspace → knowledge base → permissions →
# job queue
# Usage: ./scripts/smoke.sh [BASE_URL]  (default http://localhost:1516)
# The JSON body goes through a temp file, to avoid the encoding conversion problems of the
# Windows shell.
set -euo pipefail

BASE="${1:-http://localhost:1516}"
EMAIL="smoke-$(date +%s)@test.local"
JAR="$(mktemp)"
BODY="$(mktemp)"
trap 'rm -f "$JAR" "$BODY"' EXIT

step() { echo "--- $1"; }
fail() { echo "FAIL: $1" >&2; exit 1; }

step "health"
curl -sf "$BASE/api/v1/health" | grep -q '"ok"' || fail "health"

step "register ($EMAIL)"
printf '{"email":"%s","password":"password123","display_name":"Smoke Tester"}' "$EMAIL" > "$BODY"
curl -sf -c "$JAR" -H 'Content-Type: application/json' --data-binary @"$BODY" \
  "$BASE/api/v1/auth/register" | grep -q '"user"' || fail "register"

step "me (cookie auth)"
curl -sf -b "$JAR" "$BASE/api/v1/auth/me" | grep -q "$EMAIL" || fail "me"

step "workspaces (should contain at least the default workspace)"
curl -sf -b "$JAR" "$BASE/api/v1/workspaces" | grep -q '"id"' || fail "workspace list is empty"

step "create a workspace of one's own (becoming its owner)"
printf '{"name":"Smoke WS"}' > "$BODY"
WS_ID=$(curl -sf -b "$JAR" -H 'Content-Type: application/json' --data-binary @"$BODY" \
  "$BASE/api/v1/workspaces" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p' | head -1)
[ -n "$WS_ID" ] || fail "create workspace"
echo "    workspace: $WS_ID"

step "create a knowledge base"
printf '{"name":"Smoke KB"}' > "$BODY"
KB_ID=$(curl -sf -b "$JAR" -H 'Content-Type: application/json' --data-binary @"$BODY" \
  "$BASE/api/v1/workspaces/$WS_ID/kbs" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p' | head -1)
[ -n "$KB_ID" ] || fail "create kb"
echo "    kb: $KB_ID"

step "upload a document (mixed Chinese/English markdown)"
# Note: do not use curl's ";filename=" syntax -- the MSYS path conversion of Git Bash mangles
# arguments that contain a semicolon
DOCDIR="$(mktemp -d)"
DOC="$DOCDIR/smoke.md"
# "凤凰项目由张三负责。" ("The Phoenix project is run by Zhang San.") written out as UTF-8
# bytes, to get around the encoding conversion of the Windows shell
printf '# Phoenix Handbook\n\n\xe5\x87\xa4\xe5\x87\xb0\xe9\xa1\xb9\xe7\x9b\xae\xe7\x94\xb1\xe5\xbc\xa0\xe4\xb8\x89\xe8\xb4\x9f\xe8\xb4\xa3\xe3\x80\x82 Budget is 12 million CNY.\n' > "$DOC"
curl -sf -b "$JAR" -F "files=@$DOC" \
  "$BASE/api/v1/kbs/$KB_ID/documents" | grep -q '"created"' || fail "upload"
rm -rf "$DOCDIR"

step "wait for the ingest pipeline to finish"
for i in $(seq 1 20); do
  S=$(curl -sf -b "$JAR" "$BASE/api/v1/kbs/$KB_ID/documents" | grep -o '"status":"[^"]*"' | head -1)
  case "$S" in
    *ready*) break;;
    *failed*) fail "document processing failed";;
  esac
  sleep 2
done
case "$S" in *ready*) ;; *) fail "processing timed out (currently $S)";; esac

step "Chinese search (凤凰项目 / the Phoenix project)"
printf '{"q":"\xe5\x87\xa4\xe5\x87\xb0\xe9\xa1\xb9\xe7\x9b\xae"}' > "$BODY"
curl -sf -b "$JAR" -H 'Content-Type: application/json' --data-binary @"$BODY" \
  "$BASE/api/v1/kbs/$KB_ID/search" | grep -q '"filename":"smoke.md"' || fail "the Chinese search did not hit"

step "unauthenticated access should be 401"
CODE=$(curl -s -o /dev/null -w '%{http_code}' "$BASE/api/v1/workspaces")
[ "$CODE" = "401" ] || fail "expected 401, got $CODE"

step "log in (a second session)"
printf '{"email":"%s","password":"password123"}' "$EMAIL" > "$BODY"
curl -sf -c "$JAR.2" -H 'Content-Type: application/json' --data-binary @"$BODY" \
  "$BASE/api/v1/auth/login" | grep -q '"token"' || fail "login"
rm -f "$JAR.2"

step "enqueue a noop job"
curl -sf -b "$JAR" -X POST "$BASE/api/v1/jobs/noop" | grep -q '"job_id"' || fail "enqueue"

echo "=== every smoke test passed ==="
