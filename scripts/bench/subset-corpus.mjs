#!/usr/bin/env node
// Picks a few articles out of one corpus and makes a new corpus out of them.
//
// The reason this exists is that `wiki-history` never finishes: 219 snapshots are roughly 7000
// chunks, which at the measured rate takes six to seven hours, and it also runs into the
// endpoint's per-minute token quota. And **the two questions it answers place different demands
// on the corpus**:
//
// - "does the violation rate dropping to zero hold up" is a statistics question. 60 chunks yield
//   149 checkable facts, and by the rule of three the confidence upper bound for zero violations
//   is `3/n`, so a few hundred chunks is already conclusive; running more adds no information.
// - "will the graph genuinely change its mind" is a structural question, and statistics cannot
//   help. What it needs is **the whole snapshot chain going in in `doc_time` order**, because
//   `supersedes` only happens between adjacent snapshots of the same article.
//
// So the way to slice is **whole articles, never chunks**: random chunk sampling satisfies the
// first question and destroys the second outright. A handful of articles that interlock with each
// other is worth more than a few thousand extra chunks.
//
// The output is sorted ascending by `doc_time`. The load order *is* the order in which knowledge
// grows, and a graph loaded out of order is meaningless along the provenance-time axis.
//
// Usage: node scripts/bench/subset-corpus.mjs <corpus.json> <article,article,...> > new-corpus.json
//
// Example (the November 2023 OpenAI affair, seven articles that share entities):
//   node scripts/bench/subset-corpus.mjs scripts/bench/corpora/wiki-history.json \
//     openai,removal-of-sam-altman-from-openai,sam-altman,ilya-sutskever,\
//     mira-murati,greg-brockman,emmett-shear \
//     > scripts/bench/corpora/wiki-nov2023.json
//
// With no article argument it only lists which articles the source corpus has and how many
// snapshots each one has, and emits no corpus.

import fs from "node:fs";

const [, , src, titlesRaw] = process.argv;
if (!src) {
  console.error("usage: subset-corpus.mjs <corpus.json> [article,article,...]");
  console.error("       with no articles it only lists the source corpus's articles");
  process.exit(2);
}

const corpus = JSON.parse(fs.readFileSync(src, "utf8"));
if (!Array.isArray(corpus.docs)) {
  console.error(`${src} has no docs array, this does not look like a corpus`);
  process.exit(2);
}

/// Filenames look like `openai@2023-11-19.txt`; everything before the `@` is the article.
/// Current-revision corpora (fetch-ai-timeline) have no `@`, so the whole filename is the article.
const titleOf = (filename) => filename.replace(/@.*$/, "").replace(/\.txt$/, "");

// The article list: snapshot count and size. For looking at before you pick
const groups = new Map();
for (const [filename, text] of corpus.docs) {
  const t = titleOf(filename);
  const g = groups.get(t) || { n: 0, chars: 0 };
  g.n += 1;
  g.chars += text.length;
  groups.set(t, g);
}

if (!titlesRaw) {
  const rows = [...groups].sort((a, b) => b[1].chars - a[1].chars);
  const total = rows.reduce((s, [, g]) => s + g.chars, 0);
  for (const [t, g] of rows) {
    const pct = ((100 * g.chars) / total).toFixed(1).padStart(5);
    console.error(
      `${String(g.n).padStart(4)} snaps  ${String(Math.round(g.chars / 1000)).padStart(6)}k  ${pct}%  ${t}`,
    );
  }
  console.error(`\n${rows.length} articles, ${corpus.docs.length} snapshots, ${(total / 1e6).toFixed(2)}M characters in total`);
  process.exit(0);
}

const wanted = new Set(titlesRaw.split(",").map((t) => t.trim()).filter(Boolean));

// **An unrecognised article name has to be an error; it must not silently produce a small corpus.**
// One typo means one article fewer, and the article that went missing is exactly where the shared
// entities came from, so the graph falls apart into mutually disconnected clusters --
// and in the results that just looks like "it did not work as well", with no way to trace it back.
const unknown = [...wanted].filter((t) => !groups.has(t));
if (unknown.length) {
  console.error(`the source corpus does not have these articles: ${unknown.join(", ")}`);
  console.error(`re-run with no article argument to see all the article names.`);
  process.exit(2);
}

const docs = corpus.docs
  .filter(([filename]) => wanted.has(titleOf(filename)))
  // Ascending by doc_time. When the third element is absent (current-revision corpora) fall back
  // to sorting by filename, which is at least deterministic
  .sort((a, b) => String(a[2] ?? a[0]).localeCompare(String(b[2] ?? b[0])));

const chars = docs.reduce((s, d) => s + d[1].length, 0);

process.stdout.write(
  JSON.stringify({
    name: `${corpus.name}-subset`,
    note:
      `A subset of ${corpus.name}, articles: ${[...wanted].join(", ")}. ` +
      `Taken whole-article and sorted ascending by doc_time, because supersedes only happens between adjacent snapshots of the same article. ` +
      (corpus.note ? ` Source corpus notes: ${corpus.note}` : ""),
    source: corpus.source,
    license: corpus.license,
    sampling: corpus.sampling,
    subset_of: corpus.name,
    docs,
  }),
);

// The stats go to stderr so that stdout can be redirected straight into a corpus file
console.error(
  `${docs.length} snapshots, ${Math.round(chars / 1000)}k characters, ` +
    // 1200-character budget, 150 overlap; see chunk_text in utopia-ingest
    `~${Math.round(chars / 1050)} chunks`,
);
