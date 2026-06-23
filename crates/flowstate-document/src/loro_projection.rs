use std::{collections::BTreeMap, io, sync::Arc};

use gpui_flowtext::{
  AssetId, BlockId, DocumentProjection, DocumentSection, DocumentTheme, HighlightStyle, InputBlock, InputBlockAlignment, InputEquationBlock,
  InputEquationDisplay, InputEquationSyntax, InputImageBlock, InputImageSizing, InputParagraph, InputRun, InputTableBlock, InputTableCell,
  InputTableCellBlock, InputTableColumnWidth, InputTableRow, InputTableStyle, ParagraphId, RunSemanticStyle, RunStyles, SectionId, SectionKind,
  document_from_input_blocks,
};
use loro::{Container, ContainerID, ContainerTrait, LoroDoc, LoroMap, LoroText, LoroValue, ValueOrContainer, cursor::Cursor};
use rustc_hash::FxHashMap;

use crate::{
  BLOCKS_BY_ID, FLOW_TEXT_KEY, FLOWS_BY_ID, MAIN_BODY_BLOCK_ID, MARK_DIRECT_UNDERLINE, MARK_HIGHLIGHT_STYLE, MARK_PARAGRAPH_STYLE,
  MARK_RUN_SEMANTIC_STYLE, MARK_STRIKETHROUGH, OBJECT_REPLACEMENT, PARAGRAPHS_BY_ID, ROOT, ROOT_BODY_FLOW_ID, ROOT_FIRST_PARAGRAPH_ID,
  SECTIONS_BY_ID, flowstate_document_theme,
};

pub fn document_from_loro(doc: &LoroDoc) -> io::Result<DocumentProjection> {
  let projection = projection_from_loro(doc)?;
  let mut document = document_from_projection_blocks(projection);
  document.frontier = doc.state_frontiers().encode();
  Ok(document)
}

pub(crate) fn input_blocks_from_loro(doc: &LoroDoc) -> io::Result<Vec<InputBlock>> {
  Ok(projection_from_loro(doc)?.blocks)
}

pub fn object_input_blocks_from_loro(doc: &LoroDoc) -> io::Result<Vec<(BlockId, InputBlock)>> {
  let projector = Projector::new(doc)?;
  let mut blocks = Vec::new();
  for key in projector.blocks.keys().map(|key| key.to_string()) {
    let Some(block) = child_map(&projector.blocks, &key)? else {
      continue;
    };
    if map_string_opt(&block, "kind")?.as_deref() == Some("paragraph") {
      continue;
    }
    let id = map_string_opt(&block, "id")?.unwrap_or(key);
    blocks.push((BlockId(loro_id_u128(&id)), projector.object_block(&block)?));
  }
  blocks.sort_by_key(|(id, _)| id.0);
  Ok(blocks)
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct ProjectionBlocks {
  #[serde(default)]
  pub document_id: u128,
  pub blocks: Vec<InputBlock>,
  pub paragraph_ids: Vec<ParagraphId>,
  pub block_ids: Vec<BlockId>,
  #[serde(default)]
  pub sections: Vec<DocumentSection>,
}

fn projection_from_loro(doc: &LoroDoc) -> io::Result<ProjectionBlocks> {
  let projector = Projector::new(doc)?;
  projector.body_projection()
}

pub(crate) fn projection_blocks_from_loro(doc: &LoroDoc) -> io::Result<ProjectionBlocks> {
  projection_from_loro(doc)
}

pub(crate) fn document_from_projection_blocks(projection: ProjectionBlocks) -> DocumentProjection {
  let mut document = document_from_input_blocks(DocumentTheme::clone(&flowstate_document_theme()), projection.blocks);
  if projection.document_id != 0 {
    document.ids.document_id = projection.document_id;
  }
  if projection.paragraph_ids.len() == document.paragraphs.len() {
    document.ids.paragraph_ids = projection.paragraph_ids;
  }
  if projection.block_ids.len() == document.blocks.len() {
    document.ids.block_ids = projection.block_ids;
  }
  if !projection.sections.is_empty() {
    document.sections = Arc::new(projection.sections);
  }
  document
}

struct Projector<'a> {
  doc: &'a LoroDoc,
  flows: LoroMap,
  blocks: LoroMap,
}

impl<'a> Projector<'a> {
  fn new(doc: &'a LoroDoc) -> io::Result<Self> {
    let root = doc.get_map(ROOT);
    let flows = child_map(&root, FLOWS_BY_ID)?.ok_or_else(|| invalid("Flowstate Loro package has no flows map"))?;
    let blocks = child_map(&root, BLOCKS_BY_ID)?.ok_or_else(|| invalid("Flowstate Loro package has no block registry"))?;
    Ok(Self { doc, flows, blocks })
  }

  fn body_projection(&self) -> io::Result<ProjectionBlocks> {
    let body = self.flow_text(ROOT_BODY_FLOW_ID)?;
    let body_blocks = self.object_blocks_for_flow(ROOT_BODY_FLOW_ID)?;
    let mut blocks = Vec::new();
    let mut paragraph_ids = Vec::new();
    let mut block_ids = Vec::new();
    self.push_flow_blocks(&body, &body_blocks, Some(&mut paragraph_ids), Some(&mut block_ids), &mut blocks)?;
    if blocks.is_empty() {
      blocks.push(InputBlock::Paragraph(InputParagraph {
        style: gpui_flowtext::ParagraphStyle::Normal,
        runs: Vec::new(),
      }));
      paragraph_ids.push(ParagraphId(loro_id_u128("paragraph.projection.empty")));
      block_ids.push(BlockId(loro_id_u128("block.projection.empty")));
    }
    let sections = self.sections_for_projection(&paragraph_ids)?;
    Ok(ProjectionBlocks {
      document_id: crate::loro_schema::document_id(self.doc).map_or(0, |id| id.as_u128()),
      blocks,
      paragraph_ids,
      block_ids,
      sections,
    })
  }

