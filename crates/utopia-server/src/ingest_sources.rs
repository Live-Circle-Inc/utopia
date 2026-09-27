//! Source sync jobs: url (page fetch) / rss (feeds, pubDate -> doc_time) /
//! github_issues / jira_issues (tickets, update time -> doc_time, with the status change history
//! in the body) / s3 (object storage, LastModified -> doc_time).
//! All of them dedupe by sha256 (duplicate content is skipped silently), and new documents enter
//! the standard ingest pipeline (process_document).
//! folder is a pure container (uploads land inside it) and api is push-based -- neither has any
//! pull semantics.

use crate::state::AppState;
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use utopia_core::models::Source;
use utopia_core::models::SourceKind;
use uuid::Uuid;

/// Cap on new documents per sync (stops an overlong feed/URL list dragging the job down)
const MAX_NEW_PER_SYNC: usize = 200;

/// The User-Agent used for fetching. reqwest sends none at all by default, while Wikipedia
/// explicitly refuses anonymous requests (403), as do sites fronted by Cloudflare generally --
/// so the URL and RSS source kinds simply did not work against a large set of real websites.
/// Announcing who we are is also crawler etiquette: the site can recognise us and contact us.
const UA: &str = concat!(
    "Utopia/",
    env!("CARGO_PKG_VERSION"),
    " (+https://utopia.bi; self-hosted knowledge platform)"
);

/// What a single sync produced (Moved/Unchanged are not counted).
#[derive(Debug, Default, Clone, Copy)]
pub struct SyncStats {
    pub created: usize,
    pub updated: usize,
}

impl SyncStats {
    fn absorb(&mut self, action: IngestAction) {
        match action {
            IngestAction::Created => self.created += 1,
            IngestAction::Updated => self.updated += 1,
            _ => {}
        }
    }
    fn total(&self) -> usize {
        self.created + self.updated
    }
}

pub async fn sync_source(state: &AppState, source_id: Uuid) -> anyhow::Result<()> {
    let source = utopia_store::sources::get(&state.pool, source_id).await?;
    utopia_store::sources::mark_running(&state.pool, source_id).await?;
    let run_id = utopia_store::sources::start_run(&state.pool, source_id).await?;
    state.emit_source(source.kb_id);

    // Exhaustive over the enum: add a source kind and you have to decide here how it syncs; the
    // compiler will not let a missing arm through
    let outcome = match SourceKind::parse(&source.kind) {
        Some(SourceKind::Url) => sync_urls(state, &source).await,
        Some(SourceKind::Rss) => sync_rss(state, &source).await,
        Some(SourceKind::Custom) => sync_custom(state, &source).await,
        Some(SourceKind::GithubIssues) => sync_github_issues(state, &source).await,
        Some(SourceKind::JiraIssues) => sync_jira_issues(state, &source).await,
        Some(SourceKind::S3 | SourceKind::AzureBlob | SourceKind::Gcs) => {
            sync_object_storage(state, &source).await
        }
        Some(SourceKind::Webdav) => sync_webdav(state, &source).await,
        Some(SourceKind::Notion) => sync_notion(state, &source).await,
        // Passive containers: folder / api / memory / upload have no pull semantics
        Some(SourceKind::Folder | SourceKind::Api | SourceKind::Memory | SourceKind::Upload) => {
            Ok(SyncStats::default())
        }
        None => Err(anyhow::anyhow!("unknown source kind `{}`", source.kind)),
    };

    match outcome {
        Ok(stats) => {
            utopia_store::sources::finish_run(
                &state.pool,
                run_id,
                source_id,
                None,
                stats.created as i32,
                stats.updated as i32,
            )
            .await?;
            utopia_store::sources::finish_sync(&state.pool, source_id, None, stats.total() as i32)
                .await?;
            state.emit_source(source.kb_id);
            tracing::info!(%source_id, kind = %source.kind, created = stats.created, updated = stats.updated, "source sync finished");
            Ok(())
        }
        Err(e) => {
            utopia_store::sources::finish_run(
                &state.pool,
                run_id,
                source_id,
                Some(&e.to_string()),
                0,
                0,
            )
            .await?;
            utopia_store::sources::finish_sync(&state.pool, source_id, Some(&e.to_string()), 0)
                .await?;
            state.emit_source(source.kb_id);
            Err(e)
        }
    }
}

