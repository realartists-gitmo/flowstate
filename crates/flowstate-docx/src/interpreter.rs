mod omml;
mod structured;

use std::{io, path::Path};

use quick_xml::{
  Reader as XmlReader,
  events::{BytesStart, Event},
};
use rdocx::Document as RDocxDocument;
use rdocx_opc::OpcPackage;
use rdocx_oxml::document::CT_Document;
use rdocx_oxml::properties::{CT_PPr, CT_RPr};
use rdocx_oxml::shared::ST_Underline;
use rustc_hash::{FxHashMap, FxHashSet};

use super::cleaner::{CleanedDocx, DocxCleanReport, clean_docx_path};
use flowstate_document::{
  DocumentParagraphInput, DocumentProjection, DocumentRunInput, ImportedLoroDocument, ParagraphStyle, RunSemanticStyle, RunStyles,
  SOFT_LINE_BREAK_STR, VertAlign, document_from_input_blocks, document_from_paragraphs, flowstate_document_theme, import_document_projection,
};

/// Map an OOXML `w:vertAlign` value (`superscript` / `subscript` / `baseline`)
/// to the model's [`VertAlign`]. Unknown/absent ⇒ `Baseline`.
fn vert_align_from_ooxml(value: Option<&str>) -> VertAlign {
  match value {
    Some("superscript") => VertAlign::Superscript,
    Some("subscript") => VertAlign::Subscript,
    _ => VertAlign::Baseline,
  }
}
use flowstate_fidelity::FidelityClass;

