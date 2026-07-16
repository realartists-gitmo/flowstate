use std::io;

use gpui_flowtext::{DocumentTheme, InputBlock, InputParagraph, ParagraphStyle, RunSemanticStyle};
use loro::{Container, LoroDoc, LoroMap, LoroText, LoroValue, ValueOrContainer, cursor::Side};

use crate::{
  BLOCKS_BY_ID, FLOW_TEXT_KEY, FLOWS_BY_ID, OBJECT_REPLACEMENT, ROOT, ROOT_BODY_FLOW_ID, flowstate_document_theme, package::SearchUnitChunk,
};

/// T-S5: derive search units for ANY Loro document (the tub runs this over
/// imported .docx docs so their units carry the same semantics as .db8's).
pub fn search_units_for_doc(doc: &LoroDoc) -> io::Result<Vec<SearchUnitChunk>> {
  let frontier = doc.state_frontiers().encode();
  let document_id = crate::loro_schema::document_id(doc).map_or(0, |id| id.as_u128());
  search_units_from_loro(doc, document_id, &frontier)
}

pub(crate) fn search_units_from_loro(doc: &LoroDoc, document_id: u128, frontier: &[u8]) -> io::Result<Vec<SearchUnitChunk>> {
  let input_blocks = crate::loro_projection::input_blocks_from_loro(doc)?;
  search_units_from_input_blocks(doc, &input_blocks, document_id, frontier)
}

/// §act-five P9: build search units from PRE-MATERIALIZED input blocks, so a
/// checkpoint that already materialized the projection (for the projection cache)
/// does not re-run the full `document_from_loro` a SECOND time just for search —
/// the two independent materializations pegged the off-gate package thread.
pub(crate) fn search_units_from_input_blocks(
  doc: &LoroDoc,
  input_blocks: &[InputBlock],
  document_id: u128,
  frontier: &[u8],
) -> io::Result<Vec<SearchUnitChunk>> {
  let probe0 = std::time::Instant::now();
  let body = crate::loro_schema::body_text(doc);
  let body_paragraph_ranges = body_paragraph_cursor_ranges(&body);
  let probe_ranges = probe0.elapsed();
  // §A13.4.2 (vendor patch #26): resolve EVERY paragraph range's cursors in
  // one batched state acquisition — per-call `get_cursor` overhead dominated
  // the search reindex (~21k calls on a 5k-paragraph doc).
  let cursor_queries: Vec<(usize, loro::cursor::Side)> = body_paragraph_ranges
    .iter()
    .flat_map(|range| [(range.start, loro::cursor::Side::Left), (range.end, loro::cursor::Side::Right)])
    .collect();
  let probe = std::env::var_os("FLOWSTATE_OPEN_PROBE").is_some();
  let probe_t = std::time::Instant::now();
  let resolved = {
    use loro::ContainerTrait as _;
    body.to_handler().get_cursors_batch(&cursor_queries)
  };
  let probe_cursors = probe_t.elapsed();
  let body_paragraph_cursors: Vec<(Vec<u8>, Vec<u8>)> = resolved
    .chunks(2)
    .map(|pair| {
      let start = pair[0]
        .as_ref()
        .map(loro::cursor::Cursor::encode)
        .unwrap_or_default();
      let end = pair
        .get(1)
        .and_then(|c| c.as_ref())
        .map(loro::cursor::Cursor::encode)
        .unwrap_or_default();
      (start, end)
    })
    .collect();
  let mut builder = SearchUnitBuilder {
    document_id,
    frontier,
    units: Vec::new(),
    next_unit_ix: 0,
    heading_path: Vec::new(),
    body_paragraph_cursors,
    body_paragraph_ix: 0,
    theme: flowstate_document_theme(),
  };
  let probe_t = std::time::Instant::now();
  for block in input_blocks {
    builder.push_block(block, &body);
  }
  let probe_blocks = probe_t.elapsed();
  let probe_t = std::time::Instant::now();
  builder.push_loro_object_units(doc)?;
  if probe {
    eprintln!(
      "[flowstate-search-probe] ranges={probe_ranges:?} batch={probe_cursors:?} blocks={probe_blocks:?} objects={:?} units={}",
      probe_t.elapsed(),
      builder.units.len()
    );
  }
  Ok(builder.units)
}

#[derive(Clone, Copy, Debug)]
struct BodyParagraphRange {
  start: usize,
  end: usize,
}

struct SearchUnitBuilder<'a> {
  document_id: u128,
  frontier: &'a [u8],
  units: Vec<SearchUnitChunk>,
  next_unit_ix: usize,
  heading_path: Vec<String>,
  /// §A13.4.2: pre-resolved (start, end) cursor encodings per body
  /// paragraph, from ONE batched resolution.
  body_paragraph_cursors: Vec<(Vec<u8>, Vec<u8>)>,
  body_paragraph_ix: usize,
  theme: DocumentTheme,
}

