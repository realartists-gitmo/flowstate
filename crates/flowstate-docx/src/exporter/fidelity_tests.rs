//! Structural DOCX/PDF export fidelity fixtures (FS-122..FS-130).
//!
//! Each test builds a [`DocumentProjection`] exercising one finding, writes a
//! real `.docx`, unzips `word/document.xml`, and asserts on OOXML *structure*
//! (element presence) rather than exact bytes. A PDF smoke test drives the
//! shared `write_pdf` path. These run in-crate so they can use the exporter's
//! own `zip` / `image` dependencies to read entries and synthesize assets.
#![allow(
  clippy::default_trait_access,
  reason = "section-attr fixture types are not nameable from this crate; `Default::default()` is the only way to build them"
)]
#![allow(
  clippy::case_sensitive_file_extension_comparisons,
  reason = "docx-rs writes media parts as deterministically lowercase .png; the fixtures assert that exact output"
)]

use std::{
  io::Read,
  path::PathBuf,
  sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
  },
};

use flowstate_document::{
  AssetId, AssetRecord, CellId, ColumnId, DocumentProjection, DocumentSection, InputBlock, InputBlockAlignment, InputEquationBlock,
  InputEquationDisplay, InputEquationSyntax, InputImageBlock, InputImageSizing, InputParagraph, InputRun, InputTableBlock, InputTableCell,
  InputTableCellBlock, InputTableColumn, InputTableColumnWidth, InputTableRow, InputTableStyle, ParagraphStyle, RowId, RunStyles, SectionId,
  SectionKind, document_from_input_blocks, flowstate_document_theme,
};

use crate::{write_docx, write_pdf};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_path(extension: &str) -> PathBuf {
  let sequence = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
  std::env::temp_dir().join(format!("flowstate-fidelity-{}-{sequence}.{extension}", std::process::id()))
}

fn read_zip_entry(path: &std::path::Path, name: &str) -> String {
  let file = std::fs::File::open(path).expect("open exported docx");
  let mut archive = zip::ZipArchive::new(file).expect("read docx zip");
  let mut entry = archive.by_name(name).unwrap_or_else(|_| panic!("missing zip entry {name}"));
  let mut text = String::new();
  entry.read_to_string(&mut text).expect("read zip entry");
  text
}

fn zip_entry_names(path: &std::path::Path) -> Vec<String> {
  let file = std::fs::File::open(path).expect("open exported docx");
  let mut archive = zip::ZipArchive::new(file).expect("read docx zip");
  (0..archive.len())
    .filter_map(|index| archive.by_index(index).ok().map(|entry| entry.name().to_string()))
    .collect()
}

fn paragraph_block(text: &str) -> InputBlock {
  InputBlock::Paragraph(InputParagraph {
    style: ParagraphStyle::Normal,
    runs: vec![InputRun {
      text: text.to_string(),
      styles: RunStyles::default(),
    }],
  })
}

fn text_cell(row_id: RowId, column_id: ColumnId, text: &str, row_span: u16, col_span: u16) -> InputTableCell {
  InputTableCell {
    id: CellId::from_coordinate(row_id, column_id),
    row_id,
    column_id,
    blocks: vec![InputTableCellBlock::Paragraph(InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![InputRun {
        text: text.to_string(),
        styles: RunStyles::default(),
      }],
    })],
    row_span,
    col_span,
  }
}

fn encode_image(format: image::ImageFormat) -> Vec<u8> {
  let buffer = image::RgbImage::from_pixel(4, 3, image::Rgb([200u8, 60, 60]));
  let mut cursor = std::io::Cursor::new(Vec::new());
  image::DynamicImage::ImageRgb8(buffer)
    .write_to(&mut cursor, format)
    .expect("encode fixture image");
  cursor.into_inner()
}

fn insert_asset(document: &mut DocumentProjection, id: AssetId, mime: &'static str, name: &'static str, bytes: Vec<u8>) {
  let content_hash = AssetRecord::stable_content_hash(&bytes);
  document.assets.assets.insert(
    id,
    AssetRecord {
      id,
      mime_type: mime.into(),
      original_name: Some(name.into()),
      content_hash,
      bytes: Arc::new(bytes),
    },
  );
}

