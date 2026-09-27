#!/usr/bin/env node
// End-to-end: type-drift handling in entity resolution.
//
// Scenario: the same team name is extracted as three different types by three documents --
// organization / project / team (that last one is off the whitelist, so it falls back to
// concept). Expected: the drifting pairs land in the review queue → the mock adjudicates
// every one of them "same" → the chained merges converge on a single entity (whose type,
// once reconciled, is not concept), and the leads timeline is coherent (the predecessor is
// closed automatically at the successor's start).
//
// This script embeds a mock LLM (OpenAI-compatible /chat/completions: it replays extraction
// results keyed off the chunk markers, and recognises adjudication requests by the shape of
// the prompt and judges them all same). With no embedding configured → we take the "no
// context to compare" path: rather split than merge, plus a review pair.
//
// Prerequisite: utopia-server is running and connected to a clean database. See
// scripts/e2e_type_drift.sh for the orchestration.
// Environment variables: E2E_BASE (default http://127.0.0.1:8317), E2E_MOCK_PORT
// (default 9317).

import http from "node:http";

const BASE = process.env.E2E_BASE || "http://127.0.0.1:8317";
const MOCK_PORT = Number(process.env.E2E_MOCK_PORT || 9317);

// ---------------------------------------------------------------------------
// mock LLM
// ---------------------------------------------------------------------------

const EXTRACTIONS = {
  "E2E-DOC1": {
    entities: [
      { name: "Orion platform team", type: "organization" },
      { name: "Alice Zhang", type: "person" },
    ],
    facts: [
      {
        subject: "Alice Zhang",
        predicate: "leads",
        object: "Orion platform team",
        valid_from: "2023-01-15",
        confidence: 0.95,
        quote: "Alice Zhang has led the Orion platform team since 2023-01-15.",
      },
    ],
  },
  "E2E-DOC2": {
    entities: [
      { name: "Orion platform team", type: "project" },
      { name: "Bob Li", type: "person" },
    ],
    facts: [
      {
        subject: "Bob Li",
        predicate: "leads",
        object: "Orion platform team",
        valid_from: "2024-05-20",
        confidence: 0.95,
        quote: "Bob Li took over as lead of the Orion platform team on 2024-05-20.",
      },
    ],
  },
  // "team" is not in the built-in ontology whitelist → the extraction side degrades it to
  // concept (record_miss + fallback), which is exactly the third leg of the type drift
  "E2E-DOC3": {
    entities: [
      { name: "Orion platform team", type: "team" },
      { name: "Utopia Labs", type: "organization" },
    ],
    facts: [
      {
        subject: "Orion platform team",
        predicate: "part_of",
        object: "Utopia Labs",
        confidence: 0.9,
        quote: "The Orion platform team is part of Utopia Labs.",
      },
    ],
  },
};

function mockReply(messages) {
  const all = messages.map((m) => m.content).join("\n");
  let payload;
  if (all.includes("entity-resolution adjudicator")) {
    const pairs = [...all.matchAll(/^Pair (\d+):/gm)].map((m) => Number(m[1]));
    payload = {
      verdicts: pairs.map((i) => ({ i, verdict: "same", confidence: 0.95 })),
    };
  } else {
    const marker = Object.keys(EXTRACTIONS).find((k) => all.includes(k));
    payload = marker ? EXTRACTIONS[marker] : { entities: [], facts: [] };
  }
  return "```json\n" + JSON.stringify(payload) + "\n```";
}

function startMock() {
  return new Promise((resolve) => {
    const srv = http.createServer((req, res) => {
      let body = "";
      req.on("data", (c) => (body += c));
      req.on("end", () => {
        if (!req.url.endsWith("/chat/completions")) {
          res.writeHead(404).end();
          return;
        }
        const messages = JSON.parse(body).messages || [];
        res.writeHead(200, { "content-type": "application/json" });
        res.end(
          JSON.stringify({
            choices: [{ message: { content: mockReply(messages) } }],
          }),
        );
      });
    });
    srv.listen(MOCK_PORT, "127.0.0.1", () => resolve(srv));
  });
}

// ---------------------------------------------------------------------------
// API driver
// ---------------------------------------------------------------------------

let cookie = "";

