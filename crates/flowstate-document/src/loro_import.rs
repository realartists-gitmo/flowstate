use std::{io, path::Path};

use gpui_flowtext::{
  Block, BlockAlignment, Document, EquationDisplay, EquationSyntax, HighlightStyle, ImageSizing, Paragraph, ParagraphStyle, RunSemanticStyle,
  RunStyles, TableBlock, TableCellBlock, TableColumnWidth, paragraph_text,
};
use loro::{LoroDoc, LoroMap, LoroResult, LoroText, cursor::Side};
use uuid::Uuid;

use crate::{
  AssetChunk, BODY_FLOW_ID, BLOCKS_BY_ID, FLOW_ATTRS_KEY, FLOW_ID_KEY, FLOW_KIND_KEY, FLOW_TEXT_KEY, FLOWS_BY_ID, MARK_DIRECT_UNDERLINE,
  MARK_HIGHLIGHT_STYLE, MARK_PARAGRAPH_STYLE, MARK_RUN_SEMANTIC_STYLE, MARK_STRIKETHROUGH, OBJECT_REPLACEMENT, PARAGRAPHS_BY_ID, ROOT,
  ROOT_BODY_FLOW_ID, SENTINEL_NEWLINE,
  loro_schema::{ASSETS_BY_ID, REVISIONS},
};

pub fn document_to_loro(document: &Document, title: &str) -> io::Result<LoroDoc> {
  let doc = crate::new_loro_document(title).map_err(loro_io_error)?;
  replace_body_from_document(&doc, document).map_err(loro_io_error)?;
  import_assets(&doc, document).map_err(loro_io_error)?;
  doc.commit();
  Ok(doc)
}

pub fn document_to_loro_db8_bytes(document: &Document, title: &str) -> io::Result<Vec<u8>> {
  let doc = document_to_loro(document, title)?;
  crate::DocumentPackage::from_loro_snapshot_with_assets(&doc, title, assets_from_document(document))?.to_bytes()
}

pub fn write_document_as_loro_db8(path: impl AsRef<Path>, document: &Document, title: &str) -> io::Result<()> {
  let doc = document_to_loro(document, title)?;
  crate::DocumentPackage::from_loro_snapshot_with_assets(&doc, title, assets_from_document(document))?.write(path)
}

fn replace_body_from_document(doc: &LoroDoc, document: &Document) -> LoroResult<()> {
  let root = doc.get_map(ROOT);
  let flows = root.ensure_mergeable_map(FLOWS_BY_ID)?;
  let blocks = root.ensure_mergeable_map(BLOCKS_BY_ID)?;
  let paragraphs = root.ensure_mergeable_map(PARAGRAPHS_BY_ID)?;
  root.ensure_mergeable_list(REVISIONS)?;

  let body_flow = ensure_flow(&flows, ROOT_BODY_FLOW_ID, "body")?;
  let body_text = body_flow.ensure_mergeable_text(FLOW_TEXT_KEY)?;
  replace_text(&body_text, SENTINEL_NEWLINE)?;
  clear_map(&blocks)?;
  clear_map(&paragraphs)?;

  let mut paragraph_ix = 0_usize;
  for (block_ix, block) in document.blocks.iter().enumerate() {
    match block {
      Block::Paragraph(paragraph) => {
        let text = paragraph_text(document, paragraph_ix);
        append_paragraph(&body_text, &paragraphs, &blocks, BODY_FLOW_ID, paragraph, &text, block_ix, paragraph_ix)?;
        paragraph_ix += 1;
      }
      Block::Image(image) => {
        let pos = body_text.len_unicode();
        body_text.insert(pos, &OBJECT_REPLACEMENT.to_string())?;
        let block = ensure_block(&blocks, block_id("image", block_ix), "image", BODY_FLOW_ID, &body_text, pos)?;
        block.insert("asset_id", image.asset_id.0.to_string())?;
        block.insert("alt_text_flow_id", nested_flow_id("image_alt", block_ix))?;
        let alt_flow = ensure_flow(&flows, &nested_flow_id("image_alt", block_ix), "alt_text")?;
        replace_text(&alt_flow.ensure_mergeable_text(FLOW_TEXT_KEY)?, image.alt_text.as_ref())?;
        if let Some(caption) = &image.caption {
          block.insert("caption_flow_id", nested_flow_id("image_caption", block_ix))?;
          let caption_flow = ensure_flow(&flows, &nested_flow_id("image_caption", block_ix), "caption")?;
          replace_text(&caption_flow.ensure_mergeable_text(FLOW_TEXT_KEY)?, SENTINEL_NEWLINE)?;
          append_paragraph_text_only(&caption_flow.ensure_mergeable_text(FLOW_TEXT_KEY)?, caption, "")?;
        }
        let attrs = block.ensure_mergeable_map("attrs")?;
        attrs.insert("alignment", alignment_name(image.alignment))?;
        match image.sizing {
          ImageSizing::Intrinsic => attrs.insert("sizing", "intrinsic")?,
          ImageSizing::FitWidth => attrs.insert("sizing", "fit_width")?,
          ImageSizing::Fixed { width_px, height_px } => {
            attrs.insert("sizing", "fixed")?;
            attrs.insert("width_px", i64::from(width_px))?;
            if let Some(height_px) = height_px {
              attrs.insert("height_px", i64::from(height_px))?;
            }
          }
        };
      }
      Block::Equation(equation) => {
        let pos = body_text.len_unicode();
        body_text.insert(pos, &OBJECT_REPLACEMENT.to_string())?;
        let block = ensure_block(&blocks, block_id("equation", block_ix), "equation", BODY_FLOW_ID, &body_text, pos)?;
        let source_flow_id = nested_flow_id("equation_source", block_ix);
        block.insert("source_flow_id", source_flow_id.as_str())?;
        let source_flow = ensure_flow(&flows, &source_flow_id, "equation_source")?;
        replace_text(&source_flow.ensure_mergeable_text(FLOW_TEXT_KEY)?, equation.source.as_ref())?;
        let attrs = block.ensure_mergeable_map("attrs")?;
        attrs.insert("syntax", equation_syntax_name(equation.syntax))?;
        attrs.insert("display", equation_display_name(equation.display))?;
      }
      Block::Table(table) => {
        let pos = body_text.len_unicode();
        body_text.insert(pos, &OBJECT_REPLACEMENT.to_string())?;
        let block = ensure_block(&blocks, block_id("table", block_ix), "table", BODY_FLOW_ID, &body_text, pos)?;
        import_table(&flows, &block, table, &format!("table.{block_ix}"))?;
      }
    }
  }
  doc.commit();
  Ok(())
}