pub const RECOGNITION_RULES: &[RecognitionRule] = &[
  RecognitionRule::ParagraphStyle {
    docx_style_id: "Heading1",
    db8_style: flowstate_document::PARAGRAPH_POCKET,
  },
  RecognitionRule::ParagraphStyle {
    docx_style_id: "Heading2",
    db8_style: flowstate_document::PARAGRAPH_HAT,
  },
  RecognitionRule::ParagraphStyle {
    docx_style_id: "Heading3",
    db8_style: flowstate_document::PARAGRAPH_BLOCK,
  },
  RecognitionRule::ParagraphStyle {
    docx_style_id: "Heading4",
    db8_style: flowstate_document::PARAGRAPH_TAG,
  },
  RecognitionRule::ParagraphStyle {
    docx_style_id: "Analytic",
    db8_style: flowstate_document::PARAGRAPH_ANALYTIC,
  },
  RecognitionRule::ParagraphStyle {
    docx_style_id: "Undertag",
    db8_style: flowstate_document::PARAGRAPH_UNDERTAG,
  },
  RecognitionRule::ParagraphFallbackNormal,
  RecognitionRule::RunStyle {
    docx_style_id: "Style13ptBold",
    db8_semantic: flowstate_document::SEMANTIC_CITE,
  },
  RecognitionRule::RunStyle {
    docx_style_id: "Emphasis",
    db8_semantic: flowstate_document::SEMANTIC_EMPHASIS,
  },
  RecognitionRule::RunStyle {
    docx_style_id: "StyleUnderline",
    db8_semantic: flowstate_document::SEMANTIC_UNDERLINE,
  },
  RecognitionRule::RunDirectUnderline,
  RecognitionRule::RunStrikethrough,
  RecognitionRule::RunHighlightToSpoken,
  RecognitionRule::RunShadingToSpoken,
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecognitionRule {
  ParagraphStyle {
    docx_style_id: &'static str,
    db8_style: ParagraphStyle,
  },
  ParagraphFallbackNormal,
  RunStyle {
    docx_style_id: &'static str,
    db8_semantic: RunSemanticStyle,
  },
  RunDirectUnderline,
  RunStrikethrough,
  RunHighlightToSpoken,
  RunShadingToSpoken,
}

#[derive(Clone, Debug)]
pub struct DocxConversionReport {
  pub clean: DocxCleanReport,
  pub recognition_rules: &'static [RecognitionRule],
  pub paragraphs_imported: usize,
  pub runs_imported: usize,
  pub tables_imported: usize,
  pub images_imported: usize,
  pub equations_imported: usize,
  pub unknown_paragraph_styles: Vec<String>,
  pub unknown_run_styles: Vec<String>,
}

#[hotpath::measure]
pub fn convert_docx_to_document(path: impl AsRef<Path>) -> io::Result<(DocumentProjection, DocxConversionReport)> {
  let cleaned = clean_docx_path(path)?;
  convert_cleaned_docx_to_document(cleaned)
}

#[hotpath::measure]
pub fn convert_docx_bytes_to_document(bytes: &[u8]) -> io::Result<(DocumentProjection, DocxConversionReport)> {
  let cleaned = super::cleaner::clean_docx_bytes(bytes)?;
  convert_cleaned_docx_to_document(cleaned)
}

#[hotpath::measure]
pub fn convert_cleaned_docx_to_document(cleaned: CleanedDocx) -> io::Result<(DocumentProjection, DocxConversionReport)> {
  let interpreted = interpret_cleaned_docx(&cleaned)?;
  build_structured_document(&cleaned, interpreted)
}

/// Walks the cleaned DOCX body into the structured block model (paragraphs,
/// tables, images, equations) and folds the object counts into the report. When
/// the body has no tables/images/equations the structured walk is discarded and
/// the flat paragraph projection is used, preserving the prior behavior exactly.
#[hotpath::measure]
fn build_structured_document(cleaned: &CleanedDocx, interpreted: InterpretedDocx) -> io::Result<(DocumentProjection, DocxConversionReport)> {
  // §perf: the structured walk (full DOM parse, OPC package open, per-run text
  // clones) is discarded whenever the body has no tables/images/equations, yet it
  // ran unconditionally. When the captured main-document XML provably contains
  // none of those object markers, skip the entire build and take the object-free
  // path directly — byte-identical to the discard branch below.
  //
  // Correctness: the walk recognizes elements by prefix-stripped local name and
  // detects equations via the `oMath` substring, so scanning the same bytes for
  // the bare substrings `tbl`/`drawing`/`oMath` can never produce a false
  // negative (a real object's start tag always contains its local name regardless
  // of namespace prefix). False positives (the substring appearing in text) only
  // forgo the optimization and fall through to the identical full build. When no
  // captured XML is available (`None`), `interpret_structured` would read from the
  // package, so we conservatively run the full build (`is_some_and` → `false`).
  let object_free = cleaned.main_document_xml.as_deref().is_some_and(|xml| {
    memchr::memmem::find(xml, b"tbl").is_none()
      && memchr::memmem::find(xml, b"drawing").is_none()
      && memchr::memmem::find(xml, b"oMath").is_none()
      // §act-eleven C7 (fixture-caught): legacy VML images (`w:pict`) are
      // objects too — without this probe a VML-ONLY document took the
      // object-free fast path and its images were never scanned at all.
      && memchr::memmem::find(xml, b"imagedata").is_none()
  });

  let (document, report) = if object_free {
    let mut report = interpreted.report;
    // Matches the counts a fully-walked object-free body would yield.
    report.tables_imported = 0;
    report.images_imported = 0;
    report.equations_imported = 0;
    let document = document_from_paragraphs(flowstate_document_theme(), interpreted.paragraphs);
    (document, report)
  } else {
    // §act-nine A9.2: the structured walk consumes the CT_Document the
    // direct-properties pass already parsed instead of building its own owned
    // XmlNode tree (collapses the third heavy document.xml parse).
    let structured = structured::interpret_structured(cleaned, &interpreted.document, &interpreted.paragraphs)?;
    let mut report = interpreted.report;
    report.tables_imported = structured.tables_imported;
    report.images_imported = structured.images_imported;
    report.equations_imported = structured.equations_imported;

    let has_objects = structured.tables_imported + structured.images_imported + structured.equations_imported > 0;
    let document = if has_objects {
      let mut document = document_from_input_blocks(flowstate_document_theme(), structured.blocks);
      for (asset_id, record) in structured.assets {
        document.assets.assets.insert(asset_id, record);
      }
      document
    } else {
      document_from_paragraphs(flowstate_document_theme(), interpreted.paragraphs)
    };
    (document, report)
  };
  // Import/export fidelity: record the shape of the projection produced from the
  // cleaned DOCX. Shared funnel for both the db8 and Loro import entries.
  flowstate_fidelity::event(FidelityClass::ImportExport, "import-docx", || {
    format!(
      "paragraphs={} runs={} tables={} images={} equations={} sections={}",
      report.paragraphs_imported,
      report.runs_imported,
      report.tables_imported,
      report.images_imported,
      report.equations_imported,
      document.sections.len()
    )
  });
  Ok((document, report))
}

/// Imports DOCX semantics directly into the canonical Loro document and returns
/// the frontier-matched initial projection. No package, snapshot, search cache,
/// or second Loro projection is created on the open path.
#[hotpath::measure]
pub fn import_docx_to_loro(path: impl AsRef<Path>, title: &str) -> io::Result<(ImportedLoroDocument, DocxConversionReport)> {
  let cleaned = clean_docx_path(path)?;
  import_cleaned_docx_to_loro(cleaned, title)
}

#[hotpath::measure]
pub fn import_docx_bytes_to_loro(bytes: &[u8], title: &str) -> io::Result<(ImportedLoroDocument, DocxConversionReport)> {
  let cleaned = super::cleaner::clean_docx_bytes(bytes)?;
  import_cleaned_docx_to_loro(cleaned, title)
}

#[hotpath::measure]
pub fn import_cleaned_docx_to_loro(cleaned: CleanedDocx, title: &str) -> io::Result<(ImportedLoroDocument, DocxConversionReport)> {
  let interpreted = interpret_cleaned_docx(&cleaned)?;
  let (document, report) = build_structured_document(&cleaned, interpreted)?;
  let imported = import_document_projection(document, title)?;
  Ok((imported, report))
}

struct InterpretedDocx {
  paragraphs: Vec<DocumentParagraphInput>,
  /// The typed main-document parse harvested for `DirectParagraphFacts`, kept
  /// alive so the structured object walk can consume it too (§act-nine A9.2).
  document: CT_Document,
  report: DocxConversionReport,
}

/// rdocx's run/paragraph `text()` collapses every `<w:br>` (line, page, and
/// column alike) to '\n' before the break type is visible here. A docx break is
/// an INTRA-paragraph line break, while '\n' is the paragraph separator in the
/// body flow — letting it through fabricates a bare paragraph boundary with no
/// metadata/block/style record, which full reprojection reports as
/// `missing_paragraph_*` defects (see `structured::collect_run_text`, the
/// table-cell counterpart of this mapping). Remap to the model's soft break.
/// Page/column breaks also becoming soft breaks matches the table-cell path
/// and is provisional pending a product decision on their semantics. `<w:cr>`
/// is dropped inside rdocx itself and never reaches this text.
fn soften_rdocx_breaks(text: String) -> String {
  // U+FFFC (OBJECT_REPLACEMENT) is a RESERVED body-flow encoding sentinel: the
  // projector treats every U+FFFC as an object placeholder. A literal one sitting
  // in source run text (seen in real docx) reads back as an OrphanObjectPlaceholder
  // defect, so strip it at ingestion. Rare, so the contains-checks keep the common
  // path allocation-free.
  let strip_object = text.contains('\u{FFFC}');
  // §act-ten A9.6: normalize CR too. quick-xml does not perform XML line-ending
  // normalization, so a literal `\r\n` (or bare `\r`) inside `w:t` survives into
  // run text; the un-normalized `\r` then rides the body flow as a phantom char
  // the exporter cannot represent — the corpus caught a 52-CRLF doc losing one
  // char per CRLF on roundtrip. Map CRLF/CR/LF each to ONE soft line break, the
  // same visual Word gives them.
  let soften = text.contains('\n') || text.contains('\r');
  match (strip_object, soften) {
    (false, false) => text,
    (true, false) => text.replace('\u{FFFC}', ""),
    (false, true) => soften_line_endings(&text),
    (true, true) => soften_line_endings(&text.replace('\u{FFFC}', "")),
  }
}

fn soften_line_endings(text: &str) -> String {
  let mut out = String::with_capacity(text.len());
  let mut chars = text.chars().peekable();
  while let Some(ch) = chars.next() {
    match ch {
      '\r' => {
        if chars.peek() == Some(&'\n') {
          chars.next();
        }
        out.push_str(SOFT_LINE_BREAK_STR);
      },
      '\n' => out.push_str(SOFT_LINE_BREAK_STR),
      other => out.push(other),
    }
  }
  out
}

#[hotpath::measure]
fn interpret_cleaned_docx(cleaned: &CleanedDocx) -> io::Result<InterpretedDocx> {
  // §act-eleven A11.2: ONE typed parse + ONE unzip feed rdocx, the facts
  // harvest, AND (via `InterpretedDocx.document`) the structured walk. The
  // cleaner's already-open package supplies styles.xml; the vendored
  // `from_parsed_parts` skips rdocx's own document/numbering/core parses and
  // the `cleaned.bytes` re-inflate. Files whose package or captured
  // document.xml is unavailable fall back to the previous overlapped
  // two-parse path — output is byte-identical either way (corpus-netted).
  let (docx, mut parsed_document, direct_properties) = match (cleaned.package.as_deref(), cleaned.main_document_xml.as_deref()) {
    (Some(package), Some(doc_xml)) => {
      // §act-five P6 carried over: the light streaming run-border pass hides
      // under the one heavy typed parse.
      let probe_t0 = std::time::Instant::now();
      let (document, run_borders) = std::thread::scope(|scope| {
        let borders = scope.spawn(|| direct_run_borders_by_paragraph_xml(doc_xml));
        // §act-twelve A12.3.1-full: fragment-parallel typed parse (equal tree,
        // corpus-netted; falls back to the sequential parse on any anomaly).
        let document = crate::fragment_parse::ct_document_from_xml(doc_xml).map_err(rdocx_oxml_error);
        let borders = borders
          .join()
          .unwrap_or_else(|_| Err(io::Error::other("docx run-border parse thread panicked")));
        (document, borders)
      });
      let probe_parse = probe_t0.elapsed();
      let document = document?;
      let probe_t1 = std::time::Instant::now();
      let facts = direct_paragraph_facts(&document, &run_borders?);
      let docx = RDocxDocument::from_parsed_parts(package, document).map_err(rdocx_error)?;
      static PARSE_PROBE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
      if *PARSE_PROBE.get_or_init(|| std::env::var_os("FLOWSTATE_PARSE_PROBE").is_some()) {
        eprintln!("[flowstate-parse-probe] typed_parse={probe_parse:?} facts+parts={:?}", probe_t1.elapsed());
      }
      (docx, None, facts)
    },
    _ => {
      let (docx, direct_properties) = std::thread::scope(|scope| {
        let direct_properties = scope.spawn(|| match cleaned.main_document_xml.as_deref() {
          Some(doc_xml) => direct_properties_by_paragraph_xml(doc_xml),
          None => direct_properties_by_paragraph_package(&cleaned.bytes),
        });
        let docx = RDocxDocument::from_bytes(&cleaned.bytes).map_err(rdocx_error);
        (
          docx,
          direct_properties
            .join()
            .expect("direct-properties parse thread panicked"),
        )
      });
      let docx = docx?;
      let (document, direct_properties) = direct_properties?;
      (docx, Some(document), direct_properties)
    },
  };
  let probe_walk_t0 = std::time::Instant::now();
  let style_resolver = StyleResolver::new(&docx);
  let docx_paragraphs = docx.paragraphs();
  let mut paragraphs = Vec::with_capacity(docx_paragraphs.len());
  let mut paragraph_property_cache: FxHashMap<Option<String>, CT_PPr> = FxHashMap::default();
  let mut run_property_cache: FxHashMap<(Option<String>, Option<String>), CT_RPr> = FxHashMap::default();
  let mut runs_imported = 0_usize;
  let mut unknown_paragraph_styles = Vec::new();
  let mut unknown_run_styles = Vec::new();
  let mut unknown_paragraph_style_seen = FxHashSet::default();
  let mut unknown_run_style_seen = FxHashSet::default();
  let mut current_section_has_underline = false;
  let mut after_heading_seeking_text = false;

  for (paragraph_ix, paragraph) in docx_paragraphs.into_iter().enumerate() {
    let style_id = paragraph.style_id();
    let paragraph_style_key = style_id.map(str::to_owned);
    let resolved_paragraph_properties = paragraph_property_cache
      .entry(paragraph_style_key.clone())
      .or_insert_with(|| docx.resolve_paragraph_properties(style_id));
    let paragraph_properties = EffectiveParagraphProperties {
      direct_outline_lvl: direct_properties
        .get(paragraph_ix)
        .and_then(|properties| properties.outline_lvl),
      resolved: resolved_paragraph_properties,
    };
    let direct_runs: &[DirectRunProperties] = direct_properties
      .get(paragraph_ix)
      .map_or(&[], |properties| properties.runs.as_slice());
    let run_facts = paragraph
      .runs()
      .enumerate()
      .map(|(run_ix, run)| {
        let text = soften_rdocx_breaks(run.text());
        let run_style_id = run.style_id().map(str::to_owned);
        let run_style_id_ref = run_style_id.as_deref();
        let effective_properties = run_property_cache
          .entry((paragraph_style_key.clone(), run_style_id.clone()))
          .or_insert_with(|| docx.resolve_run_properties(style_id, run_style_id_ref));
        let effective: &CT_RPr = effective_properties;
        let direct = direct_runs.get(run_ix).copied().unwrap_or_default();
        let run_size = run.size();
        let source_size_pt = direct.size_pt.or(run_size);
        RunFact {
          text,
          style_id: run_style_id,
          bold: run.is_bold() || direct.bold || effective.bold == Some(true) || effective.bold_cs == Some(true),
          bold_off: direct.bold_off || (effective.bold == Some(false) && effective.bold_cs != Some(true)),
          underline: direct.underline || underline_is_on(effective.underline.as_ref()),
          strikethrough: direct.strikethrough || effective.strike == Some(true) || effective.dstrike == Some(true),
          highlight: direct.highlight || effective.highlight.is_some() || effective.shading.is_some(),
          border: direct.border,
          source_size_pt,
          size_pt: source_size_pt.or_else(|| effective.sz.map(rdocx_oxml::HalfPoint::to_pt)),
          color: run.color().is_some() || direct.color || effective.color.is_some() || effective.color_theme.is_some(),
          vert_align: if direct.vert_align == VertAlign::Baseline {
            vert_align_from_ooxml(effective.vert_align.as_deref())
          } else {
            direct.vert_align
          },
        }
      })
      .collect::<Vec<_>>();

    let style = recognize_paragraph_style(style_id, &paragraph_properties, &run_facts, &style_resolver);
    if style == ParagraphStyle::Normal
      && let Some(style_id) = style_id
      && !style_resolver.is_known_paragraph_style(style_id)
    {
      push_unique_with_seen(&mut unknown_paragraph_styles, &mut unknown_paragraph_style_seen, style_id);
    }

    let is_heading = matches!(
      style,
      flowstate_document::PARAGRAPH_POCKET
        | flowstate_document::PARAGRAPH_HAT
        | flowstate_document::PARAGRAPH_BLOCK
        | flowstate_document::PARAGRAPH_TAG
        | flowstate_document::PARAGRAPH_ANALYTIC
    );
    let structural_run_formatting_allowed = matches!(
      style,
      flowstate_document::PARAGRAPH_TAG | flowstate_document::PARAGRAPH_ANALYTIC | flowstate_document::PARAGRAPH_UNDERTAG
    );
    let direct_highlight_allowed = !matches!(
      style,
      flowstate_document::PARAGRAPH_POCKET | flowstate_document::PARAGRAPH_HAT | flowstate_document::PARAGRAPH_BLOCK
    );
    let suppress_semantic_styles = matches!(
      style,
      flowstate_document::PARAGRAPH_POCKET
        | flowstate_document::PARAGRAPH_HAT
        | flowstate_document::PARAGRAPH_BLOCK
        | flowstate_document::PARAGRAPH_TAG
        | flowstate_document::PARAGRAPH_ANALYTIC
        | flowstate_document::PARAGRAPH_UNDERTAG
    );
    let mut can_process_citations = false;
    if is_heading {
      current_section_has_underline = false;
      after_heading_seeking_text = true;
    } else {
      #[allow(
        clippy::collapsible_else_if,
        reason = "Collapsing this branch triggers else_if_without_else under the workspace lint set."
      )]
      if after_heading_seeking_text {
        let has_text = run_facts.iter().any(|run| !run.text.trim().is_empty());
        if has_text && style != flowstate_document::PARAGRAPH_UNDERTAG {
          can_process_citations = true;
          after_heading_seeking_text = false;
        }
      }
    }
    if !is_heading && run_facts.iter().any(|run| run.underline && !run.bold) {
      current_section_has_underline = true;
    }

    let bold_paragraph_overrides = if can_process_citations {
      entirely_bold_paragraph_overrides(&run_facts)
    } else {
      None
    };

    let mut runs = Vec::with_capacity(run_facts.len());
    for (run_ix, run) in run_facts.into_iter().enumerate() {
      if run.text.is_empty() {
        continue;
      }
      if let Some(style_id) = run.style_id.as_deref()
        && recognize_run_semantic(style_id, &style_resolver).is_none()
      {
        push_unique_with_seen(&mut unknown_run_styles, &mut unknown_run_style_seen, style_id);
      }

      let styles = recognize_run_styles_for_context(
        &run,
        run_ix,
        bold_paragraph_overrides.as_deref(),
        suppress_semantic_styles,
        structural_run_formatting_allowed,
        direct_highlight_allowed,
        style,
        can_process_citations,
        current_section_has_underline,
        &style_resolver,
      );

      runs.push(DocumentRunInput { text: run.text, styles });
      runs_imported += 1;
    }

    if runs.is_empty() {
      let text = soften_rdocx_breaks(paragraph.text());
      if !text.is_empty() {
        runs.push(DocumentRunInput {
          text,
          styles: RunStyles::default(),
        });
        runs_imported += 1;
      }
    }

    paragraphs.push(DocumentParagraphInput { style, runs });
  }

  let report = DocxConversionReport {
    clean: cleaned.report.clone(),
    recognition_rules: RECOGNITION_RULES,
    paragraphs_imported: paragraphs.len(),
    runs_imported,
    tables_imported: 0,
    images_imported: 0,
    equations_imported: 0,
    unknown_paragraph_styles,
    unknown_run_styles,
  };
  let document = parsed_document
    .take()
    .unwrap_or_else(|| docx.into_typed_document());
  static PARSE_PROBE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
  if *PARSE_PROBE.get_or_init(|| std::env::var_os("FLOWSTATE_PARSE_PROBE").is_some()) {
    eprintln!(
      "[flowstate-parse-probe] conversion_walk={:?} paragraphs={}",
      probe_walk_t0.elapsed(),
      paragraphs.len()
    );
  }
  Ok(InterpretedDocx {
    paragraphs,
    document,
    report,
  })
}
#[derive(Clone, Debug)]
struct RunFact {
  text: String,
  style_id: Option<String>,
  bold: bool,
  bold_off: bool,
  underline: bool,
  strikethrough: bool,
  highlight: bool,
  border: bool,
  source_size_pt: Option<f64>,
  size_pt: Option<f64>,
  color: bool,
  vert_align: VertAlign,
}

