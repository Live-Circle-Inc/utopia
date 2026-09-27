//! Graph extraction job: call the LLM chunk by chunk → entity resolution v2 → write facts and
//! evidence into the ledger.
//! Separate from the ingest pipeline (usable in two stages): once indexing is done it is
//! searchable and answerable, while extraction takes its time.
//! Resolution grey areas only go into the review queue and trigger a separate batched
//! adjudication job -- LLM adjudication never blocks this job.

use crate::llm_util;
use crate::predicate_match::PredicateIndex;
use crate::state::AppState;
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use uuid::Uuid;

/// How many times a rate limit is backed off and retried. **Only applies to 429**: a wrong key
/// is still wrong after ten thousand retries.
const RATE_LIMIT_TRIES: u32 = 5;
/// Cap on a single backoff. Total waiting is therefore capped at a little over two minutes, so
/// an account whose quota is permanently maxed out fails cleanly instead of pinning a worker
/// slot forever.
const RATE_LIMIT_CAP: Duration = Duration::from_secs(60);

/// Jitter for the backoff. **No `rand` dependency**: all that is wanted here is "do not let N
/// chunks wake up at the same moment", and the nanosecond clock spreads them well enough, while
/// one more dependency drags a supply chain along with it.
///
/// A half interval (base/2 to base) rather than the full one: the backoff still grows
/// monotonically, it is only staggered.
fn jitter(base: Duration) -> Duration {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::from(d.subsec_nanos()))
        .unwrap_or(0);
    let half = (base.as_millis() as u64) / 2;
    base / 2 + Duration::from_millis(if half == 0 { 0 } else { nanos % half })
}

/// The chat call for extraction, **with backoff and retry on rate limits**.
///
/// What separates a rate limit from other failures is that it heals by itself, so the old
/// "skip this chunk" line, applied to it, traded a one-minute wait for a permanent hole in the
/// data -- measured on one 1884-chunk ingest, 55 of 60 documents failed in their entirety while
/// the endpoint was fine the whole time.
///
/// Two things are easy to get wrong:
///
/// - **The permit must not be held during backoff.** The permit wraps only the actual call and
///   is handed back before going to sleep, otherwise one waiting chunk blocks another that
///   would have gone through.
/// - **Most vendors do not send `Retry-After`**, so it is only "more precise when present";
///   the criterion is the error type itself, and without it we take exponential backoff.
async fn chat_retrying_rate_limits(
    state: &AppState,
    settings: &utopia_core::models::LlmSettings,
    client: &utopia_llm::LlmClient,
    messages: &[utopia_llm::ChatMessage],
) -> anyhow::Result<String> {
    let mut backoff = Duration::from_secs(2);
    for attempt in 1..=RATE_LIMIT_TRIES {
        // The permit wraps only the call itself and is handed back on leaving this block
        let outcome = {
            let _permit = llm_util::acquire_chat(state, settings).await;
            client.chat(messages).await
        };
        let err = match outcome {
            Ok(reply) => return Ok(reply),
            Err(e) => e,
        };
        let Some(hit) = utopia_llm::rate_limited(&err) else {
            return Err(err);
        };
        if attempt == RATE_LIMIT_TRIES {
            return Err(err.context(format!("still throttled after {RATE_LIMIT_TRIES} backoffs")));
        }
        let delay = jitter(hit.retry_after.unwrap_or(backoff).min(RATE_LIMIT_CAP));
        tracing::warn!(
            attempt,
            delay_ms = delay.as_millis() as u64,
            from_header = hit.retry_after.is_some(),
            "endpoint rate limited, retrying after backoff"
        );
        tokio::time::sleep(delay).await;
        backoff = (backoff * 2).min(RATE_LIMIT_CAP);
    }
    unreachable!("the loop always returns")
}

const MIN_CONFIDENCE: f32 = 0.6;

/// Whether this string **is the name of a thing**.
///
/// The criterion is **word count**, not character count. Character count cannot tell the real
/// from the fake: `US District Court for the Northern District of California` (57 characters)
/// is a real entity, while `removal was driven by growing discontent and distrust with Altman`
/// (65 characters) is an entire clause -- the character counts are close, the word counts are
/// close too (9 vs 10), but the latter carries a **finite verb**.
///
/// So the two rules work together: the word cap stops long sentences, while a **finite verb in
/// the string** stops the clauses that are not long. Institution names get long
/// ("US District Court for the Northern District of California"), but they never contain
/// predicates like "was driven by", "showed", "giving off".
///
/// The cap is 12 words: measured, the longest institution name among the real entities was
/// 9 words, leaving three words of headroom. The ones that got blocked averaged 14 words.
const MAX_NAME_WORDS: usize = 12;

/// Markers of a predicate in a sentence. **Finite forms only** -- participles like `used` and
/// `flying` are perfectly normal inside a noun phrase ("equipment used by X"), and listing them
/// would hit real entities.
const CLAUSE_MARKERS: &[&str] = &[
    "was", "were", "is", "are", "has", "have", "had", "will", "would", "showed", "said", "says",
    "became", "went", "came", "did", "does", "gave", "took", "made",
];

fn is_entity_name(name: &str) -> bool {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 100 {
        return false;
    }
    let words: Vec<String> = name
        .split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .collect();
    if words.len() > MAX_NAME_WORDS {
        return false;
    }
    // A one-word name cannot be a clause; do not let proper names like "Is" get hit
    if words.len() > 2 && words.iter().any(|w| CLAUSE_MARKERS.contains(&w.as_str())) {
        return false;
    }
    // Partitive: `745 of OpenAI's 770 employees` is a quantity description, not a thing.
    // It has no finite verb and is not many words, so neither rule above catches it.
    //
    // The criterion is kept very narrow -- **the first word is all digits and the second is of**.
    // Real entities that start with a number (`3M`, `7-Eleven`, `23andMe`) do not have an
    // all-digit first word; `2023 Nobel Prize` does, but its second word is not `of`. One notch
    // wider and they get hit.
    if words.len() >= 3
        && words[1] == "of"
        && !words[0].is_empty()
        && words[0].chars().all(|c| c.is_ascii_digit())
    {
        return false;
    }
    true
}

/// Record a drop signal. The extractor has seven `continue` sites, and every one of them is
/// "a fact was extracted, got blocked, and nothing was said about it". Failing to write the
/// signal must never take down extraction of the whole document, so errors are swallowed here.
async fn drop_signal(
    state: &AppState,
    kb_id: Uuid,
    document_id: Uuid,
    reason: &str,
    detail: &str,
    example: Option<&str>,
) {
    let _ = utopia_store::extraction_drops::record(
        &state.pool,
        kb_id,
        document_id,
        reason,
        detail,
        example,
    )
    .await;
}

/// Whether this round finished extracting -- and when it did not, **the sentence that goes
/// into graph_error**.
///
/// The criterion is "every chunk done" rather than some percentage: any percentage is made up,
/// while there is already a criterion here that needs no making up -- it counts as done only
/// when every single chunk succeeded.
///
/// `attempted` is **the number of chunks picked up this round**, not the document's total: a
/// retry only picks up chunks with `extracted_at IS NULL`, so the denominator on the second
/// round is naturally smaller. The wording says "this round" so the reader does not come away
/// thinking the document only has that many chunks.
fn incomplete_reason(unextracted: &[(i32, String)], attempted: usize) -> Option<String> {
    if unextracted.is_empty() {
        return None;
    }
    // Only the first three are listed: the reason is usually the same one (if the vendor is
    // down then every chunk is down), and listing them all just copies one sentence twenty times
    let sample: Vec<String> = unextracted
        .iter()
        .take(3)
        .map(|(seq, why)| format!("#{seq} {why}"))
        .collect();
    let more = unextracted.len().saturating_sub(sample.len());
    let tail = if more > 0 {
        format!("; {more} more")
    } else {
        String::new()
    };
    Some(format!(
        "{} of this round's {attempted} chunks could not be extracted: {}{tail}",
        unextracted.len(),
        sample.join("; ")
    ))
}

/// The only enqueue point for auto-extending the ontology. **Both the success and the failure
/// path have to reach it.**
///
/// The switch is re-read here instead of reusing the caller's copy: the failure path never
/// loaded the kb at all, and the success path's copy was read when the document **started**
/// extracting -- a 73-chunk document runs for well over an hour, and if someone turned the
/// switch off in settings during that time, reusing the old value acts on an hour-old intent.
async fn enqueue_bootstrap(state: &AppState, kb_id: Uuid) -> anyhow::Result<()> {
    if !utopia_store::kbs::get(&state.pool, kb_id)
        .await?
        .auto_extend_ontology
    {
        return Ok(());
    }
    if !utopia_store::documents::extraction_idle(&state.pool, kb_id).await? {
        return Ok(());
    }
    utopia_store::jobs::enqueue(
        &state.pool,
        "bootstrap_ontology",
        serde_json::json!({ "kb_id": kb_id }),
    )
    .await?;
    Ok(())
}

