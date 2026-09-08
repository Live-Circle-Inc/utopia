# Security audit

**Scope:** the whole repository at `883bb7f` ("The singleton-row checks run one after the other", #266), branch `dev`.
**Date:** 2026-09-03.
**Question asked:** is there malicious or dangerous code in this repository?

**Answer: no malicious code.** No backdoors, no obfuscated payloads, no data exfiltration, no
tampering with the build or release path. What follows records what was examined, what was ruled
out, the five real defensive gaps that were found, and the one thing this pass could not verify.

Coverage: all 278 tracked files — ~45.7k lines of Rust across 8 crates, ~13k lines of
TypeScript/React, 21 SQL migrations, 3 GitHub workflows, the Dockerfile and compose files, 3 shell
scripts, 7 `.mjs` scripts, the 5 binary ontology packs, and the git history of the whole tree.

---

## What was ruled out

| Threat | Finding |
|---|---|
| Memory-unsafe or FFI escape hatches | **No `unsafe` block anywhere in the workspace.** No `libc`, no `transmute`, no `dlopen`, no `from_raw`. |
| Command execution | No `Command::new`, no `std::process`, no shell invocation anywhere in the Rust tree. |
| Frontend code injection | No `eval`, `new Function`, `innerHTML`, `outerHTML`, `document.write`, `insertAdjacentHTML`, or `dangerouslySetInnerHTML`. Markdown renders through `react-markdown` with `remark-gfm` + `rehype-highlight` and **no `rehype-raw`**, so embedded HTML stays escaped — which matters because chat renders model output and ingested document text. |
| Phoning home | **No telemetry, analytics, or crash reporting.** `web/src/i18n/en.ts:287` makes that promise to users; the promise holds. Startup (`crates/utopia-server/src/main.rs`) opens database connections and TCP listeners and makes no outbound HTTP call. |
| Supply chain, Rust | All 568 `Cargo.lock` entries resolve to the crates.io registry. No git dependencies, no path dependencies outside the workspace, **no `build.rs` in any crate**. All direct dependencies are mainstream and recognizable. |
| Supply chain, npm | No `postinstall`/`preinstall`/`prepare` scripts in `web/package.json`. No `.npmrc`, no registry override, no `resolutions`, no `patchedDependencies`, no tarball/git/directory resolutions in the lockfile. |
| Hidden payloads in binaries | The 5 `packs/*.gz` files are genuine gzip streams decompressing to plausible RDF/Turtle (schema.org 1.1 MB, IOF Core 404 KB, PROV-O 113 KB, W3C Org 84 KB, FOAF 44 KB), each with sane internal filenames. `assets/banner.webp` is a real RIFF/WebP. `include_bytes!` appears only for these five packs. |
| Obfuscation | No base64 or hex blob over 120 chars in any source file. No `atob`/`btoa` in the frontend; no `base64::decode` reaching an execution sink. |
| Committed secrets | None. Every `AKIA`/`ghp_` hit is a test fixture, a UI placeholder, or documentation. `.env.example` contains only placeholders and localhost dev defaults. |
| Secrets in git history | Largest blobs ever committed are benchmark corpora, `Cargo.lock`, and `web/src/pages/Graph.tsx`. Deleted paths are migration renumbering and crate consolidation (`utopia-connectors`, `utopia-graph`, `utopia-mcp` folded in; `query_engine.rs` split into a module). Nothing dropped and hidden. |
| Malicious SQL in migrations | No `COPY … FROM/TO PROGRAM`, no `pg_read_file`, no `pg_ls_dir`, no `lo_import`, no `dblink`, no `postgres_fdw`, no `plpython`/`plperl`. The only extension is `vector` (`migrations/0001_core.sql:3`). |
| CI/release tampering | See below. |

### CI and release are locked down

`.github/workflows/ci.yml` triggers on `pull_request`, **not** `pull_request_target`, and declares
`permissions: contents: read` explicitly rather than inheriting the repo default — with a comment
saying that is exactly the point. No secret is reachable from a fork PR.

`release.yml` holds `packages: write`, uses only first-party `docker/*` and `actions/*` actions at
pinned major versions, and gates the registry push behind a real boot-migrate-register smoke test.

`star-history.yml` holds `contents: write` and a classic PAT (`METRICS_TOKEN`), the only genuinely
privileged combination in the repo. It is defensible: it runs on `schedule`/`workflow_dispatch`
only (never on untrusted input), executes a first-party script (`scripts/star-history.mjs`) rather
than a third-party action — the file comment says replacing `lowlighter/metrics` for that reason
was deliberate — and commits only to the orphan `assets` branch, which shares no history with any
source branch.

### Security-sensitive code is unusually well-built

Worth stating plainly, because it is the main evidence that the gaps below are oversights rather
than anything deliberate:

- **Authentication** (`crates/utopia-server/src/auth.rs`) — argon2 with per-hash `OsRng` salt;
  HS256 JWT in an HttpOnly `SameSite=Lax` cookie; `Secure` derived from `X-Forwarded-Proto` with a
  documented argument for why that header needs no trusted-proxy list. The signing key is 32 bytes
  of CSPRNG generated on first boot and persisted — **there is no shipped default secret**, and the
  compose file leaves `UTOPIA_JWT_SECRET` empty deliberately so auto-generation is always reached.
  A regression test pins default `Validation` semantics against CVE-2026-25537.
- **SQL** — every `format!`-into-query site (in `audit.rs`, `documents.rs`, `pending.rs`)
  interpolates only a `const &str` declared in the same function. Every user-supplied value is a
  bound parameter. The filter predicates use the `($n IS NULL OR …)` idiom, with a comment noting
  that string-concatenating the sixteen filter combinations would create sixteen injection sites.
- **The lakehouse query engine** (`crates/utopia-server/src/query_engine/`) executes
  model-authored SQL against customer databases and defends it in four layers: `sqlparser`
  parse with a `Statement::Query`-only allowlist and a per-engine dialect, a forced outer
  `LIMIT` at 201 rows, a read-only session, and a statement timeout. Multi-statement and
  `SELECT INTO` are rejected. Tests assert the gate holds identically across all four dialects.
- **Credential containment** — `DataSourceView` (`crates/utopia-core/src/models.rs:876`) has no
  `conn_string` field; rows are mapped through `conn_summary()` to `host:port/db` before they can
  be serialized. Source secrets are redacted by a central `SOURCE_SECRET_KEYS` table, with
  `crates/utopia-store/tests/a_viewer_never_sees_a_credential.rs` exercising every connector.
- **Blob storage** is content-addressed: `blob.get` is only ever called with a server-computed
  `doc.sha256` from the database, so `dir.join(key)` cannot be steered. No path traversal.
- **OOXML unpacking** reads fixed entry names and never writes an archive path to disk — no zip slip.
- **MCP** (`crates/utopia-server/src/api/mcp.rs`) exposes 6 read-only tools; `remember`, the one
  writing tool, is not among them. Auth is two independent gates — token scope narrows reach,
  `access::require_kb` decides the role — with a comment insisting they are not the same check.
- **Agent tool surface** (`crates/utopia-server/src/api/tools.rs`) is read-only but for
  `remember`, which is gated on `can_write`. No tool can add a data source or fetch a URL, so
  prompt injection from an ingested document has no pivot into the network or the config.
- **Personal tokens** are 244 bits of entropy (two v4 UUIDs, CSPRNG) stored as SHA-256. Single-round
  hashing is correct here and the code argues why: the input is high-entropy random, not a
  human-chosen password, and per-row argon2 salts would force a full table scan on a hot path.

---

## Real weaknesses

Five defensive gaps, most consequential first. None is planted; all are ordinary omissions.

### 1. A default deployment is publicly self-registerable

`open_registration` defaults to true at all three layers —
`crates/utopia-core/src/config.rs:46`, `migrations/0001_core.sql:129`, and the
`unwrap_or(true)` at `crates/utopia-store/src/access.rs:187` — while `bind_addr` defaults to
`0.0.0.0:1516` (`config.rs:40`). Anyone who can reach the port gets an account and their own
workspace.

For a product whose README promises a one-command deploy, the safe default is closed: let the
first (bootstrapping) user through, then require an admin to open the door. The mechanism for
that already exists and is already wired to `/admin` — only the default needs to flip.

### 2. SSRF through configured sources

URL, RSS, and custom-endpoint sources are fetched server-side with no denylist for internal
addresses (`crates/utopia-server/src/ingest_sources.rs:351`, `:409`, `:688`). A user who can
create a source can point it at `169.254.169.254` or an internal service; the response body lands
in the knowledge base, where that same user can read it back.

Creating a source requires authorization, and fetching operator-chosen URLs *is* the feature — so
this is a privilege-boundary question, not an open relay. The fix is to block link-local and
private ranges after DNS resolution, with an explicit operator opt-out for on-premise sources that
legitimately live on a private network. Note that the existing loopback checks
(`ingest_sources.rs:671`, `query_engine/mod.rs:216`) are proxy-routing decisions, not security
boundaries, and do not help here.

### 3. Zip-bomb memory exhaustion on upload

`crates/utopia-ingest/src/parsers.rs:31-49` (pptx) and `:172-177` (`read_zip_entry`, used by docx)
call `read_to_string` on archive entries with no bound on the decompressed size. The 100 MB
`DefaultBodyLimit` (`crates/utopia-server/src/api/mod.rs:35`) caps the *compressed* input; a
crafted OOXML file well under that limit can expand to many gigabytes. Wrapping each entry in
`.take(N)` and failing past the cap closes it. `spreadsheet()` is already bounded, at 2000 rows.

### 4. The dev CORS origin ships to production

`crates/utopia-server/src/api/mod.rs:417-427` allows `http://localhost:5173` with
`allow_credentials(true)`, unconditionally, in every build. This is not the wildcard-plus-
credentials bug — the origin is compared literally — but it does mean anything a victim runs on
that port can make credentialed cross-origin calls to their Utopia instance. The layer exists only
for `vite dev`; it should be gated behind a debug build or a config flag.

### 5. The container runs as root

`docker/Dockerfile` never sets `USER`, so `utopia-server` runs as uid 0 in the published image.
Adding a non-root user and `chown`ing `/app/data` costs two lines. Worth doing especially because
the rest of the deployment story is careful about privilege — `docker-compose.yml` binds Postgres
to loopback only, and the optional `utopia_app` role exists precisely to keep the application off
the database owner.

---

## Not verified

`web/package.json` pins two very low-profile npm packages, both used legitimately in
`web/src/pages/Chat.tsx`:

- `remend@1.3.1` — repairs unclosed markdown syntax mid-stream (`Chat.tsx:739`)
- `thinking-orbs@0.3.1` — the thinking indicator (`Chat.tsx:675`)

`node_modules` was not installed for this pass, so **only their lockfile integrity hashes were
checked, not their code.** Nothing points to a problem, and both hold a real place in the UI. But
small-maintainer packages inside an enterprise deployment are the highest-value surface in this
tree, and these two are the only instance of it. Reading them at the pinned version — and pinning
exactly rather than with `^` — would close the last gap in this audit.

---

## Method

Static review only; nothing was built, installed, or executed. Structural sweeps (dangerous sinks,
dynamic SQL, path joins, decompression, every external host referenced anywhere in the tree,
high-entropy strings, git object sizes and deletions) were each followed by reading the
surrounding code, so the conclusions above rest on the code and not on grep counts.

Two limits are worth stating. First, dependency *code* was not audited — 568 crates and the npm
tree were checked for provenance and integrity, not read. Second, this is a point-in-time result
for `883bb7f`; it says nothing about later commits.
