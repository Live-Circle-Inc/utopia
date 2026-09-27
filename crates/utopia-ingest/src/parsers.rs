//! The per-format parsers. All of them output plain text (structure is kept as markdown-style
//! headings).

use anyhow::Context;
use quick_xml::events::Event;
use quick_xml::Reader;
use std::io::{Cursor, Read};

/// Text decoding: chardetng detects the encoding (covering GBK/GB18030/BIG5 and the other
/// encodings common for Chinese).
pub fn plain_text(bytes: &[u8]) -> String {
    use chardetng::{EncodingDetector, Iso2022JpDetection, Utf8Detection};
    let mut detector = EncodingDetector::new(Iso2022JpDetection::Deny);
    detector.feed(bytes, true);
    let encoding = detector.guess(None, Utf8Detection::Allow);
    let (text, _, _) = encoding.decode(bytes);
    text.into_owned()
}

pub fn pdf(bytes: &[u8]) -> anyhow::Result<String> {
    pdf_extract::extract_text_from_mem(bytes).context("PDF text-layer extraction failed")
}

/// docx: unzip word/document.xml, take the w:t text and split paragraphs on w:p.
pub fn docx(bytes: &[u8]) -> anyhow::Result<String> {
    let xml = read_zip_entry(bytes, "word/document.xml").context("Malformed docx structure")?;
    extract_xml_text(&xml, "w:t", "w:p")
}

/// pptx: parse ppt/slides/slideN.xml in slide-number order, take the a:t text.
pub fn pptx(bytes: &[u8]) -> anyhow::Result<String> {
    let mut archive =
        zip::ZipArchive::new(Cursor::new(bytes.to_vec())).context("Failed to unzip pptx")?;
    let mut slides: Vec<(u32, String)> = Vec::new();
    for i in 0..archive.len() {
        let name = archive.by_index(i)?.name().to_string();
        if let Some(num) = name
            .strip_prefix("ppt/slides/slide")
            .and_then(|s| s.strip_suffix(".xml"))
            .and_then(|s| s.parse::<u32>().ok())
        {
            slides.push((num, name));
        }
    }
    slides.sort();

    let mut out = String::new();
    for (num, name) in slides {
        let mut entry = archive.by_name(&name)?;
        let mut xml = String::new();
        entry.read_to_string(&mut xml)?;
        let text = extract_xml_text(&xml, "a:t", "a:p")?;
        if !text.trim().is_empty() {
            out.push_str(&format!("\n## Page {num}\n{text}\n"));
        }
    }
    Ok(out)
}

/// xlsx / xls / ods: read every one of those formats through calamine, emitting a
/// tab-separated table per sheet (first 2000 rows only).
pub fn spreadsheet(bytes: &[u8]) -> anyhow::Result<String> {
    use calamine::{Data, Reader as _};
    let mut workbook = calamine::open_workbook_auto_from_rs(Cursor::new(bytes.to_vec()))
        .context("Failed to open spreadsheet")?;
    let mut out = String::new();
    for sheet_name in workbook.sheet_names() {
        let Ok(range) = workbook.worksheet_range(&sheet_name) else {
            continue;
        };
        if range.is_empty() {
            continue;
        }
        out.push_str(&format!("\n# Sheet: {sheet_name}\n"));
        for row in range.rows().take(2000) {
            let line: Vec<String> = row
                .iter()
                .map(|c| match c {
                    Data::Empty => String::new(),
                    other => other.to_string(),
                })
                .collect();
            if line.iter().any(|s| !s.is_empty()) {
                out.push_str(&line.join("\t"));
                out.push('\n');
            }
        }
    }
    Ok(out)
}

