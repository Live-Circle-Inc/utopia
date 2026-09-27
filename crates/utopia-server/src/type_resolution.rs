//! Type resolution: getting an entity's type right -- **narrowing it, and also bending back
//! the ones that went wrong**.
//!
//! At first only the first half existed: extraction hands over a coarse type and resolution
//! refines towards its descendants. Once extraction started retrieving candidates per chunk
//! it began picking specific types itself, which means it also picks wrong ones (measured:
//! `Shaoxing → address`, `chronic-disease-management mini-app → entry_point`). And the right
//! answer is a **sibling** of the wrong class rather than a descendant of it, so it was kept
//! outside the door by the "a candidate must be more specific" rule -- a rule I wrote into
//! the prompt myself.
//!
//! Both directions count now. A correction is naturally judged as crossing an axis, so it
//! always goes to a human: overturning extraction's judgement is riskier than refining it,
//! and should not happen automatically.
//!
//! Since 0009 there is a third kind of input, and it is the commonest one: **no type at all
//! yet**. With the fallback class deleted, an entity the ontology cannot hold is no longer
//! stuffed into `concept`; it gets `type_id IS NULL`. This tier carries no "judgement by
//! extraction" to overturn -- typing it is filling in a blank, not reclassifying -- so it is
//! **not judged as crossing an axis** and lands in the database directly at high confidence.
//! Otherwise the price of deleting the sentinel is a human eyeballing every single entity.
//!
//! **Why this can be done after the fact while predicates cannot** (that asymmetry in 0001):
//! a type is an annotation hung on a node and can be attached once the evidence has piled up;
//! a predicate is the fact itself, and `(NVIDIA, ?, Mellanox)` is simply not a fact. So
//! predicates go "record the original wording first, map later", and types go "coarse first,
//! precise later".
//!
//! What resolution has in hand is completely different from what extraction had at the time:
//!
//! 1. **`proposed_type`** -- the type name the model reported itself during extraction, kept
//!    only when the vocabulary had nothing for it. The strongest one, because the task turns
//!    from "work out what this is" into "which class in the ontology is called that".
//! 2. **The entity profile** -- every predicate it takes part in, accumulated across
//!    documents.
//! 3. **Evidence quotes** -- the same sentences, but gathered together, and not having to
//!    compete for attention with dozens of other entities.
//!
//! Candidates come from **two routes**, unioned rather than score-merged: one searches the
//! class descriptions with the profile, the other searches already-typed entities with the
//! context vector and counts their classes as votes. The second route sidesteps the first
//! one's weak spot (a Chinese profile against boilerplate English descriptions), and it gets
//! more accurate the larger the knowledge base grows. The two routes' distances live in
//! different spaces, one in class space and one in entity space, so merging them into a
//! single ranking is self-deception -- all the more so since, measured, distances within one
//! route cannot even be compared across entities.
//!
//! Descendants of the coarse type come **first**, but they are not the only thing selectable
//! -- the classification axes of two ontologies often fail to line up (schema.org hangs
//! software under CreativeWork, while the coarse type extraction gave is product). The step
//! that ought to say no is adjudication, which can see the description and the coarse type
//! alike.

use crate::{llm_util, ontology_index, AppState};
use utopia_core::{AppError, AppResult};
use uuid::Uuid;

/// How many entities per round. Enough for one human pass, and enough to show whether
/// retrieval is any good.
const BATCH: i64 = 60;
/// How many candidates to retrieve per entity.
///
/// **Retrieval's job is to serve candidates; saying no is adjudication's job.** So it is
/// better to serve two too many -- adjudication can see the descriptions and the current
/// type and can say "none of these is it", whereas what retrieval misses, no amount of good
/// adjudication can reach. The failures measured in 0001 P3a were all on the retrieval side
/// (`administrative_area` and `periodical` were never served up at all).
///
/// Raised from 8 to 10: the two routes alternate, five seats each. The cost is two more lines
/// of class definitions in the prompt, and a missed retrieval has no fallback.
const CANDIDATES: i64 = 10;
/// How many already-typed neighbours to look at. Too few and the votes do not add up; too
/// many and the tail is all noise.
const NEIGHBOURS: i64 = 10;

