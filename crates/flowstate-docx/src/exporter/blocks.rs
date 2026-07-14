use std::borrow::Cow;

use docx_rs::{
  AlignmentType, BreakType, Docx, Paragraph as DocxParagraph, Pic, Run, SectionProperty, Shading, Table as DocxTable,
  TableCell as DocxTableCell, TableLayoutType, TableRow as DocxTableRow, VMergeType, VertAlignType, WidthType,
};
use flowstate_document::{
  Block, BlockAlignment, DocumentProjection, DocumentTheme, EquationBlock, EquationDisplay, HighlightStyle, ImageBlock, ImageSizing, Paragraph,
  ParagraphStyle, RunSemanticStyle, RunStyles, SOFT_LINE_BREAK, TableBlock, TableCell, TableCellBlock, TableCellParagraph, TableColumnWidth,
  VertAlign, document_text_slice,
};
use flowstate_fidelity::FidelityClass;

use super::{
  formatting::{apply_run_text_format, color_hex, docx_fonts},
  omml_export::latex_to_omml,
  sections::{SectionContext, px_to_twips, twips_to_emu},
  styles::{apply_paragraph_style, apply_semantic_run_text_border},
  xml_postprocess::SideChannel,
};

#[hotpath::measure]
pub(super) fn add_block(
  docx: Docx,
  document: &DocumentProjection,
  paragraph_ix: usize,
  block: &Block,
  theme: &DocumentTheme,
  context: &SectionContext,
  side: &mut SideChannel,
  boundary: Option<SectionProperty>,
) -> Docx {
  match block {
    Block::Paragraph(paragraph) => {
      let mut docx_paragraph = export_document_paragraph(document, paragraph_ix, paragraph, theme);
      // FS-126: a non-final section terminates in the last paragraph before its
      // boundary, carrying that section's page properties in its `w:sectPr`.
      if let Some(section_property) = boundary {
        docx_paragraph = docx_paragraph.section_property(section_property);
      }
      docx.add_paragraph(docx_paragraph)
    },
    Block::Table(table) => docx.add_table(export_table(table, theme, context)),
    Block::Image(image) => docx.add_paragraph(export_image(document, image, theme, context, side)),
    Block::Equation(equation) => docx.add_paragraph(omml_paragraph_for_equation(equation, theme, side)),
  }
}

#[hotpath::measure]
fn export_document_paragraph(document: &DocumentProjection, paragraph_ix: usize, paragraph: &Paragraph, theme: &DocumentTheme) -> DocxParagraph {
  // §perf-heaven T8.6: `byte_range` is no longer a `Paragraph` field; the text
  // span is derived on demand via `paragraph_byte_range` (block tree `O(log N)`
  // `paragraph_start`, which includes the inter-block `\n` separators).
  let text = document_text_slice(document, flowstate_document::paragraph_byte_range(document, paragraph_ix));
  export_paragraph_with_text(paragraph, &text, theme, false)
}

#[hotpath::measure]
fn export_table_cell_paragraph(paragraph: &TableCellParagraph, theme: &DocumentTheme, force_bold: bool) -> DocxParagraph {
  export_paragraph_with_text(&paragraph.paragraph, &paragraph.text, theme, force_bold)
}

#[hotpath::measure]
fn export_paragraph_with_text(paragraph: &Paragraph, text: &str, theme: &DocumentTheme, force_bold: bool) -> DocxParagraph {
  let mut out = apply_paragraph_style(DocxParagraph::new(), paragraph.style, theme);
  let mut byte = 0usize;
  for run in &paragraph.runs {
    let start = byte.min(text.len());
    let end = (byte + run.len).min(text.len()).max(start);
    out = add_text_run(out, &text[start..end], run.styles, paragraph.style, theme, force_bold);
    byte = end;
  }
  if paragraph.runs.is_empty() && text.is_empty() {
    out = out.add_run(Run::new());
  }
  if matches!(paragraph.style, flowstate_document::PARAGRAPH_POCKET | flowstate_document::PARAGRAPH_HAT) {
    // §act6 (round-trip fidelity): start the header on a new page via the
    // `w:pageBreakBefore` PARAGRAPH PROPERTY, not a trailing page-break RUN. The
    // run form reimports as body text — the importer maps EVERY `<w:br>` (page
    // included, structured.rs) to `SOFT_LINE_BREAK` — so it injected a spurious
    // U+2028 after every pocket/hat header, breaking export->reimport idempotency
    // on ~80% of the corpus. A property does not reimport as text, and a header
    // beginning a new page is the correct layout regardless.
    out = out.page_break_before(true);
  }
  out
}