/// The outcome of the three-way decision.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum IngestAction {
    Created,
    Updated,
    Moved,
    Unchanged,
    /// Tombstone: marks it "not in the source" (the document is not deleted -- whether to
    /// delete is the user's decision in the UI)
    Tombstoned,
}

async fn write_blob(state: &AppState, sha256: &str, bytes: &[u8]) -> anyhow::Result<()> {
    state.blob.put(sha256, bytes).await
}

fn sha_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A push with plain-upload semantics (KB-level /ingest -> Uploads): no source, no identity key
/// and no three-way decision -- if the same content is already in the KB it counts as a no-op,
/// and everything else is created new.
pub async fn ingest_upload(
    state: &AppState,
    kb_id: Uuid,
    filename: &str,
    mime: &str,
    bytes: &[u8],
    doc_time: Option<DateTime<Utc>>,
) -> anyhow::Result<IngestAction> {
    if bytes.is_empty() {
        return Ok(IngestAction::Unchanged);
    }
    let sha256 = sha_hex(bytes);
    write_blob(state, &sha256, bytes).await?;
    match utopia_store::documents::create(
        &state.pool,
        kb_id,
        filename,
        mime,
        bytes.len() as i64,
        &sha256,
        None,
        doc_time,
        None,
    )
    .await
    {
        Ok(doc) => {
            utopia_store::jobs::enqueue(
                &state.pool,
                "process_document",
                serde_json::json!({ "document_id": doc.id }),
            )
            .await?;
            state.emit_document(kb_id, doc.id);
            Ok(IngestAction::Created)
        }
        Err(utopia_core::AppError::Conflict(_)) => Ok(IngestAction::Unchanged),
        Err(e) => Err(e.into()),
    }
}

/// Identity-aware ingest: a three-way decision keyed by (source, external_key) --
/// new (create the document) / changed (replace in place + record a version + re-run the
/// pipeline) / unchanged (skip); the same content at a different path is recognised as a move
/// (only the identity changes, nothing is re-run). external_key is URI-shaped (file:/// relative
/// path, page URL, rss guid, api:{id}), so provenance is self-describing, and it also lines up
/// in advance with the document IRI of the P5 SPARQL projection.
#[allow(clippy::too_many_arguments)]
pub async fn ingest_item(
    state: &AppState,
    kb_id: Uuid,
    source_id: Uuid,
    external_key: &str,
    filename: &str,
    mime: &str,
    bytes: &[u8],
    doc_time: Option<DateTime<Utc>>,
) -> anyhow::Result<IngestAction> {
    if bytes.is_empty() {
        return Ok(IngestAction::Unchanged);
    }
    let sha256 = sha_hex(bytes);

    // Primary decision: logical identity. Fallback: historical documents from before the
    // migration (which have no key) are claimed by filename and given one
    let mut existing =
        utopia_store::documents::find_by_external_key(&state.pool, source_id, external_key).await?;
    if existing.is_none() {
        if let Some(legacy) =
            utopia_store::documents::find_legacy_by_filename(&state.pool, source_id, filename)
                .await?
        {
            utopia_store::documents::adopt_external_key(&state.pool, legacy.id, external_key)
                .await?;
            // On claiming, record the old content as version 1 (there was no version record
            // before)
            utopia_store::documents::record_version(
                &state.pool,
                legacy.id,
                &legacy.sha256,
                legacy.size_bytes,
            )
            .await?;
            existing = Some(legacy);
        }
    }

    if let Some(doc) = existing {
        if doc.sha256 == sha256 {
            return Ok(IngestAction::Unchanged);
        }
        // Changed: replace in place, the old version goes to document_versions (blobs are
        // content-addressed and never deleted, so replay has something to work with)
        write_blob(state, &sha256, bytes).await?;
        utopia_store::documents::replace_content(
            &state.pool,
            doc.id,
            filename,
            mime,
            bytes.len() as i64,
            &sha256,
            doc_time,
        )
        .await?;
        utopia_store::documents::record_version(&state.pool, doc.id, &sha256, bytes.len() as i64)
            .await?;
        utopia_store::jobs::enqueue(
            &state.pool,
            "process_document",
            serde_json::json!({ "document_id": doc.id }),
        )
        .await?;
        state.emit_document(kb_id, doc.id);
        return Ok(IngestAction::Updated);
    }

    // The same content appearing at a new path: recognised as a move/rename, and the pipeline
    // is not re-run
    if let Some(doc) =
        utopia_store::documents::find_by_source_sha(&state.pool, source_id, &sha256).await?
    {
        utopia_store::documents::update_location(&state.pool, doc.id, filename, external_key)
            .await?;
        state.emit_document(kb_id, doc.id);
        return Ok(IngestAction::Moved);
    }

    write_blob(state, &sha256, bytes).await?;
    match utopia_store::documents::create(
        &state.pool,
        kb_id,
        filename,
        mime,
        bytes.len() as i64,
        &sha256,
        Some(source_id),
        doc_time,
        Some(external_key),
    )
    .await
    {
        Ok(doc) => {
            utopia_store::documents::record_version(
                &state.pool,
                doc.id,
                &sha256,
                bytes.len() as i64,
            )
            .await?;
            utopia_store::jobs::enqueue(
                &state.pool,
                "process_document",
                serde_json::json!({ "document_id": doc.id }),
            )
            .await?;
            state.emit_document(kb_id, doc.id);
            Ok(IngestAction::Created)
        }
        // The same content is already in the KB (e.g. the same file was uploaded by hand): do
        // not ingest it a second time
        Err(utopia_core::AppError::Conflict(_)) => Ok(IngestAction::Unchanged),
        Err(e) => Err(e.into()),
    }
}

