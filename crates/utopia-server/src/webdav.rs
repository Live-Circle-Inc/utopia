//! WebDAV sources: Nextcloud, ownCloud, Nutstore, Synology, Apache mod_dav,
//! rclone serve webdav, and every other cloud drive that speaks this protocol.
//!
//! By the four criteria in
//! [0013](../../docs/decisions/0013-a-source-should-hand-over-its-history.md)
//! it sits in the same bracket as object storage: there is a `getlastmodified`, identity is the
//! path, and companies really do keep piles of documents on a cloud drive; whereas "does it
//! contradict itself" can only be seen from the difference between two syncs -- WebDAV's
//! versioning extension (RFC 3253) has almost no server-side implementations.
//!
//! **Do not pull in a WebDAV client library.** What this protocol needs is very narrow: one
//! `PROPFIND` for the listing, one `GET` for the content, and the responses are XML. `reqwest`
//! and `quick-xml` are both already in the tree, so together that is zero new dependencies;
//! whereas the existing dav client crates either wrap their own HTTP stack or drag in the whole
//! of RFC 4918 (locks, property writes, versioning), and we use not a single one of those.

use anyhow::Context as _;
use chrono::{DateTime, Utc};
use quick_xml::events::Event;
use quick_xml::Reader;

/// How many files at most one sync ingests. Same reason as on the object-storage side: a cloud
/// drive can hold a hundred thousand files too, and ingestion is irreversible.
const MAX_FILES_PER_SYNC: usize = 2_000;

/// Size ceiling for a single file.
const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;

/// Ceiling on directory recursion depth. **The fear is not depth, it is cycles**: some servers
/// expose symlinks or shared folders as paths that can point at themselves, and
/// `Depth: infinity` on a directory like that never comes back. Walking level by level with a
/// cap is safer than trusting the server.
const MAX_DEPTH: usize = 8;

/// One remote file.
pub struct RemoteFile {
    /// `webdav://host/path` -- `ingest_item`'s convention for external_key is URI shape
    pub external_key: String,
    pub filename: String,
    pub bytes: Vec<u8>,
    pub last_modified: Option<DateTime<Utc>>,
}

/// One entry that came back from `PROPFIND`.
#[derive(Debug, PartialEq)]
struct Entry {
    /// The href the server gave us, already decoded
    href: String,
    is_dir: bool,
    len: u64,
    modified: Option<DateTime<Utc>>,
}

/// Parse a `multistatus` response.
///
/// **Only the local name counts, never the prefix.** A server may use `D:`, `d:`, `ns0:`, or no
/// prefix at all -- the RFC allows any prefix to be bound to the `DAV:` namespace. A parser that
/// hard-matches on `d:response` reads exactly zero entries the moment you point it at a
/// different server, and the symptom is "sync succeeded, zero files".
///
/// **The test for a directory is that `<collection/>` is present, not that the href ends in
/// `/`.** The latter is a convention and not the spec, and rclone and Nextcloud already disagree
/// about it.
fn parse_multistatus(xml: &str) -> anyhow::Result<Vec<Entry>> {
    let mut r = Reader::from_str(xml);
    r.config_mut().trim_text(true);

    let mut out = Vec::new();
    let mut cur: Option<Entry> = None;
    let mut field = String::new();
    let mut buf = Vec::new();

    loop {
        match r.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = local_name(e.name().into_inner());
                match name.as_str() {
                    "response" => {
                        cur = Some(Entry {
                            href: String::new(),
                            is_dir: false,
                            len: 0,
                            modified: None,
                        });
                    }
                    "collection" => {
                        if let Some(c) = cur.as_mut() {
                            c.is_dir = true;
                        }
                    }
                    other => field = other.to_string(),
                }
            }
            // `<collection/>` is usually a self-closing tag, so it arrives as Empty and not as
            // Start -- without this arm every directory gets treated as a file and GET'd
            Ok(Event::Empty(e)) => {
                if local_name(e.name().into_inner()) == "collection" {
                    if let Some(c) = cur.as_mut() {
                        c.is_dir = true;
                    }
                }
            }
            Ok(Event::Text(t)) => {
                let Some(c) = cur.as_mut() else { continue };
                let v = t
                    .xml_content(quick_xml::XmlVersion::Implicit1_0)
                    .to_string();
                match field.as_str() {
                    "href" => c.href = percent_decode(&v),
                    "getcontentlength" => c.len = v.trim().parse().unwrap_or(0),
                    "getlastmodified" => {
                        // RFC 1123, `Wed, 02 Sep 2026 15:04:05 GMT`
                        c.modified = DateTime::parse_from_rfc2822(v.trim())
                            .ok()
                            .map(|d| d.with_timezone(&Utc));
                    }
                    _ => {}
                }
            }
            Ok(Event::End(e)) => {
                if local_name(e.name().into_inner()) == "response" {
                    if let Some(c) = cur.take() {
                        out.push(c);
                    }
                }
                field.clear();
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(anyhow::anyhow!("failed to parse PROPFIND response: {e}")),
            _ => {}
        }
        buf.clear();
    }
    Ok(out)
}