  /// Project a `DocumentSection` for each Loro section, including its §11 page
  /// structure (page size, margins, columns, orientation, page numbering,
  /// header/footer flow ids). The canonical attrs live in Loro and are read
  /// back via [`crate::loro_schema::read_section_page_attrs`] (which substitutes
  /// documented defaults for missing keys), then mapped field-for-field onto the
  /// gpui-flowtext projection mirror `gpui_flowtext::SectionPageAttrs`.
  fn sections_for_projection(&self, paragraph_ids: &[ParagraphId]) -> io::Result<Vec<DocumentSection>> {
    let root = self.doc.get_map(ROOT);
    let Some(sections_by_id) = child_map(&root, SECTIONS_BY_ID)? else {
      return Ok(Vec::new());
    };
    let paragraph_order = paragraph_ids
      .iter()
      .enumerate()
      .map(|(ix, id)| (id.0, ix))
      .collect::<BTreeMap<_, _>>();
    let mut sections = Vec::new();
    for key in map_keys(&sections_by_id) {
      let Some(section) = child_map(&sections_by_id, &key)? else {
        continue;
      };
      let Some(start_paragraph) = section_id_field(&section, "start_paragraph_id")? else {
        continue;
      };
      let section_id = map_string_opt(&section, "id")?
        .and_then(|value| parse_u128(&value))
        .unwrap_or_else(|| loro_id_u128(&key));
      let kind_slot = map_i64_opt(&section, "kind_slot")?
        .and_then(i64_to_u8)
        .unwrap_or(0);
      // §11: read the section's canonical page attrs from its `attrs` child map
      // (defaults substituted for missing keys) and project them. The section
      // map always exists here, so `page` is always `Some(..)` for determinism.
      let canonical_page = match child_map(&section, "attrs")? {
        Some(attrs) => crate::loro_schema::read_section_page_attrs(&attrs),
        None => crate::loro_schema::SectionPageAttrs::default(),
      };
      sections.push(DocumentSection {
        id: SectionId(section_id),
        parent_id: section_id_field(&section, "parent_section_id")?.map(SectionId),
        kind: SectionKind::Custom(kind_slot),
        heading_paragraph: section_id_field(&section, "heading_paragraph_id")?.map(ParagraphId),
        start_paragraph: ParagraphId(start_paragraph),
        end_paragraph_exclusive: section_id_field(&section, "end_paragraph_exclusive_id")?.map(ParagraphId),
        page: Some(project_section_page_attrs(canonical_page)),
      });
    }
    sections.sort_by_key(|section| {
      paragraph_order
        .get(&section.start_paragraph.0)
        .copied()
        .unwrap_or(usize::MAX)
    });
    Ok(sections)
  }

  fn push_flow_blocks(
    &self,
    text: &LoroText,
    object_blocks: &BTreeMap<usize, LoroMap>,
    mut paragraph_ids: Option<&mut Vec<ParagraphId>>,
    mut block_ids: Option<&mut Vec<BlockId>>,
    output: &mut Vec<InputBlock>,
  ) -> io::Result<()> {
    let mut current = InputParagraph {
      style: gpui_flowtext::ParagraphStyle::Normal,
      runs: Vec::new(),
    };
    let mut pending_style = gpui_flowtext::ParagraphStyle::Normal;
    let mut seen_sentinel = false;
    let mut unicode_pos = 0_usize;
    let mut current_boundary = None;

    for item in text.to_delta() {
      let loro::TextDelta::Insert { insert, attributes } = item else {
        continue;
      };
      let run_styles = run_styles_from_attrs(attributes.as_ref());
      for ch in insert.chars() {
        match ch {
          '\n' => {
            let style = paragraph_style_from_attrs(attributes.as_ref()).unwrap_or(pending_style);
            if !seen_sentinel {
              seen_sentinel = true;
              pending_style = style;
              current.style = style;
              current_boundary = Some(unicode_pos);
            } else if current.runs.is_empty()
              && output
                .last()
                .is_some_and(|block| !matches!(block, InputBlock::Paragraph(_)))
            {
              current.style = style;
              pending_style = style;
              current_boundary = Some(unicode_pos);
            } else {
              push_paragraph_projection_metadata(
                self.doc,
                text,
                current_boundary,
                output.len(),
                paragraph_ids.as_deref_mut(),
                block_ids.as_deref_mut(),
              );
              output.push(InputBlock::Paragraph(current));
              current = InputParagraph { style, runs: Vec::new() };
              pending_style = style;
              current_boundary = Some(unicode_pos);
            }
          },
          OBJECT_REPLACEMENT => {
            if let Some(block) = object_blocks.get(&unicode_pos) {
              if !current.runs.is_empty() {
                push_paragraph_projection_metadata(
                  self.doc,
                  text,
                  current_boundary,
                  output.len(),
                  paragraph_ids.as_deref_mut(),
                  block_ids.as_deref_mut(),
                );
                output.push(InputBlock::Paragraph(current));
                current = InputParagraph {
                  style: pending_style,
                  runs: Vec::new(),
                };
              }
              output.push(self.object_block(block)?);
              if let Some(block_ids) = block_ids.as_deref_mut() {
                block_ids.push(BlockId(loro_id_u128(&map_string(block, "id")?)));
              }
              current_boundary = None;
            }
          },
          _ => push_char(&mut current, ch, run_styles),
        }
        unicode_pos += 1;
      }
    }

    if !current.runs.is_empty() || current_boundary.is_some() || output.is_empty() && seen_sentinel {
      push_paragraph_projection_metadata(self.doc, text, current_boundary, output.len(), paragraph_ids, block_ids);
      output.push(InputBlock::Paragraph(current));
    }
    Ok(())
  }