impl SearchUnitBuilder<'_> {
  fn push_block(&mut self, block: &InputBlock, body: &LoroText) {
    match block {
      InputBlock::Paragraph(paragraph) => self.push_body_paragraph(paragraph, body),
      InputBlock::Image(_) | InputBlock::Equation(_) | InputBlock::Table(_) => {},
    }
  }

  fn push_body_paragraph(&mut self, paragraph: &InputParagraph, _body: &LoroText) {
    let text = input_paragraph_text(paragraph);
    let normalized = normalized_search_text(&text);
    if !normalized.is_empty()
      && let Some(level) = heading_level(&self.theme, paragraph.style)
    {
      self.update_heading_path(level, normalized.clone());
    }
    let cursors = self
      .body_paragraph_cursors
      .get(self.body_paragraph_ix)
      .cloned();
    self.body_paragraph_ix += 1;
    self.push_text_unit(
      paragraph_unit_kind(paragraph),
      &text,
      SearchUnitRefs {
        flow_id: Some(ROOT_BODY_FLOW_ID.to_string()),
        cursors: cursors.clone(),
        paragraph_cursors: cursors,
        ..SearchUnitRefs::default()
      },
    );
  }

  fn push_loro_object_units(&mut self, doc: &LoroDoc) -> io::Result<()> {
    let root = doc.get_map(ROOT);
    let Some(blocks) = child_map(&root, BLOCKS_BY_ID) else {
      return Ok(());
    };
    let Some(flows) = child_map(&root, FLOWS_BY_ID) else {
      return Ok(());
    };
    for block_id in map_keys(&blocks) {
      let Some(block) = child_map(&blocks, &block_id) else {
        continue;
      };
      match map_string_opt(&block, "kind").as_deref() {
        Some("image") => self.push_image_units(&flows, &block_id, &block)?,
        Some("equation") => self.push_equation_units(&flows, &block_id, &block)?,
        Some("table") => self.push_table_units(&flows, &block_id, None, &block)?,
        _ => {},
      }
    }
    Ok(())
  }

  fn push_image_units(&mut self, flows: &LoroMap, block_id: &str, block: &LoroMap) -> io::Result<()> {
    if let Some(flow_id) = map_string_opt(block, "alt_text_flow_id") {
      self.push_flow_text_unit(flows, "image_alt", &flow_id, SearchUnitRefs::for_block(block_id))?;
    }
    if let Some(flow_id) = map_string_opt(block, "caption_flow_id") {
      self.push_flow_text_unit(flows, "image_caption", &flow_id, SearchUnitRefs::for_block(block_id))?;
    }
    Ok(())
  }

  fn push_equation_units(&mut self, flows: &LoroMap, block_id: &str, block: &LoroMap) -> io::Result<()> {
    if let Some(flow_id) = map_string_opt(block, "source_flow_id") {
      self.push_flow_text_unit(flows, "equation", &flow_id, SearchUnitRefs::for_block(block_id))?;
    }
    Ok(())
  }

  fn push_table_units(&mut self, flows: &LoroMap, block_id: &str, _parent_cell_id: Option<&str>, owner: &LoroMap) -> io::Result<()> {
    let Some(table) = child_map(owner, "table") else {
      return Ok(());
    };
    let table_id = map_string_opt(owner, "id").unwrap_or_else(|| block_id.to_string());
    let Some(cells) = child_map(&table, "cells_by_id") else {
      return Ok(());
    };
    for cell_id in map_keys(&cells) {
      let Some(cell) = child_map(&cells, &cell_id) else {
        continue;
      };
      if let Some(flow_id) = map_string_opt(&cell, "flow_id") {
        self.push_flow_text_unit(
          flows,
          "table_cell",
          &flow_id,
          SearchUnitRefs {
            block_id: Some(block_id.to_string()),
            table_id: Some(table_id.clone()),
            cell_id: Some(cell_id.clone()),
            ..SearchUnitRefs::default()
          },
        )?;
      }
      if let Some(nested_tables) = child_map(&cell, "nested_tables_by_id") {
        for nested_id in ordered_ids(&cell, "nested_table_ids") {
          if let Some(nested) = child_map(&nested_tables, &nested_id) {
            self.push_table_units(flows, block_id, Some(&cell_id), &nested)?;
          }
        }
      }
    }
    Ok(())
  }

  fn push_flow_text_unit(&mut self, flows: &LoroMap, unit_kind: &str, flow_id: &str, mut refs: SearchUnitRefs) -> io::Result<()> {
    let Some(flow) = child_map(flows, flow_id) else {
      return Ok(());
    };
    let Some(text) = child_text(&flow, FLOW_TEXT_KEY) else {
      return Ok(());
    };
    let body = searchable_flow_text(&text);
    refs.flow_id = Some(flow_id.to_string());
    refs.cursors = text_cursor_fields(&text);
    self.push_text_unit(unit_kind, &body, refs);
    Ok(())
  }

  fn push_text_unit(&mut self, unit_kind: &str, text: &str, refs: SearchUnitRefs) {
    let body = normalized_search_text(text);
    if body.is_empty() {
      return;
    }
    let (unit_start_cursor, unit_end_cursor) = refs.cursors.unwrap_or_default();
    let (paragraph_start_cursor, paragraph_end_cursor) = refs.paragraph_cursors.unwrap_or_default();
    let heading = self.heading_path.last().cloned().unwrap_or_default();
    let unit_id = stable_search_unit_id(self.document_id, self.next_unit_ix, self.frontier, unit_kind, &body);
    self.next_unit_ix += 1;
    self.units.push(SearchUnitChunk {
      frontier: self.frontier.to_vec(),
      unit_id,
      unit_kind: unit_kind.to_string(),
      flow_id: refs.flow_id,
      block_id: refs.block_id,
      table_id: refs.table_id,
      cell_id: refs.cell_id,
      heading_path: self.heading_path.clone(),
      heading,
      body: body.clone(),
      insert_text: body,
      unit_start_cursor,
      unit_end_cursor,
      paragraph_start_cursor,
      paragraph_end_cursor,
    });
  }

  fn update_heading_path(&mut self, level: usize, heading: String) {
    if self.heading_path.len() <= level {
      self.heading_path.resize(level + 1, String::new());
    } else {
      self.heading_path.truncate(level + 1);
    }
    self.heading_path[level] = heading;
  }
}

