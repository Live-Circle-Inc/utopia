//! Probe the projection result against a real, public ontology.
//! Usage: cargo run -p utopia-ingest --example owl_probe -- <file>
fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).expect("a file path is required");
    let bytes = std::fs::read(&path)?;
    let fmt = utopia_ingest::ontology_rdf::RdfFormat::detect(&path, &bytes);
    let p = utopia_ingest::ontology_rdf::project(&bytes, fmt)?;
    println!(
        "format {fmt:?} · triples {} · classes {} · properties {}",
        p.triples,
        p.classes.len(),
        p.properties.len()
    );
    println!("\n-- classes (first 6) --");
    for c in p.classes.iter().take(6) {
        println!(
            "  {} | {} | parents {} | description {}",
            c.key,
            c.label,
            c.parents.len(),
            if c.description.is_empty() {
                "(none)".into()
            } else {
                format!("{}…", c.description.chars().take(50).collect::<String>())
            }
        );
    }
    println!("\n-- properties (first 6) --");
    for r in p.properties.iter().take(6) {
        println!(
            "  {} | {} | {}{}{} | domain {} range {}",
            r.key,
            r.label,
            if r.is_datatype { "literal" } else { "object" },
            if r.functional { " ·functional" } else { "" },
            if r.inverse_functional {
                " ·inv_functional"
            } else {
                ""
            },
            r.domains.len(),
            r.ranges.len()
        );
    }
    println!("\n-- not projected yet (first 8) --");
    let mut u: Vec<_> = p.unprojected.iter().collect();
    u.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    for (k, n) in u.into_iter().take(8) {
        println!("  {n:4} × {k}");
    }
    Ok(())
}
