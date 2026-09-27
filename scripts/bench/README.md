# The measuring bench for type resolution

**A fresh database for every run.** This one rule is the entire reason this directory exists.

Before it, three runs in a row tuned retrieval against the same database, and that database
carried the retyping results of the previous runs -- the easy entities had long since been
refined, and the rejection reasons said so outright: `already correctly typed as pharmacy`.
The numbers from the last two runs were simply not comparable with the first, and yet they
were taken as grounds for two code changes. The few minutes saved by reusing a database bought
a whole stretch of worthless conclusions.

## Running a set

```
node scripts/bench/run.mjs --corpus pharma --label seeds-only
node scripts/bench/run.mjs --corpus pharma --ontology /tmp/schemaorg.ttl --label schemaorg
```

Prerequisites: `utopia-server` is up, connected to a writable database, and the workspace has a
chat and an embedding model configured. See the head of `run.mjs` for the environment variables.

## The directory

- `corpora/*.json` -- fixed corpora. **Entities recurring across documents** is deliberate: in a
  single-document corpus each entity has only one or two facts, the profile is barely more than a
  name, and you cannot measure what resolution can really do.
- `truth/*.json` -- which class each entity is expected to land in. The key is a fragment of the
  name that is enough to recognise it (the name extraction hands back varies slightly every time,
  and exact matching would score that variation as a failure); the value is the set of acceptable
  classes, and any one of them counts as correct. **An empty array = the ontology has no matching
  class, and in that case the correct behaviour is to leave it alone**.
- `run.mjs` -- one run: create a database -> load the corpus -> optionally import an ontology ->
  run resolution -> score.
- `fetch-ai-timeline.mjs` -- fetches the **current revision** of an article (`prop=extracts`).
- `fetch-wiki-history.mjs` -- fetches **historical snapshots** (`action=parse&oldid`). This is what
  demonstrating epistemic time relies on: load several snapshots of the same article ordered by
  `doc_time` and the graph will genuinely change its mind.
- `subset-corpus.mjs` -- picks a few articles out of one corpus to make a new one. **Whole articles
  only**, because `supersedes` only happens between adjacent snapshots of the same article, and
  sampling random chunks would destroy the temporal axis entirely.
- `subset.mjs` -- cuts the schema.org TTL down to its first N classes, for the degradation curve.

## How to read the numbers

- `prompt_tokens_est` is an estimate for the **ontology section**, not the whole prompt. Measured
  at 4.0 characters ~= 1 token (377,735<->81,855, 396,716<->99,041). The real token count lives in
  the LLM client, and threading it out would mean changing signatures the whole way up; what we
  want to measure here is "ontology size", and a stable ratio is good enough.
- `for_review` counts as a miss, on the grounds that it **was not changed**. It really has not been
  changed yet -- scoring it as a hit means putting a human's work on the machine's tab.
- `absent` = present in the ground truth, but extraction never pulled the entity out at all. That
  is not resolution's fault, so it gets its own column.

## The ground truth will be wrong sometimes

The very first run had one written too narrowly: `心血管健康论坛` only listed
`business_event|event_series`, while the `conference_event` the system gave was correct.
**When the answer is wrong, fix the answer** -- but only after the results are in, and write down
why, otherwise the ground truth degrades into a record of "what the system happened to answer this
time" and measures nothing at all.

## Adding a corpus

Two files: `corpora/x.json` and `truth/x.json`. The corpora spanning different industries is
deliberate -- only when the same judgement holds up in two domains can you argue it is not just
overfitted to one batch of vocabulary.
