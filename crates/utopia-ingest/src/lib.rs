//! utopia-ingest: the parsing matrix + chunking.
//! Principle: solve the text layer natively in Rust (fast, zero dependencies); scans and
//! complex layouts go through a docling sidecar later.

mod chunker;
pub mod ontology_rdf;
mod parsers;

pub use chunker::{chunk_text, ChunkPiece};

/// Parse output: plain text + optional structural info.
#[derive(Debug)]
pub struct ParsedDoc {
    pub text: String,
}

/// Supported formats (P1): pdf / docx / xlsx·xls·ods / pptx / md / txt / html / csv / json / yaml / xml / log
pub fn parse(filename: &str, bytes: &[u8]) -> anyhow::Result<ParsedDoc> {
    let ext = filename
        .rsplit('.')
        .next()
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();

    // Magic-number sniffing wins over the extension (extensions can lie)
    let kind = infer::get(bytes).map(|t| t.extension()).unwrap_or("");

    let text = match (kind, ext.as_str()) {
        ("pdf", _) | (_, "pdf") => parsers::pdf(bytes)?,
        ("docx", _) | (_, "docx") => parsers::docx(bytes)?,
        ("xlsx", _) | (_, "xlsx") | (_, "xls") | (_, "ods") => parsers::spreadsheet(bytes)?,
        ("pptx", _) | (_, "pptx") => parsers::pptx(bytes)?,
        (_, "html") | (_, "htm") => parsers::html(bytes),
        (_, "csv") | (_, "tsv") => parsers::csv_text(bytes, ext == "tsv")?,
        // md/json/yaml/xml/log/txt and every unrecognized format: decode as text
        // (encoding sniffing covers GBK and friends)
        _ => parsers::plain_text(bytes),
    };

    let text = normalize(&text);
    if text.trim().is_empty() {
        anyhow::bail!("No text could be extracted (possibly a scanned or empty file)");
    }
    Ok(ParsedDoc { text })
}

/// Collapse runs of blank lines, normalize line endings.
fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut blank_run = 0;
    for line in text.replace("\r\n", "\n").replace('\r', "\n").lines() {
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            blank_run += 1;
            if blank_run <= 1 {
                out.push('\n');
            }
        } else {
            blank_run = 0;
            out.push_str(trimmed);
            out.push('\n');
        }
    }
    out
}