  fn object_blocks_for_flow(&self, flow_id: &str) -> io::Result<BTreeMap<usize, LoroMap>> {
    let mut by_pos = BTreeMap::new();
    for key in self.blocks.keys() {
      let key = key.to_string();
      let Some(block) = child_map(&self.blocks, &key)? else {
        continue;
      };
      if map_string_opt(&block, "flow_id")?.as_deref() != Some(flow_id) {
        continue;
      }
      if map_string_opt(&block, "kind")?.as_deref() == Some("paragraph") {
        continue;
      }
      let Some(cursor_bytes) = map_binary_opt(&block, "anchor_cursor")? else {
        continue;
      };
      let Ok(cursor) = Cursor::decode(&cursor_bytes) else {
        continue;
      };
      if let Ok(pos) = self.doc.get_cursor_pos(&cursor) {
        by_pos.insert(pos.current.pos, block);
      }
    }
    Ok(by_pos)
  }

  fn object_block(&self, block: &LoroMap) -> io::Result<InputBlock> {
    match map_string(block, "kind")?.as_str() {
      "image" => self.image_block(block).map(InputBlock::Image),
      "equation" => self.equation_block(block).map(InputBlock::Equation),
      "table" => self.table_block(block).map(InputBlock::Table),
      kind => Err(invalid(format!("unsupported Loro block kind `{kind}`"))),
    }
  }

  fn image_block(&self, block: &LoroMap) -> io::Result<InputImageBlock> {
    let attrs = child_map(block, "attrs")?;
    Ok(InputImageBlock {
      asset_id: AssetId(parse_u128(&map_string(block, "asset_id")?).unwrap_or_default()),
      alt_text: map_string_opt(block, "alt_text_flow_id")?
        .map(|flow_id| self.plain_flow_text(&flow_id))
        .transpose()?
        .unwrap_or_default(),
      caption: map_string_opt(block, "caption_flow_id")?
        .map(|flow_id| self.caption_paragraph(&flow_id))
        .transpose()?,
      sizing: image_sizing(attrs.as_ref())?,
      alignment: alignment(attrs.as_ref())?,
    })
  }

  fn equation_block(&self, block: &LoroMap) -> io::Result<InputEquationBlock> {
    let attrs = child_map(block, "attrs")?;
    Ok(InputEquationBlock {
      source: map_string_opt(block, "source_flow_id")?
        .map(|flow_id| self.plain_flow_text(&flow_id))
        .transpose()?
        .unwrap_or_default(),
      syntax: equation_syntax(attrs.as_ref())?,
      display: equation_display(attrs.as_ref())?,
    })
  }

  fn table_block(&self, owner: &LoroMap) -> io::Result<InputTableBlock> {
    let table = child_map(owner, "table")?.ok_or_else(|| invalid("table block has no table map"))?;
    self.table_from_map(&table)
  }

  fn table_from_map(&self, table: &LoroMap) -> io::Result<InputTableBlock> {
    // §28: resolve the table's child containers through their stored raw
    // container ids, falling back to key traversal when unavailable.
    let columns = self
      .resolve_child_map(table, "columns_container_id", "columns_by_id")?
      .ok_or_else(|| invalid("table has no column map"))?;
    let rows_by_id = self
      .resolve_child_map(table, "rows_container_id", "rows_by_id")?
      .ok_or_else(|| invalid("table has no row map"))?;
    let cells_by_id = self
      .resolve_child_map(table, "cells_container_id", "cells_by_id")?
      .ok_or_else(|| invalid("table has no cell map"))?;
    let column_ids = ordered_ids(table, "column_order")?;
    let column_positions = column_ids
      .iter()
      .enumerate()
      .map(|(ix, column_id)| (column_id.clone(), ix))
      .collect::<BTreeMap<_, _>>();
    let column_widths = column_ids
      .iter()
      .map(|column_id| {
        let column = child_map(&columns, column_id)?.ok_or_else(|| invalid(format!("missing table column `{column_id}`")))?;
        table_column_width(&column)
      })
      .collect::<io::Result<Vec<_>>>()?;

    let mut rows = Vec::new();
    for row_id in ordered_ids(table, "row_order")? {
      let _row = child_map(&rows_by_id, &row_id)?.ok_or_else(|| invalid(format!("missing table row `{row_id}`")))?;
      let mut row_cells = Vec::new();
      let mut cells_by_column = BTreeMap::new();
      for cell_id in cells_by_id.keys().map(|key| key.to_string()) {
        let Some(cell) = child_map(&cells_by_id, &cell_id)? else {
          continue;
        };
        if map_string_opt(&cell, "row_id")?.as_deref() != Some(row_id.as_str()) {
          continue;
        }
        let column_id = map_string(&cell, "column_id")?;
        if let Some(column_ix) = column_positions.get(&column_id) {
          cells_by_column.insert(*column_ix, cell);
        }
      }
      for (_, cell) in cells_by_column {
        row_cells.push(self.table_cell(&cell)?);
      }
      rows.push(InputTableRow { cells: row_cells });
    }

    Ok(InputTableBlock {
      rows,
      column_widths,
      style: InputTableStyle {
        header_row: map_bool_opt(table, "header_row")?.unwrap_or(false),
      },
    })
  }