/// `proposed_by`: if this document is a memory log, the extracted facts wait for a human nod
/// and this column records "who said it". Bulk-ingested documents pass None -- that path does
/// not go through the pending queue, so the value is unused
pub async fn extract_document(
    state: &AppState,
    document_id: Uuid,
    proposed_by: Option<Uuid>,
) -> anyhow::Result<()> {
    match run(state, document_id, proposed_by).await {
        Ok(()) => Ok(()),
        Err(e) => {
            // The reason is stored with the status: an error only in the log is no error at all
            let _ = utopia_store::documents::set_graph_failed(
                &state.pool,
                document_id,
                &format!("{e:#}"),
            )
            .await;
            if let Ok(doc) = utopia_store::documents::get(&state.pool, document_id).await {
                state.emit_document(doc.kb_id, document_id);
                // **Failure has to trigger the ontology auto-extend too.**
                //
                // The enqueue used to live only on the success path, so this sequence would
                // wedge a knowledge base forever: the first 14 documents succeed (each one
                // sees others still in flight, so it does not trigger), the 15th exhausts its
                // retries and turns failed -- at which point extraction_idle happens to be
                // true (failed counts as neither queued nor extracting), yet **no document
                // will ever complete again to run this check**. The result is proposals piling
                // up in the pool, the ontology frozen forever on its few seed relations, half
                // the graph forever on the fallback predicate, and nothing in the UI saying
                // this ever happened.
                //
                // The job itself is idempotent and re-checks the switch and the threshold, so
                // enqueueing once more here is safe.
                let _ = enqueue_bootstrap(state, doc.kb_id).await;
            }
            Err(e)
        }
    }
}

/// mention → entity id. Within one document the same name and type is reused directly (in a
/// single-document context one name is rarely two different people, and it also thins the
/// resolution calls down to once per name); cross-document ambiguity is handled by
/// resolve_mention's profile comparison.
async fn resolve(
    state: &AppState,
    kb_id: Uuid,
    // None = the model's type is not in the ontology, or this kb has no classes yet (0009)
    type_id: Option<Uuid>,
    name: &str,
    ctx: Option<&[f32]>,
    doc_cache: &mut HashMap<(Option<Uuid>, String), Uuid>,
    needs_adjudication: &mut bool,
) -> anyhow::Result<Uuid> {
    let key = (
        type_id,
        utopia_store::resolution::normalize_name(name).to_lowercase(),
    );
    if let Some(id) = doc_cache.get(&key) {
        return Ok(*id);
    }
    let r =
        utopia_store::resolution::resolve_mention(&state.pool, kb_id, type_id, name, ctx).await?;
    // Suspected duplicate pairs (same-name grey area / type drift) go into the review queue;
    // the batched adjudication job is triggered once at the finish
    for review in &r.reviews {
        utopia_store::resolution::create_review(
            &state.pool,
            kb_id,
            r.entity_id,
            review.other_id,
            review.score,
            &review.reason,
        )
        .await?;
        *needs_adjudication = true;
    }
    doc_cache.insert(key, r.entity_id);
    Ok(r.entity_id)
}

