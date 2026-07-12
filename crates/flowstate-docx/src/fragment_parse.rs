//! §act-twelve A12.3.1-full: fragment-parallel `document.xml` typed parse.
//!
//! `rdocx_oxml::CT_Document::from_xml` is a single-threaded streaming parse; on
//! large documents the typed tree build dominates the import hotpath (~107ms of
//! the fixture's ~122ms interpret step). The body is a flat sequence of
//! independent `<w:p>`/`<w:tbl>` subtrees dispatched by *local name*, so each
//! fragment parses standalone with no namespace or sibling context. This module
//! scans the body with a skip-only tokenizer pass (`read_to_end_into`, no tree
//! building) to find top-level fragment byte spans, streams each span to a
//! scoped worker pool the moment it is discovered (the parse pipelines under
//! the scan instead of waiting for it), and reassembles the `CT_Document` in
//! scan order. Everything that is not a top-level `w:p`/`w:tbl` (sectPr,
//! raw-xml captures, extra namespaces, background) is handled inline during the
//! scan with the exact upstream code paths, so the assembled tree is equal to
//! the sequential parse (`PartialEq` across the whole CT hierarchy backs the
//! oracle).
//!
//! Fail-safe: any scan or fragment error falls back to the sequential
//! `CT_Document::from_xml`, so error behavior (and messages) match upstream.
//! Knobs: `FLOWSTATE_DISABLE_FRAGMENT_PARSE` forces the sequential path;
//! `FLOWSTATE_FRAGMENT_PARSE_VERIFY` runs both and asserts tree equality;
//! `FLOWSTATE_PARSE_PROBE` prints stage timings.

use std::collections::VecDeque;
use std::ops::Range;
use std::sync::{Condvar, Mutex, OnceLock};

use quick_xml_oxml::Reader;
use quick_xml_oxml::events::Event;
use quick_xml_oxml::name::QName;
use rdocx_oxml::document::{BodyContent, CT_Body, CT_Document, CT_SectPr};
use rdocx_oxml::namespace::matches_local_name;
use rdocx_oxml::raw_xml::{capture_element, capture_empty_element};
use rdocx_oxml::table::CT_Tbl;
use rdocx_oxml::text::CT_P;

/// Below this size the sequential parse is already fast; thread setup would
/// only add noise.
const MIN_FRAGMENT_PARSE_BYTES: usize = 256 * 1024;
const MAX_WORKERS: usize = 8;

fn fragment_parse_disabled() -> bool {
  static DISABLED: OnceLock<bool> = OnceLock::new();
  *DISABLED.get_or_init(|| std::env::var_os("FLOWSTATE_DISABLE_FRAGMENT_PARSE").is_some())
}

fn fragment_parse_verify() -> bool {
  static VERIFY: OnceLock<bool> = OnceLock::new();
  *VERIFY.get_or_init(|| std::env::var_os("FLOWSTATE_FRAGMENT_PARSE_VERIFY").is_some())
}

fn parse_probe() -> bool {
  static PROBE: OnceLock<bool> = OnceLock::new();
  *PROBE.get_or_init(|| std::env::var_os("FLOWSTATE_PARSE_PROBE").is_some())
}

/// Drop-in replacement for `CT_Document::from_xml` that parses top-level body
/// fragments in parallel when the document is large enough to pay for it.
pub fn ct_document_from_xml(xml: &[u8]) -> rdocx_oxml::Result<CT_Document> {
  if fragment_parse_disabled() || xml.len() < MIN_FRAGMENT_PARSE_BYTES {
    return CT_Document::from_xml(xml);
  }
  match parse_parallel(xml) {
    Some(document) => {
      if fragment_parse_verify() {
        let sequential = CT_Document::from_xml(xml)?;
        assert!(
          document == sequential,
          "[flowstate-fragment-parse-verify] parallel CT_Document diverges from sequential parse"
        );
      }
      Ok(document)
    },
    // Any scan/fragment failure: let the sequential parse decide the outcome,
    // so error behavior is byte-for-byte upstream's.
    None => CT_Document::from_xml(xml),
  }
}

/// A top-level body fragment: its slot in the body's content sequence and the
/// byte span of its subtree.
struct Fragment {
  item_ix: usize,
  span: Range<usize>,
  is_paragraph: bool,
}