async function api(method, path, body) {
  const res = await fetch(`${BASE}/api/v1${path}`, {
    method,
    headers: {
      "content-type": "application/json",
      ...(cookie ? { cookie } : {}),
    },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const setCookie = res.headers.get("set-cookie");
  if (setCookie) cookie = setCookie.split(";")[0];
  const text = await res.text();
  if (!res.ok) throw new Error(`${method} ${path} -> ${res.status}: ${text}`);
  return text ? JSON.parse(text) : {};
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function poll(desc, timeoutMs, fn) {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const v = await fn();
    if (v !== undefined) return v;
    if (Date.now() > deadline) throw new Error(`timeout waiting for: ${desc}`);
    await sleep(1500);
  }
}

let failures = 0;
function check(cond, label) {
  console.log(`${cond ? "PASS" : "FAIL"}: ${label}`);
  if (!cond) failures++;
}

async function pushDocAndWait(kbId, filename, content) {
  await api("POST", `/kbs/${kbId}/ingest`, { filename, content });
  // Wait for extraction to finish one document at a time: resolution sees the previous
  // document's entities in persistence order, so the drift path fires deterministically
  await poll(`${filename} graph done`, 120000, async () => {
    const documents = await api("GET", `/kbs/${kbId}/documents`);
    const d = documents.find((d) => d.filename === filename);
    if (d && d.graph_status === "failed") throw new Error(`${filename} extraction failed`);
    return d && d.graph_status === "done" ? true : undefined;
  });
}

async function main() {
  const mock = await startMock();
  console.log(`mock LLM on 127.0.0.1:${MOCK_PORT}`);

  const stamp = Date.now();
  await api("POST", "/auth/register", {
    email: `e2e-drift-${stamp}@test.local`,
    password: "password123",
    display_name: "Drift E2E",
  });
  const wsId = (await api("POST", "/workspaces", { name: `Drift WS ${stamp}` })).id;
  const kbId = (await api("POST", `/workspaces/${wsId}/kbs`, { name: "Drift KB" })).id;

  await api("PUT", `/workspaces/${wsId}/settings`, {
    chat_base_url: `http://127.0.0.1:${MOCK_PORT}/v1`,
    chat_api_key: "mock",
    chat_model: "mock-adjudicator",
  });

  await pushDocAndWait(kbId, "orion-org-chart.md",
    "[E2E-DOC1] Org chart update. Alice Zhang has led the Orion platform team since 2023-01-15.");
  await pushDocAndWait(kbId, "orion-roadmap.md",
    "[E2E-DOC2] Roadmap. Bob Li took over as lead of the Orion platform team on 2024-05-20.");
  await pushDocAndWait(kbId, "orion-wiki.md",
    "[E2E-DOC3] Wiki. The Orion platform team is part of Utopia Labs.");

  // Batched adjudication does the chained merges in the background: wait until the review
  // queue is empty and the same-name entities have converged to one
  const survivor = await poll("adjudication converges to one entity", 90000, async () => {
    const review = await api("GET", `/kbs/${kbId}/review`);
    const { entities } = await api("GET", `/kbs/${kbId}/entities?q=Orion`);
    return review.reviews.length === 0 && entities.length === 1 ? entities[0] : undefined;
  });

  check(survivor.name === "Orion platform team", `survivor name (${survivor.name})`);
  check(survivor.type_key !== "concept",
    `type reconciled to a specific type, not the concept fallback (${survivor.type_key})`);
  check(survivor.disambiguator === null || survivor.disambiguator === undefined,
    "no disambiguator once the group collapsed to one entity");

  const review = await api("GET", `/kbs/${kbId}/review`);
  check(review.merges.length >= 2,
    `chain merges recorded in entity_merges ledger (${review.merges.length})`);
  check(review.merges.every((m) => /auto-merge/.test(m.reason ?? "")),
    "merges came from LLM adjudication, not manual action");

  const { facts } = await api("GET", `/kbs/${kbId}/entities/${survivor.id}`);
  const leads = facts.filter((f) => f.predicate_key === "leads" && f.direction === "in");
  check(leads.length === 2, `one coherent leads timeline with 2 live facts (${leads.length})`);
  const alice = leads.find((f) => f.other_name === "Alice Zhang");
  const bob = leads.find((f) => f.other_name === "Bob Li");
  check(!!alice && (alice.valid_from ?? "").startsWith("2023-01-15"),
    "Alice's tenure starts 2023-01-15");
  check(!!alice && (alice.valid_to ?? "").startsWith("2024-05-20"),
    `Alice's tenure auto-closed at Bob's start (valid_to=${alice?.valid_to})`);
  check(!!bob && (bob.valid_from ?? "").startsWith("2024-05-20") && bob.valid_to === null,
    "Bob's tenure open from 2024-05-20");
  const partOf = facts.find((f) => f.predicate_key === "part_of" && f.direction === "out");
  check(!!partOf && partOf.other_name === "Utopia Labs",
    "concept-side fact (part_of Utopia Labs) survived the merge onto the survivor");

  // Revertible: undo the most recent merge → the merged-away side comes back to life, its
  // type is restored from the snapshot, and the same-name group shows up again
  await api("POST", `/kbs/${kbId}/merges/${review.merges[0].id}/revert`);
  const after = await api("GET", `/kbs/${kbId}/entities?q=Orion`);
  check(after.entities.length === 2,
    `revert revives the merged-away entity (${after.entities.length} entities)`);
  const revived = after.entities.find((e) => e.id !== survivor.id);
  check(!!revived && revived.type_key !== survivor.type_key,
    `revived entity keeps its own original type (${revived?.type_key})`);
  const ledger = await api("GET", `/kbs/${kbId}/review`);
  check(ledger.merges.some((m) => m.reverted_at !== null),
    "ledger marks the merge as reverted");

  mock.close();
  if (failures > 0) {
    console.error(`\n${failures} assertion(s) failed`);
    process.exit(1);
  }
  console.log("\nE2E type-drift: all assertions passed");
}

main().catch((e) => {
  console.error(`E2E error: ${e.message}`);
  process.exit(1);
});