/// One entity's resolution suggestion (for preview; nothing is written to the database).
#[derive(Debug, serde::Serialize)]
pub struct TypeSuggestion {
    pub entity_id: Uuid,
    pub name: String,
    /// The class hung on it now, which **may be absent** (0009)
    pub coarse: Option<String>,
    /// The current class's description. Adjudication has to judge "is this class right", and
    /// the key alone is not enough
    pub coarse_description: Option<String>,
    /// The coarse class's id. Adjudication pairs it with the target class to look up "has a
    /// human approved this pair"
    #[serde(skip)]
    pub coarse_id: Option<Uuid>,
    pub proposed_type: Option<String>,
    pub specific_type: Option<String>,
    pub fact_count: i64,
    /// The text sent off to retrieval. **Returned to the caller** -- when retrieval finds
    /// nothing, the first thing to look at is "what did we go looking with"
    pub profile: String,
    /// Route one candidates: profile → class descriptions
    pub candidates: Vec<utopia_core::models::TypeCandidate>,
    /// Route two candidates: already-typed entities with similar context, voting by class
    pub neighbours: Vec<NeighbourVote>,
    /// Every descendant of the coarse class. Adjudication tiers on this: the chosen class is
    /// in here = one step down, not in here = a change of classification axis.
    /// **Not serialised** -- it is there for the tiering, not for people to read
    #[serde(skip)]
    pub descendants: std::collections::HashSet<Uuid>,
}

/// One class the neighbours voted for.
///
/// **Not merged into a single ranking with `candidates`**: the two routes' distances live in
/// different spaces, one in class space and one in entity space, and were never comparable
/// -- all the more so since, measured, distances within one route cannot even be compared
/// across entities. The union goes to adjudication with each side labelled by where it came
/// from. A side benefit is that this route's evidence is readable by a human: "like Milvus,
/// and Milvus is labelled software_application" is far more use than "cosine 0.49".
#[derive(Debug, serde::Serialize)]
pub struct NeighbourVote {
    pub key: String,
    /// How many neighbours are of this class
    pub votes: usize,
    /// How close the nearest of those neighbours is
    pub best_distance: f64,
    /// The names of the voting entities, the evidence a person reads
    pub examples: Vec<String>,
    /// Whether these neighbours **all** come from the same batch of documents.
    /// If so this vote gets discounted: an entity that appears only once has the context
    /// vector of that one block, entities from the same document naturally become each
    /// other's neighbours, and that is not evidence about type
    pub same_document_only: bool,
}