async fn sync_urls(state: &AppState, source: &Source) -> anyhow::Result<SyncStats> {
    let urls: Vec<String> = source.config["urls"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();
    if urls.is_empty() {
        anyhow::bail!("url source is missing config.urls (a list of page URLs)");
    }

    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .user_agent(UA)
        .build()?;
    let mut stats = SyncStats::default();
    let mut last_err: Option<String> = None;
    for url in urls.iter().take(MAX_NEW_PER_SYNC) {
        match fetch_page(&http, url).await {
            Ok((filename, mime, bytes)) => {
                // Logical identity = the URL itself: if the page content changed, replace it in
                // place (the history goes into the versions table)
                let action = ingest_item(
                    state,
                    source.kb_id,
                    source.id,
                    url,
                    &filename,
                    &mime,
                    &bytes,
                    None,
                )
                .await?;
                stats.absorb(action);
            }
            Err(e) => {
                tracing::warn!(%url, error = %e, "fetch failed");
                last_err = Some(format!("{url}: {e}"));
            }
        }
    }
    // Partial failure: if anything came through, treat it as success (the errors go to the
    // log); only a total wipeout is reported as an error
    if stats.total() == 0 {
        if let Some(err) = last_err {
            anyhow::bail!("{err}");
        }
    }
    // The configured list is the full set: documents that are not on it get marked "not in the
    // source" (a failed fetch does not count -- it is still configured)
    utopia_store::documents::reconcile_missing(&state.pool, source.id, &urls)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
    Ok(stats)
}

async fn fetch_page(
    http: &reqwest::Client,
    url: &str,
) -> anyhow::Result<(String, String, Vec<u8>)> {
    let resp = http.get(url).send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("HTTP {}", resp.status());
    }
    let mime = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("text/html")
        .split(';')
        .next()
        .unwrap_or("text/html")
        .to_string();
    let bytes = resp.bytes().await?.to_vec();
    let filename = filename_from_url(url, &mime);
    Ok((filename, mime, bytes))
}

fn filename_from_url(url: &str, mime: &str) -> String {
    let stripped = url
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/');
    let mut slug: String = stripped
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    slug.truncate(120);
    let has_ext = slug
        .rsplit('.')
        .next()
        .map(|e| e.len() <= 5)
        .unwrap_or(false)
        && slug.contains('.')
        && !slug.ends_with('.');
    if !has_ext || mime.contains("html") {
        format!("{slug}.html")
    } else {
        slug
    }
}