#[hotpath::measure]
fn add_text_run(
  mut paragraph: DocxParagraph,
  text: &str,
  styles: RunStyles,
  paragraph_style: ParagraphStyle,
  theme: &DocumentTheme,
  force_bold: bool,
) -> DocxParagraph {
  let mut first = true;
  for segment in text.split(SOFT_LINE_BREAK) {
    if !first {
      paragraph = paragraph.add_run(apply_run_style(
        Run::new().add_break(BreakType::TextWrapping),
        styles,
        paragraph_style,
        theme,
        force_bold,
      ));
    }
    first = false;
    if !segment.is_empty() {
      // §perf: `replace` always allocates a new String even when U+F8FF is
      // absent (the common case). Only allocate the substituted copy when the
      // sentinel is actually present; otherwise borrow the original slice.
      let segment = if segment.contains('\u{f8ff}') {
        Cow::Owned(segment.replace('\u{f8ff}', "¶"))
      } else {
        Cow::Borrowed(segment)
      };
      paragraph = paragraph.add_run(apply_run_style(
        Run::new().add_text(segment.as_ref()),
        styles,
        paragraph_style,
        theme,
        force_bold,
      ));
    }
  }
  paragraph
}

#[hotpath::measure]
fn apply_run_style(run: Run, styles: RunStyles, paragraph_style: ParagraphStyle, theme: &DocumentTheme, force_bold: bool) -> Run {
  let mut run = run.fonts(docx_fonts(theme));
  run = match styles.semantic {
    flowstate_document::SEMANTIC_CITE => apply_semantic_run_text_border(run.style("Style13ptBold"), theme, 1),
    flowstate_document::SEMANTIC_EMPHASIS => apply_semantic_run_text_border(run.style("Emphasis"), theme, 2),
    flowstate_document::SEMANTIC_UNDERLINE => apply_semantic_run_text_border(run.style("StyleUnderline"), theme, 3),
    flowstate_document::SEMANTIC_CONDENSED => apply_semantic_run_text_border(run.style("Condensed"), theme, 4),
    flowstate_document::SEMANTIC_ULTRACONDENSED => apply_semantic_run_text_border(run.style("UltraCondensed"), theme, 5),
    RunSemanticStyle::Plain | RunSemanticStyle::Custom(_) => run,
  };
  if styles.semantic == RunSemanticStyle::Plain && paragraph_style == ParagraphStyle::Normal {
    run = apply_run_text_format(
      run,
      theme.body_font_size,
      theme.default_text_color,
      theme.normal_bold,
      theme.normal_italic,
      theme.normal_underline,
    );
  }
  if styles.direct_underline {
    run = run.underline("single");
  }
  if styles.strikethrough {
    run = run.strike();
  }
  // Superscript/subscript (orthogonal to the semantic style). docx-rs exposes
  // this only on `RunProperty`, so set it there directly; `clone` keeps the
  // partial move off `run` and is negligible on the export path.
  match styles.vert_align {
    VertAlign::Baseline => {},
    VertAlign::Superscript => {
      run.run_property = run
        .run_property
        .clone()
        .vert_align(VertAlignType::SuperScript)
    },
    VertAlign::Subscript => {
      run.run_property = run
        .run_property
        .clone()
        .vert_align(VertAlignType::SubScript)
    },
  }
  if let Some(highlight) = styles.highlight {
    run = run.shading(
      Shading::new().fill(color_hex(match highlight {
        HighlightStyle::Custom(slot) => theme
          .custom_highlight_styles
          .get(&(slot & 0x7f))
          .map(|style| style.color)
          .unwrap_or(theme.default_highlight_color),
      })),
    );
  }
  // FS-124: bold the header row as a visible fallback for the `w:tblHeader`
  // repeat-on-page semantics injected during XML post-processing.
  if force_bold {
    run = run.bold();
  }
  run
}

