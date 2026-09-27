#!/usr/bin/env node
// Fetches the "historical snapshots of Wikipedia articles" corpus: versions of one article at
// different moments in time → bench corpus format.
//
// **The difference from fetch-ai-timeline.mjs is where the whole point of this corpus lies.**
// That one fetches the **current** version of each article -- 15 retrospective summaries; pour
// one openai.txt in and the entire timeline from 2015 to today comes out in one go. It can act
// out world time (the sentences carry dates) but not **cognitive time**: every document is
// recorded at the same instant, `recorded_at` is all bunched together, and `supersedes` only
// ever happens inside a single document.
//
// Bitemporality has two axes, and until now we had only ever acted out one of them.
//
// This one fetches **historical versions**. Each snapshot is what people knew at that moment,
// and poured in in `doc_time` order the graph really does grow over time and really does change
// its mind:
//
//   The OpenAI article was 1,317 bytes when it was created on 2015-12-12 (a stub); by 2026 it
//   is 60KB+
//   In the 11-19 version of the Removal of Sam Altman article Murati is interim CEO; in the
//   11-22 version Altman is back
//
// **Sampling goes by "how much changed", not by the calendar.** Early on it changes once a
// month, later it sits still for half a year -- taking one per quarter would grab a pile of
// nearly identical snapshots late on (burning extraction for nothing) and miss the most violent
// stretch early on. So size growth is the signal: a snapshot is only taken once it has grown by
// GROWTH_PCT and by no less than GROWTH_ABS, with at least MIN_GAP_DAYS between two of them.
//
// Licence: Wikipedia body text is CC BY-SA 4.0, redistributable but **requiring attribution and
// share-alike**. Unlike the public-domain State of the Union addresses, the corpus file carries
// its own license field -- do not take it for the repository's main licence.
//
// **Neither the body text nor the manifest goes into the repository** (see .gitignore): the body
// text is roughly 7MB of CC BY-SA text, and the manifest is the product of one sampling run.
// The only thing that goes in is this script.
//
// But **within a single benchmark run the revision ids must be pinned**: the sampling is
// computed from the revision history as it stands now, the articles are still being edited, and
// re-running --dry a while later picks a different set of snapshots. So write a manifest with
// --manifest first and rebuild with --from-manifest from then on -- `action=parse&oldid` is
// immutable, and re-fetching from the same manifest at any time gives byte-identical text. That
// is the only thing that makes a controlled experiment hold.
//
// Usage: node scripts/bench/fetch-wiki-history.mjs --dry       # only report the sampling and sizes
//        node scripts/bench/fetch-wiki-history.mjs --manifest  # write the manifest (no body text)
//        node scripts/bench/fetch-wiki-history.mjs --from-manifest > scripts/bench/corpora/wiki-history.json

import { execFileSync } from "node:child_process";
import fs from "node:fs";

const UA = "Utopia-bench/0.1 (+https://utopia.bi; corpus builder)";
const DRY = process.argv.includes("--dry");
const WRITE_MANIFEST = process.argv.includes("--manifest");
const FROM_MANIFEST = process.argv.includes("--from-manifest");
const MANIFEST_PATH = "scripts/bench/corpora/wiki-history.manifest.json";

// **Go through curl, not fetch.** On this machine HTTP(S)_PROXY points at a local proxy, Node
// 20's undici does not read those two environment variables, so every fetch comes back
// UND_ERR_CONNECT_TIMEOUT while curl returns 200 for the same address. Same reason as in
// fetch-ai-timeline.mjs.
const curl = (url) =>
  execFileSync(
    "curl",
    ["-sSL", "--compressed", "--max-time", "90", "-A", UA, url],
    {
      encoding: "utf8",
      maxBuffer: 128 * 1024 * 1024,
    },
  );

const api = (params) => {
  const u = new URL("https://en.wikipedia.org/w/api.php");
  u.searchParams.set("format", "json");
  u.searchParams.set("formatversion", "2");
  for (const [k, v] of Object.entries(params)) u.searchParams.set(k, v);
  return JSON.parse(curl(u.toString()));
};