async fn sync_rss(state: &AppState, source: &Source) -> anyhow::Result<SyncStats> {
    let feed_url = source.config["feed_url"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("rss source is missing config.feed_url"))?;

    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .user_agent(UA)
        .build()?;
    let resp = http.get(feed_url).send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("HTTP {} fetching feed", resp.status());
    }
    let bytes = resp.bytes().await?;
    let feed = feed_rs::parser::parse(&bytes[..])
        .map_err(|e| anyhow::anyhow!("Failed to parse feed: {e}"))?;

    let mut stats = SyncStats::default();
    for entry in feed.entries.iter().take(MAX_NEW_PER_SYNC) {
        let title = entry
            .title
            .as_ref()
            .map(|t| t.content.clone())
            .unwrap_or_else(|| "untitled".into());
        let link = entry
            .links
            .first()
            .map(|l| l.href.clone())
            .unwrap_or_default();
        // Logical identity: the guid from the feed spec (usually already a permalink/urn),
        // falling back to the entry link when it is missing
        let key = if !entry.id.trim().is_empty() {
            entry.id.trim().to_string()
        } else if !link.is_empty() {
            link.clone()
        } else {
            format!("entry:{}", sha_hex(title.as_bytes()))
        };
        let body = entry
            .content
            .as_ref()
            .and_then(|c| c.body.clone())
            .or_else(|| entry.summary.as_ref().map(|s| s.content.clone()))
            .unwrap_or_default();
        // Entry publication time -> document time: temporal extraction gets a real timestamp
        // (this is precisely where this platform differentiates itself)
        let doc_time = entry.published.or(entry.updated);

        let html = format!(
            "<html><head><title>{}</title></head><body><h1>{}</h1>\n<p><a href=\"{}\">{}</a></p>\n{}</body></html>",
            title, title, link, link, body
        );
        let mut slug: String = title
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { '-' })
            .collect();
        slug.truncate(80);
        let filename = format!("{slug}.html");

        let action = ingest_item(
            state,
            source.kb_id,
            source.id,
            &key,
            &filename,
            "text/html",
            html.as_bytes(),
            doc_time,
        )
        .await?;
        stats.absorb(action);
    }
    Ok(stats)
}

/// GitHub tickets: one document per ticket, with its status change history in the body.
///
/// Issues and comments go **repo-level + `since`** (fetched in full in one paginated pass),
/// while events go **per issue** -- that is not an inconsistency: `issues/events` does not
/// support `since` and gets drowned out by PR events, and one run against a real repository
/// showed the status change history quietly coming up empty. See [`crate::github_issues`].
///
/// `doc_time` takes `updated_at` rather than `created_at`: what each sync captures is "what this
/// ticket looks like right now", and the knowledge time should say when that state came to hold.
/// Adding a comment changes `updated_at`, so the content changed, a new version is recorded, and
/// `doc_time` moves along with it.
async fn sync_github_issues(state: &AppState, source: &Source) -> anyhow::Result<SyncStats> {
    let repo = source.config["repo"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!("github_issues source is missing config.repo (owner/name)")
        })?;
    if repo.split('/').count() != 2 {
        anyhow::bail!("config.repo should look like owner/name, got {repo:?}");
    }
    let auth = source.config["auth_header"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    // In GitHub's model a PR is an issue too. Excluded by default -- what you ask a "ticket
    // system" for is tickets; but in some repositories the decision record actually lives in the
    // PR description, so the switch was left in
    let include_prs = source.config["include_pull_requests"]
        .as_bool()
        .unwrap_or(false);

    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .user_agent(UA)
        .build()?;
    let base = format!("https://api.github.com/repos/{repo}");

    // Incremental: GitHub's since means "updated after this"
    let mut issue_q: Vec<(&str, String)> = vec![("state", "all".into())];
    let mut comment_q: Vec<(&str, String)> = Vec::new();
    if let Some(t) = source.last_sync_at {
        issue_q.push(("since", t.to_rfc3339()));
        comment_q.push(("since", t.to_rfc3339()));
    }

    let issues: Vec<crate::github_issues::Issue> =
        crate::github_issues::fetch_all(&http, &format!("{base}/issues"), &issue_q, auth).await?;
    let comments: Vec<crate::github_issues::Comment> = crate::github_issues::fetch_all(
        &http,
        &format!("{base}/issues/comments"),
        &comment_q,
        auth,
    )
    .await?;
    let mut stats = SyncStats::default();
    for (issue, cs) in crate::github_issues::group_comments(&issues, &comments)
        .into_iter()
        .filter(|(i, _)| include_prs || i.pull_request.is_none())
        .take(MAX_NEW_PER_SYNC)
    {
        // Events are fetched per issue. The N is only the number of issues being written this
        // round -- on the first sync that equals the total, and after that, with since backing
        // it up, it is usually a single digit
        let events = crate::github_issues::sort_events(
            crate::github_issues::fetch_all(
                &http,
                &format!("{base}/issues/{}/events", issue.number),
                &[],
                auth,
            )
            .await?,
        );
        let es: Vec<&crate::github_issues::Event> = events.iter().collect();
        let body = crate::github_issues::render(issue, &cs, &es);
        // The logical identity carries the repository: with two repositories connected to the
        // same KB, the two #18s will not overwrite each other
        let key = format!("github:{repo}#{}", issue.number);
        let filename = format!("{}-{}.md", issue.number, slugify(&issue.title));
        let action = ingest_item(
            state,
            source.kb_id,
            source.id,
            &key,
            &filename,
            "text/markdown",
            body.as_bytes(),
            Some(issue.updated_at),
        )
        .await?;
        stats.absorb(action);
    }
    Ok(stats)
}

