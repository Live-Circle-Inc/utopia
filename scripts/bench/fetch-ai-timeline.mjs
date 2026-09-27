#!/usr/bin/env node
// Fetches the "AI company milestones" corpus: Wikipedia article text -> bench corpus format.
//
// Why this batch rather than the State of the Union: the State of the Union was measured once,
// and of 758 facts only 33 carried a date (4.4%), with just 2 supersedes. Political speech
// **asserts**, it does not **record**, and its sentences simply have no dates in them. Every
// article picked here has facts shaped like "on such-and-such a date, X did Y".
//
// More importantly, **the same predicate changes value**: OpenAI's CEO changed four times in
// the six days from 2023-11-17 to 11-22 (Altman -> Murati -> Shear -> Altman). That is what
// bitemporality is here to demonstrate -- not "the knowledge base has time fields", but "we
// believed three different answers to the same question in turn, and all three are still there".
//
// Licence: Wikipedia article text is CC BY-SA 4.0, redistributable but **requiring attribution
// and share-alike**. That differs from the public-domain State of the Union, so the corpus file
// carries its own license field; do not mistake it for the repository's main licence.
//
// Usage: node scripts/bench/fetch-ai-timeline.mjs > scripts/bench/corpora/ai-timeline.json

import { execFileSync } from "node:child_process";

const UA = "Utopia-bench/0.1 (+https://utopia.bi; corpus builder)";

// **Goes through curl, not fetch.** On this machine HTTP(S)_PROXY points at a local proxy, and
// Node 20's undici does not read those two environment variables (NODE_USE_ENV_PROXY only
// arrived in 24), so every fetch ended in UND_ERR_CONNECT_TIMEOUT while curl returned 200 for
// the same address. A corpus script is a one-off tool; pulling in an undici ProxyAgent
// dependency for it is not worth it.
const curl = (url) =>
  execFileSync("curl", ["-sSL", "--compressed", "--max-time", "60", "-A", UA, url], {
    encoding: "utf8",
    maxBuffer: 64 * 1024 * 1024,
  });

// Three tiers, each carrying one shot of the demo
const TITLES = [
  // 1. Belief change: one predicate, six days, four values. The star of the whole corpus
  "Removal of Sam Altman from OpenAI",
  // 2. Institutions: founding dates, funding rounds, valuations, personnel -- all dated,
  // all changing
  "OpenAI",
  "Anthropic",
  "DeepMind",
  "Mistral AI",
  "Hugging Face",
  "Stability AI",
  "Inflection AI",
  "Safe Superintelligence",
  "XAI (company)",
  // 3. Products: versions supersede one another, a natural valid_from/valid_to chain
  "GPT-4",
  "ChatGPT",
  "Claude (language model)",
  "Gemini (language model)",
  "Llama (language model)",
];

function extract(title) {
  const u = new URL("https://en.wikipedia.org/w/api.php");
  u.searchParams.set("action", "query");
  u.searchParams.set("prop", "extracts");
  u.searchParams.set("explaintext", "1");
  u.searchParams.set("redirects", "1");
  u.searchParams.set("format", "json");
  u.searchParams.set("formatversion", "2");
  u.searchParams.set("titles", title);
  const page = JSON.parse(curl(u.toString())).query.pages[0];
  if (page.missing) throw new Error(`${title}: no such article`);
  return { title: page.title, text: page.extract || "" };
}

// The trailing References / External links / See also sections are nothing but links and
// template debris; the extractor treats them as body text and produces a pile of relationless
// isolated nodes. Cut them off.
const CUT = /\n==+ ?(References|External links|See also|Further reading|Notes|Bibliography|Sources) ?==+/i;
const clean = (t) => t.split(CUT)[0].replace(/\n{3,}/g, "\n\n").trim();

const docs = [];
for (const title of TITLES) {
  try {
    const { title: real, text } = extract(title);
    const body = clean(text);
    const slug = real.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "");
    docs.push([`${slug}.txt`, `${real}\n\n${body}\n`]);
    process.stderr.write(`OK  ${real.padEnd(38)} ${String(body.length).padStart(7)} chars\n`);
  } catch (e) {
    process.stderr.write(`ERR ${title}: ${e.message}\n`);
  }
}

const total = docs.reduce((n, [, t]) => n + t.length, 0);
process.stderr.write(`\n${docs.length} docs, ${total.toLocaleString()} chars, about ${Math.round(total / 950)} chunks\n`);

process.stdout.write(JSON.stringify({
  name: "ai-timeline",
  note:
    "AI company milestones. Picked because when the State of the Union corpus was measured " +
    "the time dimension came out almost empty (of 758 facts only 33 carried valid_from, and " +
    "only 2 supersedes) -- political speech asserts, it does not record. Here the sentences " +
    "themselves carry dates, and the same predicate changes value: OpenAI's CEO changed four " +
    "times between 2023-11-17 and 11-22, the most direct material bitemporality has. The " +
    "trailing References/External links sections have been cut: they are link debris and only " +
    "produce isolated nodes. Note: these companies are extremely common in model training " +
    "data, which makes this good for a demo (recognising them is a virtue) and bad as an " +
    "accuracy benchmark (what you measure is recitation).",
  source: "https://en.wikipedia.org/ — MediaWiki action=query&prop=extracts",
  license: "CC BY-SA 4.0 (attribution-sharealike, different from the repository's main licence)",
  docs,
}, null, 1));