// -- Tables (FS-123 spans, FS-124 grid/header) -------------------------------

#[hotpath::measure]
fn export_table(table: &TableBlock, theme: &DocumentTheme, context: &SectionContext) -> DocxTable {
  // FS-124 fidelity: the `w:tblHeader` marker is emitted only on the first row,
  // so a header-flagged table with no rows loses its repeat-on-page header
  // semantics.
  flowstate_fidelity::check(
    !table.style.header_row || !table.rows.is_empty(),
    FidelityClass::ImportExport,
    "export-dropped-header",
    || "table has header_row set but no rows; w:tblHeader marker not emitted".to_string(),
  );
  let content_width = context.content_width_twips.max(1);
  let column_widths: Vec<TableColumnWidth> = table
    .columns
    .iter()
    .map(|column| column.width.clone())
    .collect();
  let mut grid = compute_grid_twips(&column_widths, content_width);
  if grid.is_empty() {
    // No canonical column widths: derive an equal-width grid from the widest row
    // so column spans and vertical merges still resolve.
    let derived = derived_column_count(&table.rows);
    if derived > 0 {
      let each = (content_width / derived as i64).max(1);
      grid = vec![each; derived];
    }
  }
  let column_count = grid.len();

  // Track ongoing vertical merges per origin column so continuation cells can be
  // synthesized in following rows (docx-rs never generates them).
  let mut active: Vec<Option<ActiveMerge>> = vec![None; column_count];
  let mut rows: Vec<DocxTableRow> = Vec::with_capacity(table.rows.len());

  for (row_ix, row) in table.rows.iter().enumerate() {
    let is_header = table.style.header_row && row_ix == 0;
    let cells = export_row_cells(row, &grid, column_count, is_header, theme, context, &mut active);
    let mut docx_row = DocxTableRow::new(cells);
    if is_header {
      // The only per-row `trPr` child docx-rs 0.4.20 exposes; the post-process
      // pass rewrites this marker to `<w:tblHeader/>` (FS-124).
      docx_row = docx_row.cant_split();
    }
    rows.push(docx_row);
  }

  let mut docx_table = DocxTable::new(rows).layout(TableLayoutType::Fixed);
  if !grid.is_empty() {
    docx_table = docx_table.set_grid(grid.iter().map(|width| *width as usize).collect());
  }
  let total: i64 = grid.iter().sum();
  let table_width = if total > 0 { total } else { content_width };
  docx_table.width(table_width.max(1) as usize, WidthType::Dxa)
}

#[derive(Clone, Copy)]
struct ActiveMerge {
  rows_left: u16,
  col_span: u16,
}