fn append_paragraph(
  body_text: &LoroText,
  paragraphs: &LoroMap,
  blocks: &LoroMap,
  flow_id: &str,
  paragraph: &Paragraph,
  text: &str,
  block_ix: usize,
  paragraph_ix: usize,
) -> LoroResult<()> {
  append_paragraph_text_only(body_text, paragraph, text)?;
  let boundary_pos = body_text.len_unicode() - text.chars().count() - 1;
  let paragraph_id = block_id("paragraph", paragraph_ix);
  let paragraph_map = paragraphs.ensure_mergeable_map(&paragraph_id)?;
  paragraph_map.insert("id", paragraph_id.as_str())?;
  paragraph_map.insert("flow_id", flow_id)?;
  if let Some(cursor) = body_text.get_cursor(boundary_pos, Side::Left) {
    paragraph_map.insert("start_cursor", cursor.encode())?;
  }
  if let Some(cursor) = body_text.get_cursor(boundary_pos, Side::Right) {
    paragraph_map.insert("boundary_cursor", cursor.encode())?;
  }
  paragraph_map.ensure_mergeable_map("attrs")?;
  ensure_block(blocks, block_id("paragraph_block", block_ix), "paragraph", flow_id, body_text, boundary_pos)?;
  Ok(())
}

fn append_paragraph_text_only(text: &LoroText, paragraph: &Paragraph, paragraph_body: &str) -> LoroResult<()> {
  let use_existing_sentinel = text.len_unicode() == 1 && text.to_string() == SENTINEL_NEWLINE;
  let boundary_pos = if use_existing_sentinel {
    0
  } else {
    let pos = text.len_unicode();
    text.insert(pos, "\n")?;
    pos
  };
  text.mark(boundary_pos..boundary_pos + 1, MARK_PARAGRAPH_STYLE, paragraph_style_value(paragraph.style))?;
  let text_pos = text.len_unicode();
  if !paragraph_body.is_empty() {
    text.insert(text_pos, paragraph_body)?;
    mark_runs(text, text_pos, paragraph_body, &paragraph.runs)?;
  }
  Ok(())
}

fn mark_runs(text: &LoroText, start: usize, paragraph_body: &str, runs: &[gpui_flowtext::TextRun]) -> LoroResult<()> {
  let mut offset = 0_usize;
  for run in runs {
    let Some(run_text) = paragraph_body.get(offset..offset.saturating_add(run.len)) else {
      break;
    };
    let run_len = run_text.chars().count();
    let range = start + paragraph_body[..offset].chars().count()..start + paragraph_body[..offset].chars().count() + run_len;
    mark_run_styles(text, range, run.styles)?;
    offset = offset.saturating_add(run.len);
  }
  Ok(())
}