/// Jira tickets: one document per ticket, with a **field-level** change history in the body.
///
/// Cheaper than the GitHub path: a single `search?expand=changelog` call brings back the ticket
/// itself, the full change history and the comments, with **no N+1**. Incrementality is
/// expressed through JQL's `updated >= …`, because Jira has no `since` parameter. See
/// [`crate::jira_issues`].
///
/// `doc_time` takes `updated`, the same standard as github_issues: what each sync captures is
/// "what this ticket looks like right now", and the knowledge time should say when that state
/// came to hold.
async fn sync_jira_issues(state: &AppState, source: &Source) -> anyhow::Result<SyncStats> {
    let base_url = source.config["base_url"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("jira_issues source is missing config.base_url"))?;
    let project = source.config["project"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("jira_issues source is missing config.project"))?;
    // The project key is spliced straight into the JQL, so it cannot be an arbitrary string.
    // Jira's keys are themselves limited to alphanumerics plus underscore -- enforcing that
    // blocks JQL injection along the way
    if !project.chars().all(|c| c.is_alphanumeric() || c == '_') {
        anyhow::bail!("config.project should be a Jira project key, got {project:?}");
    }
    let auth = source.config["auth_header"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(45))
        .user_agent(UA)
        .build()?;

    let jql = crate::jira_issues::jql(project, source.last_sync_at);
    let (issues, total) = crate::jira_issues::fetch_all(&http, base_url, &jql, auth).await?;
    // **If it got truncated, say so.** A project that has been running for years easily has
    // tens of thousands of tickets, and the pagination cap means this round only covered a
    // slice; say nothing and "sync complete" in the UI is simply misleading
    if total > issues.len() as i64 {
        tracing::warn!(
            source_id = %source.id,
            fetched = issues.len(),
            total,
            "Jira results truncated by the pagination cap; this round covered only part of them, the next round's JQL window picks up from there"
        );
    }

    let mut stats = SyncStats::default();
    for issue in issues.iter().take(MAX_NEW_PER_SYNC) {
        let body = crate::jira_issues::render(issue);
        // The logical identity carries the site: with two Jiras connected to one KB, the two
        // PROJ-1s will not overwrite each other
        let host = reqwest::Url::parse(base_url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string))
            .unwrap_or_else(|| "jira".into());
        let key = format!("jira:{host}/{}", issue.key);
        let filename = format!(
            "{}-{}.md",
            issue.key,
            slugify(issue.fields.summary.as_deref().unwrap_or(""))
        );
        let action = ingest_item(
            state,
            source.kb_id,
            source.id,
            &key,
            &filename,
            "text/markdown",
            body.as_bytes(),
            issue.fields.updated.map(|t| t.0),
        )
        .await?;
        stats.absorb(action);
    }
    Ok(stats)
}

/// Title -> a filename-safe fragment. Same standard as the RSS path (non-alphanumerics become
/// -, then truncated).
fn slugify(title: &str) -> String {
    let mut s: String = title
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    s.truncate(60);
    s.trim_matches('-').to_string()
}