async fn run(state: &AppState, document_id: Uuid, proposed_by: Option<Uuid>) -> anyhow::Result<()> {
    let doc = utopia_store::documents::get(&state.pool, document_id).await?;
    let kb = utopia_store::kbs::get(&state.pool, doc.kb_id).await?;
    let settings = utopia_store::settings::get(&state.pool, kb.workspace_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("Chat model not configured; cannot extract"))?;
    let client = llm_util::chat_client(&settings)
        .ok_or_else(|| anyhow::anyhow!("Chat model not configured; cannot extract"))?;

    // Ownership token: a re-extraction bumps the epoch, and this job uses it to notice it has
    // been taken over (see the chunk loop)
    let my_epoch = utopia_store::documents::extract_epoch(&state.pool, document_id).await?;
    utopia_store::documents::set_graph_status(&state.pool, document_id, "extracting").await?;
    state.emit_document(doc.kb_id, document_id);
    let etypes = utopia_store::graph::entity_types(&state.pool, doc.kb_id).await?;
    // Facts landed this round (newly created or observed again): the signature check runs over
    // them at the finish
    let mut touched_facts: Vec<Uuid> = Vec::new();
    let rtypes = utopia_store::graph::relation_types(&state.pool, doc.kb_id).await?;
    // Relations and attributes part ways here: attributes take the literal-value channel and
    // never enter the relation list.
    //
    // **When the ontology has no matching relation there is no predicate** (see
    // `facts.predicate_id`). The original wording lands in
    // fact_evidence.proposed_predicate and is fetched back by fact_surface_predicate() for
    // display.
    //
    // This used to be a fallback relation called related_to, deliberately not listed to the
    // model -- put it in the prompt and it becomes an escape hatch: reading a relation it
    // cannot pin down, the model stops writing the original wording and just picks the
    // catch-all option. Now the row itself is gone, and the escape hatch and the "remember not
    // to list it" disappeared together.
    //
    // **These two things have to be done together; doing one of them is worse than doing
    // neither.** Deleting the row in the database is not enough, the seed in
    // `DEFAULT_RELATION_TYPES` has to go too -- and the exclusion filter here had already been
    // deleted along with it. So `ensure_default_ontology` seeded the row back seven minutes
    // later, and for the first time `related_to` was **listed in the prompt for the model to
    // see**. 0001 measured it: of 359 uses, 321 were the model picking it off the list.
    // Anyone wanting to add a fallback relation back into the seed table should read 0010
    // first -- though that table is gone now (`#128`), retired along with the seeding
    // function.
    let type_key_by_id: HashMap<Uuid, &str> =
        etypes.iter().map(|t| (t.id, t.key.as_str())).collect();
    let attr_meta: HashMap<&str, &utopia_core::models::RelationType> = rtypes
        .iter()
        .filter(|r| r.kind == "attribute")
        .map(|r| (r.key.as_str(), r))
        .collect();
    let type_ids: HashMap<&str, Uuid> = etypes.iter().map(|t| (t.key.as_str(), t.id)).collect();
    let rel_ids: HashMap<&str, Uuid> = rtypes.iter().map(|r| (r.key.as_str(), r.id)).collect();
    // Predicates the model says are landed onto relations the ontology already has: spelling,
    // tense and passive voice all get aligned (see predicate_match). Without it, `produces` was
    // right there in the vocabulary, yet the model writing `produced_by` got downgraded and
    // thrown away
    let pred_index = PredicateIndex::build(&rtypes);
    // "Does the ontology know this wording" -- the literal-value branch and the relation branch
    // have to use one and the same criterion. Written separately, a predicate that fuzzy-matches
    // would first be diverted as an attribute by the literal branch, and the same word would get
    // opposite answers on the two paths
    let known_predicate = |p: &str| rel_ids.contains_key(p) || pred_index.lookup(p).is_some();
    // Temporal reconciliation only applies to state relations with a uniqueness constraint
    // (ontology metadata): (functional, inverse_functional, temporal)
    let rel_meta: HashMap<Uuid, (bool, bool, String)> = rtypes
        .iter()
        .map(|r| {
            (
                r.id,
                (r.functional, r.inverse_functional, r.temporal.clone()),
            )
        })
        .collect();
    let type_parents: HashMap<Uuid, &[Uuid]> = etypes
        .iter()
        .map(|t| (t.id, t.parents.as_slice()))
        .collect();

    // **Lay out the whole ontology, or retrieve per chunk.**
    //
    // Laying it all out is today's behaviour, and with a small ontology it is both correct and
    // cheap: 40 classes is about 2k characters, and retrieval would just be a pointless round
    // trip. With a large ontology it is a disaster -- schema.org measured 108k tokens per chunk,
    // and on the same corpus giving only the seed classes extracted 25 entities while giving
    // the full set left only 18. **Those 959 extra classes ate 7 entities.**
    //
    // So it switches on a budget: lay it all out if it fits, retrieve per chunk if it does not.
    // The criterion measures **the actual text that will be laid out** (build_lists counts it
    // itself) rather than a separate estimation formula -- a formula would drift from the layout.
    let full = build_lists(&etypes, &rtypes, None, None);
    let budget = utopia_store::access::ontology_prompt_budget(&state.pool).await?;
    let retrieve_per_chunk = full.chars() > budget;
    if retrieve_per_chunk {
        tracing::info!(
            %document_id, chars = full.chars(), budget,
            classes = etypes.len(),
            "ontology exceeds the prompt budget, switching to per-chunk candidate retrieval"
        );
    }
    // Builtin classes are always present: a chunk that retrieval misses still needs somewhere
    // to land, otherwise the model has no class to pick
    let seed_classes: HashSet<Uuid> = etypes.iter().filter(|t| t.builtin).map(|t| t.id).collect();

    // An attribute's domain admits subclasses: it is enough for the subject type to hit the
    // domain while walking up the parent chain.
    // Walk up subClassOf. **Breadth first plus a visited set**, not a single-chain loop:
    // a class can have several parents (FOAF's Person is both Agent and SpatialThing),
    // and diamond inheritance reaches the same ancestor by two routes, which without a visited
    // set gets expanded twice.
    //
    // The depth cap was replaced by the visited set: set_parents on the write side already
    // checks for cycles, and a "walk at most ten levels" backstop here neither stops a wide
    // graph nor stops quietly letting deep hierarchies through.
    let type_matches_domain = |ty: Uuid, domain: Uuid| -> bool {
        let mut seen: HashSet<Uuid> = HashSet::new();
        let mut queue = vec![ty];
        while let Some(cur) = queue.pop() {
            if cur == domain {
                return true;
            }
            if !seen.insert(cur) {
                continue;
            }
            if let Some(ps) = type_parents.get(&cur) {
                queue.extend(ps.iter().copied());
            }
        }
        false
    };
    // This round retells this document's story from the start, so old signals are cleared
    // first (a re-extraction counts automatically)
    let _ = utopia_store::extraction_drops::clear_for_document(&state.pool, document_id).await;

    // **Facts extracted from memory wait for a human nod first** (0015). A remember is one
    // sentence at a time with the human right there in the conversation, so the cheapest moment
    // to confirm is the moment they finish saying it; bulk ingest lands tens of thousands at
    // once, where confirming them one by one is impossible, so that path still writes
    // optimistically and reviews afterwards. There is exactly one criterion: is this a memory
    // log. Entities are resolved and created as usual -- `pending_facts.subject_id` is a foreign
    // key, a trade-off settled in 0018
    let await_nod = utopia_store::memory::is_memory_document(&state.pool, document_id).await?;
    let mut pending_count = 0usize;

    let doc_time = doc.doc_time.map(|t| t.format("%Y-%m-%d").to_string());
    let chunks = utopia_store::documents::chunks_for_extraction(&state.pool, document_id).await?;

    let mut doc_cache: HashMap<(Option<Uuid>, String), Uuid> = HashMap::new();
    // Entities this document has already committed to, ordered by first appearance, fed into
    // the prompt of the chunks that follow.
    //
    // **Deduplicated by entity_id, not by name**: if chunk 3 writes "上海研究院" ("Shanghai
    // Research Institute") and it resolves to chunk 1's "星云科技上海研究院" ("Nebula Tech
    // Shanghai Research Institute"), it must not enter the list under that second name -- every
    // entity in the list has exactly one display form, the one this document used first. In
    // Chinese the full name comes first, so that is also the more complete one.
    let mut doc_entities: Vec<(Uuid, String, String)> = Vec::new();
    // Chunks that failed wholesale: (seq, reason). Used at the finish to refuse marking this
    // document done
    let mut unextracted: Vec<(i32, String)> = Vec::new();
    let mut needs_adjudication = false;
    let mut conflicts_found = false;
    let mut fact_count = 0usize;
    // No cap on chunks: silent truncation means losing knowledge, and the cost of a long
    // document is for the operator to weigh themselves (cost optimisation goes through prompt
    // prefix caching and chunk-level skipping on update, not through dropping data)
    for chunk in chunks.iter() {
        // If taken over, leave quietly: do not write failed, do not touch the status, leave
        // the stage to the new job. The check sits before the LLM call -- cancellation
        // granularity is one chunk, there is no need to wait for the whole document to finish
        if utopia_store::documents::extract_epoch(&state.pool, document_id).await? != my_epoch {
            tracing::info!(%document_id, "extraction taken over by a newer round, exiting");
            return Ok(());
        }
        let ctx: Option<&[f32]> = chunk.embedding.as_ref().map(|v| v.as_slice());
        // If the ontology fits, use the full listing; if it does not, retrieve candidates with
        // **this chunk's own vector**. The vector is already at hand -- entity resolution is
        // using it anyway (that ctx above), so retrieval costs not one extra embedding call. If
        // retrieval comes up empty (no embedding model configured, or this chunk has no vector)
        // it falls back to the full listing: a big prompt is slow, having no class to pick means
        // nothing gets extracted at all
        let lists = if retrieve_per_chunk {
            match ctx {
                Some(v) => chunk_lists(state, doc.kb_id, v, &etypes, &rtypes, &seed_classes)
                    .await
                    .unwrap_or(None),
                None => None,
            }
        } else {
            None
        };
        let lists = lists.as_ref().unwrap_or(&full);
        let known: Vec<(String, String)> = doc_entities
            .iter()
            .map(|(_, k, n)| (k.clone(), n.clone()))
            .collect();
        let messages = utopia_extract::build_messages(
            &lists.types,
            &lists.relations,
            &lists.attributes,
            doc_time.as_deref(),
            &doc.filename,
            &known,
            &chunk.text,
        );
        // These two `continue`s skip **the whole chunk** -- it produced not a single fact.
        // Record it, and use it at the finish to decide whether this document counts as fully
        // extracted (see after the loop)
        let reply = match chat_retrying_rate_limits(state, &settings, &client, &messages).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(%document_id, seq = chunk.seq, error = %e, "extraction call failed, skipping this chunk");
                unextracted.push((chunk.seq, format!("call failed: {e}")));
                continue;
            }
        };
        let extraction = match utopia_extract::parse_response(&reply) {
            Ok(x) => x,
            Err(e) => {
                tracing::warn!(%document_id, seq = chunk.seq, error = %e, "parsing the extraction result failed, skipping this chunk");
                unextracted.push((chunk.seq, format!("parsing the result failed: {e}")));
                continue;
            }
        };
        // **What got skipped has to be said out loud.** Per-item parsing rescued the chunk as
        // a whole, but if the few items that were skipped leave no signal, that is just another
        // flavour of "partial extraction reported as complete" (#108 fixed it once already)
        if extraction.truncated {
            drop_signal(
                state,
                doc.kb_id,
                document_id,
                utopia_store::extraction_drops::reason::TRUNCATED_REPLY,
                &format!("output for chunk #{} was truncated", chunk.seq),
                None,
            )
            .await;
        }
        let skipped = extraction.skipped_entities + extraction.skipped_facts;
        if skipped > 0 {
            tracing::warn!(
                %document_id,
                seq = chunk.seq,
                entities = extraction.skipped_entities,
                facts = extraction.skipped_facts,
                "skipped items with a malformed shape"
            );
            drop_signal(
                state,
                doc.kb_id,
                document_id,
                utopia_store::extraction_drops::reason::MALFORMED_ITEM,
                &format!(
                    "chunk #{} skipped {} entities / {} facts",
                    chunk.seq, extraction.skipped_entities, extraction.skipped_facts
                ),
                None,
            )
            .await;
        }

        // Entity resolution: name → entity id (facts in this chunk are wired by original name)
        let mut entity_ids: HashMap<String, Uuid> = HashMap::new();
        // name → declared type (for attribute domain checks: salary cannot hang off Organization)
        let mut entity_type_of: HashMap<String, Option<Uuid>> = HashMap::new();
        for e in &extraction.entities {
            let name = e.name.trim();
            if !is_entity_name(name) {
                // This used to be a silent `continue` -- exactly the "extracted, blocked, and
                // nothing said about it" that `drop_signal` was built for in the first place
                drop_signal(
                    state,
                    doc.kb_id,
                    document_id,
                    utopia_store::extraction_drops::reason::NOT_AN_ENTITY_NAME,
                    &e.type_key,
                    Some(name),
                )
                .await;
                continue;
            }
            // When downgrading, remember the word the model proposed: the ontology having no
            // room for it does not mean the model said it wrong. Keep only a count and later,
            // when you want to add a model class, those 43 entities cannot be found -- they are
            // mixed in among concept, and the only way out is re-extracting the whole kb
            let mut proposed: Option<&str> = None;
            let type_id = match type_ids.get(e.type_key.as_str()) {
                Some(id) => Some(*id),
                None => {
                    // A type outside the allowlist: **leave it empty** and count it as a miss
                    // (the signal for extending the ontology).
                    //
                    // This used to downgrade to that concept sentinel row. Now "not judged
                    // yet" simply is `type_id IS NULL` (0009) -- the entity is created, the
                    // fact lands, the evidence is there as usual, it just has no type for the
                    // moment. Install a pack later and run type resolution, and it gets
                    // reassigned
                    let _ = utopia_store::ontology::record_miss(
                        &state.pool,
                        doc.kb_id,
                        "entity_type",
                        &e.type_key,
                        Some(name),
                    )
                    .await;
                    proposed = Some(e.type_key.as_str());
                    None
                }
            };
            let id = resolve(
                state,
                doc.kb_id,
                type_id,
                name,
                ctx,
                &mut doc_cache,
                &mut needs_adjudication,
            )
            .await?;
            if let Some(p) = proposed {
                let _ = utopia_store::resolution::set_proposed_type(&state.pool, id, p).await;
            }
            // The model's own wording. **Stored separately from proposed_type**: that column
            // means "not in the ontology", and the growth loop sets its threshold on how rare
            // it is; this column is present on every entity. Nothing is recorded when it
            // matches the coarse class name -- that is not a more specific wording, it is just
            // the list copied back
            if let Some(st) = e
                .specific_type
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .filter(|s| !s.eq_ignore_ascii_case(&e.type_key))
            {
                let _ = utopia_store::resolution::set_specific_type(&state.pool, id, st).await;
            }
            // Only entities whose type the model declared itself are recorded: the
            // subject/object fallback path has no type to go on, and putting a guessed type
            // into the list means the chunks that follow copy the guess.
            //
            // When the ontology has no room for that class, the model's own wording (proposed)
            // is used: this list exists so later text can recognise the same people, and "do
            // not write one name as two entities" is its whole job. This used to be hardcoded
            // to concept, which instead flattened several different words into one label
            if !doc_entities.iter().any(|(eid, _, _)| *eid == id) {
                let tk = type_id
                    .and_then(|t| type_key_by_id.get(&t).copied())
                    .or(proposed)
                    .unwrap_or("?");
                doc_entities.push((id, tk.to_string(), name.to_string()));
            }
            entity_ids.insert(name.to_string(), id);
            entity_type_of.insert(name.to_string(), type_id);
        }

        for f in &extraction.facts {
            let confidence = f.confidence.unwrap_or(0.7).clamp(0.0, 1.0);
            if confidence < MIN_CONFIDENCE {
                // A deliberate threshold, but the user still has no way to know "it was
                // extracted, just not confidently enough"
                drop_signal(
                    state,
                    doc.kb_id,
                    document_id,
                    utopia_store::extraction_drops::reason::LOW_CONFIDENCE,
                    &f.predicate,
                    Some(&format!("{} ({:.0}%)", f.subject, confidence * 100.0)),
                )
                .await;
                continue;
            }
            let from = f.valid_from.as_deref().and_then(utopia_extract::parse_time);
            let to = f.valid_to.as_deref().and_then(utopia_extract::parse_time);
            // **Each end records its own precision** (see `facts.valid_to_precision`). One
            // precision column used to describe both endpoints, so "started in 2020, ended
            // 2023-05-06" had to make the two share a single value.
            //
            // A valid_to = "unknown" from the model means **the text said it ended but not on
            // which day**. parse_time cannot parse it (it is not a date to begin with), so it
            // is recognised explicitly here -- otherwise it degrades to None and that fact
            // turns back into "still ongoing"
            let ended_unknown = f
                .valid_to
                .as_deref()
                .map(str::trim)
                .is_some_and(|v| v.eq_ignore_ascii_case(utopia_store::graph::ENDED_UNKNOWN));
            let validity = utopia_store::graph::Validity {
                from: from.map(|(t, _)| t),
                from_precision: from.map(|(_, p)| p),
                to: to.map(|(t, _)| t),
                to_precision: to
                    .map(|(_, p)| p)
                    .or(ended_unknown.then_some(utopia_store::graph::ENDED_UNKNOWN)),
            };

            // Attribute facts: the predicate hits an attribute → the literal-value channel. A
            // failed datatype check prefers nothing over dirty data; the domain check (walking
            // up subclasses) stops mix-ups like "hanging salary off Organization". The model
            // occasionally copies the fully qualified "person.salary" from the list -- strip the
            // class prefix and look it up once more
            let attr_hit = attr_meta.get(f.predicate.as_str()).or_else(|| {
                f.predicate
                    .rsplit_once('.')
                    .and_then(|(_, k)| attr_meta.get(k))
            });
            if let Some(attr) = attr_hit {
                let subject_name = f.subject.trim();
                // The subject was not declared in entities: the type is unknown, the domain
                // cannot be checked, so the attribute does not land. The relation path falls
                // back to resolving as concept on the same omission -- that cannot be copied
                // here, since resolving as concept fails the domain check all the same, it
                // would only fall from here into the branch below
                let Some((&subject_id, &subject_type)) = entity_ids
                    .get(subject_name)
                    .zip(entity_type_of.get(subject_name))
                else {
                    drop_signal(
                        state,
                        doc.kb_id,
                        document_id,
                        utopia_store::extraction_drops::reason::SUBJECT_NOT_DECLARED,
                        &attr.key,
                        Some(subject_name),
                    )
                    .await;
                    continue;
                };
                // Unreachable: the store layer requires every attribute to have a domain and
                // the domain is immutable, and an attribute without one never even makes it
                // into the prompt. Kept as a defence, needs no signal
                if attr.domains.is_empty() {
                    continue;
                }
                // **Any one domain matching is enough**: when an attribute hangs off
                // several classes, the subject belonging to one of them counts
                if !attr
                    .domains
                    .iter()
                    .any(|d| subject_type.is_some_and(|t| type_matches_domain(t, *d)))
                {
                    let subj_key = subject_type
                        .and_then(|t| type_key_by_id.get(&t).copied())
                        .unwrap_or("?");
                    let dom_key = attr
                        .domains
                        .iter()
                        .filter_map(|d| type_key_by_id.get(d).copied())
                        .collect::<Vec<_>>()
                        .join("|");
                    drop_signal(
                        state,
                        doc.kb_id,
                        document_id,
                        utopia_store::extraction_drops::reason::ATTR_DOMAIN_MISMATCH,
                        &format!("{}@{subj_key} (wants {dom_key})", attr.key),
                        Some(subject_name),
                    )
                    .await;
                    continue;
                }
                let raw = match (&f.value, &f.object) {
                    (Some(v), _) => v.clone(),
                    // The model occasionally puts the value in object: catch it leniently
                    (None, Some(o)) if !o.trim().is_empty() => {
                        serde_json::Value::String(o.trim().to_string())
                    }
                    _ => {
                        drop_signal(
                            state,
                            doc.kb_id,
                            document_id,
                            utopia_store::extraction_drops::reason::ATTR_NO_VALUE,
                            &attr.key,
                            Some(subject_name),
                        )
                        .await;
                        continue;
                    }
                };
                let datatype = attr.datatype.as_deref().unwrap_or("text");
                let Some(normalized) = utopia_extract::normalize_attr_value(datatype, &raw) else {
                    tracing::debug!(%document_id, attr = attr.key, ?raw, "attribute value does not fit the datatype, skipping");
                    drop_signal(
                        state,
                        doc.kb_id,
                        document_id,
                        utopia_store::extraction_drops::reason::ATTR_DATATYPE,
                        &format!("{} ({datatype})", attr.key),
                        Some(&format!("{subject_name} → {raw}")),
                    )
                    .await;
                    continue;
                };
                let mut object_value = serde_json::json!({ "value": normalized });
                if let Some(u) = attr.unit.as_deref().filter(|u| !u.is_empty()) {
                    // The unit is written down with the fact: if the unit on the type
                    // changes later, old values are still read in the unit they were recorded in
                    object_value["unit"] = serde_json::json!(u);
                }
                if await_nod {
                    if let utopia_store::pending::Outcome::Proposed(_) =
                        utopia_store::pending::propose(
                            &state.pool,
                            utopia_store::pending::Proposal {
                                kb_id: doc.kb_id,
                                subject_id,
                                predicate_id: Some(attr.id),
                                object_id: None,
                                object_value: Some(&object_value),
                                proposed_predicate: Some(f.predicate.as_str()),
                                validity,
                                confidence,
                                chunk_id: chunk.id,
                                proposed_by,
                            },
                        )
                        .await?
                    {
                        pending_count += 1;
                    }
                    continue;
                }
                let (fact_id, created) = utopia_store::graph::insert_value_fact(
                    &state.pool,
                    doc.kb_id,
                    subject_id,
                    Some(attr.id),
                    &object_value,
                    validity,
                    confidence,
                )
                .await?;
                touched_facts.push(fact_id);
                // Attribute predicates keep the original wording too: the model occasionally
                // copies the fully qualified "person.salary", the hit is on the key with the
                // prefix stripped, and what it literally said is worth keeping
                utopia_store::graph::add_evidence(
                    &state.pool,
                    fact_id,
                    chunk.id,
                    f.quote.as_deref(),
                    Some(f.predicate.as_str()),
                )
                .await?;
                if !created {
                    continue;
                }
                fact_count += 1;
                // A single-valued attribute = functional: a new value closes the old one
                // (this is where attribute history comes from)
                if attr.functional && attr.temporal == "state" {
                    let report = utopia_store::temporal::reconcile_new_fact(
                        &state.pool,
                        doc.kb_id,
                        fact_id,
                        subject_id,
                        attr.id,
                        None,
                        Some(&object_value),
                        utopia_store::temporal::Uniqueness::SubjectSide,
                        validity,
                        confidence,
                    )
                    .await?;
                    if report.conflicts > 0 {
                        conflicts_found = true;
                    }
                }
                continue;
            }

            // **A literal outside the vocabulary: neither dropped, nor invented into an
            // entity.**
            //
            // Reaching here means the predicate is neither a known attribute nor has the
            // relation table been consulted yet. When it carries a literal there are two ways
            // it can go, and both used to be bad:
            //   value set and object empty → falls into "object is required" below, and the
            //     whole item vanishes silently;
            //   a literal stuffed into object → resolved as concept, conjuring an entity called
            //     "2015" out of thin air, one fake node more in the graph, and fixing it
            //     afterwards means changing the shape of the fact.
            // Now both are stored as object_value with no predicate: the value is in the graph,
            // with evidence and with validity, the original wording goes into
            // proposed_predicate, and the resolution pass only has to swap the predicate -- the
            // shape is already right.
            //
            // Whether the thing in object counts as a literal is judged strictly: **the model
            // did not declare it as an entity**, and **it parses as a number or a date on its
            // own**. "杭州" (a city name) satisfies neither, "2015" satisfies both. Attributes
            // with text values (323 of them in schema.org) still turn into entities in this
            // branch -- there is no reliable criterion there, and guessing wrong eats real
            // entities, so we do not guess
            let literal = match (&f.value, f.object.as_deref().map(str::trim)) {
                (Some(v), None | Some("")) if !known_predicate(f.predicate.as_str()) => {
                    Some(v.clone())
                }
                (_, Some(o))
                    if !o.is_empty()
                        && !known_predicate(f.predicate.as_str())
                        && !entity_ids.contains_key(o)
                        && looks_literal(o) =>
                {
                    Some(serde_json::Value::String(o.to_string()))
                }
                _ => None,
            };
            if let Some(value) = literal {
                let subject_name = f.subject.trim();
                let Some(&subject_id) = entity_ids.get(subject_name) else {
                    drop_signal(
                        state,
                        doc.kb_id,
                        document_id,
                        utopia_store::extraction_drops::reason::SUBJECT_NOT_DECLARED,
                        &f.predicate,
                        Some(subject_name),
                    )
                    .await;
                    continue;
                };
                let _ = utopia_store::ontology::record_miss(
                    &state.pool,
                    doc.kb_id,
                    "attribute_type",
                    &f.predicate,
                    Some(&format!("{subject_name} → {value}")),
                )
                .await;
                let literal = serde_json::json!({ "value": value });
                if await_nod {
                    if let utopia_store::pending::Outcome::Proposed(_) =
                        utopia_store::pending::propose(
                            &state.pool,
                            utopia_store::pending::Proposal {
                                kb_id: doc.kb_id,
                                subject_id,
                                predicate_id: None,
                                object_id: None,
                                object_value: Some(&literal),
                                proposed_predicate: Some(f.predicate.as_str()),
                                validity,
                                confidence,
                                chunk_id: chunk.id,
                                proposed_by,
                            },
                        )
                        .await?
                    {
                        pending_count += 1;
                    }
                    continue;
                }
                let (fact_id, created) = utopia_store::graph::insert_value_fact(
                    &state.pool,
                    doc.kb_id,
                    subject_id,
                    None,
                    &literal,
                    validity,
                    confidence,
                )
                .await?;
                touched_facts.push(fact_id);
                utopia_store::graph::add_evidence(
                    &state.pool,
                    fact_id,
                    chunk.id,
                    f.quote.as_deref(),
                    Some(f.predicate.as_str()),
                )
                .await?;
                if created {
                    fact_count += 1;
                }
                continue;
            }

            // Relation facts: the object is required
            let Some(object_name) = f.object.as_deref().map(str::trim).filter(|s| !s.is_empty())
            else {
                drop_signal(
                    state,
                    doc.kb_id,
                    document_id,
                    utopia_store::extraction_drops::reason::OBJECT_MISSING,
                    &f.predicate,
                    Some(f.subject.trim()),
                )
                .await;
                continue;
            };
            // **Undeclared subjects and objects have to pass the same criterion.**
            //
            // The guard used to sit only on the declared-entity path above, and this path went
            // around it: the model writes a whole sentence into `object`, that sentence never
            // shows up in entities, and this code turns around and creates it as an entity.
            // Measured (ai-timeline-ends × schema.org): 76 of 421 entities had no type, the
            // longest of them 111 characters -- "thermal-imaging equipment used by volunteers
            // flying over the site showed at least 33 generators giving off heat", which is an
            // entire clause, not a thing. **The guard held the front door, the back door stood
            // open.**
            //
            // The harm of these entities does not stop there: they never match any mention
            // anywhere else, they are isolated points in the graph (59 of them, measured), and
            // they drag resolution down as well -- every one has to be compared against the
            // existing entities.
            if !is_entity_name(f.subject.trim()) || !is_entity_name(object_name) {
                drop_signal(
                    state,
                    doc.kb_id,
                    document_id,
                    utopia_store::extraction_drops::reason::NOT_AN_ENTITY_NAME,
                    &f.predicate,
                    Some(if is_entity_name(f.subject.trim()) {
                        object_name
                    } else {
                        f.subject.trim()
                    }),
                )
                .await;
                continue;
            }

            // When the subject or object was not declared in entities, create it first (the
            // model occasionally omits them). Without that entities record there is no type to
            // go on, so leaving it empty is fine -- before 0009 all this could do was stuff in
            // concept
            let subject_id = match entity_ids.get(f.subject.trim()) {
                Some(id) => *id,
                None => {
                    resolve(
                        state,
                        doc.kb_id,
                        None,
                        f.subject.trim(),
                        ctx,
                        &mut doc_cache,
                        &mut needs_adjudication,
                    )
                    .await?
                }
            };
            let object_id = match entity_ids.get(object_name) {
                Some(id) => *id,
                None => {
                    resolve(
                        state,
                        doc.kb_id,
                        None,
                        object_name,
                        ctx,
                        &mut doc_cache,
                        &mut needs_adjudication,
                    )
                    .await?
                }
            };
            if subject_id == object_id {
                continue;
            }
            // First try hard to land on a relation the ontology already has (spelling / tense /
            // passive), and **only downgrade** to related_to when it cannot, counting it as a
            // miss. A downgrade flattens the original meaning into "is related to" -- the
            // original wording written into the evidence row's proposed_predicate is the only
            // place on this fact where that meaning still survives (predicate resolution maps
            // it back into the ontology from there)
            let (predicate_id, swap) = match pred_index.lookup(f.predicate.as_str()) {
                Some((id, swap)) => (Some(id), swap),
                None => {
                    let _ = utopia_store::ontology::record_miss(
                        &state.pool,
                        doc.kb_id,
                        "relation_type",
                        &f.predicate,
                        Some(&format!("{} → {}", f.subject, object_name)),
                    )
                    .await;
                    // No matching relation in the ontology → **there simply is no predicate**
                    // (see `facts.predicate_id`). The original meaning stays in the evidence's
                    // proposed_predicate and is fetched back for display.
                    // This used to land on related_to, which also meant worrying about "the
                    // fallback relation got deleted" -- that failure mode is gone along with
                    // its continue
                    (None, false)
                }
            };
            // A passive wording hits the reverse of the same edge: `ChatGPT produced_by OpenAI`
            // and `OpenAI produces ChatGPT` are one and the same edge, and it has to be stored
            // in the ontology's direction, otherwise it is stored apart from the 130 existing
            // produces facts and the graph shows two opposing arrows
            let (subject_id, object_id) = if swap {
                (object_id, subject_id)
            } else {
                (subject_id, object_id)
            };

            // **When the subject violates the domain while the object fits it, straighten it
            // out into the direction the ontology declares.**
            //
            // The prompt was tried first and lost three rounds running: the violation rate went
            // from 57% down to 35%, but everything it pushed down was the half that was a type
            // misjudgement; **true reversals did not budge** (22.7% → 17.1% → 17.6%, the last
            // two inside the noise). The model can see `employee (organization → person)` and
            // simply does not follow it -- English's "X is an employee of Y" is too strong.
            //
            // This is not a new principle: when `produced_by` hits `produces` (see that `swap`
            // above) subject and object have long been flipped automatically; the only
            // difference is whether the trigger is the **wording** or the **signature**.
            //
            // The original objection to swapping automatically was that entity types are
            // unreliable -- Elon Musk was measured being judged a `researcher`. That premise no
            // longer holds: after the ancestor floor and the always-present signature classes
            // were fixed, the same people came out as `person`. And the criterion itself is
            // narrow -- **the subject violates and the object fits** -- both sides have to line
            // up before anything moves.
            //
            // **But never silently.** Straightening has to leave a signal: what 0001 objected
            // to was "driving automatic actions off possibly wrong declarations", and an action
            // that is visible, reviewable and reversible is not of that kind.
            let (predicate_id, subject_id, object_id) = if let Some(pid) = predicate_id {
                // Types are **read from the entities in the database**, not from the
                // extractor's own `entity_type_of`: that map only covers entities the model
                // declared in this chunk, while the object is often an entity that already
                // exists elsewhere and this chunk did not declare again, so it cannot be looked
                // up, cannot be judged, cannot be straightened. The difference is far from
                // small -- with the declared map, reversals only came down to 7.0%, and the
                // remainder was exactly the objects whose type could not be found
                //
                // The judgement itself lives in the store (`ontology::judge_direction`) and is
                // **shared with adoption** (#190): more than one path writes a predicate, and a
                // guard installed on only one of them is no guard at all. A failed lookup (a
                // database error) is treated as having no criterion and lands as-is -- better to
                // straighten one fact fewer than to lose a fact over one failed query
                let fit = utopia_store::ontology::judge_direction(
                    &state.pool,
                    pid,
                    subject_id,
                    object_id,
                )
                .await
                .unwrap_or(utopia_store::ontology::Fit::Unchecked);
                match fit {
                    utopia_store::ontology::Fit::Swap => {
                        drop_signal(
                            state,
                            doc.kb_id,
                            document_id,
                            utopia_store::extraction_drops::reason::DIRECTION_CORRECTED,
                            &f.predicate,
                            Some(&format!(
                                "{} → {} straightened to {} → {} by signature",
                                f.subject,
                                f.object.as_deref().unwrap_or("?"),
                                f.object.as_deref().unwrap_or("?"),
                                f.subject
                            )),
                        )
                        .await;
                        (Some(pid), object_id, subject_id)
                    }
                    utopia_store::ontology::Fit::Neither => {
                        // **Swapping is not legal either → fall back to no predicate.**
                        //
                        // This is not a direction problem, this relation simply does not apply:
                        // schema.org's `affectedBy` is for medical tests, and the model ran
                        // into it by name while trying to express "affected by ..."; `amount`
                        // belongs to a financing instrument rather than to a company, and the
                        // model hung the edge on the company because it never created that
                        // intermediate node.
                        //
                        // Landing it as-is used to mean **saying something the ontology
                        // disagrees with, in the ontology's name** -- the graph reads "OpenAI
                        // affectedBy ...", and a reader takes that for a medical assertion.
                        // That is a confident error, far worse than an empty predicate.
                        //
                        // Falling back to an empty predicate loses no information: the original
                        // wording lands in `fact_evidence.proposed_predicate` and is fetched
                        // back for display by `fact_surface_predicate()` (0010). Subject,
                        // object, time and evidence are all kept, it just stops claiming an
                        // ontology relation it does not have. **Honest silence.**
                        drop_signal(
                            state,
                            doc.kb_id,
                            document_id,
                            utopia_store::extraction_drops::reason::DOMAIN_MISMATCH,
                            &f.predicate,
                            Some(&format!(
                                "{} — {} → neither subject nor object fits, kept original wording",
                                f.subject, f.predicate
                            )),
                        )
                        .await;
                        (None, subject_id, object_id)
                    }
                    utopia_store::ontology::Fit::Keep | utopia_store::ontology::Fit::Unchecked => {
                        (Some(pid), subject_id, object_id)
                    }
                }
            } else {
                (predicate_id, subject_id, object_id)
            };

            if await_nod {
                if let utopia_store::pending::Outcome::Proposed(_) = utopia_store::pending::propose(
                    &state.pool,
                    utopia_store::pending::Proposal {
                        kb_id: doc.kb_id,
                        subject_id,
                        predicate_id,
                        object_id: Some(object_id),
                        object_value: None,
                        proposed_predicate: Some(f.predicate.as_str()),
                        validity,
                        confidence,
                        chunk_id: chunk.id,
                        proposed_by,
                    },
                )
                .await?
                {
                    pending_count += 1;
                }
                continue;
            }
            {
                let (fact_id, created) = utopia_store::graph::insert_fact(
                    &state.pool,
                    doc.kb_id,
                    subject_id,
                    predicate_id,
                    object_id,
                    validity,
                    confidence,
                )
                .await?;
                touched_facts.push(fact_id);
                // A repeat observation gets evidence attached too: several sources corroborate
                // each other, and deleting any one of them does not orphan the fact. The
                // surface predicate is written down with every observation -- one chunk says
                // "runs on", another says "optimized for", and they merge into the same fact;
                // on the fact that would be first writer wins, on the evidence both are kept
                utopia_store::graph::add_evidence(
                    &state.pool,
                    fact_id,
                    chunk.id,
                    f.quote.as_deref(),
                    Some(f.predicate.as_str()),
                )
                .await?;
                if !created {
                    continue;
                }
                fact_count += 1;
                // Temporal reconciliation: for state relations with a uniqueness constraint, a
                // new fact triggers contradiction detection right away (a pure rule-based point
                // lookup; automatic closing goes through "void + rewrite", and anything unclear
                // goes to fact_conflicts for a human to rule on).
                // No predicate means no relation metadata, and therefore no part in temporal
                // reconciliation -- an edge that cannot say what relation it is could never have
                // carried a uniqueness constraint in the first place
                if let Some((pid, (func, inv_func, temporal))) =
                    predicate_id.and_then(|id| rel_meta.get(&id).map(|m| (id, m)))
                {
                    if temporal == "state" {
                        let mut directions = Vec::new();
                        if *func {
                            directions.push(utopia_store::temporal::Uniqueness::SubjectSide);
                        }
                        if *inv_func {
                            directions.push(utopia_store::temporal::Uniqueness::ObjectSide);
                        }
                        for dir in directions {
                            let report = utopia_store::temporal::reconcile_new_fact(
                                &state.pool,
                                doc.kb_id,
                                fact_id,
                                subject_id,
                                pid,
                                Some(object_id),
                                None,
                                dir,
                                validity,
                                confidence,
                            )
                            .await?;
                            if report.conflicts > 0 {
                                conflicts_found = true;
                            }
                        }
                    }
                }
            }
        }

        // Mark the chunk as soon as it finishes extracting: on update a claimed chunk carries
        // the mark and is skipped, and an interrupted extraction can resume (chunks whose LLM
        // call or parsing failed were `continue`d above, are left unmarked, and are retried
        // next time)
        utopia_store::documents::mark_chunk_extracted(&state.pool, chunk.id).await?;
    }
    // People are only woken when something was actually added to the queue: the Review count
    // and that confirmation card in the conversation both depend on this one call
    if pending_count > 0 {
        tracing::info!(%document_id, pending_count, "memory facts went into the pending queue");
        state.emit_pending(doc.kb_id);
        state.emit_review(doc.kb_id);
    }

    // Disambiguation suffixes computed at entity creation time would run before their facts
    // are written -- so every name this document touched is refreshed in one pass at the finish
    let names: std::collections::HashSet<&str> =
        doc_cache.keys().map(|(_, name)| name.as_str()).collect();
    for name in names {
        utopia_store::resolution::refresh_disambiguators(&state.pool, doc.kb_id, name).await?;
    }

    // Check once more on the way out: a takeover can happen after the last chunk, by which
    // time the check inside the loop has already run. Miss this and the displaced job writes
    // done on a document whose extracted_at was just cleared -- the UI shows "complete" while
    // in fact nothing was extracted, and it is only corrected once the new job starts running.
    if utopia_store::documents::extract_epoch(&state.pool, document_id).await? != my_epoch {
        tracing::info!(%document_id, "extraction taken over by a newer round, exiting at the finish");
        return Ok(());
    }

    // **If any chunk failed to extract, done must not be written.**
    //
    // This used to write done unconditionally: one network hiccup left only 12 of 60 chunks
    // across six documents extracted, all six showed "extraction complete", eighty percent of
    // the content never made it into the graph, and nothing in the UI said so.
    // The failures only reached the log, and an error that only reaches the log is no error.
    //
    // The chain is complete once Err is returned: `extract_document` records graph_failed plus
    // the reason, and that document becomes a failed one in the UI that can be clicked open to
    // see the error; the job retries with 30s×attempts² backoff, and chunks that already
    // succeeded carry extracted_at and get skipped -- so retrying is cheap and it heals itself
    // the moment the network comes back. Only once the retries are exhausted does it stay
    // failed, and by then it is telling the truth.
    //
    // The criterion is "every chunk done" rather than some percentage: any percentage is made
    // up, while there is already a criterion here that needs no making up -- **it counts as
    // done only when every single chunk succeeded**.
    if let Some(msg) = incomplete_reason(&unextracted, chunks.len()) {
        return Err(anyhow::anyhow!(msg));
    }
    // Facts that just landed go through the signature check immediately. Writing only
    // straightens the direction (judge_direction); the ones it cannot straighten -- neither
    // direction fits, or the object has no type so it cannot be judged -- used to surface only
    // when someone pressed Run check in Review, so Axioms stayed at 0 while reversed facts lay
    // in the graph (#222).
    // A failed check does not affect extraction itself: the facts are already in the database,
    // and the next Run check still finds them
    if !touched_facts.is_empty() {
        match utopia_store::reasoning::signature_breaks(
            &state.pool,
            doc.kb_id,
            Some(&touched_facts),
        )
        .await
        {
            Ok(broken) if !broken.is_empty() => {
                match utopia_store::reasoning::record_signature_breaks(
                    &state.pool,
                    doc.kb_id,
                    &broken,
                )
                .await
                {
                    Ok(_) => state.emit_review(doc.kb_id),
                    Err(e) => {
                        tracing::warn!(%document_id, error = %e, "post-extraction signature violations were not recorded in the queue")
                    }
                }
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(%document_id, error = %e, "post-extraction signature check failed")
            }
        }
    }
    utopia_store::documents::set_graph_status(&state.pool, document_id, "done").await?;
    state.emit_document(doc.kb_id, document_id);

    // Grey-area pairs went into the review queue → trigger the batched adjudication job (runs
    // independently in the background; extraction itself is finished at this point)
    if needs_adjudication {
        utopia_store::jobs::enqueue(
            &state.pool,
            "adjudicate_entities",
            serde_json::json!({ "kb_id": doc.kb_id }),
        )
        .await?;
    }
    if needs_adjudication || conflicts_found {
        state.emit_review(doc.kb_id);
    }
    // Ontology auto-extend: with the switch on and this batch fully extracted, the last
    // document triggers it. The criterion is the explicit switch rather than "has the ontology
    // been touched" -- the latter infers intent from behaviour, and getting that inference wrong
    // is absurd (one click of Add on a proposal turns suggestions off forever); worse, once it
    // is false it is never true again, and the ontology freezes on whatever vocabulary the
    // first batch of documents happened to contain.
    // Under concurrency it may be enqueued twice; the job re-checks the switch and the status
    // itself
    enqueue_bootstrap(state, doc.kb_id).await?;

    tracing::info!(%document_id, facts = fact_count, "graph extraction complete");
    Ok(())
}