#[derive(Clone, Debug, Default)]
struct DirectParagraphFacts {
  outline_lvl: Option<u32>,
  runs: Vec<DirectRunProperties>,
}

#[derive(Clone, Copy, Debug, Default)]
struct DirectRunProperties {
  bold: bool,
  bold_off: bool,
  underline: bool,
  strikethrough: bool,
  highlight: bool,
  border: bool,
  size_pt: Option<f64>,
  color: bool,
  vert_align: VertAlign,
}

struct EffectiveParagraphProperties<'properties> {
  direct_outline_lvl: Option<u32>,
  resolved: &'properties CT_PPr,
}

#[hotpath::measure_all]
impl ParagraphProperties for EffectiveParagraphProperties<'_> {
  fn outline_lvl(&self) -> Option<u32> {
    self.direct_outline_lvl.or(self.resolved.outline_lvl)
  }
}

#[hotpath::measure]
fn direct_properties_by_paragraph_package(bytes: &[u8]) -> io::Result<(CT_Document, Vec<DirectParagraphFacts>)> {
  let package = OpcPackage::from_reader(std::io::Cursor::new(bytes)).map_err(rdocx_opc_error)?;
  let doc_part_name = package
    .main_document_part()
    .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "DOCX package has no main document part"))?;
  let doc_xml = package
    .get_part(&doc_part_name)
    .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "DOCX package has no main document XML"))?;
  direct_properties_by_paragraph_xml(doc_xml)
}

