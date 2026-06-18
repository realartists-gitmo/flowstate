use std::{collections::BTreeMap, io};

use gpui_flowtext::{
  AssetId, Document, DocumentTheme, HighlightStyle, InputBlock, InputBlockAlignment, InputEquationBlock, InputEquationDisplay,
  InputEquationSyntax, InputImageBlock, InputImageSizing, InputParagraph, InputRun, InputTableBlock, InputTableCell, InputTableCellBlock,
  InputTableColumnWidth, InputTableRow, InputTableStyle, RunSemanticStyle, RunStyles, document_from_input_blocks,
};
use loro::{Container, ContainerTrait, LoroDoc, LoroMap, LoroText, LoroValue, ValueOrContainer, cursor::Cursor};
use rustc_hash::FxHashMap;

use crate::{
  BLOCKS_BY_ID, FLOW_TEXT_KEY, FLOWS_BY_ID, MARK_DIRECT_UNDERLINE, MARK_HIGHLIGHT_STYLE, MARK_PARAGRAPH_STYLE, MARK_RUN_SEMANTIC_STYLE,
  MARK_STRIKETHROUGH, OBJECT_REPLACEMENT, ROOT, ROOT_BODY_FLOW_ID, flowstate_document_theme,
};

pub fn document_from_loro(doc: &LoroDoc) -> io::Result<Document> {
  let blocks = input_blocks_from_loro(doc)?;
  Ok(document_from_input_blocks(DocumentTheme::clone(&flowstate_document_theme()), blocks))
}

pub(crate) fn input_blocks_from_loro(doc: &LoroDoc) -> io::Result<Vec<InputBlock>> {
  let projector = Projector::new(doc)?;
  projector.body_blocks()
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

  fn body_blocks(&self) -> io::Result<Vec<InputBlock>> {
    let body = self.flow_text(ROOT_BODY_FLOW_ID)?;
    let body_blocks = self.object_blocks_for_flow(ROOT_BODY_FLOW_ID)?;
    let mut blocks = Vec::new();
    self.push_flow_blocks(&body, &body_blocks, &mut blocks)?;
    if blocks.is_empty() {
      blocks.push(InputBlock::Paragraph(InputParagraph {
        style: gpui_flowtext::ParagraphStyle::Normal,
        runs: Vec::new(),
      }));
    }
    Ok(blocks)
  }

  fn push_flow_blocks(&self, text: &LoroText, object_blocks: &BTreeMap<usize, LoroMap>, output: &mut Vec<InputBlock>) -> io::Result<()> {
    let mut current = InputParagraph {
      style: gpui_flowtext::ParagraphStyle::Normal,
      runs: Vec::new(),
    };
    let mut pending_style = gpui_flowtext::ParagraphStyle::Normal;
    let mut seen_sentinel = false;
    let mut unicode_pos = 0_usize;

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
            } else {
              output.push(InputBlock::Paragraph(current));
              current = InputParagraph { style, runs: Vec::new() };
              pending_style = style;
            }
          }
          OBJECT_REPLACEMENT => {
            if let Some(block) = object_blocks.get(&unicode_pos) {
              if !current.runs.is_empty() {
                output.push(InputBlock::Paragraph(current));
                current = InputParagraph {
                  style: pending_style,
                  runs: Vec::new(),
                };
              }
              output.push(self.object_block(block)?);
            }
          }
          _ => push_char(&mut current, ch, run_styles),
        }
        unicode_pos += 1;
      }
    }

    if !current.runs.is_empty() || output.is_empty() && seen_sentinel {
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
    let columns = child_map(table, "columns_by_id")?.ok_or_else(|| invalid("table has no column map"))?;
    let rows_by_id = child_map(table, "rows_by_id")?.ok_or_else(|| invalid("table has no row map"))?;
    let cells_by_id = child_map(table, "cells_by_id")?.ok_or_else(|| invalid("table has no cell map"))?;
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
    self.push_flow_blocks(&flow, &object_blocks, &mut projected)?;
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
      row_span: map_i64_opt(cell, "row_span")?.and_then(i64_to_u16).unwrap_or(1),
      col_span: map_i64_opt(cell, "column_span")?.and_then(i64_to_u16).unwrap_or(1),
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
    child_text(&flow, FLOW_TEXT_KEY)?.ok_or_else(|| invalid(format!("flow `{flow_id}` has no text")))
  }

  fn plain_flow_text(&self, flow_id: &str) -> io::Result<String> {
    Ok(self.flow_text(flow_id)?.to_string().trim_start_matches('\n').to_string())
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
    LoroValue::I64(slot) => u8::try_from(*slot).ok().map(gpui_flowtext::ParagraphStyle::Custom),
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
      width_px: map_i64_opt(attrs, "width_px")?.and_then(i64_to_u32).unwrap_or(640),
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
    Some("fixed_px") => InputTableColumnWidth::FixedPx(map_i64_opt(column, "width_px")?.and_then(i64_to_u32).unwrap_or(120)),
    Some("fraction") => InputTableColumnWidth::Fraction(map_i64_opt(column, "fraction")?.and_then(i64_to_u32).unwrap_or(1)),
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

fn invalid(message: impl Into<String>) -> io::Error {
  io::Error::new(io::ErrorKind::InvalidData, message.into())
}
