//! The Notion source: sync in the pages the integration can see.
//!
//! Against the four criteria of
//! [0013](../../docs/decisions/0013-a-source-should-hand-over-its-history.md) it is a tier
//! above object storage:
//!
//! | Criterion | Notion |
//! |---|---|
//! | Real timestamps | `last_edited_time`, **the doc's own edit moment**, not when we grabbed it |
//! | Does it overturn itself | pages being rewritten over and over is precisely its normal state |
//! | Stable identity | the page UUID; retitling and moving both leave it alone |
//! | Does enterprise knowledge live there | policies, minutes, decision records -- just what this system wants |
//!
//! **But it hands over only the present state, not the history.** Notion's version history is
//! not in the public API, so unlike the issue trackers: one sync can only see this instant,
//! and the edits before it are accumulated slowly, one sync at a time. This is the same shape
//! as `url` / `rss`, whereas those two issue trackers can fetch the whole change history in
//! one go.
//!
//! ## Two easy things to trip on
//!
//! **The `Notion-Version` header is mandatory**, and its value is a date. Without it the
//! endpoint just 400s, and the error message only says "missing version" -- it will not tell
//! you which one to fill in.
//!
//! **The rate limit is three per second** (the official wording is "three on average"). So
//! page contents are fetched sequentially rather than concurrently -- going concurrent only
//! buys a string of 429s, and our retry backoff was designed for the extraction path; the
//! ingest path should not borrow it.

use anyhow::Context as _;
use chrono::{DateTime, Utc};

/// The API version in the request header. **Hard-coded rather than left to configuration**:
/// the shape of the response changes along with it, and letting a user fill in a version we
/// have never adapted to buys silently mismatched parsing.
const NOTION_VERSION: &str = "2026-03-11";

/// How many pages one sync fetches at most. Same reason as the other sources: ingestion is
/// irreversible, and a workspace can hold tens of thousands of pages.
const MAX_PAGES_PER_SYNC: usize = 500;

/// How many blocks are fetched per page at most. Truncating a page deeper than that beats
/// letting one sync get stuck on a single page.
const MAX_BLOCKS_PER_PAGE: usize = 500;

/// A page waiting to be ingested.
pub struct NotionPage {
    /// `notion://{page_id}` -- the page UUID is its most stable identity
    pub external_key: String,
    pub filename: String,
    pub text: String,
    pub last_edited: Option<DateTime<Utc>>,
}

fn client(token: &str) -> anyhow::Result<reqwest::Client> {
    let mut h = reqwest::header::HeaderMap::new();
    h.insert("Notion-Version", NOTION_VERSION.parse()?);
    h.insert(
        reqwest::header::AUTHORIZATION,
        format!("Bearer {token}").parse()?,
    );
    Ok(reqwest::Client::builder()
        .default_headers(h)
        .timeout(std::time::Duration::from_secs(60))
        .build()?)
}

/// Fetch every page the integration can see.
///
/// **Search pages only, not data sources.** The latter are containers for tables and have no
/// body of their own; every row in a table is a page and turns up in the same search.
pub async fn fetch(token: &str, query: Option<&str>) -> anyhow::Result<(Vec<NotionPage>, bool)> {
    let http = client(token)?;
    let mut out = Vec::new();
    let mut cursor: Option<String> = None;
    let mut truncated = false;

    loop {
        let mut body = serde_json::json!({
            "filter": { "property": "object", "value": "page" },
            "page_size": 100,
        });
        if let Some(q) = query {
            body["query"] = serde_json::Value::String(q.to_string());
        }
        if let Some(c) = &cursor {
            body["start_cursor"] = serde_json::Value::String(c.clone());
        }

        let resp = http
            .post("https://api.notion.com/v1/search")
            .json(&body)
            .send()
            .await
            .context("notion search")?;
        let status = resp.status();
        let v: serde_json::Value = resp.json().await.context("notion search response")?;
        if !status.is_success() {
            anyhow::bail!(
                "notion search returned {status}: {}",
                v["message"].as_str().unwrap_or("unknown")
            );
        }

        for p in v["results"].as_array().unwrap_or(&vec![]).clone() {
            // Skip the trashed and the archived -- they no longer count in the UI
            if p["in_trash"].as_bool() == Some(true) || p["is_archived"].as_bool() == Some(true) {
                continue;
            }
            if out.len() >= MAX_PAGES_PER_SYNC {
                truncated = true;
                break;
            }
            let Some(id) = p["id"].as_str() else { continue };
            let title = page_title(&p);
            let text = page_text(&http, id).await.unwrap_or_else(|e| {
                tracing::warn!(%id, error = %e, "cannot fetch page body, keeping only the title");
                String::new()
            });

            out.push(NotionPage {
                external_key: format!("notion://{id}"),
                filename: format!("{}.md", slug(&title)),
                text: format!("# {title}\n\n{text}"),
                last_edited: p["last_edited_time"]
                    .as_str()
                    .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                    .map(|d| d.with_timezone(&Utc)),
            });
        }

        if truncated || v["has_more"].as_bool() != Some(true) {
            break;
        }
        cursor = v["next_cursor"].as_str().map(str::to_string);
        if cursor.is_none() {
            break;
        }
    }
    Ok((out, truncated))
}

