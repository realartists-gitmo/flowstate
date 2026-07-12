//! Net for the schema restoration: the debate tail (`card_signatures`,
//! `debate_annotations`, `raw_tail`), per-author `qualifications`, and the bibliographic
//! extras (`database`, `accessed_date`, `retrieved_date`) were re-added to the label schema
//! after they had been dropped in the surname/name simplification. The deterministic layer
//! (`reconstruct` → normalize → snap → checks) was written when those fields were absent, so
//! this proves it neither drops nor corrupts them, and — critically — that a cutter signature
//! is NOT swept into `authors`.

#![allow(
    clippy::tests_outside_test_module,
    reason = "integration-test crate: the whole file is test code, tests live at module root"
)]

use flowstate_citation::process_raw;

/// A brace-free decode (as T5 emits — no object braces) carrying the full restored schema.
const RAW: &str = concat!(
    r#""status":"parsed","authors":["#,
    r#""surname":"Moten","name":"Fred Moten","qualifications":["professor of Performance Studies at New York University"],"#,
    r#""surname":"Harney","name":"Stefano Harney"],"#,
    r#""title":"Talk with Fred Moten and Stefano Harney","source_type":"interview","#,
    r#""publisher":"Duke University Press","database":"JSTOR","year":2020,"#,
    r#""accessed_date":"March 3, 2021","retrieved_date":"March 5, 2021","#,
    r#""card_signatures":["//ah"],"debate_annotations":["*recut for finals"],"raw_tail":"leftover context""#,
);

const SOURCE: &str = "Moten and Harney 20, Fred Moten is a professor of Performance Studies at \
    New York University, and Stefano Harney, Talk with Fred Moten and Stefano Harney, Duke \
    University Press, JSTOR, accessed March 3, 2021, retrieved March 5, 2021 //ah *recut for \
    finals leftover context";

#[test]
fn restored_schema_survives_the_deterministic_layer() {
    let (obj, _passed) = process_raw(RAW, SOURCE);
    let v = obj.expect("brace-free input must reconstruct into an object");

    let authors = v["authors"].as_array().expect("authors is an array");
    assert_eq!(authors.len(), 2, "both coauthors preserved (signature not added as a third)");
    assert_eq!(authors[0]["surname"], "Moten");
    assert_eq!(authors[1]["surname"], "Harney");

    // the debater-read quals blob survives attached to its author
    assert_eq!(
        authors[0]["qualifications"][0],
        "professor of Performance Studies at New York University",
        "qualifications preserved through normalize/snap",
    );

    // bibliographic extras pass through untouched
    assert_eq!(v["database"], "JSTOR");
    assert_eq!(v["accessed_date"], "March 3, 2021");
    assert_eq!(v["retrieved_date"], "March 5, 2021");

    // the debate tail survives
    assert_eq!(v["card_signatures"][0], "//ah");
    assert_eq!(v["debate_annotations"][0], "*recut for finals");
    assert_eq!(v["raw_tail"], "leftover context");

    // the cutter signature must never have leaked into authors as a surname/name
    for a in authors {
        assert_ne!(a["surname"].as_str(), Some("//ah"), "signature must not become an author surname");
        assert_ne!(a["surname"].as_str(), Some("ah"), "signature must not become an author surname");
        let name = a["name"].as_str().unwrap_or("");
        assert!(!name.contains("//ah"), "signature must not leak into a name");
    }
}

#[test]
fn qualifications_survive_the_snap_fallback_path() {
    // Force the checks-fail → snap_authors fallback by making a surname ungrounded, and prove
    // the fallback (which mutates authors in place) does not drop the sibling's qualifications.
    let raw = concat!(
        r#""status":"parsed","authors":["#,
        r#""surname":"Zzzungrounded","name":"Fred Moten","qualifications":["professor at NYU"]],"#,
        r#""title":"Talk with Fred Moten","source_type":"interview""#,
    );
    let source = "Fred Moten is a professor at NYU, Talk with Fred Moten";
    let (obj, _passed) = process_raw(raw, source);
    let v = obj.expect("must reconstruct");
    let a = v["authors"].as_array().expect("authors array");
    assert_eq!(a[0]["qualifications"][0], "professor at NYU", "quals survive the fallback snap");
}