/// Returns the typed `CT_Document` ALONGSIDE the harvested facts (§act-nine
/// A9.2): the same parse also feeds the structured object walk, which used to
/// re-derive an owned `XmlNode` tree from the same bytes.
#[hotpath::measure]
fn direct_properties_by_paragraph_xml(doc_xml: &[u8]) -> io::Result<(CT_Document, Vec<DirectParagraphFacts>)> {
  // §act-five P6: the two INDEPENDENT parses of the same bytes — the heavy
  // structured `CT_Document::from_xml` and the light streaming run-border pass —
  // overlap on a scoped thread so the border parse hides under the structured
  // one (both borrow `doc_xml` immutably; the scope guarantees it outlives them).
  let (document, run_borders_by_paragraph) = std::thread::scope(|scope| {
    let borders = scope.spawn(|| direct_run_borders_by_paragraph_xml(doc_xml));
    let document = crate::fragment_parse::ct_document_from_xml(doc_xml).map_err(rdocx_oxml_error);
    let borders = borders
      .join()
      .unwrap_or_else(|_| Err(io::Error::other("docx run-border parse thread panicked")));
    (document, borders)
  });
  let document = document?;
  let facts = direct_paragraph_facts(&document, &run_borders_by_paragraph?);
  Ok((document, facts))
}

