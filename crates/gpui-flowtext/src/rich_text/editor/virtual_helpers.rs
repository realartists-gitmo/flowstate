#[hotpath::measure]
fn item_lookup_for_virtual_items(items: &[VirtualItem], paragraph_count: usize) -> (Vec<Range<usize>>, Vec<u32>) {
  let mut paragraph_chunk_item_ranges = vec![0..0; paragraph_count];
  let mut paragraph_remainder_items = vec![NO_REMAINDER_ITEM; paragraph_count];

  for (item_ix, item) in items.iter().enumerate() {
    match item {
      VirtualItem::ParagraphChunk {
        paragraph_ix, ..
      } => {
        if let Some(range) = paragraph_chunk_item_ranges.get_mut(*paragraph_ix) {
          if range.start == range.end {
            *range = item_ix..item_ix + 1;
          } else {
            range.end = range.end.max(item_ix + 1);
          }
        }
      },
      VirtualItem::ParagraphRemainder { paragraph_ix, .. } => {
        if let Some(slot) = paragraph_remainder_items.get_mut(*paragraph_ix) {
          *slot = encode_remainder_item_ix(item_ix);
        }
      },
      VirtualItem::HiddenBlock { .. } | VirtualItem::StructuralBlock { .. } => {},
    }
  }

  (paragraph_chunk_item_ranges, paragraph_remainder_items)
}

#[hotpath::measure]
fn patch_item_lookup_for_paragraph_range(
  paragraph_chunk_item_ranges: &mut [Range<usize>],
  paragraph_remainder_items: &mut [u32],
  items: &[VirtualItem],
  replace_start: usize,
  new_len: usize,
  range: Range<usize>,
  item_delta: isize,
) -> Option<()> {
  for paragraph_ix in range.clone() {
    if let Some(chunk_range) = paragraph_chunk_item_ranges.get_mut(paragraph_ix) {
      *chunk_range = 0..0;
    }
    if let Some(remainder_item) = paragraph_remainder_items.get_mut(paragraph_ix) {
      *remainder_item = NO_REMAINDER_ITEM;
    }
  }

  for (relative_item_ix, item) in items.get(replace_start..replace_start + new_len)?.iter().enumerate() {
    let item_ix = replace_start + relative_item_ix;
    match item {
      VirtualItem::ParagraphChunk {
        paragraph_ix, ..
      } if range.contains(paragraph_ix) => {
        if let Some(chunk_range) = paragraph_chunk_item_ranges.get_mut(*paragraph_ix) {
          if chunk_range.start == chunk_range.end {
            *chunk_range = item_ix..item_ix + 1;
          } else {
            chunk_range.end = chunk_range.end.max(item_ix + 1);
          }
        }
      },
      VirtualItem::ParagraphRemainder { paragraph_ix, .. } if range.contains(paragraph_ix) => {
        if let Some(remainder_item) = paragraph_remainder_items.get_mut(*paragraph_ix) {
          *remainder_item = encode_remainder_item_ix(item_ix);
        }
      },
      _ => {},
    }
  }

  if item_delta != 0 {
    for chunk_range in paragraph_chunk_item_ranges.iter_mut().skip(range.end) {
      if chunk_range.start != chunk_range.end {
        chunk_range.start = chunk_range.start.checked_add_signed(item_delta)?;
        chunk_range.end = chunk_range.end.checked_add_signed(item_delta)?;
      }
    }
    for remainder_item in paragraph_remainder_items.iter_mut().skip(range.end) {
      if let Some(item_ix) = decode_remainder_item_ix(*remainder_item) {
        *remainder_item = encode_remainder_item_ix(item_ix.checked_add_signed(item_delta)?);
      }
    }
  }

  Some(())
}

#[hotpath::measure]
fn expand_paragraph_range(range: Range<usize>, paragraph_count: usize, padding: usize) -> Range<usize> {
  if paragraph_count == 0 {
    return 0..0;
  }
  let start = range.start.saturating_sub(padding).min(paragraph_count);
  let end = range
    .end
    .saturating_add(padding)
    .min(paragraph_count)
    .max(start);
  start..end
}

#[hotpath::measure]
fn byte_at_ratio_in_paragraph(document: &DocumentProjection, paragraph_ix: usize, start_byte: usize, end_byte: usize, ratio: f32) -> usize {
  let Some(paragraph) = document.paragraphs.get(paragraph_ix) else {
    return 0;
  };
  let start = start_byte.min(paragraph_text_len(paragraph));
  let end = end_byte.min(paragraph_text_len(paragraph)).max(start);
  if start == end {
    return start;
  }
  let target = start + ((end - start) as f32 * ratio.clamp(0.0, 1.0)).round() as usize;
  let text = paragraph_text(document, paragraph_ix);
  floor_char_boundary(&text, target.min(text.len()))
}

#[hotpath::measure]
fn detach_document_for_background_write(document: &DocumentProjection) -> DocumentProjection {
  DocumentProjection {
    frontier: document.frontier.clone(),
    text: document.text.clone(),
    paragraphs: Arc::new(document.paragraphs.as_ref().clone()),
    blocks: Arc::new(document.blocks.as_ref().clone()),
    assets: document.assets.clone(),
    ids: document.ids.clone(),
    sections: Arc::new(document.sections.as_ref().clone()),
    offset_index: document.offset_index.clone(),
    theme: document.theme.clone(),
  }
}

#[hotpath::measure]
fn floor_char_boundary(text: &str, mut byte: usize) -> usize {
  byte = byte.min(text.len());
  while byte > 0 && !text.is_char_boundary(byte) {
    byte -= 1;
  }
  byte
}