/// Take a tag's local name, dropping the namespace prefix and lowercasing it.
///
/// The spec says element names are case-sensitive, but in practice there are servers that write
/// `getLastModified`. The prefix is stripped here too: `D:`, `d:` and `ns0:` are all legal, and
/// the RFC allows any prefix to be bound to `DAV:`.
fn local_name(raw: &str) -> String {
    raw.rsplit(':').next().unwrap_or(raw).to_ascii_lowercase()
}

/// Percent escapes inside an href.
///
/// Decoded by hand instead of pulling in `percent-encoding`: all we need here is decoding, and
/// decoding is about a dozen lines. Invalid sequences are left exactly as they are -- the href
/// gets pasted into a URL, and guessing wrong is worse than not touching it.
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = std::str::from_utf8(&b[i + 1..i + 3]).ok();
            if let Some(v) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Walk the directories level by level and fetch every file.
///
/// **Do not use `Depth: infinity`.** The spec allows a server to refuse it
/// (`403 Propfind-Finite-Depth`), and Nextcloud and Apache mod_dav refuse it by default; the
/// servers that do accept it will spit out tens of megabytes of XML in one go on a large
/// directory. Level by level with a cap has neither problem.
pub async fn fetch(
    http: &reqwest::Client,
    base: &str,
    root: &str,
    auth: Option<(&str, &str)>,
) -> anyhow::Result<(Vec<RemoteFile>, bool)> {
    let host = reqwest::Url::parse(base)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_else(|| "webdav".into());
    let base = base.trim_end_matches('/').to_string();

    let mut out = Vec::new();
    let mut truncated = false;
    let mut queue = vec![(normalize(root), 0usize)];
    let mut unreadable = 0usize;

    while let Some((dir, depth)) = queue.pop() {
        if depth > MAX_DEPTH {
            tracing::warn!(%dir, "directory too deep, not descending further");
            continue;
        }
        let url = format!("{base}{dir}");
        let mut req = http
            .request(reqwest::Method::from_bytes(b"PROPFIND").unwrap(), &url)
            .header("Depth", "1")
            .header("Content-Type", "application/xml");
        if let Some((u, p)) = auth {
            req = req.basic_auth(u, Some(p));
        }
        let resp = req
            .send()
            .await
            .with_context(|| format!("PROPFIND {url}"))?;
        if !resp.status().is_success() {
            anyhow::bail!("PROPFIND {url} returned {}", resp.status());
        }
        let xml = resp.text().await.context("reading the PROPFIND response")?;

        for e in parse_multistatus(&xml)? {
            let path = normalize(&strip_base(&e.href, &base));
            // The server lists the queried directory itself as well -- skip it, or we spin
            // forever
            if path == dir {
                continue;
            }
            if e.is_dir {
                queue.push((path, depth + 1));
                continue;
            }
            if e.len == 0 || e.len > MAX_FILE_BYTES {
                continue;
            }
            if out.len() >= MAX_FILES_PER_SYNC {
                truncated = true;
                break;
            }

            // One unfetchable file should not take the whole sync down with it -- same call as
            // on the object-storage side
            let mut g = http.get(format!("{base}{path}"));
            if let Some((u, p)) = auth {
                g = g.basic_auth(u, Some(p));
            }
            let got = async {
                let r = g.send().await?;
                if !r.status().is_success() {
                    anyhow::bail!("{}", r.status());
                }
                Ok::<_, anyhow::Error>(r.bytes().await?)
            }
            .await;
            let bytes = match got {
                Ok(b) => b,
                Err(err) => {
                    unreadable += 1;
                    tracing::warn!(%path, error = %err, "could not fetch file, skipping");
                    continue;
                }
            };

            out.push(RemoteFile {
                external_key: format!("webdav://{host}{path}"),
                filename: path.rsplit('/').next().unwrap_or(&path).to_string(),
                bytes: bytes.to_vec(),
                last_modified: e.modified,
            });
        }
        if truncated {
            break;
        }
    }
    if unreadable > 0 {
        tracing::warn!(unreadable, "some files could not be fetched");
    }
    Ok((out, truncated))
}

/// Normalise a path into the `/a/b` shape: leading slash, no trailing slash (except the root).
fn normalize(p: &str) -> String {
    let t = p.trim();
    let t = t.strip_suffix('/').unwrap_or(t);
    if t.is_empty() {
        "/".into()
    } else if t.starts_with('/') {
        t.into()
    } else {
        format!("/{t}")
    }
}

