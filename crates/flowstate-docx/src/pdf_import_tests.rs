use super::*;

#[hotpath::measure]
fn run(text: &str, font_size: f32, bold: bool, underline: bool) -> PdfRunFact {
  PdfRunFact {
    text: text.to_string(),
    bbox: Rect::new(0.0, 0.0, text.len() as f32 * font_size * 0.5, font_size),
    bold,
    italic: false,
    underline,
    strikethrough: false,
    highlight: false,
    border: false,
    font_size,
    color: Color::black(),
  }
}

#[test]
#[hotpath::measure]
fn bold_underlined_sixteen_point_line_recognizes_block() {
  let runs = [run("2NC---AT: US Draw-In", 16.0, true, true)];

  assert_eq!(recognize_line_paragraph_style(&runs), (ParagraphStyle::Block, true));
}

#[test]
#[hotpath::measure]
fn first_bold_text_after_heading_becomes_cite() {
  let runs = vec![run("Smith 24", 13.0, true, false)];
  let overrides = entirely_bold_paragraph_overrides(&runs).unwrap();

  let styles = recognize_run_styles_for_context(
    &runs[0],
    0,
    Some(&overrides),
    false,
    false,
    true,
    ParagraphStyle::Normal,
    true,
    false,
  );

  assert_eq!(styles.semantic, RunSemanticStyle::Cite);
}

#[test]
#[hotpath::measure]
fn weak_structure_rejects_low_confidence_pdf() {
  let report = PdfConversionReport {
    recognition_rules: PDF_RECOGNITION_RULES,
    decision: PdfImportDecision::Rejected,
    rejection_reason: None,
    confidence: 0.20,
    pages_scanned: 1,
    spans_imported: 3,
    paragraphs_imported: 3,
    runs_imported: 3,
    structural_hits: 1,
    high_confidence_structural_hits: 0,
    semantic_hits: 0,
    annotation_highlights: 0,
    annotation_underlines: 0,
    annotation_strikethroughs: 0,
    vector_highlights: 0,
    vector_underlines: 0,
    vector_strikethroughs: 0,
    vector_borders: 0,
  };

  assert!(rejection_reason(&report).is_some());
}
