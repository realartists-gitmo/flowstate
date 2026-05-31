use std::{cmp::Ordering, io, path::Path};

use flowstate_document::{
  Document, DocumentParagraphInput, DocumentRunInput, DocumentTheme, HighlightStyle, ParagraphStyle, RunSemanticStyle, RunStyles,
  document_from_paragraphs, write_db8,
};
use pdf_oxide::{
  Annotation, PdfDocument,
  annotation_types::AnnotationSubtype,
  elements::PathContent,
  geometry::Rect,
  layout::{Color, TextSpan},
};
use rustc_hash::FxHashMap;

pub const PDF_RECOGNITION_RULES: &[PdfRecognitionRule] = &[
  PdfRecognitionRule::FontSizeHeading,
  PdfRecognitionRule::BoldUnderlineBlock,
  PdfRecognitionRule::BoldColoredAnalytic,
  PdfRecognitionRule::ItalicColoredUndertag,
  PdfRecognitionRule::TextMarkupAnnotations,
  PdfRecognitionRule::VectorUnderline,
  PdfRecognitionRule::VectorHighlight,
  PdfRecognitionRule::VectorBorder,
  PdfRecognitionRule::FirstTextAfterHeadingCite,
  PdfRecognitionRule::AllBoldFirstTextCite,
  PdfRecognitionRule::SmallFontCondensed,
  PdfRecognitionRule::FailClosedConfidence,
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PdfRecognitionRule {
  FontSizeHeading,
  BoldUnderlineBlock,
  BoldColoredAnalytic,
  ItalicColoredUndertag,
  TextMarkupAnnotations,
  VectorUnderline,
  VectorHighlight,
  VectorBorder,
  FirstTextAfterHeadingCite,
  AllBoldFirstTextCite,
  SmallFontCondensed,
  FailClosedConfidence,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PdfImportDecision {
  Imported,
  Rejected,
}

#[derive(Clone, Debug)]
pub struct PdfConversionReport {
  pub recognition_rules: &'static [PdfRecognitionRule],
  pub decision: PdfImportDecision,
  pub rejection_reason: Option<String>,
  pub confidence: f32,
  pub pages_scanned: usize,
  pub spans_imported: usize,
  pub paragraphs_imported: usize,
  pub runs_imported: usize,
  pub structural_hits: usize,
  pub high_confidence_structural_hits: usize,
  pub semantic_hits: usize,
  pub annotation_highlights: usize,
  pub annotation_underlines: usize,
  pub annotation_strikethroughs: usize,
  pub vector_highlights: usize,
  pub vector_underlines: usize,
  pub vector_strikethroughs: usize,
  pub vector_borders: usize,
}

#[derive(Clone, Debug, Default)]
struct PdfImportStats {
  pages_scanned: usize,
  spans_imported: usize,
  paragraphs_imported: usize,
  runs_imported: usize,
  structural_hits: usize,
  high_confidence_structural_hits: usize,
  semantic_hits: usize,
  annotation_highlights: usize,
  annotation_underlines: usize,
  annotation_strikethroughs: usize,
  vector_highlights: usize,
  vector_underlines: usize,
  vector_strikethroughs: usize,
  vector_borders: usize,
}

#[derive(Clone, Debug)]
struct PdfRunFact {
  text: String,
  bbox: Rect,
  bold: bool,
  italic: bool,
  underline: bool,
  strikethrough: bool,
  highlight: bool,
  border: bool,
  font_size: f32,
  color: Color,
}

#[derive(Clone, Debug)]
struct PdfLineFact {
  runs: Vec<PdfRunFact>,
  bbox: Rect,
  center_y: f32,
  style: ParagraphStyle,
  high_confidence_style: bool,
}

#[derive(Clone, Debug)]
struct PdfParagraphFact {
  style: ParagraphStyle,
  runs: Vec<PdfRunFact>,
  high_confidence_style: bool,
}

#[derive(Clone, Copy, Debug)]
enum InterLineJoin {
  Space,
  None,
}

#[hotpath::measure]
pub fn convert_pdf_to_document(path: impl AsRef<Path>) -> io::Result<(Document, PdfConversionReport)> {
  let mut import = import_pdf_candidate(path)?;
  if let Some(reason) = rejection_reason(&import.report) {
    import.report.decision = PdfImportDecision::Rejected;
    import.report.rejection_reason = Some(reason.clone());
    return Err(io::Error::new(
      io::ErrorKind::InvalidData,
      format!("PDF does not match Flowstate debate-document import heuristics: {reason}"),
    ));
  }
  import.report.decision = PdfImportDecision::Imported;
  Ok((import.document, import.report))
}

#[hotpath::measure]
pub fn analyze_pdf_import(path: impl AsRef<Path>) -> io::Result<PdfConversionReport> {
  let mut import = import_pdf_candidate(path)?;
  if let Some(reason) = rejection_reason(&import.report) {
    import.report.decision = PdfImportDecision::Rejected;
    import.report.rejection_reason = Some(reason);
  } else {
    import.report.decision = PdfImportDecision::Imported;
  }
  Ok(import.report)
}

#[hotpath::measure]
pub fn convert_recognized_pdf_to_db8(input_pdf: impl AsRef<Path>, output_db8: impl AsRef<Path>) -> io::Result<PdfConversionReport> {
  let (document, report) = convert_pdf_to_document(input_pdf)?;
  write_db8(output_db8, &document)?;
  Ok(report)
}

struct PdfImport {
  document: Document,
  report: PdfConversionReport,
}

#[hotpath::measure]
fn import_pdf_candidate(path: impl AsRef<Path>) -> io::Result<PdfImport> {
  let mut pdf = PdfDocument::open(path).map_err(pdf_error)?;
  let page_count = pdf.page_count().map_err(pdf_error)?;
  let mut stats = PdfImportStats {
    pages_scanned: page_count,
    ..PdfImportStats::default()
  };
  let mut all_paragraphs = Vec::new();

  for page_ix in 0..page_count {
    let spans = pdf.extract_spans(page_ix).map_err(pdf_error)?;
    let annotations = pdf.get_annotations(page_ix).unwrap_or_default();
    let lines = pdf.extract_lines(page_ix).unwrap_or_default();
    let rects = pdf.extract_rects(page_ix).unwrap_or_default();
    let mut page_runs = page_runs_from_spans(spans, &annotations, &lines, &rects, &mut stats);
    if page_runs.is_empty() {
      continue;
    }
    let median_font_size = median_font_size(&page_runs).unwrap_or(11.0);
    let page_lines = page_lines_from_runs(&mut page_runs, median_font_size);
    all_paragraphs.extend(page_paragraphs_from_lines(page_lines, median_font_size));
  }

  stats.spans_imported = all_paragraphs
    .iter()
    .map(|paragraph| paragraph.runs.len())
    .sum();
  let document_paragraphs = document_paragraphs_from_pdf_paragraphs(all_paragraphs, &mut stats);
  let document = document_from_paragraphs(DocumentTheme::default(), document_paragraphs);
  let confidence = confidence(&stats);
  let report = PdfConversionReport {
    recognition_rules: PDF_RECOGNITION_RULES,
    decision: PdfImportDecision::Rejected,
    rejection_reason: None,
    confidence,
    pages_scanned: stats.pages_scanned,
    spans_imported: stats.spans_imported,
    paragraphs_imported: stats.paragraphs_imported,
    runs_imported: stats.runs_imported,
    structural_hits: stats.structural_hits,
    high_confidence_structural_hits: stats.high_confidence_structural_hits,
    semantic_hits: stats.semantic_hits,
    annotation_highlights: stats.annotation_highlights,
    annotation_underlines: stats.annotation_underlines,
    annotation_strikethroughs: stats.annotation_strikethroughs,
    vector_highlights: stats.vector_highlights,
    vector_underlines: stats.vector_underlines,
    vector_strikethroughs: stats.vector_strikethroughs,
    vector_borders: stats.vector_borders,
  };
  Ok(PdfImport { document, report })
}

#[hotpath::measure]
fn page_runs_from_spans(
  spans: Vec<TextSpan>,
  annotations: &[Annotation],
  lines: &[PathContent],
  rects: &[PathContent],
  stats: &mut PdfImportStats,
) -> Vec<PdfRunFact> {
  let annotation_boxes = AnnotationBoxes::new(annotations);
  spans
    .into_iter()
    .filter_map(|span| {
      let text = normalize_span_text(span.text);
      if text.trim().is_empty() || span.bbox.width <= 0.0 || span.bbox.height <= 0.0 {
        return None;
      }

      let bbox = span.bbox;
      let underline_from_annotation = annotation_boxes.underline.iter().any(|mark| rects_meaningfully_overlap(&bbox, mark));
      let strikethrough_from_annotation = annotation_boxes
        .strikethrough
        .iter()
        .any(|mark| rects_meaningfully_overlap(&bbox, mark));
      let highlight_from_annotation = annotation_boxes.highlight.iter().any(|mark| rects_meaningfully_overlap(&bbox, mark));
      let underline_from_vector = lines.iter().any(|line| path_is_underline(line, &bbox, span.font_size));
      let strikethrough_from_vector = lines
        .iter()
        .any(|line| path_is_strikethrough(line, &bbox, span.font_size));
      let highlight_from_vector = rects
        .iter()
        .any(|rect| path_is_highlight_rect(rect, &bbox, span.font_size));
      let border = rects
        .iter()
        .any(|rect| path_is_text_border(rect, &bbox, span.font_size));

      stats.annotation_underlines += usize::from(underline_from_annotation);
      stats.annotation_strikethroughs += usize::from(strikethrough_from_annotation);
      stats.annotation_highlights += usize::from(highlight_from_annotation);
      stats.vector_underlines += usize::from(underline_from_vector);
      stats.vector_strikethroughs += usize::from(strikethrough_from_vector);
      stats.vector_highlights += usize::from(highlight_from_vector);
      stats.vector_borders += usize::from(border);

      Some(PdfRunFact {
        text,
        bbox,
        bold: span.font_weight.is_bold(),
        italic: span.is_italic,
        underline: underline_from_annotation || underline_from_vector,
        strikethrough: strikethrough_from_annotation || strikethrough_from_vector,
        highlight: highlight_from_annotation || highlight_from_vector,
        border,
        font_size: span.font_size,
        color: span.color,
      })
    })
    .collect()
}

#[hotpath::measure]
fn page_lines_from_runs(runs: &mut [PdfRunFact], median_font_size: f32) -> Vec<PdfLineFact> {
  runs.sort_by(reading_order_cmp);
  let line_tolerance = (median_font_size * 0.45).clamp(2.0, 7.0);
  let mut lines = Vec::<Vec<PdfRunFact>>::new();

  for run in runs.iter().cloned() {
    if let Some(line) = lines.last_mut() {
      let center_y = line_center_y(line);
      if (rect_center_y(&run.bbox) - center_y).abs() <= line_tolerance {
        line.push(run);
        continue;
      }
    }
    lines.push(vec![run]);
  }

  lines
    .into_iter()
    .map(|mut runs| {
      runs.sort_by(|a, b| a.bbox.x.total_cmp(&b.bbox.x));
      let bbox = runs
        .iter()
        .map(|run| run.bbox)
        .reduce(|left, right| left.union(&right))
        .unwrap_or_default();
      let (style, high_confidence_style) = recognize_line_paragraph_style(&runs);
      PdfLineFact {
        runs,
        bbox,
        center_y: rect_center_y(&bbox),
        style,
        high_confidence_style,
      }
    })
    .collect()
}

#[hotpath::measure]
fn page_paragraphs_from_lines(lines: Vec<PdfLineFact>, median_font_size: f32) -> Vec<PdfParagraphFact> {
  let mut paragraphs = Vec::new();
  let mut current: Option<PdfParagraphFact> = None;
  let mut previous_line: Option<PdfLineFact> = None;

  for line in lines {
    let is_structural = line.style != ParagraphStyle::Normal;
    let starts_new = current
      .as_ref()
      .is_none_or(|paragraph| paragraph.style != ParagraphStyle::Normal)
      || is_structural
      || previous_line
        .as_ref()
        .is_some_and(|previous| line_starts_new_paragraph(previous, &line, median_font_size));

    if starts_new {
      if let Some(paragraph) = current.take() {
        paragraphs.push(paragraph);
      }
      current = Some(PdfParagraphFact {
        style: line.style,
        runs: line.runs.clone(),
        high_confidence_style: line.high_confidence_style,
      });
    } else if let Some(paragraph) = current.as_mut() {
      if let Some(join) = line_join(paragraph.runs.last(), line.runs.first()) {
        paragraph.runs.push(PdfRunFact::plain_spacing(join));
      }
      paragraph.runs.extend(line.runs.clone());
      paragraph.high_confidence_style |= line.high_confidence_style;
    }
    previous_line = Some(line);
  }

  if let Some(paragraph) = current {
    paragraphs.push(paragraph);
  }
  paragraphs
}

#[hotpath::measure]
fn document_paragraphs_from_pdf_paragraphs(
  pdf_paragraphs: Vec<PdfParagraphFact>,
  stats: &mut PdfImportStats,
) -> Vec<DocumentParagraphInput> {
  let mut paragraphs = Vec::with_capacity(pdf_paragraphs.len());
  let mut current_section_has_underline = false;
  let mut after_heading_seeking_text = false;

  for paragraph in pdf_paragraphs {
    let style = paragraph.style;
    let is_heading = matches!(
      style,
      ParagraphStyle::Pocket | ParagraphStyle::Hat | ParagraphStyle::Block | ParagraphStyle::Tag | ParagraphStyle::Analytic
    );
    if style != ParagraphStyle::Normal {
      stats.structural_hits += 1;
    }
    if paragraph.high_confidence_style {
      stats.high_confidence_structural_hits += 1;
    }

    let mut can_process_citations = false;
    if is_heading {
      current_section_has_underline = false;
      after_heading_seeking_text = true;
    } else if after_heading_seeking_text {
      let has_text = paragraph.runs.iter().any(|run| !run.text.trim().is_empty());
      if has_text && style != ParagraphStyle::Undertag {
        can_process_citations = true;
        after_heading_seeking_text = false;
      }
    }
    if !is_heading && paragraph.runs.iter().any(|run| run.underline && !run.bold) {
      current_section_has_underline = true;
    }

    let suppress_semantic_styles = matches!(
      style,
      ParagraphStyle::Pocket
        | ParagraphStyle::Hat
        | ParagraphStyle::Block
        | ParagraphStyle::Tag
        | ParagraphStyle::Analytic
        | ParagraphStyle::Undertag
    );
    let structural_run_formatting_allowed = matches!(style, ParagraphStyle::Tag | ParagraphStyle::Analytic | ParagraphStyle::Undertag);
    let direct_highlight_allowed = !matches!(style, ParagraphStyle::Pocket | ParagraphStyle::Hat | ParagraphStyle::Block);
    let bold_paragraph_overrides = can_process_citations
      .then(|| entirely_bold_paragraph_overrides(&paragraph.runs))
      .flatten();

    let runs = paragraph
      .runs
      .iter()
      .enumerate()
      .filter_map(|(run_ix, run)| {
        if run.text.is_empty() {
          return None;
        }
        let styles = recognize_run_styles_for_context(
          run,
          run_ix,
          bold_paragraph_overrides.as_deref(),
          suppress_semantic_styles,
          structural_run_formatting_allowed,
          direct_highlight_allowed,
          style,
          can_process_citations,
          current_section_has_underline,
        );
        if styles.semantic != RunSemanticStyle::Plain || styles.direct_underline || styles.strikethrough || styles.highlight.is_some() {
          stats.semantic_hits += 1;
        }
        stats.runs_imported += 1;
        Some(DocumentRunInput {
          text: run.text.clone(),
          styles,
        })
      })
      .collect::<Vec<_>>();

    stats.paragraphs_imported += 1;
    paragraphs.push(DocumentParagraphInput { style, runs });
  }
  paragraphs
}

#[hotpath::measure]
fn recognize_line_paragraph_style(runs: &[PdfRunFact]) -> (ParagraphStyle, bool) {
  let text_len = paragraph_text_len(runs);
  let has_bold = runs.iter().any(|run| run.bold);
  let all_italic = !runs.is_empty() && runs.iter().all(|run| run.italic);
  let any_underline = runs.iter().any(|run| run.underline);
  let any_color = runs.iter().any(|run| color_is_non_black(run.color));
  let max_size = runs
    .iter()
    .map(|run| run.font_size)
    .fold(0.0_f32, f32::max);

  if has_bold && max_size >= 24.0 {
    return (ParagraphStyle::Pocket, true);
  }
  if has_bold && max_size >= 20.0 {
    return (ParagraphStyle::Hat, true);
  }
  if has_bold && any_underline && max_size >= 15.0 {
    return (ParagraphStyle::Block, true);
  }
  if all_italic && any_color && max_size <= 12.5 && text_len <= 180 {
    return (ParagraphStyle::Undertag, false);
  }
  if has_bold && any_color && text_len <= 180 {
    return (ParagraphStyle::Analytic, false);
  }
  if has_bold && max_size >= 12.5 && text_len <= 120 && runs.iter().all(|run| run.bold || run.text.trim().is_empty()) {
    return (ParagraphStyle::Tag, false);
  }

  (ParagraphStyle::Normal, false)
}

#[hotpath::measure]
fn recognize_run_styles_for_context(
  run: &PdfRunFact,
  run_ix: usize,
  bold_paragraph_overrides: Option<&[bool]>,
  suppress_semantic_styles: bool,
  structural_run_formatting_allowed: bool,
  direct_highlight_allowed: bool,
  paragraph_style: ParagraphStyle,
  can_process_citations: bool,
  current_section_has_underline: bool,
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
    ),
    direct_underline: structural_run_formatting_allowed && run.underline,
    strikethrough: !suppress_semantic_styles && run.strikethrough,
    highlight: (direct_highlight_allowed && run.highlight).then_some(HighlightStyle::Spoken),
  }
}

