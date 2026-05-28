use std::{
  hash::{Hash, Hasher},
  ops::Range,
};

use gpui::{
  App, Bounds, FontStyle, FontWeight, Hsla, Pixels, Point, ShapedLine, SharedString, Size, TextRun as GpuiTextRun, Window, font, point, px, size,
};
use rustc_hash::{FxHashMap, FxHasher};

use super::*;

#[derive(Clone)]
pub(super) struct LayoutState {
  pub(super) paragraphs: Vec<LaidOutParagraph>,
  pub(super) blocks: Vec<LaidOutBlock>,
  pub(super) paragraph_to_block: Vec<usize>,
  #[allow(dead_code)]
  pub(super) block_to_paragraph: Vec<Option<usize>>,
  pub(super) bounds: Option<Bounds<Pixels>>,
  pub(super) size: Size<Pixels>,
  pub(super) width: Pixels,
  pub(super) snap_underline_rules_to_pixels: bool,
}

#[hotpath::measure_all]
impl LayoutState {
  pub(super) fn block_count(&self) -> usize {
    self.blocks.len()
  }

  pub(super) fn paragraph_block_ix(&self, paragraph_ix: usize) -> Option<usize> {
    self.paragraph_to_block.get(paragraph_ix).copied()
  }

  #[allow(dead_code)]
  pub(super) fn block_paragraph_ix(&self, block_ix: usize) -> Option<usize> {
    self.block_to_paragraph.get(block_ix).copied().flatten()
  }

  pub(super) fn hit_test(&self, position: Point<Pixels>) -> DocumentOffset {
    let position = match self.bounds {
      Some(bounds) => position - bounds.origin,
      None => position,
    };
    self.hit_test_unpositioned(position)
  }

  pub(super) fn hit_test_at_bounds(&self, position: Point<Pixels>, bounds: Bounds<Pixels>) -> DocumentOffset {
    self.hit_test_unpositioned(position - bounds.origin)
  }

  fn hit_test_unpositioned(&self, position: Point<Pixels>) -> DocumentOffset {
    let paragraph_ix = first_paragraph_with_bottom_at_or_after(&self.paragraphs, position.y);
    if let Some(paragraph) = self.paragraphs.get(paragraph_ix) {
      if position.y < paragraph.top {
        return DocumentOffset {
          paragraph: paragraph.index,
          byte: paragraph.byte_range.start,
        };
      }
      return paragraph.hit_test(position);
    }
    let Some(last) = self.paragraphs.last() else {
      return DocumentOffset::default();
    };
    DocumentOffset {
      paragraph: last.index,
      byte: last.byte_range.end.min(last.len),
    }
  }
}

#[derive(Clone)]
pub(super) struct LaidOutParagraph {
  pub(super) index: usize,
  pub(super) cache_key: ParagraphCacheKey,
  pub(super) len: usize,
  pub(super) byte_range: Range<usize>,
  pub(super) top: Pixels,
  pub(super) bottom: Pixels,
  pub(super) lines: Vec<LaidOutLine>,
  pub(super) borders: Vec<RunRect>,
}

#[derive(Clone)]
#[allow(dead_code)]
pub(super) enum LaidOutBlock {
  Paragraph(LaidOutParagraph),
  Image(LaidOutObjectBlock),
  Equation(LaidOutObjectBlock),
  Table(LaidOutTable),
}

#[derive(Clone)]
#[allow(dead_code)]
pub(super) struct LaidOutObjectBlock {
  pub(super) block_ix: usize,
  pub(super) top: Pixels,
  pub(super) bottom: Pixels,
  pub(super) bounds: Bounds<Pixels>,
  pub(super) render_ready: bool,
}

#[derive(Clone)]
#[allow(dead_code)]
pub(super) struct LaidOutTable {
  pub(super) block_ix: usize,
  pub(super) top: Pixels,
  pub(super) bottom: Pixels,
  pub(super) bounds: Bounds<Pixels>,
  pub(super) rows: Vec<LaidOutTableRow>,
}

#[derive(Clone)]
#[allow(dead_code)]
pub(super) struct LaidOutTableRow {
  pub(super) top: Pixels,
  pub(super) bottom: Pixels,
  pub(super) cells: Vec<LaidOutTableCell>,
}

#[derive(Clone)]
#[allow(dead_code)]
pub(super) struct LaidOutTableCell {
  pub(super) bounds: Bounds<Pixels>,
  pub(super) blocks: Vec<LaidOutBlock>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ParagraphCacheKey {
  pub(super) fingerprint: u64,
}

#[derive(Clone, Copy, PartialEq)]
pub(super) struct ParagraphHeightCacheEntry {
  pub(super) key: ParagraphCacheKey,
  pub(super) width: Pixels,
  pub(super) invisibility_mode: bool,
  pub(super) edit_generation: u64,
  pub(super) height: Pixels,
}

#[hotpath::measure]
pub(super) fn paragraph_cache_key(_document: &Document, paragraph: &Paragraph) -> ParagraphCacheKey {
  let mut hasher = FxHasher::default();
  paragraph.style.hash(&mut hasher);
  paragraph.version.hash(&mut hasher);
  ParagraphCacheKey {
    fingerprint: hasher.finish(),
  }
}

#[hotpath::measure_all]
impl LaidOutParagraph {
  pub(super) fn shift_y(&mut self, new_top: Pixels) {
    let delta = new_top - self.top;
    self.top += delta;
    self.bottom += delta;
    for line in &mut self.lines {
      line.origin.y += delta;
    }
    for border in &mut self.borders {
      border.bounds.origin.y += delta;
    }
  }

