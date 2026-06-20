use std::{io, path::Path};

use gpui_flowtext::{
  Block, BlockAlignment, DocumentProjection, EquationDisplay, EquationSyntax, HighlightStyle, ImageSizing, Paragraph, ParagraphStyle, RunSemanticStyle,
  RunStyles, TableBlock, TableCellBlock, TableColumnWidth, paragraph_text,
};
use loro::{ContainerTrait as _, LoroDoc, LoroMap, LoroMovableList, LoroResult, LoroText, cursor::Side};
use uuid::Uuid;

use crate::{
  AssetChunk, BODY_FLOW_ID, BLOCKS_BY_ID, FLOW_ATTRS_KEY, FLOW_ID_KEY, FLOW_KIND_KEY, FLOW_TEXT_KEY, FLOWS_BY_ID, MARK_DIRECT_UNDERLINE,
  MARK_HIGHLIGHT_STYLE, MARK_PARAGRAPH_STYLE, MARK_RUN_SEMANTIC_STYLE, MARK_STRIKETHROUGH, OBJECT_REPLACEMENT, PARAGRAPHS_BY_ID, ROOT,
  ROOT_BODY_FLOW_ID, SECTIONS_BY_ID, SENTINEL_NEWLINE,
  loro_schema::{ASSETS_BY_ID, REVISIONS},
};

pub fn document_to_loro(document: &DocumentProjection, title: &str) -> io::Result<LoroDoc> {
  let doc = crate::new_loro_document(title).map_err(loro_io_error)?;
  if document.ids.document_id != 0 {
    crate::loro_schema::set_document_id(&doc, Uuid::from_u128(document.ids.document_id)).map_err(loro_io_error)?;
  }
  replace_body_from_document(&doc, document).map_err(loro_io_error)?;
  import_assets(&doc, document).map_err(loro_io_error)?;
  doc.commit();
  Ok(doc)
}

pub fn write_imported_document_as_loro_db8(path: impl AsRef<Path>, document: &DocumentProjection, title: &str) -> io::Result<()> {
  let doc = document_to_loro(document, title)?;
  crate::DocumentPackage::from_loro_snapshot_with_assets(&doc, title, assets_from_document(document))?.write(path)
}