#[hotpath::measure]
fn recognize_run_semantic_for_context(
  run: &PdfRunFact,
  run_ix: usize,
  bold_paragraph_overrides: Option<&[bool]>,
  suppress_semantic_styles: bool,
  paragraph_style: ParagraphStyle,
  can_process_citations: bool,
  current_section_has_underline: bool,
) -> RunSemanticStyle {
  if suppress_semantic_styles {
    return RunSemanticStyle::default();
  }
  if run.border {
    return RunSemanticStyle::Emphasis;
  }
  if let Some(overrides) = bold_paragraph_overrides
    && overrides.get(run_ix) == Some(&true)
  {
    return RunSemanticStyle::Cite;
  }
  if can_process_citations && run.bold {
    return RunSemanticStyle::Cite;
  }
  if run.underline && !run.bold {
    return RunSemanticStyle::Underline;
  }
  if run.bold && run.underline {
    return if current_section_has_underline {
      RunSemanticStyle::Emphasis
    } else {
      RunSemanticStyle::Underline
    };
  }
  if run.highlight {
    return RunSemanticStyle::Underline;
  }
  if paragraph_style == ParagraphStyle::Normal && run.font_size <= 8.0 && !run.underline && !run.highlight {
    return RunSemanticStyle::Condensed;
  }
  RunSemanticStyle::Plain
}