fn mark_run_styles(text: &LoroText, range: std::ops::Range<usize>, styles: RunStyles) -> LoroResult<()> {
  if let RunSemanticStyle::Custom(slot) = styles.semantic {
    text.mark(range.clone(), MARK_RUN_SEMANTIC_STYLE, i64::from(slot))?;
  }
  if let Some(HighlightStyle::Custom(slot)) = styles.highlight {
    text.mark(range.clone(), MARK_HIGHLIGHT_STYLE, i64::from(slot))?;
  }
  if styles.direct_underline {
    text.mark(range.clone(), MARK_DIRECT_UNDERLINE, true)?;
  }
  if styles.strikethrough {
    text.mark(range, MARK_STRIKETHROUGH, true)?;
  }
  Ok(())
}

fn import_table(flows: &LoroMap, block: &LoroMap, table: &TableBlock, prefix: &str) -> LoroResult<()> {
  let table_map = block.ensure_mergeable_map("table")?;
  let row_order = table_map.ensure_mergeable_list("row_order")?;
  let column_order = table_map.ensure_mergeable_list("column_order")?;
  let rows_by_id = table_map.ensure_mergeable_map("rows_by_id")?;
  let columns_by_id = table_map.ensure_mergeable_map("columns_by_id")?;
  let cells_by_id = table_map.ensure_mergeable_map("cells_by_id")?;
  table_map.insert("header_row", table.style.header_row)?;

  clear_list(&row_order)?;
  clear_list(&column_order)?;
  clear_map(&rows_by_id)?;
  clear_map(&columns_by_id)?;
  clear_map(&cells_by_id)?;

  for (column_ix, width) in table.column_widths.iter().enumerate() {
    let column_id = format!("{prefix}.column.{column_ix}");
    column_order.push(column_id.as_str())?;
    let column = columns_by_id.ensure_mergeable_map(&column_id)?;
    column.insert("id", column_id.as_str())?;
    match width {
      TableColumnWidth::Auto => column.insert("width_kind", "auto")?,
      TableColumnWidth::FixedPx(px) => {
        column.insert("width_kind", "fixed_px")?;
        column.insert("width_px", i64::from(*px))?;
      }
      TableColumnWidth::Fraction(fraction) => {
        column.insert("width_kind", "fraction")?;
        column.insert("fraction", i64::from(*fraction))?;
      }
    };
  }

  for (row_ix, row) in table.rows.iter().enumerate() {
    let row_id = format!("{prefix}.row.{row_ix}");
    row_order.push(row_id.as_str())?;
    let row_map = rows_by_id.ensure_mergeable_map(&row_id)?;
    row_map.insert("id", row_id.as_str())?;
    for (cell_ix, cell) in row.cells.iter().enumerate() {
      let cell_id = format!("{row_id}.cell.{cell_ix}");
      let cell_map = cells_by_id.ensure_mergeable_map(&cell_id)?;
      cell_map.insert("id", cell_id.as_str())?;
      cell_map.insert("row_id", row_id.as_str())?;
      cell_map.insert("column_index", i64::try_from(cell_ix).unwrap_or(i64::MAX))?;
      cell_map.insert("row_span", i64::from(cell.row_span))?;
      cell_map.insert("column_span", i64::from(cell.col_span))?;
      let flow_id = format!("{cell_id}.flow");
      cell_map.insert("flow_id", flow_id.as_str())?;
      let flow = ensure_flow(flows, &flow_id, "table_cell")?;
      let text = flow.ensure_mergeable_text(FLOW_TEXT_KEY)?;
      replace_text(&text, SENTINEL_NEWLINE)?;
      for (block_ix, cell_block) in cell.blocks.iter().enumerate() {
        match cell_block {
          TableCellBlock::Paragraph(paragraph) => append_paragraph_text_only(&text, &paragraph.paragraph, &paragraph.text)?,
          TableCellBlock::Table(nested) => {
            let pos = text.len_unicode();
            text.insert(pos, &OBJECT_REPLACEMENT.to_string())?;
            let nested_map = cell_map.ensure_mergeable_map(&format!("nested_table.{block_ix}"))?;
            import_table(flows, &nested_map, nested, &format!("{cell_id}.nested.{block_ix}"))?;
          }
        }
      }
    }
  }
  Ok(())
}