/// Single-producer/multi-consumer span queue: the scanner pushes fragments as
/// it discovers them; workers block on the condvar until spans arrive or the
/// scan finishes.
struct FragmentQueue {
  state: Mutex<(VecDeque<Fragment>, bool)>,
  ready: Condvar,
}

impl FragmentQueue {
  fn new() -> Self {
    FragmentQueue { state: Mutex::new((VecDeque::new(), false)), ready: Condvar::new() }
  }

  fn push_batch(&self, fragments: &mut Vec<Fragment>) {
    if fragments.is_empty() {
      return;
    }
    let mut state = self.state.lock().expect("fragment queue poisoned");
    state.0.extend(fragments.drain(..));
    drop(state);
    self.ready.notify_all();
  }

  fn finish(&self) {
    let mut state = self.state.lock().expect("fragment queue poisoned");
    state.1 = true;
    drop(state);
    self.ready.notify_all();
  }

  fn pop(&self) -> Option<Fragment> {
    let mut state = self.state.lock().expect("fragment queue poisoned");
    loop {
      if let Some(fragment) = state.0.pop_front() {
        return Some(fragment);
      }
      if state.1 {
        return None;
      }
      state = self.ready.wait(state).expect("fragment queue poisoned");
    }
  }
}

/// Scan-side accumulator: `slots` holds inline-parsed content (raw-xml
/// captures) at its body position; fragment slots stay `None` until the pool's
/// results are merged back in.
struct ScannedDocument {
  body_seen: bool,
  slots: Vec<Option<BodyContent>>,
  sect_pr: Option<CT_SectPr>,
  extra_namespaces: Vec<(String, String)>,
  background_xml: Option<Vec<u8>>,
}

fn parse_parallel(xml: &[u8]) -> Option<CT_Document> {
  let workers = std::thread::available_parallelism().map_or(1, |n| n.get()).min(MAX_WORKERS);
  if workers < 2 {
    return None;
  }
  let queue = FragmentQueue::new();
  let scan_t0 = std::time::Instant::now();
  let (scanned, parsed) = std::thread::scope(|scope| {
    let handles: Vec<_> = (0..workers)
      .map(|_| {
        scope.spawn(|| {
          let mut out = Vec::new();
          while let Some(fragment) = queue.pop() {
            out.push((fragment.item_ix, parse_fragment(xml, &fragment)));
          }
          out
        })
      })
      .collect();
    let scanned = scan_document(xml, &queue);
    // Unblock the workers on every exit path, including scan errors.
    queue.finish();
    let scan_elapsed = scan_t0.elapsed();
    let parsed: Vec<Vec<(usize, Option<BodyContent>)>> = handles
      .into_iter()
      .map(|handle| handle.join().expect("fragment parse worker panicked"))
      .collect();
    if parse_probe() {
      let fragment_count: usize = parsed.iter().map(Vec::len).sum();
      eprintln!(
        "[flowstate-fragment-parse] scan={scan_elapsed:?} pool_tail={:?} fragments={fragment_count} workers={workers}",
        scan_t0.elapsed().saturating_sub(scan_elapsed)
      );
    }
    (scanned, parsed)
  });
  let mut scanned = scanned.ok()?;
  if !scanned.body_seen {
    // Upstream substitutes a default body (default-letter sectPr); take the
    // sequential path rather than replicate that construction here.
    return None;
  }
  for (item_ix, content) in parsed.into_iter().flatten() {
    scanned.slots[item_ix] = Some(content?);
  }
  let content: Option<Vec<BodyContent>> = scanned.slots.into_iter().collect();
  Some(CT_Document {
    body: CT_Body { content: content?, sect_pr: scanned.sect_pr },
    extra_namespaces: scanned.extra_namespaces,
    background_xml: scanned.background_xml,
  })
}