#[hotpath::measure]
fn export_row_cells(
  row: &flowstate_document::TableRow,
  grid: &[i64],
  column_count: usize,
  is_header: bool,
  theme: &DocumentTheme,
  context: &SectionContext,
  active: &mut [Option<ActiveMerge>],
) -> Vec<DocxTableCell> {
  let mut cells: Vec<DocxTableCell> = Vec::new();
  let mut source = row.cells.iter();
  let mut column = 0usize;

  while column < column_count {
    if let Some(mut merge) = active[column] {
      // Continuation cell for a vertical merge that started in an earlier row.
      let span = (merge.col_span.max(1) as usize).min(column_count - column);
      let width = grid_slice_width(grid, column, span);
      let mut cell = DocxTableCell::new()
        .add_paragraph(empty_cell_paragraph())
        .vertical_merge(VMergeType::Continue)
        .width(width, WidthType::Dxa);
      if span > 1 {
        cell = cell.grid_span(span);
      }
      cells.push(cell);
      merge.rows_left -= 1;
      active[column] = if merge.rows_left == 0 { None } else { Some(merge) };
      column += span.max(1);
    } else if let Some(source_cell) = source.next() {
      let col_span = (source_cell.col_span.max(1) as usize).min(column_count - column);
      let row_span = source_cell.row_span.max(1);
      let width = grid_slice_width(grid, column, col_span);
      let mut cell = export_table_cell(source_cell, theme, context, is_header).width(width, WidthType::Dxa);
      if col_span > 1 {
        cell = cell.grid_span(col_span);
      }
      if row_span > 1 {
        cell = cell.vertical_merge(VMergeType::Restart);
        active[column] = Some(ActiveMerge {
          rows_left: row_span - 1,
          col_span: col_span as u16,
        });
      }
      cells.push(cell);
      column += col_span.max(1);
    } else {
      // Row declared fewer cells than the grid; pad so column topology stays valid.
      let width = grid_slice_width(grid, column, 1);
      cells.push(
        DocxTableCell::new()
          .add_paragraph(empty_cell_paragraph())
          .width(width, WidthType::Dxa),
      );
      column += 1;
    }
  }

  // Repair malformed topology: append any surplus source cells rather than
  // dropping their content (they lose span alignment but remain visible).
  for source_cell in source {
    let col_span = source_cell.col_span.max(1) as usize;
    let mut cell = export_table_cell(source_cell, theme, context, is_header);
    if col_span > 1 {
      cell = cell.grid_span(col_span);
    }
    cells.push(cell);
  }

  cells
}

fn derived_column_count(rows: &[flowstate_document::TableRow]) -> usize {
  rows
    .iter()
    .map(|row| {
      row
        .cells
        .iter()
        .map(|cell| usize::from(cell.col_span.max(1)))
        .sum::<usize>()
    })
    .max()
    .unwrap_or(0)
}

fn grid_slice_width(grid: &[i64], start: usize, span: usize) -> usize {
  let end = (start + span).min(grid.len());
  let sum: i64 = grid
    .get(start..end)
    .map(|slice| slice.iter().sum())
    .unwrap_or(0);
  sum.max(1) as usize
}

#[hotpath::measure]
fn export_table_cell(cell: &TableCell, theme: &DocumentTheme, context: &SectionContext, is_header: bool) -> DocxTableCell {
  let mut out = DocxTableCell::new();
  let mut emitted = false;
  let mut last_was_table = false;
  for block in &cell.blocks {
    out = match block {
      TableCellBlock::Paragraph(paragraph) => {
        last_was_table = false;
        emitted = true;
        out.add_paragraph(export_table_cell_paragraph(paragraph, theme, is_header))
      },
      TableCellBlock::Table(table) => {
        last_was_table = true;
        emitted = true;
        out.add_table(export_table(table, theme, context))
      },
    };
  }
  // A cell must contain at least one paragraph and cannot end with a table.
  if !emitted || last_was_table {
    out = out.add_paragraph(empty_cell_paragraph());
  }
  out
}

fn empty_cell_paragraph() -> DocxParagraph {
  DocxParagraph::new()
}

/// Map canonical column widths to a DOCX grid in twips. Fixed pixels convert
/// directly; fractional and auto columns share the content width remaining after
/// fixed columns (auto counts as one fractional unit).
fn compute_grid_twips(widths: &[TableColumnWidth], content_width: i64) -> Vec<i64> {
  if widths.is_empty() {
    return Vec::new();
  }
  let fixed_total: i64 = widths
    .iter()
    .filter_map(|width| match width {
      TableColumnWidth::FixedPx(px) => Some(px_to_twips(*px)),
      _ => None,
    })
    .sum();
  let weight_sum: i64 = widths
    .iter()
    .map(|width| match width {
      TableColumnWidth::Fraction(weight) => i64::from(*weight),
      TableColumnWidth::Auto => 1,
      TableColumnWidth::FixedPx(_) => 0,
    })
    .sum();
  let remaining = (content_width - fixed_total).max(0);
  widths
    .iter()
    .map(|width| match width {
      TableColumnWidth::FixedPx(px) => px_to_twips(*px).max(1),
      TableColumnWidth::Fraction(weight) => proportional_width(remaining, i64::from(*weight), weight_sum),
      TableColumnWidth::Auto => proportional_width(remaining, 1, weight_sum),
    })
    .collect()
}