/// §act-eleven A11.2: the facts harvest over an already-parsed typed tree —
/// shared by the single-parse fast path (which hands the tree straight to
/// rdocx afterward) and the legacy overlapped path above.
#[hotpath::measure]
fn direct_paragraph_facts(document: &CT_Document, run_borders_by_paragraph: &[Vec<bool>]) -> Vec<DirectParagraphFacts> {
  document
    .body
    .paragraphs()
    .enumerate()
    .map(|(paragraph_ix, paragraph)| {
      let paragraph_run_borders = run_borders_by_paragraph
        .get(paragraph_ix)
        .map(Vec::as_slice)
        .unwrap_or_default();
      let runs = paragraph
        .runs
        .iter()
        .enumerate()
        .map(|(run_ix, run)| {
          let Some(properties) = run.properties.as_ref() else {
            return DirectRunProperties {
              border: paragraph_run_borders
                .get(run_ix)
                .copied()
                .unwrap_or_default(),
              ..DirectRunProperties::default()
            };
          };
          DirectRunProperties {
            bold: properties.bold == Some(true) || properties.bold_cs == Some(true),
            bold_off: properties.bold == Some(false) && properties.bold_cs != Some(true),
            underline: underline_is_on(properties.underline.as_ref()),
            strikethrough: properties.strike == Some(true) || properties.dstrike == Some(true),
            highlight: properties.highlight.is_some() || properties.shading.is_some(),
            border: paragraph_run_borders
              .get(run_ix)
              .copied()
              .unwrap_or_default(),
            size_pt: properties.sz.map(|size| size.to_pt()),
            color: properties.color.is_some() || properties.color_theme.is_some(),
            vert_align: vert_align_from_ooxml(properties.vert_align.as_deref()),
          }
        })
        .collect();
      DirectParagraphFacts {
        outline_lvl: paragraph
          .properties
          .as_ref()
          .and_then(|properties| properties.outline_lvl),
        runs,
      }
    })
    .collect()
}

#[hotpath::measure]
fn direct_run_borders_by_paragraph_xml(doc_xml: &[u8]) -> io::Result<Vec<Vec<bool>>> {
  let mut reader = XmlReader::from_reader(doc_xml);
  reader.config_mut().trim_text(false);
  let mut paragraphs = Vec::new();
  let mut current_paragraph: Option<Vec<bool>> = None;
  let mut in_run = false;
  let mut in_run_properties = false;
  let mut current_run_border = false;

  loop {
    // §A14.5.1: borrowed events — `read_event_into` copies every event into
    // the buffer, and this pass only inspects names/attrs.
    match reader.read_event() {
      Ok(Event::Start(event)) if local_name_is(event.name().as_ref(), b"p") => {
        current_paragraph = Some(Vec::new());
      },
      Ok(Event::End(event)) if local_name_is(event.name().as_ref(), b"p") => {
        if let Some(paragraph) = current_paragraph.take() {
          paragraphs.push(paragraph);
        }
      },
      Ok(Event::Start(event)) if current_paragraph.is_some() && local_name_is(event.name().as_ref(), b"r") => {
        in_run = true;
        current_run_border = false;
      },
      Ok(Event::End(event)) if in_run && local_name_is(event.name().as_ref(), b"r") => {
        if let Some(paragraph) = &mut current_paragraph {
          paragraph.push(current_run_border);
        }
        in_run = false;
        in_run_properties = false;
        current_run_border = false;
      },
      Ok(Event::Start(event)) if in_run && local_name_is(event.name().as_ref(), b"rPr") => {
        in_run_properties = true;
      },
      Ok(Event::End(event)) if in_run_properties && local_name_is(event.name().as_ref(), b"rPr") => {
        in_run_properties = false;
      },
      Ok(Event::Empty(event)) if in_run_properties && local_name_is(event.name().as_ref(), b"bdr") => {
        current_run_border |= border_is_on(&event)?;
      },
      Ok(Event::Eof) => break,
      Err(error) => return Err(io::Error::new(io::ErrorKind::InvalidData, error)),
      _ => {},
    }
  }

  Ok(paragraphs)
}

#[hotpath::measure]
fn border_is_on(event: &BytesStart<'_>) -> io::Result<bool> {
  for attr in event.attributes() {
    let attr = attr.map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if local_name_is(attr.key.as_ref(), b"val") {
      let value = std::str::from_utf8(attr.value.as_ref()).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
      return Ok(!matches!(value, "nil" | "none"));
    }
  }
  Ok(true)
}

#[hotpath::measure]
fn local_name_is(name: &[u8], expected: &[u8]) -> bool {
  name == expected
    || name
      .strip_prefix(b"w:")
      .is_some_and(|local| local == expected)
}

struct StyleResolver {
  names_by_id: FxHashMap<String, String>,
  known_paragraph_style_ids: FxHashSet<String>,
  paragraph_styles_by_id: FxHashMap<String, Option<ParagraphStyle>>,
  character_heading_styles_by_id: FxHashMap<String, Option<ParagraphStyle>>,
  run_semantics_by_id: FxHashMap<String, Option<RunSemanticStyle>>,
}

#[hotpath::measure_all]
impl StyleResolver {
  fn new(docx: &RDocxDocument) -> Self {
    let mut names_by_id = FxHashMap::default();
    let mut known_paragraph_style_ids = FxHashSet::default();
    let mut paragraph_styles_by_id = FxHashMap::default();
    let mut character_heading_styles_by_id = FxHashMap::default();
    let mut run_semantics_by_id = FxHashMap::default();

    for style in docx.styles() {
      let style_id = style.style_id();
      let canonical_source = style.name().unwrap_or(style_id);
      if matches!(
        canonical_paragraph_style_name(canonical_source),
        Some("Heading1" | "Heading2" | "Heading3" | "Heading4" | "Analytic" | "Undertag" | "Normal")
      ) {
        known_paragraph_style_ids.insert(style_id.to_owned());
      }
      let style_id = style_id.to_owned();
      paragraph_styles_by_id.insert(style_id.clone(), paragraph_style_from_canonical_name(canonical_source));
      character_heading_styles_by_id.insert(style_id.clone(), paragraph_style_from_character_heading_name(canonical_source));
      run_semantics_by_id.insert(style_id.clone(), run_semantic_from_canonical_name(canonical_source));
      if let Some(name) = style.name() {
        names_by_id.insert(style_id, name.to_owned());
      }
    }

    Self {
      names_by_id,
      known_paragraph_style_ids,
      paragraph_styles_by_id,
      character_heading_styles_by_id,
      run_semantics_by_id,
    }
  }

  fn name(&self, style_id: &str) -> Option<&str> {
    self.names_by_id.get(style_id).map(String::as_str)
  }

  fn canonical_name<'style>(&'style self, style_id: Option<&'style str>) -> &'style str {
    style_id
      .and_then(|id| self.name(id))
      .unwrap_or_else(|| style_id.unwrap_or("Normal"))
  }

  fn is_known_paragraph_style(&self, style_id: &str) -> bool {
    self.known_paragraph_style_ids.contains(style_id)
      || matches!(
        canonical_paragraph_style_name(self.canonical_name(Some(style_id))),
        Some("Heading1" | "Heading2" | "Heading3" | "Heading4" | "Analytic" | "Undertag" | "Normal")
      )
  }

  fn paragraph_style(&self, style_id: Option<&str>) -> Option<ParagraphStyle> {
    let style_id = style_id?;
    if let Some(style) = self.paragraph_styles_by_id.get(style_id) {
      return *style;
    }
    paragraph_style_from_canonical_name(self.canonical_name(Some(style_id)))
  }