/// Parse one standalone top-level fragment. The span starts at (or on
/// whitespace immediately before) the fragment's start tag; local-name
/// dispatch means the fragment needs no namespace declarations in scope.
fn parse_fragment(xml: &[u8], fragment: &Fragment) -> Option<BodyContent> {
  let expected_local: &[u8] = if fragment.is_paragraph { b"p" } else { b"tbl" };
  let mut reader = Reader::from_reader(&xml[fragment.span.clone()]);
  reader.config_mut().trim_text(true);
  let mut buf = Vec::new();
  loop {
    match reader.read_event_into(&mut buf) {
      Ok(Event::Start(e)) => {
        if !matches_local_name(e.name().as_ref(), expected_local) {
          return None;
        }
        return if fragment.is_paragraph {
          CT_P::from_xml(&mut reader).ok().map(BodyContent::Paragraph)
        } else {
          CT_Tbl::from_xml(&mut reader).ok().map(BodyContent::Table)
        };
      },
      Ok(Event::Eof) | Err(_) => return None,
      // Whitespace text is trimmed away; anything else preceding the start
      // tag (comments, PIs) is ignored exactly as the sequential body loop
      // ignores it.
      _ => {},
    }
    buf.clear();
  }
}

/// Mirror of `CT_Document::from_xml`'s top-level loop, with the body handed to
/// `scan_body` instead of `CT_Body::from_xml`.
fn scan_document(xml: &[u8], queue: &FragmentQueue) -> rdocx_oxml::Result<ScannedDocument> {
  let mut reader = Reader::from_reader(xml);
  reader.config_mut().trim_text(true);

  let mut scanned = ScannedDocument {
    body_seen: false,
    slots: Vec::new(),
    sect_pr: None,
    extra_namespaces: Vec::new(),
    background_xml: None,
  };
  let mut buf = Vec::new();

  // Known namespace prefixes upstream always emits itself.
  let known_ns: &[&[u8]] = &[b"xmlns:w", b"xmlns:r", b"xmlns:mc", b"xmlns"];

  loop {
    match reader.read_event_into(&mut buf) {
      Ok(Event::Start(e)) => {
        let name = e.name();
        if matches_local_name(name.as_ref(), b"body") {
          scanned.body_seen = true;
          scan_body(&mut reader, &mut scanned, queue)?;
        } else if matches_local_name(name.as_ref(), b"document") {
          for attr in e.attributes().flatten() {
            let key = attr.key.as_ref();
            if (key.starts_with(b"xmlns:") || key == b"xmlns") && !known_ns.contains(&key) {
              let key_str = std::str::from_utf8(key).unwrap_or("").to_string();
              let val_str = std::str::from_utf8(&attr.value).unwrap_or("").to_string();
              scanned.extra_namespaces.push((key_str, val_str));
            }
          }
        } else if matches_local_name(name.as_ref(), b"background") {
          scanned.background_xml = Some(capture_element(&mut reader, &e)?);
        } else {
          let owned_name = name.as_ref().to_vec();
          let mut skip_buf = Vec::new();
          reader.read_to_end_into(QName(&owned_name), &mut skip_buf)?;
        }
      },
      Ok(Event::Empty(e)) => {
        if matches_local_name(e.name().as_ref(), b"background") {
          scanned.background_xml = Some(capture_empty_element(&e)?);
        }
      },
      Ok(Event::Eof) => break,
      Err(e) => return Err(e.into()),
      _ => {},
    }
    buf.clear();
  }

  Ok(scanned)
}

/// Batch size for handing spans to the pool: one lock + notify per batch
/// instead of per fragment.
const PUSH_BATCH: usize = 64;