// The threshold for taking a snapshot. A snapshot is only taken when **both conditions hold**.
//
// The first version said "or", and the result was that the Elon Musk article (which grew to
// 340KB) gave up a snapshot every 6KB -- 89 from that one article, six tenths of the whole
// corpus -- while most of it is about Tesla/SpaceX/politics and would dilute the graph. With
// "and", big articles grow logarithmically in proportion, while small ones still have the
// absolute floor to keep the noise out.
const GROWTH_PCT = 0.18; // grown/shrunk by 18% against the last snapshot
const GROWTH_ABS = 6000; // and changed by no less than 6KB in absolute terms
const MIN_GAP_DAYS = 45; // at least this far apart (high-density windows are exempt)

// **The articles are picked to interlock**: organisations × people × products. The people are
// where the cross-links come from -- Altman appears in OpenAI / Y Combinator / Worldcoin, Musk
// in OpenAI / Tesla / xAI, Sutskever in OpenAI / SSI / Google Brain. Fetch the organisations
// alone and the graph is a handful of star clusters with nothing between them.
const TITLES = [
  // 1. The protagonist of a change of mind: four values in six days. This one is sampled by
  //    day, not by growth
  {
    title: "Removal of Sam Altman from OpenAI",
    daily: ["2023-11-19", "2023-11-29"],
  },

  // 2. Organisations
  { title: "OpenAI" },
  { title: "Anthropic" },
  { title: "DeepMind" },
  { title: "XAI (company)" },
  { title: "Mistral AI" },
  { title: "Hugging Face" },
  { title: "Stability AI" },
  { title: "Inflection AI" },
  { title: "Safe Superintelligence" },
  { title: "Scale AI" },
  { title: "Cohere" },

  // 3. People. **This layer is where "being connected" comes from**: the same person turns up
  //    in several organisations, and their affiliations change over time -- exactly what
  //    bitemporality is here to act out
  { title: "Sam Altman" },
  { title: "Elon Musk" },
  { title: "Ilya Sutskever" },
  { title: "Greg Brockman" },
  { title: "Mira Murati" },
  { title: "Dario Amodei" },
  { title: "Demis Hassabis" },
  { title: "Emmett Shear" },
  { title: "Satya Nadella" },

  // 4. Products: versions supersede one another, a natural valid_from/valid_to chain
  { title: "GPT-4" },
  { title: "ChatGPT" },
  { title: "Claude (language model)" },
  { title: "Gemini (language model)" },
  { title: "Llama (language model)" },
];

/// Lists every revision of an article (timestamp + size). Pages all the way through.
function revisions(title) {
  const out = [];
  let cont = null;
  for (let page = 0; page < 40; page++) {
    const p = {
      action: "query",
      prop: "revisions",
      titles: title,
      redirects: "1",
      rvlimit: "500",
      rvprop: "ids|timestamp|size",
      rvdir: "newer",
    };
    if (cont) p.rvcontinue = cont;
    const j = api(p);
    const pg = j.query.pages[0];
    if (pg.missing) throw new Error(`${title}: no such article`);
    out.push(...(pg.revisions || []));
    cont = j.continue?.rvcontinue;
    if (!cont) return { real: pg.title, revs: out };
  }
  return { real: title, revs: out };
}

const day = (ts) => ts.slice(0, 10);
const days = (a, b) => (new Date(b) - new Date(a)) / 86400000;