  fn character_heading_style(&self, style_id: &str) -> Option<ParagraphStyle> {
    if let Some(style) = self.character_heading_styles_by_id.get(style_id) {
      return *style;
    }
    paragraph_style_from_character_heading_name(self.canonical_name(Some(style_id)))
  }

  fn run_semantic(&self, style_id: &str) -> Option<RunSemanticStyle> {
    if let Some(semantic) = self.run_semantics_by_id.get(style_id) {
      return *semantic;
    }
    run_semantic_from_canonical_name(self.canonical_name(Some(style_id)))
  }
}

#[hotpath::measure]
fn recognize_paragraph_style(
  style_id: Option<&str>,
  paragraph_properties: &impl ParagraphProperties,
  runs: &[RunFact],
  styles: &StyleResolver,
) -> ParagraphStyle {
  if let Some(style) = styles.paragraph_style(style_id) {
    return style;
  }

  if let Some(style) = paragraph_style_from_character_heading_runs(runs, styles) {
    return style;
  }

  if paragraph_properties.outline_lvl() == Some(0) && runs.iter().any(|run| run.bold && run.size_pt == Some(26.0)) {
    return flowstate_document::PARAGRAPH_POCKET;
  }
  if paragraph_properties.outline_lvl() == Some(1) && runs.iter().any(|run| run.bold && run.size_pt == Some(22.0)) {
    return flowstate_document::PARAGRAPH_HAT;
  }
  if paragraph_properties.outline_lvl() == Some(2)
    && runs
      .iter()
      .any(|run| run.bold && run.underline && run.size_pt == Some(16.0))
  {
    return flowstate_document::PARAGRAPH_BLOCK;
  }
  if paragraph_properties.outline_lvl() == Some(3) && runs.iter().any(|run| run.bold && run.color) {
    return flowstate_document::PARAGRAPH_TAG;
  }

  ParagraphStyle::Normal
}

trait ParagraphProperties {
  fn outline_lvl(&self) -> Option<u32>;
}

#[hotpath::measure_all]
impl ParagraphProperties for rdocx_oxml::properties::CT_PPr {
  fn outline_lvl(&self) -> Option<u32> {
    self.outline_lvl
  }
}

#[hotpath::measure]
fn recognize_run_semantic(style_id: &str, styles: &StyleResolver) -> Option<RunSemanticStyle> {
  styles.run_semantic(style_id)
}

#[hotpath::measure]
pub(crate) fn run_semantic_from_canonical_name(name: &str) -> Option<RunSemanticStyle> {
  match canonical_run_style_name(name) {
    Some("Style13ptBold") => Some(flowstate_document::SEMANTIC_CITE),
    Some("Emphasis") => Some(flowstate_document::SEMANTIC_EMPHASIS),
    Some("StyleUnderline") => Some(flowstate_document::SEMANTIC_UNDERLINE),
    _ => None,
  }
}

#[hotpath::measure]
fn recognize_run_styles_for_context(
  run: &RunFact,
  run_ix: usize,
  bold_paragraph_overrides: Option<&[bool]>,
  suppress_semantic_styles: bool,
  structural_run_formatting_allowed: bool,
  direct_highlight_allowed: bool,
  paragraph_style: ParagraphStyle,
  can_process_citations: bool,
  current_section_has_underline: bool,
  styles: &StyleResolver,
) -> RunStyles {
  RunStyles {
    semantic: recognize_run_semantic_for_context(
      run,
      run_ix,
      bold_paragraph_overrides,
      suppress_semantic_styles,
      paragraph_style,
      can_process_citations,
      current_section_has_underline,
      styles,
    ),
    direct_underline: structural_run_formatting_allowed && run.underline,
    strikethrough: !suppress_semantic_styles && run.strikethrough,
    highlight: (direct_highlight_allowed && run.highlight).then_some(flowstate_document::HIGHLIGHT_SPOKEN),
    // Vertical alignment is orthogonal to the debate semantics and is preserved
    // as-is (a `CO₂` subscript in a Tag keeps its subscript) — losslessly carried
    // even where the semantic slot is suppressed for headings.
    vert_align: run.vert_align,
  }
}

#[hotpath::measure]
fn recognize_run_semantic_for_context(
  run: &RunFact,
  run_ix: usize,
  bold_paragraph_overrides: Option<&[bool]>,
  suppress_semantic_styles: bool,
  paragraph_style: ParagraphStyle,
  can_process_citations: bool,
  current_section_has_underline: bool,
  styles: &StyleResolver,
) -> RunSemanticStyle {
  if suppress_semantic_styles {
    return RunSemanticStyle::default();
  }

  if run.border {
    return flowstate_document::SEMANTIC_EMPHASIS;
  }

  let explicit = run
    .style_id
    .as_deref()
    .and_then(|style_id| recognize_run_semantic(style_id, styles));

  if run.bold_off && explicit == Some(flowstate_document::SEMANTIC_CITE) {
    return RunSemanticStyle::default();
  }
  if explicit == Some(flowstate_document::SEMANTIC_CITE) && !can_process_citations && !run.underline {
    return if run.highlight {
      flowstate_document::SEMANTIC_UNDERLINE
    } else {
      RunSemanticStyle::default()
    };
  }
  if let Some(overrides) = bold_paragraph_overrides
    && overrides.get(run_ix) == Some(&true)
  {
    return flowstate_document::SEMANTIC_CITE;
  }
  if can_process_citations
    && run.bold
    && !matches!(
      explicit,
      Some(flowstate_document::SEMANTIC_UNDERLINE | flowstate_document::SEMANTIC_EMPHASIS)
    )
  {
    return flowstate_document::SEMANTIC_CITE;
  }
  if run.underline && !run.bold && !matches!(explicit, Some(flowstate_document::SEMANTIC_EMPHASIS | flowstate_document::SEMANTIC_CITE)) {
    return flowstate_document::SEMANTIC_UNDERLINE;
  }
  if run.bold && run.underline {
    return if current_section_has_underline {
      flowstate_document::SEMANTIC_EMPHASIS
    } else {
      flowstate_document::SEMANTIC_UNDERLINE
    };
  }
  if run.highlight && explicit.is_none() {
    return flowstate_document::SEMANTIC_UNDERLINE;
  }
  let semantic = explicit.unwrap_or_default();
  if semantic == RunSemanticStyle::Plain
    && paragraph_style == ParagraphStyle::Normal
    && !run.underline
    && !run.highlight
    && run.source_size_pt.is_some_and(|size| size <= 8.0)
  {
    return flowstate_document::SEMANTIC_CONDENSED;
  }
  semantic
}