  pub(super) fn hit_test(&self, position: Point<Pixels>) -> DocumentOffset {
    let line_ix = first_line_with_bottom_at_or_after(&self.lines, position.y);
    if let Some(line) = self.lines.get(line_ix) {
      return DocumentOffset {
        paragraph: self.index,
        byte: line.hit_test_x(position.x - line.origin.x),
      };
    }
    DocumentOffset {
      paragraph: self.index,
      byte: self.byte_range.end.min(self.len),
    }
  }

  pub(super) fn contains_byte(&self, byte: usize) -> bool {
    if self.byte_range.start == self.byte_range.end {
      return byte == self.byte_range.start;
    }

    (byte >= self.byte_range.start && byte < self.byte_range.end) || (byte == self.byte_range.end && self.byte_range.end == self.len)
  }
}

#[derive(Clone)]
pub(super) struct LaidOutLine {
  pub(super) origin: Point<Pixels>,
  pub(super) line_height: Pixels,
  pub(super) ascent: Pixels,
  pub(super) descent: Pixels,
  pub(super) width: Pixels,
  pub(super) start_byte: usize,
  pub(super) end_byte: usize,
  pub(super) segments: Vec<LaidOutSegment>,
  pub(super) rects: Vec<RunRect>,
  pub(super) underlines: Vec<Decoration>,
  pub(super) strikethroughs: Vec<Decoration>,
}

#[hotpath::measure_all]
impl LaidOutLine {
  pub(super) fn baseline_y(&self) -> Pixels {
    ((self.line_height - self.ascent - self.descent) / 2.0) + self.ascent
  }

  pub(super) fn hit_test_x(&self, x: Pixels) -> usize {
    for segment in &self.segments {
      if x <= segment.x + segment.width {
        let local_x = (x - segment.x).max(px(0.0));
        return segment.start_byte + segment.shaped.closest_index_for_x(local_x);
      }
    }
    self.end_byte
  }
}

#[derive(Clone)]
pub(super) struct LaidOutSegment {
  pub(super) shaped: ShapedLine,
  pub(super) format: EffectiveRunFormat,
  pub(super) x: Pixels,
  pub(super) width: Pixels,
  pub(super) box_pad_left: Pixels,
  pub(super) box_pad_right: Pixels,
  pub(super) ascent: Pixels,
  pub(super) descent: Pixels,
  pub(super) font_size: Pixels,
  pub(super) start_byte: usize,
}

#[derive(Clone)]
pub(super) struct RunRect {
  pub(super) bounds: Bounds<Pixels>,
  pub(super) color: Hsla,
  pub(super) snap: RuleSnap,
}

#[derive(Clone, Copy)]
pub(super) enum RuleSnap {
  None,
  Horizontal,
  Vertical,
}

#[derive(Clone)]
pub(super) struct Decoration {
  pub(super) bounds: Bounds<Pixels>,
  pub(super) color: Hsla,
}

#[derive(Clone)]
pub(super) struct EffectiveParagraphFormat {
  pub(super) font_size: Pixels,
  pub(super) font_family: SharedString,
  pub(super) bold: bool,
  pub(super) italic: bool,
  pub(super) color: Hsla,
  pub(super) align: ParagraphAlign,
  pub(super) spacing_before: Pixels,
  pub(super) spacing_after: Pixels,
  pub(super) line_spacing: f32,
  pub(super) border: Option<ParagraphBorder>,
  pub(super) underline: UnderlineKind,
}

#[derive(Clone, Copy)]
pub(super) struct ParagraphBorder {
  width: Pixels,
  space_x: Pixels,
  space_y: Pixels,
}

#[derive(Clone, Copy, PartialEq)]
pub(super) enum ParagraphAlign {
  Left,
  Center,
}

#[derive(Clone, Copy, PartialEq)]
pub(super) enum UnderlineKind {
  None,
  Single,
  Double,
}

#[hotpath::measure_all]
impl From<ThemeUnderline> for UnderlineKind {
  fn from(value: ThemeUnderline) -> Self {
    match value {
      ThemeUnderline::None => UnderlineKind::None,
      ThemeUnderline::Single => UnderlineKind::Single,
      ThemeUnderline::Double => UnderlineKind::Double,
    }
  }
}

#[derive(Clone)]
pub(super) struct EffectiveRunFormat {
  pub(super) font_size: Pixels,
  pub(super) font_family: SharedString,
  pub(super) bold: bool,
  pub(super) italic: bool,
  pub(super) color: Hsla,
  pub(super) underline: UnderlineKind,
  pub(super) strikethrough: bool,
  pub(super) highlight: Option<Hsla>,
  pub(super) border_width: Pixels,
}