/// Mirror of `CT_Body::from_xml`, recording `w:p`/`w:tbl` spans via a
/// tokenizer-only skip (borrowed `read_event`/`read_to_end`, no tree building
/// and no event-buffer copies), streaming spans to the worker queue in
/// batches, and handling everything else with upstream's own code paths.
fn scan_body(reader: &mut Reader<&[u8]>, scanned: &mut ScannedDocument, queue: &FragmentQueue) -> rdocx_oxml::Result<()> {
  let mut pending: Vec<Fragment> = Vec::with_capacity(PUSH_BATCH);
  loop {
    let event_start = usize::try_from(reader.buffer_position()).expect("in-memory reader position fits usize");
    match reader.read_event() {
      Ok(Event::Start(e)) => {
        let name = e.name();
        if matches_local_name(name.as_ref(), b"p") || matches_local_name(name.as_ref(), b"tbl") {
          let is_paragraph = matches_local_name(name.as_ref(), b"p");
          reader.read_to_end(QName(name.as_ref()))?;
          let span = event_start..usize::try_from(reader.buffer_position()).expect("in-memory reader position fits usize");
          let item_ix = scanned.slots.len();
          scanned.slots.push(None);
          pending.push(Fragment { item_ix, span, is_paragraph });
          if pending.len() >= PUSH_BATCH {
            queue.push_batch(&mut pending);
          }
        } else if matches_local_name(name.as_ref(), b"sectPr") {
          queue.push_batch(&mut pending);
          scanned.sect_pr = Some(CT_SectPr::from_xml(reader)?);
        } else {
          queue.push_batch(&mut pending);
          scanned.slots.push(Some(BodyContent::RawXml(capture_element(reader, &e)?)));
        }
      },
      Ok(Event::Empty(e)) => {
        if !matches_local_name(e.name().as_ref(), b"body") {
          scanned.slots.push(Some(BodyContent::RawXml(capture_empty_element(&e)?)));
        }
      },
      Ok(Event::End(e)) if matches_local_name(e.name().as_ref(), b"body") => break,
      Ok(Event::Eof) => break,
      Err(e) => return Err(e.into()),
      _ => {},
    }
  }
  queue.push_batch(&mut pending);
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  /// Build a synthetic document.xml big enough (> MIN_FRAGMENT_PARSE_BYTES) to
  /// engage the parallel path, exercising every top-level shape the scanner
  /// mirrors: paragraphs, tables, unknown raw-xml subtrees, self-closing
  /// elements, sectPr, background, and extra namespaces.
  fn synthetic_document_xml() -> Vec<u8> {
    let mut xml = String::new();
    xml.push_str(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#);
    xml.push_str(
      r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml">"#,
    );
    xml.push_str(r#"<w:background w:color="FFFFFF"/>"#);
    xml.push_str("<w:body>");
    for ix in 0..1200 {
      match ix % 5 {
        0 => xml.push_str(&format!(
          r#"<w:p><w:pPr><w:pStyle w:val="Heading{}"/></w:pPr><w:r><w:rPr><w:b/></w:rPr><w:t xml:space="preserve">heading {} with some padding text to grow the file body toward the parallel threshold</w:t></w:r></w:p>"#,
          (ix % 4) + 1,
          ix
        )),
        1 | 2 | 3 => xml.push_str(&format!(
          r#"<w:p><w:r><w:t xml:space="preserve">card body text {} lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua</w:t></w:r><w:r><w:rPr><w:u w:val="single"/></w:rPr><w:t>underlined tail {}</w:t></w:r></w:p>"#,
          ix, ix
        )),
        _ => xml.push_str(&format!(
          r#"<w:tbl><w:tblPr><w:tblW w:w="0" w:type="auto"/></w:tblPr><w:tblGrid><w:gridCol w:w="4675"/><w:gridCol w:w="4675"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="4675" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>cell a {}</w:t></w:r></w:p></w:tc><w:tc><w:tcPr><w:tcW w:w="4675" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>cell b {}</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#,
          ix, ix
        )),
      }
      if ix % 97 == 0 {
        // Unknown top-level subtree and a self-closing unknown element: both
        // must round-trip as RawXml captures in exactly the upstream form.
        xml.push_str(r#"<w:sdt><w:sdtContent><w:p><w:r><w:t>inside sdt</w:t></w:r></w:p></w:sdtContent></w:sdt>"#);
        xml.push_str(r#"<w:bookmarkStart w:id="7" w:name="mark"/>"#);
      }
    }
    xml.push_str(
      r#"<w:sectPr><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/></w:sectPr>"#,
    );
    xml.push_str("</w:body></w:document>");
    xml.into_bytes()
  }

  #[test]
  fn parallel_parse_equals_sequential_on_synthetic_document() {
    let xml = synthetic_document_xml();
    assert!(xml.len() >= MIN_FRAGMENT_PARSE_BYTES, "fixture must engage the parallel path ({} bytes)", xml.len());
    let sequential = CT_Document::from_xml(&xml).expect("sequential parse");
    let parallel = parse_parallel(&xml).expect("parallel path should engage on this fixture");
    assert!(parallel == sequential, "parallel CT_Document must equal the sequential parse");
  }

  #[test]
  fn small_documents_take_the_sequential_path_unchanged() {
    let xml = br#"<?xml version="1.0"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>hi</w:t></w:r></w:p></w:body></w:document>"#;
    let via_entry = ct_document_from_xml(xml).expect("entry parse");
    let sequential = CT_Document::from_xml(xml).expect("sequential parse");
    assert!(via_entry == sequential);
  }
}