/// An href may be an absolute URL or just a path -- both are legal, and every server picks its
/// own.
fn strip_base(href: &str, base: &str) -> String {
    if let Some(rest) = href.strip_prefix(base) {
        return rest.to_string();
    }
    // Absolute but with a different hostname (rewritten by a reverse proxy): fall back to
    // taking its path
    if href.starts_with("http://") || href.starts_with("https://") {
        if let Ok(u) = reqwest::Url::parse(href) {
            return u.path().to_string();
        }
    }
    href.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **Namespace prefixes are arbitrary.** The same response with a different prefix must
    /// parse into the same thing, otherwise a different server means "sync succeeded, zero
    /// files" -- the hardest kind of failure to track down.
    #[test]
    fn any_namespace_prefix_parses_the_same() {
        let with_d = r#"<?xml version="1.0"?>
<D:multistatus xmlns:D="DAV:"><D:response>
  <D:href>/docs/a.txt</D:href>
  <D:propstat><D:prop>
    <D:getcontentlength>5</D:getcontentlength>
    <D:getlastmodified>Wed, 02 Sep 2026 15:04:05 GMT</D:getlastmodified>
    <D:resourcetype/>
  </D:prop></D:propstat>
</D:response></D:multistatus>"#;
        // Swap the prefix; the namespace declaration itself is unaffected (`xmlns:D="DAV:"`
        // does not contain the substring `D:`)
        let with_ns0 = with_d.replace("D:", "ns0:");
        let a = parse_multistatus(with_d).unwrap();
        let b = parse_multistatus(&with_ns0).unwrap();
        assert_eq!(a, b, "a different prefix and it no longer parses");
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].href, "/docs/a.txt");
        assert_eq!(a[0].len, 5);
        assert!(!a[0].is_dir);
        assert!(
            a[0].modified.is_some(),
            "getlastmodified is where doc_time comes from"
        );
    }

    /// **The test for a directory is `<collection/>`, and that is usually a self-closing tag.**
    /// A parser that only handles `Event::Start` will treat every directory as a file and GET
    /// it.
    #[test]
    fn a_self_closing_collection_is_still_a_directory() {
        let xml = r#"<multistatus xmlns="DAV:"><response>
  <href>/docs/</href>
  <propstat><prop><resourcetype><collection/></resourcetype></prop></propstat>
</response></multistatus>"#;
        let e = parse_multistatus(xml).unwrap();
        assert!(
            e[0].is_dir,
            "the self-closing collection was not recognised"
        );
    }

    /// Hrefs are percent-escaped, and pasting a Chinese path straight into a URL gives a 404.
    #[test]
    fn a_percent_encoded_href_is_decoded() {
        assert_eq!(
            percent_decode("/docs/%E4%B8%AD%E6%96%87.txt"),
            "/docs/中文.txt"
        );
        // Invalid sequences are left as they are; no guessing
        assert_eq!(percent_decode("/a%ZZb"), "/a%ZZb");
    }

    /// Path normalisation: leading slash, no trailing slash. Disagreeing on the two ends means
    /// walking the same directory twice.
    #[test]
    fn paths_normalise_to_one_shape() {
        for (raw, want) in [
            ("docs", "/docs"),
            ("/docs/", "/docs"),
            ("/", "/"),
            ("", "/"),
        ] {
            assert_eq!(normalize(raw), want, "{raw}");
        }
    }

    /// Really connect to a WebDAV server. Skipped when `UTOPIA_DAV_TEST_URL` is unset.
    ///
    /// Standing one up with rclone is the least work, and it differs from Nextcloud in the shape
    /// of its hrefs, which is exactly what proves the parser is not picky about servers:
    /// ```text
    /// mkdir -p /tmp/dav/docs && echo hello > /tmp/dav/docs/a.txt
    /// rclone serve webdav /tmp/dav --addr 127.0.0.1:18081 --user u --pass p
    /// UTOPIA_DAV_TEST_URL=http://127.0.0.1:18081 UTOPIA_DAV_TEST_USER=u     ///   UTOPIA_DAV_TEST_PASS=p cargo test -p utopia-server webdav
    /// ```
    #[tokio::test]
    async fn it_reads_from_a_real_webdav_server() -> anyhow::Result<()> {
        let Ok(base) = std::env::var("UTOPIA_DAV_TEST_URL") else {
            eprintln!("skipped: UTOPIA_DAV_TEST_URL is not set");
            return Ok(());
        };
        let user = std::env::var("UTOPIA_DAV_TEST_USER").unwrap_or_default();
        let pass = std::env::var("UTOPIA_DAV_TEST_PASS").unwrap_or_default();
        let auth = (!user.is_empty()).then_some((user.as_str(), pass.as_str()));

        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()?;
        let (files, truncated) = fetch(&http, &base, "/docs", auth).await?;
        assert!(!truncated);

        let a = files.iter().find(|f| f.filename == "a.txt").expect("a.txt");
        assert_eq!(a.bytes, b"hello from webdav");
        assert!(
            a.external_key.starts_with("webdav://"),
            "{}",
            a.external_key
        );
        assert!(a.last_modified.is_some());
        // Directories themselves must not slip in
        assert!(files.iter().all(|f| !f.filename.is_empty()));
        Ok(())
    }
}