/// Custom puller -- the Utopia Ingest Interface:
/// `GET {endpoint}?since=<last sync, RFC3339>` (no since on the first sync; an Authorization
/// header can be configured), with the response
/// `{"items":[{"id":"stable unique id","title":"doc name",
///             "content":"body (plain text/Markdown/HTML)",
///             "doc_time":"RFC3339, optional","mime":"text/markdown, optional"}]}`.
/// id -> external_key (custom:{id}), and the three-way decision applies: same id with the same
/// content is skipped, new content is updated in place.
async fn sync_custom(state: &AppState, source: &Source) -> anyhow::Result<SyncStats> {
    let endpoint = source.config["endpoint"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("custom source is missing config.endpoint"))?;
    let mut url =
        reqwest::Url::parse(endpoint).map_err(|e| anyhow::anyhow!("Invalid endpoint URL: {e}"))?;
    if let Some(t) = source.last_sync_at {
        url.query_pairs_mut().append_pair("since", &t.to_rfc3339());
    }

    // A loopback endpoint does not go through the system proxy: a proxy only ever answers 502
    // for loopback addresses, and a service on this machine has to be reached directly
    let loopback = url
        .host_str()
        .map(|h| {
            h.eq_ignore_ascii_case("localhost") || h == "127.0.0.1" || h == "::1" || h == "[::1]"
        })
        .unwrap_or(false);
    let mut builder = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .user_agent(UA);
    if loopback {
        builder = builder.no_proxy();
    }
    let http = builder.build()?;
    let mut req = http.get(url);
    if let Some(auth) = source.config["auth_header"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
    {
        req = req.header(reqwest::header::AUTHORIZATION, auth.trim());
    }
    let resp = req.send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("HTTP {} from endpoint", resp.status());
    }
    let body: serde_json::Value = resp.json().await?;
    let items = body["items"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("Response is missing the items[] array"))?;

    let mut stats = SyncStats::default();
    let mut seen_keys: Vec<String> = Vec::new();
    for item in items.iter().take(MAX_NEW_PER_SYNC) {
        let Some(id) = item["id"].as_str().map(str::trim).filter(|s| !s.is_empty()) else {
            tracing::warn!(source_id = %source.id, "custom item is missing id, skipping");
            continue;
        };
        let Some(content) = item["content"].as_str().filter(|s| !s.trim().is_empty()) else {
            tracing::warn!(source_id = %source.id, %id, "custom item is missing content, skipping");
            continue;
        };
        let title = item["title"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(id);
        let mime = item["mime"].as_str().unwrap_or("text/markdown");
        let doc_time = item["doc_time"]
            .as_str()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.with_timezone(&Utc));
        let filename = ensure_extension(title, mime);
        let key = format!("custom:{id}");
        let action = ingest_item(
            state,
            source.kb_id,
            source.id,
            &key,
            &filename,
            mime,
            content.as_bytes(),
            doc_time,
        )
        .await?;
        stats.absorb(action);
        seen_keys.push(key);
    }
    // Absence from an incremental response (?since=) != deletion, so no full-set reconciliation;
    // but:
    // 1) items that show up again have their missing mark taken off (lost and found)
    // 2) only an explicit deleted[] tombstone marks something "not in the source" -- whether to
    //    delete the document is the user's decision in the UI
    if !seen_keys.is_empty() {
        utopia_store::documents::clear_missing_keys(&state.pool, source.id, &seen_keys)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;
    }
    let tombstones: Vec<String> = body["deleted"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str())
                .map(|id| format!("custom:{}", id.trim()))
                .collect()
        })
        .unwrap_or_default();
    if !tombstones.is_empty() {
        let n = utopia_store::documents::mark_missing_keys(&state.pool, source.id, &tombstones)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;
        tracing::info!(source_id = %source.id, count = n, "custom tombstones: marked not in the source");
    }
    Ok(stats)
}

