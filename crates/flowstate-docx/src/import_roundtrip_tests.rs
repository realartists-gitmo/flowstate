//! Class 5 — docx import must not produce malformed canonical state.
//!
//! The impact-doc field logs showed ~1240 paragraphs projecting with NO durable metadata
//! record and NO paragraph-style mark (`missing_paragraph_metadata` /
//! `missing_paragraph_style_mark` / `missing_paragraph_block`, plus `boundary-without-mark`
//! fidelity violations), which drove a repair storm. The current importer writes a durable
//! record + style mark for every paragraph, so a valid import SHOULD yield zero of these
//! defects — this harness locks that invariant in so a regression (an import path that skips
//! `import_paragraph_record` / the boundary style mark) fails loudly, and gives a ready knob
//! to run the SAME assertion against a real .docx (e.g. the impact doc) via an env var.

use std::sync::atomic::{AtomicU64, Ordering};

use flowstate_document::{
  AssetId, CellId, ColumnId, InputBlock, InputBlockAlignment, InputImageBlock, InputImageSizing, InputParagraph, InputRun, InputTableBlock,
  InputTableCell, InputTableCellBlock, InputTableColumn, InputTableColumnWidth, InputTableRow, InputTableStyle, ParagraphStyle, RowId,
  RunStyles, SOFT_LINE_BREAK, VertAlign, document_from_input_blocks, document_from_loro_with_defects, flowstate_document_theme,
};

use crate::{import_docx_bytes_to_loro, write_docx};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_docx_path() -> std::path::PathBuf {
  let sequence = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
  std::env::temp_dir().join(format!("flowstate-roundtrip-{}-{sequence}.docx", std::process::id()))
}

/// A structurally-rich document with the impact-doc's hazardous features at scale:
/// hundreds of paragraphs, images flanked by empty paragraphs, and a table.
fn rich_document(paragraphs: usize) -> flowstate_document::DocumentProjection {
  let para = |text: &str| {
    InputBlock::Paragraph(InputParagraph {
      style: ParagraphStyle::Normal,
      runs: if text.is_empty() {
        Vec::new()
      } else {
        vec![InputRun {
          text: text.to_string(),
          styles: RunStyles::default(),
        }]
      },
    })
  };
  let image = || {
    InputBlock::Image(InputImageBlock {
      asset_id: AssetId(1),
      alt_text: "figure".to_string(),
      caption: None,
      sizing: InputImageSizing::Intrinsic,
      alignment: InputBlockAlignment::Left,
      external_url: None,
    })
  };
  let cell = |row: u128, col: u128, text: &str| InputTableCell {
    id: CellId(row * 100 + col),
    row_id: RowId(row),
    column_id: ColumnId(col),
    blocks: vec![InputTableCellBlock::Paragraph(InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![InputRun {
        text: text.to_string(),
        styles: RunStyles::default(),
      }],
    })],
    row_span: 1,
    col_span: 1,
  };
  let table = InputBlock::Table(InputTableBlock {
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
    rows: vec![
      InputTableRow {
        id: RowId(1),
        cells: vec![cell(1, 1, "a"), cell(1, 2, "b")],
      },
      InputTableRow {
        id: RowId(2),
        cells: vec![cell(2, 1, "c"), cell(2, 2, "d")],
      },
    ],
    style: InputTableStyle { header_row: true },
  });

  let mut blocks = vec![para("Title of the brief"), table];
  for ix in 0..paragraphs {
    // §perf-heaven T8.6 guard: include the multibyte punctuation that pervades real
    // debate evidence (curly quotes, em/en dashes). Paragraph byte ranges are now
    // DERIVED from the block tree on export; a derive that omitted the inter-block
    // `\n` separators sliced `document.text` mid-character here and panicked. Keeping
    // these code points in the synthetic body makes the fast roundtrip test a guard
    // for that whole class without needing the full corpus sweep.
    blocks.push(para(&format!(
      "Body paragraph {ix} — the author\u{2019}s claim that \u{201c}deterrence\u{201d} holds \u{2013} with citations."
    )));
    if ix % 50 == 0 {
      blocks.push(image());
      blocks.push(para("")); // empty paragraph adjacent to an object
    }
  }
  // Body run with an intra-paragraph soft break: exports as a body-level
  // `<w:br/>`, which the importer must map back to U+2028 — NOT '\n', which
  // would fabricate a record-less paragraph boundary (the impact-doc defect).
  blocks.push(para(&soft_break_probe_text()));
  document_from_input_blocks(flowstate_document_theme(), blocks)
}

