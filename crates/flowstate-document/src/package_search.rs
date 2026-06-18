use std::io;

use gpui_flowtext::{DocumentTheme, InputBlock, InputParagraph, InputTableBlock, InputTableCellBlock, ParagraphStyle, RunSemanticStyle};
use loro::{LoroDoc, LoroText, cursor::Side};

use crate::{OBJECT_REPLACEMENT, flowstate_document_theme, package::SearchUnitChunk};

pub(crate) fn search_units_from_loro(doc: &LoroDoc, document_id: u128, frontier: &[u8]) -> io::Result<Vec<SearchUnitChunk>> {
  let body = crate::loro_schema::body_text(doc);
  let input_blocks = crate::loro_projection::input_blocks_from_loro(doc)?;
  let mut builder = SearchUnitBuilder {
    document_id,
    frontier,
    units: Vec::new(),
    next_unit_ix: 0,
    heading_path: Vec::new(),
    body_paragraph_ranges: body_paragraph_cursor_ranges(&body),
    body_paragraph_ix: 0,
    theme: flowstate_document_theme(),
  };
  for block in &input_blocks {
    builder.push_block(block, &body);
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
  body_paragraph_ranges: Vec<BodyParagraphRange>,
  body_paragraph_ix: usize,
  theme: DocumentTheme,
}

impl SearchUnitBuilder<'_> {
  fn push_block(&mut self, block: &InputBlock, body: &LoroText) {
    match block {
      InputBlock::Paragraph(paragraph) => self.push_body_paragraph(paragraph, body),
      InputBlock::Image(image) => {
        self.push_text_unit("image_alt", &image.alt_text, None);
        if let Some(caption) = &image.caption {
          self.push_text_unit("image_caption", &input_paragraph_text(caption), None);
        }
      }
      InputBlock::Equation(equation) => {
        self.push_text_unit("equation", &equation.source, None);
      }
      InputBlock::Table(table) => {
        self.push_table(table);
      }
    }
  }

  fn push_body_paragraph(&mut self, paragraph: &InputParagraph, body: &LoroText) {
    let text = input_paragraph_text(paragraph);
    let normalized = normalized_search_text(&text);
    if !normalized.is_empty()
      && let Some(level) = heading_level(&self.theme, paragraph.style)
    {
      self.update_heading_path(level, normalized.clone());
    }
    let cursor_range = self.body_paragraph_ranges.get(self.body_paragraph_ix).copied();
    self.body_paragraph_ix += 1;
    self.push_text_unit(paragraph_unit_kind(paragraph), &text, cursor_range.map(|range| cursor_fields(body, range)));
  }

  fn push_table(&mut self, table: &InputTableBlock) {
    for row in &table.rows {
      for cell in &row.cells {
        for block in &cell.blocks {
          match block {
            InputTableCellBlock::Paragraph(paragraph) => self.push_text_unit("table_cell", &input_paragraph_text(paragraph), None),
            InputTableCellBlock::Table(table) => self.push_table(table),
          }
        }
      }
    }
  }

  fn push_text_unit(&mut self, unit_kind: &str, text: &str, cursors: Option<(Vec<u8>, Vec<u8>)>) {
    let body = normalized_search_text(text);
    if body.is_empty() {
      return;
    }
    let (paragraph_start_cursor, paragraph_end_cursor) = cursors.unwrap_or_default();
    let heading = self.heading_path.last().cloned().unwrap_or_default();
    let unit_id = stable_search_unit_id(self.document_id, self.next_unit_ix, self.frontier, unit_kind, &body);
    self.next_unit_ix += 1;
    self.units.push(SearchUnitChunk {
      frontier: self.frontier.to_vec(),
      unit_id,
      unit_kind: unit_kind.to_string(),
      heading_path: self.heading_path.clone(),
      heading,
      body: body.clone(),
      insert_text: body,
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

fn body_paragraph_cursor_ranges(text: &LoroText) -> Vec<BodyParagraphRange> {
  let mut ranges = Vec::new();
  let mut rendered_blocks = 0_usize;
  let mut start = 0_usize;
  let mut has_text = false;
  let mut seen_sentinel = false;
  let mut unicode_pos = 0_usize;

  for item in text.to_delta() {
    let loro::TextDelta::Insert { insert, .. } = item else {
      continue;
    };
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
        }
        OBJECT_REPLACEMENT => {
          if has_text {
            ranges.push(BodyParagraphRange { start, end: unicode_pos });
            rendered_blocks += 1;
            has_text = false;
          }
          rendered_blocks += 1;
          start = unicode_pos + 1;
        }
        _ => has_text = true,
      }
      unicode_pos += 1;
    }
  }

  if has_text || rendered_blocks == 0 && seen_sentinel {
    ranges.push(BodyParagraphRange {
      start,
      end: text.len_unicode(),
    });
  }
  ranges
}

fn cursor_fields(body: &LoroText, range: BodyParagraphRange) -> (Vec<u8>, Vec<u8>) {
  let start_cursor = body
    .get_cursor(range.start, Side::Left)
    .map(|cursor| cursor.encode())
    .unwrap_or_default();
  let end_cursor = body
    .get_cursor(range.end, Side::Right)
    .map(|cursor| cursor.encode())
    .unwrap_or_default();
  (start_cursor, end_cursor)
}

fn input_paragraph_text(paragraph: &InputParagraph) -> String {
  paragraph.runs.iter().map(|run| run.text.as_str()).collect()
}

fn normalized_search_text(text: &str) -> String {
  text.chars().filter(|ch| *ch != OBJECT_REPLACEMENT).collect::<String>().trim().to_string()
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
    body.insert(body.len_unicode(), &OBJECT_REPLACEMENT.to_string()).map_err(loro_test_error)?;
    body.insert(body.len_unicode(), "after").map_err(loro_test_error)?;

    let ranges = body_paragraph_cursor_ranges(&body);
    assert_eq!(ranges.len(), 2);
    assert_eq!(ranges[0].start..ranges[0].end, 1..7);
    assert_eq!(ranges[1].start..ranges[1].end, 8..13);
    Ok(())
  }

  fn loro_test_error(error: impl std::error::Error + Send + Sync + 'static) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
  }
}