fn proportional_width(remaining: i64, weight: i64, weight_sum: i64) -> i64 {
  if weight_sum <= 0 {
    return 1;
  }
  ((i128::from(remaining) * i128::from(weight)) / i128::from(weight_sum)).max(1) as i64
}

// -- Images (FS-127 alt, FS-128 sizing, FS-129 transcode) --------------------

#[hotpath::measure]
fn export_image(
  document: &DocumentProjection,
  image: &ImageBlock,
  theme: &DocumentTheme,
  context: &SectionContext,
  side: &mut SideChannel,
) -> DocxParagraph {
  // FS-127 fidelity: an image caption has no OOXML representation in this
  // exporter, so a present caption is silently dropped on export.
  flowstate_fidelity::check(image.caption.is_none(), FidelityClass::ImportExport, "export-dropped-caption", || {
    format!("image asset {:?} caption is not written to docx export", image.asset_id)
  });
  if let Some(asset) = document.assets.assets.get(&image.asset_id)
    && !asset.is_loading_placeholder()
  {
    match prepare_embeddable_image(asset.bytes.as_ref()) {
      EmbedResult::Ready(bytes) => {
        let (width_px, height_px) = image_dimensions(&bytes, image);
        let mut pic = Pic::new_with_dimensions(bytes, width_px, height_px);
        if let Some((width_emu, height_emu)) = fit_width_emu(image, context, width_px, height_px) {
          pic = pic.size(width_emu, height_emu);
        }
        // FS-127: alt text is written onto `wp:docPr` (descr + title) during the
        // post-process pass; embeddable images keep no bracketed text fallback.
        let alt = image_alt_text(document, image);
        side.push_image(alt.clone(), alt);
        return aligned_paragraph(image.alignment).add_run(Run::new().fonts(docx_fonts(theme)).add_image(pic));
      },
      EmbedResult::Fallback(reason) => side.push_warning(reason),
    }
  }
  // §A11.9: a genuinely-LINKED image (external URL, no embeddable asset bytes)
  // exports as an inline drawing whose blip carries `r:link` to an external
  // `TargetMode="External"` relationship. docx-rs has no `r:link` API, so a
  // sentinel run is emitted here and the post-process pass swaps it for the
  // full drawing (the equation-injection seam); the package writer adds the
  // relationship. NOT `push_image` — that ledger pairs with the docx-rs-emitted
  // `wp:docPr` literals in document order, and no such literal exists here (the
  // injected drawing carries its own `docPr` with the alt text baked in).
  if let Some(url) = image.external_url.as_deref().filter(|url| !url.is_empty()) {
    let (width_px, height_px) = match image.sizing {
      ImageSizing::Fixed { width_px, height_px } => (width_px.max(1), height_px.unwrap_or(width_px).max(1)),
      // No bytes to sniff intrinsic dimensions from; match the embedded path's
      // unknown-size default.
      ImageSizing::Intrinsic | ImageSizing::FitWidth => (640, 480),
    };
    let (width_emu, height_emu) = fit_width_emu(image, context, width_px, height_px).unwrap_or((px_to_emu(width_px), px_to_emu(height_px)));
    let alt = image_alt_text(document, image);
    let sentinel = side.push_linked_image(url, alt.as_deref(), width_emu, height_emu);
    return aligned_paragraph(image.alignment).add_run(Run::new().add_text(sentinel));
  }
  // FS-127 fidelity: reaching the text fallback means no `<w:drawing>` (and thus
  // no `wp:docPr descr`) is emitted, so an image carrying alt text loses its
  // accessible descr sentinel on export.
  if flowstate_fidelity::enabled() {
    let has_alt = image_alt_text(document, image).is_some();
    flowstate_fidelity::check(!has_alt, FidelityClass::ImportExport, "export-dropped-alt", || {
      format!(
        "image asset {:?} with alt text fell back to text; docPr descr not emitted",
        image.asset_id
      )
    });
  }
  image_text_fallback(document, image, theme)
}