fn soft_break_probe_text() -> String {
  format!("wrapped argument line{SOFT_LINE_BREAK}continued after a soft line break")
}

fn assert_import_defect_free(bytes: &[u8], label: &str) -> flowstate_document::DocumentProjection {
  let (imported, report) = import_docx_bytes_to_loro(bytes, "roundtrip").expect("import docx bytes");
  let (projection, defects) = document_from_loro_with_defects(&imported.doc).expect("project imported docx");
  assert!(
    defects.is_empty(),
    "{label}: importing produced {} projection defect(s) (paragraphs_imported={:?}) — the importer left canonical state a repair pass must fix; sample: {:?}",
    defects.len(),
    report.paragraphs_imported,
    defects.iter().take(5).collect::<Vec<_>>(),
  );
  projection
}

/// A minimal single-part docx zip: `word/document.xml` + its rels + the fixed
/// package plumbing, plus any extra parts (media). Shared by the linked-image
/// fixtures (§A11.9).
fn build_docx_zip(document_xml: &str, document_rels: &str, extra_parts: &[(&str, &[u8])]) -> Vec<u8> {
  use std::io::Write as _;
  let root_rels = concat!(
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
    r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
    r#"<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>"#,
    r#"</Relationships>"#
  );
  let content_types = concat!(
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
    r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">"#,
    r#"<Default Extension="png" ContentType="image/png"/>"#,
    r#"<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>"#,
    r#"<Default Extension="xml" ContentType="application/xml"/>"#,
    r#"<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>"#,
    r#"</Types>"#
  );
  let mut cursor = std::io::Cursor::new(Vec::new());
  {
    let mut writer = zip::ZipWriter::new(&mut cursor);
    let options = zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let mut parts: Vec<(&str, &[u8])> = vec![
      ("[Content_Types].xml", content_types.as_bytes()),
      ("_rels/.rels", root_rels.as_bytes()),
      ("word/document.xml", document_xml.as_bytes()),
      ("word/_rels/document.xml.rels", document_rels.as_bytes()),
    ];
    parts.extend_from_slice(extra_parts);
    for (name, bytes) in parts {
      writer.start_file(name, options).expect("zip entry");
      writer.write_all(bytes).expect("zip bytes");
    }
    writer.finish().expect("zip finish")
  };
  cursor.into_inner()
}

fn image_blocks(projection: &flowstate_document::DocumentProjection) -> Vec<flowstate_document::ImageBlock> {
  projection
    .blocks
    .iter()
    .filter_map(|block| match block {
      flowstate_document::Block::Image(image) => Some(image.clone()),
      _ => None,
    })
    .collect()
}