  fn table_cell(&self, cell: &LoroMap) -> io::Result<InputTableCell> {
    let flow_id = map_string(cell, "flow_id")?;
    let flow = self.flow_text(&flow_id)?;
    let object_blocks = self.cell_nested_tables(cell, &flow)?;
    let mut projected = Vec::new();
    self.push_flow_blocks(&flow, &object_blocks, None, None, &mut projected)?;
    let mut blocks = projected
      .into_iter()
      .filter_map(|block| match block {
        InputBlock::Paragraph(paragraph) => Some(Ok(InputTableCellBlock::Paragraph(paragraph))),
        InputBlock::Table(table) => Some(Ok(InputTableCellBlock::Table(table))),
        InputBlock::Image(_) | InputBlock::Equation(_) => None,
      })
      .collect::<io::Result<Vec<_>>>()?;
    if blocks.is_empty() {
      blocks.push(InputTableCellBlock::Paragraph(InputParagraph {
        style: gpui_flowtext::ParagraphStyle::Normal,
        runs: Vec::new(),
      }));
    }
    Ok(InputTableCell {
      blocks,
      row_span: map_i64_opt(cell, "row_span")?
        .and_then(i64_to_u16)
        .unwrap_or(1),
      col_span: map_i64_opt(cell, "column_span")?
        .and_then(i64_to_u16)
        .unwrap_or(1),
    })
  }

  fn cell_nested_tables(&self, cell: &LoroMap, flow: &LoroText) -> io::Result<BTreeMap<usize, LoroMap>> {
    let mut tables = BTreeMap::new();
    let Some(tables_by_id) = child_map(cell, "nested_tables_by_id")? else {
      return Ok(tables);
    };
    for nested_table_id in ordered_ids(cell, "nested_table_ids")? {
      let Some(owner) = child_map(&tables_by_id, &nested_table_id)? else {
        continue;
      };
      let Some(cursor_bytes) = map_binary_opt(&owner, "anchor_cursor")? else {
        continue;
      };
      let Ok(cursor) = Cursor::decode(&cursor_bytes) else {
        continue;
      };
      if cursor.container != flow.id() {
        continue;
      }
      if let Ok(pos) = self.doc.get_cursor_pos(&cursor) {
        tables.insert(pos.current.pos, owner);
      }
    }
    Ok(tables)
  }

  fn flow_text(&self, flow_id: &str) -> io::Result<LoroText> {
    let flow = child_map(&self.flows, flow_id)?.ok_or_else(|| invalid(format!("missing flow `{flow_id}`")))?;
    // §28: prefer direct resolution via the flow's stored raw container id, and
    // only fall back to map-key traversal when the id is missing/unresolvable.
    if let Some(container_id) = map_string_opt(&flow, "text_container_id")?
      && let Some(text) = resolve_text_by_container_id(self.doc, &container_id)
    {
      return Ok(text);
    }
    child_text(&flow, FLOW_TEXT_KEY)?.ok_or_else(|| invalid(format!("flow `{flow_id}` has no text")))
  }

  /// §28: resolve a child container map by its stored raw container id, falling
  /// back to direct map-key traversal when the id is missing/unresolvable.
  fn resolve_child_map(&self, owner: &LoroMap, container_id_key: &str, fallback_key: &str) -> io::Result<Option<LoroMap>> {
    if let Some(container_id) = map_string_opt(owner, container_id_key)?
      && let Some(map) = resolve_map_by_container_id(self.doc, &container_id)
    {
      return Ok(Some(map));
    }
    child_map(owner, fallback_key)
  }

  fn plain_flow_text(&self, flow_id: &str) -> io::Result<String> {
    Ok(
      self
        .flow_text(flow_id)?
        .to_string()
        .trim_start_matches('\n')
        .to_string(),
    )
  }

  fn caption_paragraph(&self, flow_id: &str) -> io::Result<InputParagraph> {
    let paragraphs = paragraphs_from_text(&self.flow_text(flow_id)?);
    Ok(paragraphs.into_iter().next().unwrap_or(InputParagraph {
      style: gpui_flowtext::ParagraphStyle::Normal,
      runs: Vec::new(),
    }))
  }
}

fn paragraphs_from_text(text: &LoroText) -> Vec<InputParagraph> {
  let mut blocks = Vec::new();
  let projector = ParagraphOnlyProjector;
  projector.push_flow_blocks(text, &mut blocks);
  blocks
}

struct ParagraphOnlyProjector;

