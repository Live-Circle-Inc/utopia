//! The GitHub issues source: one issue = one document, with **its state-change history** in the
//! body.
//!
//! ## Why not just fetch the current state
//!
//! The most valuable part of an issue is not "it is closed right now", it is "it was opened on
//! 18 August, closed on 20 August, assigned to so-and-so in between, and its labels changed like
//! this". Fetch only the current state and that timeline has to be accumulated one sync at a
//! time -- the first sync can only see this instant, and everything before it is lost.
//!
//! Same judgement we made for the Wikipedia history snapshots: **don't take the present, take
//! the changes**. The difference is that GitHub hands the changes to you directly; no need to
//! sample a revision list the way the wiki forces you to.
//!
//! ## Comments go repo-wide, events go per issue -- not an inconsistency, two unequal endpoints
//!
//! The first version wanted all three to go through repo-wide endpoints, paged through once, to
//! avoid 401 requests for 200 issues (unauthenticated, GitHub only gives you 60 per hour).
//! **One run against real data showed the events path was wrong**:
//!
//! - `issues/comments` supports `since`, so every comment in the incremental window comes back
//!   in one pass. **Works well.**
//! - `issues/events` **does not support `since`**; you can only page backwards from the newest.
//!   And in GitHub's model PRs produce issue events too -- measured on this repo, the issue
//!   events were buried on page 5, and on a repo with active PRs they get pushed beyond the
//!   paging cap. **So the "state-change history" quietly turns up empty, and it is the very
//!   reason this source exists.**
//!
//! So events switched to per-issue `GET /repos/{repo}/issues/{n}/events`. The N+1 cost is real,
//! but N is only **the number of issues we are about to write this round**: the first sync is
//! the total issue count, after that `since` has our back and it is usually single digits.
//! Trading a bit of convenience for accuracy -- here that is the right trade.
//!
//! The three fetches are therefore:
//!
//! - `GET /repos/{repo}/issues?state=all&since=` -- the issues themselves (paged)
//! - `GET /repos/{repo}/issues/comments?since=`  -- repo-wide comments (paged), grouped by number
//! - `GET /repos/{repo}/issues/{n}/events`       -- once per issue
//!
//! ## About PRs
//!
//! In GitHub's data model a PR is an issue too, and the `/issues` endpoint returns them
//! together, told apart by the `pull_request` field. Excluded by default: when you ask an "issue
//! tracker" for issues, you want issues. But there is a switch -- in some repos (this one
//! included) the decision record actually lives in the PR description.

use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::collections::HashMap;

/// How many pages one sync fetches at most (100 per page). GitHub's pagination has no natural
/// end and an active repo can be paged for a very long time; cap it here, and whatever is left
/// over arrives on the next `since` increment.
const MAX_PAGES: u32 = 10;
const PER_PAGE: u32 = 100;

