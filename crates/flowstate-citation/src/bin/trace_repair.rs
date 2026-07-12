//! Temporary offline repair-pass tracer for a single cached decode id.

use anyhow::{Context, Result};
use flowstate_citation::{normalize, reconstruct, snap};
use serde_json::Value;

fn authors(value: &Value) -> String {
    value
        .get("authors")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|author| {
                    format!(
                        "{} ({})",
                        author.get("surname").and_then(Value::as_str).unwrap_or(""),
                        author.get("name").and_then(Value::as_str).unwrap_or("")
                    )
                })
                .collect::<Vec<_>>()
                .join("; ")
        })
        .unwrap_or_default()
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let rows = std::fs::read_to_string(&args[1])?;
    let row: Value = rows
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .find(|row| row.get("id").and_then(Value::as_str) == Some(args[2].as_str()))
        .with_context(|| format!("id {} not found", args[2]))?;
    let source = row.get("input").and_then(Value::as_str).unwrap_or("");
    let raw = row.get("raw").and_then(Value::as_str).unwrap_or("");
    let mut obj = reconstruct::to_json(raw).context("reconstruct failed")?;
    let mut previous = authors(&obj);
    println!("reconstruct: {previous}");
    macro_rules! run {
        ($label:literal, $call:expr) => {{
            $call;
            let current = authors(&obj);
            if current != previous {
                println!("{}: {}", $label, current);
                previous = current;
            }
        }};
    }
    run!("normalize1", normalize::normalize_authors(&mut obj, source));
    run!("snap_surnames", snap::snap_surnames(&mut obj, source));
    run!("snap_surname_span", snap::snap_surname_span(&mut obj, source));
    run!("snap_cite_tag", snap::snap_cite_tag(&mut obj, source));
    run!("snap_names", snap::snap_names(&mut obj, source));
    run!("ground_names", snap::ground_names(&mut obj, source));
    run!("recover_empty", snap::recover_empty_author(&mut obj, source));
    run!("recover_key", snap::recover_key_coauthors(&mut obj, source));
    run!("recover_byline", snap::recover_byline_coauthors(&mut obj, source));
    run!("drop_near_dups", snap::drop_fabricated_near_dups(&mut obj, source));
    run!("strip_markers", normalize::strip_superscript_markers(&mut obj, source));
    run!("drop_phantom", normalize::drop_phantom_authors(&mut obj));
    run!("normalize2", normalize::normalize_authors(&mut obj, source));
    run!("semicolon", snap::recover_semicolon_record_authors(&mut obj, source));
    run!("roster", snap::recover_bibliographic_roster_authors(&mut obj, source));
    run!("conjunction", snap::recover_conjunction_chain_authors(&mut obj, source));
    run!("marked", snap::recover_marked_bracket_authors(&mut obj, source));
    run!("strong_empty", snap::recover_strong_empty_author(&mut obj, source));
    run!("role", snap::recover_role_prefixed_authors(&mut obj, source));
    run!("key_reconcile", snap::reconcile_explicit_key_authors(&mut obj, source));
    run!("page_header", snap::recover_page_header_coauthor(&mut obj, source));
    run!("single_initial", snap::repair_repeated_single_initial_surname(&mut obj, source));
    run!("drop_publication", snap::drop_obvious_publication_author(&mut obj, source));
    run!("drop_non_name", normalize::drop_non_name_authors(&mut obj));
    Ok(())
}