#[derive(Default)]
struct SearchUnitRefs {
  flow_id: Option<String>,
  block_id: Option<String>,
  table_id: Option<String>,
  cell_id: Option<String>,
  cursors: Option<(Vec<u8>, Vec<u8>)>,
  paragraph_cursors: Option<(Vec<u8>, Vec<u8>)>,
}

impl SearchUnitRefs {
  fn for_block(block_id: &str) -> Self {
    Self {
      block_id: Some(block_id.to_string()),
      ..Self::default()
    }
  }
}

fn body_paragraph_cursor_ranges(text: &LoroText) -> Vec<BodyParagraphRange> {
  let mut ranges = Vec::new();
  let mut rendered_blocks = 0_usize;
  let mut start = 0_usize;
  let mut has_text = false;
  let mut seen_sentinel = false;
  let mut unicode_pos = 0_usize;

  // §A13.4.2: raw chunk iteration — the former `streaming_to_delta` walk
  // paid rich-text mark segmentation for a pass that only reads TEXT.
  let mut walk = |insert: &str| {
    for ch in insert.chars() {
      match ch {
        '\n' => {
          if seen_sentinel {
            ranges.push(BodyParagraphRange { start, end: unicode_pos });
            rendered_blocks += 1;
          } else {
            seen_sentinel = true;
          }
          start = unicode_pos + 1;
          has_text = false;
        },
        OBJECT_REPLACEMENT => {
          if has_text {
            ranges.push(BodyParagraphRange { start, end: unicode_pos });
            rendered_blocks += 1;
            has_text = false;
          }
          rendered_blocks += 1;
          start = unicode_pos + 1;
        },
        _ => has_text = true,
      }
      unicode_pos += 1;
    }
  };
  text.iter(|chunk| {
    walk(chunk);
    true
  });

  if has_text || rendered_blocks == 0 && seen_sentinel {
    ranges.push(BodyParagraphRange {
      start,
      end: text.len_unicode(),
    });
  }
  ranges
}

fn text_cursor_fields(text: &LoroText) -> Option<(Vec<u8>, Vec<u8>)> {
  let len = text.len_unicode();
  if len == 0 {
    return None;
  }
  let start = usize::from(text.to_string().starts_with('\n') && len > 1);
  Some((
    text
      .get_cursor(start, Side::Left)
      .map(|cursor| cursor.encode())
      .unwrap_or_default(),
    text
      .get_cursor(len, Side::Right)
      .map(|cursor| cursor.encode())
      .unwrap_or_default(),
  ))
}

fn input_paragraph_text(paragraph: &InputParagraph) -> String {
  paragraph.runs.iter().map(|run| run.text.as_str()).collect()
}

fn normalized_search_text(text: &str) -> String {
  text
    .chars()
    .filter(|ch| *ch != OBJECT_REPLACEMENT)
    .collect::<String>()
    .trim()
    .to_string()
}