#[test]
fn table_spans_grid_and_header_are_emitted() {
  // FS-123 spans + FS-124 grid/header: a 2-column table with a spanning header
  // cell, a row-spanned cell, and a fractional grid.
  let table = InputBlock::Table(InputTableBlock {
    rows: vec![
      InputTableRow {
        id: RowId(1),
        cells: vec![text_cell(RowId(1), ColumnId(1), "Header", 1, 2)],
      },
      InputTableRow {
        id: RowId(2),
        cells: vec![text_cell(RowId(2), ColumnId(1), "A", 2, 1), text_cell(RowId(2), ColumnId(2), "B", 1, 1)],
      },
      InputTableRow {
        id: RowId(3),
        cells: vec![text_cell(RowId(3), ColumnId(1), "C", 1, 1)],
      },
    ],
    columns: vec![
      InputTableColumn {
        id: ColumnId(1),
        width: InputTableColumnWidth::Fraction(1),
      },
      InputTableColumn {
        id: ColumnId(2),
        width: InputTableColumnWidth::Fraction(1),
      },
    ],
    style: InputTableStyle { header_row: true },
  });
  let document = document_from_input_blocks(flowstate_document_theme(), vec![paragraph_block("Intro"), table]);
  let path = temp_path("docx");
  write_docx(&path, &document).expect("write docx");
  let xml = read_zip_entry(&path, "word/document.xml");
  let _ = std::fs::remove_file(&path);

  assert!(xml.contains("<w:tblGrid>"), "table grid missing: {xml}");
  assert!(xml.contains("<w:gridCol"), "grid columns missing");
  assert!(xml.contains("<w:tcW"), "cell widths missing");
  assert!(xml.contains("w:gridSpan"), "gridSpan (col_span) missing");
  assert!(xml.contains("w:val=\"restart\""), "vMerge restart missing");
  assert!(xml.contains("w:val=\"continue\""), "vMerge continue missing");
  assert!(xml.contains("<w:tblHeader />"), "tblHeader missing");
  assert!(!xml.contains("<w:cantSplit"), "header marker leaked into output");
}

#[test]
fn image_alt_text_and_png_part_are_emitted() {
  // FS-127 alt text + FS-128 fit-width + PNG embedding.
  let mut document = document_from_input_blocks(
    flowstate_document_theme(),
    vec![InputBlock::Image(InputImageBlock {
      asset_id: AssetId(1),
      alt_text: "A red test image".to_string(),
      caption: None,
      sizing: InputImageSizing::FitWidth,
      alignment: InputBlockAlignment::Center,
    })],
  );
  insert_asset(&mut document, AssetId(1), "image/png", "red.png", encode_image(image::ImageFormat::Png));
  let path = temp_path("docx");
  write_docx(&path, &document).expect("write docx");
  let xml = read_zip_entry(&path, "word/document.xml");
  let names = zip_entry_names(&path);
  let _ = std::fs::remove_file(&path);

  assert!(xml.contains("descr=\"A red test image\""), "docPr descr missing: {xml}");
  assert!(xml.contains("<wp:extent"), "drawing extent missing");
  assert!(
    names.iter().any(|name| name.starts_with("word/media/") && name.ends_with(".png")),
    "embedded PNG media part missing: {names:?}"
  );
}

#[test]
fn non_png_image_is_transcoded_and_embedded() {
  // FS-129: a GIF asset must be transcoded and embedded, not dropped to text.
  let mut document = document_from_input_blocks(
    flowstate_document_theme(),
    vec![InputBlock::Image(InputImageBlock {
      asset_id: AssetId(7),
      alt_text: "Animated source".to_string(),
      caption: None,
      sizing: InputImageSizing::Intrinsic,
      alignment: InputBlockAlignment::Left,
    })],
  );
  insert_asset(&mut document, AssetId(7), "image/gif", "loop.gif", encode_image(image::ImageFormat::Gif));
  let path = temp_path("docx");
  let warnings = crate::write_docx_with_report(&path, &document).expect("write docx");
  let names = zip_entry_names(&path);
  let xml = read_zip_entry(&path, "word/document.xml");
  let _ = std::fs::remove_file(&path);

  assert!(warnings.is_empty(), "unexpected export warnings: {warnings:?}");
  assert!(
    names.iter().any(|name| name.starts_with("word/media/") && name.ends_with(".png")),
    "transcoded PNG media part missing: {names:?}"
  );
  assert!(!xml.contains("[Animated source]"), "GIF fell back to text instead of embedding");
}

#[test]
fn equation_is_emitted_as_omml() {
  // FS-125: a LaTeX equation becomes real OMML with the math namespace declared.
  let document = document_from_input_blocks(
    flowstate_document_theme(),
    vec![InputBlock::Equation(InputEquationBlock {
      source: "\\frac{1}{2}".to_string(),
      syntax: InputEquationSyntax::Latex,
      display: InputEquationDisplay::Display,
    })],
  );
  let path = temp_path("docx");
  write_docx(&path, &document).expect("write docx");
  let xml = read_zip_entry(&path, "word/document.xml");
  let _ = std::fs::remove_file(&path);

  assert!(xml.contains("xmlns:m="), "math namespace not declared");
  assert!(xml.contains("<m:oMathPara>"), "display equation should use oMathPara");
  assert!(xml.contains("<m:f>"), "fraction OMML missing");
  assert!(!xml.contains("@@FLOWSTATE_OMML"), "equation sentinel leaked into output");
  assert!(!xml.contains("[Equation:"), "equation fell back to text");
}