/// Whether the string in the object position is a literal rather than the name of an entity.
///
/// **Numbers and dates only.** This judgement can eat real entities, so it prefers to miss:
/// a miss merely keeps today's behaviour (creating a concept entity), while getting it wrong
/// demotes a real entity to a piece of text and the graph loses a node.
///
/// Yes for "2015", "2023-03", "6"; no for "杭州" (a city name), "首席技术官" ("CTO"), "3M",
/// "V3". The caller additionally requires that the model did **not** declare it as an entity --
/// it only counts when both gates are passed.
fn looks_literal(s: &str) -> bool {
    let s = s.trim();
    if s.is_empty() {
        return false;
    }
    // All digits (decimals and a sign included). Parsed with f64 rather than scanning the
    // characters by hand: "3M", "V3" and "２０１５" (full-width) all fail, which is exactly
    // what we want
    if s.parse::<f64>().is_ok() {
        return true;
    }
    // Dates: reuse the parser on the extraction side, which accepts 2015 / 2015-03 /
    // 2015-03-01 and so on
    utopia_extract::parse_time(s).is_some()
}

/// The three listings in the prompt: classes, relations, attributes.
///
/// **Factored out so that "give everything" and "retrieve per chunk" share one and the same
/// layout logic.** Let each path lay out its own copy and they drift apart sooner or later, and
/// drifting here means the prompt says one thing while the code accepts another.
struct PromptLists {
    types: Vec<(String, String, String)>,
    relations: Vec<utopia_extract::PromptRelation>,
    attributes: Vec<String>,
}