/// §act-eleven A11.9(a): a genuinely-LINKED `DrawingML` image (`a:blip r:link`
/// to a `TargetMode="External"` relationship, NO embedded media part) imports
/// as ONE image block carrying the external URL — not dropped, and no
/// bracketed alt text fabricated in the body.
#[test]
fn linked_drawingml_blip_imports_as_external_url_image() {
  let url = "https://example.com/linked-figure.png?cache=1&v=2";
  let document_xml = concat!(
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
    r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" "#,
    r#"xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" "#,
    r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" "#,
    r#"xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture" "#,
    r#"xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">"#,
    r#"<w:body>"#,
    r#"<w:p><w:r><w:t>before</w:t></w:r></w:p>"#,
    r#"<w:p><w:r><w:drawing><wp:inline distT="0" distB="0" distL="0" distR="0">"#,
    r#"<wp:extent cx="914400" cy="685800"/>"#,
    r#"<wp:docPr id="1" name="Picture 1" descr="linked figure"/>"#,
    r#"<a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture">"#,
    r#"<pic:pic><pic:nvPicPr><pic:cNvPr id="1" name="Picture 1"/><pic:cNvPicPr/></pic:nvPicPr>"#,
    r#"<pic:blipFill><a:blip r:link="rId8"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill>"#,
    r#"<pic:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="914400" cy="685800"/></a:xfrm>"#,
    r#"<a:prstGeom prst="rect"><a:avLst/></a:prstGeom></pic:spPr>"#,
    r#"</pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"#,
    r#"<w:p><w:r><w:t>after</w:t></w:r></w:p>"#,
    r#"</w:body></w:document>"#
  );
  let rels = format!(
    concat!(
      r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
      r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
      r#"<Relationship Id="rId8" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="{}" TargetMode="External"/>"#,
      r#"</Relationships>"#
    ),
    url.replace('&', "&amp;"),
  );
  let bytes = build_docx_zip(document_xml, &rels, &[]);

  let projection = assert_import_defect_free(&bytes, "linked drawingml import");
  let images = image_blocks(&projection);
  assert_eq!(images.len(), 1, "the link-only blip must import as ONE image block");
  assert_eq!(
    images[0]
      .external_url
      .as_ref()
      .map(|url| -> &str { url.as_ref() }),
    Some(url),
    "the external-mode relationship target must land on the image block"
  );
  assert_eq!(images[0].alt_text, "linked figure", "docPr descr must map to alt text");
  // 914400x685800 EMU = 96x72 px.
  assert_eq!(
    images[0].sizing,
    flowstate_document::ImageSizing::Fixed {
      width_px: 96,
      height_px: Some(72)
    },
    "wp:extent must map to fixed pixel sizing"
  );
  assert!(
    !projection.text.to_string().contains("linked figure"),
    "a linked image must import as an object, not bracketed body text"
  );
}

/// §act-eleven A11.9(b): a VML `v:imagedata` whose ONLY resolvable reference is
/// the external `r:href` companion (no embeddable `r:id` part) imports as a
/// linked image carrying the URL.
#[test]
fn vml_href_external_imports_as_external_url_image() {
  let url = "https://example.com/vml-tracker.gif";
  let document_xml = concat!(
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
    r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" "#,
    r#"xmlns:v="urn:schemas-microsoft-com:vml" "#,
    r#"xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">"#,
    r#"<w:body>"#,
    r#"<w:p><w:r><w:t>before</w:t></w:r></w:p>"#,
    r#"<w:p><w:r><w:pict><v:shape style="width:48pt;height:36pt"><v:imagedata r:href="rId7"/></v:shape></w:pict></w:r></w:p>"#,
    r#"<w:p><w:r><w:t>after</w:t></w:r></w:p>"#,
    r#"</w:body></w:document>"#
  );
  let rels = format!(
    concat!(
      r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
      r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
      r#"<Relationship Id="rId7" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="{url}" TargetMode="External"/>"#,
      r#"</Relationships>"#
    ),
    url = url
  );
  let bytes = build_docx_zip(document_xml, &rels, &[]);

  let projection = assert_import_defect_free(&bytes, "vml external import");
  let images = image_blocks(&projection);
  assert_eq!(images.len(), 1, "the external-only VML pict must import as ONE image block");
  assert_eq!(
    images[0]
      .external_url
      .as_ref()
      .map(|url| -> &str { url.as_ref() }),
    Some(url),
    "the r:href external target must survive"
  );
  // 48pt x 36pt at 96dpi = 64px x 48px.
  assert_eq!(
    images[0].sizing,
    flowstate_document::ImageSizing::Fixed {
      width_px: 64,
      height_px: Some(48)
    },
    "shape style extent must map to fixed pixel sizing"
  );
}