/// Compute only, write nothing: the profile and candidates for every entity awaiting
/// resolution.
///
/// Making it a step of its own is deliberate -- before spending effort on adjudication,
/// answer "can retrieval find the thing at all". If it cannot, no amount of good adjudication
/// is any use.
pub async fn preview(state: &AppState, kb_id: Uuid) -> AppResult<Vec<TypeSuggestion>> {
    // Only the class half: this step has no use for relations, and a large ontology has over
    // a thousand of them
    let _ = ontology_index::refresh_scoped(
        state,
        kb_id,
        Some(utopia_store::ontology::TypeKind::Entity),
    )
    .await;
    let subjects =
        utopia_store::resolution::entities_for_type_resolution(&state.pool, kb_id, BATCH).await?;
    if subjects.is_empty() {
        return Ok(Vec::new());
    }
    // **Two queries per entity, not one.**
    //
    // The model's own wording comes first in the profile, but the whole passage after it is
    // context and goes into the vector just the same. Measured on Chinese text, and quoted
    // verbatim because that is what was measured: the profile for "杭州拱墅区" (Gongshu
    // District, Hangzhou) was `district. 杭州拱墅区. located_in by 仁和堂连锁药房. 仁和堂连锁
    // 药房在杭州拱墅区开设了第 40 家门店` ("... located_in by Renhetang Pharmacy Chain.
    // Renhetang Pharmacy Chain opened its 40th store in Gongshu District, Hangzhou") -- the
    // passage as a whole is about a pharmacy, so the candidates came back pharmacy and store
    // and administrative_area never came up once. The name had been diluted into the
    // paragraph.
    //
    // So the name is sent on its own: a short query against a short label is exactly the
    // shape this index is good at. The profile query is still sent -- it covers the entities
    // with no specific_type and the ones that can only be judged from context. The two
    // result sets are unioned: one extra embedding buys away a whole class of misses.
    let profiles: Vec<String> = subjects.iter().map(profile_of).collect();
    let names: Vec<Option<String>> = subjects.iter().map(name_query_of).collect();
    // **Each route queries its own index** (see `entity_types.label_embedding`). The two
    // routes used to share the whole-passage index, and the short-wording route got taken
    // over by tautological classes: `district. place` came back with Map / Park / Country /
    // Museum, whose four winning embeddings were all the one-liner `X\nAn X.`, while
    // AdministrativeArea with its hundred-word definition could not place in the top eight.
    // Short against short, long against long -- that is what is comparable
    let name_queries: Vec<String> = names.iter().flatten().cloned().collect();
    let (profile_hits, name_hits) = tokio::join!(
        ontology_index::nearest_for_each(
            state,
            kb_id,
            &profiles,
            // Take extra and filter by ancestor afterwards: the filtering happens after
            // retrieval, so leave headroom for what gets filtered out
            CANDIDATES * 4,
            ontology_index::Target::Class,
        ),
        ontology_index::nearest_for_each(
            state,
            kb_id,
            &name_queries,
            CANDIDATES * 4,
            ontology_index::Target::ClassLabel,
        )
    );
    let profile_hits = profile_hits.unwrap_or_default();
    let name_hits = name_hits.unwrap_or_default();
    // Each entity's slot in the two result sets: (profile, name). On the name route only
    // entities that could offer a wording had a query sent, so those slots are counted
    // separately
    let mut slots: Vec<(usize, Option<usize>)> = Vec::with_capacity(subjects.len());
    let mut ni = 0usize;
    for (pi, n) in names.iter().enumerate() {
        let slot = n.as_ref().map(|_| {
            let k = ni;
            ni += 1;
            k
        });
        slots.push((pi, slot));
    }

    let mut out = Vec::with_capacity(subjects.len());
    for (i, s) in subjects.iter().enumerate() {
        // Descendants of the coarse class come **first, but they are not the only thing
        // selectable**.
        //
        // This started out as a hard gate (descendants only), and measured, it blocked 4
        // correct answers out of 17 entities: schema.org hangs both SoftwareApplication and
        // Periodical under CreativeWork, while the coarse types extraction gave were
        // product / organization -- the two classifications' axes simply do not line up, and
        // gating hard locks the right answer outside the door forever. Same lesson as the
        // part_of episode: **a signature is guidance, not a gate**; losing data
        // systematically costs far more than the occasional misjudgement.
        //
        // The step that ought to say no is adjudication: it can see the description and the
        // coarse class, and can say "this is not it". An entity with no class yet has no
        // "descendants of the coarse class" axis to use (0009), so the whole class table is
        // candidate material and the ordering comes purely from retrieval order
        let descendants: std::collections::HashSet<_> = match s.coarse_id {
            Some(c) => utopia_store::resolution::descendants_of(&state.pool, kb_id, c)
                .await?
                .into_iter()
                .collect(),
            None => std::collections::HashSet::new(),
        };
        // **Take from the two routes alternately, do not merge by distance.**
        //
        // Distances are not comparable between the routes: a short query ("pharmaceutical
        // group") systematically produces smaller distances than a whole profile passage, so
        // sorting by distance hands the top few places to the name route alone. Measured,
        // after doing exactly that, Renhe Pharmaceutical Group, the National Medical Products
        // Administration and the Chinese Medical Association all had their correct candidate
        // squeezed out -- and the round before, all three had passed automatically. Same rule
        // as the "union, do not merge scores" one about routes A and B, and here I broke it.
        //
        // With alternation the two routes hold half the seats each, and the size of anyone's
        // distance no longer decides who gets seen.
        let (pi, ni) = slots[i];
        let mut lists: Vec<Vec<_>> = vec![profile_hits.get(pi).cloned().unwrap_or_default()];
        if let Some(k) = ni {
            lists.push(name_hits.get(k).cloned().unwrap_or_default());
        }
        let mut seen_ids: std::collections::HashSet<Uuid> = std::collections::HashSet::new();
        let mut ranked: Vec<utopia_core::models::TypeCandidate> = Vec::new();
        let longest = lists.iter().map(Vec::len).max().unwrap_or(0);
        for rank in 0..longest {
            for list in &lists {
                let Some(c) = list.get(rank) else { continue };
                if Some(c.id) == s.coarse_id || !seen_ids.insert(c.id) {
                    continue;
                }
                ranked.push(c.clone());
            }
        }
        // **Descendants are no longer promoted unconditionally.**
        //
        // This line used to be `ranked.sort_by_key(|c| !descendants.contains(&c.id))`: the
        // key is a boolean, so the candidates got cut into two heaps, "descendants" and
        // "non-descendants", the first heap beating the second wholesale with **distance
        // playing no part at all**. Measured on "Deep Blue Vector Database" (current class
        // product, specific = vector database software): the correct answer
        // `software_application` ranked **1st** in retrieval, and was still squeezed out of
        // the candidates by these five unrelated product descendants --
        // product_collection (0.437), some_products (0.440) and the rest -- which got in
        // purely on parentage, not on resemblance.
        //
        // This preference was already expressed three times over: this sort line, the
        // narrowing/correcting distinction in the prompt, and the `crosses_axis` tiering.
        // Delete the one that pays the least attention to evidence and the other two stand as
        // before -- an axis-changing edit still does not land in the database automatically,
        // still goes to a human, and the risk surface has not moved.
        //
        // The order that remains is the alternating retrieval order of the two routes:
        // retrieval decides what gets served up, adjudication decides what it is, and
        // `crosses_axis` decides whether a human has to look. One thing per layer.
        let candidates: Vec<_> = ranked.into_iter().take(CANDIDATES as usize).collect();
        // Route two: already-typed entities with similar context, voting by class
        let raw =
            utopia_store::resolution::nearest_typed_entities(&state.pool, kb_id, s.id, NEIGHBOURS)
                .await?;
        let mut votes: std::collections::BTreeMap<String, (usize, f64, Vec<String>, bool)> =
            std::collections::BTreeMap::new();
        for (name, _tid, key, distance, same_doc) in raw {
            let slot = votes
                .entry(key)
                .or_insert_with(|| (0, distance, Vec::new(), true));
            slot.0 += 1;
            slot.1 = slot.1.min(distance);
            if slot.2.len() < 3 {
                slot.2.push(name);
            }
            slot.3 &= same_doc;
        }
        let mut neighbours: Vec<NeighbourVote> = votes
            .into_iter()
            .filter(|(key, _)| Some(key) != s.coarse_key.as_ref())
            .map(
                |(key, (votes, best_distance, examples, same_document_only))| NeighbourVote {
                    key,
                    votes,
                    best_distance,
                    examples,
                    same_document_only,
                },
            )
            .collect();
        neighbours.sort_by(|a, b| {
            b.votes
                .cmp(&a.votes)
                .then(a.best_distance.total_cmp(&b.best_distance))
        });

        out.push(TypeSuggestion {
            entity_id: s.id,
            name: s.canonical_name.clone(),
            coarse: s.coarse_key.clone(),
            coarse_description: s.coarse_description.clone(),
            coarse_id: s.coarse_id,
            proposed_type: s.proposed_type.clone(),
            specific_type: s.specific_type.clone(),
            fact_count: s.fact_count,
            profile: profiles[i].clone(),
            candidates,
            neighbours,
            descendants,
        });
    }
    Ok(out)
}