/// Whether this size stuck.
///
/// **"A lot changed" includes "somebody blanked the page".** We hit it for real: the
/// 2018-05-24T09:35:49 revision of the Elon Musk article was only 33 bytes (edit summary
/// "Replaced content with…"), 141637 → 33 clears both thresholds, so it got picked; 30 seconds
/// later it was reverted, and because the baseline had already been dragged down to 33, the
/// "restoration" counted as another huge change and got picked as well. **One act of vandalism
/// produced two junk snapshots and muddled the baseline for all the sampling that followed.**
///
/// A size floor does not stop this: an article being split (content moved out into a
/// sub-article) is a legitimate large shrink, and is indistinguishable from vandalism in terms
/// of "how much changed". What does tell them apart is **how long it lasts** -- vandalism is
/// reverted within minutes, a split stays. So look at whether the size PERSIST_DAYS days after
/// this revision is still of the same order.
const PERSIST_DAYS = 1;
function persists(revs, i) {
  const r = revs[i];
  for (let j = i + 1; j < revs.length; j++) {
    if (days(r.timestamp, revs[j].timestamp) < PERSIST_DAYS) continue;
    const hi = Math.max(revs[j].size, r.size);
    return hi === 0 || Math.abs(revs[j].size - r.size) / hi < 0.5;
  }
  return true; // there is no later revision: this one is the current state
}

/// Picks snapshots by "how much changed". The first and last revisions are always included.
function sampleByGrowth(revs) {
  const picked = [revs[0]];
  for (let i = 1; i < revs.length; i++) {
    const r = revs[i];
    const last = picked[picked.length - 1];
    const d = Math.abs(r.size - last.size);
    const grew = d >= GROWTH_ABS && d >= last.size * GROWTH_PCT;
    if (
      grew &&
      days(last.timestamp, r.timestamp) >= MIN_GAP_DAYS &&
      persists(revs, i)
    )
      picked.push(r);
  }
  const last = revs[revs.length - 1];
  if (picked[picked.length - 1].revid !== last.revid) picked.push(last);
  return picked;
}

/// High-density window: inside the window, take the last revision of each day. What this acts
/// out is "how many times we changed our mind inside a single day".
function sampleDaily(revs, [from, to]) {
  const byDay = new Map();
  for (let i = 0; i < revs.length; i++) {
    const d = day(revs[i].timestamp);
    // The last revision of a day can itself be vandalism (the day's final edit happening to be
    // a blanking), so it goes through the persistence check too
    if (d >= from && d < to && persists(revs, i)) byDay.set(d, revs[i]);
  }
  return [...byDay.values()];
}

// The trailing References / External links sections are nothing but links and template debris,
// and the extractor would treat them as body text. Cut them.
//
// **The closing side is written `=+` rather than `==+`, one notch wider on purpose.** Both sides
// used to be `==+` here, while the h-tag conversion below laid out `=` by level on the opening
// tag and hard-coded a single one on the closing tag, so a level-two heading came out as
// `== References =` and this regex never matched once -- **the entire references section of
// every single snapshot went into extraction**. Measured, 223 of 414 chunks (54%) were citation
// debris, and what it produced were facts like `Wired --employee--> Steven Levy` that take a
// journalist's byline for an employment relation, occupying the supersedes machinery on top of
// that.
//
// The closing-tag side is fixed now, but this one stays loose: whether the two sides of a
// heading are symmetrical is a rendering matter, whereas what has to be decided here is "from
// where on do we not want any of it". One notch of slack buys immunity from ever being punched
// through by that class of mismatch again, and the price is possibly cutting one extra
// level-one heading shaped like `= Foo =` -- and headings like that do not occur in article
// body text.
const CUT =
  /\n==+ ?(References|External links|See also|Further reading|Notes|Bibliography|Sources|Citations) ?=+/i;