impl PromptLists {
    /// How long these three listings are once laid into the prompt. The budget criterion uses
    /// it -- **it measures the actual text that will be laid out**, not a separate estimation
    /// formula (a formula would drift from the layout).
    fn chars(&self) -> usize {
        self.types
            .iter()
            .map(|(k, l, d)| k.len() + l.len() + d.len() + 6)
            .sum::<usize>()
            + self
                .relations
                .iter()
                .map(|r| r.key.len() + r.label.len() + r.description.len() + r.signature.len() + 8)
                .sum::<usize>()
            + self.attributes.iter().map(|a| a.len() + 1).sum::<usize>()
    }
}

/// Lay out the three listings from a **selection set**. `None` = give everything (the old path
/// for when the ontology is smaller than the budget).
///
/// All three details below come from selecting; with everything given they never trigger:
///
/// 1. **A signature may only mention selected classes**. The two keys in
///    `works_at (person → organization)` have to be ones the model can see -- writing a class
///    name that was not laid out teaches it to output a type that does not exist.
///    If a whole side went unselected it falls back to `*`.
/// 2. **Attributes follow their domain**. An attribute line is `class.attr`, and the line is
///    meaningless if its class was not laid out. That also takes care of trimming the
///    attribute section (28% of the prompt) as a side effect, with no separate handling.
/// 3. **Builtin classes are always present**. A chunk that retrieval misses still needs
///    somewhere to land, otherwise the model has no class to pick.
fn build_lists(
    etypes: &[utopia_core::models::EntityType],
    rtypes: &[utopia_core::models::RelationType],
    classes: Option<&HashSet<Uuid>>,
    rels: Option<&HashSet<Uuid>>,
) -> PromptLists {
    let picked_class = |id: &Uuid| classes.is_none_or(|s| s.contains(id));
    let picked_rel = |id: &Uuid| rels.is_none_or(|s| s.contains(id));
    let key_of: HashMap<Uuid, &str> = etypes
        .iter()
        .filter(|t| picked_class(&t.id))
        .map(|t| (t.id, t.key.as_str()))
        .collect();

    let types = etypes
        .iter()
        .filter(|t| picked_class(&t.id))
        .map(|t| (t.key.clone(), t.label.clone(), t.description.clone()))
        .collect();

    // If not one class on a side was laid out, write `*`: a signature is guidance, and
    // pointing at classes that cannot be seen only misleads
    let sig_of = |ids: &[Uuid]| -> String {
        let mut keys: Vec<&str> = ids
            .iter()
            .filter_map(|id| key_of.get(id).copied())
            .collect();
        if keys.is_empty() {
            return "*".into();
        }
        keys.sort_unstable();
        keys.join("|")
    };
    let relations = rtypes
        .iter()
        .filter(|r| r.kind != "attribute")
        .filter(|r| picked_rel(&r.id))
        .map(|r| {
            let signature = if r.domains.is_empty() && r.ranges.is_empty() {
                String::new()
            } else {
                format!("{} → {}", sig_of(&r.domains), sig_of(&r.ranges))
            };
            utopia_extract::PromptRelation {
                key: r.key.clone(),
                label: r.label.clone(),
                description: r.description.clone(),
                signature,
            }
        })
        .collect();

    let attributes = rtypes
        .iter()
        .filter(|r| r.kind == "attribute" && picked_rel(&r.id))
        .flat_map(|r| r.domains.iter().map(move |d| (r, d)))
        .filter_map(|(r, domain_id)| {
            let class_key = key_of.get(domain_id)?;
            let dt = r.datatype.as_deref().unwrap_or("text");
            let spec = match &r.unit {
                Some(u) if !u.is_empty() => format!("{dt}, {u}"),
                _ => dt.to_string(),
            };
            let d = r.description.trim();
            Some(if d.is_empty() {
                format!("- {class_key}.{} ({spec})", r.key)
            } else {
                format!("- {class_key}.{} ({spec}): {d}", r.key)
            })
        })
        .collect();

    PromptLists {
        types,
        relations,
        attributes,
    }
}

