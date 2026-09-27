//! The Charter corpus: the same batch of markdown as the frontend Docs page (a single source
//! of truth, upgraded along with the binary).
//! Adding an article = one more line in the frontend Docs.tsx list + one more line in
//! ARTICLES here.

use utopia_search::{DocsIndex, DocsSection};

/// (slug, title, body). The slug must match the frontend DOCS list (otherwise the citation
/// links /docs/{slug} will not line up).
const ARTICLES: &[(&str, &str, &str)] = &[(
    "ingest",
    "Ingest interfaces",
    include_str!("../../../web/src/docs/ingest.md"),
)];

/// Build the index at startup; the corpus is a compile-time constant, so a failure is a bug
/// in the program -- die loudly.
pub fn build_index() -> DocsIndex {
    DocsIndex::build(&sections()).expect("failed to build the Charter docs index")
}

/// Split on h2: one index record per section, so a hit comes back as the specific subsection
/// rather than the whole article.
/// The intro that precedes the first h2 goes into the "title section" (empty anchor, so the
/// link lands at the top of the article).
fn sections() -> Vec<DocsSection> {
    let mut out = Vec::new();
    for (slug, title, body) in ARTICLES {
        let mut heading = (*title).to_string();
        let mut anchor = String::new();
        let mut buf: Vec<&str> = Vec::new();
        let mut flush = |heading: &str, anchor: &str, buf: &mut Vec<&str>| {
            let text = buf.join("\n").trim().to_string();
            if !text.is_empty() {
                out.push(DocsSection {
                    slug: (*slug).to_string(),
                    title: (*title).to_string(),
                    heading: heading.to_string(),
                    anchor: anchor.to_string(),
                    body: text,
                });
            }
            buf.clear();
        };
        for line in body.lines() {
            if let Some(h) = line.strip_prefix("## ") {
                flush(&heading, &anchor, &mut buf);
                // Cleaned the same way as the frontend tocOf: drop inline code/emphasis
                // markers
                heading = h.replace(['`', '*'], "").trim().to_string();
                anchor = slugify(&heading);
            } else if !line.starts_with("# ") {
                buf.push(line);
            }
        }
        flush(&heading, &anchor, &mut buf);
    }
    out
}

/// Character-for-character aligned with the slugify in the frontend Docs.tsx (anchor jumps
/// depend on the two sides agreeing):
/// lowercase it, collapse every run outside [a-z0-9一-龥] into a single '-', then strip the
/// leading and trailing '-'.
fn slugify(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut dash = false;
    for c in s.to_lowercase().chars() {
        let keep =
            c.is_ascii_lowercase() || c.is_ascii_digit() || ('\u{4e00}'..='\u{9fa5}').contains(&c);
        if keep {
            if dash && !out.is_empty() {
                out.push('-');
            }
            dash = false;
            out.push(c);
        } else {
            dash = true;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_matches_frontend() {
        assert_eq!(slugify("Choosing between them"), "choosing-between-them");
        assert_eq!(
            slugify("Custom source — the pull interface"),
            "custom-source-the-pull-interface"
        );
        assert_eq!(
            slugify("API source — the push interface"),
            "api-source-the-push-interface"
        );
        assert_eq!(slugify("共享语义 Shared"), "共享语义-shared");
    }

    #[test]
    fn ingest_splits_into_sections() {
        let secs = sections();
        assert!(
            secs.len() >= 4,
            "ingest.md should split into an intro + 3 or more sections"
        );
        assert!(secs.iter().any(|s| s.anchor == "shared-semantics"));
        // The intro section: empty anchor, heading taken from the article title
        assert!(secs
            .iter()
            .any(|s| s.anchor.is_empty() && s.heading == "Ingest interfaces"));
    }
}
