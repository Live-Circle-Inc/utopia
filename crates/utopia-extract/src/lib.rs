//! utopia-extract: LLM extraction (entities/relations/time normalisation).
//! The prompt injects the ontology types and the document's meta time; output is strict JSON;
//! an evidence quote is mandatory (no quote lowers the confidence).

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use serde::Deserialize;
use utopia_llm::ChatMessage;

#[derive(Debug, Deserialize)]
pub struct Extraction {
    #[serde(default)]
    pub entities: Vec<ExtractedEntity>,
    #[serde(default)]
    pub facts: Vec<ExtractedFact>,
    /// How many items were skipped while parsing item by item. **Must be reported to the
    /// caller** -- not reporting it is a silent drop, the same class of bug as #108 "a partial
    /// extraction reported as complete"
    #[serde(skip)]
    pub skipped_entities: usize,
    #[serde(skip)]
    pub skipped_facts: usize,
    /// The model's output was truncated; what is here was parsed after repair
    #[serde(skip)]
    pub truncated: bool,
}

#[derive(Debug, Deserialize)]
pub struct ExtractedEntity {
    pub name: String,
    #[serde(rename = "type")]
    pub type_key: String,
    /// The model's own words: what it thinks this most specifically is. **Not validated, never
    /// enters the ontology.**
    ///
    /// The reason it exists is that the list always holds something "close enough": the ontology
    /// has product, the model decides that will do and picks it, and the "vector database
    /// software" it had in mind is lost right there. Measured: the proposed_type of all 17
    /// entities came back empty, for exactly this reason -- and this name is precisely what
    /// after-the-fact resolution needs most: a short name against a short label is far closer
    /// than taking a paragraph of Chinese prose and matching it against "A software application."
    #[serde(default)]
    pub specific_type: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ExtractedFact {
    pub subject: String,
    pub predicate: String,
    /// Object entity name for a relation fact; empty for an attribute fact
    #[serde(default)]
    pub object: Option<String>,
    /// Literal value of an attribute fact (when the predicate is an attribute)
    #[serde(default)]
    pub value: Option<serde_json::Value>,
    #[serde(default)]
    pub valid_from: Option<String>,
    #[serde(default)]
    pub valid_to: Option<String>,
    #[serde(default)]
    pub confidence: Option<f32>,
    #[serde(default)]
    pub quote: Option<String>,
}

/// One relation as it appears in the prompt.
///
/// It carries one thing more than a class does: a **type signature**. It is a signature for the
/// model, not a gate -- it cuts down on "Alice works_at Seattle" at the moment the model puts pen
/// to paper, instead of waiting for the mistake and then blocking it. After-the-fact validation
/// faces a fait accompli (a shame to throw away, dirty data if kept); the signature straightens
/// things out before they are written. When the ontology is wrong the model can still override it
/// if it sees the text say otherwise; a hard gate loses data systematically, which is exactly the
/// way `part_of` burned us.
pub struct PromptRelation {
    pub key: String,
    pub label: String,
    pub description: String,
    /// Shaped like `person|organization → vendor`; `*` means that side is unconstrained. Empty
    /// string = neither side constrained.
    /// **Always keys**: what the model has to output is the key, and in a Chinese base the label
    /// of person is "人物" -- putting that in the signature teaches it to output a type that does
    /// not exist (docs/decisions/0004)
    pub signature: String,
}

/// Builds the extraction prompt. `types` holds (key, label, description) triples;
/// when description is non-empty it is listed line by line -- the semantic guidance in the
/// ontology directly determines extraction quality.
/// `attributes` holds the attribute lines pre-formatted by the caller
/// ("person.salary (number, CNY): monthly salary"); when it is empty the prompt does not change
/// by a single word -- a base that defines no attributes costs nothing.
pub fn build_messages(
    types: &[(String, String, String)],
    relations: &[PromptRelation],
    attributes: &[String],
    doc_time: Option<&str>,
    filename: &str,
    // The (type key, entity name) pairs already recorded from earlier chunks of this document,
    // ordered by first appearance. Empty for the first chunk -- there is no "earlier" yet
    known: &[(String, String)],
    chunk_text: &str,
) -> Vec<ChatMessage> {
    // **Do not send the label when there is a description.** The label is a display name for
    // humans, and it has nothing to do with the UI -- it follows the corpus language of this
    // base: in a Chinese base the label of person is "人物". In
    // `- person (人物): a specific person with a given and family name…` that "人物" carries next
    // to no information beyond the key, yet it makes the prompt jump back and forth between the
    // corpus language and the identifiers. Only when the description is empty is it used as a
    // fallback: a bare key is too thin. See docs/decisions/0004
    let fmt_list = |items: &[(String, String, String)]| {
        items
            .iter()
            .map(|(k, l, d)| {
                let d = d.trim();
                if d.is_empty() {
                    format!("- {k} ({l})")
                } else {
                    format!("- {k}: {d}")
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let type_list = fmt_list(types);
    // Relation lines: the parenthesis holds the signature when there is one, and only falls
    // back to the label when there is not.
    // `- works_at (person → organization): a person is employed by some organisation.`
    let rel_list = relations
        .iter()
        .map(|r| {
            let d = r.description.trim();
            let paren = if !r.signature.is_empty() {
                r.signature.clone()
            } else if d.is_empty() {
                r.label.clone()
            } else {
                String::new()
            };
            match (paren.is_empty(), d.is_empty()) {
                (false, false) => format!("- {} ({paren}): {d}", r.key),
                (false, true) => format!("- {} ({paren})", r.key),
                (true, false) => format!("- {}: {d}", r.key),
                (true, true) => format!("- {}", r.key),
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    // The notation is explained exactly once, and only when signatures really exist; for a base
    // without them the prompt does not change by a single word.
    // The explanation is in English -- the **instruction language** of the prompt is English,
    // only description follows the corpus
    //
    // **The signature governs two things, and they are not equally overridable.** The first
    // version mashed the two into a single sentence, "It is a hint, not a rule — when the text
    // says otherwise, write what the text says", and so the model wrote even the argument order
    // the way the text put it:
    //
    //     Elon Musk (person) --employee--> Microsoft
    //
    // whereas schema.org declares employee (organization → person). Measured over one run,
    // 102 of the 130 checkable facts landed reversed like that -- **precisely the main selling
    // point of the ontology package failing**, given that the reason for picking schema.org is
    // that "the direction is declared, not described".
    //
    // So say the two things separately:
    //
    // - **Which types may take part**: a hint, not a gate. The ontology may be wrong; if the
    //   text says Seattle, write Seattle. The judgement in 0001 does not change here -- a hard
    //   gate loses data systematically, which is exactly the way part_of burned us.
    // - **Argument order**: fixed by the signature. Order is not an assertion about the world,
    //   it is the encoding convention of this key; the text never "said a different direction",
    //   it only said that some relation holds between two entities. When it says it the other
    //   way round, swap subject and object rather than using the relation in reverse.
    let sig_note = if relations.iter().any(|r| !r.signature.is_empty()) {
        ". A parenthesis after the key is the type signature, subject then object; \
         \"|\" means or, \"*\" means unconstrained. Which kinds of things may take part \
         is a hint, not a rule — when the text says otherwise, write what the text says. \
         The order is not a hint: the signature fixes which side is the subject. If the \
         text puts them the other way round, swap subject and object so that the subject \
         matches the left side — do not reverse the relation. For example, given \
         \"employee (organization → person)\" and a text saying \"X is an employee of Y\", \
         write Y as the subject and X as the object"
    } else {
        ""
    };
    let time_ctx = doc_time
        .map(|t| {
            format!(
                "Document date: {t}. Resolve relative time expressions (e.g. \"last year\", \
                 \"this March\") to absolute dates using it as the reference."
            )
        })
        .unwrap_or_else(|| {
            "Document date unknown — only output dates explicitly written in the text.".into()
        });

    // The attribute section is injected on demand: list + output notes + value rules. It does
    // not appear at all when no attributes are defined
    let attr_section = if attributes.is_empty() {
        String::new()
    } else {
        format!(
            "\nAttributes (literal-valued fields, listed as class.attribute_key; as \"predicate\" \
             use the attribute_key alone — e.g. \"salary\", not \"person.salary\" — with a \
             \"value\" instead of \"object\"):\n{}\n",
            attributes.join("\n")
        )
    };
    let attr_rules = if attributes.is_empty() {
        String::new()
    } else {
        "\n10. Attribute facts carry \"value\" (no \"object\"): number = plain number without \
         thousands separators or unit symbols; date = \"YYYY[-MM[-DD]]\"; bool = true/false; \
         text = a short string. Only attach an attribute to a subject of its listed class. \
         valid_from = when this value took effect, if the text says so."
            .to_string()
    };
    let system = format!(
        "You are a knowledge-graph extraction engine. Extract entities and factual relations \
         from the given text. Output exactly one JSON object and nothing else.\n\
         \n\
         Entity types (prefer these keys):\n{type_list}\n\
         \n\
         Relation types (prefer these keys){sig_note}:\n{rel_list}\n\
         {attr_section}\
         \n\
         Output format:\n\
         {{\"entities\":[{{\"name\":\"entity name\",\"type\":\"type key\",\"specific_type\":\"what you would call it\"}}],\n\
          \"facts\":[{{\"subject\":\"subject entity name\",\"predicate\":\"relation key\",\"object\":\"object entity name\",\n\
                     \"valid_from\":\"2023-01\",\"valid_to\":null,\"confidence\":0.9,\"quote\":\"verbatim supporting quote\"}}]}}\n\
         \n\
         Rules:\n\
         1. Use the canonical full name as written in the text, in the text's original language; \
            list each entity once. Text introduces a full name and then shortens it — \
            \"星云科技上海研究院\" becomes \"上海研究院\", \"Nebula Technologies Inc.\" becomes \
            \"Nebula\" — and both forms mean one entity, listed once under the fuller form. \
            Two names are two entities only when the text is talking about two things.\n\
         2. Every fact's subject/object must appear in entities.\n\
         3. Dates must be \"YYYY\", \"YYYY-MM\", \"YYYY-MM-DD\", or null — never invent dates.\n\
         3a. valid_to takes a third value: \"unknown\". Use it when the text says the relation \
            has ended but does not say when — \"former CEO of X\", \"stepped down\", \"left the \
            company\", \"no longer available\", \"until recently\". Use null only for something \
            still going on. These are not interchangeable: null asserts it still holds, and \
            writing null for a relation the text says is over makes us claim the opposite of \
            the source.\n\
         4. {time_ctx}\n\
         5. quote must be a contiguous excerpt from the source text; every fact needs one.\n\
         6. confidence in 0~1: 0.9 explicitly stated, 0.7 inferred, 0.5 uncertain.\n\
         7. If nothing can be extracted, output {{\"entities\":[],\"facts\":[]}}.\n\
         8. If no listed relation fits, do not force the nearest one — write the predicate the \
            text itself uses, in snake_case (e.g. \"available_on\", \"runs_on\"). A relation \
            named after the text is worth more than a listed one that says something false.\n\
         9. The same holds for entity types: if none of the listed types fits, write the type \
            the text implies, in snake_case (e.g. \"model\", \"technology\"). Do not fall back \
            to a broad listed type such as \"thing\" or \"creative_work\" merely because \
            nothing specific matched — that hides the gap instead of reporting it.\n\
         10. specific_type is required on every entity and is never checked against the list. \
            Name the most specific kind the thing is, in the words you would use for it. Write \
            it even when \"type\" already fits, and make it narrower than \"type\" wherever the \
            text supports it — type \"product\", specific_type \"vector database software\". \
            Repeat the listed type only when the text genuinely says nothing more precise.\
         {attr_rules}"
    );

    // Known entities sit right up against the text: compliance depends on position, see the
    // comment on known_block for why
    let user = format!(
        "Source file: \"{filename}\"\n{}\nText:\n{chunk_text}",
        known_block(known)
    );

    vec![
        ChatMessage {
            role: "system".into(),
            content: system,
        },
        ChatMessage {
            role: "user".into(),
            content: user,
        },
    ]
}

/// Character budget in the prompt for entities that have already appeared in this document.
///
/// Anything beyond it is truncated (the ones that appeared first are kept). Chinese business
/// prose gives the full name first and brings the protagonist on first, so **first-appearance
/// order naturally favours exactly those names that get shortened later on**.
const KNOWN_BUDGET_CHARS: usize = 1200;

/// Formats "the entities already recorded in this document" into a block of the prompt. Returns
/// an empty string when there are none.
///
/// **Why it goes before the text, with the instruction glued to the list**: an abstract rule
/// cannot beat the concrete block sitting next to it -- with the ontology suggestion, the
/// language requirement lost out to the English JSON skeleton that immediately followed it, and
/// only took effect once moved after the skeleton and made to name it. Compliance depends on
/// position, so the instruction goes next to the data it governs, and the two of them together
/// go next to the text.
///
/// **And one rule in passing that has nothing to do with which message it lands in: anything
/// that changes per chunk goes last.** Prefix caching matches a token prefix, and the messages
/// are concatenated system→user, so "the end of system" and "the start of user" are all but
/// equivalent; what really shatters the cache is wedging it in the **middle** (after the
/// ontology, before the rules), which pushes the rules out of the prefix. The cache itself is
/// not ours to manage -- whether the vendor turns it on and whether it reports it are both up
/// to them, and this deployment measures `cached=0` -- we are only responsible for not breaking
/// it. A self-hosted vLLM has automatic prefix caching on by default, and what that saves is
/// compute, not money.
fn known_block(known: &[(String, String)]) -> String {
    if known.is_empty() {
        return String::new();
    }
    // Grouped by type: more compact, and it incidentally holds down cross-chunk type drift
    // (the same "Canghai" being a product in one chunk and a project in another)
    let mut by_type: Vec<(&str, Vec<&str>)> = Vec::new();
    let mut used = 0usize;
    for (type_key, name) in known {
        used += name.chars().count() + 2;
        if used > KNOWN_BUDGET_CHARS {
            break;
        }
        match by_type.iter_mut().find(|(k, _)| *k == type_key.as_str()) {
            Some((_, names)) => names.push(name),
            None => by_type.push((type_key, vec![name])),
        }
    }
    if by_type.is_empty() {
        return String::new();
    }
    let lines = by_type
        .iter()
        .map(|(k, names)| format!("  {k}: {}", names.join(", ")))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "\nAlready recorded from earlier parts of this same document:\n{lines}\n\
         \n\
         If something in the text below refers to one of these, write that exact string as \
         the name, and give it that same type — documents abbreviate after first mention \
         (\"星云科技上海研究院\" later becomes \"上海研究院\"), and the shortened form must \
         not become a second entity. If it is a different thing, name it as the text does; \
         do not force it onto this list.\n"
    )
}

/// Robustly pulls the JSON block out of an LLM reply (tolerates code fences and waffle before
/// and after it).
pub fn json_block(raw: &str) -> anyhow::Result<String> {
    let text = raw.trim();
    let cleaned = text
        .strip_prefix("```json")
        .or_else(|| text.strip_prefix("```"))
        .map(|s| s.trim_end_matches("```"))
        .unwrap_or(text);
    let start = cleaned.find('{');
    let end = cleaned.rfind('}');
    match (start, end) {
        (Some(s), Some(e)) if e > s => Ok(cleaned[s..=e].to_string()),
        _ => anyhow::bail!("No JSON found in LLM reply"),
    }
}

/// Closes the brackets missing after head. Brackets inside a string literal do not count --
/// `"a[b"` is not an opening bracket.
///
/// Returning None = the structure itself is wrong (too many brackets already, say), not "cut off
/// before it was finished".
fn close_brackets(head: &str) -> Option<String> {
    let mut stack: Vec<char> = Vec::new();
    let (mut in_str, mut esc) = (false, false);
    for c in head.chars() {
        if in_str {
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '[' | '{' => stack.push(c),
            // Not written as two guarded arms: that hides the side effect of stack.pop() inside
            // the guard -- it happens to be correct, but a reader does not expect a guard to
            // change state
            ']' | '}' => {
                let want = if c == ']' { '[' } else { '{' };
                if stack.pop() != Some(want) {
                    return None;
                }
            }
            _ => {}
        }
    }
    if in_str {
        return None; // cut off in the middle of a string, this slice is unusable
    }
    let mut out = String::from(head);
    for c in stack.iter().rev() {
        out.push(if *c == '[' { ']' } else { '}' });
    }
    Some(out)
}

/// When the output is truncated, back off to the end of the **last complete object** and then
/// close the brackets.
///
/// When the model stops halfway (having hit max_tokens), the objects before that point are
/// complete and correct. Voiding the whole chunk means throwing away the dozen-odd facts that
/// were already extracted correctly -- measured, 4 of 246 calls were this case.
fn repair_truncated(json: &str) -> Option<String> {
    let mut cut = json.len();
    for _ in 0..64 {
        let idx = json[..cut].rfind('}')?;
        if let Some(closed) = close_brackets(&json[..=idx]) {
            if serde_json::from_str::<serde_json::Value>(&closed).is_ok() {
                return Some(closed);
            }
        }
        cut = idx;
    }
    None
}

/// **One bad record must not destroy a whole chunk.**
///
/// This used to be `serde_json::from_str::<Extraction>` -- all or nothing. One object missing its
/// `predicate`, or one truncated output, and the chunk's entities and facts were voided together,
/// while a chunk often holds twenty good facts. Measured, 5 of 246 calls were lost this way (2%),
/// and it also failed the whole `extract_document` task, sent it through retries, and after three
/// of those marked the document failed.
///
/// Now: parse into a `Value` first (closing the brackets first if it is truncated), then
/// `from_value` item by item, keeping the good ones and counting the bad. **The count must be
/// passed outwards** -- skipping silently is just another flavour of "reported as complete".
pub fn parse_response(raw: &str) -> anyhow::Result<Extraction> {
    let json_str = json_block(raw)?;
    let (value, truncated) = match serde_json::from_str::<serde_json::Value>(&json_str) {
        Ok(v) => (v, false),
        Err(e) => match repair_truncated(&json_str) {
            Some(fixed) => (
                serde_json::from_str::<serde_json::Value>(&fixed)
                    .map_err(|e| anyhow::anyhow!("Failed to parse extraction JSON: {e}"))?,
                true,
            ),
            // Only a reply beyond repair is a real parse failure: not even one complete object
            None => anyhow::bail!("Failed to parse extraction JSON: {e}"),
        },
    };

    fn take<T: serde::de::DeserializeOwned>(
        value: &serde_json::Value,
        key: &str,
    ) -> (Vec<T>, usize) {
        let Some(arr) = value.get(key).and_then(|v| v.as_array()) else {
            return (Vec::new(), 0);
        };
        let mut out = Vec::with_capacity(arr.len());
        let mut skipped = 0;
        for item in arr {
            match serde_json::from_value::<T>(item.clone()) {
                Ok(v) => out.push(v),
                Err(_) => skipped += 1,
            }
        }
        (out, skipped)
    }

    let (entities, skipped_entities) = take::<ExtractedEntity>(&value, "entities");
    let (facts, skipped_facts) = take::<ExtractedFact>(&value, "facts");
    Ok(Extraction {
        entities,
        facts,
        skipped_entities,
        skipped_facts,
        truncated,
    })
}

// ---------------------------------------------------------------------------
// Entity-resolution adjudication (batched: one call judges many pairs, the LLM only handles the
// grey zone that embeddings cannot separate)
// ---------------------------------------------------------------------------

/// One side of a pair awaiting adjudication: name + type + fact summary lines.
pub struct AdjudicationSide {
    pub name: String,
    pub type_label: String,
    pub facts: Vec<String>,
}

pub struct AdjudicationPair {
    pub left: AdjudicationSide,
    pub right: AdjudicationSide,
}

#[derive(Debug, Deserialize)]
pub struct AdjudicationVerdict {
    pub i: usize,
    pub verdict: String,
    #[serde(default)]
    pub confidence: Option<f32>,
}

#[derive(Debug, Deserialize)]
struct AdjudicationReply {
    #[serde(default)]
    verdicts: Vec<AdjudicationVerdict>,
}

/// Builds the batched adjudication prompt. Biased conservative: answer unsure when the evidence
/// is not enough (rather split than merge; merging needs evidence).
pub fn build_adjudication_messages(pairs: &[AdjudicationPair]) -> Vec<ChatMessage> {
    let system = "You are an entity-resolution adjudicator for a knowledge graph. \
        For each numbered pair, decide whether the two records refer to the SAME real-world \
        entity or are namesakes (different entities that share a name).\n\
        \n\
        Judge by the facts attached to each record: employer/affiliation, role, time ranges, \
        and connected entities. Identical names alone are NEVER sufficient evidence of sameness. \
        Contradictory affiliations in overlapping time periods indicate different entities \
        (but people do change jobs — non-overlapping periods can belong to one person).\n\
        \n\
        One name containing the other is a different case, and the rule above does not apply \
        to it: \"星云科技上海研究院\" against \"上海研究院\", \"Nebula Technologies Inc.\" \
        against \"Nebula\". Documents drop the qualifier after first mention, so the shorter \
        form is usually the longer one abbreviated — treat the containment as evidence FOR \
        sameness and let the facts settle it. Shared people, parent or location confirm one \
        entity; a different parent or conflicting leadership means the shorter name belongs \
        to something else.\n\
        Abbreviation removes a qualifier from the FRONT. It never adds a noun or a \
        prepositional phrase at the end, so those are different entities however much text \
        they share: \"the operator library for the Canghai Platform\" is not the Canghai \
        Platform, \"Qiming X7 programme\" is not the Qiming X7, and \"沧海平台项目\" is not \
        \"沧海平台\" — a project, a programme, a team or a component is its own record.\n\
        \n\
        Output exactly one JSON object and nothing else:\n\
        {\"verdicts\":[{\"i\":0,\"verdict\":\"same|different|unsure\",\"confidence\":0.9}]}\n\
        \n\
        Rules:\n\
        1. One verdict per pair, using the pair's number as \"i\".\n\
        2. confidence in 0~1.\n\
        3. Be conservative: if the evidence is insufficient to decide, answer \"unsure\" — \
           a wrong merge is far more damaging than leaving two records separate."
        .to_string();

    let mut user = String::new();
    for (i, p) in pairs.iter().enumerate() {
        let fmt = |s: &AdjudicationSide| {
            let facts = if s.facts.is_empty() {
                "  (no recorded facts)".to_string()
            } else {
                s.facts
                    .iter()
                    .map(|f| format!("  - {f}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            format!("\"{}\" ({})\n{}", s.name, s.type_label, facts)
        };
        user.push_str(&format!(
            "Pair {i}:\nRecord A: {}\nRecord B: {}\n\n",
            fmt(&p.left),
            fmt(&p.right)
        ));
    }

    vec![
        ChatMessage {
            role: "system".into(),
            content: system,
        },
        ChatMessage {
            role: "user".into(),
            content: user,
        },
    ]
}

pub fn parse_adjudication(raw: &str) -> anyhow::Result<Vec<AdjudicationVerdict>> {
    let json_str = json_block(raw)?;
    let reply: AdjudicationReply = serde_json::from_str(&json_str)
        .map_err(|e| anyhow::anyhow!("Failed to parse adjudication JSON: {e}"))?;
    Ok(reply.verdicts)
}

/// Normalises an attribute value by its datatype. Returns None on failure -- rather missing than
/// dirty; the caller skips it and logs.
/// number tolerates thousands separators/spaces; date requires YYYY[-MM[-DD]] and keeps the
/// original precision; bool is lenient about yes/no.
pub fn normalize_attr_value(datatype: &str, raw: &serde_json::Value) -> Option<serde_json::Value> {
    match datatype {
        "number" => match raw {
            serde_json::Value::Number(n) => Some(serde_json::Value::Number(n.clone())),
            serde_json::Value::String(s) => {
                let cleaned: String = s
                    .chars()
                    .filter(|c| !matches!(c, ',' | ' ' | '_'))
                    .collect();
                cleaned
                    .parse::<f64>()
                    .ok()
                    .filter(|f| f.is_finite())
                    .and_then(serde_json::Number::from_f64)
                    .map(serde_json::Value::Number)
            }
            _ => None,
        },
        "date" => {
            let s = raw.as_str()?.trim();
            parse_time(s).map(|_| serde_json::Value::String(s.to_string()))
        }
        "bool" => match raw {
            serde_json::Value::Bool(b) => Some(serde_json::Value::Bool(*b)),
            serde_json::Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
                "true" | "yes" | "是" => Some(serde_json::Value::Bool(true)),
                "false" | "no" | "否" => Some(serde_json::Value::Bool(false)),
                _ => None,
            },
            _ => None,
        },
        _ => {
            let s = match raw {
                serde_json::Value::String(s) => s.trim().to_string(),
                serde_json::Value::Number(n) => n.to_string(),
                _ => return None,
            };
            (!s.is_empty()).then(|| serde_json::Value::String(s.chars().take(500).collect()))
        }
    }
}

/// Parses a time string → (UTC time, precision). Supports YYYY / YYYY-MM / YYYY-MM-DD.
pub fn parse_time(s: &str) -> Option<(DateTime<Utc>, &'static str)> {
    let s = s.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("null") {
        return None;
    }
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Some((Utc.from_utc_datetime(&d.and_hms_opt(0, 0, 0)?), "day"));
    }
    if let Ok(d) = NaiveDate::parse_from_str(&format!("{s}-01"), "%Y-%m-%d") {
        return Some((Utc.from_utc_datetime(&d.and_hms_opt(0, 0, 0)?), "month"));
    }
    if s.len() == 4 {
        if let Ok(year) = s.parse::<i32>() {
            let d = NaiveDate::from_ymd_opt(year, 1, 1)?;
            return Some((Utc.from_utc_datetime(&d.and_hms_opt(0, 0, 0)?), "year"));
        }
    }
    None
}

#[cfg(test)]
mod prompt_shape_tests {
    use super::*;

    fn rel(key: &str, description: &str, signature: &str) -> PromptRelation {
        PromptRelation {
            key: key.into(),
            label: key.replace('_', " "),
            description: description.into(),
            signature: signature.into(),
        }
    }

    /// The signature goes in the parenthesis, and it is **always keys**: a Chinese base's label
    /// is "人物", and writing that into the prompt teaches the model to output a type that does
    /// not exist.
    #[test]
    fn a_signature_takes_the_parenthesis_and_uses_keys() {
        let rels = vec![rel("works_at", "受雇于某个组织。", "person → organization")];
        let msgs = build_messages(&[], &rels, &[], None, "a.txt", &[], "text");
        assert!(msgs[0]
            .content
            .contains("- works_at (person → organization): 受雇于某个组织。"));
    }

    /// Several values join with `|`, an empty side is `*` -- both are notation at the key level,
    /// not type names
    #[test]
    fn several_classes_join_with_a_pipe_and_an_empty_side_is_a_star() {
        let rels = vec![rel("buys_from", "", "employee|team → *")];
        let msgs = build_messages(&[], &rels, &[], None, "a.txt", &[], "text");
        assert!(msgs[0].content.contains("- buys_from (employee|team → *)"));
    }

    /// **For a base without signatures the prompt does not change by a single word**: the
    /// notation note does not appear either. Most bases never declare domain/range and should
    /// not pay per-chunk tokens for it
    #[test]
    fn a_base_without_signatures_pays_nothing() {
        let rels = vec![rel("works_at", "受雇于某个组织。", "")];
        let msgs = build_messages(&[], &rels, &[], None, "a.txt", &[], "text");
        assert!(msgs[0].content.contains("- works_at: 受雇于某个组织。"));
        assert!(!msgs[0].content.contains("type signature"));
        assert!(!msgs[0].content.contains('→'));
    }

    /// The signature is a hint, not a gate. That sentence has to be in the prompt -- without it
    /// the model treats the signature as a hard rule, and a wrong ontology then loses data
    /// systematically (the part_of way)
    #[test]
    fn the_prompt_says_the_signature_is_a_hint() {
        let rels = vec![rel("works_at", "d", "person → organization")];
        let msgs = build_messages(&[], &rels, &[], None, "a.txt", &[], "text");
        assert!(msgs[0].content.contains("hint, not a rule"));
    }

    /// **But the order is not a hint.**
    ///
    /// Both sentences have to be present; drop either one and an old ailment comes back. Without
    /// "a hint, not a gate", a wrong ontology loses data systematically (the part_of way);
    /// without "the signature fixes the order", the model follows its English intuition and
    /// writes `Musk --employee--> Microsoft`, whereas schema.org declares
    /// `employee (organization → person)` -- measured over one run, 102 of the 130 checkable
    /// facts landed reversed like that.
    #[test]
    fn the_prompt_says_the_order_is_not_a_hint() {
        let rels = vec![rel("employee", "d", "organization → person")];
        let msgs = build_messages(&[], &rels, &[], None, "a.txt", &[], "text");
        let c = &msgs[0].content;
        assert!(
            c.contains("hint, not a rule"),
            "the sentence about types is gone"
        );
        assert!(
            c.contains("The order is not a hint"),
            "the sentence about order is gone"
        );
        assert!(
            c.contains("swap subject and object"),
            "only says the order matters, not what to do when the text says it the other way round"
        );
        assert!(
            c.contains("do not reverse the relation"),
            "without this the model may seek a reverse relation instead of swapping subject/object"
        );
    }

    /// Known entities must land in the **user** message, right up against the text.
    ///
    /// The reason is compliance, not caching: an abstract rule cannot beat the concrete block
    /// sitting next to it. Put the list in the rules area of system and it ends up behind the
    /// output format, the ten rules and the filename -- as far as it gets from the text it is
    /// supposed to govern.
    #[test]
    fn known_entities_stay_out_of_the_system_message() {
        // Use a name that does not occur in the example in rule 1: rule 1 also mentions
        // "星云科技上海研究院", so asserting on that would not tell us which message the list
        // actually landed in
        let known = vec![(
            "organization".to_string(),
            "华瑞集团智能制造研究院".to_string(),
        )];
        let msgs = build_messages(&[], &[], &[], None, "a.txt", &known, "text");
        assert_eq!(msgs[0].role, "system");
        assert!(!msgs[0].content.contains("Already recorded"));
        assert!(!msgs[0].content.contains("华瑞集团智能制造研究院"));
        assert!(msgs[1]
            .content
            .contains("organization: 华瑞集团智能制造研究院"));
    }

    /// The first chunk has no "earlier", so that section should not appear at all -- zero cost,
    /// rather than an empty heading
    #[test]
    fn the_first_chunk_carries_no_block() {
        let msgs = build_messages(&[], &[], &[], None, "a.txt", &[], "text");
        assert!(!msgs[1].content.contains("Already recorded"));
    }

    /// The counter-guardrail has to be there: give something a reference list and it will force
    /// matches onto it (the lesson of that `concept` episode)
    #[test]
    fn the_block_tells_the_model_not_to_force_a_match() {
        let known = vec![("person".to_string(), "陈立".to_string())];
        let msgs = build_messages(&[], &[], &[], None, "a.txt", &known, "text");
        assert!(msgs[1].content.contains("do not force it onto this list"));
    }

    /// Drop the label when there is a description -- a Chinese base's label is Chinese, and
    /// mixing it into the prompt only makes the identifiers and the corpus language jump back
    /// and forth, while it carries next to no information beyond the key.
    #[test]
    fn described_types_drop_the_label() {
        let types = vec![
            (
                "person".into(),
                "人物".into(),
                "有名有姓的具体的人。".into(),
            ),
            ("event".into(), "事件".into(), String::new()),
        ];
        let msgs = build_messages(&types, &[], &[], None, "a.txt", &[], "text");
        let prompt = format!("{:?}", msgs);
        assert!(prompt.contains("- person: 有名有姓的具体的人。"));
        assert!(!prompt.contains("person (人物)"));
        // With an empty description the label is still the only extra clue, so keep it
        assert!(prompt.contains("- event (事件)"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_time_precisions() {
        assert_eq!(parse_time("2024").unwrap().1, "year");
        assert_eq!(parse_time("2024-07").unwrap().1, "month");
        assert_eq!(parse_time("2024-07-15").unwrap().1, "day");
        assert!(parse_time("null").is_none());
        assert!(parse_time("").is_none());
        assert!(parse_time("下个月").is_none());
    }

    #[test]
    fn parse_adjudication_reply() {
        let raw = "```json\n{\"verdicts\":[{\"i\":0,\"verdict\":\"same\",\"confidence\":0.92},{\"i\":1,\"verdict\":\"unsure\"}]}\n```";
        let v = parse_adjudication(raw).unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].verdict, "same");
        assert_eq!(v[1].confidence, None);
    }

    #[test]
    fn normalize_attr_values() {
        use serde_json::json;
        assert_eq!(
            normalize_attr_value("number", &json!("35,000")),
            Some(json!(35000.0))
        );
        assert_eq!(normalize_attr_value("number", &json!(42)), Some(json!(42)));
        assert_eq!(normalize_attr_value("number", &json!("about ten")), None);
        assert_eq!(
            normalize_attr_value("date", &json!("2024-07")),
            Some(json!("2024-07"))
        );
        assert_eq!(normalize_attr_value("date", &json!("下个月")), None);
        assert_eq!(
            normalize_attr_value("bool", &json!("yes")),
            Some(json!(true))
        );
        assert_eq!(
            normalize_attr_value("text", &json!(" CTO ")),
            Some(json!("CTO"))
        );
        assert_eq!(normalize_attr_value("text", &json!([1])), None);
    }

    /// **One bad record must not destroy a whole chunk.**
    ///
    /// The shape is taken from a real log: `missing field \`predicate\``. The model occasionally
    /// leaves this field out (once `related_to` had bowed out it has no catch-all option left to
    /// pick), and serde used to void the whole chunk -- while the other two facts in this one
    /// are good.
    #[test]
    fn one_malformed_fact_does_not_take_the_whole_chunk() {
        let raw = r#"{
          "entities": [{"name": "OpenAI", "type": "organization"}],
          "facts": [
            {"subject": "OpenAI", "predicate": "produces", "object": "GPT-4"},
            {"subject": "OpenAI", "object": "ChatGPT"},
            {"subject": "Sam Altman", "predicate": "leads", "object": "OpenAI"}
          ]
        }"#;
        let x = parse_response(raw).unwrap();
        assert_eq!(x.facts.len(), 2, "the two good ones should stay");
        assert_eq!(
            x.skipped_facts, 1,
            "the skipped one has to be reported, never dropped silently"
        );
        assert_eq!(x.entities.len(), 1);
        assert!(!x.truncated);
    }

    /// **When the output is truncated, the parts that were already complete must be salvaged.**
    ///
    /// On hitting max_tokens the model just stops halfway (real log: `EOF while parsing a list`).
    /// The objects before that are complete and correct; voiding the whole chunk means throwing
    /// away the dozen-odd correctly extracted ones with them.
    #[test]
    fn a_cut_off_reply_keeps_what_was_complete() {
        let raw = r#"{
          "entities": [{"name": "Anthropic", "type": "organization"}],
          "facts": [
            {"subject": "Anthropic", "predicate": "produces", "object": "Claude"},
            {"subject": "Dario Amodei", "predicate": "leads", "object": "Anthropic"},
            {"subject": "Anthropic", "predicate": "loca"#;
        let x = parse_response(raw).unwrap();
        assert!(x.truncated, "truncation has to be flagged");
        assert_eq!(x.facts.len(), 2, "the two before the cut-off are complete");
        assert_eq!(x.entities.len(), 1);
    }

    /// A bracket inside a string is not structure -- `"a[b"` is not an opening bracket.
    #[test]
    fn brackets_inside_strings_are_not_structure() {
        let raw =
            r#"{"entities": [], "facts": [{"subject": "a[b{c", "predicate": "p", "object": "o"}]}"#;
        let x = parse_response(raw).unwrap();
        assert_eq!(x.facts.len(), 1);
        assert!(
            !x.truncated,
            "the structure is complete, this must not be judged truncated"
        );
    }

    /// With not even one complete object it must still report failure -- **fault tolerance is
    /// not calling an empty result a success**.
    #[test]
    fn a_reply_with_nothing_complete_still_fails() {
        assert!(parse_response(r#"{"facts": [{"subject": "a"#).is_err());
    }

    #[test]
    fn parse_response_with_fence() {
        let raw = "好的，结果如下：\n```json\n{\"entities\":[{\"name\":\"张三\",\"type\":\"person\"}],\"facts\":[]}\n```";
        let e = parse_response(raw).unwrap();
        assert_eq!(e.entities.len(), 1);
        assert_eq!(e.entities[0].type_key, "person");
    }

    /// specific_type is in the skeleton and in the rules, and both places say "always fill it
    /// in".
    ///
    /// Putting it in the skeleton alone is not enough: **when the rules and the skeleton
    /// conflict, the skeleton wins** (the rule about language fell over on this once). Here the
    /// two agree, so pin them down together.
    #[test]
    fn every_entity_is_asked_for_its_own_words() {
        let msgs = build_messages(&[], &[], &[], None, "a.txt", &[], "text");
        let sys = &msgs[0].content;
        assert!(sys.contains("\"specific_type\":\"what you would call it\""));
        assert!(sys.contains("required on every entity"));
        // The crucial sentence: not validated. Validating it would just be one more vocabulary
        // list
        assert!(sys.contains("never checked against the list"));
        // The relation to type has to be spelled out, or the model just copies the coarse type
        assert!(sys.contains("narrower than"));
    }
}