/// How many classes / relations / attributes are retrieved per chunk. **To be measured** --
/// just like the budget, settling them takes that curve.
const PER_CHUNK_CLASSES: i64 = 40;
const PER_CHUNK_RELATIONS: i64 = 30;
const PER_CHUNK_ATTRIBUTES: i64 = 30;

/// Retrieve candidates by this chunk's vector and lay out the three listings specific to it.
///
/// A failed retrieval returns `Ok(None)` rather than an error: the caller falls back to the
/// full listing. A big prompt is slow, having no class to pick means nothing gets extracted --
/// between the two, take the former.
async fn chunk_lists(
    state: &AppState,
    kb_id: Uuid,
    embedding: &[f32],
    etypes: &[utopia_core::models::EntityType],
    rtypes: &[utopia_core::models::RelationType],
    seed_classes: &HashSet<Uuid>,
) -> anyhow::Result<Option<PromptLists>> {
    let mut classes: HashSet<Uuid> = seed_classes.clone();
    classes.extend(
        utopia_store::ontology::nearest_entity_type_ids(
            &state.pool,
            kb_id,
            embedding,
            PER_CHUNK_CLASSES,
        )
        .await?,
    );
    // **Whatever gets hit, lay out its ancestors along with it.**
    //
    // Vector retrieval naturally favours the leaf classes that appear literally in the text.
    // Measured on a chunk about Sutskever, ranking 976 classes by distance: `researcher` came
    // 4th and `corporation` 27th, while `organization` came 177th and `person` 359th -- **not
    // one generalising base class in the top 40**. The text writes "a researcher at" and "the
    // corporation", it never writes "person".
    //
    // Two symptoms, one and the same root cause:
    //
    // - the entity is judged `researcher` (in schema.org that is a subclass of `Audience`, not
    //   a person), so every `works_for (domain=person)` turns into a violation
    // - the signature of `employee (organization → person)` **degrades to `(* → *)`** --
    //   `sig_of` only accepts classes that were laid out and writes `*` when a side was not.
    //   The model never saw that direction constraint at all
    //
    // This floor used to be held up by `seed_classes` (the `builtin` classes), and the comment
    // in `build_lists` read "builtin classes are always present: a chunk that retrieval misses
    // still needs somewhere to land". Once the seeds retired (#128) the criterion was left
    // dangling -- it had only happened to be equivalent because the seed classes were exactly
    // those few generic ones.
    //
    // Filling that floor with ancestors beats maintaining a list of "generic classes": **the
    // inheritance chain is the generalisation the ontology declares itself**, and which class
    // sits above which needs no second judgement from us. The price is a few extra ancestor
    // levels laid out per chunk.
    if !classes.is_empty() {
        let picked: Vec<Uuid> = classes.iter().copied().collect();
        classes.extend(utopia_store::ontology::ancestors_of(&state.pool, &picked).await?);
    }
    let mut rels: HashSet<Uuid> = HashSet::new();
    // Relations and attributes are retrieved separately: the two sections are separate in the
    // prompt, and fetching them together lets one section squeeze the other out to nothing
    rels.extend(
        utopia_store::ontology::nearest_relation_type_ids(
            &state.pool,
            kb_id,
            embedding,
            PER_CHUNK_RELATIONS,
            Some("relation"),
        )
        .await?,
    );
    rels.extend(
        utopia_store::ontology::nearest_relation_type_ids(
            &state.pool,
            kb_id,
            embedding,
            PER_CHUNK_ATTRIBUTES,
            Some("attribute"),
        )
        .await?,
    );
    // **Once a relation is laid out, the classes its signature names have to be laid out too.**
    //
    // Classes and relations are retrieved independently, while a signature depends on the
    // intersection of the two -- `sig_of` only accepts classes that were laid out and writes
    // `*` when a side was not. So this situation comes up often: `employee` is semantically
    // close to the text and gets fished in, while its `organization` (ranked 795th) and
    // `person` (630th) are literally far from the text and neither gets fished in at all, so
    // the signature degrades to `(* → *)` -- **the direction constraint disappears entirely**,
    // and the model writes `Musk --employee--> Microsoft` on English intuition while
    // schema.org declares organization → person.
    //
    // The ancestor floor above cannot cure this kind of chunk: it grows upward from "the leaves
    // that were hit", and here not one relevant leaf was hit, and with no leaf there is no
    // ancestor either.
    //
    // The comment on `sig_of` says "a signature pointing at classes that cannot be seen only
    // misleads" -- the worry is right, but erasing the signature pays for it by losing the
    // direction. **Pulling the classes in** saves both ends: the model can see the class, and
    // the signature can still be laid out. It is incidentally right too: these classes are
    // exactly the ones the model is about to use to judge types, `employee` being present means
    // this chunk is about employment, and `organization`/`person` belonged in the candidates
    // all along -- literal similarity cannot fish them out, but **the ontology's structure
    // knows**.
    let sig_classes: HashSet<Uuid> = rtypes
        .iter()
        .filter(|r| rels.contains(&r.id))
        .flat_map(|r| r.domains.iter().chain(r.ranges.iter()).copied())
        .collect();
    classes.extend(sig_classes);

    // Not a single candidate retrieved = the index is not built yet, so fall back to the full
    // listing rather than handing over an empty one
    if classes.len() <= seed_classes.len() && rels.is_empty() {
        return Ok(None);
    }
    Ok(Some(build_lists(
        etypes,
        rtypes,
        Some(&classes),
        Some(&rels),
    )))
}