enum EmbedResult {
  Ready(Vec<u8>),
  Fallback(String),
}

/// Prepare image bytes for embedding. PNG/JPEG embed as-is (docx-rs stores PNG
/// media and transcodes on build); other decodable raster formats (GIF/BMP/
/// TIFF/WebP) are transcoded to PNG here so decode failures degrade to a visible
/// fallback instead of being silently dropped. SVG and undecodable data fall
/// back with a structured warning (FS-129).
fn prepare_embeddable_image(bytes: &[u8]) -> EmbedResult {
  if is_png(bytes) || is_jpeg(bytes) {
    return EmbedResult::Ready(bytes.to_vec());
  }
  // §perf-heaven T8.8: Windows metafiles (EMF/WMF) are VALID docx image parts
  // but the `image` crate can't decode them — they used to fall back to
  // `[Picture N]` body text (the dominant docx round-trip residual, +chars).
  // Embed the original bytes as-is; docx-rs writes them to `word/media/imageN.png`
  // and the recompress pass (`package::write_recompressed_docx`) sniffs the magic
  // and gives the part its real `.emf`/`.wmf` extension + content-type + rel — so
  // the image round-trips LOSSLESSLY as an object instead of corrupting the text.
  if is_metafile(bytes).is_some() {
    return EmbedResult::Ready(bytes.to_vec());
  }
  // §act-ten A9.6: some real-world docx media parts are HEADERLESS raw DIBs
  // (a bare BITMAPINFOHEADER, e.g. 44-byte 1x1 tracking pixels) — valid Word
  // media, but no `BM` magic, so `image` can't sniff them and they degraded to
  // the bracketed alt-text fallback (2 of the 4 corpus roundtrip residuals).
  // Prepend the 14-byte BITMAPFILEHEADER to make a decodable BMP and transcode.
  let dib_shimmed;
  let bytes = match shim_headerless_dib(bytes) {
    Some(bmp) => {
      dib_shimmed = bmp;
      dib_shimmed.as_slice()
    },
    None => bytes,
  };
  match image::load_from_memory(bytes) {
    Ok(decoded) => {
      let mut buffer = std::io::Cursor::new(Vec::new());
      match decoded.write_to(&mut buffer, image::ImageFormat::Png) {
        Ok(()) => EmbedResult::Ready(buffer.into_inner()),
        Err(error) => EmbedResult::Fallback(format!("failed to transcode image to PNG for DOCX embedding: {error}")),
      }
    },
    Err(error) => EmbedResult::Fallback(format!(
      "unsupported image format for DOCX embedding ({error}); exported as descriptive text"
    )),
  }
}