/// The entity profile: the passage of text sent off for vector retrieval.
///
/// **The type name the model reported itself goes first.** It is the strongest signal, and
/// what retrieval matches against is the class's `label + description` -- one class name
/// against a class definition is far closer than a string of predicates against one.
///
/// Name and aliases next; predicates and quotes pad the end as context.
///
/// This passage is the **context query**; it and the short query from [`name_query_of`] are
/// each sent once and the results unioned.
fn profile_of(s: &utopia_store::resolution::TypeCandidateSubject) -> String {
    let mut parts: Vec<String> = Vec::new();
    // The model's own wording goes first, and if both are there write both. They are
    // **names**, and what retrieval is aiming at (the class's label) is a name too -- name
    // against name is exactly the shape this index is good at
    for named in [s.specific_type.as_deref(), s.proposed_type.as_deref()] {
        if let Some(p) = named.map(str::trim).filter(|x| !x.is_empty()) {
            parts.push(p.to_string());
        }
    }
    parts.push(s.canonical_name.clone());
    if !s.aliases.is_empty() {
        parts.push(s.aliases.join(", "));
    }
    if !s.roles.is_empty() {
        parts.push(s.roles.join(" "));
    }
    // Quotes pad the end as context, and only two of them: they are indeed sentences about
    // this entity, but they also drag in a pile of things that have nothing to do with its
    // type (dates, numbers, other entities), and too many of them pull the profile's centre
    // of gravity from "what is this" over to "what does this passage talk about". The query
    // side already guarantees only subject-position ones are taken
    for q in s.quotes.iter().take(2) {
        parts.push(q.chars().take(120).collect());
    }
    parts.join(". ")
}