#[hotpath::measure]
fn entirely_bold_paragraph_overrides(runs: &[RunFact]) -> Option<Vec<bool>> {
  let text_run_indices = runs
    .iter()
    .enumerate()
    .filter_map(|(ix, run)| (!run.text.trim().is_empty()).then_some(ix))
    .collect::<Vec<_>>();
  if text_run_indices.is_empty() || text_run_indices.iter().any(|ix| !runs[*ix].bold) {
    return None;
  }

  let paragraph_text_len = text_run_indices
    .iter()
    .fold((0_usize, true, 0_usize), |(count, leading, pending_whitespace), ix| {
      count_trimmed_chars(&runs[*ix].text, count, leading, pending_whitespace)
    })
    .0;
  let mut cite = vec![false; runs.len()];
  if paragraph_text_len <= 60 {
    for ix in text_run_indices {
      cite[ix] = true;
    }
    return Some(cite);
  }

  if let Some(base_size) = most_common_half_point_size(runs, &text_run_indices) {
    let mut found = false;
    for ix in &text_run_indices {
      if runs[*ix].size_pt.is_some_and(|size| size > base_size + 0.5) {
        cite[*ix] = true;
        found = true;
      }
    }
    if found {
      return Some(cite);
    }
  }

  let highlighted = text_run_indices
    .iter()
    .filter(|ix| runs[**ix].highlight)
    .copied()
    .collect::<Vec<_>>();
  if !highlighted.is_empty() {
    for ix in highlighted {
      cite[ix] = true;
    }
    return Some(cite);
  }

  if let Some(first_digit_run) = text_run_indices
    .iter()
    .position(|ix| runs[*ix].text.chars().any(|ch| ch.is_ascii_digit()))
  {
    for ix in text_run_indices.iter().take(first_digit_run + 1) {
      cite[*ix] = true;
    }
    return Some(cite);
  }

  for ix in text_run_indices {
    cite[ix] = true;
  }
  Some(cite)
}

#[hotpath::measure]
fn count_trimmed_chars(text: &str, mut count: usize, mut leading: bool, mut pending_whitespace: usize) -> (usize, bool, usize) {
  for ch in text.chars() {
    if ch.is_whitespace() {
      if !leading {
        pending_whitespace += 1;
      }
    } else {
      leading = false;
      count += pending_whitespace + 1;
      pending_whitespace = 0;
    }
  }
  (count, leading, pending_whitespace)
}

#[hotpath::measure]
fn most_common_half_point_size(runs: &[RunFact], indices: &[usize]) -> Option<f64> {
  let mut counts: FxHashMap<i32, usize> = FxHashMap::default();
  for ix in indices {
    let Some(size) = runs[*ix].size_pt else {
      continue;
    };
    if (6.0..=72.0).contains(&size) {
      *counts.entry((size * 2.0).round() as i32).or_default() += 1;
    }
  }
  counts
    .into_iter()
    .max_by(|(size_a, count_a), (size_b, count_b)| count_a.cmp(count_b).then_with(|| size_b.cmp(size_a)))
    .map(|(half_points, _)| f64::from(half_points) / 2.0)
}

#[hotpath::measure]
fn canonical_paragraph_style_name(name: &str) -> Option<&'static str> {
  match normalized_style_token(name).as_str() {
    "normal" => Some("Normal"),
    "heading1" | "pocket" => Some("Heading1"),
    "heading2" | "hat" => Some("Heading2"),
    "heading3" | "block" => Some("Heading3"),
    "heading4" | "tag" => Some("Heading4"),
    "analytic" | "analytics" => Some("Analytic"),
    "undertag" => Some("Undertag"),
    _ => None,
  }
}

#[hotpath::measure]
pub(crate) fn paragraph_style_from_canonical_name(name: &str) -> Option<ParagraphStyle> {
  match canonical_paragraph_style_name(name) {
    Some("Heading1") => Some(flowstate_document::PARAGRAPH_POCKET),
    Some("Heading2") => Some(flowstate_document::PARAGRAPH_HAT),
    Some("Heading3") => Some(flowstate_document::PARAGRAPH_BLOCK),
    Some("Heading4") => Some(flowstate_document::PARAGRAPH_TAG),
    Some("Analytic") => Some(flowstate_document::PARAGRAPH_ANALYTIC),
    Some("Undertag") => Some(flowstate_document::PARAGRAPH_UNDERTAG),
    _ => None,
  }
}

#[hotpath::measure]
fn paragraph_style_from_character_heading_runs(runs: &[RunFact], styles: &StyleResolver) -> Option<ParagraphStyle> {
  let mut inferred = None;
  let mut saw_text = false;
  for run in runs.iter().filter(|run| !run.text.trim().is_empty()) {
    saw_text = true;
    let Some(style_id) = run.style_id.as_deref() else {
      continue;
    };
    let style = styles.character_heading_style(style_id)?;
    if inferred.is_some_and(|existing| existing != style) {
      return None;
    }
    inferred = Some(style);
  }
  saw_text.then_some(inferred).flatten()
}

#[hotpath::measure]
fn paragraph_style_from_character_heading_name(name: &str) -> Option<ParagraphStyle> {
  match normalized_style_token(name).as_str() {
    "heading1char" | "pocketchar" => Some(flowstate_document::PARAGRAPH_POCKET),
    "heading2char" | "hatchar" => Some(flowstate_document::PARAGRAPH_HAT),
    "heading3char" | "blockchar" => Some(flowstate_document::PARAGRAPH_BLOCK),
    "heading4char" | "tagchar" => Some(flowstate_document::PARAGRAPH_TAG),
    _ => None,
  }
}

#[hotpath::measure]
fn canonical_run_style_name(name: &str) -> Option<&'static str> {
  match normalized_style_token(name).as_str() {
    "style13ptbold" | "cite" | "oldcite" | "heading1char" | "pocketchar" => Some("Style13ptBold"),
    "styleunderline" | "underline" => Some("StyleUnderline"),
    "emphasis" | "heading2char" | "hatchar" | "heading3char" | "blockchar" | "heading4char" | "tagchar" => Some("Emphasis"),
    _ => None,
  }
}

#[hotpath::measure]
fn normalized_style_token(name: &str) -> String {
  name
    .chars()
    .filter(char::is_ascii_alphanumeric)
    .flat_map(char::to_lowercase)
    .collect()
}

#[hotpath::measure]
fn underline_is_on(underline: Option<&ST_Underline>) -> bool {
  matches!(underline, Some(value) if *value != ST_Underline::None)
}

#[hotpath::measure]
fn push_unique_with_seen(values: &mut Vec<String>, seen: &mut FxHashSet<String>, value: &str) {
  if !seen.contains(value) {
    let value = value.to_owned();
    seen.insert(value.clone());
    values.push(value);
  }
}

