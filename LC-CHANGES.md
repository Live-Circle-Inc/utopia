# LC changes

**Branch:** `security-audit` — **base:** `dev` at `883bb7f` ("The singleton-row checks run one after
the other", #266). The branch has not diverged: `883bb7f` is the merge base, so everything below is
strictly additive on top of `dev`.
**Date:** 2026-09-27.
**Head:** `0af523b` at the time of writing; the work in §2 was committed on top.

Two unrelated bodies of work sit on this branch. §1 is the earlier three commits: a security audit
and two small operability fixes. §2 splits the single chat-model setting into separate chat and
extraction slots, which came out of debugging a live Gemini misconfiguration. §3 records what was
and was not verified, and §4 the loose ends.

---

## 1. Committed (3 commits vs `dev`)

```
 SECURITY-AUDIT.md                | 170 +++++++++++++++++
 crates/utopia-core/src/config.rs |  45 ++++++++-
 crates/utopia-server/src/main.rs |   2 +-
 docker-compose.yml               |   2 +-
```

### `a4acd99` — Record a security audit of the tree

Adds `SECURITY-AUDIT.md`: a full-tree review of all 278 tracked files at `883bb7f`. Conclusion was
no malicious code — no backdoors, obfuscated payloads, exfiltration, or build/release tampering.
It records five real defensive gaps (public self-registration by default, SSRF through configured
sources, zip-bomb memory exhaustion on upload, the dev CORS origin shipping to production, and the
container running as root), plus one area it could not verify. Documentation only; no code change.

### `d1a6257` — Treat empty `migration_url` as unset

`UTOPIA_MIGRATION_URL=""` previously reached the connection code as an empty connection string,
which failed with a cryptic driver error rather than a useful one. Blank and whitespace-only values
now fall back to the runtime `database_url`. Covered by two unit tests in `config.rs`
(`an_empty_migration_url_falls_back`, `a_configured_migration_url_wins`).

### `0af523b` — Point the default compose image at the local build tag

One line in `docker-compose.yml`, so `docker compose up` after a local build uses what was just
built instead of pulling.

---

## 2. Split `chat_model` into `chat_model` + `extract_model`

### Why

One `chat_model` column was feeding **seven** call sites with incompatible requirements:

| Call site | Job | What it needs |
|---|---|---|
| `api/chat.rs:590,640` | interactive chat: streaming, 7 tools, multi-round | hardest — tool-call fidelity |
| `extraction.rs:298` | per-chunk graph extraction | cost-dominant — cheap and fast |
| `adjudication.rs:38` | entity-resolution adjudication | reasoning |
| `type_resolution.rs:437` | entity type resolution | structured judgment |
| `mappings.rs:56` | semantic-layer mappings | structured judgment |
| `api/ontology_routes.rs:543` | ontology suggestions | reasoning |
| `api/settings_routes.rs` | connectivity probe | trivial |

The single setting is pinned by the *hardest* of those, while the bill is dominated by extraction —
the same prompt re-run over every chunk of every document (1200-char chunks, 150 overlap, per
`utopia-ingest/src/chunker.rs:14`). No one value serves both. Chat also has a guard that punishes a
weak model specifically: `check_call`'s uuid check (`api/chat.rs`) rejects fabricated ids, and each
rejection costs another round trip.

### Design

**Whole-block fallback, not per-field.** `extract_*` is used only when *both* `extract_base_url`
and `extract_model` are set; otherwise extraction resolves entirely to `chat_*`. Per-field fallback
would send one provider's model name to another provider's host, and both columns would read as
"configured". Pinned by `a_half_configured_extract_model_falls_back_whole`.

**The key is inherited only on an identical host.** If `extract_base_url` equals `chat_base_url` and
no extract key is given, chat's key is reused — the common "same provider, cheaper tier" case, where
the admin cannot re-read the key because the UI is write-only. Different host, no inheritance:
shipping one vendor's credential to another vendor's machine is a leak, not a convenience.

**Resolution lives in one place.** `LlmSettings::effective_extract()` is the only implementation,
and both `llm_util::extract_client` and `llm_util::acquire_extract` read it. If each resolved the
fallback independently, a half-configured deployment could have the concurrency semaphore keyed on
one model while requests went to another.

**Only `extraction.rs` moved.** Adjudication, type resolution, mappings and ontology suggestions are
single batched calls rather than per-chunk loops, so they stay on the chat model. Quietly demoting
graph-quality judgments to a cheap model is a product decision, not a refactor.

### Changed

| File | Change |
|---|---|
| `migrations/0022_a_cheap_model_for_the_bulk_work.sql` | **new** — `extract_base_url`, `extract_api_key`, `extract_model` |
| `crates/utopia-core/src/models.rs` | 3 fields; `effective_extract()`, `extract_overridden()`, `extract_ready()`; 6 unit tests |
| `crates/utopia-store/src/settings.rs` | `upsert` now takes `LlmSettingsInput { chat, extract, embed }` instead of 9 positional `Option<&str>` |
| `crates/utopia-store/src/model_limits.rs` | `models_in_use` also reports extract pairs |
| `crates/utopia-server/src/llm_util.rs` | `extract_client()`, `acquire_extract()` |
| `crates/utopia-server/src/extraction.rs` | uses the extract client and its own concurrency gate |
| `crates/utopia-server/src/pipeline.rs` | enqueue gates on `extract_ready()`, not `chat_ready()` |
| `crates/utopia-server/src/api/settings_routes.rs` | extract in GET/PUT; probe factored to `probe_chat`; extract tested only when overridden |
| `web/src/api.ts` | `LlmSettingsView` + `testSettings` gain extract (nullable) |
| `web/src/pages/Settings.tsx` | extract section, extract-aware presets, inheritance notice, extract test row |
| `web/src/i18n/{en,zh}.ts` | 5 new keys each |
| `crates/utopia-llm/src/lib.rs` | **temporary** debug line — see §4 |

Two behaviour notes worth knowing:

- `upsert` was restructured because adding three more parameters would have produced **nine
  consecutive `Option<&str>` arguments**, where a mis-ordering compiles silently and stores a record
  that round-trips cleanly with the values in the wrong columns. Named fields make that a type error.
- `pipeline.rs` previously gated extraction-job enqueue on `chat_ready()`. A deployment configuring
  only an extraction endpoint (graph, no chat) would have indexed every document and enqueued
  nothing, without an error. It now gates on `extract_ready()`.

### UI

A new **Extraction model (optional)** section in Settings → Models, between Chat and Embedding:

- Base URL / Model / API key, with placeholders echoing the current chat values so a blank field
  reads as "inherits chat" rather than "unset".
- An explicit accent-coloured notice while inheriting: *"Currently following the chat model — set
  both Base URL and Model to use a separate one."* The predicate mirrors
  `LlmSettings::extract_overridden` exactly, so the UI cannot disagree with the backend about the
  half-configured state — the one state that previously looked configured but was not.
- **Test connection** gained an *Extraction* row, rendered only when extract has its own endpoint.
  When inheriting, the backend returns `null` and the row is hidden, so "inheriting" never reads as
  a failure, and a click does not spend two requests against one quota.
- Presets are now whole-set replacements: selecting one clears `extract_*` unless the preset
  specifies it, so a stale extraction endpoint cannot survive a provider switch.
- Two new presets: **Gemini** and **Gemini + local extract** (strong remote model for chat, local
  Ollama for the per-chunk bulk). Note the Gemini base URL **must** include the `/openai` segment —
  `https://generativelanguage.googleapis.com/v1beta/openai`. Plain `/v1beta` is the native Gemini
  API, which has no `/chat/completions`; getting this wrong produces a 4xx whose body this client
  reports as `unknown error` (§4).

---

## 3. Verification

| Check | Result |
|---|---|
| `cargo fmt --all --check` | clean |
| `cargo clippy --workspace --all-targets` | 0 diagnostics |
| `cargo test --workspace` | all pass, including 6 new `effective_extract` tests |
| `pnpm run typecheck` (`tsc --noEmit`) | clean |
| i18n key coverage | 29/29 `S.settings.*` refs resolve; `en`/`zh` key sets identical |
| Migration `0022` applied | **not run** — no database was reachable from this session |
| Runtime behaviour | **not exercised** — no end-to-end run against a live endpoint |

The extraction and settings paths have no integration test covering the new fallback at the database
level; `effective_extract` is unit-tested in isolation, and the SQL round-trip is not.

---

## 4. Loose ends

1. **A temporary debug line is still in the tree.** `crates/utopia-llm/src/lib.rs:363` logs every
   raw SSE frame (`tracing::debug!(%data, "llm sse frame")`), added to diagnose duplicated streaming
   tool-call deltas. Marked `TEMP DEBUG(remove)`. It logs full model output at debug level — remove
   before merging.

2. **Error bodies are discarded, so provider failures read as `unknown error`.** `err_detail`
   (`utopia-llm/src/lib.rs:524`) probes only `body["error"]["message"]` and `body["message"]`, then
   falls back to the literal `"unknown error"`; `failure()` consumes the body and never logs it, and
   two streaming paths (`lib.rs:339`, `:434`) additionally flatten a non-JSON body to `Null` via
   `unwrap_or_default()`. Every misconfiguration therefore presents identically and undiagnosably.
   Replacing those with `resp.text()` + a `tracing::warn!` of the raw body is the highest-value fix
   outstanding on this branch.

3. **`says_out_of_credit` cannot fire for Google.** It matches the exact string `insufficient_quota`
   in `body["error"]["code"]` or `["type"]` (`lib.rs:~540`). Google sets `error.code` to an *integer*
   and signals `status: "RESOURCE_EXHAUSTED"`, so `.as_str()` yields `None` and the check is
   unreachable. A quota-exhausted Gemini account is classified as transient rate limiting. The
   consequence is bounded, not a hang — `extraction.rs:13,50-64` retries 5 times with jittered
   backoff capped at 60s and then gives up — but it wastes those retries against a condition that
   will not clear, and reopens the data-gap failure that retry loop was written to fix.

4. **`retry_after_of` reads an HTTP header only.** Google conveys delay in-body as
   `google.rpc.RetryInfo.retryDelay`, so `retry_after` is always `None` on Gemini and the backoff
   ignores the provider's own advice.

5. **The streaming tool-call accumulator assumes fields arrive once.** `lib.rs:390` appends `id`,
   `name` and `arguments` with `push_str` on every delta. Providers that restate the whole tool call
   per chunk yield a doubled name (`find_entitiesfind_entities`) and concatenated argument JSON,
   which `check_call` then correctly refuses as `bad arguments`. Unfixed; the debug line in (1) is
   what confirms whether a given provider does this.

6. **A refused tool call poisons the next turn.** In `api/chat.rs`'s `check_call` failure branch the
   synthetic tool result is pushed to `msgs` but not to `exchange_acc`, and `exchange_acc` is what
   persists as `tool_exchange` and replays on the following turn (`chat.rs:532`). The replayed
   payload then contains an assistant message with `tool_calls` that no `tool` message answers,
   which OpenAI-compatible endpoints reject with 400 for every subsequent message in that
   conversation. Two-line fix, independent of everything above.

7. **No request-rate pacing exists.** `model_concurrency` bounds *in-flight* requests only, and only
   for background work — `llm_util::acquire`'s own doc comment excludes the interactive chat path
   deliberately. Nothing in the tree can respect a requests-per-minute or tokens-per-minute budget,
   and no `max_tokens` is ever sent, so a thinking-capable model can trip a token-per-minute ceiling.
   Relevant to any metered provider; not relevant to a local Ollama extraction endpoint.

8. **`embed_dim` is vestigial.** It exists in the schema, the GET view and `PutSettingsReq`, but the
   Settings form never sends it, so every save writes `NULL` — and nothing in `crates/` reads it
   outside the settings plumbing itself. Pre-existing; noted because the `upsert` rewrite touched
   that statement without changing the behaviour.

9. **Ollama specifics for a local extraction model.** The client is protocol-pure — it sends only
   `{model, messages, stream}`, never `options` or `response_format`. So (a) `num_ctx` must be set
   via a Modelfile or `OLLAMA_CONTEXT_LENGTH`, since Ollama's small default would silently truncate
   a ~2-4k-token extraction prompt; (b) `OLLAMA_KEEP_ALIVE` should be long, because extraction's
   economics rest on KV-cache reuse of an identical per-document system prefix; (c) the default
   `max_concurrent` of 10 will thrash one local model, so set a `model_concurrency` row matching
   `OLLAMA_NUM_PARALLEL`; and (d) structured output is prompt-instructed only — `parse_response`
   tolerates fenced and truncated JSON but *silently drops* malformed items into
   `skipped_entities`/`skipped_facts`, so a model that is 90% reliable at JSON loses 10% of the graph
   without raising an error. Adding `response_format` / Ollama's `format: json` support to the
   extract path is the change most likely to protect graph completeness.

10. **Model tags in the new presets are unverified.** `gemini-3.8-flash` and `qwen3:30b-a3b` were
    taken from the operator's stated setup and from Ollama naming conventions respectively, not
    confirmed against either provider's live model list. The base URLs are the load-bearing part;
    check the tags against `/v1/models` and `ollama list`.
