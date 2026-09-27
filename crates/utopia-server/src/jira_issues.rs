//! Jira issue source: one issue = one document, with a **field-level change history** in the
//! body.
//!
//! Same call as [`crate::github_issues`] (do not fetch the present, fetch the changes), but the
//! raw material Jira hands over is stronger and cheaper to fetch:
//!
//! ## One call is enough
//!
//! On the GitHub side it takes three fetches, and the events are forced to go issue by issue
//! (`issues/events` does not support `since`, and gets drowned in PR events). Jira's `search`
//! brings back everything in one go:
//!
//! ```text
//! GET /rest/api/2/search?jql=…&expand=changelog&fields=…,comment
//! ```
//!
//! The issue itself, the complete change history and the comments all come back together, with
//! **no N+1**.
//!
//! ## The change history is field-level from → to
//!
//! GitHub's events only say "a labeled happened"; Jira says outright "which field went from what
//! to what":
//!
//! ```text
//! 2026-08-24  Mickael Maison  Version: (empty) → 4.1.0
//! 2026-08-24  Luke Chen       status: Patch Available → Resolved
//! ```
//!
//! For the ledger this is better raw material -- `from`/`to` are themselves the two ends of one
//! change in understanding.
//!
//! ## Incrementality comes from JQL, not from a since parameter
//!
//! Jira has no `since`, but JQL can express it: `updated >= "2026-08-30 12:00"`. The time format
//! has to be the one Jira accepts (not RFC3339), and it **has to be quoted** -- get either of
//! those wrong and the symptom is a 400, not "nothing found".
//!
//! ## The difference between Server/DC and Cloud
//!
//! This module is written against **API v2** (Jira Server/DC). Cloud's v3 replaced `description`
//! and the comment bodies with ADF (a JSON tree rather than a string) -- that needs a renderer,
//! which is a different job. v2 is usually still available on Cloud and returns strings, so only
//! v2 for now; we will fill in the rest when we actually meet an instance that only has v3,
//! because that is when we will know which ADF nodes have to be handled.

use chrono::{DateTime, Utc};
use serde::Deserialize;

/// How many pages at most one sync turns. Jira's `total` is routinely in the tens of thousands,
/// and pulling all of it back is pointless -- whatever falls outside the incremental window can
/// wait for the next JQL fetch.
const MAX_PAGES: u32 = 10;
const PAGE_SIZE: u32 = 50;

/// The fields we want. **They have to be listed explicitly**: leave `comment` out and no
/// comments come back, while the default of returning every field bloats the response to
/// hundreds of KB per issue.
const FIELDS: &str = "summary,status,issuetype,priority,created,updated,resolutiondate,\
                      labels,assignee,reporter,description,comment";

#[derive(Debug, Deserialize)]
pub struct SearchPage {
    #[serde(default)]
    pub issues: Vec<Issue>,
    #[serde(default)]
    pub total: i64,
}

#[derive(Debug, Deserialize)]
pub struct Issue {
    pub key: String,
    pub fields: Fields,
    /// Only present with `expand=changelog`
    #[serde(default)]
    pub changelog: Option<Changelog>,
}

#[derive(Debug, Deserialize)]
pub struct Fields {
    pub summary: Option<String>,
    pub description: Option<String>,
    pub created: Option<JiraTime>,
    pub updated: Option<JiraTime>,
    pub resolutiondate: Option<JiraTime>,
    pub status: Option<Named>,
    pub issuetype: Option<Named>,
    pub priority: Option<Named>,
    pub assignee: Option<User>,
    pub reporter: Option<User>,
    #[serde(default)]
    pub labels: Vec<String>,
    pub comment: Option<Comments>,
}

/// Jira's timestamps look like `2026-08-24T11:11:52.944+0000` -- **a timezone offset without a
/// colon**, which is not RFC3339. chrono's `DateTime<Utc>` cannot parse it by default.
#[derive(Debug, Clone, Copy)]
pub struct JiraTime(pub DateTime<Utc>);