/// §act-eleven C7: legacy VML images (`<w:pict><v:imagedata r:id=…/>`) import
/// as real image blocks with the shape-style extent — previously dropped
/// wholesale (one of the four old corpus residuals was a 27-image VML doc).
/// Deterministic fixture so the CLASS is guarded, not just the one corpus doc.
#[test]
fn vml_imagedata_imports_as_image_block_with_style_extent() {
  use std::io::Write as _;
  let png = {
    let buffer = image::RgbImage::from_pixel(4, 3, image::Rgb([10u8, 120, 240]));
    let mut cursor = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(buffer)
      .write_to(&mut cursor, image::ImageFormat::Png)
      .expect("encode fixture png");
    cursor.into_inner()
  };
  let document_xml = concat!(
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
    r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" "#,
    r#"xmlns:v="urn:schemas-microsoft-com:vml" "#,
    r#"xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">"#,
    r#"<w:body>"#,
    r#"<w:p><w:r><w:t>before</w:t></w:r></w:p>"#,
    r#"<w:p><w:r><w:pict><v:shape style="width:48pt;height:36pt"><v:imagedata r:id="rId9"/></v:shape></w:pict></w:r></w:p>"#,
    r#"<w:p><w:r><w:t>after</w:t></w:r></w:p>"#,
    r#"</w:body></w:document>"#
  );
  let rels = concat!(
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
    r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
    r#"<Relationship Id="rId9" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/image1.png"/>"#,
    r#"</Relationships>"#
  );
  let root_rels = concat!(
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
    r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
    r#"<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>"#,
    r#"</Relationships>"#
  );
  let content_types = concat!(
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
    r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">"#,
    r#"<Default Extension="png" ContentType="image/png"/>"#,
    r#"<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>"#,
    r#"<Default Extension="xml" ContentType="application/xml"/>"#,
    r#"<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>"#,
    r#"</Types>"#
  );
  let mut cursor = std::io::Cursor::new(Vec::new());
  {
    let mut writer = zip::ZipWriter::new(&mut cursor);
    let options = zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for (name, bytes) in [
      ("[Content_Types].xml", content_types.as_bytes()),
      ("_rels/.rels", root_rels.as_bytes()),
      ("word/document.xml", document_xml.as_bytes()),
      ("word/_rels/document.xml.rels", rels.as_bytes()),
      ("word/media/image1.png", png.as_slice()),
    ] {
      writer.start_file(name, options).expect("zip entry");
      writer.write_all(bytes).expect("zip bytes");
    }
    writer.finish().expect("zip finish")
  };
  let bytes = cursor.into_inner();

  let projection = assert_import_defect_free(&bytes, "vml import");
  let images: Vec<_> = projection
    .blocks
    .iter()
    .filter_map(|block| match block {
      flowstate_document::Block::Image(image) => Some(image.clone()),
      _ => None,
    })
    .collect();
  assert_eq!(images.len(), 1, "the VML pict must import as ONE image block");
  // 48pt x 36pt at 96dpi = 64px x 48px.
  assert_eq!(
    images[0].sizing,
    flowstate_document::ImageSizing::Fixed {
      width_px: 64,
      height_px: Some(48)
    },
    "shape style extent must map to fixed pixel sizing"
  );
  // Asset BYTES live out-of-band of Loro (package-side); assert them on the
  // docx-built projection, which is what the live app persists.
  let (imported, _report) = import_docx_bytes_to_loro(&bytes, "vml import assets").expect("import docx bytes");
  let asset = imported
    .projection
    .assets
    .assets
    .get(&images[0].asset_id)
    .expect("asset record attached to the docx-built projection");
  assert_eq!(asset.bytes.as_slice(), png.as_slice(), "asset bytes must be the media part verbatim");
}

/// Build a rich document, write a real .docx, re-import it, and assert the projection has
/// ZERO defects — no record-less/mark-less paragraphs to trigger a repair storm.
#[test]
fn synthetic_docx_roundtrip_has_no_projection_defects() {
  let document = rich_document(400);
  let path = temp_docx_path();
  write_docx(&path, &document).expect("write docx");
  let bytes = std::fs::read(&path).expect("read docx");
  let _ = std::fs::remove_file(&path);
  let projection = assert_import_defect_free(&bytes, "synthetic roundtrip");
  // Defect-freeness alone would also pass if the break were silently DROPPED;
  // pin the soft break to U+2028 in the imported text as well.
  assert!(
    projection
      .text
      .to_string()
      .contains(&soft_break_probe_text()),
    "synthetic roundtrip: body-level soft break did not survive import as U+2028",
  );
}