#[hotpath::measure]
fn rdocx_error(error: rdocx::Error) -> io::Error {
  io::Error::new(io::ErrorKind::InvalidData, error)
}

#[hotpath::measure]
fn rdocx_opc_error(error: rdocx_opc::OpcError) -> io::Error {
  io::Error::new(io::ErrorKind::InvalidData, error)
}

#[hotpath::measure]
fn rdocx_oxml_error(error: rdocx_oxml::error::OxmlError) -> io::Error {
  io::Error::new(io::ErrorKind::InvalidData, error)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[derive(Default)]
  struct TestParagraphProperties {
    outline_lvl: Option<u32>,
  }

  #[hotpath::measure_all]
  impl ParagraphProperties for TestParagraphProperties {
    fn outline_lvl(&self) -> Option<u32> {
      self.outline_lvl
    }
  }

  #[hotpath::measure]
  fn style_resolver() -> StyleResolver {
    StyleResolver {
      names_by_id: FxHashMap::from_iter([
        ("Heading3Char".to_string(), "Heading 3 Char".to_string()),
        ("BlockChar".to_string(), "Block Char".to_string()),
        ("Emphasis".to_string(), "Emphasis".to_string()),
        ("Heading3".to_string(), "Heading 3".to_string()),
      ]),
      known_paragraph_style_ids: FxHashSet::from_iter(["Heading3".to_string()]),
      paragraph_styles_by_id: FxHashMap::from_iter([("Heading3".to_string(), Some(flowstate_document::PARAGRAPH_BLOCK))]),
      character_heading_styles_by_id: FxHashMap::from_iter([
        ("Heading3Char".to_string(), Some(flowstate_document::PARAGRAPH_BLOCK)),
        ("BlockChar".to_string(), Some(flowstate_document::PARAGRAPH_BLOCK)),
        ("Emphasis".to_string(), None),
      ]),
      run_semantics_by_id: FxHashMap::from_iter([
        ("Heading3Char".to_string(), Some(flowstate_document::SEMANTIC_EMPHASIS)),
        ("BlockChar".to_string(), Some(flowstate_document::SEMANTIC_EMPHASIS)),
        ("Emphasis".to_string(), Some(flowstate_document::SEMANTIC_EMPHASIS)),
      ]),
    }
  }

  #[hotpath::measure]
  fn run(style_id: Option<&str>, text: &str) -> RunFact {
    RunFact {
      text: text.to_string(),
      style_id: style_id.map(str::to_string),
      bold: false,
      bold_off: false,
      underline: false,
      strikethrough: false,
      highlight: false,
      border: false,
      source_size_pt: None,
      size_pt: None,
      color: false,
      vert_align: VertAlign::Baseline,
    }
  }

  #[test]
  #[hotpath::measure]
  fn block_character_style_reconstructs_block_paragraph() {
    let styles = style_resolver();
    let runs = [run(Some("Heading3Char"), "Plan text")];

    assert_eq!(
      recognize_paragraph_style(None, &TestParagraphProperties::default(), &runs, &styles),
      flowstate_document::PARAGRAPH_BLOCK
    );
  }

  #[test]
  #[hotpath::measure]
  fn direct_outline_level_and_formatting_reconstruct_block_paragraph() {
    let styles = style_resolver();
    let mut target_run = run(None, "2NC---AT: US Draw-In");
    target_run.bold = true;
    target_run.underline = true;
    target_run.size_pt = Some(16.0);
    let runs = [target_run];
    let paragraph_properties = TestParagraphProperties { outline_lvl: Some(2) };

    assert_eq!(
      recognize_paragraph_style(None, &paragraph_properties, &runs, &styles),
      flowstate_document::PARAGRAPH_BLOCK
    );

    let run_styles = recognize_run_styles_for_context(
      &runs[0],
      0,
      None,
      true,
      false,
      false,
      flowstate_document::PARAGRAPH_BLOCK,
      false,
      false,
      &styles,
    );
    assert_eq!(run_styles.semantic, RunSemanticStyle::Plain);
    assert!(!run_styles.direct_underline);
    assert_eq!(run_styles.highlight, None);
  }

  #[test]
  #[hotpath::measure]
  fn character_heading_used_as_structure_does_not_become_emphasis() {
    let styles = style_resolver();
    let run = run(Some("Heading3Char"), "Plan text");

    assert_eq!(
      recognize_run_semantic_for_context(&run, 0, None, true, flowstate_document::PARAGRAPH_BLOCK, false, false, &styles,),
      RunSemanticStyle::Plain
    );
  }

  #[test]
  #[hotpath::measure]
  fn ordinary_emphasis_is_rejected_in_heading_paragraphs() {
    let styles = style_resolver();
    let run = run(Some("Emphasis"), "important");

    assert_eq!(
      recognize_run_semantic_for_context(&run, 0, None, true, flowstate_document::PARAGRAPH_BLOCK, false, false, &styles,),
      RunSemanticStyle::Plain
    );
  }

  #[test]
  #[hotpath::measure]
  fn block_paragraph_rejects_direct_run_formatting() {
    let styles = style_resolver();
    let mut run = run(Some("Emphasis"), "important");
    run.underline = true;
    run.strikethrough = true;
    run.highlight = true;

    let run_styles = recognize_run_styles_for_context(
      &run,
      0,
      None,
      true,
      false,
      false,
      flowstate_document::PARAGRAPH_BLOCK,
      false,
      false,
      &styles,
    );

    assert_eq!(run_styles.semantic, RunSemanticStyle::Plain);
    assert!(!run_styles.direct_underline);
    assert!(!run_styles.strikethrough);
    assert_eq!(run_styles.highlight, None);
  }

  #[test]
  #[hotpath::measure]
  fn tag_paragraph_only_preserves_direct_underline_and_highlight() {
    let styles = style_resolver();
    let mut run = run(Some("Emphasis"), "important");
    run.underline = true;
    run.strikethrough = true;
    run.highlight = true;

    let run_styles = recognize_run_styles_for_context(&run, 0, None, true, true, true, flowstate_document::PARAGRAPH_TAG, false, false, &styles);

    assert_eq!(run_styles.semantic, RunSemanticStyle::Plain);
    assert!(run_styles.direct_underline);
    assert!(!run_styles.strikethrough);
    assert_eq!(run_styles.highlight, Some(flowstate_document::HIGHLIGHT_SPOKEN));
  }

  #[test]
  #[hotpath::measure]
  fn normal_paragraph_preserves_direct_highlight() {
    let styles = style_resolver();
    let mut run = run(None, "spoken text");
    run.highlight = true;

    let run_styles = recognize_run_styles_for_context(&run, 0, None, false, false, true, ParagraphStyle::Normal, false, false, &styles);

    assert_eq!(run_styles.highlight, Some(flowstate_document::HIGHLIGHT_SPOKEN));
  }
}