/// Fetches the body text of one revision. Historical versions have no `prop=extracts` (that one
/// only knows the current version), so this goes through `action=parse&oldid=` for the rendered
/// HTML and strips it down to plain text.
function plaintext(revid) {
  const j = api({
    action: "parse",
    oldid: String(revid),
    prop: "text",
    disablelimitreport: "1",
  });
  let h = j.parse.text;
  return h
    .replace(/<style[\s\S]*?<\/style>/gi, "")
    .replace(/<script[\s\S]*?<\/script>/gi, "")
    .replace(/<table[\s\S]*?<\/table>/gi, "") // infoboxes/navboxes: all template debris
    // **Cut the citation list by structure, not by heading.** `CUT` recognises the
    // `== References ==` line, but the references are a block that gets rendered, and it is
    // **not necessarily under that heading**: `<references/>` renders in whichever section it
    // was written in. Measured, one of 216 snapshots was exactly like that (`Mistral AI`
    // @2024-12-10, the refs block landing inside Recent Developments), so the heading was cut
    // and 93 citations stayed in the body text all the same -- a third of that revision's body
    // text, and precisely the source of fake facts of the `Wired --employee--> Steven Levy`
    // kind.
    //
    // The container name is rendered by MediaWiki, not written by the article's authors, which
    // makes it steadier than the heading text (a heading can be translated, rewritten, or
    // written as `== Notes and references ==`).
    // The heading rule stays: it still covers the trailing External links / See also sections
    // that should not go into the corpus
    .replace(/<ol class="references"[\s\S]*?<\/ol>/gi, "")
    .replace(/<sup class="reference"[\s\S]*?<\/sup>/gi, "") // footnote superscripts
    .replace(/<span class="mw-editsection"[\s\S]*?<\/span>/gi, "")
    .replace(/<h([1-6])[^>]*>/gi, (_, n) => "\n\n" + "=".repeat(+n) + " ")
    // The closing tag is laid out by level too, symmetrical with the line above. Hard-coding a
    // single `=` makes a level-two heading come out as `== References =` while CUT is waiting
    // for `==`, so the trailing sections never get cut.
    .replace(/<\/h([1-6])>/gi, (_, n) => " " + "=".repeat(+n) + "\n")
    .replace(/<li[^>]*>/gi, "\n- ")
    .replace(/<\/(p|div|li|tr)>/gi, "\n")
    .replace(/<br\s*\/?>/gi, "\n")
    .replace(/<[^>]+>/g, "")
    .replace(/&nbsp;/g, " ")
    .replace(/&amp;/g, "&")
    .replace(/&lt;/g, "<")
    .replace(/&gt;/g, ">")
    .replace(/&quot;/g, '"')
    .replace(/&#(\d+);/g, (_, n) => String.fromCharCode(+n))
    .replace(/\[edit\]/g, "")
    .replace(/[ \t]+\n/g, "\n")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
}

const docs = [];
let plan = [];

if (FROM_MANIFEST) {
  // Rebuild exactly from the pinned revision ids. Does not touch the sampling logic and does
  // not look at the article's history as it stands now
  const m = JSON.parse(fs.readFileSync(MANIFEST_PATH, "utf8"));
  plan = m.titles.map((t) => ({
    real: t.real,
    total: t.total,
    picked: t.picked,
  }));
  process.stderr.write(
    `rebuilding from manifest: ${m.titles.length} articles, ${plan.reduce((n, q) => n + q.picked.length, 0)} snapshots\n`,
  );
} else
  for (const spec of TITLES) {
    try {
      const { real, revs } = revisions(spec.title);
      if (!revs.length) throw new Error("no revisions");
      const picked = spec.daily
        ? sampleDaily(revs, spec.daily)
        : sampleByGrowth(revs);
      plan.push({ real, total: revs.length, picked, slugBase: real });
      process.stderr.write(
        `${real.padEnd(36)} ${String(revs.length).padStart(5)} revisions → took ${String(picked.length).padStart(3)}` +
          `  ${day(picked[0].timestamp)} → ${day(picked[picked.length - 1].timestamp)}` +
          `  ${Math.round(picked[0].size / 1024)}KB → ${Math.round(picked[picked.length - 1].size / 1024)}KB\n`,
      );
    } catch (e) {
      process.stderr.write(`ERR ${spec.title}: ${e.message}\n`);
    }
  }

const snapshots = plan.reduce((n, p) => n + p.picked.length, 0);
const rawBytes = plan.reduce(
  (n, p) => n + p.picked.reduce((m, r) => m + r.size, 0),
  0,
);
process.stderr.write(
  `\n${snapshots} snapshots in total, ${(rawBytes / 1048576).toFixed(1)} MB raw (templates included, about half of that left once stripped)\n`,
);

if (WRITE_MANIFEST) {
  fs.writeFileSync(
    MANIFEST_PATH,
    JSON.stringify(
      {
        note:
          "Revision-id manifest for the wiki-history corpus. The body text does not go into " +
          "the repository (roughly 6MB of CC BY-SA text); --from-manifest rebuilds it byte for " +
          "byte from this manifest, because action=parse&oldid is immutable.",
        sampling: { GROWTH_PCT, GROWTH_ABS, MIN_GAP_DAYS },
        titles: plan.map((q) => ({
          real: q.real,
          total: q.total,
          picked: q.picked.map((r) => ({
            revid: r.revid,
            timestamp: r.timestamp,
            size: r.size,
          })),
        })),
      },
      null,
      1,
    ),
  );
  process.stderr.write(`--manifest: manifest written to ${MANIFEST_PATH}\n`);
  process.exit(0);
}

if (DRY) {
  process.stderr.write("--dry: sampling reported only, no body text fetched\n");
  process.exit(0);
}

process.stderr.write("\nfetching body text…\n");
for (const p of plan) {
  const slug = p.real
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-|-$/g, "");
  for (const r of p.picked) {
    try {
      const body = plaintext(r.revid).split(CUT)[0].trim();
      if (body.length < 400) {
        process.stderr.write(
          `  skip ${slug}@${day(r.timestamp)} body text is only ${body.length} characters\n`,
        );
        continue;
      }
      // The third element is doc_time -- the snapshot's **real revision instant**. The
      // extraction prompt gets hold of it (`extraction.rs` puts doc_time in formatted as
      // %Y-%m-%d), which is what makes the relative dates in the text resolvable; and pouring
      // the documents in sorted by it is what makes `recorded_at` spread out into a line
      // instead of bunching up into a single point
      docs.push([
        `${slug}@${day(r.timestamp)}.txt`,
        `${p.real}\n\n${body}\n`,
        r.timestamp,
      ]);
    } catch (e) {
      process.stderr.write(`  ERR ${slug}@${day(r.timestamp)}: ${e.message}\n`);
    }
  }
  process.stderr.write(`OK  ${p.real}\n`);
}

