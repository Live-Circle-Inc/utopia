#!/usr/bin/env node
// The measuring bench for type resolution: **one fresh KB per run**.
//
// The reason it exists is a hole I once fell into: three rounds in a row of tuning retrieval on
// the same KB, while that KB was still carrying the retyping results of the earlier rounds -- the
// easy entities had long since been refined, and the rejection reasons read "already correctly
// typed as pharmacy". The numbers from the last two rounds were not comparable to the first at
// all, and I still used them as grounds for two code changes.
//
// One run = create a knowledge base → load a fixed corpus → optionally import an ontology → run
// type resolution → score against the ground truth.
// Both the corpus and the ground truth live in the repo (scripts/bench/), so anyone who reruns it
// gets the same set of numbers.
//
// Usage:
//   node scripts/bench/run.mjs --corpus pharma --label seeds-only
//   node scripts/bench/run.mjs --corpus pharma --ontology /tmp/schemaorg.ttl --label schemaorg
//
// Environment variables: BENCH_BASE (default http://localhost:18080), BENCH_EMAIL / BENCH_PASSWORD,
//           BENCH_PSQL (default docker exec … psql; the ontology section's character count has to
//           be read straight out of the database).

import fs from "node:fs";
import path from "node:path";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const BASE = process.env.BENCH_BASE || "http://localhost:18080";
const EMAIL = process.env.BENCH_EMAIL || "bench@test.local";
const PASSWORD = process.env.BENCH_PASSWORD || "benchbench123";

// Measured: 4.0 chars ≈ 1 token (377,735↔81,855 and 396,716↔99,041 -- 4.0 both times).
//
// The real token count of the extraction prompt cannot be had -- it lives inside the LLM client,
// and threading it out would mean changing a whole chain of signatures. The character count of
// the ontology section is stably proportional to it, and what we want to measure here is exactly
// "ontology size", so it is good enough and leaves the client alone.
const CHARS_PER_TOKEN = 4.0;
// What "untouched" looks like. Since 0009 deleted the built-in classes, an entity the ontology
// cannot hold stays at `type_id IS NULL`, which is read out as `-` -- **that is the baseline for
// judging "did anything that should not have been changed get changed"**.
//
// This used to be the nine built-in class names (concept/person/organization…). That seed set no
// longer exists, and keeping it would make every unclassified entity count as "changed", inflating
// wronglyChanged outright.
const UNTOUCHED = "-";

const args = Object.fromEntries(
  process.argv.slice(2).reduce((acc, cur, i, arr) => {
    if (cur.startsWith("--")) acc.push([cur.slice(2), arr[i + 1]]);
    return acc;
  }, []),
);
const corpusName = args.corpus || "pharma";
const label = args.label || corpusName;

let cookie = "";
async function api(method, url, body, isForm) {
  const init = { method, headers: {} };
  if (cookie) init.headers.cookie = cookie;
  if (isForm) init.body = body;
  else if (body !== undefined) {
    init.headers["content-type"] = "application/json";
    init.body = JSON.stringify(body);
  }
  const r = await fetch(BASE + url, init);
  for (const c of r.headers.getSetCookie?.() ?? []) cookie = c.split(";")[0];
  const text = await r.text();
  if (!r.ok) throw new Error(method + " " + url + " -> " + r.status + " " + text.slice(0, 200));
  return text ? JSON.parse(text) : null;
}

function psql(sql) {
  const cmd =
    process.env.BENCH_PSQL ||
    "docker exec -e PGPASSWORD=utopia landscapebi-db-1 psql -U utopia -d utopia -tAc";
  const parts = cmd.split(" ");
  return execFileSync(parts[0], [...parts.slice(1), sql], { encoding: "utf8" }).trim();
}
const num = (sql) => Number(psql(sql) || 0);

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
/// **Only being stuck counts as a timeout; being slow does not.**
///
/// This used to be "an overall cap of 15 minutes", so a 348-chunk corpus was killed every single
/// time before the documents had finished extracting -- all three runs went that way, and not one
/// result JSON came back, while the server had in fact been running along just fine the whole
/// time (the jobs queue up on the server, and the driver script dying does not affect them). A
/// bench that reports a successful run as a failure is worse than reporting nothing.
///
/// And there is no number you can pick for such a cap: a single chunk takes a minute, a 20-chunk
/// corpus finishes in three minutes, 348 chunks take seventy-five, and one overall cap cannot
/// serve both ends. So it watches **progress** instead -- `fn` reports a progress value each
/// time round, and as long as that keeps moving the alarm clock gets pushed back.
///
/// `fn` returning true means done; returning a number means "not done yet, and this is the
/// current progress".
async function until(fn, everyMs, stallMs) {
  const stall = stallMs || 900000;
  let deadline = Date.now() + stall;
  let last = null;
  for (;;) {
    const r = await fn();
    if (r === true) return;
    if (typeof r === "number" && r !== last) {
      last = r;
      deadline = Date.now() + stall;
    }
    if (Date.now() > deadline) {
      throw new Error(`Timed out: no progress at all for ${Math.round(stall / 60000)} minutes`);
    }
    await sleep(everyMs || 5000);
  }
}