#[test]
fn unconvertible_equation_falls_back_to_text() {
  let document = document_from_input_blocks(
    flowstate_document_theme(),
    vec![InputBlock::Equation(InputEquationBlock {
      // Unbalanced braces are treated as unconvertible.
      source: "\\frac{1}{2".to_string(),
      syntax: InputEquationSyntax::Latex,
      display: InputEquationDisplay::Display,
    })],
  );
  let path = temp_path("docx");
  write_docx(&path, &document).expect("write docx");
  let xml = read_zip_entry(&path, "word/document.xml");
  let _ = std::fs::remove_file(&path);
  assert!(xml.contains("[Equation:"), "expected bracketed fallback for unconvertible source");
}

#[test]
fn section_page_attributes_are_emitted() {
  // FS-126: a single custom section maps onto the document-level sectPr.
  let mut document = document_from_input_blocks(flowstate_document_theme(), vec![paragraph_block("Body")]);
  let first_paragraph = document.ids.paragraph_ids[0];
  let mut section = DocumentSection {
    id: SectionId(1),
    parent_id: None,
    kind: SectionKind::Custom(0),
    heading_paragraph: None,
    start_paragraph: first_paragraph,
    end_paragraph_exclusive: None,
    page: Some(Default::default()),
  };
  if let Some(page) = section.page.as_mut() {
    page.page_size.width_twips = 15_840;
    page.page_size.height_twips = 12_240;
    page.margins.left_twips = 720;
    page.margins.right_twips = 720;
    page.margins.top_twips = 1_000;
    page.margins.bottom_twips = 1_100;
  }
  document.sections = Arc::new(vec![section]);

  let path = temp_path("docx");
  write_docx(&path, &document).expect("write docx");
  let xml = read_zip_entry(&path, "word/document.xml");
  let _ = std::fs::remove_file(&path);

  assert!(xml.contains("<w:sectPr"), "sectPr missing");
  assert!(xml.contains("w:w=\"15840\""), "custom page width missing: {xml}");
  assert!(xml.contains("w:left=\"720\""), "custom left margin missing");
}

#[test]
fn multiple_sections_emit_boundary_sect_pr() {
  // FS-126: two sections yield a paragraph-level boundary sectPr plus the
  // document-level (final section) sectPr.
  let mut document = document_from_input_blocks(
    flowstate_document_theme(),
    vec![
      paragraph_block("First A"),
      paragraph_block("Second A"),
      paragraph_block("First B"),
      paragraph_block("Second B"),
    ],
  );
  let paragraph_ids = document.ids.paragraph_ids.clone();
  let mut section_a = DocumentSection {
    id: SectionId(1),
    parent_id: None,
    kind: SectionKind::Custom(0),
    heading_paragraph: None,
    start_paragraph: paragraph_ids[0],
    end_paragraph_exclusive: Some(paragraph_ids[2]),
    page: Some(Default::default()),
  };
  if let Some(page) = section_a.page.as_mut() {
    page.page_size.width_twips = 12_240;
    page.page_size.height_twips = 15_840;
  }
  let mut section_b = DocumentSection {
    id: SectionId(2),
    parent_id: None,
    kind: SectionKind::Custom(0),
    heading_paragraph: None,
    start_paragraph: paragraph_ids[2],
    end_paragraph_exclusive: None,
    page: Some(Default::default()),
  };
  if let Some(page) = section_b.page.as_mut() {
    page.page_size.width_twips = 15_840;
    page.page_size.height_twips = 12_240;
  }
  document.sections = Arc::new(vec![section_a, section_b]);

  let path = temp_path("docx");
  write_docx(&path, &document).expect("write docx");
  let xml = read_zip_entry(&path, "word/document.xml");
  let _ = std::fs::remove_file(&path);

  assert!(xml.matches("<w:sectPr").count() >= 2, "expected boundary + document sectPr: {xml}");
  assert!(xml.contains("w:w=\"12240\""), "first section page width missing");
  assert!(xml.contains("w:w=\"15840\""), "final section page width missing");
}

#[test]
fn pdf_smoke_test_produces_pdf() {
  // FS-130: the shared write_pdf path must still produce a PDF.
  let document = document_from_input_blocks(
    flowstate_document_theme(),
    vec![paragraph_block("PDF smoke test paragraph.")],
  );
  let path = temp_path("pdf");
  write_pdf(&path, &document).expect("write pdf");
  let bytes = std::fs::read(&path).expect("read pdf");
  let _ = std::fs::remove_file(&path);
  assert!(bytes.starts_with(b"%PDF"), "output is not a PDF");
}