// **Sort by time**: the point of this corpus is in the order. Pouring it in out of order is
// back to "every document recorded at the same instant"
docs.sort((a, b) => (a[2] < b[2] ? -1 : 1));

const total = docs.reduce((n, [, t]) => n + t.length, 0);
process.stderr.write(
  `\n${docs.length} documents, ${total.toLocaleString()} characters, roughly ${Math.round(total / 950)} chunks\n`,
);

process.stdout.write(
  JSON.stringify(
    {
      name: "wiki-history",
      note:
        "Historical snapshots of Wikipedia articles, sampled by size growth (high-density " +
        "windows by day). Same topics as ai-timeline, but fetching **historical versions " +
        "rather than current ones** -- all 15 documents in that corpus are retrospective " +
        "summaries: pour one in and the whole timeline comes out in one go, which only acts " +
        "out world time. Here each snapshot is what people knew at that moment, poured in in " +
        "doc_time order, so recorded_at spreads out into a line and supersedes happens between " +
        "documents rather than inside a single one. The articles are picked to interlock " +
        "(organisations × people × products), and the people layer is where the cross-links " +
        "come from. Trailing sections such as References/External links and infobox tables " +
        "have been cut. Note: these topics are extremely common in model training data, which " +
        "makes them well suited to a demo (being recognised is an advantage) and unsuited as " +
        "an accuracy benchmark (what gets measured is recitation).",
      source:
        "https://en.wikipedia.org/ — action=query&prop=revisions + action=parse&oldid",
      license:
        "CC BY-SA 4.0 (attribution-sharealike, not the repository's main licence)",
      sampling: { GROWTH_PCT, GROWTH_ABS, MIN_GAP_DAYS },
      /// The third element of docs is doc_time (an ISO instant). Older corpora have only two
      /// elements, and run.mjs copes with both
      docs,
    },
    null,
    1,
  ),
);