/// Object storage sync: list the objects under a prefix and ingest them one by one.
///
/// `external_key` uses `s3://bucket/key`, the same convention as `file:///` and page URLs:
/// self-describing provenance. When the prefix changes but the content does not, `ingest_item`
/// recognises it as a "move" rather than something new, and extraction is not run again.
///
/// **`doc_time` takes `LastModified`, and that is the moment of writing, not the document's own
/// time.** A contract from 2019 uploaded today lands on today in the timeline. Object storage
/// has no better source -- unless the filename or the body carries a date, and that is the
/// extractor's job. This shares its affliction with the `url` source: the first criterion of
/// `0013` is only half satisfied here.
async fn sync_object_storage(state: &AppState, source: &Source) -> anyhow::Result<SyncStats> {
    let bucket = source.config["bucket"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("{} source is missing config.bucket", source.kind))?;
    let prefix = source.config["prefix"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let store = crate::object_storage::client(&source.kind, &source.config)?;
    let (objects, truncated) =
        crate::object_storage::fetch(&source.kind, store.as_ref(), bucket, prefix).await?;

    // Hitting the ceiling is not an error, but it has to be said out loud -- otherwise "sync
    // succeeded" has things hidden underneath it that never came in, and that is exactly the
    // silent failure 0005 talks about
    if truncated {
        tracing::warn!(
            %bucket, prefix = prefix.unwrap_or(""),
            "object count hit the per-run cap, the rest is left to the next sync"
        );
    }

    let mut stats = SyncStats::default();
    for obj in objects {
        // **Don't guess the mime.** `utopia_ingest::parse` looks at the magic number first and
        // the extension second, and the comment there says "the extension may be lying" --
        // guessing one from the filename here would only add one more source that can lie, and
        // would cost an extra dependency for the privilege. `octet-stream` is honest: what we
        // got is a string of bytes and we have not looked inside it.
        let action = ingest_item(
            state,
            source.kb_id,
            source.id,
            &obj.external_key,
            &obj.filename,
            "application/octet-stream",
            &obj.bytes,
            obj.last_modified,
        )
        .await?;
        stats.absorb(action);
    }
    Ok(stats)
}

/// WebDAV sync: walk the directories level by level and ingest the files.
///
/// `external_key` uses `webdav://host/path` -- the same drive remounted somewhere else is still
/// the same file, while identical paths on different drives are two files.
async fn sync_webdav(state: &AppState, source: &Source) -> anyhow::Result<SyncStats> {
    let base = source.config["base_url"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("webdav source is missing config.base_url"))?;
    let root = source.config["path"].as_str().map(str::trim).unwrap_or("/");
    let user = source.config["username"].as_str().map(str::trim);
    let pass = source.config["password"].as_str().map(str::trim);
    let auth = match (user, pass) {
        (Some(u), Some(p)) if !u.is_empty() => Some((u, p)),
        _ => None,
    };

    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()?;
    let (files, truncated) = crate::webdav::fetch(&http, base, root, auth).await?;
    if truncated {
        tracing::warn!(
            base,
            root,
            "file count hit the per-run cap, the rest is left to the next sync"
        );
    }

    let mut stats = SyncStats::default();
    for f in files {
        // Don't guess the mime, same reason as object storage: parsing looks at the magic
        // number first
        let action = ingest_item(
            state,
            source.kb_id,
            source.id,
            &f.external_key,
            &f.filename,
            "application/octet-stream",
            &f.bytes,
            f.last_modified,
        )
        .await?;
        stats.absorb(action);
    }
    Ok(stats)
}

/// Notion sync: ingest the pages the integration can see.
///
/// `doc_time` takes `last_edited_time` -- **this is the page's own edit time**, which is more
/// solid than the write time over in object storage: a contract from 2019 uploaded to S3 today
/// lands on today, whereas a Notion page's edit time is the moment its content changed.
async fn sync_notion(state: &AppState, source: &Source) -> anyhow::Result<SyncStats> {
    let token = source.config["token"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("notion source is missing config.token"))?;
    let query = source.config["query"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let (pages, truncated) = crate::notion::fetch(token, query).await?;
    if truncated {
        tracing::warn!("page count hit the per-run cap, the rest is left to the next sync");
    }

    let mut stats = SyncStats::default();
    for p in pages {
        let action = ingest_item(
            state,
            source.kb_id,
            source.id,
            &p.external_key,
            &p.filename,
            "text/markdown",
            p.text.as_bytes(),
            p.last_edited,
        )
        .await?;
        stats.absorb(action);
    }
    Ok(stats)
}

/// When the title has no extension, add one from the mime so it plugs into the parse matrix's
/// dispatch.
fn ensure_extension(title: &str, mime: &str) -> String {
    let has_ext = title
        .rsplit('.')
        .next()
        .map(|e| e.len() <= 5 && e.len() >= 2 && !e.contains(' '))
        .unwrap_or(false)
        && title.contains('.');
    if has_ext {
        return title.to_string();
    }
    let ext = if mime.contains("html") {
        "html"
    } else if mime.contains("plain") {
        "txt"
    } else {
        "md"
    };
    format!("{title}.{ext}")
}