#[cfg(test)]
mod name_tests {
    use super::is_entity_name;

    /// Every sample is taken from a kb that was really extracted (ai-timeline-ends ×
    /// schema.org), not made up.
    #[test]
    fn a_clause_is_not_a_thing() {
        for s in [
            "thermal-imaging equipment used by volunteers flying over the site showed at least 33 generators giving off heat",
            "about the same amount of power as the Tennessee Valley Authority's large gas-fired power plant nearby",
            "removal was driven by growing discontent and distrust with Altman",
            "a risk of developing cancer at four times the national average in 2013",
            "745 of OpenAI's 770 employees",
        ] {
            assert!(!is_entity_name(s), "this is a sentence, not an entity name: {s}");
        }
    }

    /// **Real entities get long, but carry no predicate.** The criterion is word count plus
    /// finite verbs, not character count -- the first one below is 57 characters, barely
    /// shorter than that 65-character clause above
    #[test]
    fn a_long_name_is_still_a_name() {
        for s in [
            "US District Court for the Northern District of California",
            "United States District Court for the District of Delaware",
            "OpenAI's board of directors",
            "Safe Superintelligence Inc.",
            "École Polytechnique",
            "GPT-4",
        ] {
            assert!(
                is_entity_name(s),
                "this is a real entity, it must not be blocked: {s}"
            );
        }
    }