/// §act-ten A9.6: detect a headerless raw DIB (a bare `BITMAPINFOHEADER` — or
/// the ancient `BITMAPCOREHEADER` — with no `BM` file header) and wrap it into
/// a decodable BMP by prepending the 14-byte `BITMAPFILEHEADER`. Word accepts
/// such raw DIBs as media parts; the `image` crate needs the file header.
fn shim_headerless_dib(bytes: &[u8]) -> Option<Vec<u8>> {
  if bytes.len() < 12 || bytes.starts_with(b"BM") {
    return None;
  }
  let header_size = u32::from_le_bytes(bytes[0..4].try_into().ok()?);
  // 40 = BITMAPINFOHEADER, 108/124 = V4/V5, 12 = BITMAPCOREHEADER.
  if !matches!(header_size, 12 | 40 | 108 | 124) || bytes.len() <= header_size as usize {
    return None;
  }
  // Plausibility: positive dimensions bounded to Word-realistic sizes, so a
  // random binary that happens to start with 0x28 doesn't get mis-shimmed.
  let (width, height, bit_count, palette_len) = if header_size == 12 {
    let width = u16::from_le_bytes(bytes[4..6].try_into().ok()?) as i64;
    let height = u16::from_le_bytes(bytes[6..8].try_into().ok()?) as i64;
    let bit_count = u16::from_le_bytes(bytes[10..12].try_into().ok()?);
    let palette = if bit_count <= 8 { (1usize << bit_count) * 3 } else { 0 };
    (width, height, bit_count, palette)
  } else {
    if bytes.len() < 40 {
      return None;
    }
    let width = i32::from_le_bytes(bytes[4..8].try_into().ok()?) as i64;
    let height = (i32::from_le_bytes(bytes[8..12].try_into().ok()?) as i64).abs();
    let bit_count = u16::from_le_bytes(bytes[14..16].try_into().ok()?);
    let colors_used = u32::from_le_bytes(bytes[32..36].try_into().ok()?) as usize;
    let palette = if bit_count <= 8 {
      (if colors_used == 0 { 1usize << bit_count } else { colors_used }) * 4
    } else {
      0
    };
    (width, height, bit_count, palette)
  };
  if width <= 0 || height <= 0 || width > 30_000 || height > 30_000 {
    return None;
  }
  if !matches!(bit_count, 1 | 4 | 8 | 16 | 24 | 32) {
    return None;
  }
  let pixel_offset = 14usize + header_size as usize + palette_len;
  let mut bmp = Vec::with_capacity(14 + bytes.len());
  bmp.extend_from_slice(b"BM");
  bmp.extend_from_slice(&(14u32 + bytes.len() as u32).to_le_bytes());
  bmp.extend_from_slice(&[0, 0, 0, 0]);
  bmp.extend_from_slice(&(pixel_offset as u32).to_le_bytes());
  bmp.extend_from_slice(bytes);
  Some(bmp)
}

/// The docx media extension for a Windows metafile, by magic bytes: EMF (the
/// signature `" EMF"` at offset 40) or WMF (placeable `0xD7CDC69A`, or a standard
/// `META_HEADER` `0x0001`/`0x0002` with mtype `0x0009`). Used by both the export
/// pass-through and the recompress rename. §perf-heaven T8.8.
pub(crate) fn is_metafile(bytes: &[u8]) -> Option<&'static str> {
  if bytes.len() >= 44 && bytes[0..4] == [0x01, 0x00, 0x00, 0x00] && bytes[40..44] == *b" EMF" {
    return Some("emf");
  }
  if bytes.starts_with(&[0xD7, 0xCD, 0xC6, 0x9A]) || bytes.starts_with(&[0x01, 0x00, 0x09, 0x00]) || bytes.starts_with(&[0x02, 0x00, 0x09, 0x00])
  {
    return Some("wmf");
  }
  None
}

/// Pixels to EMU at the `DrawingML` convention of 9,525 EMU per 96-dpi pixel.
fn px_to_emu(px: u32) -> u32 {
  px.saturating_mul(9_525)
}

fn is_png(bytes: &[u8]) -> bool {
  bytes.starts_with(&[137, 80, 78, 71, 13, 10, 26, 10])
}

fn is_jpeg(bytes: &[u8]) -> bool {
  bytes.starts_with(&[0xFF, 0xD8, 0xFF])
}

/// Intrinsic (or explicitly fixed) pixel dimensions used to seed the `DrawingML`
/// extent. Fit-width overrides the extent afterwards via [`fit_width_emu`].
fn image_dimensions(bytes: &[u8], image: &ImageBlock) -> (u32, u32) {
  if let ImageSizing::Fixed { width_px, height_px } = image.sizing {
    return (width_px.max(1), height_px.unwrap_or(width_px).max(1));
  }
  imagesize::blob_size(bytes)
    .map(|size| (size.width.max(1) as u32, size.height.max(1) as u32))
    .unwrap_or((640, 480))
}