pub(crate) fn replace_body_from_document(doc: &LoroDoc, document: &DocumentProjection) -> LoroResult<()> {
  let root = doc.get_map(ROOT);
  let flows = root.ensure_mergeable_map(FLOWS_BY_ID)?;
  let blocks = root.ensure_mergeable_map(BLOCKS_BY_ID)?;
  let paragraphs = root.ensure_mergeable_map(PARAGRAPHS_BY_ID)?;
  let sections = root.ensure_mergeable_map(SECTIONS_BY_ID)?;
  root.ensure_mergeable_list(REVISIONS)?;

  let body_flow = ensure_flow(&flows, ROOT_BODY_FLOW_ID, "body")?;
  let body_text = body_flow.ensure_mergeable_text(FLOW_TEXT_KEY)?;
  replace_text(&body_text, SENTINEL_NEWLINE)?;
  clear_map(&blocks)?;
  clear_map(&paragraphs)?;
  clear_map(&sections)?;

  let mut paragraph_ix = 0_usize;
  for (block_ix, block) in document.blocks.iter().enumerate() {
    match block {
      Block::Paragraph(paragraph) => {
        let text = paragraph_text(document, paragraph_ix);
        append_paragraph(
          &body_text,
          &paragraphs,
          &blocks,
          BODY_FLOW_ID,
          paragraph,
          &text,
          projection_block_id(document, block_ix, "paragraph_block"),
          projection_paragraph_id(document, paragraph_ix),
        )?;
        paragraph_ix += 1;
      }
      Block::Image(image) => {
        let pos = body_text.len_unicode();
        body_text.insert(pos, &OBJECT_REPLACEMENT.to_string())?;
        let durable_block_id = projection_block_id(document, block_ix, "image");
        let block = ensure_block(&blocks, durable_block_id.clone(), "image", BODY_FLOW_ID, &body_text, pos)?;
        block.insert("asset_id", image.asset_id.0.to_string())?;
        if let Some(asset) = document.assets.assets.get(&image.asset_id) {
          block.insert("content_hash", blake3::hash(&asset.bytes).to_hex().as_str())?;
          block.insert("mime_type", asset.mime_type.as_ref())?;
          block.insert("byte_length", i64::try_from(asset.bytes.len()).unwrap_or(i64::MAX))?;
        }
        let alt_text_flow_id = nested_flow_id("image_alt", &durable_block_id);
        block.insert("alt_text_flow_id", alt_text_flow_id.as_str())?;
        let alt_flow = ensure_flow(&flows, &alt_text_flow_id, "alt_text")?;
        replace_text(&alt_flow.ensure_mergeable_text(FLOW_TEXT_KEY)?, image.alt_text.as_ref())?;
        if let Some(caption) = &image.caption {
          let caption_flow_id = nested_flow_id("image_caption", &durable_block_id);
          block.insert("caption_flow_id", caption_flow_id.as_str())?;
          let caption_flow = ensure_flow(&flows, &caption_flow_id, "caption")?;
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
        let durable_block_id = projection_block_id(document, block_ix, "equation");
        let block = ensure_block(&blocks, durable_block_id.clone(), "equation", BODY_FLOW_ID, &body_text, pos)?;
        let source_flow_id = nested_flow_id("equation_source", &durable_block_id);
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
        let durable_block_id = projection_block_id(document, block_ix, "table");
        let block = ensure_block(&blocks, durable_block_id.clone(), "table", BODY_FLOW_ID, &body_text, pos)?;
        import_table(&flows, &block, table, &durable_block_id)?;
      }
    }
  }
  import_sections(document, &sections, &body_text)?;
  doc.commit();
  Ok(())
}

fn import_sections(document: &DocumentProjection, sections: &LoroMap, body_text: &LoroText) -> LoroResult<()> {
  for section in document.sections.iter() {
    let section_id = section.id.0.to_string();
    let section_map = sections.ensure_mergeable_map(&section_id)?;
    section_map.insert("id", section_id.as_str())?;
    section_map.insert("container_id", section_map.id().to_string())?;
    section_map.insert("start_paragraph_id", section.start_paragraph.0.to_string())?;
    if let Some(parent_id) = section.parent_id {
      section_map.insert("parent_section_id", parent_id.0.to_string())?;
    }
    if let Some(heading_id) = section.heading_paragraph {
      section_map.insert("heading_paragraph_id", heading_id.0.to_string())?;
    }
    if let Some(end_id) = section.end_paragraph_exclusive {
      section_map.insert("end_paragraph_exclusive_id", end_id.0.to_string())?;
    }
    let gpui_flowtext::SectionKind::Custom(kind_slot) = section.kind;
    section_map.insert("kind_slot", i64::from(kind_slot))?;
    if let Some(paragraph_ix) = document.ids.paragraph_ids.iter().position(|id| *id == section.start_paragraph)
      && let Some(cursor) = body_text.get_cursor(paragraph_boundary_unicode_pos(document, paragraph_ix), Side::Left)
    {
      section_map.insert("start_cursor", cursor.encode())?;
    }
    let attrs = section_map.ensure_mergeable_map("attrs")?;
    section_map.insert("attrs_container_id", attrs.id().to_string())?;
    attrs.insert("source", "paragraph_style_outline")?;
  }
  Ok(())
}

fn paragraph_boundary_unicode_pos(document: &DocumentProjection, paragraph_ix: usize) -> usize {
  let mut pos = 0_usize;
  for ix in 0..paragraph_ix.min(document.paragraphs.len()) {
    pos += paragraph_text(document, ix).chars().count() + 1;
  }
  pos
}

fn append_paragraph(
  body_text: &LoroText,
  paragraphs: &LoroMap,
  blocks: &LoroMap,
  flow_id: &str,
  paragraph: &Paragraph,
  text: &str,
  block_id: String,
  paragraph_id: String,
) -> LoroResult<()> {
  append_paragraph_text_only(body_text, paragraph, text)?;
  let boundary_pos = body_text.len_unicode() - text.chars().count() - 1;
  let paragraph_map = paragraphs.ensure_mergeable_map(&paragraph_id)?;
  paragraph_map.insert("id", paragraph_id.as_str())?;
  paragraph_map.insert("container_id", paragraph_map.id().to_string())?;
  paragraph_map.insert("flow_id", flow_id)?;
  if let Some(cursor) = body_text.get_cursor(boundary_pos, Side::Left) {
    paragraph_map.insert("start_cursor", cursor.encode())?;
  }
  if let Some(cursor) = body_text.get_cursor(boundary_pos, Side::Right) {
    paragraph_map.insert("boundary_cursor", cursor.encode())?;
  }
  let attrs = paragraph_map.ensure_mergeable_map("attrs")?;
  paragraph_map.insert("attrs_container_id", attrs.id().to_string())?;
  ensure_block(blocks, block_id, "paragraph", flow_id, body_text, boundary_pos)?;
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
  for key in [
    MARK_RUN_SEMANTIC_STYLE,
    MARK_HIGHLIGHT_STYLE,
    MARK_DIRECT_UNDERLINE,
    MARK_STRIKETHROUGH,
  ] {
    text.unmark(range.clone(), key)?;
  }
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
  let row_order = table_map.ensure_mergeable_movable_list("row_order")?;
  let column_order = table_map.ensure_mergeable_movable_list("column_order")?;
  let rows_by_id = table_map.ensure_mergeable_map("rows_by_id")?;
  let columns_by_id = table_map.ensure_mergeable_map("columns_by_id")?;
  let cells_by_id = table_map.ensure_mergeable_map("cells_by_id")?;
  table_map.insert("container_id", table_map.id().to_string())?;
  table_map.insert("row_order_container_id", row_order.id().to_string())?;
  table_map.insert("column_order_container_id", column_order.id().to_string())?;
  table_map.insert("rows_container_id", rows_by_id.id().to_string())?;
  table_map.insert("columns_container_id", columns_by_id.id().to_string())?;
  table_map.insert("cells_container_id", cells_by_id.id().to_string())?;
  table_map.insert("header_row", table.style.header_row)?;

  clear_movable_list(&row_order)?;
  clear_movable_list(&column_order)?;
  clear_map(&rows_by_id)?;
  clear_map(&columns_by_id)?;
  clear_map(&cells_by_id)?;

  let column_count = table.column_widths.len().max(
    table
      .rows
      .iter()
      .map(|row| row.cells.iter().map(|cell| usize::from(cell.col_span.max(1))).sum())
      .max()
      .unwrap_or(0),
  );
  let mut column_ids = Vec::with_capacity(column_count);
  for column_ix in 0..column_count {
    let column_id = format!("{prefix}.column.{column_ix}");
    column_order.push(column_id.as_str())?;
    column_ids.push(column_id.clone());
    let column = columns_by_id.ensure_mergeable_map(&column_id)?;
    column.insert("id", column_id.as_str())?;
    column.insert("container_id", column.id().to_string())?;
    column.ensure_mergeable_map("attrs")?;
    match table.column_widths.get(column_ix) {
      Some(TableColumnWidth::Auto) | None => column.insert("width_kind", "auto")?,
      Some(TableColumnWidth::FixedPx(px)) => {
        column.insert("width_kind", "fixed_px")?;
        column.insert("width_px", i64::from(*px))?;
      }
      Some(TableColumnWidth::Fraction(fraction)) => {
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
    row_map.insert("container_id", row_map.id().to_string())?;
    row_map.ensure_mergeable_map("attrs")?;
    let mut column_ix = 0_usize;
    for (cell_ix, cell) in row.cells.iter().enumerate() {
      let cell_id = format!("{row_id}.cell.{cell_ix}");
      let column_id = &column_ids[column_ix];
      let cell_map = cells_by_id.ensure_mergeable_map(&cell_id)?;
      cell_map.insert("id", cell_id.as_str())?;
      cell_map.insert("container_id", cell_map.id().to_string())?;
      cell_map.insert("row_id", row_id.as_str())?;
      cell_map.insert("column_id", column_id.as_str())?;
      cell_map.insert("row_span", i64::from(cell.row_span))?;
      cell_map.insert("column_span", i64::from(cell.col_span))?;
      cell_map.ensure_mergeable_map("attrs")?;
      let flow_id = format!("{cell_id}.flow");
      cell_map.insert("flow_id", flow_id.as_str())?;
      let nested_table_ids = cell_map.ensure_mergeable_movable_list("nested_table_ids")?;
      let nested_tables_by_id = cell_map.ensure_mergeable_map("nested_tables_by_id")?;
      cell_map.insert("nested_table_order_container_id", nested_table_ids.id().to_string())?;
      cell_map.insert("nested_tables_container_id", nested_tables_by_id.id().to_string())?;
      clear_movable_list(&nested_table_ids)?;
      clear_map(&nested_tables_by_id)?;
      let flow = ensure_flow(flows, &flow_id, "table_cell")?;
      let text = flow.ensure_mergeable_text(FLOW_TEXT_KEY)?;
      cell_map.insert("flow_container_id", flow.id().to_string())?;
      cell_map.insert("text_container_id", text.id().to_string())?;
      replace_text(&text, SENTINEL_NEWLINE)?;
      for (block_ix, cell_block) in cell.blocks.iter().enumerate() {
        match cell_block {
          TableCellBlock::Paragraph(paragraph) => append_paragraph_text_only(&text, &paragraph.paragraph, &paragraph.text)?,
          TableCellBlock::Table(nested) => {
            let pos = text.len_unicode();
            text.insert(pos, &OBJECT_REPLACEMENT.to_string())?;
            let nested_table_id = format!("{cell_id}.nested_table.{block_ix}");
            nested_table_ids.push(nested_table_id.as_str())?;
            let nested_map = nested_tables_by_id.ensure_mergeable_map(&nested_table_id)?;
            nested_map.insert("id", nested_table_id.as_str())?;
            nested_map.insert("container_id", nested_map.id().to_string())?;
            nested_map.insert("kind", "table")?;
            if let Some(cursor) = text.get_cursor(pos, Side::Left) {
              nested_map.insert("anchor_cursor", cursor.encode())?;
            }
            nested_map.ensure_mergeable_map("attrs")?;
            import_table(flows, &nested_map, nested, &format!("{cell_id}.nested.{block_ix}"))?;
          }
        }
      }
      column_ix += usize::from(cell.col_span.max(1));
    }
  }
  Ok(())
}

pub fn import_assets(doc: &LoroDoc, document: &DocumentProjection) -> LoroResult<()> {
  let root = doc.get_map(ROOT);
  let assets = root.ensure_mergeable_map(ASSETS_BY_ID)?;
  clear_map(&assets)?;
  for asset in document.assets.assets.values() {
    let asset_id = asset.asset_id_string();
    let asset_map = assets.ensure_mergeable_map(&asset_id)?;
    let hash = blake3::hash(&asset.bytes);
    asset_map.insert("asset_id", asset_id.as_str())?;
    asset_map.insert("container_id", asset_map.id().to_string())?;
    asset_map.insert("content_hash", hash.to_hex().as_str())?;
    asset_map.insert("mime_type", asset.mime_type.as_ref())?;
    asset_map.insert("byte_length", i64::try_from(asset.bytes.len()).unwrap_or(i64::MAX))?;
    if let Some(original_name) = &asset.original_name {
      asset_map.insert("original_name", original_name.as_ref())?;
    }
  }
  Ok(())
}

pub fn assets_from_document(document: &DocumentProjection) -> Vec<AssetChunk> {
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
  let text = flow.ensure_mergeable_text(FLOW_TEXT_KEY)?;
  let attrs = flow.ensure_mergeable_map(FLOW_ATTRS_KEY)?;
  flow.insert("container_id", flow.id().to_string())?;
  flow.insert("text_container_id", text.id().to_string())?;
  flow.insert("attrs_container_id", attrs.id().to_string())?;
  Ok(flow)
}

fn ensure_block(blocks: &LoroMap, block_id: String, kind: &str, flow_id: &str, text: &LoroText, pos: usize) -> LoroResult<LoroMap> {
  let block = blocks.ensure_mergeable_map(&block_id)?;
  block.insert("id", block_id.as_str())?;
  block.insert("container_id", block.id().to_string())?;
  block.insert("kind", kind)?;
  block.insert("flow_id", flow_id)?;
  if let Some(cursor) = text.get_cursor(pos, Side::Left) {
    block.insert("anchor_cursor", cursor.encode())?;
  }
  let attrs = block.ensure_mergeable_map("attrs")?;
  let nested_refs = block.ensure_mergeable_map("nested_refs")?;
  block.insert("attrs_container_id", attrs.id().to_string())?;
  block.insert("nested_refs_container_id", nested_refs.id().to_string())?;
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

fn clear_movable_list(list: &LoroMovableList) -> LoroResult<()> {
  list.clear()
}

fn paragraph_style_value(style: ParagraphStyle) -> i64 {
  match style {
    ParagraphStyle::Normal => 0,
    ParagraphStyle::Custom(slot) => i64::from(slot) + 1,
  }
}

fn projection_block_id(document: &DocumentProjection, block_ix: usize, kind: &str) -> String {
  document
    .ids
    .block_ids
    .get(block_ix)
    .map_or_else(|| fallback_id(kind, block_ix), |id| format!("{kind}.{}", id.0))
}

fn projection_paragraph_id(document: &DocumentProjection, paragraph_ix: usize) -> String {
  document
    .ids
    .paragraph_ids
    .get(paragraph_ix)
    .map_or_else(|| fallback_id("paragraph", paragraph_ix), |id| format!("paragraph.{}", id.0))
}

fn fallback_id(kind: &str, ix: usize) -> String {
  format!("{kind}.{ix}.{}", Uuid::new_v5(&Uuid::NAMESPACE_OID, format!("{kind}.{ix}").as_bytes()).as_u128())
}

fn nested_flow_id(kind: &str, block_id: &str) -> String {
  format!("{block_id}.{kind}")
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

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn projection_identity_round_trips_into_loro() -> io::Result<()> {
    let mut source = gpui_flowtext::document_from_input_blocks(
      crate::flowstate_document_theme(),
      vec![gpui_flowtext::InputBlock::Paragraph(gpui_flowtext::InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![gpui_flowtext::InputRun {
          text: "identity".to_string(),
          styles: RunStyles::default(),
        }],
      })],
    );
    source.ids.document_id = 0x0123;
    source.ids.paragraph_ids[0] = gpui_flowtext::ParagraphId(0x0456);
    source.ids.block_ids[0] = gpui_flowtext::BlockId(0x0789);

    let doc = document_to_loro(&source, "Identity")?;
    let projected = crate::document_from_loro(&doc)?;

    assert_eq!(projected.ids.document_id, source.ids.document_id);
    assert_eq!(projected.ids.paragraph_ids, source.ids.paragraph_ids);
    assert_eq!(projected.ids.block_ids, source.ids.block_ids);
    Ok(())
  }

  #[test]
  fn custom_paragraph_style_slot_zero_round_trips() -> io::Result<()> {
    let source = gpui_flowtext::document_from_input_blocks(
      crate::flowstate_document_theme(),
      vec![gpui_flowtext::InputBlock::Paragraph(gpui_flowtext::InputParagraph {
        style: ParagraphStyle::Custom(0),
        runs: vec![gpui_flowtext::InputRun {
          text: "pocket".to_string(),
          styles: RunStyles::default(),
        }],
      })],
    );

    let doc = document_to_loro(&source, "Pocket")?;
    let projected = crate::document_from_loro(&doc)?;
    assert_eq!(projected.paragraphs[0].style, ParagraphStyle::Custom(0));
    Ok(())
  }
}