fn import_assets(doc: &LoroDoc, document: &Document) -> LoroResult<()> {
  let root = doc.get_map(ROOT);
  let assets = root.ensure_mergeable_map(ASSETS_BY_ID)?;
  clear_map(&assets)?;
  for asset in document.assets.assets.values() {
    let asset_id = asset.asset_id_string();
    let asset_map = assets.ensure_mergeable_map(&asset_id)?;
    let hash = blake3::hash(&asset.bytes);
    asset_map.insert("asset_id", asset_id.as_str())?;
    asset_map.insert("content_hash", hash.to_hex().as_str())?;
    asset_map.insert("mime_type", asset.mime_type.as_ref())?;
    asset_map.insert("byte_length", i64::try_from(asset.bytes.len()).unwrap_or(i64::MAX))?;
    if let Some(original_name) = &asset.original_name {
      asset_map.insert("original_name", original_name.as_ref())?;
    }
  }
  Ok(())
}

pub fn assets_from_document(document: &Document) -> Vec<AssetChunk> {
  document
    .assets
    .assets
    .values()
    .map(|asset| AssetChunk {
      asset_id: asset.id.0,
      content_hash: *blake3::hash(&asset.bytes).as_bytes(),
      mime_type: asset.mime_type.to_string(),
      byte_length: asset.bytes.len() as u64,
      bytes: Vec::clone(&asset.bytes),
      metadata: Vec::new(),
    })
    .collect()
}

trait AssetRecordExt {
  fn asset_id_string(&self) -> String;
}

impl AssetRecordExt for gpui_flowtext::AssetRecord {
  fn asset_id_string(&self) -> String {
    self.id.0.to_string()
  }
}

fn ensure_flow(flows: &LoroMap, flow_id: &str, kind: &str) -> LoroResult<LoroMap> {
  let flow = flows.ensure_mergeable_map(flow_id)?;
  flow.insert(FLOW_ID_KEY, flow_id)?;
  flow.insert(FLOW_KIND_KEY, kind)?;
  flow.ensure_mergeable_text(FLOW_TEXT_KEY)?;
  flow.ensure_mergeable_map(FLOW_ATTRS_KEY)?;
  Ok(flow)
}

fn ensure_block(blocks: &LoroMap, block_id: String, kind: &str, flow_id: &str, text: &LoroText, pos: usize) -> LoroResult<LoroMap> {
  let block = blocks.ensure_mergeable_map(&block_id)?;
  block.insert("id", block_id.as_str())?;
  block.insert("kind", kind)?;
  block.insert("flow_id", flow_id)?;
  if let Some(cursor) = text.get_cursor(pos, Side::Left) {
    block.insert("anchor_cursor", cursor.encode())?;
  }
  block.ensure_mergeable_map("attrs")?;
  block.ensure_mergeable_map("nested_refs")?;
  Ok(block)
}

fn replace_text(text: &LoroText, value: &str) -> LoroResult<()> {
  let len = text.len_unicode();
  if len > 0 {
    text.delete(0, len)?;
  }
  if !value.is_empty() {
    text.insert(0, value)?;
  }
  Ok(())
}

fn clear_map(map: &LoroMap) -> LoroResult<()> {
  let keys = map.keys();
  for key in keys {
    map.delete(&key)?;
  }
  Ok(())
}

fn clear_list(list: &loro::LoroList) -> LoroResult<()> {
  let len = list.len();
  if len > 0 {
    list.delete(0, len)?;
  }
  Ok(())
}

fn paragraph_style_value(style: ParagraphStyle) -> i64 {
  match style {
    ParagraphStyle::Normal => 0,
    ParagraphStyle::Custom(slot) => i64::from(slot),
  }
}

fn block_id(kind: &str, ix: usize) -> String {
  format!("{kind}.{ix}.{}", Uuid::new_v5(&Uuid::NAMESPACE_OID, format!("{kind}.{ix}").as_bytes()).as_u128())
}

fn nested_flow_id(kind: &str, ix: usize) -> String {
  format!("{kind}.{ix}")
}

fn alignment_name(alignment: BlockAlignment) -> &'static str {
  match alignment {
    BlockAlignment::Left => "left",
    BlockAlignment::Center => "center",
    BlockAlignment::Right => "right",
  }
}

fn equation_syntax_name(syntax: EquationSyntax) -> &'static str {
  match syntax {
    EquationSyntax::Latex => "latex",
  }
}

fn equation_display_name(display: EquationDisplay) -> &'static str {
  match display {
    EquationDisplay::Display => "display",
    EquationDisplay::InlineLikeParagraph => "inline_like_paragraph",
  }
}

fn loro_io_error(error: impl std::error::Error + Send + Sync + 'static) -> io::Error {
  io::Error::new(io::ErrorKind::InvalidData, error)
}