impl ParagraphOnlyProjector {
  fn push_flow_blocks(&self, text: &LoroText, output: &mut Vec<InputParagraph>) {
    let mut current = InputParagraph {
      style: gpui_flowtext::ParagraphStyle::Normal,
      runs: Vec::new(),
    };
    let mut pending_style = gpui_flowtext::ParagraphStyle::Normal;
    let mut seen_sentinel = false;
    for item in text.to_delta() {
      let loro::TextDelta::Insert { insert, attributes } = item else {
        continue;
      };
      let run_styles = run_styles_from_attrs(attributes.as_ref());
      for ch in insert.chars() {
        if ch == '\n' {
          let style = paragraph_style_from_attrs(attributes.as_ref()).unwrap_or(pending_style);
          if !seen_sentinel {
            seen_sentinel = true;
            pending_style = style;
            current.style = style;
          } else {
            output.push(current);
            current = InputParagraph { style, runs: Vec::new() };
            pending_style = style;
          }
        } else if ch != OBJECT_REPLACEMENT {
          push_char(&mut current, ch, run_styles);
        }
      }
    }
    if seen_sentinel || !current.runs.is_empty() {
      output.push(current);
    }
  }
}

fn push_char(paragraph: &mut InputParagraph, ch: char, styles: RunStyles) {
  if let Some(last) = paragraph.runs.last_mut()
    && last.styles == styles
  {
    last.text.push(ch);
    return;
  }
  paragraph.runs.push(InputRun {
    text: ch.to_string(),
    styles,
  });
}

fn push_paragraph_projection_metadata(
  doc: &LoroDoc,
  text: &LoroText,
  boundary: Option<usize>,
  block_ix: usize,
  paragraph_ids: Option<&mut Vec<ParagraphId>>,
  block_ids: Option<&mut Vec<BlockId>>,
) {
  if let Some(paragraph_ids) = paragraph_ids {
    let id = boundary
      .and_then(|boundary| paragraph_loro_id_at_boundary(doc, text, boundary))
      .unwrap_or_else(|| format!("paragraph.projection.{block_ix}"));
    paragraph_ids.push(ParagraphId(loro_id_u128(&id)));
  }
  if let Some(block_ids) = block_ids {
    let id = boundary
      .and_then(|boundary| paragraph_block_loro_id_at_boundary(doc, text, boundary))
      .unwrap_or_else(|| format!("paragraph_block.projection.{block_ix}"));
    block_ids.push(BlockId(loro_id_u128(&id)));
  }
}

fn paragraph_loro_id_at_boundary(doc: &LoroDoc, text: &LoroText, boundary: usize) -> Option<String> {
  let root = doc.get_map(ROOT);
  let paragraphs = child_map(&root, PARAGRAPHS_BY_ID).ok().flatten()?;
  let mut matches = map_keys(&paragraphs)
    .into_iter()
    .filter(|key| {
      child_map(&paragraphs, key)
        .ok()
        .flatten()
        .and_then(|paragraph| {
          live_cursor_pos(doc, text, &paragraph, "boundary_cursor").or_else(|| live_cursor_pos(doc, text, &paragraph, "start_cursor"))
        })
        == Some(boundary)
    })
    .collect::<Vec<_>>();
  if boundary == 0
    && let Some(ix) = matches
      .iter()
      .position(|key| key == ROOT_FIRST_PARAGRAPH_ID)
  {
    return Some(matches.swap_remove(ix));
  }
  matches.into_iter().next()
}

fn paragraph_block_loro_id_at_boundary(doc: &LoroDoc, text: &LoroText, boundary: usize) -> Option<String> {
  let root = doc.get_map(ROOT);
  let blocks = child_map(&root, BLOCKS_BY_ID).ok().flatten()?;
  let mut matches = map_keys(&blocks)
    .into_iter()
    .filter(|key| {
      let Some(block) = child_map(&blocks, key).ok().flatten() else {
        return false;
      };
      map_string_opt(&block, "kind").ok().flatten().as_deref() == Some("paragraph")
        && live_cursor_pos(doc, text, &block, "anchor_cursor") == Some(boundary)
    })
    .collect::<Vec<_>>();
  if boundary == 0
    && let Some(ix) = matches.iter().position(|key| key == MAIN_BODY_BLOCK_ID)
  {
    return Some(matches.swap_remove(ix));
  }
  matches.into_iter().next()
}

fn live_cursor_pos(doc: &LoroDoc, text: &LoroText, map: &LoroMap, key: &str) -> Option<usize> {
  let cursor_bytes = map_binary_opt(map, key).ok().flatten()?;
  let cursor = Cursor::decode(&cursor_bytes).ok()?;
  if cursor.container != text.id() {
    return None;
  }
  let pos = doc.get_cursor_pos(&cursor).ok()?.current.pos;
  (text.to_string().chars().nth(pos).is_some()).then_some(pos)
}

fn map_keys(map: &LoroMap) -> Vec<String> {
  let mut keys = map.keys().map(|key| key.to_string()).collect::<Vec<_>>();
  keys.sort();
  keys
}

fn loro_id_u128(id: &str) -> u128 {
  if let Some(value) = id
    .rsplit('.')
    .next()
    .and_then(|suffix| suffix.parse::<u128>().ok())
  {
    return value;
  }
  let hash = blake3::hash(id.as_bytes());
  let mut bytes = [0_u8; 16];
  bytes.copy_from_slice(&hash.as_bytes()[..16]);
  u128::from_le_bytes(bytes)
}