#[hotpath::measure]
fn entirely_bold_paragraph_overrides(runs: &[PdfRunFact]) -> Option<Vec<bool>> {
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
      if runs[*ix].font_size > base_size + 0.5 {
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
fn most_common_half_point_size(runs: &[PdfRunFact], indices: &[usize]) -> Option<f32> {
  let mut counts: FxHashMap<i32, usize> = FxHashMap::default();
  for ix in indices {
    let size = runs[*ix].font_size;
    if (6.0..=72.0).contains(&size) {
      *counts.entry((size * 2.0).round() as i32).or_default() += 1;
    }
  }
  counts
    .into_iter()
    .max_by(|(size_a, count_a), (size_b, count_b)| count_a.cmp(count_b).then_with(|| size_b.cmp(size_a)))
    .map(|(half_points, _)| half_points as f32 / 2.0)
}

struct AnnotationBoxes {
  highlight: Vec<Rect>,
  underline: Vec<Rect>,
  strikethrough: Vec<Rect>,
}

impl AnnotationBoxes {
  #[hotpath::measure]
  fn new(annotations: &[Annotation]) -> Self {
    let mut highlight = Vec::new();
    let mut underline = Vec::new();
    let mut strikethrough = Vec::new();
    for annotation in annotations {
      let target = match annotation.subtype_enum {
        AnnotationSubtype::Highlight => &mut highlight,
        AnnotationSubtype::Underline | AnnotationSubtype::Squiggly => &mut underline,
        AnnotationSubtype::StrikeOut => &mut strikethrough,
        _ => continue,
      };
      target.extend(annotation_markup_rects(annotation));
    }
    Self {
      highlight,
      underline,
      strikethrough,
    }
  }
}

#[hotpath::measure]
fn annotation_markup_rects(annotation: &Annotation) -> Vec<Rect> {
  if let Some(quads) = annotation.quad_points.as_ref() {
    return quads.iter().map(rect_from_quad).collect();
  }
  annotation
    .rect
    .map(|rect| Rect::from_points(rect[0] as f32, rect[1] as f32, rect[2] as f32, rect[3] as f32).normalize())
    .into_iter()
    .collect()
}

#[hotpath::measure]
fn rect_from_quad(quad: &[f64; 8]) -> Rect {
  let mut min_x = f32::INFINITY;
  let mut min_y = f32::INFINITY;
  let mut max_x = f32::NEG_INFINITY;
  let mut max_y = f32::NEG_INFINITY;
  for point in quad.chunks_exact(2) {
    let x = point[0] as f32;
    let y = point[1] as f32;
    min_x = min_x.min(x);
    max_x = max_x.max(x);
    min_y = min_y.min(y);
    max_y = max_y.max(y);
  }
  Rect::from_points(min_x, min_y, max_x, max_y).normalize()
}

#[hotpath::measure]
fn path_is_underline(path: &PathContent, text: &Rect, font_size: f32) -> bool {
  if !path.has_stroke() || !path.is_horizontal_line(1.0) || horizontal_overlap_fraction(&path.bbox, text) < 0.55 {
    return false;
  }
  let y = rect_center_y(&path.bbox);
  let bottom_distance = (y - text.bottom()).abs();
  let baseline_distance = (y - (text.bottom() - font_size * 0.12)).abs();
  bottom_distance <= (font_size * 0.30).max(2.0) || baseline_distance <= (font_size * 0.25).max(2.0)
}

#[hotpath::measure]
fn path_is_strikethrough(path: &PathContent, text: &Rect, font_size: f32) -> bool {
  if !path.has_stroke() || !path.is_horizontal_line(1.0) || horizontal_overlap_fraction(&path.bbox, text) < 0.55 {
    return false;
  }
  (rect_center_y(&path.bbox) - rect_center_y(text)).abs() <= (font_size * 0.22).max(2.0)
}

#[hotpath::measure]
fn path_is_highlight_rect(path: &PathContent, text: &Rect, font_size: f32) -> bool {
  if !path.has_fill() || rect_overlap_fraction(text, &path.bbox) < 0.35 {
    return false;
  }
  path.bbox.height >= font_size * 0.45 && color_is_highlight_like(path.fill_color.unwrap_or_else(Color::white))
}

#[hotpath::measure]
fn path_is_text_border(path: &PathContent, text: &Rect, font_size: f32) -> bool {
  if !path.has_stroke() || !path.is_rectangle() {
    return false;
  }
  let expanded = expand_rect(text, font_size * 0.35, font_size * 0.35);
  path.bbox.contains_rect(text) && rect_overlap_fraction(&expanded, &path.bbox) > 0.25
}

#[hotpath::measure]
fn line_starts_new_paragraph(previous: &PdfLineFact, current: &PdfLineFact, median_font_size: f32) -> bool {
  if previous.style != ParagraphStyle::Normal || current.style != ParagraphStyle::Normal {
    return true;
  }
  let center_gap = (previous.center_y - current.center_y).abs();
  let paragraph_gap_threshold = (median_font_size * 1.75).max(16.0);
  if center_gap > paragraph_gap_threshold {
    return true;
  }
  let indent_gap = (current.bbox.x - previous.bbox.x).abs();
  indent_gap > median_font_size * 2.5 && current.bbox.x < previous.bbox.x
}

#[hotpath::measure]
fn line_join(previous: Option<&PdfRunFact>, next: Option<&PdfRunFact>) -> Option<InterLineJoin> {
  let previous = previous?;
  let next = next?;
  let previous_trimmed = previous.text.trim_end();
  let next_trimmed = next.text.trim_start();
  if previous_trimmed.is_empty() || next_trimmed.is_empty() {
    return None;
  }
  if previous_trimmed.ends_with('-') {
    return Some(InterLineJoin::None);
  }
  if next_trimmed
    .chars()
    .next()
    .is_some_and(|ch| matches!(ch, ',' | '.' | ';' | ':' | ')' | ']' | '}'))
  {
    return Some(InterLineJoin::None);
  }
  Some(InterLineJoin::Space)
}

impl PdfRunFact {
  #[hotpath::measure]
  fn plain_spacing(join: InterLineJoin) -> Self {
    Self {
      text: match join {
        InterLineJoin::Space => " ".to_string(),
        InterLineJoin::None => String::new(),
      },
      bbox: Rect::default(),
      bold: false,
      italic: false,
      underline: false,
      strikethrough: false,
      highlight: false,
      border: false,
      font_size: 11.0,
      color: Color::black(),
    }
  }
}

#[hotpath::measure]
fn reading_order_cmp(a: &PdfRunFact, b: &PdfRunFact) -> Ordering {
  let y_cmp = rect_center_y(&b.bbox).total_cmp(&rect_center_y(&a.bbox));
  if y_cmp != Ordering::Equal {
    return y_cmp;
  }
  a.bbox.x.total_cmp(&b.bbox.x)
}

#[hotpath::measure]
fn normalize_span_text(text: String) -> String {
  text.replace('\r', "").replace('\n', " ")
}

#[hotpath::measure]
fn median_font_size(runs: &[PdfRunFact]) -> Option<f32> {
  let mut sizes = runs
    .iter()
    .map(|run| run.font_size)
    .filter(|size| size.is_finite() && (1.0..=96.0).contains(size))
    .collect::<Vec<_>>();
  if sizes.is_empty() {
    return None;
  }
  sizes.sort_by(f32::total_cmp);
  Some(sizes[sizes.len() / 2])
}

#[hotpath::measure]
fn line_center_y(line: &[PdfRunFact]) -> f32 {
  let total = line
    .iter()
    .map(|run| rect_center_y(&run.bbox))
    .sum::<f32>();
  total / line.len().max(1) as f32
}

#[hotpath::measure]
fn rect_center_y(rect: &Rect) -> f32 {
  rect.y + rect.height * 0.5
}

#[hotpath::measure]
fn rects_meaningfully_overlap(a: &Rect, b: &Rect) -> bool {
  rect_overlap_fraction(a, b) >= 0.25 || horizontal_overlap_fraction(a, b) >= 0.65 && vertical_overlap_fraction(a, b) >= 0.18
}

#[hotpath::measure]
fn rect_overlap_fraction(a: &Rect, b: &Rect) -> f32 {
  let Some(intersection) = a.intersection(b) else {
    return 0.0;
  };
  let denominator = a.area().min(b.area()).max(1.0);
  (intersection.area() / denominator).clamp(0.0, 1.0)
}

#[hotpath::measure]
fn horizontal_overlap_fraction(a: &Rect, b: &Rect) -> f32 {
  let left = a.left().max(b.left());
  let right = a.right().min(b.right());
  let overlap = (right - left).max(0.0);
  let denominator = a.width.min(b.width).max(1.0);
  (overlap / denominator).clamp(0.0, 1.0)
}

#[hotpath::measure]
fn vertical_overlap_fraction(a: &Rect, b: &Rect) -> f32 {
  let top = a.top().max(b.top());
  let bottom = a.bottom().min(b.bottom());
  let overlap = (bottom - top).max(0.0);
  let denominator = a.height.min(b.height).max(1.0);
  (overlap / denominator).clamp(0.0, 1.0)
}

#[hotpath::measure]
fn expand_rect(rect: &Rect, x: f32, y: f32) -> Rect {
  Rect::new(rect.x - x, rect.y - y, rect.width + x * 2.0, rect.height + y * 2.0)
}

#[hotpath::measure]
fn paragraph_text_len(runs: &[PdfRunFact]) -> usize {
  runs
    .iter()
    .fold((0_usize, true, 0_usize), |(count, leading, pending_whitespace), run| {
      count_trimmed_chars(&run.text, count, leading, pending_whitespace)
    })
    .0
}

#[hotpath::measure]
fn color_is_non_black(color: Color) -> bool {
  color.r > 0.08 || color.g > 0.08 || color.b > 0.08
}

#[hotpath::measure]
fn color_is_highlight_like(color: Color) -> bool {
  let max = color.r.max(color.g).max(color.b);
  let min = color.r.min(color.g).min(color.b);
  max > 0.55 && max - min > 0.12
}

#[hotpath::measure]
fn confidence(stats: &PdfImportStats) -> f32 {
  let structural = (stats.structural_hits as f32 * 0.12).min(0.36);
  let high_confidence = (stats.high_confidence_structural_hits as f32 * 0.22).min(0.44);
  let semantic = (stats.semantic_hits as f32 * 0.02).min(0.16);
  let annotations = ((stats.annotation_highlights + stats.annotation_underlines + stats.annotation_strikethroughs) as f32 * 0.04).min(0.12);
  let vectors = ((stats.vector_highlights + stats.vector_underlines + stats.vector_strikethroughs + stats.vector_borders) as f32 * 0.02).min(0.10);
  (structural + high_confidence + semantic + annotations + vectors).min(1.0)
}

#[hotpath::measure]
fn rejection_reason(report: &PdfConversionReport) -> Option<String> {
  if report.paragraphs_imported == 0 || report.runs_imported == 0 {
    return Some("no extractable text spans were found".to_string());
  }
  if report.structural_hits == 0 {
    return Some("no debate-document heading structure was recognized".to_string());
  }
  if report.high_confidence_structural_hits == 0 && report.structural_hits < 3 {
    return Some("only weak heading signals were found".to_string());
  }
  if report.confidence < 0.42 {
    return Some(format!("recognition confidence {:.2} is below 0.42", report.confidence));
  }
  None
}

#[hotpath::measure]
fn pdf_error(error: pdf_oxide::error::Error) -> io::Error {
  io::Error::new(io::ErrorKind::InvalidData, error)
}

#[cfg(test)]
#[path = "pdf_import_tests.rs"]
mod tests;