    /// Participles are perfectly normal inside a noun phrase; listing them as markers would
    /// hit real entities.
    #[test]
    fn a_participle_is_not_a_predicate() {
        assert!(is_entity_name("equipment used by volunteers"));
        assert!(is_entity_name("Gas-Burning Turbines"));
    }

    /// Short names are not clause-checked: things like `Is` may be part of a proper name.
    #[test]
    fn a_short_name_is_never_a_clause() {
        assert!(is_entity_name("Was"));
        assert!(is_entity_name("Is Elon"));
    }

    #[test]
    fn an_empty_name_is_not_a_name() {
        assert!(!is_entity_name(""));
        assert!(!is_entity_name("   "));
    }
}

#[cfg(test)]
mod tests {
    use super::{incomplete_reason, looks_literal};

    #[test]
    fn only_numbers_and_dates_count_as_literals() {
        // Yes: in the object position these are values, not entities
        for yes in ["2015", "2023-03", "2024-01-15", "1200", "62.5", "-3"] {
            assert!(looks_literal(yes), "{yes} should be taken as a literal");
        }
        // No: the cost of getting it wrong is demoting a real entity to a piece of text,
        // so prefer to miss
        for no in [
            "杭州",
            "首席技术官",
            "3M",
            "V3",
            "深蓝存储",
            "",
            "   ",
            "２０１５", // full-width digits: not the form we handle, left to the entity path
        ] {
            assert!(!looks_literal(no), "{no} must not be taken as a literal");
        }
    }

    /// Why this criterion exists: one network hiccup left only 12 of 60 chunks across six
    /// documents extracted, all six **showed "extraction complete"**, eighty percent of the
    /// content never made it into the graph, and nothing in the UI said so.
    #[test]
    fn a_document_with_a_skipped_chunk_is_not_complete() {
        assert_eq!(
            incomplete_reason(&[], 23),
            None,
            "only every chunk done counts as complete"
        );
        let one = [(7, "call failed: timeout".to_string())];
        let msg = incomplete_reason(&one, 23).expect("a failed chunk must not count as complete");
        assert!(
            msg.contains("23"),
            "the denominator has to be said out loud: {msg}"
        );
        assert!(msg.contains("#7"), "it has to point out which chunk: {msg}");
    }

    /// The reason is usually the same one (if the vendor is down then every chunk is down), so
    /// three examples are enough, but **how many are left has to be said** -- otherwise the
    /// reader thinks only three chunks broke.
    #[test]
    fn many_failures_are_summarised_without_hiding_the_count() {
        let many: Vec<(i32, String)> = (1..=20).map(|i| (i, "call failed".into())).collect();
        let msg = incomplete_reason(&many, 60).unwrap();
        assert!(msg.contains("20"), "the total has to be in there: {msg}");
        assert!(
            msg.contains("17 more"),
            "the omitted count has to be said out loud: {msg}"
        );
        assert_eq!(
            msg.matches("call failed").count(),
            3,
            "only three are listed"
        );
    }

    /// On a retry `chunks_for_extraction` only picks up the chunks not yet extracted, so the
    /// denominator is **this round's** count, not the document's total. The wording says so
    /// plainly, so nobody comes away thinking the document only has this many chunks.
    #[test]
    fn the_denominator_is_this_rounds_chunks_not_the_document() {
        let msg = incomplete_reason(&[(2, "x".into())], 3).unwrap();
        assert!(msg.starts_with("1 of this round's 3 chunks"), "{msg}");
    }
}