/// The page title.
///
/// **The title hides under whichever property in `properties` has `type == "title"`, and its
/// name is not fixed**: a page in a database may call it `Name`, `名称` or `任务`, while an
/// ordinary page calls it `title`. Looking it up by name finds nothing on somebody else's
/// workspace, so look it up by type.
fn page_title(page: &serde_json::Value) -> String {
    let props = page["properties"].as_object();
    let t = props.and_then(|m| {
        m.values()
            .find(|v| v["type"] == "title")
            .and_then(|v| v["title"].as_array())
    });
    let s = t
        .map(|arr| {
            arr.iter()
                .filter_map(|r| r["plain_text"].as_str())
                .collect::<String>()
        })
        .unwrap_or_default();
    if s.trim().is_empty() {
        "untitled".into()
    } else {
        s
    }
}

/// Fetch one page's body, expanding the blocks layer by layer.
async fn page_text(http: &reqwest::Client, page_id: &str) -> anyhow::Result<String> {
    let mut out = String::new();
    let mut n = 0usize;
    let mut cursor: Option<String> = None;

    loop {
        let mut url = format!("https://api.notion.com/v1/blocks/{page_id}/children?page_size=100");
        if let Some(c) = &cursor {
            url.push_str(&format!("&start_cursor={c}"));
        }
        let resp = http.get(&url).send().await?;
        if !resp.status().is_success() {
            anyhow::bail!("blocks returned {}", resp.status());
        }
        let v: serde_json::Value = resp.json().await?;

        for b in v["results"].as_array().unwrap_or(&vec![]) {
            if n >= MAX_BLOCKS_PER_PAGE {
                return Ok(out);
            }
            n += 1;
            if let Some(line) = render_block(b) {
                out.push_str(&line);
                out.push('\n');
            }
        }
        if v["has_more"].as_bool() != Some(true) {
            break;
        }
        cursor = v["next_cursor"].as_str().map(str::to_string);
        if cursor.is_none() {
            break;
        }
    }
    Ok(out)
}

/// Render one block into a line of text.
///
/// **An unrecognised type returns its plain text instead of being thrown away.** Notion keeps
/// adding block types, and hard-coding a whitelist means new types silently disappear;
/// meanwhile every block that carries text puts that text under `{type}.rich_text`, and that
/// shape is very stable.
fn render_block(b: &serde_json::Value) -> Option<String> {
    let t = b["type"].as_str()?;
    let inner = &b[t];
    let text = rich_text(&inner["rich_text"]);

    Some(match t {
        "heading_1" => format!("## {text}"),
        "heading_2" => format!("### {text}"),
        "heading_3" => format!("#### {text}"),
        "bulleted_list_item" => format!("- {text}"),
        "numbered_list_item" => format!("1. {text}"),
        "to_do" => {
            let done = inner["checked"].as_bool() == Some(true);
            format!("- [{}] {text}", if done { "x" } else { " " })
        }
        "quote" => format!("> {text}"),
        "code" => {
            let lang = inner["language"].as_str().unwrap_or("");
            format!("```{lang}\n{text}\n```")
        }
        // Dividers and images have no rich_text, and carry no information in the body either
        "divider" | "image" | "video" | "file" => return None,
        // A child_page's title lives in `title` rather than rich_text
        "child_page" => format!("- {}", inner["title"].as_str().unwrap_or("")),
        _ if text.trim().is_empty() => return None,
        _ => text,
    })
}