fn searchable_flow_text(text: &LoroText) -> String {
  let text = text.to_string();
  let searchable = text
    .strip_prefix('\n')
    .unwrap_or(&text)
    .replace(OBJECT_REPLACEMENT, " ");
  let Some(without_first_newline) = searchable.strip_prefix('\n') else {
    return searchable;
  };
  let content = without_first_newline.trim_start_matches('\n');
  let mut normalized = String::with_capacity(content.len() + 1);
  normalized.push('\n');
  normalized.push_str(content);
  normalized
}

fn child_map(parent: &LoroMap, key: &str) -> Option<LoroMap> {
  parent.get(key).and_then(|value| match value {
    ValueOrContainer::Container(container) => container.into_map().ok(),
    ValueOrContainer::Value(_) => None,
  })
}

fn child_text(parent: &LoroMap, key: &str) -> Option<LoroText> {
  parent.get(key).and_then(|value| match value {
    ValueOrContainer::Container(container) => container.into_text().ok(),
    ValueOrContainer::Value(_) => None,
  })
}

fn map_keys(map: &LoroMap) -> Vec<String> {
  let mut keys = map.keys().map(|key| key.to_string()).collect::<Vec<_>>();
  keys.sort();
  keys
}

fn map_string_opt(map: &LoroMap, key: &str) -> Option<String> {
  map.get(key).and_then(|value| match value {
    ValueOrContainer::Value(LoroValue::String(value)) => Some(value.to_string()),
    _ => None,
  })
}

fn ordered_ids(map: &LoroMap, key: &str) -> Vec<String> {
  let Some(ValueOrContainer::Container(Container::MovableList(list))) = map.get(key) else {
    return Vec::new();
  };
  (0..list.len())
    .filter_map(|ix| match list.get(ix) {
      Some(ValueOrContainer::Value(LoroValue::String(value))) => Some(value.to_string()),
      _ => None,
    })
    .collect()
}

fn paragraph_unit_kind(paragraph: &InputParagraph) -> &'static str {
  if paragraph
    .runs
    .iter()
    .any(|run| matches!(run.styles.semantic, RunSemanticStyle::Custom(1)))
  {
    return "cite";
  }
  match paragraph.style {
    ParagraphStyle::Custom(0) => "pocket",
    ParagraphStyle::Custom(1) => "hat",
    ParagraphStyle::Custom(2) => "block",
    ParagraphStyle::Custom(3) => "tag",
    ParagraphStyle::Custom(4) => "analytic",
    ParagraphStyle::Custom(6) => "undertag",
    ParagraphStyle::Normal | ParagraphStyle::Custom(_) => "paragraph",
  }
}

fn heading_level(theme: &DocumentTheme, style: ParagraphStyle) -> Option<usize> {
  let ParagraphStyle::Custom(slot) = style else {
    return None;
  };
  theme
    .custom_paragraph_styles
    .get(&(slot & 0x7f))
    .and_then(|style| style.section_level)
    .map(usize::from)
}

fn stable_search_unit_id(document_id: u128, unit_ix: usize, frontier: &[u8], unit_kind: &str, body: &str) -> u128 {
  let mut hasher = blake3::Hasher::new();
  hasher.update(&document_id.to_le_bytes());
  hasher.update(&(unit_ix as u64).to_le_bytes());
  hasher.update(frontier);
  hasher.update(unit_kind.as_bytes());
  hasher.update(body.as_bytes());
  let digest = hasher.finalize();
  let mut bytes = [0_u8; 16];
  bytes.copy_from_slice(&digest.as_bytes()[..16]);
  u128::from_le_bytes(bytes)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::loro_schema::new_loro_document;

  #[test]
  fn body_paragraph_ranges_match_projection_object_boundaries() -> io::Result<()> {
    let doc = new_loro_document("ranges").map_err(loro_test_error)?;
    let body = crate::loro_schema::body_text(&doc);
    body.insert(1, "before").map_err(loro_test_error)?;
    body
      .insert(body.len_unicode(), &OBJECT_REPLACEMENT.to_string())
      .map_err(loro_test_error)?;
    body
      .insert(body.len_unicode(), "after")
      .map_err(loro_test_error)?;

    let ranges = body_paragraph_cursor_ranges(&body);
    assert_eq!(ranges.len(), 2);
    assert_eq!(ranges[0].start..ranges[0].end, 1..7);
    assert_eq!(ranges[1].start..ranges[1].end, 8..13);
    Ok(())
  }

  #[test]
  fn searchable_flow_text_preserves_one_leading_sentinel_newline() -> io::Result<()> {
    let doc = new_loro_document("text").map_err(loro_test_error)?;
    let body = crate::loro_schema::body_text(&doc);
    body.insert(1, "\n\nalpha").map_err(loro_test_error)?;

    assert_eq!(searchable_flow_text(&body), "\nalpha");
    Ok(())
  }

  fn loro_test_error(error: impl std::error::Error + Send + Sync + 'static) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
  }
}