fn child_map(parent: &LoroMap, key: &str) -> io::Result<Option<LoroMap>> {
  Ok(parent.get(key).and_then(|value| match value {
    ValueOrContainer::Container(container) => container.into_map().ok(),
    ValueOrContainer::Value(_) => None,
  }))
}

fn child_text(parent: &LoroMap, key: &str) -> io::Result<Option<LoroText>> {
  Ok(parent.get(key).and_then(|value| match value {
    ValueOrContainer::Container(container) => container.into_text().ok(),
    ValueOrContainer::Value(_) => None,
  }))
}

/// §28: centralized resolution of a stored raw Loro container id string.
///
/// Parses the durable `*_container_id` string into a [`ContainerID`] and fetches
/// the live container directly from the document for efficient runtime access.
/// Returns `None` when the id is missing/unparseable or the container is
/// absent/detached/deleted, so callers can fall back to map-key traversal.
fn resolve_container(doc: &LoroDoc, container_id: &str) -> Option<Container> {
  let container = doc.get_container(ContainerID::try_from(container_id).ok()?)?;
  (container.is_attached() && !container.is_deleted()).then_some(container)
}

fn resolve_map_by_container_id(doc: &LoroDoc, container_id: &str) -> Option<LoroMap> {
  resolve_container(doc, container_id)?.into_map().ok()
}

fn resolve_text_by_container_id(doc: &LoroDoc, container_id: &str) -> Option<LoroText> {
  resolve_container(doc, container_id)?.into_text().ok()
}

fn ordered_ids(map: &LoroMap, key: &str) -> io::Result<Vec<String>> {
  let Some(ValueOrContainer::Container(container)) = map.get(key) else {
    return Ok(Vec::new());
  };
  let value = match container {
    Container::MovableList(list) => list.get_deep_value(),
    _ => return Ok(Vec::new()),
  };
  Ok(
    value
      .into_list()
      .unwrap_or_default()
      .iter()
      .filter_map(|value| match value {
        LoroValue::String(value) => Some(value.to_string()),
        _ => None,
      })
      .collect(),
  )
}

fn map_string(map: &LoroMap, key: &str) -> io::Result<String> {
  map_string_opt(map, key)?.ok_or_else(|| invalid(format!("missing string field `{key}`")))
}

fn map_string_opt(map: &LoroMap, key: &str) -> io::Result<Option<String>> {
  Ok(map.get(key).and_then(|value| match value {
    ValueOrContainer::Value(LoroValue::String(value)) => Some(value.to_string()),
    _ => None,
  }))
}

fn map_binary_opt(map: &LoroMap, key: &str) -> io::Result<Option<Vec<u8>>> {
  Ok(map.get(key).and_then(|value| match value {
    ValueOrContainer::Value(LoroValue::Binary(value)) => Some(value.to_vec()),
    _ => None,
  }))
}

fn map_i64_opt(map: &LoroMap, key: &str) -> io::Result<Option<i64>> {
  Ok(map.get(key).and_then(|value| match value {
    ValueOrContainer::Value(LoroValue::I64(value)) => Some(value),
    _ => None,
  }))
}

fn map_bool_opt(map: &LoroMap, key: &str) -> io::Result<Option<bool>> {
  Ok(map.get(key).and_then(|value| match value {
    ValueOrContainer::Value(LoroValue::Bool(value)) => Some(value),
    _ => None,
  }))
}

fn paragraph_style_from_attrs(attrs: Option<&FxHashMap<String, LoroValue>>) -> Option<gpui_flowtext::ParagraphStyle> {
  let value = attrs?.get(MARK_PARAGRAPH_STYLE)?;
  match value {
    LoroValue::I64(0) => Some(gpui_flowtext::ParagraphStyle::Normal),
    LoroValue::I64(slot) if *slot > 0 => u8::try_from(*slot - 1)
      .ok()
      .map(gpui_flowtext::ParagraphStyle::Custom),
    _ => None,
  }
}

fn run_styles_from_attrs(attrs: Option<&FxHashMap<String, LoroValue>>) -> RunStyles {
  let mut styles = RunStyles::default();
  let Some(attrs) = attrs else {
    return styles;
  };
  if let Some(LoroValue::I64(slot)) = attrs.get(MARK_RUN_SEMANTIC_STYLE)
    && let Ok(slot) = u8::try_from(*slot)
  {
    styles.semantic = RunSemanticStyle::Custom(slot);
  }
  if let Some(LoroValue::I64(slot)) = attrs.get(MARK_HIGHLIGHT_STYLE)
    && let Ok(slot) = u8::try_from(*slot)
  {
    styles.highlight = Some(HighlightStyle::Custom(slot));
  }
  if matches!(attrs.get(MARK_DIRECT_UNDERLINE), Some(LoroValue::Bool(true))) {
    styles.direct_underline = true;
  }
  if matches!(attrs.get(MARK_STRIKETHROUGH), Some(LoroValue::Bool(true))) {
    styles.strikethrough = true;
  }
  styles
}

fn image_sizing(attrs: Option<&LoroMap>) -> io::Result<InputImageSizing> {
  let Some(attrs) = attrs else {
    return Ok(InputImageSizing::FitWidth);
  };
  match map_string_opt(attrs, "sizing")?.as_deref() {
    Some("intrinsic") => Ok(InputImageSizing::Intrinsic),
    Some("fixed") => Ok(InputImageSizing::Fixed {
      width_px: map_i64_opt(attrs, "width_px")?
        .and_then(i64_to_u32)
        .unwrap_or(640),
      height_px: map_i64_opt(attrs, "height_px")?.and_then(i64_to_u32),
    }),
    Some("fit_width") | None => Ok(InputImageSizing::FitWidth),
    Some(_) => Ok(InputImageSizing::FitWidth),
  }
}