/// For [`ImageSizing::FitWidth`], scale the intrinsic aspect ratio to the section
/// content width and return the EMU extent to apply via [`Pic::size`].
fn fit_width_emu(image: &ImageBlock, context: &SectionContext, width_px: u32, height_px: u32) -> Option<(u32, u32)> {
  if !matches!(image.sizing, ImageSizing::FitWidth) {
    return None;
  }
  let width_emu = twips_to_emu(context.content_width_twips.max(1)).max(1);
  let height_emu = if width_px == 0 {
    width_emu
  } else {
    ((u64::from(width_emu) * u64::from(height_px.max(1))) / u64::from(width_px.max(1))) as u32
  };
  Some((width_emu, height_emu.max(1)))
}

fn image_alt_text(document: &DocumentProjection, image: &ImageBlock) -> Option<String> {
  let alt = image.alt_text.trim();
  if !alt.is_empty() {
    return Some(alt.to_string());
  }
  document
    .assets
    .assets
    .get(&image.asset_id)
    .and_then(|asset| asset.original_name.as_ref())
    .map(|name| name.trim().to_string())
    .filter(|name| !name.is_empty())
}

fn aligned_paragraph(alignment: BlockAlignment) -> DocxParagraph {
  match alignment {
    BlockAlignment::Left => DocxParagraph::new(),
    BlockAlignment::Center => DocxParagraph::new().align(AlignmentType::Center),
    BlockAlignment::Right => DocxParagraph::new().align(AlignmentType::Right),
  }
}

fn image_text_fallback(document: &DocumentProjection, image: &ImageBlock, theme: &DocumentTheme) -> DocxParagraph {
  let mut text = image.alt_text.to_string();
  if text.trim().is_empty()
    && let Some(asset) = document.assets.assets.get(&image.asset_id)
    && let Some(name) = &asset.original_name
  {
    text = name.to_string();
  }
  if text.trim().is_empty() {
    text = "Image".to_string();
  }
  aligned_paragraph(image.alignment).add_run(
    Run::new()
      .fonts(docx_fonts(theme))
      .italic()
      .add_text(format!("[{text}]")),
  )
}

// -- Equations (FS-125 OMML) -------------------------------------------------

#[hotpath::measure]
fn omml_paragraph_for_equation(equation: &EquationBlock, theme: &DocumentTheme, side: &mut SideChannel) -> DocxParagraph {
  let display = matches!(equation.display, EquationDisplay::Display);
  if let Some(omml) = latex_to_omml(&equation.source, display) {
    // Emit a placeholder run; the post-process pass swaps the enclosing run for
    // the OMML fragment (docx-rs 0.4.20 cannot express `m:oMath`).
    let sentinel = side.push_equation(omml);
    let paragraph = if display {
      DocxParagraph::new().align(AlignmentType::Center)
    } else {
      DocxParagraph::new()
    };
    return paragraph.add_run(Run::new().add_text(sentinel));
  }
  // FS-125 fidelity: the OMML conversion failed, so the equation degrades to the
  // documented bracketed text fallback. Record the degradation and assert the
  // fallback still carries the source (an equation must never vanish entirely).
  let fallback = format!("[Equation: {}]", equation.source);
  if flowstate_fidelity::enabled() {
    flowstate_fidelity::event(FidelityClass::ImportExport, "equation-omml-fallback", || {
      format!("latex not convertible to OMML; using text fallback: {:?}", equation.source)
    });
    flowstate_fidelity::check(!fallback.is_empty(), FidelityClass::ImportExport, "export-dropped-omml", || {
      format!("equation emitted neither OMML nor a text fallback: {:?}", equation.source)
    });
  }
  DocxParagraph::new().align(AlignmentType::Center).add_run(
    Run::new()
      .fonts(docx_fonts(theme))
      .italic()
      .add_text(fallback),
  )
}