/// HTML: take the body text.
///
/// On a real web page the chrome is often bigger than the body -- site-wide navigation,
/// language lists, footer legalese, editing tools. Walking all of it feeds every bit of that to
/// the extractor: it both wastes one LLM call per chunk and pulls things like "Main page" and
/// "Privacy policy" out as entities that pollute the graph (measured: one 647KB Wikipedia
/// article produced 60 chunks, the first of which was entirely the sidebar menu and the last
/// entirely the copyright notice).
///
/// So recognise the body container first (main / role=main / article), and only fall back to
/// the whole page when none of them is found; navigation and forms can still be nested inside
/// the container, which is what walk_html's SKIP list is for.
pub fn html(bytes: &[u8]) -> String {
    let raw = plain_text(bytes);
    let doc = scraper::Html::parse_document(&raw);
    let mut out = String::new();
    if let Some(title) = doc
        .select(&scraper::Selector::parse("title").unwrap())
        .next()
    {
        out.push_str(&format!(
            "# {}\n\n",
            title.text().collect::<String>().trim()
        ));
    }
    let root = ["main", "[role=main]", "article"]
        .iter()
        .find_map(|sel| {
            scraper::Selector::parse(sel)
                .ok()
                .and_then(|s| doc.select(&s).next())
        })
        .unwrap_or_else(|| doc.root_element());
    walk_html(root, &mut out);
    out
}

fn walk_html(el: scraper::ElementRef, out: &mut String) {
    // Navigation, header/footer, sidebars and form controls are chrome even when they sit
    // inside the body container -- always skip them
    const SKIP: &[&str] = &[
        "script", "style", "noscript", "head", "svg", "template", "nav", "header", "footer",
        "aside", "form", "button", "select", "iframe", "dialog",
    ];
    const BLOCK: &[&str] = &[
        "p", "div", "li", "tr", "h1", "h2", "h3", "h4", "h5", "h6", "br", "section", "article",
    ];
    for node in el.children() {
        if let Some(child) = scraper::ElementRef::wrap(node) {
            let tag = child.value().name();
            if SKIP.contains(&tag) {
                continue;
            }
            walk_html(child, out);
            if BLOCK.contains(&tag) && !out.ends_with('\n') {
                out.push('\n');
            }
        } else if let Some(text) = node.value().as_text() {
            let t: &str = text;
            if !t.trim().is_empty() {
                out.push_str(t);
            }
        }
    }
}

pub fn csv_text(bytes: &[u8], tsv: bool) -> anyhow::Result<String> {
    let decoded = plain_text(bytes);
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(if tsv { b'\t' } else { b',' })
        .flexible(true)
        .has_headers(false)
        .from_reader(decoded.as_bytes());
    let mut out = String::new();
    for (i, record) in reader.records().enumerate() {
        if i >= 10_000 {
            break;
        }
        let record = record?;
        out.push_str(&record.iter().collect::<Vec<_>>().join(" | "));
        out.push('\n');
    }
    Ok(out)
}

// ---- Helpers ----

fn read_zip_entry(bytes: &[u8], name: &str) -> anyhow::Result<String> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes.to_vec()))?;
    let mut entry = archive.by_name(name)?;
    let mut content = String::new();
    entry.read_to_string(&mut content)?;
    Ok(content)
}

/// Extract the text inside `text_tag` (w:t, say) out of OOXML, breaking a line when
/// `para_tag` (w:p, say) ends.
fn extract_xml_text(xml: &str, text_tag: &str, para_tag: &str) -> anyhow::Result<String> {
    let mut reader = Reader::from_str(xml);
    let mut out = String::new();
    let mut in_text = false;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) if e.name().as_ref() == text_tag => in_text = true,
            Ok(Event::End(e)) => {
                let name = e.name();
                if name.as_ref() == text_tag {
                    in_text = false;
                } else if name.as_ref() == para_tag {
                    out.push('\n');
                }
            }
            Ok(Event::Text(t)) if in_text => {
                out.push_str(&t.xml_content(quick_xml::XmlVersion::Implicit1_0));
            }
            Ok(Event::Eof) => break,
            Err(e) => anyhow::bail!("XML parse error: {e}"),
            _ => {}
        }
    }
    Ok(out)
}