#[derive(Debug, Deserialize)]
pub struct Issue {
    pub number: i64,
    pub title: String,
    pub state: String,
    pub body: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub labels: Vec<Label>,
    #[serde(default)]
    pub assignees: Vec<Actor>,
    pub user: Option<Actor>,
    /// Present means this row is really a PR (GitHub stores both in the same table)
    #[serde(default)]
    pub pull_request: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Label {
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Actor {
    pub login: String,
}

#[derive(Debug, Deserialize)]
pub struct Comment {
    /// Which issue the comment hangs off: only the URL carries the number, so it has to be
    /// parsed out of the last segment of `.../issues/18`
    pub issue_url: String,
    pub user: Option<Actor>,
    pub created_at: DateTime<Utc>,
    pub body: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Event {
    pub event: String,
    pub created_at: DateTime<Utc>,
    pub actor: Option<Actor>,
    pub label: Option<Label>,
    pub assignee: Option<Actor>,
}

/// The last segment of a comment's `issue_url` is the issue number.
///
/// A failed parse returns None instead of panicking: when GitHub changes the shape of its URLs,
/// the cost should be "this comment did not get grouped onto its issue", not the whole sync
/// blowing up.
fn issue_number_from_url(url: &str) -> Option<i64> {
    url.rsplit('/').next()?.parse().ok()
}

/// Lay out one issue, together with its comments and events, as a single document.
///
/// **Pure function, no network** -- fetching and organising are kept apart, so the organising
/// half is testable. The logic that stitches the three paged fetches together (what belongs to
/// what, sorted by what) is exactly the part that is easiest to get wrong.
pub fn render(issue: &Issue, comments: &[&Comment], events: &[&Event]) -> String {
    let mut out = String::new();
    out.push_str(&format!("# #{} {}\n\n", issue.number, issue.title));

    // The header is written as dated prose rather than key-value pairs: the extractor reads
    // sentences. "opened by X on 2026-08-18" yields a fact with a valid_from;
    // "created_at: 2026-08-18" leaves it to guess what that is supposed to mean
    if let Some(u) = &issue.user {
        out.push_str(&format!(
            "Opened by {} on {}.\n",
            u.login,
            issue.created_at.format("%Y-%m-%d")
        ));
    }
    out.push_str(&format!("Currently {}.\n", issue.state));
    if let Some(c) = issue.closed_at {
        out.push_str(&format!("Closed on {}.\n", c.format("%Y-%m-%d")));
    }
    if !issue.labels.is_empty() {
        let names: Vec<&str> = issue.labels.iter().map(|l| l.name.as_str()).collect();
        out.push_str(&format!("Labelled {}.\n", names.join(", ")));
    }
    if !issue.assignees.is_empty() {
        let names: Vec<&str> = issue.assignees.iter().map(|a| a.login.as_str()).collect();
        out.push_str(&format!("Assigned to {}.\n", names.join(", ")));
    }

    if let Some(b) = issue
        .body
        .as_deref()
        .map(str::trim)
        .filter(|b| !b.is_empty())
    {
        out.push_str("\n## Description\n\n");
        out.push_str(b);
        out.push('\n');
    }

    // **The state-change history is the reason this source exists.** Every line carries a date,
    // so the ledger gets "when it became what" instead of one frozen current value
    if !events.is_empty() {
        out.push_str("\n## History\n\n");
        for e in events {
            let who = e.actor.as_ref().map(|a| a.login.as_str()).unwrap_or("?");
            let detail = match (e.event.as_str(), &e.label, &e.assignee) {
                ("labeled" | "unlabeled", Some(l), _) => format!(" ({})", l.name),
                ("assigned" | "unassigned", _, Some(a)) => format!(" ({})", a.login),
                _ => String::new(),
            };
            out.push_str(&format!(
                "- {} — {} by {}{}\n",
                e.created_at.format("%Y-%m-%d"),
                e.event,
                who,
                detail
            ));
        }
    }

    if !comments.is_empty() {
        out.push_str("\n## Comments\n\n");
        for c in comments {
            let who = c.user.as_ref().map(|a| a.login.as_str()).unwrap_or("?");
            let body = c.body.as_deref().unwrap_or("").trim();
            if body.is_empty() {
                continue;
            }
            out.push_str(&format!(
                "### {} on {}\n\n{}\n\n",
                who,
                c.created_at.format("%Y-%m-%d"),
                body
            ));
        }
    }
    out
}

/// Group repo-wide comments by issue number, each list in ascending time order.
///
/// Comments are fetched **repo-wide**, so mixed in are ones that are not in this round's issue
/// set (the incremental windows do not line up). Anything that cannot be grouped onto an issue
/// is simply dropped -- next time it comes back along with its own issue.
pub fn group_comments<'a>(
    issues: &'a [Issue],
    comments: &'a [Comment],
) -> Vec<(&'a Issue, Vec<&'a Comment>)> {
    let mut by_issue: HashMap<i64, Vec<&Comment>> = HashMap::new();
    for c in comments {
        if let Some(n) = issue_number_from_url(&c.issue_url) {
            by_issue.entry(n).or_default().push(c);
        }
    }
    issues
        .iter()
        .map(|issue| {
            let mut cs = by_issue.remove(&issue.number).unwrap_or_default();
            cs.sort_by_key(|c| c.created_at);
            (issue, cs)
        })
        .collect()
}

/// Events in ascending time order. What the per-issue endpoint returns **looks** ascending, but
/// the order is not a contract, and the consequence of sorting wrong here is history told
/// backwards.
pub fn sort_events(mut events: Vec<Event>) -> Vec<Event> {
    events.sort_by_key(|e| e.created_at);
    events
}

/// Page through one endpoint until an empty page or the cap.
pub async fn fetch_all<T: for<'de> Deserialize<'de>>(
    http: &reqwest::Client,
    base: &str,
    query: &[(&str, String)],
    auth: Option<&str>,
) -> anyhow::Result<Vec<T>> {
    let mut out = Vec::new();
    for page in 1..=MAX_PAGES {
        // Build the query by hand: this combination of reqwest features has no
        // RequestBuilder::query, and sync_custom builds it this way anyway
        let mut url = reqwest::Url::parse(base)?;
        {
            let mut q = url.query_pairs_mut();
            for (k, v) in query {
                q.append_pair(k, v);
            }
            q.append_pair("per_page", &PER_PAGE.to_string());
            q.append_pair("page", &page.to_string());
        }
        let mut req = http.get(url);
        if let Some(a) = auth {
            req = req.header(reqwest::header::AUTHORIZATION, a);
        }
        let resp = req.send().await?;
        let status = resp.status();
        if !status.is_success() {
            // Say that rate limiting is rate limiting: unauthenticated it is 60 per hour, and
            // one sync of a medium repo can eat the lot. Reporting a generic HTTP error sends
            // people off to check the network, when the right move is to configure a token
            let remaining = resp
                .headers()
                .get("x-ratelimit-remaining")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("?");
            if status == reqwest::StatusCode::FORBIDDEN && remaining == "0" {
                anyhow::bail!(
                    "GitHub rate limit: this hour's quota is exhausted. 60 per hour unauthenticated; a token raises it to 5000"
                );
            }
            anyhow::bail!("HTTP {status} from GitHub");
        }
        let batch: Vec<T> = resp.json().await?;
        let n = batch.len();
        out.extend(batch);
        if n < PER_PAGE as usize {
            break;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issue(json: serde_json::Value) -> Issue {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn a_comment_url_yields_its_issue_number() {
        assert_eq!(
            issue_number_from_url("https://api.github.com/repos/a/b/issues/18"),
            Some(18)
        );
        // A changed shape means it cannot be grouped, but it must not blow up
        assert_eq!(issue_number_from_url("https://example.com/"), None);
        assert_eq!(issue_number_from_url("nonsense"), None);
    }

    /// **The state-change history has to land in the body.** That is the entire difference
    /// between this source and "scraping a web page": without it, an issue is just one frozen
    /// current value in the ledger.
    #[test]
    fn the_history_lands_in_the_document_with_dates() {
        let i = issue(serde_json::json!({
            "number": 18, "title": "Deleting a conversation removes all turns",
            "state": "closed", "body": "Steps to reproduce…",
            "created_at": "2026-08-18T16:18:27Z", "updated_at": "2026-08-20T22:31:47Z",
            "closed_at": "2026-08-20T22:31:47Z",
            "labels": [{"name": "bug"}], "assignees": [{"login": "WaylandYang"}],
            "user": {"login": "Danmushu"}
        }));
        let e1: Event = serde_json::from_value(serde_json::json!({
            "event": "labeled", "created_at": "2026-08-19T01:00:00Z",
            "actor": {"login": "WaylandYang"}, "label": {"name": "bug"}
        }))
        .unwrap();
        let e2: Event = serde_json::from_value(serde_json::json!({
            "event": "closed", "created_at": "2026-08-20T22:31:47Z",
            "actor": {"login": "WaylandYang"}
        }))
        .unwrap();
        let out = render(&i, &[], &[&e1, &e2]);

        assert!(out.contains("Opened by Danmushu on 2026-08-18."), "{out}");
        assert!(out.contains("Closed on 2026-08-20."), "{out}");
        assert!(out.contains("Labelled bug."), "{out}");
        assert!(out.contains("Assigned to WaylandYang."), "{out}");
        // Event lines carry the date and the actor; labeled also carries which label
        assert!(
            out.contains("- 2026-08-19 — labeled by WaylandYang (bug)"),
            "{out}"
        );
        assert!(
            out.contains("- 2026-08-20 — closed by WaylandYang"),
            "{out}"
        );
    }

    /// Comments are fetched **repo-wide**, so the grouping has to be by issue number, in
    /// ascending time order. Getting it wrong fails quietly: another issue's discussion shows up
    /// in this one's body.
    #[test]
    fn repo_wide_comments_land_on_the_right_issue() {
        let issues = vec![
            issue(serde_json::json!({
                "number": 1, "title": "one", "state": "open", "body": null,
                "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-02T00:00:00Z",
                "closed_at": null, "user": {"login": "a"}
            })),
            issue(serde_json::json!({
                "number": 2, "title": "two", "state": "open", "body": null,
                "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-02T00:00:00Z",
                "closed_at": null, "user": {"login": "b"}
            })),
        ];
        let comments: Vec<Comment> = serde_json::from_value(serde_json::json!([
            {"issue_url": "https://api.github.com/repos/a/b/issues/2",
             "user": {"login": "x"}, "created_at": "2026-01-05T00:00:00Z", "body": "second one"},
            {"issue_url": "https://api.github.com/repos/a/b/issues/2",
             "user": {"login": "y"}, "created_at": "2026-01-03T00:00:00Z", "body": "first one"},
            // Not in this round's issue set: should be dropped, not pinned on someone else
            {"issue_url": "https://api.github.com/repos/a/b/issues/99",
             "user": {"login": "z"}, "created_at": "2026-01-04T00:00:00Z", "body": "someone else's"}
        ]))
        .unwrap();
        let grouped = group_comments(&issues, &comments);
        assert_eq!(grouped.len(), 2);

        let (one, one_comments) = &grouped[0];
        assert_eq!(one.number, 1);
        assert!(one_comments.is_empty(), "#1 has no comments");

        let (two, two_comments) = &grouped[1];
        assert_eq!(two.number, 2);
        assert_eq!(two_comments.len(), 2, "the #99 comment must not slip in");
        // Ascending: "first one" (01-03) before "second one" (01-05)
        assert_eq!(two_comments[0].body.as_deref(), Some("first one"));
    }

    /// Events from the per-issue endpoint have **no issue field** (the context is already in the
    /// URL), so that field has to be optional -- the first version modelled the repo-wide
    /// response and made it required, which stops parsing once the endpoint changes.
    #[test]
    fn per_issue_events_parse_without_an_issue_field() {
        let evs: Vec<Event> = serde_json::from_value(serde_json::json!([
            {"event": "closed", "created_at": "2026-01-06T00:00:00Z", "actor": {"login": "x"}},
            {"event": "labeled", "created_at": "2026-01-02T00:00:00Z",
             "actor": {"login": "y"}, "label": {"name": "bug"}}
        ]))
        .unwrap();
        let sorted = sort_events(evs);
        // The endpoint's order is not a contract, so do the sorting yourself
        assert_eq!(sorted[0].event, "labeled");
        assert_eq!(sorted[1].event, "closed");
    }

    /// In GitHub's model a PR is an issue too; this field is how you recognise one.
    #[test]
    fn a_pull_request_is_recognisable_among_the_issues() {
        let pr = issue(serde_json::json!({
            "number": 3, "title": "a pr", "state": "open", "body": null,
            "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-01T00:00:00Z",
            "closed_at": null, "user": {"login": "a"},
            "pull_request": {"url": "https://api.github.com/repos/a/b/pulls/3"}
        }));
        assert!(pr.pull_request.is_some());
        let plain = issue(serde_json::json!({
            "number": 4, "title": "an issue", "state": "open", "body": null,
            "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-01T00:00:00Z",
            "closed_at": null, "user": {"login": "a"}
        }));
        assert!(plain.pull_request.is_none());
    }

    /// **Pin the field shapes with a real response.**
    ///
    /// Hand-written JSON only proves that "the shape I imagined" parses. A GitHub issue has
    /// hundreds of fields and we declare ten of them; which field is really named something
    /// else, and which one is null under some conditions, only real data can say. The fixture is
    /// taken from deeplethe/utopia with the fields trimmed down to the ones we declare (the
    /// trimming itself incidentally proves that undeclared fields do not make serde fail).
    ///
    /// One thing in particular is pinned down: **the per-issue events endpoint does not return
    /// an `issue` field**. The first version modelled the repo-wide response and made it
    /// required, which stops the whole batch from parsing once the endpoint changes.
    #[test]
    fn the_real_github_shapes_still_parse() {
        #[derive(serde::Deserialize)]
        struct Fixture {
            issues: Vec<Issue>,
            comments: Vec<Comment>,
            events_by_issue: std::collections::HashMap<String, Vec<Event>>,
        }
        let raw = include_str!("../tests/fixtures/github_issues.json");
        let f: Fixture = serde_json::from_str(raw).expect("the real response should parse");

        assert!(
            !f.issues.is_empty(),
            "the fixture is empty, so this test verifies nothing"
        );
        // If it were all PRs, the PR-filtering path would not be covered by this fixture
        assert!(
            f.issues.iter().all(|i| i.pull_request.is_none()),
            "the fixture should contain only real issues"
        );

        let grouped = group_comments(&f.issues, &f.comments);
        assert_eq!(grouped.len(), f.issues.len());

        // Every issue lays out into a non-empty document whose header line carries a real date
        for (issue, cs) in &grouped {
            let events = sort_events(
                f.events_by_issue
                    .get(&issue.number.to_string())
                    .cloned()
                    .unwrap_or_default(),
            );
            let es: Vec<&Event> = events.iter().collect();
            let doc = render(issue, cs, &es);
            assert!(
                doc.contains(&format!("# #{} ", issue.number)),
                "#{} has the wrong header: {doc}",
                issue.number
            );
            assert!(
                doc.contains(&issue.created_at.format("%Y-%m-%d").to_string()),
                "#{} has no creation date in its body",
                issue.number
            );
        }

        // Every issue in this fixture has been closed at some point, so the history section has
        // to show up -- it is the reason this source exists, and an empty one is a fall back to
        // "scraping a web page"
        let (first, cs) = &grouped[0];
        let events = sort_events(
            f.events_by_issue
                .get(&first.number.to_string())
                .cloned()
                .unwrap_or_default(),
        );
        assert!(
            !events.is_empty(),
            "#{} has no events in the fixture",
            first.number
        );
        let es: Vec<&Event> = events.iter().collect();
        let doc = render(first, cs, &es);
        assert!(
            doc.contains("## History"),
            "the history section is missing: {doc}"
        );
        assert!(
            doc.contains("— closed by"),
            "the close event is missing from the history: {doc}"
        );
    }

    /// An empty comment should not leave a heading-only stub in the body.
    #[test]
    fn an_empty_comment_leaves_no_stub() {
        let i = issue(serde_json::json!({
            "number": 1, "title": "t", "state": "open", "body": null,
            "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-01T00:00:00Z",
            "closed_at": null, "user": {"login": "a"}
        }));
        let c: Comment = serde_json::from_value(serde_json::json!({
            "issue_url": "https://api.github.com/repos/a/b/issues/1",
            "user": {"login": "x"}, "created_at": "2026-01-02T00:00:00Z", "body": "   "
        }))
        .unwrap();
        let out = render(&i, &[&c], &[]);
        assert!(
            !out.contains("### x"),
            "an empty comment should not leave a section: {out}"
        );
    }
}
