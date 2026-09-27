#!/usr/bin/env node
// Cut the schema.org TTL down to a subset of the first N classes, for the degradation curve.
//
// What the curve has to answer is: **how much vocabulary can be inlined before extraction
// starts dropping things**. That curve sets `ONTOLOGY_PROMPT_BUDGET` and how many candidates
// are retrieved per chunk; without measuring it, those numbers are pulled out of thin air.
//
// Why not "import the whole thing and then cap the inlined count": that measures "how well
// retrieval picks", mixing in the quality of retrieval. A subset plus full inlining is what
// measures the pure scale effect.
//
// Usage: node scripts/bench/subset.mjs /tmp/schemaorg.ttl 100 > /tmp/schemaorg-100.ttl

import fs from "node:fs";

const [, , src, nRaw] = process.argv;
const N = Number(nRaw);
if (!src || !Number.isFinite(N)) {
  console.error("usage: subset.mjs <schemaorg.ttl> <class-count>");
  process.exit(2);
}

const text = fs.readFileSync(src, "utf8");
const lines = text.split("\n");

// The prefix block is kept verbatim: cut it off and the file will not parse
const prefixEnd = lines.findIndex((l) => l.startsWith("@prefix") === false && l.trim() && !l.startsWith("#"));
const prefixes = lines.slice(0, prefixEnd).join("\n");

// Split into blocks on blank lines, but **you have to know whether you are inside a
// triple-quoted string**.
//
// The first two versions both came to grief on this. Ending a block on `/\.\s*$/` does not
// work: the rdfs:comment of schema.org has lines that end in a period, so blocks get split
// apart in the middle of a description (the import reports `Accountancy is not a valid
// subject`). Switching to blank lines does not work either: there are genuine blank lines
// inside `"""…"""` as well, which splits it just the same (`A is not a valid subject`, where
// "A" is the first word of the BreadcrumbList description).
//
// Without lexical context there is no cutting this TTL file up -- count how many times
// `"""` has appeared, and only an even number counts as being outside a string.
const blocks = [];
{
  let cur = [];
  let inLiteral = false;
  for (const line of lines.slice(prefixEnd)) {
    const quotes = (line.match(/"""/g) || []).length;
    if (!inLiteral && quotes % 2 === 0 && !line.trim() && cur.length) {
      blocks.push(cur.join("\n").trim());
      cur = [];
      continue;
    }
    cur.push(line);
    if (quotes % 2 === 1) inLiteral = !inLiteral;
  }
  if (cur.length) blocks.push(cur.join("\n").trim());
}

const subjectOf = (b) => (b.match(/^\s*(\S+)\s+a\s/m) || [])[1] || "";
const isClass = (b) => /\ba\s+rdfs:Class\b/.test(b);
const isProp = (b) => /\ba\s+rdf:Property\b/.test(b);

// Take the first N classes. **Order-preserving rather than a random draw**: the same N gives
// the same subset every time, so a difference between two runs can be attributed elsewhere
const classes = blocks.filter(isClass);
const keep = new Set(classes.slice(0, N).map(subjectOf).filter(Boolean));

// Properties: keep the ones whose domainIncludes lands in a kept class. Keeping properties
// that point at a class which was cut is pointless -- their domain does not resolve, so the
// import would skip them anyway
const props = blocks.filter(isProp).filter((b) => {
  const m = b.match(/schema:domainIncludes([^;.]*)/);
  if (!m) return false;
  return m[1]
    .split(",")
    .map((x) => x.trim().replace(/[.;]$/, ""))
    .some((x) => keep.has(x));
});

const kept = blocks.filter((b) => isClass(b) && keep.has(subjectOf(b)));
process.stdout.write(prefixes + "\n\n" + kept.concat(props).join("\n\n") + "\n");
process.stderr.write(`kept ${kept.length} classes and ${props.length} properties\n`);