fn alignment(attrs: Option<&LoroMap>) -> io::Result<InputBlockAlignment> {
  let Some(attrs) = attrs else {
    return Ok(InputBlockAlignment::Left);
  };
  Ok(match map_string_opt(attrs, "alignment")?.as_deref() {
    Some("center") => InputBlockAlignment::Center,
    Some("right") => InputBlockAlignment::Right,
    Some("left") | None => InputBlockAlignment::Left,
    Some(_) => InputBlockAlignment::Left,
  })
}

fn equation_syntax(attrs: Option<&LoroMap>) -> io::Result<InputEquationSyntax> {
  let Some(attrs) = attrs else {
    return Ok(InputEquationSyntax::Latex);
  };
  Ok(match map_string_opt(attrs, "syntax")?.as_deref() {
    Some("latex") | None => InputEquationSyntax::Latex,
    Some(_) => InputEquationSyntax::Latex,
  })
}

fn equation_display(attrs: Option<&LoroMap>) -> io::Result<InputEquationDisplay> {
  let Some(attrs) = attrs else {
    return Ok(InputEquationDisplay::Display);
  };
  Ok(match map_string_opt(attrs, "display")?.as_deref() {
    Some("inline_like_paragraph") => InputEquationDisplay::InlineLikeParagraph,
    Some("display") | None => InputEquationDisplay::Display,
    Some(_) => InputEquationDisplay::Display,
  })
}

fn table_column_width(column: &LoroMap) -> io::Result<InputTableColumnWidth> {
  Ok(match map_string_opt(column, "width_kind")?.as_deref() {
    Some("fixed_px") => InputTableColumnWidth::FixedPx(
      map_i64_opt(column, "width_px")?
        .and_then(i64_to_u32)
        .unwrap_or(120),
    ),
    Some("fraction") => InputTableColumnWidth::Fraction(
      map_i64_opt(column, "fraction")?
        .and_then(i64_to_u32)
        .unwrap_or(1),
    ),
    Some("auto") | None => InputTableColumnWidth::Auto,
    Some(_) => InputTableColumnWidth::Auto,
  })
}

fn parse_u128(value: &str) -> Option<u128> {
  value.parse::<u128>().ok()
}

fn i64_to_u32(value: i64) -> Option<u32> {
  u32::try_from(value).ok()
}

fn i64_to_u16(value: i64) -> Option<u16> {
  u16::try_from(value).ok()
}

fn i64_to_u8(value: i64) -> Option<u8> {
  u8::try_from(value).ok()
}

fn section_id_field(map: &LoroMap, key: &str) -> io::Result<Option<u128>> {
  Ok(map_string_opt(map, key)?.and_then(|value| parse_u128(&value)))
}

/// §11: read a section's page-structure attributes back from the canonical Loro
/// document, substituting documented defaults for any missing keys. Returns
/// `None` only when the named section does not exist.
///
/// `DocumentProjection` now carries these attrs on `DocumentSection::page`,
/// populated from canonical Loro during projection (see
/// [`Projector::sections_for_projection`]). This helper remains the direct,
/// single-section read-back path for callers that only need one section's attrs
/// without projecting the whole document. The canonical values always live in
/// Loro and round-trip losslessly.
#[must_use]
pub fn section_page_attrs(doc: &LoroDoc, section_id: &str) -> Option<crate::loro_schema::SectionPageAttrs> {
  let root = doc.get_map(ROOT);
  let sections = child_map(&root, SECTIONS_BY_ID).ok().flatten()?;
  let section = child_map(&sections, section_id).ok().flatten()?;
  let attrs = child_map(&section, "attrs").ok().flatten()?;
  Some(crate::loro_schema::read_section_page_attrs(&attrs))
}

/// §11: map canonical Loro page-structure attrs
/// (`crate::loro_schema::SectionPageAttrs`) onto the gpui-flowtext projection
/// mirror (`gpui_flowtext::SectionPageAttrs`). gpui-flowtext cannot depend on
/// `flowstate-document`, so the two types are defined field-for-field
/// identically and this is a direct copy. Fully-qualified paths disambiguate the
/// clashing type names. Takes the canonical attrs by value so the owned
/// header/footer flow id strings move rather than clone.
fn project_section_page_attrs(attrs: crate::loro_schema::SectionPageAttrs) -> gpui_flowtext::SectionPageAttrs {
  gpui_flowtext::SectionPageAttrs {
    page_size: gpui_flowtext::SectionPageSize {
      width_twips: attrs.page_size.width_twips,
      height_twips: attrs.page_size.height_twips,
    },
    margins: gpui_flowtext::SectionMargins {
      top_twips: attrs.margins.top_twips,
      right_twips: attrs.margins.right_twips,
      bottom_twips: attrs.margins.bottom_twips,
      left_twips: attrs.margins.left_twips,
    },
    columns: attrs.columns,
    orientation: match attrs.orientation {
      crate::loro_schema::SectionOrientation::Portrait => gpui_flowtext::SectionOrientation::Portrait,
      crate::loro_schema::SectionOrientation::Landscape => gpui_flowtext::SectionOrientation::Landscape,
    },
    page_numbering: gpui_flowtext::SectionPageNumbering {
      format: match attrs.page_numbering.format {
        crate::loro_schema::PageNumberFormat::None => gpui_flowtext::PageNumberFormat::None,
        crate::loro_schema::PageNumberFormat::Decimal => gpui_flowtext::PageNumberFormat::Decimal,
        crate::loro_schema::PageNumberFormat::LowerRoman => gpui_flowtext::PageNumberFormat::LowerRoman,
        crate::loro_schema::PageNumberFormat::UpperRoman => gpui_flowtext::PageNumberFormat::UpperRoman,
        crate::loro_schema::PageNumberFormat::LowerAlpha => gpui_flowtext::PageNumberFormat::LowerAlpha,
        crate::loro_schema::PageNumberFormat::UpperAlpha => gpui_flowtext::PageNumberFormat::UpperAlpha,
      },
      start: attrs.page_numbering.start,
    },
    header_flow_id: attrs.header_flow_id,
    footer_flow_id: attrs.footer_flow_id,
  }
}