/// Join a rich_text array into plain text.
fn rich_text(v: &serde_json::Value) -> String {
    v.as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|r| r["plain_text"].as_str())
                .collect::<String>()
        })
        .unwrap_or_default()
}

/// Turn a title into something that works as a filename.
fn slug(title: &str) -> String {
    let s: String = title
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    let s = s.trim_matches('-').to_string();
    if s.is_empty() {
        "untitled".into()
    } else {
        s.chars().take(60).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The name of the title property is arbitrary.** A page in a database may call it
    /// `Name`, `名称` or `任务`; looking it up by name returns untitled on somebody else's
    /// workspace, and that looks like "the page has no title" rather than "we looked in the
    /// wrong place".
    #[test]
    fn a_title_is_found_by_type_not_by_name() {
        for key in ["title", "Name", "名称", "任务"] {
            let page = serde_json::json!({
                "properties": {
                    key: { "type": "title", "title": [{ "plain_text": "季度复盘" }] },
                    "Status": { "type": "select", "select": { "name": "Done" } }
                }
            });
            assert_eq!(
                page_title(&page),
                "季度复盘",
                "no title found for property name {key}"
            );
        }
    }

    /// Rich text comes in segments -- bold and links both cut a sentence apart. Fail to join
    /// it all back and characters go missing.
    #[test]
    fn rich_text_segments_join_back_into_one_line() {
        let v = serde_json::json!([
            { "plain_text": "把总部搬到" },
            { "plain_text": "深圳" },
            { "plain_text": "了" }
        ]);
        assert_eq!(rich_text(&v), "把总部搬到深圳了");
    }

    /// **An unrecognised block type must not be dropped.** Notion keeps adding types, and
    /// every block that carries text puts the text under `{type}.rich_text` -- rendering by
    /// whitelist makes new types silently disappear.
    #[test]
    fn an_unknown_block_keeps_its_text() {
        let b = serde_json::json!({
            "type": "some_new_block_type_2027",
            "some_new_block_type_2027": { "rich_text": [{ "plain_text": "还是有内容的" }] }
        });
        assert_eq!(render_block(&b).as_deref(), Some("还是有内容的"));
    }

    /// A decorative block with no text should vanish, or the body is nothing but blank lines.
    #[test]
    fn a_divider_renders_to_nothing() {
        let b = serde_json::json!({ "type": "divider", "divider": {} });
        assert!(render_block(&b).is_none());
    }

    /// A filename must not carry path separators or newlines.
    #[test]
    fn a_slug_is_safe_as_a_filename() {
        assert_eq!(slug("2026 Q3 / 复盘"), "2026-Q3---复盘");
        assert_eq!(slug("///"), "untitled");
        assert!(slug(&"x".repeat(200)).chars().count() <= 60);
    }

    /// Really connect to a Notion workspace. **There is no emulator** -- Notion is
    /// closed-source SaaS, and the open-source alternatives (AppFlowy, AFFiNE) do not speak
    /// this API. So this one only runs when somebody hands over a real token; on CI it always
    /// skips.
    ///
    /// ```text
    /// # Settings → My connections → new internal integration, then share a page with it
    /// UTOPIA_NOTION_TEST_TOKEN=ntn_xxx cargo test -p utopia-server notion
    /// ```
    #[tokio::test]
    async fn it_reads_from_a_real_workspace() -> anyhow::Result<()> {
        let Ok(token) = std::env::var("UTOPIA_NOTION_TEST_TOKEN") else {
            eprintln!("skipped: UTOPIA_NOTION_TEST_TOKEN is not set");
            return Ok(());
        };
        let (pages, _) = fetch(&token, None).await?;
        assert!(
            !pages.is_empty(),
            "not a single page -- the integration may not have been shared any pages"
        );
        let p = &pages[0];
        assert!(
            p.external_key.starts_with("notion://"),
            "{}",
            p.external_key
        );
        assert!(
            p.text.starts_with("# "),
            "the body should start with the title"
        );
        assert!(
            p.last_edited.is_some(),
            "last_edited_time is where doc_time comes from"
        );
        Ok(())
    }
}