/// One adjudication verdict.
#[derive(Debug, serde::Deserialize)]
struct Verdict {
    /// The number of the entry in the prompt -- **the key back to preview**.
    ///
    /// This used to rely on `name`. Since 0009 untyped entities with the same name can
    /// coexist (NULL ≠ NULL, see the unique-index section of that record), and one knowledge
    /// base measured 4 people called "Zhang Wei": matching back by name collapses them into
    /// one entry, the same entity_id gets pushed into picks four times, and the write hits the
    /// (batch_id, entity_id) primary key; meanwhile the other three would never be typed at
    /// all -- they are simply invisible to this path
    #[serde(default)]
    id: Option<usize>,
    /// The entity name. The fallback for when the model omits the id, and enough when names
    /// do not repeat
    name: String,
    /// The chosen class key; empty when it cannot be judged
    #[serde(default)]
    choice: Option<String>,
    #[serde(default)]
    confidence: Option<f32>,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct VerdictReply {
    #[serde(default)]
    verdicts: Vec<Verdict>,
}

/// Anything below this line goes to a human, wherever it lands.
///
/// **It is not the main criterion for the grey zone**: measured, the confidence the model
/// reports about itself is bimodal -- 15 verdicts all ≥0.85 and 4 null, with nothing in
/// between. Self-reported confidence is prose style, not probability; the model picks a
/// number that suits its own tone of voice. Use it as a gate and the gate stops nothing.
/// What really does the tiering is the "did it cross a classification axis" below; this only
/// catches the occasional low score.
const AUTO_THRESHOLD: f32 = 0.85;

/// The result of one resolution run, accounting to the caller for where each of the three
/// tiers went.
#[derive(Debug, serde::Serialize)]
pub struct ResolutionOutcome {
    pub batch: Option<Uuid>,
    /// Changed automatically
    pub retyped: u32,
    /// Crossed a classification axis, or not confident enough: left to a human
    pub for_review: Vec<ReviewItem>,
    /// The ones adjudication called "none of these", **together with the reason it gave**.
    ///
    /// A single count is not enough: the whole design of this step bets on "choosing none of
    /// these is a respectable answer", and that is the largest tier -- without the reasons,
    /// the largest tier is opaque. Same rule as "it has to be able to say why" in the
    /// ontology import preview.
    pub left_alone: Vec<DeclineNote>,
}

#[derive(Debug, serde::Serialize)]
pub struct DeclineNote {
    pub name: String,
    pub coarse: Option<String>,
    pub specific_type: Option<String>,
    /// The reason the model gave; empty when it never mentioned this entity at all
    pub reason: Option<String>,
    /// The first candidate retrieval gave. When the reason does not add up, this is what
    /// tells you whether retrieval failed to find it or adjudication turned it down
    pub top_candidate: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct ReviewItem {
    pub entity_id: Uuid,
    pub name: String,
    pub coarse: Option<String>,
    /// The two ends of the pair. Approving this one approves **this pair of classes**, not
    /// this one entity. The starting point may be absent (0009) -- and then there is no "pair
    /// of classes" to approve, only one-by-one edits
    pub from_type_id: Option<Uuid>,
    pub to_type_id: Uuid,
    pub choice: String,
    pub confidence: f32,
    pub reason: Option<String>,
    /// The chosen class is **not** in the coarse class's subtree -- it changes the
    /// classification axis rather than stepping one level down
    pub crosses_axis: bool,
}

/// Run one round of type resolution and write it to the database.
///
/// **No actor is passed on the write, even though a human clicked run.** The actor parameter
/// of `retype_entities` now carries two identities: the ledger records "who changed it", and
/// it now also decides `type_source` -- an actor means `human`, and `human` means **the
/// engine never touches this entity again**.
///
/// Those two questions are not the same thing:
///
/// | | who started it | who judged what this entity is |
/// |---|---|---|
/// | changing an entity's type by hand | human | human |
/// | approving a class pair + naming the entity | human | human |
/// | **running a round of resolution** | a human clicked run | **the engine** |
///
/// Passing user on the third row amounts to declaring "a human judged this class", and so
/// **any entity that has been through resolution once is never resolved again**. Measured: a
/// knowledge base with no manual PATCH history at all (0 rows of `entity.retyped` audit) had
/// every entity turn into `type_source = human` after one resolution run, and the next
/// preview returned an empty list.
///
/// Who clicked run is recorded in the `ontology.types_resolved` audit entry, with the batch
/// id, and can be looked up.
pub async fn resolve(state: &AppState, kb_id: Uuid) -> AppResult<ResolutionOutcome> {
    let items = preview(state, kb_id).await?;
    let items: Vec<_> = items
        .into_iter()
        .filter(|i| !i.candidates.is_empty() || !i.neighbours.is_empty())
        .collect();
    if items.is_empty() {
        return Ok(ResolutionOutcome {
            batch: None,
            retyped: 0,
            for_review: Vec::new(),
            left_alone: Vec::new(),
        });
    }

    let kb = utopia_store::kbs::get(&state.pool, kb_id).await?;
    let settings = utopia_store::settings::get(&state.pool, kb.workspace_id)
        .await?
        .ok_or_else(|| AppError::invalid("no_chat_model", "Chat model not configured"))?;
    let client = llm_util::chat_client(&settings)
        .ok_or_else(|| AppError::invalid("no_chat_model", "Chat model not configured"))?;

    let reply = client
        .chat(&[utopia_llm::ChatMessage {
            role: "user".into(),
            content: adjudication_prompt(&items),
        }])
        .await
        .map_err(AppError::Other)?;
    let block = utopia_extract::json_block(&reply).map_err(AppError::Other)?;
    let parsed: VerdictReply =
        serde_json::from_str(&block).map_err(|e| AppError::Other(e.into()))?;

    // Name → a **list** of indices, not a single one. Untyped entities with the same name can
    // coexist (0009), and collapsing them into one entry leaves several of them without a
    // verdict forever. Verdicts are matched back by id first; the name is only the fallback
    // for when the model omits the id
    let mut by_name: std::collections::HashMap<&str, Vec<usize>> = std::collections::HashMap::new();
    for (i, it) in items.iter().enumerate() {
        by_name.entry(it.name.as_str()).or_default().push(i);
    }
    // The pairs a human has approved: the same pair does not go to a human again. Crossing an
    // axis is a matter between two classes, and an entity merely happens to run into it --
    // the second city should not have to be asked all over again
    let approved = utopia_store::resolution::approved_refinements(&state.pool, kb_id).await?;
    let mut picks: Vec<(Uuid, Uuid)> = Vec::new();
    let mut for_review: Vec<ReviewItem> = Vec::new();
    let mut left_alone: Vec<DeclineNote> = Vec::new();
    let mut decided: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let decline = |item: &TypeSuggestion, reason: Option<String>| DeclineNote {
        name: item.name.clone(),
        coarse: item.coarse.clone(),
        specific_type: item.specific_type.clone(),
        reason,
        top_candidate: item.candidates.first().map(|c| c.key.clone()),
    };
    for v in &parsed.verdicts {
        // id first; when it is missing fall back to the name and take the first entry under
        // that name that has **not been adjudicated yet**. An out-of-range id counts as not
        // given -- the model occasionally invents one
        let idx = v.id.filter(|i| *i < items.len()).or_else(|| {
            by_name
                .get(v.name.as_str())
                .and_then(|ids| ids.iter().find(|i| !decided.contains(i)).copied())
        });
        let Some(idx) = idx else { continue };
        // Only the first verdict for an entry counts. When the model answers twice, the
        // second one pushes the same entity_id into picks again and the write hits the
        // primary key
        if !decided.insert(idx) {
            continue;
        }
        let item = &items[idx];
        // **The string "null" is null too.** The model sometimes gives JSON null and
        // sometimes gives those four letters, and looking that up as a candidate key can
        // never match, so the entry got recorded as "chose a key outside the candidates" --
        // a fabricated refusal reason covering over the one the model actually gave. The
        // refusal reason is the most important output of this step, and having it dirtied by
        // our own parsing is worse than not having it
        let Some(choice) = v.choice.as_deref().map(str::trim).filter(|c| {
            !c.is_empty() && !c.eq_ignore_ascii_case("null") && !c.eq_ignore_ascii_case("none")
        }) else {
            left_alone.push(decline(item, v.reason.clone()));
            continue;
        };
        // Only keys from the candidate list count. An answer outside the list is not "a
        // better judgement", it is the model writing down a schema.org name from memory --
        // which the ontology may well not have
        let Some(target) = item.candidates.iter().find(|c| c.key == choice) else {
            left_alone.push(decline(
                item,
                Some(format!("chose {choice}, which is not among the candidates")),
            ));
            continue;
        };
        let confidence = v.confidence.unwrap_or(0.0);
        // **The tiering looks at whether a classification axis was crossed, not at the
        // number the model reports about itself.**
        //
        // Inside the coarse class's subtree = one step down, extraction's judgement was not
        // overturned, change it automatically. Outside it = a change of classification axis
        // (something judged product landing underneath CreativeWork), which is
        // reclassification rather than refinement and is worth a human glance. The one clear
        // error in that measured round ("China Data Intelligence" → publication_issue) was
        // exactly this kind.
        //
        // **Corrections come through here too, and naturally so**: after extraction picks the
        // wrong specific type (Shaoxing → address), the right answer is its sibling rather
        // than its descendant, so it is always judged as crossing an axis and always goes to
        // a human. That is exactly what is wanted -- overturning extraction's judgement is
        // riskier than refining it, and should not happen automatically.
        //
        // **An entity with no class yet does not count as crossing an axis** (0009): it
        // carries no "judgement by extraction" to be overturned, and typing it for the first
        // time is filling in a blank rather than reclassifying. Judging it as crossing an
        // axis here would mean a human looking at every single entity now that the fallback
        // class is gone, and the whole automation would be worthless
        let crosses_axis = match item.coarse_id {
            Some(from) => {
                !item.descendants.contains(&target.id) && !approved.contains(&(from, target.id))
            }
            None => false,
        };
        if confidence >= AUTO_THRESHOLD && !crosses_axis {
            picks.push((item.entity_id, target.id));
        } else {
            for_review.push(ReviewItem {
                entity_id: item.entity_id,
                name: item.name.clone(),
                coarse: item.coarse.clone(),
                from_type_id: item.coarse_id,
                to_type_id: target.id,
                choice: choice.to_string(),
                confidence,
                reason: v.reason.clone(),
                crosses_axis,
            });
        }
    }
    // Entities adjudication never mentioned at all count as "left alone" too, otherwise the
    // three tiers do not add up to the total
    for (i, item) in items.iter().enumerate() {
        if !decided.contains(&i) {
            left_alone.push(decline(item, None));
        }
    }

    let (batch, retyped) = if picks.is_empty() {
        (None, 0)
    } else {
        let (b, n) =
            utopia_store::resolution::retype_entities(&state.pool, kb_id, &picks, None).await?;
        (Some(b), n)
    };
    Ok(ResolutionOutcome {
        batch,
        retyped,
        for_review,
        left_alone,
    })
}

/// The adjudication prompt.
///
/// **"None of these" has to be a respectable answer.** This is the exact opposite of the
/// `related_to` escape hatch: there the fallback option destroyed information, so it was
/// withdrawn; here keeping the coarse class loses nothing -- the entity is still on the graph
/// and its facts are still attached, it just did not get more specific. Forcing the model to
/// pick one of the candidates buys a batch of confident errors, and ones that do not enter
/// the timeline and are not easy to see.
fn adjudication_prompt(items: &[TypeSuggestion]) -> String {
    let mut blocks = Vec::new();
    // **The number is the key, the name is not** (0009). Untyped entities with the same name
    // can coexist, and one knowledge base measured 4 people called "Zhang Wei" -- matching
    // back by name collapses them into one entry
    for (i, it) in items.iter().enumerate() {
        // Give the current class together with its description: judging "is this class right"
        // takes more than the key alone -- keys from an imported ontology often cannot
        // explain themselves (what is an `entry_point`?)
        //
        // For an entity with no class, say so outright (0009). This line used to always read
        // concept, so what the model read was "it is already a concept" -- a false prior;
        // what it reads now is "not decided yet", which is the truth
        let current = match (&it.coarse, it.coarse_description.as_deref().map(str::trim)) {
            (Some(k), Some(d)) if !d.is_empty() => format!("{k} ({d})"),
            (Some(k), _) => k.clone(),
            (None, _) => "not yet typed".into(),
        };
        let mut lines = vec![format!(
            "### [{}] {}\ncurrently: {}\nthe extractor called it: {}\nseen as: {}",
            i,
            it.name,
            current,
            it.specific_type.as_deref().unwrap_or("-"),
            it.profile.chars().take(200).collect::<String>()
        )];
        lines.push("candidates:".into());
        for c in &it.candidates {
            let d = c.description.trim();
            lines.push(if d.is_empty() {
                format!("- {} ({})", c.key, c.label)
            } else {
                format!("- {}: {d}", c.key)
            });
        }
        if !it.neighbours.is_empty() {
            let n: Vec<String> = it
                .neighbours
                .iter()
                .take(3)
                .map(|n| format!("{} (like {})", n.key, n.examples.join(", ")))
                .collect();
            lines.push(format!("similar entities are typed: {}", n.join("; ")));
        }
        blocks.push(lines.join("\n"));
    }
    format!(
        "You are fixing entity types in a knowledge graph. Each entity below has a type it was \
         given during extraction, and a list of candidate types retrieved from the ontology.\n\
         \n\
         For each entity choose ONE candidate key, or null. There are two reasons to choose \
         a candidate, and they are different:\n\
         \n\
         **Narrowing** — the current type is right but broad, and a candidate says the same \
         thing more precisely (organization → hospital).\n\
         \n\
         **Correcting** — the current type is simply wrong, and a candidate is right. This \
         happens because extraction picks from a retrieved shortlist and can pick badly: a city \
         typed as an address, an app typed as an entry point. A correcting candidate is \
         usually a sibling of the current type rather than a narrower version of it, so do not \
         withhold it on the grounds that it is not more specific. Say plainly in the reason \
         that the current type is wrong; a person will see this one before it is applied.\n\
         \n\
         Choose null whenever any of these hold, and expect null to be a common answer:\n\
         - no candidate actually means the thing (the list is retrieved by similarity, so it \
           usually contains near-misses and sometimes contains nothing right at all);\n\
         - the current type is already right and no candidate is more precise;\n\
         - the entity is not a thing of that kind at all — a quantity, a capability, a phrase.\n\
         Keeping the current type loses nothing: the entity and its facts stay exactly as they \
         are. Picking a wrong type is worse than picking none, because it reads as a decided \
         fact.\n\
         \n\
         confidence is your own 0~1: use above 0.85 only when the candidate's definition \
         plainly describes this entity, not when it is merely the closest of a weak list.\n\
         \n\
         reason is required on every verdict, including the nulls — especially the nulls. \
         When you choose null, say which candidate came closest and what it got wrong \
         (\"nearest was publication_issue, but that is one issue of a journal, not the \
         journal\"). A refusal without a reason cannot be acted on: nobody can tell whether \
         the ontology is missing the class, or the search failed to surface it, or you read \
         the entity differently.\n\
         \n\
         {}\n\
         \n\
         id is the number in the heading, and it is what identifies the verdict — not the \
         name. Two entries can carry the same name and still be different things (two people \
         called Zhang Wei, each with their own facts); judge each one on its own block and \
         give one verdict per id you answer. Never merge two ids into one verdict.\n\
         \n\
         Output exactly one JSON object:\n\
         {{\"verdicts\":[{{\"id\":0,\"name\":\"entity name exactly as given\",\"choice\":\"candidate key or null\",\"confidence\":0.0,\"reason\":\"one short clause\"}}]}}",
        blocks.join("\n\n")
    )
}

/// The name-only query: the model's own wording, and nothing else at all.
///
/// **The reason it is sent separately is dilution.** In the context query these few words
/// come first, but the whole passage after them goes into the vector just the same, and the
/// context is usually about somebody else: the quotes for "Gongshu District, Hangzhou" are
/// about a pharmacy, so the candidates come back pharmacy and store. A short query against a
/// short label does not have this problem -- what retrieval is aiming at (the class's label)
/// is a name to begin with.
///
/// Returns `None` when neither wording is there: a query down to just the entity name is
/// identical to the opening of the context query, and sending it again wastes an embedding.
fn name_query_of(s: &utopia_store::resolution::TypeCandidateSubject) -> Option<String> {
    let parts: Vec<&str> = [s.specific_type.as_deref(), s.proposed_type.as_deref()]
        .into_iter()
        .flatten()
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .collect();
    (!parts.is_empty()).then(|| parts.join(". "))
}