impl<'de> Deserialize<'de> for JiraTime {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        DateTime::parse_from_str(&s, "%Y-%m-%dT%H:%M:%S%.3f%z")
            .or_else(|_| DateTime::parse_from_rfc3339(&s))
            .map(|t| JiraTime(t.with_timezone(&Utc)))
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Deserialize)]
pub struct Named {
    pub name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct User {
    #[serde(alias = "displayName")]
    pub display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Comments {
    #[serde(default)]
    pub comments: Vec<Comment>,
}

#[derive(Debug, Deserialize)]
pub struct Comment {
    pub author: Option<User>,
    pub created: Option<JiraTime>,
    pub body: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Changelog {
    #[serde(default)]
    pub histories: Vec<History>,
}

#[derive(Debug, Deserialize)]
pub struct History {
    pub created: Option<JiraTime>,
    pub author: Option<User>,
    #[serde(default)]
    pub items: Vec<ChangeItem>,
}

#[derive(Debug, Deserialize)]
pub struct ChangeItem {
    pub field: Option<String>,
    #[serde(alias = "fromString")]
    pub from_string: Option<String>,
    #[serde(alias = "toString")]
    pub to_string: Option<String>,
}

fn name(n: &Option<Named>) -> Option<&str> {
    n.as_ref()?.name.as_deref()
}
fn who(u: &Option<User>) -> &str {
    u.as_ref()
        .and_then(|x| x.display_name.as_deref())
        .unwrap_or("?")
}

/// Lay one issue out as a document. **A pure function, no network** -- fetching and arranging
/// are kept apart so that the arranging half can actually be tested.
pub fn render(issue: &Issue) -> String {
    let f = &issue.fields;
    let mut out = String::new();
    out.push_str(&format!(
        "# {} {}\n\n",
        issue.key,
        f.summary.as_deref().unwrap_or("")
    ));

    // The header is written as dated sentences and not as key-value pairs: the extractor reads
    // sentences, and "Reported by X on 2026-08-24" yields a fact with a valid_from
    if let (Some(r), Some(c)) = (&f.reporter, &f.created) {
        out.push_str(&format!(
            "Reported by {} on {}.\n",
            who(&Some(User {
                display_name: r.display_name.clone()
            })),
            c.0.format("%Y-%m-%d")
        ));
    }
    if let Some(t) = name(&f.issuetype) {
        out.push_str(&format!("Type {t}.\n"));
    }
    if let Some(s) = name(&f.status) {
        out.push_str(&format!("Currently {s}.\n"));
    }
    if let Some(p) = name(&f.priority) {
        out.push_str(&format!("Priority {p}.\n"));
    }
    if let Some(a) = &f.assignee {
        out.push_str(&format!(
            "Assigned to {}.\n",
            a.display_name.as_deref().unwrap_or("?")
        ));
    }
    if let Some(r) = &f.resolutiondate {
        out.push_str(&format!("Resolved on {}.\n", r.0.format("%Y-%m-%d")));
    }
    if !f.labels.is_empty() {
        out.push_str(&format!("Labelled {}.\n", f.labels.join(", ")));
    }

    if let Some(d) = f
        .description
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty())
    {
        out.push_str("\n## Description\n\n");
        out.push_str(d);
        out.push('\n');
    }

    // **Field-level change history.** This is exactly what Jira gives beyond GitHub: not just
    // "what event happened" but "which field went from what to what" -- from/to are themselves
    // the two ends of one change in understanding
    let mut lines: Vec<(DateTime<Utc>, String)> = Vec::new();
    for h in issue.changelog.iter().flat_map(|c| c.histories.iter()) {
        let Some(at) = h.created else { continue };
        for item in &h.items {
            let Some(field) = item.field.as_deref() else {
                continue;
            };
            let from = item.from_string.as_deref().unwrap_or("(empty)");
            let to = item.to_string.as_deref().unwrap_or("(empty)");
            lines.push((
                at.0,
                format!(
                    "- {} — {} changed {field}: {from} → {to}\n",
                    at.0.format("%Y-%m-%d"),
                    who(&h.author),
                ),
            ));
        }
    }
    if !lines.is_empty() {
        // The endpoint's ordering is not a contract, so we sort ourselves -- get it wrong and
        // the history gets told backwards
        lines.sort_by_key(|(t, _)| *t);
        out.push_str("\n## History\n\n");
        for (_, l) in &lines {
            out.push_str(l);
        }
    }

    let comments: Vec<&Comment> = f
        .comment
        .as_ref()
        .map(|c| c.comments.iter().collect())
        .unwrap_or_default();
    if !comments.is_empty() {
        out.push_str("\n## Comments\n\n");
        for c in comments {
            let body = c.body.as_deref().unwrap_or("").trim();
            if body.is_empty() {
                continue;
            }
            let at = c
                .created
                .map(|t| t.0.format("%Y-%m-%d").to_string())
                .unwrap_or_else(|| "?".into());
            out.push_str(&format!("### {} on {}\n\n{}\n\n", who(&c.author), at, body));
        }
    }
    out
}

/// The JQL used for incrementality. **The time format is Jira's own** (`yyyy-MM-dd HH:mm`), not
/// RFC3339, and it has to be quoted -- get either wrong and the symptom is a 400, not "nothing
/// found".
pub fn jql(project: &str, since: Option<DateTime<Utc>>) -> String {
    let mut q = format!("project = {project}");
    if let Some(t) = since {
        q.push_str(&format!(
            " AND updated >= \"{}\"",
            t.format("%Y-%m-%d %H:%M")
        ));
    }
    q.push_str(" ORDER BY updated ASC");
    q
}

/// Fetch page by page. Jira uses `startAt`/`maxResults`, and `total` is routinely in the tens of
/// thousands -- MAX_PAGES caps it, and the rest waits for the next incremental window.
///
/// **Return `total` along with it**: if we truncated, we have to say so. When 500 issues come
/// back and the server has 14506, "sync complete" in the UI is a misleading sentence -- the truth
/// is "this round only covered a small slice".
pub async fn fetch_all(
    http: &reqwest::Client,
    base_url: &str,
    jql: &str,
    auth: Option<&str>,
) -> anyhow::Result<(Vec<Issue>, i64)> {
    let mut out = Vec::new();
    let mut total = 0i64;
    for page in 0..MAX_PAGES {
        let mut url = reqwest::Url::parse(&format!(
            "{}/rest/api/2/search",
            base_url.trim_end_matches('/')
        ))?;
        {
            let mut q = url.query_pairs_mut();
            q.append_pair("jql", jql);
            q.append_pair("expand", "changelog");
            q.append_pair("fields", FIELDS);
            q.append_pair("maxResults", &PAGE_SIZE.to_string());
            q.append_pair("startAt", &(page * PAGE_SIZE).to_string());
        }
        let mut req = http
            .get(url)
            .header(reqwest::header::ACCEPT, "application/json");
        if let Some(a) = auth {
            req = req.header(reqwest::header::AUTHORIZATION, a);
        }
        let resp = req.send().await?;
        let status = resp.status();
        if !status.is_success() {
            // Jira reports JQL syntax errors as a 400 too, and the reason is only in the body.
            // Saying just "HTTP 400" sends people off to check the network, when what actually
            // needs fixing is the project key or the time format
            let detail = resp.text().await.unwrap_or_default();
            anyhow::bail!(
                "HTTP {status} from Jira: {}",
                detail.chars().take(200).collect::<String>()
            );
        }
        let page_data: SearchPage = resp.json().await?;
        total = page_data.total;
        let n = page_data.issues.len();
        out.extend(page_data.issues);
        if n < PAGE_SIZE as usize {
            break;
        }
    }
    Ok((out, total))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **Jira's timestamps are not RFC3339.** `+0000` has no colon, and chrono's default
    /// implementation cannot parse it. This is the spot in the whole module most likely to break
    /// silently: fail to parse and a whole page of issues is lost.
    #[test]
    fn jira_timestamps_are_not_rfc3339() {
        let t: JiraTime = serde_json::from_str("\"2026-08-24T11:11:52.944+0000\"").unwrap();
        assert_eq!(t.0.format("%Y-%m-%d %H:%M").to_string(), "2026-08-24 11:11");
        // Genuine RFC3339 has to be accepted too (some Cloud endpoints hand out this shape)
        let t2: JiraTime = serde_json::from_str("\"2026-08-24T11:11:52.944+00:00\"").unwrap();
        assert_eq!(t2.0.format("%Y-%m-%d").to_string(), "2026-08-24");
    }

    /// JQL's time format and its quotes: get either wrong and it is a 400, not "nothing found".
    #[test]
    fn the_incremental_jql_uses_jiras_own_time_format() {
        let t = DateTime::parse_from_rfc3339("2026-08-30T12:34:56Z")
            .unwrap()
            .with_timezone(&Utc);
        let q = jql("KAFKA", Some(t));
        assert!(q.contains("project = KAFKA"), "{q}");
        assert!(q.contains("updated >= \"2026-08-30 12:34\""), "{q}");
        assert!(q.contains("ORDER BY updated ASC"), "{q}");
        // With no since there should be no updated clause. **Look for the clause, not the
        // word** -- ORDER BY updated ASC contains "updated" as well, and the first version of
        // this assertion tripped over exactly that
        assert!(!jql("KAFKA", None).contains("updated >="));
    }

    /// **Pin the field shapes down with a real response.**
    ///
    /// Hand-written JSON can only prove "the shape I thought it was". The fixture is taken from
    /// issues.apache.org (anonymously readable), for the same reason as the GitHub one -- and
    /// this time it has more to pin down: Jira's camelCase names (`fromString`/`displayName`),
    /// the non-RFC3339 times, and the nesting of the changelog.
    #[test]
    fn the_real_jira_shapes_still_parse() {
        let raw = include_str!("../tests/fixtures/jira_issues.json");
        let page: SearchPage = serde_json::from_str(raw).expect("the real response should parse");
        assert!(
            !page.issues.is_empty(),
            "the fixture is empty, so this test verified nothing"
        );

        for issue in &page.issues {
            let doc = render(issue);
            assert!(doc.starts_with(&format!("# {} ", issue.key)), "{doc}");
        }

        // The change history is the reason this source exists. At least one issue in the
        // fixture has a changelog, and it must be laid out as "field: old → new" -- writing only
        // "a change happened" says nothing at all
        let with_log = page
            .issues
            .iter()
            .find(|i| {
                i.changelog
                    .as_ref()
                    .is_some_and(|c| c.histories.iter().any(|h| !h.items.is_empty()))
            })
            .expect("the fixture should contain an issue with a changelog");
        let doc = render(with_log);
        assert!(
            doc.contains("## History"),
            "the history section is missing: {doc}"
        );
        assert!(
            doc.contains(" changed "),
            "the change lines are not written at field level: {doc}"
        );
        assert!(doc.contains(" → "), "from → to is missing: {doc}");
    }

    /// An empty comment should not leave behind a section with nothing but a heading (same rule
    /// as on the GitHub side).
    #[test]
    fn an_empty_comment_leaves_no_stub() {
        let issue: Issue = serde_json::from_value(serde_json::json!({
            "key": "X-1",
            "fields": {
                "summary": "t", "description": null, "labels": [],
                "created": "2026-01-01T00:00:00.000+0000",
                "updated": "2026-01-01T00:00:00.000+0000",
                "comment": {"comments": [
                    {"author": {"displayName": "a"},
                     "created": "2026-01-02T00:00:00.000+0000", "body": "   "}
                ]}
            }
        }))
        .unwrap();
        let out = render(&issue);
        assert!(
            !out.contains("### a"),
            "an empty comment should not leave a section behind: {out}"
        );
    }
}