fn invalid(message: impl Into<String>) -> io::Error {
  io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::{document_to_loro, flowstate_document_theme, loro_schema::body_text};
  use gpui_flowtext::{
    InputBlock, InputBlockAlignment, InputImageBlock, InputImageSizing, InputParagraph, InputRun, RunStyles, document_from_input_blocks,
  };

  #[test]
  fn projection_preserves_loro_paragraph_and_block_ids() -> io::Result<()> {
    let source = document_from_input_blocks(
      DocumentTheme::clone(&flowstate_document_theme()),
      vec![
        InputBlock::Paragraph(InputParagraph {
          style: gpui_flowtext::ParagraphStyle::Normal,
          runs: vec![InputRun {
            text: "before".to_string(),
            styles: RunStyles::default(),
          }],
        }),
        InputBlock::Image(InputImageBlock {
          asset_id: AssetId(42),
          alt_text: "alt".into(),
          caption: None,
          sizing: InputImageSizing::FitWidth,
          alignment: InputBlockAlignment::Left,
        }),
      ],
    );
    let doc = document_to_loro(&source, "Projection ids")?;
    let body = body_text(&doc);
    let root = doc.get_map(ROOT);
    let blocks = child_map(&root, BLOCKS_BY_ID)?.expect("blocks map");
    let first_paragraph_id = paragraph_loro_id_at_boundary(&doc, &body, 0).expect("first paragraph id");
    let first_block_id = paragraph_block_loro_id_at_boundary(&doc, &body, 0).expect("first paragraph block id");
    let image_id = map_keys(&blocks)
      .into_iter()
      .find(|key| {
        child_map(&blocks, key)
          .ok()
          .flatten()
          .and_then(|block| map_string_opt(&block, "kind").ok().flatten())
          .as_deref()
          == Some("image")
      })
      .expect("image block id");

    let projected = document_from_loro(&doc)?;

    assert_eq!(projected.ids.paragraph_ids[0], ParagraphId(loro_id_u128(&first_paragraph_id)));
    assert_eq!(projected.ids.block_ids[0], BlockId(loro_id_u128(&first_block_id)));
    assert_eq!(projected.ids.block_ids[1], BlockId(loro_id_u128(&image_id)));
    Ok(())
  }

  #[test]
  fn object_boundary_does_not_create_a_phantom_paragraph() -> io::Result<()> {
    let paragraph = |text: &str| {
      InputBlock::Paragraph(InputParagraph {
        style: gpui_flowtext::ParagraphStyle::Normal,
        runs: vec![InputRun {
          text: text.to_string(),
          styles: RunStyles::default(),
        }],
      })
    };
    let source = document_from_input_blocks(
      DocumentTheme::clone(&flowstate_document_theme()),
      vec![
        paragraph("before"),
        InputBlock::Image(InputImageBlock {
          asset_id: AssetId(7),
          alt_text: "figure".into(),
          caption: None,
          sizing: InputImageSizing::Intrinsic,
          alignment: InputBlockAlignment::Center,
        }),
        paragraph("after"),
      ],
    );

    let projected = document_from_loro(&document_to_loro(&source, "Object boundary")?)?;

    assert_eq!(projected.paragraphs.len(), 2);
    assert_eq!(gpui_flowtext::paragraph_text(&projected, 0), "before");
    assert_eq!(gpui_flowtext::paragraph_text(&projected, 1), "after");
    assert!(matches!(
      projected.blocks.as_slice(),
      [
        gpui_flowtext::Block::Paragraph(_),
        gpui_flowtext::Block::Image(_),
        gpui_flowtext::Block::Paragraph(_)
      ]
    ));
    Ok(())
  }

  #[test]
  fn section_page_attrs_read_back_from_loro() {
    let doc = crate::loro_schema::new_loro_document("Sections").expect("new Loro document");
    let expected = crate::loro_schema::SectionPageAttrs {
      columns: 3,
      orientation: crate::loro_schema::SectionOrientation::Landscape,
      ..crate::loro_schema::SectionPageAttrs::default()
    };
    crate::loro_schema::set_section_page_attrs(&doc, "section.alpha", &expected).expect("set section page attrs");

    assert_eq!(section_page_attrs(&doc, "section.alpha"), Some(expected));
    assert_eq!(section_page_attrs(&doc, "section.missing"), None);
  }
}