/// Collect the vertical alignment of every projected body run, in order.
fn body_run_vert_aligns(projection: &flowstate_document::DocumentProjection) -> Vec<VertAlign> {
  projection
    .paragraphs
    .iter()
    .flat_map(|paragraph| paragraph.runs.iter())
    .map(|run| run.styles.vert_align)
    .collect()
}

/// Phase-1 parser net: a real OOXML `<w:vertAlign>` run property is captured on
/// import and lands on the projected run styles (superscript AND subscript).
/// This reads genuine Word XML — not our own exporter's output — so it cannot be
/// fooled by a shared import/export bug.
#[test]
fn vert_align_run_property_is_captured_on_import() {
  let document_xml = concat!(
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
    r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">"#,
    r#"<w:body>"#,
    r#"<w:p>"#,
    r#"<w:r><w:t xml:space="preserve">E=mc</w:t></w:r>"#,
    r#"<w:r><w:rPr><w:vertAlign w:val="superscript"/></w:rPr><w:t>2</w:t></w:r>"#,
    r#"<w:r><w:t xml:space="preserve"> and CO</w:t></w:r>"#,
    r#"<w:r><w:rPr><w:vertAlign w:val="subscript"/></w:rPr><w:t>2</w:t></w:r>"#,
    r#"</w:p>"#,
    r#"</w:body></w:document>"#
  );
  let rels = concat!(
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
    r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"></Relationships>"#
  );
  let bytes = build_docx_zip(document_xml, rels, &[]);
  let projection = assert_import_defect_free(&bytes, "vert-align import");

  let aligns = body_run_vert_aligns(&projection);
  assert!(
    aligns.contains(&VertAlign::Superscript),
    "superscript `w:vertAlign` run must import as VertAlign::Superscript; saw {aligns:?}"
  );
  assert!(
    aligns.contains(&VertAlign::Subscript),
    "subscript `w:vertAlign` run must import as VertAlign::Subscript; saw {aligns:?}"
  );
}

/// Phase-1+2 net: super/subscript on a model run survives a real
/// export → re-import round-trip (`write_docx` emits `<w:vertAlign>`, the
/// importer reads it back). Guards the exporter half specifically.
#[test]
fn vert_align_survives_docx_write_and_reimport() {
  let vert_run = |text: &str, vert_align: VertAlign| InputRun {
    text: text.to_string(),
    styles: RunStyles {
      vert_align,
      ..RunStyles::default()
    },
  };
  let blocks = vec![InputBlock::Paragraph(InputParagraph {
    style: ParagraphStyle::Normal,
    runs: vec![
      vert_run("E=mc", VertAlign::Baseline),
      vert_run("2", VertAlign::Superscript),
      vert_run(" and CO", VertAlign::Baseline),
      vert_run("2", VertAlign::Subscript),
    ],
  })];
  let document = document_from_input_blocks(flowstate_document_theme(), blocks);

  let path = temp_docx_path();
  write_docx(&path, &document).expect("write docx");
  let bytes = std::fs::read(&path).expect("read docx");
  let _ = std::fs::remove_file(&path);

  let projection = assert_import_defect_free(&bytes, "vert-align roundtrip");
  let aligns = body_run_vert_aligns(&projection);
  assert!(
    aligns.contains(&VertAlign::Superscript) && aligns.contains(&VertAlign::Subscript),
    "super/subscript must survive write_docx → reimport; saw {aligns:?}"
  );
}

/// Run the same defect-free assertion against a REAL .docx supplied via
/// `FLOWSTATE_DOCX_FIXTURE=/path/to/file.docx` — point it at the impact-defense doc to
/// confirm (or catch) whether that specific file imports clean. Skips when unset so CI is
/// unaffected.
#[test]
fn external_docx_import_has_no_projection_defects() {
  let Some(path) = std::env::var_os("FLOWSTATE_DOCX_FIXTURE") else {
    eprintln!("FLOWSTATE_DOCX_FIXTURE unset — skipping external docx import check");
    return;
  };
  let bytes = std::fs::read(&path).expect("read FLOWSTATE_DOCX_FIXTURE");
  assert_import_defect_free(&bytes, &format!("external docx {path:?}"));
}