async function main() {
  const corpus = JSON.parse(
    fs.readFileSync(path.join(HERE, "corpora", corpusName + ".json"), "utf8"),
  );
  // The truth key is **optional**. Some corpora are not accuracy benchmarks: the holmes one is
  // demo b-roll plus an entity-resolution fixture, the model read it long ago, and measuring type
  // accuracy on it measures memorisation rather than this pipeline.
  // With no truth key we report only size, timings and the shape of the graph, and do not score --
  // more honest than making up a fake answer key
  const truthPath = path.join(HERE, "truth", corpusName + ".json");
  const truth = fs.existsSync(truthPath)
    ? JSON.parse(fs.readFileSync(truthPath, "utf8")).expect
    : null;

  try {
    await api("POST", "/api/v1/auth/register", {
      email: EMAIL,
      display_name: "bench",
      password: PASSWORD,
    });
  } catch {
    // Already registered, go to login
  }
  await api("POST", "/api/v1/auth/login", { email: EMAIL, password: PASSWORD });
  psql("UPDATE users SET is_admin=TRUE WHERE email='" + EMAIL + "'");
  await api("POST", "/api/v1/auth/login", { email: EMAIL, password: PASSWORD });

  const ws = (await api("GET", "/api/v1/workspaces"))[0].id;
  // **A fresh KB per run**: this one line is the reason the whole script exists; do not reuse one
  // just to save a few minutes
  const stamp = new Date().toISOString().replace(/[:.]/g, "-");
  const kb = (
    await api("POST", "/api/v1/workspaces/" + ws + "/kbs", {
      name: "bench " + label + " " + stamp,
      // --packs schema-org,prov-o: takes **the product's actual cold-start path** (the packs are
      // installed right as the KB is created). Not the same thing as --ontology: that one imports
      // a file after the KB is built, and only measures the prompt overhead
      ontology_packs: args.packs ? args.packs.split(",").map((x) => x.trim()) : [],
    })
  ).id;
  // **Auto-extending the ontology is off by default**: it would change the ontology midway
  // through the measurement, and then two runs would no longer be comparing the same thing.
  //
  // But switching it off also means **cold start has never been measured** -- a freshly created
  // KB has only 10 default relations, and the product's answer is to fill the ontology in
  // automatically once extraction is done (bootstrap_ontology, column default true), whereas here
  // it is uniformly FALSE, so the related_to share the bench reports has always been the number
  // "after the mechanism was switched off". Using that to say how bad the product's cold start is
  // means taking your own switch for a conclusion.
  //
  // So give it a switch. Run it on and you measure **the product's actual behaviour**; run it off
  // and you measure **a single variable**. We want both -- do not keep only one.
  const autoExtend = "auto-extend" in args;
  if (!autoExtend) {
    psql("UPDATE knowledge_bases SET auto_extend_ontology=FALSE WHERE id='" + kb + "'");
  }
  await sleep(6000);

  // **The order of importing and loading the corpus measures two different things.**
  //
  // Corpus first, import second (the default): extraction only ever sees the seed ontology, and
  // the big ontology acts only on the after-the-fact resolution.
  // Import first, corpus second (--ontology-first): extraction sees the big ontology right there
  // and then, and what gets measured is the prompt.
  //
  // This one once led me to a wrong conclusion: under the default order it reported a 108k
  // ontology section, and I said "a 108k prompt ate 5 entities" -- when in that run the prompt
  // held only the 9 seed classes at extraction time, the extraction inputs of the two runs were
  // in fact identical, and 25 vs 18 was run-to-run variance. So both ontology_size values below
  // get recorded, each labelled with when it was measured.
  const ontologyFirst = "ontology-first" in args;

  async function importOntology() {
    if (!args.ontology) return 0;
    const t1 = Date.now();
    const form = new FormData();
    form.append(
      "file",
      new Blob([fs.readFileSync(args.ontology)]),
      path.basename(args.ontology),
    );
    await api("POST", "/api/v1/kbs/" + kb + "/ontology/imports", form, true);
    // Retrieval only means anything once the class vectors are built. The relation half is
    // filled in by a background job, and type resolution does not need it.
    //
    // A cold start with a thousand classes takes anywhere from a few minutes to tens of minutes
    // -- it fights other KBs' backfill jobs for the same embedding concurrency semaphore. So the
    // limit is relaxed to 40 minutes, and the remaining count is written to stderr: waiting
    // twenty minutes in silence, you cannot tell whether it is running or wedged.
    await until(
      async () => {
        // **Both sets of vectors have to be waited for** (0050). Waiting only on `embedding`
        // means starting before the label set is filled in, and then the short-form path
        // retrieves nothing at all -- what you measure is a half-built thing, and you cannot tell
        const left = num(
          "SELECT count(*) FILTER (WHERE embedding IS NULL)" +
            " + count(*) FILTER (WHERE label_embedding IS NULL)" +
            " FROM entity_types WHERE kb_id='" +
            kb +
            "'",
        );
        if (left) process.stderr.write("  class vectors still missing: " + left + "\n");
        // Return the remaining count as progress: if it is going down nothing is stuck (until
        // only looks at "did it move")
        return left === 0 ? true : left;
      },
      10000,
      2400000,
    );
    return Date.now() - t1;
  }

  const sizeNow = () => {
    const c = num(
      "SELECT coalesce(sum(length('- '||key||coalesce(': '||nullif(description,''),''))+1),0)" +
        " FROM entity_types WHERE kb_id='" +
        kb +
        "'",
    );
    const r = num(
      "SELECT coalesce(sum(length('- '||key||' (x)'||coalesce(': '||nullif(description,''),''))+1),0)" +
        " FROM relation_types WHERE kb_id='" +
        kb +
        "' AND kind<>'attribute'",
    );
    const a = num(
      "SELECT coalesce(sum((length('- '||r.key||' (text)'||coalesce(': '||nullif(r.description,''),''))+1)" +
        " * greatest(1,(SELECT count(*) FROM relation_type_domains d WHERE d.relation_type_id=r.id))),0)" +
        " FROM relation_types r WHERE r.kb_id='" +
        kb +
        "' AND r.kind='attribute'",
    );
    return {
      classes: num("SELECT count(*) FROM entity_types WHERE kb_id='" + kb + "'"),
      relations: num(
        "SELECT count(*) FROM relation_types WHERE kb_id='" + kb + "' AND kind<>'attribute'",
      ),
      attributes: num(
        "SELECT count(*) FROM relation_types WHERE kb_id='" + kb + "' AND kind='attribute'",
      ),
      prompt_chars: c + r + a,
      prompt_tokens_est: Math.round((c + r + a) / CHARS_PER_TOKEN),
    };
  };

  let importMs = 0;
  if (ontologyFirst) importMs = await importOntology();

  // **How big the ontology was at extraction time** -- this is the size the prompt actually saw.
  //
  // Touch the ontology once first: the seed classes are created **lazily** (they only hit the
  // database on the first ontology read or extraction), and without touching it first you measure
  // the imported ones and miss the 9 seeds. The first version did miss them, and the symptom was
  // 24 classes at extraction and 32 at resolution, which looked like somebody had changed the
  // ontology midway
  await api("GET", "/api/v1/kbs/" + kb + "/ontology");
  const atExtraction = sizeNow();

  const t0 = Date.now();
  for (const [filename, content, docTime] of corpus.docs) {
    // The third element is doc_time (only historical-snapshot corpora have it; older corpora
    // have just two elements, so it is undefined here).
    // It goes into two places at once: the extraction prompt (extraction.rs inserts it as
    // %Y-%m-%d, which is what makes relative dates inside the text resolvable) and
    // documents.doc_time (the timeline orders by it). Without it, 247 snapshots all pile up as
    // having been recorded at the same instant
    const body = { filename, content };
    if (docTime) body.doc_time = docTime;
    await api("POST", "/api/v1/kbs/" + kb + "/ingest", body);
  }
  // Progress counts **chunks**, not documents. The document count is a very coarse scale: a
  // 73-chunk document takes more than an hour, during which the document count does not budge,
  // which looks exactly like being wedged
  await until(async () => {
    const done = num(
      "SELECT count(*) FROM documents WHERE kb_id='" + kb + "' AND graph_status='done'",
    );
    if (done >= corpus.docs.length) return true;
    const chunks = num(
      "SELECT count(*) FROM chunks WHERE kb_id='" + kb + "' AND extracted_at IS NOT NULL",
    );
    process.stderr.write(`  extracted ${chunks} chunks / ${done} docs done\n`);
    return chunks;
  }, 15000);
  const extractMs = Date.now() - t0;

  if (!ontologyFirst) importMs = await importOntology();

  // How big the ontology is at resolution time (with corpus first, it differs from extraction
  // time)
  const atResolution = sizeNow();

  const t2 = Date.now();
  const outcome = await api("POST", "/api/v1/kbs/" + kb + "/ontology/type-resolution");
  const resolveMs = Date.now() - t2;

  // Scoring. **Anything awaiting a human counts as "not changed"** -- it genuinely has not been
  // changed yet, and counting it as a hit would be charging a human's work to the machine.
  //
  // **LEFT JOIN, and write `-` when there is no class** (0009). An inner join makes unclassified
  // entities vanish entirely, so they get counted under absent -- "extraction never pulled it out
  // at all" -- when in fact it was pulled out and simply never typed. The two failures are fixed
  // in completely different ways, and mixing them into one column makes this whole table
  // pointless.
  const rows = psql(
    "SELECT e.canonical_name || '|' || coalesce(t.key, '-') FROM entities e" +
      " LEFT JOIN entity_types t ON t.id=e.type_id" +
      " WHERE e.kb_id='" +
      kb +
      "' AND e.merged_into IS NULL",
  )
    .split("\n")
    .filter(Boolean)
    .map((l) => {
      const i = l.lastIndexOf("|");
      return [l.slice(0, i), l.slice(i + 1)];
    });

  let hit = 0;
  let miss = 0;
  let correctlyLeft = 0;
  let wronglyChanged = 0;
  let absent = 0;
  const notes = [];
  for (const [frag, accept] of Object.entries(truth ?? {})) {
    // Match by fragment rather than by equality: the names extraction returns differ slightly
    // every time ("Nebula Tech" / "Nebula Tech (Shanghai) Co., Ltd"), and equality would score
    // that variation as a failure
    const found = rows.filter(([name]) => name.includes(frag));
    if (found.length === 0) {
      absent += 1;
      continue;
    }
    const keys = found.map((r) => r[1]);
    if (accept.length === 0) {
      // No matching class exists in the ontology: the correct behaviour is **to leave it
      // alone**, and only changing it counts as an error
      if (keys.some((k) => k !== UNTOUCHED)) {
        wronglyChanged += 1;
        notes.push(frag + ": should not have been changed, yet became " + keys.join("/"));
      } else correctlyLeft += 1;
    } else if (keys.some((k) => accept.includes(k))) {
      hit += 1;
    } else {
      miss += 1;
      notes.push(frag + ": expected " + accept.join("|") + ", got " + keys.join("/"));
    }
  }

  console.log(
    JSON.stringify(
      {
        label,
        corpus: corpusName,
        ontology: args.ontology ? path.basename(args.ontology) : null,
        kb_id: kb,
        order: ontologyFirst ? "ontology-first" : "documents-first",
        // The switch is written into the result rather than left to someone's memory -- the last
        // premise that did not get written down (when the ontology size was measured) has already
        // led me to a wrong conclusion
        auto_extend_ontology: autoExtend,
        // **Two of them, each labelled with when it was measured.** Reporting only one gets read
        // as "the prompt used for extraction was this big", when with corpus first extraction
        // never saw it at all -- that misreading has already happened once
        ontology_at_extraction: atExtraction,
        ontology_at_resolution: atResolution,
        graph: {
          entities: num(
            "SELECT count(*) FROM entities WHERE kb_id='" +
              kb +
              "' AND merged_into IS NULL",
          ),
          facts: num(
            "SELECT count(*) FROM facts WHERE kb_id='" +
              kb +
              "' AND invalidated_at IS NULL",
          ),
        },
        resolution: {
          retyped: outcome.retyped,
          for_review: outcome.for_review.length,
          left_alone: outcome.left_alone.length,
        },
        score: truth
          ? { hit, miss, correctlyLeft, wronglyChanged, absent, notes }
          : "no truth key, not scored",
        ms: { extract: extractMs, import: importMs, resolve: resolveMs },
      },
      null,
      2,
    ),
  );
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
